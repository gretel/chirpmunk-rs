// SPDX-License-Identifier: GPL-3.0-only

//! FutureSDR blocks unique to chirpmunk.

#![forbid(unsafe_code)]

pub mod frame_sink;
pub mod multi_sf;
pub use frame_sink::{FrameSink, FrameSinkConfig, Outbound};
pub use multi_sf::{ALL_SF, MultiSfRx, build_multi_sf_rx};
