//! Squeeze — the output gain leveler. Parameter map §1.
//!
//! Torsion's description: an upward compressor into a downward
//! compressor/limiter, keeping the texture at a consistent loudness no
//! matter what the grain engines are doing. Two stages, stereo-linked
//! (one gain for both channels — no image wobble), zero lookahead (the
//! plugin reports zero latency):
//!
//! 1. **Leveler** — a slow envelope drives makeup gain toward a target
//!    level, boost capped, gated below a silence floor so tails and
//!    silence are never amplified into noise.
//! 2. **Limiter** — instant-attack gain computer against a ceiling with a
//!    smooth release. Zero-lookahead, so inter-sample peaks can graze the
//!    ceiling; the ceiling sits below full scale to compensate.

const TARGET: f32 = 0.35;
const MAX_BOOST: f32 = 6.0; // ≈ +15.6 dB
const MAX_CUT: f32 = 0.1; // ≈ -20 dB of downward leveling
const GATE: f32 = 0.005; // ≈ -46 dBFS: below this, stop leveling
const CEILING: f32 = 0.95;

#[derive(Clone, Copy, Debug)]
pub struct Squeeze {
    env: f32,
    gain: f32,
    lim_gain: f32,
    env_coef: f32,
    gain_coef: f32,
    lim_release: f32,
}

impl Squeeze {
    pub fn new(sample_rate: f32) -> Self {
        let coef = |t: f32| 1.0 - (-1.0 / (t * sample_rate)).exp();
        Self {
            env: 0.0,
            gain: 1.0,
            lim_gain: 1.0,
            env_coef: coef(0.150),
            gain_coef: coef(0.400),
            lim_release: coef(0.080),
        }
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.gain = 1.0;
        self.lim_gain = 1.0;
    }

    /// Process a stereo block in place.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let x = a.abs().max(b.abs());
            self.env += (x - self.env) * self.env_coef;
            if self.env < 1e-20 {
                self.env = 0.0; // flush denormals
            }

            // Stage 1: leveler. Hold gain where it is while gated.
            if self.env > GATE {
                let desired = (TARGET / self.env).clamp(MAX_CUT, MAX_BOOST);
                self.gain += (desired - self.gain) * self.gain_coef;
            }
            let yl = *a * self.gain;
            let yr = *b * self.gain;

            // Stage 2: limiter. Instant attack, smooth release.
            let peak = yl.abs().max(yr.abs());
            let target = if peak > CEILING { CEILING / peak } else { 1.0 };
            if target < self.lim_gain {
                self.lim_gain = target;
            } else {
                self.lim_gain += (1.0 - self.lim_gain) * self.lim_release;
            }
            *a = yl * self.lim_gain;
            *b = yr * self.lim_gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, n: usize, sr: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (core::f32::consts::TAU * 220.0 * i as f32 / sr).sin())
            .collect()
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|x| (x * x) as f64).sum::<f64>() / v.len() as f64).sqrt() as f32
    }

    #[test]
    fn quiet_input_is_boosted_toward_target() {
        let sr = 48_000.0;
        let mut sq = Squeeze::new(sr);
        let mut l = sine(0.02, 96_000, sr);
        let mut r = l.clone();
        let in_rms = rms(&l);
        sq.process(&mut l, &mut r);
        let out_rms = rms(&l[48_000..]); // settled half
        assert!(out_rms > in_rms * 3.0, "boost too weak: {in_rms} → {out_rms}");
        for v in l.iter() {
            assert!(v.is_finite());
        }
    }

    #[test]
    fn loud_input_never_exceeds_ceiling() {
        let sr = 48_000.0;
        let mut sq = Squeeze::new(sr);
        let mut l = sine(1.5, 48_000, sr);
        let mut r = sine(1.5, 48_000, sr);
        sq.process(&mut l, &mut r);
        let peak = l.iter().chain(r.iter()).fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= CEILING + 1e-3, "peak {peak} above ceiling");
    }

    #[test]
    fn silence_stays_silent() {
        let sr = 48_000.0;
        let mut sq = Squeeze::new(sr);
        let mut l = vec![0.0f32; 48_000];
        let mut r = vec![0.0f32; 48_000];
        sq.process(&mut l, &mut r);
        assert!(l.iter().chain(r.iter()).all(|v| *v == 0.0));
        // Gain must not have wound up while gated.
        let mut probe_l = vec![0.5f32; 8];
        let mut probe_r = vec![0.5f32; 8];
        sq.process(&mut probe_l, &mut probe_r);
        assert!(probe_l[0] <= 0.55, "gain wound up during silence: {}", probe_l[0]);
    }

    #[test]
    fn stereo_linked_gain_preserves_image() {
        let sr = 48_000.0;
        let mut sq = Squeeze::new(sr);
        let mut l = sine(0.05, 96_000, sr);
        let mut r = vec![0.0f32; 96_000];
        sq.process(&mut l, &mut r);
        assert!(r.iter().all(|v| *v == 0.0), "silent channel must stay silent");
        assert!(rms(&l[48_000..]) > 0.05, "left should still be leveled up");
    }

    #[test]
    fn deterministic() {
        let sr = 48_000.0;
        let mk = || {
            let mut sq = Squeeze::new(sr);
            let mut l = sine(0.3, 24_000, sr);
            let mut r = sine(0.6, 24_000, sr);
            sq.process(&mut l, &mut r);
            (l, r)
        };
        assert_eq!(mk(), mk());
    }
}
