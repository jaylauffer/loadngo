#!/bin/sh
# Runs one cargo command in a game repo against this local loadngo checkout
# instead of the git revision its Cargo.lock pins, then puts Cargo.lock back.
#
# Games depend on loadngo by git (`git = "https://github.com/jaylauffer/loadngo",
# branch = "dev"`), and Cargo.lock records the exact commit, so an everyday
# build is what CI builds. When a change spans loadngo and a game, run the
# game's cargo commands through this script to use your local loadngo edits:
#
#   cd ~/pudding/sng-zhoenus
#   ../loadngo/scripts/with-local-loadngo.sh run --release
#   ../loadngo/scripts/with-local-loadngo.sh test
#
# It passes Cargo a `[patch]` for every loadngo package in the game's lock
# file. Cargo rewrites Cargo.lock while patched, so the script restores the
# committed lock file afterwards; never commit a lock file written in
# between. Once the loadngo change is pushed, adopt it with
# `cargo update -p ui-core` (any loadngo package moves them all) and commit
# that Cargo.lock with the game change.
#
# Switching between patched and pinned builds recompiles the loadngo crates.
set -eu

if [ $# -eq 0 ] || [ "$1" = "--help" ] || [ "$1" = "-h" ]; then
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
fi

loadngo_dir=$(cd "$(dirname "$0")/.." && pwd)
source_url="https://github.com/jaylauffer/loadngo"
[ -f Cargo.lock ] || { echo "run this from a game repo root (no Cargo.lock here)" >&2; exit 1; }

# Loadngo packages this game actually uses, as its lock file records them.
packages=$(awk -v src="source = \"git+$source_url" '
    /^name = / { name = $3 }
    index($0, src) == 1 { gsub(/"/, "", name); print name }
' Cargo.lock | sort -u)
[ -n "$packages" ] || { echo "Cargo.lock has no packages from $source_url" >&2; exit 1; }

scratch=$(mktemp -d)
trap 'cp "$scratch/Cargo.lock" Cargo.lock; rm -rf "$scratch"' EXIT
cp Cargo.lock "$scratch/Cargo.lock"

{
    echo "[patch.\"$source_url\"]"
    for package in $packages; do
        manifest=$(grep -l "^name = \"$package\"" "$loadngo_dir"/*/Cargo.toml | head -n 1)
        [ -n "$manifest" ] || { echo "no local package $package in $loadngo_dir" >&2; exit 1; }
        echo "$package = { path = \"$(dirname "$manifest")\" }"
    done
} > "$scratch/local-loadngo.toml"

cargo --config "$scratch/local-loadngo.toml" "$@"
