#!/usr/bin/env bash
# The fifteenth critic's controls of the rendered check (BSD-3-Clause; see the repository's LICENSE).
# Today's demo/ and probe page (with tools/three-reference.txt, which probe R0 reads), and pkg/ with
# one file swapped, copied to a temporary directory and checked there by its own copy of check.mjs
# (which serves the directory it sits in), so nothing in the repository changes, with the gates'
# settings (--seconds 10 --steady 60):
#   at-9a4ab9c7: pkg/web3d.js as the fourteenth critic judged it (git show 9a4ab9c7), to see that
#     probe NV's new case (a removed frame whose page declared `function closed() {}`) fails there;
#   fix-pkg: pkg/web3d.js with critic 15's trial fix of the restored listener (../fix-pkg/), to see
#     that the 80 probes still pass with it.
# Output in at-9a4ab9c7/ and fix-pkg/ beside this script, the home directory written <home>.
#   PLAYWRIGHT_CORE=.../playwright-core/index.mjs bash run.sh <port>
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
web3d="$(cd "$here/../../../.." && pwd)"
port="$1"
one() {
  local name="$1" src="$2"
  local tmp
  tmp="$(mktemp -d)"
  cp -R "$web3d/demo" "$web3d/pkg" "$tmp/"
  mkdir "$tmp/tools" && cp "$web3d/tools/three-reference.txt" "$tmp/tools/" # probe R0's reference
  cp "$src" "$tmp/pkg/web3d.js"
  mkdir -p "$here/$name"
  { echo "# pkg/web3d.js sha256 $(shasum -a 256 "$tmp/pkg/web3d.js" | cut -c1-8), wasm $(shasum -a 256 "$tmp/pkg/loadngo_web3d.wasm" | cut -c1-8), gl.js $(shasum -a 256 "$tmp/pkg/gl.js" | cut -c1-8); demo/ and probe page as in the working tree"; } > "$here/$name/check.log"
  node "$tmp/demo/check.mjs" --port "$port" --out "$here/$name" --seconds 10 --steady 60 >> "$here/$name/check.log" 2>&1
  echo ">>> exit $?" >> "$here/$name/check.log"
  rm -rf "$tmp"
  rm -f "$here/$name"/demo-*.png
  grep -rl "$HOME" "$here/$name" 2>/dev/null | while read -r f; do sed -i '' -e "s|$HOME|<home>|g" "$f"; done
  grep -E "^probes:|FAIL|>>> exit" "$here/$name/check.log"
}
old="$(mktemp)"
git -C "$web3d" show 9a4ab9c7:web3d/js/web3d.js > "$old"
one at-9a4ab9c7 "$old"
rm -f "$old"
one fix-pkg "$here/../fix-pkg/web3d.js"
