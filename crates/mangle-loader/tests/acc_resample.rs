//! Acceptance: the loader's resampler matches the reference windowed-sinc
//! output and is fast enough for long files.

use std::time::Instant;

/// The original direct-form resampler (per-tap sinc + Blackman, per-sample
/// weight normalization, edge samples clamped). Kept as the quality reference.
fn reference(src: &[f32], src_rate: f64, dst_rate: f64) -> Vec<f32> {
    if src.is_empty() {
        return Vec::new();
    }
    if (src_rate - dst_rate).abs() < 1e-9 {
        return src.to_vec();
    }
    let ratio = src_rate / dst_rate;
    let scale = ratio.max(1.0);
    const ZEROS: f64 = 12.0;
    let half = (ZEROS * scale).ceil() as isize;
    let cutoff = 0.92 / scale;
    let out_len = ((src.len() as f64) / ratio).floor() as usize;
    let last = src.len() as isize - 1;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let t = i as f64 * ratio;
        let it = t.floor() as isize;
        let frac = t - it as f64;
        let (mut acc, mut wsum) = (0.0f64, 0.0f64);
        for k in -half..=half {
            let x = k as f64 - frac;
            let y = std::f64::consts::PI * x * cutoff;
            let sinc = if y.abs() < 1e-12 { 1.0 } else { y.sin() / y };
            let u = x / half as f64;
            let w = if u.abs() <= 1.0 {
                0.42 + 0.5 * (std::f64::consts::PI * u).cos()
                    + 0.08 * (2.0 * std::f64::consts::PI * u).cos()
            } else {
                0.0
            };
            let h = sinc * w;
            acc += src[(it + k).clamp(0, last) as usize] as f64 * h;
            wsum += h;
        }
        out.push(if wsum.abs() > 1e-12 {
            (acc / wsum) as f32
        } else {
            0.0
        });
    }
    out
}

/// Deterministic white-ish noise in -1..1.
fn noise(n: usize, seed: u64) -> Vec<f32> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        })
        .collect()
}

#[test]
fn matches_reference_for_common_ratios() {
    for (src_rate, dst_rate) in [
        (44_100.0, 48_000.0),
        (96_000.0, 48_000.0),
        (48_000.0, 44_100.0),
        (22_050.0, 48_000.0),
        (32_000.0, 44_100.0),
    ] {
        let src = noise(src_rate as usize / 4, 99); // 0.25 s, includes both edges
        let want = reference(&src, src_rate, dst_rate);
        let got = mangle_loader::resample_channel(&src, src_rate, dst_rate);
        assert_eq!(got.len(), want.len(), "{src_rate}->{dst_rate}: length");
        let worst = got
            .iter()
            .zip(&want)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst < 1e-3,
            "{src_rate}->{dst_rate}: max deviation {worst} from reference"
        );
    }
}

#[test]
fn identity_when_rates_match() {
    let src = noise(1000, 5);
    assert_eq!(
        mangle_loader::resample_channel(&src, 48_000.0, 48_000.0),
        src
    );
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing is only meaningful in release builds"
)]
fn twenty_seconds_resample_quickly() {
    // The direct form needs ~6 s (44.1k) / ~10 s (96k) here; a 300 s stereo file
    // took minutes. Require at least a 6x speed-up with a wide safety margin.
    for (src_rate, budget) in [(44_100.0f64, 1.0f64), (96_000.0, 1.5)] {
        let src = noise((src_rate * 20.0) as usize, 3);
        let t = Instant::now();
        let out = mangle_loader::resample_channel(&src, src_rate, 48_000.0);
        let secs = t.elapsed().as_secs_f64();
        assert_eq!(out.len(), 960_000);
        assert!(
            secs < budget,
            "{src_rate}->48k: 20 s took {secs:.2} s (budget {budget} s)"
        );
    }
}
