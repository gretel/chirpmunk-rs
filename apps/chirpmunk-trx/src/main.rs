// SPDX-License-Identifier: GPL-3.0-only

//! chirpmunk-trx: full-duplex LoRa transceiver daemon.
//!
//! Driven entirely by a TOML config file (default
//! `~/.config/chirpmunk/config.toml`, override via `--config <path>`).
//! Schema mirrors `gr4-lora/apps/config-pluto.toml` plus a chirpmunk
//! extension section for fields without an upstream analogue.
//!
//! Two modes (selected by `[chirpmunk] loopback`):
//!
//!   * loopback = true  — TX block routed back into RX inside the
//!     flowgraph. No hardware required.
//!   * loopback = false — seify (UHD/Soapy) for RX and TX.
//!
//! Wires (UDP plane, both modes): `chirpmunk-udp::Server` for
//! subscribe + broadcast + inbound `lora_tx`, `chirpmunk-blocks::FrameSink`
//! for per-decode CBOR `lora_frame`, `chirpmunk-blocks::dispatch_lora_tx`
//! for the inbound dispatcher with `lora_tx_ack` reply.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chirpmunk_blocks::{
    ChannelActivityDetector, DedupState, FrameSink, FrameSinkConfig, LbtPolicy, SoapyDirectSink,
    SoapyDirectSource, SoapyRxConfig, SoapyTxConfig, default_alpha, dispatch_lora_tx, open_device,
};
use chirpmunk_cbor::LoraTx;
use chirpmunk_config::{Config, Radio, RadioOrSection};
use chirpmunk_phy::default_values::HAS_CRC;
use chirpmunk_phy::utils::{
    Bandwidth, Channel, CodeRate, HeaderMode, LdroMode, SpreadingFactor, SynchWord,
};
use chirpmunk_phy::{build_lora_rx_soft_decoding, build_lora_tx};
use chirpmunk_udp::Server;
use clap::Parser;
use futuresdr::blocks::{NullSink, StreamDuplicator};
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use tokio::sync::mpsc::unbounded_channel;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const PAD: usize = 10_000;

#[derive(Parser, Debug)]
#[clap(version, about = "chirpmunk full-duplex LoRa transceiver daemon")]
struct Args {
    /// TOML config path. Default: $XDG_CONFIG_HOME/chirpmunk/config.toml
    /// or $HOME/.config/chirpmunk/config.toml.
    #[clap(long)]
    config: Option<PathBuf>,
    /// Force loopback mode and synthesize sensible defaults when no
    /// config file is supplied.
    #[clap(long)]
    loopback: bool,
    /// Override the UDP bind address (`host:port`).
    #[clap(long)]
    bind: Option<String>,
}

fn default_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("chirpmunk").join("config.toml"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config/chirpmunk/config.toml"))
}

fn init_tracing(level: &str) {
    let default = format!(
        "chirpmunk_phy=warn,chirpmunk_blocks={lvl},chirpmunk_trx={lvl},chirpmunk_udp={lvl}",
        lvl = level.to_lowercase()
    );
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn pick_radio<'a>(cfg: &'a Config, name: &str) -> Result<&'a Radio> {
    cfg.radios
        .get(name)
        .and_then(|r| match r {
            RadioOrSection::Radio(radio) => Some(radio),
            RadioOrSection::Other(_) => None,
        })
        .ok_or_else(|| anyhow!("config: radio section [{name}] not found"))
}

fn build_seify_args(driver: &str, param: &str) -> String {
    if param.is_empty() {
        format!("soapy_driver={driver}")
    } else {
        format!("soapy_driver={driver},{param}")
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    let cfg_opt: Option<Config> = match (&args.config, args.loopback) {
        (Some(p), _) => {
            Some(Config::from_path(p).with_context(|| format!("read config {}", p.display()))?)
        }
        (None, true) => None,
        (None, false) => {
            let path = default_config_path()
                .ok_or_else(|| anyhow!("could not determine config path; pass --config"))?;
            if !path.exists() {
                let parent = path.parent().unwrap_or(std::path::Path::new("."));
                bail!(
                    "no config at {}\n\nFirst run? Try one of:\n  chirpmunk-trx --loopback                 # standalone, no hardware\n  chirpmunk-trx --config <path>            # explicit config\n  mkdir -p {} && cp apps/chirpmunk-trx/config.example.toml {}\n",
                    path.display(),
                    parent.display(),
                    path.display(),
                );
            }
            Some(
                Config::from_path(&path)
                    .with_context(|| format!("read config {}", path.display()))?,
            )
        }
    };

    let log_level = cfg_opt
        .as_ref()
        .map(|c| c.logging.level.as_str())
        .unwrap_or("INFO");
    init_tracing(log_level);
    if let Some(_cfg) = cfg_opt.as_ref() {
        info!("config loaded");
    } else {
        info!("standalone loopback (no config)");
    }

    let want_loopback = args.loopback
        || cfg_opt
            .as_ref()
            .map(|c| c.chirpmunk.loopback)
            .unwrap_or(false);

    let trx_opt = cfg_opt.as_ref().and_then(|c| c.trx.as_ref());
    let tx_cfg_opt = trx_opt.and_then(|t| t.transmit.as_ref());
    let net_cfg_opt = trx_opt.and_then(|t| t.network.as_ref());

    let (sf_u8, bw_u32, cr_u8, sync_word_u8, preamble_len_u): (u8, u32, u8, u8, u32) =
        match tx_cfg_opt {
            Some(t) => (t.sf, t.bw, t.cr, t.sync_word as u8, t.preamble_len),
            None if want_loopback => (7, 125_000, 4, 0x12, 8),
            None => bail!("config: [trx.transmit] section is required"),
        };

    let sf = SpreadingFactor::try_from(sf_u8)
        .map_err(|_| anyhow!("config: invalid [trx.transmit] sf={sf_u8}; expected 7..=12"))?;
    let bw = Bandwidth::try_from(bw_u32).map_err(|_| {
        anyhow!(
            "config: invalid [trx.transmit] bw={bw_u32} Hz; expected one of 7800, 10400, 15600, 20800, 31200, 41700, 62500, 125000, 250000, 500000"
        )
    })?;
    let cr_normalised = match cr_u8 {
        1..=4 => cr_u8,
        5..=8 => cr_u8 - 4,
        _ => bail!(
            "config: invalid [trx.transmit] cr={cr_u8}; expected 1..=4 (chirpmunk: numerator-1) or 5..=8 (gr4-lora: denominator)"
        ),
    };
    let cr = CodeRate::try_from(cr_normalised).map_err(|_| {
        anyhow!("config: invalid [trx.transmit] cr={cr_u8} (normalised {cr_normalised})")
    })?;
    if preamble_len_u < 6 {
        bail!(
            "config: invalid [trx.transmit] preamble_len={preamble_len_u}; LoRa requires >= 6 (typical 8 or 16)"
        );
    }
    let sync_word: SynchWord = SynchWord::from(sync_word_u8);
    let preamble_len = preamble_len_u as usize;

    let bw_hz = Into::<u32>::into(bw);
    let rate_hz = trx_opt.map(|t| t.rate).unwrap_or(0);
    let rate_hz = if rate_hz == 0 {
        u64::from(bw_hz) * 4
    } else {
        rate_hz
    };
    if rate_hz % u64::from(bw_hz) != 0 {
        bail!("config: rate {rate_hz} not an integer multiple of bw {bw_hz}");
    }
    let os_factor = (rate_hz / u64::from(bw_hz)) as usize;
    let sample_rate = rate_hz as f64;

    let bind = match (&args.bind, net_cfg_opt) {
        (Some(b), _) => b.clone(),
        (None, Some(n)) => format!("{}:{}", n.udp_listen, n.udp_port),
        (None, None) if want_loopback => "127.0.0.1:5556".into(),
        (None, None) => bail!("config: [trx.network] section or --bind is required"),
    };
    let bind_addr: std::net::SocketAddr = bind.parse().context("bind address")?;
    info!(bind = %bind_addr, sf = ?sf, bw = bw_hz, cr = ?cr, sync_word = format!("0x{:02x}", sync_word_u8), preamble_len, sample_rate, os_factor, loopback = want_loopback, "params");

    let (server, mut inbound_rx) = Server::bind_with_inbound(bind_addr)
        .await
        .context("bind UDP")?;
    info!(local = ?server.local_addr()?, "udp ready");

    {
        let s = server.clone();
        tokio::spawn(async move {
            if let Err(e) = s.run().await {
                error!(error = %e, "server.run terminated");
            }
        });
    }

    let (cbor_tx, mut cbor_rx) = unbounded_channel::<chirpmunk_blocks::Outbound>();
    {
        let s = server.clone();
        tokio::spawn(async move {
            while let Some((buf, sw)) = cbor_rx.recv().await {
                if let Err(e) = s.broadcast(&buf, Some(sw)).await {
                    warn!(error = %e, "broadcast failed");
                }
            }
        });
    }

    let mut fg = Flowgraph::new();
    let transmitter = build_lora_tx(
        &mut fg,
        bw,
        sf,
        cr,
        HAS_CRC,
        LdroMode::AUTO,
        HeaderMode::Explicit,
        os_factor,
        sync_word,
        Some(preamble_len),
        PAD,
    )?;
    let (frame_sync, decoder) = build_lora_rx_soft_decoding(
        &mut fg,
        Channel::EU868_1,
        bw,
        sf,
        HeaderMode::Explicit,
        LdroMode::AUTO,
        Some(&[sync_word]),
        os_factor,
        Some(preamble_len),
        None,
        false,
        None,
    )?;

    let device_label = if want_loopback {
        "chirpmunk-trx-loopback".to_string()
    } else {
        let cfg = cfg_opt
            .as_ref()
            .ok_or_else(|| anyhow!("hardware mode requires --config"))?;
        build_seify_args(&cfg.device.driver, &cfg.device.param)
    };
    let dedup_window_ms = trx_opt
        .and_then(|t| t.receive.as_ref())
        .and_then(|r| r.dedup_window_ms)
        .unwrap_or(0);
    let dedup = DedupState::from_window_ms(dedup_window_ms, cbor_tx);
    info!(dedup_window_ms, "dedup state");

    let cfg_sink_template = FrameSinkConfig {
        sf: u8::from(sf),
        bw: bw_hz,
        cr: u8::from(cr),
        sync_word: sync_word_u8 as u16,
        device: Some(device_label.clone()),
        decode_label: Some(
            if want_loopback {
                "loopback"
            } else {
                "hardware"
            }
            .into(),
        ),
        rx_channel: Some(0),
    };
    let frame_sink = fg.add(FrameSink::new(cfg_sink_template.clone(), dedup.clone()));

    // CAD busy flag — writer = ChannelActivityDetector, reader = LBT poll
    // in dispatch_lora_tx.
    let busy = Arc::new(AtomicBool::new(false));
    let cad_alpha = trx_opt
        .and_then(|t| t.receive.as_ref())
        .and_then(|r| r.cad_min_ratio)
        .unwrap_or_else(|| default_alpha(u8::from(sf), os_factor as u32));
    let cad_release = trx_opt
        .and_then(|t| t.receive.as_ref())
        .and_then(|r| r.cad_release_symbols)
        .unwrap_or(4);
    let cad_block =
        ChannelActivityDetector::new(u8::from(sf), os_factor as u32, cad_release, busy.clone())
            .with_alpha(cad_alpha);
    let entry_dup = fg.add(StreamDuplicator::<Complex32, 2>::new());
    let cad_id = fg.add(cad_block);

    if want_loopback {
        connect!(fg,
            transmitter > entry_dup;
            entry_dup.outputs[0] > frame_sync;
            entry_dup.outputs[1] > cad_id;
            decoder.out_annotated | frame_sink;
        );
    } else {
        let cfg = cfg_opt
            .as_ref()
            .ok_or_else(|| anyhow!("hardware mode requires --config"))?;
        let trx = cfg
            .trx
            .as_ref()
            .ok_or_else(|| anyhow!("hardware mode requires [trx] in config"))?;
        let radio = pick_radio(cfg, &trx.radio)?;

        // RX chain count = number of antennas to decode in parallel.
        // Empty `rx_channel` defaults to single-chain RX on chan 0.
        let rx_chain_count = radio.rx_channel.len().max(1);
        if cfg.device.driver == "plutoPAPR" && rx_chain_count > 1 {
            bail!("PlutoSDR has 1 RX channel; [radio_*] rx_channel must be [0] or empty");
        }
        if rx_chain_count > 2 {
            bail!("[radio_*] rx_channel.len()={rx_chain_count} unsupported (max 2 for B210/B220)");
        }

        info!(
            device_args = %device_label,
            freq = radio.freq,
            rx_gain = radio.rx_gain,
            tx_gain = radio.tx_gain,
            rx_chain_count,
            "opening soapy direct device"
        );

        // Single physical USRP, single soapysdr::Device handle, cloned
        // (Arc-shared) to RX source and TX sink. seify cannot be used
        // for TX on the LibreSDR_B220mini clone — its TxStreamer trait
        // omits read_status / multi-channel buddy-share semantics that
        // this hardware requires. See `references/tx-gap.md` skill.
        let dev = open_device(device_label.as_str()).context("open soapy device")?;

        let rx_cfg = SoapyRxConfig {
            freq_hz: radio.freq as f64,
            rate_hz: sample_rate,
            gain_db: radio.rx_gain,
            antenna: radio.rx_antenna.first().cloned(),
        };
        let tx_cfg = SoapyTxConfig {
            freq_hz: radio.freq as f64,
            rate_hz: sample_rate,
            gain_db: radio.tx_gain,
            antenna: if radio.tx_antenna.is_empty() {
                None
            } else {
                Some(radio.tx_antenna.clone())
            },
            // 200 ms in the future — gives host time to pre-fill UHD's
            // internal ring before any RF is emitted.
            activation_offset_ns: 200_000_000,
        };

        let rx_source = fg.add(SoapyDirectSource::new(dev.clone(), rx_cfg));
        let tx_sink = fg.add(SoapyDirectSink::new(dev.clone(), tx_cfg));

        if rx_chain_count == 2 {
            // Diversity RX: chain 1 gets its own (frame_sync, decoder,
            // frame_sink) pair; both FrameSinks share the dedup state
            // so identical decodes within `dedup_window_ms` collapse
            // into one emitted lora_frame with merged phy.diversity.
            // CAD stays on chain 0 only — single LBT decision point
            // for the radio.
            let (frame_sync_1, decoder_1) = build_lora_rx_soft_decoding(
                &mut fg,
                Channel::EU868_1,
                bw,
                sf,
                HeaderMode::Explicit,
                LdroMode::AUTO,
                Some(&[sync_word]),
                os_factor,
                Some(preamble_len),
                None,
                false,
                None,
            )?;
            let mut cfg1 = cfg_sink_template.clone();
            cfg1.rx_channel = Some(1);
            cfg1.decode_label = Some("hardware-chan1".into());
            let frame_sink_1 = fg.add(FrameSink::new(cfg1, dedup.clone()));

            connect!(fg,
                rx_source.out0 > entry_dup;
                rx_source.out1 > frame_sync_1;
                entry_dup.outputs[0] > frame_sync;
                entry_dup.outputs[1] > cad_id;
                transmitter > tx_sink;
                decoder.out_annotated | frame_sink;
                decoder_1.out_annotated | frame_sink_1;
            );
        } else {
            // Single-chain RX. Channel 1 is still opened on the
            // hardware (B200 symmetry rule) so we sink its IQ to
            // keep the streamer draining both channels in lockstep.
            let rx_chan1_null = fg.add(NullSink::<Complex32>::new());
            connect!(fg,
                rx_source.out0 > entry_dup;
                rx_source.out1 > rx_chan1_null;
                entry_dup.outputs[0] > frame_sync;
                entry_dup.outputs[1] > cad_id;
                transmitter > tx_sink;
                decoder.out_annotated | frame_sink;
            );
        }
    }

    // Build LBT policy from [trx.network] (gr4-lora layout). When the
    // network section is absent and we are in loopback, default LBT on
    // for testability; otherwise off.
    let (lbt_enabled, lbt_timeout_ms_raw) = net_cfg_opt
        .map(|n| (n.lbt, n.lbt_timeout_ms))
        .unwrap_or((want_loopback, 2000));
    let lbt_timeout_ms = if lbt_timeout_ms_raw == 0 {
        2000
    } else {
        lbt_timeout_ms_raw
    };
    let policy = if lbt_enabled {
        Some(LbtPolicy {
            busy: busy.clone(),
            timeout: Duration::from_millis(lbt_timeout_ms as u64),
            poll_interval: Duration::from_millis(10),
        })
    } else {
        None
    };
    info!(
        lbt = policy.is_some(),
        lbt_timeout_ms, cad_alpha, cad_release, "LBT policy"
    );

    let transmitter_id: BlockId = transmitter.into();
    let runtime = Runtime::new();
    let handle = runtime.start(fg).context("start flowgraph")?.handle();
    info!("flowgraph running");

    {
        let handle = handle.clone();
        let server = server.clone();
        let policy = policy.clone();
        tokio::spawn(async move {
            while let Some((peer, bytes)) = inbound_rx.recv().await {
                let req = match LoraTx::from_slice(&bytes) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(?peer, error = %e, "discarding malformed inbound");
                        continue;
                    }
                };
                let ack = dispatch_lora_tx(&handle, transmitter_id, &req, policy.as_ref()).await;
                match chirpmunk_cbor::to_vec(&ack) {
                    Ok(buf) => {
                        if let Err(e) = server.send_to(&buf, peer).await {
                            warn!(?peer, error = %e, "send_to failed");
                        }
                    }
                    Err(e) => warn!(error = %e, "encoding ack failed"),
                }
            }
        });
    }

    tokio::signal::ctrl_c().await.ok();
    info!("shutdown signal received");
    Ok(())
}
