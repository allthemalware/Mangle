//! Mangle CLAP shell (nih-plug).
//!
//! Step-5 state: the 32-slot mod matrix runs. Per block: decode rows once,
//! apply the **global pass** (destinations on the six global LFOs) using
//! last-tick source values, tick the global sources (LFOs, Drift, Noise,
//! env follower), then per voice apply the **voice pass per channel** —
//! the matrix modulates L and R independently, so each grain engine gets
//! two parameter sets and its emitters diverge (Torsion stereo modulation).
//! Loop Position lands as a window shift, Loop LFO feedback uses one-block
//! delay, Macros (with per-note offsets) feed the matrix, and pressure
//! events drive the Pressure source.
//!
//! Known simplifications, flagged inline: control rate = host block;
//! per-channel modulation of an LFO's *rate* resolves from the left
//! channel (stereo LFOs still split via St.Phase); note-expression tuning
//! not yet wired.
//!
//! NOTE: this crate is not compiled in the sandbox (nih-plug is a git
//! dependency); `API:` comments mark the likely friction points.

use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use mangle_dsp::curve::Curve;
use mangle_dsp::matrix;
use mangle_dsp::modengine::{
    apply_global, apply_voice, decode_matrix, fill_voice_sources, DecodedRow, GlobalMod,
    SourceValues, VoiceSourceInputs,
};
use mangle_dsp::params::{self as mp, flags, Map, Module, ParamInfo, ParamTable, Patch};
use mangle_dsp::resolve;
use mangle_dsp::rng::Rng;
use mangle_dsp::engine::MODE_NAMES;
use mangle_dsp::sample::{SampleBank, SampleSlot, ALGO_NAMES};
use mangle_dsp::squeeze::Squeeze;
use mangle_dsp::state::{self, ExtraState};
use mangle_dsp::state::SlotRef;
use mangle_dsp::voice::VoiceManager;

mod editor;

/// Per-slot display data for the waveform strip (peaks of the mono mix).
/// GUI-only; never serialized.
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
        (Module::Matrix, 0) | (Module::Matrix, 5) => {
            Arc::new(|v| matrix::source_name(v.max(0) as u16))
        }
        (Module::Matrix, 2) | (Module::Matrix, 4) => {
            Arc::new(|v| matrix::dest_name(v.max(0) as u16))
        }
        (Module::Slot, 0) => Arc::new(note_name),
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
                        s.trim()
                            .split_whitespace()
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
            seen_gen: 0,
            rng: Rng::new(0x564F_5944), // "VOYD"
            sample_rate: 48_000.0,
            params,
        }
    }
}

/// Build a min/max peak summary of a slot's mono mix for the GUI.
fn summarize(slot: &SampleSlot) -> WaveSummary {
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

        // Drain any GUI-side sample loads (drag-drop / dialog). The decode
        // and resample already happened on the GUI thread; here we only
        // swap the boxed sample into the bank. try_lock keeps it RT-safe —
        // if the GUI holds the lock we just pick them up next block.
        if let Ok(mut pending) = self.params.pending_loads.try_lock() {
            if !pending.is_empty() {
                let mut refs: Vec<(usize, String, u64)> = Vec::new();
                for p in pending.drain(..) {
                    let i = p.slot.min(5);
                    self.bank.slots[i] = Some(*p.sample);
                    self.refresh_waveform(i);
                    refs.push((i, p.path, p.content_hash));
                }
                drop(pending);
                if let Ok(mut st) = self.params.extra.write() {
                    for (i, path, hash) in refs {
                        st.slot_refs[i] = Some(SlotRef { path, content_hash: hash });
                    }
                }
                self.params.extra_gen.fetch_add(1, Ordering::Release);
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

        // 4. Events. Block-quantized for now.
        while let Some(event) = context.next_event() {
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
                NoteEvent::PolyModulation { voice_id, poly_modulation_id, normalized_offset, .. } => {
                    if let Some(v) =
                        self.vm.voices.iter_mut().find(|v| v.note_id == voice_id)
                    {
                        v.set_mod_offset(poly_modulation_id, normalized_offset);
                    }
                }
                NoteEvent::MonoAutomation { poly_modulation_id, normalized_value, .. } => {
                    self.set_mono_override(poly_modulation_id, normalized_value);
                }
                _ => {}
            }
        }

        // 5. Globals + per-slot settings (from the base patch — the Global
        //    and Slot modules are not matrix destinations).
        let g = resolve::global_out(&self.base, &self.params.table);
        self.vm.set_max_voices(g.voices);
        for s in 0..6u8 {
            if let Some(slot) = self.bank.slots[s as usize].as_mut() {
                let st = resolve::slot_settings(&self.base, &self.params.table, s);
                let len = slot.len();
                slot.root_note = st.root_note;
                slot.loop_start = (st.loop_start * len as f32) as usize;
                slot.loop_end =
                    ((st.loop_end * len as f32) as usize).clamp(slot.loop_start + 1, len);
                slot.gain = st.gain;
            }
        }

        // 6. Output slices.
        let output = buffer.as_slice();
        let (first, second) = output.split_at_mut(1);
        let out_l: &mut [f32] = &mut first[0];
        let out_r: &mut [f32] = &mut second[0];
        out_l.fill(0.0);
        out_r.fill(0.0);

        // 7. Control-chunk loop (~64 samples): global mod pass + source
        //    tick, then per voice the stereo voice pass and rendering. A
        //    fast Loop LFO now scans *within* a host block instead of
        //    jumping once per block.
        const CTRL_CHUNK: usize = 64;
        let rows = &self.rows[..self.n_rows];
        let mut off = 0usize;
        while off < block {
            let n = (block - off).min(CTRL_CHUNK);
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

        // 9. Stereo width (mid/side) → Squeeze → master level.
        for n in 0..block {
            let l = out_l[n];
            let r = out_r[n];
            let mid = 0.5 * (l + r);
            let side = 0.5 * (l - r) * g.stereo;
            out_l[n] = mid + side;
            out_r[n] = mid - side;
        }
        if g.squeeze {
            self.squeeze.process(out_l, out_r);
        }
        for n in 0..block {
            out_l[n] *= g.level;
            out_r[n] *= g.level;
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
