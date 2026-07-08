//! One grain engine "lane": the per-voice instance of a grain engine.
//!
//! A lane owns two emission clocks (one per stereo channel — Torsion's
//! independent L/R grain streams), a fixed-capacity pool of active grains,
//! and renders additively into a stereo block. Everything obeys the
//! snapshot rule: a grain is fully described at creation and never touched
//! again.
//!
//! Allocation-free in steady state: the grain pool and the emission
//! scratch buffer are pre-sized; overflow steals the oldest grain.

use crate::grain::{grain_env_coeffs, grain_envelope_at, Emitter, GrainSnapshot};
use crate::rng::Rng;
use crate::sample::{read, Algo, SampleBank};
use crate::spectral::SpectralState;

/// Hard cap of simultaneously sounding grains per lane (per voice, per
/// engine). Oldest is stolen beyond this.
pub const MAX_GRAINS: usize = 96;

/// Length, in output samples, of the fade applied to a stolen voice's leftover
/// grains so voice stealing doesn't click (~5.3 ms at 48 kHz, SR-independent
/// enough for a declick).
const STEAL_FADE: u32 = 256;

/// Synthesis paradigm for a grain engine lane, chosen by the per-engine
/// `Mode` parameter (`params::GRAIN_MODE_IDX`). All three Tier-1 modes share
/// the grain snapshot/render path; they differ only in how each grain's read
/// start and increment are chosen:
///
/// * `Granular` — classic asynchronous granular (Torsion default). Start =
///   Loop-LFO scan + Position; increment = Direction × pitch ratio.
/// * `Stretch` — time-stretch / scrub. A per-channel playhead walks the loop
///   window at Direction× (decoupled from pitch); grains emit at the playhead
///   and read forward at the pitch ratio only. Direction 0 freezes the
///   playhead while pitch still transposes — a moving freeze.
/// * `Cloud` — diffuse / Paulstretch-style smear. Like Granular, but every
///   grain is sprayed to a random read-phase around Position (Jitter is
///   reinterpreted as position spray, and emission-time jitter is dropped);
///   dense overlapping grains average into a phase-randomized texture.
/// * `Spectral` — streaming phase vocoder (Tier 2): clean time-stretch, smooth
///   spectral freeze (Direction 0), and frequency-domain pitch shift. Does not
///   use the grain pool; see `crate::spectral`.
///
/// Append-only: the `Mode` step index is persisted as a plain integer, so new
/// modes may be added at the end without breaking saved patches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrainMode {
    Granular,
    Stretch,
    Cloud,
    Spectral,
}

pub const MODE_NAMES: [&str; 4] = ["Grains", "Stretch", "Cloud", "Spectral"];

impl GrainMode {
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => GrainMode::Stretch,
            2 => GrainMode::Cloud,
            3 => GrainMode::Spectral,
            _ => GrainMode::Granular,
        }
    }
}

/// Per-engine pitch quantization (scale lock), chosen by the `Quant` param.
/// Applied post-modulation to the composed pitch at snapshot time, so modulated
/// or randomized pitch lands on scale degrees instead of between them.
/// Append-only: the step index is persisted as a plain integer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PitchQuant {
    Off,
    Chromatic,
    Octave,
    Fifth,
    Major,
    Minor,
    Penta,
    MinorPenta,
}

pub const QUANT_NAMES: [&str; 8] =
    ["Off", "Chrom", "Oct", "5th", "Maj", "Min", "Pent", "mPent"];

impl PitchQuant {
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => PitchQuant::Chromatic,
            2 => PitchQuant::Octave,
            3 => PitchQuant::Fifth,
            4 => PitchQuant::Major,
            5 => PitchQuant::Minor,
            6 => PitchQuant::Penta,
            7 => PitchQuant::MinorPenta,
            _ => PitchQuant::Off,
        }
    }

    /// Allowed semitone pitch-classes (mod 12), or empty for Off/Chromatic/Octave
    /// which are handled directly.
    fn classes(self) -> &'static [i32] {
        match self {
            PitchQuant::Fifth => &[0, 7],
            PitchQuant::Major => &[0, 2, 4, 5, 7, 9, 11],
            PitchQuant::Minor => &[0, 2, 3, 5, 7, 8, 10],
            PitchQuant::Penta => &[0, 2, 4, 7, 9],
            PitchQuant::MinorPenta => &[0, 3, 5, 7, 10],
            _ => &[],
        }
    }
}

/// Snap a pitch offset in semitones to the selected scale (relative to 0 = the
/// slot's root reading). Ties resolve toward the lower semitone.
pub fn quantize_semitones(st: f32, q: PitchQuant) -> f32 {
    match q {
        PitchQuant::Off => st,
        PitchQuant::Chromatic => st.round(),
        PitchQuant::Octave => (st / 12.0).round() * 12.0,
        _ => {
            let classes = q.classes();
            let base = st.round() as i32;
            let mut best = st;
            let mut best_d = f32::INFINITY;
            // Search a full octave either side so wrap-arounds are covered.
            for m in (base - 7)..=(base + 7) {
                if classes.contains(&m.rem_euclid(12)) {
                    let d = (m as f32 - st).abs();
                    if d < best_d {
                        best_d = d;
                        best = m as f32;
                    }
                }
            }
            best
        }
    }
}

/// Resolved (display-domain, post-modulation) parameters for one engine for
/// one block. The voice layer produces these from the patch + mod matrix.
#[derive(Clone, Copy, Debug)]
pub struct LaneParams {
    pub gate: bool,
    /// Emission rate, Hz. <= 0 is off.
    pub rate_hz: f32,
    /// 0..1.
    pub jitter: f32,
    /// Rate>Len link on?
    pub linked: bool,
    /// Linked: percent (100 = COLA-gapless). Free: milliseconds.
    pub length_val: f32,
    /// 0..1 channel crossfade (0 keep, 0.5 mono-ish, 1 swap).
    pub pan: f32,
    /// 0..1.
    pub level: f32,
    /// 0..1 continuous slot selector across *loaded* slots.
    pub sample_sel: f32,
    /// 0..1 start offset from loop start.
    pub position: f32,
    /// Loop LFO scanning enabled?
    pub loop_on: bool,
    /// 0..2, scales the loop window about its midpoint.
    pub loop_size: f32,
    /// Matrix Loop Position contribution: ±1 shifts the window by ± one
    /// window length (clamped to the sample).
    pub loop_pos: f32,
    /// Semitones (engine Pitch + note-expression tuning). Key tracking is
    /// applied at spawn against the chosen slot's root note.
    pub pitch_st: f32,
    /// 0..1 — Global Key amount.
    pub key_track: f32,
    /// MIDI note of the owning voice (float: tuning expressions welcome).
    pub note: f32,
    /// -1..1 signed speed (0 freezes).
    pub direction: f32,
    /// 0..1.
    pub shape: f32,
    /// -1..1.
    pub skew: f32,
    /// Pos>Pitch: when true, the grain read speed follows the scan-position
    /// velocity (tape/vinyl drag) instead of Direction. Granular/Cloud only.
    pub pos_pitch: bool,
    pub algo: Algo,
    /// Synthesis mode for this lane (Granular / Stretch / Cloud).
    pub mode: GrainMode,
    /// Pitch quantization (scale lock), applied to the composed pitch at spawn.
    pub quant: PitchQuant,
}

impl Default for LaneParams {
    fn default() -> Self {
        // Mirrors the init patch (engine gated on for tests).
        Self {
            gate: true,
            rate_hz: 10.0,
            jitter: 0.0,
            linked: true,
            length_val: 100.0,
            pan: 0.0,
            level: 1.0,
            sample_sel: 0.0,
            position: 0.0,
            loop_on: true,
            loop_size: 1.0,
            loop_pos: 0.0,
            pitch_st: 0.0,
            key_track: 0.0,
            note: 60.0,
            direction: 1.0,
            shape: 0.0,
            skew: 0.0,
            pos_pitch: false,
            algo: Algo::Linear,
            mode: GrainMode::Granular,
            quant: PitchQuant::Off,
        }
    }
}

struct ActiveGrain {
    snap: GrainSnapshot,
    pos: f64,
    age: u32,
    /// Samples to wait before this grain starts (intra-block offset).
    delay: u32,
}

/// Pick the active slot and resolve the read window: the slot loop scaled
/// about its midpoint by Loop Size, then shifted by the matrix Loop Position.
/// Returns `(slot_index, wstart, wlen)`, or `None` when no slot is loaded.
/// Shared by `spawn` (per emission), the Stretch playhead advance, and the
/// spectral vocoder.
pub(crate) fn resolve_window(p: &LaneParams, bank: &SampleBank) -> Option<(usize, usize, usize)> {
    let (loaded, n_loaded) = bank.loaded();
    if n_loaded == 0 {
        return None;
    }
    let pick = ((p.sample_sel.clamp(0.0, 1.0) * n_loaded as f32) as usize).min(n_loaded - 1);
    let slot_idx = loaded[pick];
    let slot = bank.slots[slot_idx].as_ref().unwrap();

    // Clamp `ls` below `len` so `ls + 1 <= len`; a slot whose loop_start reaches
    // len (Loop Start = Loop End = 100%) would otherwise make the clamp below
    // `clamp(len + 1, len)`, which panics (min > max) on the audio thread.
    let ls = slot.loop_start.min(slot.len().saturating_sub(1));
    let le = slot.loop_end.clamp(ls + 1, slot.len());
    let mid = (ls + le) as f32 * 0.5;
    let half = (le - ls) as f32 * 0.5 * p.loop_size.clamp(0.0, 2.0);
    let mut wstart = (mid - half).max(0.0) as usize;
    let wend = ((mid + half) as usize).min(slot.len());
    let wlen = (wend.saturating_sub(wstart)).max(1);
    if p.loop_pos != 0.0 {
        let shift = (p.loop_pos * wlen as f32) as isize;
        let max_start = slot.len().saturating_sub(wlen) as isize;
        wstart = (wstart as isize + shift).clamp(0, max_start) as usize;
    }
    Some((slot_idx, wstart, wlen))
}

/// Loop-LFO scan contribution to a grain's start phase (0..1), or 0 when the
/// engine's Loop is off. `scan` is the per-channel Loop LFO output in -1..1.
#[inline]
fn scan_offset(p: &LaneParams, scan: (f32, f32), channel: u8) -> f32 {
    if p.loop_on {
        let s = if channel == 0 { scan.0 } else { scan.1 };
        s * 0.5 + 0.5
    } else {
        0.0
    }
}

pub struct Lane {
    emitters: [Emitter; 2],
    grains: Vec<ActiveGrain>,
    /// Grains left over from a stolen voice, ringing out under a short fade so
    /// the steal doesn't click. Kept separate from `grains` so the new note
    /// spawns into a clean pool.
    fading: Vec<ActiveGrain>,
    /// Output samples elapsed in the current steal fade.
    fade_age: u32,
    scratch: Vec<(u8, usize)>,
    rng: Rng,
    /// Stretch-mode read playhead per channel, as normalized loop-window
    /// phase in [0, 1). Unused by Granular/Cloud.
    playheads: [f64; 2],
    /// Pos>Pitch: previous scan position (scan01 + Position) per channel, and
    /// the velocity derived from it (source samples per output sample). NaN
    /// phase = uninitialized (first chunk after reset has zero velocity).
    pos_phase: [f64; 2],
    pos_vel: [f64; 2],
    /// Spectral-mode phase vocoder. Pre-allocated (boxed to keep `Lane`
    /// small) so switching into Spectral never allocates on the audio thread.
    spectral: Box<SpectralState>,
}

impl Lane {
    pub fn new(seed: u64) -> Self {
        Self {
            emitters: [Emitter::default(), Emitter::default()],
            grains: Vec::with_capacity(MAX_GRAINS),
            fading: Vec::with_capacity(MAX_GRAINS),
            fade_age: 0,
            scratch: Vec::with_capacity(64),
            rng: Rng::new(seed),
            playheads: [0.0; 2],
            pos_phase: [f64::NAN; 2],
            pos_vel: [0.0; 2],
            spectral: Box::new(SpectralState::new()),
        }
    }

    pub fn active_grains(&self) -> usize {
        self.grains.len()
    }

    /// True while the lane still has sound to emit: live grains, a steal
    /// fade-out tail, or a Spectral overlap-add release tail. The voice manager
    /// and shell use this so a released Spectral voice rings out instead of
    /// being cut.
    pub fn is_ringing(&self) -> bool {
        !self.grains.is_empty() || !self.fading.is_empty() || self.spectral.ringing()
    }

    /// Hard-stop everything (Choke / panic): no fade, immediate silence.
    pub fn reset(&mut self) {
        self.grains.clear();
        self.fading.clear();
        self.fade_age = 0;
        self.emitters = [Emitter::default(), Emitter::default()];
        self.playheads = [0.0; 2];
        self.pos_phase = [f64::NAN; 2];
        self.pos_vel = [0.0; 2];
        self.spectral.reset();
    }

    /// Reset for voice reuse, but let any live grains ring out under a short
    /// fade instead of a hard clear — removes the click when a still-sounding
    /// voice is stolen. Emitters/playheads reset immediately so the new note
    /// starts clean; the leftover grains fade in `fading` over `STEAL_FADE`.
    pub fn soft_reset(&mut self) {
        if !self.grains.is_empty() {
            // Move the live grains into the fade pool (discarding any older
            // in-progress tail — it is older and quieter) and restart the fade.
            core::mem::swap(&mut self.grains, &mut self.fading);
            self.grains.clear();
            self.fade_age = 0;
        }
        self.emitters = [Emitter::default(), Emitter::default()];
        self.playheads = [0.0; 2];
        self.pos_phase = [f64::NAN; 2];
        self.pos_vel = [0.0; 2];
        self.spectral.reset();
    }

    /// Render (and advance) the steal fade-out tail. Runs before the main path
    /// so it plays out and clears regardless of the new note's mode.
    fn render_fading(&mut self, bank: &SampleBank, out_l: &mut [f32], out_r: &mut [f32], block: usize) {
        if self.fading.is_empty() {
            self.fade_age = 0;
            return;
        }
        for g in self.fading.iter_mut() {
            let slot = match bank.slots[g.snap.slot].as_ref() {
                Some(s) => s,
                None => {
                    g.age = g.snap.length;
                    continue;
                }
            };
            let data = &slot.data[g.snap.channel as usize];
            let own_gain = (g.snap.pan * core::f32::consts::FRAC_PI_2).cos();
            let swap_gain = (g.snap.pan * core::f32::consts::FRAC_PI_2).sin();
            let (peak, exponent) = grain_env_coeffs(g.snap.shape, g.snap.skew);
            let mut n = 0usize;
            if g.delay as usize >= block {
                g.delay -= block as u32;
                continue;
            }
            n += g.delay as usize;
            g.delay = 0;
            while n < block && g.age < g.snap.length {
                // Linear steal-fade: 1 → 0 over STEAL_FADE samples.
                let fade = 1.0 - (self.fade_age + n as u32) as f32 / STEAL_FADE as f32;
                if fade <= 0.0 {
                    break;
                }
                let t = g.age as f32 / g.snap.length as f32;
                let env = grain_envelope_at(t, peak, exponent);
                let s = read(data, g.pos, g.snap.wstart, g.snap.wlen, g.snap.algo)
                    * env
                    * g.snap.level
                    * fade;
                if g.snap.channel == 0 {
                    out_l[n] += s * own_gain;
                    out_r[n] += s * swap_gain;
                } else {
                    out_r[n] += s * own_gain;
                    out_l[n] += s * swap_gain;
                }
                g.pos += g.snap.increment;
                g.age += 1;
                n += 1;
            }
        }
        self.fading.retain(|g| g.age < g.snap.length);
        self.fade_age = self.fade_age.saturating_add(block as u32);
        if self.fade_age >= STEAL_FADE {
            self.fading.clear();
            self.fade_age = 0;
        }
    }

    /// Render `block` samples additively into `out_l` / `out_r`.
    ///
    /// `p` is one resolved parameter set **per channel** — the matrix
    /// modulates L and R independently (Torsion stereo modulation), so the
    /// two emitters can run at different rates, pitches, positions…
    /// `scan` is the engine's Loop LFO output per channel, -1..1.
    /// `gate` already folds in engine Gate ∧ global On ∧ note gate.
    // Eight params is over clippy's default, but each is a distinct, meaningful
    // input to the render; bundling them into a struct would not aid clarity.
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        p: &[LaneParams; 2],
        bank: &SampleBank,
        scan: (f32, f32),
        sample_rate: f32,
        out_l: &mut [f32],
        out_r: &mut [f32],
        block: usize,
    ) {
        // Ring out any stolen-voice fade tail first, so it declicks the steal
        // regardless of the new note's mode, mixing additively under the note.
        self.render_fading(bank, out_l, out_r, block);

        // Spectral mode bypasses the grain pool entirely (Mode is not matrix-
        // modulated, so both channels agree). Drop any grains left over from a
        // previous mode, then run the phase vocoder.
        if p[0].mode == GrainMode::Spectral {
            self.grains.clear();
            self.spectral
                .process(p, bank, sample_rate, out_l, out_r, block);
            return;
        }

        // 0. Pos>Pitch: track the scan-position velocity per channel (source
        //    samples per output sample) so spawn can drive grain read speed from
        //    it (tape/vinyl drag). Tracked every chunk so a toggle doesn't spike.
        for (ch, pc) in p.iter().enumerate() {
            self.pos_vel[ch] = match resolve_window(pc, bank) {
                Some((_, _, wlen)) => {
                    let cur = scan_offset(pc, scan, ch as u8) as f64 + pc.position as f64;
                    let vel = if self.pos_phase[ch].is_nan() {
                        0.0 // first chunk after reset: no previous phase
                    } else {
                        (cur - self.pos_phase[ch]) * wlen as f64 / block.max(1) as f64
                    };
                    self.pos_phase[ch] = cur;
                    vel.clamp(-32.0, 32.0)
                }
                None => 0.0,
            };
        }

        // 1. Collect emissions for this block (no spawning inside the
        //    closure — it holds &mut rng and &mut emitter).
        self.scratch.clear();
        {
            let Lane { emitters, scratch, rng, .. } = self;
            for (ch, emitter) in emitters.iter_mut().enumerate() {
                let pc = &p[ch];
                if pc.gate && pc.rate_hz > 0.0 {
                    // Cloud mode reinterprets Jitter as position spray (applied
                    // at spawn), so its emission clock stays regular.
                    let timing_jitter = if pc.mode == GrainMode::Cloud { 0.0 } else { pc.jitter };
                    emitter.advance(block, pc.rate_hz, timing_jitter, sample_rate, rng, |off| {
                        scratch.push((ch as u8, off));
                    });
                }
            }
        }

        // 2. Spawn snapshots from the matching channel's params.
        for i in 0..self.scratch.len() {
            let (ch, off) = self.scratch[i];
            self.spawn(&p[ch as usize], bank, scan, sample_rate, ch, off);
        }

        // 3. Render all live grains.
        for g in self.grains.iter_mut() {
            let slot = match bank.slots[g.snap.slot].as_ref() {
                Some(s) => s,
                None => {
                    g.age = g.snap.length; // slot vanished: kill silently
                    continue;
                }
            };
            let data = &slot.data[g.snap.channel as usize];
            let own_gain = (g.snap.pan * core::f32::consts::FRAC_PI_2).cos();
            let swap_gain = (g.snap.pan * core::f32::consts::FRAC_PI_2).sin();
            // Shape/Skew are frozen per grain — derive the envelope coefficients
            // once here, not per sample.
            let (peak, exponent) = grain_env_coeffs(g.snap.shape, g.snap.skew);
            let mut n = 0usize;
            if g.delay as usize >= block {
                g.delay -= block as u32;
                continue;
            }
            n += g.delay as usize;
            g.delay = 0;
            while n < block && g.age < g.snap.length {
                let t = g.age as f32 / g.snap.length as f32;
                let env = grain_envelope_at(t, peak, exponent);
                let s = read(data, g.pos, g.snap.wstart, g.snap.wlen, g.snap.algo)
                    * env
                    * g.snap.level;
                if g.snap.channel == 0 {
                    out_l[n] += s * own_gain;
                    out_r[n] += s * swap_gain;
                } else {
                    out_r[n] += s * own_gain;
                    out_l[n] += s * swap_gain;
                }
                g.pos += g.snap.increment;
                g.age += 1;
                n += 1;
            }
        }

        // 4. Reap finished grains (no allocation).
        self.grains.retain(|g| g.age < g.snap.length);

        // 5. Advance Stretch-mode playheads one block (normalized window
        //    phase), per channel at that channel's Direction.
        for (ch, pp) in p.iter().enumerate() {
            if pp.mode == GrainMode::Stretch {
                if let Some((_, _, wlen)) = resolve_window(pp, bank) {
                    let adv = (pp.direction.clamp(-1.0, 1.0) as f64 * block as f64) / wlen as f64;
                    self.playheads[ch] = (self.playheads[ch] + adv).rem_euclid(1.0);
                }
            }
        }
    }

    fn spawn(
        &mut self,
        p: &LaneParams,
        bank: &SampleBank,
        scan: (f32, f32),
        sample_rate: f32,
        channel: u8,
        offset: usize,
    ) {
        let (slot_idx, wstart, wlen) = match resolve_window(p, bank) {
            Some(w) => w,
            None => return,
        };
        let slot = bank.slots[slot_idx].as_ref().unwrap();

        let pitch = quantize_semitones(
            p.pitch_st + p.key_track * (p.note - slot.root_note as f32),
            p.quant,
        );
        let pitch_ratio = 2.0f64.powf(pitch as f64 / 12.0);
        let dir = p.direction.clamp(-1.0, 1.0) as f64;
        // Pos>Pitch (tape/vinyl drag): grain read speed follows the scan-position
        // velocity instead of Direction; Pitch still transposes via pitch_ratio.
        let speed = if p.pos_pitch { self.pos_vel[channel as usize] } else { dir };

        // Per-mode read start (absolute samples) and per-output-sample increment.
        let (start, increment) = match p.mode {
            // Spectral never reaches spawn (handled before the grain path);
            // fold it in as a harmless fallback to keep the match exhaustive.
            GrainMode::Granular | GrainMode::Spectral => {
                // Loop-LFO scan (when looping) + Position; speed = Direction × pitch.
                let frac = (scan_offset(p, scan, channel) + p.position).rem_euclid(1.0);
                (wstart as f64 + frac as f64 * wlen as f64, speed * pitch_ratio)
            }
            GrainMode::Stretch => {
                // Playhead (normalized window phase) sampled at this emission's
                // intra-block offset; the grain reads forward at the pitch ratio
                // only, so scrub speed (Direction) and pitch stay independent.
                let ph = self.playheads[channel as usize] + (dir * offset as f64) / wlen as f64;
                let frac = (ph + p.position as f64).rem_euclid(1.0);
                (wstart as f64 + frac * wlen as f64, pitch_ratio)
            }
            GrainMode::Cloud => {
                // Position spray (Jitter reinterpreted) decorrelates grains into
                // a smear; the Loop-LFO scan still offsets the spray centre.
                let spray = self.rng.bipolar() * p.jitter.clamp(0.0, 1.0);
                let frac = (scan_offset(p, scan, channel) + p.position + spray).rem_euclid(1.0);
                (wstart as f64 + frac as f64 * wlen as f64, speed * pitch_ratio)
            }
        };

        let length_samples = if p.linked {
            // 100 % linked = 2× the emission period: Hann windows at 50 %
            // overlap sum to exactly 1 — gapless by construction.
            (p.length_val / 100.0) * 2.0 * (sample_rate / p.rate_hz.max(0.01))
        } else {
            p.length_val / 1000.0 * sample_rate
        };
        let length = (length_samples.max(16.0).min(sample_rate * 20.0)) as u32;

        let snap = GrainSnapshot {
            slot: slot_idx,
            wstart,
            wlen,
            algo: p.algo,
            start,
            increment,
            length,
            pan: p.pan.clamp(0.0, 1.0),
            level: p.level.clamp(0.0, 1.0) * slot.gain,
            shape: p.shape,
            skew: p.skew,
            channel,
        };
        let grain = ActiveGrain { snap, pos: start, age: 0, delay: offset as u32 };

        if self.grains.len() < MAX_GRAINS {
            self.grains.push(grain);
        } else {
            // Steal the oldest.
            let mut oldest = 0;
            let mut best = 0u32;
            for (i, g) in self.grains.iter().enumerate() {
                if g.age >= best {
                    best = g.age;
                    oldest = i;
                }
            }
            self.grains[oldest] = grain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::SampleSlot;

    fn run_lane(p: &LaneParams, bank: &SampleBank, seconds: f32, sr: f32) -> (Vec<f32>, Vec<f32>) {
        let total = (seconds * sr) as usize;
        let mut lane = Lane::new(0xBADC0DE);
        let mut l = vec![0.0f32; total];
        let mut r = vec![0.0f32; total];
        let pp = [*p, *p];
        let mut done = 0;
        while done < total {
            let block = (total - done).min(512);
            let (bl, br) = (&mut l[done..done + block], &mut r[done..done + block]);
            lane.process(&pp, bank, (0.0, 0.0), sr, bl, br, block);
            done += block;
        }
        (l, r)
    }

    /// The init-patch promise: Rate>Len at 100 % over a constant source is
    /// *exactly* gapless (Hann COLA at 50 % overlap).
    #[test]
    fn cola_gapless_at_linked_100_percent() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot::constant(1.0, 96_000, sr));
        let p = LaneParams { loop_on: false, ..Default::default() };
        let (l, r) = run_lane(&p, &bank, 1.0, sr);
        // Skip warm-up (first grain pair), then demand flatness.
        for n in 14_400..48_000 {
            assert!(
                (l[n] - 1.0).abs() < 1e-3,
                "L not gapless at {n}: {}",
                l[n]
            );
            assert!((r[n] - 1.0).abs() < 1e-3, "R not gapless at {n}: {}", r[n]);
        }
    }

    #[test]
    fn produces_audio_and_stays_finite() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot::test_tone(sr));
        let p = LaneParams {
            rate_hz: 25.0,
            pitch_st: 7.0,
            jitter: 0.4,
            shape: 0.3,
            algo: Algo::Hermite,
            ..Default::default()
        };
        let (l, r) = run_lane(&p, &bank, 1.0, sr);
        let rms = (l.iter().map(|v| (v * v) as f64).sum::<f64>() / l.len() as f64).sqrt();
        assert!(rms > 0.05, "rms too low: {rms}");
        for v in l.iter().chain(r.iter()) {
            assert!(v.is_finite());
        }
    }

    #[test]
    fn algo_choice_changes_the_output() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot::test_tone(sr));
        let base = LaneParams { pitch_st: 5.3, jitter: 0.0, loop_on: false, ..Default::default() };
        let near = run_lane(&LaneParams { algo: Algo::Nearest, ..base }, &bank, 0.5, sr).0;
        let sinc = run_lane(&LaneParams { algo: Algo::Sinc, ..base }, &bank, 0.5, sr).0;
        let diff: f64 = near
            .iter()
            .zip(sinc.iter())
            .map(|(a, b)| (a - b).abs() as f64)
            .sum();
        assert!(diff > 1.0, "Nearest and Sinc-8 outputs identical (diff {diff})");
    }

    #[test]
    fn pan_crossfades_channels() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        // L channel loud, R silent — pan 1.0 must swap them.
        let slot = SampleSlot {
            data: [vec![1.0; 96_000], vec![0.0; 96_000]],
            sample_rate: sr,
            root_note: 60,
            loop_start: 0,
            loop_end: 96_000,
            gain: 1.0,
            mono: false,
        };
        bank.slots[0] = Some(slot);
        let p = LaneParams { pan: 1.0, loop_on: false, ..Default::default() };
        let (l, r) = run_lane(&p, &bank, 0.5, sr);
        let e_l: f64 = l.iter().map(|v| (v * v) as f64).sum();
        let e_r: f64 = r.iter().map(|v| (v * v) as f64).sum();
        assert!(e_r > 10.0 * e_l.max(1e-12), "pan swap failed: L {e_l}, R {e_r}");
    }

    #[test]
    fn grain_pool_never_exceeds_cap() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot::test_tone(sr));
        // Absurd density: 500 Hz × 2 channels × 1000 ms free-running grains
        // → ~1000 nominal concurrent, must clamp at the pool cap.
        let p = LaneParams { rate_hz: 500.0, linked: false, length_val: 1000.0, ..Default::default() };
        let pp = [p, p];
        let mut lane = Lane::new(7);
        let mut l = vec![0.0f32; 512];
        let mut r = vec![0.0f32; 512];
        for _ in 0..200 {
            l.iter_mut().for_each(|v| *v = 0.0);
            r.iter_mut().for_each(|v| *v = 0.0);
            lane.process(&pp, &bank, (0.0, 0.0), sr, &mut l, &mut r, 512);
            assert!(lane.active_grains() <= MAX_GRAINS);
            for v in l.iter() {
                assert!(v.is_finite());
            }
        }
        assert_eq!(lane.active_grains(), MAX_GRAINS);
    }

    #[test]
    fn per_channel_params_are_independent() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot::test_tone(sr));
        // L emits, R is rate-gated off — only the left stream may sound
        // (pan 0 keeps each grain on its own channel).
        let base = LaneParams { loop_on: false, ..Default::default() };
        let p = [base, LaneParams { rate_hz: 0.0, ..base }];
        let mut lane = Lane::new(3);
        let total = 24_000usize;
        let mut l = vec![0.0f32; total];
        let mut r = vec![0.0f32; total];
        let mut done = 0;
        while done < total {
            let block = (total - done).min(512);
            lane.process(&p, &bank, (0.0, 0.0), sr, &mut l[done..done + block], &mut r[done..done + block], block);
            done += block;
        }
        let e = |v: &Vec<f32>| v.iter().map(|x| (x * x) as f64).sum::<f64>();
        assert!(e(&l) > 1.0, "left silent");
        assert!(e(&r) < 1e-9, "right should be silent, energy {}", e(&r));
    }

    #[test]
    fn loop_pos_shifts_the_read_window() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        let n = 96_000;
        let slot = SampleSlot {
            data: [
                (0..n).map(|i| (i as f32 / n as f32) * 2.0 - 1.0).collect(),
                vec![0.0; n],
            ],
            sample_rate: sr,
            root_note: 60,
            loop_start: 0,
            loop_end: n / 2, // window = first half
            gain: 1.0,
            mono: false,
        };
        bank.slots[0] = Some(slot);
        let base = LaneParams {
            direction: 0.0, // freeze: output = window-start ramp value
            position: 0.0,
            loop_on: false,
            shape: 1.0,
            rate_hz: 2.0,
            linked: false,
            length_val: 100.0,
            ..Default::default()
        };
        let frozen = |loop_pos: f32| -> f32 {
            let p = LaneParams { loop_pos, ..base };
            let (l, _) = run_lane(&p, &bank, 0.3, sr);
            let vals: Vec<f32> = l.iter().copied().filter(|v| v.abs() > 0.05).collect();
            assert!(!vals.is_empty());
            vals.iter().sum::<f32>() / vals.len() as f32
        };
        let unshifted = frozen(0.0); // window start 0 → ramp value -1
        let shifted = frozen(0.5); // shifted half a window (24000) → value -0.5
        assert!((unshifted + 1.0).abs() < 0.05, "unshifted {unshifted}");
        assert!((shifted + 0.5).abs() < 0.05, "shifted {shifted}");
    }

    #[test]
    fn direction_zero_freezes() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        // Ramp data: a frozen grain reads a constant, a moving one doesn't.
        let n = 96_000;
        let slot = SampleSlot {
            data: [
                (0..n).map(|i| (i as f32 / n as f32) * 2.0 - 1.0).collect(),
                vec![0.0; n],
            ],
            sample_rate: sr,
            root_note: 60,
            loop_start: 0,
            loop_end: n,
            gain: 1.0,
            mono: false,
        };
        bank.slots[0] = Some(slot);
        let p = LaneParams {
            direction: 0.0,
            position: 0.25, // ramp value here is exactly -0.5
            loop_on: false,
            shape: 1.0,      // rect-ish window: output ≈ raw read
            rate_hz: 2.0,    // sparse, non-overlapping grains
            linked: false,
            length_val: 100.0,
            ..Default::default()
        };
        let (l, _) = run_lane(&p, &bank, 0.5, sr);
        // Every clearly-audible sample must sit on the frozen read value.
        let vals: Vec<f32> = l.iter().copied().filter(|v| v.abs() > 0.1).collect();
        assert!(vals.len() > 1_000, "freeze produced almost no audio");
        for v in &vals {
            assert!((v + 0.5).abs() < 0.02, "freeze drifted: {v}");
        }
    }

    /// A full-length linear ramp from -1..+1: a sample's output value reveals
    /// where in the source it was read, which makes mode behavior testable.
    fn ramp_bank(n: usize, sr: f32) -> SampleBank {
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot {
            data: [(0..n).map(|i| (i as f32 / n as f32) * 2.0 - 1.0).collect(), vec![0.0; n]],
            sample_rate: sr,
            root_note: 60,
            loop_start: 0,
            loop_end: n,
            gain: 1.0,
            mono: false,
        });
        bank
    }

    #[test]
    fn modes_render_differently() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot::test_tone(sr));
        let base = LaneParams { rate_hz: 30.0, loop_on: false, ..Default::default() };
        let gran = run_lane(&LaneParams { mode: GrainMode::Granular, ..base }, &bank, 0.5, sr).0;
        let stretch = run_lane(&LaneParams { mode: GrainMode::Stretch, ..base }, &bank, 0.5, sr).0;
        let cloud =
            run_lane(&LaneParams { mode: GrainMode::Cloud, jitter: 0.8, ..base }, &bank, 0.5, sr).0;
        let diff = |a: &[f32], b: &[f32]| {
            a.iter().zip(b).map(|(x, y)| (x - y).abs() as f64).sum::<f64>()
        };
        assert!(diff(&gran, &stretch) > 1.0, "Granular vs Stretch identical");
        assert!(diff(&gran, &cloud) > 1.0, "Granular vs Cloud identical");
    }

    #[test]
    fn stretch_reads_forward_while_scrub_frozen() {
        let sr = 48_000.0;
        let bank = ramp_bank(96_000, sr);
        // Direction 0 freezes the playhead; pitch 0 → read increment 1, so the
        // grain still reads *forward* through the ramp (a moving freeze).
        let p = LaneParams {
            mode: GrainMode::Stretch,
            direction: 0.0,
            pitch_st: 0.0,
            position: 0.0,
            loop_on: false,
            shape: 1.0,
            rate_hz: 4.0,
            linked: false,
            length_val: 200.0, // 200 ms grains, non-overlapping
            ..Default::default()
        };
        let (l, _) = run_lane(&p, &bank, 0.5, sr);
        let span = l.iter().cloned().fold(f32::MIN, f32::max)
            - l.iter().cloned().fold(f32::MAX, f32::min);
        assert!(span > 0.1, "Stretch grain did not read forward: span {span}");

        // Granular in the same setup freezes on one sample (≈ flat output).
        let pg = LaneParams { mode: GrainMode::Granular, ..p };
        let (lg, _) = run_lane(&pg, &bank, 0.5, sr);
        let gvals: Vec<f32> = lg.iter().copied().filter(|v| v.abs() > 0.01).collect();
        let gspan = gvals.iter().cloned().fold(f32::MIN, f32::max)
            - gvals.iter().cloned().fold(f32::MAX, f32::min);
        assert!(gspan < 0.05, "Granular freeze should sit on one value: span {gspan}");
    }

    #[test]
    fn stretch_playhead_advances_through_sample() {
        let sr = 48_000.0;
        let bank = ramp_bank(96_000, sr); // 2 s ramp
        // Direction 1 (1× scrub), pitch 0: the playhead sweeps the window, so
        // the read position (ramp value) climbs over time.
        let p = LaneParams {
            mode: GrainMode::Stretch,
            direction: 1.0,
            pitch_st: 0.0,
            position: 0.0,
            loop_on: false,
            shape: 1.0,
            rate_hz: 20.0,
            linked: true, // overlapping → near-continuous output
            length_val: 100.0,
            ..Default::default()
        };
        let (l, _) = run_lane(&p, &bank, 1.0, sr);
        let half = l.len() / 2;
        let mean = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
        let m1 = mean(&l[1000..half]); // skip warm-up
        let m2 = mean(&l[half..]);
        assert!(m2 > m1 + 0.2, "Stretch playhead did not advance: {m1} -> {m2}");
    }

    #[test]
    fn cloud_spray_widens_read_distribution() {
        let sr = 48_000.0;
        let bank = ramp_bank(96_000, sr);
        // Frozen read (Direction 0) so each grain sits on its sprayed start;
        // the output value then reports where in the window it landed.
        let base = LaneParams {
            mode: GrainMode::Cloud,
            direction: 0.0,
            pitch_st: 0.0,
            position: 0.5, // ramp value 0 at the spray centre
            loop_on: false,
            shape: 1.0,
            rate_hz: 50.0,
            linked: false,
            length_val: 5.0,
            ..Default::default()
        };
        let span = |s: &[f32]| {
            s.iter().cloned().fold(f32::MIN, f32::max) - s.iter().cloned().fold(f32::MAX, f32::min)
        };
        let narrow = run_lane(&LaneParams { jitter: 0.0, ..base }, &bank, 0.5, sr).0;
        let wide = run_lane(&LaneParams { jitter: 1.0, ..base }, &bank, 0.5, sr).0;
        assert!(
            span(&wide) > span(&narrow) + 0.5,
            "Cloud spray did not widen the read distribution: narrow {} wide {}",
            span(&narrow),
            span(&wide)
        );
    }

    // ---- Spectral (phase vocoder) integration ----

    fn sine_bank(freq: f32, n: usize, sr: f32) -> SampleBank {
        let data: Vec<f32> =
            (0..n).map(|i| (core::f32::consts::TAU * freq * i as f32 / sr).sin()).collect();
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot {
            data: [data.clone(), data],
            sample_rate: sr,
            root_note: 60,
            loop_start: 0,
            loop_end: n,
            gain: 1.0,
            mono: true,
        });
        bank
    }

    /// Dominant frequency (Hz) of the steady tail of a signal, via a 4096-pt
    /// FFT of the last window — used to verify the vocoder's pitch behavior.
    fn dominant_freq(sig: &[f32], sr: f32) -> f32 {
        const N: usize = 4096;
        let f = crate::fft::Fft::new(N);
        let start = sig.len().saturating_sub(N);
        let mut re = vec![0.0f32; N];
        let mut im = vec![0.0f32; N];
        for i in 0..N {
            re[i] = sig[start + i];
        }
        f.transform(&mut re, &mut im, false);
        let mut peak = 1usize;
        let mut best = 0.0f32;
        for k in 1..N / 2 {
            let m = re[k] * re[k] + im[k] * im[k];
            if m > best {
                best = m;
                peak = k;
            }
        }
        peak as f32 * sr / N as f32
    }

    fn rms(s: &[f32]) -> f64 {
        (s.iter().map(|v| (v * v) as f64).sum::<f64>() / s.len() as f64).sqrt()
    }

    #[test]
    fn spectral_passthrough_preserves_pitch() {
        let sr = 48_000.0;
        let freq = 1000.0;
        let bank = sine_bank(freq, 48_000, sr);
        let p = LaneParams {
            mode: GrainMode::Spectral,
            direction: 1.0,
            pitch_st: 0.0,
            loop_on: false,
            ..Default::default()
        };
        let (l, _) = run_lane(&p, &bank, 1.0, sr);
        for v in &l {
            assert!(v.is_finite());
        }
        assert!(rms(&l) > 0.05, "spectral passthrough too quiet: rms {}", rms(&l));
        let f = dominant_freq(&l, sr);
        assert!((f - freq).abs() < 60.0, "spectral passthrough freq {f}, expected {freq}");
    }

    #[test]
    fn spectral_freeze_sustains_past_the_source() {
        let sr = 48_000.0;
        let freq = 800.0;
        // 0.1 s source; a spectral freeze should hold a full second.
        let bank = sine_bank(freq, (sr * 0.1) as usize, sr);
        let p = LaneParams {
            mode: GrainMode::Spectral,
            direction: 0.0, // freeze
            pitch_st: 0.0,
            loop_on: false,
            ..Default::default()
        };
        let (l, _) = run_lane(&p, &bank, 1.0, sr);
        for v in &l {
            assert!(v.is_finite());
        }
        let tail = &l[(sr as usize) * 3 / 4..]; // last 0.25 s
        assert!(rms(tail) > 0.05, "spectral freeze did not sustain: tail rms {}", rms(tail));
        let f = dominant_freq(&l, sr);
        assert!((f - freq).abs() < 60.0, "freeze pitch drifted: {f} vs {freq}");
    }

    #[test]
    fn spectral_pitch_shifts_frequency() {
        let sr = 48_000.0;
        let freq = 500.0;
        let bank = sine_bank(freq, 48_000, sr);
        let up = LaneParams {
            mode: GrainMode::Spectral,
            direction: 1.0,
            pitch_st: 12.0, // +1 octave → ~2× freq
            loop_on: false,
            ..Default::default()
        };
        let down = LaneParams { pitch_st: -12.0, ..up }; // −1 octave → ~½ freq
        let fu = dominant_freq(&run_lane(&up, &bank, 1.0, sr).0, sr);
        let fd = dominant_freq(&run_lane(&down, &bank, 1.0, sr).0, sr);
        assert!((fu - 1000.0).abs() < 80.0, "pitch +12 → {fu} Hz, expected ~1000");
        assert!((fd - 250.0).abs() < 50.0, "pitch −12 → {fd} Hz, expected ~250");
    }

    #[test]
    fn spectral_rings_out_after_release() {
        let sr = 48_000.0;
        let bank = sine_bank(800.0, 48_000, sr);
        let mut lane = Lane::new(1);
        let held = LaneParams {
            mode: GrainMode::Spectral,
            direction: 1.0,
            loop_on: false,
            ..Default::default()
        };
        let released = LaneParams { gate: false, ..held };
        let mut l = vec![0.0f32; 512];
        let mut r = vec![0.0f32; 512];
        let mut run = |lane: &mut Lane, p: &LaneParams| {
            l.iter_mut().for_each(|v| *v = 0.0);
            r.iter_mut().for_each(|v| *v = 0.0);
            lane.process(&[*p, *p], &bank, (0.0, 0.0), sr, &mut l, &mut r, 512);
            rms(&l)
        };
        // Hold: prime the vocoder for several blocks.
        for _ in 0..40 {
            run(&mut lane, &held);
        }
        assert!(lane.is_ringing(), "should be ringing while gated");
        // Release: the OLA tail still sounds on the next block...
        assert!(run(&mut lane, &released) > 0.01, "release tail should still sound");
        // ...then rings out fully (no abrupt cut, eventually silent).
        for _ in 0..10 {
            run(&mut lane, &released);
        }
        assert!(!lane.is_ringing(), "should have rung out after release");
        assert!(run(&mut lane, &released) < 1e-6, "should be silent after ring-out");
    }

    /// Harmonics of `f0` shaped by a fixed Gaussian formant — the spectral
    /// envelope peaks near `formant` regardless of pitch.
    fn formant_bank(f0: f32, formant: f32, n: usize, sr: f32) -> SampleBank {
        let mut data = vec![0.0f32; n];
        for h in 1..40 {
            let fh = f0 * h as f32;
            if fh > sr * 0.45 {
                break;
            }
            let amp = (-(((fh - formant) / 800.0).powi(2))).exp();
            for (i, d) in data.iter_mut().enumerate() {
                *d += amp * (core::f32::consts::TAU * fh * i as f32 / sr).sin();
            }
        }
        let peak = data.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-9);
        data.iter_mut().for_each(|v| *v *= 0.8 / peak);
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot {
            data: [data.clone(), data],
            sample_rate: sr,
            root_note: 60,
            loop_start: 0,
            loop_end: n,
            gain: 1.0,
            mono: true,
        });
        bank
    }

    fn spectral_centroid(sig: &[f32], sr: f32) -> f32 {
        const N: usize = 4096;
        let f = crate::fft::Fft::new(N);
        let start = sig.len().saturating_sub(N);
        let mut re = vec![0.0f32; N];
        let mut im = vec![0.0f32; N];
        for i in 0..N {
            re[i] = sig[start + i];
        }
        f.transform(&mut re, &mut im, false);
        let (mut num, mut den) = (0.0f64, 0.0f64);
        for k in 1..N / 2 {
            let m = (re[k] * re[k] + im[k] * im[k]).sqrt() as f64;
            num += k as f64 * m;
            den += m;
        }
        (num / den.max(1e-12)) as f32 * sr / N as f32
    }

    #[test]
    fn spectral_formant_preserve_holds_envelope() {
        let sr = 48_000.0;
        let bank = formant_bank(150.0, 2000.0, 48_000, sr);
        let classic = LaneParams {
            mode: GrainMode::Spectral,
            direction: 1.0,
            pitch_st: 12.0, // +1 octave
            shape: 0.0,     // formant-shifting (classic)
            loop_on: false,
            ..Default::default()
        };
        let preserved = LaneParams { shape: 1.0, ..classic }; // formant-preserving
        let c_classic = spectral_centroid(&run_lane(&classic, &bank, 1.0, sr).0, sr);
        let c_preserve = spectral_centroid(&run_lane(&preserved, &bank, 1.0, sr).0, sr);
        // Classic shift drags the spectral centre up with the pitch; preserving
        // holds it near the original formant, so it must be clearly lower.
        assert!(
            c_preserve < c_classic * 0.85,
            "formant not preserved: classic {c_classic} Hz, preserved {c_preserve} Hz"
        );
    }

    #[test]
    fn spectral_stereo_channels_are_independent() {
        // A genuinely stereo source (mono:false) must take the two-vocoder path:
        // left carries 400 Hz, right 900 Hz, and they must not bleed together.
        let sr = 48_000.0;
        let n = 48_000;
        let dl: Vec<f32> =
            (0..n).map(|i| (core::f32::consts::TAU * 400.0 * i as f32 / sr).sin()).collect();
        let dr: Vec<f32> =
            (0..n).map(|i| (core::f32::consts::TAU * 900.0 * i as f32 / sr).sin()).collect();
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot {
            data: [dl, dr],
            sample_rate: sr,
            root_note: 60,
            loop_start: 0,
            loop_end: n,
            gain: 1.0,
            mono: false,
        });
        let p = LaneParams {
            mode: GrainMode::Spectral,
            direction: 1.0,
            loop_on: false,
            ..Default::default()
        };
        let (l, r) = run_lane(&p, &bank, 1.0, sr);
        let fl = dominant_freq(&l, sr);
        let fr = dominant_freq(&r, sr);
        assert!((fl - 400.0).abs() < 60.0, "left channel freq {fl} Hz, expected ~400");
        assert!((fr - 900.0).abs() < 60.0, "right channel freq {fr} Hz, expected ~900");
    }

    #[test]
    fn spectral_blur_smears_without_shifting_pitch() {
        let sr = 48_000.0;
        let bank = sine_bank(1000.0, 48_000, sr);
        let clean = LaneParams {
            mode: GrainMode::Spectral,
            direction: 1.0,
            skew: 0.0, // no blur
            loop_on: false,
            ..Default::default()
        };
        let blurred = LaneParams { skew: 1.0, ..clean }; // Skew = full spectral blur
        let lc = run_lane(&clean, &bank, 1.0, sr).0;
        let lb = run_lane(&blurred, &bank, 1.0, sr).0;
        // Blur randomizes phase only, so the dominant pitch is preserved...
        let fb = dominant_freq(&lb, sr);
        assert!((fb - 1000.0).abs() < 80.0, "blur shifted pitch to {fb} Hz");
        // ...but the time-domain signal is clearly diffused vs the clean one.
        let diff: f64 = lc.iter().zip(&lb).map(|(a, b)| (a - b).abs() as f64).sum();
        assert!(diff > 1.0, "blur had no audible effect (diff {diff})");
    }

    #[test]
    fn spectral_onset_is_immediate() {
        // Priming pre-fills the overlap-add buffer, so the very first output
        // block is already at full amplitude (zero onset latency) rather than
        // ramping in over a frame.
        let sr = 48_000.0;
        let bank = sine_bank(1000.0, 48_000, sr);
        let p = LaneParams {
            mode: GrainMode::Spectral,
            direction: 1.0,
            loop_on: false,
            ..Default::default()
        };
        let (l, _) = run_lane(&p, &bank, 0.05, sr);
        let early = rms(&l[0..256]); // first hop
        let later = rms(&l[1024..1280]); // steady state
        assert!(early > 0.5 * later, "onset ramped, not primed: early {early}, later {later}");
    }

    #[test]
    fn pos_pitch_drives_pitch_from_scan_velocity() {
        let sr = 48_000.0;
        let n_samples = 96_000usize;
        let bank = sine_bank(1000.0, n_samples, sr); // 1 kHz over the full window
        let wlen = n_samples as f32;
        // Direction 0 would freeze the read; Pos>Pitch instead drives read speed
        // from the scan velocity. Sweep the scan so velocity = 2.0 source-samples
        // per output-sample → the 1 kHz sine reads at 2× → ~2 kHz.
        let p = LaneParams {
            mode: GrainMode::Granular,
            pos_pitch: true,
            direction: 0.0,
            pitch_st: 0.0,
            loop_on: true,
            rate_hz: 80.0,
            linked: true,
            length_val: 100.0,
            ..Default::default()
        };
        let mut lane = Lane::new(11);
        let total = 48_000usize;
        let mut l = vec![0.0f32; total];
        let mut r = vec![0.0f32; total];
        let block = 512usize;
        let (mut done, mut s) = (0usize, 0.0f32);
        while done < total {
            let n = (total - done).min(block);
            lane.process(&[p, p], &bank, (s, s), sr, &mut l[done..done + n], &mut r[done..done + n], n);
            s += 4.0 * n as f32 / wlen; // holds scan velocity at 2.0 regardless of n
            done += n;
        }
        for v in &l {
            assert!(v.is_finite());
        }
        let f = dominant_freq(&l, sr);
        assert!((f - 2000.0).abs() < 150.0, "Pos>Pitch tape-drag freq {f} Hz, expected ~2000");
    }

    #[test]
    fn voice_steal_fades_instead_of_clicking() {
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        bank.slots[0] = Some(SampleSlot::test_tone(sr));
        let p = LaneParams {
            rate_hz: 200.0,
            linked: false,
            length_val: 200.0,
            loop_on: false,
            ..Default::default()
        };
        let pp = [p, p];
        let mut lane = Lane::new(5);
        let mut l = vec![0.0f32; 512];
        let mut r = vec![0.0f32; 512];
        // Build up active grains.
        for _ in 0..8 {
            l.iter_mut().for_each(|v| *v = 0.0);
            r.iter_mut().for_each(|v| *v = 0.0);
            lane.process(&pp, &bank, (0.0, 0.0), sr, &mut l, &mut r, 512);
        }
        assert!(lane.active_grains() > 0, "no grains to steal");

        // Steal: soft_reset moves live grains into the fade pool.
        lane.soft_reset();
        assert_eq!(lane.active_grains(), 0, "new note's pool must start empty");
        assert!(lane.is_ringing(), "steal tail should keep the lane ringing");

        // Render the fade with the gate OFF (no new grains): the tail must sound
        // rather than cutting to instant silence (the click this fix removes).
        let gate_off = LaneParams { rate_hz: 0.0, gate: false, ..p };
        let mut fl = vec![0.0f32; 256];
        let mut fr = vec![0.0f32; 256];
        lane.process(&[gate_off, gate_off], &bank, (0.0, 0.0), sr, &mut fl, &mut fr, 256);
        let tail: f64 = fl.iter().chain(fr.iter()).map(|v| (v * v) as f64).sum();
        assert!(tail > 0.0, "steal produced instant silence (click), not a fade");
        for v in fl.iter().chain(fr.iter()) {
            assert!(v.is_finite());
        }

        // The fade spans ~256 samples; a couple more blocks and it is fully gone.
        for _ in 0..3 {
            fl.iter_mut().for_each(|v| *v = 0.0);
            fr.iter_mut().for_each(|v| *v = 0.0);
            lane.process(&[gate_off, gate_off], &bank, (0.0, 0.0), sr, &mut fl, &mut fr, 256);
        }
        assert!(!lane.is_ringing(), "steal tail should have faded out");
    }

    #[test]
    fn pitch_quantize_snaps_to_scales() {
        // Off passes through; Chromatic rounds to the nearest semitone.
        assert_eq!(quantize_semitones(3.7, PitchQuant::Off), 3.7);
        assert_eq!(quantize_semitones(3.7, PitchQuant::Chromatic), 4.0);
        // Octaves snap to multiples of 12.
        assert_eq!(quantize_semitones(5.0, PitchQuant::Octave), 0.0);
        assert_eq!(quantize_semitones(7.0, PitchQuant::Octave), 12.0);
        // Major scale: 3 (D#) is off-scale → nearest is 2 (D) or 4 (E); tie → lower.
        assert_eq!(quantize_semitones(3.0, PitchQuant::Major), 2.0);
        assert_eq!(quantize_semitones(5.9, PitchQuant::Major), 5.0); // ~F stays F
        // Every quantized value must itself be on the scale.
        for q in [PitchQuant::Fifth, PitchQuant::Major, PitchQuant::Minor, PitchQuant::Penta, PitchQuant::MinorPenta] {
            for i in -240..=240 {
                let st = i as f32 / 10.0;
                let out = quantize_semitones(st, q);
                let pc = (out.round() as i32).rem_euclid(12);
                assert!(q.classes().contains(&pc), "{q:?}: {st} -> {out} (pc {pc} off-scale)");
                assert!((out - st).abs() <= 6.0, "{q:?}: snapped too far {st} -> {out}");
            }
        }
    }

    #[test]
    fn resolve_window_survives_degenerate_loop() {
        // Loop Start = Loop End = 100% resolves loop_start to len; the clamp in
        // resolve_window must not panic (regression: clamp(len + 1, len)).
        let sr = 48_000.0;
        let mut bank = SampleBank::new();
        let mut slot = SampleSlot::test_tone(sr);
        let len = slot.len();
        slot.loop_start = len; // degenerate: start at the very end
        slot.loop_end = len;
        bank.slots[0] = Some(slot);
        let p = LaneParams { loop_on: false, ..Default::default() };
        let got = resolve_window(&p, &bank);
        assert!(got.is_some(), "resolve_window returned None for a loaded slot");
        let (_, wstart, wlen) = got.unwrap();
        assert!(wstart + wlen <= len, "window {wstart}+{wlen} exceeds slot len {len}");
        assert!(wlen >= 1, "window length must be positive");
    }
}
