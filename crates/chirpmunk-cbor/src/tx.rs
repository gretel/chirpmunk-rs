// SPDX-License-Identifier: GPL-3.0-only

//! `lora_tx` and `lora_tx_ack` schemas.

use minicbor::decode::Decoder;
use minicbor::encode::{Encoder, Write};

use crate::{Error, Frame, Result};

/// TX request from a client to the daemon. Mirrors the `lora_tx` map
/// in `gr4-lora/CBOR-SCHEMA.md`. Optional overrides default to `None`
/// — the daemon falls back on its startup configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoraTx {
    pub payload: Vec<u8>,
    pub seq: Option<u64>,
    pub cr: Option<u8>,
    pub sync_word: Option<u16>,
    pub preamble_len: Option<u32>,
    pub repeat: Option<u32>,
    pub gap_ms: Option<u32>,
    pub dry_run: bool,
}

impl Frame for LoraTx {
    const TYPE: &'static str = "lora_tx";

    fn encode_into<W: Write>(
        &self,
        e: &mut Encoder<W>,
    ) -> core::result::Result<(), minicbor::encode::Error<W::Error>> {
        let mut n: u64 = 2;
        for opt in [
            self.seq.is_some(),
            self.cr.is_some(),
            self.sync_word.is_some(),
            self.preamble_len.is_some(),
            self.repeat.is_some(),
            self.gap_ms.is_some(),
        ] {
            if opt {
                n += 1;
            }
        }
        if self.dry_run {
            n += 1;
        }
        e.map(n)?;
        e.str("type")?.str(Self::TYPE)?;
        e.str("payload")?.bytes(&self.payload)?;
        if let Some(v) = self.seq {
            e.str("seq")?.u64(v)?;
        }
        if let Some(v) = self.cr {
            e.str("cr")?.u8(v)?;
        }
        if let Some(v) = self.sync_word {
            e.str("sync_word")?.u16(v)?;
        }
        if let Some(v) = self.preamble_len {
            e.str("preamble_len")?.u32(v)?;
        }
        if let Some(v) = self.repeat {
            e.str("repeat")?.u32(v)?;
        }
        if let Some(v) = self.gap_ms {
            e.str("gap_ms")?.u32(v)?;
        }
        if self.dry_run {
            e.str("dry_run")?.bool(true)?;
        }
        Ok(())
    }
}

impl LoraTx {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let mut dec = Decoder::new(bytes);
        let n = dec.map()?.ok_or(Error::Decode("expected map".into()))?;
        let mut ty: Option<String> = None;
        let mut req = Self::default();
        for _ in 0..n {
            let key = dec.str()?.to_owned();
            match key.as_str() {
                "type" => ty = Some(dec.str()?.to_owned()),
                "payload" => req.payload = dec.bytes()?.to_vec(),
                "seq" => req.seq = Some(dec.u64()?),
                "cr" => req.cr = Some(dec.u8()?),
                "sync_word" => req.sync_word = Some(dec.u16()?),
                "preamble_len" => req.preamble_len = Some(dec.u32()?),
                "repeat" => req.repeat = Some(dec.u32()?),
                "gap_ms" => req.gap_ms = Some(dec.u32()?),
                "dry_run" => req.dry_run = dec.bool()?,
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
        if req.payload.is_empty() {
            return Err(Error::MissingField("payload"));
        }
        Ok(req)
    }
}

/// Reply sent by the daemon after a `lora_tx` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoraTxAck {
    pub seq: u64,
    pub ok: bool,
    pub error: Option<String>,
}

impl Frame for LoraTxAck {
    const TYPE: &'static str = "lora_tx_ack";

    fn encode_into<W: Write>(
        &self,
        e: &mut Encoder<W>,
    ) -> core::result::Result<(), minicbor::encode::Error<W::Error>> {
        let n: u64 = if self.error.is_some() { 4 } else { 3 };
        e.map(n)?;
        e.str("type")?.str(Self::TYPE)?;
        e.str("seq")?.u64(self.seq)?;
        e.str("ok")?.bool(self.ok)?;
        if let Some(err) = &self.error {
            e.str("error")?.str(err)?;
        }
        Ok(())
    }
}

impl LoraTxAck {
    pub fn ok(seq: u64) -> Self {
        Self {
            seq,
            ok: true,
            error: None,
        }
    }

    pub fn err(seq: u64, error: impl Into<String>) -> Self {
        Self {
            seq,
            ok: false,
            error: Some(error.into()),
        }
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let mut dec = Decoder::new(bytes);
        let n = dec.map()?.ok_or(Error::Decode("expected map".into()))?;
        let mut ty: Option<String> = None;
        let mut seq: Option<u64> = None;
        let mut ok: Option<bool> = None;
        let mut error: Option<String> = None;
        for _ in 0..n {
            let key = dec.str()?.to_owned();
            match key.as_str() {
                "type" => ty = Some(dec.str()?.to_owned()),
                "seq" => seq = Some(dec.u64()?),
                "ok" => ok = Some(dec.bool()?),
                "error" => error = Some(dec.str()?.to_owned()),
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
            seq: seq.ok_or(Error::MissingField("seq"))?,
            ok: ok.ok_or(Error::MissingField("ok"))?,
            error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::to_vec;

    #[test]
    fn lora_tx_minimal_roundtrip() {
        let req = LoraTx {
            payload: b"hello".to_vec(),
            ..LoraTx::default()
        };
        let buf = to_vec(&req).unwrap();
        let g = LoraTx::from_slice(&buf).unwrap();
        assert_eq!(req, g);
    }

    #[test]
    fn lora_tx_full_roundtrip() {
        let req = LoraTx {
            payload: b"data".to_vec(),
            seq: Some(7),
            cr: Some(4),
            sync_word: Some(0x12),
            preamble_len: Some(16),
            repeat: Some(3),
            gap_ms: Some(500),
            dry_run: true,
        };
        let buf = to_vec(&req).unwrap();
        let g = LoraTx::from_slice(&buf).unwrap();
        assert_eq!(req, g);
    }

    #[test]
    fn ack_roundtrips() {
        let a = LoraTxAck::ok(42);
        let buf = to_vec(&a).unwrap();
        assert_eq!(LoraTxAck::from_slice(&buf).unwrap(), a);

        let b = LoraTxAck::err(43, "channel_busy");
        let buf = to_vec(&b).unwrap();
        assert_eq!(LoraTxAck::from_slice(&buf).unwrap(), b);
    }

    #[test]
    fn missing_payload_rejected() {
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        e.map(1)
            .unwrap()
            .str("type")
            .unwrap()
            .str("lora_tx")
            .unwrap();
        let err = LoraTx::from_slice(&buf).unwrap_err();
        matches!(err, Error::MissingField("payload"));
    }
}
