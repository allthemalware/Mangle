# Mangle — build notes (read before first `cargo build`)

The `mangle-dsp` and `mangle-loader` crates are compiled and unit-tested
(`cargo test -p mangle-dsp` → 94 pass; the loader's 10 pass). The `mangle`
plugin crate **compiles and loads** against egui 0.31.1 / current nih-plug.
The notes below are retained as a reference for the API-drift points that
were resolved.

**Not yet host-audited:** the selectable engine modes, the Spectral phase
vocoder, and sample-accurate event timing landed after the original alpha
test — they pass the unit tests but have not been played through a live host. Audition them in Bitwig (listen for
artifacts, watch CPU at polyphony, confirm `assert_process_allocs` never trips)
before relying on them.

Expect the first `cargo build` to surface real signature mismatches. This
file is the map. None of these are logic bugs — they're API-drift fixes
against whatever revision of nih-plug / egui your checkout pins.

## How to build

```sh
cargo xtask bundle mangle --release      # -> target/bundled/mangle.clap
```

rust-analyzer will surface every issue below in seconds; fix, repeat. The
DSP core compiling cleanly means the hard correctness work is already
validated — this is plumbing.

## Likely friction points, by file

### `crates/mangle/src/editor.rs` (highest risk)
All egui paint calls funnel through three helpers near the top — `fill`,
`frame`, `txt` — precisely so version drift is fixed in *one* place. Check
these first:

- **`rect_stroke`** — RESOLVED: egui 0.31 takes a `StrokeKind` argument
  (`rect_stroke(rect, 0.0, stroke, StrokeKind::Inside)`). Done in
  `frame()` plus the two drop/curve highlight call sites.
- **`rect_filled` rounding** — the rounding arg type changed across
  versions (`Rounding::ZERO` vs `0.0.into()` vs `CornerRadius`). Fix in
  `fill()` only.
- **`Response::drag_stopped()`** — CONFIRMED present in egui 0.31.
- **`input.raw_scroll_delta`** — CONFIRMED present in egui 0.31.
- **`Frame::none()`** — RESOLVED: deprecated in 0.31, now `Frame::new()`.
- **Drag & drop** — uses `ctx.input(|i| &i.raw).hovered_files /
  dropped_files`. Originally dead code: the pinned baseview had no X11
  XDND support and the pinned egui-baseview never translated baseview drag
  events into egui raw input. Both are now fixed by vendored patched copies
  in `vendor/` (wired up via `[patch]` in the workspace `Cargo.toml`):
  baseview gains the XDND target implementation backported from upstream
  master, egui-baseview forwards `DragEntered/DragMoved/DragLeft/DragDropped`
  into `hovered_files`/`dropped_files` plus a synthetic `PointerMoved` (no
  real motion events arrive during an X11 drag — the source grabs the
  pointer). See `vendor/README.md`. The per-slot LOAD buttons (rfd picker)
  remain as the host-independent path.
- **rfd file dialog** — `rfd = "0.15"`. The picker MUST run on a separate
  thread (`std::thread::spawn`), never synchronously inside the egui render
  closure, or it deadlocks/freezes the editor. Results come back through a
  `Mutex<Vec<(slot, path)>>` polled each frame; `ctx.request_repaint()` is
  called while a dialog is open so the result appears without a mouse move.
- **`context_menu` / `menu_button`** — the closure-based menu API has been
  stable, but `ui.close_menu()` placement is occasionally fussy; if a menu
  won't close, that's why.
- **`EguiState::from_size`, `create_egui_editor`** — confirm these names
  against your `nih_plug_egui`. The editor build closure signature
  (`|ctx, setter, state|`) is the current shape; older versions omit the
  state generic.
- **Font/style fields** — `style.override_font_id`,
  `visuals.panel_fill`, `visuals.override_text_color` are current names;
  adjust if your egui renamed them.

### `crates/mangle/src/lib.rs` (shell)
- **`unsafe impl Params`** — the manual `param_map()` returns
  `Vec<(String, ParamPtr, String)>`. Confirm the tuple shape and that
  `ParamPtr` / `.as_ptr()` still match; this is the one place we bypass
  the `#[derive(Params)]` macro to generate 380 params from the table.
- **`with_poly_modulation_id` / `PolyModulationConfig`** — poly-mod opt-in.
  Confirm the const name `CLAP_POLY_MODULATION_CONFIG`.
- **`NoteEvent` arms** — `PolyModulation`, `MonoAutomation`, `PolyTuning`,
  `PolyPressure`, `MidiChannelPressure`, `Choke`, `VoiceTerminated`. Field
  names (`poly_modulation_id`, `normalized_offset`, `tuning`, `pressure`)
  are from memory; the compiler will name any that moved.
- **`context.transport()`** — `.tempo`, `.playing`, `.pos_seconds()`.
  Confirm `pos_seconds()` exists (vs `pos_samples()` + manual divide).
- **`serialize_fields` / `deserialize_fields`** — `BTreeMap<String,String>`
  persistence hooks on the `Params` trait. Confirm the signatures.
- **`AsyncExecutor<Self>`** in the `editor()` hook signature — the generic
  may differ by version.

### What is NOT in question
- All of `mangle-dsp` (params table, grain engine, mod matrix, resolver,
  curves, drift/noise, squeeze, state codec, randomizer + locks).
- All of `mangle-loader` (decode, resample, hash, multi-drop).
- The data contract between them and the shell (the `Patch` /
  `ParamTable` / `ExtraState` types are shared and compile).

## First-run sanity (once it builds)
1. Load in Bitwig; confirm 380 params appear in the generic editor.
2. Hold a note — you should hear the built-in fallback tone (no sample
   loaded). If silent, check the `process()` output split and `g.level`.
3. Launch with `MANGLE_SAMPLE=/path/to/a.wav` — slot 1 should show a
   waveform and play your sample with key tracking.
4. Open the GUI; drag a grain cell, toggle a GATE, drag a loop edge, hit
   RND, open CURVES and draw. Each maps to begin/set/end param gestures.
5. Verify `assert_process_allocs` doesn't trip (it's the canary for any
   audio-thread allocation regressions).

## Headless smoketest (no DAW needed)

`bash tools/run-smoketest.sh` bundles `mangle.clap` and drives it through a
minimal in-process CLAP host (`tools/smoketest.cpp`, CLAP headers vendored under
`tools/clap/`). It covers the shell layer the `mangle-dsp` unit tests can't:
factory/descriptor, the full 384-param table + `value_to_text`, note ports,
note-on/off, note-expression Volume/Pan, the master filter (LP cut + a
matrix→filter route), the `Loop Start = Loop End = 100%` clamp regression (would
crash the audio thread pre-fix), state save/mutate/load roundtrip, and a
null-transport pass. Wire it into CI next to `cargo test`.

The **editor** has its own headless render test (`cargo test -p mangle`,
`editor::gui_tests`): a bare `egui::Context` + stub `GuiContext` drive
`draw_editor` across every screen (main, curves, each matrix page, each slot)
while injecting clicks, drags, scrolls and out-of-bounds input, asserting no
panic, finite geometry (no NaN rects), and balanced begin/end param gestures.
No DAW or window required.
