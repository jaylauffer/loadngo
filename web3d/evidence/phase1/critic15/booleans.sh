#!/usr/bin/env bash
# The fifteenth critic: compares this folder's rerun with the fifteenth round's (../critic14-rerun/),
# every boolean leaf by path (../critic11/tools/bool-diff.mjs <round's file> <this rerun's file>):
# the earlier critics' scripts, their pages' results (results/rerun-*.json), and the round's own page
# (its results/r15.json against results/rerun-round15-r15.json here). Writes booleans.txt beside this
# script. BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
r15="$here/../critic14-rerun"
diff="$here/../critic11/tools/bool-diff.mjs"
{
  echo "# the fifteenth critic's rerun against the fifteenth round's (node ../critic11/tools/bool-diff.mjs <round's file> <this rerun's file>)"
  for f in critic3.json critic4.json critic5-exhaust.json critic5-main.json critic6-extra.json critic6.json critic7.json critic8-batch2.json critic8-batch3.json critic8.json run9.json run9-batch2.json run9-frame-first.json run10-bf.json run10-notop.json run10-pm.json run10-state.json run10-state2.json run10-xtop.json critic1/probe-report.json critic2/report-probe.json; do
    echo "## rerun/$f: $(node "$diff" "$r15/rerun/$f" "$here/rerun/$f")"
  done
  for f in "$r15"/results/rerun-*.json; do
    b="$(basename "$f")"
    echo "## results/$b: $(node "$diff" "$f" "$here/results/$b")"
  done
  echo "## ../critic14-rerun/results/r15.json vs results/rerun-round15-r15.json: $(node "$diff" "$r15/results/r15.json" "$here/results/rerun-round15-r15.json")"
} > "$here/booleans.txt"
cat "$here/booleans.txt"
