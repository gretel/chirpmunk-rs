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

    fn build_frame(&self, payload: Vec<u8>, telemetry: Telemetry) -> LoraFrame {
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
                snr_db: telemetry.snr_db.unwrap_or(0.0),
                noise_floor_db: telemetry.noise_floor_db,
                peak_db: telemetry.peak_db,
                snr_db_td: telemetry.snr_db_td,
                channel_freq: telemetry.channel_freq,
                decode_bw: telemetry.decode_bw,
                cfo_int: telemetry.cfo_int,
                cfo_frac: telemetry.cfo_frac,
                sfo_hat: telemetry.sfo_hat,
                sample_rate: telemetry.sample_rate,
                frequency_corrected: telemetry.frequency_corrected,
                ppm_error: telemetry.ppm_error,
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
            Pmt::Blob(payload) => self.emit(payload, Telemetry::default()).await,
            Pmt::MapStrPmt(map) => {
                let mut payload = match map.get("payload") {
                    Some(Pmt::Blob(p)) => p.clone(),
                    _ => return Ok(Pmt::InvalidValue),
                };
                let has_crc = match map.get("has_crc") {
                    Some(Pmt::Bool(b)) => *b,
                    _ => false,
                };
                if has_crc && payload.len() >= 2 {
                    payload.truncate(payload.len() - 2);
                }
                let telemetry = Telemetry::from_map(&map);
                self.emit(payload, telemetry).await
            }
            Pmt::Finished => {
                io.finished = true;
                Ok(Pmt::Ok)
            }
            _ => Ok(Pmt::InvalidValue),
        }
    }

    async fn emit(&mut self, payload: Vec<u8>, telemetry: Telemetry) -> Result<Pmt> {
        self.seq = self.seq.wrapping_add(1);
        let frame = self.build_frame(payload, telemetry);
        tracing::info!(
            seq = frame.seq,
            sf = frame.phy.sf,
            bw = frame.phy.bw,
            sync = format_args!("0x{:02x}", frame.phy.sync_word),
            cr = frame.cr,
            crc = if frame.crc_valid { "ok" } else { "bad" },
            len = frame.payload_len,
            snr_db = frame.phy.snr_db,
            label = frame.decode_label.as_deref().unwrap_or(""),
            "frame"
        );
        let buf = chirpmunk_cbor::to_vec(&frame)
            .map_err(|e| futuresdr::runtime::Error::RuntimeError(e.to_string()))?;
        if self.tx.send((buf, self.cfg.sync_word)).is_err() {
            tracing::warn!("frame_sink: broadcaster channel closed");
        }
        Ok(Pmt::Ok)
    }
}

/// Optional PHY measurements harvested from the upstream
/// `MapStrPmt`. Missing keys remain `None` and are omitted in the
/// emitted CBOR `lora_frame`.
#[derive(Debug, Default, Clone, Copy)]
struct Telemetry {
    snr_db: Option<f64>,
    noise_floor_db: Option<f64>,
    peak_db: Option<f64>,
    snr_db_td: Option<f64>,
    channel_freq: Option<f64>,
    decode_bw: Option<f64>,
    cfo_int: Option<f64>,
    cfo_frac: Option<f64>,
    sfo_hat: Option<f64>,
    sample_rate: Option<f64>,
    frequency_corrected: Option<f64>,
    ppm_error: Option<f64>,
}

impl Telemetry {
    fn from_map(map: &std::collections::HashMap<String, Pmt>) -> Self {
        let f = |k: &str| -> Option<f64> {
            map.get(k)
                .and_then(|p| match p {
                    Pmt::F32(v) => Some(*v as f64),
                    Pmt::F64(v) => Some(*v),
                    Pmt::U64(v) => Some(*v as f64),
                    Pmt::Usize(v) => Some(*v as f64),
                    Pmt::Isize(v) => Some(*v as f64),
                    _ => None,
                })
                .filter(|v| v.is_finite())
        };
        // Source key list per chirpmunk-phy frame_sync emissions; the
        // schema-named keys (`snr_db`, `noise_floor_db`) are accepted
        // as fallbacks so future PHY upgrades that switch to the
        // dB-explicit name still propagate without code change.
        Self {
            snr_db: f("snr").or_else(|| f("snr_db")),
            noise_floor_db: f("noise_floor").or_else(|| f("noise_floor_db")),
            peak_db: f("peak_db"),
            snr_db_td: f("snr_db_td"),
            channel_freq: f("channel_freq"),
            decode_bw: f("decode_bw"),
            cfo_int: f("cfo_int"),
            cfo_frac: f("cfo_frac"),
            sfo_hat: f("sfo_hat"),
            sample_rate: f("sample_rate"),
            frequency_corrected: f("frequency_corrected"),
            ppm_error: f("ppm_error"),
        }
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
