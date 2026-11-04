#!/usr/bin/env bash
# Install the prebuilt plugin from a release archive (Linux, macOS).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
case "$(uname -s)" in
  Linux)  clap_dir="$HOME/.clap"; data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/mangle" ;;
  Darwin) clap_dir="$HOME/Library/Audio/Plug-Ins/CLAP"; data_dir="$HOME/Library/Application Support/mangle" ;;
  *) echo "Unsupported system" >&2; exit 1 ;;
esac
mkdir -p "$clap_dir" "$data_dir/factory"
rm -rf "$clap_dir/mangle.clap"
cp -R "$here/mangle.clap" "$clap_dir/mangle.clap"
[ "$(uname -s)" = Darwin ] && xattr -dr com.apple.quarantine "$clap_dir/mangle.clap" 2>/dev/null || true
cp -R "$here/factory/." "$data_dir/factory/"
echo "Installed to $clap_dir. Rescan plugins in your host."
