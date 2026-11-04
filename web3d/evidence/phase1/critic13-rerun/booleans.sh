#!/usr/bin/env bash
# Compares this round's rerun with the thirteenth critic's, every boolean leaf by path
# (../critic11/tools/bool-diff.mjs <critic's file> <this round's file>): the critic's rerun
# (../critic13/rerun/, ../critic13/results/rerun-*) against this folder's, and the critic's own pages
# (../critic13/results/<page>.json) against their rerun here (results/rerun-critic13-<page>.json).
# Writes booleans.txt beside this script. BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
c13="$here/../critic13"
diff="$here/../critic11/tools/bool-diff.mjs"
{
  echo "# The fourteenth round's rerun against the thirteenth critic's (node ../critic11/tools/bool-diff.mjs <critic's file> <this round's file>), every boolean leaf by path"
  for f in critic3.json critic4.json critic5-exhaust.json critic5-main.json critic6-extra.json critic6.json critic7.json critic8-batch2.json critic8-batch3.json critic8.json run9.json run9-batch2.json run9-frame-first.json run10-bf.json run10-notop.json run10-pm.json run10-state.json run10-state2.json run10-xtop.json critic1/probe-report.json critic2/report-probe.json; do
    echo "## rerun/$f: $(node "$diff" "$c13/rerun/$f" "$here/rerun/$f")"
  done
  for f in "$c13"/results/rerun-*.json; do
    b="$(basename "$f")"
    echo "## results/$b: $(node "$diff" "$f" "$here/results/$b")"
  done
  for page in f1 f1b f2 f2b f2c f2-fix f3 f4; do
    echo "## critic13/results/$page.json vs results/rerun-critic13-$page.json: $(node "$diff" "$c13/results/$page.json" "$here/results/rerun-critic13-$page.json")"
  done
} > "$here/booleans.txt"
cat "$here/booleans.txt"
