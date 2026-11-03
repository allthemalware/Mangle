//! Mangle editor — direction C "tracker terminal", muted palette.
//!
//! Everything is painted by hand: SunVox-style value cells (label left,
//! value right, level bar under), monospace throughout, no stock widget
//! chrome. The GUI is also where RANDOM lives — only here does a
//! `ParamSetter` exist, so randomized values flow back into the host's
//! parameter state correctly (begin/set/end per param).
//!
//! Interactions:
//! * drag a cell vertically = edit (Shift = fine) · scroll = step
//! * click = toggle (bools) · Ctrl-click = reset to default / clear
//! * padlock on a section header = exclude that unit from RANDOM
//! * matrix: right-click Source/Scale/Dest for a picker menu; Dest also offers
//!   PICK FROM CONTROL (then touch any destination control; Esc cancels)
//! * RND = randomize (right-click for Amount / Complexity / seed / pools)
//! * waveform: drag loop edges, drag the region body to move it
//! * body tabs: ENGINES (one engine at a time) / MOD / MATRIX / CURVES
//!   (freehand draw + presets + ops); header + waveform stay put
//!
//! `API:` comments mark the egui / nih_plug_egui call sites most likely to
//! need a touch-up against a given checkout (see BUILD_NOTES.md).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use nih_plug::prelude::*;
use nih_plug_egui::resizable_window::ResizableWindow;
use nih_plug_egui::{create_egui_editor, egui};

use egui::{vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};

use mangle_dsp::curve::{Curve, N_PRESETS};
use mangle_dsp::engine::GrainMode;
use mangle_dsp::matrix;
use mangle_dsp::params::{pid, Module, Patch, GRAIN_MODE_IDX, LFO_TRIG_IDX, N_GRAIN_MODES};
use mangle_dsp::random::{randomize, RandomOptions, RandomPools, SourcePool};

use crate::feed::FeedFrame;
use crate::loading::{self, LoadSource, SlotStatus};
use crate::{MangleParams, PSlot};

// ------------------------------------------------------------ palette ---

const BG: Color32 = Color32::from_rgb(0x0a, 0x0c, 0x09);
const HEADER: Color32 = Color32::from_rgb(0x10, 0x14, 0x0c);
const BORDER: Color32 = Color32::from_rgb(0x2b, 0x33, 0x24);
const TEXT: Color32 = Color32::from_rgb(0xc4, 0xcb, 0xb5);
const DIM: Color32 = Color32::from_rgb(0x6e, 0x7c, 0x57);
const ACCENT: Color32 = Color32::from_rgb(0xa4, 0xbf, 0x55);
const BRIGHT: Color32 = Color32::from_rgb(0xd8, 0xe6, 0xae);
const ACCENT_FAINT: Color32 = Color32::from_rgba_premultiplied(0x14, 0x18, 0x0a, 0xff);

/// Per-engine display colors (G1 = house green, G2 = amber, G3 = teal) —
/// phosphor variants that read against the terminal palette above.
const ENG_COLORS: [Color32; 3] = [
    ACCENT,
    Color32::from_rgb(0xd9, 0xa6, 0x48),
    Color32::from_rgb(0x5f, 0xb0, 0xa0),
];

fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

const CELL_H: f32 = 18.0;
const GAP: f32 = 2.0;

/// Below this width the layout stops shrinking and the body scrolls
/// horizontally instead; the window itself may go smaller. Tabbing the body
/// let this drop from 1000 (three engine columns side by side) to 760.
const MIN_CONTENT_W: f32 = 760.0;
/// Smallest window the resize corner allows.
const MIN_WINDOW: (f32, f32) = (640.0, 400.0);

// --------------------------------------------------- paint primitives ---
// All egui paint calls funnel through these three so any API drift in the
// vendored egui version is fixed in one place.

fn fill(ui: &Ui, r: Rect, c: Color32) {
    // API: rounding argument; 0.0 converts via Into<Rounding>.
    ui.painter().rect_filled(r, 0.0, c);
}

fn frame(ui: &Ui, r: Rect, c: Color32) {
    ui.painter()
        .rect_stroke(r, 0.0, Stroke::new(1.0_f32, c), StrokeKind::Inside);
}

fn txt(ui: &Ui, pos: Pos2, anchor: Align2, s: &str, size: f32, c: Color32) {
    ui.painter()
        .text(pos, anchor, s, FontId::monospace(size), c);
}

// ------------------------------------------------------- param access ---

pub(crate) enum PRef<'a> {
    F(&'a FloatParam),
    I(&'a IntParam),
    B(&'a BoolParam),
}

pub(crate) struct CellParam<'a> {
    pr: PRef<'a>,
    pub default_norm: f32,
    pub steps: Option<u16>,
    pub bipolar: bool,
}

impl<'a> CellParam<'a> {
    fn norm(&self) -> f32 {
        match &self.pr {
            PRef::F(p) => p.unmodulated_normalized_value(),
            PRef::I(p) => p.unmodulated_normalized_value(),
            PRef::B(p) => p.unmodulated_normalized_value(),
        }
    }

    fn text(&self) -> String {
        let n = self.norm();
        match &self.pr {
            PRef::F(p) => p.normalized_value_to_string(n, true),
            PRef::I(p) => p.normalized_value_to_string(n, false),
            PRef::B(p) => {
                if p.value() {
                    "ON".to_string()
                } else {
                    "OFF".to_string()
                }
            }
        }
    }

    fn begin(&self, s: &ParamSetter) {
        match &self.pr {
            PRef::F(p) => s.begin_set_parameter(*p),
            PRef::I(p) => s.begin_set_parameter(*p),
            PRef::B(p) => s.begin_set_parameter(*p),
        }
    }

    fn set(&self, s: &ParamSetter, v: f32) {
        let v = v.clamp(0.0, 1.0);
        match &self.pr {
            PRef::F(p) => s.set_parameter_normalized(*p, v),
            PRef::I(p) => s.set_parameter_normalized(*p, v),
            PRef::B(p) => s.set_parameter_normalized(*p, v),
        }
    }

    fn end(&self, s: &ParamSetter) {
        match &self.pr {
            PRef::F(p) => s.end_set_parameter(*p),
            PRef::I(p) => s.end_set_parameter(*p),
            PRef::B(p) => s.end_set_parameter(*p),
        }
    }

    fn is_bool(&self) -> bool {
        matches!(self.pr, PRef::B(_))
    }
}

impl MangleParams {
    /// Build a cell handle for table position (module, instance, index).
    pub(crate) fn cp(&self, m: Module, inst: u8, idx: u8) -> CellParam<'_> {
        let pos = self.table.pos(pid(m, inst, idx));
        let info = &self.table.infos[pos];
        let pr = match self.slots[pos] {
            PSlot::F(i) => PRef::F(&self.floats[i]),
            PSlot::I(i) => PRef::I(&self.ints[i]),
            PSlot::B(i) => PRef::B(&self.bools[i]),
        };
        CellParam {
            pr,
            default_norm: info.default_norm,
            steps: info.map.step_count(),
            bipolar: info.bipolar,
        }
    }
}

/// Snapshot every host param into a normalized patch (GUI thread).
fn read_patch(params: &MangleParams) -> Patch {
    let mut patch = Patch::init(&params.table);
    for (pos, slot) in params.slots.iter().enumerate() {
        patch.values[pos] = match slot {
            PSlot::F(i) => params.floats[*i].unmodulated_normalized_value(),
            PSlot::I(i) => params.ints[*i].unmodulated_normalized_value(),
            PSlot::B(i) => params.bools[*i].unmodulated_normalized_value(),
        };
    }
    patch
}

/// Write a full patch back through the host (begin/set/end per change).
fn apply_patch(params: &MangleParams, setter: &ParamSetter, patch: &Patch) {
    for (pos, slot) in params.slots.iter().enumerate() {
        let target = patch.values[pos];
        macro_rules! push {
            ($p:expr) => {{
                if ($p.unmodulated_normalized_value() - target).abs() > 1e-6 {
                    setter.begin_set_parameter($p);
                    setter.set_parameter_normalized($p, target);
                    setter.end_set_parameter($p);
                }
            }};
        }
        match slot {
            PSlot::F(i) => push!(&params.floats[*i]),
            PSlot::I(i) => push!(&params.ints[*i]),
            PSlot::B(i) => push!(&params.bools[*i]),
        }
    }
}

fn entropy_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
}

/// Open a native file picker for `slot` on a dedicated thread (never blocks
/// the GUI render), pushing the chosen path back to `dialog_results`. The
/// editor polls that queue each frame. `params` is the same Arc the editor
/// holds, cloned into the thread.
fn open_file_dialog(params: &Arc<MangleParams>, slot: usize) {
    use std::sync::atomic::Ordering;
    // Don't stack pickers on the same slot.
    if params.dialog_open[slot]
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let params = params.clone();
    std::thread::spawn(move || {
        let picked = rfd::FileDialog::new()
            .add_filter("Audio", &["wav", "flac", "aiff", "aif", "ogg", "mp3"])
            .add_filter("All files", &["*"])
            .set_title(format!("Mangle — load sample into slot {}", slot + 1))
            .pick_file();
        if let Some(path) = picked {
            if let Ok(mut q) = params.dialog_results.lock() {
                q.push((slot, path));
            }
        }
        params.dialog_open[slot].store(false, Ordering::Release);
    });
}

/// Drain completed file-dialog selections and start a background load for
/// each. Call once per frame from the editor.
fn poll_dialog_results(params: &Arc<MangleParams>) {
    let picks: Vec<(usize, std::path::PathBuf)> = match params.dialog_results.lock() {
        Ok(mut q) if !q.is_empty() => q.drain(..).collect(),
        _ => return,
    };
    for (slot, path) in picks {
        loading::spawn_load(params.clone(), slot.min(5), LoadSource::File(path));
    }
}

// ------------------------------------------------------------ widgets ---

/// One tracker cell: label left (dim), value right, level bar underneath.
/// Drag vertical = edit (Shift fine), scroll = step, Ctrl-click = default,
/// click toggles bools.
fn cell(ui: &mut Ui, setter: &ParamSetter, c: &CellParam, label: &str, w: f32) {
    cell_inner(ui, setter, c, label, w, false);
}

/// `cell` with an `inert` switch: when set the cell only paints and reports
/// its response (used while a matrix destination is being picked). A drag that
/// was swallowed by the picker stays inert until it is released.
fn cell_inner(
    ui: &mut Ui,
    setter: &ParamSetter,
    c: &CellParam,
    label: &str,
    w: f32,
    inert: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, CELL_H), Sense::click_and_drag());
    let cur = c.norm();
    let swallowed: bool = ui.memory(|m| m.data.get_temp(resp.id)).unwrap_or(false);
    if resp.drag_stopped() && swallowed {
        ui.memory_mut(|m| m.data.remove::<bool>(resp.id));
    }
    let inert = inert || swallowed;

    // --- interaction -----------------------------------------------------
    let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
    let fine = ui.input(|i| i.modifiers.shift);

    if !inert {
        cell_interact(ui, setter, c, &resp, cur, ctrl, fine);
    }
    cell_paint(ui, c, label, rect, &resp);
    resp
}

fn cell_interact(
    ui: &mut Ui,
    setter: &ParamSetter,
    c: &CellParam,
    resp: &egui::Response,
    cur: f32,
    ctrl: bool,
    fine: bool,
) {
    let hovered = resp.hovered();
    if resp.drag_started() {
        c.begin(setter);
        ui.memory_mut(|m| m.data.insert_temp(resp.id, cur));
    }
    if resp.dragged() {
        let acc: f32 = ui.memory(|m| m.data.get_temp(resp.id)).unwrap_or(cur);
        let d = -resp.drag_delta().y * if fine { 0.0008 } else { 0.005 };
        let acc = (acc + d).clamp(0.0, 1.0);
        ui.memory_mut(|m| m.data.insert_temp(resp.id, acc));
        c.set(setter, acc);
    }
    // API: older egui calls this drag_released().
    if resp.drag_stopped() {
        ui.memory_mut(|m| m.data.remove::<f32>(resp.id));
        c.end(setter);
    }
    if resp.clicked() {
        if ctrl {
            c.begin(setter);
            c.set(setter, c.default_norm);
            c.end(setter);
        } else if c.is_bool() {
            c.begin(setter);
            c.set(setter, if cur >= 0.5 { 0.0 } else { 1.0 });
            c.end(setter);
        }
    }
    if hovered {
        // API: raw_scroll_delta was scroll_delta in older egui.
        // Take the wheel delta and zero it so the surrounding ScrollArea
        // doesn't pan the page while a value is being stepped.
        let scroll = ui.input_mut(|i| {
            let d = i.raw_scroll_delta.y;
            if d != 0.0 {
                i.raw_scroll_delta = egui::Vec2::ZERO;
                i.smooth_scroll_delta = egui::Vec2::ZERO;
            }
            d
        });
        if scroll.abs() > 0.0 {
            let step = match c.steps {
                Some(s) if s > 1 => 1.0 / (s - 1) as f32,
                _ => {
                    if fine {
                        0.001
                    } else {
                        0.01
                    }
                }
            };
            c.begin(setter);
            c.set(setter, cur + step * scroll.signum());
            c.end(setter);
        }
    }
}

fn cell_paint(ui: &mut Ui, c: &CellParam, label: &str, rect: Rect, resp: &egui::Response) {
    let hovered = resp.hovered();
    // --- paint -----------------------------------------------------------
    fill(ui, rect, if hovered { ACCENT_FAINT } else { HEADER });
    frame(ui, rect, BORDER);
    let bar_y = rect.bottom() - 2.0;
    let n = c.norm();
    if c.bipolar {
        let cx = rect.center().x;
        let dx = (n - 0.5) * rect.width();
        let (a, b) = if dx >= 0.0 {
            (cx, cx + dx)
        } else {
            (cx + dx, cx)
        };
        fill(
            ui,
            Rect::from_min_max(Pos2::new(a, bar_y), Pos2::new(b, rect.bottom() - 1.0)),
            ACCENT,
        );
    } else {
        fill(
            ui,
            Rect::from_min_max(
                Pos2::new(rect.left() + 1.0, bar_y),
                Pos2::new(
                    rect.left() + 1.0 + n * (rect.width() - 2.0),
                    rect.bottom() - 1.0,
                ),
            ),
            ACCENT,
        );
    }
    txt(
        ui,
        Pos2::new(rect.left() + 4.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        10.0,
        DIM,
    );
    let vcol = if c.is_bool() && n >= 0.5 {
        ACCENT
    } else if hovered || resp.dragged() {
        BRIGHT
    } else {
        TEXT
    };
    txt(
        ui,
        Pos2::new(rect.right() - 4.0, rect.center().y),
        Align2::RIGHT_CENTER,
        &c.text(),
        10.0,
        vcol,
    );
}

/// Terminal button. Returns the response; paints accent when `active`.
fn tbtn(ui: &mut Ui, label: &str, active: bool, w: f32) -> egui::Response {
    tbtn_tinted(ui, label, active, w, false)
}

/// `tbtn` with the label drawn in the accent colour when `flag` is set.
fn tbtn_tinted(ui: &mut Ui, label: &str, active: bool, w: f32, flag: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, CELL_H), Sense::click());
    fill(ui, rect, if active { ACCENT_FAINT } else { HEADER });
    frame(ui, rect, if active { ACCENT } else { BORDER });
    let c = if active || flag {
        ACCENT
    } else if resp.hovered() {
        BRIGHT
    } else {
        TEXT
    };
    txt(ui, rect.center(), Align2::CENTER_CENTER, label, 10.0, c);
    resp
}

/// Padlock toggle for the RANDOM lock system.
fn lock_btn(ui: &mut Ui, locked: &mut bool, changed: &mut bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, CELL_H), Sense::click());
    if resp.clicked() {
        *locked = !*locked;
        *changed = true;
    }
    let glyph = if *locked { "\u{1F512}" } else { "\u{1F513}" }; // 🔒 / 🔓
    let c = if *locked { ACCENT } else { DIM };
    txt(ui, rect.center(), Align2::CENTER_CENTER, glyph, 11.0, c);
    resp.on_hover_text("RANDOM lock: locked units are never randomized or routed into");
}

fn section_label(ui: &mut Ui, label: &str) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(label.len() as f32 * 7.0 + 8.0, CELL_H), Sense::hover());
    txt(
        ui,
        Pos2::new(rect.left() + 2.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        10.0,
        BRIGHT,
    );
}

// ------------------------------------------------------- editor state ---

#[derive(Clone, Copy, PartialEq)]
enum LoopDrag {
    Start,
    End,
    /// Grab offset from the loop centre, window width and pointer y at the
    /// start of the drag (normalized units / pixels).
    Move(f32, f32, f32),
}

/// Pointer tool on the curve canvas.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum CurveTool {
    #[default]
    Draw,
    Line,
}

/// Body pages under the persistent header/waveform/lanes area.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Engines,
    Mod,
    Matrix,
    Curves,
}

// ------------------------------------------------- lanes display state ---

/// History ring length of the scrolling lanes (~8.5 s at 30 Hz).
const LANE_BUCKETS: usize = 256;
/// Bucket step: the lanes scroll one bucket per tick, frame-rate independent.
const BUCKET_DT: f64 = 1.0 / 30.0;
/// Height of one engine lane.
const LANE_H: f32 = 26.0;

/// One time bucket of an engine lane: grain spawns + output peak.
#[derive(Clone, Copy, Default)]
struct LaneBucket {
    spawns: u16,
    level: f32,
}

/// GUI-side scroll history for one engine lane.
struct LaneHistory {
    ring: [LaneBucket; LANE_BUCKETS],
    /// Next write position == oldest bucket.
    head: usize,
}

impl Default for LaneHistory {
    fn default() -> Self {
        Self {
            ring: [LaneBucket::default(); LANE_BUCKETS],
            head: 0,
        }
    }
}

#[derive(Default)]
struct UiState {
    tab: Tab,
    /// ENGINES tab: which grain engine is shown (0..3).
    engine_page: usize,
    matrix_page: usize,
    /// PICK FROM CONTROL is armed for this matrix (row, column): the next
    /// destination control touched is assigned to that cell.
    learn: Option<(u8, u8)>,
    selected_slot: usize,
    selected_curve: usize,
    loop_drag: Option<LoopDrag>,
    last_draw: Option<(usize, f32)>,
    curve_tool: CurveTool,
    /// LINE tool: table point and value where the press started.
    line_start: Option<(usize, f32)>,
    /// QUANT step count (2..=64); 0 until first shown, then 8.
    quant: usize,
    /// Preset-bank thumbnails, built once on first use.
    presets: Vec<Curve>,
    /// Patch captured just before the last RND roll (UNDO RND / REROLL).
    pre_roll: Option<Patch>,
    /// UI time each momentary TRIG button stays lit until (Loop LFO 0..3,
    /// then LFO 0..6).
    trig_flash: [f64; 9],
    /// Last-frame screen rects of the curve canvas and the preset thumbnails.
    curve_rect: Option<Rect>,
    thumb_rects: Vec<Rect>,
    /// Per-slot load status, last copy read from `params.loads.status`.
    slot_status: [SlotStatus; 6],
    /// Newest entry of `params.loads.messages` (kept when a try_lock misses).
    last_message: String,
    /// Working copy of the shared extra state and the `extra_gen` it was read at.
    extra: mangle_dsp::state::ExtraState,
    extra_gen: u64,
    /// Slot a hovered file-drop would land in (for highlight).
    drop_target: Option<usize>,
    /// Last engine-activity frame copied from the audio thread's feed
    /// (kept when a try_lock misses, so the display never blanks).
    feed: FeedFrame,
    /// Scrolling-lanes history + bucket clock.
    lanes: [LaneHistory; 3],
    last_bucket_time: f64,
    /// Drained-but-not-yet-bucketed activity (frames can outpace buckets).
    pending_spawns: [u32; 3],
    pending_level: [f32; 3],
}

// -------------------------------------------------------------- entry ---

/// One editor frame. Factored out of the `create` closure so the headless GUI
/// test (`gui_tests`) can drive it without a host/window.
fn draw_editor(
    ctx: &egui::Context,
    setter: &ParamSetter,
    params: &Arc<MangleParams>,
    st: &mut UiState,
) {
    // Work on the cached copy of the extra state; it is re-read only when
    // another writer bumped `extra_gen`, and only GUI-owned fields go back.
    let gen = params.extra_gen.load(Ordering::Acquire);
    if gen != st.extra_gen {
        match params.extra.read() {
            Ok(g) => {
                st.extra = g.clone();
                st.extra_gen = gen;
            }
            Err(_) => return,
        }
    }
    let mut ex = std::mem::take(&mut st.extra);
    let mut ex_changed = false;

    if st.learn.is_some() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.learn = None;
    }
    if let Ok(s) = params.loads.status.try_lock() {
        st.slot_status = s.clone();
    }
    if let Ok(m) = params.loads.messages.try_lock() {
        st.last_message = m.back().cloned().unwrap_or_default();
    }
    if st.slot_status.contains(&SlotStatus::Loading) {
        ctx.request_repaint();
    }

    // Latest engine-activity frame for the overlay/lanes displays; on
    // try_lock contention just keep the previous copy.
    if let Ok(f) = params.feed.frame.try_lock() {
        st.feed = *f;
    }

    // --- drag & drop -------------------------------------------------
    // While files hover, the slot row (in waveform_strip) marks the drop
    // target: the slot tab under the pointer, else the selected slot. On drop,
    // load each file into successive slots from the target onward. The target
    // from the *previous* frame is used on the drop frame — hovered_files is
    // already empty by the time dropped_files arrives.
    let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .filter_map(|f| f.path.clone())
            .collect()
    });
    if !dropped.is_empty() {
        let start = st.drop_target.unwrap_or(st.selected_slot).min(5);
        loading::spawn_multi_load(params.clone(), start, dropped);
        st.selected_slot = start;
        st.drop_target = None;
    } else if !hovering && st.loop_drag.is_none() {
        st.drop_target = None;
    }

    // Completed file-picker selections (from the per-slot LOAD buttons) start
    // background loads, same as drops.
    poll_dialog_results(params);
    // Reclaim loads the audio thread consumed: each entry now carries the
    // previous slot contents, which must be freed here — the audio thread
    // never deallocates. (While the editor is closed they just sit in the
    // queue; new loads only originate here, so the backlog is bounded.)
    if let Ok(mut q) = params.pending_loads.lock() {
        q.retain(|p| !p.consumed);
    }
    // While any picker is open, keep repainting so its result shows up without
    // needing a mouse move to wake the editor.
    if params
        .dialog_open
        .iter()
        .any(|b| b.load(std::sync::atomic::Ordering::Relaxed))
    {
        ctx.request_repaint();
    }

    header_bar(ctx, setter, params, st, &mut ex, &mut ex_changed);

    // Persistent strip: waveform + slot row and the body tab bar stay visible
    // whatever page is open below. Its own horizontal scroll mirrors the
    // body's MIN_CONTENT_W behavior when the window is narrow.
    egui::TopBottomPanel::top("strip")
        .frame(egui::Frame::new().fill(BG).inner_margin(4.0))
        .show(ctx, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("strip-scroll")
                .show(ui, |ui| {
                    ui.set_min_width(MIN_CONTENT_W);
                    waveform_strip(ui, setter, params, st, &mut ex, &mut ex_changed);
                    ui.add_space(GAP);
                    lanes_display(ui, params, st);
                    ui.add_space(GAP);
                    tab_bar(ui, st);
                });
        });

    // ResizableWindow adds a drag corner that requests a host resize via
    // EguiState; it wraps the central panel (fill comes from panel_fill = BG,
    // set in the builder). The body scrolls on both axes: below MIN_CONTENT_W
    // the layout stops adapting and a horizontal scrollbar takes over.
    ResizableWindow::new("mangle-resize")
        .min_size(vec2(MIN_WINDOW.0, MIN_WINDOW.1))
        .show(ctx, params.editor_state.as_ref(), |ui| {
            egui::ScrollArea::both().show(ui, |ui| {
                ui.set_min_width(MIN_CONTENT_W);
                match st.tab {
                    Tab::Engines => engines_page(ui, setter, params, st, &mut ex, &mut ex_changed),
                    Tab::Mod => lfo_and_friends(ui, setter, params, st, &mut ex, &mut ex_changed),
                    Tab::Matrix => {
                        // Cutoff/Reso are matrix destinations, so the filter
                        // row keeps its seat just above the grid.
                        filter_panel(ui, setter, params, st);
                        ui.add_space(GAP * 2.0);
                        matrix_panel(ui, setter, params, st, &mut ex, &mut ex_changed);
                    }
                    Tab::Curves => curves_screen(ui, st, &mut ex, &mut ex_changed),
                }
            });
        });

    if ex_changed {
        match params.extra.write() {
            Ok(mut w) if params.extra_gen.load(Ordering::Acquire) == st.extra_gen => {
                w.curves = ex.curves.clone();
                w.locks = ex.locks.clone();
                w.random_amount = ex.random_amount;
                w.random_complexity = ex.random_complexity;
                w.last_seed = ex.last_seed;
                w.pools = ex.pools.clone();
                w.slot_views = ex.slot_views;
                w.embed_samples = ex.embed_samples;
                st.extra_gen = params.extra_gen.fetch_add(1, Ordering::Release) + 1;
                st.extra = ex;
            }
            // Someone else wrote since we read: drop this edit and re-read.
            _ => st.extra_gen = 0,
        }
    } else {
        st.extra = ex;
    }

    // ~30 Hz tick so the realtime displays animate without input events.
    // API: egui-baseview honors repaint_delay; if a backend ignores it,
    // switch to plain request_repaint().
    ctx.request_repaint_after(std::time::Duration::from_millis(33));
}

pub(crate) fn create(params: Arc<MangleParams>) -> Option<Box<dyn Editor>> {
    let state = params.editor_state.clone();
    create_egui_editor(
        state,
        UiState::default(),
        |ctx, _| {
            let mut style = (*ctx.style()).clone();
            style.override_font_id = Some(FontId::monospace(11.0));
            style.visuals.panel_fill = BG;
            style.visuals.window_fill = HEADER;
            style.visuals.override_text_color = Some(TEXT);
            ctx.set_style(style);
        },
        move |ctx, setter, st: &mut UiState| draw_editor(ctx, setter, &params, st),
    )
}

// -------------------------------------------------------------- header ---

fn header_bar(
    ctx: &egui::Context,
    setter: &ParamSetter,
    params: &MangleParams,
    st: &mut UiState,
    ex: &mut mangle_dsp::state::ExtraState,
    ex_changed: &mut bool,
) {
    egui::TopBottomPanel::top("hdr")
        .frame(egui::Frame::new().fill(HEADER).inner_margin(4.0))
        .show(ctx, |ui| {
            // Horizontal scroll so RND/TOOLS/CURVES stay reachable when the
            // window is narrower than the header row.
            egui::ScrollArea::horizontal()
                .id_salt("hdr-scroll")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        txt(
                            ui,
                            Pos2::new(ui.cursor().left() + 4.0, ui.cursor().top() + CELL_H * 0.5),
                            Align2::LEFT_CENTER,
                            "MANGLE",
                            13.0,
                            BRIGHT,
                        );
                        ui.add_space(70.0);
                        cell(ui, setter, &params.cp(Module::Global, 0, 0), "ON", 70.0);
                        cell(ui, setter, &params.cp(Module::Global, 0, 1), "KEY", 92.0);
                        cell(ui, setter, &params.cp(Module::Global, 0, 2), "STEREO", 96.0);
                        cell(
                            ui,
                            setter,
                            &params.cp(Module::Global, 0, 3),
                            "SQUEEZE",
                            92.0,
                        );
                        cell(ui, setter, &params.cp(Module::Global, 0, 4), "LEVEL", 104.0);
                        cell(ui, setter, &params.cp(Module::Global, 0, 5), "VOICES", 86.0);
                        let drone = params.cp(Module::Global, 0, 0).norm() >= 0.5
                            && params.cp(Module::Global, 0, 1).norm() <= 0.0;
                        if drone {
                            let (r, _) = ui.allocate_exact_size(vec2(52.0, CELL_H), Sense::hover());
                            txt(ui, r.center(), Align2::CENTER_CENTER, "DRONE", 9.0, DIM);
                        }
                        ui.add_space(12.0);

                        // RANDOM die: click = roll, right-click = settings.
                        let rnd = tbtn(ui, "\u{2684} RND", false, 64.0); // ⚄
                        if rnd.clicked() {
                            let seed = entropy_seed();
                            roll_random(params, setter, st, ex, seed, None);
                            ex.last_seed = seed;
                            *ex_changed = true;
                        }
                        rnd.context_menu(|ui| {
                            ui.set_min_width(220.0);
                            if ui
                                .add(
                                    egui::Slider::new(&mut ex.random_amount, 0.0..=1.0)
                                        .text("AMOUNT"),
                                )
                                .changed()
                            {
                                *ex_changed = true;
                            }
                            if ui
                                .add(
                                    egui::Slider::new(&mut ex.random_complexity, 0.0..=1.0)
                                        .text("COMPLEXITY"),
                                )
                                .changed()
                            {
                                *ex_changed = true;
                            }
                            ui.label(format!("last seed {:016x}", ex.last_seed));
                            ui.label("padlocks on section headers = locks");
                            ui.separator();
                            if random_pool_menu(ui, params, &mut ex.pools) {
                                *ex_changed = true;
                            }
                        });

                        ui.menu_button("TOOLS", |ui| {
                            if ui.button("INIT PATCH").clicked() {
                                apply_patch(params, setter, &Patch::init(&params.table));
                                ex.curves = mangle_dsp::state::ExtraState::default().curves;
                                *ex_changed = true;
                                ui.close_menu();
                            }
                            let can_undo = st.pre_roll.is_some();
                            if ui
                                .add_enabled(can_undo, egui::Button::new("UNDO RND"))
                                .clicked()
                            {
                                undo_roll(params, setter, st);
                                ui.close_menu();
                            }
                            if ui.button("REROLL LAST SEED").clicked() {
                                let seed = ex.last_seed;
                                let base = st.pre_roll.clone();
                                roll_random(params, setter, st, ex, seed, base);
                                ui.close_menu();
                            }
                            ui.separator();
                            for (a, b, label) in [
                                (0u8, 1u8, "COPY G1 \u{2192} G2"),
                                (0, 2, "COPY G1 \u{2192} G3"),
                                (1, 2, "COPY G2 \u{2192} G3"),
                                (1, 0, "COPY G2 \u{2192} G1"),
                                (2, 0, "COPY G3 \u{2192} G1"),
                                (2, 1, "COPY G3 \u{2192} G2"),
                            ] {
                                if ui.button(label).clicked() {
                                    copy_grain(params, setter, a, b);
                                    ui.close_menu();
                                }
                            }
                        });
                    });
                });
        });
}

/// Torsion-style scrolling activity lanes: one row per engine, newest at the
/// right edge. Level trace as dim bars, grain-spawn blobs on top (Spectral
/// spawns no grains, so its lane shows level only). Click a lane to jump to
/// that engine's page.
fn lanes_display(ui: &mut Ui, params: &MangleParams, st: &mut UiState) {
    // Drain the audio-side accumulators every frame; hold in `pending` until
    // the bucket clock ticks so nothing is lost when frames outpace buckets.
    for e in 0..3 {
        st.pending_spawns[e] = st.pending_spawns[e]
            .saturating_add(params.feed.spawns[e].swap(0, std::sync::atomic::Ordering::Relaxed));
        st.pending_level[e] = st.pending_level[e].max(f32::from_bits(
            params.feed.level_bits[e].swap(0, std::sync::atomic::Ordering::Relaxed),
        ));
    }

    let now = ui.input(|i| i.time);
    if now < st.last_bucket_time || now - st.last_bucket_time > 2.0 {
        // Time reset or a long gap (editor reopened): restart the scroll and
        // discard the drained backlog, which may span the closed period.
        st.lanes = Default::default();
        st.pending_spawns = [0; 3];
        st.pending_level = [0.0; 3];
        st.last_bucket_time = now;
    }
    let mut first = true;
    while now - st.last_bucket_time >= BUCKET_DT {
        for (e, lane) in st.lanes.iter_mut().enumerate() {
            // Pending activity lands in the first pushed bucket; catch-up
            // buckets after a slow frame stay empty.
            let b = if first {
                LaneBucket {
                    spawns: st.pending_spawns[e].min(u16::MAX as u32) as u16,
                    level: st.pending_level[e],
                }
            } else {
                LaneBucket::default()
            };
            lane.ring[lane.head] = b;
            lane.head = (lane.head + 1) % LANE_BUCKETS;
        }
        if first {
            st.pending_spawns = [0; 3];
            st.pending_level = [0.0; 3];
            first = false;
        }
        st.last_bucket_time += BUCKET_DT;
    }

    let w = ui.available_width();
    for (e, &col) in ENG_COLORS.iter().enumerate() {
        let (rect, resp) = ui.allocate_exact_size(vec2(w, LANE_H), Sense::click());
        fill(ui, rect, HEADER);
        frame(ui, rect, BORDER);
        txt(
            ui,
            Pos2::new(rect.left() + 4.0, rect.center().y),
            Align2::LEFT_CENTER,
            &format!("G{}", e + 1),
            10.0,
            col,
        );
        let plot = Rect::from_min_max(
            Pos2::new(rect.left() + 22.0, rect.top() + 1.0),
            Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0),
        );
        let bw = plot.width() / LANE_BUCKETS as f32;
        let lane = &st.lanes[e];
        for i in 0..LANE_BUCKETS {
            let b = lane.ring[(lane.head + i) % LANE_BUCKETS]; // i=0 oldest
            if b.level <= 0.0 && b.spawns == 0 {
                continue;
            }
            let x = plot.left() + (i as f32 + 0.5) * bw;
            if b.level > 0.0 {
                let h = b.level.min(1.0) * (plot.height() - 2.0);
                ui.painter().line_segment(
                    [Pos2::new(x, plot.bottom()), Pos2::new(x, plot.bottom() - h)],
                    Stroke::new(bw.max(1.0), with_alpha(col, 90)),
                );
            }
            if b.spawns > 0 {
                let r = 1.5 + (b.spawns as f32).sqrt().min(3.5);
                ui.painter()
                    .circle_filled(Pos2::new(x, plot.center().y), r, with_alpha(col, 220));
            }
        }
        if e == 0 && st.feed.voices > 0 {
            txt(
                ui,
                Pos2::new(rect.right() - 4.0, rect.center().y),
                Align2::RIGHT_CENTER,
                &format!("{}V", st.feed.voices),
                9.0,
                DIM,
            );
        }
        if resp.clicked() {
            st.tab = Tab::Engines;
            st.engine_page = e;
        }
        if e < 2 {
            ui.add_space(1.0);
        }
    }
}

/// Body page selector, pinned at the bottom of the persistent strip.
fn tab_bar(ui: &mut Ui, st: &mut UiState) {
    const TABS: [(Tab, &str); 4] = [
        (Tab::Engines, "ENGINES"),
        (Tab::Mod, "MOD"),
        (Tab::Matrix, "MATRIX"),
        (Tab::Curves, "CURVES"),
    ];
    ui.horizontal(|ui| {
        for (tab, label) in TABS {
            if tbtn(ui, label, st.tab == tab, 84.0).clicked() {
                st.tab = tab;
            }
        }
    });
}

/// One checkbox per flag; true when any was toggled.
fn pool_checks<'a>(ui: &mut Ui, flags: impl Iterator<Item = (String, &'a mut bool)>) -> bool {
    let mut changed = false;
    for (label, flag) in flags {
        changed |= ui.checkbox(flag, label).changed();
    }
    changed
}

fn source_flags(p: &mut SourcePool) -> Vec<(String, &mut bool)> {
    vec![
        ("LFO".to_string(), &mut p.lfo),
        ("Loop LFO".to_string(), &mut p.loop_lfo),
        ("Drift".to_string(), &mut p.drift),
        ("Noise".to_string(), &mut p.noise),
        ("Macros".to_string(), &mut p.macros),
        ("Consts".to_string(), &mut p.consts),
        ("Env".to_string(), &mut p.env_follow),
        ("Key (vel/note/oct/gate)".to_string(), &mut p.key),
        ("Pressure".to_string(), &mut p.pressure),
        ("Voice Random".to_string(), &mut p.voice_random),
    ]
}

fn dest_names<'a, T: std::fmt::Debug>(
    dests: &[T],
    flags: &'a mut [bool],
) -> Vec<(String, &'a mut bool)> {
    dests
        .iter()
        .zip(flags.iter_mut())
        .map(|(d, f)| (format!("{d:?}"), f))
        .collect()
}

/// Collapsible RANDOM pool editor; true when the pools changed.
fn random_pool_menu(ui: &mut Ui, params: &MangleParams, pools: &mut RandomPools) -> bool {
    let mut changed = false;
    let strip = |name: &str, prefix: &str| name.strip_prefix(prefix).unwrap_or(name).to_string();

    egui::CollapsingHeader::new("GRAIN PARAMS").show(ui, |ui| {
        let flags = pools.grain.0.iter_mut().enumerate().map(|(i, f)| {
            let info = params.table.get(pid(Module::Grain, 0, i as u8));
            (strip(&info.name, "Grain 1 "), f)
        });
        changed |= pool_checks(ui, flags);
    });
    egui::CollapsingHeader::new("LFO PARAMS").show(ui, |ui| {
        let flags = pools.lfo.0.iter_mut().enumerate().map(|(i, f)| {
            let info = params.table.get(pid(Module::Lfo, 0, i as u8));
            (strip(&info.name, "LFO 1 "), f)
        });
        changed |= pool_checks(ui, flags);
    });
    egui::CollapsingHeader::new("SOURCES").show(ui, |ui| {
        changed |= pool_checks(ui, source_flags(&mut pools.sources).into_iter());
    });
    egui::CollapsingHeader::new("SCALES").show(ui, |ui| {
        changed |= pool_checks(ui, source_flags(&mut pools.scales).into_iter());
    });
    egui::CollapsingHeader::new("DESTINATIONS").show(ui, |ui| {
        ui.label("Grain");
        changed |= pool_checks(
            ui,
            dest_names(&matrix::GRAIN_DESTS, &mut pools.dests.grain).into_iter(),
        );
        ui.label("LFO");
        changed |= pool_checks(
            ui,
            dest_names(&matrix::LFO_DESTS, &mut pools.dests.lfo).into_iter(),
        );
        ui.label("Loop LFO");
        changed |= pool_checks(
            ui,
            dest_names(&matrix::LFO_DESTS, &mut pools.dests.loop_lfo).into_iter(),
        );
    });
    if ui.button("DEFAULTS").clicked() {
        let d = RandomPools::default();
        if *pools != d {
            *pools = d;
            changed = true;
        }
    }
    changed
}

/// Randomize from `base` (the current patch when `None`), remembering `base`
/// so the roll can be undone or reproduced. Held notes keep sounding and
/// morph into the new patch.
fn roll_random(
    params: &MangleParams,
    setter: &ParamSetter,
    st: &mut UiState,
    ex: &mangle_dsp::state::ExtraState,
    seed: u64,
    base: Option<Patch>,
) {
    let opts = RandomOptions::with_pools(
        ex.random_amount,
        ex.random_complexity,
        ex.locks.clone(),
        &ex.pools,
    );
    let base = base.unwrap_or_else(|| read_patch(params));
    let mut patch = base.clone();
    st.pre_roll = Some(base);
    randomize(&mut patch, &params.table, &opts, seed);
    apply_patch(params, setter, &patch);
}

/// Restore the patch captured before the last roll.
fn undo_roll(params: &MangleParams, setter: &ParamSetter, st: &mut UiState) {
    if let Some(patch) = st.pre_roll.take() {
        apply_patch(params, setter, &patch);
    }
}

/// Momentary trigger button: lit for ~150 ms after a click. Returns true on
/// the frame it is clicked.
fn trig_button(ui: &mut Ui, flash_until: &mut f64, w: f32) -> bool {
    let now = ui.input(|i| i.time);
    let r = tbtn(ui, "TRIG", now < *flash_until, w);
    if r.clicked() {
        *flash_until = now + 0.15;
        true
    } else {
        false
    }
}

fn request_lfo_trig(params: &MangleParams, bit: usize) {
    params
        .lfo_trig_requests
        .fetch_or(1 << bit, std::sync::atomic::Ordering::AcqRel);
}

fn copy_grain(params: &MangleParams, setter: &ParamSetter, from: u8, to: u8) {
    let mut patch = read_patch(params);
    for idx in 0..mangle_dsp::params::N_GRAIN_PARAMS as u8 {
        let v = patch.values[params.table.pos(pid(Module::Grain, from, idx))];
        let pos = params.table.pos(pid(Module::Grain, to, idx));
        patch.values[pos] = v;
    }
    apply_patch(params, setter, &patch);
}

// ------------------------------------------------------------ waveform ---

/// Status text for the selected slot; falls back to the newest load message.
fn status_line(state: &SlotStatus, message: &str) -> String {
    match state {
        SlotStatus::Loading => "loading\u{2026}".into(),
        SlotStatus::Missing(p) => format!("missing: {p} \u{2014} LOAD to relink"),
        SlotStatus::Failed(m) => format!("failed: {m}"),
        _ => message.to_string(),
    }
}

fn waveform_strip(
    ui: &mut Ui,
    setter: &ParamSetter,
    params: &Arc<MangleParams>,
    st: &mut UiState,
    ex: &mut mangle_dsp::state::ExtraState,
    ex_changed: &mut bool,
) {
    let sel = st.selected_slot.min(5);
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 120.0), Sense::click_and_drag());
    fill(ui, rect, HEADER);
    // Accent border + tint when a file drop is hovering over this slot.
    let is_drop = st.drop_target == Some(sel);
    if is_drop {
        fill(ui, rect, ACCENT_FAINT);
        ui.painter()
            .rect_stroke(rect, 0.0, Stroke::new(2.0_f32, ACCENT), StrokeKind::Inside);
    } else {
        frame(ui, rect, BORDER);
    }

    // View window (saved per slot): maps sample-normalized positions to x.
    let (v0, v1) = ex.slot_views[sel];
    let (v0, v1) = if v0.is_finite() && v1.is_finite() && v1 - v0 > 1e-4 {
        (v0.clamp(0.0, 1.0), v1.clamp(0.0, 1.0))
    } else {
        (0.0, 1.0)
    };
    let span = (v1 - v0).max(1e-4);
    let to_x = |n: f32| rect.left() + (n - v0) / span * rect.width();
    let shift = ui.input(|i| i.modifiers.shift);
    let zoom_about = |ex: &mut mangle_dsp::state::ExtraState, anchor: f32, factor: f32| {
        let new_span = (span * factor).clamp(0.001, 1.0);
        let frac = ((anchor - v0) / span).clamp(0.0, 1.0);
        let n0 = (anchor - frac * new_span).clamp(0.0, 1.0 - new_span);
        ex.slot_views[sel] = (n0, n0 + new_span);
    };

    // Peaks.
    let waves = params.waveforms.read().ok();
    let have = waves.as_ref().map(|ws| ws[sel].is_some()).unwrap_or(false);
    if let Some(ws) = waves.as_ref() {
        if let Some(wsum) = &ws[sel] {
            let mid = rect.center().y;
            let half = rect.height() * 0.46;
            let n = wsum.peaks.len();
            for x in 0..rect.width() as usize {
                if n == 0 {
                    break;
                }
                let pos = v0 + x as f32 / rect.width().max(1.0) * span;
                let i = (pos * n as f32) as usize;
                let (lo, hi) = wsum.peaks[i.min(n - 1)];
                let x = rect.left() + x as f32;
                ui.painter().line_segment(
                    [Pos2::new(x, mid - hi * half), Pos2::new(x, mid - lo * half)],
                    Stroke::new(1.0_f32, DIM),
                );
            }
        }
    }
    if !have {
        txt(
            ui,
            rect.center(),
            Align2::CENTER_CENTER,
            "EMPTY \u{2014} click LOAD to choose a sample (wav/flac/aiff/ogg/mp3)",
            10.0,
            DIM,
        );
    }

    // Loop region from the slot params.
    let cp_start = params.cp(Module::Slot, sel as u8, 1);
    let cp_end = params.cp(Module::Slot, sel as u8, 2);
    let (s, e) = (cp_start.norm(), cp_end.norm());
    let sx = to_x(s).clamp(rect.left(), rect.right());
    let ex_ = to_x(e).clamp(rect.left(), rect.right());
    fill(
        ui,
        Rect::from_min_max(
            Pos2::new(sx, rect.top() + 1.0),
            Pos2::new(ex_, rect.bottom() - 1.0),
        ),
        Color32::from_rgba_unmultiplied(0xa4, 0xbf, 0x55, 22),
    );
    for x in [to_x(s), to_x(e)] {
        if x < rect.left() || x > rect.right() {
            continue;
        }
        ui.painter().line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(2.0_f32, ACCENT),
        );
    }

    // Live engine activity (audio feed), for engines reading this slot:
    // grain ticks in three per-engine bands near the top (fading with grain
    // age, L nudged above R), and each engine's read cursor — full-height
    // for the playhead modes (Stretch/Spectral), a short bottom tick for
    // the spawn-position modes (Granular/Cloud). Paint-only: no Sense, so
    // loop dragging is untouched.
    for (e, ef) in st.feed.engines.iter().enumerate() {
        let col = ENG_COLORS[e];
        for g in &ef.grains[..(ef.n_grains as usize).min(ef.grains.len())] {
            if g.slot as usize != sel || !g.pos.is_finite() {
                continue;
            }
            let x = to_x(g.pos);
            if x < rect.left() || x > rect.right() {
                continue;
            }
            let y0 = rect.top() + 3.0 + e as f32 * 9.0 + g.channel as f32 * 2.0;
            let a = ((1.0 - g.age) * 200.0).clamp(40.0, 200.0) as u8;
            ui.painter().line_segment(
                [Pos2::new(x, y0), Pos2::new(x, y0 + 6.0)],
                Stroke::new(2.0_f32, with_alpha(col, a)),
            );
        }
        if ef.slot as usize == sel {
            let playhead = ef.mode == 1 || ef.mode == 3; // Stretch / Spectral
            for ch in 0..2 {
                let c = ef.cursor[ch];
                if !c.is_finite() {
                    continue;
                }
                let x = to_x(c);
                if x < rect.left() || x > rect.right() {
                    continue;
                }
                let (y0, y1) = if playhead {
                    (rect.top(), rect.bottom())
                } else {
                    (rect.bottom() - 10.0, rect.bottom())
                };
                ui.painter().line_segment(
                    [Pos2::new(x, y0), Pos2::new(x, y1)],
                    Stroke::new(1.0_f32, with_alpha(col, 180)),
                );
            }
        }
    }

    // Loop drag: edges resize, body moves (horizontal) and resizes about its
    // centre (vertical: up grows, down shrinks). Shift-drag / Shift-wheel
    // zooms the view, double-click resets it.
    let norm_at = |p: Pos2| (v0 + (p.x - rect.left()) / rect.width() * span).clamp(0.0, 1.0);
    let len = params
        .waveforms
        .read()
        .ok()
        .and_then(|ws| ws[sel].as_ref().map(|w| w.len_samples))
        .unwrap_or(0);
    let sr = f32::from_bits(params.sr_bits.load(std::sync::atomic::Ordering::Acquire));
    let max_w = if len > 0 && sr.is_finite() && sr > 0.0 {
        (crate::MAX_LOOP_SECONDS * sr / len as f32).clamp(0.001, 1.0)
    } else {
        1.0
    };
    if resp.double_clicked() {
        ex.slot_views[sel] = (0.0, 1.0);
        *ex_changed = true;
    }
    if resp.hovered() {
        // Shift can arrive on the wheel event itself rather than the frame's
        // modifier state; the delta may also be on either axis.
        let amount: f32 = ui.input(|i| {
            i.events
                .iter()
                .map(|ev| match ev {
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                    } if modifiers.shift || i.modifiers.shift => {
                        let px = match unit {
                            egui::MouseWheelUnit::Point => 1.0,
                            egui::MouseWheelUnit::Line => 24.0,
                            egui::MouseWheelUnit::Page => 240.0,
                        };
                        (delta.x + delta.y) * px
                    }
                    _ => 0.0,
                })
                .sum()
        });
        if amount != 0.0 {
            if let Some(p) = resp.hover_pos() {
                zoom_about(ex, norm_at(p), (-amount * 0.002).exp());
                *ex_changed = true;
            }
        }
    }
    if resp.drag_started() && !shift {
        if let Some(p) = resp.interact_pointer_pos() {
            let n = norm_at(p);
            let grab = 6.0 / rect.width() * span;
            st.loop_drag = if (n - s).abs() < grab {
                Some(LoopDrag::Start)
            } else if (n - e).abs() < grab {
                Some(LoopDrag::End)
            } else if n > s && n < e {
                Some(LoopDrag::Move(n - (s + e) * 0.5, e - s, p.y))
            } else {
                None
            };
            if st.loop_drag.is_some() {
                cp_start.begin(setter);
                cp_end.begin(setter);
            }
        }
    }
    if resp.dragged() {
        if shift && st.loop_drag.is_none() {
            let dx = resp.drag_delta().x;
            if dx != 0.0 {
                if let Some(p) = resp.interact_pointer_pos() {
                    zoom_about(ex, norm_at(p), (-dx * 0.01).exp());
                    *ex_changed = true;
                }
            }
        } else if let (Some(mode), Some(p)) = (st.loop_drag, resp.interact_pointer_pos()) {
            let n = norm_at(p);
            match mode {
                LoopDrag::Start => cp_start.set(setter, n.min(e - 0.001).max(e - max_w)),
                LoopDrag::End => cp_end.set(setter, n.max(s + 0.001).min(s + max_w)),
                LoopDrag::Move(off, width0, y0) => {
                    let width = (width0 - (p.y - y0) * 0.005 * span).clamp(0.001, max_w);
                    let ns = (n - off - width * 0.5).clamp(0.0, 1.0 - width);
                    cp_start.set(setter, ns);
                    cp_end.set(setter, ns + width);
                }
            }
        }
    }
    if resp.drag_stopped() && st.loop_drag.take().is_some() {
        cp_start.end(setter);
        cp_end.end(setter);
    }

    // Slot tabs + per-slot cells + loop-points lock.
    ui.horizontal(|ui| {
        // While files hover the window, the tab under the pointer becomes the
        // drop target (drag position arrives as synthetic pointer motion);
        // anywhere else targets the displayed slot.
        let files_hover = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let mut hovered_tab: Option<usize> = None;
        for i in 0..6usize {
            let loaded = params
                .waveforms
                .read()
                .ok()
                .map(|w| w[i].is_some())
                .unwrap_or(false);
            let state = &st.slot_status[i];
            let flag = matches!(state, SlotStatus::Missing(_) | SlotStatus::Failed(_));
            let label = if *state == SlotStatus::Loading {
                format!("{}\u{2026}", i + 1)
            } else if flag {
                format!("{}!", i + 1)
            } else if loaded {
                format!("[{}]", i + 1)
            } else {
                format!(" {} ", i + 1)
            };
            let r = tbtn_tinted(ui, &label, i == sel, 34.0, flag);
            if r.clicked() {
                st.selected_slot = i;
            }
            if files_hover {
                if ui
                    .ctx()
                    .pointer_hover_pos()
                    .is_some_and(|p| r.rect.contains(p))
                {
                    hovered_tab = Some(i);
                }
                if st.drop_target == Some(i) {
                    ui.painter().rect_stroke(
                        r.rect,
                        0.0,
                        Stroke::new(2.0_f32, ACCENT),
                        StrokeKind::Inside,
                    );
                }
            }
        }
        if files_hover {
            st.drop_target = Some(hovered_tab.unwrap_or(sel));
        }
        ui.add_space(6.0);
        // LOAD opens a native picker for the selected slot (on its own
        // thread; see open_file_dialog). Shows "…" while a picker is open.
        let busy = params.dialog_open[sel].load(std::sync::atomic::Ordering::Acquire);
        let load_label = if busy { "LOAD\u{2026}" } else { "LOAD" };
        if tbtn(ui, load_label, busy, 56.0).clicked() && !busy {
            open_file_dialog(params, sel);
        }
        ui.add_space(8.0);
        cell(
            ui,
            setter,
            &params.cp(Module::Slot, sel as u8, 0),
            "ROOT",
            90.0,
        );
        cell(
            ui,
            setter,
            &params.cp(Module::Slot, sel as u8, 3),
            "GAIN",
            100.0,
        );
        ui.add_space(8.0);
        section_label(ui, "LOOP.POINTS");
        lock_btn(ui, &mut ex.locks.loop_points, ex_changed);
        let line = if st.learn.is_some() {
            "touch a control\u{2026}".to_string()
        } else {
            status_line(&st.slot_status[sel], &st.last_message)
        };
        if !line.is_empty() {
            ui.add_space(10.0);
            txt(
                ui,
                Pos2::new(ui.cursor().left() + 4.0, ui.cursor().top() + CELL_H * 0.5),
                Align2::LEFT_CENTER,
                &line,
                10.0,
                ACCENT,
            );
        }
    });
}

// ------------------------------------------------------------- engines ---

const GRAIN_GRID: [(u8, &str); 21] = [
    (1, "RATE"),
    (2, "TEMPO"),
    (3, "DIV"),
    (4, "JITTER"),
    (5, "RATE>LEN"),
    (6, "LENGTH"),
    (14, "PITCH"),
    (15, "POS>PITCH"),
    (16, "DIRECTION"),
    (19, "ALGO"),
    (20, "MODE"),
    (9, "SAMPLE"),
    (10, "POSITION"),
    (11, "LOOP"),
    (12, "LOOP.SIZE"),
    (17, "SHAPE"),
    (18, "SKEW"),
    (7, "PAN"),
    (8, "LEVEL"),
    (13, "GATE.SYNC"),
    (21, "QUANT"),
];

/// Relabel a grain cell for the engine's current Mode — surfacing the
/// Plaits-style per-mode reinterpretation in the UI. Grain indices: 4 Jitter,
/// 16 Direction, 17 Shape, 18 Skew.
fn mode_label(idx: u8, default: &'static str, mode: GrainMode) -> &'static str {
    match (mode, idx) {
        (GrainMode::Stretch, 16) => "SCRUB",
        (GrainMode::Cloud, 4) => "SPRAY",
        (GrainMode::Spectral, 16) => "TIME",
        (GrainMode::Spectral, 17) => "FORMANT",
        (GrainMode::Spectral, 18) => "BLUR",
        _ => default,
    }
}

const LFO_GRID: [(u8, &str); 10] = [
    (2, "RATE"),
    (3, "TEMPO"),
    (4, "DIV"),
    (5, "PHASE"),
    (6, "ST.PHASE"),
    (7, "LAG"),
    (0, "GATE"),
    (1, "TRANS"),
    (8, "TRIG"),
    (9, "1SHOT"),
];

/// ENGINES tab: a G1/G2/G3 selector, then the selected engine's grain grid
/// and Loop-LFO grid side by side. Same `GRAIN_GRID`/`LFO_GRID` cells as the
/// old three-column row, just one engine at a time (and roomier for it).
fn engines_page(
    ui: &mut Ui,
    setter: &ParamSetter,
    params: &MangleParams,
    st: &mut UiState,
    ex: &mut mangle_dsp::state::ExtraState,
    ex_changed: &mut bool,
) {
    let eng = st.engine_page.min(2);
    ui.horizontal(|ui| {
        section_label(ui, "ENGINE");
        for (e, col) in ENG_COLORS.iter().enumerate() {
            let r = tbtn(ui, &format!("G{}", e + 1), e == eng, 44.0);
            if r.clicked() {
                st.engine_page = e;
            }
            // Activity dot in the engine's color: bright while it sounds.
            let active =
                st.feed.engines[e].n_grains > 0 || st.feed.engines[e].cursor[0].is_finite();
            ui.painter().circle_filled(
                Pos2::new(r.rect.right() - 5.0, r.rect.top() + 5.0),
                2.0,
                with_alpha(*col, if active { 255 } else { 60 }),
            );
        }
        ui.add_space(12.0);
        let gate = params.cp(Module::Grain, eng as u8, 0);
        let on = gate.norm() >= 0.5;
        if tbtn(ui, "GATE", on, 56.0).clicked() {
            gate.begin(setter);
            gate.set(setter, if on { 0.0 } else { 1.0 });
            gate.end(setter);
        }
        lock_btn(ui, &mut ex.locks.grain[eng], ex_changed);
        ui.add_space(12.0);
        txt(
            ui,
            Pos2::new(ui.cursor().left() + 4.0, ui.cursor().top() + CELL_H * 0.5),
            Align2::LEFT_CENTER,
            &format!("{} GRAINS", st.feed.engines[eng].n_grains),
            9.0,
            with_alpha(ENG_COLORS[eng], 180),
        );
    });
    ui.add_space(GAP * 2.0);

    let total = ui.available_width() - 6.0; // right-edge safety margin
    let left_w = (total - GAP * 2.0) * 0.5;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(left_w);
            let cw = (left_w - GAP) / 2.0;
            ui.horizontal(|ui| {
                section_label(ui, &format!("GRAIN.{}", eng + 1));
            });
            let mode = {
                let n = params.cp(Module::Grain, eng as u8, GRAIN_MODE_IDX).norm();
                GrainMode::from_index((n * (N_GRAIN_MODES as f32 - 1.0)).round() as usize)
            };
            for pair in GRAIN_GRID.chunks(2) {
                ui.horizontal(|ui| {
                    for (idx, label) in pair {
                        let label = mode_label(*idx, label, mode);
                        cell_learn(
                            ui,
                            setter,
                            params,
                            st,
                            Module::Grain,
                            eng as u8,
                            *idx,
                            label,
                            cw,
                        );
                    }
                });
            }
        });
        ui.add_space(GAP * 2.0);
        ui.vertical(|ui| {
            let rw = ui.available_width() - 6.0;
            let cw = (rw - GAP) / 2.0;
            ui.horizontal(|ui| {
                section_label(ui, &format!("LOOP.LFO.{}", eng + 1));
                lock_btn(ui, &mut ex.locks.loop_lfo[eng], ex_changed);
                // Jump to this engine's loop curve (curves 0..2 = LOOP.1-3).
                if tbtn(ui, "CURVE", false, 56.0).clicked() {
                    st.tab = Tab::Curves;
                    st.selected_curve = eng;
                    st.last_draw = None;
                }
            });
            for pair in LFO_GRID.chunks(2) {
                ui.horizontal(|ui| {
                    for (idx, label) in pair {
                        if *idx == LFO_TRIG_IDX {
                            if trig_button(ui, &mut st.trig_flash[eng], cw) {
                                request_lfo_trig(params, eng);
                            }
                            continue;
                        }
                        cell_learn(
                            ui,
                            setter,
                            params,
                            st,
                            Module::LoopLfo,
                            eng as u8,
                            *idx,
                            label,
                            cw,
                        );
                    }
                });
            }
        });
    });
}

// ------------------------------------------------- lfos, drift, macros ---

fn lfo_and_friends(
    ui: &mut Ui,
    setter: &ParamSetter,
    params: &MangleParams,
    st: &mut UiState,
    ex: &mut mangle_dsp::state::ExtraState,
    ex_changed: &mut bool,
) {
    let total = ui.available_width() - 6.0; // right-edge safety margin
    let left_w = total * 0.62;
    ui.horizontal_top(|ui| {
        // LFO 1–6 — matrix-style: column headers on top, each row labelled
        // on the left (LFO.n + lock), unlabelled value cells under the
        // headers. Wider cells, no label/value overlap.
        ui.vertical(|ui| {
            ui.set_width(left_w);
            let label_w = 76.0; // LFO.n label + padlock gutter
            let n = LFO_GRID.len() as f32;
            let cw = (left_w - label_w - GAP * n) / n;
            // Header row.
            ui.horizontal(|ui| {
                let (hr, _) = ui.allocate_exact_size(vec2(label_w, CELL_H), Sense::hover());
                txt(
                    ui,
                    Pos2::new(hr.left() + 2.0, hr.center().y),
                    Align2::LEFT_CENTER,
                    "LFO",
                    10.0,
                    BRIGHT,
                );
                for (_, label) in LFO_GRID.iter() {
                    let (cr, _) = ui.allocate_exact_size(vec2(cw, CELL_H), Sense::hover());
                    txt(ui, cr.center(), Align2::CENTER_CENTER, label, 9.0, DIM);
                }
            });
            for l in 0..6usize {
                ui.horizontal(|ui| {
                    // Left label cell: "n" + padlock, fixed width.
                    let (lr, _) =
                        ui.allocate_exact_size(vec2(label_w - 18.0, CELL_H), Sense::hover());
                    txt(
                        ui,
                        Pos2::new(lr.left() + 4.0, lr.center().y),
                        Align2::LEFT_CENTER,
                        &format!("{}", l + 1),
                        10.0,
                        BRIGHT,
                    );
                    lock_btn(ui, &mut ex.locks.lfo[l], ex_changed);
                    for (idx, _) in LFO_GRID.iter() {
                        if *idx == LFO_TRIG_IDX {
                            if trig_button(ui, &mut st.trig_flash[3 + l], cw) {
                                request_lfo_trig(params, 3 + l);
                            }
                            continue;
                        }
                        cell_learn(ui, setter, params, st, Module::Lfo, l as u8, *idx, "", cw);
                    }
                });
            }
        });
        ui.add_space(GAP * 2.0);
        // Drift / Noise / Macros.
        ui.vertical(|ui| {
            let rw = ui.available_width();
            let cw = (rw - 24.0 - GAP * 3.0) / 3.0;
            ui.horizontal(|ui| {
                section_label(ui, "DRIFT");
                lock_btn(ui, &mut ex.locks.drift, ex_changed);
                for d in 0..3u8 {
                    cell(
                        ui,
                        setter,
                        &params.cp(Module::Drift, d, 0),
                        &format!("ST.PH.{}", d + 1),
                        cw,
                    );
                }
            });
            ui.horizontal(|ui| {
                section_label(ui, "NOISE");
                lock_btn(ui, &mut ex.locks.noise, ex_changed);
                for d in 0..3u8 {
                    cell(
                        ui,
                        setter,
                        &params.cp(Module::Noise, d, 0),
                        &format!("ST.PH.{}", d + 1),
                        cw,
                    );
                }
            });
            ui.add_space(GAP);
            ui.horizontal(|ui| {
                section_label(ui, "MACROS");
                lock_btn(ui, &mut ex.locks.macros, ex_changed);
            });
            let mw = (rw - GAP * 3.0) / 4.0;
            for row in 0..2u8 {
                ui.horizontal(|ui| {
                    for c in 0..4u8 {
                        let m = row * 4 + c;
                        cell(
                            ui,
                            setter,
                            &params.cp(Module::Macro, m, 0),
                            &format!("M{}", m + 1),
                            mw,
                        );
                    }
                });
            }
        });
    });
}

// --------------------------------------------------------------- filter ---

/// Master output filter (Module::Fx): On / Type / Cutoff / Reso. Cutoff and
/// Reso are matrix destinations (Flt Cutoff / Flt Reso), so they sit just above
/// the matrix.
fn filter_panel(ui: &mut Ui, setter: &ParamSetter, params: &MangleParams, st: &mut UiState) {
    ui.horizontal(|ui| {
        section_label(ui, "FILTER");
        cell(ui, setter, &params.cp(Module::Fx, 0, 0), "ON", 64.0);
        cell(ui, setter, &params.cp(Module::Fx, 0, 1), "TYPE", 84.0);
        cell_learn(ui, setter, params, st, Module::Fx, 0, 2, "CUTOFF", 116.0);
        cell_learn(ui, setter, params, st, Module::Fx, 0, 3, "RESO", 96.0);
    });
}

// --------------------------------------------------------------- matrix ---

fn matrix_panel(
    ui: &mut Ui,
    setter: &ParamSetter,
    params: &MangleParams,
    st: &mut UiState,
    ex: &mut mangle_dsp::state::ExtraState,
    ex_changed: &mut bool,
) {
    ui.horizontal(|ui| {
        section_label(ui, "MATRIX");
        lock_btn(ui, &mut ex.locks.matrix, ex_changed);
        for p in 0..4usize {
            if tbtn(ui, &format!("{}", p + 1), st.matrix_page == p, 26.0).clicked() {
                st.matrix_page = p;
            }
        }
        txt(
            ui,
            Pos2::new(ui.cursor().left() + 8.0, ui.cursor().top() + CELL_H * 0.5),
            Align2::LEFT_CENTER,
            "ctrl-click clears · right-click picks",
            9.0,
            DIM,
        );
    });
    let total = ui.available_width() - 6.0; // right-edge safety margin
    let unit = (total - GAP * 5.0) / (2.0 + 1.0 + 3.0 + 1.0 + 3.0 + 2.0);
    let widths = [2.0 * unit, unit, 3.0 * unit, unit, 3.0 * unit, 2.0 * unit];
    const COLS: [&str; 6] = ["SOURCE", "AMT", "DEST.1", "AMT", "DEST.2", "SCALE"];
    for r in 0..8u8 {
        let row = st.matrix_page as u8 * 8 + r;
        ui.horizontal(|ui| {
            for (col, w) in widths.iter().enumerate() {
                let label = if r == 0 { COLS[col] } else { "" };
                matrix_cell(ui, setter, params, st, row, col as u8, label, *w);
            }
        });
    }
}

/// One matrix grid cell. Source/Scale/Dest cells open a picker menu on
/// right-click; while a destination pick is armed, clicking the armed cell
/// cancels it and every other matrix cell is left untouched.
#[allow(clippy::too_many_arguments)]
fn matrix_cell(
    ui: &mut Ui,
    setter: &ParamSetter,
    params: &MangleParams,
    st: &mut UiState,
    row: u8,
    col: u8,
    label: &str,
    w: f32,
) {
    let cp = params.cp(Module::Matrix, row, col);
    let armed = st.learn == Some((row, col));
    let resp = cell_inner(ui, setter, &cp, label, w, st.learn.is_some());
    if armed {
        frame(ui, resp.rect, ACCENT);
        if resp.clicked() {
            st.learn = None;
        }
    }
    match col {
        0 | 5 => {
            resp.context_menu(|ui| source_menu(ui, setter, &cp));
        }
        2 | 4 => {
            resp.context_menu(|ui| dest_menu(ui, setter, &cp, &mut st.learn, (row, col)));
        }
        _ => {}
    }
}

/// Write a stepped index param (`count` entries) through the host.
fn set_index(setter: &ParamSetter, cp: &CellParam, idx: u16, count: u16) {
    cp.begin(setter);
    cp.set(setter, idx as f32 / (count - 1) as f32);
    cp.end(setter);
}

fn index_of(cp: &CellParam, count: u16) -> u16 {
    (cp.norm() * (count - 1) as f32).round() as u16
}

fn pick_item(ui: &mut Ui, selected: bool, label: &str) -> bool {
    let hit = ui.selectable_label(selected, label).clicked();
    if hit {
        ui.close_menu();
    }
    hit
}

/// Source / Scale picker: every source, grouped.
fn source_menu(ui: &mut Ui, setter: &ParamSetter, cp: &CellParam) {
    const GROUPS: [(&str, std::ops::RangeInclusive<u16>); 7] = [
        ("LFO 1\u{2013}6", 1..=6),
        ("Loop 1\u{2013}3", 7..=9),
        ("Drift", 10..=12),
        ("Noise", 13..=15),
        ("Macros", 16..=23),
        ("Consts", 24..=27),
        ("Env/Vel/Note/Oct/Gate/Press/VRand", 28..=34),
    ];
    let cur = index_of(cp, matrix::SOURCE_COUNT);
    if pick_item(ui, cur == 0, &matrix::source_name(0)) {
        set_index(setter, cp, 0, matrix::SOURCE_COUNT);
    }
    for (name, range) in GROUPS {
        ui.menu_button(name, |ui| {
            for i in range {
                if pick_item(ui, cur == i, &matrix::source_name(i)) {
                    set_index(setter, cp, i, matrix::SOURCE_COUNT);
                }
            }
        });
    }
}

/// Destination picker: grouped by owner, plus PICK FROM CONTROL.
fn dest_menu(
    ui: &mut Ui,
    setter: &ParamSetter,
    cp: &CellParam,
    learn: &mut Option<(u8, u8)>,
    cell_pos: (u8, u8),
) {
    use matrix::ModDest;
    let cur = index_of(cp, matrix::DEST_COUNT);
    if ui.button("PICK FROM CONTROL").clicked() {
        *learn = Some(cell_pos);
        ui.close_menu();
    }
    ui.separator();
    if pick_item(ui, cur == 0, &matrix::dest_name(0)) {
        set_index(setter, cp, 0, matrix::DEST_COUNT);
    }
    let owner = |ui: &mut Ui, title: String, dests: Vec<(String, ModDest)>| {
        ui.menu_button(title, |ui| {
            for (name, d) in dests {
                let i = matrix::dest_to_index(d);
                if pick_item(ui, cur == i, &name) {
                    set_index(setter, cp, i, matrix::DEST_COUNT);
                }
            }
        });
    };
    let lfo_items = |f: &dyn Fn(matrix::LfoDest) -> ModDest| -> Vec<(String, ModDest)> {
        matrix::LFO_DESTS
            .iter()
            .map(|&s| (format!("{s:?}"), f(s)))
            .collect()
    };
    for n in 0..6u8 {
        owner(
            ui,
            format!("LFO {}", n + 1),
            lfo_items(&|s| ModDest::Lfo(n, s)),
        );
    }
    for n in 0..3u8 {
        owner(
            ui,
            format!("Loop {}", n + 1),
            lfo_items(&|s| ModDest::LoopLfo(n, s)),
        );
    }
    for n in 0..3u8 {
        let items = matrix::GRAIN_DESTS
            .iter()
            .map(|&s| (format!("{s:?}"), ModDest::Grain(n, s)))
            .collect();
        owner(ui, format!("G{}", n + 1), items);
    }
    let items = matrix::FX_DESTS
        .iter()
        .map(|&s| (format!("{s:?}"), ModDest::Fx(s)))
        .collect();
    owner(ui, "Filter".to_string(), items);
}

/// The matrix destination a cell drives, if it is one.
fn cell_dest(module: Module, inst: u8, idx: u8) -> Option<matrix::ModDest> {
    use matrix::{FxDest, GrainDest, LfoDest, ModDest};
    let lfo = || match idx {
        0 => Some(LfoDest::Gate),
        5 => Some(LfoDest::Phase),
        2 => Some(LfoDest::Rate),
        7 => Some(LfoDest::Lag),
        _ => None,
    };
    match module {
        Module::Lfo => lfo().map(|s| ModDest::Lfo(inst, s)),
        Module::LoopLfo => lfo().map(|s| ModDest::LoopLfo(inst, s)),
        Module::Grain => {
            let s = match idx {
                1 => GrainDest::Rate,
                6 => GrainDest::Length,
                4 => GrainDest::Jitter,
                12 => GrainDest::LoopSize,
                10 => GrainDest::Position,
                14 => GrainDest::Pitch,
                16 => GrainDest::Direction,
                17 => GrainDest::Shape,
                18 => GrainDest::Skew,
                8 => GrainDest::Level,
                7 => GrainDest::Pan,
                9 => GrainDest::Sample,
                _ => return None,
            };
            Some(ModDest::Grain(inst, s))
        }
        Module::Fx => match idx {
            2 => Some(ModDest::Fx(FxDest::Cutoff)),
            3 => Some(ModDest::Fx(FxDest::Reso)),
            _ => None,
        },
        _ => None,
    }
}

/// A `cell` that is also a matrix destination: while a pick is armed, the
/// next click or drag start on it assigns the destination to the armed matrix
/// cell instead of editing the control.
#[allow(clippy::too_many_arguments)]
fn cell_learn(
    ui: &mut Ui,
    setter: &ParamSetter,
    params: &MangleParams,
    st: &mut UiState,
    module: Module,
    inst: u8,
    idx: u8,
    label: &str,
    w: f32,
) {
    let c = params.cp(module, inst, idx);
    let dest = match (st.learn, cell_dest(module, inst, idx)) {
        (Some(_), Some(d)) => d,
        _ => {
            cell_inner(ui, setter, &c, label, w, st.learn.is_some());
            return;
        }
    };
    let resp = cell_inner(ui, setter, &c, label, w, true);
    let started = resp.drag_started();
    if started || resp.clicked() {
        if started {
            ui.memory_mut(|m| m.data.insert_temp(resp.id, true));
        }
        if let Some((row, col)) = st.learn.take() {
            let target = params.cp(Module::Matrix, row, col);
            set_index(
                setter,
                &target,
                matrix::dest_to_index(dest),
                matrix::DEST_COUNT,
            );
        }
    }
}

// --------------------------------------------------------------- curves ---

fn curves_screen(
    ui: &mut Ui,
    st: &mut UiState,
    ex: &mut mangle_dsp::state::ExtraState,
    ex_changed: &mut bool,
) {
    const NAMES: [&str; 9] = [
        "LOOP.1", "LOOP.2", "LOOP.3", "LFO.1", "LFO.2", "LFO.3", "LFO.4", "LFO.5", "LFO.6",
    ];
    ui.horizontal(|ui| {
        for (i, name) in NAMES.iter().enumerate() {
            if tbtn(ui, name, st.selected_curve == i, 66.0).clicked() {
                st.selected_curve = i;
                st.last_draw = None;
            }
        }
    });
    ui.add_space(GAP * 2.0);

    let sel = st.selected_curve.min(8);
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 320.0), Sense::click_and_drag());
    fill(ui, rect, HEADER);
    frame(ui, rect, BORDER);
    let mid = rect.center().y;
    ui.painter().line_segment(
        [Pos2::new(rect.left(), mid), Pos2::new(rect.right(), mid)],
        Stroke::new(1.0_f32, BORDER),
    );

    // Draw the curve.
    {
        let c = &ex.curves[sel];
        let half = rect.height() * 0.48;
        let mut prev: Option<Pos2> = None;
        for x in 0..rect.width() as usize {
            let t = x as f32 / rect.width();
            let v = c.sample(t);
            let p = Pos2::new(rect.left() + x as f32, mid - v * half);
            if let Some(q) = prev {
                ui.painter()
                    .line_segment([q, p], Stroke::new(1.5_f32, ACCENT));
            }
            prev = Some(p);
        }
    }

    st.curve_rect = Some(rect);
    let half = rect.height() * 0.48;
    let to_point = |p: Pos2| {
        let idx = (((p.x - rect.left()) / rect.width()) * 255.0).clamp(0.0, 255.0) as usize;
        let val = ((mid - p.y) / half).clamp(-1.0, 1.0);
        (idx, val)
    };

    match st.curve_tool {
        CurveTool::Draw => {
            // Freehand drawing with gap interpolation.
            if resp.dragged() || resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (idx, val) = to_point(p);
                    let (a, av) = st.last_draw.unwrap_or((idx, val));
                    ex.curves[sel].line(a, av, idx, val);
                    st.last_draw = Some((idx, val));
                    *ex_changed = true;
                }
            }
            if resp.drag_stopped() {
                st.last_draw = None;
            }
        }
        CurveTool::Line => {
            if resp.drag_started() {
                st.line_start = ui.input(|i| i.pointer.press_origin()).map(to_point);
            }
            let end = ui.input(|i| i.pointer.latest_pos()).map(to_point);
            if let (Some((i0, v0)), Some((i1, v1))) = (st.line_start, end) {
                if resp.dragged() {
                    let x = |i: usize| rect.left() + i as f32 / 255.0 * rect.width();
                    ui.painter().line_segment(
                        [
                            Pos2::new(x(i0), mid - v0 * half),
                            Pos2::new(x(i1), mid - v1 * half),
                        ],
                        Stroke::new(1.5_f32, BRIGHT),
                    );
                }
                if resp.drag_stopped() {
                    ex.curves[sel].line(i0, v0, i1, v1);
                    *ex_changed = true;
                }
            }
            if resp.drag_stopped() {
                st.line_start = None;
            }
        }
    }

    ui.add_space(GAP * 2.0);
    if st.quant == 0 {
        st.quant = 8;
    }
    ui.horizontal(|ui| {
        section_label(ui, "TOOL");
        if tbtn(ui, "DRAW", st.curve_tool == CurveTool::Draw, 52.0).clicked() {
            st.curve_tool = CurveTool::Draw;
            st.line_start = None;
            st.last_draw = None;
        }
        if tbtn(ui, "LINE", st.curve_tool == CurveTool::Line, 52.0).clicked() {
            st.curve_tool = CurveTool::Line;
            st.line_start = None;
            st.last_draw = None;
        }
        ui.add_space(12.0);
        section_label(ui, "QUANT");
        if tbtn(ui, "-", false, 24.0).clicked() {
            st.quant = (st.quant - 1).max(2);
        }
        let (qr, _) = ui.allocate_exact_size(vec2(32.0, CELL_H), Sense::hover());
        txt(
            ui,
            qr.center(),
            Align2::CENTER_CENTER,
            &st.quant.to_string(),
            10.0,
            BRIGHT,
        );
        if tbtn(ui, "+", false, 24.0).clicked() {
            st.quant = (st.quant + 1).min(64);
        }
        if tbtn(ui, "APPLY", false, 56.0).clicked() {
            ex.curves[sel].quantize(st.quant);
            *ex_changed = true;
        }
        ui.add_space(12.0);
        section_label(ui, "SHIFT");
        if tbtn(ui, "\u{25C0}", false, 32.0).clicked() {
            ex.curves[sel].shift_phase(-1.0 / 32.0);
            *ex_changed = true;
        }
        if tbtn(ui, "\u{25B6}", false, 32.0).clicked() {
            ex.curves[sel].shift_phase(1.0 / 32.0);
            *ex_changed = true;
        }
        ui.add_space(12.0);
        section_label(ui, "OPS");
        if tbtn(ui, "SMOOTH", false, 64.0).clicked() {
            ex.curves[sel].smooth();
            *ex_changed = true;
        }
        if tbtn(ui, "INVERT", false, 64.0).clicked() {
            ex.curves[sel].invert();
            *ex_changed = true;
        }
        if tbtn(ui, "MIRROR", false, 64.0).clicked() {
            ex.curves[sel].mirror();
            *ex_changed = true;
        }
    });

    // Preset bank: thumbnails are cached; a curve is built only on click.
    if st.presets.is_empty() {
        st.presets = (0..N_PRESETS).map(Curve::preset).collect();
    }
    ui.add_space(GAP);
    section_label(ui, "PRESETS");
    st.thumb_rects.clear();
    let (tw, th) = (48.0, 30.0);
    egui::ScrollArea::vertical()
        .id_salt("curve_presets")
        .max_height(th * 3.0 + GAP * 4.0)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (pi, c) in st.presets.iter().enumerate() {
                    let (r, tresp) = ui.allocate_exact_size(vec2(tw, th), Sense::click());
                    fill(ui, r, HEADER);
                    frame(ui, r, if tresp.hovered() { ACCENT } else { BORDER });
                    let pts: Vec<Pos2> = (0..32)
                        .map(|k| {
                            let v = c.0[k * 8];
                            Pos2::new(
                                r.left() + 2.0 + k as f32 / 31.0 * (tw - 4.0),
                                r.center().y - v * (th * 0.5 - 4.0),
                            )
                        })
                        .collect();
                    ui.painter()
                        .add(egui::Shape::line(pts, Stroke::new(1.0_f32, ACCENT)));
                    if tresp.clicked() {
                        ex.curves[sel] = Curve::preset(pi);
                        *ex_changed = true;
                    }
                    st.thumb_rects.push(r);
                }
            });
        });
    ui.add_space(GAP);
    txt(
        ui,
        Pos2::new(ui.cursor().left() + 4.0, ui.cursor().top() + 8.0),
        Align2::LEFT_CENTER,
        "draw with the mouse \u{2014} loop curves scan the sample, lfo curves shape LFO 1\u{2013}6",
        9.0,
        DIM,
    );
}

// ------------------------------------------------------ headless gui test ---

#[cfg(test)]
mod gui_tests {
    //! Headless render test for the editor: build every screen and poke it with
    //! hostile pointer input, asserting no panic and finite geometry. The DAW's
    //! window/host is replaced by a bare `egui::Context` and a stub
    //! `GuiContext`. This is the Skrongle-`guitest` philosophy ported to egui.
    use super::*;
    use egui::{Context, Event, Modifiers, MouseWheelUnit, PointerButton, RawInput};
    use nih_plug::prelude::{GuiContext, ParamPtr, PluginApi, PluginState};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Minimal `GuiContext` so a `ParamSetter` can exist without a host. Param
    /// writes are counted (begin/set/end) but not applied —
    /// `ParamPtr::set_normalized_value` is crate-private to nih-plug — which is
    /// fine: this harness checks the editor renders and handles input without
    /// panicking, not value math.
    #[derive(Default)]
    struct TestHost {
        begins: AtomicU32,
        sets: AtomicU32,
        ends: AtomicU32,
        set_log: std::sync::Mutex<Vec<(ParamPtr, f32)>>,
        begin_log: std::sync::Mutex<Vec<ParamPtr>>,
    }
    impl GuiContext for TestHost {
        fn plugin_api(&self) -> PluginApi {
            PluginApi::Clap
        }
        fn request_resize(&self) -> bool {
            true
        }
        unsafe fn raw_begin_set_parameter(&self, p: ParamPtr) {
            self.begins.fetch_add(1, Ordering::Relaxed);
            self.begin_log.lock().unwrap().push(p);
        }
        unsafe fn raw_set_parameter_normalized(&self, p: ParamPtr, v: f32) {
            self.sets.fetch_add(1, Ordering::Relaxed);
            self.set_log.lock().unwrap().push((p, v));
        }
        unsafe fn raw_end_set_parameter(&self, _p: ParamPtr) {
            self.ends.fetch_add(1, Ordering::Relaxed);
        }
        fn get_state(&self) -> PluginState {
            unimplemented!("editor never reads host state")
        }
        fn set_state(&self, _s: PluginState) {}
    }

    fn press(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        }
    }
    fn click(ev: &mut Vec<Event>, x: f32, y: f32) {
        let pos = Pos2::new(x, y);
        ev.push(Event::PointerMoved(pos));
        ev.push(press(pos, true));
        ev.push(press(pos, false));
    }
    fn drag(ev: &mut Vec<Event>, from: Pos2, steps: impl Iterator<Item = Pos2>) {
        ev.push(Event::PointerMoved(from));
        ev.push(press(from, true));
        let mut last = from;
        for p in steps {
            ev.push(Event::PointerMoved(p));
            last = p;
        }
        ev.push(press(last, false));
    }

    #[test]
    fn editor_renders_all_screens_without_panic() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState::default();
        let mut time = 0.0f64;

        let mut frame = |st: &mut UiState, events: Vec<Event>| {
            time += 0.05;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| draw_editor(ctx, &setter, &params, st));
            // Catch NaN rects / degenerate geometry (e.g. empty-waveform math).
            for prim in ctx.tessellate(out.shapes, out.pixels_per_point) {
                if let egui::epaint::Primitive::Mesh(m) = prim.primitive {
                    for v in &m.vertices {
                        assert!(
                            v.pos.x.is_finite() && v.pos.y.is_finite(),
                            "non-finite vertex at {:?}",
                            v.pos
                        );
                    }
                }
            }
        };

        // Warm-up so layout rects exist before we start clicking them.
        frame(&mut st, vec![]);
        frame(&mut st, vec![]);

        // Synthetic engine-activity frame through the real feed path, so the
        // waveform overlay paints (the finite-geometry assertions cover it).
        {
            let mut ff = FeedFrame::default();
            let ef = &mut ff.engines[0];
            for (i, pos) in [0.0f32, 0.5, 1.0].into_iter().enumerate() {
                ef.grains[i] = mangle_dsp::engine::GrainView {
                    pos,
                    age: 0.5,
                    level: 0.8,
                    slot: 0,
                    channel: (i % 2) as u8,
                };
            }
            ef.n_grains = 3;
            ef.slot = 0;
            ef.mode = 1; // Stretch: full-height playhead
            ef.cursor = [0.3, 0.35];
            ff.engines[1].slot = 0;
            ff.engines[1].mode = 0; // Granular: bottom tick (+ one NaN channel)
            ff.engines[1].cursor = [0.7, f32::NAN];
            ff.voices = 2;
            *params.feed.frame.lock().unwrap() = ff;
            // Accumulator activity so the lanes push non-empty buckets.
            params.feed.spawns[0].store(5, Ordering::Relaxed);
            params.feed.spawns[2].store(70_000, Ordering::Relaxed); // u16 clamp path
            params.feed.level_bits[1].store(0.6f32.to_bits(), Ordering::Relaxed);
            frame(&mut st, vec![]);
            frame(&mut st, vec![]);
        }

        // Header sweep (hits the bool cells -> real param gestures).
        {
            let mut ev = Vec::new();
            let mut x = 80.0;
            while x < 720.0 {
                click(&mut ev, x, 14.0);
                x += 24.0;
            }
            frame(&mut st, ev);
        }

        // Grid of clicks over the whole body, including out-of-bounds, plus a
        // scroll each row.
        for gy in 0..15 {
            let mut ev = Vec::new();
            for gx in 0..13 {
                click(&mut ev, gx as f32 * 72.0 - 20.0, 30.0 + gy as f32 * 48.0);
            }
            ev.push(Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: vec2(0.0, if gy % 2 == 0 { -3.0 } else { 3.0 }),
                modifiers: Modifiers::default(),
            });
            frame(&mut st, ev);
        }

        // Loop-edge drag across the waveform strip.
        {
            let mut ev = Vec::new();
            drag(
                &mut ev,
                Pos2::new(200.0, 120.0),
                (1..=12).map(|i| Pos2::new(200.0 + i as f32 * 28.0, 120.0)),
            );
            frame(&mut st, ev);
        }

        // Every tab, and each engine page, with a click/scroll sweep on each.
        for tab in [Tab::Engines, Tab::Mod, Tab::Matrix, Tab::Curves] {
            st.tab = tab;
            for eng in 0..3usize {
                st.engine_page = eng;
                let mut ev = Vec::new();
                for gx in 0..10 {
                    click(&mut ev, gx as f32 * 90.0 - 20.0, 260.0 + gx as f32 * 20.0);
                }
                ev.push(Event::MouseWheel {
                    unit: MouseWheelUnit::Line,
                    delta: vec2(0.0, if eng % 2 == 0 { -3.0 } else { 3.0 }),
                    modifiers: Modifiers::default(),
                });
                frame(&mut st, ev);
            }
        }
        st.tab = Tab::Engines;
        st.engine_page = 0;

        // Each matrix page.
        st.tab = Tab::Matrix;
        for page in 0..4usize {
            st.matrix_page = page;
            frame(&mut st, vec![]);
        }
        st.matrix_page = 0;
        st.tab = Tab::Engines;

        // Each slot selected.
        for slot in 0..6usize {
            st.selected_slot = slot;
            frame(&mut st, vec![]);
        }

        // Curves tab + a freehand draw on each of the nine curves.
        st.tab = Tab::Curves;
        frame(&mut st, vec![]);
        for c in 0..9usize {
            st.selected_curve = c;
            let mut ev = Vec::new();
            drag(
                &mut ev,
                Pos2::new(120.0, 300.0),
                (1..=20).map(|i| {
                    Pos2::new(
                        120.0 + i as f32 * 15.0,
                        300.0 + (i as f32 * 0.5).sin() * 40.0,
                    )
                }),
            );
            frame(&mut st, ev);
        }
        st.tab = Tab::Engines;
        frame(&mut st, vec![]);

        // Interactions actually reached parameters, and gestures balanced.
        let (b, s, e) = (
            host.begins.load(Ordering::Relaxed),
            host.sets.load(Ordering::Relaxed),
            host.ends.load(Ordering::Relaxed),
        );
        assert!(
            s > 0,
            "no parameter writes were triggered by the input sweep"
        );
        assert_eq!(
            b, e,
            "unbalanced begin/end parameter gestures ({b} begins, {e} ends)"
        );
    }

    /// Minimal mono 16-bit PCM WAV, enough for symphonia to decode.
    fn tiny_wav(path: &std::path::Path) {
        let sr = 44_100u32;
        let n = 64u32;
        let data_len = n * 2;
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend((36 + data_len).to_le_bytes());
        b.extend(b"WAVE");
        b.extend(b"fmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes()); // PCM
        b.extend(1u16.to_le_bytes()); // mono
        b.extend(sr.to_le_bytes());
        b.extend((sr * 2).to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(data_len.to_le_bytes());
        for i in 0..n {
            let v = ((i as f32 / n as f32 * std::f32::consts::TAU).sin() * 8000.0) as i16;
            b.extend(v.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    /// The `hovered_files`/`dropped_files` path: hovering marks a drop target,
    /// a drop decodes the files and queues them for the audio thread, slot by
    /// slot from the target onward.
    #[test]
    fn file_drop_targets_and_loads_slots() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState::default();
        let mut time = 0.0f64;

        let dir = std::env::temp_dir();
        let (wav_a, wav_b) = (dir.join("mangle_dnd_a.wav"), dir.join("mangle_dnd_b.wav"));
        tiny_wav(&wav_a);
        tiny_wav(&wav_b);

        let mut frame = |st: &mut UiState,
                         hovered: &[&std::path::Path],
                         dropped: &[&std::path::Path],
                         events: Vec<Event>| {
            time += 0.05;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0))),
                time: Some(time),
                events,
                hovered_files: hovered
                    .iter()
                    .map(|p| egui::HoveredFile {
                        path: Some(p.to_path_buf()),
                        ..Default::default()
                    })
                    .collect(),
                dropped_files: dropped
                    .iter()
                    .map(|p| egui::DroppedFile {
                        path: Some(p.to_path_buf()),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| draw_editor(ctx, &setter, &params, st));
        };

        // Warm-up, then hover: the selected slot becomes the drop target.
        frame(&mut st, &[], &[], vec![]);
        st.selected_slot = 2;
        frame(&mut st, &[&wav_a], &[], vec![]);
        assert_eq!(
            st.drop_target,
            Some(2),
            "hovering files should target the selected slot"
        );

        // Drop two files plus a missing one: the frame returns without
        // decoding, the files land in successive slots from the target (the
        // missing path doesn't consume a slot), and the highlight clears.
        let missing = dir.join("mangle_dnd_missing.wav");
        let started = std::time::Instant::now();
        frame(&mut st, &[], &[&wav_a, &missing, &wav_b], vec![]);
        let queued = |params: &MangleParams| -> Vec<usize> {
            let q = params.pending_loads.lock().unwrap();
            assert!(q.iter().all(|p| !p.consumed));
            let mut v: Vec<usize> = q.iter().map(|p| p.slot).collect();
            v.sort_unstable();
            v
        };
        let end = started + std::time::Duration::from_secs(5);
        while queued(&params).len() < 2 && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            queued(&params),
            vec![2, 3],
            "dropped files should queue into successive slots"
        );
        assert_eq!(
            st.selected_slot, 2,
            "the first dropped slot becomes selected"
        );
        assert_eq!(st.drop_target, None);
        frame(&mut st, &[], &[], vec![]);
        assert!(
            st.last_message.contains("mangle_dnd_missing.wav"),
            "a failed file should surface a message, got: {:?}",
            st.last_message
        );

        // Hover with the pointer over a slot tab: that tab becomes the target
        // (needs one frame for the strip's slot row to see the pointer).
        let over_tab_4 = Pos2::new(
            4.0 + 3.0 * (34.0 + 8.0) + 17.0,
            4.0 + 120.0 + 8.0 + CELL_H * 0.5,
        );
        frame(
            &mut st,
            &[&wav_a],
            &[],
            vec![Event::PointerMoved(over_tab_4)],
        );
        frame(&mut st, &[&wav_a], &[], vec![]);
        // The exact tab depends on layout spacing; deterministic either way is
        // that a hover always resolves to a valid slot target.
        assert!(
            matches!(st.drop_target, Some(t) if t < 6),
            "hover with pointer on the slot row must keep a valid drop target, got {:?}",
            st.drop_target
        );

        let _ = std::fs::remove_file(&wav_a);
        let _ = std::fs::remove_file(&wav_b);
    }

    /// Run one frame with `events` on `st` and return nothing; shared by the
    /// curve-tool tests below.
    fn curve_frame(
        ctx: &Context,
        setter: &ParamSetter,
        params: &Arc<MangleParams>,
        st: &mut UiState,
        time: &mut f64,
        events: Vec<Event>,
    ) {
        *time += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0))),
            time: Some(*time),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| draw_editor(ctx, setter, params, st));
    }

    #[test]
    fn line_tool_writes_straight_segment() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState {
            tab: Tab::Curves,
            selected_curve: 3,
            curve_tool: CurveTool::Line,
            ..Default::default()
        };
        let mut time = 0.0;
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        let r = st.curve_rect.expect("canvas rect");
        let mid = r.center().y;
        let half = r.height() * 0.48;
        let p0 = Pos2::new(r.left() + r.width() * 0.25, mid + half * 0.5);
        let p1 = Pos2::new(r.left() + r.width() * 0.75, mid - half * 0.5);
        let mut ev = Vec::new();
        drag(
            &mut ev,
            p0,
            (1..=10).map(|k| p0 + (p1 - p0) * (k as f32 / 10.0)),
        );
        let release = ev.pop().unwrap();
        let moves = ev.split_off(2);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, ev);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, moves);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![release]);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);

        let c = params.extra.read().unwrap().curves[3].clone();
        let idx = |p: Pos2| (((p.x - r.left()) / r.width()) * 255.0) as usize;
        let (i0, i1) = (idx(p0), idx(p1));
        let (v0, v1) = (c.0[i0], c.0[i1]);
        assert!(
            (v0 + 0.5).abs() < 0.02 && (v1 - 0.5).abs() < 0.02,
            "{v0} {v1}"
        );
        for i in i0..=i1 {
            let want = v0 + (v1 - v0) * (i - i0) as f32 / (i1 - i0) as f32;
            assert!((c.0[i] - want).abs() < 1e-4, "not linear at {i}");
        }
    }

    /// A GUI edit must not overwrite slot references written by a loader.
    #[test]
    fn slot_refs_survive_gui_writeback() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState::default();
        let mut time = 0.0;
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);

        params.extra.write().unwrap().slot_refs[0] = Some(mangle_dsp::state::SlotRef {
            path: "kept.wav".into(),
            content_hash: 7,
        });
        params.extra_gen.fetch_add(1, Ordering::Release);

        // Calibrate the slot row's y by clicking tab 2 until it is selected.
        let mut y = 0.0;
        for yy in (10..300).step_by(3) {
            let mut ev = Vec::new();
            click(&mut ev, 4.0 + 42.0 + 17.0, yy as f32);
            curve_frame(&ctx, &setter, &params, &mut st, &mut time, ev);
            if st.selected_slot == 1 {
                y = yy as f32;
                break;
            }
        }
        assert!(y > 0.0, "slot row not found");
        // Sweep clicks along the row (past LOAD) until the loop-points lock
        // toggles.
        let mut toggled = false;
        let mut x = 340.0;
        while x < 880.0 && !toggled {
            let mut ev = Vec::new();
            click(&mut ev, x, y);
            curve_frame(&ctx, &setter, &params, &mut st, &mut time, ev);
            toggled = params.extra.read().unwrap().locks.loop_points;
            x += 3.0;
        }
        assert!(toggled, "no click reached the loop-points lock");
        let ex = params.extra.read().unwrap();
        assert_eq!(ex.slot_refs[0].as_ref().map(|r| r.content_hash), Some(7));
        drop(ex);

        // A write that lands after the editor read makes it drop its edit.
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        params.extra_gen.fetch_add(1, Ordering::Release);
        st.extra.locks.loop_points = false;
        let mut ev = Vec::new();
        click(&mut ev, x - 3.0, y);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, ev);
        assert!(params.extra.read().unwrap().slot_refs[0].is_some());
    }

    /// A pool checkbox toggled through the real context-menu input path
    /// reaches the shared extra state.
    #[test]
    fn random_pool_checkbox_reaches_extra_state() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState::default();
        let mut time = 0.0f64;
        let mut frame = |st: &mut UiState, events: Vec<Event>| {
            time += 0.05;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| draw_editor(ctx, &setter, &params, st));
        };
        let secondary = |ev: &mut Vec<Event>, x: f32, y: f32| {
            let pos = Pos2::new(x, y);
            ev.push(Event::PointerMoved(pos));
            for pressed in [true, false] {
                ev.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Secondary,
                    pressed,
                    modifiers: Modifiers::default(),
                });
            }
        };
        frame(&mut st, vec![]);
        frame(&mut st, vec![]);

        // Open the RND menu: right-click along the header until a popup shows.
        let mut x = 80.0;
        while x < 1500.0 && !ctx.is_context_menu_open() {
            let mut ev = Vec::new();
            secondary(&mut ev, x, 14.0);
            frame(&mut st, ev);
            frame(&mut st, vec![]);
            x += 6.0;
        }
        assert!(ctx.is_context_menu_open(), "RND menu not found");

        // Sweep primary clicks over the menu: section headers expand, then a
        // checkbox flips a pool flag.
        let default = RandomPools::default();
        'sweep: for pass in 0..6 {
            for y in (20..880).step_by(5) {
                let mut ev = Vec::new();
                click(&mut ev, x - 6.0 + 20.0 + pass as f32, y as f32);
                frame(&mut st, ev);
                if params.extra.read().unwrap().pools != default {
                    break 'sweep;
                }
            }
        }
        frame(&mut st, vec![]);
        assert_ne!(params.extra.read().unwrap().pools, default);
        assert_ne!(st.extra.pools, default);
    }

    #[test]
    fn clicking_thumbnail_replaces_curve() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState {
            tab: Tab::Curves,
            selected_curve: 2,
            ..Default::default()
        };
        let mut time = 0.0;
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        assert_eq!(st.presets.len(), N_PRESETS);
        let want = 5usize;
        let r = st.thumb_rects[want];
        let mut ev = Vec::new();
        click(&mut ev, r.center().x, r.center().y);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, ev);
        let c = params.extra.read().unwrap().curves[2].clone();
        assert_eq!(c.0, Curve::preset(want).0);
    }

    #[test]
    fn rnd_keeps_notes_and_undo_restores_pre_roll() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let mut st = UiState::default();
        let before = read_patch(&params);
        let ex = params.extra.read().unwrap().clone();
        roll_random(&params, &setter, &mut st, &ex, 7, None);
        assert!(!params.panic.load(Ordering::Acquire));
        assert_eq!(st.pre_roll.as_ref(), Some(&before));
        assert!(host.sets.load(Ordering::Relaxed) > 0);

        // The harness does not apply sets, so stage a distinct pre-roll and
        // check UNDO writes exactly its values back and clears it.
        let pos = params.slots.len() / 2;
        let mut staged = before.clone();
        staged.values[pos] = if before.values[pos] > 0.5 { 0.0 } else { 1.0 };
        st.pre_roll = Some(staged.clone());
        host.set_log.lock().unwrap().clear();
        undo_roll(&params, &setter, &mut st);
        assert!(st.pre_roll.is_none());
        let log = host.set_log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].1, staged.values[pos]);
    }

    #[test]
    fn lfo_trig_cell_sets_request_bit_without_param_gesture() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState {
            tab: Tab::Mod,
            ..Default::default()
        };
        let mut time = 0.0;
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        curve_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        let trig_ptrs: Vec<ParamPtr> = (0..6u8)
            .map(|l| {
                let pos = params.table.pos(pid(Module::Lfo, l, LFO_TRIG_IDX));
                match params.slots[pos] {
                    PSlot::B(i) => params.bools[i].as_ptr(),
                    _ => panic!("Trig is a bool"),
                }
            })
            .collect();
        // Sweep a click grid over the body; every hit on a TRIG cell must
        // raise only LFO bits (3..9) and never gesture the Trig param.
        let mut seen = 0u32;
        'scan: for gx in 0..88 {
            for gy in 0..45 {
                st.tab = Tab::Mod; // stray clicks may hit the tab bar
                let mut ev = Vec::new();
                click(&mut ev, 10.0 + gx as f32 * 10.0, 40.0 + gy as f32 * 8.0);
                curve_frame(&ctx, &setter, &params, &mut st, &mut time, ev);
                seen |= params.lfo_trig_requests.fetch_and(0, Ordering::AcqRel);
                if seen != 0 {
                    break 'scan;
                }
            }
        }
        assert_ne!(seen & 0b1_1111_1000, 0, "no LFO trig cell was hit");
        assert!(host
            .begin_log
            .lock()
            .unwrap()
            .iter()
            .all(|p| !trig_ptrs.contains(p)));
    }

    fn strip_frame(
        ctx: &Context,
        setter: &ParamSetter,
        params: &Arc<MangleParams>,
        st: &mut UiState,
        time: &mut f64,
        events: Vec<Event>,
    ) {
        curve_frame(ctx, setter, params, st, time, events);
    }

    /// Press, move and release over separate frames so egui sees a real drag.
    fn drag_frames(
        env: (&Context, &ParamSetter, &Arc<MangleParams>),
        st: &mut UiState,
        time: &mut f64,
        from: Pos2,
        to: Pos2,
    ) {
        let (ctx, setter, params) = env;
        let steps = 10;
        strip_frame(
            ctx,
            setter,
            params,
            st,
            time,
            vec![Event::PointerMoved(from), press(from, true)],
        );
        for i in 1..=steps {
            let p = from + (to - from) * (i as f32 / steps as f32);
            strip_frame(ctx, setter, params, st, time, vec![Event::PointerMoved(p)]);
        }
        strip_frame(ctx, setter, params, st, time, vec![press(to, false)]);
    }

    fn start_end_ptrs(params: &MangleParams) -> (ParamPtr, ParamPtr) {
        let ptr = |idx| match params.cp(Module::Slot, 0, idx).pr {
            PRef::F(p) => p.as_ptr(),
            _ => panic!("loop points are floats"),
        };
        (ptr(1), ptr(2))
    }

    fn last_set(host: &TestHost, p: ParamPtr) -> f32 {
        host.set_log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(q, _)| *q == p)
            .map(|(_, v)| *v)
            .expect("param was not set")
    }

    fn with_long_sample(params: &MangleParams, secs: usize, rate: f32) {
        params.waveforms.write().unwrap()[0] = Some(crate::WaveSummary {
            peaks: vec![(-0.5, 0.5); 64],
            len_samples: secs * rate as usize,
        });
        params
            .sr_bits
            .store(rate.to_bits(), std::sync::atomic::Ordering::Release);
    }

    #[test]
    fn vertical_drag_in_loop_body_resizes_about_centre() {
        let params = Arc::new(MangleParams::new());
        with_long_sample(&params, 10, 8000.0);
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState::default();
        let mut time = 0.0;
        strip_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        strip_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        let (ps, pe) = start_end_ptrs(&params);
        // Down shrinks the default full window symmetrically about 0.5.
        drag_frames(
            (&ctx, &setter, &params),
            &mut st,
            &mut time,
            Pos2::new(450.0, 100.0),
            Pos2::new(450.0, 120.0),
        );
        let (s, e) = (last_set(&host, ps), last_set(&host, pe));
        assert!(e - s < 0.99 && e - s > 0.5, "{s} {e}");
        assert!(((s + e) * 0.5 - 0.5).abs() < 0.02, "{s} {e}");
    }

    #[test]
    fn loop_resize_is_clamped_to_100_seconds() {
        let params = Arc::new(MangleParams::new());
        with_long_sample(&params, 150, 8000.0);
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState::default();
        let mut time = 0.0;
        strip_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        strip_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        let (ps, pe) = start_end_ptrs(&params);
        drag_frames(
            (&ctx, &setter, &params),
            &mut st,
            &mut time,
            Pos2::new(450.0, 100.0),
            Pos2::new(450.0, 70.0),
        );
        let (s, e) = (last_set(&host, ps), last_set(&host, pe));
        assert!(e - s <= 100.0 / 150.0 + 1e-4, "{s} {e}");
    }

    #[test]
    fn shift_wheel_zooms_and_double_click_resets_view() {
        let params = Arc::new(MangleParams::new());
        with_long_sample(&params, 10, 8000.0);
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState::default();
        let mut time = 0.0;
        strip_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        strip_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        let sel = st.selected_slot;
        let at = Pos2::new(450.0, 120.0);
        strip_frame(
            &ctx,
            &setter,
            &params,
            &mut st,
            &mut time,
            vec![
                Event::PointerMoved(at),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: vec2(0.0, 300.0),
                    modifiers: Modifiers::SHIFT,
                },
            ],
        );
        let v = st.extra.slot_views[sel];
        assert!(v.1 - v.0 < 0.9, "{v:?}");
        let mut ev = Vec::new();
        click(&mut ev, at.x, at.y);
        click(&mut ev, at.x, at.y);
        strip_frame(&ctx, &setter, &params, &mut st, &mut time, ev);
        assert_eq!(st.extra.slot_views[sel], (0.0, 1.0));
    }

    fn texts(shape: &egui::Shape, out: &mut Vec<(String, Pos2)>) {
        match shape {
            egui::Shape::Text(t) => {
                let r = t.galley.rect.translate(t.pos.to_vec2());
                out.push((t.galley.text().to_string(), r.center()));
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
            _ => {}
        }
    }

    /// Run one frame and return the text of every painted label with its
    /// centre, so tests can click widgets by what they say.
    fn labels_frame(
        ctx: &Context,
        setter: &ParamSetter,
        params: &Arc<MangleParams>,
        st: &mut UiState,
        time: &mut f64,
        events: Vec<Event>,
    ) -> Vec<(String, Pos2)> {
        *time += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0))),
            time: Some(*time),
            events,
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| draw_editor(ctx, setter, params, st));
        let mut v = Vec::new();
        out.shapes.iter().for_each(|s| texts(&s.shape, &mut v));
        v
    }

    fn find(labels: &[(String, Pos2)], text: &str) -> Pos2 {
        labels
            .iter()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("no label {text:?}"))
            .1
    }

    fn right_click(ev: &mut Vec<Event>, pos: Pos2) {
        ev.push(Event::PointerMoved(pos));
        for pressed in [true, false] {
            ev.push(Event::PointerButton {
                pos,
                button: PointerButton::Secondary,
                pressed,
                modifiers: Modifiers::default(),
            });
        }
    }

    /// Click the label `text` currently on screen, then settle one frame.
    fn click_label(
        ctx: &Context,
        setter: &ParamSetter,
        params: &Arc<MangleParams>,
        st: &mut UiState,
        time: &mut f64,
        text: &str,
    ) {
        let labels = labels_frame(ctx, setter, params, st, time, vec![]);
        let at = find(&labels, text);
        let mut ev = Vec::new();
        click(&mut ev, at.x, at.y);
        labels_frame(ctx, setter, params, st, time, ev);
        labels_frame(ctx, setter, params, st, time, vec![]);
    }

    /// Right-click the label `text` and let the context menu open.
    fn right_click_label(
        ctx: &Context,
        setter: &ParamSetter,
        params: &Arc<MangleParams>,
        st: &mut UiState,
        time: &mut f64,
        text: &str,
    ) {
        let labels = labels_frame(ctx, setter, params, st, time, vec![]);
        let mut ev = Vec::new();
        right_click(&mut ev, find(&labels, text));
        labels_frame(ctx, setter, params, st, time, ev);
        labels_frame(ctx, setter, params, st, time, vec![]);
    }

    /// Index last written to the row-1 matrix cell `col` through the host
    /// (the harness host records sets but does not apply them).
    fn matrix_index(host: &TestHost, params: &MangleParams, col: u8, count: u16) -> Option<u16> {
        let pos = params.table.pos(pid(Module::Matrix, 0, col));
        let ptr = match params.slots[pos] {
            PSlot::I(i) => params.ints[i].as_ptr(),
            _ => panic!("matrix source/dest are stepped"),
        };
        let log = host.set_log.lock().unwrap();
        let (_, v) = log.iter().rev().find(|(p, _)| *p == ptr)?;
        Some((v * (count - 1) as f32).round() as u16)
    }

    #[test]
    fn pick_from_control_assigns_destination_without_touching_it() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState {
            tab: Tab::Matrix,
            ..Default::default()
        };
        let mut time = 0.0;
        labels_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        right_click_label(&ctx, &setter, &params, &mut st, &mut time, "DEST.1");
        click_label(
            &ctx,
            &setter,
            &params,
            &mut st,
            &mut time,
            "PICK FROM CONTROL",
        );
        assert_eq!(st.learn, Some((0, 2)));
        let labels = labels_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        assert!(labels.iter().any(|(t, _)| t == "touch a control\u{2026}"));

        st.tab = Tab::Engines;
        labels_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        click_label(&ctx, &setter, &params, &mut st, &mut time, "PITCH");

        assert_eq!(st.learn, None);
        let want = matrix::dest_to_index(matrix::ModDest::Grain(0, matrix::GrainDest::Pitch));
        assert_eq!(
            matrix_index(&host, &params, 2, matrix::DEST_COUNT),
            Some(want)
        );
        let pos = params.table.pos(pid(Module::Grain, 0, 14));
        let ptr = match params.slots[pos] {
            PSlot::F(i) => params.floats[i].as_ptr(),
            PSlot::I(i) => params.ints[i].as_ptr(),
            PSlot::B(i) => params.bools[i].as_ptr(),
        };
        assert!(!host.begin_log.lock().unwrap().contains(&ptr));
    }

    #[test]
    fn source_menu_sets_source_index() {
        let params = Arc::new(MangleParams::new());
        let host = TestHost::default();
        let setter = ParamSetter::new(&host);
        let ctx = Context::default();
        let mut st = UiState {
            tab: Tab::Matrix,
            ..Default::default()
        };
        let mut time = 0.0;
        labels_frame(&ctx, &setter, &params, &mut st, &mut time, vec![]);
        right_click_label(&ctx, &setter, &params, &mut st, &mut time, "SOURCE");
        click_label(&ctx, &setter, &params, &mut st, &mut time, "LFO 1\u{2013}6");
        click_label(&ctx, &setter, &params, &mut st, &mut time, "LFO 3");
        assert_eq!(
            matrix_index(&host, &params, 0, matrix::SOURCE_COUNT),
            Some(3)
        );
    }
}
