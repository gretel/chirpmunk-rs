// SPDX-License-Identifier: GPL-3.0-only

//! M3 acceptance: a CBOR `lora_tx` request drives the daemon's TX
//! pipeline, the loopback decode produces a CBOR `lora_frame` whose
//! payload matches the request, and `dispatch_lora_tx` returns an `ok`
//! `lora_tx_ack` echoing the original sequence number.

use std::time::Duration;

use chirpmunk_blocks::{FrameSink, FrameSinkConfig, dispatch_lora_tx};
use chirpmunk_cbor::{LoraFrame, LoraTx};
use chirpmunk_phy::default_values::{HAS_CRC, PREAMBLE_LEN};
use chirpmunk_phy::utils::{
    Bandwidth, Channel, CodeRate, HeaderMode, LdroMode, SpreadingFactor, SynchWord,
};
use chirpmunk_phy::{build_lora_rx_soft_decoding, build_lora_tx};

use futuresdr::prelude::*;
use futuresdr::runtime::Timer;
use tokio::sync::mpsc::unbounded_channel;

const PAD: usize = 10_000;

#[test]
fn cbor_lora_tx_drives_loopback_to_frame_sink() -> Result<()> {
    let (cbor_tx, mut rx) = unbounded_channel();

    let mut fg = Flowgraph::new();
    let transmitter = build_lora_tx(
        &mut fg,
        Bandwidth::default(),
        SpreadingFactor::SF7,
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

    let (frame_sync, decoder) = build_lora_rx_soft_decoding(
        &mut fg,
        Channel::EU868_1,
        Bandwidth::default(),
        SpreadingFactor::SF7,
        HeaderMode::Explicit,
        LdroMode::AUTO,
        Some(&[SynchWord::Private]),
        1,
        None,
        None,
        false,
        None,
    )
    .expect("build rx");

    let cfg = FrameSinkConfig {
        sf: 7,
        bw: 125_000,
        cr: 4,
        sync_word: 0x12,
        device: None,
        decode_label: Some("m3-tx".into()),
        rx_channel: Some(0),
    };
    let frame_sink = fg.add(FrameSink::new(cfg, cbor_tx));

    connect!(fg,
        transmitter > frame_sync;
        decoder.out_annotated | frame_sink;
    );

    let transmitter_id: BlockId = transmitter.into();
    let runtime = Runtime::new();
    let handle = runtime.start(fg).expect("start").handle();

    let req = LoraTx {
        payload: b"chirpmunk-m3-tx-payload".to_vec(),
        seq: Some(99),
        ..LoraTx::default()
    };

    let (ack, frame) = Runtime::block_on(async move {
        let ack = dispatch_lora_tx(&handle, transmitter_id, &req).await;
        let mut frame: Option<LoraFrame> = None;
        for _ in 0..200 {
            if let Ok((bytes, _sw)) = rx.try_recv() {
                frame = LoraFrame::from_slice(&bytes).ok();
                break;
            }
            Timer::after(Duration::from_millis(50)).await;
        }
        (ack, frame)
    });

    assert!(ack.ok, "ack not ok: {:?}", ack);
    assert_eq!(ack.seq, 99);
    assert!(ack.error.is_none());

    let frame = frame.expect("FrameSink never received a decoded frame");
    assert_eq!(frame.payload, b"chirpmunk-m3-tx-payload");
    assert_eq!(frame.payload_len, b"chirpmunk-m3-tx-payload".len() as u32);
    assert_eq!(frame.phy.sf, 7);
    Ok(())
}

#[test]
fn dry_run_acks_without_dispatching() -> Result<()> {
    let (cbor_tx, mut rx) = unbounded_channel();
    let mut fg = Flowgraph::new();
    let transmitter = build_lora_tx(
        &mut fg,
        Bandwidth::default(),
        SpreadingFactor::SF7,
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

    let (frame_sync, decoder) = build_lora_rx_soft_decoding(
        &mut fg,
        Channel::EU868_1,
        Bandwidth::default(),
        SpreadingFactor::SF7,
        HeaderMode::Explicit,
        LdroMode::AUTO,
        Some(&[SynchWord::Private]),
        1,
        None,
        None,
        false,
        None,
    )
    .expect("build rx");

    let cfg = FrameSinkConfig {
        sf: 7,
        bw: 125_000,
        cr: 4,
        sync_word: 0x12,
        device: None,
        decode_label: Some("m3-dryrun".into()),
        rx_channel: Some(0),
    };
    let frame_sink = fg.add(FrameSink::new(cfg, cbor_tx));

    connect!(fg,
        transmitter > frame_sync;
        decoder.out_annotated | frame_sink;
    );

    let transmitter_id: BlockId = transmitter.into();
    let runtime = Runtime::new();
    let handle = runtime.start(fg).expect("start").handle();

    let req = LoraTx {
        payload: b"never-emitted".to_vec(),
        seq: Some(7),
        dry_run: true,
        ..LoraTx::default()
    };

    let (ack, observed) = Runtime::block_on(async move {
        let ack = dispatch_lora_tx(&handle, transmitter_id, &req).await;
        Timer::after(Duration::from_millis(200)).await;
        let any = rx.try_recv().is_ok();
        (ack, any)
    });

    assert!(ack.ok);
    assert_eq!(ack.seq, 7);
    assert!(!observed, "dry_run should not produce a frame");
    Ok(())
}
