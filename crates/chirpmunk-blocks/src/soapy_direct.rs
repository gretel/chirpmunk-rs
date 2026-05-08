// SPDX-License-Identifier: GPL-3.0-only

//! Direct-soapysdr RX + TX blocks for chirpmunk-trx hardware mode.
//!
//! Replaces FutureSDR's `seify::Source` / `seify::Sink` for hardware
//! TX on the LibreSDR_B220mini (B210 clone). seify 0.18/0.19 cannot
//! drive this hardware: its `TxStreamer` trait omits `read_status`,
//! omits multi-channel buddy-share semantics, and its `Sink` block
//! single-channel `tx_streamer.write(...)` triggers UHD `Corruption`
//! events on every write with the FPGA TX FIFO never reaching the
//! antenna PA.
//!
//! TX submission strategy — accumulate-then-flush with wall-clock
//! idle detection. Each `work()` tick with input drains it into an
//! internal `Vec<Complex32>` and stamps `last_input_time`. On a tick
//! with empty input, if the buffer is non-empty AND wall-clock since
//! the last input exceeds `IDLE_FLUSH_MS`, the burst is flushed using
//! the proven probe recipe (activate at `now + activation_offset_ns`,
//! tight write loop, `end_burst=true` on last chunk, 400 ms FPGA TX
//! FIFO drain, deactivate). Streamer is dormant between bursts — no
//! sustained DAC drain on this clone, no SSSS storm. Idle ticks sleep
//! briefly to cooperate with the FutureSDR scheduler instead of busy-
//! spinning.

use anyhow::{Context, Result, anyhow};
use futuresdr::num_complex::Complex32;
use futuresdr::runtime::dev::prelude::*;
use soapysdr::Direction;

/// Translate chirpmunk-style `soapy_driver=...` device args to the form
/// `soapysdr::Device::new` accepts (`driver=...`).
pub fn translate_args(s: &str) -> String {
    s.replace("soapy_driver=", "driver=")
}

/// Open a SoapySDR device using chirpmunk-style args. Args MUST contain
/// `master_clock_rate=24000000` for the LibreSDR_B220mini clone — this
/// pins the AD9361 HBF chain at a clean 24 MHz before any rate or
/// stream config touches the hardware.
pub fn open_device(args: &str) -> Result<soapysdr::Device> {
    let translated = translate_args(args);
    let dev = soapysdr::Device::new(translated.as_str())
        .map_err(|e| anyhow!("soapysdr::Device::new({translated}): {e:?}"))?;
    Ok(dev)
}

/// Static configuration for the RX source.
#[derive(Debug, Clone)]
pub struct SoapyRxConfig {
    pub freq_hz: f64,
    pub rate_hz: f64,
    pub gain_db: f64,
    pub antenna: Option<String>,
    pub channel: usize,
}

/// Direct-soapysdr RX source block.
///
/// Opens RX on TWO channels because the B200 family enforces symmetric
/// channel counts when paired with a 2-channel TX stream. Channel 0
/// carries the real signal and is forwarded to the FutureSDR output
/// port; channel 1's samples are read into a scratch buffer and
/// discarded.
#[derive(Block)]
#[blocking]
pub struct SoapyDirectSource {
    #[output]
    output: DefaultCpuWriter<Complex32>,
    dev: soapysdr::Device,
    cfg: SoapyRxConfig,
    rx: Option<soapysdr::RxStream<Complex32>>,
    /// Scratch buffer for channel 0 IQ (copied into the FutureSDR
    /// output port after read). Avoids `unsafe` aliasing of the two
    /// `&mut [Complex32]` slices required by `RxStream::read`.
    scratch0: Vec<Complex32>,
    /// Discard buffer for channel 1 IQ.
    discard: Vec<Complex32>,
}

impl SoapyDirectSource {
    pub fn new(dev: soapysdr::Device, cfg: SoapyRxConfig) -> Self {
        Self {
            output: DefaultCpuWriter::default(),
            dev,
            cfg,
            rx: None,
            scratch0: Vec::new(),
            discard: Vec::new(),
        }
    }
}

impl Kernel for SoapyDirectSource {
    async fn init(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        // Configure both RX channels to keep UHD's channel-symmetry
        // constraint satisfied; the user-facing config drives chan 0,
        // chan 1 mirrors it (gain irrelevant — discarded).
        for chan in [0usize, 1usize] {
            self.dev
                .set_sample_rate(Direction::Rx, chan, self.cfg.rate_hz)
                .with_context(|| format!("set_sample_rate(Rx, {chan})"))?;
            self.dev
                .set_frequency(Direction::Rx, chan, self.cfg.freq_hz, "")
                .with_context(|| format!("set_frequency(Rx, {chan})"))?;
            let gain = if chan == 0 { self.cfg.gain_db } else { 0.0 };
            self.dev
                .set_gain(Direction::Rx, chan, gain)
                .with_context(|| format!("set_gain(Rx, {chan})"))?;
            if let Some(ant) = &self.cfg.antenna {
                let _ = self.dev.set_antenna(Direction::Rx, chan, ant.as_str());
            }
        }
        let mut rx = self
            .dev
            .rx_stream::<Complex32>(&[0, 1])
            .map_err(|e| anyhow!("rx_stream(2ch): {e:?}"))?;
        // Multi-channel RX requires a timed activation so UHD can
        // time-align the channels. Read current hardware time and
        // start 100 ms in the future — independent of which block
        // (Source or Sink) ran init() first.
        let now_ns = self.dev.get_hardware_time(None).unwrap_or(0);
        rx.activate(Some(now_ns + 100_000_000))
            .map_err(|e| anyhow!("rx activate: {e:?}"))?;
        self.rx = Some(rx);
        tracing::info!(
            freq = self.cfg.freq_hz,
            rate = self.cfg.rate_hz,
            gain = self.cfg.gain_db,
            "soapy_direct rx active (2ch, chan0 forwarded, chan1 discarded)"
        );
        Ok(())
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let n = self.output.slice().len();
        if n == 0 {
            return Ok(());
        }
        if self.scratch0.len() < n {
            self.scratch0.resize(n, Complex32::new(0.0, 0.0));
        }
        if self.discard.len() < n {
            self.discard.resize(n, Complex32::new(0.0, 0.0));
        }
        let rx = self.rx.as_mut().ok_or_else(|| anyhow!("rx not active"))?;
        let (chan0, chan1) = {
            let s0 = self.scratch0.as_mut_slice();
            let s1 = self.discard.as_mut_slice();
            (&mut s0[..n], &mut s1[..n])
        };
        let result = rx.read(&mut [chan0, chan1], 500_000);
        match result {
            Ok(len) => {
                let dst = self.output.slice();
                dst[..len].copy_from_slice(&self.scratch0[..len]);
                self.output.produce(len);
                io.call_again = true;
            }
            Err(e) => {
                use soapysdr::ErrorCode;
                match e.code {
                    ErrorCode::Overflow => {
                        tracing::warn!("soapy rx overflow");
                    }
                    ErrorCode::Timeout => {}
                    other => {
                        tracing::error!(?other, "soapy rx error");
                        io.finished = true;
                    }
                }
            }
        }
        Ok(())
    }

    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        if let Some(rx) = self.rx.as_mut() {
            let _ = rx.deactivate(None);
        }
        Ok(())
    }
}

/// Static configuration for the TX sink.
#[derive(Debug, Clone)]
pub struct SoapyTxConfig {
    pub freq_hz: f64,
    pub rate_hz: f64,
    pub gain_db: f64,
    pub antenna: Option<String>,
    /// Activation offset in nanoseconds (typical 200 ms = 200_000_000).
    /// Streamer activates at `get_hardware_time + activation_offset_ns`
    /// so the host has time to pre-fill UHD's internal ring before any
    /// RF is emitted.
    pub activation_offset_ns: i64,
}

/// Wall-clock idle threshold: time since last input arrival before a
/// pending burst is flushed. Tradeoff: lower = more responsive but
/// risks fragmenting one LoRa frame across multiple flushes if the
/// scheduler hiccups; higher = more latency before TX hits the air.
/// 100 ms is well above any FutureSDR scheduler hiccup we observe and
/// well below the inter-frame gap of any sane sender.
const IDLE_FLUSH_MS: u64 = 100;

/// Sleep duration on idle ticks so the FutureSDR scheduler can run
/// upstream blocks instead of busy-spinning this sink.
const IDLE_TICK_SLEEP_MS: u64 = 5;

/// Cap pending burst at 16 M complex samples (~128 MB). Bound against
/// pathological infinite-input scenarios; at LoRa rates this is dozens
/// of seconds of audio — far above any realistic burst.
const MAX_BURST_SAMPLES: usize = 16 * 1024 * 1024;

/// Direct-soapysdr TX sink block. Accumulate-then-flush with wall-
/// clock idle detection. Streamer is dormant between bursts so the
/// AD9361 PA never has to sustain wire-rate fed-empty operation —
/// continuous-streaming was tried and confirmed to corrupt the USB
/// transport on this clone (SSSS storm → `LIBUSB_ERROR_NO_DEVICE`).
///
/// Single Complex32 input port; channel-1 zero IQ is synthesized
/// internally to engage the second DUC chain in the FPGA (required
/// by this clone for the TX path to engage cleanly).
#[derive(Block)]
#[blocking]
pub struct SoapyDirectSink {
    #[input]
    input: DefaultCpuReader<Complex32>,
    dev: soapysdr::Device,
    cfg: SoapyTxConfig,
    tx: Option<soapysdr::TxStream<Complex32>>,
    /// Pre-allocated zero IQ buffer for chan-1 zero-fill.
    zero: Vec<Complex32>,
    /// Cached MTU (bounded at 2040). Resolved in `init()`.
    mtu: usize,
    /// Pending burst — accumulated from upstream across multiple
    /// `work()` ticks until idle threshold is reached.
    pending: Vec<Complex32>,
    /// Wall-clock instant of the most recent input append. `None`
    /// means buffer is empty / no burst in progress.
    last_input_at: Option<std::time::Instant>,
}

impl SoapyDirectSink {
    pub fn new(dev: soapysdr::Device, cfg: SoapyTxConfig) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            dev,
            cfg,
            tx: None,
            zero: vec![Complex32::new(0.0, 0.0); 4096],
            mtu: 2040,
            pending: Vec::new(),
            last_input_at: None,
        }
    }

    fn ensure_zero(&mut self, n: usize) {
        if self.zero.len() < n {
            self.zero.resize(n, Complex32::new(0.0, 0.0));
        }
    }

    /// Activate streamer at `now + activation_offset_ns`, tight-loop
    /// write the whole burst in MTU-sized chunks (last chunk carries
    /// `end_burst=true`), drain UHD async events, sleep for the FPGA
    /// TX FIFO to drain, then deactivate. Matches the proven probe
    /// recipe in `tmp/soapy-tx-probe/`.
    fn flush_burst(&mut self, burst: &[Complex32]) -> Result<()> {
        let mtu = self.mtu;
        self.ensure_zero(mtu);
        let now_ns = self.dev.get_hardware_time(None).unwrap_or(0);
        let activate_at = now_ns + self.cfg.activation_offset_ns;

        {
            let tx = self.tx.as_mut().ok_or_else(|| anyhow!("tx not open"))?;
            tx.activate(Some(activate_at))
                .map_err(|e| anyhow!("tx.activate: {e:?}"))?;
        }
        tracing::info!(activate_at, samples = burst.len(), mtu, "burst flush start");

        let mut idx = 0;
        while idx < burst.len() {
            let take = (burst.len() - idx).min(mtu);
            let last = idx + take == burst.len();
            let real = &burst[idx..idx + take];
            let zero_slice = &self.zero[..take];
            let written = {
                let tx = self.tx.as_mut().unwrap();
                tx.write(&[real, zero_slice], None, last, 5_000_000)
                    .map_err(|e| anyhow!("tx.write: {e:?}"))?
            };
            if written == 0 {
                return Err(anyhow!("tx.write returned 0; ring stalled"));
            }
            idx += written;
        }

        self.drain_async();
        // FPGA TX FIFO drain window. Matches the proven probe recipe;
        // shorter values race the deactivate against in-flight DAC
        // samples on this clone.
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let Some(tx) = self.tx.as_mut() {
            let _ = tx.deactivate(None);
        }
        tracing::info!(written = idx, "burst flush end");
        Ok(())
    }

    /// Non-blocking drain of UHD async events. Logs underflow / time-
    /// error / corruption.
    fn drain_async(&mut self) {
        use soapysdr::ErrorCode;
        let tx = match self.tx.as_mut() {
            Some(t) => t,
            None => return,
        };
        loop {
            let mut chan_mask = 0usize;
            let mut ev_flags = 0i32;
            let mut t_ns = 0i64;
            match tx.read_status(&mut chan_mask, &mut ev_flags, &mut t_ns, 0) {
                Ok(_) => {
                    tracing::trace!(flags = format!("0x{:x}", ev_flags), "tx async ok");
                }
                Err(e) => match e.code {
                    ErrorCode::Timeout => break,
                    ErrorCode::Underflow => {
                        tracing::warn!("tx underflow");
                    }
                    ErrorCode::TimeError => {
                        tracing::warn!("tx time_error");
                    }
                    ErrorCode::Corruption => {
                        tracing::error!("tx corruption");
                    }
                    other => {
                        tracing::warn!(?other, "tx async error");
                        break;
                    }
                },
            }
        }
    }
}

impl Kernel for SoapyDirectSink {
    async fn init(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        // Configure both TX channels (0 = real, 1 = zero-fill). UHD
        // requires consistent config to engage the second DUC chain.
        for chan in [0usize, 1usize] {
            self.dev
                .set_sample_rate(Direction::Tx, chan, self.cfg.rate_hz)
                .with_context(|| format!("set_sample_rate(Tx, {chan})"))?;
            self.dev
                .set_frequency(Direction::Tx, chan, self.cfg.freq_hz, "")
                .with_context(|| format!("set_frequency(Tx, {chan})"))?;
            let gain = if chan == 0 { self.cfg.gain_db } else { 0.0 };
            self.dev
                .set_gain(Direction::Tx, chan, gain)
                .with_context(|| format!("set_gain(Tx, {chan})"))?;
            if let Some(ant) = &self.cfg.antenna {
                let _ = self.dev.set_antenna(Direction::Tx, chan, ant.as_str());
            }
        }

        let tx = self
            .dev
            .tx_stream::<Complex32>(&[0, 1])
            .map_err(|e| anyhow!("tx_stream(2ch): {e:?}"))?;
        let mtu = tx.mtu().unwrap_or(2040).min(2040);
        self.mtu = mtu;
        self.ensure_zero(mtu);
        // Streamer stays dormant between bursts — `flush_burst`
        // activates / writes / deactivates per LoRa frame.
        self.tx = Some(tx);
        tracing::info!(
            freq = self.cfg.freq_hz,
            rate = self.cfg.rate_hz,
            gain = self.cfg.gain_db,
            mtu,
            "soapy_direct tx ready (2ch, dormant; activates per burst)"
        );
        Ok(())
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let n_input = self.input.slice().len();

        if n_input > 0 {
            let room = MAX_BURST_SAMPLES.saturating_sub(self.pending.len());
            let take = n_input.min(room);
            if take > 0 {
                let src: &[Complex32] = self.input.slice();
                self.pending.extend_from_slice(&src[..take]);
                self.input.consume(take);
                self.last_input_at = Some(std::time::Instant::now());
            }
            io.call_again = true;
            return Ok(());
        }

        if self.pending.is_empty() {
            // Truly idle. Yield to the scheduler so upstream can run
            // when input arrives. Brief blocking sleep is the
            // simplest way to avoid busy-spinning a `#[blocking]`
            // kernel thread.
            std::thread::sleep(std::time::Duration::from_millis(IDLE_TICK_SLEEP_MS));
            io.call_again = true;
            return Ok(());
        }

        let elapsed = self.last_input_at.map(|t| t.elapsed()).unwrap_or_default();
        if elapsed < std::time::Duration::from_millis(IDLE_FLUSH_MS) {
            std::thread::sleep(std::time::Duration::from_millis(IDLE_TICK_SLEEP_MS));
            io.call_again = true;
            return Ok(());
        }

        let burst = std::mem::take(&mut self.pending);
        self.last_input_at = None;
        let res = self.flush_burst(&burst);
        io.call_again = true;
        res
    }

    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        if let Some(tx) = self.tx.as_mut() {
            // Empty write with end_burst=true to flush UHD's FIFO
            // cleanly; brief drain window before deactivate.
            let _ = tx.write(&[&[], &[]], None, true, 1_000_000);
            std::thread::sleep(std::time::Duration::from_millis(400));
            let _ = tx.deactivate(None);
        }
        Ok(())
    }
}
