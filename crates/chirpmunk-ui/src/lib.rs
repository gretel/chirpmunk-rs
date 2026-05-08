// SPDX-License-Identifier: GPL-3.0-only

//! WASM frontend for chirpmunk-trx. Connects to the FutureSDR
//! ControlPort exposed by chirpmunk-trx (default
//! `http://127.0.0.1:1337/api/fg/0/`) and to the spectrum
//! WebSocket (default `ws://127.0.0.1:9001`) and renders a
//! waterfall + flowgraph view via prophecy components.

#[cfg(target_arch = "wasm32")]
pub mod frontend;
