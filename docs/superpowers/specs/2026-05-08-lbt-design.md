# LBT — Listen Before Talk

Status: Draft. Author: Tom + agent. Date: 2026-05-08.

## Goal

Channel-aware TX gate: transmit only when the air is clear. NACK with
`error: "channel_busy"` when the channel stays busy past a configurable
deadline. Side benefit: a working CAD path exercises full-duplex on
the B210 (concurrent CAD-RX during armed TX).

## Background

### gr4-lora reference

- `blocks/include/gnuradio-4.0/lora/ChannelActivityDetector.hpp` — 2-symbol
  dechirp, peak-ratio detector. Writes `std::atomic<bool> channel_busy`.
- `apps/lora_trx.cpp:522` — `channel_busy` declared at top of `main`,
  passed by reference to `build_rx_graph` and to the TX worker.
- `apps/tx_worker.cpp:216` — TX poll loop:

      if (cfg.lbt) {
          auto deadline = now + cfg.lbt_timeout_ms;
          while (channel_busy.load(acquire)) {
              if (now >= deadline) NACK("channel_busy"); return;
              sleep 10ms;
          }
      }
- Config defaults: `lbt = true`, `lbt_timeout_ms = 2000`, `min_ratio = 5.0`.

### chirpmunk current state

- `crates/chirpmunk-blocks/src/lib.rs` (`dispatch_lora_tx`): async fn,
  posts `Pmt::Blob` to Transmitter `msg` port. No busy gate, no LBT.
- `crates/chirpmunk-blocks/src/multi_sf.rs`: `StreamDuplicator<6>` →
  6 SF chains. Single tap, no CAD branch.
- `chirpmunk-config::TrxTransmit`: no `lbt` fields.

## Design

### Components

1. **`chirpmunk-blocks::cad::ChannelActivityDetector`** — port from
   gr4-lora. Stream-in `Complex<f32>` at the configured RX `bw`.
   Holds an FFT plan sized `1 << sf` (matches gr4-lora). Sliding 2-symbol
   window, dechirp-and-FFT, peak/mean ratio; sets `Arc<AtomicBool>`
   busy when ratio ≥ `min_ratio` for a full window. Hysteresis:
   release after `release_symbols` clean windows (default 4).

   Settings (via `chirpmunk-config`):
   - `cad_sf: u8` (default = lowest SF in the receive set)
   - `cad_bw: u32` (default = first BW in the receive set)
   - `cad_min_ratio: f32` (default 5.0)
   - `cad_release_symbols: u8` (default 4)

2. **Plumbing** — `chirpmunk-trx::main` constructs
   `let busy = Arc::new(AtomicBool::new(false));`. Cloned to:
   - the CAD block (writer)
   - `dispatch_lora_tx` task (reader, via shared TrxState)

3. **TX gate** — extend `dispatch_lora_tx`:

       if cfg.lbt {
           let deadline = Instant::now() + Duration::from_millis(cfg.lbt_timeout_ms);
           while busy.load(Acquire) {
               if Instant::now() >= deadline {
                   return LoraTxAck::err(seq, "channel_busy");
               }
               sleep(Duration::from_millis(10)).await;
           }
       }

4. **Source duplication** — extend `build_multi_sf_rx` from
   `StreamDuplicator<6>` to `StreamDuplicator<7>`. The 7th tap feeds CAD.
   Caveat: the FutureSDR `connect!` macro requires a literal index,
   so the existing unrolled loop expands by one branch. Manageable.

5. **Config** — `chirpmunk-config`:
   - `TrxTransmit { lbt: bool, lbt_timeout_ms: u32 }`
     (defaults `true` / `2000`).
   - `TrxReceive { cad_min_ratio: f32, cad_release_symbols: u8 }`
     (defaults `5.0` / `4`).

6. **Wire protocol** — `LoraTxAck::err(seq, "channel_busy")` already
   supported (string error). Document the new error string in
   `CBOR-SCHEMA.md` (chirpmunk side) and gr4-lora's CBOR-SCHEMA.md if
   not already there.

### Data flow

    SoapyDirectSource → StreamDuplicator<7> ─┬─ tap0..tap5 → multi-SF chains → FrameSink
                                              └─ tap6 → CAD → busy: Arc<AtomicBool>

    busy ──→ dispatch_lora_tx (poll + deadline + NACK)

### Error handling

- Channel busy past deadline → `LoraTxAck::err(seq, "channel_busy")`.
  Client decides retry policy.
- `lbt = false` → bypass busy flag entirely (current behaviour).
- CAD block panic → loud (project rule). Scheduler kills graph; daemon
  exits. Acceptable: CAD is required when `lbt = true`.
- Loopback (`--loopback`): no SoapyDirectSource. CAD wired off the
  loopback's TX path instead, so end-to-end test still works.
- Hardware mode: CAD is on the same StreamDuplicator as the decoders;
  if any branch lags the source by more than the back-pressure budget,
  CAD lag could mask a busy channel. Mitigation: CAD output is a single
  bool, work() is short, back-pressure unlikely. Worst case: false-clear
  (transmit collides). Acceptable failure mode.

### Testing

1. Unit: `chirpmunk-blocks::cad::tests::detection_ratio_synthetic` —
   feed clean noise → busy=false; feed chirp + noise at 5 dB SNR →
   busy=true within one window.

2. Unit: `release_hysteresis` — drive busy high, then clean; assert
   release after exactly `release_symbols` clean windows.

3. Integration: `apps/chirpmunk-trx/tests/lbt_loopback.rs`:
   - Spawn daemon with `lbt = true, lbt_timeout_ms = 200, --loopback`.
   - Inject persistent IQ via a synthetic interferer (TX a long burst).
   - Issue `lora_tx` from a third client → assert NACK
     `"channel_busy"` arrives within `200 ± 100 ms`.
   - Stop interferer, retry → assert `ok = true`.

4. Hardware (manual, post-merge): MeshCore companion mid-burst, then
   issue `lora hwtest tx`. Expect NACK during companion burst, success
   after.

## Approach trade-offs

| Option | Description | Verdict |
|---|---|---|
| (i) Standalone CAD on dedicated tap | Mirrors gr4-lora; reusable as full-duplex test | **Recommended** |
| (ii) Reuse FrameSync preamble detection | Cheaper, but couples LBT to per-SF chain — N busy flags to OR | Reject — fragile |
| (iii) RSSI energy detector | Fastest. But LoRa rides under noise floor → false busy on RF noise | Reject — wrong layer |

## Open questions / assumptions

1. Single-BW CAD (matches multi-SF M2 architecture). Multi-BW LBT is a
   follow-up after Spec 2 lands.
2. Wait-policy = spin-poll (10 ms). `tokio::sync::Notify` async wakeup
   is plausible v2 — reduce CPU. v1 keeps it simple and matches gr4-lora.
3. No deferred queue: timeout = drop. Matches gr4-lora.
4. CAD runs at a single SF (operator's "default decode SF"). Full
   busy-flag-per-SF is a future spec when wide-area meshing matters.

## License

GPL-3.0-only. The CAD block is a port from gr4-lora (ISC), legal.
The `dispatch_lora_tx` extension is fresh code under chirpmunk's
GPL-3.0-only.

## Next

Spec review by operator. On approval → invoke `writing-plans` skill.
