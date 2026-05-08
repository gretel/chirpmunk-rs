# Basic UI — Spectrum + Frame Log

Status: Draft. Author: Tom + agent. Date: 2026-05-08.

## Goal

Single web page that shows live spectrum/waterfall + a rolling frame
log. Operator can open `http://localhost:8088/` and see what the
radio is hearing. "Basic" = no auth, no replay, no TX controls,
read-only.

## Background

### gr4-lora reference

- `apps/lora_trx.cpp` — `SpectrumState` tap fed by FFT samples; CBOR
  emitted as `scan_spectrum` (in chirpmunk-scan, not chirpmunk-trx).
- gr4-lora has no native UI. Consumers are Python tools (`lora_mon`,
  `lora_duckdb`) reading UDP CBOR.

### chirpmunk current state

- `chirpmunk-trx` daemon: emits `lora_frame`, `lora_tx_ack`,
  `subscribe`-driven keepalives. No `scan_spectrum`.
- `chirpmunk-scan` (M4): emits `scan_spectrum`, `scan_detection`,
  `wideband_sweep`. CBOR over UDP.
- `chirpmunk-cbor`: encode/decode for all the above.
- No web stack in workspace today.

## Design

### Components

1. **New crate `chirpmunk-ui`** — binary `chirpmunk-ui-server`.

   Cargo deps:
   - `axum` (HTTP server, WebSocket).
   - `tokio` (already pinned by workspace).
   - `chirpmunk-cbor`, `chirpmunk-udp` (workspace).
   - `serde_json` (CBOR → JSON re-encode for browser).
   - `tracing` / `tracing-subscriber` (already in workspace).

2. **HTTP server** —
   - `GET /` → static HTML/JS/CSS bundle, embedded via `include_bytes!`
     (single page, ~300 lines total).
   - `GET /ws` → WebSocket: forwards decoded events as JSON.

3. **UDP→WS bridge** — UDP subscriber thread reads CBOR, decodes via
   `chirpmunk-cbor::peek_type` + per-type decode, re-encodes as JSON,
   forwards through a `tokio::sync::broadcast` channel to all WS
   clients. Drops on backpressure (ring-buffer behaviour; UI is
   best-effort).

4. **Spectrum tap in `chirpmunk-trx`** — to feed the UI, the trx
   daemon must emit `scan_spectrum` periodically:
   - New block `chirpmunk-blocks::spectrum_tap::SpectrumTap`. Stream
     in `Complex<f32>`. FFT size 1024 (configurable). Emits a
     `scan_spectrum` CBOR every `interval_ms` (default 100 ms) via the
     existing UDP server.
   - Wire on the same `StreamDuplicator` tap as the multi-SF chains
     (or extend the duplicator by 1 — already grew to 7 in Spec 1
     for CAD; would grow to 8 here).
   - Config: `[trx.spectrum] enabled: bool = true`,
     `fft_size: u16 = 1024`, `interval_ms: u32 = 100`.

5. **Frontend** — single HTML file.
   - **Top panel**: waterfall canvas. ~200 history rows. Colormap:
     viridis (precomputed lookup). Frequency axis derived from
     `scan_spectrum.freq` and `sample_rate`.
   - **Bottom panel**: rolling frame log table. Columns: time, freq,
     sf, bw, sync, payload (hex, truncated 32 bytes), snr, crc.
     Rows added on `lora_frame` events. Cap 500 rows; FIFO eviction.
   - WS protocol: JSON text messages, one event per message:

         {"type":"lora_frame", "ts":..., "phy":..., ...}
         {"type":"scan_spectrum", "freq":..., "magnitudes":[...]}

6. **Config** —
   - `chirpmunk-ui-server` CLI args (no config file for v1):
     `--bind 127.0.0.1:8088`, `--trx-udp 127.0.0.1:5557`,
     `--subscribe-sync-words "0x12,0x34"` (comma-separated; default
     all).
   - chirpmunk-trx gains `[trx.spectrum]` section as above.

### Data flow

    chirpmunk-trx
       ├── lora_frame   ──┐
       └── scan_spectrum ─┤  UDP CBOR
                           ↓
       chirpmunk-ui-server (subscriber)
       ├── HTTP GET /  → static HTML+JS bundle
       └── WS /ws      → JSON stream  → browser canvas + table

### Error handling

- chirpmunk-trx not running → UDP socket open succeeds (it's a
  client), but no events arrive. WS clients see no events. JS
  reconnect logic on WS close (5 s backoff).
- WS client disconnect → drop from broadcast tally. No state loss for
  other clients.
- Browser refresh → fresh empty waterfall (no replay; this is "basic").
- chirpmunk-trx restarts (re-subscribe needed) → UDP server detects
  new client on the next subscribe; `chirpmunk-ui-server` re-issues
  subscribes on its end too. Already covered by the existing 5 s
  keepalive pattern.

### Testing

1. Unit: CBOR→JSON conversion edge cases (NaN, missing keys,
   `Infinity` from `determine_snr`). Already filtered in
   `FrameSink::Telemetry::from_map`; UI side just trusts the wire
   format.
2. Integration: spawn `chirpmunk-trx --loopback` + `chirpmunk-ui-server`
   + a synthetic frame producer; connect a WS client; assert one
   `lora_frame` JSON message received.
3. Manual smoke: open browser at `http://localhost:8088/`, fire
   loopback TX, observe waterfall update + frame appear in log.

## Approach trade-offs

| Option | Description | Verdict |
|---|---|---|
| (i) Web (axum + vanilla JS, embedded HTML) | No client install. Cross-platform. CBOR→JSON is one boundary. ~300 lines JS | **Recommended** |
| (ii) TUI (`ratatui`) | Faster over SSH. No browser. But waterfall in terminal is ugly (block characters); not "basic and good-looking" | Defer — useful for SSH-only ops as v2 |
| (iii) Native (`egui`/`iced`) | Best perf. But Rust GUI deps are heavyweight (~5 MB binary growth); cross-platform builds harder; one-window-only | Reject for v1 |

## Open questions / assumptions

1. Spectrum data source: trx must emit `scan_spectrum`. Adding
   `SpectrumTap` block extends the StreamDuplicator. Alternative: have
   `chirpmunk-ui-server` open its own SoapyDirectSource — rejected,
   would contend with trx for the device.
2. WS message format = JSON text. CPU-cheap for v1. Switch to CBOR-binary
   later if `scan_spectrum` (1024 floats × 10 Hz = ~80 kB/s/channel)
   becomes a bottleneck.
3. Auth: none. Bind defaults to `127.0.0.1` for v1. Reverse-proxy
   (nginx/caddy) for remote ops.
4. No persistence: frame log lives in browser memory only. DuckDB
   integration via `lora_duckdb` is out of scope (separate tool).
5. `--subscribe-sync-words` filter applies to `lora_frame` only;
   `scan_spectrum` is not sync-filtered (it's pre-decode).
6. FFT size 1024 fixed default — most operators won't tune this.
   Configurable for power users.

## License

GPL-3.0-only. axum is MIT/Apache-2.0 — compatible. Embedded JS is
fresh code under chirpmunk's GPL-3.0-only.

## Next

Spec review by operator. On approval → invoke `writing-plans` skill.
This is the most decoupled of the four; it can be implemented in
parallel with the others by a separate session, since the only
dependency is the `scan_spectrum` emitter in chirpmunk-trx.
