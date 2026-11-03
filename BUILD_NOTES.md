# Mangle — build notes

The `mangle-dsp`, `mangle-loader`, and `mangle` plugin crates compile cleanly
against egui 0.31.1 and current nih-plug, and are validated by the workspace
test suite (`cargo test --workspace`) and the headless CLAP smoketest
(`bash tools/run-smoketest.sh --release`). The API-drift notes below are
retained as a **historical** reference for the nih-plug / egui integration
points that were resolved during initial bring-up.

**Live-host audition notes:** the selectable engine modes, the Spectral phase
vocoder, and sample-accurate event timing pass the workspace unit tests, the
scripted-event `render_block` harness, and the headless CLAP smoketest. When
auditioning in a DAW such as Bitwig, listen for artifacts, watch CPU at
polyphony, and confirm `assert_process_allocs` never trips.

## How to build

```sh
cargo xtask bundle mangle --release      # -> target/bundled/mangle.clap
```

## Historical API-drift reference (resolved), by file

### `crates/mangle/src/editor.rs`
All egui paint calls funnel through three helpers near the top — `fill`,
`frame`, `txt` — precisely so version drift is fixed in *one* place:

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

### `crates/mangle/src/lib.rs` (shell — resolved)
- **`unsafe impl Params`** — the manual `param_map()` returns
  `Vec<(String, ParamPtr, String)>` with string IDs formatted as `p{:06x}`
  (e.g. `p020104`), bypassing the `#[derive(Params)]` macro to generate 396
  params from the table (`table_has_396_params`).
- **`with_poly_modulation_id` / `PolyModulationConfig`** — poly-mod opt-in via
  `CLAP_POLY_MODULATION_CONFIG` (`voice-info` reports the fixed 16-voice
  capacity).
- **`NoteEvent` arms** — `PolyModulation`, `MonoAutomation`, `PolyTuning`,
  `PolyPressure`, `PolyVolume`, `PolyPan`, `MidiChannelPressure`,
  `MidiPitchBend`, `Choke`, `VoiceTerminated`.
- **`context.transport()`** — `.tempo`, `.playing`, `.pos_seconds()`.
- **`serialize_fields` / `deserialize_fields`** — `BTreeMap<String,String>`
  persistence hooks on the `Params` trait (`mangle_state` hex blob + optional
  `mangle_embed_<slot>` base64 sample bytes).
- **`AsyncExecutor<Self>`** in the `editor()` hook signature.

### What is NOT in question
- All of `mangle-dsp` (params table, grain engine, mod matrix, resolver,
  curves, drift/noise, filter, squeeze, state codec, randomizer + locks/pools).
- All of `mangle-loader` (decode, resample, hash, base64, multi-drop).
- The data contract between them and the shell (the `Patch` /
  `ParamTable` / `ExtraState` types are shared and compile).

## First-run sanity
1. Load in Bitwig; confirm 396 params appear in the generic editor.
2. Hold a note — you should hear the built-in fallback tone (no sample
   loaded). With Global On and Key at 0 % and no note held, the dedicated drone
   voice plays automatically.
3. Launch with `MANGLE_SAMPLE=/path/to/a.wav` — slot 1 should show a
   waveform and play your sample with key tracking.
4. Open the GUI; drag a grain cell, toggle a GATE, drag a loop edge, hit
   RND, open CURVES and draw. Each maps to begin/set/end param gestures.
5. Verify `assert_process_allocs` doesn't trip (it's the canary for any
   audio-thread allocation regressions).

## Headless smoketest & CI (no DAW needed)

`bash tools/run-smoketest.sh --release` bundles `mangle.clap` and drives it
through a minimal in-process CLAP host (`tools/smoketest.cpp`, CLAP headers
vendored under `tools/clap/`). It covers the shell layer that `mangle-dsp` unit
tests alone don't: factory/descriptor, the full 396-param table +
`value_to_text`, note ports, note-on/off, note-expression Volume/Pan, the
master filter (LP cut + a matrix→filter route), the `Loop Start = Loop End =
100%` clamp regression (would crash the audio thread pre-fix), state
save/mutate/load roundtrip, and a null-transport pass. It runs in CI
(`.github/workflows/ci.yml`) alongside `cargo fmt --all --check`,
`cargo clippy --all-targets -- -D warnings`, and the workspace unit tests.

The **shell** also has an in-crate scripted-event render harness
(`render_block` tests in `crates/mangle/src/lib.rs`), and the **editor** has
its own headless render tests (`cargo test -p mangle`, `editor::gui_tests`): a
bare `egui::Context` + stub `GuiContext` drive `draw_editor` across every
screen (main, curves, each matrix page, each slot) while injecting clicks,
drags, scrolls and out-of-bounds input, asserting no panic, finite geometry (no
NaN rects), and balanced begin/end param gestures. No DAW or window required.
