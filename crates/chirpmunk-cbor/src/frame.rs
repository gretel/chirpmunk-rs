// SPDX-License-Identifier: GPL-3.0-only

//! `lora_frame` schema. Decoded LoRa RX frames produced by FrameSink.

use minicbor::data::Type as CborType;
use minicbor::decode::Decoder;
use minicbor::encode::{Encoder, Write};

use crate::{Error, Frame, Result};

/// PHY-layer measurements per decoded frame. Required fields plus the
/// most common optionals; unknown keys are skipped on decode.
#[derive(Debug, Clone, PartialEq)]
pub struct Phy {
    pub sf: u8,
    pub bw: u32,
    pub cr: u8,
    pub crc_valid: bool,
    pub sync_word: u16,
    pub snr_db: f64,
    pub noise_floor_db: Option<f64>,
    pub peak_db: Option<f64>,
    pub snr_db_td: Option<f64>,
    pub channel_freq: Option<f64>,
    pub decode_bw: Option<f64>,
    pub cfo_int: Option<f64>,
    pub cfo_frac: Option<f64>,
    pub sfo_hat: Option<f64>,
    pub sample_rate: Option<f64>,
    pub frequency_corrected: Option<f64>,
    pub ppm_error: Option<f64>,
}

/// Carrier configuration the decoder used.
#[derive(Debug, Clone, PartialEq)]
pub struct Carrier {
    pub sync_word: u16,
    pub sf: u8,
    pub bw: u32,
    pub cr: u8,
    pub ldro_cfg: bool,
}

/// `lora_frame` — a single decoded LoRa packet. Mirrors
/// `FrameSink::buildFrameCbor()` in gr4-lora.
#[derive(Debug, Clone, PartialEq)]
pub struct LoraFrame {
    pub ts: String,
    pub seq: u64,
    pub phy: Phy,
    pub carrier: Carrier,
    pub payload: Vec<u8>,
    pub payload_len: u32,
    pub crc_valid: bool,
    pub cr: u8,
    pub is_downchirp: bool,
    pub id: String,
    pub payload_hash: u64,
    pub rx_channel: Option<u32>,
    pub decode_label: Option<String>,
    pub device: Option<String>,
}

impl Frame for LoraFrame {
    const TYPE: &'static str = "lora_frame";

    fn encode_into<W: Write>(
        &self,
        e: &mut Encoder<W>,
    ) -> core::result::Result<(), minicbor::encode::Error<W::Error>> {
        let mut n: u64 = 12;
        if self.rx_channel.is_some() {
            n += 1;
        }
        if self.decode_label.is_some() {
            n += 1;
        }
        if self.device.is_some() {
            n += 1;
        }
        e.map(n)?;
        e.str("type")?.str(Self::TYPE)?;
        e.str("ts")?.str(&self.ts)?;
        e.str("seq")?.u64(self.seq)?;
        e.str("phy")?;
        encode_phy(e, &self.phy)?;
        e.str("carrier")?;
        encode_carrier(e, &self.carrier)?;
        e.str("payload")?.bytes(&self.payload)?;
        e.str("payload_len")?.u32(self.payload_len)?;
        e.str("crc_valid")?.bool(self.crc_valid)?;
        e.str("cr")?.u8(self.cr)?;
        e.str("is_downchirp")?.bool(self.is_downchirp)?;
        e.str("id")?.str(&self.id)?;
        e.str("payload_hash")?.u64(self.payload_hash)?;
        if let Some(rx) = self.rx_channel {
            e.str("rx_channel")?.u32(rx)?;
        }
        if let Some(label) = &self.decode_label {
            e.str("decode_label")?.str(label)?;
        }
        if let Some(dev) = &self.device {
            e.str("device")?.str(dev)?;
        }
        Ok(())
    }
}

impl LoraFrame {
    /// Decode a `lora_frame` from raw CBOR bytes. Returns
    /// [`Error::UnexpectedType`] if the `type` field does not match.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let mut dec = Decoder::new(bytes);
        let n = dec.map()?.ok_or(Error::Decode("expected map".into()))?;

        let mut ty: Option<String> = None;
        let mut ts: Option<String> = None;
        let mut seq: Option<u64> = None;
        let mut phy: Option<Phy> = None;
        let mut carrier: Option<Carrier> = None;
        let mut payload: Option<Vec<u8>> = None;
        let mut payload_len: Option<u32> = None;
        let mut crc_valid: Option<bool> = None;
        let mut cr: Option<u8> = None;
        let mut is_downchirp: Option<bool> = None;
        let mut id: Option<String> = None;
        let mut payload_hash: Option<u64> = None;
        let mut rx_channel: Option<u32> = None;
        let mut decode_label: Option<String> = None;
        let mut device: Option<String> = None;

        for _ in 0..n {
            let key = dec.str()?.to_owned();
            match key.as_str() {
                "type" => ty = Some(dec.str()?.to_owned()),
                "ts" => ts = Some(dec.str()?.to_owned()),
                "seq" => seq = Some(dec.u64()?),
                "phy" => phy = Some(decode_phy(&mut dec)?),
                "carrier" => carrier = Some(decode_carrier(&mut dec)?),
                "payload" => payload = Some(dec.bytes()?.to_vec()),
                "payload_len" => payload_len = Some(dec.u32()?),
                "crc_valid" => crc_valid = Some(dec.bool()?),
                "cr" => cr = Some(dec.u8()?),
                "is_downchirp" => is_downchirp = Some(dec.bool()?),
                "id" => id = Some(dec.str()?.to_owned()),
                "payload_hash" => payload_hash = Some(dec.u64()?),
                "rx_channel" => rx_channel = Some(dec.u32()?),
                "decode_label" => decode_label = Some(dec.str()?.to_owned()),
                "device" => device = Some(dec.str()?.to_owned()),
                _ => dec.skip()?,
            }
        }

        let ty = ty.ok_or(Error::MissingField("type"))?;
        if ty != Self::TYPE {
            return Err(Error::UnexpectedType {
                got: ty,
                expected: Self::TYPE,
            });
        }

        Ok(Self {
            ts: ts.ok_or(Error::MissingField("ts"))?,
            seq: seq.ok_or(Error::MissingField("seq"))?,
            phy: phy.ok_or(Error::MissingField("phy"))?,
            carrier: carrier.ok_or(Error::MissingField("carrier"))?,
            payload: payload.ok_or(Error::MissingField("payload"))?,
            payload_len: payload_len.ok_or(Error::MissingField("payload_len"))?,
            crc_valid: crc_valid.ok_or(Error::MissingField("crc_valid"))?,
            cr: cr.ok_or(Error::MissingField("cr"))?,
            is_downchirp: is_downchirp.ok_or(Error::MissingField("is_downchirp"))?,
            id: id.ok_or(Error::MissingField("id"))?,
            payload_hash: payload_hash.ok_or(Error::MissingField("payload_hash"))?,
            rx_channel,
            decode_label,
            device,
        })
    }
}

fn encode_phy<W: Write>(
    e: &mut Encoder<W>,
    p: &Phy,
) -> core::result::Result<(), minicbor::encode::Error<W::Error>> {
    let mut n: u64 = 6;
    let opts: &[(&str, bool)] = &[
        ("noise_floor_db", p.noise_floor_db.is_some()),
        ("peak_db", p.peak_db.is_some()),
        ("snr_db_td", p.snr_db_td.is_some()),
        ("channel_freq", p.channel_freq.is_some()),
        ("decode_bw", p.decode_bw.is_some()),
        ("cfo_int", p.cfo_int.is_some()),
        ("cfo_frac", p.cfo_frac.is_some()),
        ("sfo_hat", p.sfo_hat.is_some()),
        ("sample_rate", p.sample_rate.is_some()),
        ("frequency_corrected", p.frequency_corrected.is_some()),
        ("ppm_error", p.ppm_error.is_some()),
    ];
    for (_, present) in opts {
        if *present {
            n += 1;
        }
    }
    e.map(n)?;
    e.str("sf")?.u8(p.sf)?;
    e.str("bw")?.u32(p.bw)?;
    e.str("cr")?.u8(p.cr)?;
    e.str("crc_valid")?.bool(p.crc_valid)?;
    e.str("sync_word")?.u16(p.sync_word)?;
    e.str("snr_db")?.f64(p.snr_db)?;
    if let Some(v) = p.noise_floor_db {
        e.str("noise_floor_db")?.f64(v)?;
    }
    if let Some(v) = p.peak_db {
        e.str("peak_db")?.f64(v)?;
    }
    if let Some(v) = p.snr_db_td {
        e.str("snr_db_td")?.f64(v)?;
    }
    if let Some(v) = p.channel_freq {
        e.str("channel_freq")?.f64(v)?;
    }
    if let Some(v) = p.decode_bw {
        e.str("decode_bw")?.f64(v)?;
    }
    if let Some(v) = p.cfo_int {
        e.str("cfo_int")?.f64(v)?;
    }
    if let Some(v) = p.cfo_frac {
        e.str("cfo_frac")?.f64(v)?;
    }
    if let Some(v) = p.sfo_hat {
        e.str("sfo_hat")?.f64(v)?;
    }
    if let Some(v) = p.sample_rate {
        e.str("sample_rate")?.f64(v)?;
    }
    if let Some(v) = p.frequency_corrected {
        e.str("frequency_corrected")?.f64(v)?;
    }
    if let Some(v) = p.ppm_error {
        e.str("ppm_error")?.f64(v)?;
    }
    Ok(())
}

fn decode_phy(dec: &mut Decoder) -> Result<Phy> {
    let n = dec.map()?.ok_or(Error::Decode("phy expected map".into()))?;
    let mut sf = None;
    let mut bw = None;
    let mut cr = None;
    let mut crc_valid = None;
    let mut sync_word = None;
    let mut snr_db = None;
    let mut noise_floor_db = None;
    let mut peak_db = None;
    let mut snr_db_td = None;
    let mut channel_freq = None;
    let mut decode_bw = None;
    let mut cfo_int = None;
    let mut cfo_frac = None;
    let mut sfo_hat = None;
    let mut sample_rate = None;
    let mut frequency_corrected = None;
    let mut ppm_error = None;
    for _ in 0..n {
        let key = dec.str()?.to_owned();
        match key.as_str() {
            "sf" => sf = Some(dec.u8()?),
            "bw" => bw = Some(dec.u32()?),
            "cr" => cr = Some(dec.u8()?),
            "crc_valid" => crc_valid = Some(dec.bool()?),
            "sync_word" => sync_word = Some(dec.u16()?),
            "snr_db" => snr_db = Some(read_float(dec)?),
            "noise_floor_db" => noise_floor_db = Some(read_float(dec)?),
            "peak_db" => peak_db = Some(read_float(dec)?),
            "snr_db_td" => snr_db_td = Some(read_float(dec)?),
            "channel_freq" => channel_freq = Some(read_float(dec)?),
            "decode_bw" => decode_bw = Some(read_float(dec)?),
            "cfo_int" => cfo_int = Some(read_float(dec)?),
            "cfo_frac" => cfo_frac = Some(read_float(dec)?),
            "sfo_hat" => sfo_hat = Some(read_float(dec)?),
            "sample_rate" => sample_rate = Some(read_float(dec)?),
            "frequency_corrected" => frequency_corrected = Some(read_float(dec)?),
            "ppm_error" => ppm_error = Some(read_float(dec)?),
            _ => dec.skip()?,
        }
    }
    Ok(Phy {
        sf: sf.ok_or(Error::MissingField("phy.sf"))?,
        bw: bw.ok_or(Error::MissingField("phy.bw"))?,
        cr: cr.ok_or(Error::MissingField("phy.cr"))?,
        crc_valid: crc_valid.ok_or(Error::MissingField("phy.crc_valid"))?,
        sync_word: sync_word.ok_or(Error::MissingField("phy.sync_word"))?,
        snr_db: snr_db.ok_or(Error::MissingField("phy.snr_db"))?,
        noise_floor_db,
        peak_db,
        snr_db_td,
        channel_freq,
        decode_bw,
        cfo_int,
        cfo_frac,
        sfo_hat,
        sample_rate,
        frequency_corrected,
        ppm_error,
    })
}

fn encode_carrier<W: Write>(
    e: &mut Encoder<W>,
    c: &Carrier,
) -> core::result::Result<(), minicbor::encode::Error<W::Error>> {
    e.map(5)?;
    e.str("sync_word")?.u16(c.sync_word)?;
    e.str("sf")?.u8(c.sf)?;
    e.str("bw")?.u32(c.bw)?;
    e.str("cr")?.u8(c.cr)?;
    e.str("ldro_cfg")?.bool(c.ldro_cfg)?;
    Ok(())
}

fn decode_carrier(dec: &mut Decoder) -> Result<Carrier> {
    let n = dec
        .map()?
        .ok_or(Error::Decode("carrier expected map".into()))?;
    let mut sync_word = None;
    let mut sf = None;
    let mut bw = None;
    let mut cr = None;
    let mut ldro_cfg = None;
    for _ in 0..n {
        let key = dec.str()?.to_owned();
        match key.as_str() {
            "sync_word" => sync_word = Some(dec.u16()?),
            "sf" => sf = Some(dec.u8()?),
            "bw" => bw = Some(dec.u32()?),
            "cr" => cr = Some(dec.u8()?),
            "ldro_cfg" => ldro_cfg = Some(dec.bool()?),
            _ => dec.skip()?,
        }
    }
    Ok(Carrier {
        sync_word: sync_word.ok_or(Error::MissingField("carrier.sync_word"))?,
        sf: sf.ok_or(Error::MissingField("carrier.sf"))?,
        bw: bw.ok_or(Error::MissingField("carrier.bw"))?,
        cr: cr.ok_or(Error::MissingField("carrier.cr"))?,
        ldro_cfg: ldro_cfg.ok_or(Error::MissingField("carrier.ldro_cfg"))?,
    })
}

/// Decode any CBOR float-like number into `f64`. Accepts half / single /
/// double precision (gr4-lora emits f64 but we accept narrower forms for
/// future-proofing against producer changes).
fn read_float(dec: &mut Decoder) -> Result<f64> {
    match dec.datatype()? {
        CborType::F16 | CborType::F32 => Ok(dec.f32()? as f64),
        CborType::F64 => Ok(dec.f64()?),
        other => Err(Error::Decode(format!("expected float, got {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{peek_type, to_vec};

    fn sample_frame() -> LoraFrame {
        LoraFrame {
            ts: "2026-02-17T10:00:00Z".into(),
            seq: 1,
            phy: Phy {
                sf: 8,
                bw: 62500,
                cr: 4,
                crc_valid: true,
                sync_word: 0x12,
                snr_db: 12.3,
                noise_floor_db: Some(-42.1),
                peak_db: None,
                snr_db_td: None,
                channel_freq: Some(869618000.0),
                decode_bw: None,
                cfo_int: None,
                cfo_frac: None,
                sfo_hat: None,
                sample_rate: None,
                frequency_corrected: None,
                ppm_error: None,
            },
            carrier: Carrier {
                sync_word: 0x12,
                sf: 8,
                bw: 62500,
                cr: 4,
                ldro_cfg: false,
            },
            payload: b"Hello".to_vec(),
            payload_len: 5,
            crc_valid: true,
            cr: 4,
            is_downchirp: false,
            id: "550e8400-e29b-41d4-a716-446655440000".into(),
            payload_hash: 12345678901234,
            rx_channel: Some(0),
            decode_label: Some("meshcore".into()),
            device: None,
        }
    }

    #[test]
    fn roundtrip_idempotent() {
        let f = sample_frame();
        let a = to_vec(&f).expect("encode");
        let g = LoraFrame::from_slice(&a).expect("decode");
        assert_eq!(f, g);
        let b = to_vec(&g).expect("re-encode");
        assert_eq!(a, b);
    }

    #[test]
    fn peek_type_round() {
        let f = sample_frame();
        let buf = to_vec(&f).unwrap();
        assert_eq!(peek_type(&buf).unwrap(), "lora_frame");
    }

    #[test]
    fn rejects_wrong_type() {
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        e.map(1)
            .unwrap()
            .str("type")
            .unwrap()
            .str("subscribe")
            .unwrap();
        let err = LoraFrame::from_slice(&buf).unwrap_err();
        match err {
            Error::UnexpectedType { got, expected } => {
                assert_eq!(got, "subscribe");
                assert_eq!(expected, "lora_frame");
            }
            other => panic!("wrong error variant: {other:?}"),
        }
    }
}
