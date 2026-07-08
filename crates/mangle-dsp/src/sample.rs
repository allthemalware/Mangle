//! Sample slots and grain read algorithms.
//!
//! The per-engine **Algo** parameter selects how grains read and resample
//! their source — the "stretch character" of the engine:
//!
//! * `Nearest` — zero-order hold. Gritty, aliased, lo-fi.
//! * `Linear`  — 2-point. The neutral granular workhorse.
//! * `Hermite` — 4-point Catmull-Rom. Smooth, low-cost.
//! * `Sinc`    — 8-tap Blackman-windowed sinc. Clean even at big pitch
//!   shifts.
//!
//! The enum is stored in state as an integer step index, so spectral modes
//! (Paulstretch-style smear etc.) can append in v2 without breaking saves.

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Algo {
    Nearest,
    Linear,
    Hermite,
    Sinc,
}

pub const ALGO_NAMES: [&str; 4] = ["Nearest", "Linear", "Hermite", "Sinc-8"];

impl Algo {
    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Algo::Nearest,
            1 => Algo::Linear,
            2 => Algo::Hermite,
            _ => Algo::Sinc,
        }
    }
}

/// One loaded sample: stereo f32 at the host rate (the loader resamples on
/// import), plus loop window and root note.
pub struct SampleSlot {
    /// [left, right]
    pub data: [Vec<f32>; 2],
    pub sample_rate: f32,
    pub root_note: u8,
    /// Loop window in samples, `start < end <= len`.
    pub loop_start: usize,
    pub loop_end: usize,
    /// Linear gain (Slot Gain param), baked into grain level at snapshot.
    pub gain: f32,
    /// True when both channels hold identical content (a mono source upmixed
    /// to stereo). Lets the spectral vocoder run a single shared analysis for
    /// both output channels when their params also match.
    pub mono: bool,
}

impl SampleSlot {
    pub fn len(&self) -> usize {
        self.data[0].len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Zero-length slot. Does not allocate, so it can be built on the audio
    /// thread (e.g. as a swap placeholder when draining GUI sample loads).
    pub fn empty() -> Self {
        Self {
            data: [Vec::new(), Vec::new()],
            sample_rate: 0.0,
            root_note: 60,
            loop_start: 0,
            loop_end: 0,
            gain: 1.0,
            mono: true,
        }
    }

    /// Constant-valued sample — used by the COLA/engine tests.
    pub fn constant(value: f32, n: usize, sample_rate: f32) -> Self {
        Self {
            data: [vec![value; n], vec![value; n]],
            sample_rate,
            root_note: 60,
            loop_start: 0,
            loop_end: n,
            gain: 1.0,
            mono: true,
        }
    }

    /// Built-in fallback tone: 2 s additive saw at 110 Hz with slight
    /// stereo detune. Loaded into slot 1 when the bank is empty so the
    /// plugin makes sound before the sample loader lands (build step 4).
    pub fn test_tone(sample_rate: f32) -> Self {
        let n = (sample_rate * 2.0) as usize;
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        for h in 1..=24u32 {
            let amp = 1.0 / h as f32;
            let fl = 110.0 * h as f32;
            let fr = 110.5 * h as f32;
            for i in 0..n {
                let t = i as f32 / sample_rate;
                l[i] += amp * (core::f32::consts::TAU * fl * t).sin();
                r[i] += amp * (core::f32::consts::TAU * fr * t).sin();
            }
        }
        let peak = l
            .iter()
            .chain(r.iter())
            .fold(0.0f32, |m, v| m.max(v.abs()))
            .max(1e-9);
        let g = 0.8 / peak;
        for v in l.iter_mut().chain(r.iter_mut()) {
            *v *= g;
        }
        Self {
            data: [l, r],
            sample_rate,
            root_note: 45, // A1 = 110 Hz
            loop_start: 0,
            loop_end: n,
            gain: 1.0,
            mono: false, // L/R are detuned — genuinely stereo
        }
    }
}

/// The six user sample slots.
pub struct SampleBank {
    pub slots: [Option<SampleSlot>; 6],
}

impl SampleBank {
    pub fn new() -> Self {
        Self { slots: [None, None, None, None, None, None] }
    }

    /// Indices of loaded slots, in slot order, allocation-free.
    pub fn loaded(&self) -> ([usize; 6], usize) {
        let mut idx = [0usize; 6];
        let mut n = 0;
        for (i, s) in self.slots.iter().enumerate() {
            if s.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
                idx[n] = i;
                n += 1;
            }
        }
        (idx, n)
    }
}

impl Default for SampleBank {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------- sinc table

const SINC_TAPS: usize = 8;
const SINC_PHASES: usize = 512;

struct SincTable {
    /// taps[phase][tap], rows normalized to unity DC gain.
    taps: Vec<[f32; SINC_TAPS]>,
}

fn sinc_table() -> &'static SincTable {
    static TABLE: OnceLock<SincTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut taps = Vec::with_capacity(SINC_PHASES + 1);
        for p in 0..=SINC_PHASES {
            let frac = p as f32 / SINC_PHASES as f32;
            let mut row = [0.0f32; SINC_TAPS];
            let mut sum = 0.0f32;
            for (k, t) in row.iter_mut().enumerate() {
                // Tap offsets -3..=4 relative to the integer read position.
                let x = (k as f32 - 3.0) - frac;
                let sinc = if x.abs() < 1e-6 {
                    1.0
                } else {
                    let px = core::f32::consts::PI * x;
                    px.sin() / px
                };
                // Blackman window over the 8-tap span (|x| <= 4).
                let u = (x / 4.0).clamp(-1.0, 1.0);
                let w = 0.42 + 0.5 * (core::f32::consts::PI * u).cos()
                    + 0.08 * (2.0 * core::f32::consts::PI * u).cos();
                *t = sinc * w;
                sum += *t;
            }
            for t in row.iter_mut() {
                *t /= sum;
            }
            taps.push(row);
        }
        SincTable { taps }
    })
}

/// Pre-build lazily-initialized tables. Call from a non-realtime context
/// (plugin `initialize`) so the first Sinc grain never allocates on the
/// audio thread.
pub fn warm_tables() {
    let _ = sinc_table();
}

// ------------------------------------------------------------------ read

/// Read one channel at fractional position `pos` (absolute samples),
/// wrapped into the window `[wstart, wstart + wlen)`, using `algo`.
///
/// Caller guarantees `wstart + wlen <= data.len()` and `wlen >= 1`.
pub fn read(data: &[f32], pos: f64, wstart: usize, wlen: usize, algo: Algo) -> f32 {
    if wlen == 0 || data.is_empty() {
        return 0.0;
    }
    let rel = (pos - wstart as f64).rem_euclid(wlen as f64);
    let base = rel.floor();
    let frac = (rel - base) as f32;
    let i0 = base as isize;
    let wlen_i = wlen as isize;
    let at = |k: isize| -> f32 {
        let idx = (i0 + k).rem_euclid(wlen_i) as usize;
        data[wstart + idx]
    };
    match algo {
        Algo::Nearest => at(if frac < 0.5 { 0 } else { 1 }),
        Algo::Linear => at(0) * (1.0 - frac) + at(1) * frac,
        Algo::Hermite => {
            // 4-point Catmull-Rom.
            let xm1 = at(-1);
            let x0 = at(0);
            let x1 = at(1);
            let x2 = at(2);
            let c1 = 0.5 * (x1 - xm1);
            let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
            let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
            ((c3 * frac + c2) * frac + c1) * frac + x0
        }
        Algo::Sinc => {
            let table = sinc_table();
            let row = &table.taps[(frac * SINC_PHASES as f32) as usize];
            let mut acc = 0.0;
            for (k, w) in row.iter().enumerate() {
                acc += w * at(k as isize - 3);
            }
            acc
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resample a test sine through each algorithm and rank reconstruction
    /// quality. Sinc must beat Hermite must beat Linear must beat Nearest.
    #[test]
    fn algo_quality_ordering() {
        let sr = 48_000.0f64;
        let freq = 5_000.0f64;
        let n = 48_000usize;
        let data: Vec<f32> = (0..n)
            .map(|i| (core::f64::consts::TAU * freq * i as f64 / sr).sin() as f32)
            .collect();

        let ratio = 1.27f64; // read increment (pitch up ~4 st)
        let snr = |algo: Algo| -> f32 {
            let mut sig = 0.0f64;
            let mut err = 0.0f64;
            let mut pos = 100.0f64;
            for _ in 0..4_000 {
                let truth = (core::f64::consts::TAU * freq * pos / sr).sin();
                let got = read(&data, pos, 0, n, algo) as f64;
                sig += truth * truth;
                err += (got - truth) * (got - truth);
                pos += ratio;
            }
            (10.0 * (sig / err.max(1e-30)).log10()) as f32
        };

        let near = snr(Algo::Nearest);
        let lin = snr(Algo::Linear);
        let herm = snr(Algo::Hermite);
        let sinc = snr(Algo::Sinc);
        assert!(near < lin, "nearest {near} dB !< linear {lin} dB");
        assert!(lin < herm, "linear {lin} dB !< hermite {herm} dB");
        assert!(herm < sinc, "hermite {herm} dB !< sinc {sinc} dB");
        assert!(sinc > 50.0, "sinc only {sinc} dB");
        assert!(near < 35.0, "nearest suspiciously clean: {near} dB");
    }

    #[test]
    fn read_wraps_inside_window() {
        // Window [4, 8) over a ramp 0..12 — reads must never leave it.
        let data: Vec<f32> = (0..12).map(|i| i as f32).collect();
        for algo in [Algo::Nearest, Algo::Linear, Algo::Hermite, Algo::Sinc] {
            let mut pos = -20.0f64;
            while pos < 40.0 {
                let v = read(&data, pos, 4, 4, algo);
                assert!(
                    (3.0..=8.0).contains(&v),
                    "{algo:?} read {v} outside window at pos {pos}"
                );
                pos += 0.37;
            }
        }
    }

    #[test]
    fn dc_gain_is_unity() {
        let data = vec![1.0f32; 64];
        for algo in [Algo::Nearest, Algo::Linear, Algo::Hermite, Algo::Sinc] {
            for i in 0..50 {
                let v = read(&data, 7.0 + i as f64 * 0.13, 0, 64, algo);
                assert!((v - 1.0).abs() < 1e-3, "{algo:?} DC gain {v}");
            }
        }
    }

    #[test]
    fn bank_loaded_indices() {
        let mut bank = SampleBank::new();
        assert_eq!(bank.loaded().1, 0);
        bank.slots[2] = Some(SampleSlot::constant(0.5, 100, 48_000.0));
        bank.slots[5] = Some(SampleSlot::constant(0.5, 100, 48_000.0));
        let (idx, n) = bank.loaded();
        assert_eq!(n, 2);
        assert_eq!(&idx[..2], &[2, 5]);
    }
}
