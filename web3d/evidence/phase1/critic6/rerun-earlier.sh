#!/usr/bin/env bash
# Sixth critic: reruns every earlier critic's scripts against the current pkg/, one after another on one
# port, each with its own server and headless Chrome (the scripts stop both). Results in earlier/.
#   PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build bash rerun-earlier.sh <port>
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
p1="$here/.."
out="$here/earlier"
port="$1"
mkdir -p "$out/critic1" "$out/critic2"
run() {
  local name="$1"; shift
  echo "== $name: $*"
  "$@" > "$out/$name.txt" 2>&1
  echo ">>> exit $?" >> "$out/$name.txt"
  tail -3 "$out/$name.txt"
}
run critic1-run node "$p1/critic/run.mjs" "$port" "$out/critic1"
run critic1-loss node "$p1/critic/run-loss.mjs" "$port"
run critic1-wasm-edge node "$p1/critic/wasm-edge.mjs"
run critic2-run node "$p1/critic2/run-critic.mjs" "$port" "$out/critic2"
run critic2-abi-edge node "$p1/critic2/abi-edge.mjs"
run critic3-run node "$p1/critic3/run3.mjs" "$port" "$out/critic3.json"
run critic4-run node "$p1/critic4/run4.mjs" "$port" "$out/critic4.json"
run critic5-main node "$p1/critic5/run5.mjs" "$port" "$out/critic5-main.json" x8 main heap cap
run critic5-exhaust node "$p1/critic5/run5.mjs" "$port" "$out/critic5-exhaust.json" exhaust
echo "all done"
