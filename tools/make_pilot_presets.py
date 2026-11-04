#!/usr/bin/env python3
"""Writes the pilot factory presets into factory/presets/ (see FACTORY.md).

Values are normalised 0..1 host values. Usage: python3 tools/make_pilot_presets.py
"""
import os, re

OUT = os.path.join(os.path.dirname(__file__), "..", "factory", "presets")
SRC = os.path.join(os.path.dirname(__file__), "..", "factory", "sources.tsv")

def pid(module, inst, idx):
    return f"{(module << 16) | (inst << 8) | idx:06x}"

GLOBAL, GRAIN, LOOPLFO, SLOT, MATRIX, FX = 0, 2, 3, 7, 8, 9
roots = {}
for line in open(SRC).read().splitlines()[1:]:
    c = line.split("\t")
    roots[int(c[0])] = int(c[5])

def cube(v, mx):  # display -> normalised for Map::Cube
    return (v / mx) ** (1 / 3)

def grain(e, **kw):
    names = dict(gate=0, rate=1, tempo=2, div=3, jitter=4, rate_len=5, length=6,
                 pan=7, level=8, sample=9, position=10, loop=11, loop_size=12,
                 pitch=14, direction=16, shape=17, skew=18, algo=19, mode=20)
    return [(pid(GRAIN, e, names[k]), v) for k, v in kw.items()]

MODE = {"grains": 0, "stretch": 1 / 3, "cloud": 2 / 3, "spectral": 1.0}
ALGO = {"hermite": 2 / 3, "sinc": 1.0}
def hz(v): return cube(v, 500.0)
def length(pct): return cube(pct, 1000.0)
def semis(st): return (st + 12) / 24
def div(i): return i / 30
NEUTRAL_PITCH = 0.5

def preset(name, cat, src, p=None, key=None, level=None, stereo=None, extra=None):
    lines = ["mangle-preset 1", f"name {name}", f"category {cat}", "author Mangle"]
    slots = src if isinstance(src, list) else [src]
    for i, s in enumerate(slots):
        lines.append(f"slot {i} {s:04d}")
        if roots.get(s, -1) >= 0:
            lines.append(f"p {pid(SLOT, i, 0)} {roots[s] / 127:.4f}")
    if key is not None: lines.append(f"p {pid(GLOBAL, 0, 1)} {key:.4f}")
    if stereo is not None: lines.append(f"p {pid(GLOBAL, 0, 2)} {stereo:.4f}")
    if level is not None: lines.append(f"p {pid(GLOBAL, 0, 4)} {level:.4f}")
    for k, v in (p or []) + (extra or []):
        lines.append(f"p {k} {v:.4f}")
    slug = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")
    return slug, "\n".join(lines) + "\n"

def row0(src, dest, amt):  # matrix row 1: one source -> one destination
    return [(pid(MATRIX, 0, 0), src / 34), (pid(MATRIX, 0, 1), 0.5 + amt / 2),
            (pid(MATRIX, 0, 2), dest / 80)]

P = []
# Pads
P.append(preset("Glass Choir", "Pads", 500, stereo=0.62, p=grain(0, rate=hz(25), jitter=0.30,
    length=length(400), position=0.20, mode=MODE["cloud"], algo=ALGO["hermite"])))
P.append(preset("Slow Strings", "Pads", 509, stereo=0.60, p=grain(0, rate=hz(12), jitter=0.15,
    length=length(300), mode=MODE["stretch"], algo=ALGO["hermite"])))
# Textures
P.append(preset("Dust Harp", "Textures", 502, p=grain(0, rate=hz(40), jitter=0.70, length=length(60),
    position=0.35, pan=0.30, algo=ALGO["hermite"])))
P.append(preset("Rain Gong", "Textures", 515, stereo=0.70, p=grain(0, rate=hz(80), jitter=0.90,
    length=length(35), position=0.10, pan=0.50)))
# Drones (Key 0% = free-run drone mode)
P.append(preset("Didge Cloud", "Drones", 505, key=0.0, stereo=0.65, p=grain(0, rate=hz(18),
    jitter=0.25, length=length(500), mode=MODE["stretch"])))
P.append(preset("Sub Fog", "Drones", 16, key=0.0, p=grain(0, rate=hz(30), jitter=0.40,
    length=length(350), mode=MODE["cloud"], pitch=semis(-12))))
# Plucks
P.append(preset("Nylon Spray", "Plucks", 1, p=grain(0, rate=hz(6), jitter=0.10, length=length(150),
    algo=ALGO["sinc"])))
P.append(preset("Kalimba Sparks", "Plucks", 501, stereo=0.60, p=grain(0, rate=hz(14), jitter=0.35,
    length=length(90), pan=0.20, algo=ALGO["hermite"])))
# Keys
P.append(preset("Organ Grain", "Keys", 508, p=grain(0, rate=hz(60), jitter=0.05, length=length(200),
    algo=ALGO["hermite"])))
P.append(preset("FM Glass Keys", "Keys", [506, 3], p=grain(0, rate=hz(22), jitter=0.12,
    length=length(180), sample=0.0) + grain(1, gate=1.0, rate=hz(22), jitter=0.12, length=length(180),
    sample=0.2, pitch=semis(12), level=0.45)))
# Rhythmic (tempo-synced)
P.append(preset("Marimba Chop", "Rhythmic", 513, p=grain(0, tempo=1.0, div=div(25), jitter=0.0,
    length=length(40), position=0.0, rate_len=0.0)))
P.append(preset("Pulse Reed", "Rhythmic", 503, p=grain(0, tempo=1.0, div=div(23), jitter=0.05,
    length=length(60), rate_len=0.0, shape=0.30)))
# Basses
P.append(preset("Sub Grain", "Basses", 16, p=grain(0, rate=hz(45), jitter=0.02, length=length(250),
    pitch=semis(-12))))
P.append(preset("Tenor Growl", "Basses", 504, p=grain(0, rate=hz(70), jitter=0.10, length=length(120),
    pitch=semis(-12), algo=ALGO["hermite"])))
# FX
P.append(preset("Spectral Gong", "FX", 515, stereo=0.70, p=grain(0, mode=MODE["spectral"],
    rate=hz(10), length=length(300))))
# Stereo flip glide: Loop LFO 1 sweeps Grain 1 Pan between the sample and its mirror image.
P.append(preset("Flip Glide", "FX", 500, stereo=0.70, p=grain(0, rate=hz(30), jitter=0.10,
    length=length(300), mode=MODE["cloud"]) + [(pid(LOOPLFO, 0, 2), 0.20)] + row0(7, 49, 1.0)))

os.makedirs(OUT, exist_ok=True)
for f in os.listdir(OUT):
    if f.endswith(".mpreset"): os.remove(os.path.join(OUT, f))
for i, (slug, text) in enumerate(P, 1):
    open(os.path.join(OUT, f"{i:03d}-{slug}.mpreset"), "w").write(text)
print(f"wrote {len(P)} presets")
