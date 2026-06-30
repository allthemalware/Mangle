# Mangle

Granular texture instrument (CLAP). Three grain engines × six sample slots,
per-engine Loop LFOs, six global LFOs, Drift + Noise modulators, a 32-slot
mod matrix, drawable curves, true CLAP polyphony with per-note modulation.
Each engine runs a selectable synthesis **mode** — Grains / Stretch / Cloud /
Spectral (phase vocoder) — in the spirit of Clouds & Plaits. Spiritual port of
the Torsion Texture Lab Rack Extension, minus the rack.

Design source of truth: [`mangle-parameter-map.md`](mangle-parameter-map.md)
(ranges, defaults, flags, ID scheme, build order).

## Status

Build-order steps 1–7 and 9 present, plus the tested cores of steps 8 and 10.
The `mangle` plugin crate compiles and loads (see `BUILD_NOTES.md`); the
DSP core is the fully validated part — `cargo test -p mangle-dsp` (94 tests).

- `mangle-dsp` — dependency-free core: stable 380-entry parameter table,
  value mappings, the full grain engine (snapshot rule, COLA-gapless linked
  length, per-engine read/stretch **Algo**: Nearest / Linear / Hermite /
  Sinc-8, **per-channel parameter sets** so the L/R emitters diverge under
  stereo modulation), polyphonic voice manager, the patch→runtime resolver
  (tempo sync, per-note offset composition), the **mod-matrix runtime**
  (row decode, global/voice application scopes, stereo source table, Loop
  Position window shifting, env follower, global LFO/Drift/Noise source
  engine with one-block feedback), **Squeeze** (gated upward leveler into
  a zero-lookahead limiter, stereo-linked), the **state format** (versioned
  binary blob: 9 curves, RANDOM locks + settings, sample slot references;
  hex codec for the host field store), the RANDOM engine with per-unit
  locks, and **selectable per-engine modes** — Grains / Stretch (time-stretch +
  scrub) / Cloud (diffuse smear) / Spectral (a streaming phase vocoder over an
  in-crate FFT: spectral freeze, formant-preserving pitch shift, blur, identity
  phase locking, and a zero-latency primed onset). 94 unit tests
  (`cargo test -p mangle-dsp`).
- `mangle` — nih-plug CLAP shell with the **full 380-parameter set
  generated from the table** (manual `Params` impl, no derive): continuous
  params normalized with table-driven display formatting, stepped params
  with proper names (divisions, matrix sources/dests, algorithms, note
  names), per-note-modulatable params carrying CLAP poly-mod IDs.
  `PolyModulation`/`MonoAutomation`/`Choke` handled; `VoiceTerminated`
  reported. All three engines render per voice through their Loop LFOs with
  tempo sync, key tracking (per-slot root), slot loop/gain params applied
  live, stereo width + master level. **The matrix runs live**: global pass
  → source tick → per-voice stereo passes, with Macros (incl. per-note
  offsets), pressure, velocity, note, Loop-LFO feedback and the output env
  follower all wired as sources. Non-param state (curves, locks, random
  settings, slot refs) persists through nih-plug's field store with an
  RT-safe generation-counted curve sync, and Squeeze sits in the output
  chain (width → Squeeze → level). Built-in additive fallback tone until
  the loader (step 4) lands. Untested against a live host yet — `API:`
  comments mark the likely nih-plug friction points.
- `mangle-loader` — sample loading: symphonia decode (wav/flac/aiff/ogg/
  vorbis/mp3/adpcm), built-in arbitrary-ratio windowed-sinc resampling
  (alias-filtered decimation, verified by SNR tests), BLAKE3 content
  hashing, 300 s cap, mono→stereo, Torsion multi-drop slot filling.
  10 tests (`cargo test` inside the crate). blake3 is pinned to 1.5.x so
  older toolchains can resolve the graph — safe to bump on current Rust.
- `mangle` editor (`crates/mangle/src/editor.rs`) — the full
  tracker-terminal GUI (egui): hand-painted value cells, waveform strip
  with loop-edge/region dragging, all three grain engines + Loop LFOs,
  LFO 1–6, Drift/Noise/Macros, the 32-slot matrix (4 pages), the RANDOM
  die with per-unit padlocks + Amount/Complexity menu, a TOOLS menu
  (init, reroll, copy-grain), and the EDIT.CURVES screen (freehand draw,
  presets, smooth/invert/mirror). Written against the egui API from
  memory — **will need an API-drift fixing pass on first build**; the
  fragile call sites are catalogued in `BUILD_NOTES.md`.
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

## Loading samples (pre-GUI)

Until the GUI lands, two paths exist: presets — slot references stored in
state load automatically on activate (with hash-mismatch warnings) — and
the dev hook `MANGLE_SAMPLE=/path/to/file` which fills slot 1 and records
the reference. Drag-and-drop arrives with the GUI (build step 6).

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

Planned UI: a padlock glyph on every section header toggles that unit's
lock; the RANDOM button fires a seeded roll, and long-press/right-click
opens a popover with Amount, Complexity, the parameter pools (grain Level +
Pitch excluded by default, matching Torsion), and the source/dest pools.
Seeds are stored in state so a happy accident can be recalled.

## Hardening notes

The audio path is allocation-free in steady state and at note-on: all 16
voices (grain pools, offset lists, scratch) are pre-allocated, and the
Sinc-8 table is pre-warmed in `initialize`. Modulation runs on a fixed
~64-sample control chunk regardless of host buffer size, Drift's random
forcing is scaled by 1/√dt so its character is buffer-size-invariant, LFOs
honor their Transport flag (phase locked to the host timeline while
playing), CLAP note-expression tuning lands in the pitch path, and the lag
/ envelope one-poles flush denormals.

## License note

VST3 export is deliberately disabled (`default-features = false` on
`nih_plug`) to keep the GPLv3 `vst3-sys` out of the dependency tree. CLAP
only. Re-enable the `vst3` feature if you ever want a VST3 build and accept
the license implications.
