#!/usr/bin/env python3
"""Merge the generated and recorded index fragments into factory/sources.tsv.

usage: assemble_sources.py <generated.tsv> <recorded.tsv>   (both headerless; see FACTORY.md)
Rows are sorted by id; duplicate ids are an error.
"""
import os, sys

HEADER = "id\tname\tcategory\tkind\tfile\troot\tlicense\torigin\n"
rows = {}
for path in sys.argv[1:3]:
    for line in open(path).read().splitlines():
        if not line.strip():
            continue
        cols = line.split("\t")
        if len(cols) != 8 or cols[0] in rows:
            sys.exit(f"bad or duplicate row in {path}: {line[:80]}")
        rows[cols[0]] = line
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "factory", "sources.tsv")
with open(out, "w") as f:
    f.write(HEADER)
    for k in sorted(rows):
        f.write(rows[k] + "\n")
print(f"{len(rows)} sources -> factory/sources.tsv")
