#!/usr/bin/env bash
# The sixteenth round: compares this folder's rerun with the fifteenth critic's (../critic15/), every
# boolean leaf by path (../critic11/tools/bool-diff.mjs <critic's file> <this rerun's file>): its
# rerun of the earlier critics and of the fifteenth round's page, then its own pages' results against
# this rerun of them. Writes booleans.txt beside this script. BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
c15="$here/../critic15"
diff="$here/../critic11/tools/bool-diff.mjs"
{
  echo "# the sixteenth round's rerun against the fifteenth critic's (node ../critic11/tools/bool-diff.mjs <critic's file> <this round's file>)"
  for f in critic3.json critic4.json critic5-exhaust.json critic5-main.json critic6-extra.json critic6.json critic7.json critic8-batch2.json critic8-batch3.json critic8.json run9.json run9-batch2.json run9-frame-first.json run10-bf.json run10-notop.json run10-pm.json run10-state.json run10-state2.json run10-xtop.json critic1/probe-report.json critic2/report-probe.json; do
    echo "## rerun/$f: $(node "$diff" "$c15/rerun/$f" "$here/rerun/$f")"
  done
  for f in "$c15"/results/rerun-*.json; do
    b="$(basename "$f")"
    echo "## results/$b: $(node "$diff" "$f" "$here/results/$b")"
  done
  for p in s1-restored-create s2-worker-closed s3-builtins-per-call; do
    echo "## critic15/results/$p.json vs results/rerun-critic15-$p.json: $(node "$diff" "$c15/results/$p.json" "$here/results/rerun-critic15-$p.json")"
  done
} > "$here/booleans.txt"
cat "$here/booleans.txt"
