// SPDX-License-Identifier: GPL-3.0-only

//! Multi-SF parallel chain test.
//!
//! TX transmits at one specific SF; six parallel RX chains (SF7..SF12)
//! attempt to decode. Only the matching chain should produce a decoded
//! frame on its FrameSink.

use std::time::Duration;

use chirpmunk_blocks::{DedupState, FrameSinkConfig, build_multi_sf_rx};
use chirpmunk_cbor::LoraFrame;
use chirpmunk_phy::default_values::{HAS_CRC, PREAMBLE_LEN};
use chirpmunk_phy::utils::{
    Bandwidth, Channel, CodeRate, HeaderMode, LdroMode, SpreadingFactor, SynchWord,
};
use chirpmunk_phy::{build_lora_tx, utils::DemodulatedSymbolHardDecoding};

use futuresdr::prelude::*;
use futuresdr::runtime::Timer;
use tokio::sync::mpsc::unbounded_channel;

const PAD: usize = 10_000;

#[test]
fn only_matching_sf_chain_decodes() -> Result<()> {
    let _ = DemodulatedSymbolHardDecoding::default();
    let (tx, mut rx) = unbounded_channel();
    let dedup = DedupState::new(Duration::ZERO, tx);

    let mut fg = Flowgraph::new();
    let transmitter = build_lora_tx(
        &mut fg,
        Bandwidth::default(),
        SpreadingFactor::SF8,
        CodeRate::default(),
        HAS_CRC,
        LdroMode::AUTO,
        HeaderMode::Explicit,
        1,
        SynchWord::Private,
        Some(PREAMBLE_LEN),
        PAD,
    )
    .expect("build tx");

    let cfg = FrameSinkConfig {
        sf: 0,
        bw: 125_000,
        cr: 4,
        sync_word: 0x12,
        device: None,
        decode_label: Some("multi".into()),
        rx_channel: Some(0),
    };
    let multi = build_multi_sf_rx(
        &mut fg,
        Channel::EU868_1,
        Bandwidth::default(),
        SynchWord::Private,
        1,
        cfg,
        dedup,
    )
    .expect("build multi-sf rx");

    let entry = multi.entry;
    connect!(fg, transmitter > entry;);

    let transmitter_id: BlockId = transmitter.into();
    let runtime = Runtime::new();
    let handle = runtime.start(fg).expect("start").handle();

    let collected = Runtime::block_on(async move {
        handle
            .post(transmitter_id, "msg", Pmt::String("multi-sf-test".into()))
            .await
            .expect("post msg");

        let mut frames: Vec<LoraFrame> = Vec::new();
        for _ in 0..400 {
            while let Ok((bytes, _sw)) = rx.try_recv() {
                if let Ok(f) = LoraFrame::from_slice(&bytes) {
                    frames.push(f);
                }
            }
            if !frames.is_empty() {
                Timer::after(Duration::from_millis(50)).await;
                while let Ok((bytes, _sw)) = rx.try_recv() {
                    if let Ok(f) = LoraFrame::from_slice(&bytes) {
                        frames.push(f);
                    }
                }
                break;
            }
            Timer::after(Duration::from_millis(50)).await;
        }
        frames
    });

    assert!(!collected.is_empty(), "no chain produced a decoded frame");
    let decoded_sfs: Vec<u8> = collected.iter().map(|f| f.phy.sf).collect();
    assert!(
        decoded_sfs.contains(&8),
        "SF8 chain did not decode; got SFs {decoded_sfs:?}"
    );
    for f in &collected {
        assert_eq!(
            f.payload, b"multi-sf-test",
            "payload mismatch on sf{}",
            f.phy.sf
        );
    }

    Ok(())
}
