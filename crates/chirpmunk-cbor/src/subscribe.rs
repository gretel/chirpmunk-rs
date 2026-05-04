// SPDX-License-Identifier: GPL-3.0-only

//! `subscribe` schema. Clients send this to register with the daemon.

use minicbor::decode::Decoder;
use minicbor::encode::{Encoder, Write};

use crate::{Error, Frame, Result};

/// Client-to-daemon subscription request. Empty `sync_words` means the
/// client wants all frames regardless of sync word.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subscribe {
    pub sync_words: Vec<u16>,
}

impl Frame for Subscribe {
    const TYPE: &'static str = "subscribe";

    fn encode_into<W: Write>(
        &self,
        e: &mut Encoder<W>,
    ) -> core::result::Result<(), minicbor::encode::Error<W::Error>> {
        let n: u64 = if self.sync_words.is_empty() { 1 } else { 2 };
        e.map(n)?;
        e.str("type")?.str(Self::TYPE)?;
        if !self.sync_words.is_empty() {
            e.str("sync_word")?.array(self.sync_words.len() as u64)?;
            for sw in &self.sync_words {
                e.u16(*sw)?;
            }
        }
        Ok(())
    }
}

impl Subscribe {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let mut dec = Decoder::new(bytes);
        let n = dec.map()?.ok_or(Error::Decode("expected map".into()))?;
        let mut ty: Option<String> = None;
        let mut sync_words: Vec<u16> = Vec::new();
        for _ in 0..n {
            let key = dec.str()?.to_owned();
            match key.as_str() {
                "type" => ty = Some(dec.str()?.to_owned()),
                "sync_word" => {
                    let len = dec
                        .array()?
                        .ok_or(Error::Decode("sync_word expected array".into()))?;
                    sync_words.reserve(len as usize);
                    for _ in 0..len {
                        sync_words.push(dec.u16()?);
                    }
                }
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
        Ok(Self { sync_words })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::to_vec;

    #[test]
    fn empty_filter_roundtrips() {
        let s = Subscribe::default();
        let buf = to_vec(&s).unwrap();
        let g = Subscribe::from_slice(&buf).unwrap();
        assert_eq!(s, g);
    }

    #[test]
    fn filter_roundtrips() {
        let s = Subscribe {
            sync_words: vec![0x12, 0x2B, 0x34],
        };
        let buf = to_vec(&s).unwrap();
        let g = Subscribe::from_slice(&buf).unwrap();
        assert_eq!(s, g);
    }
}
