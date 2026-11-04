#!/usr/bin/env bash
# The sixteenth critic: compares this folder's rerun with the sixteenth round's (../critic15-rerun/),
# every boolean leaf by path (../critic11/tools/bool-diff.mjs <round's file> <this rerun's file>): the
# round's rerun of every earlier critic and round page, the fifteenth critic's pages among them. Writes booleans.txt beside this script. BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
c15="$here/../critic15-rerun"
diff="$here/../critic11/tools/bool-diff.mjs"
{
  echo "# the sixteenth critic's rerun against the sixteenth round's (node ../critic11/tools/bool-diff.mjs <round's file> <this critic's file>)"
  for f in critic3.json critic4.json critic5-exhaust.json critic5-main.json critic6-extra.json critic6.json critic7.json critic8-batch2.json critic8-batch3.json critic8.json run9.json run9-batch2.json run9-frame-first.json run10-bf.json run10-notop.json run10-pm.json run10-state.json run10-state2.json run10-xtop.json critic1/probe-report.json critic2/report-probe.json; do
    echo "## rerun/$f: $(node "$diff" "$c15/rerun/$f" "$here/rerun/$f")"
  done
  for f in "$c15"/results/rerun-*.json; do
    b="$(basename "$f")"
    echo "## results/$b: $(node "$diff" "$f" "$here/results/$b")"
  done
} > "$here/booleans.txt"
cat "$here/booleans.txt"
