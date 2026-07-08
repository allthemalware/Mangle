//! Master resonant filter — a Cytomic-style trapezoidal state-variable filter,
//! ported from Skrongle's `Svf` (src/dsp.hpp). Cheap and stable when the cutoff
//! is modulated hard. One `Svf` per output channel; it runs after the grain sum
//! and before Squeeze (parameter map §Fx / the output chain in `lib.rs`).

/// Filter response, chosen by the Fx `Type` param. The order is load-bearing
/// (persisted as a plain step index) — append new responses at the end only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType {
    Lowpass,
    Bandpass,
    Highpass,
    Notch,
}

impl FilterType {
    /// Map the stepped `Type` param index to a response (out-of-range → Lowpass).
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => FilterType::Bandpass,
            2 => FilterType::Highpass,
            3 => FilterType::Notch,
            _ => FilterType::Lowpass,
        }
    }
}

/// Trapezoidal SVF. `set` recomputes the coefficients from a cutoff/resonance;
/// `process` runs one sample. State is two integrators (`ic1`, `ic2`).
#[derive(Clone, Copy, Debug)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
    g: f32,
    k: f32,
}

impl Default for Svf {
    fn default() -> Self {
        Self { ic1: 0.0, ic2: 0.0, g: 0.1, k: 1.0 }
    }
}

impl Svf {
    /// Recompute coefficients. `cutoff_hz` is clamped to `[20, 0.47·sr]`;
    /// `reso` is 0..1, mapped to `k = 2 − 1.96·reso` (kept just short of
    /// self-oscillation so hard modulation stays stable).
    pub fn set(&mut self, cutoff_hz: f32, reso: f32, sample_rate: f32) {
        let c = cutoff_hz.clamp(20.0, sample_rate * 0.47);
        self.g = (core::f32::consts::PI * c / sample_rate).tan();
        self.k = 2.0 - 1.96 * reso.clamp(0.0, 1.0);
    }

    /// Process one sample through the selected response.
    #[inline]
    pub fn process(&mut self, v0: f32, ftype: FilterType) -> f32 {
        let a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        let a2 = self.g * a1;
        let a3 = self.g * a2;
        let v3 = v0 - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        // Denormal flush on the integrator states (Skrongle's original lacks
        // this; the port keeps Mangle's one-pole hardening discipline).
        if self.ic1.abs() < 1e-20 {
            self.ic1 = 0.0;
        }
        if self.ic2.abs() < 1e-20 {
            self.ic2 = 0.0;
        }
        match ftype {
            FilterType::Lowpass => v2,
            FilterType::Bandpass => v1,
            FilterType::Highpass => v0 - self.k * v1 - v2,
            FilterType::Notch => v0 - self.k * v1,
        }
    }

    pub fn clear(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settled RMS of a sine at `freq` through the filter at `cutoff`/`reso`.
    fn tone_rms(freq: f32, cutoff: f32, reso: f32, ftype: FilterType, sr: f32) -> f32 {
        let mut f = Svf::default();
        f.set(cutoff, reso, sr);
        let n = 9_600usize;
        let mut acc = 0.0f64;
        for i in 0..n {
            let x = (core::f32::consts::TAU * freq * i as f32 / sr).sin();
            let y = f.process(x, ftype);
            if i >= n / 2 {
                acc += (y as f64) * (y as f64);
            }
        }
        (acc / (n / 2) as f64).sqrt() as f32
    }

    #[test]
    fn lowpass_attenuates_highs() {
        let sr = 48_000.0;
        let pass = tone_rms(200.0, 1000.0, 0.0, FilterType::Lowpass, sr);
        let stop = tone_rms(8000.0, 1000.0, 0.0, FilterType::Lowpass, sr);
        assert!(pass > 0.5, "LP killed its passband: {pass}");
        assert!(stop < 0.2 * pass, "LP failed to attenuate 8 kHz: {stop} vs {pass}");
    }

    #[test]
    fn highpass_attenuates_lows() {
        let sr = 48_000.0;
        let pass = tone_rms(10000.0, 2000.0, 0.0, FilterType::Highpass, sr);
        let stop = tone_rms(100.0, 2000.0, 0.0, FilterType::Highpass, sr);
        assert!(pass > 0.5, "HP killed its passband: {pass}");
        assert!(stop < 0.2 * pass, "HP passed the lows: {stop} vs {pass}");
    }

    #[test]
    fn bandpass_peaks_at_center() {
        let sr = 48_000.0;
        let below = tone_rms(100.0, 1000.0, 0.6, FilterType::Bandpass, sr);
        let center = tone_rms(1000.0, 1000.0, 0.6, FilterType::Bandpass, sr);
        let above = tone_rms(9000.0, 1000.0, 0.6, FilterType::Bandpass, sr);
        assert!(center > below && center > above, "BP not centered: {below}/{center}/{above}");
    }

    #[test]
    fn stable_under_hard_reso_and_fast_modulation() {
        let sr = 48_000.0;
        let mut f = Svf::default();
        for i in 0..96_000usize {
            // Sweep the cutoff every sample at maximum resonance.
            let cutoff = 50.0 + 15_000.0 * (0.5 + 0.5 * (i as f32 * 0.02).sin());
            f.set(cutoff, 1.0, sr);
            let x = (i as f32 * 0.03).sin();
            let y = f.process(x, FilterType::Lowpass);
            assert!(y.is_finite(), "filter went non-finite at {i}: {y}");
            assert!(y.abs() < 100.0, "filter ran away at {i}: {y}");
        }
    }

    #[test]
    fn clear_resets_state() {
        let sr = 48_000.0;
        let mut f = Svf::default();
        f.set(500.0, 0.9, sr);
        for i in 0..2000 {
            f.process((i as f32 * 0.1).sin(), FilterType::Lowpass);
        }
        f.clear();
        // After clear, the first output of silence is exactly zero.
        assert_eq!(f.process(0.0, FilterType::Lowpass), 0.0);
    }
}
