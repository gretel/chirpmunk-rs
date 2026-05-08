# UI Handoff — adopt prophecy as shipped

Status: Phases A, B.1, B.2 all done and verified in-session. Phase
C deferred. Author: Tom + agent. Date: 2026-05-08.

## Verification log (2026-05-08)

- Phase A — confirmed live. `chirpmunk-trx --loopback` exposes the
  generic prophecy GUI at `http://127.0.0.1:1337/`. `curl /api/fg/`
  returns `[0]` and `curl /` returns
  `<title>FutureSDR :: Prophecy GUI</title>`. No code change.
- Phase B.1 — landed in commit `728b8b3 feat(trx,config): spectrum
  tap WS feed for prophecy waterfall`. The `[trx.spectrum]` TOML
  section gates an FFT → magnitude → moving-avg → WebsocketSink
  branch off `entry_dup.outputs[2]`. Smoke test with `enabled =
  true, ws_port = 9101`: `lsof -iTCP:9101 -sTCP:LISTEN` confirms
  `chirpmunk-trx` listening, log line `spectrum tap enabled
  ws_port=9101 fft_size=2048` present. `entry_dup` was bumped from
  `<Complex32, 2>` to `<Complex32, 3>`; the third output is wired
  to a `NullSink` when the section is absent or `enabled = false`.
- Phase B.2 — chirpmunk-ui crate scaffolded at
  `/Users/tom/src/uhd/chirpmunk/crates/chirpmunk-ui/`. Detached from
  the chirpmunk Cargo workspace (own `[workspace]` stanza, builds
  via Trunk for `wasm32-unknown-unknown` only). Uses prophecy as a
  Leptos component library. `trunk build --release` succeeded:
  `dist/index.html` (1.2 K), `dist/frontend-…_bg.wasm` (813 K),
  `dist/frontend-…js` (54 K), `dist/style-…css` (19 K). Smoke test
  with `FUTURESDR_FRONTEND_PATH=…/chirpmunk-ui/dist` env var: `curl
  http://127.0.0.1:1337/` returns `<title>chirpmunk :: LoRa</title>`,
  ports 1337 (HTTP) + 9101 (spectrum WS) both listening. Required
  toolchains — `brew install trunk` (0.21.14), `rustup toolchain
  install nightly --component rust-src`, `rustup target add
  wasm32-unknown-unknown --toolchain nightly`. The `nightly` channel
  is needed because `prophecy` enables Leptos's `nightly` feature
  (signal-as-function call shorthand). `rust-toolchain.toml` in the
  crate pins the channel locally.



Supersedes — `2026-05-08-ui-spectrum-design.md` and
`2026-05-08-ui-spectrum-plan.md`. Those propose a brand-new
`chirpmunk-ui` crate with axum HTTP server, embedded HTML/JS,
`SpectrumTap` block and UDP→WS bridge. About 80 % of that work
disappears once we use prophecy directly. The original docs stay
in-tree for reference; delete after the next session confirms this
handoff.

## Why the pivot

FutureSDR ships a complete WASM web GUI (`prophecy`) plus a built-in
HTTP control port that serves it. `chirpmunk-trx` already starts a
FutureSDR `Runtime`, so the GUI is one default away from being live
on `http://127.0.0.1:1337/`. Verified defaults from
`/Users/tom/src/uhd/FutureSDR/src/runtime/config.rs`:

```
ctrlport_enable:  true
ctrlport_bind:    "127.0.0.1:1337"
frontend_path:    None  →  fallback /Users/tom/src/uhd/FutureSDR/crates/prophecy/dist
```

`prophecy/dist/` is pre-built in tree:

```
/Users/tom/src/uhd/FutureSDR/crates/prophecy/dist/index.html
/Users/tom/src/uhd/FutureSDR/crates/prophecy/dist/prophecy-2adb2b6ef66a8d7_bg.wasm   722 K
/Users/tom/src/uhd/FutureSDR/crates/prophecy/dist/prophecy-2adb2b6ef66a8d7.js         39 K
/Users/tom/src/uhd/FutureSDR/crates/prophecy/dist/style-2395c28b0745a7f1.css          18 K
```

`fallback_service(ServeDir::new(...))` is wired in
`/Users/tom/src/uhd/FutureSDR/src/runtime/ctrl_port.rs` line 137,
relative to FutureSDR's `CARGO_MANIFEST_DIR` baked in at compile
time, so the path resolves at runtime as long as the FutureSDR
checkout stays at `/Users/tom/src/uhd/FutureSDR/`.

## Phase A — zero-code: get the generic GUI running

Goal: open a browser and see the live chirpmunk-trx flowgraph at
`http://127.0.0.1:1337/`.

Steps (no code changes):

1. Build + launch chirpmunk-trx as usual:

   ```
   cd /Users/tom/src/uhd/chirpmunk
   cargo run -p chirpmunk-trx -- --loopback
   ```

2. While it runs, open `http://127.0.0.1:1337/` in a browser.

3. Sanity-check the JSON API:

   ```
   curl http://127.0.0.1:1337/api/fg/                     # list flowgraphs
   curl http://127.0.0.1:1337/api/fg/0/                   # describe fg 0
   curl http://127.0.0.1:1337/api/fg/0/block/0/           # describe block 0
   curl http://127.0.0.1:1337/api/fg/0/block/0/call/HANDLER/   # GET-invoke
   ```

What you get from the generic GUI:

- Live flowgraph topology (block names, ports, connections).
- Per-block JSON description (handlers, port types).
- Invoke any message handler from the UI — e.g. trigger `dispatch_lora_tx`
  interactively from a browser instead of the UDP socket.
- PMT inspector / editor.

What you do NOT get from Phase A:

- Spectrum / waterfall — the generic GUI shell does not include them.
- `lora_frame` log — that data lives on the chirpmunk UDP CBOR plane,
  not on the FutureSDR ctrl-port. prophecy cannot see it.

Open question for Phase A — confirm the bake-in path actually
resolves on the dev machine. If not, set
`frontend_path = /Users/tom/src/uhd/FutureSDR/crates/prophecy/dist`
explicitly via the FutureSDR config file or env var.

Acceptance — open `http://127.0.0.1:1337/`, see the chirpmunk
flowgraph blocks rendered. No diff produced.

## Phase B — small chirpmunk WASM frontend with waterfall

Goal: a chirpmunk-branded page at `http://127.0.0.1:1337/` showing
spectrum + waterfall + the live flowgraph + invoke buttons.

Approach: copy the pattern from
`/Users/tom/src/uhd/FutureSDR/examples/spectrum/`. That example does
exactly what we want — live waterfall, live flowgraph, message
inputs — and it uses prophecy as a Leptos component library, not by
forking it.

### B.1 — flowgraph side (chirpmunk-trx, Rust)

Add an FFT + WebsocketSink branch to the chirpmunk-trx flowgraph
behind a config flag. Reference recipe at
`/Users/tom/src/uhd/FutureSDR/examples/spectrum/src/bin/cpu.rs`:

```rust
use futuresdr::blocks::{Apply, Fft, FftDirection, MovingAvg, WebsocketSinkBuilder, WebsocketSinkMode};

const FFT_SIZE: usize = 2048;
let fft     = Fft::with_options(FFT_SIZE, FftDirection::Forward, true, None);
let mag_sqr = Apply::new(|x: &Complex32| x.norm_sqr());
let avg     = MovingAvg::<FFT_SIZE>::new(0.1, 3);
let snk     = WebsocketSinkBuilder::<f32>::new(9001)
    .mode(WebsocketSinkMode::FixedBlocking(FFT_SIZE))
    .build();
connect!(fg, src.outputs[N] > fft > mag_sqr > avg > snk);
```

Concrete wiring inside chirpmunk-trx (file
`/Users/tom/src/uhd/chirpmunk/apps/chirpmunk-trx/src/main.rs`):

- Bump the existing `StreamDuplicator<Complex32, 2>` (line 308) to
  `<Complex32, 3>`. Output 0 → frame_sync, 1 → CAD, 2 → spectrum chain.
- Gate behind `[trx.spectrum] enabled = true, fft_size = 2048,
  ws_port = 9001` in `chirpmunk-config`. When disabled, pin the third
  output to a `NullSink<Complex32>` so the duplicator stays drained.

Cost — about one screenful of Rust plus one TOML field. No new block,
all four blocks (`Fft`, `Apply`, `MovingAvg`, `WebsocketSinkBuilder`)
are stock FutureSDR.

### B.2 — frontend side (new `crates/chirpmunk-ui/`)

Mirror the layout of the spectrum example:

```
/Users/tom/src/uhd/FutureSDR/examples/spectrum/Cargo.toml          ← workspace, lib + bin
/Users/tom/src/uhd/FutureSDR/examples/spectrum/Trunk-web.toml      ← trunk build target
/Users/tom/src/uhd/FutureSDR/examples/spectrum/index-web.html      ← Trunk entry
/Users/tom/src/uhd/FutureSDR/examples/spectrum/src/wasm/frontend.rs   ← prophecy <Spectrum/> + <Gui/>
/Users/tom/src/uhd/FutureSDR/examples/spectrum/src/wasm/web.rs        ← wasm entry shim
/Users/tom/src/uhd/FutureSDR/examples/spectrum/src/bin/web.rs         ← wasm-pack-style binary
```

Action — create `/Users/tom/src/uhd/chirpmunk/crates/chirpmunk-ui/`
(detach from the chirpmunk Cargo workspace; chirpmunk-ui builds via
Trunk only, cdylib + WASM target). Copy the spectrum frontend
verbatim, then strip controls that are not relevant to chirpmunk
(sample-rate radio buttons, freq slider) or rebind them to
chirpmunk's own message handlers (`dispatch_lora_tx`, etc.).

Build:

```
cd /Users/tom/src/uhd/chirpmunk/crates/chirpmunk-ui
trunk build --release
```

Output goes to `/Users/tom/src/uhd/chirpmunk/crates/chirpmunk-ui/dist/`.

Tell chirpmunk-trx to serve that bundle by setting
`frontend_path` via env var (FutureSDR reads this through
`/Users/tom/src/uhd/FutureSDR/src/runtime/config.rs`):

```
FUTURESDR_frontend_path=/Users/tom/src/uhd/chirpmunk/crates/chirpmunk-ui/dist \
  cargo run -p chirpmunk-trx -- --loopback
```

(Confirm the exact env-var spelling against `config.rs`; the file
shows `frontend_path` as a config key. If there is no env-var
hookup, write a `chirpmunk-trx.toml` under `~/.config/futuresdr/` —
both paths are read at runtime by the FutureSDR config layer.)

Acceptance — browser opens `http://127.0.0.1:1337/`, sees the
chirpmunk page with live waterfall (fed via `ws://localhost:9001`)
and the chirpmunk flowgraph rendered through prophecy components.

Cost — pretty small. Most of the WASM frontend is copy-paste from
`examples/spectrum/src/wasm/frontend.rs` (392 lines). Removing
SDR-specific knobs trims it. New work is mostly the Trunk plumbing
plus styling.

## Phase C — deferred: lora_frame log integration

Two ways to surface the chirpmunk UDP CBOR plane in the GUI later:

1. Bridge inside chirpmunk-trx — a new tokio task subscribes to its
   own UDP server on `127.0.0.1:5556`, decodes `lora_frame`s,
   forwards them as JSON over a custom axum route mounted on the
   ctrl-port via `Router::merge` (FutureSDR already supports
   `custom_routes` — see `ControlPort::new(handle, routes)` in
   `/Users/tom/src/uhd/FutureSDR/src/runtime/ctrl_port.rs:102`).
2. Stay on UDP — keep `lora_mon` (Python CLI) as the frame-log
   surface; the GUI is for live RF + flowgraph control only.

Recommend deferring until Phase B is in front of an operator. Most
real frame-log workflows want filtering, persistence and history
that a DuckDB-backed Python tool already does well.

## Files to clean up after handoff

If Phase A confirms the generic GUI is the v1 product:

- Delete `/Users/tom/src/uhd/chirpmunk/docs/superpowers/specs/2026-05-08-ui-spectrum-design.md`.
- Delete `/Users/tom/src/uhd/chirpmunk/docs/superpowers/plans/2026-05-08-ui-spectrum-plan.md`.

Both are superseded; nothing in them is still load-bearing.

## Quick-start cheat sheet

```
# Phase A (zero code, generic prophecy GUI)
cd /Users/tom/src/uhd/chirpmunk
cargo run -p chirpmunk-trx -- --loopback &
open http://127.0.0.1:1337/

# Phase B (chirpmunk-styled GUI with waterfall)
# 1. patch /Users/tom/src/uhd/chirpmunk/apps/chirpmunk-trx/src/main.rs
#    - bump StreamDuplicator<Complex32, 2> → 3
#    - add Fft + Apply + MovingAvg + WebsocketSinkBuilder branch
#    - gate on [trx.spectrum] enabled in chirpmunk-config
# 2. scaffold /Users/tom/src/uhd/chirpmunk/crates/chirpmunk-ui/ from
#    /Users/tom/src/uhd/FutureSDR/examples/spectrum/
# 3. trunk build --release
# 4. FUTURESDR_frontend_path=.../chirpmunk-ui/dist cargo run -p chirpmunk-trx
```

## Risks / open questions

1. **Path bake-in.** prophecy's fallback path is baked into the
   FutureSDR binary at compile time via `CARGO_MANIFEST_DIR`. On
   another machine without `/Users/tom/src/uhd/FutureSDR/` the
   fallback fails — Phase A becomes a JSON-only API. Phase B fixes
   this because we explicitly set `frontend_path`.
2. **Ctrl-port collision.** Default port 1337 may collide with
   another local service. Override with `ctrlport_bind` in the
   FutureSDR config.
3. **Two TCP ports.** Phase B uses :1337 (HTTP) + :9001 (WS for FFT
   data). Document both in the chirpmunk runbook; firewall both for
   remote access via reverse proxy.
4. **Backpressure on FFT WS.** `MovingAvg::new(0.1, 3)` from the
   spectrum example is tuned for 3.2 MS/s. At chirpmunk RX rates
   (typically 500 kS/s – 1 MS/s) the spectrum update rate may be
   too slow or too fast. Pick `MovingAvg` parameters experimentally
   when wiring B.1.

## Pointers

- prophecy crate — `/Users/tom/src/uhd/FutureSDR/crates/prophecy/`
- prophecy main shell — `/Users/tom/src/uhd/FutureSDR/crates/prophecy/src/main.rs`
- prophecy reusable components — `/Users/tom/src/uhd/FutureSDR/crates/prophecy/src/{waterfall,time_sink,constellation_sink,flowgraph_canvas,flowgraph_table,pmt}.rs`
- ctrl-port server impl — `/Users/tom/src/uhd/FutureSDR/src/runtime/ctrl_port.rs`
- FutureSDR config defaults — `/Users/tom/src/uhd/FutureSDR/src/runtime/config.rs`
- spectrum example (clone source) — `/Users/tom/src/uhd/FutureSDR/examples/spectrum/`
- spectrum example WASM frontend — `/Users/tom/src/uhd/FutureSDR/examples/spectrum/src/wasm/frontend.rs`
- chirpmunk-trx flowgraph — `/Users/tom/src/uhd/chirpmunk/apps/chirpmunk-trx/src/main.rs`
- chirpmunk-config TrxReceive section — `/Users/tom/src/uhd/chirpmunk/crates/chirpmunk-config/src/lib.rs`
