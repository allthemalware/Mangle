#!/usr/bin/env bash
# Build the Mangle CLAP bundle and run the headless smoketest against it.
# Exercises the nih-plug shell end-to-end (params, notes, note expressions,
# the loop-clamp regression, state roundtrip) without a DAW.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"   # workspace root
cd "$here"

echo "== bundling mangle.clap =="
cargo xtask bundle mangle "$@"

bin="${TMPDIR:-/tmp}/mangle_smoketest"
echo "== building harness =="
c++ -O2 -std=c++17 -Itools/clap/include tools/smoketest.cpp -ldl -o "$bin"

echo "== running smoketest =="
"$bin" "target/bundled/mangle.clap"
