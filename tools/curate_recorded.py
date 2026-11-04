#!/usr/bin/env python3
"""Deterministically pick recorded sources from VCSL and VSCO 2 CE (both CC0-1.0).

Writes tools/curation/picks.tsv: id name category lib path root
Needs the `gh` CLI (GitHub tree listing only; no audio is downloaded here).
Pilot picks (ids 0500-0515) are kept verbatim from tools/curation/pilot_picks.tsv.
"""
import json, os, re, subprocess, collections

HERE = os.path.dirname(os.path.abspath(__file__))
FIRST_ID, LAST_ID = 500, 1023
MIN_B, MAX_B = 150 * 1024, 6 * 1024 * 1024
PER_GROUP = 3
SKIP = re.compile(r"(release|_rel[_.]|/rel|roll|phrase|scrape|noise_?floor|metronome|/rr\d+/|\.asd)", re.I)
NOTE = re.compile(r"(?<![A-Za-z])([A-G])(#|b)?(-?\d)(?![0-9])")
SEMI = {"C": 0, "D": 2, "E": 4, "F": 5, "G": 7, "A": 9, "B": 11}

def tree(repo):
    out = subprocess.check_output(["gh", "api", f"repos/{repo}/git/trees/master?recursive=1"])
    return [(t["path"], t["size"]) for t in json.loads(out)["tree"]
            if t["type"] == "blob" and t["path"].lower().endswith(".wav")]

def root_of(name):
    m = NOTE.search(name)
    if not m:
        return -1
    n = SEMI[m.group(1)] + (1 if m.group(2) == "#" else -1 if m.group(2) == "b" else 0)
    return 12 * (int(m.group(3)) + 1) + n

CATS = {"Aerophones": "winds", "Chordophones": "strings", "Idiophones": "percussion",
        "Membranophones": "drums", "Electrophones": "synth", "Brass": "brass",
        "Strings": "strings", "Woodwinds": "winds", "Keys": "keys", "Percussion": "percussion",
        "VSCO 1 Percussion": "percussion", "Miscellania Raw": "misc"}

def category(path):
    top = path.split("/")[0]
    low = path.lower()
    if "bell" in low or "glock" in low or "chime" in low: return "bells"
    if "gong" in low or "cymbal" in low or re.search(r"tam[- ]?tam", low): return "metal"
    if "tamb" in low or "shaker" in low or "rattle" in low or "snare" in low or "tom" in low: return "percussion"
    if "organ" in low or "harmonium" in low or "accordion" in low: return "keys"
    if "harp" in low or "zither" in low or "kalimba" in low or "mbira" in low: return "plucked"
    return CATS.get(top, "misc")

def candidates(lib, repo):
    groups = collections.defaultdict(list)
    for path, size in tree(repo):
        if not (MIN_B <= size <= MAX_B) or SKIP.search(path):
            continue
        groups["/".join(path.split("/")[:-1])].append(path)
    return [(lib, g, sorted(v)) for g, v in sorted(groups.items())]

def spread(items, k):
    if len(items) <= k:
        return items
    return [items[round(i * (len(items) - 1) / (k - 1))] for i in range(k)] if k > 1 else [items[len(items) // 2]]

def main():
    pilot = [l.rstrip("\n").split("\t") for l in open(os.path.join(HERE, "curation", "pilot_picks.tsv"))]
    taken = {(p[3], p[4]) for p in pilot}
    groups = candidates("VCSL", "sgossner/VCSL") + candidates("VSCO", "sgossner/VSCO-2-CE")
    # Interleave the two libraries so both are represented evenly.
    vc = [g for g in groups if g[0] == "VCSL"]
    vs = [g for g in groups if g[0] == "VSCO"]
    order = [g for pair in zip(vc, vs) for g in pair] + vc[len(vs):] + vs[len(vc):]
    queues = []
    for lib, gdir, files in order:
        files = [f for f in files if (lib, f) not in taken]
        picks = spread(files, PER_GROUP)
        if picks:
            queues.append((lib, gdir, picks))
    rows, next_id = [p for p in pilot], FIRST_ID + len(pilot)
    rnd = 0
    while next_id <= LAST_ID and any(rnd < len(q[2]) for q in queues):
        for lib, gdir, picks in queues:
            if rnd < len(picks) and next_id <= LAST_ID:
                f = picks[rnd]
                base = os.path.splitext(os.path.basename(f))[0]
                inst = gdir.split("/")[-1] if gdir.split("/")[-1] not in ("Main", "Sustains") else gdir.split("/")[-2]
                name = re.sub(r"\s+", " ", f"{inst} {base}".replace("_", " "))[:60]
                rows.append([f"{next_id:04d}", name, category(f), lib, f, str(root_of(base))])
                next_id += 1
        rnd += 1
    with open(os.path.join(HERE, "curation", "picks.tsv"), "w") as out:
        for r in rows:
            out.write("\t".join(r) + "\n")
    print(f"{len(rows)} picks, ids {rows[0][0]}..{rows[-1][0]}")

if __name__ == "__main__":
    main()
