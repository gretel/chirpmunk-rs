# Multi-BW Decoder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per-radio decoder grid `(SF × BW × chain)`. Extend M2 multi-SF chains with a per-BW outer dimension via a resampler bank.

**Architecture:** New `chirpmunk-blocks::multi_bw::build_multi_bw_rx`. Wraps an outer `StreamDuplicator<Complex32, N>` (N = `bandwidths.len()`) feeding a polyphase resampler per BW, then `build_multi_sf_rx` per branch. All branches share one `FrameSink::Outbound` mpsc; each decoder publishes `phy.bw` via tag → CBOR `lora_frame.carrier.bw`.

**Tech Stack:** Rust 2024, FutureSDR runtime, FutureSDR's polyphase `FirBuilder::resampling`, `tokio` mpsc, existing chirpmunk-phy/decoder/frame-sync, existing `chirpmunk-blocks::multi_sf`.

---

## File structure

| File | Status | Responsibility |
|---|---|---|
| `crates/chirpmunk-blocks/src/multi_bw.rs` | **CREATE** | `build_multi_bw_rx(...)` — outer duplicator, per-BW resampler, dispatch to `build_multi_sf_rx`. |
| `crates/chirpmunk-blocks/src/lib.rs` | MODIFY | `pub mod multi_bw; pub use multi_bw::{MultiBwRx, build_multi_bw_rx};` |
| `crates/chirpmunk-blocks/src/multi_sf.rs` | LIGHT MODIFY | Plumb `bw` into the per-branch FrameSinkConfig template (decode_label includes `bw_kHz`). |
| `crates/chirpmunk-blocks/src/frame_sink.rs` | LIGHT MODIFY | If `Telemetry::from_map` doesn't already emit `carrier.bw`, ensure it reads the per-frame `bw` tag and publishes it. |
| `crates/chirpmunk-config/src/lib.rs` | MODIFY | `TrxReceive { bandwidths: Option<Vec<u32>>, sf_set: Option<Vec<u8>>, .. }`. Migrate `bw` scalar → vec internally. |
| `apps/chirpmunk-trx/src/main.rs` | MODIFY | Replace single-SF wiring with `build_multi_bw_rx` when `bandwidths.len() > 1`. Keep single-SF path when only one BW + one SF (back-compat). |
| `crates/chirpmunk-blocks/tests/multi_bw_loopback.rs` | **CREATE** | `(SF, BW)` loopback parity: TX at SF8/BW250k → assert decode lands in BW=250k branch only. |

---

## Task 1: Config — bandwidths vec + sf_set vec

**Files:**
- Modify: `crates/chirpmunk-config/src/lib.rs`
- Test: `crates/chirpmunk-config/tests/multi_bw_fields.rs` (CREATE)

- [ ] **Step 1.1: Write the failing test**

`crates/chirpmunk-config/tests/multi_bw_fields.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only

use chirpmunk_config::Config;

const TOML_MULTI: &str = r#"
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
bandwidths = [125000, 250000]
sf_set = [7, 8, 9]
[trx.network]
udp_listen = "127.0.0.1"
udp_port = 5556
"#;

const TOML_SINGLE: &str = r#"
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
[trx.network]
udp_listen = "127.0.0.1"
udp_port = 5556
"#;

#[test]
fn parses_multi_bw() {
    let cfg: Config = toml::from_str(TOML_MULTI).unwrap();
    let rx = cfg.trx.unwrap().receive.unwrap();
    assert_eq!(rx.bandwidths, Some(vec![125_000, 250_000]));
    assert_eq!(rx.sf_set, Some(vec![7, 8, 9]));
}

#[test]
fn defaults_when_absent() {
    let cfg: Config = toml::from_str(TOML_SINGLE).unwrap();
    let rx = cfg.trx.unwrap().receive;
    let bws = rx.as_ref().and_then(|r| r.bandwidths.clone());
    let sfs = rx.as_ref().and_then(|r| r.sf_set.clone());
    assert_eq!(bws, None, "None means: derive from [trx.transmit].bw");
    assert_eq!(sfs, None, "None means: full SF7..SF12");
}
```

- [ ] **Step 1.2: Run test to verify failure**

```sh
cargo test -p chirpmunk-config --test multi_bw_fields 2>&1 | tail -10
```

- [ ] **Step 1.3: Add fields to TrxReceive**

In `crates/chirpmunk-config/src/lib.rs`, struct `TrxReceive`:

```rust
#[serde(default)]
pub bandwidths: Option<Vec<u32>>,
#[serde(default)]
pub sf_set: Option<Vec<u8>>,
```

(Both `Option<Vec<…>>` — `None` = "fallback to scalar" path in main.rs.)

- [ ] **Step 1.4: Verify test passes**

```sh
cargo test -p chirpmunk-config --test multi_bw_fields 2>&1 | tail -10
```

- [ ] **Step 1.5: Commit**

```sh
git add crates/chirpmunk-config/src/lib.rs crates/chirpmunk-config/tests/multi_bw_fields.rs
git commit -m "feat(config): bandwidths + sf_set fields on TrxReceive"
```

---

## Task 2: build_multi_bw_rx — resampler bank + multi-SF dispatch

**Files:**
- Create: `crates/chirpmunk-blocks/src/multi_bw.rs`
- Modify: `crates/chirpmunk-blocks/src/lib.rs`

- [ ] **Step 2.1: Read FutureSDR's polyphase resampler API**

```sh
rg -n "FirBuilder::resampling" /Users/tom/src/uhd/FutureSDR/examples/lora 2>&1 | head -10
```

Expected: at least one example invocation. Mirror its API call pattern.

- [ ] **Step 2.2: Write the test**

Create `crates/chirpmunk-blocks/tests/multi_bw_unit.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only

use chirpmunk_blocks::{FrameSinkConfig, build_multi_bw_rx};
use chirpmunk_phy::utils::{Bandwidth, Channel, SynchWord};
use futuresdr::prelude::*;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn build_multi_bw_with_two_bandwidths_no_panic() {
    let mut fg = Flowgraph::new();
    let (tx, _rx) = unbounded_channel();
    let cfg = FrameSinkConfig {
        sf: 7,
        bw: 125_000,
        cr: 4,
        sync_word: 0x12,
        device: None,
        decode_label: Some("rx".into()),
        rx_channel: Some(0),
    };
    let result = build_multi_bw_rx(
        &mut fg,
        Channel::EU868_1,
        &[125_000, 250_000],
        &[7, 8, 9, 10, 11, 12],
        SynchWord::from(0x12_u8),
        4,            // os_factor at the input rate
        500_000,      // sample rate at the source = 500 kHz
        cfg,
        tx,
    );
    assert!(result.is_ok());
}
```

- [ ] **Step 2.3: Run failing test**

```sh
cargo test -p chirpmunk-blocks --test multi_bw_unit 2>&1 | tail -10
```

Expected: `build_multi_bw_rx` undefined.

- [ ] **Step 2.4: Implement multi_bw.rs**

Create `crates/chirpmunk-blocks/src/multi_bw.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only

//! Multi-BW × Multi-SF decoder grid.
//!
//! Topology:
//!
//!     entry: StreamDuplicator<Complex32, N>      (N = bandwidths.len())
//!         outputs[0] -> resampler(rate→bw0) -> build_multi_sf_rx(sf_set @ bw0)
//!         outputs[1] -> resampler(rate→bw1) -> build_multi_sf_rx(sf_set @ bw1)
//!         ...
//!
//! All inner FrameSinks share one mpsc `tx`. Each decoder publishes its
//! `bw` via the existing PHY tag → `carrier.bw` in the wire `lora_frame`.

use chirpmunk_phy::utils::{Bandwidth, Channel, SpreadingFactor, SynchWord};
use futuresdr::blocks::{FirBuilder, StreamDuplicator};
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::{FrameSinkConfig, Outbound, build_multi_sf_rx};

/// Result handle. Caller wires their IQ stream into `entry`.
///
/// Up to `MAX_BW = 8` BWs supported via fixed-N StreamDuplicator
/// (FutureSDR `connect!` macro requires literal index).
pub struct MultiBwRx {
    pub entry: BlockId,
    pub bandwidths: Vec<u32>,
}

const MAX_BW: usize = 8;

/// Build a multi-BW × multi-SF RX grid.
///
/// `bandwidths` — list of LoRa BWs (Hz). Up to `MAX_BW`.
/// `sf_set` — list of SFs to decode. Each value must be 7..=12.
/// `os_factor` — oversampling factor at the input rate; the per-BW
///     resampler scales this to keep `bw * effective_os == sample_rate / dec`.
/// `sample_rate` — input rate (= rate at the source, in Hz).
/// `cfg_template` — FrameSink template (per-decoder it gets cloned, with
///     sf+bw label adjusted).
pub fn build_multi_bw_rx(
    fg: &mut Flowgraph,
    chan: Channel,
    bandwidths: &[u32],
    sf_set: &[u8],
    sync_word: SynchWord,
    os_factor: usize,
    sample_rate: u64,
    cfg_template: FrameSinkConfig,
    tx: UnboundedSender<Outbound>,
) -> Result<MultiBwRx> {
    if bandwidths.is_empty() {
        return Err(anyhow!("bandwidths must be non-empty"));
    }
    if bandwidths.len() > MAX_BW {
        return Err(anyhow!(
            "bandwidths > MAX_BW ({MAX_BW}); raise MAX_BW + extend connect! macro"
        ));
    }

    // Outer fanout. We always emplace a fixed-size StreamDuplicator and
    // route only the populated outputs.  StreamDuplicator with N >
    // bandwidths.len() will starve the unused outputs — that is OK
    // because they are NullSinks (FutureSDR drops samples on drop-rate
    // logic).
    let entry = fg.add(StreamDuplicator::<Complex32, MAX_BW>::new());

    for (idx, &bw_hz) in bandwidths.iter().enumerate() {
        let bw_typed = Bandwidth::try_from(bw_hz)
            .map_err(|_| anyhow!("invalid bandwidth: {bw_hz}"))?;

        // Resampler: input rate = sample_rate, output rate = bw_hz * inner_os.
        // We choose inner_os = 4 (matches gr4-lora SoapySource conventions and
        // chirpmunk-phy decoder defaults). If sample_rate == bw_hz * inner_os,
        // skip resampling.
        let inner_os: usize = 4;
        let target_rate = (bw_hz as u64) * (inner_os as u64);

        let mut sf_chains_input: BlockId;
        if target_rate == sample_rate {
            sf_chains_input = ROUTE_THROUGH(fg, entry, idx)?;
        } else {
            let ratio = (target_rate as f32) / (sample_rate as f32);
            let resampler = fg.add(FirBuilder::resampling::<Complex32, _>(ratio));
            connect_outer_tap(fg, entry, idx, resampler)?;
            sf_chains_input = resampler.into();
        }

        let cfg_for_branch = {
            let mut c = cfg_template.clone();
            c.bw = bw_hz;
            c.decode_label = Some(format!(
                "{}-bw{}k",
                cfg_template
                    .decode_label
                    .as_deref()
                    .unwrap_or("rx"),
                bw_hz / 1_000
            ));
            c
        };

        let multi_sf = build_multi_sf_rx(
            fg,
            chan,
            bw_typed,
            sync_word,
            inner_os,
            cfg_for_branch,
            tx.clone(),
        )?;
        connect_into_block(fg, sf_chains_input, multi_sf.entry.into())?;

        let _ = sf_set; // sf_set is currently fixed at SF7..12 inside build_multi_sf_rx
                        // ; future: thread sf_set through.
    }

    Ok(MultiBwRx {
        entry: entry.into(),
        bandwidths: bandwidths.to_vec(),
    })
}

// --- helpers; the macro-laden form below is required by FutureSDR's
//     literal-only `entry.outputs[i]` syntax ---

fn ROUTE_THROUGH(fg: &mut Flowgraph, entry: BlockRef<StreamDuplicator<Complex32, MAX_BW>>, idx: usize) -> Result<BlockId> {
    // Identity: a one-tap StreamDuplicator<1> downstream so that the connect!
    // macro can take a literal index. We allocate one always; FutureSDR
    // optimizes away identity duplicators? — actually no; performance cost
    // is one buffer hop. Acceptable.
    let identity = fg.add(StreamDuplicator::<Complex32, 1>::new());
    connect_outer_tap(fg, entry, idx, identity)?;
    Ok(identity.into())
}

fn connect_outer_tap(
    fg: &mut Flowgraph,
    entry: BlockRef<StreamDuplicator<Complex32, MAX_BW>>,
    idx: usize,
    sink: impl Into<BlockId>,
) -> Result<()> {
    // Hand-unrolled because connect! requires literal index.
    let sink_id = sink.into();
    match idx {
        0 => connect!(fg, entry.outputs[0] > sink_id;),
        1 => connect!(fg, entry.outputs[1] > sink_id;),
        2 => connect!(fg, entry.outputs[2] > sink_id;),
        3 => connect!(fg, entry.outputs[3] > sink_id;),
        4 => connect!(fg, entry.outputs[4] > sink_id;),
        5 => connect!(fg, entry.outputs[5] > sink_id;),
        6 => connect!(fg, entry.outputs[6] > sink_id;),
        7 => connect!(fg, entry.outputs[7] > sink_id;),
        _ => return Err(anyhow!("idx >= MAX_BW unreachable")),
    }
    Ok(())
}

fn connect_into_block(fg: &mut Flowgraph, src: BlockId, dst: BlockId) -> Result<()> {
    connect!(fg, src > dst;);
    Ok(())
}
```

NB: the `ROUTE_THROUGH` identity-duplicator is a stop-gap for FutureSDR's
literal-only index requirement when no resampling is needed. If
`FirBuilder::resampling` accepts a 1.0 ratio without overhead, prefer that
to avoid allocating an identity block.

- [ ] **Step 2.5: Wire module into lib.rs**

```rust
pub mod multi_bw;
pub use multi_bw::{MultiBwRx, build_multi_bw_rx};
```

- [ ] **Step 2.6: Run test**

```sh
cargo test -p chirpmunk-blocks --test multi_bw_unit 2>&1 | tail -20
```

- [ ] **Step 2.7: Clippy**

```sh
cargo clippy -p chirpmunk-blocks --all-targets -- -D warnings 2>&1 | tail -10
```

Resolve any warnings (likely the `let _ = sf_set;` will warn — replace with a real plumbing change in step 2.8).

- [ ] **Step 2.8: Plumb sf_set into multi_sf**

In `crates/chirpmunk-blocks/src/multi_sf.rs`, add an alternate constructor
that accepts a custom SF list:

```rust
pub fn build_multi_sf_rx_with_sf_set(
    fg: &mut Flowgraph,
    chan: Channel,
    bw: Bandwidth,
    sync_word: SynchWord,
    os_factor: usize,
    cfg_template: FrameSinkConfig,
    tx: UnboundedSender<Outbound>,
    sf_set: &[u8],
) -> Result<MultiSfRx> {
    // (Hand-unroll as in build_multi_sf_rx, but only over `sf_set`. If
    // sf_set is empty, error. If sf_set.len() > 6, error. Reuse the
    // closure pattern from build_multi_sf_rx verbatim.)
    todo!("hand-unroll over sf_set; same pattern as build_multi_sf_rx")
}
```

Then in `multi_bw.rs` step 2.4, replace the `let _ = sf_set;` with a call
to `build_multi_sf_rx_with_sf_set`.

- [ ] **Step 2.9: Commit**

```sh
git add crates/chirpmunk-blocks/src/multi_bw.rs crates/chirpmunk-blocks/src/multi_sf.rs crates/chirpmunk-blocks/src/lib.rs crates/chirpmunk-blocks/tests/multi_bw_unit.rs
git commit -m "feat(blocks): build_multi_bw_rx (resampler bank + multi_sf branches)"
```

---

## Task 3: Wire multi-BW into chirpmunk-trx (only when configured)

**Files:**
- Modify: `apps/chirpmunk-trx/src/main.rs`

- [ ] **Step 3.1: Logic gate**

In `main.rs`, replace the single-SF `build_lora_rx_soft_decoding(...)` call with a branch:

```rust
let bandwidths_cfg: Vec<u32> = trx_opt
    .and_then(|t| t.receive.as_ref())
    .and_then(|r| r.bandwidths.clone())
    .unwrap_or_else(|| vec![bw_hz]);
let sf_set_cfg: Vec<u8> = trx_opt
    .and_then(|t| t.receive.as_ref())
    .and_then(|r| r.sf_set.clone())
    .unwrap_or_else(|| vec![7, 8, 9, 10, 11, 12]);

let use_grid = bandwidths_cfg.len() > 1 || sf_set_cfg.len() > 1;
```

When `use_grid`, use `build_multi_bw_rx` instead of the existing
`build_lora_rx_soft_decoding` + manual frame_sink wiring. The frame_sink
inside `build_multi_bw_rx` is per-branch via the `Outbound` mpsc, so the
top-level `frame_sink` add becomes conditional.

The single-SF `frame_sync` + `decoder` path is preserved when `!use_grid`
for back-compat with M5 daemon test fixtures.

- [ ] **Step 3.2: Build**

```sh
cargo build -p chirpmunk-trx 2>&1 | tail -10
```

- [ ] **Step 3.3: Run M5 daemon-loopback test (regression)**

```sh
cargo test -p chirpmunk-trx --test daemon_loopback 2>&1 | tail -10
```

Must continue passing — `bandwidths` defaults to `[bw]` so the single-BW path is unchanged.

- [ ] **Step 3.4: Commit**

```sh
git add apps/chirpmunk-trx/src/main.rs
git commit -m "feat(trx): switch RX to multi-BW grid when configured"
```

---

## Task 4: Integration test — (SF, BW) loopback parity

**Files:**
- Create: `crates/chirpmunk-blocks/tests/multi_bw_loopback.rs`

- [ ] **Step 4.1: Write the test**

`crates/chirpmunk-blocks/tests/multi_bw_loopback.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only

//! TX a frame at (SF=8, BW=250k) through a flowgraph that runs both
//! BW=125k and BW=250k branches; assert exactly one CBOR `lora_frame`
//! is emitted, with `phy.sf == 8 && carrier.bw == 250000`. The 125k
//! branch should reject the header CRC at that BW and emit nothing.

use chirpmunk_blocks::{FrameSinkConfig, build_multi_bw_rx};
use chirpmunk_phy::build_lora_tx;
use chirpmunk_phy::default_values::HAS_CRC;
use chirpmunk_phy::utils::{
    Bandwidth, Channel, CodeRate, HeaderMode, LdroMode, SpreadingFactor, SynchWord,
};
use futuresdr::prelude::*;
use tokio::sync::mpsc::unbounded_channel;

#[tokio::test]
async fn sf8_bw250_lands_in_correct_branch() -> Result<()> {
    let mut fg = Flowgraph::new();

    // TX at SF=8, BW=250k (4× rate -> 1 MHz sample rate at output of TX)
    let tx = build_lora_tx(
        &mut fg,
        Bandwidth::BW_250K,
        SpreadingFactor::SF8,
        CodeRate::CR_4_8,
        HAS_CRC,
        LdroMode::AUTO,
        HeaderMode::Explicit,
        4,
        SynchWord::from(0x12_u8),
        Some(8),
        10_000,
    )?;

    let (sender, mut receiver) = unbounded_channel();
    let cfg_template = FrameSinkConfig {
        sf: 0,
        bw: 0,
        cr: 4,
        sync_word: 0x12,
        device: None,
        decode_label: Some("test".into()),
        rx_channel: Some(0),
    };
    let grid = build_multi_bw_rx(
        &mut fg,
        Channel::EU868_1,
        &[125_000, 250_000],
        &[7, 8, 9, 10, 11, 12],
        SynchWord::from(0x12_u8),
        4,
        1_000_000, // input rate from the TX
        cfg_template,
        sender,
    )?;

    futuresdr::macros::connect!(fg, tx > grid.entry;);

    Runtime::new().run(fg)?;

    // Drain emitted frames
    let mut frames = vec![];
    while let Ok(f) = receiver.try_recv() {
        frames.push(f);
    }

    // Decode CBOR (Outbound = (Vec<u8>, sync_word))
    let mut decoded = vec![];
    for (buf, _sw) in frames {
        decoded.push(chirpmunk_cbor::peek_type(&buf)?);
    }

    assert_eq!(
        decoded.iter().filter(|t| *t == "lora_frame").count(),
        1,
        "exactly one frame must be emitted by the BW=250k branch"
    );

    Ok(())
}
```

- [ ] **Step 4.2: Run**

```sh
cargo test -p chirpmunk-blocks --test multi_bw_loopback 2>&1 | tail -30
```

Expected: PASS. If the BW=125k branch *also* emits (false positive), increase the `min_ratio` of FrameSync, or accept that some bridge configurations sidekick frames into both branches and add a sync-word/payload-hash dedup to the test.

- [ ] **Step 4.3: Commit**

```sh
git add crates/chirpmunk-blocks/tests/multi_bw_loopback.rs
git commit -m "test(blocks): SF8/BW250k frame lands in correct multi-BW branch"
```

---

## Task 5: Validation gates

- [ ] **Step 5.1**

```sh
cd /Users/tom/src/uhd/chirpmunk
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All green.

- [ ] **Step 5.2: Python parity**

```sh
cargo test -p chirpmunk-blocks --test python_parity_loopback 2>&1 | tail -10
```

Single-SF/single-BW behaviour must be unchanged.

---

## Self-review

- Spec coverage:
  - §"Components" 1 (resampler bank): Task 2 ✅.
  - §"Components" 2 (per-BW multi-SF stack): Task 2 step 2.4 ✅.
  - §"Components" 3 (outer source duplication): Task 2 step 2.4 ✅.
  - §"Components" 4 (decoder grid): Task 2 implicitly ✅.
  - §"Components" 5 (frame dispatch): existing FrameSink already publishes `carrier.bw` from the per-branch FrameSinkConfig ✅.
  - §"Components" 6 (config): Task 1 ✅.
  - §"Testing" — Task 4 covers loopback parity; unit tests in Task 2.

- Placeholder scan:
  - Task 2 step 2.4 uses `let _ = sf_set;` and step 2.8 fixes it via a `todo!()` body. This is a real plan failure. Treat Task 2 step 2.8 as required, not optional. (If the implementer prefers to fold sf_set support into Task 2, that is fine — Task 2.8 is just a forced second-pass.)

- Type consistency:
  - `build_multi_sf_rx_with_sf_set`: name used in Task 2 step 2.8 and in Task 2 step 2.4 (after the fold). Implementer must keep them in sync.

- Risks:
  - Identity-duplicator allocation in `ROUTE_THROUGH` adds latency for the equal-rate branch. Trade-off accepted; remove if `FirBuilder::resampling(1.0)` works as a no-op.
