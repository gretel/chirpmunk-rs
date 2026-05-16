// SPDX-License-Identifier: GPL-3.0-only

//! FutureSDR blocks unique to chirpmunk.

#![forbid(unsafe_code)]

pub mod cad;
pub mod dedup;
pub mod frame_sink;
#[cfg(feature = "iio")]
pub mod iio_direct;
pub mod multi_bw;
pub mod multi_sf;
#[cfg(feature = "soapy")]
pub mod soapy_direct;
pub mod tx_dispatch;
pub use cad::{ChannelActivityDetector, Detector, StreamingDetector, default_alpha};
pub use dedup::DedupState;
pub use frame_sink::{FrameSink, FrameSinkConfig, Outbound};
#[cfg(feature = "iio")]
pub use iio_direct::{IioDirectSink, IioDirectSource, IioRxConfig, IioTxConfig, open_iio_device};
pub use multi_bw::{MAX_BW, MultiBwRx, build_multi_bw_rx};
pub use multi_sf::{ALL_SF, MultiSfRx, build_multi_sf_rx};
#[cfg(feature = "soapy")]
pub use soapy_direct::{
    SoapyDirectSink, SoapyDirectSource, SoapyRxConfig, SoapyTxConfig, open_device,
};
pub use tx_dispatch::{LbtOutcome, LbtPolicy, dispatch_lora_tx, wait_until_clear};
