#!/usr/bin/env python3
"""Download the picks in tools/curation/picks.tsv (CC0 VCSL / VSCO 2 CE) and convert them
to factory/sources/<id>.flac (stereo kept, first 6 s, 44.1 kHz, 16-bit, peak -1 dB, gain capped at +24 dB).

Writes the matching index rows to <cache>/recorded.tsv. Downloads are cached in
$MANGLE_CACHE (default ~/.cache/mangle-factory). Needs curl and ffmpeg.
"""
import os, subprocess, sys, urllib.parse
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CACHE = os.environ.get("MANGLE_CACHE", os.path.expanduser("~/.cache/mangle-factory"))
REPO = {"VCSL": "sgossner/VCSL", "VSCO": "sgossner/VSCO-2-CE"}
LIB = {"VCSL": "VCSL (Versilian Community Sample Library)", "VSCO": "VSCO 2 Community Edition"}
OUT = os.path.join(ROOT, "factory", "sources")
MAX_GAIN, MIN_PEAK_DB = 24.0, -70.0

def run(*a):
    return subprocess.run(a, capture_output=True, text=True)

def build(row):
    id_, name, cat, lib, path, root = row
    dest = os.path.join(OUT, f"{id_}.flac")
    raw = os.path.join(CACHE, "raw", lib, path)
    if not os.path.exists(dest):
        if not os.path.exists(raw):
            os.makedirs(os.path.dirname(raw), exist_ok=True)
            url = f"https://raw.githubusercontent.com/{REPO[lib]}/master/" + urllib.parse.quote(path)
            r = run("curl", "-sfL", "--retry", "3", "-o", raw + ".part", url)
            if r.returncode:
                return id_, None, f"download failed: {path}"
            os.replace(raw + ".part", raw)
        v = run("ffmpeg", "-nostdin", "-i", raw, "-t", "6", "-af", "volumedetect", "-f", "null", "-")
        mx = [l for l in v.stderr.splitlines() if "max_volume" in l]
        if not mx:
            return id_, None, f"unreadable: {path}"
        peak = float(mx[0].split("max_volume:")[1].split()[0])
        if peak < MIN_PEAK_DB:
            return id_, None, f"silent: {path}"
        gain = min(-1.0 - peak, MAX_GAIN)
        r = run("ffmpeg", "-nostdin", "-y", "-v", "error", "-i", raw, "-t", "6", "-ar", "44100",
                "-af", f"volume={gain}dB,afade=t=out:st=5.7:d=0.3", "-c:a", "flac", "-sample_fmt", "s16", dest)
        if r.returncode:
            return id_, None, f"convert failed: {path}"
    note = "stereo kept, first 6 s, peak -1 dB (gain <= +24 dB)"
    return id_, "\t".join([id_, name, cat, "rec", f"sources/{id_}.flac", root, "CC0-1.0",
                           f"{LIB[lib]}: {path}; {note}"]), None

def main():
    os.makedirs(OUT, exist_ok=True)
    rows = [l.rstrip("\n").split("\t") for l in open(os.path.join(HERE, "curation", "picks.tsv"))]
    names_file = os.path.join(HERE, "curation", "names.tsv")  # id<TAB>display name overrides
    if os.path.exists(names_file):
        names = dict(l.rstrip("\n").split("\t", 1) for l in open(names_file) if "\t" in l)
        for r in rows:
            r[1] = names.get(r[0], r[1])
    ok, bad = [], []
    with ThreadPoolExecutor(8) as ex:
        for id_, line, err in ex.map(build, rows):
            (ok if line else bad).append(line or f"{id_}: {err}")
    with open(os.path.join(CACHE, "recorded.tsv"), "w") as f:
        f.write("\n".join(ok) + "\n")
    print(f"{len(ok)} built, {len(bad)} skipped")
    for b in bad:
        print("  skipped", b, file=sys.stderr)

if __name__ == "__main__":
    os.makedirs(CACHE, exist_ok=True)
    main()
