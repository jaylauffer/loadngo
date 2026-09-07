#!/usr/bin/env bash
# Type-check the Windows-only code from a non-Windows host.
#
# Windows is a first-class target but the only Windows machine belongs to
# someone else, so `cargo check --target x86_64-pc-windows-msvc` is the
# closest thing to a gate we have. It works because `check` never links --
# but blake3's build script still wants MASM (`ml64.exe`) to assemble its
# SIMD path, which no macOS/Linux host has. blake3's `pure` feature swaps
# that for a Rust implementation with an identical API, which is all a type
# check needs, so this enables it just for the duration of the run and puts
# Cargo.toml back afterwards (including on failure or Ctrl-C).
#
# Requires: rustup target add x86_64-pc-windows-msvc
#
# This is a type check, not a run: it catches the cfg-gated compile breaks
# that a macOS or Linux build silently skips. It cannot tell you whether
# D3D12 actually behaves. See docs/CLIP_AND_SCISSOR.md for what that
# distinction has already cost.
set -euo pipefail

cd "$(dirname "$0")/.."
manifest="Cargo.toml"
backup="$(mktemp)"
cp "$manifest" "$backup"
trap 'cp "$backup" "$manifest"; rm -f "$backup"' EXIT

python3 - "$manifest" <<'PY'
import sys

path = sys.argv[1]
text = open(path).read()
needle = 'blake3 = "1"'
if needle not in text:
    raise SystemExit(f"expected {needle!r} in {path}; update check-windows.sh")
open(path, "w").write(
    text.replace(needle, 'blake3 = { version = "1", features = ["pure"] }', 1)
)
PY

echo "==> cargo check --target x86_64-pc-windows-msvc"
cargo check -p loadngo-gfx-dx12 -p loadngo-host-desktop \
    --target x86_64-pc-windows-msvc --all-features "$@"
echo "==> Windows targets type-check"
