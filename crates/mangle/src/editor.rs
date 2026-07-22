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
//! * RND = randomize (right-click for Amount / Complexity / seed)
//! * waveform: drag loop edges, drag the region body to move it
//! * body tabs: ENGINES (one engine at a time) / MOD / MATRIX / CURVES
//!   (freehand draw + presets + ops); header + waveform stay put
//!
//! `API:` comments mark the egui / nih_plug_egui call sites most likely to
//! need a touch-up against a given checkout (see BUILD_NOTES.md).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use nih_plug::prelude::*;
use nih_plug_egui::resizable_window::ResizableWindow;
use nih_plug_egui::{create_egui_editor, egui};

use egui::{
    vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui,
};

use mangle_dsp::curve::Curve;
use mangle_dsp::engine::GrainMode;
use mangle_dsp::params::{pid, Module, Patch, GRAIN_MODE_IDX, N_GRAIN_MODES};
use mangle_dsp::random::{randomize, RandomOptions};

use crate::{MangleParams, PendingLoad, PSlot};

// ------------------------------------------------------------ palette ---

const BG: Color32 = Color32::from_rgb(0x0a, 0x0c, 0x09);
const HEADER: Color32 = Color32::from_rgb(0x10, 0x14, 0x0c);
const BORDER: Color32 = Color32::from_rgb(0x2b, 0x33, 0x24);
const TEXT: Color32 = Color32::from_rgb(0xc4, 0xcb, 0xb5);
const DIM: Color32 = Color32::from_rgb(0x6e, 0x7c, 0x57);
const ACCENT: Color32 = Color32::from_rgb(0xa4, 0xbf, 0x55);
const BRIGHT: Color32 = Color32::from_rgb(0xd8, 0xe6, 0xae);
const ACCENT_FAINT: Color32 = Color32::from_rgba_premultiplied(0x14, 0x18, 0x0a, 0xff);

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
    ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, c), StrokeKind::Inside);
}

fn txt(ui: &Ui, pos: Pos2, anchor: Align2, s: &str, size: f32, c: Color32) {
    ui.painter().text(pos, anchor, s, FontId::monospace(size), c);
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
        CellParam { pr, default_norm: info.default_norm, steps: info.map.step_count(), bipolar: info.bipolar }
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

/// Load a dropped/selected file into `slot`: decode + resample happen here
/// on the GUI thread, then the result is queued for the audio thread to
/// swap into the bank. Returns a short status string for display.
fn load_into_slot(params: &MangleParams, slot: usize, path: &std::path::Path) -> String {
    use std::sync::atomic::Ordering;
    let sr = f32::from_bits(params.sr_bits.load(Ordering::Acquire));
    match mangle_loader::load_sample(path, sr) {
        Ok(loaded) => {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            let path_str = path.to_string_lossy().into_owned();
            // Compute the waveform summary and update the display/slot-ref state
            // here on the GUI thread so the audio-thread drain never allocates or
            // takes a blocking lock.
            let summary = crate::summarize(&loaded.slot);
            if let Ok(mut w) = params.waveforms.write() {
                w[slot.min(5)] = Some(summary.clone());
            }
            if let Ok(mut st) = params.extra.write() {
                st.slot_refs[slot.min(5)] = Some(mangle_dsp::state::SlotRef {
                    path: path_str.clone(),
                    content_hash: loaded.content_hash,
                });
            }
            params.extra_gen.fetch_add(1, Ordering::Release);
            if let Ok(mut q) = params.pending_loads.lock() {
                // Replace any queued load already targeting this slot.
                q.retain(|p| p.slot != slot);
                q.push(PendingLoad {
                    slot,
                    sample: Box::new(loaded.slot),
                    path: path_str,
                    content_hash: loaded.content_hash,
                    summary,
                    consumed: false,
                });
            }
            format!("slot {} \u{2190} {}", slot + 1, name)
        }
        Err(e) => format!("load failed: {e}"),
    }
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

/// Drain any completed file-dialog selections and load them. Call once per
/// frame from the editor; returns a status string if anything loaded.
fn poll_dialog_results(params: &Arc<MangleParams>) -> Option<String> {
    let picks: Vec<(usize, std::path::PathBuf)> = match params.dialog_results.lock() {
        Ok(mut q) if !q.is_empty() => q.drain(..).collect(),
        _ => return None,
    };
    let mut status = None;
    for (slot, path) in picks {
        status = Some(load_into_slot(params, slot, &path));
    }
    status
}

// ------------------------------------------------------------ widgets ---

/// Like `cell` but with no inline label — used where a column header
/// names the parameter (matrix-style layout). Identical interaction.
fn cell_nolabel(ui: &mut Ui, setter: &ParamSetter, c: &CellParam, w: f32) {
    cell(ui, setter, c, "", w);
}

/// One tracker cell: label left (dim), value right, level bar underneath.
/// Drag vertical = edit (Shift fine), scroll = step, Ctrl-click = default,
/// click toggles bools.
fn cell(ui: &mut Ui, setter: &ParamSetter, c: &CellParam, label: &str, w: f32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, CELL_H), Sense::click_and_drag());
    let cur = c.norm();
    let hovered = resp.hovered();

    // --- interaction -----------------------------------------------------
    let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
    let fine = ui.input(|i| i.modifiers.shift);

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
                _ => if fine { 0.001 } else { 0.01 },
            };
            c.begin(setter);
            c.set(setter, cur + step * scroll.signum());
            c.end(setter);
        }
    }

    // --- paint -----------------------------------------------------------
    fill(ui, rect, if hovered { ACCENT_FAINT } else { HEADER });
    frame(ui, rect, BORDER);
    let bar_y = rect.bottom() - 2.0;
    let n = c.norm();
    if c.bipolar {
        let cx = rect.center().x;
        let dx = (n - 0.5) * rect.width();
        let (a, b) = if dx >= 0.0 { (cx, cx + dx) } else { (cx + dx, cx) };
        fill(ui, Rect::from_min_max(Pos2::new(a, bar_y), Pos2::new(b, rect.bottom() - 1.0)), ACCENT);
    } else {
        fill(
            ui,
            Rect::from_min_max(
                Pos2::new(rect.left() + 1.0, bar_y),
                Pos2::new(rect.left() + 1.0 + n * (rect.width() - 2.0), rect.bottom() - 1.0),
            ),
            ACCENT,
        );
    }
    txt(ui, Pos2::new(rect.left() + 4.0, rect.center().y), Align2::LEFT_CENTER, label, 10.0, DIM);
    let vcol = if c.is_bool() && n >= 0.5 {
        ACCENT
    } else if hovered || resp.dragged() {
        BRIGHT
    } else {
        TEXT
    };
    txt(ui, Pos2::new(rect.right() - 4.0, rect.center().y), Align2::RIGHT_CENTER, &c.text(), 10.0, vcol);
}

/// Terminal button. Returns the response; paints accent when `active`.
fn tbtn(ui: &mut Ui, label: &str, active: bool, w: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, CELL_H), Sense::click());
    fill(ui, rect, if active { ACCENT_FAINT } else { HEADER });
    frame(ui, rect, if active { ACCENT } else { BORDER });
    let c = if active { ACCENT } else if resp.hovered() { BRIGHT } else { TEXT };
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
    let (rect, _) = ui.allocate_exact_size(vec2(label.len() as f32 * 7.0 + 8.0, CELL_H), Sense::hover());
    txt(ui, Pos2::new(rect.left() + 2.0, rect.center().y), Align2::LEFT_CENTER, label, 10.0, BRIGHT);
}

// ------------------------------------------------------- editor state ---

#[derive(Clone, Copy, PartialEq)]
enum LoopDrag {
    Start,
    End,
    Move(f32), // grab offset from loop start, in normalized units
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

#[derive(Default)]
struct UiState {
    tab: Tab,
    /// ENGINES tab: which grain engine is shown (0..3).
    engine_page: usize,
    matrix_page: usize,
    selected_slot: usize,
    selected_curve: usize,
    loop_drag: Option<LoopDrag>,
    last_draw: Option<(usize, f32)>,
    /// Transient status line (drop results, etc.).
    status: String,
    /// Slot a hovered file-drop would land in (for highlight).
    drop_target: Option<usize>,
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
    // Work on a copy of the extra state; write back once if edited.
    let mut ex = match params.extra.read() {
        Ok(g) => g.clone(),
        Err(_) => return,
    };
    let mut ex_changed = false;

    // --- drag & drop -------------------------------------------------
    // While files hover, mark a drop target (slot under the pointer if it's
    // over the waveform strip, else the selected slot). On drop, load each file
    // into successive slots from the target onward.
    let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
    if hovering {
        st.drop_target = Some(st.selected_slot.min(5));
    } else if st.loop_drag.is_none() {
        st.drop_target = None;
    }
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
        i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect()
    });
    if !dropped.is_empty() {
        let start = st.drop_target.unwrap_or(st.selected_slot).min(5);
        let mut last = String::new();
        for (slot, path) in (start..).zip(&dropped) {
            if slot > 5 {
                break;
            }
            last = load_into_slot(params, slot, path);
        }
        st.selected_slot = start;
        st.status = last;
        st.drop_target = None;
    }

    // Completed file-picker selections (from the per-slot LOAD buttons) load
    // here, on the GUI thread, same path as drops.
    if let Some(status) = poll_dialog_results(params) {
        st.status = status;
    }
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
            egui::ScrollArea::horizontal().id_salt("strip-scroll").show(ui, |ui| {
                ui.set_min_width(MIN_CONTENT_W);
                waveform_strip(ui, setter, params, st, &mut ex, &mut ex_changed);
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
                    Tab::Mod => lfo_and_friends(ui, setter, params, &mut ex, &mut ex_changed),
                    Tab::Matrix => {
                        // Cutoff/Reso are matrix destinations, so the filter
                        // row keeps its seat just above the grid.
                        filter_panel(ui, setter, params);
                        ui.add_space(GAP * 2.0);
                        matrix_panel(ui, setter, params, st, &mut ex, &mut ex_changed);
                    }
                    Tab::Curves => curves_screen(ui, st, &mut ex, &mut ex_changed),
                }
            });
        });

    if ex_changed {
        if let Ok(mut w) = params.extra.write() {
            *w = ex;
        }
        params.extra_gen.fetch_add(1, std::sync::atomic::Ordering::Release);
    }
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
    _st: &mut UiState,
    ex: &mut mangle_dsp::state::ExtraState,
    ex_changed: &mut bool,
) {
    egui::TopBottomPanel::top("hdr")
        .frame(egui::Frame::new().fill(HEADER).inner_margin(4.0))
        .show(ctx, |ui| {
            // Horizontal scroll so RND/TOOLS/CURVES stay reachable when the
            // window is narrower than the header row.
            egui::ScrollArea::horizontal().id_salt("hdr-scroll").show(ui, |ui| {
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
                    cell(ui, setter, &params.cp(Module::Global, 0, 3), "SQUEEZE", 92.0);
                    cell(ui, setter, &params.cp(Module::Global, 0, 4), "LEVEL", 104.0);
                    cell(ui, setter, &params.cp(Module::Global, 0, 5), "VOICES", 86.0);
                    ui.add_space(12.0);

                    // RANDOM die: click = roll, right-click = settings.
                    let rnd = tbtn(ui, "\u{2684} RND", false, 64.0); // ⚄
                    if rnd.clicked() {
                        let seed = entropy_seed();
                        roll_random(params, setter, ex, seed);
                        ex.last_seed = seed;
                        *ex_changed = true;
                    }
                    rnd.context_menu(|ui| {
                        ui.set_min_width(220.0);
                        if ui
                            .add(egui::Slider::new(&mut ex.random_amount, 0.0..=1.0).text("AMOUNT"))
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
                    });

                    ui.menu_button("TOOLS", |ui| {
                        if ui.button("INIT PATCH").clicked() {
                            apply_patch(params, setter, &Patch::init(&params.table));
                            ui.close_menu();
                        }
                        if ui.button("REROLL LAST SEED").clicked() {
                            let seed = ex.last_seed;
                            roll_random(params, setter, ex, seed);
                            ui.close_menu();
                        }
                        ui.separator();
                        for (a, b, label) in
                            [(0u8, 1u8, "COPY G1 \u{2192} G2"), (0, 2, "COPY G1 \u{2192} G3"), (1, 2, "COPY G2 \u{2192} G3")]
                        {
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

fn roll_random(
    params: &MangleParams,
    setter: &ParamSetter,
    ex: &mangle_dsp::state::ExtraState,
    seed: u64,
) {
    let opts = RandomOptions {
        amount: ex.random_amount,
        complexity: ex.random_complexity,
        locks: ex.locks.clone(),
        ..RandomOptions::default()
    };
    let mut patch = read_patch(params);
    randomize(&mut patch, &params.table, &opts, seed);
    apply_patch(params, setter, &patch);
    // Ask the audio thread to hard-stop all voices so no grains/notes hang
    // over the new patch.
    params
        .panic
        .store(true, std::sync::atomic::Ordering::Release);
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
            .rect_stroke(rect, 0.0, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
    } else {
        frame(ui, rect, BORDER);
    }

    // Peaks.
    let waves = params.waveforms.read().ok();
    let have = waves
        .as_ref()
        .map(|ws| ws[sel].is_some())
        .unwrap_or(false);
    if let Some(ws) = waves.as_ref() {
        if let Some(wsum) = &ws[sel] {
            let mid = rect.center().y;
            let half = rect.height() * 0.46;
            let n = wsum.peaks.len().max(1);
            for x in 0..rect.width() as usize {
                let i = x * n / rect.width().max(1.0) as usize;
                let (lo, hi) = wsum.peaks[i.min(n - 1)];
                let x = rect.left() + x as f32;
                ui.painter().line_segment(
                    [Pos2::new(x, mid - hi * half), Pos2::new(x, mid - lo * half)],
                    Stroke::new(1.0, DIM),
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
    let sx = rect.left() + s * rect.width();
    let ex_ = rect.left() + e * rect.width();
    fill(
        ui,
        Rect::from_min_max(Pos2::new(sx, rect.top() + 1.0), Pos2::new(ex_, rect.bottom() - 1.0)),
        Color32::from_rgba_unmultiplied(0xa4, 0xbf, 0x55, 22),
    );
    for x in [sx, ex_] {
        ui.painter()
            .line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(2.0, ACCENT));
    }

    // Loop drag: edges resize, body moves.
    let norm_at = |p: Pos2| ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let n = norm_at(p);
            let grab = 6.0 / rect.width();
            st.loop_drag = if (n - s).abs() < grab {
                Some(LoopDrag::Start)
            } else if (n - e).abs() < grab {
                Some(LoopDrag::End)
            } else if n > s && n < e {
                Some(LoopDrag::Move(n - s))
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
        if let (Some(mode), Some(p)) = (st.loop_drag, resp.interact_pointer_pos()) {
            let n = norm_at(p);
            match mode {
                LoopDrag::Start => cp_start.set(setter, n.min(e - 0.001)),
                LoopDrag::End => cp_end.set(setter, n.max(s + 0.001)),
                LoopDrag::Move(off) => {
                    let width = e - s;
                    let ns = (n - off).clamp(0.0, 1.0 - width);
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
        for i in 0..6usize {
            let loaded = params
                .waveforms
                .read()
                .ok()
                .map(|w| w[i].is_some())
                .unwrap_or(false);
            let label = if loaded { format!("[{}]", i + 1) } else { format!(" {} ", i + 1) };
            if tbtn(ui, &label, i == sel, 34.0).clicked() {
                st.selected_slot = i;
            }
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
        cell(ui, setter, &params.cp(Module::Slot, sel as u8, 0), "ROOT", 90.0);
        cell(ui, setter, &params.cp(Module::Slot, sel as u8, 3), "GAIN", 100.0);
        ui.add_space(8.0);
        section_label(ui, "LOOP.POINTS");
        lock_btn(ui, &mut ex.locks.loop_points, ex_changed);
        if !st.status.is_empty() {
            ui.add_space(10.0);
            txt(
                ui,
                Pos2::new(ui.cursor().left() + 4.0, ui.cursor().top() + CELL_H * 0.5),
                Align2::LEFT_CENTER,
                &st.status,
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
        for e in 0..3usize {
            if tbtn(ui, &format!("G{}", e + 1), e == eng, 44.0).clicked() {
                st.engine_page = e;
            }
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
                        let cp = params.cp(Module::Grain, eng as u8, *idx);
                        cell(ui, setter, &cp, mode_label(*idx, label, mode), cw);
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
            });
            for pair in LFO_GRID.chunks(2) {
                ui.horizontal(|ui| {
                    for (idx, label) in pair {
                        cell(ui, setter, &params.cp(Module::LoopLfo, eng as u8, *idx), label, cw);
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
                txt(ui, Pos2::new(hr.left() + 2.0, hr.center().y), Align2::LEFT_CENTER, "LFO", 10.0, BRIGHT);
                for (_, label) in LFO_GRID.iter() {
                    let (cr, _) = ui.allocate_exact_size(vec2(cw, CELL_H), Sense::hover());
                    txt(ui, cr.center(), Align2::CENTER_CENTER, label, 9.0, DIM);
                }
            });
            for l in 0..6usize {
                ui.horizontal(|ui| {
                    // Left label cell: "n" + padlock, fixed width.
                    let (lr, _) = ui.allocate_exact_size(vec2(label_w - 18.0, CELL_H), Sense::hover());
                    txt(ui, Pos2::new(lr.left() + 4.0, lr.center().y), Align2::LEFT_CENTER, &format!("{}", l + 1), 10.0, BRIGHT);
                    lock_btn(ui, &mut ex.locks.lfo[l], ex_changed);
                    for (idx, _) in LFO_GRID.iter() {
                        cell_nolabel(ui, setter, &params.cp(Module::Lfo, l as u8, *idx), cw);
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
                    cell(ui, setter, &params.cp(Module::Drift, d, 0), &format!("ST.PH.{}", d + 1), cw);
                }
            });
            ui.horizontal(|ui| {
                section_label(ui, "NOISE");
                lock_btn(ui, &mut ex.locks.noise, ex_changed);
                for d in 0..3u8 {
                    cell(ui, setter, &params.cp(Module::Noise, d, 0), &format!("ST.PH.{}", d + 1), cw);
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
                        cell(ui, setter, &params.cp(Module::Macro, m, 0), &format!("M{}", m + 1), mw);
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
fn filter_panel(ui: &mut Ui, setter: &ParamSetter, params: &MangleParams) {
    ui.horizontal(|ui| {
        section_label(ui, "FILTER");
        cell(ui, setter, &params.cp(Module::Fx, 0, 0), "ON", 64.0);
        cell(ui, setter, &params.cp(Module::Fx, 0, 1), "TYPE", 84.0);
        cell(ui, setter, &params.cp(Module::Fx, 0, 2), "CUTOFF", 116.0);
        cell(ui, setter, &params.cp(Module::Fx, 0, 3), "RESO", 96.0);
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
            "ctrl-click clears a cell",
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
                cell(ui, setter, &params.cp(Module::Matrix, row, col as u8), label, *w);
            }
        });
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
        Stroke::new(1.0, BORDER),
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
                ui.painter().line_segment([q, p], Stroke::new(1.5, ACCENT));
            }
            prev = Some(p);
        }
    }

    // Freehand drawing with gap interpolation.
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let half = rect.height() * 0.48;
            let idx = (((p.x - rect.left()) / rect.width()) * 255.0).clamp(0.0, 255.0) as usize;
            let val = ((mid - p.y) / half).clamp(-1.0, 1.0);
            let (a, av) = st.last_draw.unwrap_or((idx, val));
            let (lo, hi) = if a <= idx { (a, idx) } else { (idx, a) };
            for i in lo..=hi {
                let t = if hi == lo { 1.0 } else { (i - lo) as f32 / (hi - lo) as f32 };
                let v = if a <= idx { av + (val - av) * t } else { val + (av - val) * t };
                ex.curves[sel].0[i] = v;
            }
            st.last_draw = Some((idx, val));
            *ex_changed = true;
        }
    }
    if resp.drag_stopped() {
        st.last_draw = None;
    }

    ui.add_space(GAP * 2.0);
    ui.horizontal(|ui| {
        section_label(ui, "PRESET");
        let seed = entropy_seed();
        let presets: [(&str, Curve); 6] = [
            ("RAMP", Curve::ramp()),
            ("SINE", Curve::sine()),
            ("TRI", Curve::triangle()),
            ("SQR", Curve::square()),
            ("STEPS", Curve::steps(8, seed)),
            ("WALK", Curve::random_walk(seed)),
        ];
        for (name, c) in presets {
            if tbtn(ui, name, false, 56.0).clicked() {
                ex.curves[sel] = c.clone();
                *ex_changed = true;
            }
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
    }
    impl GuiContext for TestHost {
        fn plugin_api(&self) -> PluginApi {
            PluginApi::Clap
        }
        fn request_resize(&self) -> bool {
            true
        }
        unsafe fn raw_begin_set_parameter(&self, _p: ParamPtr) {
            self.begins.fetch_add(1, Ordering::Relaxed);
        }
        unsafe fn raw_set_parameter_normalized(&self, _p: ParamPtr, _v: f32) {
            self.sets.fetch_add(1, Ordering::Relaxed);
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
        Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::default() }
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
            drag(&mut ev, Pos2::new(200.0, 120.0), (1..=12).map(|i| Pos2::new(200.0 + i as f32 * 28.0, 120.0)));
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
                (1..=20).map(|i| Pos2::new(120.0 + i as f32 * 15.0, 300.0 + (i as f32 * 0.5).sin() * 40.0)),
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
        assert!(s > 0, "no parameter writes were triggered by the input sweep");
        assert_eq!(b, e, "unbalanced begin/end parameter gestures ({b} begins, {e} ends)");
    }
}
