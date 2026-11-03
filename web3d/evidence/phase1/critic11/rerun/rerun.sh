#!/usr/bin/env bash
# Critic 11: every earlier critic's probe scripts (first to tenth) against the current web3d/pkg/,
# as ../../critic10-rerun/rerun-critics.sh runs them, with every result written here instead.
#   PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build bash rerun.sh <port>
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
p1="$here/../.."
port="$1"
run() {
  local name="$1"; shift
  echo "== $name"
  "$@" > "$here/$name.txt" 2>&1
  echo ">>> exit $?" >> "$here/$name.txt"
  tail -2 "$here/$name.txt"
}
run critic1-run node "$p1/critic/run.mjs" "$port" "$here/critic1"
run critic1-loss node "$p1/critic/run-loss.mjs" "$port"
run critic1-wasm-edge node "$p1/critic/wasm-edge.mjs"
run critic2-run node "$p1/critic2/run-critic.mjs" "$port" "$here/critic2"
run critic2-abi-edge node "$p1/critic2/abi-edge.mjs"
run critic3-run node "$p1/critic3/run3.mjs" "$port" "$here/critic3.json"
run critic4-run node "$p1/critic4/run4.mjs" "$port" "$here/critic4.json"
run critic5-main node "$p1/critic5/run5.mjs" "$port" "$here/critic5-main.json" x8 main heap cap
run critic5-exhaust node "$p1/critic5/run5.mjs" "$port" "$here/critic5-exhaust.json" exhaust
run critic6-main node "$p1/critic6/run6.mjs" "$port" "$here/critic6.json"
run critic6-extra node "$p1/critic6/run6.mjs" "$port" "$here/critic6-extra.json" extra.html
run critic7-main node "$p1/critic7/run7.mjs" "$port" "$here/critic7.json"
run critic8-main node "$p1/critic8/run8.mjs" "$port" "$here/critic8.json"
run critic8-batch2 node "$p1/critic8/run8.mjs" "$port" "$here/critic8-batch2.json" batch2.html
run critic8-batch3 node "$p1/critic8/run8.mjs" "$port" "$here/critic8-batch3.json" batch3.html
run critic9-main node "$p1/critic9/run9.mjs" "$port" "$here/run9.json"
run critic9-batch2 node "$p1/critic9/run9.mjs" "$port" "$here/run9-batch2.json" batch2.html
run critic9-frame-first node "$p1/critic9/run9.mjs" "$port" "$here/run9-frame-first.json" frame-first.html
for page in state state2 pm notop xtop bf; do
  run "critic10-$page" node "$here/run10-copy.mjs" "$port" "$((port + 1))" "$page"
done
grep -rl --exclude='*.png' "$HOME" "$here" 2>/dev/null | while read -r f; do sed -i '' -e "s|$HOME|~|g" "$f"; done
echo "all done"
