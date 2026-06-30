//! RANDOM patch generation with per-unit locks.
//!
//! Every randomizable unit can be locked individually: each grain engine,
//! each Loop LFO, each of the six LFOs, Drift, Noise, the Macros, the mod
//! matrix, and the sample loop points. A locked unit's parameters are never
//! touched, and the matrix generator will not even *route into* a locked
//! unit. Locks live in plugin state (they are not host params) and surface
//! in the UI as a padlock glyph on every section header.
//!
//! Semantics follow the Torsion manual:
//! * **Complexity** — probability that any given candidate parameter is
//!   displaced, and how many matrix routings get generated.
//! * **Amount** — how much of the displacement is applied. Continuous
//!   params lerp toward their random target by `amount`; stepped params
//!   switch with probability `amount`. Low amounts morph patches slowly.
//! * Parameter pools mirror Torsion's defaults: grain Level and Pitch are
//!   excluded out of the box, as are destructive destinations (Gate, Level,
//!   Pitch) and the Noise source.

use crate::matrix::{self, ModDest, ModSource, Owner};
use crate::params::{
    pid, Module, ParamTable, Patch, GRAIN_LEVEL_IDX, GRAIN_PITCH_IDX, LFO_TRIG_IDX,
    N_GRAIN_PARAMS, N_LFO_PARAMS, N_MATRIX_ROWS, SLOT_LOOP_END_IDX, SLOT_LOOP_START_IDX,
};
use crate::rng::Rng;

/// Per-unit exclusion switches. `true` = locked = RANDOM keeps its hands off.
#[derive(Clone, Debug, PartialEq)]
pub struct RandomLocks {
    pub grain: [bool; 3],
    pub loop_lfo: [bool; 3],
    pub lfo: [bool; 6],
    pub drift: bool,
    pub noise: bool,
    pub macros: bool,
    pub matrix: bool,
    pub loop_points: bool,
}

impl Default for RandomLocks {
    fn default() -> Self {
        Self {
            grain: [false; 3],
            loop_lfo: [false; 3],
            lfo: [false; 6],
            drift: false,
            noise: false,
            // Macros are performance controls; locked unless explicitly opened up.
            macros: true,
            matrix: false,
            loop_points: false,
        }
    }
}

/// Which grain parameters are in the randomization pool (by index, §3 order).
#[derive(Clone, Debug)]
pub struct GrainParamPool(pub [bool; N_GRAIN_PARAMS]);

impl Default for GrainParamPool {
    fn default() -> Self {
        let mut p = [true; N_GRAIN_PARAMS];
        p[GRAIN_LEVEL_IDX as usize] = false; // Torsion default: Level unchecked
        p[GRAIN_PITCH_IDX as usize] = false; // Torsion default: Pitch unchecked
        Self(p)
    }
}

/// Which LFO / Loop LFO parameters are in the pool (by index, §4 order).
#[derive(Clone, Debug)]
pub struct LfoParamPool(pub [bool; N_LFO_PARAMS]);

impl Default for LfoParamPool {
    fn default() -> Self {
        let mut p = [true; N_LFO_PARAMS];
        p[LFO_TRIG_IDX as usize] = false; // momentary; pointless to randomize
        Self(p)
    }
}

/// Which source families may appear in generated matrix rows.
#[derive(Clone, Debug)]
pub struct SourcePool {
    pub lfo: bool,
    pub loop_lfo: bool,
    pub drift: bool,
    pub noise: bool,
    pub macros: bool,
    pub consts: bool,
    pub env_follow: bool,
    /// Velocity / Note / Note Oct / Note Gate.
    pub key: bool,
    pub pressure: bool,
    pub voice_random: bool,
}

impl Default for SourcePool {
    fn default() -> Self {
        // Mirrors Torsion's Modulation Source Pool defaults: LFO + Drift +
        // Env Follower on, Noise off; Mangle adds Loop LFO + Voice Random on.
        Self {
            lfo: true,
            loop_lfo: true,
            drift: true,
            noise: false,
            macros: false,
            consts: false,
            env_follow: true,
            key: false,
            pressure: false,
            voice_random: true,
        }
    }
}

impl SourcePool {
    fn admits(&self, s: ModSource) -> bool {
        match s {
            ModSource::None => false,
            ModSource::Lfo(_) => self.lfo,
            ModSource::LoopLfo(_) => self.loop_lfo,
            ModSource::Drift(_) => self.drift,
            ModSource::Noise(_) => self.noise,
            ModSource::Macro(_) => self.macros,
            ModSource::Const(_) => self.consts,
            ModSource::EnvFollow => self.env_follow,
            ModSource::Velocity | ModSource::Note | ModSource::NoteOct | ModSource::NoteGate => {
                self.key
            }
            ModSource::Pressure => self.pressure,
            ModSource::VoiceRandom => self.voice_random,
        }
    }

    /// Stepped-param indices of every admitted source. Locked source units
    /// are also excluded so a locked LFO never gets pulled into new routings.
    pub fn allowed(&self, locks: &RandomLocks) -> Vec<u16> {
        (1..matrix::SOURCE_COUNT)
            .filter(|&i| {
                let s = matrix::source_from_index(i);
                let unlocked = match s {
                    ModSource::Lfo(n) => !locks.lfo[n as usize],
                    ModSource::LoopLfo(n) => !locks.loop_lfo[n as usize],
                    ModSource::Drift(_) => !locks.drift,
                    ModSource::Noise(_) => !locks.noise,
                    ModSource::Macro(_) => !locks.macros,
                    _ => true,
                };
                unlocked && self.admits(s)
            })
            .collect()
    }
}

/// Which destination kinds may appear in generated matrix rows.
#[derive(Clone, Debug)]
pub struct DestPool {
    /// Indexed by `matrix::GRAIN_DESTS` order.
    pub grain: [bool; 14],
    /// Indexed by `matrix::LFO_DESTS` order (Gate, Phase, Rate, Lag).
    pub lfo: [bool; 4],
    pub loop_lfo: [bool; 4],
}

impl Default for DestPool {
    fn default() -> Self {
        // Mirrors Torsion's Modulation Dest Pool defaults: Gate, Pitch and
        // Level destinations off; LFO Rate the only LFO destination on.
        let mut grain = [true; 14];
        grain[matrix::GRAIN_DESTS.iter().position(|&d| d == matrix::GrainDest::Gate).unwrap()] =
            false;
        grain[matrix::GRAIN_DESTS.iter().position(|&d| d == matrix::GrainDest::Pitch).unwrap()] =
            false;
        grain[matrix::GRAIN_DESTS.iter().position(|&d| d == matrix::GrainDest::Level).unwrap()] =
            false;
        let lfo = [false, false, true, false];
        Self { grain, lfo, loop_lfo: lfo }
    }
}

impl DestPool {
    /// Stepped-param indices of every admitted destination, excluding any
    /// destination owned by a locked unit.
    pub fn allowed(&self, locks: &RandomLocks) -> Vec<u16> {
        (1..matrix::DEST_COUNT)
            .filter(|&i| {
                let d = matrix::dest_from_index(i);
                let in_pool = match d {
                    ModDest::None => false,
                    ModDest::Lfo(_, sub) => {
                        self.lfo[matrix::LFO_DESTS.iter().position(|&x| x == sub).unwrap()]
                    }
                    ModDest::LoopLfo(_, sub) => {
                        self.loop_lfo[matrix::LFO_DESTS.iter().position(|&x| x == sub).unwrap()]
                    }
                    ModDest::Grain(_, sub) => {
                        self.grain[matrix::GRAIN_DESTS.iter().position(|&x| x == sub).unwrap()]
                    }
                };
                let unlocked = match matrix::dest_owner(d) {
                    Owner::Global => true,
                    Owner::Lfo(n) => !locks.lfo[n as usize],
                    Owner::LoopLfo(n) => !locks.loop_lfo[n as usize],
                    Owner::Grain(n) => !locks.grain[n as usize],
                };
                in_pool && unlocked
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct RandomOptions {
    /// 0..1 — how much of each displacement is applied (morph speed).
    pub amount: f32,
    /// 0..1 — how many parameters get displaced / how many routings appear.
    pub complexity: f32,
    pub locks: RandomLocks,
    pub grain_pool: GrainParamPool,
    pub lfo_pool: LfoParamPool,
    pub sources: SourcePool,
    pub dests: DestPool,
    pub scales: SourcePool,
    /// Hard cap on generated matrix rows (≤ 32).
    pub max_matrix_rows: usize,
}

impl Default for RandomOptions {
    fn default() -> Self {
        Self {
            amount: 1.0,
            complexity: 0.6,
            locks: RandomLocks::default(),
            grain_pool: GrainParamPool::default(),
            lfo_pool: LfoParamPool::default(),
            sources: SourcePool::default(),
            dests: DestPool::default(),
            scales: SourcePool::default(),
            max_matrix_rows: 16,
        }
    }
}

impl RandomOptions {
    /// May RANDOM touch this (non-matrix) parameter at all?
    fn candidate(&self, module: Module, instance: u8, index: u8) -> bool {
        let l = &self.locks;
        match module {
            Module::Global => false, // never randomize the main section
            Module::Macro => !l.macros,
            Module::Grain => !l.grain[instance as usize] && self.grain_pool.0[index as usize],
            Module::LoopLfo => !l.loop_lfo[instance as usize] && self.lfo_pool.0[index as usize],
            Module::Lfo => !l.lfo[instance as usize] && self.lfo_pool.0[index as usize],
            Module::Drift => !l.drift,
            Module::Noise => !l.noise,
            Module::Slot => {
                !l.loop_points && (index == SLOT_LOOP_START_IDX || index == SLOT_LOOP_END_IDX)
            }
            Module::Matrix => false, // handled by randomize_matrix
        }
    }
}

/// Randomize `patch` in place. Deterministic for a given (patch, opts, seed).
pub fn randomize(patch: &mut Patch, table: &ParamTable, opts: &RandomOptions, seed: u64) {
    let mut rng = Rng::new(seed);
    let amount = opts.amount.clamp(0.0, 1.0);
    let complexity = opts.complexity.clamp(0.0, 1.0);

    for (i, p) in table.infos.iter().enumerate() {
        if !opts.candidate(p.module, p.instance, p.index) {
            continue;
        }
        if !rng.chance(complexity) {
            continue;
        }
        match p.map.step_count() {
            Some(steps) => {
                if rng.chance(amount) {
                    let step = rng.below(steps as usize);
                    patch.values[i] = if steps <= 1 {
                        0.0
                    } else {
                        step as f32 / (steps - 1) as f32
                    };
                }
            }
            None => {
                let target = rng.f32();
                patch.values[i] += (target - patch.values[i]) * amount;
            }
        }
    }

    // Keep slot loop windows sane: start < end with a minimum width.
    if !opts.locks.loop_points {
        for s in 0..6u8 {
            let a = table.pos(pid(Module::Slot, s, SLOT_LOOP_START_IDX));
            let b = table.pos(pid(Module::Slot, s, SLOT_LOOP_END_IDX));
            if patch.values[a] > patch.values[b] {
                patch.values.swap(a, b);
            }
            if patch.values[b] - patch.values[a] < 0.01 {
                patch.values[b] = (patch.values[a] + 0.01).min(1.0);
                patch.values[a] = patch.values[b] - 0.01;
            }
        }
    }

    if !opts.locks.matrix {
        randomize_matrix(patch, table, opts, amount, complexity, &mut rng);
    }
}

fn randomize_matrix(
    patch: &mut Patch,
    table: &ParamTable,
    opts: &RandomOptions,
    amount: f32,
    complexity: f32,
    rng: &mut Rng,
) {
    let sources = opts.sources.allowed(&opts.locks);
    let dests = opts.dests.allowed(&opts.locks);
    let scales = opts.scales.allowed(&opts.locks);
    if sources.is_empty() || dests.is_empty() {
        return;
    }

    let target_rows = ((2.0 + complexity * 12.0).round() as usize)
        .min(opts.max_matrix_rows)
        .min(N_MATRIX_ROWS as usize);

    let src_norm = |idx: u16| idx as f32 / (matrix::SOURCE_COUNT - 1) as f32;
    let dst_norm = |idx: u16| idx as f32 / (matrix::DEST_COUNT - 1) as f32;

    for r in 0..N_MATRIX_ROWS {
        let base = table.pos(pid(Module::Matrix, r, 0));
        if (r as usize) < target_rows {
            if rng.chance(amount) {
                patch.values[base] = src_norm(*rng.pick(&sources));
                patch.values[base + 2] = dst_norm(*rng.pick(&dests));
                patch.values[base + 4] = if rng.chance(0.6) {
                    dst_norm(*rng.pick(&dests))
                } else {
                    0.0
                };
                patch.values[base + 5] = if !scales.is_empty() && rng.chance(0.3) {
                    src_norm(*rng.pick(&scales))
                } else {
                    0.0
                };
            }
            // Amounts always morph by `amount` (bipolar params: 0.5 = zero).
            for col in [1usize, 3] {
                let target = rng.f32();
                patch.values[base + col] += (target - patch.values[base + col]) * amount;
            }
        } else if rng.chance(amount) {
            // Clear surplus rows back to defaults.
            patch.values[base] = 0.0;
            patch.values[base + 1] = 0.5;
            patch.values[base + 2] = 0.0;
            patch.values[base + 3] = 0.5;
            patch.values[base + 4] = 0.0;
            patch.values[base + 5] = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff_ids(table: &ParamTable, a: &Patch, b: &Patch) -> Vec<u32> {
        table
            .infos
            .iter()
            .enumerate()
            .filter(|(i, _)| (a.values[*i] - b.values[*i]).abs() > 1e-9)
            .map(|(_, p)| p.id)
            .collect()
    }

    #[test]
    fn deterministic_for_seed() {
        let t = ParamTable::new();
        let opts = RandomOptions::default();
        let mut a = Patch::init(&t);
        let mut b = Patch::init(&t);
        randomize(&mut a, &t, &opts, 0xC0FFEE);
        randomize(&mut b, &t, &opts, 0xC0FFEE);
        assert_eq!(a, b);
        let mut c = Patch::init(&t);
        randomize(&mut c, &t, &opts, 0xC0FFEE + 1);
        assert_ne!(a, c);
    }

    #[test]
    fn amount_zero_is_a_noop() {
        let t = ParamTable::new();
        let opts = RandomOptions { amount: 0.0, complexity: 1.0, ..Default::default() };
        let init = Patch::init(&t);
        let mut p = init.clone();
        randomize(&mut p, &t, &opts, 99);
        assert_eq!(p, init);
    }

    #[test]
    fn locked_units_are_untouched() {
        let t = ParamTable::new();
        let mut opts = RandomOptions { amount: 1.0, complexity: 1.0, ..Default::default() };
        opts.locks.grain[1] = true; // lock Grain 2
        opts.locks.lfo[4] = true; // lock LFO 5
        // Open every parameter pool so only the locks are doing the protecting.
        opts.grain_pool = GrainParamPool([true; N_GRAIN_PARAMS]);
        opts.lfo_pool = LfoParamPool([true; N_LFO_PARAMS]);

        let init = Patch::init(&t);
        let mut p = init.clone();
        randomize(&mut p, &t, &opts, 1234);

        let changed = diff_ids(&t, &init, &p);
        assert!(!changed.is_empty(), "randomize should change something");
        for id in &changed {
            let info = t.get(*id);
            assert!(
                !(info.module == Module::Grain && info.instance == 1),
                "locked Grain 2 param changed: {}",
                info.name
            );
            assert!(
                !(info.module == Module::Lfo && info.instance == 4),
                "locked LFO 5 param changed: {}",
                info.name
            );
        }
        // Something in the unlocked engines did move.
        assert!(changed
            .iter()
            .any(|id| { let i = t.get(*id); i.module == Module::Grain && i.instance == 0 }));
    }

    #[test]
    fn matrix_never_routes_into_locked_units() {
        let t = ParamTable::new();
        let mut opts = RandomOptions { amount: 1.0, complexity: 1.0, ..Default::default() };
        opts.locks.grain[2] = true; // lock Grain 3
        opts.locks.lfo[0] = true; // lock LFO 1

        let mut p = Patch::init(&t);
        randomize(&mut p, &t, &opts, 777);

        for r in 0..N_MATRIX_ROWS {
            let base = t.pos(pid(Module::Matrix, r, 0));
            for col in [2usize, 4] {
                let steps = matrix::DEST_COUNT;
                let idx = (p.values[base + col] * (steps - 1) as f32).round() as u16;
                let owner = matrix::dest_owner(matrix::dest_from_index(idx));
                assert_ne!(owner, Owner::Grain(2), "row {r} routed into locked Grain 3");
                assert_ne!(owner, Owner::Lfo(0), "row {r} routed into locked LFO 1");
            }
            // Locked LFO 1 must not be used as a source or scale either.
            for col in [0usize, 5] {
                let idx =
                    (p.values[base + col] * (matrix::SOURCE_COUNT - 1) as f32).round() as u16;
                assert_ne!(
                    matrix::source_from_index(idx),
                    ModSource::Lfo(0),
                    "row {r} used locked LFO 1 as source/scale"
                );
            }
        }
    }

    #[test]
    fn matrix_lock_freezes_all_rows() {
        let t = ParamTable::new();
        let mut opts = RandomOptions { amount: 1.0, complexity: 1.0, ..Default::default() };
        opts.locks.matrix = true;
        let init = Patch::init(&t);
        let mut p = init.clone();
        randomize(&mut p, &t, &opts, 31337);
        for (i, info) in t.infos.iter().enumerate() {
            if info.module == Module::Matrix {
                assert_eq!(p.values[i], init.values[i], "matrix param moved: {}", info.name);
            }
        }
    }

    #[test]
    fn default_pools_exclude_grain_level_and_pitch() {
        let t = ParamTable::new();
        let opts = RandomOptions { amount: 1.0, complexity: 1.0, ..Default::default() };
        let init = Patch::init(&t);
        let mut p = init.clone();
        randomize(&mut p, &t, &opts, 4242);
        for g in 0..3u8 {
            for idx in [GRAIN_LEVEL_IDX, GRAIN_PITCH_IDX] {
                let pos = t.pos(pid(Module::Grain, g, idx));
                assert_eq!(p.values[pos], init.values[pos]);
            }
        }
    }

    #[test]
    fn loop_points_lock_and_window_sanity() {
        let t = ParamTable::new();
        // Locked: untouched.
        let mut opts = RandomOptions { amount: 1.0, complexity: 1.0, ..Default::default() };
        opts.locks.loop_points = true;
        let init = Patch::init(&t);
        let mut p = init.clone();
        randomize(&mut p, &t, &opts, 5);
        for s in 0..6u8 {
            for idx in [SLOT_LOOP_START_IDX, SLOT_LOOP_END_IDX] {
                let pos = t.pos(pid(Module::Slot, s, idx));
                assert_eq!(p.values[pos], init.values[pos]);
            }
        }
        // Unlocked: start < end always holds.
        let opts = RandomOptions { amount: 1.0, complexity: 1.0, ..Default::default() };
        for seed in 0..32u64 {
            let mut p = Patch::init(&t);
            randomize(&mut p, &t, &opts, seed);
            for s in 0..6u8 {
                let a = p.values[t.pos(pid(Module::Slot, s, SLOT_LOOP_START_IDX))];
                let b = p.values[t.pos(pid(Module::Slot, s, SLOT_LOOP_END_IDX))];
                assert!(a < b, "seed {seed} slot {s}: start {a} !< end {b}");
            }
        }
    }

    #[test]
    fn root_note_and_gain_never_randomized() {
        let t = ParamTable::new();
        let opts = RandomOptions { amount: 1.0, complexity: 1.0, ..Default::default() };
        let init = Patch::init(&t);
        let mut p = init.clone();
        randomize(&mut p, &t, &opts, 6);
        for s in 0..6u8 {
            for idx in [0u8, 3] {
                let pos = t.pos(pid(Module::Slot, s, idx));
                assert_eq!(p.values[pos], init.values[pos]);
            }
        }
    }
}
