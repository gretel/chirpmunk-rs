# chirpmunk — SPEC

Standalone Rust LoRa transceiver/scanner project. Ports the value-add of
`gr4-lora` (LOST) onto the `FutureSDR` runtime. Reuses existing FutureSDR
`examples/lora/` PHY where applicable. Keeps the `lora.*` Python userland
verbatim — UDP CBOR is the contract.

## Mission

Deliver a Rust-native LoRa transceiver + wideband scanner with CBOR/UDP
control plane, deployable on host (Linux/macOS) first and on PlutoSDR
(armv7) eventually. Behaviourally interoperable with the existing `gr4-lora`
control plane and the `lora.*` userland (identity, mesh bridge, telemetry,
hwtests).

## Non-goals

- Re-implement gnuradio4 in Rust.
- Port the entire C++ qa suite. Coverage too wide. Use Python `lora.*` and
  pytest as primary integration test surface (per (5) in user feedback).
- Web GUI in v0. `viz/waterfall.py` continues to work via UDP.
- Re-implement `lora.*` userland in Rust. Stays Python.
- `opendigitizer` integration.

## License

`GPL-3.0-only` (default). FutureSDR `examples/lora/` is GPL-3.0-only;
copying source from there is the fastest path and forces this license on
chirpmunk too.

### Future option: ISC

If we later choose to drop GPL reuse and re-implement the PHY clean-room
from `gr4-lora` C++ (ISC, your own code), chirpmunk can ship under ISC.
Trade-off: more port work; cleaner ownership; matches gr4-lora upstream.

License switch is reversible early (M0–M1). After M1 freezes the reuse
pattern, switching costs scale with ported LoC. Decide at M1 review.

## Repo layout

```
chirpmunk/
├── Cargo.toml                    # workspace
├── crates/
│   ├── chirpmunk-phy/            # SF lockstep, CSS demod hooks, DecodeChain port
│   ├── chirpmunk-blocks/         # Splitter, MultiSfDecoder, FrameSink,
│   │                             # ScanController, CAD, SpectrumTap, TxQueue,
│   │                             # BurstTaper
│   ├── chirpmunk-cbor/           # frame schema + minicbor codecs (typed)
│   ├── chirpmunk-config/         # TOML config (toml crate, mirror config.hpp)
│   └── chirpmunk-udp/            # UDP fanout + client mgmt (mirror udp_state.hpp)
├── apps/
│   ├── chirpmunk-trx/            # full-duplex transceiver
│   └── chirpmunk-scan/           # wideband scanner
├── tests/                        # Rust integ tests (smoke only)
├── examples/                     # mini reproducers
├── docs/
│   └── milestones/               # per-milestone notes (live)
├── tmp/                          # local scratch (gitignored)
├── .gitignore
├── README.md                     # short
├── LICENSE
├── REPORT.md
└── SPEC.md
```

## Dependencies (pinned)

| Crate | Version | Why |
|---|---|---|
| `futuresdr` | `path = "../FutureSDR"` (dev), `git = ".../FutureSDR/futuresdr"` + tag (prod) | runtime |
| `futuredsp` | path | DSP primitives |
| `seify` | `0.18` (via futuresdr feature) | hardware (initial) |
| `minicbor` | `0.25+` | CBOR encode/decode (no_std-ready, no serde, fits armv7) |
| `minicbor-derive` | `0.16+` | derive macros |
| `toml` | `0.9+` | config |
| `tokio` | `1` | async runtime where needed |
| `tracing` | re-export from futuresdr | logging |
| `clap` | `4.5` derive | CLI |
| `anyhow` / `thiserror` | latest | errors |

Track FutureSDR `0.0.41-dev` HEAD during development; pin to a tag at v0
freeze. API churn risk acknowledged (see Risks).

## CBOR control plane

Re-use existing schema from `gr4-lora/CBOR-SCHEMA.md` and Wireshark
dissector verbatim. Minimum frame types:

| Type | Direction | Producer |
|---|---|---|
| `lora_frame` | RX → clients | FrameSink |
| `lora_tx` | clients → daemon | external TX request |
| `lora_tx_ack` | daemon → requester | TX worker |
| `scan_spectrum` | scanner → clients | SpectrumTap |
| `scan_detection` | scanner → clients | ScanController |
| `wideband_sweep` | scanner → clients | ScanController |
| Telemetry | both | property_map → CBOR auto |

`chirpmunk-cbor` provides typed encoders + decoders via `minicbor::Encode` /
`Decode` derives, plus a `Telemetry` builder mirroring
`algorithm/Telemetry.hpp`. Auto-emit ISO 8601 `"ts"` field. Test parity
against captured CBOR samples from gr4-lora runs.

## Architecture

UDP bind addresses live in `chirpmunk-config` (TOML); diagrams below use
logical names (`udp.frame_bind`, `udp.scan_bind`, `udp.tx_bind`) — never
hardcoded literals in code or examples.

### RX flowgraph (single channel)

```
seify::SourceBuilder → DCBlocker (optional) → Splitter
    → MultiSfDecoder
        → FrameSink → BlobToUdp(udp.frame_bind)
                      └─ telemetry → BlobToUdp(udp.frame_bind)  (shared client list)
```

### RX flowgraph (dual channel)

```
seify::SourceBuilder (2 RX) → MultiSfDecoder ×2 → FrameSink ×2 → UDP
```

(Two independent narrowband chains. Antenna combining deferred to post-M6.)

### Scanner

```
seify::SourceBuilder(L1 rate) → Splitter
    → SpectrumTap → NullSink           # L1 energy snapshot
    → CaptureSink                      # L2 dwell capture
+ orchestrator task drives ScanController state machine
  (Accumulate → Probe → Report)
```

### TX

```
TxRequest (CBOR over UDP) → bounded queue → worker task
  → ephemeral graph: FutureSDR Transmitter → seify::SinkBuilder
  → wait for completion → CBOR ACK/NACK to requester
```

Reuse FutureSDR `examples/lora/Transmitter` as the TX block until proven
inadequate. LBT (Listen-Before-Talk) deferred — implement only if needed
post-M5. Worker returns `Result<_, TxError>`; malformed CBOR or seify error
→ `lora_tx_ack {ok=false, error=...}`.

## Module map

| Module | Source ref (gr4-lora) | Notes |
|---|---|---|
| `chirpmunk-phy::css_demod` | `phy/CssDemod.hpp` | hard + soft |
| `chirpmunk-phy::decode_chain` | `phy/DecodeChain.hpp` | post-demod pipeline |
| `chirpmunk-phy::preamble_sync` | `phy/PreambleSync.hpp` | Xhonneux §6 |
| `chirpmunk-phy::antenna_combiner` | `phy/AntennaCombiner.hpp` | post-M6 |
| `chirpmunk-phy::tx_chain` | `algorithm/tx_chain.hpp` | CRC+Hamming+interleave+Gray |
| `chirpmunk-phy::utilities` | `algorithm/utilities.hpp` | window, hop, bin math |
| `chirpmunk-phy::tables` | `algorithm/tables.hpp` | constants |
| `chirpmunk-blocks::multi_sf_decoder` | `MultiSfDecoder.hpp` | block, all SF lockstep |
| `chirpmunk-blocks::frame_sink` | `FrameSink.hpp` | block, UDP fanout |
| `chirpmunk-blocks::splitter` | `Splitter.hpp` | tee block |
| `chirpmunk-blocks::spectrum_tap` | `SpectrumTap.hpp` + algo | block + state |
| `chirpmunk-blocks::scan_controller` | `ScanController.hpp` | block, state machine |
| `chirpmunk-blocks::cad` | `ChannelActivityDetector.hpp` | block |
| `chirpmunk-blocks::capture_sink` | `CaptureSink.hpp` | block |
| `chirpmunk-cbor::*` | `cbor.hpp` + `algorithm/Telemetry.hpp` | minicbor codec |
| `chirpmunk-config::*` | `apps/config.{hpp,cpp}` | toml-rs |
| `chirpmunk-udp::*` | `apps/udp_state.hpp` | client mgmt + broadcast |

### Reuse of FutureSDR `examples/lora/`

`examples/lora/` is an independent Cargo workspace, not a published crate.
Mechanism for reuse:

1. **Copy + attribute** the relevant `.rs` files into `chirpmunk-phy` /
   `chirpmunk-blocks`, preserve GPL-3.0 SPDX headers, add chirpmunk
   modifications under same license. Track upstream divergence by file
   header comment.
2. Avoid path-depending on `examples/lora/` directly — too coupled to the
   FutureSDR repo lifecycle, breaks at every `cargo build` against a
   moving FutureSDR.

Modules in scope for reuse (by copy):

| FutureSDR upstream | chirpmunk role |
|---|---|
| `Encoder`, `Modulator`, `Transmitter` | TX path foundation |
| `FftDemod`, `GrayMapping`, `HammingDecoder`, `Deinterleaver` | RX post-demod |
| `FrameSync` | reference; `PreambleSync` port replaces it once parity verified |
| `header_decoder`, `Decoder` | header + payload assembly |
| `utils::{Channel, Bandwidth, SpreadingFactor, SynchWord, CodeRate, LdroMode}` | enums / parsers |
| `default_values.rs` | constants |

### Port-from-gr4-lora candidates (when copy is insufficient)

For the gr4-lora-specific value-add (no equivalent in `examples/lora`),
port from gr4-lora C++ (ISC, in-license under GPL-3.0-only when written
fresh in Rust):

| chirpmunk module | gr4-lora source |
|---|---|
| `chirpmunk-phy::preamble_sync` | `phy/PreambleSync.hpp` (Xhonneux §6 iterative) |
| `chirpmunk-blocks::multi_sf_decoder` | `MultiSfDecoder.hpp` |
| `chirpmunk-blocks::scan_controller` | `ScanController.hpp` |
| `chirpmunk-blocks::frame_sink` | `FrameSink.hpp` (CBOR+UDP semantics) |
| `chirpmunk-blocks::spectrum_tap` | `SpectrumTap.hpp` |
| `chirpmunk-blocks::cad` | `ChannelActivityDetector.hpp` |
| `chirpmunk-blocks::splitter` | `Splitter.hpp` |
| `chirpmunk-blocks::capture_sink` | `CaptureSink.hpp` |
| `chirpmunk-cbor::*` | `cbor.hpp` + `algorithm/Telemetry.hpp` |
| `chirpmunk-config::*` | `apps/config.{hpp,cpp}` |
| `chirpmunk-udp::*` | `apps/udp_state.hpp` |

## Hardware

Single target through M5: USRP via seify (Soapy/UHD path). Available device:
B210 / B220 Mini. Discover only the UHD seify path; do not generalise to
PlutoSDR / RTL-SDR / HackRF until needed.

- M0..M5: seify UHD on host (macOS/Linux).
- M6: re-evaluate. PlutoSDR / IIO direct path / tezuka_fw armv7 cross-build
  enter scope here if and when needed.

User input (4): gr4-lora's UHD/Soapy patches address upstream issues that
seify likely papers over differently. Do not pre-port patches; observe
behaviour first, fix only when problems reproduce.

## Milestones

### M0 — Skeleton (DONE)
- [x] Workspace layout (Cargo.toml, crates/, apps/), .gitignore, README,
      LICENSE.
- [x] Lockfile pinned. `cargo build --workspace` clean.
- [x] `chirpmunk-cbor` encodes/decodes `lora_frame` (idempotent
      round-trip + Python `cbor2` parity).
- [x] `chirpmunk-udp` skeleton (subscribe/broadcast/client list +
      filter + send-failure eviction).
- [x] `chirpmunk-config` parses gr4-lora `config-pluto.toml` verbatim.
- Result: 10 tests pass, clippy clean, fmt clean.

### M1 — Single-channel RX (one SF) (DONE)
- [x] FutureSDR `examples/lora` PHY pipeline copied into `chirpmunk-phy`
      under GPL-3.0-only with attribution.
- [x] `chirpmunk-blocks::FrameSink` builds `LoraFrame` from
      `Decoder.out_annotated`, ships CBOR over mpsc.
- [x] Loopback acceptance: `tx_to_framesink_decodes_payload` round-trips
      a payload through the full TX→RX pipeline.
- [x] IQ replay acceptance: `replay_sf7_cr1_bw125_decodes_hello_meshcore`
      decodes the canonical `gr4-lora/test_vectors/sf7_cr1_bw125000`
      capture and matches `payload == "Hello MeshCore"`.
- [x] Python parity acceptance: `full_m1_loopback_to_python` proves the
      Rust-emitted CBOR `lora_frame` is consumed correctly by Python
      `cbor2` over UDP after a Subscribe handshake.
- [x] Telemetry: FrameSink reads snr_db, noise_floor_db, peak_db,
      snr_db_td, channel_freq, decode_bw, cfo_int, cfo_frac, sfo_hat,
      sample_rate, frequency_corrected, ppm_error from upstream
      annotations.
- [ ] `chirpmunk-trx` binary CLI (deferred — first real hardware spike).
- License review gate: GPL-3.0-only confirmed for now (PHY copied from
  FutureSDR `examples/lora`).

### M2 — Multi-SF + dual channel (DONE — parallel chains variant)
- [x] `chirpmunk-blocks::build_multi_sf_rx` builds 6 parallel SF chains
      (SF7..SF12) sharing one `StreamDuplicator`. Each chain owns its
      FrameSink.
- [x] FrameSink extracts telemetry (snr, cfo_int, cfo_frac, sfo_hat,
      noise_floor_db, peak_db, snr_db_td, channel_freq, decode_bw,
      sample_rate, frequency_corrected, ppm_error) from upstream
      `MapStrPmt` annotations.
- [x] FrameSink strips the 2-byte CRC trailer when `has_crc=true`.
- [x] Loopback test: TX(SF8) → 6 chains → SF8 chain decodes the payload.
- [ ] gr4-lora-style lockstep `MultiSfDecoder` (single block, all SFs).
      Defer until perf shows scheduler overhead matters.
- [ ] Dual-channel: pattern is `build_multi_sf_rx` ×2 sharing a
      broadcaster. No dedicated test yet — proven by composition.

### M3 — TX (single packet) (DONE — loopback variant)
- [x] `chirpmunk-cbor::{LoraTx, LoraTxAck}` codecs, full schema parity
      (payload, seq, cr, sync_word, preamble_len, repeat, gap_ms,
      dry_run).
- [x] `chirpmunk-blocks::dispatch_lora_tx` posts `Pmt::Blob` to a
      running Flowgraph's Transmitter `msg` port; honours `repeat` /
      `gap_ms`; respects `dry_run`; returns matching `lora_tx_ack`.
- [x] Loopback: `lora_tx` CBOR → dispatch → TX → RX → FrameSink →
      `lora_frame` with byte-equal payload.
- [x] dry_run path: ack returned, no TX dispatched.
- [ ] Hardware verification (live RX on Heltec V3) — manual session.
- [ ] Ephemeral graph with seify Sink — needed at hardware time, not
      for the loopback test.

### M4 — Wideband scanner (`lora_scan` parity)
- [ ] `SpectrumTap` block (FFT energy snapshot).
- [ ] `ScanController` state machine block (Accumulate/Probe/Report).
- [ ] CAD per-channel dwell.
- [ ] `scan_spectrum` + `scan_detection` + `wideband_sweep` events.
- Acceptance: 16 MS/s sweep on B210 → `lora.viz.waterfall` renders;
  `lora.hwtests.scan_test` passes.

### M5 — Full duplex (DONE — loopback variant)
- [x] `chirpmunk-udp::Server::bind_with_inbound` forwards non-subscribe
      datagrams via mpsc to the application; `Server::send_to` provides
      unicast for ack delivery.
- [x] `chirpmunk-trx` binary: clap CLI (`--bind`, `--loopback`),
      tracing-subscriber init, tokio multi-thread runtime. Spawns
      Server::run, FrameSink broadcaster, lora_tx dispatcher; builds
      single-SF TX→loopback→RX flowgraph; SIGINT shutdown.
- [x] End-to-end test
      (`apps/chirpmunk-trx/tests/daemon_loopback.rs`): spawns the binary
      as a subprocess, Python `cbor2` subscribes, sends `lora_tx`,
      receives back `lora_frame` and `lora_tx_ack`.
- [ ] Daemon supervisor (process-level): SIGINT handled; richer
      lifecycle deferred.
- [ ] Hardware: real seify Sink + Source paths (M6 deferred research).

### M6 — Hardware bring-up (IN PROGRESS)
- [x] `chirpmunk-trx` hardware mode: seify Source + Sink wired into the
      flowgraph (`--device-args 'soapy_driver=uhd'`, `--freq`, `--rx-gain`,
      `--tx-gain`, `--os-factor`, optional `--rx-antenna` / `--tx-antenna`).
      `--loopback` retained for software CI.
- [x] LibreSDR B220 Mini (B210 product) bring-up via UHD: FPGA loads,
      register loopback passes, master clock auto-set to 32 MHz,
      sample rate 500 kHz (BW 125 k × os 4) accepted, flowgraph runs
      clean for 22 s with no errors. Daemon shuts down gracefully on
      SIGINT.
- [ ] On-air `lora_tx` decode against companion (Heltec V3 / RAK4631).
- [ ] DC spur observation + mitigation (likely needed; gr4-lora found
      `dc_blocker_cutoff = 2 kHz` at zero-IF).
- [ ] PlutoSDR / IIO direct path (deferred research).
- [ ] LBT (Listen-Before-Talk) — only if contention measured.
- [ ] tezuka_fw armv7 cross-build (deferred research).

## Test strategy

Per (6) in user feedback: **don't port the C++ qa suite wholesale**.

Layers:

1. **Rust unit tests** — only for non-trivial algorithm helpers (`crc`,
   `hamming`, `interleaving`, `tx_chain`, `utilities`, `tables`,
   `preamble_sync`). Keep focused. Approx ≤10 test files.
2. **Rust integration smoke tests** — flowgraph wiring, CBOR round-trip,
   UDP fanout. Approx 3–5 files. Live in `tests/`.
3. **`lora.*` pytest harness** — primary integration surface. Drives
   `chirpmunk-trx` and `chirpmunk-scan` via UDP CBOR. Existing tests in
   `gr4-lora/tests/` apply unchanged once daemon is binary-compatible.
4. **Hardware A/B** — `lora hwtest` matrix (decode, tx, scan, bridge_live)
   against companion (Heltec V3, RAK4631) and against gr4-lora running on
   the same antenna.
5. **Bench** — `criterion` for `css_demod`, `decode_chain`, `multi_sf_decoder`
   inner loop. Compare to `bm_lora_phy` numbers.

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| FutureSDR API churn (0.0.41-dev) | high | track HEAD daily during M0–M2, pin to tag at M3 freeze |
| seify/Soapy quirks differ from gr4-lora | medium | observe before porting patches; do not pre-empt |
| `MultiSfDecoder` lockstep semantics hard to express in async Rust | medium | prototype as single block emitting per-lane state; benchmark vs C++ |
| PlutoSDR/IIO buffer impedance | medium | defer to M6; use seify Soapy first |
| Python ABI churn (`lora.*`) | low | keep pinned; chirpmunk does not import; UDP CBOR contract only |
| gnuradio4 fork rebase (gretel/mitradio4 ↔ fair-acc) | low | orthogonal to chirpmunk; runs in gr4-lora workspace |
| CBOR schema drift between C++ and Rust encoders | medium | parity tests in M0; capture-replay at every milestone |

## Open questions (non-blocking)

- Does `seify::Builder` map cleanly to the `device_args` strings used in
  `apps/config.cpp`? Spike during M0.
- Are FutureSDR's `vmcircbuffer` semantics adequate for the
  burst-then-quiet scanner pattern, or does L2 capture want a separate
  buffer impl? Profile during M4.
- IIO direct path: write a custom seify driver, or fork `xilinx-dma` for
  PlutoSDR. Defer to M7 brainstorm.

## Engineering conventions

- KISS. Minimalism. Prefer reuse over reimplementation. No speculative
  generality. Add a feature only when a milestone demands it.
- Rust 2024, MSRV `1.89` (matches FutureSDR).
- Commit-style: short imperative subject, conventional prefix where it adds
  clarity (`feat:`, `fix:`, `port:`). DCO sign-off optional.
- Per AGENTS.md: no `rm`, use `trash`. Tmp files in `./tmp/`.

### Rust idioms (per `rust-engineer` skill)

- `thiserror` for error types in every `chirpmunk-*` crate.
- No `unwrap()` in production code paths. `expect("invariant: ...")` only
  where panics encode genuine invariants.
- Borrow over clone. `&str` over `String`. `&[T]` over `Vec<T>`.
- Document every `unsafe` block with safety invariants. Target: zero
  unsafe blocks in chirpmunk; FutureSDR runtime owns those.
- Doctests on public API where the example is short enough to be useful.
- Prefer trait-based composition + generics with associated types over
  enum-of-impls when dispatch is on type, not data.
- Async via tokio. Never mix blocking IO inside async scope; wrap with
  `tokio::task::spawn_blocking` when unavoidable.

### Validation gates (every commit)

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo test --doc
cargo bench         # phy crate, when present
```

Toolchain: stable rustfmt 1.9+ (Homebrew cargo). Nightly not required.

### Comments

Inline code comments describe **what the code does** at this point in the
file: invariants, intent, non-obvious algorithm steps, parameter units.

Inline code comments do **not** describe project history, change rationale,
or before/after diffs. No "was changed because", "previously this used X",
"TODO from M2 milestone". Project state, history, and decisions live in
SPEC.md, milestone docs, and git log.

## Out of scope (explicit)

- gnuradio4 work (lives in gr4-lora repo).
- AD9361 overclock patch port (UHD-specific, doesn't apply to FutureSDR).
- DuckDB writer port (Python continues handling storage).
- MeshCore protocol Rust port (Python `lora.bridges.meshcore.*` keeps role).
- Web/Prophecy GUI integration.
- Multi-antenna combining (deferred to post-M6).
- LBT (deferred to post-M5; ephemeral TX graph without contention check first).
- Collision-decoding algorithms beyond what gr4-lora exposes.
- Non-UHD seify backends through M5.
