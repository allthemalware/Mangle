# Mangle

[![CI](https://github.com/allthemalware/Mangle/actions/workflows/ci.yml/badge.svg)](https://github.com/allthemalware/Mangle/actions/workflows/ci.yml)

Granular texture instrument (CLAP). Three grain engines × six sample slots,
per-engine Loop LFOs, six global LFOs, Drift + Noise modulators, a 32-slot
mod matrix, drawable curves, true CLAP polyphony with per-note modulation.
Each engine runs a selectable synthesis **mode** — Grains / Stretch / Cloud /
Spectral (phase vocoder) — in the spirit of Clouds & Plaits. Spiritual port of
the Torsion Texture Lab Rack Extension, minus the rack.

Design source of truth: [`mangle-parameter-map.md`](mangle-parameter-map.md)
(ranges, defaults, flags, ID scheme, build order).

## Status

See [`README.md`](README.md) for current build, install, and feature status.
All build-order steps 1–10 are implemented and verified by the workspace test
suite (`cargo test --workspace`) and the headless CLAP smoketest
(`bash tools/run-smoketest.sh --release`; see `BUILD_NOTES.md`), except the
deferred factory preset bank.

- `mangle-dsp` — dependency-free core: stable 396-entry parameter table
  (`table_has_396_params`), value mappings, the full grain engine (snapshot
  rule, COLA-gapless linked length, per-engine read/stretch **Algo**: Nearest /
  Linear / Hermite / Sinc-8, **per-channel parameter sets** so the L/R emitters
  diverge under stereo modulation), polyphonic voice manager (16-voice pool
  plus a dedicated drone voice when Global On and Key = 0 % with no held note),
  the patch→runtime resolver (tempo sync, per-note offset composition, pitch
  scale lock), the **mod-matrix runtime** (row decode for `Mod 01`–`Mod 32`,
  global/voice application scopes, stereo source table, Loop Position window
  shifting, env follower, global LFO/Drift/Noise source engine with one-block
  feedback and 750 Hz control-rate sample-and-hold Noise), the **master
  filter** (Cytomic SVF, LP/BP/HP/Notch, ~5 ms smoothed cutoff/reso updated per
  control chunk), **Squeeze** (gated upward leveler into a zero-lookahead
  limiter, stereo-linked), the **state format** (versioned binary blob v2: 9
  curves, RANDOM locks + settings + pools, sample slot references, per-instance
  seed, per-slot waveform zoom views, and the base64 embed-samples flag; v1
  presets load cleanly), the RANDOM engine with per-unit locks and customizable
  pools, and **selectable per-engine modes** — Grains / Stretch (time-stretch +
  scrub) / Cloud (diffuse smear) / Spectral (a streaming phase vocoder over an
  in-crate FFT: spectral freeze, formant-preserving pitch shift, blur, identity
  phase locking, and a zero-latency primed onset). Covered by the workspace
  test suite (`cargo test -p mangle-dsp`).
- `mangle` — nih-plug CLAP shell with the **full 396-parameter set
  generated from the table** (manual `Params` impl, no derive, exposing string
  IDs `p{:06x}` such as `p020104`): continuous
  params normalized with table-driven display formatting, stepped params
  with proper names (divisions, matrix sources/dests, algorithms, modes, scale
  quantize, filter types, note names), per-note-modulatable params carrying
  CLAP poly-mod IDs.
  `PolyModulation`/`MonoAutomation`/`Choke` handled; `VoiceTerminated`
  (`CLAP_EVENT_NOTE_END`) reported on release, voice steal, and RND/panic.
  All three engines render per voice through their Loop LFOs with
  tempo sync, key tracking (per-slot root), MIDI pitch bend (±2 semitones,
  channel-aware), note-expression tuning/volume/pan/pressure, slot loop/gain
  params applied live, stereo width → master filter → Squeeze → master level.
  **The matrix runs live**: global pass → source tick → per-voice stereo
  passes, with Macros (incl. per-note offsets), pressure (note-expression and
  MIDI channel pressure), velocity, note, Loop-LFO feedback and the output env
  follower all wired as sources. Non-param state (curves, locks, random
  settings/pools, slot refs, instance seed, waveform zoom views, and optional
  base64 embedded sample bytes) persists through nih-plug's field store with an
  RT-safe generation-counted sync. Built-in additive fallback tone when no
  sample is loaded; verified by the scripted-event `render_block` test harness
  and the headless CLAP smoketest.
- `mangle-loader` — sample loading: symphonia decode (wav/flac/aiff/ogg/
  vorbis/mp3/adpcm), built-in arbitrary-ratio windowed-sinc resampling
  (alias-filtered decimation, verified by SNR tests), BLAKE3 content
  hashing, base64 codec for embedded sample bytes, 300 s cap, mono→stereo,
  Torsion multi-drop slot filling. Covered by unit tests (`cargo test -p
  mangle-loader`). blake3 is pinned to 1.5.x so older toolchains can resolve
  the graph — safe to bump on current Rust.
- `mangle` editor (`crates/mangle/src/editor.rs`) — the full
  tracker-terminal GUI (egui): hand-painted value cells, waveform strip
  with loop-edge/region dragging (drag ←→ moves, ↑↓ resizes about centre,
  Shift-drag/wheel zooms, double-click resets) and live grain/cursor overlays,
  all three grain engines + Loop LFOs, LFO 1–6 (with One-Shot and momentary
  TRIG), Drift/Noise/Macros, the 32-slot matrix (4 pages, grouped right-click
  menus, `PICK FROM CONTROL`, Ctrl-click clear) + master filter, the RANDOM
  die with per-unit padlocks + Amount/Complexity + pool editor, a TOOLS menu
  (init, `UNDO RND`, `REROLL LAST SEED`, copy-grain, `EMBED` toggle), and the
  tabbed single-curve `CURVES` editor (DRAW/LINE tools, step quantize 2–64,
  phase shift, smooth/invert/mirror, 64-shape thumbnail preset bank). Covered
  by headless egui render tests (`cargo test -p mangle`).
- `xtask` — nih-plug bundler.

## Layout

```
mangle-parameter-map.md     design doc (the contract)
crates/mangle-dsp/          pure-Rust core, zero deps
  src/params.rs             param table: IDs, maps, defaults, CLAP flags
  src/matrix.rs             mod sources/dests, stable enum indices, slot eval
  src/random.rs             RANDOM engine + per-unit locks + pools
  src/curve.rs              drawable curves, stereo LFOs, drift, noise
  src/grain.rs              grain snapshot, shape/skew envelope, emitter clock
  src/rng.rs                seeded SplitMix64→Xoroshiro128** (reproducible)
crates/mangle/              nih-plug CLAP shell (cdylib)
xtask/                      `cargo xtask bundle` support
```

## Loading samples

Sample loads run on a background thread with per-slot status (`loading`,
`missing — LOAD to relink`, `failed`) and an editor message log:
- **Per-slot LOAD button** (native `rfd` file dialog on its own thread).
- **Drag-and-drop** onto the editor (on X11 via the vendored XDND patches in
  `vendor/`; multi-file drops fill successive slots).
- **Presets** store slot file references + BLAKE3 hashes and reload them on the
  background thread without stalling audio, clearing unreferenced slots. With
  the per-preset **EMBED** toggle enabled, each referenced file's original
  bytes (base64, bit-exact) are stored in state and used when a file is missing
  or changed.
- **`MANGLE_SAMPLE=/path/to/file`** fills slot 1 on load.

## Build

Needs current stable Rust (rustup). From the repo root:

```sh
cargo xtask bundle mangle --release
```

Output: `target/bundled/mangle.clap` → copy/symlink into `~/.clap/`.
Bitwig picks it up on the next plugin rescan.

Run the core tests (no nih-plug fetch needed):

```sh
cargo test -p mangle-dsp
```

## Stretch algorithms

Each grain engine has an **Algo** switch choosing how grains read and
resample their source: Nearest (aliased, gritty), Linear (the neutral
workhorse), Hermite (smooth 4-point), Sinc-8 (clean windowed sinc, holds up
under big pitch shifts). It is snapshot-frozen per grain like every other
grain parameter, randomizable (in the default pool), and stored as a step
index so spectral modes (Paulstretch-style smear) can append in v2 without
breaking saved patches.

## RANDOM locks

Every randomizable unit has its own lock: Grain 1–3, Loop LFO 1–3, LFO 1–6,
Drift, Noise, Macros (locked by default), the mod matrix, and the sample
loop points. A locked unit's parameters are never touched, and the matrix
generator will neither route *into* it nor use it as a source/scale for new
rows. Locks live in plugin state, not host params.

UI: a padlock glyph on every section header toggles that unit's lock; the
RANDOM button (`⚄ RND`) fires a seeded roll while keeping held notes playing;
the popover exposes Amount, Complexity, and the parameter/source/scale/dest
pool editor (with Mode, Level, Pitch, and Quant excluded from the default
grain pool, and pools saved with the preset in v2 state). TOOLS provides
`UNDO RND` and `REROLL LAST SEED` for exact recall of the last roll in the
session.

## Hardening notes

The audio path is allocation-free in steady state and at note-on: all 16
voices (plus the dedicated drone voice, grain pools, offset lists, scratch) are
pre-allocated, and the Sinc-8 table is pre-warmed in `initialize`. Modulation
runs on a fixed ~64-sample control chunk regardless of host buffer size,
Noise is control-rate sample-and-hold on a fixed 750 Hz clock independent of
block size and note events, Drift's random forcing is scaled by 1/√dt so its
character is buffer-size-invariant, the master filter's modulation targets
follow the control chunk with ~5 ms cutoff/resonance smoothing, LFOs honor
their Transport flag (phase locked to the host timeline while playing;
transport-locked one-shots re-fire each cycle), CLAP note-expression tuning and
MIDI pitch bend (±2 st) land in the pitch path, and the lag / envelope
one-poles flush denormals.

## License note

Mangle is licensed under **GPLv3** (`LICENSE`). Only the CLAP plugin is built;
a VST3 build would require a `Vst3Plugin` implementation and enabling
nih-plug's `vst3` feature (whose GPLv3 `vst3-sys` crate is compatible with this
license).
