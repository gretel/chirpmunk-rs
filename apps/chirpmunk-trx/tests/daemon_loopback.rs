// SPDX-License-Identifier: GPL-3.0-only

//! M5 acceptance: spawn `chirpmunk-trx --loopback` as a subprocess.
//! Drive it via Python (`cbor2`) — subscribe, send a `lora_tx` request,
//! collect the matching `lora_frame` and the `lora_tx_ack`. Validates
//! the daemon as an integrated unit (subscribe registry + broadcaster +
//! flowgraph + lora_tx dispatcher).
//!
//! Skipped when `cbor2` is unavailable. Requires the `chirpmunk-trx`
//! binary to be already built (`cargo build` ahead of `cargo test`).

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::sleep;

const DEFAULT_PYTHON: &str = "/Users/tom/src/uhd/gr4-lora/.venv/bin/python";

fn python_bin() -> String {
    std::env::var("CHIRPMUNK_PYTHON").unwrap_or_else(|_| DEFAULT_PYTHON.into())
}

async fn cbor2_available(py: &str) -> bool {
    let status = Command::new(py)
        .arg("-c")
        .arg("import cbor2")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    matches!(status, Ok(s) if s.success())
}

fn pick_free_port() -> u16 {
    use std::net::UdpSocket;
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    s.local_addr().unwrap().port()
}

fn daemon_path() -> std::path::PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .join("../../target")
        .join(if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        })
        .join("chirpmunk-trx")
}

const PY_DRIVER: &str = r#"
import json, os, socket, sys, time
import cbor2

server = (os.environ["SERVER_HOST"], int(os.environ["SERVER_PORT"]))
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.settimeout(20.0)
sock.bind(("127.0.0.1", 0))

sock.sendto(cbor2.dumps({"type": "subscribe"}), server)
print("READY", flush=True)

req = {
    "type": "lora_tx",
    "payload": b"chirpmunk-m5-daemon",
    "seq": 7,
}
sock.sendto(cbor2.dumps(req), server)

deadline = time.monotonic() + 30.0
seen_frame = False
seen_ack = False
while time.monotonic() < deadline and not (seen_frame and seen_ack):
    try:
        data, _ = sock.recvfrom(65536)
    except socket.timeout:
        continue
    msg = cbor2.loads(data)
    if not isinstance(msg, dict):
        continue
    if msg.get("type") == "lora_frame":
        seen_frame = True
        print("FRAME=" + json.dumps(msg, default=str), flush=True)
    elif msg.get("type") == "lora_tx_ack":
        seen_ack = True
        print("ACK=" + json.dumps(msg, default=str), flush=True)

if not seen_frame or not seen_ack:
    sys.stderr.write(f"missing: frame={seen_frame} ack={seen_ack}\n")
    sys.exit(2)
sys.exit(0)
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_lora_tx_roundtrip() {
    let py = python_bin();
    if !cbor2_available(&py).await {
        eprintln!("SKIP: python or cbor2 unavailable at {py}");
        return;
    }

    let bin = daemon_path();
    if !bin.exists() {
        panic!(
            "chirpmunk-trx binary not built. Run `cargo build -p chirpmunk-trx` first. Path: {}",
            bin.display()
        );
    }

    let port = pick_free_port();
    let bind = format!("127.0.0.1:{port}");

    let mut daemon = Command::new(&bin)
        .arg("--loopback")
        .arg("--bind")
        .arg(&bind)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon");

    let stdout = daemon.stdout.take().expect("stdout");
    let mut daemon_lines = BufReader::new(stdout).lines();

    let mut booted = false;
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_millis(100), daemon_lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                eprintln!("daemon: {line}");
                if line.contains("flowgraph running") || line.contains("udp ready") {
                    booted = true;
                }
            }
            _ => break,
        }
    }
    if !booted {
        sleep(Duration::from_millis(800)).await;
    }

    let mut child = Command::new(&py)
        .arg("-c")
        .arg(PY_DRIVER)
        .env("SERVER_HOST", "127.0.0.1")
        .env("SERVER_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn python");

    let py_stdout = child.stdout.take().expect("stdout");
    let mut py_lines = BufReader::new(py_stdout).lines();

    let mut frame_json: Option<String> = None;
    let mut ack_json: Option<String> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline && !(frame_json.is_some() && ack_json.is_some()) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(remaining, py_lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                if let Some(rest) = line.strip_prefix("FRAME=") {
                    frame_json = Some(rest.to_owned());
                } else if let Some(rest) = line.strip_prefix("ACK=") {
                    ack_json = Some(rest.to_owned());
                } else if line == "READY" {
                    eprintln!("python ready");
                }
            }
            Ok(Ok(None)) | Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    let frame_json = frame_json.expect("python never reported FRAME");
    let ack_json = ack_json.expect("python never reported ACK");

    let frame: serde_json::Value = serde_json::from_str(&frame_json).expect("frame json");
    let ack: serde_json::Value = serde_json::from_str(&ack_json).expect("ack json");

    assert_eq!(frame["type"], "lora_frame");
    let payload_repr = frame["payload"].as_str().expect("payload as str");
    assert!(
        payload_repr.contains("chirpmunk-m5-daemon"),
        "payload mismatch: {payload_repr}"
    );

    assert_eq!(ack["type"], "lora_tx_ack");
    assert_eq!(ack["seq"], 7);
    assert_eq!(ack["ok"], true);

    let _ = child.kill().await;
    let _ = daemon.kill().await;
}
