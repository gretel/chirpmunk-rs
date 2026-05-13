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
//! TX strategy — continuous streaming with chunked writes across
//! scheduler ticks.  Stream activates once at init (untimed, no
//! HAS_TIME), stays active for the block lifetime.  Each `work()`
//! tick writes at most one MTU-sized chunk so the scheduler can
//! interleave RX processing between chunks.  Idle ticks sleep and
//! let the DAC underflow (tolerated — underflows are logged, not
//! fatal).  Deactivate only at deinit.
//!
//! This replaces the previous per-burst timed-activation pattern
//! (`activate(Some(now+offset))` + `end_burst=true` + drain +
//! deactivate) which was confirmed by gr4-lora commit 0fd7434 to
//! hang B220 TX by blocking the scheduler on first processBulk.

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
}

/// Direct-soapysdr RX source block.
///
/// Always opens RX on TWO channels — the B200 family enforces symmetric
/// channel counts when paired with a 2-channel TX stream — and exposes
/// both as independent stream output ports (`out0`, `out1`). Callers
/// that only want single-antenna RX wire `out1` to a `NullSink`; the
/// hardware still reads channel 1 because driver symmetry demands it.
#[derive(Block)]
#[blocking]
pub struct SoapyDirectSource {
    #[output]
    out0: DefaultCpuWriter<Complex32>,
    #[output]
    out1: DefaultCpuWriter<Complex32>,
    dev: soapysdr::Device,
    cfg: SoapyRxConfig,
    rx: Option<soapysdr::RxStream<Complex32>>,
}

impl SoapyDirectSource {
    pub fn new(dev: soapysdr::Device, cfg: SoapyRxConfig) -> Self {
        Self {
            out0: DefaultCpuWriter::default(),
            out1: DefaultCpuWriter::default(),
            dev,
            cfg,
            rx: None,
        }
    }
}

impl Kernel for SoapyDirectSource {
    async fn init(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
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
        let now_ns = self.dev.get_hardware_time(None).unwrap_or(0);
        rx.activate(Some(now_ns + 100_000_000))
            .map_err(|e| anyhow!("rx activate: {e:?}"))?;
        self.rx = Some(rx);
        tracing::info!(
            freq = self.cfg.freq_hz,
            rate = self.cfg.rate_hz,
            gain = self.cfg.gain_db,
            "soapy_direct rx active (2ch, both forwarded as out0/out1)"
        );
        Ok(())
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let n = {
            let s0 = self.out0.slice();
            let s1 = self.out1.slice();
            s0.len().min(s1.len())
        };
        if n == 0 {
            return Ok(());
        }
        let rx = self.rx.as_mut().ok_or_else(|| anyhow!("rx not active"))?;
        let result = {
            let chan0: &mut [Complex32] = &mut self.out0.slice()[..n];
            let chan1: &mut [Complex32] = &mut self.out1.slice()[..n];
            rx.read(&mut [chan0, chan1], 500_000)
        };
        match result {
            Ok(len) => {
                self.out0.produce(len);
                self.out1.produce(len);
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
}

/// Sleep duration on idle ticks so the FutureSDR scheduler can run
/// upstream blocks instead of busy-spinning this sink.
const IDLE_TICK_SLEEP_MS: u64 = 5;

/// Cap pending burst at 16 M complex samples (~128 MB). Bound against
/// pathological infinite-input scenarios; at LoRa rates this is dozens
/// of seconds of audio — far above any realistic burst.
const MAX_BURST_SAMPLES: usize = 16 * 1024 * 1024;

/// Write one MTU-sized chunk of `burst[start..]` to the TX stream.
/// Channel 0 carries real IQ; channel 1 is zero-filled.
/// Does NOT set `end_burst`.
fn write_tx_chunk(
    tx: &mut soapysdr::TxStream<Complex32>,
    zero: &[Complex32],
    mtu: usize,
    burst: &[Complex32],
    start: usize,
) -> Result<usize> {
    let remaining = burst.len() - start;
    let take = remaining.min(mtu);
    if take == 0 {
        return Ok(0);
    }
    let real = &burst[start..start + take];
    let zero_slice = &zero[..take];
    let written = tx
        .write(&[real, zero_slice], None, false, 5_000_000)
        .map_err(|e| anyhow!("tx.write: {e:?}"))?;
    if written == 0 {
        return Err(anyhow!("tx.write returned 0; ring stalled"));
    }
    Ok(written)
}

/// Direct-soapysdr TX sink block. Continuous streaming mode with
/// chunked writes across scheduler ticks.
///
/// Stream activates once at init (untimed) and stays active for the
/// block lifetime.  Each `work()` tick writes at most one MTU-sized
/// chunk so the scheduler can interleave RX processing between chunks.
/// Deactivate only at deinit.
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
    /// Burst being actively written across scheduler ticks. Empty
    /// means no write in progress.
    write_burst: Vec<Complex32>,
    /// How many samples of `write_burst` have already been written.
    write_cursor: usize,
    /// Whether the TX stream is currently activated. Activated per burst,
    /// deactivated when the burst is fully written so the PA is off
    /// between transmissions.
    stream_active: bool,
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
            write_burst: Vec::new(),
            write_cursor: 0,
            stream_active: false,
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

        self.tx = Some(tx);
        tracing::info!(
            freq = self.cfg.freq_hz,
            rate = self.cfg.rate_hz,
            gain = self.cfg.gain_db,
            mtu,
            "soapy_direct tx ready (2ch, activates per burst)"
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

        if n_input > 0 && self.write_cursor == 0 {
            let room = MAX_BURST_SAMPLES.saturating_sub(self.pending.len());
            let take = n_input.min(room);
            if take > 0 {
                let src: &[Complex32] = self.input.slice();
                self.pending.extend_from_slice(&src[..take]);
                self.input.consume(take);
            }
            io.call_again = true;
            return Ok(());
        }

        if self.write_cursor > 0 {
            self.ensure_zero(self.mtu);
            let tx = self.tx.as_mut().ok_or_else(|| anyhow!("tx not open"))?;
            let zero = &self.zero[..self.mtu];
            let burst = &self.write_burst;
            let cursor = self.write_cursor;
            let written = write_tx_chunk(tx, zero, self.mtu, burst, cursor)?;
            self.write_cursor += written;
            if self.write_cursor >= self.write_burst.len() {
                // Burst fully written — deactivate stream so PA is off
                // between bursts.  If daemon dies now, no RF emission.
                let _ = tx.deactivate(None);
                self.stream_active = false;
                self.write_burst.clear();
                self.write_cursor = 0;
            }
            io.call_again = true;
            return Ok(());
        }

        if self.pending.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(IDLE_TICK_SLEEP_MS));
            io.call_again = true;
            return Ok(());
        }

        // Start a new burst: activate stream, write first chunk.
        self.write_burst = std::mem::take(&mut self.pending);
        self.write_cursor = 0;

        self.ensure_zero(self.mtu);
        let tx = self.tx.as_mut().ok_or_else(|| anyhow!("tx not open"))?;
        tx.activate(None)
            .map_err(|e| anyhow!("tx.activate: {e:?}"))?;
        self.stream_active = true;

        let zero = &self.zero[..self.mtu];
        let burst = &self.write_burst;
        let written = write_tx_chunk(tx, zero, self.mtu, burst, 0)?;
        self.write_cursor = written;
        if self.write_cursor >= self.write_burst.len() {
            let _ = tx.deactivate(None);
            self.stream_active = false;
            self.write_burst.clear();
            self.write_cursor = 0;
        }
        io.call_again = true;
        Ok(())
    }

    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        if !self.stream_active {
            return Ok(());
        }
        self.ensure_zero(self.mtu);
        #[allow(clippy::collapsible_if)]
        if !self.write_burst.is_empty() && self.write_cursor < self.write_burst.len() {
            if let Some(tx) = self.tx.as_mut() {
                let remaining = &self.write_burst[self.write_cursor..];
                let zero = &self.zero[..self.mtu];
                let mut idx = 0;
                while idx < remaining.len() {
                    if let Ok(written) = write_tx_chunk(tx, zero, self.mtu, remaining, idx) {
                        idx += written;
                    } else {
                        break;
                    }
                }
            }
        }
        if let Some(tx) = self.tx.as_mut() {
            let _ = tx.deactivate(None);
        }
        Ok(())
    }
}
