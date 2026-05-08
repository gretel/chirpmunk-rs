# Diversity RX

Status: Draft. Author: Tom + agent. Date: 2026-05-08.

## Goal

Use both B210/B220 RX channels for parallel decoding. v1: independent
decode per channel + frame-level dedup ("selection diversity"). v2
(future spec): IQ-level maximal-ratio combining (MRC).

## Background

### gr4-lora reference

- `apps/config.cpp:154` — `rx_antenna: vector<string>` (per-channel),
  `rx_chains: vector<u8>` (channel indexes 0..1).
- `apps/graph_builder.hpp:132,206` — multi-chain RX is independent
  decoders per chain. Combining is implicit at the consumer side
  (frames hit UDP, dedup happens in `lora_agg`/`lora_duckdb` by
  `(payload_hash, sync, sf)` within a window).
- `apps/lora_trx.cpp:371` — `rx_antenna` array indexed per chain.

### chirpmunk current state

- `crates/chirpmunk-blocks/src/soapy_direct.rs::SoapyDirectSource`:
  opens `&[0, 1]` channels (B200 channel-symmetry rule) but **discards
  channel 1**; merges into a single output stream.
- `chirpmunk-config::TrxReceive`: scalar `bw`, `sf`, etc. No
  per-channel config.
- B210 / LibreSDR_B220mini hardware: 2 RX channels, both can be RX
  simultaneously, share the same LO (phase-coherent — usable for MRC
  in v2).

## Design

### Components

1. **`SoapyDirectSource` extension** — emit *N* output streams, one
   per RX channel. Currently opens `&[0,1]` and merges. Change: expose
   2 output ports `out0`, `out1`, each carrying its channel's IQ.
   When `rx_chains = [0]` only, `out1` is connected to a `NullSink`
   (channel 1 still opened, IQ discarded — preserves the channel-symmetry
   workaround).

2. **N parallel multi-SF (× multi-BW) stacks** — one stack per active
   RX chain. Each consumes its own IQ from `outN`. Same
   `build_multi_sf_rx` machinery.

3. **Frame dedup in FrameSink** — extend `FrameSink::Telemetry` with a
   per-frame dedup window:
   - Key: `(sha256(payload), sync_word, sf, bw)`.
   - Window: `dedup_window_ms` (default 50 ms).
   - On hit:
     - Suppress UDP fanout for the duplicate.
     - Update the in-flight `phy.diversity` map of the *first* frame:
       extend `antennas: Vec<u8>`, push per-antenna SNR into
       `snr_db_per_ant`, take max into `snr_db_max`.
     - Fanout the consolidated frame to UDP only once the dedup window
       expires (small added latency, ~50 ms; acceptable for non-realtime
       packet messaging).
   - Record `phy.diversity` on the wire as:

         "phy.diversity": {
             "antennas": [0, 1],
             "snr_db_max": 12.3,
             "snr_db_per_ant": [12.3, 8.7]
         }

4. **Config** — `chirpmunk-config::TrxReceive`:
   - `rx_chains: Vec<u8>` (default `[0]`; `[0, 1]` enables diversity).
   - `rx_antennas: Vec<String>` (per-channel antenna name; default `[]`
     = driver default per channel; e.g. `["TX/RX", "RX2"]`).
   - `rx_gains: Vec<f64>` (per-channel gain; default = scalar `gain`
     applied to all chains).
   - `dedup_window_ms: u32 = 50`.

5. **Hardware constraint matrix** —
   - B210/B220: 2 RX channels — diversity supported.
   - Pluto / `driver = "plutoPAPR"`: 1 RX channel — `rx_chains.len() > 1`
     rejected at config-validate time with a clear error.

### Data flow

    SoapyDirectSource ──┬─ out0 → multi_sf_rx grid → FrameSink (chain=0)
                        └─ out1 → multi_sf_rx grid → FrameSink (chain=1)

    FrameSink:
      key = (sha256(payload), sync, sf, bw)
      if seen within window: merge into existing entry (extend antennas, max snr)
      else: open new entry, schedule fanout at +dedup_window_ms

### Error handling

- Channel 1 driver fault mid-run → log error; chain-0 keeps working.
  No automatic restart; daemon stays up.
- Channel symmetry: `rx_chains = [0]` still opens both channels at
  the source level; channel-1 IQ is sunk to `NullSink`. (Required by
  B210 hardware; documented.)
- Dedup window collision (legitimate retransmit < `dedup_window_ms`):
  duplicates suppressed. Document the trade-off; allow
  `dedup_window_ms = 0` to disable dedup entirely.
- One antenna disconnected (RF cable pulled) → SNR collapses on that
  chain; dedup naturally falls back to the working chain. No special
  handling needed.

### Testing

1. Unit: `FrameSink::dedup_within_window` — feed two frames with
   identical payload+sync+sf+bw within 30 ms; assert one
   `lora_frame` emitted, with `phy.diversity.antennas = [0, 1]`.
2. Unit: `dedup_disabled_when_window_zero` — same frames with
   `dedup_window_ms = 0`; assert two `lora_frame`s emitted.
3. Integration: loopback test where the loopback driver echoes IQ to
   both channels (or daemon-side fanout in the LoopbackDevice).
   TX one frame → assert single emitted `lora_frame` with
   `phy.diversity.antennas` length = 2.
4. Hardware (manual): two antennas ≥ λ/2 apart, MeshCore companion TX
   from a moving location. Assert the merged decode rate is ≥ the best
   single-antenna decode rate over a 100-frame sample.

## Approach trade-offs

| Option | Description | Verdict |
|---|---|---|
| (a) Selection combining (per-frame, best CRC+SNR) | Equivalent to (d) once dedup is added | Subsumed |
| (b) Maximal-Ratio Combining (IQ-level, weighted by SNR) | Best sensitivity. Requires phase-coherent ADCs (B210 has — both chans share LO). Complex: needs CFO alignment + cross-correlation between channels | **Defer to v2** |
| (c) Equal-gain IQ sum | Worse than (b), better than (d) for correlated noise; ugly when one antenna has DC spur | Reject |
| (d) Independent decode + frame dedup | Simplest. Reuses M2 multi-SF stack twice. No phase coherence required. Each antenna gives its best independent decode | **Recommended** |

## Open questions / assumptions

1. Companion antenna geometry: assume operator places antennas
   ≥ λ/2 apart for spatial diversity. At 868 MHz that's ~17 cm.
   Document; not enforced in code.
2. Per-channel gain: settable separately via Soapy
   `setGain(RX, ch, gain)`. Add `rx_gains: Vec<f64>` config (default =
   scalar `gain` applied to all chains).
3. Dedup added latency = `dedup_window_ms` worst case. Trade-off:
   higher window catches more diversity matches; lower window means
   faster UDP fanout. Default 50 ms is conservative for LoRa packet
   timing (≥10× minimum airtime at SF12/BW125k).
4. SHA-256 in dedup is fine — payload ≤ 255 bytes; this runs at frame
   rate (~10 Hz max), not sample rate.
5. v1 dedup is per-decoder-instance (single FrameSink). When Spec 2
   (multi-BW) is also live, the dedup key already includes `bw`,
   so cross-(SF, BW) dedup is correct without further work.

## License

GPL-3.0-only. SoapyDirectSource is chirpmunk's own port (using
`soapysdr` crate, MIT). FrameSink + dedup are fresh code.

## Next

Spec review by operator. On approval → invoke `writing-plans` skill.

A v2 follow-up spec for IQ-level MRC stays in the backlog: requires
CFO/phase alignment between RX channels, optional `phy.mrc_weight`
tag, additional `combine_iq` block.
