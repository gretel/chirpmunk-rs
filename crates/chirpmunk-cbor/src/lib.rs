// SPDX-License-Identifier: GPL-3.0-only

//! CBOR codecs for the chirpmunk wire protocol.
//!
//! Frame types match `gr4-lora/CBOR-SCHEMA.md`. Every wire frame is a
//! CBOR map with a text key `"type"` identifying the frame kind.
//!
//! All structures use string-keyed CBOR maps for parity with the C++
//! producer (`FrameSink::buildFrameCbor()`) and the Python consumer
//! (`lora.core.cbor_stream`, `cbor2`).

#![forbid(unsafe_code)]

use thiserror::Error;

pub mod frame;
pub mod subscribe;
pub mod tx;

pub use frame::{Carrier, Diversity, LoraFrame, Phy};
pub use subscribe::Subscribe;
pub use tx::{LoraTx, LoraTxAck};

#[derive(Debug, Error)]
pub enum Error {
    #[error("encode: {0}")]
    Encode(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("unexpected frame type: got {got:?}, expected {expected:?}")]
    UnexpectedType { got: String, expected: &'static str },
}

pub type Result<T> = core::result::Result<T, Error>;

impl<T: core::fmt::Display> From<minicbor::encode::Error<T>> for Error {
    fn from(e: minicbor::encode::Error<T>) -> Self {
        Error::Encode(e.to_string())
    }
}

impl From<minicbor::decode::Error> for Error {
    fn from(e: minicbor::decode::Error) -> Self {
        Error::Decode(e.to_string())
    }
}

/// Encode any frame type (`LoraFrame`, `Subscribe`, …) to a CBOR byte
/// vector ready for UDP transmission.
pub fn to_vec<T: Frame>(frame: &T) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(256);
    let mut enc = minicbor::Encoder::new(&mut buf);
    frame.encode_into(&mut enc)?;
    Ok(buf)
}

/// Identify the `type` field of a CBOR-encoded frame without fully
/// decoding it. Useful for dispatching subscribe / config / lora_frame /
/// lora_tx in a single UDP listener loop.
pub fn peek_type(bytes: &[u8]) -> Result<String> {
    let mut dec = minicbor::Decoder::new(bytes);
    let n = dec
        .map()?
        .ok_or_else(|| Error::Decode("not a CBOR map".into()))?;
    for _ in 0..n {
        let key = dec.str()?;
        if key == "type" {
            return Ok(dec.str()?.to_owned());
        }
        dec.skip()?;
    }
    Err(Error::MissingField("type"))
}

/// Trait every chirpmunk wire frame implements.
pub trait Frame {
    /// Frame `type` discriminator (matches `"type"` field on the wire).
    const TYPE: &'static str;

    /// Encode this frame into the supplied minicbor encoder. Implementors
    /// must include the `"type"` field as part of the encoding.
    fn encode_into<W>(
        &self,
        enc: &mut minicbor::Encoder<W>,
    ) -> core::result::Result<(), minicbor::encode::Error<W::Error>>
    where
        W: minicbor::encode::Write;
}
