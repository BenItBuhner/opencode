#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${OPENGOAL_INSTALL_DIR:-$HOME/.local/bin}"

echo "Building opengoal-rust (release)..."
cargo build --release --manifest-path "$root/Cargo.toml" -p opengoal-cli

install -d "$target"
install -m 755 "$root/target/release/opengoal-rust" "$target/opengoal-rust"

echo "Installed $target/opengoal-rust"
echo "Run: opengoal-rust"
