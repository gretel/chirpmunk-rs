// SPDX-License-Identifier: GPL-3.0-only

//! Dispatch a CBOR `lora_tx` request through a running Flowgraph's
//! Transmitter block. Honours `repeat` and `gap_ms`, drops the request
//! when `dry_run = true`. Returns the matching `lora_tx_ack`.

use std::time::Duration;

use chirpmunk_cbor::{LoraTx, LoraTxAck};
use futuresdr::prelude::*;
use futuresdr::runtime::Timer;

/// Send `req.payload` `req.repeat` times via the running Flowgraph's
/// transmitter `msg` message port. `gap_ms` separates repeats.
///
/// Errors result in `LoraTxAck { ok: false, error: "internal" }` and
/// are logged. The receipt of an ack does not imply over-the-air
/// success — only that the runtime accepted the request.
pub async fn dispatch_lora_tx(
    handle: &FlowgraphHandle,
    transmitter: BlockId,
    req: &LoraTx,
) -> LoraTxAck {
    let seq = req.seq.unwrap_or(0);
    if req.dry_run {
        return LoraTxAck::ok(seq);
    }
    if req.payload.is_empty() {
        return LoraTxAck::err(seq, "internal");
    }
    let repeat = req.repeat.unwrap_or(1).max(1);
    let gap = Duration::from_millis(req.gap_ms.unwrap_or(0) as u64);

    for i in 0..repeat {
        let pmt = Pmt::Blob(req.payload.clone());
        if let Err(e) = handle.post(transmitter, "msg", pmt).await {
            tracing::warn!(error = %e, seq, "tx dispatch failed");
            return LoraTxAck::err(seq, "internal");
        }
        if i + 1 < repeat && !gap.is_zero() {
            Timer::after(gap).await;
        }
    }
    LoraTxAck::ok(seq)
}
