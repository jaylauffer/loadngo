#!/usr/bin/env bash
# Reruns every critic's probe scripts (first to ninth) against the current web3d/pkg/, one after
# another, each with its own server and headless Chrome (each script stops both). Results in
# earlier/ (critics 1 to 8) and here (the ninth's run9*.json), with the home directory written "~".
# Uses the Mac's GPU: claim it first.
#   PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build bash rerun-critics.sh <port>
# (critics 6 and 7 also take <port>+1.)
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
p1="$here/.."
out="$here/earlier"
port="$1"
mkdir -p "$out/critic1" "$out/critic2"
run() {
  local name="$1"; shift
  echo "== $name: ${*//$p1/..}" | sed -e "s|$HOME|~|g"
  "$@" > "$out/$name.txt" 2>&1
  echo ">>> exit $?" >> "$out/$name.txt"
  tail -3 "$out/$name.txt" | sed -e "s|$HOME|~|g"
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
run critic6-main node "$p1/critic6/run6.mjs" "$port" "$out/critic6.json"
run critic6-extra node "$p1/critic6/run6.mjs" "$port" "$out/critic6-extra.json" extra.html
run critic7-main node "$p1/critic7/run7.mjs" "$port" "$out/critic7.json"
run critic8-main node "$p1/critic8/run8.mjs" "$port" "$out/critic8.json"
run critic8-batch2 node "$p1/critic8/run8.mjs" "$port" "$out/critic8-batch2.json" batch2.html
run critic8-batch3 node "$p1/critic8/run8.mjs" "$port" "$out/critic8-batch3.json" batch3.html
# The ninth's pages (fix.html is left out: it tried the critic's patched copy of the previous
# web3d.js, whose shim cannot load the present module).
run critic9-main node "$p1/critic9/run9.mjs" "$port" "$here/run9.json"
run critic9-batch2 node "$p1/critic9/run9.mjs" "$port" "$here/run9-batch2.json" batch2.html
run critic9-frame-first node "$p1/critic9/run9.mjs" "$port" "$here/run9-frame-first.json" frame-first.html
# No home paths in anything written here.
grep -rl --exclude='*.png' "$HOME" "$out" "$here"/run9*.json 2>/dev/null | while read -r f; do sed -i '' -e "s|$HOME|~|g" "$f"; done
echo "all done"
