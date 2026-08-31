#!/usr/bin/env bash
# Records a perf profile of proactor-harness's profile-target binary
# against the real io_uring backend and renders a flamegraph. Linux-only
# (io_uring is Linux-specific) -- run this on dolores, not on macOS.
#
# Usage:
#   scripts/profile-uring.sh frame [seconds] [deferred_per_frame] [jobs_per_frame]
#   scripts/profile-uring.sh flood [seconds] [threads] [batch]
#
# Requires `perf` (apt install linux-perf on Debian/Raspberry Pi OS) and
# either `cargo-flamegraph` (cargo install flamegraph) or the classic
# Brendan Gregg FlameGraph scripts on PATH (stackcollapse-perf.pl,
# flamegraph.pl) as a fallback -- this script tries cargo-flamegraph
# first, falls back to raw perf + those scripts if it's not installed.
#
# Output: flamegraph.svg in the current directory (cargo-flamegraph path)
# or ./perf-out/flamegraph.svg (raw-perf fallback path).

set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "profile-uring.sh: this profiles the io_uring backend, Linux-only. Run it on dolores." >&2
    exit 1
fi

MODE="${1:-frame}"
shift || true

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

echo "profile-uring.sh: building profile-target in release mode..."
cargo build --release --bin profile-target

# Workspace-level target dir (this crate is a workspace member, so cargo
# puts binaries in ../target relative to this crate, not a target dir of
# its own) -- adjust if your cargo config overrides target-dir.
BIN="$REPO_ROOT/../target/release/profile-target"
if [[ ! -x "$BIN" ]]; then
    echo "profile-uring.sh: expected binary at $BIN, not found" >&2
    exit 1
fi

if command -v cargo-flamegraph >/dev/null 2>&1; then
    echo "profile-uring.sh: using cargo-flamegraph"
    cargo flamegraph --release --bin profile-target -- "$MODE" "$@"
    echo "profile-uring.sh: wrote flamegraph.svg"
elif command -v perf >/dev/null 2>&1 && command -v stackcollapse-perf.pl >/dev/null 2>&1 && command -v flamegraph.pl >/dev/null 2>&1; then
    echo "profile-uring.sh: using raw perf + FlameGraph scripts"
    mkdir -p perf-out
    perf record -g -o perf-out/perf.data -- "$BIN" "$MODE" "$@"
    perf script -i perf-out/perf.data | stackcollapse-perf.pl > perf-out/out.folded
    flamegraph.pl perf-out/out.folded > perf-out/flamegraph.svg
    echo "profile-uring.sh: wrote perf-out/flamegraph.svg"
else
    echo "profile-uring.sh: need either cargo-flamegraph (cargo install flamegraph)" >&2
    echo "  or perf + stackcollapse-perf.pl + flamegraph.pl (the Brendan Gregg" >&2
    echo "  FlameGraph toolkit) on PATH. Falling back to a plain perf report" >&2
    echo "  instead of a flamegraph." >&2
    mkdir -p perf-out
    perf record -g -o perf-out/perf.data -- "$BIN" "$MODE" "$@"
    perf report -i perf-out/perf.data
fi
