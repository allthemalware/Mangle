//! Acceptance: state format v3 (persisted pre-RND patch, A/B patch, last factory preset).
use mangle_dsp::state::{self, ExtraState, SlotRef, Snapshot};

fn snap(seed: f32) -> Snapshot {
    let mut s = Snapshot {
        values: (0..40)
            .map(|i| ((i as f32 * 0.037 + seed) % 1.0).abs())
            .collect(),
        slot_refs: Default::default(),
    };
    s.slot_refs[1] = Some(SlotRef {
        path: "factory:0042".into(),
        content_hash: 7,
    });
    s
}

#[test]
fn version_is_three() {
    assert_eq!(state::VERSION, 3);
}

#[test]
fn defaults_have_no_v3_fields() {
    let s = ExtraState::default();
    assert!(s.pre_roll.is_none() && s.ab.is_none());
    assert!(!s.ab_active);
    assert_eq!(s.factory_preset, None);
}

#[test]
fn v3_fields_round_trip() {
    let mut s = ExtraState::default();
    s.pre_roll = Some(snap(0.1));
    s.ab = Some(snap(0.6));
    s.ab_active = true;
    s.factory_preset = Some(513);
    let back = state::deserialize(&state::serialize(&s)).expect("round trip");
    assert_eq!(back.pre_roll, s.pre_roll);
    assert_eq!(back.ab, s.ab);
    assert!(back.ab_active);
    assert_eq!(back.factory_preset, Some(513));
}

#[test]
fn v2_blob_loads_with_defaults() {
    let mut blob = state::serialize(&ExtraState::default());
    // A default v3 state appends exactly one flags byte after the v2 fields.
    blob.pop();
    blob[4..6].copy_from_slice(&2u16.to_le_bytes());
    let back = state::deserialize(&blob).expect("v2 loads");
    assert!(back.pre_roll.is_none() && back.ab.is_none());
    assert_eq!(back.factory_preset, None);
}

#[test]
fn existing_fields_keep_their_bytes() {
    // v3 only appends: the v2 prefix of a v3 blob must equal the v2 serialisation.
    let mut a = ExtraState::default();
    a.embed_samples = true;
    a.instance_seed = 99;
    let plain = state::serialize(&a);
    a.pre_roll = Some(snap(0.3));
    let with = state::serialize(&a);
    assert_eq!(&with[6..plain.len() - 1], &plain[6..plain.len() - 1]);
}

#[test]
fn hostile_snapshots_are_rejected_or_sanitised() {
    let mut s = ExtraState::default();
    s.pre_roll = Some(Snapshot {
        values: vec![f32::NAN, 2.0, -1.0],
        slot_refs: Default::default(),
    });
    let back = state::deserialize(&state::serialize(&s)).expect("loads");
    let v = back.pre_roll.expect("kept").values;
    assert!(v.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)));
    // A truncated blob must not panic.
    let blob = state::serialize(&s);
    for cut in 0..blob.len() {
        let _ = state::deserialize(&blob[..cut]);
    }
}
