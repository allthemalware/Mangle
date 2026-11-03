# Mangle

[![CI](https://github.com/allthemalware/Mangle/actions/workflows/ci.yml/badge.svg)](https://github.com/allthemalware/Mangle/actions/workflows/ci.yml)

A granular texture instrument in **CLAP** plugin form. Load up to six
samples, sculpt them with three independent grain engines, and modulate
everything with LFOs, drift, noise, drawable curves and a 32-slot mod
matrix. Each engine can run in one of four synthesis modes — **Grains,
Stretch, Cloud, or Spectral** (a phase vocoder) — so it moves between
Clouds-style clouds, tape-scrub, and spectral freeze. A spiritual port of
the Torsion Texture Lab, minus the rack.

> **Alpha (v0.1.0).** This is a testing build. The DSP core and plugin
> shell are unit-tested and load in a CLAP host, but several features have
> not yet been auditioned live in a DAW — see [Known limitations](#known-limitations-what-to-test).
> Feedback and bug reports are exactly what this build is for.

---

## What you need

- **A CLAP host.** Developed and tested against **Bitwig**; any CLAP-capable
  host (Bitwig, Reaper 7+, Qtractor, …) should work.
- **Rust (current stable)** to build the plugin — install via [rustup](https://rustup.rs).
- **Linux** is the tested platform (developed on Arch). It should also build
  on macOS/Windows via the same command, but those are untested — reports welcome.

## Build & install

From the repo root:

```sh
cargo xtask bundle mangle --release
```

This produces the plugin bundle at:

```
target/bundled/mangle.clap
```

Copy or symlink it into your CLAP folder, then rescan plugins in your host:

| OS | CLAP folder |
|----|-------------|
| Linux | `~/.clap/` |
| macOS | `~/Library/Audio/Plug-Ins/CLAP/` |
| Windows | `%COMMONPROGRAMFILES%\CLAP\` |

```sh
# Linux example
ln -s "$PWD/target/bundled/mangle.clap" ~/.clap/mangle.clap
```

Bitwig picks it up on the next plugin rescan.

## First sound (30 seconds)

1. Add **Mangle** to an instrument track and play a note — you'll hear a
   built-in fallback tone even with no sample loaded, so you know it's alive.
2. Open the GUI and click a slot's **LOAD** button to load a sample
   (wav / flac / aiff / ogg / mp3). The waveform appears in the strip.
3. Play again — the sample now plays with key tracking. Drag the loop edges
   on the waveform to set the grain window.
4. Tweak the grain cells on the **ENGINES** tab (RATE, LENGTH, SPRAY, PITCH,
   SHAPE…) — **G1 / G2 / G3** switch between the three engines — switch
   **MODE** between Grains / Stretch / Cloud / Spectral, and hit **⚄ RND** to
   roll a random patch. The other tabs hold the global **MOD** sources, the
   **MATRIX** (with the master filter), and **CURVES** for drawing your own
   LFO/envelope shapes.

### Loading samples — other paths
- **Per-slot LOAD button** (native file picker) — the reliable path, works in
  every host. Sample loads run on a background thread with per-slot status
  (`loading`, `missing — LOAD to relink`, `failed`) and a message log in the
  editor.
- **Drag & drop** — drag audio files from a file manager or the host's
  browser onto the editor. Files land in the slot tab under the pointer
  (or the selected slot), successive slots for multiple files. On Linux/X11
  this uses a vendored XDND patch (see `vendor/README.md`); Wayland-native
  hosts are not covered yet.
- **`MANGLE_SAMPLE=/path/to/file`** environment variable — fills slot 1 on
  load (handy for quick testing from the terminal).
- **Presets & embedded samples** — presets remember their sample references,
  reload them on the background thread without stalling audio, and clear any
  slots the preset does not reference. A per-preset **EMBED** toggle stores
  each referenced file's original bytes (base64) in the plugin state —
  bit-exact, so compressed formats stay compressed — and uses them
  automatically when a file is missing or changed.

## What's in it

- **6 sample slots**, each with root note, loop points (capped at 100 s; drag
  ←→ to move, ↑↓ to resize about the centre, or drag edges; Shift-drag /
  Shift-wheel zooms the per-slot view, saved in state; double-click resets) and
  gain.
- **3 grain engines**, each with its own mode (Grains / Stretch / Cloud /
  Spectral), read/resample algorithm (Nearest / Linear / Hermite / Sinc-8),
  pitch, spray, jitter, shape/skew envelope, pan, Pos→Pitch tape-drag, and a
  **pitch-quantize / scale lock** (chromatic, octave, 5th, major, minor,
  pentatonic).
- **Modulators:** per-engine Loop LFOs (per-voice), 6 global LFOs (shared
  across voices, with a one-shot / MSEG mode that reads from Phase to the end
  of the curve and holds the final point, plus host/note/GUI TRIG), Drift,
  Noise (control-rate sample-and-hold on a fixed 750 Hz clock independent of
  block size and note events), Macros, and drawable curves (tabbed single-curve
  editor with DRAW/LINE tools, step quantize 2–64, phase shift,
  smooth/invert/mirror, and a 64-shape thumbnail preset bank).
- **32-slot mod matrix** (`Mod 01`–`Mod 32`) with stereo sources, per-channel
  routing, grouped right-click Source/Scale/Dest menus, `PICK FROM CONTROL`
  destination assignment, and Ctrl-click clear.
- **Master filter** (LP / BP / HP / Notch) with matrix-modulatable cutoff &
  resonance (~5 ms smoothing, updated per control chunk).
- **Polyphony, drone mode & MIDI/CLAP:** 16-voice pool plus a dedicated
  **drone** voice (outside the 16-voice pool) that plays when Global On is
  active, Key is at 0 %, and no notes are held; playing a note takes over and
  releasing returns to the drone. Supports per-note tuning, pressure, volume
  and pan note expressions, MIDI channel pressure, and MIDI **pitch bend**
  (±2 semitones, channel-aware).
- **Squeeze** output leveler, stereo width, master level.
- **RANDOM** with per-section padlocks, Amount/Complexity, a customizable pool
  editor (grain params, LFO params, sources, scales, destinations — saved with
  the preset; Mode, Level, Pitch and Quant are excluded from the default grain
  pool), and TOOLS (`UNDO RND` and `REROLL LAST SEED` for exact recall of the
  last roll in the session) while keeping held notes playing.
- **Realtime engine display:** live grain markers and read cursors overlaid
  on the waveform, plus a scrolling activity lane per engine (grain bursts +
  level; click a lane to jump to that engine's page).

The full **396**-parameter reference (ranges, defaults, `p{:06x}` host IDs such
as `p020104`) lives in [`mangle-parameter-map.md`](mangle-parameter-map.md).

## Known limitations — what to test

These landed after the last live-host session and pass automated tests but
**have not been auditioned in a DAW yet**. Please give them a hard listen and
report anything off:

- Master **filter** (especially heavy cutoff/resonance modulation from the matrix)
- **Pitch-quantize / scale lock**
- **One-shot / MSEG** LFO mode
- **Note-expression** volume & pan
- **Spectral** mode (phase vocoder) — freeze, formant shift, blur
- Sample-accurate note timing at speed

When testing, watch for: clicks/zipper on parameter automation, audible
glitches when stealing voices under heavy polyphony, CPU spikes, and any
dropout or crash. Note your host, sample rate and buffer size in reports.

## Reporting issues

Open an issue at **[github.com/allthemalware/Mangle/issues](https://github.com/allthemalware/Mangle/issues)**.
Include: host + version, OS, sample rate / buffer size, steps to reproduce,
and (if you can) the patch or a short clip of the artifact.

## For developers

Run the workspace test suite and headless smoketest:

```sh
cargo test -p mangle-dsp                 # core DSP unit tests (incl. table_has_396_params)
cargo test --workspace                   # full workspace test suite (DSP, loader, editor & render_block harness)
bash tools/run-smoketest.sh --release    # drives the built .clap through a headless CLAP host
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --all --check`,
`cargo clippy --all-targets -- -D warnings`, the workspace unit tests
(including headless editor render tests in `cargo test -p mangle` and the
scripted-event `render_block` harness in `lib.rs`), and
`bash tools/run-smoketest.sh --release`.

Architecture, crate breakdown and historical API notes are in
[`README.old.md`](README.old.md) and [`BUILD_NOTES.md`](BUILD_NOTES.md).

[Support on Ko-fi](https://ko-fi.com/T5F122USRI)

## License note

Mangle is licensed under **GPLv3** (see `LICENSE`). Only the CLAP plugin is
built; building VST3 would require implementing `Vst3Plugin` and enabling
nih-plug's `vst3` feature (whose GPLv3 `vst3-sys` binding is compatible with
this license).
