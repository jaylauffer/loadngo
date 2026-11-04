#!/usr/bin/env bash
# The thirteenth critic: a copy of the thirteenth round's script (../critic12-rerun/rerun-critics.sh), every critic's probe scripts and pages (the first to the twelfth), and the
# twelfth round's own pages, rerun against the current web3d/pkg/, with every result written under
# this folder (rerun/ and results/rerun-*.json) and the home directory written "~". The twelfth
# critic's pages are served from its own folder, ../critic12/site/, which is left as it is.
#   PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build bash rerun-critics.sh <port>
# BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
p1="$here/.."
out="$here/rerun"
port="$1"
mkdir -p "$out/critic1" "$out/critic2" "$here/results"
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
run critic9-main node "$p1/critic9/run9.mjs" "$port" "$out/run9.json"
run critic9-batch2 node "$p1/critic9/run9.mjs" "$port" "$out/run9-batch2.json" batch2.html
run critic9-frame-first node "$p1/critic9/run9.mjs" "$port" "$out/run9-frame-first.json" frame-first.html
for page in state state2 pm notop xtop bf; do
  run "critic10-$page" node "$here/run10-copy.mjs" "$port" "$((port + 1))" "$page"
done
for page in realm observe reset nowarn intent; do
  run "critic11-$page" node "$here/rerun-tools/run.mjs" "$port" "$page.html" --site evidence/phase1/critic11/site --out "rerun-critic11-$page" --timeout 170000
done
run critic11-dd node "$here/rerun-tools/run.mjs" "$port" dd.html --site evidence/phase1/critic11/site --out rerun-critic11-dd --host a.localhost --timeout 170000
run round12-intent node "$here/rerun-tools/run.mjs" "$port" intent.html --site evidence/phase1/critic11-rerun/site --out rerun-round12-intent --timeout 170000
run round12-dd node "$here/rerun-tools/run.mjs" "$port" dd.html --site evidence/phase1/critic11-rerun/site --out rerun-round12-dd --host a.localhost --timeout 170000
run round12-facts node "$here/rerun-tools/run.mjs" "$port" facts.html --site evidence/phase1/critic11-rerun/site --out rerun-round12-facts --timeout 170000
run round12-listeners node "$here/rerun-tools/run.mjs" "$port" listeners.html --site evidence/phase1/critic11-rerun/site --out rerun-round12-listeners --timeout 170000
for page in adopt blank counter docopen f4 midcreate nav override per transfer; do
  run "critic12-$page" node "$here/rerun-tools/run.mjs" "$port" "$page.html" --site evidence/phase1/critic12/site --out "rerun-critic12-$page" --timeout 170000
done
run critic12-dd2 node "$here/rerun-tools/run.mjs" "$port" dd2.html --site evidence/phase1/critic12/site --out rerun-critic12-dd2 --host a.localhost --timeout 170000
run critic12-bf node "$here/rerun-tools/run.mjs" "$port" bf.html --site evidence/phase1/critic12/site --out rerun-critic12-bf --bfcache --timeout 170000
grep -rl --exclude='*.png' "$HOME" "$out" "$here/results" 2>/dev/null | while read -r f; do sed -i '' -e "s|$HOME|~|g" "$f"; done
echo "all done"
