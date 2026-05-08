// SPDX-License-Identifier: GPL-3.0-only

//! End-to-end loopback: build_lora_tx → FrameSync → FftDemod → … →
//! Decoder → FrameSink. Pushes a payload through the TX block and
//! verifies the FrameSink emits a CBOR `lora_frame` whose payload bytes
//! match what we sent.

use std::time::Duration;

use chirpmunk_blocks::{DedupState, FrameSink, FrameSinkConfig};
use chirpmunk_cbor::LoraFrame;
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
fn tx_to_framesink_decodes_payload() -> Result<()> {
    let (tx, mut rx) = unbounded_channel();
    let dedup = DedupState::new(Duration::ZERO, tx);

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
        decode_label: Some("loopback".into()),
        rx_channel: Some(0),
    };
    let frame_sink = fg.add(FrameSink::new(cfg, dedup));

    connect!(fg,
        transmitter > frame_sync;
        decoder.out_annotated | frame_sink;
    );

    let transmitter_id: BlockId = transmitter.into();

    let runtime = Runtime::new();
    let handle = runtime.start(fg).expect("start").handle();

    let received = Runtime::block_on(async move {
        handle
            .post(
                transmitter_id,
                "msg",
                Pmt::String("chirpmunk-loopback-payload".into()),
            )
            .await
            .expect("post msg");

        for _ in 0..200 {
            if let Ok(item) = rx.try_recv() {
                return Some(item);
            }
            Timer::after(Duration::from_millis(50)).await;
        }
        None
    });

    let (cbor_bytes, sync_word) = received.expect("frame_sink never received a frame");
    assert_eq!(sync_word, 0x12);
    let frame = LoraFrame::from_slice(&cbor_bytes).expect("decode cbor");
    assert_eq!(frame.payload, b"chirpmunk-loopback-payload");
    assert_eq!(
        frame.payload_len,
        b"chirpmunk-loopback-payload".len() as u32
    );
    assert_eq!(frame.phy.sf, 7);
    assert_eq!(frame.phy.sync_word, 0x12);
    assert_eq!(frame.carrier.sync_word, 0x12);
    assert_eq!(frame.decode_label.as_deref(), Some("loopback"));
    Ok(())
}
