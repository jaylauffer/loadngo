#!/usr/bin/env bash
# The seventeenth round (a copy of ../../critic15-rerun/gates-contention/run.sh): runs gates.sh with
# the given ports from the workspace root, and every 5 s while it runs records the Chrome browsers
# running (desktop or headless; "before" if it was running when the gates began, so another
# session's or the desktop's) and the count of GPU processes, into contention-during.txt beside
# this script. Exits with gates.sh's status.
#   PLAYWRIGHT_CORE=... THREE_DIR=... bash run.sh <port> <port for --blind>
# BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ws="$(cd "$here/../../../../.." && pwd)"
out="$here/contention-during.txt"
browsers() { ps -axo pid=,args= | awk '/\/Contents\/MacOS\/Google Chrome( |$)/ && !/Helper/ { h = ($0 ~ /--headless/) ? "headless" : "desktop"; print $1 ":" h }'; }
before=" $(browsers | cut -d: -f1 | tr '\n' ' ') "
echo "# every 5 s during: bash web3d/evidence/phase1/gates.sh $1 $2 (\"before\": running when the gates began)" > "$out"
cd "$ws"
bash web3d/evidence/phase1/gates.sh "$1" "$2" &
gates=$!
while kill -0 "$gates" 2>/dev/null; do
  line=""
  for b in $(browsers); do
    pid="${b%%:*}"; kind="${b#*:}"
    case "$before" in *" $pid "*) who="before" ;; *) who="since" ;; esac
    line="$line $who $kind pid $pid;"
  done
  gpu="$(ps -axo args= | grep -c -- '--type=gpu-process' | tr -d ' ')"
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) browsers:${line:- none;} gpu processes: $((gpu - 1))" >> "$out"
  sleep 5
done
wait "$gates"
