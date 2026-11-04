//! Acceptance: the shipped `factory/` folder indexes cleanly and every source decodes.
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn pilot_bank_loads_and_every_source_decodes() {
    let bank = mangle::FactoryBank::load(&repo_root()).expect("factory/ indexes");
    assert!(bank.sources.len() >= 32, "got {}", bank.sources.len());
    assert!(bank.warnings.is_empty(), "{:?}", bank.warnings);
    // The audio is rebuilt by tools/build_factory.sh and not committed; without it
    // (a fresh checkout, CI) only the index and presets are checked.
    if !repo_root().join("factory/sources").is_dir() {
        eprintln!("factory audio not built; skipping decode check");
        return;
    }
    // Decoding is slow in debug builds: check an evenly spaced sample unless
    // MANGLE_FACTORY_FULL=1 asks for every source.
    let full = std::env::var("MANGLE_FACTORY_FULL").is_ok_and(|v| v == "1");
    let stride = if full {
        1
    } else {
        (bank.sources.len() / 48).max(1)
    };
    for s in bank.sources.iter().step_by(stride) {
        let path = bank.source_path(s.id).expect("path");
        let loaded = mangle_loader::load_sample(&path, 48_000.0)
            .unwrap_or_else(|e| panic!("{:04} {}: {e}", s.id, s.name));
        assert!(loaded.slot.len() > 4_800, "{:04} too short", s.id);
        if s.kind == mangle_dsp::factory::SourceKind::Recorded {
            assert!(s.license == "CC0-1.0", "{:04} licence {}", s.id, s.license);
        }
    }
}

#[test]
fn pilot_presets_use_known_sources_and_params() {
    let bank = mangle::FactoryBank::load(&repo_root()).expect("factory/ indexes");
    let table = mangle_dsp::params::ParamTable::new();
    let mut per_cat = [0usize; 8];
    for (file, p) in &bank.presets {
        per_cat[p.category as usize] += 1;
        for s in p.slots.iter().flatten() {
            assert!(bank.source_path(*s).is_some(), "{file}: unknown source {s}");
        }
        for (id, _) in &p.params {
            assert!(
                table.try_pos(*id).is_some(),
                "{file}: unknown param {id:06x}"
            );
        }
    }
    assert!(
        per_cat.iter().all(|&n| n >= 2),
        "need >=2 presets per category: {per_cat:?}"
    );
}
