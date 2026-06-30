//! Drawable curves, curve-driven stereo LFOs, Drift and Noise mod sources.
//! Parameter map §4–§6, §9.

use crate::rng::Rng;

pub const CURVE_LEN: usize = 256;

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
            self.running_phase = (self.running_phase + p.rate_hz * dt).rem_euclid(1.0);
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
        self.running_phase = phase.rem_euclid(1.0);
        self.emit(curve, p, dt)
    }

    fn emit(&mut self, curve: &Curve, p: &LfoParams, dt: f32) -> (f32, f32) {
        let l_raw = curve.sample(self.running_phase + p.phase);
        let r_raw = curve.sample(self.running_phase + p.phase + p.st_phase * 0.5);
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

/// One Noise unit: uniform random per *grain* (no smoothing). The engine
/// calls `roll` once per grain snapshot per channel pair.
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
        let p = LfoParams { rate_hz: 1.0, phase: 0.0, st_phase: 1.0, lag: 0.0, gate: true };
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
        let p = LfoParams { rate_hz: 3.7, phase: 0.0, st_phase: 0.0, lag: 0.0, gate: true };
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
    fn lag_slows_response() {
        let c = Curve::square();
        let fast = LfoParams { rate_hz: 0.0, phase: 0.0, st_phase: 0.0, lag: 0.0, gate: true };
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
