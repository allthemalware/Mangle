//! Acceptance: factory bank file formats (see FACTORY.md).
use mangle_dsp::factory::{self, SourceKind};

const SOURCES: &str = "id\tname\tcategory\tkind\tfile\troot\tlicense\torigin\n\
# comment\n\n\
0001\tNylon Pluck\tplucked\tgen\tsources/0001.wav\t57\tgenerated\tkarplus-strong f=220\n\
0042\tFrame Drum\tdrums\trec\tsources/0042.flac\t-1\tCC0-1.0\tVCSL Membranophones/frame.wav\n";

#[test]
fn parses_sources() {
    let v = factory::parse_sources(SOURCES).unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].id, 1);
    assert_eq!(v[0].kind, SourceKind::Generated);
    assert_eq!(v[0].root, 57);
    assert_eq!(v[1].id, 42);
    assert_eq!(v[1].kind, SourceKind::Recorded);
    assert_eq!(v[1].root, -1);
    assert_eq!(v[1].license, "CC0-1.0");
    assert_eq!(v[1].file, "sources/0042.flac");
}

#[test]
fn rejects_bad_sources() {
    let head = "id\tname\tcategory\tkind\tfile\troot\tlicense\torigin\n";
    for bad in [
        "0001\tA\tc\tgen\tf\t60\tgenerated\to\n0001\tB\tc\tgen\tf\t60\tgenerated\to\n", // dup id
        "00x1\tA\tc\tgen\tf\t60\tgenerated\to\n",
        "0001\tA\tc\tbogus\tf\t60\tgenerated\to\n",
        "0001\tA\tc\tgen\tf\t200\tgenerated\to\n", // root out of range
        "0001\tA\tc\tgen\tf\t60\n",                // too few columns
        "0001\tA\tc\tgen\t../x\t60\tgenerated\to\n", // escapes the folder
    ] {
        assert!(
            factory::parse_sources(&format!("{head}{bad}")).is_err(),
            "{bad}"
        );
    }
    assert!(factory::parse_sources("nope\n").is_err()); // header required
}

const PRESET: &str = "mangle-preset 1\n# c\nname Glass Choir\ncategory Pads\nauthor Mangle\n\
slot 0 0042\nslot 3 0001\np 010003 0.75\np 000020 1\n";

#[test]
fn parses_preset() {
    let p = factory::parse_preset(PRESET).unwrap();
    assert_eq!(p.name, "Glass Choir");
    assert_eq!(factory::CATEGORIES[p.category as usize], "Pads");
    assert_eq!(p.slots, [Some(42), None, None, Some(1), None, None]);
    assert_eq!(p.params, vec![(0x010003, 0.75), (0x20, 1.0)]);
}

#[test]
fn preset_round_trips() {
    let p = factory::parse_preset(PRESET).unwrap();
    assert_eq!(
        factory::parse_preset(&factory::write_preset(&p)).unwrap(),
        p
    );
}

#[test]
fn rejects_bad_presets() {
    for bad in [
        "name x\ncategory Pads\n",                  // no magic
        "mangle-preset 2\nname x\ncategory Pads\n", // unknown version
        "mangle-preset 1\nname x\ncategory Nope\n", // unknown category
        "mangle-preset 1\nname x\ncategory Pads\nslot 6 0001\n",
        "mangle-preset 1\nname x\ncategory Pads\np zzzzzz 0.5\n",
        "mangle-preset 1\nname x\ncategory Pads\np 000001 NaN\n",
        "mangle-preset 1\ncategory Pads\n", // name required
    ] {
        assert!(factory::parse_preset(bad).is_err(), "{bad}");
    }
}

#[test]
fn out_of_range_values_are_clamped() {
    let p =
        factory::parse_preset("mangle-preset 1\nname x\ncategory FX\np 000001 7\np 000002 -3\n")
            .unwrap();
    assert_eq!(p.params, vec![(1, 1.0), (2, 0.0)]);
}

#[test]
fn factory_paths() {
    assert_eq!(factory::factory_path(42), "factory:0042");
    assert_eq!(factory::parse_factory_path("factory:0042"), Some(42));
    assert_eq!(factory::parse_factory_path("factory:xx"), None);
    assert_eq!(factory::parse_factory_path("/home/x/a.wav"), None);
    assert_eq!(factory::CATEGORIES.len(), 8);
    assert_eq!(factory::CATEGORIES[0], "Pads");
}
