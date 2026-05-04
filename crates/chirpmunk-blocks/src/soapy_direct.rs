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
//! Working recipe (from `references/tx-gap.md` skill):
//!   1. `master_clock_rate=24000000` in device args (HBF-clean for
//!      1 MS/s and 250 kS/s).
//!   2. TX stream opened on TWO channels `&[0, 1]` with chan-1 zero
//!      IQ. Activates the second DUC chain in the FPGA — required.
//!   3. `set_hardware_time(None, 0)` then `tx.activate(Some(t_future))`
//!      with `t_future` ~100–200 ms ahead. Streamer holds in deferred
//!      state; host pre-fills UHD's internal ring; UHD fires precisely.
//!   4. Tight write loop, no host pacing. UHD's writeStream blocks
//!      via the timeout argument when the ring is full — only working
//!      backpressure source on this hardware.
//!   5. `end_burst=true` on the last write of each LoRa burst, brief
//!      drain delay, then deactivate.

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
        // RxStream::read needs &mut [&mut [T]] (one mut slice per
        // channel). We split-borrow the two scratch Vecs separately,
        // then memcpy chan 0 into the FutureSDR output buffer. The
        // single-copy overhead is acceptable for a 1 MS/s RX path.
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
    pub activation_offset_ns: i64,
}

/// Direct-soapysdr TX sink block. Implements the gr4-lora SoapySink
/// wrapping pattern in per-burst activation mode (proven path on
/// LibreSDR_B220mini). Single Complex32 input port; channel-1 zero IQ
/// is synthesized internally.
///
/// State machine:
///   * Idle: streamer dormant, input ignored.
///   * Burst: streamer activated for the current burst, samples being
///     written until input drains or `end_burst` is sent.
#[derive(Block)]
#[blocking]
pub struct SoapyDirectSink {
    #[input]
    input: DefaultCpuReader<Complex32>,
    dev: soapysdr::Device,
    cfg: SoapyTxConfig,
    tx: Option<soapysdr::TxStream<Complex32>>,
    /// Pre-allocated zero IQ buffer for chan 1 zero-fill.
    zero: Vec<Complex32>,
    /// Whether we currently hold an active burst.
    burst_active: bool,
    /// How many work() calls we've seen with empty input while a
    /// burst is active. Three consecutive empty reads → end-of-burst.
    idle_polls: u32,
}

impl SoapyDirectSink {
    pub fn new(dev: soapysdr::Device, cfg: SoapyTxConfig) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            dev,
            cfg,
            tx: None,
            zero: vec![Complex32::new(0.0, 0.0); 4096],
            burst_active: false,
            idle_polls: 0,
        }
    }

    fn ensure_zero(&mut self, n: usize) {
        if self.zero.len() < n {
            self.zero.resize(n, Complex32::new(0.0, 0.0));
        }
    }
}

impl Kernel for SoapyDirectSink {
    async fn init(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        // Configure both TX channels (0 = real, 1 = zero-fill). Both
        // share the same RF settings — only chan 0's antenna matters
        // for actual emission, but UHD requires consistent config to
        // engage the second DUC chain.
        for chan in [0usize, 1usize] {
            self.dev
                .set_sample_rate(Direction::Tx, chan, self.cfg.rate_hz)
                .with_context(|| format!("set_sample_rate(Tx, {chan})"))?;
            self.dev
                .set_frequency(Direction::Tx, chan, self.cfg.freq_hz, "")
                .with_context(|| format!("set_frequency(Tx, {chan})"))?;
            // chan 0 gets the configured gain; chan 1 stays at 0 dB
            let gain = if chan == 0 { self.cfg.gain_db } else { 0.0 };
            self.dev
                .set_gain(Direction::Tx, chan, gain)
                .with_context(|| format!("set_gain(Tx, {chan})"))?;
            if let Some(ant) = &self.cfg.antenna {
                let _ = self.dev.set_antenna(Direction::Tx, chan, ant.as_str());
            }
        }

        // Open the streamer but DO NOT activate it. Activation is
        // done lazily per-burst in work() so the streamer is never
        // running with an empty FIFO (which corrupts USB transport on
        // this clone). Per-burst activate/deactivate matches the
        // proven `tmp/soapy-tx-probe/` recipe.
        let tx = self
            .dev
            .tx_stream::<Complex32>(&[0, 1])
            .map_err(|e| anyhow!("tx_stream(2ch): {e:?}"))?;

        self.tx = Some(tx);
        tracing::info!(
            freq = self.cfg.freq_hz,
            rate = self.cfg.rate_hz,
            gain = self.cfg.gain_db,
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
        // Per-burst state machine. Streamer is dormant when input is
        // empty and no burst in progress. When input arrives, activate
        // streamer at a near-future timestamp, drain the burst, send
        // end_burst on the last chunk, sleep for FPGA TX FIFO to
        // empty, then deactivate. Repeat per burst.
        let n_input = self.input.slice().len();

        if !self.burst_active {
            if n_input == 0 {
                // truly idle, nothing to do
                return Ok(());
            }
            // start a new burst
            let now_ns = self.dev.get_hardware_time(None).unwrap_or(0);
            let activate_at = now_ns + self.cfg.activation_offset_ns;
            let tx = self.tx.as_mut().ok_or_else(|| anyhow!("tx not open"))?;
            tx.activate(Some(activate_at))
                .map_err(|e| anyhow!("tx.activate: {e:?}"))?;
            self.burst_active = true;
            self.idle_polls = 0;
            tracing::debug!(activate_at, "burst start");
        }

        let mtu = self.tx.as_ref().unwrap().mtu().unwrap_or(2040);
        let chunk = mtu.min(2040);

        if n_input == 0 {
            // burst was active; input drained. Send a zero-padded
            // end_burst marker so UHD finalises the burst cleanly,
            // sleep to let the FPGA TX FIFO empty, then deactivate.
            self.idle_polls = self.idle_polls.saturating_add(1);
            if self.idle_polls < 3 {
                // give upstream a few work() ticks to deliver final
                // samples; some FutureSDR producers fragment bursts
                io.call_again = true;
                return Ok(());
            }
            self.ensure_zero(64);
            {
                let zero_slice = &self.zero[..64];
                let tx = self.tx.as_mut().unwrap();
                let _ = tx.write(&[zero_slice, zero_slice], None, true, 1_000_000);
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
            self.drain_async();
            if let Some(tx) = self.tx.as_mut() {
                let _ = tx.deactivate(None);
            }
            self.burst_active = false;
            self.idle_polls = 0;
            tracing::debug!("burst end");
            return Ok(());
        }

        // burst active + input has samples → write next chunk
        self.idle_polls = 0;
        let want = n_input.min(chunk);
        self.ensure_zero(want);
        let written = {
            let real = &self.input.slice()[..want];
            let zero_slice = &self.zero[..want];
            let tx = self.tx.as_mut().unwrap();
            tx.write(&[real, zero_slice], None, false, 1_000_000)
                .map_err(|e| anyhow!("tx.write: {e:?}"))?
        };
        self.input.consume(written);
        self.drain_async();
        io.call_again = true;
        Ok(())
    }

    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        if let Some(tx) = self.tx.as_mut() {
            // empty write with end_burst=true to flush the FIFO cleanly
            let _ = tx.write(&[&[], &[]], None, true, 1_000_000);
            std::thread::sleep(std::time::Duration::from_millis(400));
            let _ = tx.deactivate(None);
        }
        Ok(())
    }
}

impl SoapyDirectSink {
    /// Non-blocking drain of UHD async events. Logs underflow / time-
    /// error / corruption. Called after every successful write.
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
