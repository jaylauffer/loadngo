#!/usr/bin/env bash
# Builds loadngo-animation for the browser into animation/pkg/: the wasm module (no imports)
# and js/motion.js, which loads and wraps it. Offline; needs the wasm32-unknown-unknown
# target and wasm-opt (binaryen).
#
#   animation/build.sh
#
# CARGO_TARGET_DIR defaults to the workspace's target/. pkg/ is build output, not committed.
set -euo pipefail

crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_dir="$(cd "$crate_dir/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$workspace_dir/target}"
out_dir="$crate_dir/pkg"

command -v wasm-opt >/dev/null || { echo "build.sh: wasm-opt (binaryen) is not on PATH" >&2; exit 1; }

# Release settings for this build only (the workspace's other release builds are left alone).
# Speed matters here: the search runs every tenth of a second on a Raspberry Pi too.
export CARGO_PROFILE_RELEASE_OPT_LEVEL=3
export CARGO_PROFILE_RELEASE_LTO=true
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_PANIC=abort
export CARGO_PROFILE_RELEASE_DEBUG=false

echo "== toolchain: $(rustc --version); $(wasm-opt --version)"
sysroot="$(rustc --print sysroot)"
commit="$(rustc -vV | sed -n 's/^commit-hash: //p')"
remap=(
  "--remap-path-prefix=$sysroot/lib/rustlib/src/rust=/rustc/$commit"
  "--remap-path-prefix=$workspace_dir=/loadngo"
  "--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
)
# 128-bit SIMD: every browser this targets has it (Chrome 91+, Firefox 89+, Safari 16.4+),
# and Chromium on a Raspberry Pi's ARM cores too.
simd=(-C target-feature=+simd128)

# Through RUSTFLAGS, not `cargo rustc -- ...`, so the remaps reach the dependencies too (a
# dependency's panic locations carry its registry path). With --target, RUSTFLAGS does not
# touch host build scripts.
(cd "$workspace_dir" && CARGO_TARGET_DIR="$target_dir" RUSTFLAGS="${remap[*]} ${simd[*]}" \
  cargo rustc --release --locked --offline --target wasm32-unknown-unknown -p loadngo-animation --lib --crate-type cdylib)

raw="$target_dir/wasm32-unknown-unknown/release/loadngo_animation.wasm"
rm -rf "$out_dir"
mkdir -p "$out_dir"
wasm-opt -O3 --enable-simd --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --strip-debug --strip-producers "$raw" -o "$out_dir/loadngo_animation.wasm"
cp "$crate_dir/js/motion.js" "$out_dir/"

if strings -n 6 "$out_dir/loadngo_animation.wasm" | grep -E "$HOME|/Users/|/home/" >/dev/null; then
  echo "build.sh: the wasm still embeds a local path" >&2
  exit 1
fi
if command -v sha256sum >/dev/null; then sum=sha256sum; else sum="shasum -a 256"; fi
echo "== $(cd "$out_dir" && $sum loadngo_animation.wasm)"
for f in "$out_dir"/loadngo_animation.wasm "$out_dir"/motion.js; do
  printf '%-24s %8d  gzip %7d\n' "$(basename "$f")" "$(wc -c <"$f")" "$(gzip -9 -c "$f" | wc -c)"
done
