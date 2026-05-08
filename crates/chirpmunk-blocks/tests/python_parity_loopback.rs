// SPDX-License-Identifier: GPL-3.0-only

//! Full M1 acceptance: TX → loopback → RX → FrameSink → CBOR → UDP →
//! Python `cbor2` decoder. Verifies the wire-level payload and the
//! schema fields a downstream `lora.core.cbor_stream` consumer
//! expects.
//!
//! Skipped if the gr4-lora `.venv` Python or `cbor2` is unavailable.
//! Override the interpreter with `CHIRPMUNK_PYTHON`.

use std::process::Stdio;
use std::time::Duration;

use chirpmunk_blocks::{DedupState, FrameSink, FrameSinkConfig};
use chirpmunk_phy::default_values::{HAS_CRC, PREAMBLE_LEN};
use chirpmunk_phy::utils::{
    Bandwidth, Channel, CodeRate, HeaderMode, LdroMode, SpreadingFactor, SynchWord,
};
use chirpmunk_phy::{build_lora_rx_soft_decoding, build_lora_tx};
use chirpmunk_udp::Server;

use futuresdr::prelude::*;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::sleep;

const PAD: usize = 10_000;
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

const PY_RECEIVER: &str = r#"
import json, os, socket, sys, time
import cbor2

server_addr = (os.environ["SERVER_HOST"], int(os.environ["SERVER_PORT"]))
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.settimeout(10.0)
sock.bind(("127.0.0.1", 0))

sock.sendto(cbor2.dumps({"type": "subscribe"}), server_addr)
print("READY", flush=True)

deadline = time.monotonic() + 30.0
while time.monotonic() < deadline:
    try:
        data, _ = sock.recvfrom(65536)
    except socket.timeout:
        continue
    msg = cbor2.loads(data)
    if isinstance(msg, dict) and msg.get("type") == "lora_frame":
        print("FRAME=" + json.dumps(msg, default=str), flush=True)
        sys.exit(0)

sys.stderr.write("python timeout\n")
sys.exit(2)
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_m1_loopback_to_python() -> Result<()> {
    let py = python_bin();
    if !cbor2_available(&py).await {
        eprintln!("SKIP: python or cbor2 unavailable at {py}");
        return Ok(());
    }

    let server = Server::bind("127.0.0.1:0").await.expect("bind");
    let addr = server.local_addr().unwrap();

    {
        let s = server.clone();
        tokio::spawn(async move { s.run().await });
    }

    let (cbor_tx, mut cbor_rx) = unbounded_channel::<(Vec<u8>, u16)>();
    let dedup = DedupState::new(Duration::ZERO, cbor_tx);
    {
        let s = server.clone();
        tokio::spawn(async move {
            while let Some((buf, sw)) = cbor_rx.recv().await {
                let _ = s.broadcast(&buf, Some(sw)).await;
            }
        });
    }

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
    let mut out = BufReader::new(stdout).lines();

    let ready = tokio::time::timeout(Duration::from_secs(5), out.next_line())
        .await
        .expect("ready timeout")
        .expect("readline")
        .expect("python exited before READY");
    assert_eq!(ready, "READY");

    for _ in 0..100 {
        sleep(Duration::from_millis(20)).await;
        if server.client_count().await == 1 {
            break;
        }
    }
    assert_eq!(server.client_count().await, 1, "subscribe never observed");

    let mut fg = Flowgraph::new();
    let transmitter = build_lora_tx(
        &mut fg,
        Bandwidth::default(),
        SpreadingFactor::SF7,
        CodeRate::default(),
        HAS_CRC,
        LdroMode::AUTO,
        HeaderMode::Explicit,
        1,
        SynchWord::Private,
        Some(PREAMBLE_LEN),
        PAD,
    )
    .expect("build tx");

    let (frame_sync, decoder) = build_lora_rx_soft_decoding(
        &mut fg,
        Channel::EU868_1,
        Bandwidth::default(),
        SpreadingFactor::SF7,
        HeaderMode::Explicit,
        LdroMode::AUTO,
        Some(&[SynchWord::Private]),
        1,
        None,
        None,
        false,
        None,
    )
    .expect("build rx");

    let cfg = FrameSinkConfig {
        sf: 7,
        bw: 125_000,
        cr: 4,
        sync_word: 0x12,
        device: Some("loopback".into()),
        decode_label: Some("loopback".into()),
        rx_channel: Some(0),
    };
    let frame_sink = fg.add(FrameSink::new(cfg, dedup));

    connect!(fg,
        transmitter > frame_sync;
        decoder.out_annotated | frame_sink;
    );

    let transmitter_id: BlockId = transmitter.into();
    let runtime = Runtime::new();
    let handle = runtime.start(fg).expect("start").handle();

    handle
        .post(
            transmitter_id,
            "msg",
            Pmt::String("chirpmunk-m1-payload".into()),
        )
        .await
        .expect("post msg");

    let frame_line = tokio::time::timeout(Duration::from_secs(20), out.next_line())
        .await
        .expect("frame line timeout")
        .expect("readline")
        .expect("no frame line");
    let payload_json = frame_line
        .strip_prefix("FRAME=")
        .expect("missing FRAME prefix");
    let parsed: serde_json::Value = serde_json::from_str(payload_json).expect("python json");

    assert_eq!(parsed["type"], "lora_frame");
    assert_eq!(parsed["phy"]["sf"], 7);
    assert_eq!(parsed["phy"]["sync_word"], 0x12);
    assert_eq!(parsed["carrier"]["sync_word"], 0x12);
    assert_eq!(parsed["decode_label"], "loopback");
    let snr = parsed["phy"]["snr_db"]
        .as_f64()
        .expect("snr_db missing or non-numeric");
    assert!(snr > 5.0, "loopback snr_db too low: {snr}");
    let nf = parsed["phy"]["noise_floor_db"]
        .as_f64()
        .expect("noise_floor_db missing or non-numeric");
    assert!(nf < 0.0, "loopback noise_floor_db looks suspicious: {nf}");
    let payload_b64 = parsed["payload"]
        .as_str()
        .expect("python encodes bytes as str via json default=str");
    assert!(
        payload_b64.contains("chirpmunk-m1-payload"),
        "payload mismatch in python repr: {payload_b64}"
    );

    let _ = child.kill().await;
    Ok(())
}
