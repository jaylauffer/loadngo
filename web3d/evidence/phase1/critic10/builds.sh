#!/usr/bin/env bash
# The tenth critic's build gates: build.sh twice in place and once from an empty target directory,
# each pkg/ hashed; a scan of pkg/ for local paths (strings and grep -a); and three-reference --check.
# Output with the home directory written <home>.
#   THREE_DIR=.../node_modules/three bash builds.sh > builds.txt
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ws="$(cd "$here/../../../.." && pwd)"
cd "$ws"
red() { sed -e "s|$HOME|<home>|g" -e "s|/private/var/folders/[^ ]*|<tmp>|g" -e "s|/var/folders/[^ ]*|<tmp>|g"; }
echo "# loadngo $(git rev-parse --short HEAD) ($(git branch --show-current)); $(rustc --version); $(wasm-opt --version)"
for i in 1 2 3; do
  if [ "$i" = 3 ]; then
    empty="$(mktemp -d)"
    echo "== build $i: an empty target directory ($(ls -A "$empty" | wc -l | tr -d ' ') entries)"
    CARGO_TARGET_DIR="$empty" web3d/build.sh 2>&1 | grep -v -E '^\s+Compiling|^\s+Finished' | red
    echo ">>> exit ${PIPESTATUS[0]}"
    rm -rf "$empty"
  else
    echo "== build $i: the workspace's target/"
    web3d/build.sh 2>&1 | grep -v -E '^\s+Compiling|^\s+Finished' | red
    echo ">>> exit ${PIPESTATUS[0]}"
  fi
  shasum -a 256 web3d/pkg/* | tee "$here/.sums-$i"
done
if cmp -s "$here/.sums-1" "$here/.sums-2" && cmp -s "$here/.sums-2" "$here/.sums-3"; then echo "== the three builds are byte-identical"; else echo "== THE BUILDS DIFFER"; fi
rm -f "$here"/.sums-*
echo "== local paths in pkg/ (strings -n 4, then grep -a): expect none"
user="$(basename "$HOME")"
strings -n 4 web3d/pkg/loadngo_web3d.wasm | grep -E "/Users/|/home/|$HOME|$user|\.cargo|rustup|/private/|/var/folders" | red || echo "strings: none"
grep -a -c -E "/Users/|/home/|$user|/private/|/var/folders" web3d/pkg/* | red
echo "== imports and exports of the module"
node -e '
const fs = require("fs");
const m = new WebAssembly.Module(fs.readFileSync("web3d/pkg/loadngo_web3d.wasm"));
const im = WebAssembly.Module.imports(m), ex = WebAssembly.Module.exports(m);
console.log("imports", im.length, "modules", [...new Set(im.map(i => i.module))].join(","));
console.log("exports", ex.length, "functions", ex.filter(e => e.kind === "function").length, ex.filter(e => e.kind !== "function").map(e => e.name + ":" + e.kind).join(","));
'
echo "== sizes"
for f in web3d/pkg/loadngo_web3d.wasm web3d/pkg/gl.js web3d/pkg/web3d.js; do printf '%-30s %8d gzip -9 %7d\n' "$f" "$(wc -c < "$f")" "$(gzip -9 -c "$f" | wc -c)"; done
echo "== three-reference --check"
node web3d/tools/three-reference.mjs --check 2>&1 | red
echo ">>> exit ${PIPESTATUS[0]}"
