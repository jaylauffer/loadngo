#!/usr/bin/env bash
# Critic 14: compares this folder's rerun with the fourteenth round's (../critic13-rerun/), every
# boolean leaf by path (../critic11/tools/bool-diff.mjs <round's file> <this rerun's file>).
# Writes booleans.txt beside this script. BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
r14="$here/../critic13-rerun"
diff="$here/../critic11/tools/bool-diff.mjs"
{
  echo "# critic 14's rerun against the fourteenth round's (node ../critic11/tools/bool-diff.mjs <round's file> <critic 14's file>)"
  for f in critic3.json critic4.json critic5-exhaust.json critic5-main.json critic6-extra.json critic6.json critic7.json critic8-batch2.json critic8-batch3.json critic8.json run9.json run9-batch2.json run9-frame-first.json run10-bf.json run10-notop.json run10-pm.json run10-state.json run10-state2.json run10-xtop.json critic1/probe-report.json critic2/report-probe.json; do
    echo "## rerun/$f: $(node "$diff" "$r14/rerun/$f" "$here/rerun/$f")"
  done
  for f in "$r14"/results/rerun-*.json; do
    b="$(basename "$f")"
    echo "## results/$b: $(node "$diff" "$f" "$here/results/$b")"
  done
  echo "## results/reopen-paths.json vs results/rerun-round14-reopen-paths.json: $(node "$diff" "$r14/results/reopen-paths.json" "$here/results/rerun-round14-reopen-paths.json")"
} > "$here/booleans.txt"
cat "$here/booleans.txt"
