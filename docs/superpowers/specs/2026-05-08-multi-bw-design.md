# Multi-BW Decoder

Status: Draft. Author: Tom + agent. Date: 2026-05-08.

## Goal

Concurrent decoding across the cartesian product `(SF × BW × chain)`
per radio channel. M2 already ships SF7..SF12 chains at a single BW;
this spec adds the BW dimension.

## Background

### gr4-lora reference

- `apps/graph_builder.hpp:116` — topology comment:

      // Topology (per radio channel, multi-BW):
      //   SoapySource → fork(per-BW) → resampler → fork(per-SF) → decoder

- `apps/config.hpp` — `cfg.rx_bandwidths: vector<uint32_t>`. Defaults
  per region; user-overridable in TOML.
- `apps/lora_trx.cpp:289` — comment:
  "configuration they need at startup via multi-BW + multi-chain + ..."

### chirpmunk current state

- `crates/chirpmunk-blocks/src/multi_sf.rs::build_multi_sf_rx` —
  fixed `StreamDuplicator<6>`, six SF chains, all at config-driven
  single BW. Decoder is constructed per-chain with `bw` baked in
  (FFT size = `1 << sf`, decimator slope from `bw`).
- `chirpmunk-config::TrxReceive`: scalar `bw: u32`, no vec.

## Design

### Components

1. **Resampler bank** — for each `bw_i` in config, insert a polyphase
   resampler before the multi-SF chain. If `bw_0 == sample_rate`,
   identity (skip). Use FutureSDR `FirBuilder::resampling::<f32, _>`.
   Filter design: standard Kaiser, transition 0.1, attenuation 60 dB.

2. **Per-BW multi-SF stack** — call existing `build_multi_sf_rx` once
   per BW. Inputs: BW-resampled stream. Output: 6 SF chains × the
   number of `chains` (multi-chain ≠ multi-antenna; chains is
   gr4-lora's per-radio-channel decoder count, used here as 1 by
   default).

3. **Outer source duplication** — `StreamDuplicator<|BW|>` upstream of
   the resampler bank. Total: `|BW| × 6 × chains` decoders per radio
   channel.

4. **Frame dispatch** — every decoder emits to the same `FrameSink`
   (mpsc channel). Decoder publishes a `phy.bw` tag (already published
   by chirpmunk-phy via settings); `FrameSink` reads `carrier.bw`
   into the CBOR `lora_frame.carrier.bw` field.

5. **Config** — `chirpmunk-config::TrxReceive`:
   - `bandwidths: Vec<u32>` (default `[125000]`).
   - `sf_set: Vec<u8>` (default `[7,8,9,10,11,12]`).
   - Existing `bw: u32` deprecated; treat as alias for `bandwidths = [bw]`
     during a one-release migration window.
   - Reject empty sets at parse time. Reject `|BW|` outside
     `{62500, 125000, 250000, 500000}` with a warn (not a hard reject —
     unusual BWs are useful for research).

6. **Memory budget** — each decoder ~few MB (FFT plans + buffers).
   Worst case: 4 BWs × 6 SFs = 24 decoders ≈ 60–120 MB. Acceptable.

### "SF priority" interpretation

User said "MultiSF and MultiBW decoder (SF has priority)".
Two readings:

- (a) **Roadmap**: ship multi-SF first (done in M2), multi-BW second
  (this spec). Default interpretation.
- (b) **Dedup**: when the same payload decodes on multiple `(SF, BW)`
  pairs, prefer the SF dimension (e.g., lower SF wins). Only matters
  for FrameSink dedup logic.

This spec executes reading (a). Reading (b) is folded into the Spec 3
(diversity) FrameSink dedup design where it matters more.

### Data flow

    SoapyDirectSource
       → StreamDuplicator<|BW|> ─┬─ resampler(bw_0) → build_multi_sf_rx → 6 decoders → FrameSink
                                  ├─ resampler(bw_1) → build_multi_sf_rx → 6 decoders → FrameSink
                                  └─ ...

### Error handling

- Empty BW set → config parse error (loud).
- Resampler ratio non-rational → log warn, drop offending BW from the
  set; continue.
- Per-decoder CRC fail / pile-up → existing behaviour (drop frame, or
  emit `lora_frame` with `crc_valid: false` per current FrameSink config).

### Testing

1. Unit: resampler bank construction — given `[125000, 250000]` and
   sample rate 1 Mhz, assert one branch is identity-skipped, one
   branch produces 250 ksps. Existing FutureSDR `FirBuilder::resampling`
   has its own tests; we test wiring.

2. Integration: extend `python_parity_loopback`:
   - TX a frame at `(SF=8, BW=250k)`.
   - Spawn daemon with `bandwidths = [125000, 250000]`.
   - Assert exactly one `lora_frame` emitted with
     `phy.sf == 8 && carrier.bw == 250000`.
   - Assert no `lora_frame` from the BW=125k branch (rejected by
     header CRC at that BW).

3. Hardware (post-merge): SDRangel-driven `(SF, BW)` matrix sweep;
   record per-cell decode ratio; assert each configured cell fires.

## Approach trade-offs

| Option | Description | Verdict |
|---|---|---|
| (i) Resampler bank → multi-SF | Canonical port; clean rate per (SF,BW) | **Recommended** |
| (ii) Single rate, decoder reads BW from settings | Decoder is BW-locked at construction (FFT size, dechirp slope) — runtime BW switch needs full rebuild | Reject — won't work without major rework |
| (iii) BW-aware FrameSync (dynamic per detected preamble) | Far harder; not gr4-lora's approach; race conditions across BWs | Defer — research v2 |

## Open questions / assumptions

1. Spec assumes the existing `chirpmunk-phy::Decoder` correctly
   parameterises its FFT/dechirp from the `bw` setting at
   construction. Verify before impl. (Spot-check: yes — `frame_sync.rs`
   reads `bw` in `Settings`.)
2. Multi-BW + LBT (Spec 1) interaction: only one CAD branch per
   radio channel for v1. Operator picks `cad_bw`. Per-BW CAD is a
   future spec.
3. BW set bounded to "sane" values; no hard cap, but log warn outside
   `{62500, 125000, 250000, 500000}`.
4. Per-BW `sf_set` not supported in v1 (one global SF set across
   all BWs). Add only if real-world configs need asymmetry.

## License

GPL-3.0-only. Resampler is FutureSDR upstream (Apache-2.0 →
GPL-3.0-only compatible). Reuses existing chirpmunk-phy decoder.

## Next

Spec review by operator. On approval → invoke `writing-plans` skill.
