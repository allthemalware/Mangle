//! Mod-matrix runtime. Parameter map §8.
//!
//! Everything modulation: the per-channel source value table, decoding the
//! 32 matrix rows from a patch, and applying contributions in two scopes —
//! a **global pass** (destinations on the six global LFOs, evaluated once
//! per block) and a **voice pass** (Loop LFO + Grain destinations,
//! evaluated per voice *per channel* — Torsion's stereo modulation).
//!
//! Modulation operates on normalized values exactly as documented:
//! `final = clamp(base + Σ amt/100 · src · scale)`. The one destination
//! that is not a parameter, Grain *Loop Position*, accumulates into a side
//! channel and shifts the engine's read window at spawn.
//!
//! Feedback (an LFO modulating an LFO, a Loop LFO as a source for its own
//! rate) uses last-block values — a one-block delay, standard practice.
//!
//! Current control rate = one tick per host block. Sub-block ticking is a
//! later refinement; grain snapshots already read instantaneous values.

use crate::curve::{Curve, DriftPair, LfoState, NoisePair};
use crate::matrix::{self, dest_from_index, GrainDest, LfoDest, ModDest, ModSource};
use crate::params::{pid, Map, Module, ParamTable, Patch, N_MATRIX_ROWS};
use crate::resolve;
use crate::rng::Rng;

pub const N_SOURCES: usize = matrix::SOURCE_COUNT as usize;

/// Source values for one tick: `v[channel][source index]`, -1..1 domain
/// (unipolar sources occupy 0..1, constants reach ±2).
#[derive(Clone, Copy)]
pub struct SourceValues {
    pub v: [[f32; N_SOURCES]; 2],
}

impl SourceValues {
    pub fn zero() -> Self {
        Self { v: [[0.0; N_SOURCES]; 2] }
    }

    pub fn set(&mut self, s: ModSource, l: f32, r: f32) {
        let i = matrix::source_to_index(s) as usize;
        self.v[0][i] = l;
        self.v[1][i] = r;
    }
}

// ----------------------------------------------------------- targets ---

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    None,
    /// Table position of a global-LFO parameter (global pass).
    GlobalParam(u16),
    /// Table position of a Loop LFO / Grain parameter (voice pass).
    VoiceParam(u16),
    /// Grain Loop Position for engine n (voice pass, side channel).
    LoopPos(u8),
}

fn lfo_dest_param(sub: LfoDest) -> u8 {
    match sub {
        LfoDest::Gate => 0,
        LfoDest::Phase => 5,
        LfoDest::Rate => 2,
        LfoDest::Lag => 7,
    }
}

fn grain_dest_param(gd: GrainDest) -> Option<u8> {
    Some(match gd {
        GrainDest::Rate => 1,
        GrainDest::Length => 6,
        GrainDest::Jitter => 4,
        GrainDest::Gate => 0,
        GrainDest::LoopSize => 12,
        GrainDest::LoopPosition => return None,
        GrainDest::Position => 10,
        GrainDest::Pitch => 14,
        GrainDest::Direction => 16,
        GrainDest::Shape => 17,
        GrainDest::Skew => 18,
        GrainDest::Level => 8,
        GrainDest::Pan => 7,
        GrainDest::Sample => 9,
    })
}

pub fn dest_target(idx: u16, table: &ParamTable) -> Target {
    match dest_from_index(idx) {
        ModDest::None => Target::None,
        ModDest::Lfo(n, sub) => {
            Target::GlobalParam(table.pos(pid(Module::Lfo, n, lfo_dest_param(sub))) as u16)
        }
        ModDest::LoopLfo(n, sub) => {
            Target::VoiceParam(table.pos(pid(Module::LoopLfo, n, lfo_dest_param(sub))) as u16)
        }
        ModDest::Grain(n, gd) => match grain_dest_param(gd) {
            Some(i) => Target::VoiceParam(table.pos(pid(Module::Grain, n, i)) as u16),
            None => Target::LoopPos(n),
        },
    }
}

// ------------------------------------------------------------ decode ---

#[derive(Clone, Copy, Debug)]
pub struct DecodedRow {
    pub source: usize,
    pub scale: usize,
    pub amt1: f32,
    pub t1: Target,
    pub amt2: f32,
    pub t2: Target,
}

impl Default for DecodedRow {
    fn default() -> Self {
        Self { source: 0, scale: 0, amt1: 0.0, t1: Target::None, amt2: 0.0, t2: Target::None }
    }
}

/// Decode active rows from a patch into `out`, returning the count.
/// Decode once per block from the base patch (routing and amounts are not
/// per-note-modulatable).
pub fn decode_matrix(patch: &Patch, table: &ParamTable, out: &mut [DecodedRow; 32]) -> usize {
    let amt_map = Map::Lin { min: -100.0, max: 100.0 };
    let src_steps = (matrix::SOURCE_COUNT - 1) as f32;
    let dst_steps = (matrix::DEST_COUNT - 1) as f32;
    let mut n = 0;
    for r in 0..N_MATRIX_ROWS {
        let base = table.pos(pid(Module::Matrix, r, 0));
        let source = (patch.values[base] * src_steps).round() as usize;
        if source == 0 {
            continue;
        }
        let t1 = dest_target((patch.values[base + 2] * dst_steps).round() as u16, table);
        let t2 = dest_target((patch.values[base + 4] * dst_steps).round() as u16, table);
        if t1 == Target::None && t2 == Target::None {
            continue;
        }
        out[n] = DecodedRow {
            source,
            scale: (patch.values[base + 5] * src_steps).round() as usize,
            amt1: amt_map.to_display(patch.values[base + 1]),
            t1,
            amt2: amt_map.to_display(patch.values[base + 3]),
            t2,
        };
        n += 1;
    }
    n
}

// ------------------------------------------------------------- apply ---

#[inline]
fn contribution(row: &DecodedRow, sv: &SourceValues, ch: usize) -> f32 {
    let src = sv.v[ch][row.source];
    let scale = if row.scale != 0 { sv.v[ch][row.scale] } else { 1.0 };
    src * scale / 100.0
}

#[inline]
fn add_param(patch: &mut Patch, pos: u16, c: f32) {
    let v = &mut patch.values[pos as usize];
    *v = (*v + c).clamp(0.0, 1.0);
}

/// Voice pass: Loop LFO + Grain parameter destinations and Loop Position,
/// for one channel.
pub fn apply_voice(
    patch: &mut Patch,
    rows: &[DecodedRow],
    sv: &SourceValues,
    ch: usize,
    loop_pos: &mut [f32; 3],
) {
    for row in rows {
        let c = contribution(row, sv, ch);
        for (amt, t) in [(row.amt1, row.t1), (row.amt2, row.t2)] {
            match t {
                Target::VoiceParam(pos) => add_param(patch, pos, amt * c),
                Target::LoopPos(e) => loop_pos[e as usize] += amt * c,
                _ => {}
            }
        }
    }
}

/// Global pass: parameters of the six global LFOs, left channel, once per
/// block. Voice-local source slots should be zero in `sv` here.
pub fn apply_global(patch: &mut Patch, rows: &[DecodedRow], sv: &SourceValues) {
    for row in rows {
        let c = contribution(row, sv, 0);
        for (amt, t) in [(row.amt1, row.t1), (row.amt2, row.t2)] {
            if let Target::GlobalParam(pos) = t {
                add_param(patch, pos, amt * c);
            }
        }
    }
}

// ----------------------------------------------------------- sources ---

/// Output envelope follower (the "Env" source). Feed it the final stereo
/// output each block; sources read last block's value.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvFollower {
    value: f32,
}

impl EnvFollower {
    pub fn feed(&mut self, l: &[f32], r: &[f32], sample_rate: f32) {
        let atk = 1.0 - (-1.0 / (0.005 * sample_rate)).exp();
        let rel = 1.0 - (-1.0 / (0.200 * sample_rate)).exp();
        for (a, b) in l.iter().zip(r.iter()) {
            let x = a.abs().max(b.abs());
            let coef = if x > self.value { atk } else { rel };
            self.value += (x - self.value) * coef;
        }
    }

    pub fn value(&self) -> f32 {
        self.value.min(1.0)
    }
}

/// Global modulation sources: six LFOs, three Drifts, three Noises, the
/// env follower and the constants. Owns its state; curves are passed in
/// (they live in plugin state).
pub struct GlobalMod {
    lfos: [LfoState; 6],
    pub last_lfo: [(f32, f32); 6],
    drifts: [DriftPair; 3],
    pub last_drift: [(f32, f32); 3],
    pub last_noise: [(f32, f32); 3],
    pub env: EnvFollower,
    rng: Rng,
}

impl GlobalMod {
    pub fn new(seed: u64) -> Self {
        Self {
            lfos: [LfoState::default(); 6],
            last_lfo: [(0.0, 0.0); 6],
            drifts: [DriftPair::default(); 3],
            last_drift: [(0.0, 0.0); 3],
            last_noise: [(0.0, 0.0); 3],
            env: EnvFollower::default(),
            rng: Rng::new(seed),
        }
    }

    /// Advance every global source by one control tick. `patch` should be
    /// the globally-modulated patch (LFO params may be matrix targets).
    /// `transport_seconds` is the host timeline position at this tick;
    /// LFOs with their Transport flag set lock their phase to it.
    pub fn tick(
        &mut self,
        patch: &Patch,
        table: &ParamTable,
        curves: &[Curve; 6],
        bpm: f32,
        dt: f32,
        transport_seconds: Option<f64>,
    ) {
        for i in 0..6 {
            let (p, transport) = resolve::lfo_params(patch, table, Module::Lfo, i as u8, bpm);
            self.last_lfo[i] = match (transport, transport_seconds) {
                (true, Some(sec)) => {
                    let phase = (sec * p.rate_hz as f64).rem_euclid(1.0) as f32;
                    self.lfos[i].tick_transport(&curves[i], &p, phase, dt)
                }
                _ => self.lfos[i].tick(&curves[i], &p, dt),
            };
        }
        for d in 0..3 {
            let st = resolve::st_phase(patch, table, Module::Drift, d as u8);
            self.last_drift[d] = self.drifts[d].tick(dt, st, &mut self.rng);
            let stn = resolve::st_phase(patch, table, Module::Noise, d as u8);
            self.last_noise[d] = NoisePair.roll(stn, &mut self.rng);
        }
    }

    /// Write the global source slots (LFOs, Drift, Noise, constants, Env)
    /// into `sv`. Voice-local slots are left untouched.
    pub fn fill_sources(&self, sv: &mut SourceValues) {
        for i in 0..6u8 {
            let (l, r) = self.last_lfo[i as usize];
            sv.set(ModSource::Lfo(i), l, r);
        }
        for d in 0..3u8 {
            let (l, r) = self.last_drift[d as usize];
            sv.set(ModSource::Drift(d), l, r);
            let (l, r) = self.last_noise[d as usize];
            sv.set(ModSource::Noise(d), l, r);
        }
        for c in matrix::CONSTS {
            let v = c as f32 / 100.0;
            sv.set(ModSource::Const(c), v, v);
        }
        let e = self.env.value();
        sv.set(ModSource::EnvFollow, e, e);
    }
}

/// Voice-local source inputs (everything a voice contributes).
#[derive(Clone, Copy, Debug)]
pub struct VoiceSourceInputs {
    pub velocity: f32,
    pub note: u8,
    pub gate: bool,
    pub pressure: f32,
    pub voice_random: f32,
    /// Last block's Loop LFO outputs (one-block feedback delay).
    pub loop_lfo: [(f32, f32); 3],
}

/// Fill the voice-local source slots: Macros (read from the voice patch so
/// per-note macro modulation lands), key sources, and Loop LFOs.
pub fn fill_voice_sources(
    sv: &mut SourceValues,
    vi: &VoiceSourceInputs,
    voice_patch: &Patch,
    table: &ParamTable,
) {
    for m in 0..8u8 {
        let norm = voice_patch.values[table.pos(pid(Module::Macro, m, 0))];
        let v = norm * 2.0 - 1.0;
        sv.set(ModSource::Macro(m), v, v);
    }
    for (i, (l, r)) in vi.loop_lfo.iter().enumerate() {
        sv.set(ModSource::LoopLfo(i as u8), *l, *r);
    }
    sv.set(ModSource::Velocity, vi.velocity, vi.velocity);
    let note_full = (vi.note as f32 / 127.0) * 2.0 - 1.0;
    sv.set(ModSource::Note, note_full, note_full);
    let oct = ((vi.note % 12) as f32 / 11.0) * 2.0 - 1.0;
    sv.set(ModSource::NoteOct, oct, oct);
    let g = if vi.gate { 1.0 } else { 0.0 };
    sv.set(ModSource::NoteGate, g, g);
    sv.set(ModSource::Pressure, vi.pressure, vi.pressure);
    sv.set(ModSource::VoiceRandom, vi.voice_random, vi.voice_random);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{dest_to_index, source_to_index};
    use crate::params::GRAIN_PITCH_IDX;

    fn setup() -> (ParamTable, Patch) {
        let t = ParamTable::new();
        let p = Patch::init(&t);
        (t, p)
    }

    /// Write one matrix row into the patch in normalized encoding.
    fn set_row(
        patch: &mut Patch,
        table: &ParamTable,
        row: u8,
        src: ModSource,
        amt1: f32,
        d1: ModDest,
        amt2: f32,
        d2: ModDest,
        scale: ModSource,
    ) {
        let base = table.pos(pid(Module::Matrix, row, 0));
        let s = (matrix::SOURCE_COUNT - 1) as f32;
        let d = (matrix::DEST_COUNT - 1) as f32;
        patch.values[base] = source_to_index(src) as f32 / s;
        patch.values[base + 1] = (amt1 + 100.0) / 200.0;
        patch.values[base + 2] = dest_to_index(d1) as f32 / d;
        patch.values[base + 3] = (amt2 + 100.0) / 200.0;
        patch.values[base + 4] = dest_to_index(d2) as f32 / d;
        patch.values[base + 5] = source_to_index(scale) as f32 / s;
    }

    #[test]
    fn dest_targets_map_correctly() {
        let (t, _) = setup();
        let pitch = dest_target(dest_to_index(ModDest::Grain(1, GrainDest::Pitch)), &t);
        assert_eq!(
            pitch,
            Target::VoiceParam(t.pos(pid(Module::Grain, 1, GRAIN_PITCH_IDX)) as u16)
        );
        let lp = dest_target(dest_to_index(ModDest::Grain(0, GrainDest::LoopPosition)), &t);
        assert_eq!(lp, Target::LoopPos(0));
        let lr = dest_target(dest_to_index(ModDest::Lfo(2, LfoDest::Rate)), &t);
        assert_eq!(lr, Target::GlobalParam(t.pos(pid(Module::Lfo, 2, 2)) as u16));
        for i in 0..matrix::DEST_COUNT {
            let _ = dest_target(i, &t); // must not panic anywhere
        }
    }

    #[test]
    fn decode_skips_inactive_rows() {
        let (t, mut p) = setup();
        let mut rows = [DecodedRow::default(); 32];
        assert_eq!(decode_matrix(&p, &t, &mut rows), 0);
        set_row(
            &mut p, &t, 3,
            ModSource::Lfo(0), 50.0, ModDest::Grain(0, GrainDest::Pitch),
            0.0, ModDest::None, ModSource::None,
        );
        // A row with a source but no destinations is also skipped.
        set_row(
            &mut p, &t, 7,
            ModSource::Drift(0), 80.0, ModDest::None,
            0.0, ModDest::None, ModSource::None,
        );
        let n = decode_matrix(&p, &t, &mut rows);
        assert_eq!(n, 1);
        assert!((rows[0].amt1 - 50.0).abs() < 0.5);
        assert_eq!(rows[0].source, source_to_index(ModSource::Lfo(0)) as usize);
    }

    #[test]
    fn voice_pass_modulates_per_channel() {
        let (t, mut p) = setup();
        set_row(
            &mut p, &t, 0,
            ModSource::Drift(0), 100.0, ModDest::Grain(0, GrainDest::Pitch),
            0.0, ModDest::None, ModSource::None,
        );
        let mut rows = [DecodedRow::default(); 32];
        let n = decode_matrix(&p, &t, &mut rows);
        let mut sv = SourceValues::zero();
        sv.set(ModSource::Drift(0), 1.0, -0.5);

        let pitch_pos = t.pos(pid(Module::Grain, 0, GRAIN_PITCH_IDX));
        let mut lp = [0.0f32; 3];
        let mut left = p.clone();
        apply_voice(&mut left, &rows[..n], &sv, 0, &mut lp);
        assert!((left.values[pitch_pos] - 1.0).abs() < 1e-6); // 0.5 + 1.0 clamped
        let mut right = p.clone();
        apply_voice(&mut right, &rows[..n], &sv, 1, &mut lp);
        assert!((right.values[pitch_pos] - 0.0).abs() < 1e-6); // 0.5 - 0.5
    }

    #[test]
    fn scale_column_multiplies() {
        let (t, mut p) = setup();
        set_row(
            &mut p, &t, 0,
            ModSource::Lfo(0), 100.0, ModDest::Grain(0, GrainDest::Position),
            0.0, ModDest::None, ModSource::Macro(0),
        );
        let mut rows = [DecodedRow::default(); 32];
        let n = decode_matrix(&p, &t, &mut rows);
        let mut sv = SourceValues::zero();
        sv.set(ModSource::Lfo(0), 1.0, 1.0);
        sv.set(ModSource::Macro(0), 0.5, 0.5);
        let pos = t.pos(pid(Module::Grain, 0, 10));
        let mut lp = [0.0f32; 3];
        let mut out = p.clone();
        apply_voice(&mut out, &rows[..n], &sv, 0, &mut lp);
        assert!((out.values[pos] - 0.5).abs() < 1e-6, "got {}", out.values[pos]);
    }

    #[test]
    fn loop_position_accumulates_into_side_channel() {
        let (t, mut p) = setup();
        set_row(
            &mut p, &t, 0,
            ModSource::Const(100), 30.0, ModDest::Grain(2, GrainDest::LoopPosition),
            0.0, ModDest::None, ModSource::None,
        );
        let mut rows = [DecodedRow::default(); 32];
        let n = decode_matrix(&p, &t, &mut rows);
        let mut sv = SourceValues::zero();
        sv.set(ModSource::Const(100), 1.0, 1.0);
        let before = p.clone();
        let mut lp = [0.0f32; 3];
        apply_voice(&mut p, &rows[..n], &sv, 0, &mut lp);
        assert_eq!(p, before, "loop position must not touch params");
        assert!((lp[2] - 0.3).abs() < 1e-6);
        assert_eq!(lp[0], 0.0);
    }

    #[test]
    fn scopes_are_disjoint() {
        let (t, mut p) = setup();
        set_row(
            &mut p, &t, 0,
            ModSource::Const(100), 50.0, ModDest::Lfo(1, LfoDest::Rate),
            40.0, ModDest::Grain(0, GrainDest::Shape), ModSource::None,
        );
        let mut rows = [DecodedRow::default(); 32];
        let n = decode_matrix(&p, &t, &mut rows);
        let mut sv = SourceValues::zero();
        sv.set(ModSource::Const(100), 1.0, 1.0);
        let lfo_rate = t.pos(pid(Module::Lfo, 1, 2));
        let shape = t.pos(pid(Module::Grain, 0, 17));

        let mut vp = p.clone();
        let mut lp = [0.0f32; 3];
        apply_voice(&mut vp, &rows[..n], &sv, 0, &mut lp);
        assert_eq!(vp.values[lfo_rate], p.values[lfo_rate], "voice pass touched a global dest");
        assert!((vp.values[shape] - (p.values[shape] + 0.4)).abs() < 1e-6);

        let mut gp = p.clone();
        apply_global(&mut gp, &rows[..n], &sv);
        assert!((gp.values[lfo_rate] - (p.values[lfo_rate] + 0.5)).abs() < 1e-6);
        assert_eq!(gp.values[shape], p.values[shape], "global pass touched a voice dest");
    }

    #[test]
    fn env_follower_attacks_and_releases() {
        let mut env = EnvFollower::default();
        let ones = vec![1.0f32; 4800];
        let zeros = vec![0.0f32; 4800];
        env.feed(&ones, &zeros, 48_000.0);
        assert!(env.value() > 0.9, "attack too slow: {}", env.value());
        env.feed(&zeros, &zeros, 48_000.0);
        let mid = env.value();
        assert!(mid < 0.7 && mid > 0.05, "release off: {mid}");
        for _ in 0..20 {
            env.feed(&zeros, &zeros, 48_000.0);
        }
        assert!(env.value() < 0.01);
    }

    #[test]
    fn global_mod_ticks_deterministically() {
        let (t, p) = setup();
        let curves: [Curve; 6] = [
            Curve::sine(), Curve::sine(), Curve::sine(),
            Curve::sine(), Curve::sine(), Curve::sine(),
        ];
        let mut a = GlobalMod::new(9);
        let mut b = GlobalMod::new(9);
        for _ in 0..25 {
            a.tick(&p, &t, &curves, 120.0, 0.01, None);
            b.tick(&p, &t, &curves, 120.0, 0.01, None);
        }
        assert_eq!(a.last_lfo, b.last_lfo);
        assert_eq!(a.last_drift, b.last_drift);
        // LFO 1 at 1 Hz after a quarter cycle of sine: near the +1 peak.
        assert!(a.last_lfo[0].0.abs() > 0.5, "lfo barely moved: {:?}", a.last_lfo[0]);
        let mut sv = SourceValues::zero();
        a.fill_sources(&mut sv);
        let i = source_to_index(ModSource::Const(200)) as usize;
        assert_eq!(sv.v[0][i], 2.0);
    }

    #[test]
    fn voice_sources_fill_expected_slots() {
        let (t, p) = setup();
        let mut sv = SourceValues::zero();
        let vi = VoiceSourceInputs {
            velocity: 0.8,
            note: 127,
            gate: true,
            pressure: 0.25,
            voice_random: -0.6,
            loop_lfo: [(0.1, -0.1), (0.0, 0.0), (1.0, 1.0)],
        };
        fill_voice_sources(&mut sv, &vi, &p, &t);
        let idx = |s| matrix::source_to_index(s) as usize;
        assert_eq!(sv.v[0][idx(ModSource::Velocity)], 0.8);
        assert!((sv.v[0][idx(ModSource::Note)] - 1.0).abs() < 1e-6);
        assert_eq!(sv.v[0][idx(ModSource::NoteGate)], 1.0);
        assert_eq!(sv.v[1][idx(ModSource::LoopLfo(0))], -0.1);
        assert_eq!(sv.v[0][idx(ModSource::VoiceRandom)], -0.6);
        // Macros default to 0 % → bipolar 0.
        assert!(sv.v[0][idx(ModSource::Macro(3))].abs() < 1e-6);
    }
}
