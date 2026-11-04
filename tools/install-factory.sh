#!/usr/bin/env bash
# Copy the factory bank to the data folder the plugin looks in first
# (see FACTORY.md): $MANGLE_DATA, else $XDG_DATA_HOME/mangle, else ~/.local/share/mangle.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${MANGLE_DATA:-${XDG_DATA_HOME:-$HOME/.local/share}/mangle}"
mkdir -p "$dest/factory"
rsync -a --delete "$here/factory/" "$dest/factory/"
echo "installed $(ls "$dest/factory/sources" | wc -l) sources and $(ls "$dest/factory/presets" | wc -l) presets to $dest/factory"
