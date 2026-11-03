//! Background sample loading: decode, resample, hash and summarize on a
//! short-lived worker thread, then hand the result to the audio thread through
//! `MangleParams::pending_loads`. Nothing here runs on the audio thread.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mangle_dsp::sample::SampleSlot;
use mangle_dsp::state::SlotRef;

use crate::{summarize, EmbeddedSample, MangleParams, PendingLoad, WaveSummary};

/// Where a sample comes from.
#[derive(Clone, Debug)]
pub enum LoadSource {
    File(PathBuf),
    /// Raw original-file bytes embedded in the preset (parameter map §7).
    Embedded {
        bytes: Arc<Vec<u8>>,
        ext: Option<String>,
        path: String,
    },
    /// Hot-reload source: the worker reads `path` and uses it when its content
    /// hash equals `hash`, otherwise decodes `embedded`.
    FileOrEmbedded {
        path: String,
        hash: u64,
        embedded: EmbeddedSample,
    },
}

/// Per-slot load status for the editor (GUI-only; the audio thread never reads it).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SlotStatus {
    #[default]
    Empty,
    Loading,
    Loaded,
    Missing(String),
    Failed(String),
}

/// Shared loader bookkeeping, a field of `MangleParams`.
#[derive(Default)]
pub struct LoadState {
    /// Latest requested load per slot; a finishing worker publishes only if
    /// its generation is still the latest (newest request wins).
    pub generation: [AtomicU64; 6],
    pub status: Mutex<[SlotStatus; 6]>,
    /// Recent load notes for the editor's status line (newest last, at most
    /// `MAX_MESSAGES`): decode warnings and failed multi-drop files.
    pub messages: Mutex<VecDeque<String>>,
}

const MAX_MESSAGES: usize = 16;

/// A decoded, display-ready sample.
pub struct Decoded {
    pub sample: SampleSlot,
    pub path: String,
    pub content_hash: u64,
    pub summary: WaveSummary,
    pub warnings: Vec<String>,
}

fn set_status(params: &MangleParams, slot: usize, status: SlotStatus) {
    if let Ok(mut s) = params.loads.status.lock() {
        s[slot] = status;
    }
}

fn push_message(params: &MangleParams, msg: String) {
    if let Ok(mut m) = params.loads.messages.lock() {
        while m.len() >= MAX_MESSAGES {
            m.pop_front();
        }
        m.push_back(msg);
    }
}

/// Add a note to the editor's status messages.
pub fn note(params: &MangleParams, msg: String) {
    push_message(params, msg);
}

/// Decode on the calling thread (file or embedded bytes) at `target_rate`.
/// A missing file maps to `SlotStatus::Missing(path)`, other errors to `Failed(msg)`.
pub fn decode(source: &LoadSource, target_rate: f32) -> Result<Decoded, SlotStatus> {
    decode_resolved(&resolve(source).0, target_rate)
}

/// Settle a `FileOrEmbedded` source into a plain `File` or `Embedded` one by
/// reading the file; the flag says the preset's embedded bytes were chosen.
/// Reads and hashes the file: never call from the audio thread.
fn resolve(source: &LoadSource) -> (LoadSource, bool) {
    let LoadSource::FileOrEmbedded {
        path,
        hash,
        embedded,
    } = source
    else {
        return (source.clone(), false);
    };
    let file_matches = std::fs::read(path)
        .map(|b| mangle_loader::hash_bytes(&b) == *hash)
        .unwrap_or(false);
    if file_matches {
        (LoadSource::File(PathBuf::from(path)), false)
    } else {
        (
            LoadSource::Embedded {
                bytes: embedded.bytes.clone(),
                ext: embedded.ext.clone(),
                path: path.clone(),
            },
            true,
        )
    }
}

fn decode_resolved(source: &LoadSource, target_rate: f32) -> Result<Decoded, SlotStatus> {
    let (result, path) = match source {
        LoadSource::File(p) => (
            mangle_loader::load_sample(p, target_rate),
            p.to_string_lossy().into_owned(),
        ),
        LoadSource::Embedded { bytes, ext, path } => (
            mangle_loader::load_sample_from_bytes(bytes, ext.as_deref(), target_rate),
            path.clone(),
        ),
        LoadSource::FileOrEmbedded { .. } => unreachable!("resolved before decoding"),
    };
    match result {
        Ok(loaded) => {
            let summary = summarize(&loaded.slot);
            Ok(Decoded {
                sample: loaded.slot,
                path,
                content_hash: loaded.content_hash,
                summary,
                warnings: loaded.warnings,
            })
        }
        Err(mangle_loader::LoadError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(SlotStatus::Missing(path))
        }
        Err(e) => Err(SlotStatus::Failed(e.to_string())),
    }
}

/// Mark `slot` Loading and return the new generation.
pub fn begin(params: &MangleParams, slot: usize) -> u64 {
    let gen = params.loads.generation[slot].fetch_add(1, Ordering::AcqRel) + 1;
    set_status(params, slot, SlotStatus::Loading);
    gen
}

/// Publish a decoded sample for the audio thread, if `generation` is still the
/// latest for `slot`: waveform summary, `extra.slot_refs[slot]` (write only that
/// field, then bump `extra_gen`), push a `PendingLoad` (first drop any queued
/// entry for the slot and every consumed entry), status Loaded. Returns
/// whether it published. Never call from the audio thread.
pub fn publish(params: &MangleParams, slot: usize, generation: u64, decoded: Decoded) -> bool {
    let latest = || params.loads.generation[slot].load(Ordering::Acquire) == generation;
    if !latest() {
        return false;
    }
    let Decoded {
        sample,
        path,
        content_hash,
        summary,
        warnings,
    } = decoded;

    // Hold the queue lock across the final staleness check and the push so a
    // concurrent newer publish cannot be overtaken by this one.
    let Ok(mut queue) = params.pending_loads.lock() else {
        return false;
    };
    if !latest() {
        return false;
    }
    if let Ok(mut w) = params.waveforms.write() {
        w[slot] = Some(summary.clone());
    }
    if let Ok(mut extra) = params.extra.write() {
        extra.slot_refs[slot] = Some(SlotRef {
            path: path.clone(),
            content_hash,
        });
    }
    params.extra_gen.fetch_add(1, Ordering::Release);
    queue.retain(|p| !p.consumed && p.slot != slot);
    queue.push(PendingLoad {
        slot,
        sample: Box::new(sample),
        path,
        content_hash,
        summary,
        consumed: false,
    });
    drop(queue);
    set_status(params, slot, SlotStatus::Loaded);
    for w in warnings {
        push_message(params, format!("slot {}: {w}", slot + 1));
    }
    true
}

fn source_name(source: &LoadSource) -> String {
    let path = match source {
        LoadSource::File(p) => p.to_string_lossy(),
        LoadSource::Embedded { path, .. } | LoadSource::FileOrEmbedded { path, .. } => {
            std::borrow::Cow::Borrowed(path.as_str())
        }
    };
    std::path::Path::new(path.as_ref())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.into_owned())
}

fn status_message(source: &LoadSource, status: &SlotStatus) -> String {
    let err = match status {
        SlotStatus::Missing(_) => "file not found".to_string(),
        SlotStatus::Failed(m) => m.clone(),
        _ => String::new(),
    };
    format!("{}: {err}", source_name(source))
}

fn load_and_publish(params: &MangleParams, slot: usize, generation: u64, source: &LoadSource) {
    let rate = f32::from_bits(params.sr_bits.load(Ordering::Relaxed));
    let (source, from_preset) = resolve(source);
    let source = &source;
    match decode_resolved(source, rate) {
        Ok(decoded) => {
            if publish(params, slot, generation, decoded) && from_preset {
                push_message(params, format!("slot {}: from preset", slot + 1));
            }
        }
        Err(status) => {
            if params.loads.generation[slot].load(Ordering::Acquire) == generation {
                push_message(params, status_message(source, &status));
                set_status(params, slot, status);
            }
        }
    }
}

/// One worker thread: begin + decode + publish (or set the failure status).
pub fn spawn_load(params: Arc<MangleParams>, slot: usize, source: LoadSource) {
    let generation = begin(&params, slot);
    let spawned = std::thread::Builder::new()
        .name("mangle-load".into())
        .spawn({
            let params = params.clone();
            move || load_and_publish(&params, slot, generation, &source)
        });
    if spawned.is_err() {
        set_status(
            &params,
            slot,
            SlotStatus::Failed("could not start loader thread".into()),
        );
    }
}

/// Torsion multi-drop on one worker: load `paths` in order into successive slots
/// from `start`; a failed file does not consume a slot and touches no slot
/// status (its error goes to `LoadState::messages`); stops after slot 6.
pub fn spawn_multi_load(params: Arc<MangleParams>, start: usize, paths: Vec<PathBuf>) {
    let _ = std::thread::Builder::new()
        .name("mangle-load".into())
        .spawn(move || {
            let mut slot = start;
            for path in paths {
                if slot >= 6 {
                    break;
                }
                let source = LoadSource::File(path);
                let rate = f32::from_bits(params.sr_bits.load(Ordering::Relaxed));
                match decode(&source, rate) {
                    Ok(decoded) => {
                        let generation = begin(&params, slot);
                        publish(&params, slot, generation, decoded);
                        slot += 1;
                    }
                    Err(status) => push_message(&params, status_message(&source, &status)),
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn write_wav(path: &std::path::Path, frames: usize) {
        let mut data = Vec::new();
        for i in 0..frames {
            let v = ((i as f32 * 0.05).sin() * 12000.0) as i16;
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&48_000u32.to_le_bytes());
        out.extend_from_slice(&96_000u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        std::fs::write(path, out).unwrap();
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mangle-loading-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn wait_for(mut f: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn status(params: &MangleParams, slot: usize) -> SlotStatus {
        params.loads.status.lock().unwrap()[slot].clone()
    }

    fn queued(params: &MangleParams, slot: usize) -> usize {
        params
            .pending_loads
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.slot == slot)
            .count()
    }

    fn decoded(path: &std::path::Path) -> Decoded {
        decode(&LoadSource::File(path.to_path_buf()), 48_000.0).expect("decode")
    }

    #[test]
    fn spawn_load_publishes_valid_wav() {
        let dir = tmp_dir("valid");
        let wav = dir.join("a.wav");
        write_wav(&wav, 4800);
        let params = Arc::new(MangleParams::new());
        spawn_load(params.clone(), 2, LoadSource::File(wav.clone()));
        assert!(wait_for(|| status(&params, 2) == SlotStatus::Loaded));
        assert_eq!(queued(&params, 2), 1);
        assert!(params.waveforms.read().unwrap()[2].is_some());
        let extra = params.extra.read().unwrap();
        assert_eq!(
            extra.slot_refs[2].as_ref().unwrap().path,
            wav.to_string_lossy()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_path_sets_missing_status() {
        let dir = tmp_dir("missing");
        let nope = dir.join("nope.wav");
        let params = Arc::new(MangleParams::new());
        spawn_load(params.clone(), 0, LoadSource::File(nope.clone()));
        let want = SlotStatus::Missing(nope.to_string_lossy().into_owned());
        assert!(wait_for(|| status(&params, 0) == want));
        assert!(params.pending_loads.lock().unwrap().is_empty());
        assert!(params.extra.read().unwrap().slot_refs[0].is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_generation_is_not_published() {
        let dir = tmp_dir("stale");
        let wav = dir.join("a.wav");
        write_wav(&wav, 1000);
        let params = MangleParams::new();
        let old = begin(&params, 1);
        let new = begin(&params, 1);
        assert!(new > old);
        assert!(!publish(&params, 1, old, decoded(&wav)));
        assert_eq!(queued(&params, 1), 0);
        assert!(publish(&params, 1, new, decoded(&wav)));
        assert_eq!(queued(&params, 1), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn multi_load_skips_failed_files() {
        let dir = tmp_dir("multi");
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_wav(&a, 1000);
        write_wav(&b, 1000);
        let missing = dir.join("missing.wav");
        let params = Arc::new(MangleParams::new());
        spawn_multi_load(params.clone(), 1, vec![a, missing, b]);
        assert!(wait_for(|| {
            status(&params, 1) == SlotStatus::Loaded && status(&params, 2) == SlotStatus::Loaded
        }));
        assert_eq!(status(&params, 3), SlotStatus::Empty);
        assert_eq!(queued(&params, 1), 1);
        assert_eq!(queued(&params, 2), 1);
        assert_eq!(queued(&params, 3), 0);
        assert!(params.extra.read().unwrap().slot_refs[3].is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn multi_load_failure_goes_to_messages_not_status() {
        let dir = tmp_dir("msgs");
        let params = Arc::new(MangleParams::new());
        spawn_multi_load(params.clone(), 0, vec![dir.join("gone.wav")]);
        let have = |p: &MangleParams| !p.loads.messages.lock().unwrap().is_empty();
        assert!(wait_for(|| have(&params)));
        let msg = params.loads.messages.lock().unwrap().back().cloned();
        assert!(msg.unwrap().starts_with("gone.wav: "));
        assert_eq!(status(&params, 0), SlotStatus::Empty);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn publish_drops_consumed_and_stale_entries() {
        let dir = tmp_dir("drop");
        let wav = dir.join("a.wav");
        write_wav(&wav, 1000);
        let params = MangleParams::new();
        let g = begin(&params, 4);
        assert!(publish(&params, 4, g, decoded(&wav)));
        let g0 = begin(&params, 0);
        assert!(publish(&params, 0, g0, decoded(&wav)));
        {
            let mut q = params.pending_loads.lock().unwrap();
            for p in q.iter_mut().filter(|p| p.slot == 0) {
                p.consumed = true;
            }
        }
        let g = begin(&params, 4);
        assert!(publish(&params, 4, g, decoded(&wav)));
        let q = params.pending_loads.lock().unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].slot, 4);
        assert!(!q[0].consumed);
        drop(q);
        let _ = std::fs::remove_dir_all(dir);
    }
}
