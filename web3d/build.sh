#!/usr/bin/env bash
# Builds loadngo-web3d for the browser into web3d/pkg/: the wasm module, optimised, and the JS
# that loads and wraps it. Offline: the crate has no dependencies.
#
#   web3d/build.sh
#
# CARGO_TARGET_DIR defaults to the workspace's target/. Needs the wasm32-unknown-unknown target
# and wasm-opt (binaryen). There is no wasm-bindgen step: the bindings are hand-written
# (js/gl.js, js/web3d.js) over the raw wasm ABI; docs/WEB3D.md says why.
set -euo pipefail

crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_dir="$(cd "$crate_dir/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$workspace_dir/target}"
out_dir="$crate_dir/pkg"

command -v wasm-opt >/dev/null || { echo "build.sh: wasm-opt (binaryen) is not on PATH" >&2; exit 1; }

# Release settings for this build only: a workspace sets profiles at its root, and the other
# crates' release builds should not change. Size matters more than speed here (the GPU is the
# bottleneck), and panics abort: the panic hook has already logged the message.
export CARGO_PROFILE_RELEASE_OPT_LEVEL=s
export CARGO_PROFILE_RELEASE_LTO=true
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_PANIC=abort
export CARGO_PROFILE_RELEASE_DEBUG=false

# Reproducible across machines and users: with the rust-src component installed, rustc writes the
# local sysroot's path (under the builder's home) into panic locations of standard-library code
# compiled into this crate. Remap it to the /rustc/<commit> prefix the standard library's own
# paths already use, and the workspace to a fixed name. `cargo rustc` passes these to this crate
# only, without replacing any rustflags the builder has configured.
# The output is byte-identical for a given rustc and wasm-opt; record which.
echo "== toolchain: $(rustc --version); $(wasm-opt --version)"
sysroot="$(rustc --print sysroot)"
commit="$(rustc -vV | sed -n 's/^commit-hash: //p')"
remap=(
  "--remap-path-prefix=$sysroot/lib/rustlib/src/rust=/rustc/$commit"
  "--remap-path-prefix=$workspace_dir=/loadngo"
  "--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
)

echo "== cargo rustc (target dir: $target_dir)"
(cd "$workspace_dir" && CARGO_TARGET_DIR="$target_dir" \
  cargo rustc --release --locked --offline --target wasm32-unknown-unknown -p loadngo-web3d --lib -- "${remap[@]}")

raw="$target_dir/wasm32-unknown-unknown/release/loadngo_web3d.wasm"
rm -rf "$out_dir"
mkdir -p "$out_dir"

echo "== wasm-opt -Os"
wasm-opt -Os --strip-debug --strip-producers "$raw" -o "$out_dir/loadngo_web3d.wasm"
cp "$crate_dir/js/gl.js" "$crate_dir/js/web3d.js" "$crate_dir/js/web3d.d.ts" "$out_dir/"

# Nothing of this machine may remain in the module.
if strings -n 6 "$out_dir/loadngo_web3d.wasm" | grep -E "$HOME|/Users/|/home/" >/dev/null; then
  echo "build.sh: the wasm still embeds a local path:" >&2
  strings -n 6 "$out_dir/loadngo_web3d.wasm" | grep -E "$HOME|/Users/|/home/" >&2
  exit 1
fi

echo "== sizes (bytes, and gzip -9)"
for f in "$raw" "$out_dir"/loadngo_web3d.wasm "$out_dir"/gl.js "$out_dir"/web3d.js; do
  printf '%-34s %8d  gzip %7d\n' "$(basename "$(dirname "$f")")/$(basename "$f")" \
    "$(wc -c <"$f")" "$(gzip -9 -c "$f" | wc -c)"
done
