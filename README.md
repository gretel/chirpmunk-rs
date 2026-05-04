# chirpmunk

Rust LoRa transceiver + wideband scanner on FutureSDR.

Behaviourally interoperable with the `gr4-lora` (LOST) CBOR/UDP control
plane and the `lora.*` Python userland.

See [SPEC.md](SPEC.md) for architecture and milestones, [REPORT.md](REPORT.md)
for the upstream reverse-engineering notes.

## Status

M0..M3 done. IQ replay decodes the canonical
`gr4-lora/test_vectors/sf7_cr1_bw125000` capture (payload
`Hello MeshCore`). Loopback proves TX→RX→FrameSink→CBOR→UDP→Python.
CBOR `lora_tx` drives dispatch and returns `lora_tx_ack`.

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

21 tests, 19 suites. M4 (wideband scanner) skipped per direction; M5
(full duplex daemon) next; hardware verification of M3/M4/M5 deferred to
a manual session.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
