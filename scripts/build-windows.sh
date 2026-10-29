#!/usr/bin/env bash
# Cross-compiles the Windows executable from macOS or Linux into dist/.
# Needs rustup and the mingw-w64 linker (brew install mingw-w64). Uses Rust 1.88 by default;
# set TOOLCHAIN to use another (1.88 or newer). On Windows itself, `cargo build --release` is
# all you need.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLCHAIN="${TOOLCHAIN:-1.88.0}"
rustup toolchain install "$TOOLCHAIN" --profile minimal --target x86_64-pc-windows-gnu
export RUSTC="$(rustup which --toolchain "$TOOLCHAIN" rustc)"
CARGO_TARGET_DIR=target/windows "$(rustup which --toolchain "$TOOLCHAIN" cargo)" build --release --target x86_64-pc-windows-gnu
mkdir -p dist
cp target/windows/x86_64-pc-windows-gnu/release/oink.exe "dist/Oink.exe"
echo "Built dist/Oink.exe"
