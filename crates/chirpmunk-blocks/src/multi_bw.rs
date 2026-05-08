// SPDX-License-Identifier: GPL-3.0-only

//! Multi-BW × Multi-SF decoder grid.
//!
//! Outer fanout: `StreamDuplicator<Complex32, MAX_BW>`. Each populated tap
//! feeds a `build_multi_sf_rx` instance with `os_factor = sample_rate / bw`.
//! Unused taps are plugged with `NullSink<Complex32>` so the duplicator's
//! per-output FIFOs stay drained.
//!
//! Per-BW resampling is intentionally skipped: chirpmunk-phy's decoder is
//! parametric in `os_factor`, so the same input rate can drive multiple
//! BWs concurrently as long as `sample_rate % bw == 0`.

use std::sync::Arc;

use anyhow::anyhow;
use chirpmunk_phy::utils::{Bandwidth, Channel, SynchWord};
use futuresdr::blocks::{NullSink, StreamDuplicator};
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

use crate::{DedupState, FrameSinkConfig, MultiSfRx, build_multi_sf_rx};

/// Maximum number of bandwidths supported per radio channel.
/// Limited by the literal-only index requirement of FutureSDR's
/// `connect!` macro on `StreamDuplicator::outputs[N]`.
pub const MAX_BW: usize = 4;

pub struct MultiBwRx {
    pub entry: BlockId,
    pub bandwidths: Vec<u32>,
}

fn build_branch(
    fg: &mut Flowgraph,
    chan: Channel,
    bw_hz: u32,
    sync_word: SynchWord,
    sample_rate: u64,
    cfg_template: &FrameSinkConfig,
    dedup: Arc<DedupState>,
) -> Result<MultiSfRx> {
    let os = (sample_rate / u64::from(bw_hz)) as usize;
    let bw_typed =
        Bandwidth::try_from(bw_hz).map_err(|_| anyhow!("multi_bw: invalid bandwidth {}", bw_hz))?;
    let mut cfg = cfg_template.clone();
    cfg.bw = bw_hz;
    cfg.decode_label = Some(format!(
        "{}-bw{}k",
        cfg_template.decode_label.as_deref().unwrap_or("rx"),
        bw_hz / 1_000
    ));
    build_multi_sf_rx(fg, chan, bw_typed, sync_word, os, cfg, dedup)
}

/// Build a multi-BW × multi-SF (SF7..SF12) RX grid.
///
/// Pre-conditions:
/// * `bandwidths` non-empty, `<= MAX_BW`.
/// * `sample_rate` divisible by every BW (so `os_factor` is integer).
pub fn build_multi_bw_rx(
    mut fg: &mut Flowgraph,
    chan: Channel,
    bandwidths: &[u32],
    sync_word: SynchWord,
    sample_rate: u64,
    cfg_template: FrameSinkConfig,
    dedup: Arc<DedupState>,
) -> Result<MultiBwRx> {
    if bandwidths.is_empty() {
        return Err(anyhow!("multi_bw: bandwidths must be non-empty"));
    }
    if bandwidths.len() > MAX_BW {
        return Err(anyhow!(
            "multi_bw: bandwidths.len()={} exceeds MAX_BW={}",
            bandwidths.len(),
            MAX_BW
        ));
    }
    for &bw in bandwidths {
        if bw == 0 {
            return Err(anyhow!("multi_bw: bw must be > 0"));
        }
        if !sample_rate.is_multiple_of(u64::from(bw)) {
            return Err(anyhow!(
                "multi_bw: sample_rate {} not a multiple of bw {}",
                sample_rate,
                bw
            ));
        }
    }

    let entry = fg.add(StreamDuplicator::<Complex32, MAX_BW>::new());

    // Hand-unrolled per N. The connect! macro requires literal output
    // indices, so we cannot loop. Each branch is either a multi-SF
    // sub-flowgraph (entry: BlockRef<StreamDuplicator<Complex32, 6>>)
    // or a NullSink<Complex32> for unused taps.
    match bandwidths.len() {
        1 => {
            let m0 = build_branch(
                fg,
                chan,
                bandwidths[0],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let s0 = m0.entry;
            let n1 = fg.add(NullSink::<Complex32>::new());
            let n2 = fg.add(NullSink::<Complex32>::new());
            let n3 = fg.add(NullSink::<Complex32>::new());
            connect!(fg,
                entry.outputs[0] > s0;
                entry.outputs[1] > n1;
                entry.outputs[2] > n2;
                entry.outputs[3] > n3;
            );
        }
        2 => {
            let m0 = build_branch(
                fg,
                chan,
                bandwidths[0],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let m1 = build_branch(
                fg,
                chan,
                bandwidths[1],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let s0 = m0.entry;
            let s1 = m1.entry;
            let n2 = fg.add(NullSink::<Complex32>::new());
            let n3 = fg.add(NullSink::<Complex32>::new());
            connect!(fg,
                entry.outputs[0] > s0;
                entry.outputs[1] > s1;
                entry.outputs[2] > n2;
                entry.outputs[3] > n3;
            );
        }
        3 => {
            let m0 = build_branch(
                fg,
                chan,
                bandwidths[0],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let m1 = build_branch(
                fg,
                chan,
                bandwidths[1],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let m2 = build_branch(
                fg,
                chan,
                bandwidths[2],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let s0 = m0.entry;
            let s1 = m1.entry;
            let s2 = m2.entry;
            let n3 = fg.add(NullSink::<Complex32>::new());
            connect!(fg,
                entry.outputs[0] > s0;
                entry.outputs[1] > s1;
                entry.outputs[2] > s2;
                entry.outputs[3] > n3;
            );
        }
        4 => {
            let m0 = build_branch(
                fg,
                chan,
                bandwidths[0],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let m1 = build_branch(
                fg,
                chan,
                bandwidths[1],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let m2 = build_branch(
                fg,
                chan,
                bandwidths[2],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let m3 = build_branch(
                fg,
                chan,
                bandwidths[3],
                sync_word,
                sample_rate,
                &cfg_template,
                dedup.clone(),
            )?;
            let s0 = m0.entry;
            let s1 = m1.entry;
            let s2 = m2.entry;
            let s3 = m3.entry;
            connect!(fg,
                entry.outputs[0] > s0;
                entry.outputs[1] > s1;
                entry.outputs[2] > s2;
                entry.outputs[3] > s3;
            );
        }
        _ => unreachable!("guarded above"),
    }

    let _ = &mut fg; // silence unused mut warning if connects don't reborrow

    Ok(MultiBwRx {
        entry: entry.into(),
        bandwidths: bandwidths.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg_template() -> FrameSinkConfig {
        FrameSinkConfig {
            sf: 7,
            bw: 125_000,
            cr: 4,
            sync_word: 0x12,
            device: None,
            decode_label: Some("rx".into()),
            rx_channel: Some(0),
        }
    }

    fn passthrough_dedup() -> Arc<DedupState> {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        DedupState::new(Duration::ZERO, tx)
    }

    #[test]
    fn rejects_empty() {
        let mut fg = Flowgraph::new();
        let r = build_multi_bw_rx(
            &mut fg,
            Channel::EU868_1,
            &[],
            SynchWord::from(0x12_u8),
            500_000,
            cfg_template(),
            passthrough_dedup(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn rejects_non_divisible_rate() {
        let mut fg = Flowgraph::new();
        let r = build_multi_bw_rx(
            &mut fg,
            Channel::EU868_1,
            &[125_000],
            SynchWord::from(0x12_u8),
            123_456,
            cfg_template(),
            passthrough_dedup(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn builds_two_bandwidths() {
        let mut fg = Flowgraph::new();
        let r = build_multi_bw_rx(
            &mut fg,
            Channel::EU868_1,
            &[125_000, 250_000],
            SynchWord::from(0x12_u8),
            500_000,
            cfg_template(),
            passthrough_dedup(),
        );
        assert!(r.is_ok(), "build_multi_bw_rx should succeed: {:?}", r.err());
        let h = r.unwrap();
        assert_eq!(h.bandwidths, vec![125_000, 250_000]);
    }
}
