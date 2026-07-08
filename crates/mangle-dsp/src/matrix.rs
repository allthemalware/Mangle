//! Mod matrix: source/destination enums, their stable stepped-param index
//! encodings, and per-slot evaluation. See parameter map §8.

/// None · LFO 1–6 · Loop LFO 1–3 · Drift 1–3 · Noise 1–3 · Macro 1–8 ·
/// Const ×4 · EnvFollow · Velocity · Note · NoteOct · NoteGate · Pressure ·
/// VoiceRandom  = 35 entries.
pub const SOURCE_COUNT: u16 = 35;

/// None · LFO {Gate,Phase,Rate,Lag} ×6 · Loop LFO ×3 · Grain ×3 ×14 · Fx ×2 = 81.
/// Append-only: matrix Dest is an `IntParam` storing the plain index, so growing
/// this only unlocks new indices — existing saved routings keep their meaning.
pub const DEST_COUNT: u16 = 81;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModSource {
    None,
    Lfo(u8),
    LoopLfo(u8),
    Drift(u8),
    Noise(u8),
    Macro(u8),
    /// Constant value in percent: -200, -100, +100, +200.
    Const(i16),
    EnvFollow,
    Velocity,
    Note,
    NoteOct,
    NoteGate,
    Pressure,
    VoiceRandom,
}

pub const CONSTS: [i16; 4] = [-200, -100, 100, 200];

pub fn source_from_index(i: u16) -> ModSource {
    match i {
        0 => ModSource::None,
        1..=6 => ModSource::Lfo((i - 1) as u8),
        7..=9 => ModSource::LoopLfo((i - 7) as u8),
        10..=12 => ModSource::Drift((i - 10) as u8),
        13..=15 => ModSource::Noise((i - 13) as u8),
        16..=23 => ModSource::Macro((i - 16) as u8),
        24..=27 => ModSource::Const(CONSTS[(i - 24) as usize]),
        28 => ModSource::EnvFollow,
        29 => ModSource::Velocity,
        30 => ModSource::Note,
        31 => ModSource::NoteOct,
        32 => ModSource::NoteGate,
        33 => ModSource::Pressure,
        34 => ModSource::VoiceRandom,
        _ => ModSource::None,
    }
}

pub fn source_to_index(s: ModSource) -> u16 {
    match s {
        ModSource::None => 0,
        ModSource::Lfo(n) => 1 + n as u16,
        ModSource::LoopLfo(n) => 7 + n as u16,
        ModSource::Drift(n) => 10 + n as u16,
        ModSource::Noise(n) => 13 + n as u16,
        ModSource::Macro(n) => 16 + n as u16,
        ModSource::Const(c) => 24 + CONSTS.iter().position(|&x| x == c).unwrap_or(2) as u16,
        ModSource::EnvFollow => 28,
        ModSource::Velocity => 29,
        ModSource::Note => 30,
        ModSource::NoteOct => 31,
        ModSource::NoteGate => 32,
        ModSource::Pressure => 33,
        ModSource::VoiceRandom => 34,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LfoDest {
    Gate,
    Phase,
    Rate,
    Lag,
}
pub const LFO_DESTS: [LfoDest; 4] = [LfoDest::Gate, LfoDest::Phase, LfoDest::Rate, LfoDest::Lag];

/// Order matches the manual's destination table (parameter map §8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrainDest {
    Rate,
    Length,
    Jitter,
    Gate,
    LoopSize,
    LoopPosition,
    Position,
    Pitch,
    Direction,
    Shape,
    Skew,
    Level,
    Pan,
    Sample,
}
pub const GRAIN_DESTS: [GrainDest; 14] = [
    GrainDest::Rate,
    GrainDest::Length,
    GrainDest::Jitter,
    GrainDest::Gate,
    GrainDest::LoopSize,
    GrainDest::LoopPosition,
    GrainDest::Position,
    GrainDest::Pitch,
    GrainDest::Direction,
    GrainDest::Shape,
    GrainDest::Skew,
    GrainDest::Level,
    GrainDest::Pan,
    GrainDest::Sample,
];

/// Master-filter (Fx module) destinations. Append-only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FxDest {
    Cutoff,
    Reso,
}
pub const FX_DESTS: [FxDest; 2] = [FxDest::Cutoff, FxDest::Reso];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModDest {
    None,
    Lfo(u8, LfoDest),
    LoopLfo(u8, LfoDest),
    Grain(u8, GrainDest),
    Fx(FxDest),
}

pub fn dest_from_index(i: u16) -> ModDest {
    if i == 0 {
        return ModDest::None;
    }
    let mut i = i - 1;
    if i < 24 {
        return ModDest::Lfo((i / 4) as u8, LFO_DESTS[(i % 4) as usize]);
    }
    i -= 24;
    if i < 12 {
        return ModDest::LoopLfo((i / 4) as u8, LFO_DESTS[(i % 4) as usize]);
    }
    i -= 12;
    if i < 42 {
        return ModDest::Grain((i / 14) as u8, GRAIN_DESTS[(i % 14) as usize]);
    }
    i -= 42;
    if i < 2 {
        return ModDest::Fx(FX_DESTS[i as usize]);
    }
    ModDest::None
}

fn lfo_dest_pos(d: LfoDest) -> u16 {
    LFO_DESTS.iter().position(|&x| x == d).unwrap() as u16
}

fn grain_dest_pos(d: GrainDest) -> u16 {
    GRAIN_DESTS.iter().position(|&x| x == d).unwrap() as u16
}

fn fx_dest_pos(d: FxDest) -> u16 {
    FX_DESTS.iter().position(|&x| x == d).unwrap() as u16
}

pub fn dest_to_index(d: ModDest) -> u16 {
    match d {
        ModDest::None => 0,
        ModDest::Lfo(n, sub) => 1 + n as u16 * 4 + lfo_dest_pos(sub),
        ModDest::LoopLfo(n, sub) => 25 + n as u16 * 4 + lfo_dest_pos(sub),
        ModDest::Grain(n, sub) => 37 + n as u16 * 14 + grain_dest_pos(sub),
        ModDest::Fx(sub) => 79 + fx_dest_pos(sub),
    }
}

/// Which unit a destination belongs to — used by the randomizer lock system
/// so locked units are never even *routed into*.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
    Global,
    Lfo(u8),
    LoopLfo(u8),
    Grain(u8),
}

pub fn dest_owner(d: ModDest) -> Owner {
    match d {
        ModDest::None => Owner::Global,
        ModDest::Lfo(n, _) => Owner::Lfo(n),
        ModDest::LoopLfo(n, _) => Owner::LoopLfo(n),
        ModDest::Grain(n, _) => Owner::Grain(n),
        ModDest::Fx(_) => Owner::Global, // master filter is global-scope
    }
}

/// One decoded matrix row in display units (amts -100..100).
#[derive(Clone, Copy, Debug, Default)]
pub struct MatrixSlot {
    pub source: u16,
    pub amt1: f32,
    pub dest1: u16,
    pub amt2: f32,
    pub dest2: u16,
    pub scale: u16,
}

impl MatrixSlot {
    pub fn is_active(&self) -> bool {
        self.source != 0 && (self.dest1 != 0 || self.dest2 != 0)
    }

    /// Contribution of this slot to (dest1, dest2) given source values in
    /// -1..1. `scale_value` is the Scale source's value, 1.0 when None.
    pub fn eval(&self, source_value: f32, scale_value: f32) -> (f32, f32) {
        let s = source_value * scale_value;
        (self.amt1 / 100.0 * s, self.amt2 / 100.0 * s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_index_roundtrip() {
        for i in 0..SOURCE_COUNT {
            let s = source_from_index(i);
            assert_eq!(source_to_index(s), i, "source index {i} ({s:?})");
        }
    }

    #[test]
    fn dest_index_roundtrip() {
        for i in 0..DEST_COUNT {
            let d = dest_from_index(i);
            assert_eq!(dest_to_index(d), i, "dest index {i} ({d:?})");
        }
    }

    #[test]
    fn dest_owner_examples() {
        assert_eq!(dest_owner(dest_from_index(1)), Owner::Lfo(0));
        assert_eq!(dest_owner(ModDest::Grain(2, GrainDest::Skew)), Owner::Grain(2));
        assert_eq!(dest_owner(ModDest::LoopLfo(1, LfoDest::Rate)), Owner::LoopLfo(1));
    }

    #[test]
    fn slot_eval_scales() {
        let slot = MatrixSlot { source: 1, amt1: 50.0, dest1: 1, amt2: -100.0, dest2: 2, scale: 0 };
        let (a, b) = slot.eval(1.0, 1.0);
        assert!((a - 0.5).abs() < 1e-6);
        assert!((b + 1.0).abs() < 1e-6);
        let (a, _) = slot.eval(1.0, 0.5);
        assert!((a - 0.25).abs() < 1e-6);
    }
}

/// Short display name for a source index (host param formatting + matrix UI).
pub fn source_name(i: u16) -> String {
    match source_from_index(i) {
        ModSource::None => "—".to_string(),
        ModSource::Lfo(n) => format!("LFO {}", n + 1),
        ModSource::LoopLfo(n) => format!("Loop {}", n + 1),
        ModSource::Drift(n) => format!("Drift {}", n + 1),
        ModSource::Noise(n) => format!("Noise {}", n + 1),
        ModSource::Macro(n) => format!("Macro {}", n + 1),
        ModSource::Const(c) => format!("{c:+}"),
        ModSource::EnvFollow => "Env".to_string(),
        ModSource::Velocity => "Vel".to_string(),
        ModSource::Note => "Note".to_string(),
        ModSource::NoteOct => "Oct".to_string(),
        ModSource::NoteGate => "Gate".to_string(),
        ModSource::Pressure => "Press".to_string(),
        ModSource::VoiceRandom => "VRand".to_string(),
    }
}

/// Short display name for a destination index.
pub fn dest_name(i: u16) -> String {
    match dest_from_index(i) {
        ModDest::None => "—".to_string(),
        ModDest::Lfo(n, sub) => format!("LFO{} {:?}", n + 1, sub),
        ModDest::LoopLfo(n, sub) => format!("Loop{} {:?}", n + 1, sub),
        ModDest::Grain(n, sub) => format!("G{} {:?}", n + 1, sub),
        ModDest::Fx(sub) => format!("Flt {:?}", sub),
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn all_indices_have_names() {
        for i in 0..SOURCE_COUNT {
            assert!(!source_name(i).is_empty());
        }
        for i in 0..DEST_COUNT {
            assert!(!dest_name(i).is_empty());
        }
        assert_eq!(source_name(1), "LFO 1");
        assert_eq!(dest_name(0), "—");
    }
}
