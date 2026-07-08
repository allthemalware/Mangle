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
use mangle_dsp::matrix;
use mangle_dsp::modengine::{
    apply_global, apply_voice, decode_matrix, fill_voice_sources, modulated_filter, DecodedRow,
    GlobalMod, SourceValues, VoiceSourceInputs,
};
use mangle_dsp::params::{self as mp, flags, Map, Module, ParamInfo, ParamTable, Patch};
use mangle_dsp::resolve;
use mangle_dsp::rng::Rng;
use mangle_dsp::engine::{MODE_NAMES, QUANT_NAMES};
use mangle_dsp::filter::Svf;
use mangle_dsp::sample::{SampleBank, SampleSlot, ALGO_NAMES};
use mangle_dsp::squeeze::Squeeze;

/// Master-filter response names (Fx `Type` param), display order = step index.
const FILTER_TYPE_NAMES: [&str; 4] = ["LP", "BP", "HP", "Notch"];
use mangle_dsp::state::{self, ExtraState};
use mangle_dsp::state::SlotRef;
use mangle_dsp::voice::VoiceManager;

mod editor;

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
    const N: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
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
                    bools.push(BoolParam::new(info.name.clone(), info.default_display >= 0.5));
                    slots.push(PSlot::B(bools.len() - 1));
                }
                Map::Steps { count } => {
                    let p = IntParam::new(
                        info.name.clone(),
                        info.default_display as i32,
                        IntRange::Linear { min: 0, max: count as i32 - 1 },
                    )
                    .with_value_to_string(int_formatter(info));
                    ints.push(p);
                    slots.push(PSlot::I(ints.len() - 1));
                }
                Map::Int { min, max } => {
                    let p = IntParam::new(
                        info.name.clone(),
                        info.default_display as i32,
                        IntRange::Linear { min, max },
                    )
                    .with_value_to_string(int_formatter(info));
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
                    if info.flags & flags::PN != 0 {
                        // API: nih-plug poly modulation opt-in.
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
            editor_state: EguiState::from_size(1280, 860),
            waveforms: RwLock::new([None, None, None, None, None, None]),
            extra: RwLock::new(ExtraState::default()),
            extra_gen: AtomicU64::new(1),
            panic: AtomicBool::new(false),
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
        }
    }

    /// Snapshot every host param into a normalized patch.
    fn read_into(&self, patch: &mut Patch) {
        for (pos, slot) in self.slots.iter().enumerate() {
            patch.values[pos] = match slot {
                PSlot::F(i) => self.floats[*i].value(),
                PSlot::I(i) => self.table.infos[pos].map.to_norm(self.ints[*i].value() as f32),
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
            map.insert("mangle_state".to_string(), state::hex_encode(&state::serialize(&st)));
        }
        map
    }

    fn deserialize_fields(&self, serialized: &BTreeMap<String, String>) {
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
    /// MonoAutomation overrides for poly-modulatable params.
    mono_overrides: Vec<(u32, f32)>,
    squeeze: Squeeze,
    /// Master output filter, one SVF per channel (Module::Fx). Runs after the
    /// grain sum, before Squeeze.
    filter: [Svf; 2],
    /// Smoothed filter cutoff (Hz) per channel, ramped toward the (matrix-
    /// modulated, per-channel) target across control chunks so sweeps don't
    /// zipper. Two values so stereo filter modulation diverges L/R.
    filt_cutoff: [f32; 2],
    /// Smoothed master Level / Stereo width. The map's ~5 ms smoothing contract
    /// covers these continuous, non-grain params; applying the resolved value
    /// per block unramped would zipper on automation.
    cur_level: f32,
    cur_stereo: f32,
    smooth_coef: f32,
    /// Last seen `extra` generation (curve sync).
    seen_gen: u64,
    rng: Rng,
    sample_rate: f32,
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
            mono_overrides: Vec::with_capacity(256),
            squeeze: Squeeze::new(48_000.0),
            filter: [Svf::default(); 2],
            filt_cutoff: [20_000.0; 2],
            cur_level: 1.0,
            cur_stereo: 1.0,
            smooth_coef: 1.0 - (-1.0f32 / (0.005 * 48_000.0)).exp(),
            seen_gen: 0,
            rng: Rng::new(0x564F_5944), // "VOYD"
            sample_rate: 48_000.0,
            params,
        }
    }
}

/// Build a min/max peak summary of a slot's mono mix for the GUI.
pub(crate) fn summarize(slot: &SampleSlot) -> WaveSummary {
    const BUCKETS: usize = 1024;
    let n = slot.len();
    let mut peaks = Vec::with_capacity(BUCKETS);
    if n == 0 {
        return WaveSummary { peaks, len_samples: 0 };
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
    WaveSummary { peaks, len_samples: n }
}

impl Mangle {
    fn refresh_waveform(&self, slot_index: usize) {
        if let Ok(mut w) = self.params.waveforms.write() {
            w[slot_index] = self.bank.slots[slot_index].as_ref().map(summarize);
        }
    }

    fn set_mono_override(&mut self, id: u32, value: f32) {
        if let Some(e) = self.mono_overrides.iter_mut().find(|(i, _)| *i == id) {
            e.1 = value;
        } else if self.mono_overrides.len() < self.mono_overrides.capacity() {
            self.mono_overrides.push((id, value));
        }
    }

    /// Apply one note/automation event. Called from the control-chunk loop at
    /// the event's sample offset so note timing is sample-accurate. `gate_sync`
    /// is the per-engine Gate-Sync flag set (resolved once from the base patch).
    fn apply_event(&mut self, event: NoteEvent<()>, gate_sync: &[bool; 3]) {
        match event {
            NoteEvent::NoteOn { note, velocity, voice_id, channel, .. } => {
                let vr = self.rng.bipolar();
                let v = self.vm.note_on(note, voice_id.unwrap_or(-1), channel, velocity, vr);
                for (e, gs) in gate_sync.iter().enumerate() {
                    if *gs {
                        v.loop_lfos[e].trig();
                    }
                }
            }
            NoteEvent::NoteOff { note, voice_id, .. } => {
                self.vm.note_off(note, voice_id.unwrap_or(-1));
            }
            NoteEvent::Choke { voice_id, note, .. } => {
                for v in self.vm.voices.iter_mut() {
                    let hit = match voice_id {
                        Some(id) => v.note_id == id,
                        None => v.note == note,
                    };
                    if hit {
                        v.gate = false;
                        for lane in v.lanes.iter_mut() {
                            lane.reset();
                        }
                    }
                }
            }
            NoteEvent::PolyPressure { voice_id, note, pressure, .. } => {
                for v in self.vm.voices.iter_mut() {
                    let hit = match voice_id {
                        Some(id) => v.note_id == id,
                        None => v.note == note,
                    };
                    if hit {
                        v.pressure = pressure;
                    }
                }
            }
            NoteEvent::MidiChannelPressure { pressure, .. } => {
                for v in self.vm.voices.iter_mut() {
                    if v.gate {
                        v.pressure = pressure;
                    }
                }
            }
            NoteEvent::PolyTuning { voice_id, note, tuning, .. } => {
                for v in self.vm.voices.iter_mut() {
                    let hit = match voice_id {
                        Some(id) => v.note_id == id,
                        None => v.note == note,
                    };
                    if hit {
                        v.tuning = tuning;
                    }
                }
            }
            NoteEvent::PolyVolume { voice_id, note, gain, .. } => {
                for v in self.vm.voices.iter_mut() {
                    let hit = match voice_id {
                        Some(id) => v.note_id == id,
                        None => v.note == note,
                    };
                    if hit {
                        v.expr_volume = gain.max(0.0);
                    }
                }
            }
            NoteEvent::PolyPan { voice_id, note, pan, .. } => {
                for v in self.vm.voices.iter_mut() {
                    let hit = match voice_id {
                        Some(id) => v.note_id == id,
                        None => v.note == note,
                    };
                    if hit {
                        v.expr_pan = pan.clamp(-1.0, 1.0);
                    }
                }
            }
            NoteEvent::PolyModulation { voice_id, poly_modulation_id, normalized_offset, .. } => {
                if let Some(v) = self.vm.voices.iter_mut().find(|v| v.note_id == voice_id) {
                    v.set_mod_offset(poly_modulation_id, normalized_offset);
                }
            }
            NoteEvent::MonoAutomation { poly_modulation_id, normalized_value, .. } => {
                self.set_mono_override(poly_modulation_id, normalized_value);
            }
            _ => {}
        }
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

    const MIDI_INPUT: MidiConfig = MidiConfig::Basic;
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
        self.sample_rate = buffer_config.sample_rate;
        self.params
            .sr_bits
            .store(self.sample_rate.to_bits(), Ordering::Release);
        self.squeeze = Squeeze::new(self.sample_rate);
        for f in self.filter.iter_mut() {
            f.clear();
        }
        self.smooth_coef = 1.0 - (-1.0f32 / (0.005 * self.sample_rate)).exp();
        mangle_dsp::sample::warm_tables(); // no lazy alloc on the audio thread
        mangle_dsp::spectral::warm(); // pre-build the spectral FFT plan + window

        // Load referenced samples from state. `initialize` runs with the
        // plugin deactivated, so blocking file IO + resampling is fine
        // here; live (GUI-triggered) loads will use a background task +
        // swap when the GUI lands.
        let refs: Vec<(usize, String, u64)> = self
            .params
            .extra
            .read()
            .map(|st| {
                st.slot_refs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| r.as_ref().map(|r| (i, r.path.clone(), r.content_hash)))
                    .collect()
            })
            .unwrap_or_default();
        for (i, path, expected) in refs {
            match mangle_loader::load_sample(std::path::Path::new(&path), self.sample_rate) {
                Ok(loaded) => {
                    if expected != 0 && loaded.content_hash != expected {
                        nih_log!(
                            "Mangle: slot {} content hash mismatch for '{}' (file changed?)",
                            i + 1,
                            path
                        );
                    }
                    for w in &loaded.warnings {
                        nih_log!("Mangle: slot {}: {}", i + 1, w);
                    }
                    self.bank.slots[i] = Some(loaded.slot);
                    self.refresh_waveform(i);
                }
                Err(e) => nih_log!("Mangle: slot {} load failed '{}': {}", i + 1, path, e),
            }
        }

        // Dev hook until the GUI lands: MANGLE_SAMPLE=/path/to/file fills
        // slot 1 and records the reference in state.
        if self.bank.loaded().1 == 0 {
            if let Ok(p) = std::env::var("MANGLE_SAMPLE") {
                match mangle_loader::load_sample(std::path::Path::new(&p), self.sample_rate) {
                    Ok(loaded) => {
                        nih_log!("Mangle: loaded MANGLE_SAMPLE '{}'", p);
                        if let Ok(mut st) = self.params.extra.write() {
                            st.slot_refs[0] =
                                Some(SlotRef { path: p.clone(), content_hash: loaded.content_hash });
                        }
                        self.params.extra_gen.fetch_add(1, Ordering::Release);
                        self.bank.slots[0] = Some(loaded.slot);
                        self.refresh_waveform(0);
                    }
                    Err(e) => nih_log!("Mangle: MANGLE_SAMPLE failed: {}", e),
                }
            }
        }

        if self.bank.loaded().1 == 0 {
            // Still empty: built-in fallback tone so the instrument sounds.
            self.bank.slots[0] = Some(SampleSlot::test_tone(self.sample_rate));
            self.refresh_waveform(0);
        }
        nih_log!("Mangle: {} host params generated from the table", self.params.table.len());
        true
    }

    fn reset(&mut self) {
        for v in self.vm.voices.iter_mut() {
            v.gate = false;
            v.was_sounding = false;
            for lane in v.lanes.iter_mut() {
                lane.reset();
            }
        }
        self.mono_overrides.clear();
        self.squeeze.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let block = buffer.samples();
        let bpm = context.transport().tempo.unwrap_or(120.0) as f32;
        let playing = context.transport().playing;
        let pos_sec0 = context.transport().pos_seconds();

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
        // happened on the GUI thread; here we only swap the boxed sample into
        // the bank — a pointer move, no allocation or lock. try_lock keeps it
        // RT-safe: if the GUI holds the queue lock we pick them up next block.
        if let Ok(mut pending) = self.params.pending_loads.try_lock() {
            for p in pending.drain(..) {
                let i = p.slot.min(5);
                self.bank.slots[i] = Some(*p.sample);
            }
        }

        // Panic request from the GUI (RND etc.): hard-stop every voice so
        // nothing hangs over the parameter change.
        if self.params.panic.swap(false, Ordering::AcqRel) {
            for v in self.vm.voices.iter_mut() {
                v.gate = false;
                v.was_sounding = false;
                for lane in v.lanes.iter_mut() {
                    lane.reset();
                }
            }
        }

        // 1. Host params → base patch (+ mono-modulation overrides).
        self.params.read_into(&mut self.base);
        for (id, v) in self.mono_overrides.iter() {
            if let Some(pos) = self.params.table.try_pos(*id) {
                self.base.values[pos] = *v;
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
                    slot.loop_end = ((st.loop_end * len as f32) as usize).clamp(ls + 1, len);
                }
                slot.gain = st.gain;
            }
        }

        // 6. Output slices.
        let output = buffer.as_slice();
        let (first, second) = output.split_at_mut(1);
        let out_l: &mut [f32] = first[0];
        let out_r: &mut [f32] = second[0];
        out_l.fill(0.0);
        out_r.fill(0.0);

        // 7. Control-chunk loop (~64 samples): global mod pass + source
        //    tick, then per voice the stereo voice pass and rendering. A
        //    fast Loop LFO now scans *within* a host block instead of
        //    jumping once per block.
        const CTRL_CHUNK: usize = 64;
        // Stack-copy the decoded rows so the loop body can take &mut self (for
        // sample-accurate event handling) without conflicting with a borrow on
        // self.rows. DecodedRow is Copy — see mangle-borrow-fallback.md.
        let rows_buf = self.rows;
        let rows = &rows_buf[..self.n_rows];
        let mut off = 0usize;
        let mut next_event = context.next_event();
        while off < block {
            // Apply every event due at or before this sample, then bound the
            // segment by the next event so its onset lands on the exact sample.
            while let Some(ev) = next_event {
                if (ev.timing() as usize) > off {
                    break;
                }
                self.apply_event(ev, &gate_sync);
                next_event = context.next_event();
            }
            let seg_end =
                segment_end(off, next_event.map(|e| e.timing() as usize), block, CTRL_CHUNK);
            let n = seg_end - off;
            let dt = n as f32 / self.sample_rate;
            let t_sec = if playing {
                pos_sec0.map(|s| s + off as f64 / self.sample_rate as f64)
            } else {
                None
            };

            // Global pass: last-tick sources modulate global LFO params,
            // then the sources advance one control tick.
            let mut sv_pre = SourceValues::zero();
            self.gmod.fill_sources(&mut sv_pre);
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

                let mut loop_pos_l = [0.0f32; 3];
                let mut loop_pos_r = [0.0f32; 3];
                self.scratch_l.values.clear();
                self.scratch_l.values.extend_from_slice(&self.scratch_v.values);
                apply_voice(&mut self.scratch_l, rows, &sv, 0, &mut loop_pos_l);
                self.scratch_r.values.clear();
                self.scratch_r.values.extend_from_slice(&self.scratch_v.values);
                apply_voice(&mut self.scratch_r, rows, &sv, 1, &mut loop_pos_r);

                let note = voice.note as f32 + voice.tuning;
                // Note-expression Volume/Pan (parameter map §10): a per-voice
                // gain plus a balance pan (center = unity, so a voice with no
                // expression is untouched), applied to each lane's level.
                let expr_gl = voice.expr_volume
                    * if voice.expr_pan > 0.0 { 1.0 - voice.expr_pan } else { 1.0 };
                let expr_gr = voice.expr_volume
                    * if voice.expr_pan < 0.0 { 1.0 + voice.expr_pan } else { 1.0 };
                for e in 0..3usize {
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
                    lane_l.level *= expr_gl;
                    lane_r.level *= expr_gr;
                    if !lane_l.gate && !lane_r.gate && !voice.lanes[e].is_ringing() {
                        continue;
                    }
                    // Loop LFO params from the left-modulated patch;
                    // St.Phase still splits the channels.
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
                    voice.lanes[e].process(
                        &[lane_l, lane_r],
                        &self.bank,
                        scan,
                        self.sample_rate,
                        &mut out_l[off..off + n],
                        &mut out_r[off..off + n],
                        n,
                    );
                }
            }
            off += n;
        }
        // Apply any events at/after the final sample so none are dropped (their
        // state carries into the next block).
        while let Some(ev) = next_event {
            self.apply_event(ev, &gate_sync);
            next_event = context.next_event();
        }

        // 8. Voice termination (lets the host recycle CLAP note ids).
        for voice in self.vm.voices.iter_mut() {
            if voice.was_sounding && !voice.sounding() {
                voice.was_sounding = false;
                if voice.note_id >= 0 {
                    context.send_event(NoteEvent::VoiceTerminated {
                        timing: block.saturating_sub(1) as u32,
                        voice_id: Some(voice.note_id),
                        channel: voice.channel,
                        note: voice.note,
                    });
                }
            }
        }

        // 9. Stereo width (mid/side) → filter → Squeeze → master level. Level
        //    and width are smoothed per sample (~5 ms) so automation doesn't
        //    zipper.
        let coef = self.smooth_coef;
        for n in 0..block {
            self.cur_stereo += (g.stereo - self.cur_stereo) * coef;
            let l = out_l[n];
            let r = out_r[n];
            let mid = 0.5 * (l + r);
            let side = 0.5 * (l - r) * self.cur_stereo;
            out_l[n] = mid + side;
            out_r[n] = mid - side;
        }
        // Master filter (Module::Fx): On/Type from the base patch; Cutoff/Reso
        // are matrix-modulated *per channel* (an LFO with St.Phase sweeps L and
        // R divergently). The cutoff is smoothed per channel and coefficients
        // refresh each control chunk (tan is too costly per sample).
        let fo = resolve::filter_out(&self.base, &self.params.table);
        if fo.on {
            let cut_pos = self.params.table.pos(mp::pid(Module::Fx, 0, 2));
            let res_pos = self.params.table.pos(mp::pid(Module::Fx, 0, 3));
            let cut_map = self.params.table.infos[cut_pos].map;
            // End-of-block source values drive the per-block filter target.
            let mut sv = SourceValues::zero();
            self.gmod.fill_sources(&mut sv);
            let modf = modulated_filter(
                &self.base,
                &self.rows[..self.n_rows],
                &sv,
                cut_pos as u16,
                res_pos as u16,
            );
            let target_hz = [cut_map.to_display(modf[0].0), cut_map.to_display(modf[1].0)];
            let reso = [modf[0].1, modf[1].1];
            let ctrl = CTRL_CHUNK;
            let cutoff_coef = 1.0 - (-(ctrl as f32) / (0.005 * self.sample_rate)).exp();
            let mut off = 0;
            while off < block {
                let end = (off + ctrl).min(block);
                for (cutoff, &target) in self.filt_cutoff.iter_mut().zip(target_hz.iter()) {
                    *cutoff += (target - *cutoff) * cutoff_coef;
                }
                self.filter[0].set(self.filt_cutoff[0], reso[0], self.sample_rate);
                self.filter[1].set(self.filt_cutoff[1], reso[1], self.sample_rate);
                for n in off..end {
                    out_l[n] = self.filter[0].process(out_l[n], fo.ftype);
                    out_r[n] = self.filter[1].process(out_r[n], fo.ftype);
                }
                off = end;
            }
        } else {
            // Keep the smoother tracking the target and states clear so the first
            // block after enabling starts from the right cutoff, silent.
            self.filt_cutoff = [fo.cutoff; 2];
            self.filter[0].clear();
            self.filter[1].clear();
        }
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

        ProcessStatus::Normal
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
    const CLAP_POLY_MODULATION_CONFIG: Option<PolyModulationConfig> =
        Some(PolyModulationConfig { max_voice_capacity: 16, supports_overlapping_voices: true });
}

nih_export_clap!(Mangle);

#[cfg(test)]
mod tests {
    use super::segment_end;

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
            assert!(segs.iter().any(|&(s, _)| s == t), "event {t} not a segment boundary");
        }
    }
}
