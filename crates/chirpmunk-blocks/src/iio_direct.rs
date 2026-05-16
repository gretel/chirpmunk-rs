// SPDX-License-Identifier: GPL-3.0-only

//! seify-IIO direct RX source block for chirpmunk-trx hardware mode.
//!
//! Uses seify's `DeviceTrait` with the `iio` backend (PlutoSDR via
//! libiio).  Single-channel RX — Pluto has 1 RX antenna.
//!
//! Replaces `SoapyDirectSource` when `driver = "iio"` in config.
//! Unlike the Soapy path, no buddy-share / dual-channel drain needed.

use anyhow::{Context, Result, anyhow};
use futuresdr::num_complex::Complex32;
use futuresdr::runtime::dev::prelude::*;
use seify::{Args, Device, Direction};

/// Static configuration for the IIO RX source.
#[derive(Debug, Clone)]
pub struct IioRxConfig {
    pub freq_hz: f64,
    pub rate_hz: f64,
    pub gain_db: f64,
    pub antenna: Option<String>,
    pub uri: String,
}

/// Open a PlutoSDR device via seify IIO backend.
pub fn open_iio_device(uri: &str) -> Result<Device<seify::GenericDevice>> {
    let args_str = format!("driver=iio, uri={uri}");
    let args = Args::from(args_str.as_str()).map_err(|e| anyhow!("Args::from: {e:?}"))?;
    let dev = Device::from_args(args).map_err(|e| anyhow!("Device::from_args(iio): {e:?}"))?;
    Ok(dev)
}

/// seify-IIO RX source block.  Single output port (Pluto is 1RX).
#[derive(Block)]
#[blocking]
pub struct IioDirectSource {
    #[output]
    out: DefaultCpuWriter<Complex32>,
    dev: Device<seify::GenericDevice>,
    cfg: IioRxConfig,
    rx: Option<Box<dyn seify::RxStreamer>>,
}

impl IioDirectSource {
    pub fn new(dev: Device<seify::GenericDevice>, cfg: IioRxConfig) -> Self {
        Self {
            out: DefaultCpuWriter::default(),
            dev,
            cfg,
            rx: None,
        }
    }
}

impl Kernel for IioDirectSource {
    async fn init(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        self.dev
            .set_sample_rate(Direction::Rx, 0, self.cfg.rate_hz)
            .with_context(|| "set_sample_rate(Rx)")?;
        self.dev
            .set_frequency(Direction::Rx, 0, self.cfg.freq_hz)
            .with_context(|| "set_frequency(Rx)")?;
        self.dev
            .set_gain(Direction::Rx, 0, self.cfg.gain_db)
            .with_context(|| "set_gain(Rx)")?;
        if let Some(ant) = &self.cfg.antenna {
            self.dev
                .set_antenna(Direction::Rx, 0, ant.as_str())
                .with_context(|| "set_antenna(Rx)")?;
        }

        // AGC slow_attack on Pluto
        let _ = self.dev.enable_agc(Direction::Rx, 0, true);

        let mut rx = self
            .dev
            .rx_streamer_with_args(&[0], Args::new())
            .with_context(|| "rx_streamer")?;
        rx.activate().map_err(|e| anyhow!("rx activate: {e:?}"))?;
        self.rx = Some(rx);

        tracing::info!(
            freq = self.cfg.freq_hz,
            rate = self.cfg.rate_hz,
            gain = self.cfg.gain_db,
            uri = self.cfg.uri,
            "iio_direct rx active (1ch)"
        );
        Ok(())
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let n = self.out.slice().len();
        if n == 0 {
            return Ok(());
        }
        let rx = self.rx.as_mut().ok_or_else(|| anyhow!("rx not active"))?;
        let buf: &mut [Complex32] = &mut self.out.slice()[..n];
        match rx.read(&mut [buf], 500_000) {
            Ok(len) => {
                self.out.produce(len);
                io.call_again = true;
            }
            Err(seify::Error::Overflow) => {
                tracing::warn!("iio rx overflow");
            }
            Err(seify::Error::Inactive) => {
                tracing::warn!("iio rx inactive (buffer cancelled)");
                io.finished = true;
            }
            Err(e) => {
                tracing::error!(error = %e, "iio rx error");
                io.finished = true;
            }
        }
        Ok(())
    }

    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        if let Some(rx) = self.rx.as_mut() {
            let _ = rx.deactivate();
        }
        Ok(())
    }
}

// =========================================================================
// TX Sink
// =========================================================================

/// Static configuration for the IIO TX sink.
#[derive(Debug, Clone)]
pub struct IioTxConfig {
    pub freq_hz: f64,
    pub rate_hz: f64,
    pub gain_db: f64,
}

/// seify-IIO TX sink block.  Single input port (Pluto is 1TX).
///
/// Continuous streaming mode – stream activates once in init() and
/// stays active for the block lifetime.  Each work() tick writes at
/// most one chunk so the scheduler can interleave RX processing.
/// Deactivate only at deinit.
#[derive(Block)]
#[blocking]
pub struct IioDirectSink {
    #[input]
    input: DefaultCpuReader<Complex32>,
    dev: Device<seify::GenericDevice>,
    cfg: IioTxConfig,
    tx: Option<Box<dyn seify::TxStreamer>>,
    /// Samples accumulated from upstream, to be written in chunks.
    pending: Vec<Complex32>,
    /// Whether the TX stream has been activated (once).
    stream_active: bool,
    /// How many samples of `pending` have already been written.
    write_cursor: usize,
}

impl IioDirectSink {
    pub fn new(dev: Device<seify::GenericDevice>, cfg: IioTxConfig) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            dev,
            cfg,
            tx: None,
            pending: Vec::new(),
            write_cursor: 0,
            stream_active: false,
        }
    }
}

impl Kernel for IioDirectSink {
    async fn init(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        self.dev
            .set_sample_rate(Direction::Tx, 0, self.cfg.rate_hz)
            .with_context(|| "set_sample_rate(Tx)")?;
        self.dev
            .set_frequency(Direction::Tx, 0, self.cfg.freq_hz)
            .with_context(|| "set_frequency(Tx)")?;
        self.dev
            .set_gain(Direction::Tx, 0, self.cfg.gain_db)
            .with_context(|| "set_gain(Tx)")?;

        let mut tx = self
            .dev
            .tx_streamer_with_args(&[0], Args::new())
            .with_context(|| "tx_streamer")?;
        tracing::info!("iio tx streamer created, activating...");
        tx.activate().map_err(|e| anyhow!("tx activate: {e:?}"))?;
        tracing::info!("iio tx streamer activated");
        self.stream_active = true;
        self.tx = Some(tx);

        tracing::info!(
            freq = self.cfg.freq_hz,
            rate = self.cfg.rate_hz,
            gain = self.cfg.gain_db,
            "iio_direct tx ready (1ch)"
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

        // Accumulate samples from upstream (up to a reasonable cap)
        const MAX_PENDING: usize = 16 * 1024 * 1024; // 128 MB
        if n_input > 0 && self.pending.len() < MAX_PENDING {
            let src: &[Complex32] = self.input.slice();
            let take = n_input.min(MAX_PENDING - self.pending.len());
            self.pending.extend_from_slice(&src[..take]);
            self.input.consume(take);
            io.call_again = true;
            return Ok(());
        }

        // Write one chunk of pending samples per tick.
        // Stream was activated in init(), stays active for block lifetime.
        if self.write_cursor < self.pending.len() {
            let tx = self
                .tx
                .as_mut()
                .ok_or_else(|| anyhow!("tx streamer not initialized"))?;

            let remaining = self.pending.len() - self.write_cursor;
            let chunk = remaining.min(2040);
            let slice = &self.pending[self.write_cursor..self.write_cursor + chunk];
            let is_last = self.write_cursor + chunk >= self.pending.len();
            tx.write_all(&[slice], None, is_last, 5_000_000)
                .map_err(|e| anyhow!("tx write_all: {e:?}"))?;
            self.write_cursor += chunk;
            if is_last {
                tracing::debug!(samples = self.pending.len(), "tx burst complete");
                self.pending.clear();
                self.write_cursor = 0;
            }
            io.call_again = true;
            return Ok(());
        }

        // Idle: nothing to send
        std::thread::sleep(std::time::Duration::from_millis(5));
        io.call_again = true;
        Ok(())
    }

    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        // Flush any remaining pending samples with end_burst=true
        if !self.pending.is_empty() {
            if let Some(tx) = self.tx.as_mut() {
                let mut idx = 0;
                while idx < self.pending.len() {
                    let chunk = (self.pending.len() - idx).min(2040);
                    let slice = &self.pending[idx..idx + chunk];
                    let is_last = idx + chunk >= self.pending.len();
                    let _ = tx.write_all(&[slice], None, is_last, 5_000_000);
                    idx += chunk;
                }
            }
            self.pending.clear();
            self.write_cursor = 0;
        }
        if self.stream_active {
            if let Some(tx) = self.tx.as_mut() {
                let _ = tx.deactivate();
            }
            self.stream_active = false;
        }
        Ok(())
    }
}
