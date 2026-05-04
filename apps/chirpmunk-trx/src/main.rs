// SPDX-License-Identifier: GPL-3.0-only

//! chirpmunk-trx: full-duplex LoRa transceiver daemon.
//!
//! Wires:
//!
//!   * `chirpmunk-udp::Server` — subscribe registry + CBOR broadcaster
//!     + inbound `lora_tx` forwarder.
//!   * FutureSDR Flowgraph — single-SF TX → loopback → RX (`--loopback`)
//!     or TX → seify Sink / seify Source → RX (deferred, M6 hardware).
//!   * `chirpmunk-blocks::FrameSink` — per-decode CBOR `lora_frame`
//!     producer; the broadcaster fans them out.
//!   * `chirpmunk-blocks::dispatch_lora_tx` — inbound `lora_tx` request
//!     dispatcher; replies with `lora_tx_ack` to the originator.

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
use futuresdr::prelude::*;
use tokio::sync::mpsc::unbounded_channel;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const PAD: usize = 10_000;
const SF: SpreadingFactor = SpreadingFactor::SF7;

#[derive(Parser, Debug)]
#[clap(version, about = "chirpmunk full-duplex LoRa transceiver daemon")]
struct Args {
    /// UDP bind address for the CBOR control plane.
    #[clap(long, default_value = "127.0.0.1:5556")]
    bind: SocketAddr,
    /// Loopback mode: TX block routed straight back into RX without
    /// hardware. Without this flag the daemon will refuse to start
    /// (hardware path not yet implemented).
    #[clap(long)]
    loopback: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();

    let args = Args::parse();
    if !args.loopback {
        anyhow::bail!("only --loopback mode is implemented in M5; hardware path is M6");
    }
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
        1,
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
        1,
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
        device: Some("chirpmunk-trx-loopback".into()),
        decode_label: Some("loopback".into()),
        rx_channel: Some(0),
    };
    let frame_sink = fg.add(FrameSink::new(cfg, cbor_tx));
    connect!(fg,
        transmitter > frame_sync;
        decoder.out_annotated | frame_sink;
    );

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
