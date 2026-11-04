#!/usr/bin/env bash
# The sixteenth critic: compares every string and null leaf of each page's result in this folder's
# rerun with the sixteenth round's (../critic15-rerun/results/), by path, with tools/leaf-diff.mjs
# (the fifteenth critic's). Writes strings.txt beside this script. BSD-3-Clause; see the
# repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
round="$here/../critic15-rerun"
{
  echo "# the sixteenth critic's rerun against the round's, every string or null leaf of each page's result (node tools/leaf-diff.mjs <round's file> <this rerun's file>)"
  for f in "$round"/results/rerun-*.json; do
    b="$(basename "$f")"
    echo "## results/$b: $(node "$here/tools/leaf-diff.mjs" "$f" "$here/results/$b")"
  done
} > "$here/strings.txt"
grep -c "; 0 differ" "$here/strings.txt"
