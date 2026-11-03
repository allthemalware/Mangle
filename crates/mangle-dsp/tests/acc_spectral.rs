//! Acceptance: Spectral-mode freeze keeps partials at their true frequency.

use mangle_dsp::engine::{GrainMode, Lane, LaneParams};
use mangle_dsp::sample::{SampleBank, SampleSlot};

const SR: f32 = 48_000.0;

fn sine_bank(freq: f32, n: usize) -> SampleBank {
    let d: Vec<f32> = (0..n)
        .map(|i| 0.5 * (std::f32::consts::TAU * freq * i as f32 / SR).sin())
        .collect();
    let mut b = SampleBank::new();
    b.slots[0] = Some(SampleSlot {
        data: [d.clone(), d],
        sample_rate: SR,
        root_note: 60,
        loop_start: 0,
        loop_end: n,
        gain: 1.0,
        mono: true,
    });
    b
}

fn render(p: LaneParams, bank: &SampleBank, secs: f32) -> Vec<f32> {
    let total = (secs * SR) as usize;
    let mut lane = Lane::new(7);
    let (mut l, mut r) = (vec![0.0f32; total], vec![0.0f32; total]);
    let mut done = 0;
    while done < total {
        let n = (total - done).min(64);
        lane.process(
            &[p, p],
            bank,
            (0.0, 0.0),
            SR,
            &mut l[done..done + n],
            &mut r[done..done + n],
            n,
        );
        done += n;
    }
    l
}

/// Strongest frequency in `lo..hi` by a fine Goertzel scan.
fn peak_freq(sig: &[f32], lo: f32, hi: f32) -> f32 {
    let mut best = (0.0f32, 0.0f64);
    let mut f = lo;
    while f <= hi {
        let c = 2.0 * (2.0 * std::f64::consts::PI * f as f64 / SR as f64).cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in sig {
            let s0 = x as f64 + c * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let power = s1 * s1 + s2 * s2 - c * s1 * s2;
        if power > best.1 {
            best = (f, power);
        }
        f += 0.25;
    }
    best.0
}

fn cents(f: f32, reference: f32) -> f32 {
    1200.0 * (f / reference).log2()
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt()
}

fn spectral(direction: f32) -> LaneParams {
    LaneParams {
        mode: GrainMode::Spectral,
        loop_on: false,
        key_track: 0.0,
        direction,
        ..Default::default()
    }
}

#[test]
fn frozen_partials_keep_their_pitch() {
    // Off-bin frequencies (bin spacing is ~46.9 Hz at 1024/48k): a freeze that
    // plays each bin at its centre frequency detunes these by up to ~280 cents.
    for f in [110.0f32, 147.0, 220.0, 333.0, 523.25] {
        let bank = sine_bank(f, 96_000);
        let out = render(spectral(0.0), &bank, 1.5);
        let tail = &out[24_000..];
        assert!(
            rms(tail) > 0.05,
            "{f} Hz freeze too quiet: rms {}",
            rms(tail)
        );
        let got = peak_freq(tail, f * 0.7, f * 1.3);
        let c = cents(got, f);
        assert!(c.abs() < 12.0, "{f} Hz frozen at {got} Hz ({c:+.1} cents)");
    }
}

#[test]
fn moving_playback_stays_in_tune() {
    for f in [110.0f32, 333.0] {
        let bank = sine_bank(f, 96_000);
        let out = render(spectral(1.0), &bank, 1.5);
        let got = peak_freq(&out[24_000..], f * 0.7, f * 1.3);
        let c = cents(got, f);
        assert!(c.abs() < 5.0, "{f} Hz moving at {got} Hz ({c:+.1} cents)");
    }
}

#[test]
fn freeze_after_motion_keeps_pitch() {
    // Play forward, then freeze mid-note: the held spectrum must stay in tune.
    let f = 147.0f32;
    let bank = sine_bank(f, 96_000);
    let total = (1.5 * SR) as usize;
    let mut lane = Lane::new(3);
    let (mut l, mut r) = (vec![0.0f32; total], vec![0.0f32; total]);
    let mut done = 0;
    while done < total {
        let n = (total - done).min(64);
        let dir = if done < 24_000 { 1.0 } else { 0.0 };
        let p = spectral(dir);
        lane.process(
            &[p, p],
            &bank,
            (0.0, 0.0),
            SR,
            &mut l[done..done + n],
            &mut r[done..done + n],
            n,
        );
        done += n;
    }
    let got = peak_freq(&l[36_000..], f * 0.7, f * 1.3);
    let c = cents(got, f);
    assert!(
        c.abs() < 12.0,
        "freeze after motion at {got} Hz ({c:+.1} cents)"
    );
}
