#!/usr/bin/env bash
# Critic 11: the cargo gates as CI runs them (macOS row, the wasm32 step), plus the tests.
# Each gate's exit status is recorded; home written "~". Writes beside this script.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ws="$(cd "$here/../../../../.." && pwd)"
cd "$ws"
red() { sed -e "s|$HOME|~|g"; }
gate() {
  local name="$1"; shift
  echo "\$ $*" > "$here/$name.txt"
  "$@" >> "$here/$name.txt" 2>&1
  local code=$?
  echo ">>> exit $code" >> "$here/$name.txt"
  sed -i '' -e "s|$HOME|~|g" "$here/$name.txt"
  echo "$name: exit $code"
}
gate fmt cargo fmt --check
gate clippy-macos-row cargo clippy --locked --workspace --all-targets --all-features --exclude loadngo-gfx-dx12 --exclude gui-win32 --exclude proactor-harness -- -D warnings
gate clippy-wasm32 cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings
gate clippy-wasm32-allfeatures cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets --all-features -- -D warnings
gate clippy-linux-arm64 cargo clippy --locked --offline -p loadngo-web3d --target aarch64-unknown-linux-gnu --all-targets --all-features -- -D warnings
gate test-web3d cargo test --locked --no-fail-fast -p loadngo-web3d
gate test-web3d-release cargo test --locked -p loadngo-web3d --release
