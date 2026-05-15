# chirpmunk-rs

**chirpmunk-rs** — a Rust LoRa PHY transceiver and wideband scanner built on [FutureSDR](https://github.com/gretel/FutureSDR), paired with a CBOR/UDP control plane compatible with `chirpmunk-gr4`.

> **Status:** research prototype. APIs, configuration, and on-wire formats change without notice. Not production-ready.

## Requirements

- Rust toolchain (MSRV 1.89, edition 2024)
- SDR:
  - UHD via SoapySDR (`soapy_driver=uhd`) — B200 / B210 (other UHD devices untested)
  - IIO via libiio (under development)

## Build & run

```sh
# Build everything
cargo build --workspace

# Run tests
cargo test  --workspace

# Lint
cargo clippy --workspace --all-targets -- -D warnings
```

Binaries land in `target/debug/`. Configuration is via TOML (see `apps/chirpmunk-trx/config.example.toml`). Default UDP ports:

| Port | Endpoint                | Direction            |
|------|-------------------------|----------------------|
| 5555 | `chirpmunk core`        | publish to consumers |
| 5556 | `chirpmunk-trx`         | producer → core      |
| 5557 | `chirpmunk-scan`        | producer → core      |

Start (each in its own shell):

```sh
# data-plane core (start before producers) — from chirpmunk-gr4
lora core --config apps/config.toml

# Rust transceiver (hardware)
./target/debug/chirpmunk-trx --config apps/chirpmunk-trx/config.example.toml

# Loopback (no hardware)
./target/debug/chirpmunk-trx --loopback --bind 127.0.0.1:5556
```

## Credits

The DSP pipeline draws on the EPFL TCL reference implementation ([gr-lora_sdr](https://github.com/tapparelj/gr-lora_sdr), GPL-3.0), adapted for FutureSDR's Rust block model. The CBOR/UDP control plane is a shared design with [`chirpmunk-gr4`](https://github.com/gretel/chirpmunk-gr4), authored alongside it.

## Documentation

Is currently lacking. Work in progress. Please stay tuned!

## License

[GPL-3.0-only](LICENSE) — SPDX identifier `GPL-3.0-only`.
Copyright © 2025–2026 Tom Hensel &lt;code@jitter.eu&gt;.
