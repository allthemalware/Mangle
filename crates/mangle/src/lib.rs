//! Mangle CLAP shell (nih-plug).
//!
//! Per block: snapshot host params into the base patch, decode the 32-slot
//! matrix once, then run a control-chunk loop (≤64 samples, split at note-event
//! timings so note onsets are sample-accurate). Each chunk applies
//! the **global pass** (destinations on the six global LFOs) from last-tick
//! source values, ticks the global sources (LFOs, Drift, Noise, env follower),
//! then per voice applies the **voice pass per channel** so L and R diverge
//! (Torsion stereo modulation) — each grain engine gets two parameter sets and
//! renders in its selected mode (Grains / Stretch / Cloud / Spectral). Loop
//! Position lands as a window shift, Loop LFO feedback uses a one-block delay,
//! Macros (with per-note offsets) feed the matrix, and the pressure / tuning
//! note expressions drive the Pressure source and the pitch path. Output chain:
//! stereo width → master filter → Squeeze → master level → env-follower feed.
//!
//! Remaining simplification: per-channel modulation of an LFO's *rate* resolves
//! from the left channel (stereo LFOs still split via St.Phase).
//!
//! `API:` comments mark the nih-plug call sites most likely to need an
//! API-drift fix against a given checkout (see BUILD_NOTES.md).

use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use mangle_dsp::curve::Curve;
use mangle_dsp::engine::{MODE_NAMES, QUANT_NAMES};
use mangle_dsp::filter::Svf;
use mangle_dsp::matrix;
use mangle_dsp::modengine::{
    apply_global, apply_voice, decode_matrix, fill_global_voice_sources, fill_voice_sources,
    modulated_filter, DecodedRow, GlobalMod, SourceValues, VoiceSourceInputs,
};
use mangle_dsp::params::{
    self as mp, flags, Map, Module, ParamInfo, ParamTable, Patch, LFO_TRIG_IDX,
};
use mangle_dsp::resolve;
use mangle_dsp::rng::Rng;
use mangle_dsp::sample::{SampleBank, SampleSlot, ALGO_NAMES};
use mangle_dsp::squeeze::Squeeze;

/// Master-filter response names (Fx `Type` param), display order = step index.
const FILTER_TYPE_NAMES: [&str; 4] = ["LP", "BP", "HP", "Notch"];

/// Control-chunk size in samples: modulation tick rate and the upper bound of
/// any one render segment (segments also split at note-event timings).
const CTRL_CHUNK: usize = 64;
use mangle_dsp::state::SlotRef;
use mangle_dsp::state::{self, ExtraState};
use mangle_dsp::voice::{Voice, VoiceManager};

mod editor;
pub mod feed;
pub mod loading;

use feed::{EngineFeed, FeedFrame};

/// Longest loop window, in seconds of the slot's sample.
/// Key's normalized value below which Global On starts the drone.
pub(crate) const DRONE_KEY_MAX_NORM: f32 = 1e-4;
pub(crate) const MAX_LOOP_SECONDS: f32 = 100.0;

/// Per-slot display data for the waveform strip (peaks of the mono mix).
/// GUI-only; never serialized.
#[derive(Clone)]
pub struct WaveSummary {
    pub peaks: Vec<(f32, f32)>,
    pub len_samples: usize,
}

/// A decoded sample handed from the GUI thread (where file IO + resampling
/// happen) to the audio thread (which just swaps it into the bank).
pub struct PendingLoad {
    pub slot: usize,
    pub sample: Box<SampleSlot>,
    pub path: String,
    pub content_hash: u64,
    /// Waveform summary, computed on the GUI thread at enqueue time so the
    /// audio-thread drain stays allocation- and lock-free.
    pub summary: WaveSummary,
    /// Set by the audio thread once the sample has been swapped into the
    /// bank. The entry then holds the *previous* slot contents (if any) and
    /// stays queued until the GUI thread reclaims it — the audio thread must
    /// never deallocate, so it hands the garbage back instead of dropping it.
    pub consumed: bool,
}

// ------------------------------------------------------------- params ---

#[derive(Clone, Copy)]
pub(crate) enum PSlot {
    F(usize),
    I(usize),
    B(usize),
}

pub struct MangleParams {
    pub table: ParamTable,
    pub(crate) floats: Vec<FloatParam>,
    pub(crate) ints: Vec<IntParam>,
    pub(crate) bools: Vec<BoolParam>,
    /// Table position → which vec the host param lives in.
    pub(crate) slots: Vec<PSlot>,
    /// Editor window state (size); GUI-only.
    pub editor_state: std::sync::Arc<EguiState>,
    /// Waveform display summaries, filled by the shell on load.
    pub waveforms: RwLock<[Option<WaveSummary>; 6]>,
    /// Non-param state (curves, locks, random settings, slot refs).
    /// Edited by the GUI/main thread; the audio thread syncs copies via
    /// the generation counter and a non-blocking try_read.
    pub extra: RwLock<ExtraState>,
    pub extra_gen: AtomicU64,
    /// Set by the GUI (e.g. RND) to ask the audio thread to hard-stop all
    /// voices on its next block — prevents grains/notes hanging over a
    /// randomize. Consumed (cleared) in `process`.
    pub panic: AtomicBool,
    /// GUI LFO trigger requests, consumed once per block: bits 0-2 restart
    /// Loop LFO 1-3 in every gated voice, bits 3-8 restart LFO 1-6.
    pub lfo_trig_requests: AtomicU32,
    /// Drag-and-drop / dialog loads: the GUI decodes + resamples, pushes
    /// here, and the audio thread swaps the sample into the bank on its
    /// next block (no audio-thread file IO). Drained via try_lock.
    pub pending_loads: std::sync::Mutex<Vec<PendingLoad>>,
    /// Active sample rate (bits), published by `initialize` so the GUI can
    /// resample dropped files to match. 0 until the first activate.
    pub sr_bits: AtomicU32,
    /// File-dialog results: the picker runs on its own thread (never
    /// blocking the GUI render) and pushes (slot, path) here; the editor
    /// polls this each frame and loads via the same path as drag-drop.
    pub dialog_results: std::sync::Mutex<Vec<(usize, std::path::PathBuf)>>,
    /// Slots with a file dialog currently open, so the button can show a
    /// busy state and we don't stack pickers.
    pub dialog_open: [AtomicBool; 6],
    /// Realtime engine-activity feed for the editor's displays. Written by
    /// the audio thread only while the editor is open; GUI-only, never
    /// serialized.
    pub feed: EngineFeed,
    /// Background-load bookkeeping (generations + per-slot status).
    pub loads: loading::LoadState,
    /// Original file bytes already read for embedding, per slot, keyed by
    /// path + content hash; avoids re-reading files on every state save.
    pub embed_cache: std::sync::Mutex<[Option<EmbedCacheEntry>; 6]>,
    /// Samples found in a loaded preset (`mangle_embed_<slot>` keys).
    pub(crate) embedded: std::sync::Mutex<[Option<EmbeddedSample>; 6]>,
}

/// (path, content hash, original file bytes).
pub type EmbedCacheEntry = (String, u64, Arc<Vec<u8>>);

/// Original file bytes carried inside a preset.
#[derive(Clone, Debug)]
pub struct EmbeddedSample {
    pub ext: Option<String>,
    pub hash: u64,
    pub bytes: Arc<Vec<u8>>,
}

fn group_name(info: &ParamInfo) -> String {
    match info.module {
        Module::Global => "Global".to_string(),
        Module::Macro => "Macros".to_string(),
        Module::Grain => format!("Grain {}", info.instance + 1),
        Module::LoopLfo => format!("Loop LFO {}", info.instance + 1),
        Module::Lfo => format!("LFO {}", info.instance + 1),
        Module::Drift => "Drift".to_string(),
        Module::Noise => "Noise".to_string(),
        Module::Slot => format!("Slot {}", info.instance + 1),
        Module::Matrix => "Matrix".to_string(),
        Module::Fx => "Filter".to_string(),
    }
}

fn note_name(v: i32) -> String {
    const N: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let v = v.clamp(0, 127);
    format!("{}{}", N[(v % 12) as usize], v / 12 - 2)
}

/// Display formatter for a stepped parameter, by table identity.
fn int_formatter(info: &ParamInfo) -> Arc<dyn Fn(i32) -> String + Send + Sync> {
    match (info.module, info.index) {
        (Module::Grain, 3) | (Module::LoopLfo, 4) | (Module::Lfo, 4) => {
            Arc::new(|v| mp::DIV_NAMES[(v.clamp(0, 30)) as usize].to_string())
        }
        (Module::Grain, mp::GRAIN_ALGO_IDX) => {
            Arc::new(|v| ALGO_NAMES[(v.clamp(0, 3)) as usize].to_string())
        }
        (Module::Grain, mp::GRAIN_MODE_IDX) => {
            let last = MODE_NAMES.len() as i32 - 1;
            Arc::new(move |v| MODE_NAMES[v.clamp(0, last) as usize].to_string())
        }
        (Module::Grain, mp::GRAIN_QUANT_IDX) => {
            let last = QUANT_NAMES.len() as i32 - 1;
            Arc::new(move |v| QUANT_NAMES[v.clamp(0, last) as usize].to_string())
        }
        (Module::Matrix, 0) | (Module::Matrix, 5) => {
            Arc::new(|v| matrix::source_name(v.max(0) as u16))
        }
        (Module::Matrix, 2) | (Module::Matrix, 4) => {
            Arc::new(|v| matrix::dest_name(v.max(0) as u16))
        }
        (Module::Slot, 0) => Arc::new(note_name),
        (Module::Fx, 1) => Arc::new(|v| FILTER_TYPE_NAMES[v.clamp(0, 3) as usize].to_string()),
        _ => Arc::new(|v| v.to_string()),
    }
}

impl MangleParams {
    pub fn new() -> Self {
        let table = ParamTable::new();
        let mut floats = Vec::new();
        let mut ints = Vec::new();
        let mut bools = Vec::new();
        let mut slots = Vec::with_capacity(table.len());

        for info in table.infos.iter() {
            match info.map {
                Map::Bool => {
                    let mut p = BoolParam::new(info.name.clone(), info.default_display >= 0.5);
                    if info.flags & flags::A == 0 {
                        p = p.non_automatable();
                    }
                    if info.flags & flags::PN != 0 {
                        p = p.with_poly_modulation_id(info.id);
                    }
                    bools.push(p);
                    slots.push(PSlot::B(bools.len() - 1));
                }
                Map::Steps { count } => {
                    let mut p = IntParam::new(
                        info.name.clone(),
                        info.default_display as i32,
                        IntRange::Linear {
                            min: 0,
                            max: count as i32 - 1,
                        },
                    )
                    .with_value_to_string(int_formatter(info));
                    if info.flags & flags::A == 0 {
                        p = p.non_automatable();
                    }
                    if info.flags & flags::PN != 0 {
                        p = p.with_poly_modulation_id(info.id);
                    }
                    ints.push(p);
                    slots.push(PSlot::I(ints.len() - 1));
                }
                Map::Int { min, max } => {
                    let mut p = IntParam::new(
                        info.name.clone(),
                        info.default_display as i32,
                        IntRange::Linear { min, max },
                    )
                    .with_value_to_string(int_formatter(info));
                    if info.flags & flags::A == 0 {
                        p = p.non_automatable();
                    }
                    if info.flags & flags::PN != 0 {
                        p = p.with_poly_modulation_id(info.id);
                    }
                    ints.push(p);
                    slots.push(PSlot::I(ints.len() - 1));
                }
                _ => {
                    // Continuous: exposed normalized; the table's Map owns
                    // the display math so dsp and host always agree.
                    let map = info.map;
                    let unit = info.unit;
                    let mut p = FloatParam::new(
                        info.name.clone(),
                        info.default_norm,
                        FloatRange::Linear { min: 0.0, max: 1.0 },
                    )
                    .with_value_to_string(Arc::new(move |n| {
                        let d = map.to_display(n);
                        if unit.is_empty() {
                            format!("{d:.2}")
                        } else {
                            format!("{d:.2} {unit}")
                        }
                    }))
                    .with_string_to_value(Arc::new(move |s| {
                        s.split_whitespace()
                            .next()
                            .and_then(|t| t.parse::<f32>().ok())
                            .map(|d| map.to_norm(d))
                    }));
                    if info.flags & flags::A == 0 {
                        p = p.non_automatable();
                    }
                    if info.flags & flags::PN != 0 {
                        p = p.with_poly_modulation_id(info.id);
                    }
                    floats.push(p);
                    slots.push(PSlot::F(floats.len() - 1));
                }
            }
        }

        Self {
            table,
            floats,
            ints,
            bools,
            slots,
            editor_state: EguiState::from_size(1000, 760),
            waveforms: RwLock::new([None, None, None, None, None, None]),
            extra: RwLock::new(ExtraState::default()),
            extra_gen: AtomicU64::new(1),
            panic: AtomicBool::new(false),
            lfo_trig_requests: AtomicU32::new(0),
            pending_loads: std::sync::Mutex::new(Vec::new()),
            sr_bits: AtomicU32::new(48_000f32.to_bits()),
            dialog_results: std::sync::Mutex::new(Vec::new()),
            dialog_open: [
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
            ],
            feed: EngineFeed::default(),
            loads: loading::LoadState::default(),
            embed_cache: std::sync::Mutex::new(Default::default()),
            embedded: std::sync::Mutex::new(Default::default()),
        }
    }

    /// Snapshot every host param into a normalized patch.
    fn read_into(&self, patch: &mut Patch) {
        for (pos, slot) in self.slots.iter().enumerate() {
            patch.values[pos] = match slot {
                PSlot::F(i) => self.floats[*i].value(),
                PSlot::I(i) => self.table.infos[pos]
                    .map
                    .to_norm(self.ints[*i].value() as f32),
                PSlot::B(i) => {
                    if self.bools[*i].value() {
                        1.0
                    } else {
                        0.0
                    }
                }
            };
        }
    }
}

impl Default for MangleParams {
    fn default() -> Self {
        Self::new()
    }
}

impl MangleParams {
    /// Original bytes of the slot's referenced file, from the cache, the
    /// embedded store or disk; only returned when they hash to the ref's hash.
    fn embed_bytes(&self, slot: usize, r: &SlotRef) -> Option<Arc<Vec<u8>>> {
        if let Ok(c) = self.embed_cache.lock() {
            if let Some((p, h, b)) = &c[slot] {
                if *p == r.path && *h == r.content_hash {
                    return Some(b.clone());
                }
            }
        }
        let from_preset = self.embedded.lock().ok().and_then(|e| {
            e[slot]
                .as_ref()
                .filter(|e| e.hash == r.content_hash)
                .map(|e| e.bytes.clone())
        });
        let bytes = match from_preset {
            Some(b) => b,
            None => Arc::new(std::fs::read(&r.path).ok()?),
        };
        if mangle_loader::hash_bytes(&bytes) != r.content_hash {
            return None;
        }
        if let Ok(mut c) = self.embed_cache.lock() {
            c[slot] = Some((r.path.clone(), r.content_hash, bytes.clone()));
        }
        Some(bytes)
    }

    fn serialize_embedded(&self, map: &mut BTreeMap<String, String>) {
        let Ok(st) = self.extra.read() else { return };
        if !st.embed_samples {
            return;
        }
        for (i, r) in st.slot_refs.iter().enumerate() {
            let Some(r) = r else { continue };
            let Some(bytes) = self.embed_bytes(i, r) else {
                nih_log!(
                    "Mangle: slot {} not embedded: '{}' unreadable or changed",
                    i + 1,
                    r.path
                );
                continue;
            };
            let ext = std::path::Path::new(&r.path)
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default();
            map.insert(
                format!("mangle_embed_{i}"),
                format!("{ext}:{}", mangle_loader::b64_encode(&bytes)),
            );
        }
    }

    fn deserialize_embedded(&self, serialized: &BTreeMap<String, String>) {
        let mut found: [Option<EmbeddedSample>; 6] = Default::default();
        for (i, entry) in found.iter_mut().enumerate() {
            let Some((ext, b64)) = serialized
                .get(&format!("mangle_embed_{i}"))
                .and_then(|v| v.split_once(':'))
            else {
                continue;
            };
            let Some(bytes) = mangle_loader::b64_decode(b64) else {
                continue;
            };
            *entry = Some(EmbeddedSample {
                ext: (!ext.is_empty()).then(|| ext.to_string()),
                hash: mangle_loader::hash_bytes(&bytes),
                bytes: Arc::new(bytes),
            });
        }
        if let Ok(mut e) = self.embedded.lock() {
            *e = found;
        }
    }
}

// API: `Params` is an unsafe trait in nih-plug (param_map hands out raw
// ParamPtrs that must stay valid; our vecs are never touched after new()).
unsafe impl Params for MangleParams {
    fn param_map(&self) -> Vec<(String, ParamPtr, String)> {
        self.slots
            .iter()
            .enumerate()
            .map(|(pos, slot)| {
                let info = &self.table.infos[pos];
                let ptr = match slot {
                    PSlot::F(i) => self.floats[*i].as_ptr(),
                    PSlot::I(i) => self.ints[*i].as_ptr(),
                    PSlot::B(i) => self.bools[*i].as_ptr(),
                };
                (format!("p{:06x}", info.id), ptr, group_name(info))
            })
            .collect()
    }

    // API: default-empty hooks on the Params trait; nih-plug stores these
    // alongside the param values in the host state blob.
    fn serialize_fields(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        if let Ok(st) = self.extra.read() {
            map.insert(
                "mangle_state".to_string(),
                state::hex_encode(&state::serialize(&st)),
            );
        }
        self.serialize_embedded(&mut map);
        map
    }

    fn deserialize_fields(&self, serialized: &BTreeMap<String, String>) {
        self.deserialize_embedded(serialized);
        if let Some(hex) = serialized.get("mangle_state") {
            if let Some(st) = state::hex_decode(hex).and_then(|b| state::deserialize(&b)) {
                if let Ok(mut w) = self.extra.write() {
                    *w = st;
                }
                self.extra_gen.fetch_add(1, Ordering::Release);
            }
        }
    }
}

// -------------------------------------------------------------- plugin ---

pub struct Mangle {
    params: Arc<MangleParams>,
    vm: VoiceManager,
    bank: SampleBank,
    gmod: GlobalMod,
    loop_curves: [Curve; 3],
    lfo_curves: [Curve; 6],
    base: Patch,
    scratch_g: Patch,
    scratch_v: Patch,
    scratch_l: Patch,
    scratch_r: Patch,
    rows: [DecodedRow; 32],
    n_rows: usize,
    /// Pitch bend per MIDI channel, in semitones.
    bend: [f32; 16],
    squeeze: Squeeze,
    /// Master output filter, one SVF per channel (Module::Fx). Runs after the
    /// grain sum, before Squeeze.
    filter: [Svf; 2],
    /// Smoothed filter cutoff (Hz) per channel, ramped toward the (matrix-
    /// modulated, per-channel) target across control chunks so sweeps don't
    /// zipper. Two values so stereo filter modulation diverges L/R.
    filt_cutoff: [f32; 2],
    /// Smoothed filter resonance (0..1) per channel, same smoothing as the cutoff.
    filt_reso: [f32; 2],
    /// Smoothed master Level / Stereo width. The map's ~5 ms smoothing contract
    /// covers these continuous, non-grain params; applying the resolved value
    /// per block unramped would zipper on automation.
    cur_level: f32,
    cur_stereo: f32,
    smooth_coef: f32,
    /// Last seen `extra` generation (curve sync).
    seen_gen: u64,
    /// Previous block's LFO 1-6 Trig values, for rising-edge detection.
    prev_lfo_trig: [bool; 6],
    rng: Rng,
    sample_rate: f32,
    /// Path and content hash of what each bank slot holds (empty path = no
    /// sample referenced), and the sample rate those samples were prepared at.
    loaded: [(String, u64); 6],
    loaded_rate: f32,
    /// True from the first `process` call until `deactivate`.
    processing: bool,
    /// Instance seed the voices, global modulators and RNG were built from.
    built_seed: u64,
}

impl Default for Mangle {
    fn default() -> Self {
        let params = Arc::new(MangleParams::new());
        let base = Patch::init(&params.table);
        Self {
            vm: VoiceManager::new(8, 0x4D41_4E47), // "MANG"
            bank: SampleBank::new(),
            gmod: GlobalMod::new(0x6D6F_6421), // "mod!"
            loop_curves: [Curve::ramp(), Curve::ramp(), Curve::ramp()],
            lfo_curves: [
                Curve::sine(),
                Curve::sine(),
                Curve::sine(),
                Curve::sine(),
                Curve::sine(),
                Curve::sine(),
            ],
            scratch_g: base.clone(),
            scratch_v: base.clone(),
            scratch_l: base.clone(),
            scratch_r: base.clone(),
            base,
            rows: [DecodedRow::default(); 32],
            n_rows: 0,
            bend: [0.0; 16],
            squeeze: Squeeze::new(48_000.0),
            filter: [Svf::default(); 2],
            filt_cutoff: [20_000.0; 2],
            filt_reso: [0.0; 2],
            cur_level: 1.0,
            cur_stereo: 1.0,
            smooth_coef: 1.0 - (-1.0f32 / (0.005 * 48_000.0)).exp(),
            seen_gen: 0,
            prev_lfo_trig: [false; 6],
            rng: Rng::new(0x564F_5944), // "VOYD"
            sample_rate: 48_000.0,
            loaded: Default::default(),
            loaded_rate: 0.0,
            processing: false,
            built_seed: 0,
            params,
        }
    }
}

/// SplitMix64 finalizer, to spread similar seeds apart.
fn mix_seed(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Build a min/max peak summary of a slot's mono mix for the GUI.
pub(crate) fn summarize(slot: &SampleSlot) -> WaveSummary {
    const BUCKETS: usize = 1024;
    let n = slot.len();
    let mut peaks = Vec::with_capacity(BUCKETS);
    if n == 0 {
        return WaveSummary {
            peaks,
            len_samples: 0,
        };
    }
    for b in 0..BUCKETS {
        let a = b * n / BUCKETS;
        let z = (((b + 1) * n / BUCKETS).max(a + 1)).min(n);
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for i in a..z {
            let v = 0.5 * (slot.data[0][i] + slot.data[1][i]);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        peaks.push((lo, hi));
    }
    WaveSummary {
        peaks,
        len_samples: n,
    }
}

impl Mangle {
    fn refresh_waveform(&self, slot_index: usize) {
        if let Ok(mut w) = self.params.waveforms.write() {
            w[slot_index] = self.bank.slots[slot_index].as_ref().map(summarize);
        }
    }

    /// Drone: with Global On and Key at 0 % the engines free-run when no note
    /// is held. Starts the drone voice or releases it as the condition changes.
    fn update_drone(&mut self, gate_sync: &[bool; 3]) {
        let on_pos = self.params.table.pos(mp::pid(Module::Global, 0, 0));
        let key_pos = self.params.table.pos(mp::pid(Module::Global, 0, 1));
        let wanted = self.base.values[on_pos] >= 0.5
            && self.base.values[key_pos] < DRONE_KEY_MAX_NORM
            && !self.vm.any_note_gated();
        if wanted && !self.vm.drone_mut().gate {
            let vr = self.rng.bipolar();
            let v = self.vm.start_drone(vr);
            for (e, gs) in gate_sync.iter().enumerate() {
                if *gs {
                    v.loop_lfos[e].trig();
                }
            }
        } else if !wanted {
            self.vm.drone_mut().gate = false;
        }
    }

    /// Apply one note/automation event. Called from the control-chunk loop at
    /// the event's sample offset so note timing is sample-accurate. `gate_sync`
    /// is the per-engine Gate-Sync flag set (resolved once from the base patch).
    fn apply_event<E: BlockEvents>(
        &mut self,
        event: NoteEvent<()>,
        gate_sync: &[bool; 3],
        io: &mut E,
    ) {
        match event {
            NoteEvent::NoteOn {
                timing,
                note,
                velocity,
                voice_id,
                channel,
            } => {
                let vr = self.rng.bipolar();
                let (v, ended) =
                    self.vm
                        .note_on(note, voice_id.unwrap_or(-1), channel, velocity, vr);
                if let Some(old) = ended.filter(|o| o.note_id >= 0) {
                    io.send_event(NoteEvent::VoiceTerminated {
                        timing,
                        voice_id: Some(old.note_id),
                        channel: old.channel,
                        note: old.note,
                    });
                }
                for (e, gs) in gate_sync.iter().enumerate() {
                    if *gs {
                        v.loop_lfos[e].trig();
                    }
                }
                // A played note takes over from the drone.
                self.vm.drone_mut().gate = false;
            }
            NoteEvent::NoteOff {
                note,
                voice_id,
                channel,
                ..
            } => {
                self.vm.note_off(note, voice_id.unwrap_or(-1), channel);
            }
            NoteEvent::Choke {
                voice_id,
                note,
                channel,
                ..
            } => {
                for v in self.vm.voices.iter_mut() {
                    if voice_matches(v, voice_id, note, channel) {
                        v.gate = false;
                        for lane in v.lanes.iter_mut() {
                            lane.reset();
                        }
                    }
                }
            }
            NoteEvent::PolyPressure {
                voice_id,
                note,
                channel,
                pressure,
                ..
            } => {
                for v in self.vm.voices.iter_mut() {
                    if voice_matches(v, voice_id, note, channel) {
                        v.pressure = pressure;
                    }
                }
            }
            NoteEvent::MidiPitchBend { channel, value, .. } => {
                self.bend[(channel & 15) as usize] = (value - 0.5) * 2.0 * PITCH_BEND_RANGE_ST;
            }
            NoteEvent::MidiChannelPressure { pressure, .. } => {
                for v in self.vm.voices.iter_mut() {
                    if v.gate {
                        v.pressure = pressure;
                    }
                }
            }
            NoteEvent::PolyTuning {
                voice_id,
                note,
                channel,
                tuning,
                ..
            } => {
                for v in self.vm.voices.iter_mut() {
                    if voice_matches(v, voice_id, note, channel) {
                        v.tuning = tuning;
                    }
                }
            }
            NoteEvent::PolyVolume {
                voice_id,
                note,
                channel,
                gain,
                ..
            } => {
                for v in self.vm.voices.iter_mut() {
                    if voice_matches(v, voice_id, note, channel) {
                        v.expr_volume = gain.max(0.0);
                    }
                }
            }
            NoteEvent::PolyPan {
                voice_id,
                note,
                channel,
                pan,
                ..
            } => {
                for v in self.vm.voices.iter_mut() {
                    if voice_matches(v, voice_id, note, channel) {
                        v.expr_pan = pan.clamp(-1.0, 1.0);
                    }
                }
            }
            NoteEvent::PolyModulation {
                voice_id,
                poly_modulation_id,
                normalized_offset,
                ..
            } => {
                if let Some(v) = self.vm.voices.iter_mut().find(|v| v.note_id == voice_id) {
                    v.set_mod_offset(poly_modulation_id, normalized_offset);
                }
            }
            // nih-plug has already written the value into the parameter.
            NoteEvent::MonoAutomation { .. } => {}
            _ => {}
        }
    }
}

/// Pitch-bend range in semitones for a full-scale MIDI bend.
const PITCH_BEND_RANGE_ST: f32 = 2.0;

/// Whether a voice is the target of a per-note event: by voice id when the host
/// gave one, else by note and channel.
fn voice_matches(v: &Voice, voice_id: Option<i32>, note: u8, channel: u8) -> bool {
    match voice_id {
        Some(id) => v.note_id == id,
        None => v.note == note && v.channel == channel,
    }
}

/// Sample-accurate block splitting: end (exclusive) of the control segment that
/// starts at `off`, bounded by the next event's sample offset, the control-chunk
/// size, and the block end. Assumes events at or before `off` have already been
/// drained, so the result is always `> off` while `off < block` (forward
/// progress, no zero-length segments).
fn segment_end(off: usize, next_event: Option<usize>, block: usize, chunk: usize) -> usize {
    next_event.unwrap_or(block).min(off + chunk).min(block)
}

impl Plugin for Mangle {
    const NAME: &'static str = "Mangle";
    const VENDOR: &'static str = "voyd";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: None,
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(self.params.clone())
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        self.prepare(buffer_config.sample_rate);

        self.ensure_instance_seed();
        self.sync_slots_with_state(!self.processing);
        nih_log!(
            "Mangle: {} host params generated from the table",
            self.params.table.len()
        );
        true
    }

    fn deactivate(&mut self) {
        self.processing = false;
    }

    fn reset(&mut self) {
        for v in self.vm.voices.iter_mut() {
            v.gate = false;
            v.was_sounding = false;
            for lane in v.lanes.iter_mut() {
                lane.reset();
            }
        }
        self.bend = [0.0; 16];
        self.squeeze.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let transport = {
            let t = context.transport();
            BlockTransport {
                tempo: t.tempo,
                playing: t.playing,
                pos_seconds: t.pos_seconds(),
            }
        };
        self.processing = true;
        self.sync_gui_state();

        // 1. Host params → base patch.
        self.params.read_into(&mut self.base);

        let output = buffer.as_slice();
        let (first, second) = output.split_at_mut(1);
        self.render_block(first[0], second[0], transport, &mut ContextEvents(context));
        ProcessStatus::Normal
    }
}

/// Adapts nih-plug's `ProcessContext` to `BlockEvents`.
struct ContextEvents<'a, C: ProcessContext<Mangle>>(&'a mut C);

impl<C: ProcessContext<Mangle>> BlockEvents for ContextEvents<'_, C> {
    fn next_event(&mut self) -> Option<NoteEvent<()>> {
        self.0.next_event()
    }

    fn send_event(&mut self, event: NoteEvent<()>) {
        self.0.send_event(event);
    }
}

/// Transport snapshot for one block, independent of the host API.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BlockTransport {
    pub tempo: Option<f64>,
    pub playing: bool,
    pub pos_seconds: Option<f64>,
}

/// Note-event I/O for one block. `process` adapts nih-plug's ProcessContext;
/// tests use a scripted queue.
pub(crate) trait BlockEvents {
    fn next_event(&mut self) -> Option<NoteEvent<()>>;
    fn send_event(&mut self, event: NoteEvent<()>);
}

impl Mangle {
    /// Sample-rate dependent, IO-free setup (everything `initialize` does except
    /// loading samples): sample_rate, sr_bits, Squeeze, filter clear, smoothing
    /// coefficient, table warm-up.
    pub(crate) fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.params
            .sr_bits
            .store(self.sample_rate.to_bits(), Ordering::Release);
        self.squeeze = Squeeze::new(self.sample_rate);
        for f in self.filter.iter_mut() {
            f.clear();
        }
        self.smooth_coef = 1.0 - (-1.0f32 / (0.005 * self.sample_rate)).exp();
        self.bend = [0.0; 16];
        mangle_dsp::sample::warm_tables(); // no lazy alloc on the audio thread
        mangle_dsp::spectral::warm(); // pre-build the spectral FFT plan + window
    }

    /// Assign `instance_seed` if unset, and rebuild the seeded voice, modulator
    /// and RNG state when it differs from the one they were built with.
    fn ensure_instance_seed(&mut self) {
        let mut seed = self.params.extra.read().map_or(0, |st| st.instance_seed);
        if seed == 0 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            let addr = Arc::as_ptr(&self.params) as usize as u64;
            seed = mix_seed(nanos ^ addr.rotate_left(32)).max(1);
            if let Ok(mut st) = self.params.extra.write() {
                st.instance_seed = seed;
            }
            self.params.extra_gen.fetch_add(1, Ordering::Release);
        }
        if seed != self.built_seed {
            self.vm = VoiceManager::new(8, mix_seed(seed ^ 0x4D41_4E47));
            self.gmod = GlobalMod::new(mix_seed(seed ^ 0x6D6F_6421));
            self.rng = Rng::new(mix_seed(seed ^ 0x564F_5944));
            self.built_seed = seed;
        }
    }

    fn set_slot_status(&self, slot: usize, status: loading::SlotStatus) {
        if let Ok(mut s) = self.params.loads.status.lock() {
            s[slot] = status;
        }
    }

    /// Empty a slot and cancel any load still in flight for it.
    fn clear_slot(&mut self, i: usize) {
        self.params.loads.generation[i].fetch_add(1, Ordering::AcqRel);
        if let Ok(mut q) = self.params.pending_loads.lock() {
            q.retain(|p| p.consumed || p.slot != i);
        }
        self.bank.slots[i] = None;
        if let Ok(mut w) = self.params.waveforms.write() {
            w[i] = None;
        }
        self.loaded[i] = (String::new(), 0);
        self.set_slot_status(i, loading::SlotStatus::Empty);
    }

    /// Make the bank match `extra.slot_refs`: unchanged slots keep their
    /// samples, unreferenced slots are cleared, changed ones are decoded here
    /// (`cold`) or on a loader thread (hot reload while processing, so audio
    /// is never stalled by file IO).
    /// The preset's embedded bytes for `slot`, when they match the ref's hash
    /// and the referenced file is missing or has different content.
    fn embedded_source(&self, slot: usize, r: &SlotRef, cold: bool) -> Option<loading::LoadSource> {
        if r.content_hash == 0 {
            return None;
        }
        let e = self.params.embedded.lock().ok()?[slot].clone()?;
        if e.hash != r.content_hash {
            return None;
        }
        if !cold {
            // The worker reads and hashes the file, not this thread.
            return Some(loading::LoadSource::FileOrEmbedded {
                path: r.path.clone(),
                hash: r.content_hash,
                embedded: e,
            });
        }
        let on_disk = mangle_loader::hash_file(std::path::Path::new(&r.path)).ok();
        if on_disk == Some(r.content_hash) {
            return None;
        }
        Some(loading::LoadSource::Embedded {
            bytes: e.bytes,
            ext: e.ext,
            path: r.path.clone(),
        })
    }

    fn sync_slots_with_state(&mut self, cold: bool) {
        if let Ok(mut q) = self.params.pending_loads.lock() {
            q.retain(|p| !p.consumed);
        }
        let refs: Vec<Option<SlotRef>> = self
            .params
            .extra
            .read()
            .map(|st| st.slot_refs.to_vec())
            .unwrap_or_default();
        let mut any_ref = false;
        for (i, r) in refs.iter().enumerate() {
            let Some(r) = r else {
                self.clear_slot(i);
                continue;
            };
            any_ref = true;
            if self.loaded[i].0 == r.path
                && self.loaded[i].1 == r.content_hash
                && self.loaded_rate == self.sample_rate
            {
                continue;
            }
            self.clear_slot(i);
            let embedded = self.embedded_source(i, r, cold);
            let from_preset = cold && embedded.is_some();
            let source = embedded
                .unwrap_or_else(|| loading::LoadSource::File(std::path::PathBuf::from(&r.path)));
            if !cold {
                self.set_slot_status(i, loading::SlotStatus::Loading);
                loading::spawn_load(self.params.clone(), i, source);
                continue;
            }
            match loading::decode(&source, self.sample_rate) {
                Ok(d) => {
                    if r.content_hash != 0 && d.content_hash != r.content_hash {
                        nih_log!(
                            "Mangle: slot {} content hash mismatch for '{}' (file changed?)",
                            i + 1,
                            r.path
                        );
                    }
                    for w in &d.warnings {
                        nih_log!("Mangle: slot {}: {}", i + 1, w);
                    }
                    self.bank.slots[i] = Some(d.sample);
                    if let Ok(mut w) = self.params.waveforms.write() {
                        w[i] = Some(d.summary);
                    }
                    self.loaded[i] = (d.path, d.content_hash);
                    self.set_slot_status(i, loading::SlotStatus::Loaded);
                    if from_preset {
                        loading::note(&self.params, format!("slot {}: from preset", i + 1));
                    }
                }
                Err(status) => {
                    nih_log!(
                        "Mangle: slot {} load failed '{}': {:?}",
                        i + 1,
                        r.path,
                        status
                    );
                    self.set_slot_status(i, status);
                }
            }
        }
        self.loaded_rate = self.sample_rate;

        if !any_ref && self.bank.loaded().1 == 0 {
            // Dev hook: MANGLE_SAMPLE=/path/to/file fills slot 1 and records
            // the reference in state.
            let dev_sample = if cold {
                std::env::var("MANGLE_SAMPLE").ok()
            } else {
                None
            };
            if let Some(p) = dev_sample {
                match mangle_loader::load_sample(std::path::Path::new(&p), self.sample_rate) {
                    Ok(loaded) => {
                        nih_log!("Mangle: loaded MANGLE_SAMPLE '{}'", p);
                        if let Ok(mut st) = self.params.extra.write() {
                            st.slot_refs[0] = Some(SlotRef {
                                path: p.clone(),
                                content_hash: loaded.content_hash,
                            });
                        }
                        self.params.extra_gen.fetch_add(1, Ordering::Release);
                        self.loaded[0] = (p, loaded.content_hash);
                        self.bank.slots[0] = Some(loaded.slot);
                        self.refresh_waveform(0);
                        self.set_slot_status(0, loading::SlotStatus::Loaded);
                    }
                    Err(e) => nih_log!("Mangle: MANGLE_SAMPLE failed: {}", e),
                }
            }
            if self.bank.loaded().1 == 0 {
                // Still empty: built-in fallback tone so the instrument sounds.
                self.bank.slots[0] = Some(SampleSlot::test_tone(self.sample_rate));
                self.refresh_waveform(0);
            }
        }
    }

    /// GUI → audio hand-offs done at the top of each block: curve sync and the
    /// pending-load swap.
    fn sync_gui_state(&mut self) {
        // 0. Sync curve copies when the main thread edited state
        //    (non-blocking; skip this block on contention).
        let gen = self.params.extra_gen.load(Ordering::Acquire);
        if gen != self.seen_gen {
            if let Ok(st) = self.params.extra.try_read() {
                for i in 0..3 {
                    self.loop_curves[i] = st.curves[i].clone();
                }
                for i in 0..6 {
                    self.lfo_curves[i] = st.curves[3 + i].clone();
                }
                self.seen_gen = gen;
            }
        }

        // Drain any GUI-side sample loads (drag-drop / dialog). The decode,
        // resample, waveform summary, and slot-ref/extra bookkeeping all
        // happened on the GUI thread; here we only swap the new sample into
        // the bank and the old slot contents back into the queue entry —
        // pointer moves only, no allocation or deallocation (the GUI thread
        // reclaims consumed entries and frees the old sample there). try_lock
        // keeps it RT-safe: if the GUI holds the lock we retry next block.
        if let Ok(mut pending) = self.params.pending_loads.try_lock() {
            for p in pending.iter_mut() {
                if p.consumed {
                    continue;
                }
                let i = p.slot.min(5);
                let new = std::mem::replace(p.sample.as_mut(), SampleSlot::empty());
                if let Some(old) = self.bank.slots[i].replace(new) {
                    *p.sample = old;
                }
                std::mem::swap(&mut self.loaded[i].0, &mut p.path);
                std::mem::swap(&mut self.loaded[i].1, &mut p.content_hash);
                p.consumed = true;
            }
        }
    }

    /// Render one block into `out_l`/`out_r` (overwritten) from `self.base`,
    /// which the caller has already filled. Starts with the GUI panic check and
    /// contains everything `process` did after filling `self.base`.
    pub(crate) fn render_block<E: BlockEvents>(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        transport: BlockTransport,
        io: &mut E,
    ) {
        let block = out_l.len();
        let bpm = transport.tempo.unwrap_or(120.0) as f32;
        let playing = transport.playing;
        let pos_sec0 = transport.pos_seconds;

        // Panic request from the GUI (RND etc.): hard-stop every voice so
        // nothing hangs over the parameter change.
        if self.params.panic.swap(false, Ordering::AcqRel) {
            for v in self.vm.voices.iter_mut() {
                if v.was_sounding && v.note_id >= 0 {
                    io.send_event(NoteEvent::VoiceTerminated {
                        timing: 0,
                        voice_id: Some(v.note_id),
                        channel: v.channel,
                        note: v.note,
                    });
                }
                v.gate = false;
                v.was_sounding = false;
                for lane in v.lanes.iter_mut() {
                    lane.reset();
                }
            }
        }

        // LFO Trig: host/GUI edge-triggered restarts.
        let gui_trig = self.params.lfo_trig_requests.swap(0, Ordering::AcqRel);
        for i in 0..6usize {
            let pos = self
                .params
                .table
                .pos(mp::pid(Module::Lfo, i as u8, LFO_TRIG_IDX));
            let on = self.base.values[pos] >= 0.5;
            if (on && !self.prev_lfo_trig[i]) || gui_trig & (1 << (3 + i)) != 0 {
                self.gmod.trig_lfo(i);
            }
            self.prev_lfo_trig[i] = on;
        }
        for v in self.vm.voices.iter_mut().filter(|v| v.gate) {
            for n in 0..3usize {
                if gui_trig & (1 << n) != 0 {
                    v.loop_lfos[n].trig();
                }
            }
        }

        // 2. Decode matrix rows once per block (the `rows` slice is bound
        //    after the event loop, which needs &mut self).
        self.n_rows = decode_matrix(&self.base, &self.params.table, &mut self.rows);

        // 3. (Global modulation pass + source ticking moved into the
        //    control-chunk loop below so modulation runs at a fixed ~64
        //    sample rate instead of once per host block.)

        // Per-engine Gate Sync flags (needed at note-on).
        let mut gate_sync = [false; 3];
        for (e, gs) in gate_sync.iter_mut().enumerate() {
            let pos = self.params.table.pos(mp::pid(Module::Grain, e as u8, 13));
            *gs = self.base.values[pos] >= 0.5;
        }

        self.update_drone(&gate_sync);

        // 4. Events are applied inside the control-chunk loop (step 7) at their
        //    sample offsets, via `apply_event` — so note timing is sample-
        //    accurate rather than quantized to the block. `gate_sync` feeds it.

        // 5. Globals + per-slot settings (from the base patch — the Global
        //    and Slot modules are not matrix destinations).
        let g = resolve::global_out(&self.base, &self.params.table);
        self.vm.set_max_voices(g.voices);
        for s in 0..6u8 {
            if let Some(slot) = self.bank.slots[s as usize].as_mut() {
                let st = resolve::slot_settings(&self.base, &self.params.table, s);
                let len = slot.len();
                slot.root_note = st.root_note;
                if len > 0 {
                    // Clamp loop_start below `len` so `loop_start + 1 <= len` holds;
                    // otherwise Loop Start = Loop End = 100% makes the clamp below
                    // `clamp(len + 1, len)`, which panics (min > max) on the audio thread.
                    let ls = ((st.loop_start * len as f32) as usize).min(len - 1);
                    slot.loop_start = ls;
                    let max_len = (MAX_LOOP_SECONDS * self.sample_rate) as usize;
                    slot.loop_end = ((st.loop_end * len as f32) as usize)
                        .clamp(ls + 1, len)
                        .min(ls + max_len.max(1));
                }
                slot.gain = st.gain;
            }
        }

        // 6. Output slices.
        out_l.fill(0.0);
        out_r.fill(0.0);

        // Engine-activity capture for the GUI feed. Lanes render into local
        // per-engine chunk buffers (summed into the master out after each
        // chunk's voice loop) so per-engine peaks exist at all; the buffers
        // stay local to dodge split-borrow conflicts with the voice iteration
        // (see mangle-borrow-fallback.md). All fixed-size — no allocation.
        let feed_open = self.params.editor_state.is_open();
        let mut eng_chunk = [[[0.0f32; CTRL_CHUNK]; 2]; 3];
        let mut voice_buf = [[0.0f32; CTRL_CHUNK]; 2];
        let mut eng_peak = [0.0f32; 3];
        let mut spawn_acc = [0u32; 3];
        let mut ff = FeedFrame::default();

        // 7. Control-chunk loop (~64 samples): global mod pass + source
        //    tick, then per voice the stereo voice pass and rendering. A
        //    fast Loop LFO now scans *within* a host block instead of
        //    jumping once per block.
        // Stack-copy the decoded rows so the loop body can take &mut self (for
        // sample-accurate event handling) without conflicting with a borrow on
        // self.rows. DecodedRow is Copy — see mangle-borrow-fallback.md.
        let rows_buf = self.rows;
        let rows = &rows_buf[..self.n_rows];
        let fo = resolve::filter_out(&self.base, &self.params.table);
        let filt_cut_pos = self.params.table.pos(mp::pid(Module::Fx, 0, 2));
        let filt_res_pos = self.params.table.pos(mp::pid(Module::Fx, 0, 3));
        let filt_cut_map = self.params.table.infos[filt_cut_pos].map;
        let mut off = 0usize;
        let mut next_event = io.next_event();
        while off < block {
            // Apply every event due at or before this sample, then bound the
            // segment by the next event so its onset lands on the exact sample.
            while let Some(ev) = next_event {
                if (ev.timing() as usize) > off {
                    break;
                }
                self.apply_event(ev, &gate_sync, io);
                next_event = io.next_event();
            }
            let seg_end = segment_end(
                off,
                next_event.map(|e| e.timing() as usize),
                block,
                CTRL_CHUNK,
            );
            let n = seg_end - off;
            let dt = n as f32 / self.sample_rate;
            for chunk in eng_chunk.iter_mut() {
                chunk[0][..n].fill(0.0);
                chunk[1][..n].fill(0.0);
            }
            let t_sec = if playing {
                pos_sec0.map(|s| s + off as f64 / self.sample_rate as f64)
            } else {
                None
            };

            // Global pass: last-tick sources modulate global LFO params,
            // then the sources advance one control tick.
            let last_vi = self
                .vm
                .voices
                .iter()
                .filter(|v| v.gate)
                .max_by_key(|v| v.stamp)
                .map(|v| VoiceSourceInputs {
                    velocity: v.velocity,
                    note: v.note,
                    gate: v.gate,
                    pressure: v.pressure,
                    voice_random: v.voice_random,
                    loop_lfo: v.last_scan,
                });
            let mut sv_pre = SourceValues::zero();
            self.gmod.fill_sources(&mut sv_pre);
            fill_global_voice_sources(
                &mut sv_pre,
                last_vi.as_ref(),
                &self.base,
                &self.params.table,
            );
            self.scratch_g.values.clear();
            self.scratch_g.values.extend_from_slice(&self.base.values);
            apply_global(&mut self.scratch_g, rows, &sv_pre);
            self.gmod.tick(
                &self.scratch_g,
                &self.params.table,
                &self.lfo_curves,
                bpm,
                dt,
                t_sec,
            );
            let mut sv_template = SourceValues::zero();
            self.gmod.fill_sources(&mut sv_template);
            fill_global_voice_sources(
                &mut sv_template,
                last_vi.as_ref(),
                &self.base,
                &self.params.table,
            );

            for voice in self.vm.voices.iter_mut() {
                if !voice.sounding() {
                    continue;
                }
                resolve::compose_voice_patch(
                    &self.base,
                    &voice.mod_offsets,
                    &self.params.table,
                    &mut self.scratch_v,
                );
                let mut sv = sv_template;
                let vi = VoiceSourceInputs {
                    velocity: voice.velocity,
                    note: voice.note,
                    gate: voice.gate,
                    pressure: voice.pressure,
                    voice_random: voice.voice_random,
                    loop_lfo: voice.last_scan,
                };
                fill_voice_sources(&mut sv, &vi, &self.scratch_v, &self.params.table);

                for n in 0..3usize {
                    let pos =
                        self.params
                            .table
                            .pos(mp::pid(Module::LoopLfo, n as u8, LFO_TRIG_IDX));
                    let on = self.scratch_v.values[pos] >= 0.5;
                    if on && !voice.trig_prev[n] {
                        voice.loop_lfos[n].trig();
                    }
                    voice.trig_prev[n] = on;
                }

                let mut loop_pos_l = [0.0f32; 3];
                let mut loop_pos_r = [0.0f32; 3];
                self.scratch_l.values.clear();
                self.scratch_l
                    .values
                    .extend_from_slice(&self.scratch_v.values);
                apply_voice(&mut self.scratch_l, rows, &sv, 0, &mut loop_pos_l);
                self.scratch_r.values.clear();
                self.scratch_r
                    .values
                    .extend_from_slice(&self.scratch_v.values);
                apply_voice(&mut self.scratch_r, rows, &sv, 1, &mut loop_pos_r);

                let note =
                    voice.note as f32 + voice.tuning + self.bend[(voice.channel & 15) as usize];
                // Note-expression Volume/Pan (parameter map §10): a per-voice
                // gain plus a balance pan (center = unity, so a voice with no
                // expression is untouched), applied to the voice's output.
                let expr_gl = voice.expr_volume
                    * if voice.expr_pan > 0.0 {
                        1.0 - voice.expr_pan
                    } else {
                        1.0
                    };
                let expr_gr = voice.expr_volume
                    * if voice.expr_pan < 0.0 {
                        1.0 + voice.expr_pan
                    } else {
                        1.0
                    };
                for e in 0..3usize {
                    let gate_pos = self.params.table.pos(mp::pid(Module::Grain, e as u8, 0));
                    let gate_on = g.on && voice.gate;
                    let active = gate_on
                        && (self.scratch_l.values[gate_pos] >= 0.5
                            || self.scratch_r.values[gate_pos] >= 0.5);
                    // Loop LFO params from the left-modulated patch;
                    // St.Phase still splits the channels. It keeps ticking
                    // while the engine is idle so it stays a usable source.
                    let (lfop, transport) = resolve::lfo_params(
                        &self.scratch_l,
                        &self.params.table,
                        Module::LoopLfo,
                        e as u8,
                        bpm,
                    );
                    let scan = match (transport, t_sec) {
                        (true, Some(sec)) => {
                            let phase = (sec * lfop.rate_hz as f64).rem_euclid(1.0) as f32;
                            voice.loop_lfos[e].tick_transport(
                                &self.loop_curves[e],
                                &lfop,
                                phase,
                                dt,
                            )
                        }
                        _ => voice.loop_lfos[e].tick(&self.loop_curves[e], &lfop, dt),
                    };
                    voice.last_scan[e] = scan;
                    if !active && !voice.lanes[e].is_ringing() {
                        continue;
                    }
                    let (mut lane_l, _) = resolve::lane_params(
                        &self.scratch_l,
                        &self.params.table,
                        e as u8,
                        bpm,
                        note,
                        voice.gate,
                        g.on,
                    );
                    let (mut lane_r, _) = resolve::lane_params(
                        &self.scratch_r,
                        &self.params.table,
                        e as u8,
                        bpm,
                        note,
                        voice.gate,
                        g.on,
                    );
                    lane_l.loop_pos = loop_pos_l[e];
                    lane_r.loop_pos = loop_pos_r[e];
                    // Each voice renders to its own scratch so the expression
                    // gains apply to the voice output, after grain Pan.
                    for ch in voice_buf.iter_mut() {
                        ch[..n].fill(0.0);
                    }
                    let [vbuf_l, vbuf_r] = &mut voice_buf;
                    voice.lanes[e].process(
                        &[lane_l, lane_r],
                        &self.bank,
                        scan,
                        self.sample_rate,
                        &mut vbuf_l[..n],
                        &mut vbuf_r[..n],
                        n,
                    );
                    let [buf_l, buf_r] = &mut eng_chunk[e];
                    for i in 0..n {
                        buf_l[i] += vbuf_l[i] * expr_gl;
                        buf_r[i] += vbuf_r[i] * expr_gr;
                    }
                    // Always drain the lane's spawn counter so it can't go
                    // stale; the accumulator is only published while the
                    // editor is open.
                    spawn_acc[e] = spawn_acc[e].saturating_add(voice.lanes[e].take_spawned());
                    if feed_open && seg_end == block {
                        // Cursor/mode snapshot from the block's final chunk;
                        // with several sounding voices the last one wins
                        // (typical use is low polyphony — good enough for a
                        // display).
                        let ef = &mut ff.engines[e];
                        ef.mode = lane_l.mode.index() as u8;
                        ef.gate = lane_l.gate || lane_r.gate;
                        if let Some((cur, slot)) =
                            voice.lanes[e].read_cursor(&lane_l, &self.bank, scan)
                        {
                            ef.cursor = cur;
                            ef.slot = slot;
                        }
                    }
                }
            }
            // Fold the per-engine chunks into the master out and the peaks.
            for (e, chunk) in eng_chunk.iter().enumerate() {
                for i in 0..n {
                    let (sl, sr) = (chunk[0][i], chunk[1][i]);
                    out_l[off + i] += sl;
                    out_r[off + i] += sr;
                    eng_peak[e] = eng_peak[e].max(sl.abs()).max(sr.abs());
                }
            }

            // Stereo width, then the master filter, over this segment so
            // filter modulation follows the control rate, not the host block.
            for i in off..seg_end {
                self.cur_stereo += (g.stereo - self.cur_stereo) * self.smooth_coef;
                let (l, r) = (out_l[i], out_r[i]);
                let mid = 0.5 * (l + r);
                let side = 0.5 * (l - r) * self.cur_stereo;
                out_l[i] = mid + side;
                out_r[i] = mid - side;
            }
            if fo.on {
                let modf = modulated_filter(
                    &self.base,
                    rows,
                    &sv_template,
                    filt_cut_pos as u16,
                    filt_res_pos as u16,
                );
                let coef = 1.0 - (-(n as f32) / (0.005 * self.sample_rate)).exp();
                for (ch, &(cut, res)) in modf.iter().enumerate() {
                    let target = filt_cut_map.to_display(cut);
                    self.filt_cutoff[ch] += (target - self.filt_cutoff[ch]) * coef;
                    self.filt_reso[ch] += (res - self.filt_reso[ch]) * coef;
                    self.filter[ch].set(self.filt_cutoff[ch], self.filt_reso[ch], self.sample_rate);
                }
                for i in off..seg_end {
                    out_l[i] = self.filter[0].process(out_l[i], fo.ftype);
                    out_r[i] = self.filter[1].process(out_r[i], fo.ftype);
                }
            } else {
                // Keep the smoothers tracking the target and states clear so
                // the first segment after enabling starts from the right
                // cutoff, silent.
                let modf = modulated_filter(
                    &self.base,
                    rows,
                    &sv_template,
                    filt_cut_pos as u16,
                    filt_res_pos as u16,
                );
                self.filt_cutoff = [fo.cutoff; 2];
                self.filt_reso = [modf[0].1, modf[1].1];
                self.filter[0].clear();
                self.filter[1].clear();
            }
            off += n;
        }
        // Apply any events at/after the final sample so none are dropped (their
        // state carries into the next block).
        while let Some(ev) = next_event {
            self.apply_event(ev, &gate_sync, io);
            next_event = io.next_event();
        }
        // Notes released in this block hand back to the drone straight away.
        self.update_drone(&gate_sync);

        // 8. Voice termination (lets the host recycle CLAP note ids).
        for voice in self.vm.voices.iter_mut() {
            if voice.was_sounding && !voice.sounding() {
                voice.was_sounding = false;
                if voice.note_id >= 0 {
                    io.send_event(NoteEvent::VoiceTerminated {
                        timing: block.saturating_sub(1) as u32,
                        voice_id: Some(voice.note_id),
                        channel: voice.channel,
                        note: voice.note,
                    });
                }
            }
        }

        // 9. Squeeze → master level (stereo width and the master filter ran
        //    per control chunk above). Level is smoothed per sample (~5 ms) so
        //    automation doesn't zipper.
        let coef = self.smooth_coef;
        if g.squeeze {
            self.squeeze.process(out_l, out_r);
        }
        for n in 0..block {
            self.cur_level += (g.level - self.cur_level) * coef;
            out_l[n] *= self.cur_level;
            out_r[n] *= self.cur_level;
        }

        // 10. Feed the env-follower source with the final output.
        self.gmod.env.feed(out_l, out_r, self.sample_rate);

        // 11. Publish the engine-activity feed for the editor (skipped while
        //     it is closed). Atomic accumulators plus one try_lock'd frame
        //     copy — the audio thread never blocks or allocates here.
        if feed_open {
            for (e, ef) in ff.engines.iter_mut().enumerate() {
                let mut filled = 0usize;
                for voice in self.vm.voices.iter() {
                    if !voice.sounding() || filled >= ef.grains.len() {
                        continue;
                    }
                    filled += voice.lanes[e].report_grains(&self.bank, &mut ef.grains[filled..]);
                }
                ef.n_grains = filled as u8;
            }
            ff.voices = self.vm.sounding_count().min(u8::MAX as usize) as u8;
            for e in 0..3 {
                if spawn_acc[e] > 0 {
                    self.params.feed.spawns[e].fetch_add(spawn_acc[e], Ordering::Relaxed);
                }
                self.params.feed.level_bits[e].fetch_max(eng_peak[e].to_bits(), Ordering::Relaxed);
            }
            if let Ok(mut f) = self.params.feed.frame.try_lock() {
                *f = ff;
            }
        }
    }
}

impl ClapPlugin for Mangle {
    const CLAP_ID: &'static str = "com.voyd.mangle";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Granular texture instrument: 3 grain engines, 6 samples, 32-slot mod matrix");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Synthesizer,
        ClapFeature::Stereo,
    ];
    // API: lets hosts stream per-note modulation to the PN-flagged params.
    const CLAP_POLY_MODULATION_CONFIG: Option<PolyModulationConfig> = Some(PolyModulationConfig {
        max_voice_capacity: 16,
        supports_overlapping_voices: true,
    });
}

nih_export_clap!(Mangle);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Scripted {
        input: VecDeque<NoteEvent<()>>,
        sent: Vec<NoteEvent<()>>,
    }

    impl BlockEvents for Scripted {
        fn next_event(&mut self) -> Option<NoteEvent<()>> {
            self.input.pop_front()
        }
        fn send_event(&mut self, event: NoteEvent<()>) {
            self.sent.push(event);
        }
    }

    fn test_plugin() -> Mangle {
        let mut m = Mangle::default();
        m.prepare(48_000.0);
        m.bank.slots[0] = Some(SampleSlot::test_tone(48_000.0));
        m
    }

    fn note_on() -> NoteEvent<()> {
        NoteEvent::NoteOn {
            timing: 0,
            voice_id: Some(5),
            channel: 0,
            note: 45,
            velocity: 0.8,
        }
    }

    fn render(m: &mut Mangle, io: &mut Scripted, out: &mut Vec<f32>, samples: usize, bs: usize) {
        let mut l = vec![0.0f32; bs];
        let mut r = vec![0.0f32; bs];
        for _ in 0..samples / bs {
            m.params.read_into(&mut m.base);
            m.render_block(&mut l, &mut r, BlockTransport::default(), io);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }
    }

    fn rms(out: &[f32]) -> f32 {
        (out.iter().map(|s| s * s).sum::<f32>() / out.len().max(1) as f32).sqrt()
    }

    fn drone_setup(m: &mut Mangle, on: bool, key: f32) {
        let on_pos = m.params.table.pos(mp::pid(Module::Global, 0, 0));
        let key_pos = m.params.table.pos(mp::pid(Module::Global, 0, 1));
        m.base.values[on_pos] = if on { 1.0 } else { 0.0 };
        m.base.values[key_pos] = key;
    }

    #[test]
    fn drone_runs_without_notes_at_key_zero() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        let out = render_setup(&mut m, &mut io, 24_000 / 512 * 512, 512, &|m| {
            drone_setup(m, true, 0.0)
        });
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(rms(&out) > 0.01, "rms {}", rms(&out));
        assert!(m.vm.voices[VoiceManager::DRONE].gate);
    }

    #[test]
    fn no_drone_when_key_tracks() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        let out = render_setup(&mut m, &mut io, 24_000 / 512 * 512, 512, &|_| {});
        assert!(rms(&out) < 1e-6);
        assert!(!m.vm.voices[VoiceManager::DRONE].gate);
    }

    #[test]
    fn drone_respects_global_on() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        let out = render_setup(&mut m, &mut io, 24_000 / 512 * 512, 512, &|m| {
            drone_setup(m, false, 0.0)
        });
        assert!(rms(&out) < 1e-6);
        assert!(!m.vm.voices[VoiceManager::DRONE].gate);
    }

    #[test]
    fn note_takes_over_from_drone() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        let setup = |m: &mut Mangle| drone_setup(m, true, 0.0);
        render_setup(&mut m, &mut io, 2048, 512, &setup);
        assert!(m.vm.voices[VoiceManager::DRONE].gate);

        io.input.push_back(note_on());
        render_setup(&mut m, &mut io, 512, 512, &setup);
        assert!(!m.vm.voices[VoiceManager::DRONE].gate);
        assert!(m.vm.voices[..VoiceManager::DRONE].iter().any(|v| v.gate));

        io.input.push_back(NoteEvent::NoteOff {
            timing: 0,
            voice_id: Some(5),
            channel: 0,
            note: 45,
            velocity: 0.0,
        });
        render_setup(&mut m, &mut io, 512, 512, &setup);
        assert!(m.vm.voices[VoiceManager::DRONE].gate);

        render_setup(&mut m, &mut io, 48_000 / 512 * 512, 512, &setup);
        assert!(!io.sent.iter().any(|e| matches!(
            e,
            NoteEvent::VoiceTerminated {
                voice_id: None | Some(-1),
                ..
            }
        )));
    }

    #[test]
    fn note_renders_audio_and_reports_note_end() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        io.input.push_back(note_on());
        let mut out = Vec::new();
        render(&mut m, &mut io, &mut out, 24_000 / 512 * 512, 512);
        assert!(out.iter().all(|s| s.is_finite()));
        let rms = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms > 0.01, "rms {rms}");

        io.input.push_back(NoteEvent::NoteOff {
            timing: 0,
            voice_id: Some(5),
            channel: 0,
            note: 45,
            velocity: 0.0,
        });
        let mut tail = Vec::new();
        render(&mut m, &mut io, &mut tail, 144_000 / 512 * 512, 512);
        assert!(tail.iter().all(|s| s.is_finite()));
        let ended: Vec<_> = io
            .sent
            .iter()
            .filter(|e| matches!(e, NoteEvent::VoiceTerminated { .. }))
            .collect();
        assert_eq!(ended.len(), 1);
        assert!(matches!(
            ended[0],
            NoteEvent::VoiceTerminated {
                voice_id: Some(5),
                ..
            }
        ));
    }

    fn note_on_id(id: i32) -> NoteEvent<()> {
        NoteEvent::NoteOn {
            timing: 0,
            voice_id: Some(id),
            channel: 0,
            note: 45,
            velocity: 0.8,
        }
    }

    fn terminated(io: &Scripted, id: i32) -> usize {
        io.sent
            .iter()
            .filter(
                |e| matches!(e, NoteEvent::VoiceTerminated { voice_id: Some(v), .. } if *v == id),
            )
            .count()
    }

    #[test]
    fn stolen_voice_reports_note_end() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        let mut l = vec![0.0f32; 512];
        let mut r = vec![0.0f32; 512];
        let pos = m.params.table.pos(mp::pid(Module::Global, 0, 5));
        for id in [1, 2] {
            m.params.read_into(&mut m.base);
            m.base.values[pos] = 0.0; // Voices = 1
            io.input.push_back(note_on_id(id));
            m.render_block(&mut l, &mut r, BlockTransport::default(), &mut io);
        }
        assert_eq!(terminated(&io, 1), 1);
        assert_eq!(terminated(&io, 2), 0);
    }

    #[test]
    fn panic_reports_note_end() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        let mut out = Vec::new();
        io.input.push_back(note_on_id(3));
        render(&mut m, &mut io, &mut out, 512, 512);
        m.params.panic.store(true, Ordering::Release);
        render(&mut m, &mut io, &mut out, 512, 512);
        assert_eq!(terminated(&io, 3), 1);
    }

    #[test]
    fn pitch_bend_changes_pitch() {
        let run = |bend: bool| {
            let mut m = test_plugin();
            let mut io = Scripted::default();
            if bend {
                io.input.push_back(NoteEvent::MidiPitchBend {
                    timing: 0,
                    channel: 0,
                    value: 1.0,
                });
            }
            io.input.push_back(note_on());
            let mut out = Vec::new();
            render(&mut m, &mut io, &mut out, 4096, 512);
            (m.bend[0], out)
        };
        let (b, bent) = run(true);
        let (z, plain) = run(false);
        assert_eq!(b, 2.0);
        assert_eq!(z, 0.0);
        assert_ne!(bent, plain);
    }

    #[test]
    fn channel_pressure_reaches_gated_voices() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        let mut out = Vec::new();
        io.input.push_back(note_on());
        io.input.push_back(NoteEvent::MidiChannelPressure {
            timing: 0,
            channel: 0,
            pressure: 0.75,
        });
        render(&mut m, &mut io, &mut out, 512, 512);
        assert!(m.vm.voices.iter().any(|v| v.gate && v.pressure == 0.75));
    }

    #[test]
    fn block_size_does_not_change_output() {
        let run = |bs: usize| {
            let mut m = test_plugin();
            let mut io = Scripted::default();
            io.input.push_back(note_on());
            let mut l = vec![0.0f32; bs];
            let mut r = vec![0.0f32; bs];
            let mut out = Vec::new();
            for _ in 0..8192 / bs {
                m.params.read_into(&mut m.base);
                m.render_block(&mut l, &mut r, BlockTransport::default(), &mut io);
                out.extend_from_slice(&l);
                out.extend_from_slice(&r);
            }
            // Interleave-free comparison: split per channel.
            let mut lo = Vec::new();
            let mut ro = Vec::new();
            for c in out.chunks(bs * 2) {
                lo.extend_from_slice(&c[..bs]);
                ro.extend_from_slice(&c[bs..]);
            }
            (lo, ro)
        };
        assert_eq!(run(512), run(64));
    }

    /// Write one matrix row (source → one destination) into `base`.
    fn set_matrix_row(
        m: &mut Mangle,
        row: u8,
        src: matrix::ModSource,
        amt: f32,
        dest: matrix::ModDest,
    ) {
        let t = &m.params.table;
        let p = t.pos(mp::pid(Module::Matrix, row, 0));
        let s = (matrix::SOURCE_COUNT - 1) as f32;
        let d = (matrix::DEST_COUNT - 1) as f32;
        m.base.values[p] = matrix::source_to_index(src) as f32 / s;
        m.base.values[p + 1] = (amt + 100.0) / 200.0;
        m.base.values[p + 2] = matrix::dest_to_index(dest) as f32 / d;
    }

    /// Render with `setup` applied to the freshly read base before each block.
    fn render_setup(
        m: &mut Mangle,
        io: &mut Scripted,
        samples: usize,
        bs: usize,
        setup: &dyn Fn(&mut Mangle),
    ) -> Vec<f32> {
        let mut out = Vec::new();
        let mut l = vec![0.0f32; bs];
        let mut r = vec![0.0f32; bs];
        for _ in 0..samples / bs {
            m.params.read_into(&mut m.base);
            setup(m);
            m.render_block(&mut l, &mut r, BlockTransport::default(), io);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }
        out
    }

    fn filter_on_lp(m: &mut Mangle, cutoff: f32) {
        let t = &m.params.table;
        let on = t.pos(mp::pid(Module::Fx, 0, 0));
        let ty = t.pos(mp::pid(Module::Fx, 0, 1));
        let cut = t.pos(mp::pid(Module::Fx, 0, 2));
        m.base.values[on] = 1.0;
        m.base.values[ty] = 0.0;
        m.base.values[cut] = cutoff;
    }

    fn channel_rms(out: &[f32], bs: usize) -> (f32, f32) {
        let (mut sl, mut sr, mut n) = (0.0f32, 0.0f32, 0usize);
        for c in out.chunks(bs * 2) {
            sl += c[..bs].iter().map(|s| s * s).sum::<f32>();
            sr += c[bs..].iter().map(|s| s * s).sum::<f32>();
            n += bs;
        }
        ((sl / n as f32).sqrt(), (sr / n as f32).sqrt())
    }

    #[test]
    fn expression_pan_moves_the_voice_output() {
        let run = |pan: f32| {
            let mut m = test_plugin();
            let mut io = Scripted::default();
            io.input.push_back(note_on());
            io.input.push_back(NoteEvent::PolyPan {
                timing: 0,
                voice_id: Some(5),
                channel: 0,
                note: 45,
                pan,
            });
            let out = render_setup(&mut m, &mut io, 16384, 512, &|m| {
                let p = m.params.table.pos(mp::pid(Module::Grain, 0, 7));
                m.base.values[p] = 1.0; // grain Pan 100 % swaps the emitters
            });
            channel_rms(&out, 512)
        };
        let (l, r) = run(1.0);
        assert!(r > 0.001, "right {r}");
        assert!(l < 0.01 * r, "left {l} right {r}");
        let (l, r) = run(-1.0);
        assert!(l > 0.001, "left {l}");
        assert!(r < 0.01 * l, "left {l} right {r}");
    }

    fn param_flags_and_poly_id(m: &Mangle, id: u32) -> (ParamFlags, Option<u32>) {
        let pos = m.params.table.pos(id);
        match m.params.slots[pos] {
            PSlot::F(i) => (
                m.params.floats[i].flags(),
                m.params.floats[i].poly_modulation_id(),
            ),
            PSlot::I(i) => (
                m.params.ints[i].flags(),
                m.params.ints[i].poly_modulation_id(),
            ),
            PSlot::B(i) => (
                m.params.bools[i].flags(),
                m.params.bools[i].poly_modulation_id(),
            ),
        }
    }

    #[test]
    fn host_flags_follow_the_table() {
        let m = test_plugin();
        let voices = mp::pid(Module::Global, 0, 5);
        assert!(param_flags_and_poly_id(&m, voices)
            .0
            .contains(ParamFlags::NON_AUTOMATABLE));
        let root = mp::pid(Module::Slot, 0, 0);
        assert!(param_flags_and_poly_id(&m, root)
            .0
            .contains(ParamFlags::NON_AUTOMATABLE));
        let gate = mp::pid(Module::Grain, 0, 0);
        assert_eq!(param_flags_and_poly_id(&m, gate).1, Some(gate));
        let lfo_rate = mp::pid(Module::Lfo, 0, 2);
        assert_eq!(param_flags_and_poly_id(&m, lfo_rate).1, None);
        let loop_rate = mp::pid(Module::LoopLfo, 0, 2);
        assert_eq!(param_flags_and_poly_id(&m, loop_rate).1, Some(loop_rate));
    }

    #[test]
    fn macro_reaches_master_filter() {
        let run = |with_row: bool| {
            let mut m = test_plugin();
            let mut io = Scripted::default();
            io.input.push_back(note_on());
            let out = render_setup(&mut m, &mut io, 16384, 512, &|m| {
                filter_on_lp(m, 0.5);
                let mac = m.params.table.pos(mp::pid(Module::Macro, 0, 0));
                m.base.values[mac] = 1.0;
                if with_row {
                    set_matrix_row(
                        m,
                        0,
                        matrix::ModSource::Macro(0),
                        -100.0,
                        matrix::ModDest::Fx(matrix::FxDest::Cutoff),
                    );
                }
            });
            (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt()
        };
        let with = run(true);
        let without = run(false);
        assert!(without > 0.001, "rms {without}");
        assert!(with < 0.3 * without, "with {with} without {without}");
    }

    #[test]
    fn filter_modulation_is_block_size_invariant() {
        let run = |bs: usize| {
            let mut m = test_plugin();
            let mut io = Scripted::default();
            io.input.push_back(note_on());
            render_setup(&mut m, &mut io, 16384, bs, &|m| {
                filter_on_lp(m, 0.5);
                let rate = m.params.table.pos(mp::pid(Module::Lfo, 0, 2));
                m.base.values[rate] = 0.7368;
                set_matrix_row(
                    m,
                    0,
                    matrix::ModSource::Lfo(0),
                    60.0,
                    matrix::ModDest::Fx(matrix::FxDest::Cutoff),
                );
            })
        };
        let a = run(1024);
        let b = run(64);
        // Interleave differs per block layout; compare per-channel streams.
        let split = |v: &[f32], bs: usize| {
            let mut l = Vec::new();
            let mut r = Vec::new();
            for c in v.chunks(bs * 2) {
                l.extend_from_slice(&c[..bs]);
                r.extend_from_slice(&c[bs..]);
            }
            (l, r)
        };
        let (al, ar) = split(&a, 1024);
        let (bl, br) = split(&b, 64);
        assert!(al.iter().any(|s| s.abs() > 0.001));
        let d = al
            .iter()
            .zip(&bl)
            .chain(ar.iter().zip(&br))
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(d < 1e-5, "max diff {d}");
    }

    #[test]
    fn segment_end_caps_and_breaks() {
        assert_eq!(segment_end(0, None, 512, 64), 64); // chunk cap
        assert_eq!(segment_end(500, None, 512, 64), 512); // block cap
        assert_eq!(segment_end(0, Some(40), 512, 64), 40); // break at next event
        assert_eq!(segment_end(0, Some(100), 512, 64), 64); // chunk before event
        assert_eq!(segment_end(60, Some(1000), 512, 64), 124); // event past block
        assert_eq!(segment_end(511, None, 512, 64), 512); // progress at last sample
    }

    #[test]
    fn segments_tile_block_and_break_on_events() {
        // Mirror the control-chunk loop's drain + split with these event timings
        // (sorted, duplicates allowed) and verify the resulting segmentation.
        let (block, chunk) = (200usize, 64usize);
        let events = [0usize, 10, 10, 75, 199];
        let (mut ev, mut off) = (0usize, 0usize);
        let mut applied = Vec::new();
        let mut segs: Vec<(usize, usize)> = Vec::new();
        while off < block {
            while ev < events.len() && events[ev] <= off {
                applied.push(events[ev]);
                ev += 1;
            }
            let seg_end = segment_end(off, events.get(ev).copied(), block, chunk);
            assert!(seg_end > off, "no progress at off={off}");
            assert!(seg_end - off <= chunk, "segment exceeds chunk");
            segs.push((off, seg_end));
            off = seg_end;
        }
        while ev < events.len() {
            applied.push(events[ev]);
            ev += 1;
        }
        assert_eq!(applied, vec![0, 10, 10, 75, 199]); // each event once, in order
        assert_eq!(segs.first().unwrap().0, 0);
        assert_eq!(segs.last().unwrap().1, block); // tiles [0, block)
        for w in segs.windows(2) {
            assert_eq!(w[0].1, w[1].0, "gap/overlap between segments");
        }
        for &t in &[10usize, 75, 199] {
            assert!(
                segs.iter().any(|&(s, _)| s == t),
                "event {t} not a segment boundary"
            );
        }
    }
    fn one_shot_global_lfo_held(m: &mut Mangle, io: &mut Scripted) {
        m.lfo_curves[0] = Curve::ramp();
        for _ in 0..2000 {
            render_setup(m, io, 512, 512, &|m| {
                let t = &m.params.table;
                m.base.values[t.pos(mp::pid(Module::Lfo, 0, mp::LFO_ONESHOT_IDX))] = 1.0;
            });
            if m.gmod.last_lfo[0].0 > 0.95 {
                return;
            }
        }
        panic!("one-shot never reached its end");
    }

    #[test]
    fn lfo_trig_param_restarts_global_one_shot() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        one_shot_global_lfo_held(&mut m, &mut io);
        let set = |m: &mut Mangle| {
            let t = &m.params.table;
            m.base.values[t.pos(mp::pid(Module::Lfo, 0, mp::LFO_ONESHOT_IDX))] = 1.0;
            m.base.values[t.pos(mp::pid(Module::Lfo, 0, LFO_TRIG_IDX))] = 1.0;
        };
        render_setup(&mut m, &mut io, 512, 512, &set);
        assert!(m.gmod.last_lfo[0].0 < -0.5, "{}", m.gmod.last_lfo[0].0);
        for _ in 0..400 {
            render_setup(&mut m, &mut io, 512, 512, &set);
        }
        assert!(m.gmod.last_lfo[0].0 > 0.95, "held Trig must not re-fire");
    }

    #[test]
    fn loop_window_is_capped_at_100_seconds() {
        let mut m = Mangle::default();
        m.prepare(8000.0);
        m.bank.slots[0] = Some(SampleSlot::constant(0.5, 150 * 8000, 8000.0));
        let mut io = Scripted::default();
        render_setup(&mut m, &mut io, 64, 64, &|m| {
            let t = &m.params.table;
            m.base.values[t.pos(mp::pid(Module::Slot, 0, 1))] = 0.0;
            m.base.values[t.pos(mp::pid(Module::Slot, 0, 2))] = 1.0;
        });
        let slot = m.bank.slots[0].as_ref().unwrap();
        assert_eq!(slot.loop_end - slot.loop_start, 100 * 8000);
    }

    #[test]
    fn gui_trig_request_restarts_global_one_shot() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        one_shot_global_lfo_held(&mut m, &mut io);
        m.params.lfo_trig_requests.store(1 << 3, Ordering::Release);
        render_setup(&mut m, &mut io, 512, 512, &|m| {
            let t = &m.params.table;
            m.base.values[t.pos(mp::pid(Module::Lfo, 0, mp::LFO_ONESHOT_IDX))] = 1.0;
        });
        assert!(m.gmod.last_lfo[0].0 < -0.5, "{}", m.gmod.last_lfo[0].0);
        assert_eq!(m.params.lfo_trig_requests.load(Ordering::Acquire), 0);
    }

    #[test]
    fn loop_lfo_advances_with_engine_gated_off() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        io.input.push_back(note_on());
        let off = |m: &mut Mangle| {
            let t = &m.params.table;
            m.base.values[t.pos(mp::pid(Module::Grain, 1, 0))] = 0.0;
        };
        render_setup(&mut m, &mut io, 512, 512, &off);
        let i = m.vm.voices.iter().position(|v| v.gate).unwrap();
        let a = m.vm.voices[i].last_scan[1];
        render_setup(&mut m, &mut io, 512, 512, &off);
        let b = m.vm.voices[i].last_scan[1];
        assert_ne!(a, b);
    }

    #[test]
    fn loop_lfo_trig_restarts_voice_loop_lfo() {
        let mut m = test_plugin();
        let mut io = Scripted::default();
        io.input.push_back(note_on());
        render_setup(&mut m, &mut io, 512 * 8, 512, &|_| {});
        let i = m.vm.voices.iter().position(|v| v.gate).unwrap();
        let before = m.vm.voices[i].loop_lfos[0].running_phase;
        render_setup(&mut m, &mut io, 512, 512, &|m| {
            let t = &m.params.table;
            m.base.values[t.pos(mp::pid(Module::LoopLfo, 0, LFO_TRIG_IDX))] = 1.0;
        });
        let after = m.vm.voices[i].loop_lfos[0].running_phase;
        assert!(after < before, "{after} vs {before}");
        assert!(after < 0.05, "{after}");
    }

    fn write_test_wav(path: &std::path::Path, frames: usize) {
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

    fn sync_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mangle-sync-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn set_ref(m: &Mangle, slot: usize, path: &std::path::Path, hash: u64) {
        m.params.extra.write().unwrap().slot_refs[slot] = Some(SlotRef {
            path: path.to_string_lossy().into_owned(),
            content_hash: hash,
        });
    }

    fn slot_status(m: &Mangle, slot: usize) -> loading::SlotStatus {
        m.params.loads.status.lock().unwrap()[slot].clone()
    }

    #[test]
    fn cold_sync_loads_refs_and_keeps_untouched_slots() {
        let dir = sync_dir("cold");
        let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
        write_test_wav(&a, 4800);
        write_test_wav(&b, 2400);
        let mut m = test_plugin();
        set_ref(&m, 0, &a, 0);
        set_ref(&m, 1, &b, 0);
        m.sync_slots_with_state(true);
        assert!(m.bank.slots[0].is_some() && m.bank.slots[1].is_some());
        assert_eq!(slot_status(&m, 1), loading::SlotStatus::Loaded);
        assert!(!m.loaded[1].0.is_empty() && m.loaded[1].1 != 0);

        // The ref hashes are now unknown to the state; store the real ones.
        let (h0, h1) = (m.loaded[0].1, m.loaded[1].1);
        set_ref(&m, 0, &a, h0);
        set_ref(&m, 1, &b, h1);
        m.sync_slots_with_state(true);
        let ptr = m.bank.slots[0].as_ref().unwrap().data[0].as_ptr();

        m.params.extra.write().unwrap().slot_refs[1] = None;
        m.sync_slots_with_state(true);
        assert!(m.bank.slots[1].is_none());
        assert!(m.loaded[1].0.is_empty());
        assert_eq!(slot_status(&m, 1), loading::SlotStatus::Empty);
        assert_eq!(m.bank.slots[0].as_ref().unwrap().data[0].as_ptr(), ptr);
    }

    #[test]
    fn embed_serializes_file_bytes_only_when_enabled() {
        let dir = sync_dir("embed-ser");
        let wav = dir.join("a.wav");
        write_test_wav(&wav, 2400);
        let bytes = std::fs::read(&wav).unwrap();
        let m = test_plugin();
        set_ref(&m, 0, &wav, mangle_loader::hash_bytes(&bytes));

        assert!(!m.params.serialize_fields().contains_key("mangle_embed_0"));
        m.params.extra.write().unwrap().embed_samples = true;
        let fields = m.params.serialize_fields();
        let v = fields.get("mangle_embed_0").expect("embed key");
        let (ext, b64) = v.split_once(':').unwrap();
        assert_eq!(ext, "wav");
        assert_eq!(mangle_loader::b64_decode(b64).unwrap(), bytes);
        assert!(!fields.contains_key("mangle_embed_1"));
    }

    #[test]
    fn embedded_sample_round_trips_without_the_file() {
        let dir = sync_dir("embed-rt");
        let wav = dir.join("a.wav");
        write_test_wav(&wav, 2400);
        let hash = mangle_loader::hash_file(&wav).unwrap();
        let m = test_plugin();
        set_ref(&m, 0, &wav, hash);
        m.params.extra.write().unwrap().embed_samples = true;
        let fields = m.params.serialize_fields();
        std::fs::remove_file(&wav).unwrap();

        let mut fresh = Mangle::default();
        fresh.prepare(48_000.0);
        fresh.params.deserialize_fields(&fields);
        fresh.sync_slots_with_state(true);
        assert!(fresh.bank.slots[0].is_some());
        assert_eq!(fresh.loaded[0].1, hash);
        assert_eq!(slot_status(&fresh, 0), loading::SlotStatus::Loaded);
    }

    #[test]
    fn corrupt_embedded_base64_is_ignored() {
        let dir = sync_dir("embed-bad");
        let wav = dir.join("gone.wav");
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("mangle_embed_0".to_string(), "wav:@@@".to_string());
        let mut m = test_plugin();
        m.params.deserialize_fields(&fields);
        set_ref(&m, 0, &wav, 7);
        m.sync_slots_with_state(true);
        assert!(m.bank.slots[0].is_none());
        assert!(matches!(
            slot_status(&m, 0),
            loading::SlotStatus::Missing(_)
        ));
    }

    #[test]
    fn missing_file_leaves_slot_empty_and_keeps_ref() {
        let dir = sync_dir("missing");
        let mut m = test_plugin();
        set_ref(&m, 2, &dir.join("nope.wav"), 7);
        m.sync_slots_with_state(true);
        assert!(m.bank.slots[2].is_none());
        assert!(matches!(
            slot_status(&m, 2),
            loading::SlotStatus::Missing(_)
        ));
        assert!(m.params.extra.read().unwrap().slot_refs[2].is_some());
    }

    #[test]
    fn hot_sync_clears_then_installs_through_pending_load() {
        let dir = sync_dir("hot");
        let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
        write_test_wav(&a, 4800);
        write_test_wav(&b, 2400);
        let mut m = test_plugin();
        set_ref(&m, 0, &a, 0);
        m.sync_slots_with_state(true);
        let h = m.loaded[0].1;
        set_ref(&m, 0, &b, 0);
        m.sync_slots_with_state(false);
        assert!(m.bank.slots[0].is_none());
        assert!(m.loaded[0].0.is_empty());

        let params = m.params.clone();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut queued = false;
        while std::time::Instant::now() < end && !queued {
            queued = params
                .pending_loads
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.slot == 0 && !p.consumed);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(queued, "no pending load for slot 0");

        m.sync_gui_state();
        let mut l = vec![0.0f32; 64];
        let mut r = vec![0.0f32; 64];
        m.params.read_into(&mut m.base);
        m.render_block(
            &mut l,
            &mut r,
            BlockTransport::default(),
            &mut Scripted::default(),
        );
        assert!(m.bank.slots[0].is_some());
        assert_eq!(m.loaded[0].0, b.to_string_lossy());
        assert_ne!(m.loaded[0].1, h);
    }

    #[test]
    fn hot_sync_without_refs_installs_fallback_tone() {
        let mut m = test_plugin();
        m.sync_slots_with_state(false);
        assert!(m.bank.slots[0].is_some());
        assert!(m.params.waveforms.read().unwrap()[0].is_some());
    }

    #[test]
    fn hot_sync_prefers_embedded_when_file_changed() {
        let dir = sync_dir("hot-embed");
        let wav = dir.join("a.wav");
        write_test_wav(&wav, 2400);
        let bytes = std::fs::read(&wav).unwrap();
        let hash = mangle_loader::hash_bytes(&bytes);
        let m = test_plugin();
        set_ref(&m, 0, &wav, hash);
        m.params.extra.write().unwrap().embed_samples = true;
        let fields = m.params.serialize_fields();
        write_test_wav(&wav, 4800);

        let mut m = test_plugin();
        m.params.deserialize_fields(&fields);
        set_ref(&m, 0, &wav, hash);
        m.sync_slots_with_state(false);
        assert!(m.bank.slots[0].is_none());

        let params = m.params.clone();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < end
            && !params
                .pending_loads
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.slot == 0 && !p.consumed)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let q = params.pending_loads.lock().unwrap();
        let p = q.iter().find(|p| p.slot == 0).expect("pending load");
        assert_eq!(p.content_hash, hash);
    }

    #[test]
    fn instance_seed_is_assigned_once_and_decorrelates_instances() {
        let mut a = test_plugin();
        let mut b = test_plugin();
        a.ensure_instance_seed();
        b.ensure_instance_seed();
        let sa = a.params.extra.read().unwrap().instance_seed;
        assert_ne!(sa, 0);
        a.ensure_instance_seed();
        assert_eq!(a.params.extra.read().unwrap().instance_seed, sa);

        a.params.extra.write().unwrap().instance_seed = 1;
        b.params.extra.write().unwrap().instance_seed = 2;
        a.ensure_instance_seed();
        b.ensure_instance_seed();
        let va: Vec<f32> = (0..8).map(|_| a.rng.bipolar()).collect();
        let vb: Vec<f32> = (0..8).map(|_| b.rng.bipolar()).collect();
        assert_ne!(va, vb);

        let mut c = test_plugin();
        c.params.extra.write().unwrap().instance_seed = 1;
        c.ensure_instance_seed();
        let vc: Vec<f32> = (0..8).map(|_| c.rng.bipolar()).collect();
        assert_eq!(va, vc);
    }
}
