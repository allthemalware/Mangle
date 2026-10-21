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
  every host.
- **Drag & drop** — drag audio files from a file manager or the host's
  browser onto the editor. Files land in the slot tab under the pointer
  (or the selected slot), successive slots for multiple files. On Linux/X11
  this uses a vendored XDND patch (see `vendor/README.md`); Wayland-native
  hosts are not covered yet.
- **`MANGLE_SAMPLE=/path/to/file`** environment variable — fills slot 1 on
  load (handy for quick testing from the terminal).
- **Presets** remember their sample references and reload them automatically
  (you'll get a log warning if a file changed or moved).

## What's in it

- **6 sample slots**, each with root note, loop points and gain.
- **3 grain engines**, each with its own mode (Grains / Stretch / Cloud /
  Spectral), read/resample algorithm (Nearest / Linear / Hermite / Sinc-8),
  pitch, spray, jitter, shape/skew envelope, pan, Pos→Pitch tape-drag, and a
  **pitch-quantize / scale lock** (chromatic, octave, 5th, major, minor,
  pentatonic).
- **Modulators:** per-engine Loop LFOs, 6 global LFOs (with a one-shot / MSEG
  mode), Drift, Noise, Macros, and drawable curves.
- **32-slot mod matrix** with stereo sources and per-channel routing.
- **Master filter** (LP / BP / HP / Notch) with matrix-modulatable cutoff &
  resonance.
- **Note expression:** per-note tuning, pressure, volume and pan.
- **Squeeze** output leveler, stereo width, master level.
- **RANDOM** with per-section padlocks so you can lock what you like and
  reroll the rest.
- **Realtime engine display:** live grain markers and read cursors overlaid
  on the waveform, plus a scrolling activity lane per engine (grain bursts +
  level; click a lane to jump to that engine's page).

The full parameter reference (ranges, defaults, IDs) lives in
[`mangle-parameter-map.md`](mangle-parameter-map.md).

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

Run the validated DSP tests (no plugin/host fetch needed):

```sh
cargo test -p mangle-dsp        # ~110 unit tests
cargo test --workspace          # everything
bash tools/run-smoketest.sh     # drives the built .clap through a headless CLAP host
```

Architecture, crate breakdown and API notes are in
[`README.old.md`](README.old.md) and [`BUILD_NOTES.md`](BUILD_NOTES.md).

<script type='text/javascript' src='https://storage.ko-fi.com/cdn/widget/Widget_2.js'></script><script type='text/javascript'>kofiwidget2.init('Support me on Ko-fi', '#48663b', 'T5F122USRI');kofiwidget2.draw();</script> 

## License note

CLAP only — VST3 export is deliberately disabled to keep the GPLv3
`vst3-sys` crate out of the dependency tree. Re-enable the `vst3` feature in
`Cargo.toml` if you want a VST3 build and accept the license implications.
