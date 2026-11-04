#!/usr/bin/env bash
# Build Mangle from source and install it for the current user (Linux, macOS).
#   bash install.sh          asks one question, then does everything
#   bash install.sh --yes    no questions (also used when stdin is not a terminal)
#   bash install.sh --no-factory   skip the factory bank download
set -euo pipefail

repo="allthemalware/Mangle"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
yes=0; factory=1
for a in "$@"; do
  case "$a" in
    -y|--yes) yes=1 ;;
    --no-factory) factory=0 ;;
    -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

case "$(uname -s)" in
  Linux)  clap_dir="$HOME/.clap"
          data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/mangle" ;;
  Darwin) clap_dir="$HOME/Library/Audio/Plug-Ins/CLAP"
          data_dir="$HOME/Library/Application Support/mangle" ;;
  *) echo "Unsupported system. On Windows run install.ps1 instead." >&2; exit 1 ;;
esac

if [ "$yes" = 0 ] && [ -t 0 ]; then
  printf 'Build Mangle and install it to %s? [Y/n] ' "$clap_dir"
  read -r reply
  case "$reply" in n|N|no|NO) echo "Cancelled."; exit 0 ;; esac
fi

if ! command -v cargo >/dev/null 2>&1; then
  if [ -x "$HOME/.cargo/bin/cargo" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
  else
    echo "Rust not found; installing it with rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    export PATH="$HOME/.cargo/bin:$PATH"
  fi
fi

cd "$here"
echo "Building (the first build takes a few minutes)..."
if ! cargo xtask bundle mangle --release; then
  if [ "$(uname -s)" = Linux ]; then
    echo "Build failed. Linux needs these development packages (Debian/Ubuntu names):" >&2
    echo "  libasound2-dev libgl-dev libjack-jackd2-dev libx11-xcb-dev libxcb1-dev" >&2
    echo "  libxcb-dri2-0-dev libxcb-icccm4-dev libxcursor-dev libxkbcommon-dev" >&2
    echo "  libxcb-shape0-dev libxcb-xfixes0-dev libwayland-dev g++ pkg-config" >&2
  fi
  exit 1
fi

mkdir -p "$clap_dir"
rm -rf "$clap_dir/mangle.clap"
cp -R target/bundled/mangle.clap "$clap_dir/mangle.clap"
echo "Installed $clap_dir/mangle.clap"

mkdir -p "$data_dir/factory"
cp -R factory/. "$data_dir/factory/"
if [ "$factory" = 1 ]; then
  echo "Downloading the factory sounds (about 235 MB)..."
  tmp="$(mktemp -d)"
  if curl -fL --progress-bar -o "$tmp/bank.zip" \
       "https://github.com/$repo/releases/latest/download/mangle-factory-bank.zip" \
     && unzip -qo "$tmp/bank.zip" -d "$data_dir"; then
    echo "Factory sounds installed to $data_dir/factory"
  else
    echo "Could not download the factory sounds. Presets are installed; build the audio" >&2
    echo "yourself with tools/build_factory.sh, then tools/install-factory.sh." >&2
  fi
  rm -rf "$tmp"
fi

echo "Done. Rescan plugins in your host."
