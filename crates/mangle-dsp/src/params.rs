//! Stable parameter table. Codifies `mangle-parameter-map.md` §0–§11.
//!
//! IDs are `(module << 16) | (instance << 8) | index` and are never reused
//! or renumbered. The table is the single source of truth the plugin shell,
//! GUI, state, and randomizer all read from.

use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Module {
    Global = 0x00,
    Macro = 0x01,
    Grain = 0x02,
    LoopLfo = 0x03,
    Lfo = 0x04,
    Drift = 0x05,
    Noise = 0x06,
    Slot = 0x07,
    Matrix = 0x08,
}

pub const fn pid(m: Module, instance: u8, index: u8) -> u32 {
    ((m as u32) << 16) | ((instance as u32) << 8) | index as u32
}

pub mod flags {
    /// CLAP_PARAM_IS_AUTOMATABLE
    pub const A: u8 = 1;
    /// CLAP_PARAM_IS_MODULATABLE
    pub const M: u8 = 2;
    /// CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID
    pub const PN: u8 = 4;
    /// CLAP_PARAM_IS_STEPPED
    pub const S: u8 = 8;
}

/// Display-value mapping for a normalized 0..1 parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Map {
    Lin { min: f32, max: f32 },
    /// display = max * n^3 — zero-anchored quasi-exponential taper.
    Cube { max: f32 },
    /// Linear in dB.
    Db { min: f32, max: f32 },
    Bool,
    Steps { count: u16 },
    Int { min: i32, max: i32 },
}

impl Map {
    pub fn to_norm(self, display: f32) -> f32 {
        match self {
            Map::Lin { min, max } => ((display - min) / (max - min)).clamp(0.0, 1.0),
            Map::Cube { max } => (display / max).clamp(0.0, 1.0).powf(1.0 / 3.0),
            Map::Db { min, max } => ((display - min) / (max - min)).clamp(0.0, 1.0),
            Map::Bool => {
                if display >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Map::Steps { count } => {
                if count <= 1 {
                    0.0
                } else {
                    (display / (count as f32 - 1.0)).clamp(0.0, 1.0)
                }
            }
            Map::Int { min, max } => {
                ((display - min as f32) / ((max - min) as f32)).clamp(0.0, 1.0)
            }
        }
    }

    pub fn to_display(self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        match self {
            Map::Lin { min, max } => min + n * (max - min),
            Map::Cube { max } => max * n * n * n,
            Map::Db { min, max } => min + n * (max - min),
            Map::Bool => {
                if n >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Map::Steps { count } => (n * (count as f32 - 1.0)).round(),
            Map::Int { min, max } => (min as f32 + n * ((max - min) as f32)).round(),
        }
    }

    pub fn step_count(self) -> Option<u16> {
        match self {
            Map::Bool => Some(2),
            Map::Steps { count } => Some(count),
            Map::Int { min, max } => Some((max - min + 1) as u16),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub id: u32,
    pub module: Module,
    pub instance: u8,
    pub index: u8,
    pub name: String,
    pub unit: &'static str,
    pub map: Map,
    pub default_display: f32,
    pub default_norm: f32,
    pub flags: u8,
    pub bipolar: bool,
}

/// (name, unit, map, default_display, flags, bipolar)
struct Spec(&'static str, &'static str, Map, f32, u8, bool);

use self::flags::{A, M, PN, S};
const A_M: u8 = A | M;
const A_M_PN: u8 = A | M | PN;
const A_S: u8 = A | S;

/// Tempo-sync division enum, shared by every synced Rate (manual order).
pub const DIV_NAMES: [&str; 31] = [
    "32/4", "28/4", "24/4", "20/4", "16/4", "12/4", "8/4", "7/4", "6/4", "5/4", "4/4", "7/8",
    "3/4", "5/8", "2/4", "7/16", "5/8T", "3/8", "4/8T", "5/16", "1/4", "3/16", "2/8T", "1/8",
    "1/8T", "1/16", "1/16T", "1/32", "1/32T", "1/64", "1/128",
];
pub const N_DIVS: u16 = DIV_NAMES.len() as u16;
pub const DIV_1_4: f32 = 20.0;
pub const DIV_1_8: f32 = 23.0;

/// Length of each division in beats (quarter notes); `T` = triplet (×2/3).
/// One modulation/emission cycle spans this many beats.
pub const DIV_BEATS: [f32; 31] = [
    32.0, 28.0, 24.0, 20.0, 16.0, 12.0, 8.0, 7.0, 6.0, 5.0, 4.0, 3.5, 3.0, 2.5, 2.0, 1.75,
    2.5 * 2.0 / 3.0, 1.5, 4.0 / 3.0, 1.25, 1.0, 0.75, 2.0 / 3.0, 0.5, 1.0 / 3.0, 0.25,
    1.0 / 6.0, 0.125, 1.0 / 12.0, 0.0625, 0.03125,
];

// ---------------------------------------------------------------- specs ---

fn global_specs() -> Vec<Spec> {
    vec![
        Spec("On", "", Map::Bool, 1.0, A_S, false),
        Spec("Key", "%", Map::Lin { min: 0.0, max: 100.0 }, 100.0, A, false),
        Spec("Stereo", "%", Map::Lin { min: 0.0, max: 200.0 }, 100.0, A_M, false),
        Spec("Squeeze", "", Map::Bool, 1.0, A_S, false),
        Spec("Level", "dB", Map::Db { min: -60.0, max: 6.0 }, 0.0, A_M, false),
        Spec("Voices", "", Map::Int { min: 1, max: 16 }, 8.0, S, false),
    ]
}

/// Index order is load-bearing (see parameter map §3) — never reorder.
fn grain_specs() -> Vec<Spec> {
    vec![
        Spec("Gate", "", Map::Bool, 0.0, A | M | PN | S, false), // 0 (G1 default overridden to on)
        Spec("Rate", "Hz", Map::Cube { max: 500.0 }, 10.0, A_M_PN, false), // 1
        Spec("Rate Tempo", "", Map::Bool, 0.0, A_S, false),      // 2
        Spec("Rate Div", "", Map::Steps { count: N_DIVS }, DIV_1_8, A_S, false), // 3
        Spec("Jitter", "%", Map::Lin { min: 0.0, max: 100.0 }, 0.0, A_M_PN, false), // 4
        Spec("Rate>Len", "", Map::Bool, 1.0, A_S, false),        // 5
        Spec("Length", "%|ms", Map::Cube { max: 1000.0 }, 100.0, A_M_PN, false), // 6
        Spec("Pan", "%", Map::Lin { min: 0.0, max: 100.0 }, 0.0, A_M_PN, false), // 7
        Spec("Level", "%", Map::Lin { min: 0.0, max: 100.0 }, 100.0, A_M_PN, false), // 8
        Spec("Sample", "%", Map::Lin { min: 0.0, max: 100.0 }, 0.0, A_M_PN, false), // 9
        Spec("Position", "%", Map::Lin { min: 0.0, max: 100.0 }, 0.0, A_M_PN, false), // 10
        Spec("Loop", "", Map::Bool, 1.0, A_S, false),            // 11
        Spec("Loop Size", "x", Map::Lin { min: 0.0, max: 2.0 }, 1.0, A_M_PN, false), // 12
        Spec("Gate Sync", "", Map::Bool, 0.0, A_S, false),       // 13
        Spec("Pitch", "st", Map::Lin { min: -12.0, max: 12.0 }, 0.0, A_M_PN, true), // 14
        Spec("Pos>Pitch", "", Map::Bool, 0.0, A_S, false),       // 15
        Spec("Direction", "%", Map::Lin { min: -100.0, max: 100.0 }, 100.0, A_M_PN, true), // 16
        Spec("Shape", "%", Map::Lin { min: 0.0, max: 100.0 }, 0.0, A_M_PN, false), // 17
        Spec("Skew", "%", Map::Lin { min: -100.0, max: 100.0 }, 0.0, A_M_PN, true), // 18
        Spec("Algo", "", Map::Steps { count: 4 }, 1.0, A_S, false), // 19 [new] read/stretch algorithm
        Spec("Mode", "", Map::Steps { count: N_GRAIN_MODES }, 0.0, A_S, false), // 20 [new] synthesis mode
    ]
}
pub const GRAIN_LEVEL_IDX: u8 = 8;
pub const GRAIN_PITCH_IDX: u8 = 14;
pub const GRAIN_ALGO_IDX: u8 = 19;
pub const GRAIN_MODE_IDX: u8 = 20;
pub const N_GRAIN_PARAMS: usize = 21;

/// Synthesis modes a grain engine can run (`Mode` param). Append-only: the
/// step index is persisted by the host as a plain integer, so new modes may
/// be added at the end without breaking saved patches. See `engine::GrainMode`.
/// (Grains, Stretch, Cloud, Spectral.)
pub const N_GRAIN_MODES: u16 = 4;

/// Shared by LFO 1–6 and Loop LFO 1–3 (only the Rate default differs).
fn lfo_specs(loop_lfo: bool) -> Vec<Spec> {
    let rate_default = if loop_lfo { 0.10 } else { 1.0 };
    vec![
        Spec("Gate", "", Map::Bool, 1.0, A | M | S, false),    // 0
        Spec("Transport", "", Map::Bool, 0.0, A_S, false),     // 1
        Spec("Rate", "Hz", Map::Cube { max: 50.0 }, rate_default, A_M_PN, false), // 2
        Spec("Rate Tempo", "", Map::Bool, 0.0, A_S, false),    // 3
        Spec("Rate Div", "", Map::Steps { count: N_DIVS }, DIV_1_4, A_S, false), // 4
        Spec("Phase", "deg", Map::Lin { min: 0.0, max: 360.0 }, 0.0, A_M_PN, false), // 5
        Spec("St.Phase", "%", Map::Lin { min: 0.0, max: 100.0 }, 0.0, A_M, false), // 6
        Spec("Lag", "%", Map::Lin { min: 0.0, max: 100.0 }, 0.0, A_M, false), // 7
        Spec("Trig", "", Map::Bool, 0.0, A | PN | S, false),   // 8 (edge-triggered)
    ]
}
pub const LFO_TRIG_IDX: u8 = 8;
pub const N_LFO_PARAMS: usize = 9;

fn stphase_specs() -> Vec<Spec> {
    vec![Spec("St.Phase", "%", Map::Lin { min: 0.0, max: 100.0 }, 0.0, A_M, false)]
}

fn slot_specs() -> Vec<Spec> {
    vec![
        Spec("Root Note", "", Map::Int { min: 0, max: 127 }, 60.0, S, false), // 0
        Spec("Loop Start", "%", Map::Lin { min: 0.0, max: 100.0 }, 0.0, A, false), // 1
        Spec("Loop End", "%", Map::Lin { min: 0.0, max: 100.0 }, 100.0, A, false), // 2
        Spec("Gain", "dB", Map::Db { min: -12.0, max: 12.0 }, 0.0, A_M, true), // 3
    ]
}
pub const SLOT_LOOP_START_IDX: u8 = 1;
pub const SLOT_LOOP_END_IDX: u8 = 2;

fn matrix_specs() -> Vec<Spec> {
    use crate::matrix::{DEST_COUNT, SOURCE_COUNT};
    vec![
        Spec("Source", "", Map::Steps { count: SOURCE_COUNT }, 0.0, A_S, false), // 0
        Spec("Amt 1", "", Map::Lin { min: -100.0, max: 100.0 }, 0.0, A_M, true), // 1
        Spec("Dest 1", "", Map::Steps { count: DEST_COUNT }, 0.0, A_S, false),   // 2
        Spec("Amt 2", "", Map::Lin { min: -100.0, max: 100.0 }, 0.0, A_M, true), // 3
        Spec("Dest 2", "", Map::Steps { count: DEST_COUNT }, 0.0, A_S, false),   // 4
        Spec("Scale", "", Map::Steps { count: SOURCE_COUNT }, 0.0, A_S, false),  // 5
    ]
}
pub const N_MATRIX_ROWS: u8 = 32;
pub const N_MATRIX_COLS: usize = 6;

// ---------------------------------------------------------------- table ---

fn push(out: &mut Vec<ParamInfo>, module: Module, instance: u8, prefix: &str, specs: Vec<Spec>) {
    for (i, s) in specs.into_iter().enumerate() {
        let name = if prefix.is_empty() {
            s.0.to_string()
        } else {
            format!("{prefix} {}", s.0)
        };
        let default_norm = s.2.to_norm(s.3);
        out.push(ParamInfo {
            id: pid(module, instance, i as u8),
            module,
            instance,
            index: i as u8,
            name,
            unit: s.1,
            map: s.2,
            default_display: s.3,
            default_norm,
            flags: s.4,
            bipolar: s.5,
        });
    }
}

pub fn build_param_table() -> Vec<ParamInfo> {
    let mut t = Vec::with_capacity(380);
    push(&mut t, Module::Global, 0, "", global_specs());
    for m in 0..8u8 {
        let spec = vec![Spec(
            "Value",
            "%",
            Map::Lin { min: -100.0, max: 100.0 },
            0.0,
            A | M | PN,
            true,
        )];
        push(&mut t, Module::Macro, m, &format!("Macro {}", m + 1), spec);
    }
    for g in 0..3u8 {
        let mut s = grain_specs();
        if g == 0 {
            s[0].3 = 1.0; // Grain 1 gated on in the init patch
        }
        push(&mut t, Module::Grain, g, &format!("Grain {}", g + 1), s);
    }
    for l in 0..3u8 {
        push(&mut t, Module::LoopLfo, l, &format!("Loop LFO {}", l + 1), lfo_specs(true));
    }
    for l in 0..6u8 {
        push(&mut t, Module::Lfo, l, &format!("LFO {}", l + 1), lfo_specs(false));
    }
    for d in 0..3u8 {
        push(&mut t, Module::Drift, d, &format!("Drift {}", d + 1), stphase_specs());
    }
    for n in 0..3u8 {
        push(&mut t, Module::Noise, n, &format!("Noise {}", n + 1), stphase_specs());
    }
    for s in 0..6u8 {
        push(&mut t, Module::Slot, s, &format!("Slot {}", s + 1), slot_specs());
    }
    for r in 0..N_MATRIX_ROWS {
        push(&mut t, Module::Matrix, r, &format!("Mod {:02}", r), matrix_specs());
    }
    t
}

/// Built table plus an id → position index.
pub struct ParamTable {
    pub infos: Vec<ParamInfo>,
    index: HashMap<u32, usize>,
}

impl ParamTable {
    pub fn new() -> Self {
        let infos = build_param_table();
        let index = infos.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
        Self { infos, index }
    }

    pub fn len(&self) -> usize {
        self.infos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.infos.is_empty()
    }

    pub fn pos(&self, id: u32) -> usize {
        self.index[&id]
    }

    pub fn try_pos(&self, id: u32) -> Option<usize> {
        self.index.get(&id).copied()
    }

    pub fn get(&self, id: u32) -> &ParamInfo {
        &self.infos[self.pos(id)]
    }
}

impl Default for ParamTable {
    fn default() -> Self {
        Self::new()
    }
}

/// A full set of normalized parameter values, aligned with `ParamTable`.
#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    pub values: Vec<f32>,
}

impl Patch {
    pub fn init(table: &ParamTable) -> Self {
        Self {
            values: table.infos.iter().map(|p| p.default_norm).collect(),
        }
    }

    pub fn get(&self, table: &ParamTable, id: u32) -> f32 {
        self.values[table.pos(id)]
    }

    pub fn set(&mut self, table: &ParamTable, id: u32, norm: f32) {
        self.values[table.pos(id)] = norm.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_380_params() {
        // 6 global + 8 macro + 3×21 grain + 3×9 loop-lfo + 6×9 lfo
        // + 3 drift + 3 noise + 6×4 slot + 32×6 matrix = 380.
        assert_eq!(ParamTable::new().len(), 380);
    }

    #[test]
    fn ids_are_unique_and_stable() {
        let t = ParamTable::new();
        let mut seen = std::collections::HashSet::new();
        for p in &t.infos {
            assert!(seen.insert(p.id), "duplicate id {:#x} ({})", p.id, p.name);
        }
        // Spot checks against the documented scheme.
        assert_eq!(pid(Module::Global, 0, 4), 0x0000_0004); // Level
        assert_eq!(pid(Module::Grain, 1, 4), 0x0002_0104); // Grain 2 Jitter
        assert_eq!(pid(Module::Matrix, 31, 5), 0x0008_1F05); // Mod 31 Scale
        assert_eq!(t.get(0x0002_0104).name, "Grain 2 Jitter");
    }

    #[test]
    fn matrix_columns_are_consecutive_in_table_order() {
        let t = ParamTable::new();
        for r in 0..N_MATRIX_ROWS {
            let base = t.pos(pid(Module::Matrix, r, 0));
            for c in 1..N_MATRIX_COLS as u8 {
                assert_eq!(t.pos(pid(Module::Matrix, r, c)), base + c as usize);
            }
        }
    }

    #[test]
    fn init_patch_defaults() {
        let t = ParamTable::new();
        let p = Patch::init(&t);
        assert_eq!(p.get(&t, pid(Module::Grain, 0, 0)), 1.0); // Grain 1 Gate on
        assert_eq!(p.get(&t, pid(Module::Grain, 1, 0)), 0.0); // Grain 2 Gate off
        assert_eq!(p.get(&t, pid(Module::Grain, 2, 0)), 0.0); // Grain 3 Gate off
        // Grain Rate default 10 Hz round-trips through the cube taper.
        let info = t.get(pid(Module::Grain, 0, 1));
        let disp = info.map.to_display(p.get(&t, info.id));
        assert!((disp - 10.0).abs() < 0.05, "grain rate default = {disp}");
    }

    #[test]
    fn map_roundtrips() {
        let maps = [
            Map::Lin { min: -12.0, max: 12.0 },
            Map::Cube { max: 500.0 },
            Map::Db { min: -60.0, max: 6.0 },
            Map::Steps { count: N_DIVS },
            Map::Int { min: 1, max: 16 },
        ];
        for m in maps {
            for i in 0..=20 {
                let n = i as f32 / 20.0;
                let d = m.to_display(n);
                let n2 = m.to_norm(d);
                let d2 = m.to_display(n2);
                assert!((d - d2).abs() < 1e-3, "{m:?}: {d} vs {d2}");
            }
        }
    }
}

#[cfg(test)]
mod div_tests {
    use super::*;

    #[test]
    fn div_beats_match_names() {
        assert_eq!(DIV_BEATS.len(), DIV_NAMES.len());
        assert_eq!(DIV_BEATS[DIV_1_4 as usize], 1.0); // 1/4 = one beat
        assert_eq!(DIV_BEATS[DIV_1_8 as usize], 0.5);
        for w in DIV_BEATS.windows(2) {
            assert!(w[0] > w[1], "divisions must shorten monotonically");
        }
    }
}
