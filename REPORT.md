# REPORT — Naive Reverse-Engineering of FutureSDR and gr4-lora

Date: 2026-05-04. Method: read-only scan of source trees, git logs, build configs.
Skipped CLAUDE.md, README.md, plugin skills per request. Conclusions inferred
from code shape, file layout, function names, comments-as-code.
~150 files inspected across both repos.

## 1. FutureSDR

### Identity

`/Users/tom/src/uhd/FutureSDR`. Cargo:

| Field | Value |
|---|---|
| name | `futuresdr` |
| version | `0.0.41-dev` |
| edition | `2024` (Rust 1.89+) |
| license | Apache-2.0 |
| repo | github.com/FutureSDR/FutureSDR |
| description | "An Experimental Async SDR Runtime for Heterogeneous Architectures" |

Workspace: root + `crates/futuredsp`, `crates/macros`, `crates/types`. Independent
Cargo workspaces: `crates/prophecy` (web GUI), `crates/remote` (control client),
every `examples/*`, every `perf/*`.

### Architecture

- **Block.** Actor-style. `#[derive(Block)]` proc-macro. `Kernel` trait
  (`init() / work() / deinit()`). `WorkIo` flags signal state.
- **Flowgraph.** DAG of stream + message ports. `connect!(fg, src > head > snk;
  producer | consumer;)`. `>` = stream, `|` = message.
- **Scheduler.** Pluggable. `SmolScheduler` default. `FlowScheduler`
  feature-gated. `WasmScheduler` for browser.
- **Buffers.** Pluggable. `circular` (vmcircbuffer 0.0.14) default native.
  `slab` default WASM. `circuit` in-place. `vulkan` / `wgpu` / `burn` GPU.
  `zynq` Xilinx DMA (Linux only).
- **Messages.** Typed `Pmt` (`crates/types/pmt.rs`, 21 KB). `#[message_handler]`.
- **Control port.** Axum REST on `127.0.0.1:1337`. Prophecy GUI = Leptos+WASM.

### Block library (`src/blocks/`)

47 blocks across stream/message I/O, FFT/FIR/IIR DSP, audio, signal sources,
Vulkan, WGPU, Zynq. Subdirs: `audio/`, `pfb/`, `seify/` (SoapySDR-style HW
abstraction), `signal_source/`, `wasm/`, `zeromq/`. `seify` crate v0.18 from
`/Users/tom/src/uhd/seify`.

### futuredsp crate

`firdes/`, `math/`, `decimating_fir.rs`, `fir.rs`, `iir.rs`,
`polyphase_resampling_fir.rs`, `rotator.rs`, `windows.rs`. ~50 KB.
Self-contained, framework-independent.

### Examples (26 total)

```
adsb android audio burn custom-routes cw egui file-trx firdes fm-receiver
inplace keyfob logging lora m17 macros rattlegram spectrum ssb vulkan
wasm wgpu wlan zeromq zigbee zynq
```

Each `examples/*/` independent Cargo workspace.

### LoRa example (`examples/lora/`)

Cargo `lora 0.1.0`, GPL-3.0. Direct port of `gr-lora_sdr` (Tapparel et al.,
EPFL).

Modules (~6900 LoC Rust):

| File | LoC | Purpose |
|---|---|---|
| `frame_sync.rs` | 1371+ | Detect/Sync/SfoCompensation, NetID caching, CFO/STO |
| `utils.rs` | 1166 | Channels (EU868), bandwidths, SF, sync words, parsers |
| `encoder.rs` / `decoder.rs` / `header_decoder.rs` | ~250 each | TX/RX/header |
| `fft_demod.rs` | ~500 | dechirp+FFT |
| `gray_mapping.rs` / `hamming_dec.rs` / `deinterleaver.rs` | ~250 each | post-demod |
| `modulator.rs` / `transmitter.rs` | ~150 | TX synthesis |
| `meshtastic.rs` | 19 KB | Meshtastic protocol decode |
| `packet_forwarder_client.rs` | 9.7 KB | Semtech packet forwarder UDP |

Bin entrypoints: `rx.rs`, `tx.rs`, `loopback.rs`, `rx_meshtastic.rs`,
`rx_meshtastic_all_channels.rs`, `rx_all_channels_eu.rs`, `tx_meshtastic.rs`.

Builders: `build_lora_tx`, `build_lora_rx_dyn`, `build_lora_rx_soft_decoding`,
`build_lora_rx_hard_decoding`. Soft-decision LLR present. Hardware glue via
`seify::Builder`.

### Activity

50 commits visible. Latest `00cbb34` 2026-05-02 "dev versions". Recent flurry:
docs/book (~20 commits), runtime API churn, `vmcircbuffer` upstream switch,
`claude.md` removed, burst-pad and console sink deleted. **API still in flux**;
AGENTS.md confirms "API stability is not a goal."

## 2. gr4-lora ("LOST")

### Identity

`/Users/tom/src/uhd/gr4-lora`. README: `# LOST`. Fork of `gr-lora_sdr`
reimplemented on gnuradio4. Single author Tom Hensel. Origin
`gretel/gr4-lora.git`. ISC license. C++23, CMake 3.27+.

`git log` shows 6 main commits — squashed/force-pushed history. Real
development in branches and in the gnuradio4 submodule fork. Active project
with deceptively flat top-level history.

### Submodule: `gnuradio4` → `gretel/mitradio4`

User-maintained fork of `fair-acc/gnuradio4` (C++23 GR rewrite, GSI/FAIR
particle accelerator origin). Branch `rebase/upstream-20260420`, HEAD
`e990a22 sdr: SoapySource/Sink hardening + LoopbackDevice observability`.
**11 commits ahead of upstream/main.** Branches: `feature/upstream-soapy`,
`pr754`, `arm64-clang22`, `wip/decoder-canonical`, `backup-*` snapshots.

### Tree

```
apps/        4 binaries + shared infra (~4400 LoC)
blocks/      gr4 block library (~7600 LoC, 37 .hpp files)
benchmarks/  3 google-bench microbenches
test/        30 qa_lora_*.cpp + qa_soapy_loopback.cpp
tests/       66 pytest files (Python)
scripts/     userland Python (lora.* package, 70 files) + Lua wireshark dissector
docs/        git-history.txt (500 KB), superpowers/
data/        runtime artifacts (DuckDB, audio captures, meshcore)
gnuradio4/   submodule (the C++23 GR fork)
patches/     ad9361-overclock.patch (61.44 → 122.88 MS/s)
```

### Apps

| File | LoC | Role |
|---|---|---|
| `lora_scan.cpp` | 1361 | Wideband spectral scanner |
| `config.cpp` / `config.hpp` | 845+245 | TOML config |
| `lora_trx.cpp` | 787 | Full-duplex TX/RX transceiver |
| `graph_builder.hpp` | 540 | Reusable flowgraph builders |
| `tx_worker.cpp/hpp` | 235+55 | TX request worker (LBT, ACK/NACK) |
| `udp_state.hpp` | 184 | CBOR/UDP client mgmt + broadcast/subscribe |
| `common.hpp` | 135 | signal handlers, terminate handler, hw probe |

`lora_trx` topology:

```
1-ch RX: SoapySimpleSource → Splitter → MultiSfDecoder(per BW) → FrameSink
2-ch RX: SoapyDualSource → Splitter×2 → MultiSfDecoder×2 → FrameSink
TX:       bounded request queue → worker thread → ephemeral graph
                                  LBT defers TX until clear
                                  CBOR `lora_tx` requests → IQ → ACK/NACK
```

`MultiSfDecoder` decodes all SFs (7-12) simultaneously on each channel.

`lora_scan` topology:

```
SoapySimpleSource(L1 rate, e.g. 16 MS/s) → Splitter
    → SpectrumTapBlock → NullSink           # L1 wideband FFT energy
    → CaptureSink                           # L2 on-demand capture
Orchestrator thread:
    L1: wideband FFT energy → candidate channels
    L2: per-channel CAD dwell → SF/preamble detection
```

### Blocks (`blocks/include/gnuradio-4.0/lora/`)

Top-level GR blocks (registered via `gr_add_block_library`):

| Block | LoC | Purpose |
|---|---|---|
| `MultiSfDecoder.hpp` | 735 | All-SF parallel decode |
| `FrameSink.hpp` | 640 | Frame collector + UDP fanout |
| `phy/PreambleSync.hpp` | 559 | Full Xhonneux §6 iterative sync |
| `ScanController.hpp` | 542 | L1/L2 scanner state machine |
| `ChannelActivityDetector.hpp` | 504 | CAD |
| `Splitter.hpp` | 90 | Tee |
| `TxQueueSource.hpp` | 90 | Producer for TX worker |
| `ScanSink.hpp` | 107 | Scan emitter |
| `CaptureSink.hpp` | 83 | On-demand capture |
| `SpectrumTap.hpp` | 167 | FFT energy tap |
| `tx_burst_taper.hpp` | 79 | Burst windowing |

Algorithm helpers (`algorithm/`, framework-independent):
`Telemetry.hpp` 211 (property_map → CBOR), `tx_chain.hpp` 206,
`utilities.hpp` 208, `L1Detector.hpp` 237, `PreambleId.hpp` 406,
`HalfBandDecimator.hpp` 448, `interleaving.hpp` 135, `hamming.hpp` 141,
`crc.hpp` 120, `SpectrumTap.hpp` 116, `Channelize.hpp` 100,
`DCBlocker.hpp` 98, `RingBuffer.hpp` 87, `SfLaneDetail.hpp` 65,
`tables.hpp` 18, `GrayPartition.hpp` 44.

PHY (`phy/`): `DecodeChain.hpp` 296 (header+payload pipeline post-demod),
`CssDemod.hpp` 165 (per-symbol dechirp+FFT+argmax, hard + soft),
`Types.hpp` 87, `AntennaCombiner.hpp` 81.

Detail: `FftPool.hpp`, `ChirpRefs.hpp`. Scan: `VangelistaThreshold.hpp`.

Plumbing: `cbor.hpp` 397 (hand-rolled single-header CBOR encoder/decoder
RFC 8949), `log.hpp` 105.

### Tests

C++ qa suite (~28 files): `qa_lora_multisf` (40 KB), `qa_lora_scan` (27 KB),
`qa_lora_tx` (24 KB), `qa_lora_framesink` (25 KB), `qa_lora_rx` (19 KB),
`qa_lora_preamble_sync` (17 KB), `qa_lora_config` (16 KB),
`qa_soapy_loopback` (16 KB), `qa_lora_cbor` (13 KB),
`qa_lora_decode_chain` (12 KB), `qa_lora_tx_queue` (12 KB), and 17 more.

Pytest suite (66 files) mirrors `lora.*` Python package — core, identity,
aggregator, bridges/meshcore, hwtests, storage, viz, tools.

Benchmarks: `bm_lora_phy.cpp`, `bm_lora_decode.cpp`, `bm_lora_algorithms.cpp`
(33 KB total) — google-benchmark on production PHY hot path.

### Userland Python: `lora.*` package (`scripts/src/lora/`)

70 Python files. The userland stack.

```
core/        cbor_stream, config_typed (17 KB), formatters (14 KB),
             schema (24 KB), types, udp, meshcore_crypto (21 KB),
             meshcore_uri, constants, logging
identity/    store, watch                          # crypto identity mgmt
aggregator/  diversity                             # multi-RX combining
bridges/
  meshcore/  companion, protocol, cli, repeater,   # MeshCore mesh bridge
             driver, state
  serial/    -                                       # serial bridge
hwtests/     matrix, harness, decode_test, tx_test, # hardware test harness
             bridge_test, bridge_live, scan_perf,
             cli, transmit_test, trx_perf, scan_test, report
storage/     duckdb_writer, schema, _lora_frame_row
viz/         waterfall                              # spectrum viz
tools/       wav, meshcore_tx, migrate_meshcore_json
__init__.py + cli.py (6.6 KB)                       # `lora ...` CLI
```

`scripts/wireshark/lora_trx.lua` (17 KB) — Wireshark dissector for the UDP
CBOR protocol. Frame types: `lora_frame`, `lora_tx`, `scan_spectrum`,
`scan_detection`, `wideband_sweep`. Auto-tagged with ISO 8601 `"ts"` field.

## 3. Comparison

### Runtime

| Axis | FutureSDR | gr4-lora (gnuradio4) |
|---|---|---|
| Lang | Rust 2024 | C++23 |
| Concurrency | async (async-executor / smol / WASM) | C++ threads + GR4 lock-free scheduler |
| Buffers | vmcircbuffer / slab / circuit / GPU / Zynq | GR4 ring buffers (zero-copy template dispatch) |
| Block dispatch | trait + macro-derived KernelInterface | template (no virtual in data path) |
| Hardware | seify (Soapy/RTL/HackRF/Aaronia) | `gr::blocks::sdr::SoapySource/Sink` (over Soapy) |
| Origin | FutureSDR project (~independent academic) | fair-acc/GNU Radio Foundation, GSI accelerator-grade |
| API stability | declared "not a goal" | upstream GR4 still WIP, gretel fork ahead |
| Targets | Linux/macOS/Win/WASM/Android/Zynq | Linux/macOS, armv7/aarch64 cross via tezuka_fw |
| GPU | first-class (Vulkan, WGPU, Burn) | none (yet) |
| Web | yes (Prophecy GUI, WASM blocks) | no |

### LoRa scope

| Feature | FutureSDR `examples/lora/` | gr4-lora |
|---|---|---|
| Origin | port of gr-lora_sdr (EPFL) | reimplementation on GR4 |
| RX | yes (single-mode, soft + hard decision) | MultiSfDecoder all SF7-12 in parallel |
| TX | yes (basic Transmitter block + tx.rs bin) | full-duplex TX worker queue with LBT, ACK/NACK |
| Channels | 1 active | 1 or 2 (DualSource) simultaneous |
| Wideband scan | no | `lora_scan` 16 MS/s L1+L2 channelizer |
| Sync | DecoderState{Detect,Sync,SfoCompensation} | full Xhonneux §6 iterative (PreambleSync) |
| Multi-antenna | no | AntennaCombiner.hpp |
| Control plane | `BlobToUdp` raw blobs | CBOR (RFC 8949) over UDP, schema, Wireshark |
| Protocol stack | meshtastic + Semtech packet forwarder | MeshCore (own crypto+protocol+bridge), DuckDB telemetry |
| Tests | 23 .rs files in example | 30 C++ qa_* + 66 pytest + 3 google-bench |
| Hardware chases | seify abstraction | DC spur fix, AD9361 overclock patch, B210 quirks, lo_offset |
| GUI | Prophecy/Leptos generic | viz/waterfall.py |
| Storage | none | DuckDB lora_frames.duckdb |

### One-line summary

- **FutureSDR**: a *runtime*. LoRa is one of 26 example DSP demos.
- **gr4-lora**: a *product*. LoRa transceiver + scanner + protocol stack +
  telemetry + harness, riding on a vendored gnuradio4 fork.

## 4. State of development

- **FutureSDR**: active, latest 2026-05-02. Pre-1.0 with explicit
  "API stability not a goal". Recent work dominated by docs and runtime API
  refactor. LoRa example mature, not actively grown.
- **gr4-lora**: active, latest 2026-05-02. Squashed top-line history hides
  real activity in branches + submodule. Single author. Production-grade test
  footprint (96 test files). Hardware-oriented (B210, B220 Mini, PlutoSDR).
  Recent direction: daemon supervisor, B210 stock-UHD compat, DC-blocker cutoff
  fix, lora.* package layout — moving toward operational deployment.

## 5. Feature set (gr4-lora unique value vs FutureSDR LoRa)

1. MultiSfDecoder — 6 SFs in lockstep on one stream.
2. lora_scan wideband scanner — 16 MS/s channelization + L1 energy + L2 CAD.
3. Xhonneux full iterative sync — DetectU1U3 / EstimateU4U6 / 5-bin λ̂_CFO.
4. Full-duplex TX worker — bounded request queue, LBT, ephemeral TX graph,
   separate device handle.
5. CBOR/UDP control plane — `lora_frame`, `lora_tx`, `scan_*`, plus Wireshark
   dissector and Python `lora.core.cbor_stream`.
6. MeshCore integration — bridges/meshcore/, core/meshcore_crypto.py
   (Ed25519/X25519/AES-128-ECB+HMAC).
7. Identity/key management — identity/store.py, identity/watch.py.
8. Diversity aggregation — aggregator/diversity.py.
9. DuckDB telemetry sink — storage/duckdb_writer.py.
10. Hardware test harness — hwtests/ 13 files.
11. Visualization — viz/waterfall.py.
12. AD9361 overclock patch — UHD source patch raising max sample rate to
    122.88 MS/s.
13. TOML config — shared between trx and scan, plus config-pluto.toml.
14. Daemon supervisor.

## 6. Use cases (imagined)

### gr4-lora

- MeshCore gateway — multi-SF decode + identity/crypto + mesh bridge; deploy
  as PlutoSDR-resident daemon (tezuka_fw armv7 build pipeline lives in
  workspace).
- Spectrum surveillance — `lora_scan` for ISM band site survey; classifies
  SF/preamble/CFO/SNR; data lands in DuckDB for offline analysis.
- Protocol research — full-duplex transceiver with timed-burst TX, suitable
  for LoRaWAN class B/C, MeshCore protocol R&D, collision-decoding evaluation.
- Field replay/test infra — pytest harness drives matrix tests over hardware.
- Edge SDR appliance — submodule fork already cross-builds armv7/aarch64;
  roadmap → IIO direct on FISH Ball PlutoSDR.

### FutureSDR

- Cross-platform SDR research framework — Rust with GPU/WASM/Android/embedded.
- Protocol PHY teaching/demos — wlan, zigbee, m17, adsb, lora, rattlegram.
- WASM-deployable SDR demos — browser-resident receivers.
- GPU-accelerated DSP pipelines — Vulkan/WGPU/Burn buffers in data plane.
- Embedded/FPGA prototyping — Zynq DMA path (Linux only).
