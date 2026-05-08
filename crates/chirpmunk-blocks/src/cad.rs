// SPDX-License-Identifier: GPL-3.0-only

//! Listen-Before-Talk channel activity detector. Minimal-subset port of
//! gr4-lora `blocks/include/gnuradio-4.0/lora/ChannelActivityDetector.hpp`.
//!
//! Algorithm: Vangelista & Calvagno 2024, Algorithm 1, single-SF, AND mode.
//! Buffer two oversampled symbols (`2 * sf_size * os_factor` IQ samples,
//! `sf_size = 1 << sf`). For each symbol and each of `os_factor` sub-chip
//! timing offsets, decimate by `os_factor`, multiply elementwise by
//! `conj(upchirp_1x)` (= downchirp reference), FFT over `sf_size` bins,
//! and compute `peak_ratio = max|Y| / mean|Y|`. Keep the maximum across
//! offsets per symbol. Channel busy when BOTH symbols' max ratios exceed
//! `alpha`. Hysteresis: release after `release_symbols` consecutive
//! clear windows.
//!
//! The block writes through to a caller-owned `Arc<AtomicBool>` which the
//! TX dispatcher polls (see `tx_dispatch::wait_until_clear`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futuresdr::num_complex::Complex32;
use futuresdr::runtime::dev::prelude::*;
use rustfft::{Fft, FftPlanner};

const TWO_PI: f32 = std::f32::consts::TAU;

/// Build a single-rate up-chirp of length `1 << sf` (os_factor = 1).
/// Phase model `2π * (k²/(2N) − k/2)` matches gr4-lora
/// `blocks/include/gnuradio-4.0/lora/algorithm/utilities.hpp::build_ref_chirps`.
fn build_upchirp_1x(sf: u8) -> Vec<Complex32> {
    let n: usize = 1 << sf;
    let n_f = n as f32;
    (0..n)
        .map(|k| {
            let kf = k as f32;
            let phase = TWO_PI * (kf * kf / (2.0 * n_f) - kf / 2.0);
            Complex32::new(phase.cos(), phase.sin())
        })
        .collect()
}

/// Vangelista & Calvagno alpha:
///     α = √(−(4/π)·ln(1−(1−p_fa)^{1/L})) ; L = os_factor · M ; M = 2^sf
/// Default `p_fa = 0.001`. Higher SF and higher os_factor both raise α.
pub fn default_alpha(sf: u8, os_factor: u32) -> f32 {
    let m = (1u64 << sf) as f64;
    let l = f64::from(os_factor) * m;
    let p_fa = 0.001_f64;
    let inner = 1.0 - (1.0_f64 - p_fa).powf(1.0 / l);
    (-(4.0 / std::f64::consts::PI) * inner.ln()).sqrt() as f32
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DetectResult {
    pub peak_ratio_up: f32,
    pub up_detected: bool,
}

/// Single-SF, single-window detector. Owns a 2^sf FFT plan + ref-chirp
/// tables. Stateless across calls (each `process_window` is independent);
/// streaming wrapper handles accumulation.
pub struct Detector {
    os_factor: u32,
    sf_size: usize,
    sym_len: usize,
    downchirp: Vec<Complex32>,
    fft: Arc<dyn Fft<f32>>,
    fft_buf: Vec<Complex32>,
    alpha: f32,
}

impl Detector {
    pub fn new(sf: u8, os_factor: u32) -> Self {
        let sf_size = 1usize << sf;
        let sym_len = sf_size * os_factor as usize;
        let upchirp = build_upchirp_1x(sf);
        let downchirp: Vec<Complex32> = upchirp.iter().map(|c| c.conj()).collect();
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(sf_size);
        let alpha = default_alpha(sf, os_factor);
        Self {
            os_factor,
            sf_size,
            sym_len,
            downchirp,
            fft,
            fft_buf: vec![Complex32::default(); sf_size],
            alpha,
        }
    }

    pub fn alpha(&self) -> f32 {
        self.alpha
    }

    pub fn set_alpha(&mut self, alpha: f32) {
        self.alpha = alpha;
    }

    /// Best peak/mean ratio across `os_factor` sub-chip timing offsets.
    fn peak_ratio(&mut self, sym: &[Complex32]) -> f32 {
        debug_assert_eq!(sym.len(), self.sym_len);
        let mut best = 0.0_f32;
        for offset in 0..self.os_factor as usize {
            for k in 0..self.sf_size {
                self.fft_buf[k] = sym[offset + k * self.os_factor as usize] * self.downchirp[k];
            }
            self.fft.process(&mut self.fft_buf);
            let mut peak = 0.0_f32;
            let mut total = 0.0_f32;
            for &y in &self.fft_buf {
                let mag = (y.re * y.re + y.im * y.im).sqrt();
                total += mag;
                if mag > peak {
                    peak = mag;
                }
            }
            let mean = total / self.sf_size as f32;
            let ratio = if mean > 0.0 { peak / mean } else { 0.0 };
            if ratio > best {
                best = ratio;
            }
        }
        best
    }

    /// AND-mode 2-symbol detection. `win.len()` must be `>= 2 * sym_len`.
    pub fn process_window(&mut self, win: &[Complex32]) -> DetectResult {
        let need = self.sym_len * 2;
        debug_assert!(win.len() >= need, "window too short");
        let r1 = self.peak_ratio(&win[..self.sym_len]);
        let r2 = self.peak_ratio(&win[self.sym_len..need]);
        DetectResult {
            peak_ratio_up: r1.max(r2),
            up_detected: (r1 > self.alpha) && (r2 > self.alpha),
        }
    }
}

/// Streaming wrapper: accumulates input samples, emits busy/clear, applies
/// release-hysteresis. Writes through to an `Arc<AtomicBool>` so the TX
/// dispatch loop can poll it without lock contention.
pub struct StreamingDetector {
    detector: Detector,
    win_len: usize,
    accum: Vec<Complex32>,
    fill: usize,
    release_symbols: u8,
    clean_count: u8,
    busy: Arc<AtomicBool>,
}

impl StreamingDetector {
    pub fn new(sf: u8, os_factor: u32, release_symbols: u8, busy: Arc<AtomicBool>) -> Self {
        let detector = Detector::new(sf, os_factor);
        let win_len = detector.sym_len * 2;
        Self {
            accum: vec![Complex32::default(); win_len],
            fill: 0,
            detector,
            win_len,
            release_symbols,
            clean_count: 0,
            busy,
        }
    }

    pub fn detector_mut(&mut self) -> &mut Detector {
        &mut self.detector
    }

    /// Feed any number of samples; on each filled window run a detection.
    pub fn feed(&mut self, mut samples: &[Complex32]) {
        while !samples.is_empty() {
            let need = self.win_len - self.fill;
            let take = need.min(samples.len());
            self.accum[self.fill..self.fill + take].copy_from_slice(&samples[..take]);
            self.fill += take;
            samples = &samples[take..];
            if self.fill == self.win_len {
                let r = self.detector.process_window(&self.accum);
                if r.up_detected {
                    self.clean_count = 0;
                    self.busy.store(true, Ordering::Release);
                } else {
                    self.clean_count = self.clean_count.saturating_add(1);
                    if self.clean_count >= self.release_symbols {
                        self.busy.store(false, Ordering::Release);
                    }
                }
                self.fill = 0;
            }
        }
    }
}

/// FutureSDR block: stream-in `Complex32`, no stream-out. Drives the
/// caller-supplied `Arc<AtomicBool>`. Side-channel only — no Pmt outputs.
#[derive(Block)]
pub struct ChannelActivityDetector {
    streaming: StreamingDetector,
    #[input]
    input: DefaultCpuReader<Complex32>,
}

impl ChannelActivityDetector {
    pub fn new(sf: u8, os_factor: u32, release_symbols: u8, busy: Arc<AtomicBool>) -> Self {
        Self {
            streaming: StreamingDetector::new(sf, os_factor, release_symbols, busy),
            input: DefaultCpuReader::default(),
        }
    }

    /// Override the auto-derived alpha (e.g. from `cad_min_ratio` config).
    pub fn with_alpha(mut self, alpha: f32) -> Self {
        self.streaming.detector_mut().set_alpha(alpha);
        self
    }
}

impl Kernel for ChannelActivityDetector {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let n = self.input.slice().len();
        if n > 0 {
            let src: &[Complex32] = self.input.slice();
            self.streaming.feed(src);
            self.input.consume(n);
        }
        if self.input.finished() {
            io.finished = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upchirp_unit_magnitude() {
        let chirp = build_upchirp_1x(7);
        assert_eq!(chirp.len(), 128);
        for c in &chirp {
            let mag = (c.re * c.re + c.im * c.im).sqrt();
            assert!((mag - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn alpha_grows_with_sf() {
        let a7 = default_alpha(7, 1);
        let a12 = default_alpha(12, 1);
        assert!(a7 > 0.0);
        assert!(a12 > a7);
    }

    #[test]
    fn alpha_grows_with_os() {
        assert!(default_alpha(7, 4) > default_alpha(7, 1));
    }

    fn synth_oversampled_upchirp(sf: u8, os: u32) -> Vec<Complex32> {
        let one = build_upchirp_1x(sf);
        let mut out = Vec::with_capacity(one.len() * os as usize);
        for &s in &one {
            for _ in 0..os {
                out.push(s);
            }
        }
        out
    }

    #[test]
    fn detects_clean_chirp() {
        let sf = 7u8;
        let os = 4u32;
        let mut det = Detector::new(sf, os);
        let mut win = synth_oversampled_upchirp(sf, os);
        win.extend(synth_oversampled_upchirp(sf, os));
        let r = det.process_window(&win);
        assert!(r.up_detected, "clean chirp must detect");
        assert!(r.peak_ratio_up > default_alpha(sf, os));
    }

    #[test]
    fn rejects_silence() {
        let sf = 7u8;
        let os = 4u32;
        let mut det = Detector::new(sf, os);
        let n = (1usize << sf) * os as usize * 2;
        let win = vec![Complex32::new(0.0, 0.0); n];
        let r = det.process_window(&win);
        assert!(!r.up_detected);
    }

    #[test]
    fn streaming_busy_after_one_window() {
        let sf = 7u8;
        let os = 4u32;
        let busy = Arc::new(AtomicBool::new(false));
        let mut sd = StreamingDetector::new(sf, os, 4, busy.clone());
        let mut win = synth_oversampled_upchirp(sf, os);
        win.extend(synth_oversampled_upchirp(sf, os));
        sd.feed(&win);
        assert!(busy.load(Ordering::Acquire));
    }

    #[test]
    fn streaming_releases_after_hysteresis() {
        let sf = 7u8;
        let os = 4u32;
        let busy = Arc::new(AtomicBool::new(false));
        let release = 3u8;
        let mut sd = StreamingDetector::new(sf, os, release, busy.clone());
        let mut win = synth_oversampled_upchirp(sf, os);
        win.extend(synth_oversampled_upchirp(sf, os));
        sd.feed(&win);
        assert!(busy.load(Ordering::Acquire));
        let zeros = vec![Complex32::new(0.0, 0.0); win.len()];
        for _ in 0..release {
            sd.feed(&zeros);
        }
        assert!(!busy.load(Ordering::Acquire));
    }
}
