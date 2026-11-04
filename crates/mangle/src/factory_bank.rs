//! Factory data discovery and indexing. All IO and cache access is off the audio thread.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use mangle_dsp::factory::{parse_preset, parse_sources, PresetFile, SourceEntry};

/// Find the folder containing `factory/`, in data-folder lookup order.
pub fn data_dir() -> Option<PathBuf> {
    find_data_dir(
        std::env::var_os("MANGLE_DATA").map(PathBuf::from),
        platform_data_home(),
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::current_exe().ok(),
    )
}

/// The per-user data folder for this OS (`$XDG_DATA_HOME` wins when set).
fn platform_data_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(p));
    }
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        None
    }
}

fn find_data_dir(
    explicit: Option<PathBuf>,
    xdg: Option<PathBuf>,
    home: Option<PathBuf>,
    executable: Option<PathBuf>,
) -> Option<PathBuf> {
    [
        explicit,
        xdg.map(|p| p.join("mangle")),
        home.map(|p| p.join(".local/share/mangle")),
        executable.and_then(|p| p.parent().map(|p| p.join("mangle"))),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.join("factory").is_dir())
}

#[derive(Debug)]
pub struct FactoryBank {
    /// Folder containing `factory/`.
    pub root: PathBuf,
    pub sources: Vec<SourceEntry>,
    /// Parsed presets in file-name order.
    pub presets: Vec<(String, PresetFile)>,
    pub warnings: Vec<String>,
}

impl FactoryBank {
    /// Read an explicit data folder without changing the process-wide cache.
    pub fn load(root: &Path) -> Result<Self, String> {
        let factory = root.join("factory");
        let index = factory.join("sources.tsv");
        let text =
            std::fs::read_to_string(&index).map_err(|e| format!("{}: {e}", index.display()))?;
        let sources = parse_sources(&text).map_err(|e| format!("{}: {e}", index.display()))?;
        let mut bank = Self {
            root: root.to_path_buf(),
            sources,
            presets: Vec::new(),
            warnings: Vec::new(),
        };
        let preset_dir = factory.join("presets");
        let entries = match std::fs::read_dir(&preset_dir) {
            Ok(entries) => entries,
            Err(e) => {
                bank.warnings.push(format!("{}: {e}", preset_dir.display()));
                return Ok(bank);
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) if entry.path().extension().is_some_and(|ext| ext == "mpreset") => {
                    paths.push(entry.path());
                }
                Ok(_) => {}
                Err(e) => bank.warnings.push(format!("{}: {e}", preset_dir.display())),
            }
        }
        paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for path in paths {
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| parse_preset(&text));
            match parsed {
                Ok(mut preset) => {
                    for (slot, id) in preset.slots.iter_mut().enumerate() {
                        if let Some(missing) =
                            id.filter(|id| !bank.sources.iter().any(|s| s.id == *id))
                        {
                            bank.warnings.push(format!(
                                "{}: slot {slot}: unknown source {missing:04}",
                                path.display()
                            ));
                            *id = None;
                        }
                    }
                    bank.presets.push((
                        path.file_name().unwrap().to_string_lossy().into_owned(),
                        preset,
                    ));
                }
                Err(e) => bank.warnings.push(format!("{}: {e}", path.display())),
            }
        }
        Ok(bank)
    }

    pub fn source_path(&self, id: u16) -> Option<PathBuf> {
        self.sources
            .iter()
            .find(|source| source.id == id)
            .map(|source| self.root.join("factory").join(&source.file))
    }
}

// The outer Option distinguishes an unattempted load from a missing bank.
static BANK: OnceLock<Mutex<Option<Option<Arc<FactoryBank>>>>> = OnceLock::new();

fn load_bank() -> Option<Arc<FactoryBank>> {
    FactoryBank::load(&data_dir()?).ok().map(Arc::new)
}

/// Lazily read and share the factory bank, caching absence until `reload_bank()`.
/// Never call from the audio thread.
pub fn bank() -> Option<Arc<FactoryBank>> {
    let mut cache = BANK.get_or_init(|| Mutex::new(None)).lock().ok()?;
    cache.get_or_insert_with(load_bank).clone()
}

/// Rediscover and reread the bank. Existing readers retain their snapshot.
/// Never call from the audio thread.
pub fn reload_bank() {
    if let Ok(mut cache) = BANK.get_or_init(|| Mutex::new(None)).lock() {
        *cache = Some(load_bank());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) struct Fixture(pub PathBuf);

    impl Fixture {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "mangle-factory-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let factory = root.join("factory");
            std::fs::create_dir_all(factory.join("sources")).unwrap();
            std::fs::create_dir_all(factory.join("presets")).unwrap();
            std::fs::write(
                factory.join("sources.tsv"),
                "id\tname\tcategory\tkind\tfile\troot\tlicense\torigin\n\
                 0001\tOne\tPads\tgen\tsources/0001.wav\t60\tgenerated\ttest\n\
                 0003\tThree\tFX\trec\tsources/0003.wav\t-1\tCC0-1.0\ttest\n",
            )
            .unwrap();
            let mut wav = Vec::new();
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&100u32.to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16u32.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&48_000u32.to_le_bytes());
            wav.extend_from_slice(&96_000u32.to_le_bytes());
            wav.extend_from_slice(&2u16.to_le_bytes());
            wav.extend_from_slice(&16u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&64u32.to_le_bytes());
            wav.extend_from_slice(&[0; 64]);
            for id in [1, 3] {
                std::fs::write(factory.join(format!("sources/{id:04}.wav")), &wav).unwrap();
            }
            for (name, text) in [
                (
                    "020-three.mpreset",
                    "mangle-preset 1\nname Three\ncategory FX\nslot 1 0003\n",
                ),
                (
                    "010-one.mpreset",
                    "mangle-preset 1\nname One\ncategory Pads\nslot 0 0001\nslot 5 0002\n",
                ),
                ("bad.mpreset", "not a preset"),
                ("ignored.txt", "not a preset"),
            ] {
                std::fs::write(factory.join("presets").join(name), text).unwrap();
            }
            Self(root)
        }

        pub(crate) fn bank(&self) -> Arc<FactoryBank> {
            Arc::new(FactoryBank::load(&self.0).unwrap())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn indexes_sources_and_sorted_presets_with_warnings() {
        let fixture = Fixture::new();
        let bank = fixture.bank();
        assert_eq!(bank.root, fixture.0);
        assert_eq!(bank.sources.len(), 2);
        assert_eq!(bank.presets.len(), 2);
        assert_eq!(bank.presets[0].0, "010-one.mpreset");
        assert_eq!(bank.presets[1].0, "020-three.mpreset");
        assert_eq!(
            bank.presets[0].1.slots,
            [Some(1), None, None, None, None, None]
        );
        assert_eq!(bank.presets[1].1.slots[1], Some(3));
        assert_eq!(bank.warnings.len(), 2);
        assert!(bank.warnings.iter().any(|w| w.contains("bad.mpreset")));
        assert!(bank
            .warnings
            .iter()
            .any(|w| w.contains("unknown source 0002")));
        assert_eq!(
            bank.source_path(1),
            Some(fixture.0.join("factory/sources/0001.wav"))
        );
        assert_eq!(bank.source_path(2), None);
    }

    #[test]
    fn discovery_uses_first_existing_factory_folder() {
        let fixture = Fixture::new();
        let explicit = fixture.0.clone();
        let xdg = fixture.0.join("xdg");
        let home = fixture.0.join("home");
        let exe = fixture.0.join("bin/plugin");
        let candidates = [
            explicit.clone(),
            xdg.join("mangle"),
            home.join(".local/share/mangle"),
            exe.parent().unwrap().join("mangle"),
        ];
        for candidate in &candidates {
            std::fs::create_dir_all(candidate.join("factory")).unwrap();
        }
        for expected in &candidates {
            assert_eq!(
                find_data_dir(
                    Some(explicit.clone()),
                    Some(xdg.clone()),
                    Some(home.clone()),
                    Some(exe.clone())
                ),
                Some(expected.clone())
            );
            std::fs::remove_dir_all(expected.join("factory")).unwrap();
        }
        assert_eq!(
            find_data_dir(Some(explicit), Some(xdg), Some(home), Some(exe)),
            None
        );
        assert_eq!(find_data_dir(None, None, None, None), None);
    }

    #[test]
    fn invalid_index_fails_but_missing_presets_preserves_sources() {
        let fixture = Fixture::new();
        std::fs::remove_dir_all(fixture.0.join("factory/presets")).unwrap();
        let bank = fixture.bank();
        assert_eq!(bank.sources.len(), 2);
        assert!(bank.presets.is_empty());
        assert_eq!(bank.warnings.len(), 1);
        std::fs::write(fixture.0.join("factory/sources.tsv"), "bad header").unwrap();
        assert!(FactoryBank::load(&fixture.0)
            .unwrap_err()
            .contains("sources.tsv"));
        std::fs::remove_file(fixture.0.join("factory/sources.tsv")).unwrap();
        assert!(FactoryBank::load(&fixture.0).is_err());
    }
}
