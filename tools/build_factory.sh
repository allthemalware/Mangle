#!/usr/bin/env bash
# Rebuild the factory audio and index from scratch (needs cargo, ffmpeg, curl, python3).
# Audio is not committed: factory/sources/*.flac is produced here; sources.tsv, presets and
# tools/curation/picks.tsv are the committed record.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cache="${MANGLE_CACHE:-$HOME/.cache/mangle-factory}"
mkdir -p "$cache/gen" "$here/factory/sources"
cargo run -q --release -p xtask -- gen-sources "$cache/gen"
for w in "$cache"/gen/sources/*.wav; do
  b="$(basename "$w" .wav)"
  [ -f "$here/factory/sources/$b.flac" ] && [ "$here/factory/sources/$b.flac" -nt "$w" ] && continue
  ffmpeg -nostdin -y -v error -i "$w" -c:a flac -sample_fmt s16 "$here/factory/sources/$b.flac"
done
python3 "$here/tools/build_recorded.py"
sed -E 's#\.wav\t#.flac\t#' "$cache/gen/generated.tsv" > "$cache/gen/generated_flac.tsv"
python3 "$here/tools/assemble_sources.py" "$cache/gen/generated_flac.tsv" "$cache/recorded.tsv"
