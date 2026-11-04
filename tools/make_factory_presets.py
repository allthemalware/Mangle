#!/usr/bin/env python3
"""Writes the 256 factory presets (8 categories x 4 archetypes x 8 variants) into factory/presets/.

Deterministic: sources are chosen from factory/sources.tsv by category suitability, parameters by
a per-preset seeded RNG. Values are normalised 0..1 host values (see FACTORY.md).
usage: python3 tools/make_factory_presets.py
"""
import os, random, re

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
OUT = os.path.join(ROOT, "factory", "presets")
GLOBAL, GRAIN, LOOPLFO, SLOT, MATRIX, FX = 0, 2, 3, 7, 8, 9

def pid(m, inst, idx): return f"{(m << 16) | (inst << 8) | idx:06x}"
def cube(v, mx): return (v / mx) ** (1 / 3)
def hz(v): return cube(v, 500.0)
def length(pct): return cube(pct, 1000.0)
def semis(st): return (st + 12) / 24
def div(i): return i / 30
MODE = {"grains": 0.0, "stretch": 1 / 3, "cloud": 2 / 3, "spectral": 1.0}
ALGO = {"linear": 1 / 3, "hermite": 2 / 3, "sinc": 1.0}
GP = dict(gate=0, rate=1, tempo=2, div=3, jitter=4, rate_len=5, length=6, pan=7, level=8, sample=9,
          position=10, loop=11, loop_size=12, pitch=14, pos_pitch=15, direction=16, shape=17,
          skew=18, algo=19, mode=20, quant=21)
def grain(e, **kw): return [(pid(GRAIN, e, GP[k]), v) for k, v in kw.items()]
# matrix source / destination indices (matrix.rs): Loop n = 6+n, Drift n = 9+n, Noise n = 12+n
LOOP, DRIFT, NOISE = (lambda n: 6 + n), (lambda n: 9 + n), (lambda n: 12 + n)
def dest(e, name):
    off = dict(rate=0, length=1, jitter=2, gate=3, loop_size=4, loop_pos=5, position=6, pitch=7,
               direction=8, shape=9, skew=10, level=11, pan=12, sample=13)[name]
    return 37 + 14 * e + off
FLT_CUTOFF = 79
def matrix(rows):
    out = []
    for r, (src, d1, a1, d2, a2) in enumerate(rows):
        out += [(pid(MATRIX, r, 0), src / 34), (pid(MATRIX, r, 1), 0.5 + a1 / 2), (pid(MATRIX, r, 2), d1 / 80)]
        if d2:
            out += [(pid(MATRIX, r, 3), 0.5 + a2 / 2), (pid(MATRIX, r, 4), d2 / 80)]
    return out
def loop_lfo(e, rate): return [(pid(LOOPLFO, e, 2), rate)]
def lowpass(cut, reso=0.12): return [(pid(FX, 0, 0), 1.0), (pid(FX, 0, 2), cut), (pid(FX, 0, 3), reso)]
def U(r, a, b): return r.uniform(a, b)

# ---- sources -------------------------------------------------------------------------------
SRC = []
for line in open(os.path.join(ROOT, "factory", "sources.tsv")).read().splitlines()[1:]:
    c = line.split("\t")
    SRC.append(dict(id=int(c[0]), name=c[1], cat=c[2], kind=c[3], root=int(c[5])))

SUIT = {
    "Pads": ["pads", "voice", "strings", "winds", "brass", "keys", "synth", "drones"],
    "Textures": ["noise", "metal", "bells", "percussion", "misc", "strings", "plucked", "glass", "bars"],
    "Drones": ["drones", "pads", "strings", "winds", "brass", "voice", "metal", "keys"],
    "Plucks": ["plucked", "bars", "bells", "percussion", "keys", "strings", "synth"],
    "Keys": ["keys", "bars", "bells", "synth", "plucked", "pads"],
    "Rhythmic": ["drums", "percussion", "plucked", "bars", "metal", "bells"],
    "Basses": ["drones", "brass", "strings", "synth", "winds", "keys", "pads", "plucked"],
    "FX": ["noise", "metal", "bells", "voice", "drums", "misc", "glass", "synth", "percussion", "pads"],
}
NEEDS_PITCH = {"Plucks", "Keys", "Basses"}

def pick_sources(cat, n, rng, used):
    pool = [s for s in SRC if s["cat"] in SUIT[cat] and s["id"] not in used
            and (cat not in NEEDS_PITCH or s["root"] >= 0)]
    if cat == "Basses":
        pool.sort(key=lambda s: (s["root"] > 55, s["root"]))
        pool = pool[: max(n * 3, 60)]
    if cat == "Rhythmic":
        pool = [s for s in pool if s["cat"] in ("drums", "percussion")] + [s for s in pool if s["cat"] not in ("drums", "percussion")][:20]
    rng.shuffle(pool)
    if len(pool) < n:  # fall back to anything suitable, ignoring reuse
        pool = [s for s in SRC if s["cat"] in SUIT[cat]]
        rng.shuffle(pool)
    return pool[:n]

# ---- archetypes: each returns (extra_sources, params) --------------------------------------
def pads_cloud(r, v):
    return [], (grain(0, rate=hz(U(r, 18, 34)), jitter=U(r, .2, .4), length=length(U(r, 300, 500)),
                      position=U(r, 0, .4), mode=MODE["cloud"], algo=ALGO["hermite"])
                + loop_lfo(0, U(r, .08, .2)) + matrix([(DRIFT(1), dest(0, "pan"), U(r, .2, .5), 0, 0)]), dict(stereo=U(r, .58, .72)))
def pads_stretch(r, v):
    return [], (grain(0, rate=hz(U(r, 9, 16)), jitter=U(r, .1, .25), length=length(U(r, 250, 420)),
                      mode=MODE["stretch"], algo=ALGO["hermite"]) + matrix([(DRIFT(2), dest(0, "position"), U(r, .03, .1), 0, 0)]),
                dict(stereo=U(r, .55, .68)))
def pads_octave(r, v):
    return [1], (grain(0, rate=hz(U(r, 20, 30)), jitter=U(r, .15, .3), length=length(U(r, 320, 450)), mode=MODE["cloud"])
                 + grain(1, gate=1.0, rate=hz(U(r, 20, 30)), jitter=U(r, .15, .3), length=length(U(r, 320, 450)),
                         mode=MODE["cloud"], sample=0.2, pitch=semis(12), level=U(r, .3, .5)), dict(stereo=U(r, .6, .72)))
def pads_warm(r, v):
    return [], (grain(0, rate=hz(U(r, 22, 36)), jitter=U(r, .1, .25), length=length(U(r, 350, 520)), mode=MODE["cloud"],
                      algo=ALGO["hermite"]) + lowpass(U(r, .5, .68), U(r, .08, .2)) + matrix([(LOOP(1), FLT_CUTOFF, U(r, .08, .2), 0, 0)])
                + loop_lfo(0, U(r, .06, .15)), dict(stereo=U(r, .58, .7)))

def tex_dust(r, v):
    return [], (grain(0, rate=hz(U(r, 40, 90)), jitter=U(r, .6, .9), length=length(U(r, 30, 70)), pan=U(r, .3, .6),
                      position=U(r, 0, .5)) + matrix([(NOISE(1), dest(0, "position"), U(r, .15, .4), 0, 0)]), dict(stereo=U(r, .6, .75)))
def tex_swarm(r, v):
    return [], (grain(0, rate=hz(U(r, 120, 250)), jitter=U(r, .7, .95), length=length(U(r, 18, 40)), pan=U(r, .4, .7))
                + matrix([(DRIFT(1), dest(0, "pitch"), U(r, .04, .1), dest(0, "pan"), U(r, .3, .6))]), dict(stereo=U(r, .6, .78)))
def tex_shimmer(r, v):
    return [1], (grain(0, rate=hz(U(r, 25, 45)), jitter=U(r, .4, .6), length=length(U(r, 120, 220)), mode=MODE["cloud"])
                 + grain(1, gate=1.0, rate=hz(U(r, 25, 45)), jitter=U(r, .4, .6), length=length(U(r, 120, 220)), mode=MODE["cloud"],
                         sample=0.2, pitch=semis(r.choice([7, 12])), level=U(r, .35, .6)), dict(stereo=U(r, .65, .8)))
def tex_scanner(r, v):
    return [], (grain(0, rate=hz(U(r, 45, 80)), jitter=U(r, .1, .3), length=length(U(r, 60, 110)), position=U(r, 0, .3))
                + loop_lfo(0, U(r, .3, .55)) + matrix([(LOOP(1), dest(0, "pan"), U(r, .3, .7), 0, 0)]), dict(stereo=U(r, .6, .75)))

def drone_fog(r, v):
    return [], (grain(0, rate=hz(U(r, 14, 26)), jitter=U(r, .2, .35), length=length(U(r, 400, 650)), mode=MODE["stretch"]) 
                + matrix([(DRIFT(1), dest(0, "pan"), U(r, .2, .5), 0, 0)]), dict(key=0.0, stereo=U(r, .6, .72)))
def drone_bloom(r, v):
    return [], (grain(0, rate=hz(U(r, 24, 40)), jitter=U(r, .3, .5), length=length(U(r, 300, 500)), mode=MODE["cloud"])
                + loop_lfo(0, U(r, .04, .1)) + matrix([(LOOP(1), dest(0, "position"), U(r, .1, .3), 0, 0)]), dict(key=0.0, stereo=U(r, .62, .75)))
def drone_frozen(r, v):
    return [], (grain(0, rate=hz(U(r, 5, 9)), length=length(U(r, 200, 350)), mode=MODE["spectral"], position=U(r, 0, .8)),
                dict(key=0.0, stereo=U(r, .55, .7)))
def drone_twin(r, v):
    iv = r.choice([-12, 7, 12, -5])
    return [1], (grain(0, rate=hz(U(r, 14, 22)), jitter=U(r, .15, .3), length=length(U(r, 380, 560)), mode=MODE["stretch"])
                 + grain(1, gate=1.0, rate=hz(U(r, 14, 22)), jitter=U(r, .15, .3), length=length(U(r, 380, 560)), mode=MODE["stretch"],
                         sample=0.2, pitch=semis(iv), level=U(r, .4, .7)), dict(key=0.0, stereo=U(r, .6, .75)))

def pluck_tight(r, v):
    return [], (grain(0, rate=hz(U(r, 4, 8)), jitter=U(r, 0, .08), length=length(U(r, 100, 200)), algo=ALGO["sinc"]), {})
def pluck_spray(r, v):
    return [], (grain(0, rate=hz(U(r, 12, 20)), jitter=U(r, .25, .4), length=length(U(r, 60, 120)), pan=U(r, .1, .3),
                      algo=ALGO["hermite"]), dict(stereo=U(r, .55, .66)))
def pluck_echo(r, v):
    return [], (grain(0, tempo=1.0, div=div(r.choice([23, 25, 24])), jitter=U(r, 0, .05), length=length(U(r, 120, 200)),
                      rate_len=0.0, algo=ALGO["hermite"]), dict(stereo=U(r, .55, .66)))
def pluck_soft(r, v):
    return [], (grain(0, rate=hz(U(r, 2.5, 4.5)), jitter=U(r, 0, .05), length=length(U(r, 280, 420)), algo=ALGO["sinc"], shape=U(r, .2, .5))
                + lowpass(U(r, .7, .85), .05), {})

def keys_glass(r, v):
    return [1], (grain(0, rate=hz(U(r, 20, 30)), jitter=U(r, .05, .15), length=length(U(r, 150, 250)), algo=ALGO["hermite"])
                 + grain(1, gate=1.0, rate=hz(U(r, 20, 30)), jitter=U(r, .05, .15), length=length(U(r, 150, 250)), sample=0.2,
                         pitch=semis(12), level=U(r, .2, .4)), dict(stereo=U(r, .55, .66)))
def keys_warm(r, v):
    return [], (grain(0, rate=hz(U(r, 22, 34)), jitter=U(r, .03, .1), length=length(U(r, 180, 300)), algo=ALGO["hermite"])
                + lowpass(U(r, .55, .72), U(r, .06, .16)), dict(stereo=U(r, .5, .6)))
def keys_wide(r, v):
    return [], (grain(0, rate=hz(U(r, 24, 38)), jitter=U(r, .08, .2), length=length(U(r, 160, 260)), pan=U(r, .1, .25), algo=ALGO["hermite"])
                + matrix([(DRIFT(1), dest(0, "pan"), U(r, .3, .6), 0, 0)]), dict(stereo=U(r, .72, .88)))
def keys_chorus(r, v):
    d = U(r, .08, .25)
    return [1], (grain(0, rate=hz(U(r, 22, 32)), jitter=U(r, .05, .12), length=length(U(r, 170, 260)), pitch=semis(-d), algo=ALGO["hermite"])
                 + grain(1, gate=1.0, rate=hz(U(r, 22, 32)), jitter=U(r, .05, .12), length=length(U(r, 170, 260)), sample=0.0,
                         pitch=semis(d), algo=ALGO["hermite"], level=U(r, .7, 1.0)), dict(stereo=U(r, .6, .74)))

def rhy_pulse(r, v):
    return [], (grain(0, tempo=1.0, div=div(r.choice([23, 25, 20])), jitter=U(r, 0, .04), length=length(U(r, 35, 80)), rate_len=0.0,
                      shape=U(r, 0, .3), position=U(r, 0, .5)), {})
def rhy_chop(r, v):
    return [], (grain(0, tempo=1.0, div=div(r.choice([25, 27, 26])), jitter=U(r, 0, .1), length=length(U(r, 25, 55)), rate_len=0.0,
                      shape=U(r, .1, .4), skew=U(r, .3, .5)) + matrix([(LOOP(1), dest(0, "position"), U(r, .1, .4), 0, 0)]) + loop_lfo(0, U(r, .15, .3)), dict(stereo=U(r, .55, .68)))
def rhy_stutter(r, v):
    return [], (grain(0, tempo=1.0, div=div(r.choice([25, 23])), length=length(U(r, 30, 60)), rate_len=0.0, pan=U(r, 0, .3), direction=r.choice([1.0, 1.0, 0.0])), {})
def rhy_triplet(r, v):
    return [], (grain(0, tempo=1.0, div=div(r.choice([24, 22, 26])), jitter=U(r, 0, .05), length=length(U(r, 45, 90)), rate_len=0.0,
                      pitch=semis(r.choice([0, 0, -12, 7, 12]))), dict(stereo=U(r, .55, .66)))

def bass_sub(r, v):
    return [], (grain(0, rate=hz(U(r, 40, 60)), jitter=U(r, 0, .05), length=length(U(r, 220, 300)), pitch=semis(-12)) + lowpass(U(r, .42, .55), .1), {})
def bass_growl(r, v):
    return [], (grain(0, rate=hz(U(r, 65, 95)), jitter=U(r, .12, .25), length=length(U(r, 90, 150)), pitch=semis(-12), algo=ALGO["hermite"])
                + matrix([(DRIFT(1), dest(0, "position"), U(r, .05, .15), 0, 0)]), {})
def bass_reese(r, v):
    d = U(r, .12, .3)
    return [1], (grain(0, rate=hz(U(r, 50, 70)), jitter=U(r, .02, .08), length=length(U(r, 200, 300)), pitch=semis(-12 - d))
                 + grain(1, gate=1.0, rate=hz(U(r, 50, 70)), jitter=U(r, .02, .08), length=length(U(r, 200, 300)), sample=0.0,
                         pitch=semis(-12 + d)) + lowpass(U(r, .5, .62), .12), dict(stereo=U(r, .55, .66)))
def bass_pluck(r, v):
    return [], (grain(0, rate=hz(U(r, 5, 8)), jitter=U(r, 0, .04), length=length(U(r, 100, 150)), pitch=semis(-12), algo=ALGO["sinc"])
                + lowpass(U(r, .45, .58), .14), {})

def fx_spectral(r, v):
    return [], (grain(0, mode=MODE["spectral"], rate=hz(U(r, 6, 14)), length=length(U(r, 220, 380)), position=U(r, 0, .9))
                + matrix([(LOOP(1), dest(0, "position"), U(r, .1, .3), 0, 0)]) + loop_lfo(0, U(r, .05, .15)), dict(stereo=U(r, .65, .8)))
def fx_flip(r, v):
    return [], (grain(0, rate=hz(U(r, 22, 40)), jitter=U(r, .08, .2), length=length(U(r, 220, 380)), mode=MODE["cloud"])
                + loop_lfo(0, U(r, .12, .4)) + matrix([(LOOP(1), dest(0, "pan"), 1.0, 0, 0)]), dict(stereo=U(r, .65, .78)))
def fx_rain(r, v):
    return [], (grain(0, rate=hz(U(r, 15, 35)), jitter=U(r, .4, .7), length=length(U(r, 60, 140)), pos_pitch=1.0, quant=r.choice([4 / 7, 5 / 7, 6 / 7]))
                + matrix([(NOISE(1), dest(0, "pitch"), U(r, .3, .6), 0, 0)]), dict(stereo=U(r, .6, .75)))
def fx_chaos(r, v):
    return [], (grain(0, rate=hz(U(r, 30, 80)), jitter=U(r, .7, 1.0), length=length(U(r, 20, 90)), pan=U(r, .3, .7))
                + matrix([(NOISE(1), dest(0, "rate"), U(r, .15, .4), dest(0, "pitch"), U(r, .1, .3)),
                          (NOISE(2), dest(0, "length"), U(r, .2, .5), 0, 0)]), dict(stereo=U(r, .65, .8)))

PLAN = {
    "Pads": [("Cloud Pad", pads_cloud), ("Stretch Pad", pads_stretch), ("Octave Pad", pads_octave), ("Warm Pad", pads_warm)],
    "Textures": [("Dust", tex_dust), ("Swarm", tex_swarm), ("Shimmer", tex_shimmer), ("Scanner", tex_scanner)],
    "Drones": [("Fog", drone_fog), ("Bloom", drone_bloom), ("Frozen", drone_frozen), ("Twin", drone_twin)],
    "Plucks": [("Tight Pluck", pluck_tight), ("Spray Pluck", pluck_spray), ("Echo Pluck", pluck_echo), ("Soft Strike", pluck_soft)],
    "Keys": [("Glass Keys", keys_glass), ("Warm Keys", keys_warm), ("Wide Keys", keys_wide), ("Chorus Keys", keys_chorus)],
    "Rhythmic": [("Pulse", rhy_pulse), ("Chop", rhy_chop), ("Stutter", rhy_stutter), ("Triplet", rhy_triplet)],
    "Basses": [("Sub", bass_sub), ("Growl", bass_growl), ("Reese", bass_reese), ("Pluck Bass", bass_pluck)],
    "FX": [("Spectral Wash", fx_spectral), ("Flip Glide", fx_flip), ("Pitch Rain", fx_rain), ("Chaos", fx_chaos)],
}
CATS = ["Pads", "Textures", "Drones", "Plucks", "Keys", "Rhythmic", "Basses", "FX"]

def short(name):
    words = [w for w in re.split(r"[^A-Za-z]+", name) if len(w) > 2 and w.lower() not in ("vib", "main", "sus", "stac", "mute", "normal", "loud", "soft")]
    return " ".join(w.capitalize() for w in words[:2]) or "Source"

def build():
    used, files, n = set(), [], 0
    seen_names = {}
    for cat in CATS:
        for arch, fn in PLAN[cat]:
            rng0 = random.Random(f"{cat}/{arch}/src")
            for v in range(8):
                r = random.Random(f"{cat}/{arch}/{v}")
                extra, (params, g) = fn(r, v)
                base = pick_sources(cat, 1 + len(extra), rng0, used)
                srcs = [base[0]] + ([base[0]] * len(extra))  # layers share the primary source (Sample 0.2 -> slot 1 below)
                if extra:
                    # a second, different source in slot 1 for layered engines when one is available
                    alt = pick_sources(cat, 1, rng0, used | {base[0]["id"]})
                    if alt: srcs[1] = alt[0]
                used.update(s["id"] for s in srcs)
                n += 1
                name = f"{arch} {short(base[0]['name'])}"
                seen_names[name] = seen_names.get(name, 0) + 1
                if seen_names[name] > 1:
                    name += f" {seen_names[name]}"
                lines = ["mangle-preset 1", f"name {name}", f"category {cat}", "author Mangle"]
                for i, s in enumerate(srcs):
                    lines.append(f"slot {i} {s['id']:04d}")
                    if s["root"] >= 0:
                        lines.append(f"p {pid(SLOT, i, 0)} {s['root'] / 127:.4f}")
                if "key" in g: lines.append(f"p {pid(GLOBAL, 0, 1)} {g['key']:.4f}")
                if "stereo" in g: lines.append(f"p {pid(GLOBAL, 0, 2)} {g['stereo']:.4f}")
                for k, val in params:
                    lines.append(f"p {k} {min(max(val, 0.0), 1.0):.4f}")
                slug = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")
                files.append((f"{n:03d}-{slug}.mpreset", "\n".join(lines) + "\n"))
    return files

if __name__ == "__main__":
    files = build()
    os.makedirs(OUT, exist_ok=True)
    for f in os.listdir(OUT):
        if f.endswith(".mpreset"): os.remove(os.path.join(OUT, f))
    for name, text in files:
        open(os.path.join(OUT, name), "w").write(text)
    print(f"wrote {len(files)} presets")
