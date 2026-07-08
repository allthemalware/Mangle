//! Voices and the voice manager. Parameter map §10.
//!
//! A voice = one sounding note: three grain lanes, three Loop LFO phase
//! instances, and the voice-local mod sources. The six main LFOs, Drift,
//! Noise and the env follower stay global (the texture breathes together).

use crate::curve::LfoState;
use crate::engine::Lane;

pub struct Voice {
    pub note: u8,
    /// Host note id (CLAP), -1 when unknown.
    pub note_id: i32,
    pub channel: u8,
    pub velocity: f32,
    /// Per-voice random constant, rolled at note-on (mod source).
    pub voice_random: f32,
    pub pressure: f32,
    /// CLAP note-expression tuning, semitones (added to the note pitch).
    pub tuning: f32,
    /// CLAP note-expression volume, a linear gain ratio (1.0 = unity).
    pub expr_volume: f32,
    /// CLAP note-expression pan, -1..1 (-1 hard left, +1 hard right).
    pub expr_pan: f32,
    pub gate: bool,
    pub lanes: [Lane; 3],
    pub loop_lfos: [LfoState; 3],
    /// CLAP per-note modulation offsets: (param id, normalized offset).
    pub mod_offsets: Vec<(u32, f32)>,
    /// Last block's Loop LFO outputs — fed back as mod sources.
    pub last_scan: [(f32, f32); 3],
    /// True from note-on until the shell reports voice termination.
    pub was_sounding: bool,
    /// Allocation order stamp for oldest-steal.
    pub stamp: u64,
}

impl Voice {
    fn new(seed: u64) -> Self {
        Self {
            note: 0,
            note_id: -1,
            channel: 0,
            velocity: 0.0,
            voice_random: 0.0,
            pressure: 0.0,
            tuning: 0.0,
            expr_volume: 1.0,
            expr_pan: 0.0,
            gate: false,
            lanes: [Lane::new(seed), Lane::new(seed ^ 0x9E37), Lane::new(seed ^ 0x79B9)],
            loop_lfos: [LfoState::default(), LfoState::default(), LfoState::default()],
            mod_offsets: Vec::with_capacity(128),
            last_scan: [(0.0, 0.0); 3],
            was_sounding: false,
            stamp: 0,
        }
    }

    /// Record a CLAP per-note modulation offset (update or insert).
    pub fn set_mod_offset(&mut self, id: u32, offset: f32) {
        if let Some(e) = self.mod_offsets.iter_mut().find(|(i, _)| *i == id) {
            e.1 = offset;
        } else if self.mod_offsets.len() < self.mod_offsets.capacity() {
            self.mod_offsets.push((id, offset));
        }
    }

    pub fn sounding(&self) -> bool {
        self.gate || self.lanes.iter().any(|l| l.is_ringing())
    }
}

pub struct VoiceManager {
    pub voices: Vec<Voice>,
    pub max_voices: usize,
    counter: u64,
}

impl VoiceManager {
    /// All 16 voices are allocated up front so `note_on` never allocates
    /// on the audio thread (grain pools, offset lists, scratch — all
    /// pre-sized).
    pub fn new(max_voices: usize, seed: u64) -> Self {
        let voices = (0..16)
            .map(|i| Voice::new(seed ^ (i as u64).wrapping_mul(0xA5A5_5A5A)))
            .collect();
        Self { voices, max_voices: max_voices.clamp(1, 16), counter: 0 }
    }

    pub fn set_max_voices(&mut self, n: usize) {
        self.max_voices = n.clamp(1, 16);
    }

    /// Allocate (or steal) a voice for a note-on. `voice_random` is rolled
    /// by the caller's RNG so the whole engine stays seed-deterministic.
    pub fn note_on(
        &mut self,
        note: u8,
        note_id: i32,
        channel: u8,
        velocity: f32,
        voice_random: f32,
    ) -> &mut Voice {
        self.counter += 1;
        let stamp = self.counter;

        // Prefer a fully-finished (free) voice in the polyphony window. Else
        // steal the least disruptive sounding one: a released (gate-off,
        // ringing-out) voice before a held one, oldest within each group. This
        // is the design's "oldest-then-quietest", made release-aware — a
        // releasing tail is the quietest thing to cut. Voices beyond
        // `max_voices` (after the limit was lowered mid-flight) ring out but are
        // never re-allocated.
        let limit = self.max_voices.min(self.voices.len());
        let idx = if let Some(i) = self.voices[..limit].iter().position(|v| !v.sounding()) {
            i
        } else {
            self.voices[..limit]
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| (v.gate, v.stamp))
                .map(|(i, _)| i)
                .unwrap_or(0)
        };

        let v = &mut self.voices[idx];
        v.note = note;
        v.note_id = note_id;
        v.channel = channel;
        v.velocity = velocity;
        v.voice_random = voice_random;
        v.pressure = 0.0;
        v.tuning = 0.0;
        v.expr_volume = 1.0;
        v.expr_pan = 0.0;
        v.gate = true;
        v.was_sounding = true;
        v.mod_offsets.clear();
        v.last_scan = [(0.0, 0.0); 3];
        v.stamp = stamp;
        for lane in v.lanes.iter_mut() {
            // soft_reset (not reset) so a stolen, still-sounding voice's grains
            // fade out instead of clicking; a free voice has nothing to fade.
            lane.soft_reset();
        }
        for lfo in v.loop_lfos.iter_mut() {
            *lfo = LfoState::default();
        }
        v
    }

    /// Gate off the matching voice (by note id when given, else by note).
    pub fn note_off(&mut self, note: u8, note_id: i32) {
        for v in self.voices.iter_mut() {
            let hit = if note_id >= 0 { v.note_id == note_id } else { v.note == note };
            if hit && v.gate {
                v.gate = false;
            }
        }
    }

    pub fn sounding_count(&self) -> usize {
        self.voices.iter().filter(|v| v.sounding()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_and_steals_oldest() {
        let mut vm = VoiceManager::new(2, 1);
        vm.note_on(60, 1, 0, 0.8, 0.1);
        vm.note_on(64, 2, 0, 0.8, 0.2);
        assert_eq!(vm.voices.iter().filter(|v| v.gate).count(), 2);
        // Third note steals the oldest (note 60) within the 2-voice window.
        vm.note_on(67, 3, 0, 0.8, 0.3);
        assert_eq!(vm.voices.iter().filter(|v| v.gate).count(), 2);
        assert!(vm.voices.iter().any(|v| v.note == 67 && v.gate));
        assert!(!vm.voices.iter().any(|v| v.note == 60 && v.gate));
        // Pre-allocated pool: note_on never grows the vec.
        assert_eq!(vm.voices.len(), 16);
    }

    #[test]
    fn steals_released_ringing_voice_before_older_held_one() {
        use crate::engine::LaneParams;
        use crate::sample::{SampleBank, SampleSlot};
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot::test_tone(sr));
        let mut vm = VoiceManager::new(2, 1);
        vm.note_on(60, 1, 0, 0.8, 0.0); // voice 0: older, stays held
        vm.note_on(64, 2, 0, 0.8, 0.0); // voice 1: newer
        // Give both voices live grains so they count as "ringing".
        let p = LaneParams { loop_on: false, ..Default::default() };
        let mut l = vec![0.0f32; 256];
        let mut r = vec![0.0f32; 256];
        for v in vm.voices.iter_mut().take(2) {
            v.lanes[0].process(&[p, p], &bank, (0.0, 0.0), sr, &mut l, &mut r, 256);
        }
        assert!(vm.voices[0].lanes[0].active_grains() > 0);
        assert!(vm.voices[1].lanes[0].active_grains() > 0);
        // Release the *newer* voice (64): gate off, but its tail still rings.
        vm.note_off(64, 2);
        assert!(!vm.voices[1].gate && vm.voices[1].sounding());
        // A new note steals the released voice, sparing the older *held* note 60
        // (pure oldest-stealing would have taken 60 instead).
        vm.note_on(67, 3, 0, 0.8, 0.0);
        assert!(vm.voices.iter().any(|v| v.note == 60 && v.gate), "older held note was stolen");
        assert!(vm.voices.iter().any(|v| v.note == 67 && v.gate), "new note not allocated");
        assert!(
            !vm.voices.iter().any(|v| v.note == 64 && v.note_id == 2),
            "released voice should have been reused"
        );
    }

    #[test]
    fn note_off_by_id_and_by_note() {
        let mut vm = VoiceManager::new(4, 1);
        vm.note_on(60, 11, 0, 0.8, 0.0);
        vm.note_on(60, 12, 0, 0.8, 0.0);
        vm.note_off(60, 11);
        let gates: Vec<bool> = vm.voices.iter().map(|v| v.gate).collect();
        assert_eq!(gates.iter().filter(|g| **g).count(), 1);
        vm.note_off(60, -1); // by note: kills the rest
        assert_eq!(vm.voices.iter().filter(|v| v.gate).count(), 0);
    }

    #[test]
    fn released_voice_is_reused() {
        let mut vm = VoiceManager::new(8, 1);
        vm.note_on(60, 1, 0, 0.8, 0.0);
        vm.note_off(60, 1);
        // No grains were ever spawned, so the voice is free and reused.
        vm.note_on(72, 2, 0, 0.8, 0.0);
        assert_eq!(vm.voices[0].note, 72);
        assert_eq!(vm.voices.iter().filter(|v| v.gate).count(), 1);
    }
}

#[cfg(test)]
mod offset_tests {
    use super::*;

    #[test]
    fn mod_offsets_update_and_clear_on_reuse() {
        let mut vm = VoiceManager::new(2, 1);
        vm.note_on(60, 1, 0, 0.8, 0.0);
        vm.voices[0].set_mod_offset(42, 0.5);
        vm.voices[0].set_mod_offset(42, -0.25);
        assert_eq!(vm.voices[0].mod_offsets, vec![(42, -0.25)]);
        vm.note_off(60, 1);
        vm.note_on(72, 2, 0, 0.8, 0.0);
        assert!(vm.voices[0].mod_offsets.is_empty());
        assert!(vm.voices[0].was_sounding);
    }
}
