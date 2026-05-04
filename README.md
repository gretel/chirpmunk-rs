# chirpmunk

Rust LoRa transceiver + wideband scanner on FutureSDR.

Behaviourally interoperable with the `gr4-lora` (LOST) CBOR/UDP control
plane and the `lora.*` Python userland.

See [SPEC.md](SPEC.md) for architecture and milestones, [REPORT.md](REPORT.md)
for the upstream reverse-engineering notes.

## Status

M0..M5 done (loopback). M6 hardware bring-up: USRP B210/B220 boots
cleanly through `chirpmunk-trx --device-args 'soapy_driver=uhd'`,
flowgraph runs without errors. On-air decode against a LoRa
companion is the remaining hardware acceptance criterion.

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

# Hardware bring-up
./target/debug/chirpmunk-trx \
    --device-args 'soapy_driver=uhd' \
    --bind 127.0.0.1:5556
```

22 tests, 20 suites. M4 (wideband scanner) deferred. M6 in progress.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
