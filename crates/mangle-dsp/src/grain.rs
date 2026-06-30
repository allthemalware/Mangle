//! Grain primitives: the snapshot, the Shape/Skew envelope, and the
//! emission clock with Jitter. Parameter map §3; Torsion grain flow chart.

use crate::rng::Rng;

/// Everything a grain needs, frozen at creation time (the snapshot rule).
#[derive(Clone, Copy, Debug)]
pub struct GrainSnapshot {
    /// Which sample slot this grain reads from.
    pub slot: usize,
    /// Resolved read window (loop window after Loop Size scaling), absolute
    /// samples into the slot.
    pub wstart: usize,
    pub wlen: usize,
    /// Read/stretch algorithm, frozen like everything else.
    pub algo: crate::sample::Algo,
    /// Start position in the slot, in samples.
    pub start: f64,
    /// Signed playback increment per output sample (direction × pitch ratio).
    /// 0.0 freezes the grain on its start sample.
    pub increment: f64,
    /// Grain duration in output samples.
    pub length: u32,
    /// 0..1 channel crossfade (Torsion Pan semantics).
    pub pan: f32,
    /// 0..1 amplitude.
    pub level: f32,
    /// 0..1 envelope shape (cosine → rect).
    pub shape: f32,
    /// -1..1 envelope tilt.
    pub skew: f32,
    /// Which stereo channel emitted this grain (0 = L, 1 = R).
    pub channel: u8,
}

/// Per-grain envelope coefficients, derived once from the (frozen) Shape/Skew
/// snapshot: the warp `peak` (from Skew) and the power `exponent` (from Shape).
/// Computing these once per grain instead of per sample — and the `>= 1.0`
/// exponent check in `grain_envelope_at` skipping `powf` when Shape is 0 (the
/// default) — keeps the render hot path cheap.
#[inline]
pub fn grain_env_coeffs(shape: f32, skew: f32) -> (f32, f32) {
    let peak = (0.5 + 0.499 * skew.clamp(-1.0, 1.0)).clamp(0.001, 0.999);
    let exponent = 1.0 - 0.999 * shape.clamp(0.0, 1.0);
    (peak, exponent)
}

/// Amplitude envelope at normalized grain time `t` from precomputed
/// coefficients (see `grain_env_coeffs`). Skew warps time so the peak lands
/// early (pluck) or late (reverse swell); Shape morphs a raised cosine toward
/// a rectangle by flattening the window's power.
#[inline]
pub fn grain_envelope_at(t: f32, peak: f32, exponent: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let warped = if t < peak {
        0.5 * t / peak
    } else {
        0.5 + 0.5 * (t - peak) / (1.0 - peak)
    };
    let cos_win = 0.5 - 0.5 * (core::f32::consts::TAU * warped).cos();
    if exponent >= 1.0 {
        cos_win // Shape 0 (default): the raised cosine itself, no powf
    } else {
        cos_win.powf(exponent)
    }
}

/// Amplitude envelope from raw Shape/Skew. `t` is normalized grain time 0..1.
/// Convenience wrapper over `grain_env_coeffs` + `grain_envelope_at` for tests
/// and non-hot-path callers.
pub fn grain_envelope(t: f32, shape: f32, skew: f32) -> f32 {
    let (peak, exponent) = grain_env_coeffs(shape, skew);
    grain_envelope_at(t, peak, exponent)
}

/// Per-engine, per-channel emission clock. Counts down in samples; Jitter
/// adds 0..1 periods of random delay to each emission (timing only — it
/// never touches Length, per the manual).
#[derive(Clone, Copy, Debug)]
pub struct Emitter {
    next_in: f32,
}

impl Default for Emitter {
    fn default() -> Self {
        Self { next_in: 0.0 }
    }
}

impl Emitter {
    /// Walk `block` output samples; call `emit(offset)` for every grain due
    /// in this block. `rate_hz <= 0` means off.
    pub fn advance(
        &mut self,
        block: usize,
        rate_hz: f32,
        jitter: f32,
        sample_rate: f32,
        rng: &mut Rng,
        mut emit: impl FnMut(usize),
    ) {
        if rate_hz <= 0.0 {
            return;
        }
        let period = sample_rate / rate_hz;
        let mut remaining = block as f32;
        while self.next_in < remaining {
            remaining -= self.next_in;
            let offset = block as f32 - remaining;
            emit(offset as usize);
            self.next_in = (period * (1.0 + jitter.clamp(0.0, 1.0) * rng.f32())).max(1.0);
        }
        self.next_in -= remaining;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_bounds_and_endpoints() {
        for shape in [0.0, 0.5, 1.0] {
            for skew in [-1.0, 0.0, 1.0] {
                for i in 0..=100 {
                    let t = i as f32 / 100.0;
                    let v = grain_envelope(t, shape, skew);
                    assert!((0.0..=1.0001).contains(&v), "env({t},{shape},{skew}) = {v}");
                }
            }
        }
        // Cosine window: silent at the edges, full at the centre.
        assert!(grain_envelope(0.0, 0.0, 0.0) < 1e-3);
        assert!(grain_envelope(1.0, 0.0, 0.0) < 1e-3);
        assert!((grain_envelope(0.5, 0.0, 0.0) - 1.0).abs() < 1e-3);
        // Rectangle-ish at shape 1: loud almost immediately.
        assert!(grain_envelope(0.02, 1.0, 0.0) > 0.9);
    }

    #[test]
    fn skew_moves_the_peak() {
        let argmax = |skew: f32| {
            (0..=200)
                .map(|i| i as f32 / 200.0)
                .max_by(|a, b| {
                    grain_envelope(*a, 0.0, skew)
                        .partial_cmp(&grain_envelope(*b, 0.0, skew))
                        .unwrap()
                })
                .unwrap()
        };
        assert!(argmax(-1.0) < 0.05, "pluck peak too late");
        assert!(argmax(1.0) > 0.95, "reverse peak too early");
        assert!((argmax(0.0) - 0.5).abs() < 0.05);
    }

    #[test]
    fn envelope_matches_reference_formula() {
        // The hoisted coeffs + powf-skip must reproduce the original closed form.
        for shape in [0.0f32, 0.25, 1.0] {
            for skew in [-0.7f32, 0.0, 0.6] {
                for i in 0..=20 {
                    let t = i as f32 / 20.0;
                    let pk = (0.5 + 0.499 * skew).clamp(0.001, 0.999);
                    let w = if t < pk { 0.5 * t / pk } else { 0.5 + 0.5 * (t - pk) / (1.0 - pk) };
                    let cw = 0.5 - 0.5 * (core::f32::consts::TAU * w).cos();
                    let reference = cw.powf(1.0 - 0.999 * shape);
                    let got = grain_envelope(t, shape, skew);
                    assert!(
                        (got - reference).abs() < 1e-4,
                        "env mismatch t={t} shape={shape} skew={skew}: {got} vs {reference}"
                    );
                }
            }
        }
    }

    #[test]
    fn emitter_rate_is_exact_without_jitter() {
        let mut e = Emitter::default();
        let mut rng = Rng::new(1);
        let mut count = 0;
        // 1 second at 48 kHz, 100 Hz → exactly 100 grains, in 512-sample blocks.
        let mut done = 0;
        while done < 48_000 {
            let block = (48_000 - done).min(512);
            e.advance(block, 100.0, 0.0, 48_000.0, &mut rng, |_| count += 1);
            done += block;
        }
        assert_eq!(count, 100);
    }

    #[test]
    fn jitter_delays_but_keeps_average_rate_sane() {
        let mut e = Emitter::default();
        let mut rng = Rng::new(2);
        let mut count = 0u32;
        for _ in 0..200 {
            e.advance(4800, 100.0, 1.0, 48_000.0, &mut rng, |_| count += 1);
        }
        // 20 s at 100 Hz with full jitter → mean period 1.5× → ≈ 1333 grains.
        assert!((1000..1900).contains(&count), "count = {count}");
    }

    #[test]
    fn zero_rate_emits_nothing() {
        let mut e = Emitter::default();
        let mut rng = Rng::new(3);
        let mut count = 0;
        e.advance(48_000, 0.0, 0.0, 48_000.0, &mut rng, |_| count += 1);
        assert_eq!(count, 0);
    }
}
