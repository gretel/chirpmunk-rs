# chirpmunk

Rust LoRa transceiver + wideband scanner on FutureSDR.

Behaviourally interoperable with the `gr4-lora` (LOST) CBOR/UDP control
plane and the `lora.*` Python userland.

See [SPEC.md](SPEC.md) for architecture and milestones, [REPORT.md](REPORT.md)
for the upstream reverse-engineering notes.

## Status

M0..M5 done (loopback). `chirpmunk-trx --loopback` runs as a daemon:
binds UDP, accepts subscribe, dispatches incoming `lora_tx` requests
through the flowgraph, broadcasts `lora_frame` events back, replies with
`lora_tx_ack`. End-to-end Python parity test spawns the binary and
validates the round-trip.

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

22 tests, 20 suites. M4 (wideband scanner) deferred. Hardware
verification (real seify Sink/Source) deferred to M6.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
