// SPDX-License-Identifier: GPL-3.0-only

//! FrameSink — terminal RX block.
//!
//! Subscribes to a `Decoder`'s `out_annotated` message port (a
//! `Pmt::MapStrPmt` with `payload` + annotations), assembles a
//! `chirpmunk_cbor::LoraFrame` using the static PHY configuration, and
//! ships the encoded CBOR over an mpsc channel to a tokio task that
//! broadcasts it via `chirpmunk_udp::Server`.

use chirpmunk_cbor::{Carrier, LoraFrame, Phy};
use futuresdr::runtime::dev::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

/// Static PHY configuration broadcast as the `phy` / `carrier` blocks
/// of every emitted `lora_frame`. SNR / CFO / noise-floor are filled in
/// from message annotations when available; absent fields are encoded
/// as CBOR omissions.
#[derive(Debug, Clone)]
pub struct FrameSinkConfig {
    pub sf: u8,
    pub bw: u32,
    pub cr: u8,
    pub sync_word: u16,
    pub device: Option<String>,
    pub decode_label: Option<String>,
    pub rx_channel: Option<u32>,
}

/// `(cbor_bytes, sync_word)` pairs published to the broadcaster task.
pub type Outbound = (Vec<u8>, u16);

#[derive(Block)]
#[message_inputs(r#in)]
#[null_kernel]
pub struct FrameSink {
    cfg: FrameSinkConfig,
    seq: u64,
    tx: UnboundedSender<Outbound>,
}

impl FrameSink {
    pub fn new(cfg: FrameSinkConfig, tx: UnboundedSender<Outbound>) -> Self {
        Self { cfg, seq: 0, tx }
    }

    fn build_frame(&self, payload: Vec<u8>, snr_db: Option<f64>) -> LoraFrame {
        let payload_hash = fnv1a_64(&payload);
        let payload_len = payload.len() as u32;
        LoraFrame {
            ts: chrono::Utc::now().to_rfc3339(),
            seq: self.seq,
            phy: Phy {
                sf: self.cfg.sf,
                bw: self.cfg.bw,
                cr: self.cfg.cr,
                crc_valid: true,
                sync_word: self.cfg.sync_word,
                snr_db: snr_db.unwrap_or(0.0),
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
            },
            carrier: Carrier {
                sync_word: self.cfg.sync_word,
                sf: self.cfg.sf,
                bw: self.cfg.bw,
                cr: self.cfg.cr,
                ldro_cfg: false,
            },
            payload,
            payload_len,
            crc_valid: true,
            cr: self.cfg.cr,
            is_downchirp: false,
            id: uuid::Uuid::new_v4().to_string(),
            payload_hash,
            rx_channel: self.cfg.rx_channel,
            decode_label: self.cfg.decode_label.clone(),
            device: self.cfg.device.clone(),
        }
    }

    async fn r#in(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        pmt: Pmt,
    ) -> Result<Pmt> {
        match pmt {
            Pmt::Blob(payload) => self.emit(payload, None).await,
            Pmt::MapStrPmt(map) => {
                let mut payload = match map.get("payload") {
                    Some(Pmt::Blob(p)) => p.clone(),
                    _ => return Ok(Pmt::InvalidValue),
                };
                let has_crc = matches!(map.get("has_crc"), Some(Pmt::Bool(true)));
                if has_crc && payload.len() >= 2 {
                    payload.truncate(payload.len() - 2);
                }
                let snr = map.get("snr_db").and_then(|p| match p {
                    Pmt::F32(v) => Some(*v as f64),
                    Pmt::F64(v) => Some(*v),
                    _ => None,
                });
                self.emit(payload, snr).await
            }
            Pmt::Finished => {
                io.finished = true;
                Ok(Pmt::Ok)
            }
            _ => Ok(Pmt::InvalidValue),
        }
    }

    async fn emit(&mut self, payload: Vec<u8>, snr_db: Option<f64>) -> Result<Pmt> {
        self.seq = self.seq.wrapping_add(1);
        let frame = self.build_frame(payload, snr_db);
        let buf = chirpmunk_cbor::to_vec(&frame)
            .map_err(|e| futuresdr::runtime::Error::RuntimeError(e.to_string()))?;
        if self.tx.send((buf, self.cfg.sync_word)).is_err() {
            tracing::warn!("frame_sink: broadcaster channel closed");
        }
        Ok(Pmt::Ok)
    }
}

/// FNV-1a 64-bit hash; matches gr4-lora `FrameSink::payload_hash`.
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_known_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x85944171f73967e8);
    }
}
