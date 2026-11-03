//! Drawable curves, curve-driven stereo LFOs, Drift and Noise mod sources.
//! Parameter map §4–§6, §9.

use crate::rng::Rng;

pub const CURVE_LEN: usize = 256;
/// Number of shapes in the preset bank.
pub const N_PRESETS: usize = 64;

/// A drawable modulation curve: 256 samples in -1..1, linearly interpolated,
/// wrapping. Edited on the EDIT.CURVES screen; stored in plugin state.
#[derive(Clone)]
pub struct Curve(pub [f32; CURVE_LEN]);

impl Curve {
    fn from_fn(f: impl Fn(f32) -> f32) -> Self {
        let mut data = [0.0; CURVE_LEN];
        for (i, v) in data.iter_mut().enumerate() {
            let t = i as f32 / CURVE_LEN as f32;
            *v = f(t).clamp(-1.0, 1.0);
        }
        Curve(data)
    }

    /// Rising ramp -1 → 1: the default Loop LFO shape (linear sample scan).
    pub fn ramp() -> Self {
        Self::from_fn(|t| t * 2.0 - 1.0)
    }

    pub fn sine() -> Self {
        Self::from_fn(|t| (t * core::f32::consts::TAU).sin())
    }

    pub fn triangle() -> Self {
        Self::from_fn(|t| 1.0 - 4.0 * (t - 0.5).abs())
    }

    pub fn square() -> Self {
        Self::from_fn(|t| if t < 0.5 { 1.0 } else { -1.0 })
    }

    pub fn steps(n: usize, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let n = n.clamp(2, 64);
        let levels: Vec<f32> = (0..n).map(|_| rng.bipolar()).collect();
        Self::from_fn(|t| levels[((t * n as f32) as usize).min(n - 1)])
    }

    pub fn random_walk(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let mut data = [0.0f32; CURVE_LEN];
        let mut v = 0.0f32;
        for d in data.iter_mut() {
            v = (v + rng.bipolar() * 0.18).clamp(-1.0, 1.0);
            *d = v;
        }
        // Drift back toward the start value so the wrap point doesn't snap.
        let end = data[CURVE_LEN - 1];
        for (i, d) in data.iter_mut().enumerate() {
            let t = i as f32 / (CURVE_LEN - 1) as f32;
            *d = (*d - end * t * 0.5).clamp(-1.0, 1.0);
        }
        Curve(data)
    }

    /// Straight segment between table points `i0` and `i1` (either order),
    /// inclusive. Indices clamp to the table; values clamp to -1..1.
    pub fn line(&mut self, i0: usize, v0: f32, i1: usize, v1: f32) {
        let (i0, v0, i1, v1) = if i0 <= i1 {
            (i0, v0, i1, v1)
        } else {
            (i1, v1, i0, v0)
        };
        let (i0, i1) = (i0.min(CURVE_LEN - 1), i1.min(CURVE_LEN - 1));
        let (v0, v1) = (v0.clamp(-1.0, 1.0), v1.clamp(-1.0, 1.0));
        if i0 == i1 {
            self.0[i0] = v1;
            return;
        }
        let span = (i1 - i0) as f32;
        for i in i0..=i1 {
            let t = (i - i0) as f32 / span;
            self.0[i] = (v0 + (v1 - v0) * t).clamp(-1.0, 1.0);
        }
        self.0[i0] = v0;
        self.0[i1] = v1;
    }

    /// Step-quantize: split the table into `steps` (clamped 2..=64) equal
    /// regions, each set to its region's mean.
    pub fn quantize(&mut self, steps: usize) {
        let steps = steps.clamp(2, 64);
        for k in 0..steps {
            let lo = k * CURVE_LEN / steps;
            let hi = (k + 1) * CURVE_LEN / steps;
            let mean = self.0[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
            self.0[lo..hi].fill(mean.clamp(-1.0, 1.0));
        }
    }

    /// Rotate by `cycles` of a cycle (positive = later), linear-interpolated.
    pub fn shift_phase(&mut self, cycles: f32) {
        let src = self.clone();
        for (i, v) in self.0.iter_mut().enumerate() {
            *v = src.sample(i as f32 / CURVE_LEN as f32 - cycles);
        }
    }

    /// Preset bank entry `i` (wraps modulo `N_PRESETS`): basic waves, pulse
    /// widths, stepped shapes, random walks, chaos maps, gate patterns and
    /// exp/log/S ramps.
    pub fn preset(i: usize) -> Curve {
        use core::f32::consts::{PI, TAU};
        let i = i % N_PRESETS;
        let k = i % 8;
        match i / 8 {
            0 => match k {
                0 => Self::sine(),
                1 => Self::triangle(),
                2 => Self::from_fn(|t| t * 2.0 - 1.0),
                3 => Self::from_fn(|t| 1.0 - t * 2.0),
                4 => Self::from_fn(|t| (t * TAU).sin().abs() * 2.0 - 1.0),
                5 => Self::from_fn(|t| (t * TAU).sin().max(0.0) * 2.0 - 1.0),
                6 => Self::from_fn(|t| (t * 2.0 * TAU).sin()),
                _ => Self::from_fn(|t| (t * 3.0 * TAU).sin()),
            },
            1 => {
                let width = (k + 1) as f32 / 9.0;
                Self::from_fn(|t| if t < width { 1.0 } else { -1.0 })
            }
            2 => {
                const N: [usize; 8] = [2, 3, 4, 5, 6, 8, 12, 16];
                Self::steps(N[k], 0x5712 + k as u64)
            }
            3 => Self::random_walk(0x3a1c + k as u64),
            4 => {
                let r = 3.6 + 0.055 * k as f32;
                Self::chaos(0.1 + 0.1 * k as f32, |x| r * x * (1.0 - x))
            }
            5 => {
                let mu = 1.2 + 0.1 * k as f32;
                Self::chaos(0.2 + 0.07 * k as f32, |x| mu * x.min(1.0 - x) + 0.01)
            }
            6 => {
                const PATTERNS: [u16; 8] = [
                    0b1000_1000_1000_1000,
                    0b1010_1010_1010_1010,
                    0b1001_0010_0100_1001,
                    0b1110_1110_1110_1110,
                    0b1011_0110,
                    0b1000_0100,
                    0b1101_0010,
                    0b1100_1010_1100_0110,
                ];
                let p = PATTERNS[k];
                let n = if k < 4 || k == 7 { 16 } else { 8 };
                Self::from_fn(|t| {
                    let step = ((t * n as f32) as usize).min(n - 1);
                    if p >> (n - 1 - step) & 1 == 1 {
                        1.0
                    } else {
                        -1.0
                    }
                })
            }
            _ => match k {
                0 => Self::from_fn(|t| (t * 4.0).exp_m1() / 4f32.exp_m1() * 2.0 - 1.0),
                1 => Self::from_fn(|t| (-t * 4.0).exp_m1() / (-4f32).exp_m1() * -2.0 + 1.0),
                2 => Self::from_fn(|t| (1.0 + t * 15.0).ln() / 16f32.ln() * 2.0 - 1.0),
                3 => Self::from_fn(|t| 1.0 - (1.0 + t * 15.0).ln() / 16f32.ln() * 2.0),
                4 => Self::from_fn(|t| -(t * PI).cos()),
                5 => Self::from_fn(|t| (t * PI).cos()),
                6 => Self::from_fn(|t| (t * 8.0).exp_m1() / 8f32.exp_m1() * 2.0 - 1.0),
                _ => Self::from_fn(|t| {
                    let s = t * t * (3.0 - 2.0 * t);
                    s * s * (3.0 - 2.0 * s) * 2.0 - 1.0
                }),
            },
        }
    }

    /// Iterated 1-D map rendered as a bipolar curve.
    fn chaos(x0: f32, f: impl Fn(f32) -> f32) -> Self {
        let mut x = x0;
        for _ in 0..16 {
            x = f(x).clamp(0.0, 1.0);
        }
        let mut data = [0.0; CURVE_LEN];
        for d in data.iter_mut() {
            x = f(x).clamp(0.0, 1.0);
            *d = x * 2.0 - 1.0;
        }
        Curve(data)
    }

    /// Wrapping, linearly interpolated lookup. `phase` in cycles (any value).
    pub fn sample(&self, phase: f32) -> f32 {
        let x = (phase - phase.floor()) * CURVE_LEN as f32;
        let i = x as usize % CURVE_LEN;
        let j = (i + 1) % CURVE_LEN;
        let frac = x - x.floor();
        self.0[i] * (1.0 - frac) + self.0[j] * frac
    }

    pub fn invert(&mut self) {
        for v in self.0.iter_mut() {
            *v = -*v;
        }
    }

    pub fn mirror(&mut self) {
        self.0.reverse();
    }

    /// One pass of a 3-tap smoother (wrapping).
    pub fn smooth(&mut self) {
        let src = self.0;
        for i in 0..CURVE_LEN {
            let a = src[(i + CURVE_LEN - 1) % CURVE_LEN];
            let b = src[i];
            let c = src[(i + 1) % CURVE_LEN];
            self.0[i] = 0.25 * a + 0.5 * b + 0.25 * c;
        }
    }
}

/// Phase at which a one-shot envelope holds — the last curve sample point, so
/// `sample()` returns the final drawn value (not the wrap-around to point 0).
const ONE_SHOT_HOLD: f32 = (CURVE_LEN - 1) as f32 / CURVE_LEN as f32;

/// Runtime parameters for one (stereo) LFO. Display units already resolved.
#[derive(Clone, Copy, Debug)]
pub struct LfoParams {
    pub rate_hz: f32,
    /// Initial / reset phase in cycles (0..1).
    pub phase: f32,
    /// Stereo phase 0..1 → up to half a cycle of L/R offset.
    pub st_phase: f32,
    /// 0..1 slew limit; 0 = instant.
    pub lag: f32,
    pub gate: bool,
    /// One-shot: run once to the end of the curve and hold the final value
    /// instead of looping, turning the drawable curve into an MSEG envelope.
    /// Phase is the start point: the read position is `cycle + phase`, held
    /// at the end of the curve (free-running, or per transport cycle).
    /// Retrigger (Trig / Gate-Sync) re-fires it.
    pub one_shot: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LfoState {
    pub running_phase: f32,
    lag_l: f32,
    lag_r: f32,
}

impl LfoState {
    pub fn trig(&mut self) {
        self.running_phase = 0.0;
    }

    /// Advance by `dt` seconds, return the lagged (L, R) pair in -1..1.
    pub fn tick(&mut self, curve: &Curve, p: &LfoParams, dt: f32) -> (f32, f32) {
        if p.gate {
            let next = self.running_phase + p.rate_hz * dt;
            self.running_phase = if p.one_shot {
                next.min(ONE_SHOT_HOLD) // run once, then hold the final value
            } else {
                next.rem_euclid(1.0)
            };
        }
        self.emit(curve, p, dt)
    }

    /// Transport-locked variant: phase comes from the host timeline (in
    /// cycles) instead of free-running. Phase offset, St.Phase and Lag
    /// still apply.
    pub fn tick_transport(
        &mut self,
        curve: &Curve,
        p: &LfoParams,
        phase: f32,
        dt: f32,
    ) -> (f32, f32) {
        let ph = phase.rem_euclid(1.0);
        self.running_phase = ph;
        self.emit(curve, p, dt)
    }

    fn emit(&mut self, curve: &Curve, p: &LfoParams, dt: f32) -> (f32, f32) {
        let l_pos = self.running_phase + p.phase;
        let r_pos = l_pos + p.st_phase * 0.5;
        let (l_pos, r_pos) = if p.one_shot {
            (l_pos.min(ONE_SHOT_HOLD), r_pos.min(ONE_SHOT_HOLD))
        } else {
            (l_pos, r_pos)
        };
        let l_raw = curve.sample(l_pos);
        let r_raw = curve.sample(r_pos);
        let a = lag_coeff(p.lag, dt);
        self.lag_l += (l_raw - self.lag_l) * a;
        self.lag_r += (r_raw - self.lag_r) * a;
        // Flush denormals from the lag one-poles.
        if self.lag_l.abs() < 1e-20 {
            self.lag_l = 0.0;
        }
        if self.lag_r.abs() < 1e-20 {
            self.lag_r = 0.0;
        }
        (self.lag_l, self.lag_r)
    }
}

fn lag_coeff(lag: f32, dt: f32) -> f32 {
    if lag <= 0.0 {
        return 1.0;
    }
    // lag 0..1 → time constant up to ~2 s, squared for fine low-end control.
    let tau = lag * lag * 2.0 + 1e-4;
    1.0 - (-dt / tau).exp()
}

/// Spring-mass drift core: random accelerations on a damped spring —
/// Torsion's "very slow natural motions".
#[derive(Clone, Copy, Debug, Default)]
pub struct DriftCore {
    pos: f32,
    vel: f32,
}

impl DriftCore {
    pub fn tick(&mut self, dt: f32, rng: &mut Rng) -> f32 {
        const K: f32 = 2.0; // spring constant
        const C: f32 = 1.2; // damping
                            // Noise scaled by 1/sqrt(dt) so the diffusion (and therefore the
                            // *sound*) is independent of the control rate / host buffer size.
        const IMPULSE: f32 = 0.4;
        let noise = rng.bipolar() * IMPULSE / dt.max(1e-6).sqrt();
        let accel = -K * self.pos - C * self.vel + noise;
        self.vel += accel * dt;
        self.pos = (self.pos + self.vel * dt).clamp(-1.5, 1.5);
        self.pos.clamp(-1.0, 1.0)
    }
}

/// One Drift unit: two independent cores crossfaded by St.Phase into L/R.
#[derive(Clone, Copy, Debug, Default)]
pub struct DriftPair {
    a: DriftCore,
    b: DriftCore,
}

impl DriftPair {
    /// `st` 0..1: 0 → mono (both channels = core A), 1 → fully decorrelated.
    pub fn tick(&mut self, dt: f32, st: f32, rng: &mut Rng) -> (f32, f32) {
        let va = self.a.tick(dt, rng);
        let vb = self.b.tick(dt, rng);
        (va, va * (1.0 - st) + vb * st)
    }
}

/// One Noise unit: uniform random −1..1, sample-and-hold (no smoothing),
/// re-rolled by `GlobalMod` at a fixed 750 Hz.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoisePair;

impl NoisePair {
    pub fn roll(&self, st: f32, rng: &mut Rng) -> (f32, f32) {
        let a = rng.bipolar();
        let b = rng.bipolar();
        (a, a * (1.0 - st) + b * st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_sample_wraps_and_interpolates() {
        let c = Curve::ramp();
        assert!((c.sample(0.0) + 1.0).abs() < 0.02);
        // Last table point (wrap-lerp blends across the seam after this).
        assert!(c.sample(255.0 / 256.0) > 0.95);
        assert!((c.sample(1.25) - c.sample(0.25)).abs() < 1e-6);
        assert!(c.sample(0.5).abs() < 0.02);
    }

    #[test]
    fn presets_respect_curve_range() {
        for seed in 0..16u64 {
            for c in [
                Curve::ramp(),
                Curve::sine(),
                Curve::triangle(),
                Curve::square(),
                Curve::steps(7, seed),
                Curve::random_walk(seed),
            ] {
                for v in c.0.iter() {
                    assert!((-1.0..=1.0).contains(v), "curve value {v} out of range");
                }
            }
        }
    }

    #[test]
    fn lfo_stereo_phase_offsets_channels() {
        let c = Curve::sine();
        let p = LfoParams {
            rate_hz: 1.0,
            phase: 0.0,
            st_phase: 1.0,
            lag: 0.0,
            gate: true,
            one_shot: false,
        };
        let mut s = LfoState::default();
        // After a quarter cycle, L ≈ sin(2π·0.25) = 1, R offset by half a
        // cycle ≈ sin(2π·0.75) = -1.
        let mut out = (0.0, 0.0);
        for _ in 0..250 {
            out = s.tick(&c, &p, 0.001);
        }
        assert!(out.0 > 0.9, "L = {}", out.0);
        assert!(out.1 < -0.9, "R = {}", out.1);
    }

    #[test]
    fn transport_lock_pins_phase() {
        let c = Curve::sine();
        let p = LfoParams {
            rate_hz: 3.7,
            phase: 0.0,
            st_phase: 0.0,
            lag: 0.0,
            gate: true,
            one_shot: false,
        };
        let mut s = LfoState::default();
        let (a, _) = s.tick_transport(&c, &p, 0.25, 0.001);
        assert!(a > 0.99, "quarter-cycle sine should be ~1, got {a}");
        // Same timeline phase → identical output, regardless of dt history.
        let mut s2 = LfoState::default();
        for _ in 0..17 {
            s2.tick(&c, &p, 0.013);
        }
        let (b, _) = s2.tick_transport(&c, &p, 0.25, 0.001);
        assert!((a - b).abs() < 1e-6);
    }

    #[test]
    fn one_shot_holds_final_value_and_retriggers() {
        let c = Curve::ramp(); // -1 at the start, +1 at the end
        let p = LfoParams {
            rate_hz: 2.0,
            phase: 0.0,
            st_phase: 0.0,
            lag: 0.0,
            gate: true,
            one_shot: true,
        };
        let mut s = LfoState::default();
        // Run well past one full cycle: a wrapping LFO would be mid-ramp again,
        // but one-shot must hold the ramp's end (+1).
        let mut out = (0.0, 0.0);
        for _ in 0..2000 {
            out = s.tick(&c, &p, 0.001);
        }
        assert!(
            out.0 > 0.9,
            "one-shot should hold the ramp end (+1), got {}",
            out.0
        );
        // Retrigger restarts the envelope at the beginning (-1).
        s.trig();
        let (a, _) = s.tick(&c, &p, 0.001);
        assert!(
            a < -0.9,
            "after trig, one-shot should restart near the start (-1), got {a}"
        );
        // A wrapping LFO over the same time lands back near the start, not held.
        let pw = LfoParams {
            one_shot: false,
            ..p
        };
        let mut sw = LfoState::default();
        let mut wout = (0.0, 0.0);
        for _ in 0..2000 {
            wout = sw.tick(&c, &pw, 0.001);
        }
        assert!(
            wout.0 < 0.5,
            "wrapping LFO should not be held at the end: {}",
            wout.0
        );
    }

    #[test]
    fn line_endpoints_exact_and_linear() {
        let mut c = Curve::sine();
        c.line(40, -0.5, 10, 0.7); // reversed order
        assert_eq!(c.0[10], 0.7);
        assert_eq!(c.0[40], -0.5);
        for i in 10..=40 {
            let want = 0.7 + (-0.5 - 0.7) * (i - 10) as f32 / 30.0;
            assert!((c.0[i] - want).abs() < 1e-5);
        }
        c.line(0, 5.0, 255, -5.0);
        assert_eq!((c.0[0], c.0[255]), (1.0, -1.0));
    }

    #[test]
    fn quantize_gives_flat_regions_at_means() {
        let mut c = Curve::ramp();
        let src = c.clone();
        c.quantize(4);
        for k in 0..4 {
            let r = k * 64..(k + 1) * 64;
            let mean = src.0[r.clone()].iter().sum::<f32>() / 64.0;
            for i in r {
                assert!((c.0[i] - mean).abs() < 1e-6);
            }
        }
        assert_ne!(c.0[63], c.0[64]);
    }

    #[test]
    fn shift_phase_round_trips() {
        let src = Curve::random_walk(5);
        let mut c = src.clone();
        c.shift_phase(0.25);
        assert!((c.0[64] - src.0[0]).abs() < 1e-3);
        c.shift_phase(-0.25);
        for i in 0..CURVE_LEN {
            assert!((c.0[i] - src.0[i]).abs() < 1e-3);
        }
    }

    #[test]
    fn presets_in_range_deterministic_and_distinct() {
        let all: Vec<Curve> = (0..N_PRESETS).map(Curve::preset).collect();
        for (i, c) in all.iter().enumerate() {
            assert!(c.0.iter().all(|v| (-1.0..=1.0).contains(v)), "preset {i}");
            assert_eq!(c.0, Curve::preset(i).0);
        }
        assert_eq!(Curve::preset(N_PRESETS + 3).0, all[3].0);
        for a in 0..N_PRESETS {
            for b in a + 1..N_PRESETS {
                assert_ne!(all[a].0, all[b].0, "presets {a} and {b} identical");
            }
        }
    }

    #[test]
    fn lag_slows_response() {
        let c = Curve::square();
        let fast = LfoParams {
            rate_hz: 0.0,
            phase: 0.0,
            st_phase: 0.0,
            lag: 0.0,
            gate: true,
            one_shot: false,
        };
        let slow = LfoParams { lag: 1.0, ..fast };
        let mut sf = LfoState::default();
        let mut ss = LfoState::default();
        let (f, _) = sf.tick(&c, &fast, 0.01);
        let (s, _) = ss.tick(&c, &slow, 0.01);
        assert!((f - 1.0).abs() < 1e-3); // no lag: instantly at the curve value
        assert!(s < 0.1, "lagged output jumped: {s}");
    }

    #[test]
    fn drift_stays_in_range_and_moves() {
        let mut d = DriftPair::default();
        let mut rng = Rng::new(9);
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for _ in 0..200_000 {
            let (l, _r) = d.tick(0.001, 1.0, &mut rng);
            min = min.min(l);
            max = max.max(l);
            assert!((-1.0..=1.0).contains(&l));
        }
        assert!(max - min > 0.5, "drift barely moved: {min}..{max}");
    }

    #[test]
    fn noise_mono_when_st_zero() {
        let n = NoisePair;
        let mut rng = Rng::new(3);
        for _ in 0..100 {
            let (l, r) = n.roll(0.0, &mut rng);
            assert_eq!(l, r);
        }
    }
}
