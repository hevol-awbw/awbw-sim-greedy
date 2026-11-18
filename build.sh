#!/bin/sh
# Builds the bot reproducibly: the same commit gives the same module, byte for byte, on any
# machine with the toolchain in rust-toolchain.toml. Paths that differ between machines (this
# folder, cargo's home) are rewritten in the module, so they don't change its sha256.
#   ./build.sh          -> target/wasm32-unknown-unknown/release/*.wasm, and prints its sha256
set -eu
cd "$(dirname "$0")"
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTFLAGS="--remap-path-prefix=$PWD=/bot --remap-path-prefix=$CARGO_HOME=/cargo"
cargo build --release --locked --target wasm32-unknown-unknown
for f in target/wasm32-unknown-unknown/release/*.wasm; do
  if command -v sha256sum >/dev/null; then sha256sum "$f"; else shasum -a 256 "$f"; fi
done
