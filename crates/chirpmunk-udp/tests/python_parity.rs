// SPDX-License-Identifier: GPL-3.0-only

//! Cross-language parity check.
//!
//! Subscribes a Python process (via `gr4-lora`'s `lora.core` venv) to a
//! `chirpmunk-udp::Server`, broadcasts a `lora_frame`, and asserts that
//! Python's `cbor2.loads()` reconstructs every required field with the
//! expected types and values.
//!
//! Skipped (printed as `IGNORED`) when the Python interpreter or `cbor2`
//! package is not available. The interpreter path can be overridden with
//! the `CHIRPMUNK_PYTHON` env var; default is the gr4-lora venv.

use std::process::Stdio;
use std::time::Duration;

use chirpmunk_cbor::{Carrier, LoraFrame, Phy};
use chirpmunk_udp::Server;
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

fn sample_frame() -> LoraFrame {
    LoraFrame {
        ts: "2026-05-04T12:00:00Z".into(),
        seq: 7,
        phy: Phy {
            sf: 8,
            bw: 62500,
            cr: 4,
            crc_valid: true,
            sync_word: 0x12,
            snr_db: 12.5,
            noise_floor_db: Some(-42.0),
            peak_db: None,
            snr_db_td: None,
            channel_freq: Some(869618000.0),
            decode_bw: None,
            cfo_int: None,
            cfo_frac: None,
            sfo_hat: None,
            sample_rate: None,
            frequency_corrected: None,
            ppm_error: None,
            diversity: None,
        },
        carrier: Carrier {
            sync_word: 0x12,
            sf: 8,
            bw: 62500,
            cr: 4,
            ldro_cfg: false,
        },
        payload: b"chirp".to_vec(),
        payload_len: 5,
        crc_valid: true,
        cr: 4,
        is_downchirp: false,
        id: "550e8400-e29b-41d4-a716-446655440000".into(),
        payload_hash: 12345678901234,
        rx_channel: Some(0),
        decode_label: Some("meshcore".into()),
        device: Some("31DE7F5".into()),
    }
}

const PY_RECEIVER: &str = r#"
import json, os, socket, sys, time
import cbor2

server_addr = (os.environ["SERVER_HOST"], int(os.environ["SERVER_PORT"]))
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.settimeout(5.0)
sock.bind(("127.0.0.1", 0))

# Subscribe (no filter — receive all frames).
sock.sendto(cbor2.dumps({"type": "subscribe"}), server_addr)

# Notify Rust via stdout that we are ready, so the harness can broadcast.
print("READY", flush=True)

deadline = time.monotonic() + 5.0
while time.monotonic() < deadline:
    try:
        data, _ = sock.recvfrom(65536)
    except socket.timeout:
        continue
    msg = cbor2.loads(data)
    if isinstance(msg, dict) and msg.get("type") == "lora_frame":
        print("FRAME=" + json.dumps(msg, default=str), flush=True)
        sys.exit(0)

sys.stderr.write("timeout\n")
sys.exit(2)
"#;

#[tokio::test]
async fn python_cbor2_decodes_lora_frame() {
    let py = python_bin();
    if !cbor2_available(&py).await {
        eprintln!("SKIP: python or cbor2 unavailable at {py}");
        return;
    }

    let server = Server::bind("127.0.0.1:0").await.expect("bind");
    let addr = server.local_addr().unwrap();

    let runner = server.clone();
    tokio::spawn(async move {
        let _ = runner.run().await;
    });

    let mut child = Command::new(&py)
        .arg("-c")
        .arg(PY_RECEIVER)
        .env("SERVER_HOST", addr.ip().to_string())
        .env("SERVER_PORT", addr.port().to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn python");

    let stdout = child.stdout.take().expect("stdout");

    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut out = BufReader::new(stdout).lines();

    let ready = tokio::time::timeout(Duration::from_secs(3), out.next_line())
        .await
        .expect("ready timeout")
        .expect("readline")
        .expect("python exited before READY");
    assert_eq!(ready, "READY");

    for _ in 0..50 {
        sleep(Duration::from_millis(20)).await;
        if server.client_count().await == 1 {
            break;
        }
    }
    assert_eq!(server.client_count().await, 1, "subscribe never observed");

    let frame = sample_frame();
    let buf = chirpmunk_cbor::to_vec(&frame).expect("encode");
    server
        .broadcast(&buf, Some(frame.carrier.sync_word))
        .await
        .expect("broadcast");

    let frame_line = tokio::time::timeout(Duration::from_secs(3), out.next_line())
        .await
        .expect("frame line timeout")
        .expect("readline")
        .expect("no frame line");

    let payload = frame_line
        .strip_prefix("FRAME=")
        .expect("missing FRAME prefix");
    let parsed: serde_json::Value = serde_json::from_str(payload).expect("python json");

    assert_eq!(parsed["type"], "lora_frame");
    assert_eq!(parsed["seq"], 7);
    assert_eq!(parsed["phy"]["sf"], 8);
    assert_eq!(parsed["phy"]["bw"], 62500);
    assert_eq!(parsed["phy"]["sync_word"], 0x12);
    assert_eq!(parsed["phy"]["crc_valid"], true);
    assert!((parsed["phy"]["snr_db"].as_f64().unwrap() - 12.5).abs() < 1e-9);
    assert!((parsed["phy"]["channel_freq"].as_f64().unwrap() - 869618000.0).abs() < 1e-3);
    assert_eq!(parsed["carrier"]["sync_word"], 0x12);
    assert_eq!(parsed["payload_len"], 5);
    assert_eq!(parsed["crc_valid"], true);
    assert_eq!(parsed["is_downchirp"], false);
    assert_eq!(parsed["rx_channel"], 0);
    assert_eq!(parsed["decode_label"], "meshcore");
    assert_eq!(parsed["device"], "31DE7F5");

    let exit = tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .expect("python wait timeout")
        .expect("python exit");
    assert!(exit.success(), "python exited with {exit:?}");
}
