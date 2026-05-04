# chirpmunk

Rust LoRa transceiver + wideband scanner on FutureSDR.

Behaviourally interoperable with the `gr4-lora` (LOST) CBOR/UDP control
plane and the `lora.*` Python userland.

See [SPEC.md](SPEC.md) for architecture and milestones, [REPORT.md](REPORT.md)
for the upstream reverse-engineering notes.

## Status

M1 done. TX → loopback channel → RX (FutureSDR PHY) → FrameSink →
CBOR `lora_frame` → UDP fanout → Python `cbor2` decode — full pipeline
green.

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

13 tests, 16 suites. M2 (multi-SF lockstep + dual channel) next.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
