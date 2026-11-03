#!/usr/bin/env bash
# Seventh critic: build.sh four times and compare pkg/ with the pkg found before any build.
#   1, 2: in place, the workspace's target dir;
#   3:    in place, an empty target dir;
#   4:    a `git archive` copy of HEAD at another path, an empty target dir (path independence).
# Usage: builds.sh <scratch-dir> <dir-holding-the-pkg-found>
set -uo pipefail
ws="$(cd "$(dirname "$0")/../../../.." && pwd)"
scratch="$1"; found="$2"
hashes() { (cd "$1" && shasum -a 256 gl.js loadngo_web3d.wasm web3d.d.ts web3d.js); }
echo "== loadngo $(git -C "$ws" rev-parse --short HEAD); $(rustc --version); $(wasm-opt --version)"
echo "== the pkg found"; hashes "$found" | tee "$scratch/h0"
for i in 1 2; do
  echo; echo "== build $i: in place, workspace target dir"
  "$ws/web3d/build.sh" 2>&1 | grep -v '^\s*Compiling\|^\s*Finished' | sed "s#$HOME#~#g"
  hashes "$ws/web3d/pkg" | tee "$scratch/h$i"
  cmp -s "$scratch/h0" "$scratch/h$i" && echo "build $i: identical to the pkg found" || echo "build $i: DIFFERS"
done
echo; echo "== build 3: in place, empty target dir"
rm -rf "$scratch/target-b3"
CARGO_TARGET_DIR="$scratch/target-b3" "$ws/web3d/build.sh" 2>&1 | grep -v '^\s*Compiling\|^\s*Finished' | sed "s#$scratch#<scratch>#g; s#$HOME#~#g"
hashes "$ws/web3d/pkg" | tee "$scratch/h3"
cmp -s "$scratch/h0" "$scratch/h3" && echo "build 3: identical to the pkg found" || echo "build 3: DIFFERS"
echo; echo "== build 4: git archive of HEAD at another path, empty target dir"
rm -rf "$scratch/copy" "$scratch/target-b4"; mkdir -p "$scratch/copy/elsewhere-ws"
git -C "$ws" archive HEAD | tar -x -C "$scratch/copy/elsewhere-ws"
CARGO_TARGET_DIR="$scratch/target-b4" "$scratch/copy/elsewhere-ws/web3d/build.sh" 2>&1 | grep -v '^\s*Compiling\|^\s*Finished' | sed "s#$scratch#<scratch>#g; s#$HOME#~#g"
hashes "$scratch/copy/elsewhere-ws/web3d/pkg" | tee "$scratch/h4"
cmp -s "$scratch/h0" "$scratch/h4" && echo "build 4: identical to the pkg found" || echo "build 4: DIFFERS"
echo; echo "== path-like strings in the wasm (len >= 6 containing / )"
strings -n 6 "$ws/web3d/pkg/loadngo_web3d.wasm" | grep '/' | sort -u | sed "s#$HOME#~#g"
echo "== local-path grep (Users, home, scratch, private/tmp) in pkg:"
grep -l -E "/Users/|/home/|/private/tmp|scratchpad" "$ws/web3d/pkg"/* || echo "none"
