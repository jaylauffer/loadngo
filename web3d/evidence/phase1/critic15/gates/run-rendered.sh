#!/usr/bin/env bash
# The fifteenth critic's rendered checks, by the commands gates.sh runs, with their output here
# instead of over the committed evidence: check.mjs into rendered/ (must exit 0) and with --blind
# into blind/ (must exit 3). While each runs, every 5 s, the Chrome browsers running (desktop or
# headless; "before" if running when that check began) and the count of GPU processes go into its
# contention-during.txt (the round's gates-contention/run.sh sampler). The home directory is written
# <home>. BSD-3-Clause; see the repository's LICENSE.
#   PLAYWRIGHT_CORE=.../playwright-core/index.mjs bash run-rendered.sh <port> <port for --blind>
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ws="$(cd "$here/../../../../.." && pwd)"
browsers() { ps -axo pid=,args= | awk '/\/Contents\/MacOS\/Google Chrome( |$)/ && !/Helper/ { h = ($0 ~ /--headless/) ? "headless" : "desktop"; print $1 ":" h }'; }
cd "$ws"
check() {
  local dir="$1" port="$2"; shift 2
  mkdir -p "$here/$dir"
  local before=" $(browsers | cut -d: -f1 | tr '\n' ' ') "
  echo "# every 5 s during: node web3d/demo/check.mjs --port $port --out .../critic15/gates/$dir --seconds 10 --steady 60 $* (\"before\": running when it began)" > "$here/$dir/contention-during.txt"
  node web3d/demo/check.mjs --port "$port" --out "$here/$dir" --seconds 10 --steady 60 "$@" > "$here/$dir/check.log" 2>&1 &
  local pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    local line=""
    for b in $(browsers); do
      local bp="${b%%:*}" kind="${b#*:}" who
      case "$before" in *" $bp "*) who="before" ;; *) who="since" ;; esac
      line="$line $who $kind pid $bp;"
    done
    local gpu
    gpu="$(ps -axo args= | grep -c -- '--type=gpu-process' | tr -d ' ')"
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) browsers:${line:- none;} gpu processes: $((gpu - 1))" >> "$here/$dir/contention-during.txt"
    sleep 5
  done
  wait "$pid"
  local code=$?
  echo ">>> exit $code" >> "$here/$dir/check.log"
  echo "$dir: exit $code"
}
check rendered "$1"
check blind "$2" --blind
rm -f "$here"/blind/demo-*.png
grep -rl "$HOME" "$here/rendered" "$here/blind" 2>/dev/null | grep -v '\.png$' | while read -r f; do sed -i '' -e "s|$HOME|<home>|g" "$f"; done
