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

use anyhow::{Context, Result, anyhow, bail};
use chirpmunk_blocks::{FrameSink, FrameSinkConfig, dispatch_lora_tx};
use chirpmunk_cbor::LoraTx;
use chirpmunk_config::{Config, Radio, RadioOrSection};
use chirpmunk_phy::default_values::HAS_CRC;
use chirpmunk_phy::utils::{
    Bandwidth, Channel, CodeRate, HeaderMode, LdroMode, SpreadingFactor, SynchWord,
};
use chirpmunk_phy::{build_lora_rx_soft_decoding, build_lora_tx};
use chirpmunk_udp::Server;
use clap::Parser;
use futuresdr::blocks::seify::Builder as SeifyBuilder;
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
    let cfg_sink = FrameSinkConfig {
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
    let frame_sink = fg.add(FrameSink::new(cfg_sink, cbor_tx));

    if want_loopback {
        connect!(fg,
            transmitter > frame_sync;
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

        info!(
            device_args = %device_label,
            freq = radio.freq,
            rx_gain = radio.rx_gain,
            tx_gain = radio.tx_gain,
            "building seify source + sink"
        );

        let mut rx_builder = SeifyBuilder::new(device_label.as_str())
            .context("parse seify device args")?
            .frequency(radio.freq as f64)
            .sample_rate(sample_rate)
            .gain(radio.rx_gain);
        if let Some(ant) = radio.rx_antenna.first() {
            rx_builder = rx_builder.antenna(ant.as_str());
        }
        let rx_source = rx_builder.build_source().context("build seify source")?;

        let mut tx_builder = SeifyBuilder::new(device_label.as_str())
            .context("parse seify device args")?
            .frequency(radio.freq as f64)
            .sample_rate(sample_rate)
            .gain(radio.tx_gain);
        if !radio.tx_antenna.is_empty() {
            tx_builder = tx_builder.antenna(radio.tx_antenna.as_str());
        }
        let tx_sink = tx_builder.build_sink().context("build seify sink")?;

        connect!(fg,
            rx_source.outputs[0] > frame_sync;
            transmitter > inputs[0].tx_sink;
            decoder.out_annotated | frame_sink;
        );
    }

    let transmitter_id: BlockId = transmitter.into();
    let runtime = Runtime::new();
    let handle = runtime.start(fg).context("start flowgraph")?.handle();
    info!("flowgraph running");

    {
        let handle = handle.clone();
        let server = server.clone();
        tokio::spawn(async move {
            while let Some((peer, bytes)) = inbound_rx.recv().await {
                let req = match LoraTx::from_slice(&bytes) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(?peer, error = %e, "discarding malformed inbound");
                        continue;
                    }
                };
                let ack = dispatch_lora_tx(&handle, transmitter_id, &req).await;
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
