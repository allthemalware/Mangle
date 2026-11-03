//! Acceptance: Pos>Pitch across Loop-LFO wraps, grain display positions, and
//! the Granular → Spectral mode switch.

use mangle_dsp::curve::{Curve, LfoParams, LfoState};
use mangle_dsp::engine::{GrainMode, GrainView, Lane, LaneParams};
use mangle_dsp::sample::{SampleBank, SampleSlot};

const SR: f32 = 48_000.0;

fn bank_of(data: Vec<f32>, loop_start: usize, loop_end: usize) -> SampleBank {
    let mut b = SampleBank::new();
    b.slots[0] = Some(SampleSlot {
        data: [data.clone(), data],
        sample_rate: SR,
        root_note: 60,
        loop_start,
        loop_end,
        gain: 1.0,
        mono: true,
    });
    b
}

fn sine(freq: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 * (std::f32::consts::TAU * freq * i as f32 / SR).sin())
        .collect()
}

/// High-frequency energy (first difference) per 5 ms window of `l[skip..]`.
fn hf_windows(l: &[f32], skip: usize) -> Vec<f64> {
    l[skip..]
        .chunks(240)
        .map(|w| {
            w.windows(2)
                .map(|d| ((d[1] - d[0]) as f64).powi(2))
                .sum::<f64>()
        })
        .collect()
}

#[test]
fn pos_pitch_has_no_chirp_at_loop_lfo_wrap() {
    // A ramp Loop LFO at 0.5 Hz scans a 2 s window at 1x and wraps every 2 s.
    // Pos>Pitch must treat the wrap as continuous motion (or no motion), never
    // as a -1 window jump that clamps grains to -32x read speed.
    let n = 96_000;
    let bank = bank_of(sine(1000.0, n), 0, n);
    let curve = Curve::ramp();
    let lp = LfoParams {
        rate_hz: 0.5,
        phase: 0.0,
        st_phase: 0.0,
        lag: 0.0,
        gate: true,
        one_shot: false,
    };
    let p = LaneParams {
        pos_pitch: true,
        direction: 1.0,
        loop_on: true,
        rate_hz: 200.0,
        key_track: 0.0,
        ..Default::default()
    };
    let mut lfo = LfoState::default();
    let mut lane = Lane::new(3);
    let total = (5.0 * SR) as usize;
    let (mut l, mut r) = (vec![0.0f32; total], vec![0.0f32; total]);
    let mut done = 0;
    while done < total {
        let k = (total - done).min(64);
        let scan = lfo.tick(&curve, &lp, k as f32 / SR);
        lane.process(
            &[p, p],
            &bank,
            scan,
            SR,
            &mut l[done..done + k],
            &mut r[done..done + k],
            k,
        );
        done += k;
    }
    let hf = hf_windows(&l, 48_000);
    let mut sorted = hf.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2].max(1e-12);
    let spikes: Vec<usize> = (0..hf.len()).filter(|&i| hf[i] > 20.0 * median).collect();
    assert!(
        spikes.is_empty(),
        "HF spikes (>20x median) in 5 ms windows {spikes:?}"
    );
    // Still a tape drag: scan at 1x over a 1 kHz source reads at ~1 kHz.
    let rms = (l.iter().map(|v| v * v).sum::<f32>() / l.len() as f32).sqrt();
    assert!(rms > 0.05, "Pos>Pitch output too quiet: {rms}");
}

/// Render `secs` of a Pos>Pitch lane scanned by a ramp Loop LFO.
fn pos_pitch_render(lp: LfoParams, secs: f32) -> Vec<f32> {
    let n = 96_000;
    let bank = bank_of(sine(1000.0, n), 0, n);
    let curve = Curve::ramp();
    let p = LaneParams {
        pos_pitch: true,
        direction: 1.0,
        loop_on: true,
        rate_hz: 200.0,
        key_track: 0.0,
        ..Default::default()
    };
    let mut lfo = LfoState::default();
    let mut lane = Lane::new(3);
    let total = (secs * SR) as usize;
    let (mut l, mut r) = (vec![0.0f32; total], vec![0.0f32; total]);
    let mut done = 0;
    while done < total {
        let k = (total - done).min(64);
        let scan = lfo.tick(&curve, &lp, k as f32 / SR);
        lane.process(
            &[p, p],
            &bank,
            scan,
            SR,
            &mut l[done..done + k],
            &mut r[done..done + k],
            k,
        );
        done += k;
    }
    l
}

#[test]
fn pos_pitch_has_no_chirp_with_a_slow_loop_lfo() {
    // 0.05 Hz over a 2 s window: tape speed 0.1x, while the ramp's closing
    // segment (1/256 of the cycle, ~78 ms) sweeps back at about -25x, inside
    // the +/-32x clamp. Phase 0.95 brings that wrap in at ~0.92 s.
    let lp = LfoParams {
        rate_hz: 0.05,
        phase: 0.95,
        st_phase: 0.0,
        lag: 0.0,
        gate: true,
        one_shot: false,
    };
    let l = pos_pitch_render(lp, 3.0);
    let hf = hf_windows(&l, 12_000);
    let mut sorted = hf.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2].max(1e-12);
    let spikes: Vec<usize> = (0..hf.len()).filter(|&i| hf[i] > 20.0 * median).collect();
    assert!(
        spikes.is_empty(),
        "HF spikes (>20x median) in 5 ms windows {spikes:?}"
    );
}

#[test]
fn pos_pitch_follows_a_triangle_scan_reversal() {
    // A triangle scan reverses direction at equal speed: real tape motion that
    // must keep driving the read speed (no jump suppression at the turn).
    let n = 96_000;
    let bank = bank_of(sine(1000.0, n), 0, n);
    let curve = Curve::triangle();
    let lp = LfoParams {
        rate_hz: 0.25,
        phase: 0.0,
        st_phase: 0.0,
        lag: 0.0,
        gate: true,
        one_shot: false,
    };
    let p = LaneParams {
        pos_pitch: true,
        direction: 1.0,
        loop_on: true,
        rate_hz: 200.0,
        key_track: 0.0,
        ..Default::default()
    };
    let mut lfo = LfoState::default();
    let mut lane = Lane::new(3);
    let total = (4.0 * SR) as usize;
    let (mut l, mut r) = (vec![0.0f32; total], vec![0.0f32; total]);
    let mut done = 0;
    while done < total {
        let k = (total - done).min(64);
        let scan = lfo.tick(&curve, &lp, k as f32 / SR);
        lane.process(
            &[p, p],
            &bank,
            scan,
            SR,
            &mut l[done..done + k],
            &mut r[done..done + k],
            k,
        );
        done += k;
    }
    // Triangle at 0.25 Hz over a 2 s window: |speed| = 2 * 0.25 * 2 s = 1x, so
    // the 1 kHz source reads at ~1 kHz throughout, including after the turn.
    let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
    for (a, b) in [(24_000usize, 72_000usize), (120_000, 168_000)] {
        assert!(
            rms(&l[a..b]) > 0.05,
            "silent stretch {a}..{b}: tape motion was suppressed"
        );
    }
}

#[test]
fn reported_grain_positions_stay_inside_their_window() {
    // Loop window = 25 %..50 % of the slot. Grains start late in the window and
    // read past its end, wrapping inside it; the display must wrap them too.
    let n = 96_000;
    let bank = bank_of(sine(220.0, n), n / 4, n / 2);
    let p = LaneParams {
        loop_on: false,
        position: 0.9,
        rate_hz: 10.0,
        key_track: 0.0,
        ..Default::default()
    };
    let mut lane = Lane::new(5);
    let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
    let mut views = [GrainView::default(); 64];
    let mut seen = 0;
    for _ in 0..1500 {
        l.fill(0.0);
        r.fill(0.0);
        lane.process(&[p, p], &bank, (0.0, 0.0), SR, &mut l, &mut r, 64);
        let k = lane.report_grains(&bank, &mut views);
        for v in &views[..k] {
            seen += 1;
            assert!(
                (0.249..=0.501).contains(&v.pos),
                "grain displayed at {} outside the 0.25..0.5 window",
                v.pos
            );
        }
    }
    assert!(seen > 0, "no grains reported");
}

#[test]
fn switching_to_spectral_fades_live_grains() {
    // Granular grains on a DC source, then switch the lane to Spectral with
    // Level 0 (the vocoder itself is silent): the old grains must fade out
    // over a few hundred samples, not stop dead on the switch sample.
    let n = 96_000;
    let bank = bank_of(vec![1.0; n], 0, n);
    let gran = LaneParams {
        loop_on: false,
        key_track: 0.0,
        ..Default::default()
    };
    let spec = LaneParams {
        mode: GrainMode::Spectral,
        level: 0.0,
        ..gran
    };
    let mut lane = Lane::new(9);
    let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
    for _ in 0..300 {
        l.fill(0.0);
        r.fill(0.0);
        lane.process(&[gran, gran], &bank, (0.0, 0.0), SR, &mut l, &mut r, 64);
    }
    let before = l[63];
    assert!(before > 0.5, "granular warm-up too quiet: {before}");
    l.fill(0.0);
    r.fill(0.0);
    lane.process(&[spec, spec], &bank, (0.0, 0.0), SR, &mut l, &mut r, 64);
    assert!(
        l[0] > 0.5 * before,
        "grains cut dead on the mode switch: {} -> {}",
        before,
        l[0]
    );
    let mut last = 0.0;
    for _ in 0..16 {
        l.fill(0.0);
        r.fill(0.0);
        lane.process(&[spec, spec], &bank, (0.0, 0.0), SR, &mut l, &mut r, 64);
        last = l[63];
    }
    assert!(
        last.abs() < 1e-3,
        "old grains still sounding 1024 samples after the switch: {last}"
    );
}
