# Mangle — Parameter Map v0.1

Canonical parameter reference for the Mangle CLAP instrument. Companion to the
locked UI direction (muted tracker-terminal). Everything in Torsion 1.0.0 is
retained unless marked **[new]** or **[changed]**. Rack-Extension-only features
(CV jacks, Reason patch browser, Combinator targets) are gone; their roles are
absorbed by the mod matrix, Macros, and CLAP host facilities.

---

## 0. Conventions

**Flags**

| Flag | Meaning |
|------|---------|
| A    | `CLAP_PARAM_IS_AUTOMATABLE` |
| M    | `CLAP_PARAM_IS_MODULATABLE` (host modulation, non-destructive offset) |
| PN   | `CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID` (per-voice offset) |
| S    | `CLAP_PARAM_IS_STEPPED` (bools, enums, ints) |

**Value model.** All params normalized 0..1 internally (bipolar params map
-1..+1 across 0..1). Display ranges below. `lin` = linear, `exp` =
exponential/log taper, `enum` = stepped list.

**Snapshot rule (Torsion semantics, load-bearing).** Every parameter in the
Grain group is sampled exactly once at grain creation. A live grain is never
affected by later changes. Non-grain continuous params smooth over ~5 ms.
Noise sources are re-rolled per grain; this is what makes Noise the
"per-grain variation" source.

**Modulation formula.** For each destination, in normalized units:

```
final = clamp( base + Σ_rows ( amt/100 · src · scale ) )
```

where `src` ∈ -1..+1, `scale` is the value of the row's Scale source
(`×1.0` when Scale = None). Unipolar params clamp 0..1, bipolar -1..+1.

**Shared tempo-sync division enum** (31 steps, used by every synced Rate):
`32/4, 28/4, 24/4, 20/4, 16/4, 12/4, 8/4, 7/4, 6/4, 5/4, 4/4, 7/8, 3/4, 5/8,
2/4, 7/16, 5/8T, 3/8, 4/8T, 5/16, 1/4, 3/16, 2/8T, 1/8, 1/8T, 1/16, 1/16T,
1/32, 1/32T, 1/64, 1/128`

---

## 1. Global

| Param   | Range        | Default | Map  | Flags | Notes |
|---------|--------------|---------|------|-------|-------|
| On      | off / on     | on      | bool | A S   | Master gate for all grain generation (Torsion "On"). |
| Key     | 0–100 %      | 100 %   | lin  | A     | Note→pitch tracking amount; uses per-slot Root Note. 0 % = ignore notes' pitch entirely. |
| Stereo  | 0–200 %      | 100 %   | lin  | A M   | Output width. 0 % mono-sum, >100 % widened with partial inversion. |
| Squeeze | off / on     | on      | bool | A S   | Gain leveler: upward compressor → downward comp/limiter. (Front panel said DYN; manual name wins.) |
| Level   | -60–+6 dB    | 0 dB    | exp  | A M   | Master output volume. |
| Voices  | 1–16         | 8       | int  | S     | **[new]** Max polyphony. 1 = mono/drone mode. |

## 2. Macros 1–8  **[new]**

| Param     | Range       | Default | Map | Flags  |
|-----------|-------------|---------|-----|--------|
| Macro 1–8 | -100–+100 % | 0 %     | lin | A M PN |

Host-facing knobs whose only job is to be matrix sources — the CLAP-native
replacement for Torsion's CV In 1–8. Because they carry M + PN, a host can
stream sample-accurate or per-note modulation into them, which then fans out
through the matrix. Shown as eight cells on a collapsible strip under the
header bar.

## 3. Grain engines ×3

All per-grain params carry **PN** so CLAP per-note modulation can offset them
on individual voices. Snapshot rule applies to everything marked ⟲.

| Param      | Range                                  | Default            | Map  | Flags     | Notes |
|------------|----------------------------------------|--------------------|------|-----------|-------|
| Gate       | off / on                               | G1 on, G2/G3 off   | bool | A M PN S  | **[changed]** Button, not knob (your note 1). Bindable/automatable for neutral-pitch soundscape triggering. ANDed with global On and, in poly mode, the voice's note gate. |
| Rate ⟲     | 0–500 Hz (0 = off)                     | 10 Hz              | exp  | A M PN    | Grain emission clock. |
| Rate Tempo | off / on                               | off                | bool | A S       | Switches Rate to the division enum. |
| Rate Div ⟲ | 31 divisions                           | 1/8                | enum | A S       | |
| Jitter ⟲   | 0–100 %                                | 0 %                | lin  | A M PN    | Random emission-time offset; 100 % = up to one full clock period. Affects timing only, never Length. |
| Rate>Len   | off / on                               | on                 | bool | A S       | Inverse rate/length link for constant grain overlap. |
| Length ⟲   | linked: 0–1000 % · free: 0–1000 ms     | 100 % / 100 ms     | exp  | A M PN    | At 100 % linked, output is gapless with defaults elsewhere. |
| Pan ⟲      | 0–100 %                                | 0 %                | lin  | A M PN    | Channel crossfade: 0 keep channel, 50 mono mix, 100 swap (ping-pong). Modulate to spray the field. |
| Level ⟲    | 0–100 %                                | 100 %              | lin  | A M PN    | Per-grain amplitude. |
| Sample ⟲   | 0–100 %                                | 0 % (slot 1)       | lin  | A M PN    | Continuous range divided evenly across *loaded* slots; modulate to smear grains across samples. |
| Position ⟲ | 0–100 %                                | 0 %                | lin  | A M PN    | Grain start offset from the slot's loop start. |
| Loop       | off / on                               | on                 | bool | A S       | Enables Loop LFO → Position modulation. |
| Loop Size ⟲| 0.00–2.00×                             | 1.00×              | lin  | A M PN    | Scales the slot loop window about its midpoint. |
| Gate Sync  | off / on                               | off                | bool | A S       | Retriggers the Loop LFO on engine gate / note-on — loop-player restart behavior. |
| Pitch ⟲    | -12–+12 st (cent resolution)           | 0 st               | lin  | A M PN    | Added on top of Key tracking and note-expression tuning. |
| Pos>Pitch  | off / on                               | off                | bool | A S       | Pitch follows position delta (tape/vinyl drag). Audition by sweeping Loop LFO Rate with Loop on. |
| Direction ⟲| -100–+100 %                            | +100 %             | lin  | A M PN    | Signed playback-speed scalar: -100 reverse, +100 forward, 0 freezes the grain on its start sample (spectral-freeze territory). |
| Shape ⟲    | 0–100 %                                | 0 %                | lin  | A M PN    | Grain envelope: 0 cosine, 50 smooth trapezoid, 100 rectangle. |
| Skew ⟲     | -100–+100 %                            | 0 %                | lin  | A M PN    | Envelope tilt: -100 pluck attack, +100 reverse swell. |
| Algo       | Nearest / Linear / Hermite / Sinc-8    | Linear             | enum | A S       | **[new]** Per-grain read interpolation, frozen at snapshot like everything else: zero-order hold (gritty) → linear → 4-pt Hermite → 8-tap windowed sinc (clean). Independent of Mode (it's *how* a grain reads, not *what paradigm* the engine runs). |
| Mode       | Grains / Stretch / Cloud / Spectral    | Grains             | enum | A S       | **[new]** Synthesis paradigm for the engine (Clouds/Plaits-style). Append-only enum; state stores the step index so modes can grow. See §3a. |

### 3a. Grain engine modes **[new]**

The `Mode` switch reinterprets the existing grain controls so one engine can
behave as several Clouds-flavored textures. All Tier-1 modes share the grain
snapshot/render path and the per-grain `Algo`; they differ only in how each
grain's read **start** and **increment** are chosen.

| Mode | Character | Reinterpreted controls |
|------|-----------|------------------------|
| **Grains** | Classic asynchronous granular (Torsion default; bit-identical to pre-Mode behavior). | — start = Loop-LFO scan + Position; increment = Direction × pitch. |
| **Stretch** | Time-stretch / scrub. A per-channel playhead walks the loop window at **Direction×**, *decoupled from pitch*; grains emit at the playhead and read forward at the pitch ratio only. Direction 0 freezes the playhead while Pitch still transposes — a *moving* freeze. | **Direction** = scrub speed/sign (not grain read speed); **Pitch** = pure transpose; Loop-LFO scan is bypassed (Position offsets the playhead). |
| **Cloud** | Diffuse / Paulstretch-style smear: each grain is sprayed to a random read-phase, and dense overlapping grains average into a phase-randomized texture. | **Jitter** = position **spray** (0 = tight at Position, 100 % = whole window) rather than emission-time jitter; the emission clock runs regular. |
| **Spectral** | Streaming **phase vocoder** (the Clouds spectral mode): clean time-stretch, smooth **spectral freeze**, frequency-domain pitch shift, and diffuse blur, per voice / per channel. Bypasses the grain pool. | **Direction** = time scrub (0 = freeze the spectrum, decoupled from pitch; negative = backward); **Pitch** = transpose; **Position** = playhead offset; **Shape** = formant preserve (0 = classic bin-shift, 1 = formants held while harmonics move); **Skew** = blur (per-bin phase randomization → wash). Jitter/Loop-LFO are ignored. |

*Why reinterpret instead of adding params:* this is the Plaits move — a fixed
control set whose meaning shifts per engine — and it keeps the mod matrix,
state format, and ID scheme unchanged (Mode is a normal stepped param). The
editor relabels each affected cell per mode: Direction→SCRUB in Stretch,
Jitter→SPRAY in Cloud; in Spectral, Direction→TIME, Shape→FORMANT, Skew→BLUR.

**Spectral implementation notes** (`crate::spectral`, `crate::fft`):

* STFT, 1024-pt frame, 256-pt hop (75 % overlap), Hann window, in-crate
  radix-2 FFT (no new dependency). Phases propagate via the standard
  instantaneous-frequency estimate; pitch maps analysis bin *k* → synthesis
  bin round(*k·r*).
* **Zero latency — nothing reported, no host restart.** Because Mangle reads a
  *stored buffer* (not live input), the vocoder is **primed** at note-on
  (`Channel::prime` pre-rolls the overlap-add buffer through its ramp-up frames)
  so the first emitted sample is already at full amplitude. Onset is therefore
  zero-latency for every mode, the plugin reports 0, and no latency-change
  restart ever occurs. (Trade-off: priming runs a few extra FFTs at note-on.)
* **Release tail.** A released Spectral voice drains its overlap-add tail (~one
  frame) instead of cutting — `SpectralState::ringing()` keeps the voice alive
  through `Lane::is_ringing()` so it rings out click-free on both channels.
* **Formant preserve (Shape).** Blends from classic formant-shifting (0) to
  formant-preserving (1). The envelope is the **true cepstral envelope** (real
  cepstrum → low-quefrency lifter → exp), which rides formant peaks far better
  than a magnitude smooth; it costs one extra FFT pair per frame, only when
  Shape > 0.
* **Blur (Skew).** Per-bin phase randomization scaled by Skew → a diffuse wash;
  magnitude-preserving, so pitch is unchanged. Uses per-channel RNG so L/R
  decorrelate (a blurred lane skips the mono shared-vocoder fast path).
* **Phase locking (always on).** Identity phase locking (Laroche–Dolson): the
  synthesis spectrum is peak-picked and every non-peak bin is re-locked to its
  region's peak via the analysis phase relationship, cutting the vocoder's
  "phasiness" during stretch/freeze. Disabled automatically under Blur (which
  wants the opposite — decorrelated phases).
* **CPU / polyphony:** one vocoder per voice **per channel** → up to
  16 × 3 × 2 = 96 FFT streams worst case. Mitigations: only gated/ringing
  channels of sounding voices run; a **mono source with matching per-channel
  params runs a single shared vocoder** (halving FFT work in the common case);
  the FFT plan + window are shared statics; all buffers are pre-allocated
  (~35 KB × 48 lanes, no audio-thread allocation). Still far heavier than Clouds
  (one stereo processor) — for dense spectral patches lower **Voices** (the
  design's drone path) or limit Spectral to one engine.
* **Known v1 limits:** a share→independent transition mid-note (per-channel
  modulation diverging on a mono source, or engaging Blur) re-primes the second
  vocoder with a soft ramp; transient sharpening (peak phase reset on onsets) is
  a possible future refinement on top of the steady-state phase locking.

## 4. Loop LFOs ×3 (one per grain engine; per-voice instanced)

Each engine's Loop LFO scans Position when the engine's Loop is on. In poly
mode every voice runs its own phase accumulator so loops restart per note
when Gate Sync is on.

| Param      | Range         | Default  | Map  | Flags   | Notes |
|------------|---------------|----------|------|---------|-------|
| Curve      | 256-pt table  | ramp     | —    | —       | Drawable; edited in EDIT.CURVES. State data, not a host param (see §9). |
| Gate       | off / on      | on       | bool | A M S   | Matrix destination per manual; off freezes output at current value. |
| Transport  | off / on      | off      | bool | A S     | Locks phase to song position. |
| Rate       | 0–50 Hz       | 0.10 Hz  | exp  | A M PN  | |
| Rate Tempo | off / on      | off      | bool | A S     | |
| Rate Div   | 31 divisions  | 1/4      | enum | A S     | |
| Phase      | 0–360°        | 0°       | lin  | A M PN  | Also the reset/trig landing phase. |
| St.Phase   | 0–100 %       | 0 %      | lin  | A M     | Phase offset between the L and R modulation channels. |
| Lag        | 0–100 %       | 0 %      | lin  | A M     | Slew limiter on the output. |
| Trig       | momentary     | —        | bool | A PN S  | Edge-triggered reset to Phase. Fired by note-on when the engine's Gate Sync is on. |

## 5. LFOs ×6 (global)

Identical parameter block to §4 with two differences: **Rate default is
1.00 Hz**, and the six LFOs are global — a single instance each, shared by
all voices (this preserves the "whole texture breathing together" character
and keeps CPU sane). Torsion's 1000 fixed waveforms are replaced by the
drawable curve + visual preset bank.

## 6. Drift ×3 / Noise ×3

| Param            | Range   | Default | Map | Flags | Notes |
|------------------|---------|---------|-----|-------|-------|
| Drift n St.Phase | 0–100 % | 0 %     | lin | A M   | Crossfades between two independent drift cores feeding L/R. 0 % mono, 100 % fully decorrelated stereo. |
| Noise n St.Phase | 0–100 % | 0 %     | lin | A M   | Same crossfade for the noise pair. |

Drift = spring-mass physical model with random accelerations (very slow,
organic motion); internal rate fixed, as in Torsion. Noise = uniform random
-1..+1 with no smoothing, re-rolled at every grain snapshot — the per-grain
variation source.

## 7. Sample slots ×6

| Param      | Range               | Default | Map | Flags | Notes |
|------------|---------------------|---------|-----|-------|-------|
| Root Note  | C-2–G8 (MIDI 0–127) | C3 (60) | int | S     | Reference pitch for Key tracking. |
| Loop Start | 0–100 %             | 0 %     | lin | A     | Set by click-drag on the waveform: drag ←→ moves the window, ↑↓ grows/shrinks it (Torsion gestures). |
| Loop End   | 0–100 %             | 100 %   | lin | A     | `start < end` enforced; window length capped at 100 s. |
| Gain       | -12–+12 dB          | 0 dB    | lin | A M   | **[new]** Per-slot trim for level-matching sources. |

**Constraints & handling.** Max 300 s per slot; working loop window max 100 s
(your spec). Import: drag-and-drop from the host DAW or a file manager, plus a
file dialog; a multi-file drop fills slots from the selected one onward
(Torsion behavior — select slot 1 first to fill all six). Decode via
`symphonia` (wav/flac/aiff/ogg/vorbis/mp3/adpcm) to f32, resampled to the
host rate by a built-in arbitrary-ratio windowed sinc (kernel stretched
when decimating, so no aliasing on downsampling). Memory honesty: 300 s stereo f32 @ 48 kHz ≈ 115 MB per slot,
~690 MB absolute worst case — fine on a modern machine but worth a note in
the README; mmap/lazy paging is a possible v2.

**State strategy.** Plugin state stores file path + BLAKE3 content hash +
root/loop metadata; samples are *referenced* by default with a relink dialog
for missing files, plus a per-preset **Embed samples** toggle that packs
FLAC-compressed audio into the state blob for portable patches. Shift-drag
zoom view (per slot) is saved as cosmetic state, never a param.

## 8. Mod matrix — 32 slots

Per slot (all six columns are host params; amounts carry M for host
modulation of modulation depth):

| Column | Range / values        | Default | Flags |
|--------|-----------------------|---------|-------|
| Source | source enum           | None    | A S   |
| Amt 1  | -100–+100             | 0       | A M   |
| Dest 1 | destination enum      | None    | A S   |
| Amt 2  | -100–+100             | 0       | A M   |
| Dest 2 | destination enum      | None    | A S   |
| Scale  | source enum           | None    | A S   |

**Sources** (enum order): None · LFO 1–6 · Loop LFO 1–3 · Drift 1–3 ·
Noise 1–3 · Macro 1–8 · Const -200 / -100 / +100 / +200 · Env Follower
(output audio) · Velocity · Note · Note Oct · Note Gate · Pressure **[new]** ·
Voice Random **[new]**.

*Voice Random* is a uniform -100..+100 value rolled once per note-on — a
per-voice constant for detune, position spray, per-note character. *Pressure*
maps from the CLAP pressure note expression / channel aftertouch.

**Destinations**: None · LFO 1–6 {Gate, Phase, Rate, Lag} · Loop LFO 1–3
{Gate, Phase, Rate, Lag} · Grain 1–3 {Rate, Length, Jitter, Gate, Loop Size,
Loop Position, Position, Pitch, Direction, Shape, Skew, Level, Pan, Sample}.

*Loop Position* shifts the slot's loop window itself; *Position* is the grain
start offset inside it. Both kept, per the manual's destination table.

**Scale semantics** (Torsion): the row's contribution is multiplied by the
instantaneous value of the Scale source; None = ×1.

**Evaluation.** Continuous destinations update on a 64-sample control tick;
grain-snapshot destinations read instantaneous values (including a fresh
Noise roll) at emission time. Rows whose source or destination is voice-local
evaluate per voice; purely global rows evaluate once per tick.

**UI.** 8 rows per page × 4 pages. Ctrl-click a cell to clear/zero it
(Torsion gesture). Right-click a destination cell offers "pick by dragging a
control" — the drag-to-assign flow from the manual.

## 9. Curves (EDIT.CURVES screen)

Nine curve objects: Loop 1–3, LFO 1–6. Each is `f32[256]` in -1..+1, linear
interpolation with optional smoothing pass on edit. The EDIT.CURVES button
swaps the main view for a 3×3 grid of editors. Editing: freehand draw, line
tool, step quantize (2–64), smooth, invert, mirror, phase shift; plus a
preset bank of ~64 shapes rendered as thumbnails only, no names (your spec) —
sine/tri/saw/square families, steps, random walks, chaos maps, gate patterns.
Curves live in plugin state, not host params; hosts automate Rate/Phase
instead.

## 10. Voice architecture & CLAP integration

**Per voice:** 3 grain emitters, 3 Loop LFO phase instances, voice-local
sources (Velocity, Note, Note Oct, Note Gate, Pressure, Voice Random), and
any per-note param offsets the host sends. **Global:** LFO 1–6, Drift, Noise,
Env Follower, Macro base values, all base parameter values.

**Lifecycle.** Note-on allocates a voice (steal oldest-then-quietest beyond
Voices). Note-off stops emission; sounding grains ring out naturally (tail ≤
the longest live grain), then `CLAP_EVENT_NOTE_END` is sent so the host can
recycle the note ID. With Key at 0 % and engine Gates on, Mangle free-runs as
a Torsion-style drone box — notes optional.

**Extensions:** `audio-ports` (1 stereo out) · `note-ports` (CLAP dialect,
MIDI fallback) · `params` · `state` · `gui` (X11 first, then Win32, Cocoa
last) · `voice-info` (reports Voices) · `timer-support` · `latency` (0 —
Squeeze must stay zero-lookahead or report honestly) · `render`.

**Note expressions:** tuning → adds into the engine Pitch path (post-Key) ·
volume → voice gain · pan → voice output pan · pressure → Pressure source.

**Threading.** Audio thread is allocation- and lock-free. Sample loads and
curve edits happen on the main thread and are handed to audio via arc-swap /
triple buffer.

## 11. Parameter IDs & count

Stable `u32`: `id = (module << 16) | (instance << 8) | index`

| Module | Code | Instances | Params each |
|--------|------|-----------|-------------|
| Global   | 0x00 | 1  | 6  |
| Macro    | 0x01 | 8  | 1  |
| Grain    | 0x02 | 3  | 21 |
| Loop LFO | 0x03 | 3  | 9  |
| LFO      | 0x04 | 6  | 9  |
| Drift    | 0x05 | 3  | 1  |
| Noise    | 0x06 | 3  | 1  |
| Slot     | 0x07 | 6  | 4  |
| Matrix   | 0x08 | 32 (rows) | 6 (columns) |

Total: 6 + 8 + 63 + 27 + 54 + 3 + 3 + 24 + 192 = **380 params**. IDs are
never reused or renumbered; new params append within their module.

## 12. Init patch

Grain 1 gated on at 10 Hz, Rate>Len on, Length 100 %, Loop on over the full
sample, ramp Loop curve at 0.10 Hz — load a sample into slot 1, hold a key,
and you get clean, gapless, key-tracked loop playback. Engines 2 and 3 are
configured identically but gated off. LFOs sine at 1 Hz, matrix empty,
Key 100 %, Voices 8, Squeeze on. Deliberately vanilla: the blank canvas that
RANDOM mutates from.

## 13. Build order (proposal)

1. Scaffold `nih-plug` workspace: `mangle-dsp` (no_std-ish core) + `mangle`
   (plugin + GUI) crates.
2. DSP core in flow-chart order: clock → jitter → snapshot → sample fetch
   (pitch/direction) → envelope (shape/skew) → pan/level → per-channel sum.
3. Param plumbing generated from this document's tables (codegen or macro).
4. Sample loader + slot state, drag-and-drop.
5. Mod matrix + sources (LFO/curve engine, drift, noise, env follower).
6. GUI per the locked mockup; waveform view with loop gestures + grain
   overlay modes (per-engine / per-channel / per-sample).
7. EDIT.CURVES screen.
8. RANDOM generator + pools + TOOLS menu (reset, copy grain).
9. Squeeze leveler; final gain staging.
10. Preset format (+ embed-samples) and factory bank.
