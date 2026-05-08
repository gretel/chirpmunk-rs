# chirpmunk-ui

WASM frontend for `chirpmunk-trx`. Adopts FutureSDR's
[`prophecy`](../../../FutureSDR/crates/prophecy/) GUI components as a
library: `Waterfall`, `TimeSink`, `FlowgraphCanvas`, `FlowgraphTable`,
`PmtEditor`. Connects in-browser to:

- `http://127.0.0.1:1337/api/fg/0/` — chirpmunk-trx ControlPort (REST).
- `ws://127.0.0.1:9001` — spectrum WebSocket (FFT magnitudes from the
  `[trx.spectrum]` tap).

Detached from the chirpmunk Cargo workspace; builds via Trunk for the
`wasm32-unknown-unknown` target only.

## Prerequisites

```sh
brew install trunk          # Trunk static-asset bundler
rustup toolchain install nightly --component rust-src
rustup target add wasm32-unknown-unknown --toolchain nightly
```

The `nightly` channel is required because `prophecy` enables Leptos's
`nightly` feature (signal-as-function call shorthand).
`rust-toolchain.toml` in this crate pins the channel automatically.

## Build

```sh
cd /Users/tom/src/uhd/chirpmunk/crates/chirpmunk-ui
trunk build --release           # output: ./dist/
```

## Run

Start `chirpmunk-trx` with `[trx.spectrum] enabled = true` in its TOML
and point FutureSDR at the built dist:

```sh
FUTURESDR_FRONTEND_PATH=/Users/tom/src/uhd/chirpmunk/crates/chirpmunk-ui/dist \
  cargo run -p chirpmunk-trx -- --config /path/to/your/config.toml
```

Browse `http://127.0.0.1:1337/`.

## Hacking

```sh
trunk serve                     # dev server with hot reload
```
