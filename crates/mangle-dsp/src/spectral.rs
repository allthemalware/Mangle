//! Spectral grain mode — a streaming phase vocoder (Tier-2 synthesis
//! paradigm). The signature "Clouds spectral" texture: clean time-stretch,
//! smooth spectral **freeze**, and frequency-domain pitch shift, all per
//! voice / per channel.
//!
//! Design:
//! * STFT with a Hann window, frame `FRAME`, hop `HOP` (75 % overlap).
//! * The analysis playhead walks the loop window at **Direction×** (so
//!   Direction 0 freezes the spectrum, decoupled from pitch; negative scrubs
//!   backward). `Position` offsets the playhead; fractional reads use the
//!   engine's per-grain `Algo` interpolation so big stretches stay clean.
//! * Instantaneous-frequency phase propagation (the standard estimate from
//!   the inter-frame phase delta) keeps partials coherent across frames.
//! * Pitch shift by ratio `r = 2^(semitones/12)` remaps analysis bin `k` to
//!   synthesis bin `round(k·r)` and scales its frequency by `r` (formant-
//!   shifting, like Clouds' default — formant preservation is a v2 refinement).
//!
//! Dependency-free: the FFT lives in `crate::fft`. All per-frame buffers are
//! pre-allocated (`SpectralState::new`, off the audio thread), so synthesis
//! never allocates.

use std::sync::OnceLock;

use crate::engine::{resolve_window, LaneParams};
use crate::fft::Fft;
use crate::rng::Rng;
use crate::sample::{read, SampleBank};

/// STFT frame size (power of two), ~21 ms at 48 kHz. The pipeline's group delay
/// would normally be plugin latency, but because Mangle reads a stored buffer
/// the vocoder is **primed** at note-on (see `Channel::prime`) so onset is
/// full-amplitude and zero-latency — nothing is reported to the host, so no
/// latency-change restarts.
pub const FRAME: usize = 1024;
/// Synthesis hop. 75 % overlap (FRAME/4) for clean Hann reconstruction.
pub const HOP: usize = FRAME / 4;
const NBINS: usize = FRAME / 2 + 1;

/// Frames of overlap-add overlap. Doubles as the post-note-off tail length (so
/// a released voice rings out smoothly) and the note-on priming pre-roll.
const TAIL_FRAMES: usize = FRAME / HOP;

fn hann() -> &'static [f32; FRAME] {
    static W: OnceLock<[f32; FRAME]> = OnceLock::new();
    W.get_or_init(|| {
        let mut w = [0.0f32; FRAME];
        for (n, wn) in w.iter_mut().enumerate() {
            *wn = 0.5 - 0.5 * (core::f32::consts::TAU * n as f32 / FRAME as f32).cos();
        }
        w
    })
}

fn plan() -> &'static Fft {
    static F: OnceLock<Fft> = OnceLock::new();
    F.get_or_init(|| Fft::new(FRAME))
}

/// Pre-build the shared FFT plan and window. Call from `initialize` so the
/// first spectral frame never lazily allocates on the audio thread.
pub fn warm() {
    let _ = hann();
    let _ = plan();
}

/// Principal argument: wrap a phase into (-π, π].
#[inline]
fn princarg(p: f32) -> f32 {
    let twopi = core::f32::consts::TAU;
    p - twopi * ((p + core::f32::consts::PI) / twopi).floor()
}

/// Per-channel vocoder state (the two stereo emitters diverge, as everywhere
/// else in the engine).
struct Channel {
    /// Analysis playhead, samples into the loop window (wrapped to its length).
    pos: f64,
    last_phase: Vec<f32>, // NBINS — previous frame's analysis phase
    sum_phase: Vec<f32>,  // NBINS — accumulated synthesis phase
    acc: Vec<f32>,        // FRAME — overlap-add accumulator
    pending: Vec<f32>,    // HOP   — finalized samples awaiting output
    pending_head: usize,  // index into `pending`; == HOP when drained
    /// Overlap-add tail frames still available to drain after note-off. Kept
    /// topped up to `TAIL_FRAMES` while gated; counts down once released.
    draining_frames: usize,
    /// Per-channel RNG for spectral blur (phase randomization). Seeded
    /// differently per channel so blur decorrelates L/R.
    rng: Rng,
    /// Whether the overlap-add buffer has been pre-rolled since the last reset
    /// (note-on). Priming gives a full-amplitude, zero-latency onset.
    primed: bool,
}

impl Channel {
    fn new(seed: u64) -> Self {
        Self {
            pos: 0.0,
            last_phase: vec![0.0; NBINS],
            sum_phase: vec![0.0; NBINS],
            acc: vec![0.0; FRAME],
            pending: vec![0.0; HOP],
            pending_head: HOP,
            draining_frames: 0,
            rng: Rng::new(seed),
            primed: false,
        }
    }

    fn reset(&mut self) {
        self.pos = 0.0;
        self.last_phase.iter_mut().for_each(|v| *v = 0.0);
        self.sum_phase.iter_mut().for_each(|v| *v = 0.0);
        self.acc.iter_mut().for_each(|v| *v = 0.0);
        self.pending_head = HOP;
        self.draining_frames = 0;
        self.primed = false;
    }

    /// Pre-roll the overlap-add buffer at note-on so the first emitted sample
    /// is already at steady, full amplitude — i.e. **zero onset latency**.
    /// This exploits that Mangle reads a stored buffer (not live input): we run
    /// the `TAIL_FRAMES - 1` ramp-up frames up front and discard them, leaving
    /// the accumulator at full overlap before the first real sample.
    fn prime(&mut self, st: &mut Scratch, norm: f32, cx: &ChanCtx) {
        if let Some((wstart, wlen)) = cx.win {
            for _ in 0..TAIL_FRAMES - 1 {
                generate(self, st, norm, cx.data, wstart, wlen, cx.p, cx.ha, cx.r, cx.gain, cx.blur);
            }
            self.pending_head = HOP; // discard the ramp; next gen pops steady output
        }
        self.primed = true;
    }

    /// True while the channel still has output to emit (pending samples or
    /// overlap-add tail). The voice manager uses this so a released Spectral
    /// voice rings out instead of being recycled mid-tail.
    fn ringing(&self) -> bool {
        self.draining_frames > 0 || self.pending_head < HOP
    }

    /// Move the next HOP finalized samples from the accumulator into `pending`
    /// and shift the accumulator down. Shared by frame generation and tail
    /// draining.
    fn pop_and_shift(&mut self) {
        self.pending[..HOP].copy_from_slice(&self.acc[..HOP]);
        self.pending_head = 0;
        self.acc.copy_within(HOP..FRAME, 0);
        self.acc[FRAME - HOP..].iter_mut().for_each(|v| *v = 0.0);
    }

    /// Drain one tail frame after release: emit accumulated overlap, add no new
    /// energy. The OLA tail decays naturally (fewer overlapping frames), so the
    /// release is click-free.
    fn drain(&mut self) {
        self.pop_and_shift();
        self.draining_frames = self.draining_frames.saturating_sub(1);
    }

    /// Produce the next output sample (slot Gain baked in; output level/pan
    /// applied by the caller). Generates a new frame when gated, drains the
    /// tail when released, or returns silence once fully rung out.
    fn next_sample(&mut self, st: &mut Scratch, norm: f32, cx: &ChanCtx) -> f32 {
        if self.pending_head >= HOP {
            if let Some((wstart, wlen)) = cx.win {
                generate(self, st, norm, cx.data, wstart, wlen, cx.p, cx.ha, cx.r, cx.gain, cx.blur);
            } else if self.draining_frames > 0 {
                self.drain();
            } else {
                return 0.0;
            }
        }
        let s = self.pending[self.pending_head];
        self.pending_head += 1;
        s
    }
}

/// Per-channel render context for one block (params are fixed across the
/// block). Resolving it once lets the hot loop stay branch-light and lets the
/// shared fast path compare the two channels cheaply.
struct ChanCtx<'a> {
    win: Option<(usize, usize)>, // (wstart, wlen) when gated + slot loaded
    data: &'a [f32],
    p: &'a LaneParams,
    r: f32,
    ha: f64,
    gain: f32,
    /// Output gains: to own channel (level·keep) and to the opposite (level·swap).
    gl: f32,
    gswap: f32,
    gate: bool,
    mono: bool,
    /// Spectral blur amount 0..1 (Skew reinterpreted) — per-bin phase
    /// randomization for a diffuse smear.
    blur: f32,
}

impl<'a> ChanCtx<'a> {
    fn resolve(p: &'a LaneParams, bank: &'a SampleBank, c: usize) -> Self {
        let level = p.level.clamp(0.0, 1.0);
        let pan = p.pan.clamp(0.0, 1.0);
        let gl = level * (pan * core::f32::consts::FRAC_PI_2).cos();
        let gswap = level * (pan * core::f32::consts::FRAC_PI_2).sin();
        let ha = p.direction.clamp(-1.0, 1.0) as f64 * HOP as f64;
        // Resolve the slot regardless of gate so the mono/share decision still
        // holds during the release tail; only *generate* when gated.
        match resolve_window(p, bank) {
            Some((slot_idx, wstart, wlen)) => {
                let slot = bank.slots[slot_idx].as_ref().unwrap();
                let pitch = p.pitch_st + p.key_track * (p.note - slot.root_note as f32);
                let r = 2.0f32.powf(pitch / 12.0).clamp(0.03125, 32.0);
                ChanCtx {
                    win: if p.gate { Some((wstart, wlen)) } else { None },
                    data: slot.data[c].as_slice(),
                    p,
                    r,
                    ha,
                    gain: slot.gain,
                    gl,
                    gswap,
                    gate: p.gate,
                    mono: slot.mono,
                    blur: p.skew.abs().clamp(0.0, 1.0),
                }
            }
            None => ChanCtx {
                win: None,
                data: &[],
                p,
                r: 1.0,
                ha,
                gain: 1.0,
                gl,
                gswap,
                gate: p.gate,
                mono: false,
                blur: p.skew.abs().clamp(0.0, 1.0),
            },
        }
    }

    /// Both channels would produce identical vocoder output (so one can feed
    /// both outputs): same window, transpose, scrub, gain and grain shaping.
    fn shareable(&self, o: &ChanCtx) -> bool {
        self.win == o.win
            && self.r == o.r
            && self.ha == o.ha
            && self.gain == o.gain
            && self.p.position == o.p.position
            && self.p.algo == o.p.algo
            && self.p.shape == o.p.shape
    }

    /// Nothing to emit: not analyzing and no tail left.
    fn silent(&self, chan: &Channel) -> bool {
        self.win.is_none() && !chan.ringing()
    }
}

/// Reusable per-frame scratch (one frame is processed at a time, so the two
/// channels share it).
struct Scratch {
    re: Vec<f32>,
    im: Vec<f32>,
    mag: Vec<f32>,
    freq: Vec<f32>,
    /// Current analysis phase per bin (for identity phase locking).
    cur_phase: Vec<f32>,
    /// Cepstral spectral envelope, for formant-preserving shift.
    env: Vec<f32>,
    syn_mag: Vec<f32>,
    syn_freq: Vec<f32>,
    /// Analysis phase carried to each synthesis bin through the pitch remap.
    syn_phase_an: Vec<f32>,
    /// Nearest peak bin ≤ / ≥ each bin (phase-locking region assignment).
    peak_prev: Vec<usize>,
    peak_next: Vec<usize>,
}

impl Scratch {
    fn new() -> Self {
        Self {
            re: vec![0.0; FRAME],
            im: vec![0.0; FRAME],
            mag: vec![0.0; NBINS],
            freq: vec![0.0; NBINS],
            cur_phase: vec![0.0; NBINS],
            env: vec![0.0; NBINS],
            syn_mag: vec![0.0; NBINS],
            syn_freq: vec![0.0; NBINS],
            syn_phase_an: vec![0.0; NBINS],
            peak_prev: vec![0; NBINS],
            peak_next: vec![0; NBINS],
        }
    }
}

pub struct SpectralState {
    ch: [Channel; 2],
    scratch: Scratch,
    /// Σ w² at stride HOP — the Hann overlap-add gain to divide back out.
    norm: f32,
}

impl SpectralState {
    pub fn new() -> Self {
        let w = hann();
        let mut norm = 0.0f32;
        let mut i = 0;
        while i < FRAME {
            norm += w[i] * w[i];
            i += HOP;
        }
        Self {
            ch: [Channel::new(0x5EED_0001), Channel::new(0x5EED_0002)],
            scratch: Scratch::new(),
            norm: norm.max(1e-9),
        }
    }

    pub fn reset(&mut self) {
        self.ch[0].reset();
        self.ch[1].reset();
    }

    /// True while either channel still has output to emit (a release tail).
    /// Lets the voice manager keep a released Spectral voice alive until it
    /// has rung out, and the shell keep calling `process` to drain it.
    pub fn ringing(&self) -> bool {
        self.ch[0].ringing() || self.ch[1].ringing()
    }

    /// Render `block` samples for this lane, **adding** into `out_l`/`out_r`
    /// (matching the grain path). Each channel runs its own vocoder against
    /// its own resolved window / pitch / scrub, honoring per-channel gate.
    pub fn process(
        &mut self,
        p: &[LaneParams; 2],
        bank: &SampleBank,
        _sample_rate: f32,
        out_l: &mut [f32],
        out_r: &mut [f32],
        block: usize,
    ) {
        let SpectralState { ch, scratch, norm } = self;
        let cx = [ChanCtx::resolve(&p[0], bank, 0), ChanCtx::resolve(&p[1], bank, 1)];

        // Fast path: a mono source with matching per-channel params (and gate)
        // means the two vocoders would be identical, so run one and feed both
        // outputs — halving the FFT work in the common case. Each output still
        // gets its own pan/level, and the shared tail rings out on both sides.
        // Blur uses per-channel randomness, so a blurred lane can't be shared
        // (we want L/R to decorrelate) — fall to the independent path then.
        let share = cx[0].mono
            && cx[1].mono
            && cx[0].gate == cx[1].gate
            && cx[0].blur == 0.0
            && cx[1].blur == 0.0
            && cx[0].shareable(&cx[1]);

        if share {
            if cx[0].silent(&ch[0]) {
                return;
            }
            if !ch[0].primed && cx[0].win.is_some() {
                ch[0].prime(scratch, *norm, &cx[0]);
            }
            for i in 0..block {
                let s = ch[0].next_sample(scratch, *norm, &cx[0]);
                out_l[i] += s * cx[0].gl + s * cx[1].gswap; // ch0 keep→L + ch1 swap→L
                out_r[i] += s * cx[0].gswap + s * cx[1].gl; // ch0 swap→R + ch1 keep→R
            }
        } else {
            for (c, chan) in ch.iter_mut().enumerate() {
                let cc = &cx[c];
                if cc.silent(chan) {
                    continue;
                }
                if !chan.primed && cc.win.is_some() {
                    chan.prime(scratch, *norm, cc);
                }
                for i in 0..block {
                    let s = chan.next_sample(scratch, *norm, cc);
                    if c == 0 {
                        out_l[i] += s * cc.gl;
                        out_r[i] += s * cc.gswap;
                    } else {
                        out_r[i] += s * cc.gl;
                        out_l[i] += s * cc.gswap;
                    }
                }
            }
        }
    }
}

impl Default for SpectralState {
    fn default() -> Self {
        Self::new()
    }
}

/// Analyze one frame at the playhead, propagate phases, pitch-remap, and
/// overlap-add HOP finalized samples into `chan.pending`.
// Index-based frame loops read three parallel buffers (source, window, FFT)
// plus the running sample index, so explicit indexing is clearer than zips.
#[allow(clippy::too_many_arguments, clippy::needless_range_loop)]
fn generate(
    chan: &mut Channel,
    st: &mut Scratch,
    norm: f32,
    data: &[f32],
    wstart: usize,
    wlen: usize,
    p: &LaneParams,
    ha: f64,
    r: f32,
    gain: f32,
    blur: f32,
) {
    let w = hann();
    let fft = plan();
    let bin_w = core::f32::consts::TAU / FRAME as f32; // nominal rad/sample per bin

    // 1. Windowed analysis frame at the playhead (+ static Position offset).
    //    Slot Gain is baked in here so the release tail needs no live slot.
    let read_start = chan.pos + p.position as f64 * wlen as f64;
    for i in 0..FRAME {
        st.re[i] = read(data, read_start + i as f64, wstart, wlen, p.algo) * w[i] * gain;
        st.im[i] = 0.0;
    }
    fft.transform(&mut st.re, &mut st.im, false);

    // 2. Magnitude + instantaneous (true) per-sample frequency per bin.
    let frozen = ha.abs() < 1e-6;
    for k in 0..NBINS {
        let (re, im) = (st.re[k], st.im[k]);
        st.mag[k] = (re * re + im * im).sqrt();
        let phase = im.atan2(re);
        let nominal = bin_w * k as f32;
        st.freq[k] = if frozen {
            nominal // freeze: sustain each bin at its centre frequency
        } else {
            let expected = nominal * ha as f32;
            let dev = princarg(phase - chan.last_phase[k] - expected);
            nominal + dev / ha as f32
        };
        st.cur_phase[k] = phase;
        chan.last_phase[k] = phase;
    }

    // 2a. Spectral envelope for optional formant-preserving pitch shift. In
    //     Spectral mode the otherwise-unused Shape param sets the amount: 0 =
    //     classic (formant-shifting, the "chipmunk" shift), 1 = formants held
    //     while harmonics move. The envelope is the true cepstral envelope:
    //     real cepstrum (IDFT of the log-magnitude), low-quefrency lifter, DFT
    //     back, exponentiate. It rides formant peaks far better than a plain
    //     magnitude smooth. Reuses st.re/st.im as scratch — the analysis
    //     spectrum is already captured in mag/freq.
    let formant = p.shape.clamp(0.0, 1.0);
    if formant > 0.0 {
        const EPS: f32 = 1e-6;
        for k in 0..NBINS {
            st.re[k] = (st.mag[k] + EPS).ln();
            st.im[k] = 0.0;
        }
        for k in 1..FRAME / 2 {
            st.re[FRAME - k] = st.re[k]; // even-symmetric: log-magnitude is real
            st.im[FRAME - k] = 0.0;
        }
        fft.transform(&mut st.re, &mut st.im, true); // → FRAME · real cepstrum
        // Lifter: keep low quefrency (smooth envelope), drop the fine harmonic
        // structure. Symmetric, so the envelope stays real.
        const Q: usize = 48;
        for n in (Q + 1)..(FRAME - Q) {
            st.re[n] = 0.0;
        }
        st.im.iter_mut().for_each(|v| *v = 0.0);
        fft.transform(&mut st.re, &mut st.im, false); // → FRAME · log-envelope
        let inv = 1.0 / FRAME as f32;
        for k in 0..NBINS {
            st.env[k] = (st.re[k] * inv).exp();
        }
    }

    // 3. Pitch remap: analysis bin k → synthesis bin round(k·r), freq × r.
    //    Carry the analysis phase to the synthesis bin for phase locking.
    for j in 0..NBINS {
        st.syn_mag[j] = 0.0;
        st.syn_freq[j] = bin_w * j as f32;
        st.syn_phase_an[j] = 0.0;
    }
    for k in 0..NBINS {
        let j = (k as f32 * r).round() as usize;
        if j < NBINS {
            st.syn_mag[j] += st.mag[k];
            st.syn_freq[j] = st.freq[k] * r;
            st.syn_phase_an[j] = st.cur_phase[k];
        }
    }

    // 3a. Formant preserve: replace the shifted envelope at output bin j with
    //     the original envelope there (scale by env[j] / env[source bin]).
    if formant > 0.0 {
        for j in 0..NBINS {
            let src = ((j as f32 / r).round() as usize).min(NBINS - 1);
            let ratio = (st.env[j] / st.env[src].max(1e-9)).clamp(0.0, 8.0);
            st.syn_mag[j] *= 1.0 + formant * (ratio - 1.0); // lerp(1, ratio, formant)
        }
    }

    // 4. Advance each synthesis phase by its frequency (+ optional blur: a
    //    per-bin phase randomization that diffuses the signal into a wash).
    for j in 0..NBINS {
        let jitter = if blur > 0.0 {
            blur * core::f32::consts::PI * chan.rng.bipolar()
        } else {
            0.0
        };
        chan.sum_phase[j] = princarg(chan.sum_phase[j] + st.syn_freq[j] * HOP as f32 + jitter);
    }

    // 4a. Identity phase locking (Laroche–Dolson): re-couple the bins around
    //     each spectral peak to the peak's phase, cutting the vocoder's
    //     "phasiness" / reverberant smear during stretch and freeze. Skipped
    //     under blur, which intentionally decorrelates phases.
    if blur == 0.0 {
        lock_phases(
            &st.syn_mag,
            &st.syn_phase_an,
            &mut chan.sum_phase,
            &mut st.peak_prev,
            &mut st.peak_next,
        );
    }

    // 4b. Build the half-spectrum from magnitude + (locked) phase.
    for j in 0..NBINS {
        st.re[j] = st.syn_mag[j] * chan.sum_phase[j].cos();
        st.im[j] = st.syn_mag[j] * chan.sum_phase[j].sin();
    }
    st.im[0] = 0.0;
    st.im[FRAME / 2] = 0.0;

    // 5. Hermitian-mirror the upper half for a real inverse transform.
    for k in 1..FRAME / 2 {
        st.re[FRAME - k] = st.re[k];
        st.im[FRAME - k] = -st.im[k];
    }
    fft.transform(&mut st.re, &mut st.im, true);

    // 6. Synthesis window + de-gain, overlap-add, pop HOP finalized samples.
    let scale = 1.0 / (FRAME as f32 * norm);
    for i in 0..FRAME {
        chan.acc[i] += st.re[i] * w[i] * scale;
    }
    chan.pop_and_shift();
    chan.draining_frames = TAIL_FRAMES; // full tail available if released next block

    // 7. Advance the analysis playhead (wrapped into the window).
    chan.pos = (chan.pos + ha).rem_euclid(wlen.max(1) as f64);
}

/// Identity phase locking (Laroche–Dolson). Each spectral peak keeps its freely
/// propagated synthesis phase; every other bin in the peak's region of
/// influence is reset to the peak's synthesis phase plus that bin's *analysis*
/// phase offset from the peak. This re-locks the bins of each partial together,
/// removing most of the phase-vocoder "phasiness" during stretch and freeze.
/// `peak_prev`/`peak_next` are pre-allocated scratch (nearest peak ≤ / ≥ bin).
fn lock_phases(
    mag: &[f32],
    ana_phase: &[f32],
    sum_phase: &mut [f32],
    peak_prev: &mut [usize],
    peak_next: &mut [usize],
) {
    let n = mag.len();
    if n < 5 {
        return;
    }
    const NONE: usize = usize::MAX;
    let is_peak = |j: usize| -> bool {
        j >= 2
            && j + 2 < n
            && mag[j] > mag[j - 1]
            && mag[j] > mag[j - 2]
            && mag[j] > mag[j + 1]
            && mag[j] > mag[j + 2]
    };
    let mut last = NONE;
    for j in 0..n {
        if is_peak(j) {
            last = j;
        }
        peak_prev[j] = last;
    }
    let mut next = NONE;
    for j in (0..n).rev() {
        if is_peak(j) {
            next = j;
        }
        peak_next[j] = next;
    }
    for j in 0..n {
        let p = match (peak_prev[j], peak_next[j]) {
            (NONE, NONE) => continue, // no peaks (e.g. silence) — leave free phase
            (a, NONE) => a,
            (NONE, b) => b,
            (a, b) => {
                if j - a <= b - j {
                    a
                } else {
                    b
                }
            }
        };
        if p != j {
            sum_phase[j] = princarg(sum_phase[p] + ana_phase[j] - ana_phase[p]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hann_overlap_add_is_unity() {
        // Σ w[n + m·HOP] over the frame must be ~constant (COLA for Hann at
        // 75 % overlap) — the precondition for click-free reconstruction.
        let w = hann();
        for n0 in 0..HOP {
            let mut s = 0.0f32;
            let mut i = n0;
            while i < FRAME {
                s += w[i];
                i += HOP;
            }
            assert!((s - 2.0).abs() < 1e-3, "Hann COLA not flat at {n0}: {s}");
        }
    }

    #[test]
    fn norm_matches_w_squared_overlap() {
        let st = SpectralState::new();
        assert!(st.norm > 0.5 && st.norm < 2.5, "unexpected OLA norm {}", st.norm);
    }

    #[test]
    fn lock_phases_enforces_identity_relation() {
        let n = 64;
        let mut mag = vec![0.01f32; n];
        mag[10] = 1.0;
        mag[30] = 0.8; // two clear peaks; everything else flat (no other peaks)
        let ana: Vec<f32> = (0..n).map(|k| (k as f32 * 0.37).sin()).collect();
        let mut sum: Vec<f32> = (0..n).map(|k| (k as f32 * 0.11).cos()).collect();
        let (peak10, peak30) = (sum[10], sum[30]);
        let mut pp = vec![0usize; n];
        let mut pn = vec![0usize; n];
        lock_phases(&mag, &ana, &mut sum, &mut pp, &mut pn);
        // Peaks keep their free phase.
        assert!((sum[10] - peak10).abs() < 1e-6);
        assert!((sum[30] - peak30).abs() < 1e-6);
        // Bins lock to their nearest peak with the analysis phase offset.
        for (j, peak, base) in [(8usize, 10usize, peak10), (32usize, 30usize, peak30)] {
            let expect = princarg(base + ana[j] - ana[peak]);
            assert!(princarg(sum[j] - expect).abs() < 1e-5, "bin {j} not locked to {peak}");
        }
    }
}
