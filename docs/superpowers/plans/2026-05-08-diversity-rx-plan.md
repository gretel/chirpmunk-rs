# Diversity RX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Use both B210/B220 RX channels for parallel decoding. Independent decode per channel; FrameSink dedups identical payloads inside `dedup_window_ms` and emits one consolidated `lora_frame` with `phy.diversity` metadata.

**Architecture:** `SoapyDirectSource` exposes 2 outputs (out0, out1) — currently merges. Each output feeds an independent multi-BW × multi-SF stack (Task in this plan or Spec 2's grid). All FrameSinks share one outbound mpsc; the FrameSink-side dedup keys `(sha256(payload), sync, sf, bw)` within a window, merging diversity metadata.

**Tech Stack:** Rust 2024, FutureSDR runtime, `soapysdr` crate, `sha2`, existing chirpmunk-blocks (FrameSink, multi_sf, multi_bw).

---

## File structure

| File | Status | Responsibility |
|---|---|---|
| `crates/chirpmunk-blocks/src/soapy_direct.rs` | MODIFY | `SoapyDirectSource` gains 2 stream outputs (`out0`, `out1`). New `SoapyRxConfig::channels: Vec<usize>` (default `[0]`). Channel 1 routed to `NullSink` when `channels = [0]`. |
| `crates/chirpmunk-blocks/src/frame_sink.rs` | MODIFY | Add `dedup_window_ms` to FrameSinkConfig. State map `(payload_hash, sync, sf, bw) → DedupEntry { antennas: Vec<u8>, snr_db_per_ant: Vec<f64>, scheduled_emit_at: Instant }`. Emit only after window expiry; merge new arrivals. |
| `crates/chirpmunk-config/src/lib.rs` | MODIFY | `TrxReceive { rx_chains: Option<Vec<u8>>, rx_antennas: Option<Vec<String>>, rx_gains: Option<Vec<f64>>, dedup_window_ms: Option<u32> }`. |
| `apps/chirpmunk-trx/src/main.rs` | MODIFY | Wire two parallel RX stacks when `rx_chains.len() == 2`. Reject `rx_chains.len() > 1` when driver = pluto. |
| `crates/chirpmunk-blocks/tests/dedup_window.rs` | **CREATE** | Unit test: two identical payloads in 30 ms → one emitted; in `dedup_window_ms = 0` → two emitted. |

---

## Task 1: Config — rx_chains, rx_antennas, rx_gains, dedup_window_ms

**Files:**
- Modify: `crates/chirpmunk-config/src/lib.rs`
- Test: `crates/chirpmunk-config/tests/diversity_fields.rs` (CREATE)

- [ ] **Step 1.1: Write tests**

`crates/chirpmunk-config/tests/diversity_fields.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only

use chirpmunk_config::Config;

const TOML_DIV: &str = r#"
[chirpmunk]
loopback = true
[logging]
level = "INFO"
[trx]
radio = "default"
[trx.transmit]
sf = 7
bw = 125000
cr = 4
sync_word = 0x12
preamble_len = 8
[trx.receive]
rx_chains = [0, 1]
rx_antennas = ["TX/RX", "RX2"]
rx_gains = [50.0, 50.0]
dedup_window_ms = 80
[trx.network]
udp_listen = "127.0.0.1"
udp_port = 5556
"#;

#[test]
fn parses_diversity_fields() {
    let cfg: Config = toml::from_str(TOML_DIV).unwrap();
    let rx = cfg.trx.unwrap().receive.unwrap();
    assert_eq!(rx.rx_chains, Some(vec![0, 1]));
    assert_eq!(
        rx.rx_antennas,
        Some(vec!["TX/RX".to_string(), "RX2".to_string()])
    );
    assert_eq!(rx.rx_gains, Some(vec![50.0, 50.0]));
    assert_eq!(rx.dedup_window_ms, Some(80));
}
```

- [ ] **Step 1.2: Run failing**

```sh
cargo test -p chirpmunk-config --test diversity_fields 2>&1 | tail -10
```

- [ ] **Step 1.3: Add fields to TrxReceive**

```rust
#[serde(default)]
pub rx_chains: Option<Vec<u8>>,
#[serde(default)]
pub rx_antennas: Option<Vec<String>>,
#[serde(default)]
pub rx_gains: Option<Vec<f64>>,
#[serde(default)]
pub dedup_window_ms: Option<u32>,
```

- [ ] **Step 1.4: Commit**

```sh
git add crates/chirpmunk-config/src/lib.rs crates/chirpmunk-config/tests/diversity_fields.rs
git commit -m "feat(config): rx_chains + rx_antennas + rx_gains + dedup_window_ms"
```

---

## Task 2: SoapyDirectSource — expose 2 channel outputs

**Files:**
- Modify: `crates/chirpmunk-blocks/src/soapy_direct.rs`

- [ ] **Step 2.1: Inspect current SoapyDirectSource**

```sh
sed -n '1,60p' /Users/tom/src/uhd/chirpmunk/crates/chirpmunk-blocks/src/soapy_direct.rs
```

Confirm: it currently opens `&[0, 1]` channels, runs one streamer with both channels, but only forwards channel 0 to its single stream output port.

- [ ] **Step 2.2: Add `channels: Vec<usize>` to SoapyRxConfig**

In `SoapyRxConfig`:

```rust
pub channels: Vec<usize>,
```

(`Vec<usize>` defaulting to `[0]` via builder. Replace the existing scalar `channel: usize` field by removing it AFTER all call sites in main.rs are updated.)

- [ ] **Step 2.3: Add a second PortOut**

In the `#[derive(Block)]` struct fields:

```rust
#[output]
pub out0: PortOut<Complex32>,
#[output]
pub out1: PortOut<Complex32>,
```

The `work` loop calls `dev.read_streams(&mut [chan0_buf, chan1_buf], ...)` (or whatever the current readStream API takes for multi-channel). Whatever the present API is, both buffers must be drained per call. If `channels.len() == 1`, fill `out1` with zeroed samples (or simply leave as 0 — the consumer's NullSink will discard them).

NB: B200/B210 enforces channel symmetry — opening `&[0]` only on a 2-RX device is rejected. So we always open `&[0, 1]` and route channel-1 IQ either to `out1` (when `rx_chains = [0,1]`) or to `out1` writes that go nowhere.

- [ ] **Step 2.4: Run existing tests**

```sh
cargo test -p chirpmunk-blocks 2>&1 | tail -10
cargo build --workspace 2>&1 | tail -5
```

If main.rs breaks because the SoapyRxConfig API changed, fix it: pass `channels: vec![0]` (single-antenna preserves prior behaviour) and use `rx_source.out0` (the new name) in the `connect!` line.

- [ ] **Step 2.5: Commit**

```sh
git add crates/chirpmunk-blocks/src/soapy_direct.rs apps/chirpmunk-trx/src/main.rs
git commit -m "feat(blocks): SoapyDirectSource exposes out0/out1 + channels: Vec"
```

---

## Task 3: FrameSink dedup — windowed emission with diversity metadata

**Files:**
- Modify: `crates/chirpmunk-blocks/src/frame_sink.rs`

- [ ] **Step 3.1: Inspect FrameSink**

```sh
cat /Users/tom/src/uhd/chirpmunk/crates/chirpmunk-blocks/src/frame_sink.rs
```

Note the `Telemetry::from_map` shape and how the CBOR is built. We will:

1. Compute `payload_hash = sha256(payload)`.
2. Look up `(payload_hash, sync, sf, bw)` in a `HashMap<DedupKey, DedupEntry>`.
3. If present and not yet emitted: append antenna index + snr to it, update `snr_db_max`. Reset emit-deadline.
4. If absent: insert with `scheduled_emit_at = now + dedup_window_ms`. Spawn a tokio task that sleeps until `scheduled_emit_at`, then atomically removes the entry and emits the consolidated CBOR.

Keep it simple: per-key tokio sleep is fine; max active entries at any moment is bounded by frame rate × dedup_window_ms ≈ tens.

- [ ] **Step 3.2: Add sha2 dep**

Workspace `Cargo.toml`:

```toml
[workspace.dependencies]
sha2 = "0.10"
```

`crates/chirpmunk-blocks/Cargo.toml`:

```toml
sha2 = { workspace = true }
```

- [ ] **Step 3.3: Write tests**

`crates/chirpmunk-blocks/tests/dedup_window.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only

use chirpmunk_blocks::{FrameSink, FrameSinkConfig};
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

// (Construct a FrameSink with `dedup_window_ms = 30`, hand-feed two
// PMTs/maps that decode to the same payload+sync+sf+bw within 10 ms,
// then advance time and assert one mpsc message emitted with
// `phy.diversity.antennas == [0,1]`.)

#[tokio::test]
async fn dedup_within_window_emits_once() {
    // (See: read frame_sink.rs to discover its public test surface.
    //  The unit test must drive whatever ingress path FrameSink exposes
    //  for synthetic input.  If none exists, factor a `dedup_layer`
    //  helper out of the block kernel into a pub(crate) function
    //  that this test can call directly.)
}

#[tokio::test]
async fn dedup_disabled_when_window_zero_emits_twice() {
    // (Same setup but dedup_window_ms = 0.)
}
```

- [ ] **Step 3.4: Implement dedup**

In `frame_sink.rs`, add the dedup state inside the `FrameSink` struct:

```rust
struct DedupEntry {
    antennas: Vec<u8>,
    snr_db_per_ant: Vec<f64>,
    base_cbor_template: Vec<u8>,  // pre-built CBOR with placeholder fields
    sync_word: u16,
}

type DedupKey = ([u8; 32], u16, u8, u32);  // (payload_hash, sync, sf, bw)

struct DedupState {
    entries: tokio::sync::Mutex<HashMap<DedupKey, DedupEntry>>,
}
```

Modify the work loop / message handler to:

1. Hash the payload.
2. `mu.lock()` the entries, look up key.
3. If present: append, drop lock.
4. If absent: insert, drop lock, spawn `tokio::spawn(async move { sleep(window); /* lock, remove, build CBOR, send */ })`.

Send via the existing `Outbound` mpsc once.

- [ ] **Step 3.5: Run tests**

```sh
cargo test -p chirpmunk-blocks --test dedup_window 2>&1 | tail -20
```

- [ ] **Step 3.6: Commit**

```sh
git add crates/chirpmunk-blocks/src/frame_sink.rs crates/chirpmunk-blocks/tests/dedup_window.rs crates/chirpmunk-blocks/Cargo.toml Cargo.toml
git commit -m "feat(blocks): FrameSink dedup with phy.diversity metadata"
```

---

## Task 4: Wire 2-channel RX into chirpmunk-trx

**Files:**
- Modify: `apps/chirpmunk-trx/src/main.rs`

- [ ] **Step 4.1: Pluto rejection check**

In main.rs after parsing config: if `cfg.device.driver == "plutoPAPR"` and `rx_chains.len() > 1`, `bail!("PlutoSDR has 1 RX channel; rx_chains must be [0]")`.

- [ ] **Step 4.2: Build two RX stacks**

When `rx_chains.len() == 2`, build two parallel multi_bw_rx (or multi_sf_rx for back-compat) stacks, one rooted at `rx_source.out0` and one at `rx_source.out1`. Both stacks share the same Outbound mpsc and FrameSink (since FrameSink does dedup).

- [ ] **Step 4.3: Tag per-frame antenna index**

Each multi_sf chain inside the diversity branch needs to know its antenna index (0 or 1) so FrameSink can record it. Cleanest: pass `chain_antenna: u8` into `build_multi_sf_rx`'s `cfg_template.rx_channel`. The FrameSink already has `rx_channel: Option<u8>`.

In the main.rs call:

```rust
let mut cfg0 = cfg_sink_template.clone();
cfg0.rx_channel = Some(0);
let mut cfg1 = cfg_sink_template.clone();
cfg1.rx_channel = Some(1);

let _ = build_multi_bw_rx(&mut fg, /* ... */, cfg0, sender.clone())?;
let _ = build_multi_bw_rx(&mut fg, /* ... */, cfg1, sender.clone())?;
```

- [ ] **Step 4.4: Build + smoke**

```sh
cargo build -p chirpmunk-trx 2>&1 | tail -10
cargo test -p chirpmunk-trx --test daemon_loopback 2>&1 | tail -10
```

Single-RX case (`rx_chains = [0]`) must continue to pass.

- [ ] **Step 4.5: Commit**

```sh
git add apps/chirpmunk-trx/src/main.rs
git commit -m "feat(trx): two-channel RX stacks share dedup FrameSink"
```

---

## Task 5: Validation gates

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All green. Hardware verification (B210, two antennas, MeshCore companion TX) is out of scope — covered by `hw-testing` skill on the next session.

---

## Self-review

- Spec coverage:
  - §"Components" 1 (SoapyDirectSource extension): Task 2 ✅.
  - §"Components" 2 (parallel multi-SF stacks): Task 4 ✅.
  - §"Components" 3 (frame dedup): Task 3 ✅.
  - §"Components" 4 (config): Task 1 ✅.
  - §"Components" 5 (hardware constraints / pluto reject): Task 4 step 4.1 ✅.

- Placeholder scan:
  - Task 3 step 3.3 has skeleton tests with comments "construct a FrameSink with…". Implementer must read `frame_sink.rs` first to expose either a `pub fn ingest_for_test` or a public block-construction path. Acceptable as long as that exposure is added in step 3.4.

- Type consistency:
  - `DedupKey = ([u8; 32], u16, u8, u32)` — used only inside `frame_sink.rs`.
  - `phy.diversity` map keys: `antennas: Vec<u8>`, `snr_db_max: f64`, `snr_db_per_ant: Vec<f64>`. Match spec §"Components" 3.

- Open issue:
  - Sharing one FrameSink instance between two stacks requires the FrameSink to be thread-safe across its work loops. FutureSDR blocks own their state; if the multi_sf chains push to the FrameSink via mpsc (they do — `Outbound`), then a single FrameSink instance plus a single mpsc is fine. The dedup state is `Mutex`-protected.
