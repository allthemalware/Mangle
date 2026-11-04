# Factory bank — data formats

Shipped next to the plugin as a folder (`factory/`), never compiled in.

```
factory/
  sources.tsv              index of source sounds
  sources/<id>.flac|.wav   audio, mono unless the stereo image matters
  presets/<nnn>-<slug>.mpreset
```

## sources.tsv
UTF-8, tab separated, first line is the header
`id	name	category	kind	file	root	license	origin`.
- `id`: 4-digit decimal, unique, stable forever (`0001`).
- `kind`: `gen` (synthesised by `cargo xtask gen-sources`) or `rec` (a recording).
- `file`: path relative to `factory/`.
- `root`: MIDI root note 0..127, or `-1` for unpitched.
- `license`: SPDX-style, e.g. `CC0-1.0`, or `generated`.
- `origin`: where it came from (library + original file name, or the generator and its parameters).
Blank lines and lines starting with `#` are ignored.

## .mpreset
Line based text, `#` comments allowed:
```
mangle-preset 1
name Glass Choir
category Pads
author Mangle
slot 0 0042
p 010003 0.7500
```
- `category` is one of `dsp::factory::CATEGORIES`.
- `slot <0-5> <source id>` loads that source into the slot. Unlisted slots are cleared.
- `p <6 hex digits> <normalised 0..1>` is a parameter id (the `p{:06x}` host id) and its normalised value. Unlisted params take their defaults.

## In saved state
A slot reference to a factory source has the path `factory:<id>` (e.g. `factory:0042`); the shell resolves it against the data folder, so a project does not depend on where the folder lives.
State v3 (append-only) carries the persisted pre-RND patch, the A/B compare patch, and the last factory preset id; see `state.rs`.

## Data folder lookup (shell)
`$MANGLE_DATA` if set and containing `factory/`, else `$XDG_DATA_HOME/mangle` (default `~/.local/share/mangle`), else `<dir of the plugin binary>/mangle`.
