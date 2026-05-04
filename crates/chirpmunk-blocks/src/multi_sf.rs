// SPDX-License-Identifier: GPL-3.0-only

//! Multi-SF parallel-chain RX. Fans one IQ stream out to six independent
//! decoders (SF7..SF12) via FutureSDR's `StreamDuplicator`. Each chain
//! emits to its own FrameSink so the caller can distinguish which SF
//! decoded the frame.
//!
//! Parallel chains over a single lockstep `MultiSfDecoder` block: the
//! gr4-lora C++ block batches all SFs in one block to amortise scheduler
//! cost. Worth porting later if benchmarks demand it. For now,
//! parallelism is clearer and keeps blocks orthogonal.

use chirpmunk_phy::build_lora_rx_soft_decoding;
use chirpmunk_phy::utils::{Bandwidth, Channel, HeaderMode, LdroMode, SpreadingFactor, SynchWord};

use futuresdr::blocks::StreamDuplicator;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

use crate::{FrameSink, FrameSinkConfig, Outbound};
use tokio::sync::mpsc::UnboundedSender;

/// SF7..SF12 in chain order. Index 0 = SF7, …, 5 = SF12.
pub const ALL_SF: [SpreadingFactor; 6] = [
    SpreadingFactor::SF7,
    SpreadingFactor::SF8,
    SpreadingFactor::SF9,
    SpreadingFactor::SF10,
    SpreadingFactor::SF11,
    SpreadingFactor::SF12,
];

/// Result of [`build_multi_sf_rx`]. Caller wires their IQ stream into
/// `entry`.
pub struct MultiSfRx {
    pub entry: BlockRef<StreamDuplicator<Complex32, 6>>,
}

/// Build six parallel SF chains sharing one duplicator. All chains
/// publish CBOR `lora_frame`s onto the shared `tx` mpsc.
pub fn build_multi_sf_rx(
    mut fg: &mut Flowgraph,
    chan: Channel,
    bw: Bandwidth,
    sync_word: SynchWord,
    os_factor: usize,
    cfg_template: FrameSinkConfig,
    tx: UnboundedSender<Outbound>,
) -> Result<MultiSfRx> {
    let entry = fg.add(StreamDuplicator::<Complex32, 6>::new());

    let chain = |mut fg: &mut Flowgraph, sf: SpreadingFactor| -> Result<BlockRef<_>> {
        let (frame_sync, decoder) = build_lora_rx_soft_decoding(
            fg,
            chan,
            bw,
            sf,
            HeaderMode::Explicit,
            LdroMode::AUTO,
            Some(&[sync_word]),
            os_factor,
            None,
            None,
            false,
            None,
        )?;
        let mut cfg = cfg_template.clone();
        cfg.sf = sf_to_u8(sf);
        let label = format!(
            "{}-sf{}",
            cfg_template.decode_label.as_deref().unwrap_or("rx"),
            cfg.sf
        );
        cfg.decode_label = Some(label);
        let frame_sink = fg.add(FrameSink::new(cfg, tx.clone()));
        connect!(fg, decoder.out_annotated | frame_sink;);
        Ok(frame_sync)
    };

    let fs0 = chain(fg, ALL_SF[0])?;
    let fs1 = chain(fg, ALL_SF[1])?;
    let fs2 = chain(fg, ALL_SF[2])?;
    let fs3 = chain(fg, ALL_SF[3])?;
    let fs4 = chain(fg, ALL_SF[4])?;
    let fs5 = chain(fg, ALL_SF[5])?;

    connect!(fg,
        entry.outputs[0] > fs0;
        entry.outputs[1] > fs1;
        entry.outputs[2] > fs2;
        entry.outputs[3] > fs3;
        entry.outputs[4] > fs4;
        entry.outputs[5] > fs5;
    );

    Ok(MultiSfRx { entry })
}

fn sf_to_u8(sf: SpreadingFactor) -> u8 {
    match sf {
        SpreadingFactor::SF5 => 5,
        SpreadingFactor::SF6 => 6,
        SpreadingFactor::SF7 => 7,
        SpreadingFactor::SF8 => 8,
        SpreadingFactor::SF9 => 9,
        SpreadingFactor::SF10 => 10,
        SpreadingFactor::SF11 => 11,
        SpreadingFactor::SF12 => 12,
    }
}
