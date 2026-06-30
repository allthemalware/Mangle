//! Minimal in-crate complex FFT — iterative radix-2 Cooley–Tukey, power-of-
//! two lengths only. Exists so the spectral grain mode (`spectral.rs`) needs
//! no external dependency, keeping `mangle-dsp` dependency-free.
//!
//! Twiddles and the bit-reversal permutation are precomputed once per size
//! (the plugin builds one `Fft` in `initialize`), so `transform` allocates
//! nothing and is safe to call on the audio thread.

/// Plan for a fixed transform length.
pub struct Fft {
    n: usize,
    /// Bit-reversal permutation.
    rev: Vec<usize>,
    /// `cos[k] = cos(-2π k / n)`, `sin[k] = sin(-2π k / n)` for k in 0..n/2.
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2, "FFT length must be a power of two ≥ 2");
        let bits = n.trailing_zeros();
        let rev = (0..n).map(|i| bit_reverse(i, bits)).collect();
        let mut cos = vec![0.0f32; n / 2];
        let mut sin = vec![0.0f32; n / 2];
        for k in 0..n / 2 {
            let a = -2.0 * core::f32::consts::PI * k as f32 / n as f32;
            cos[k] = a.cos();
            sin[k] = a.sin();
        }
        Self { n, rev, cos, sin }
    }

    #[allow(clippy::len_without_is_empty)] // length is a fixed power of two ≥ 2
    pub fn len(&self) -> usize {
        self.n
    }

    /// In-place transform of `(re, im)`. `inverse = true` uses the conjugate
    /// twiddles and applies **no** 1/N scaling — the caller divides by N.
    pub fn transform(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.n;
        debug_assert_eq!(re.len(), n);
        debug_assert_eq!(im.len(), n);

        // Reorder into bit-reversed index order.
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }

        // Butterflies, sub-transform length doubling each stage.
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len; // twiddle stride into cos/sin
            let mut base = 0;
            while base < n {
                let mut tw = 0;
                for j in 0..half {
                    let wr = self.cos[tw];
                    let wi = if inverse { -self.sin[tw] } else { self.sin[tw] };
                    let a = base + j;
                    let b = base + j + half;
                    let tr = wr * re[b] - wi * im[b];
                    let ti = wr * im[b] + wi * re[b];
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                    tw += step;
                }
                base += len;
            }
            len <<= 1;
        }
    }
}

fn bit_reverse(mut x: usize, bits: u32) -> usize {
    let mut r = 0;
    for _ in 0..bits {
        r = (r << 1) | (x & 1);
        x >>= 1;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ifft_scaled(f: &Fft, re: &mut [f32], im: &mut [f32]) {
        f.transform(re, im, true);
        let inv = 1.0 / f.len() as f32;
        for v in re.iter_mut() {
            *v *= inv;
        }
        for v in im.iter_mut() {
            *v *= inv;
        }
    }

    #[test]
    fn roundtrips() {
        let f = Fft::new(64);
        let orig: Vec<f32> = (0..64)
            .map(|i| (i as f32 * 0.3).sin() + 0.5 * (i as f32 * 1.1).cos())
            .collect();
        let mut re = orig.clone();
        let mut im = vec![0.0f32; 64];
        f.transform(&mut re, &mut im, false);
        ifft_scaled(&f, &mut re, &mut im);
        for (a, b) in orig.iter().zip(re.iter()) {
            assert!((a - b).abs() < 1e-4, "roundtrip drift {a} vs {b}");
        }
        for v in im.iter() {
            assert!(v.abs() < 1e-4, "imag leak {v}");
        }
    }

    #[test]
    fn cosine_has_a_single_peak_bin() {
        let n = 128;
        let f = Fft::new(n);
        let freq = 7usize;
        let mut re: Vec<f32> = (0..n)
            .map(|i| (core::f32::consts::TAU * freq as f32 * i as f32 / n as f32).cos())
            .collect();
        let mut im = vec![0.0f32; n];
        f.transform(&mut re, &mut im, false);
        let mag: Vec<f32> = (0..n).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        // Dominant bin in 1..n/2 must be `freq`, magnitude ≈ n/2.
        let peak = (1..n / 2).max_by(|a, b| mag[*a].partial_cmp(&mag[*b]).unwrap()).unwrap();
        assert_eq!(peak, freq, "peak at bin {peak}, expected {freq}");
        assert!((mag[freq] - n as f32 / 2.0).abs() < 1.0, "peak magnitude {}", mag[freq]);
    }

    #[test]
    fn matches_naive_dft() {
        let n = 32;
        let f = Fft::new(n);
        let x: Vec<f32> = (0..n).map(|i| ((i * 7 % 5) as f32) - 2.0).collect();
        let mut re = x.clone();
        let mut im = vec![0.0f32; n];
        f.transform(&mut re, &mut im, false);
        for k in 0..n {
            let mut dr = 0.0f64;
            let mut di = 0.0f64;
            for (t, &xt) in x.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * k as f64 * t as f64 / n as f64;
                dr += xt as f64 * a.cos();
                di += xt as f64 * a.sin();
            }
            assert!((re[k] as f64 - dr).abs() < 1e-2, "bin {k} re {} vs {dr}", re[k]);
            assert!((im[k] as f64 - di).abs() < 1e-2, "bin {k} im {} vs {di}", im[k]);
        }
    }
}
