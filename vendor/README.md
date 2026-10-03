# Vendored dependencies

Patched copies of the GUI-stack crates, substituted for the git revisions
nih-plug pins via `[patch]` sections in the workspace `Cargo.toml`. Both
exist for one reason: **file drag-and-drop into the plugin window**, which
the pinned revisions cannot deliver on Linux/X11.

## baseview (`vendor/baseview`)

Base revision: `RustAudio/baseview @ 9a0b42c09d712777b2edb4c5e0cb6baf21e988f0`
(the exact revision `nih_plug_egui`'s egui-baseview pins).

Patch: **X11 XDND (drag-and-drop target) support**, backported from upstream
baseview master (`src/platform/x11/drag_n_drop.rs`, merged upstream after the
pinned revision, in the restructured 0.2.x tree that nih-plug is not
compatible with). Changes:

- `src/x11/drag_n_drop.rs` — the XDND session state machine (new file;
  adapted: `PhyPoint`/logical `Point` instead of the `dpi` crate, old-style
  two-argument `WindowHandler::on_event`).
- `src/x11/get_property.rs` — chunked window-property fetch, copied verbatim
  from upstream (`xcb_connection/get_property.rs`, winit-derived).
- `src/x11/xcb_connection.rs` — XDND atoms in the `atom_manager!` block, plus
  a `get_property` helper method.
- `src/x11/window.rs` — sets the `XdndAware` (version 5) property on the
  window; `WindowInner::window_id` made `pub(crate)`.
- `src/x11/event_loop.rs` — dispatches `XdndEnter/Position/Drop/Leave`
  ClientMessages and `SelectionNotify` to the state machine.
- `Cargo.toml` — adds `percent-encoding` + `bytemuck` (Linux only); examples,
  dev-dependencies and the examples workspace stripped.

Windows and macOS drag-and-drop already existed at the base revision and are
untouched.

## egui-baseview (`vendor/egui-baseview`)

Base revision: `BillyDM/egui-baseview @ ec70c3fe6b2f070dcacbc22924431edbe24bd1c0`
(the exact revision nih-plug pins).

Patch (all in `src/window.rs`): translate baseview's
`DragEntered/DragMoved/DragLeft/DragDropped` mouse events into egui raw
input — `hovered_files` / `dropped_files` — and push a synthetic
`PointerMoved` for each drag position (during an X11 drag the source grabs
the pointer, so no real motion events arrive). Drag events return
`EventStatus::AcceptDrop(Copy)` so the Windows/macOS backends accept the
drop as well. This was absent upstream at every revision at the time of
vendoring (checked 2026-07-22).

## Upgrading

If nih-plug ever moves to baseview 0.2+/a newer egui-baseview with native
drag-and-drop on X11, delete `vendor/` and the two `[patch]` sections in the
workspace `Cargo.toml`, and drop-handling in `crates/mangle/src/editor.rs`
should keep working unchanged (it only consumes egui's
`hovered_files`/`dropped_files`).
