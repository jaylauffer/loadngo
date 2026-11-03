#!/usr/bin/env bash
# Seventh critic: the Rust gates, as CI runs them, plus clippy for web3d on more targets.
# Usage: gates.sh <empty-target-dir>   (output to stdout; the caller tees it into gates.txt)
set -uo pipefail
cd "$(dirname "$0")/../../../.." || exit 1   # the loadngo workspace
fresh="$1"
run() { echo; echo "\$ $*"; "$@"; echo "exit $?"; }
echo "== $(date -u +%FT%TZ) loadngo $(git rev-parse --short HEAD) on $(git branch --show-current)"
echo "== $(rustc --version); $(cargo clippy --version)"
run cargo fmt --check
run cargo fmt --check -p loadngo-web3d
# CI's macOS row (this machine), with the workspace's own target dir (shared, incremental).
run cargo clippy --locked --workspace --all-targets --all-features --exclude loadngo-gfx-dx12 --exclude gui-win32 --exclude proactor-harness -- -D warnings
# web3d alone, from an empty target dir.
export CARGO_TARGET_DIR="$fresh"
run cargo clippy --locked -p loadngo-web3d --all-targets -- -D warnings
run cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings
run cargo clippy --locked -p loadngo-web3d --target aarch64-unknown-linux-gnu --all-targets -- -D warnings
run cargo clippy --locked -p loadngo-web3d --target x86_64-pc-windows-msvc --all-targets -- -D warnings
run cargo test --locked -p loadngo-web3d
run cargo test --locked -p loadngo-web3d --release
