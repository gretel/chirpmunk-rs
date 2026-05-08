// SPDX-License-Identifier: GPL-3.0-only

//! Selection-diversity dedup for `lora_frame` emissions.
//!
//! Multiple FrameSink instances (one per RX chain) submit decoded
//! `LoraFrame`s here. Identical `(payload_hash, sync_word, sf, bw)`
//! arrivals inside a configurable wall-clock window collapse into a
//! single emitted frame carrying merged `phy.diversity` metadata.
//! When the window is `Duration::ZERO`, every submission is encoded
//! and forwarded immediately (back-compat passthrough).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chirpmunk_cbor::{Diversity, LoraFrame};
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use crate::frame_sink::Outbound;

type Key = (u64, u16, u8, u32);

struct Entry {
    base: LoraFrame,
    antennas: Vec<u32>,
    snr_per_ant: Vec<f64>,
    snr_max: f64,
}

pub struct DedupState {
    window: Duration,
    tx: UnboundedSender<Outbound>,
    entries: Mutex<HashMap<Key, Entry>>,
}

impl DedupState {
    pub fn new(window: Duration, tx: UnboundedSender<Outbound>) -> Arc<Self> {
        Arc::new(Self {
            window,
            tx,
            entries: Mutex::new(HashMap::new()),
        })
    }

    /// Window in ms — convenience for callers that hold the dedup
    /// window as a `u32` from the TOML config.
    pub fn from_window_ms(window_ms: u32, tx: UnboundedSender<Outbound>) -> Arc<Self> {
        Self::new(Duration::from_millis(window_ms as u64), tx)
    }

    pub fn window(&self) -> Duration {
        self.window
    }

    /// Submit a freshly-built `LoraFrame`. With `window == 0` the frame
    /// is encoded and forwarded immediately. Otherwise the first
    /// arrival per `(payload_hash, sync, sf, bw)` opens a dedup entry
    /// and schedules a delayed emit; subsequent arrivals merge into it.
    pub async fn submit(self: &Arc<Self>, frame: LoraFrame, sync_word: u16) {
        if self.window.is_zero() {
            self.encode_and_send(&frame, sync_word);
            return;
        }
        let key: Key = (
            frame.payload_hash,
            frame.phy.sync_word,
            frame.phy.sf,
            frame.phy.bw,
        );
        let chan = frame.rx_channel.unwrap_or(0);
        let snr = frame.phy.snr_db;

        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get_mut(&key) {
            if !entry.antennas.contains(&chan) {
                entry.antennas.push(chan);
                entry.snr_per_ant.push(snr);
                if snr > entry.snr_max {
                    entry.snr_max = snr;
                }
            }
            return;
        }
        let deadline = Instant::now() + self.window;
        entries.insert(
            key,
            Entry {
                base: frame,
                antennas: vec![chan],
                snr_per_ant: vec![snr],
                snr_max: snr,
            },
        );
        drop(entries);

        let this = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let entry = {
                let mut entries = this.entries.lock().await;
                entries.remove(&key)
            };
            let Some(entry) = entry else {
                return;
            };
            let mut frame = entry.base;
            if entry.antennas.len() > 1 {
                frame.phy.diversity = Some(Diversity {
                    antennas: entry.antennas.iter().map(|c| *c as u8).collect(),
                    snr_db_max: entry.snr_max,
                    snr_db_per_ant: entry.snr_per_ant,
                });
            }
            this.encode_and_send(&frame, sync_word);
        });
    }

    fn encode_and_send(&self, frame: &LoraFrame, sync_word: u16) {
        match chirpmunk_cbor::to_vec(frame) {
            Ok(buf) => {
                if self.tx.send((buf, sync_word)).is_err() {
                    tracing::warn!("dedup: outbound channel closed");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "dedup: encode failed");
            }
        }
    }
}
