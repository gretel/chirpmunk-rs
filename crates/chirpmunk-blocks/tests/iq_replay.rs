// SPDX-License-Identifier: GPL-3.0-only

//! IQ replay decode using `gr4-lora/test_vectors/sf7_cr1_bw125000`.
//! Loads the canonical `tx_07_iq_frame.cf32` capture and feeds it
//! through the chirpmunk RX flowgraph; asserts FrameSink emits a
//! `lora_frame` with the original payload "Hello MeshCore".

use std::time::Duration;

use chirpmunk_blocks::{DedupState, FrameSink, FrameSinkConfig};
use chirpmunk_cbor::LoraFrame;
use chirpmunk_phy::build_lora_rx_soft_decoding;
use chirpmunk_phy::utils::{Bandwidth, Channel, HeaderMode, LdroMode, SpreadingFactor, SynchWord};

use futuresdr::blocks::VectorSource;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use futuresdr::runtime::Timer;
use tokio::sync::mpsc::unbounded_channel;

const VECTOR_PATH: &str = "../../../gr4-lora/test_vectors/sf7_cr1_bw125000/tx_07_iq_frame.cf32";

fn load_cf32(path: &str) -> Vec<Complex32> {
    let bytes = std::fs::read(path).expect("read cf32 vector");
    assert_eq!(bytes.len() % 8, 0, "cf32 file size not a multiple of 8");
    bytes
        .chunks_exact(8)
        .map(|c| {
            let i = f32::from_le_bytes(c[0..4].try_into().unwrap());
            let q = f32::from_le_bytes(c[4..8].try_into().unwrap());
            Complex32::new(i, q)
        })
        .collect()
}

#[test]
fn replay_sf7_cr1_bw125_decodes_hello_meshcore() -> Result<()> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let absolute_path = manifest_dir.join(VECTOR_PATH);
    let samples = load_cf32(absolute_path.to_str().expect("utf-8 path"));
    assert!(!samples.is_empty(), "empty IQ vector");

    let (cbor_tx, mut rx) = unbounded_channel();
    let dedup = DedupState::new(Duration::ZERO, cbor_tx);

    let mut fg = Flowgraph::new();
    let source = fg.add(VectorSource::<Complex32>::new(samples));

    let (frame_sync, decoder) = build_lora_rx_soft_decoding(
        &mut fg,
        Channel::EU868_1,
        Bandwidth::BW125,
        SpreadingFactor::SF7,
        HeaderMode::Explicit,
        LdroMode::AUTO,
        Some(&[SynchWord::Private]),
        4,
        None,
        None,
        false,
        None,
    )
    .expect("build rx");

    let cfg = FrameSinkConfig {
        sf: 7,
        bw: 125_000,
        cr: 1,
        sync_word: 0x12,
        device: Some("test_vector".into()),
        decode_label: Some("replay".into()),
        rx_channel: Some(0),
    };
    let frame_sink = fg.add(FrameSink::new(cfg, dedup));

    connect!(fg,
        source > frame_sync;
        decoder.out_annotated | frame_sink;
    );

    let runtime = Runtime::new();
    let _running = runtime.start(fg).expect("start");

    let frame = Runtime::block_on(async move {
        for _ in 0..400 {
            if let Ok((bytes, _sw)) = rx.try_recv()
                && let Ok(f) = LoraFrame::from_slice(&bytes)
            {
                return Some(f);
            }
            Timer::after(Duration::from_millis(50)).await;
        }
        None
    });

    let frame = frame.expect("FrameSink never received a decoded frame");
    assert_eq!(frame.payload, b"Hello MeshCore");
    assert_eq!(frame.payload_len, b"Hello MeshCore".len() as u32);
    assert_eq!(frame.phy.sf, 7);
    assert_eq!(frame.phy.sync_word, 0x12);
    Ok(())
}
