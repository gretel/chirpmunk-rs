// SPDX-License-Identifier: GPL-3.0-only

//! TOML configuration loader. Mirrors `gr4-lora/apps/config.{hpp,cpp}`.
//!
//! The on-disk format follows `gr4-lora/apps/config-pluto.toml`. Sections
//! that gr4-lora's C++ loader treats as required are required here; the
//! rest are optional.

#![forbid(unsafe_code)]

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
}

pub type Result<T> = core::result::Result<T, Error>;

/// Top-level config tree. Matches `gr4-lora/apps/config-pluto.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub device: Device,
    #[serde(default)]
    pub logging: Logging,
    #[serde(flatten)]
    pub radios: std::collections::BTreeMap<String, RadioOrSection>,
    pub trx: Option<Trx>,
    pub scan: Option<Scan>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    pub driver: String,
    #[serde(default)]
    pub param: String,
    #[serde(default)]
    pub clock: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Logging {
    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_level() -> String {
    "INFO".into()
}

/// Either a `[radio_*]` table or some other top-level section.
/// `#[serde(flatten)]` on `Config` means we can't statically pattern-match
/// the radio sections, so we accept arbitrary tables here and the app
/// resolves them by name when reading `trx.radio` or `scan.radio`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RadioOrSection {
    Radio(Radio),
    Other(toml::Value),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Radio {
    pub freq: u64,
    #[serde(default)]
    pub rx_channel: Vec<u32>,
    #[serde(default)]
    pub rx_antenna: Vec<String>,
    #[serde(default)]
    pub tx_channel: u32,
    #[serde(default)]
    pub tx_antenna: String,
    #[serde(default)]
    pub rx_gain: f64,
    #[serde(default)]
    pub tx_gain: f64,
    #[serde(default)]
    pub lo_offset: i64,
    #[serde(default)]
    pub dc_offset_auto: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Trx {
    #[serde(default)]
    pub name: String,
    pub radio: String,
    pub rate: u64,
    #[serde(default)]
    pub enable_tx: bool,
    #[serde(default)]
    pub use_aa_filter: bool,
    pub transmit: Option<TrxTransmit>,
    pub receive: Option<TrxReceive>,
    pub network: Option<TrxNetwork>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrxTransmit {
    pub sf: u8,
    pub bw: u32,
    pub cr: u8,
    pub sync_word: u16,
    #[serde(default)]
    pub preamble_len: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrxReceive {
    pub bandwidths: Vec<u32>,
    #[serde(default)]
    pub chain: Vec<TrxReceiveChain>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrxReceiveChain {
    pub label: String,
    #[serde(default)]
    pub sync_word: Option<u16>,
    #[serde(default)]
    pub sf: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrxNetwork {
    #[serde(default = "default_listen")]
    pub udp_listen: String,
    pub udp_port: u16,
    #[serde(default)]
    pub status_interval: u32,
    #[serde(default)]
    pub lbt: bool,
    #[serde(default)]
    pub lbt_timeout_ms: u32,
    #[serde(default)]
    pub tx_queue_depth: u32,
}

fn default_listen() -> String {
    "127.0.0.1".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scan {
    pub radio: String,
    pub freq_start: u64,
    pub freq_stop: u64,
    pub l1_rate: u64,
    #[serde(default)]
    pub master_clock: u64,
    #[serde(default)]
    pub channel_bw: u32,
    pub network: Option<ScanNetwork>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScanNetwork {
    #[serde(default)]
    pub udp_listen: Option<String>,
    #[serde(default)]
    pub udp_port: Option<u16>,
}

impl Config {
    pub fn from_toml_str(s: &str) -> Result<Self> {
        Ok(toml::from_str(s)?)
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::from_toml_str(&s)
    }

    /// Resolve a radio section by name (e.g. `"radio_868"`).
    pub fn radio(&self, name: &str) -> Option<&Radio> {
        match self.radios.get(name)? {
            RadioOrSection::Radio(r) => Some(r),
            RadioOrSection::Other(_) => None,
        }
    }
}
