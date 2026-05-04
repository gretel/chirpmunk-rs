// SPDX-License-Identifier: GPL-3.0-only

//! chirpmunk-trx: full-duplex LoRa transceiver daemon.
//!
//! Two modes:
//!
//!   * `--loopback` — TX block routed back into RX inside the
//!     flowgraph; no hardware required. Used by integration tests.
//!   * (default) — hardware path: seify::Source feeds the RX chain,
//!     TX block writes into seify::Sink. UHD/Soapy backends.
//!
//! Wires (UDP plane, both modes): `chirpmunk-udp::Server` for
//! subscribe + broadcast + inbound `lora_tx`, `chirpmunk-blocks::FrameSink`
//! for per-decode CBOR `lora_frame`, `chirpmunk-blocks::dispatch_lora_tx`
//! for the inbound dispatcher with `lora_tx_ack` reply.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use chirpmunk_blocks::{FrameSink, FrameSinkConfig, dispatch_lora_tx};
use chirpmunk_cbor::LoraTx;
use chirpmunk_phy::default_values::{HAS_CRC, PREAMBLE_LEN};
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
const SF: SpreadingFactor = SpreadingFactor::SF7;
const BW_HZ: f64 = 125_000.0;

#[derive(Parser, Debug)]
#[clap(version, about = "chirpmunk full-duplex LoRa transceiver daemon")]
struct Args {
    /// UDP bind address for the CBOR control plane.
    #[clap(long, default_value = "127.0.0.1:5556")]
    bind: SocketAddr,
    /// Loopback mode: TX block routed straight back into RX without
    /// hardware. Without this flag the daemon uses seify (UHD/Soapy)
    /// for both TX and RX.
    #[clap(long)]
    loopback: bool,
    /// seify device args. E.g. "driver=uhd" or
    /// "driver=uhd,serial=BADC10E". Ignored in loopback mode.
    #[clap(long, default_value = "driver=uhd")]
    device_args: String,
    /// Carrier centre frequency in Hz.
    #[clap(long, default_value_t = 869_618_000.0)]
    freq: f64,
    /// RX gain in dB.
    #[clap(long, default_value_t = 60.0)]
    rx_gain: f64,
    /// TX gain in dB. Default 0 = minimum power.
    #[clap(long, default_value_t = 0.0)]
    tx_gain: f64,
    /// Oversampling factor. Sample rate = BW × os_factor.
    #[clap(long, default_value_t = 4)]
    os_factor: usize,
    /// RX antenna name (driver-specific, e.g. "RX2", "TX/RX", "A").
    #[clap(long)]
    rx_antenna: Option<String>,
    /// TX antenna name (driver-specific).
    #[clap(long)]
    tx_antenna: Option<String>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();

    let args = Args::parse();
    info!(?args, "starting chirpmunk-trx");

    let (server, mut inbound_rx) = Server::bind_with_inbound(args.bind)
        .await
        .context("bind UDP")?;
    let local = server.local_addr()?;
    info!(?local, "udp ready");

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
        Bandwidth::default(),
        SF,
        CodeRate::default(),
        HAS_CRC,
        LdroMode::AUTO,
        HeaderMode::Explicit,
        args.os_factor,
        SynchWord::Private,
        Some(PREAMBLE_LEN),
        PAD,
    )?;
    let (frame_sync, decoder) = build_lora_rx_soft_decoding(
        &mut fg,
        Channel::EU868_1,
        Bandwidth::default(),
        SF,
        HeaderMode::Explicit,
        LdroMode::AUTO,
        Some(&[SynchWord::Private]),
        args.os_factor,
        None,
        None,
        false,
        None,
    )?;

    let cfg = FrameSinkConfig {
        sf: 7,
        bw: 125_000,
        cr: 4,
        sync_word: 0x12,
        device: Some(if args.loopback {
            "chirpmunk-trx-loopback".into()
        } else {
            args.device_args.clone()
        }),
        decode_label: Some(if args.loopback {
            "loopback".into()
        } else {
            "hardware".into()
        }),
        rx_channel: Some(0),
    };
    let frame_sink = fg.add(FrameSink::new(cfg, cbor_tx));

    if args.loopback {
        connect!(fg,
            transmitter > frame_sync;
            decoder.out_annotated | frame_sink;
        );
    } else {
        let sample_rate = BW_HZ * args.os_factor as f64;
        info!(
            device_args = %args.device_args,
            freq = args.freq,
            sample_rate,
            rx_gain = args.rx_gain,
            tx_gain = args.tx_gain,
            "building seify source + sink"
        );

        let mut rx_builder = SeifyBuilder::new(args.device_args.as_str())
            .context("parse seify device args")?
            .frequency(args.freq)
            .sample_rate(sample_rate)
            .gain(args.rx_gain);
        if let Some(ant) = &args.rx_antenna {
            rx_builder = rx_builder.antenna(ant.as_str());
        }
        let rx_source = rx_builder.build_source().context("build seify source")?;

        let mut tx_builder = SeifyBuilder::new(args.device_args.as_str())
            .context("parse seify device args")?
            .frequency(args.freq)
            .sample_rate(sample_rate)
            .gain(args.tx_gain);
        if let Some(ant) = &args.tx_antenna {
            tx_builder = tx_builder.antenna(ant.as_str());
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
