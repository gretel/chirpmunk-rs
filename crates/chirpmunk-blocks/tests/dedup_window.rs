// SPDX-License-Identifier: GPL-3.0-only

//! `DedupState` unit tests — shared selection-diversity dedup used by
//! FrameSink. Two RX chains decoding the same packet inside the window
//! collapse into one emitted `lora_frame` with merged `phy.diversity`.

use std::time::Duration;

use chirpmunk_blocks::DedupState;
use chirpmunk_cbor::{Carrier, LoraFrame, Phy};
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::sleep;

fn frame(payload: &[u8], rx_channel: u32, snr_db: f64) -> LoraFrame {
    let payload_hash = fnv1a_64(payload);
    LoraFrame {
        ts: "2026-05-08T00:00:00Z".into(),
        seq: 0,
        phy: Phy {
            sf: 7,
            bw: 125_000,
            cr: 4,
            crc_valid: true,
            sync_word: 0x12,
            snr_db,
            noise_floor_db: None,
            peak_db: None,
            snr_db_td: None,
            channel_freq: None,
            decode_bw: None,
            cfo_int: None,
            cfo_frac: None,
            sfo_hat: None,
            sample_rate: None,
            frequency_corrected: None,
            ppm_error: None,
            diversity: None,
        },
        carrier: Carrier {
            sync_word: 0x12,
            sf: 7,
            bw: 125_000,
            cr: 4,
            ldro_cfg: false,
        },
        payload: payload.to_vec(),
        payload_len: payload.len() as u32,
        crc_valid: true,
        cr: 4,
        is_downchirp: false,
        id: format!("test-{rx_channel}"),
        payload_hash,
        rx_channel: Some(rx_channel),
        decode_label: None,
        device: None,
    }
}

fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[tokio::test]
async fn dedup_within_window_emits_once_with_merged_diversity() {
    let (tx, mut rx) = unbounded_channel();
    let state = DedupState::new(Duration::from_millis(50), tx);

    state.submit(frame(b"hello world", 0, 12.3), 0x12).await;
    sleep(Duration::from_millis(10)).await;
    state.submit(frame(b"hello world", 1, 8.7), 0x12).await;

    sleep(Duration::from_millis(120)).await;

    let (buf, sync) = rx
        .try_recv()
        .expect("exactly one frame expected after window");
    assert_eq!(sync, 0x12);
    let decoded = LoraFrame::from_slice(&buf).expect("decode lora_frame");
    let diversity = decoded.phy.diversity.expect("phy.diversity present");
    assert_eq!(diversity.antennas, vec![0, 1]);
    assert_eq!(diversity.snr_db_per_ant, vec![12.3, 8.7]);
    assert!((diversity.snr_db_max - 12.3).abs() < 1e-9);
    assert!(rx.try_recv().is_err(), "no second frame expected");
}

#[tokio::test]
async fn dedup_window_zero_emits_each_decode() {
    let (tx, mut rx) = unbounded_channel();
    let state = DedupState::new(Duration::ZERO, tx);

    state.submit(frame(b"hi", 0, 10.0), 0x34).await;
    state.submit(frame(b"hi", 1, 11.0), 0x34).await;

    let (b0, _) = rx.try_recv().expect("first frame emitted immediately");
    let (b1, _) = rx.try_recv().expect("second frame emitted immediately");
    let f0 = LoraFrame::from_slice(&b0).unwrap();
    let f1 = LoraFrame::from_slice(&b1).unwrap();
    assert!(f0.phy.diversity.is_none(), "no merge with window=0");
    assert!(f1.phy.diversity.is_none(), "no merge with window=0");
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn dedup_distinct_payloads_emit_separately() {
    let (tx, mut rx) = unbounded_channel();
    let state = DedupState::new(Duration::from_millis(30), tx);

    state.submit(frame(b"alpha", 0, 5.0), 0x12).await;
    state.submit(frame(b"bravo", 0, 5.0), 0x12).await;

    sleep(Duration::from_millis(80)).await;

    let _ = rx.try_recv().expect("alpha");
    let _ = rx.try_recv().expect("bravo");
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn dedup_same_chain_repeat_does_not_double_count() {
    let (tx, mut rx) = unbounded_channel();
    let state = DedupState::new(Duration::from_millis(50), tx);

    state.submit(frame(b"echo", 0, 10.0), 0x12).await;
    state.submit(frame(b"echo", 0, 11.0), 0x12).await;

    sleep(Duration::from_millis(100)).await;

    let (buf, _) = rx.try_recv().expect("one merged frame");
    let decoded = LoraFrame::from_slice(&buf).unwrap();
    // Same chain → no diversity merge (antennas list would be [0])
    // and we emit no `diversity` map at all.
    assert!(decoded.phy.diversity.is_none());
    assert!(rx.try_recv().is_err());
}
