# chirpmunk

Rust LoRa transceiver + wideband scanner on FutureSDR.

Behaviourally interoperable with the `gr4-lora` (LOST) CBOR/UDP control
plane and the `lora.*` Python userland.

See [SPEC.md](SPEC.md) for architecture and milestones, [REPORT.md](REPORT.md)
for the upstream reverse-engineering notes.

## Status

M0..M3, M5, M6 done. RX confirmed **on-air**: a 30 s ambient listen
on EU868 captured 4 MeshCore frames (SF8 BW62.5k sync 0x12), all CRC
OK, SNR ~15 dB.

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

# Hardware (defaults: MeshCore EU868 SF8 BW62.5k preamble 16 sync 0x12)
./target/debug/chirpmunk-trx \
    --device-args 'soapy_driver=uhd,type=b200' \
    --rx-antenna RX2 --tx-antenna TX/RX \
    --rx-gain 40 --bind 127.0.0.1:5556

# Loopback (no hardware)
./target/debug/chirpmunk-trx --loopback --bind 127.0.0.1:5556
```

22 tests, 20 suites. M4 (wideband scanner) deferred.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
