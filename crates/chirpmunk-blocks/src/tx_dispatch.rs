// SPDX-License-Identifier: GPL-3.0-only

//! Dispatch a CBOR `lora_tx` request through a running Flowgraph's
//! Transmitter block. Honours `repeat` and `gap_ms`, drops the request
//! when `dry_run = true`. Optionally LBT-gates the dispatch on a
//! channel-busy flag. Returns the matching `lora_tx_ack`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chirpmunk_cbor::{LoraTx, LoraTxAck};
use futuresdr::prelude::*;
use futuresdr::runtime::Timer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LbtOutcome {
    Clear,
    Timeout,
}

/// Poll `busy` until it reads `false` or `deadline_after` elapses.
/// Sleeps `poll_interval` between reads.
pub async fn wait_until_clear(
    busy: &AtomicBool,
    deadline_after: Duration,
    poll_interval: Duration,
) -> LbtOutcome {
    let start = Instant::now();
    loop {
        if !busy.load(Ordering::Acquire) {
            return LbtOutcome::Clear;
        }
        if start.elapsed() >= deadline_after {
            return LbtOutcome::Timeout;
        }
        Timer::after(poll_interval).await;
    }
}

/// LBT policy passed to `dispatch_lora_tx`. `None` = no gating.
#[derive(Debug, Clone)]
pub struct LbtPolicy {
    pub busy: Arc<AtomicBool>,
    pub timeout: Duration,
    /// Defaults to 10 ms (matches gr4-lora `tx_worker.cpp`).
    pub poll_interval: Duration,
}

/// Send `req.payload` `req.repeat` times via the running Flowgraph's
/// transmitter `msg` message port. `gap_ms` separates repeats.
///
/// When `policy` is `Some`, each TX is gated on the channel-busy flag;
/// timeout returns `LoraTxAck { ok: false, error: "channel_busy" }`.
///
/// Errors result in `LoraTxAck { ok: false, error: "internal" }` and
/// are logged. The receipt of an ack does not imply over-the-air
/// success — only that the runtime accepted the request.
pub async fn dispatch_lora_tx(
    handle: &FlowgraphHandle,
    transmitter: BlockId,
    req: &LoraTx,
    policy: Option<&LbtPolicy>,
) -> LoraTxAck {
    let seq = req.seq.unwrap_or(0);
    let payload_len = req.payload.len();
    let repeat_req = req.repeat.unwrap_or(1).max(1);
    tracing::info!(
        seq,
        payload_len,
        repeat = repeat_req,
        dry_run = req.dry_run,
        lbt = policy.is_some(),
        "lora_tx dispatch entry"
    );
    if req.dry_run {
        return LoraTxAck::ok(seq);
    }
    if req.payload.is_empty() {
        return LoraTxAck::err(seq, "internal");
    }
    let gap = Duration::from_millis(req.gap_ms.unwrap_or(0) as u64);

    for i in 0..repeat_req {
        if let Some(policy) = policy {
            match wait_until_clear(&policy.busy, policy.timeout, policy.poll_interval).await {
                LbtOutcome::Clear => {}
                LbtOutcome::Timeout => {
                    tracing::warn!(
                        seq,
                        attempt = i + 1,
                        of = repeat_req,
                        timeout_ms = policy.timeout.as_millis() as u64,
                        "LBT timeout — NACK channel_busy"
                    );
                    return LoraTxAck::err(seq, "channel_busy");
                }
            }
        }
        let pmt = Pmt::Blob(req.payload.clone());
        match handle.post(transmitter, "msg", pmt).await {
            Ok(()) => tracing::info!(
                seq,
                attempt = i + 1,
                of = repeat_req,
                "Pmt::Blob posted to Transmitter"
            ),
            Err(e) => {
                tracing::warn!(error = %e, seq, "tx dispatch post failed");
                return LoraTxAck::err(seq, "internal");
            }
        }
        if i + 1 < repeat_req && !gap.is_zero() {
            Timer::after(gap).await;
        }
    }
    LoraTxAck::ok(seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn busy_persistent_returns_timeout() {
        let busy = Arc::new(AtomicBool::new(true));
        let outcome =
            wait_until_clear(&busy, Duration::from_millis(50), Duration::from_millis(5)).await;
        assert_eq!(outcome, LbtOutcome::Timeout);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn busy_clears_before_deadline_returns_clear() {
        let busy = Arc::new(AtomicBool::new(true));
        let busy2 = busy.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            busy2.store(false, Ordering::Release);
        });
        let outcome =
            wait_until_clear(&busy, Duration::from_millis(200), Duration::from_millis(5)).await;
        assert_eq!(outcome, LbtOutcome::Clear);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn already_clear_returns_immediately() {
        let busy = Arc::new(AtomicBool::new(false));
        let outcome = wait_until_clear(
            &busy,
            Duration::from_millis(1000),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(outcome, LbtOutcome::Clear);
    }
}
