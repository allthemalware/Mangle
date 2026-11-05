# Mangle

**Good evening, and welcome.**

This is a sound. A perfectly ordinary sound.

Mangle takes it apart.

Mangle is a free plugin for your music software. You give it a recording, and it plays the thing back in tiny pieces, called grains, and lets you decide what becomes of each one. Clouds, drones, frozen moments, shimmering textures, things that were never quite a cello. Marvellous.

> **Alpha release.** Mangle is new and still being tested. It works, but it may have rough edges. If something misbehaves, [tell us](https://github.com/allthemalware/Mangle/issues). That's rather the point of this release.

---

## What you'll need

- Music software that supports **CLAP** plugins. Bitwig and Reaper (version 7 or later) both do.
- Windows, macOS or Linux. Linux is the best tested. Windows and macOS builds are available but have had less attention, so reports are very welcome.

## Getting it

1. Go to the **[latest release](https://github.com/allthemalware/Mangle/releases/latest)**.
2. Download the file for your computer:
   - `mangle-windows-x86_64.zip` for Windows
   - `mangle-macos-arm64.zip` for a Mac with an Apple chip (M1 and newer)
   - `mangle-macos-x86_64.zip` for an older, Intel Mac
   - `mangle-linux-x86_64.zip` for Linux
3. Unzip it and run the installer inside: `install-prebuilt.ps1` on Windows, `install-prebuilt.sh` on Mac and Linux. It puts Mangle where your music software can find it.
4. Open your music software and rescan your plugins.

**On a Mac?** Mangle isn't signed yet, so macOS may be suspicious of it. The installer deals with this. If it still complains, right-click the plugin and choose Open.

## Your first sound

1. Add **Mangle** to an instrument track and play a note. You'll hear a simple tone, so you know it's alive.
2. Click **LOAD** and pick any audio file. Or drag one in.
3. Play again. That's your sound, in pieces.
4. Press **⚄ RND** to roll a new patch. If it goes badly, **UNDO RND** puts everything back where it was. Go on. Have a go.

## Sounds to play with

The installer already gives you 256 ready-made presets. For the sounds behind them, download **`mangle-factory-bank.zip`** from the same release page. It holds over a thousand sounds: strings, winds, bells, voices, and things being hit, mostly by professionals.

Unzip it and put the `factory` folder inside Mangle's folder, replacing the one that's there:

| System | Mangle's folder |
|--------|-----------------|
| Windows | `%APPDATA%\mangle` |
| macOS | `~/Library/Application Support/mangle` |
| Linux | `~/.local/share/mangle` |

Then open the **BROWSER** tab.

## What it does

- **Six sample slots.** Bring your own sounds, or use ours.
- **Three grain engines.** Each reads a sound in its own way: Grains, Stretch, Cloud, or Spectral, for the more adventurous.
- **Freeze.** In Spectral mode, set TIME to zero and the moment holds. It will wait.
- **Motion.** Draw a curve, then tell it what to move. Anything can move anything.
- **Scale lock.** Should the pitches wander, lock them to a scale. Pentatonic is very popular with beginners.
- **Random, with padlocks.** Keep the parts you like. Roll the rest.

## Something wrong?

[Open an issue](https://github.com/allthemalware/Mangle/issues) and tell us which music software you use, what your computer is, and what happened. A short clip of the sound helps enormously.

## The small print

Mangle is free software under the **GPLv3** licence. If you'd like to build it yourself or help improve it, see [DEVELOPING.md](DEVELOPING.md).

If you enjoy it, you can [support its making on Ko-fi](https://ko-fi.com/T5F122USRI).

*Never leave Mangle unattended with a cello.*
