//! Acceptance: O(1) parameter lookup, 1-based matrix row names, resolver cost.

use mangle_dsp::params::{pid, Module, ParamTable, Patch};
use mangle_dsp::resolve;
use std::time::Instant;

#[test]
fn every_id_maps_to_its_position() {
    let t = ParamTable::new();
    for (i, p) in t.infos.iter().enumerate() {
        assert_eq!(t.pos(p.id), i, "{}", p.name);
        assert_eq!(t.try_pos(p.id), Some(i), "{}", p.name);
    }
    assert_eq!(t.try_pos(pid(Module::Fx, 0, 4)), None);
    assert_eq!(t.try_pos(0x0102_0000), None);
    assert_eq!(t.try_pos(u32::MAX), None);
}

#[test]
fn matrix_rows_are_named_from_one() {
    let t = ParamTable::new();
    assert_eq!(t.get(pid(Module::Matrix, 0, 0)).name, "Mod 01 Source");
    assert_eq!(t.get(pid(Module::Matrix, 31, 5)).name, "Mod 32 Scale");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing is only meaningful in release builds"
)]
fn lane_params_resolves_quickly() {
    // Called 6x per sounding voice per 64-sample control chunk; the hashed
    // lookup made each call ~1.8 us (~13 % of a core at 16 voices).
    let t = ParamTable::new();
    let p = Patch::init(&t);
    let iters = 200_000u32;
    let start = Instant::now();
    let mut acc = 0.0f32;
    for i in 0..iters {
        let (lp, _) = resolve::lane_params(&p, &t, (i % 3) as u8, 120.0, 60.0, true, true);
        acc += lp.rate_hz;
    }
    let ns = start.elapsed().as_secs_f64() / iters as f64 * 1e9;
    assert!(acc > 0.0);
    assert!(
        ns < 400.0,
        "lane_params takes {ns:.0} ns per call (budget 400 ns)"
    );
}
