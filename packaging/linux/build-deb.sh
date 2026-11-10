#!/usr/bin/env bash
# Build and install edgehop on Ubuntu/Debian. Run from the repo root:  ./packaging/linux/build-deb.sh
set -euo pipefail
cd "$(dirname "$0")/../.."

echo "==> Installing build dependencies (sudo)"
sudo apt-get update -qq
sudo apt-get install -y build-essential pkg-config libdbus-1-dev curl wl-clipboard xclip libnotify-bin

if ! command -v cargo >/dev/null 2>&1; then
    echo "==> Installing Rust (rustup, user-local)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi
command -v cargo-deb >/dev/null 2>&1 || cargo install cargo-deb --locked

echo "==> Building"
cargo build --release
cargo run --release --quiet -- --write-icon packaging/linux/edgehop.png
cargo deb --no-build

DEB=$(ls -t target/debian/edgehop_*.deb | head -n1)
echo "==> Installing $DEB"
sudo apt-get install -y "./$DEB"
echo
echo "Done. Launch 'edgehop' from your app menu (log out/in once so /dev/uinput permissions apply)."
