#!/usr/bin/env bash
# Builds web3d as it was at a commit (default: 994f9bc8, the round the ninth critic judged) into
# <out>/pkg/, as a standalone crate outside the workspace, for the before-the-fix control: the
# current probes run against it with probes-against.mjs. Offline; the crate has no dependencies.
# Not byte-identical to build.sh's output (no path remapping): it is a control, never shipped.
#   bash build-before-fix.sh <out dir> [commit]
set -euo pipefail
out="$1"
commit="${2:-994f9bc8}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
rm -rf "$out"
mkdir -p "$out/pkg"
git -C "$repo" archive "$commit" web3d/Cargo.toml web3d/src web3d/js | tar -x -C "$out"
cd "$out/web3d"
# The workspace's package fields, written in, and a workspace of its own.
sed -i.bak -e 's/^edition.workspace = true/edition = "2021"/' \
  -e 's/^authors.workspace = true/authors = ["loadngo maintainers"]/' \
  -e 's/^license.workspace = true/license = "BSD-3-Clause"/' Cargo.toml
printf '\n[workspace]\n' >> Cargo.toml
CARGO_TARGET_DIR="$out/target" CARGO_PROFILE_RELEASE_OPT_LEVEL=s CARGO_PROFILE_RELEASE_LTO=true \
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 CARGO_PROFILE_RELEASE_PANIC=abort \
  cargo rustc --release --offline --target wasm32-unknown-unknown --lib
wasm-opt -Os --strip-debug --strip-producers "$out/target/wasm32-unknown-unknown/release/loadngo_web3d.wasm" -o "$out/pkg/loadngo_web3d.wasm"
cp js/gl.js js/web3d.js "$out/pkg/"
echo "built $commit into $out/pkg"
