# chirpmunk

Rust LoRa transceiver + wideband scanner on FutureSDR.

Behaviourally interoperable with the `gr4-lora` (LOST) CBOR/UDP control
plane and the `lora.*` Python userland.

See [SPEC.md](SPEC.md) for architecture and milestones, [REPORT.md](REPORT.md)
for the upstream reverse-engineering notes.

## Status

M2 done. Parallel SF7..SF12 chains via `StreamDuplicator`, telemetry
fields propagated into the CBOR `lora_frame`, CRC trailer stripped from
the payload.

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

14 tests, 17 suites. M3 (TX) next.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
