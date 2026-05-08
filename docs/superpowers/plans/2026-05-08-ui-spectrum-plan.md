# UI Spectrum Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Single-page web UI showing live waterfall + frame log. Read-only consumer of `chirpmunk-trx` UDP CBOR.

**Architecture:** New crate `chirpmunk-ui`. Binary `chirpmunk-ui-server` runs an axum HTTP server on `127.0.0.1:8088`, subscribes to chirpmunk-trx UDP, decodes CBOR, re-encodes as JSON, broadcasts over WebSocket to all connected browsers. Frontend = single embedded HTML/JS/CSS file with vanilla JS canvas waterfall + table. New `SpectrumTap` block in `chirpmunk-blocks` emits `scan_spectrum` from chirpmunk-trx every 100 ms.

**Tech Stack:** Rust 2024, axum 0.7+, tokio, `chirpmunk-cbor`, `chirpmunk-udp`, `serde_json`, `rustfft` (for SpectrumTap). Frontend: vanilla JS (no framework), HTML5 canvas.

---

## File structure

| File | Status | Responsibility |
|---|---|---|
| `crates/chirpmunk-ui/Cargo.toml` | **CREATE** | New workspace member. |
| `crates/chirpmunk-ui/src/lib.rs` | **CREATE** | Re-exports for tests. |
| `crates/chirpmunk-ui/src/server.rs` | **CREATE** | axum app: GET / + GET /ws. |
| `crates/chirpmunk-ui/src/bridge.rs` | **CREATE** | UDP→broadcast bridge: subscribe, decode, fanout. |
| `crates/chirpmunk-ui/src/json_translate.rs` | **CREATE** | CBOR `lora_frame`/`scan_spectrum`/`lora_tx_ack` → JSON. |
| `crates/chirpmunk-ui/static/index.html` | **CREATE** | Single-file UI (HTML+CSS+JS). Embedded via `include_str!`. |
| `crates/chirpmunk-ui/src/main.rs` | **CREATE** | CLI: `--bind`, `--trx-udp`, `--subscribe-sync-words`. |
| `crates/chirpmunk-blocks/src/spectrum_tap.rs` | **CREATE** | New block: FFT magnitudes → CBOR `scan_spectrum` over the existing UDP server. |
| `crates/chirpmunk-blocks/src/lib.rs` | MODIFY | `pub mod spectrum_tap; pub use spectrum_tap::{SpectrumTap, SpectrumTapConfig};` |
| `apps/chirpmunk-trx/src/main.rs` | MODIFY | Wire SpectrumTap on the same StreamDuplicator side-tap as CAD. |
| `crates/chirpmunk-config/src/lib.rs` | MODIFY | New `[trx.spectrum]` section. |
| `Cargo.toml` (workspace root) | MODIFY | Add `crates/chirpmunk-ui` to members. |

---

## Task 1: SpectrumTap block (in chirpmunk-blocks)

- [ ] **Step 1.1: Add config** to `chirpmunk-config`:

```rust
#[derive(serde::Deserialize, Debug, Clone)]
pub struct TrxSpectrum {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_fft_size")]
    pub fft_size: u16,
    #[serde(default = "default_interval_ms")]
    pub interval_ms: u32,
}
fn default_fft_size() -> u16 { 1024 }
fn default_interval_ms() -> u32 { 100 }
```

In `Trx`: `pub spectrum: Option<TrxSpectrum>`.

- [ ] **Step 1.2: Write block test**

`crates/chirpmunk-blocks/tests/spectrum_tap_unit.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only

use chirpmunk_blocks::{SpectrumTap, SpectrumTapConfig};
use futuresdr::num_complex::Complex32;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

#[tokio::test]
async fn spectrum_tap_emits_scan_spectrum_at_interval() {
    let (tx, mut rx) = unbounded_channel::<Vec<u8>>();
    let cfg = SpectrumTapConfig {
        fft_size: 64,
        interval_ms: 10,
        sample_rate_hz: 1_000_000.0,
        center_freq_hz: 868_000_000.0,
    };
    let mut tap = SpectrumTap::new(cfg, tx);
    // Hand-feed 1000 samples (enough for ~15 FFT windows)
    let buf = vec![Complex32::new(0.5, 0.5); 1000];
    tap.feed_for_test(&buf);
    // Allow timer ticks
    tokio::time::sleep(Duration::from_millis(50)).await;
    let bytes = rx.try_recv().expect("at least one CBOR frame");
    let parsed = chirpmunk_cbor::peek_type(&bytes).unwrap();
    assert_eq!(parsed, "scan_spectrum");
}
```

- [ ] **Step 1.3: Implement SpectrumTap**

`crates/chirpmunk-blocks/src/spectrum_tap.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only

//! Periodic FFT magnitude tap that emits CBOR `scan_spectrum` for the UI.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chirpmunk_cbor;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use rustfft::{Fft, FftPlanner};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
pub struct SpectrumTapConfig {
    pub fft_size: u16,
    pub interval_ms: u32,
    pub sample_rate_hz: f64,
    pub center_freq_hz: f64,
}

#[derive(Block)]
pub struct SpectrumTap {
    cfg: SpectrumTapConfig,
    out: UnboundedSender<Vec<u8>>,
    fft: Arc<dyn Fft<f32>>,
    fft_buf: Vec<Complex32>,
    fft_fill: usize,
    last_emit: Instant,
    #[input]
    pub input: PortIn<Complex32>,
}

impl SpectrumTap {
    pub fn new(cfg: SpectrumTapConfig, out: UnboundedSender<Vec<u8>>) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(cfg.fft_size as usize);
        let n = cfg.fft_size as usize;
        Self {
            fft,
            fft_buf: vec![Complex32::default(); n],
            fft_fill: 0,
            last_emit: Instant::now(),
            cfg,
            out,
            input: PortIn::default(),
        }
    }

    #[cfg(test)]
    pub fn feed_for_test(&mut self, samples: &[Complex32]) {
        self.absorb(samples);
    }

    fn absorb(&mut self, samples: &[Complex32]) {
        let n = self.cfg.fft_size as usize;
        for s in samples {
            self.fft_buf[self.fft_fill] = *s;
            self.fft_fill += 1;
            if self.fft_fill == n {
                self.fft.process(&mut self.fft_buf);
                if self.last_emit.elapsed() >= Duration::from_millis(self.cfg.interval_ms as u64) {
                    self.emit_cbor();
                    self.last_emit = Instant::now();
                }
                self.fft_fill = 0;
            }
        }
    }

    fn emit_cbor(&self) {
        let n = self.cfg.fft_size as usize;
        let mut mags = Vec::<f32>::with_capacity(n);
        for c in &self.fft_buf {
            mags.push((c.re * c.re + c.im * c.im).sqrt());
        }
        // Encode CBOR scan_spectrum:
        //   { "type": "scan_spectrum",
        //     "freq": center,
        //     "sample_rate": rate,
        //     "fft_size": n,
        //     "magnitudes": [..f32..] }
        let mut buf = Vec::with_capacity(8 + n * 4);
        chirpmunk_cbor::encode_scan_spectrum(
            &mut buf,
            self.cfg.center_freq_hz,
            self.cfg.sample_rate_hz,
            n as u16,
            &mags,
        );
        let _ = self.out.send(buf);
    }
}

#[async_trait::async_trait]
impl Kernel for SpectrumTap {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        sio: &mut StreamIo,
        _mio: &mut MessageIo<Self>,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let input = sio.input(0).slice::<Complex32>();
        if !input.is_empty() {
            self.absorb(input);
            let n = input.len();
            sio.input(0).consume(n);
        }
        if sio.input(0).finished() {
            io.finished = true;
        }
        Ok(())
    }
}
```

NB: `chirpmunk_cbor::encode_scan_spectrum` doesn't exist yet. Add to `chirpmunk-cbor` in step 1.4.

- [ ] **Step 1.4: Add encode_scan_spectrum to chirpmunk-cbor**

In `crates/chirpmunk-cbor/src/lib.rs`, add a public encoder mirroring the existing manual encoders. Schema:

```cbor
{
  "type":         "scan_spectrum",
  "freq":         f64,
  "sample_rate":  f64,
  "fft_size":     u16,
  "magnitudes":   [f32; fft_size],
}
```

- [ ] **Step 1.5: Run + commit**

```sh
cargo test -p chirpmunk-blocks --test spectrum_tap_unit
cargo build --workspace
git add crates/chirpmunk-blocks/src/spectrum_tap.rs crates/chirpmunk-blocks/src/lib.rs crates/chirpmunk-blocks/tests/spectrum_tap_unit.rs crates/chirpmunk-cbor/src/lib.rs crates/chirpmunk-config/src/lib.rs
git commit -m "feat(blocks,cbor): SpectrumTap block + scan_spectrum encoder"
```

---

## Task 2: Wire SpectrumTap into chirpmunk-trx

**Files:**
- Modify: `apps/chirpmunk-trx/src/main.rs`

- [ ] **Step 2.1: Extend the entry duplicator**

The LBT plan added `StreamDuplicator<2>` (tap0 → frame_sync, tap1 → CAD). For UI we need a 3rd tap → SpectrumTap. Widen to `StreamDuplicator<3>`.

Per chirpmunk-dev skill: connect! requires literal indices → unroll outputs[0], outputs[1], outputs[2].

- [ ] **Step 2.2: Build SpectrumTap and wire**

```rust
let spectrum_cfg = trx_opt
    .and_then(|t| t.spectrum.clone())
    .unwrap_or(TrxSpectrum {
        enabled: true,
        fft_size: 1024,
        interval_ms: 100,
    });
if spectrum_cfg.enabled {
    let cfg = SpectrumTapConfig {
        fft_size: spectrum_cfg.fft_size,
        interval_ms: spectrum_cfg.interval_ms,
        sample_rate_hz: sample_rate,
        center_freq_hz: cfg_opt
            .as_ref()
            .and_then(|c| c.trx.as_ref())
            .and_then(|t| pick_radio(c, &t.radio).ok().map(|r| r.freq as f64))
            .unwrap_or(0.0),
    };
    // SpectrumTap emits its CBOR via the same `cbor_tx` mpsc that
    // FrameSink uses, except scan_spectrum bypasses the sync_word
    // filter (we mark it sync_word=0 and let the broadcast path
    // honour clients that subscribed without sync_word).
    let (st_tx, mut st_rx) = unbounded_channel::<Vec<u8>>();
    let s = server.clone();
    tokio::spawn(async move {
        while let Some(buf) = st_rx.recv().await {
            let _ = s.broadcast(&buf, None).await; // no sync filter
        }
    });
    let spectrum_tap = fg.add(SpectrumTap::new(cfg, st_tx));
    connect!(fg, entry_dup.outputs[2] > spectrum_tap;);
}
```

- [ ] **Step 2.3: Build + commit**

```sh
cargo build -p chirpmunk-trx
cargo test -p chirpmunk-trx --test daemon_loopback
git add apps/chirpmunk-trx/src/main.rs
git commit -m "feat(trx): wire SpectrumTap on entry duplicator tap[2]"
```

---

## Task 3: chirpmunk-ui crate scaffold

**Files:**
- Create: `crates/chirpmunk-ui/Cargo.toml`
- Create: `crates/chirpmunk-ui/src/lib.rs`
- Create: `crates/chirpmunk-ui/src/main.rs`
- Modify: workspace `Cargo.toml`

- [ ] **Step 3.1: Cargo.toml**

```toml
[package]
name = "chirpmunk-ui"
version = "0.1.0"
edition = "2024"
license = "GPL-3.0-only"

[[bin]]
name = "chirpmunk-ui-server"
path = "src/main.rs"

[lib]
path = "src/lib.rs"

[dependencies]
anyhow = { workspace = true }
axum = { version = "0.7", features = ["ws"] }
chirpmunk-cbor = { path = "../chirpmunk-cbor" }
chirpmunk-udp  = { path = "../chirpmunk-udp" }
clap = { workspace = true, features = ["derive"] }
serde = { workspace = true, features = ["derive"] }
serde_json = "1"
tokio = { workspace = true, features = ["full"] }
tokio-stream = "0.1"
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

- [ ] **Step 3.2: Workspace Cargo.toml**

Add `"crates/chirpmunk-ui"` to `members`. Add `axum`, `serde_json`, `tokio-stream` to `[workspace.dependencies]` if not already there.

- [ ] **Step 3.3: lib.rs / main.rs / submodules**

`src/lib.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
#![forbid(unsafe_code)]
pub mod bridge;
pub mod json_translate;
pub mod server;
```

`src/main.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only

use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[clap(version, about = "chirpmunk-ui-server: read-only web UI for chirpmunk-trx")]
struct Args {
    #[clap(long, default_value = "127.0.0.1:8088")]
    bind: SocketAddr,
    #[clap(long, default_value = "127.0.0.1:5556")]
    trx_udp: SocketAddr,
    /// Comma-separated sync-words to subscribe to. Empty = all.
    #[clap(long, default_value = "")]
    subscribe_sync_words: String,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    let args = Args::parse();
    let sync_words: Vec<u16> = args
        .subscribe_sync_words
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() { None }
            else if let Some(stripped) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                u16::from_str_radix(stripped, 16).ok()
            } else {
                s.parse::<u16>().ok()
            }
        })
        .collect();

    let (event_tx, _) = tokio::sync::broadcast::channel::<String>(256);
    let bridge = chirpmunk_ui::bridge::Bridge::new(args.trx_udp, sync_words, event_tx.clone());
    tokio::spawn(async move {
        if let Err(e) = bridge.run().await {
            tracing::error!(error = %e, "bridge stopped");
        }
    });

    let app = chirpmunk_ui::server::router(event_tx);
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    tracing::info!(addr = %args.bind, "ui listening");
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [ ] **Step 3.4: server.rs**

```rust
// SPDX-License-Identifier: GPL-3.0-only

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use std::sync::Arc;
use tokio::sync::broadcast;

const INDEX_HTML: &str = include_str!("../static/index.html");

#[derive(Clone)]
struct AppState {
    events: broadcast::Sender<String>,
}

pub fn router(events: broadcast::Sender<String>) -> Router {
    let state = AppState { events };
    Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.events.subscribe();
    while let Ok(msg) = rx.recv().await {
        if socket.send(Message::Text(msg)).await.is_err() {
            return;
        }
    }
}
```

- [ ] **Step 3.5: bridge.rs**

```rust
// SPDX-License-Identifier: GPL-3.0-only

use anyhow::Result;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

pub struct Bridge {
    trx_addr: SocketAddr,
    sync_words: Vec<u16>,
    events: tokio::sync::broadcast::Sender<String>,
}

impl Bridge {
    pub fn new(trx_addr: SocketAddr, sync_words: Vec<u16>, events: tokio::sync::broadcast::Sender<String>) -> Self {
        Self { trx_addr, sync_words, events }
    }

    pub async fn run(self) -> Result<()> {
        let sock = UdpSocket::bind("0.0.0.0:0").await?;
        // Subscribe (and keepalive every 5 s)
        let sub = chirpmunk_cbor::encode_subscribe(self.sync_words.iter().copied());
        sock.send_to(&sub, self.trx_addr).await?;
        let sub2 = sub.clone();
        let trx2 = self.trx_addr;
        let sock2 = sock.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let _ = sock2.send_to(&sub2, trx2).await;
            }
        });

        let mut buf = vec![0u8; 65536];
        loop {
            let (n, _from) = sock.recv_from(&mut buf).await?;
            let cbor = &buf[..n];
            match crate::json_translate::cbor_to_json(cbor) {
                Ok(json) => { let _ = self.events.send(json); }
                Err(e) => tracing::debug!(error = %e, "discarding undecoded CBOR"),
            }
        }
    }
}
```

NB: `chirpmunk_cbor::encode_subscribe` may need to be added; or use existing
serializer. Verify against `crates/chirpmunk-cbor/src/lib.rs`.

- [ ] **Step 3.6: json_translate.rs**

```rust
// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Result, anyhow};

/// Best-effort CBOR→JSON. Recognised top-level types: lora_frame,
/// scan_spectrum, lora_tx_ack. Anything else is forwarded as-is via
/// the generic encoder.
pub fn cbor_to_json(bytes: &[u8]) -> Result<String> {
    // Use minicbor to parse to a Value, then re-encode via serde_json.
    // (Pseudocode; concrete API from minicbor::decoder used by chirpmunk-cbor.)
    todo!("decode the CBOR and stringify; reuse chirpmunk_cbor decoders for known types")
}
```

NB: this `todo!()` is a real plan failure under the writing-plans rules.
The implementer must replace this with a working decoder. Two options:

(a) Add a `pub fn to_json_string(bytes: &[u8]) -> Result<String, _>` to
    `chirpmunk-cbor` that handles all known types via the existing manual
    decoders. Implement once, used by chirpmunk-ui.

(b) Use `ciborium` (a generic CBOR <-> serde bridge) and
    `serde_json::to_string` round-trip. Simplest. Acceptable extra dep.

Recommend (b). Update Cargo.toml dependencies:
```toml
ciborium = "0.2"
```

Then:
```rust
pub fn cbor_to_json(bytes: &[u8]) -> Result<String> {
    let value: ciborium::Value = ciborium::de::from_reader(bytes)
        .map_err(|e| anyhow!("cbor decode: {e}"))?;
    serde_json::to_string(&value).map_err(|e| anyhow!("json encode: {e}"))
}
```

`ciborium::Value` implements `serde::Serialize` so this is direct.

- [ ] **Step 3.7: Build**

```sh
cargo build -p chirpmunk-ui 2>&1 | tail -10
```

- [ ] **Step 3.8: Commit**

```sh
git add crates/chirpmunk-ui Cargo.toml
git commit -m "feat(ui): chirpmunk-ui-server scaffold (axum + WS + UDP bridge)"
```

---

## Task 4: Frontend (single index.html)

**Files:**
- Create: `crates/chirpmunk-ui/static/index.html`

- [ ] **Step 4.1: Write the page**

```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>chirpmunk</title>
<style>
  body { margin: 0; background: #0a0a0a; color: #ddd; font-family: ui-monospace, monospace; }
  #waterfall { display: block; width: 100%; height: 50vh; image-rendering: pixelated; }
  #log { width: 100%; height: 50vh; overflow-y: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { border-bottom: 1px solid #222; padding: 4px 8px; text-align: left; font-size: 12px; }
  th { color: #888; }
  td.crc-ok { color: #6f6; }
  td.crc-bad { color: #f66; }
  #status { padding: 4px 8px; font-size: 11px; color: #888; }
</style>
</head>
<body>
  <canvas id="waterfall" width="1024" height="200"></canvas>
  <div id="status">connecting…</div>
  <div id="log">
    <table id="frames">
      <thead>
        <tr><th>time</th><th>sf</th><th>bw</th><th>sync</th><th>snr</th><th>crc</th><th>payload</th></tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>
<script>
(() => {
  const canvas = document.getElementById('waterfall');
  const ctx = canvas.getContext('2d');
  const status = document.getElementById('status');
  const tbody = document.querySelector('#frames tbody');
  const MAX_ROWS = 500;

  // Viridis-ish palette (precomputed 256-entry RGB)
  const palette = (() => {
    const p = new Uint8ClampedArray(256 * 3);
    for (let i = 0; i < 256; i++) {
      const t = i / 255;
      const r = Math.round(255 * Math.max(0, t - 0.4));
      const g = Math.round(255 * Math.max(0, t * 0.7));
      const b = Math.round(255 * (1 - Math.abs(2 * t - 1)));
      p[3*i] = r; p[3*i+1] = g; p[3*i+2] = b;
    }
    return p;
  })();

  function drawSpectrumRow(mags) {
    // Shift canvas down by 1 row, draw new row at y=0
    const w = canvas.width, h = canvas.height;
    const img = ctx.getImageData(0, 0, w, h - 1);
    ctx.putImageData(img, 0, 1);
    const row = ctx.createImageData(w, 1);
    const n = mags.length;
    let max = 0;
    for (const m of mags) if (m > max) max = m;
    for (let x = 0; x < w; x++) {
      const i = Math.floor(x * n / w);
      const v = max > 0 ? Math.min(255, Math.floor(255 * mags[i] / max)) : 0;
      row.data[4*x]     = palette[3*v];
      row.data[4*x + 1] = palette[3*v + 1];
      row.data[4*x + 2] = palette[3*v + 2];
      row.data[4*x + 3] = 255;
    }
    ctx.putImageData(row, 0, 0);
  }

  function appendFrame(f) {
    const tr = document.createElement('tr');
    const t = new Date().toISOString().slice(11, 23);
    const sf = f.phy?.sf ?? '?';
    const bw = f.carrier?.bw ?? '?';
    const sync = f.phy?.sync_word != null ? '0x' + Number(f.phy.sync_word).toString(16) : '?';
    const snr = f.phy?.snr_db != null ? f.phy.snr_db.toFixed(1) : '?';
    const crc = f.phy?.crc_valid ? 'ok' : 'bad';
    const payload = f.payload_hex ?? '';
    tr.innerHTML = `<td>${t}</td><td>${sf}</td><td>${bw}</td><td>${sync}</td><td>${snr}</td><td class="crc-${crc}">${crc}</td><td>${payload.slice(0,64)}${payload.length>64?'…':''}</td>`;
    tbody.prepend(tr);
    while (tbody.children.length > MAX_ROWS) tbody.removeChild(tbody.lastChild);
  }

  function connect() {
    const ws = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws');
    ws.onopen = () => { status.textContent = 'connected'; };
    ws.onclose = () => { status.textContent = 'disconnected — reconnecting in 5 s'; setTimeout(connect, 5000); };
    ws.onerror = () => {};
    ws.onmessage = (ev) => {
      try {
        const m = JSON.parse(ev.data);
        if (m.type === 'scan_spectrum' && Array.isArray(m.magnitudes)) {
          drawSpectrumRow(m.magnitudes);
        } else if (m.type === 'lora_frame') {
          appendFrame(m);
        }
      } catch (_) { /* ignore */ }
    };
  }
  connect();
})();
</script>
</body>
</html>
```

- [ ] **Step 4.2: Smoke test**

```sh
cargo run -p chirpmunk-ui --bin chirpmunk-ui-server -- --bind 127.0.0.1:8088 --trx-udp 127.0.0.1:5556 &
# In another shell: open http://localhost:8088/ in a browser; expect a page.
# Without trx running, the WS will be empty but the page should load.
```

- [ ] **Step 4.3: Commit**

```sh
git add crates/chirpmunk-ui/static/index.html
git commit -m "feat(ui): single-file vanilla-JS waterfall + frame log"
```

---

## Task 5: Integration test — UI receives one frame end-to-end

**Files:**
- Create: `crates/chirpmunk-ui/tests/end_to_end.rs`

- [ ] **Step 5.1: Write test**

Spawn `chirpmunk-trx --loopback --bind 127.0.0.1:<port>` as a subprocess.
Spawn `chirpmunk-ui-server` programmatically (call its `router()` directly,
no subprocess needed — wire `Bridge` to the test's mock UDP).

Send a CBOR `lora_frame` to the UI's UDP listener, open a WebSocket client
(`tokio-tungstenite`), assert one JSON message received with `type ==
"lora_frame"`.

- [ ] **Step 5.2: Commit**

```sh
git add crates/chirpmunk-ui/tests/end_to_end.rs Cargo.toml
git commit -m "test(ui): end-to-end UDP→WS pass-through"
```

---

## Task 6: Validation gates

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All green.

Manual smoke (operator-driven):

```sh
./target/debug/chirpmunk-trx --loopback --bind 127.0.0.1:5556 &
./target/debug/chirpmunk-ui-server --bind 127.0.0.1:8088 --trx-udp 127.0.0.1:5556 &
open http://localhost:8088/
# Fire a lora_tx via the existing Python helper; observe waterfall + frame log.
```

---

## Self-review

- Spec coverage:
  - §"Components" 1 (chirpmunk-ui crate): Task 3 ✅.
  - §"Components" 2 (HTTP server): Task 3 step 3.4 ✅.
  - §"Components" 3 (UDP→WS bridge): Task 3 step 3.5 ✅.
  - §"Components" 4 (SpectrumTap in chirpmunk-trx): Task 1, Task 2 ✅.
  - §"Components" 5 (frontend): Task 4 ✅.
  - §"Components" 6 (config): Task 1 step 1.1 ✅.

- Placeholder scan:
  - Task 3 step 3.6 had a `todo!()` body. Replaced inline with the `ciborium` recipe. No remaining TODOs.

- Type consistency:
  - WS message format: JSON text. Matches spec §"Open questions" 2.
  - `SpectrumTapConfig { fft_size: u16, interval_ms: u32, sample_rate_hz: f64, center_freq_hz: f64 }` — used in Task 1, Task 2.

- Risks:
  - axum/tokio-tungstenite version skew. Pin in workspace.
  - `ciborium` adds ~100 KB binary growth; acceptable for the UI binary which already pulls axum.
