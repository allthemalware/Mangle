//! Patch → runtime resolution.
//!
//! Pure functions that turn normalized patch values into the display-domain
//! structs the DSP consumes: `LaneParams`, `LfoParams`, global output
//! settings, slot settings. Tempo sync and per-note offset composition live
//! here too, so the nih-plug shell stays thin and everything is testable
//! without a host.
//!
//! Mod-matrix application slots in between `compose_voice_patch` and the
//! `lane_params`/`lfo` resolvers in build step 5.

use crate::curve::LfoParams;
use crate::engine::{GrainMode, LaneParams, PitchQuant};
use crate::params::{pid, Map, Module, ParamTable, Patch, DIV_BEATS};
use crate::sample::Algo;

#[inline]
fn disp(patch: &Patch, table: &ParamTable, m: Module, inst: u8, idx: u8) -> f32 {
    let pos = table.pos(pid(m, inst, idx));
    table.infos[pos].map.to_display(patch.values[pos])
}

/// Frequency of one cycle of tempo division `idx` at `bpm`.
pub fn div_to_hz(idx: usize, bpm: f32) -> f32 {
    let beats = DIV_BEATS[idx.min(DIV_BEATS.len() - 1)];
    (bpm.max(1.0) / 60.0) / beats
}

/// Resolve one grain engine. Returns the lane params plus the engine's
/// Gate Sync flag (the voice layer retriggers the Loop LFO with it).
pub fn lane_params(
    patch: &Patch,
    table: &ParamTable,
    engine: u8,
    bpm: f32,
    note: f32,
    voice_gate: bool,
    global_on: bool,
) -> (LaneParams, bool) {
    let d = |idx: u8| disp(patch, table, Module::Grain, engine, idx);
    let rate_hz = if d(2) >= 0.5 {
        div_to_hz(d(3) as usize, bpm)
    } else {
        d(1)
    };
    let key_track = disp(patch, table, Module::Global, 0, 1) / 100.0;
    let p = LaneParams {
        gate: global_on && voice_gate && d(0) >= 0.5,
        rate_hz,
        jitter: d(4) / 100.0,
        linked: d(5) >= 0.5,
        length_val: d(6),
        pan: d(7) / 100.0,
        level: d(8) / 100.0,
        sample_sel: d(9) / 100.0,
        position: d(10) / 100.0,
        loop_on: d(11) >= 0.5,
        loop_size: d(12),
        loop_pos: 0.0, // matrix Loop Position lands here post-resolve
        pitch_st: d(14),
        key_track,
        note,
        direction: d(16) / 100.0,
        shape: d(17) / 100.0,
        skew: d(18) / 100.0,
        pos_pitch: d(15) >= 0.5,
        algo: Algo::from_index(d(19) as usize),
        mode: GrainMode::from_index(d(20) as usize),
        quant: PitchQuant::from_index(d(21) as usize),
    };
    (p, d(13) >= 0.5)
}

/// Resolve a Loop LFO (`Module::LoopLfo`) or global LFO (`Module::Lfo`).
/// Returns the runtime params plus the Transport-sync flag.
pub fn lfo_params(
    patch: &Patch,
    table: &ParamTable,
    module: Module,
    inst: u8,
    bpm: f32,
) -> (LfoParams, bool) {
    let d = |idx: u8| disp(patch, table, module, inst, idx);
    let rate_hz = if d(3) >= 0.5 {
        div_to_hz(d(4) as usize, bpm)
    } else {
        d(2)
    };
    let p = LfoParams {
        rate_hz,
        phase: d(5) / 360.0,
        st_phase: d(6) / 100.0,
        lag: d(7) / 100.0,
        gate: d(0) >= 0.5,
        one_shot: d(9) >= 0.5,
    };
    (p, d(1) >= 0.5)
}

/// St.Phase (0..1) of a Drift or Noise unit.
pub fn st_phase(patch: &Patch, table: &ParamTable, module: Module, inst: u8) -> f32 {
    disp(patch, table, module, inst, 0) / 100.0
}

#[derive(Clone, Copy, Debug)]
pub struct GlobalOut {
    pub on: bool,
    /// 0..1 key tracking amount.
    pub key: f32,
    /// 0..2 stereo width.
    pub stereo: f32,
    pub squeeze: bool,
    /// Linear master gain.
    pub level: f32,
    pub voices: usize,
}

pub fn global_out(patch: &Patch, table: &ParamTable) -> GlobalOut {
    let d = |idx: u8| disp(patch, table, Module::Global, 0, idx);
    GlobalOut {
        on: d(0) >= 0.5,
        key: d(1) / 100.0,
        stereo: d(2) / 100.0,
        squeeze: d(3) >= 0.5,
        level: 10.0f32.powf(d(4) / 20.0),
        voices: d(5) as usize,
    }
}

/// Master filter settings (Module::Fx), resolved once per block from the base
/// patch. Applied in the output chain after the grain sum, before Squeeze.
#[derive(Clone, Copy, Debug)]
pub struct FilterOut {
    pub on: bool,
    pub ftype: crate::filter::FilterType,
    /// Cutoff in Hz.
    pub cutoff: f32,
    /// Resonance 0..1.
    pub reso: f32,
}

pub fn filter_out(patch: &Patch, table: &ParamTable) -> FilterOut {
    let d = |idx: u8| disp(patch, table, Module::Fx, 0, idx);
    FilterOut {
        on: d(0) >= 0.5,
        ftype: crate::filter::FilterType::from_index(d(1) as u32),
        cutoff: d(2),
        reso: (d(3) / 100.0).clamp(0.0, 1.0),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SlotSettings {
    pub root_note: u8,
    /// 0..1 fractions of the sample length.
    pub loop_start: f32,
    pub loop_end: f32,
    /// Linear gain.
    pub gain: f32,
}

pub fn slot_settings(patch: &Patch, table: &ParamTable, slot: u8) -> SlotSettings {
    let d = |idx: u8| disp(patch, table, Module::Slot, slot, idx);
    let mut start = d(1) / 100.0;
    let mut end = d(2) / 100.0;
    if start > end {
        core::mem::swap(&mut start, &mut end);
    }
    SlotSettings {
        root_note: d(0) as u8,
        loop_start: start,
        loop_end: end.max(start + 1e-4),
        gain: 10.0f32.powf(d(3) / 20.0),
    }
}

/// Compose a per-voice patch: base values plus this voice's CLAP per-note
/// modulation offsets, clamped. `out` is reused (no allocation after the
/// first call).
pub fn compose_voice_patch(
    base: &Patch,
    offsets: &[(u32, f32)],
    table: &ParamTable,
    out: &mut Patch,
) {
    out.values.clear();
    out.values.extend_from_slice(&base.values);
    for (id, off) in offsets {
        if let Some(pos) = table.try_pos(*id) {
            out.values[pos] = (out.values[pos] + off).clamp(0.0, 1.0);
        }
    }
}

/// Convenience used by tests and the shell: how a normalized value maps for
/// a stepped parameter of `count` steps.
pub fn step_norm(map: Map, step: usize) -> f32 {
    match map.step_count() {
        Some(c) if c > 1 => step.min(c as usize - 1) as f32 / (c - 1) as f32,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{pid, GRAIN_ALGO_IDX, GRAIN_MODE_IDX};

    fn setup() -> (ParamTable, Patch) {
        let t = ParamTable::new();
        let p = Patch::init(&t);
        (t, p)
    }

    #[test]
    fn defaults_resolve_to_init_lane() {
        let (t, p) = setup();
        let (lp, gate_sync) = lane_params(&p, &t, 0, 120.0, 60.0, true, true);
        assert!(lp.gate);
        assert!((lp.rate_hz - 10.0).abs() < 0.05);
        assert!(lp.linked);
        assert!((lp.length_val - 100.0).abs() < 0.5);
        assert!((lp.level - 1.0).abs() < 1e-3);
        assert!((lp.key_track - 1.0).abs() < 1e-3);
        assert!((lp.pitch_st).abs() < 1e-3);
        assert!((lp.direction - 1.0).abs() < 1e-3);
        assert!((lp.loop_size - 1.0).abs() < 0.02);
        assert_eq!(lp.algo, Algo::Linear);
        assert!(lp.loop_on);
        assert!(!gate_sync);
        // Engine 2 is gated off in the init patch.
        let (lp2, _) = lane_params(&p, &t, 1, 120.0, 60.0, true, true);
        assert!(!lp2.gate);
        // Voice gate and global On both kill the lane gate.
        assert!(!lane_params(&p, &t, 0, 120.0, 60.0, false, true).0.gate);
        assert!(!lane_params(&p, &t, 0, 120.0, 60.0, true, false).0.gate);
    }

    #[test]
    fn tempo_sync_resolves_rate() {
        let (t, mut p) = setup();
        assert!((div_to_hz(23, 120.0) - 4.0).abs() < 1e-4); // 1/8 @ 120
        assert!((div_to_hz(20, 90.0) - 1.5).abs() < 1e-4); // 1/4 @ 90
        // Switch Grain 1 to tempo mode, division stays the 1/8 default.
        let pos = t.pos(pid(Module::Grain, 0, 2));
        p.values[pos] = 1.0;
        let (lp, _) = lane_params(&p, &t, 0, 120.0, 60.0, true, true);
        assert!((lp.rate_hz - 4.0).abs() < 1e-3, "rate {}", lp.rate_hz);
    }

    #[test]
    fn loop_lfo_defaults() {
        let (t, p) = setup();
        let (lfo, transport) = lfo_params(&p, &t, Module::LoopLfo, 0, 120.0);
        assert!((lfo.rate_hz - 0.10).abs() < 0.005, "rate {}", lfo.rate_hz);
        assert!(lfo.gate);
        assert!(!transport);
        assert_eq!(lfo.phase, 0.0);
        let (l6, _) = lfo_params(&p, &t, Module::Lfo, 5, 120.0);
        assert!((l6.rate_hz - 1.0).abs() < 0.02);
    }

    #[test]
    fn global_and_slot_defaults() {
        let (t, p) = setup();
        let g = global_out(&p, &t);
        assert!(g.on && g.squeeze);
        assert!((g.level - 1.0).abs() < 1e-3);
        assert!((g.stereo - 1.0).abs() < 1e-3);
        assert_eq!(g.voices, 8);
        let s = slot_settings(&p, &t, 0);
        assert_eq!(s.root_note, 60);
        assert_eq!(s.loop_start, 0.0);
        assert!((s.loop_end - 1.0).abs() < 1e-6);
        assert!((s.gain - 1.0).abs() < 1e-3);
    }

    #[test]
    fn compose_applies_and_clamps_offsets() {
        let (t, base) = setup();
        let mut out = Patch::init(&t);
        let id = pid(Module::Grain, 0, 14); // Pitch, default norm 0.5
        compose_voice_patch(&base, &[(id, 0.25), (0xDEAD_BEEF, 9.0)], &t, &mut out);
        let pos = t.pos(id);
        assert!((out.values[pos] - 0.75).abs() < 1e-6);
        // Everything else untouched.
        assert_eq!(out.values.len(), base.values.len());
        compose_voice_patch(&base, &[(id, 9.0)], &t, &mut out);
        assert_eq!(out.values[pos], 1.0);
    }

    #[test]
    fn algo_resolves_from_steps() {
        let (t, mut p) = setup();
        let pos = t.pos(pid(Module::Grain, 0, GRAIN_ALGO_IDX));
        let map = t.infos[pos].map;
        p.values[pos] = step_norm(map, 3);
        let (lp, _) = lane_params(&p, &t, 0, 120.0, 60.0, true, true);
        assert_eq!(lp.algo, Algo::Sinc);
    }

    #[test]
    fn mode_resolves_from_steps() {
        let (t, mut p) = setup();
        // Default patch → Granular.
        let (lp0, _) = lane_params(&p, &t, 0, 120.0, 60.0, true, true);
        assert_eq!(lp0.mode, GrainMode::Granular);
        // Step index 1 → Stretch, 2 → Cloud.
        let pos = t.pos(pid(Module::Grain, 0, GRAIN_MODE_IDX));
        let map = t.infos[pos].map;
        p.values[pos] = step_norm(map, 1);
        assert_eq!(lane_params(&p, &t, 0, 120.0, 60.0, true, true).0.mode, GrainMode::Stretch);
        p.values[pos] = step_norm(map, 2);
        assert_eq!(lane_params(&p, &t, 0, 120.0, 60.0, true, true).0.mode, GrainMode::Cloud);
    }
}
