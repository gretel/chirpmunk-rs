# chirpmunk

Rust LoRa transceiver + wideband scanner on FutureSDR.

Behaviourally interoperable with the `gr4-lora` (LOST) CBOR/UDP control
plane and the `lora.*` Python userland.

See [SPEC.md](SPEC.md) for architecture and milestones, [REPORT.md](REPORT.md)
for the upstream reverse-engineering notes.

## Status

M0 done. Workspace boots, CBOR `lora_frame` round-trips against Python
`cbor2`, UDP fanout works, config parses `gr4-lora/apps/config-pluto.toml`.

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

10 tests, 14 suites. M1 (single-channel RX) next.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
