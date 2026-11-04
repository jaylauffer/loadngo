#!/usr/bin/env bash
# The seventeenth round: compares this folder's rerun with the sixteenth critic's (../critic16/), every
# boolean leaf by path (../critic11/tools/bool-diff.mjs <critic's file> <this rerun's file>): its
# rerun of the earlier critics and of the rounds' pages, then its own pages' results against
# this rerun of them. Writes booleans.txt beside this script. BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
c16="$here/../critic16"
diff="$here/../critic11/tools/bool-diff.mjs"
{
  echo "# the seventeenth round's rerun against the sixteenth critic's (node ../critic11/tools/bool-diff.mjs <critic's file> <this round's file>)"
  for f in critic3.json critic4.json critic5-exhaust.json critic5-main.json critic6-extra.json critic6.json critic7.json critic8-batch2.json critic8-batch3.json critic8.json run9.json run9-batch2.json run9-frame-first.json run10-bf.json run10-notop.json run10-pm.json run10-state.json run10-state2.json run10-xtop.json critic1/probe-report.json critic2/report-probe.json; do
    echo "## rerun/$f: $(node "$diff" "$c16/rerun/$f" "$here/rerun/$f")"
  done
  for f in "$c16"/results/rerun-*.json; do
    b="$(basename "$f")"
    echo "## results/$b: $(node "$diff" "$f" "$here/results/$b")"
  done
  for p in g1-loss-in-dispatch b1-builtins-at-load o1-frame-order n1-stopped-and-cancelled; do
    echo "## critic16/results/$p.json vs results/rerun-critic16-$p.json: $(node "$diff" "$c16/results/$p.json" "$here/results/rerun-critic16-$p.json")"
  done
} > "$here/booleans.txt"
cat "$here/booleans.txt"
