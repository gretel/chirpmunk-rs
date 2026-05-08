// SPDX-License-Identifier: GPL-3.0-only

//! FutureSDR blocks unique to chirpmunk.

#![forbid(unsafe_code)]

pub mod cad;
pub mod dedup;
pub mod frame_sink;
pub mod multi_bw;
pub mod multi_sf;
pub mod soapy_direct;
pub mod tx_dispatch;
pub use cad::{ChannelActivityDetector, Detector, StreamingDetector, default_alpha};
pub use dedup::DedupState;
pub use frame_sink::{FrameSink, FrameSinkConfig, Outbound};
pub use multi_bw::{MAX_BW, MultiBwRx, build_multi_bw_rx};
pub use multi_sf::{ALL_SF, MultiSfRx, build_multi_sf_rx};
pub use soapy_direct::{
    SoapyDirectSink, SoapyDirectSource, SoapyRxConfig, SoapyTxConfig, open_device,
};
pub use tx_dispatch::{LbtOutcome, LbtPolicy, dispatch_lora_tx, wait_until_clear};
