// SPDX-License-Identifier: GPL-3.0-only

//! FutureSDR blocks unique to chirpmunk.

#![forbid(unsafe_code)]

pub mod frame_sink;
pub use frame_sink::{FrameSink, FrameSinkConfig};
