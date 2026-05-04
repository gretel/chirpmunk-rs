# chirpmunk

Rust LoRa transceiver + wideband scanner on FutureSDR.

Behaviourally interoperable with the `gr4-lora` (LOST) CBOR/UDP control
plane and the `lora.*` Python userland.

See [SPEC.md](SPEC.md) for architecture and milestones, [REPORT.md](REPORT.md)
for the upstream reverse-engineering notes.

## Status

M3 done. CBOR `lora_tx` requests dispatched to the Flowgraph
Transmitter; loopback verifies payload round-trip and `lora_tx_ack`
return.

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

20 tests, 18 suites. M4 (wideband scanner) next; hardware verification
of M3 deferred to a manual session.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
