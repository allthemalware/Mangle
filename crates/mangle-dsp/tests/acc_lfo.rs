//! Acceptance: one-shot LFO terminal behaviour and Noise cadence.

use mangle_dsp::curve::{Curve, LfoParams, LfoState};
use mangle_dsp::modengine::GlobalMod;
use mangle_dsp::params::{ParamTable, Patch};

fn one_shot(phase: f32, st_phase: f32) -> LfoParams {
    LfoParams {
        rate_hz: 2.0,
        phase,
        st_phase,
        lag: 0.0,
        gate: true,
        one_shot: true,
    }
}

#[test]
fn one_shot_holds_final_point_for_any_phase() {
    let c = Curve::ramp(); // final drawn point = +0.992
    for phase in [0.0f32, 0.25, 0.5, 0.75, 0.99] {
        let p = one_shot(phase, 0.0);
        let mut s = LfoState::default();
        let mut out = (0.0, 0.0);
        for _ in 0..3000 {
            out = s.tick(&c, &p, 0.001);
        }
        assert!(
            out.0 > 0.95,
            "phase {phase}: one-shot held {} instead of the final point",
            out.0
        );
    }
}

#[test]
fn one_shot_right_channel_also_holds_final_point() {
    let c = Curve::ramp();
    let p = one_shot(0.5, 1.0); // R offset by half a cycle
    let mut s = LfoState::default();
    let mut out = (0.0, 0.0);
    for _ in 0..3000 {
        out = s.tick(&c, &p, 0.001);
    }
    assert!(out.0 > 0.95, "L held {}", out.0);
    assert!(out.1 > 0.95, "R held {}", out.1);
}

#[test]
fn one_shot_retrigger_restarts_at_phase() {
    let c = Curve::ramp();
    let p = one_shot(0.5, 0.0);
    let mut s = LfoState::default();
    for _ in 0..3000 {
        s.tick(&c, &p, 0.001);
    }
    s.trig();
    let (a, _) = s.tick(&c, &p, 0.001);
    // Phase 0.5 of the ramp is 0.0; one 1 ms tick at 2 Hz adds ~0.004.
    assert!(
        a.abs() < 0.05,
        "retriggered one-shot should restart at Phase (≈0.0), got {a}"
    );
    let mut out = (0.0, 0.0);
    for _ in 0..3000 {
        out = s.tick(&c, &p, 0.001);
    }
    assert!(
        out.0 > 0.95,
        "retriggered one-shot should hold the final point again, got {}",
        out.0
    );
}

#[test]
fn transport_one_shot_runs_from_phase_and_holds_within_each_cycle() {
    let c = Curve::ramp();
    let mut s = LfoState::default();
    // Phase 0: cycle position 0.2 reads curve 0.2 (= -0.6); 0.9999 holds.
    let p0 = one_shot(0.0, 0.0);
    let (v, _) = s.tick_transport(&c, &p0, 0.2, 0.001);
    assert!((v + 0.6).abs() < 0.02, "phase 0 @0.2: {v}");
    let (v, _) = s.tick_transport(&c, &p0, 0.9999, 0.001);
    assert!(v > 0.95, "phase 0 @0.9999 should hold: {v}");
    // Phase 0.5: starts mid-curve, reaches the end half-way through the cycle and holds there.
    let p5 = one_shot(0.5, 0.0);
    let (v, _) = s.tick_transport(&c, &p5, 0.2, 0.001);
    assert!(
        (v - 0.4).abs() < 0.02,
        "phase 0.5 @0.2 should read curve 0.7 (=0.4): {v}"
    );
    let (v, _) = s.tick_transport(&c, &p5, 0.6, 0.001);
    assert!(
        v > 0.95,
        "phase 0.5 @0.6 should already hold the final point: {v}"
    );
    let (v, _) = s.tick_transport(&c, &p5, 0.9999, 0.001);
    assert!(v > 0.95, "phase 0.5 @0.9999 should hold: {v}");
    // A new transport cycle re-fires from Phase.
    let (v, _) = s.tick_transport(&c, &p5, 0.0, 0.001);
    assert!(
        v.abs() < 0.02,
        "new cycle should restart at Phase (0.0): {v}"
    );
}

#[test]
fn looping_lfo_phase_offset_still_wraps() {
    let c = Curve::ramp();
    let p = LfoParams {
        one_shot: false,
        ..one_shot(0.5, 0.0)
    };
    let mut s = LfoState::default();
    let mut out = (0.0, 0.0);
    for _ in 0..125 {
        out = s.tick(&c, &p, 0.001); // 0.25 cycle at 2 Hz
    }
    // running 0.25 + phase 0.5 = 0.75 → ramp value 0.5 (unchanged behaviour).
    assert!(
        (out.0 - 0.5).abs() < 0.02,
        "looping LFO with phase: {}",
        out.0
    );
    let mut s2 = LfoState::default();
    for _ in 0..400 {
        out = s2.tick(&c, &p, 0.001); // 0.8 cycle: 0.8 + 0.5 wraps to 0.3
    }
    assert!(
        (out.0 + 0.4).abs() < 0.02,
        "looping LFO must wrap past the end: {}",
        out.0
    );
}

fn curves() -> [Curve; 6] {
    [
        Curve::sine(),
        Curve::sine(),
        Curve::sine(),
        Curve::sine(),
        Curve::sine(),
        Curve::sine(),
    ]
}

#[test]
fn noise_cadence_is_independent_of_segmentation() {
    // Same seed, same elapsed time, different control-tick splits (as note
    // events split host blocks). Noise must re-roll on its own fixed clock, so
    // both instances hold identical values at every common time point.
    let t = ParamTable::new();
    let p = Patch::init(&t);
    let cs = curves();
    let sr = 48_000.0f32;
    let mut a = GlobalMod::new(42);
    let mut b = GlobalMod::new(42);
    for step in 0..750 {
        a.tick(&p, &t, &cs, 120.0, 64.0 / sr, None);
        b.tick(&p, &t, &cs, 120.0, 10.0 / sr, None);
        b.tick(&p, &t, &cs, 120.0, 54.0 / sr, None);
        assert_eq!(a.last_noise, b.last_noise, "noise diverged at chunk {step}");
    }
}

#[test]
fn noise_rerolls_at_about_750_hz() {
    let t = ParamTable::new();
    let p = Patch::init(&t);
    let cs = curves();
    let mut g = GlobalMod::new(7);
    let mut changes = 0;
    let mut last = g.last_noise[0].0;
    for _ in 0..48_000 {
        g.tick(&p, &t, &cs, 120.0, 1.0 / 48_000.0, None);
        if g.last_noise[0].0 != last {
            changes += 1;
            last = g.last_noise[0].0;
        }
    }
    assert!(
        (740..=760).contains(&changes),
        "noise re-rolled {changes} times in 1 s, expected ~750"
    );
}
