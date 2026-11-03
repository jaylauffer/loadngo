#!/usr/bin/env bash
# Ninth critic: the Rust gates as CI runs them (macOS row, CI's wasm32 step), and the tests.
# Output has the home directory replaced by "~".
set -u
cd "$(dirname "$0")/../../../.."   # the loadngo workspace
red() { sed -e "s|$HOME|~|g"; }
step() { echo; echo "== $*"; "$@" 2>&1 | red; echo "exit ${PIPESTATUS[0]}"; }
echo "== toolchain: $(rustc --version); $(cargo --version); $(cargo clippy --version)"
echo "== HEAD: $(git rev-parse --short HEAD) on $(git branch --show-current); changed paths outside critic9: $(git status --porcelain -- . ':!web3d/evidence/phase1/critic9' | wc -l | tr -d ' ')"
step cargo fmt --check
step cargo fmt --check -p loadngo-web3d
step cargo clippy --locked --workspace --all-targets --all-features --exclude loadngo-gfx-dx12 --exclude gui-win32 --exclude proactor-harness -- -D warnings
step cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings
step cargo clippy --locked -p loadngo-web3d --all-targets --all-features -- -D warnings
step cargo test --locked -p loadngo-web3d
step cargo test --locked -p loadngo-web3d --release
