#!/usr/bin/env bash
# The sixteenth round's control of the rendered check, a copy of the fifteenth critic's
# (../../critic15/control/run.sh). BSD-3-Clause; see the repository's LICENSE.
# Today's demo/ and probe page (with tools/three-reference.txt, which probe R0 reads), and pkg/ with
# one file swapped, copied to a temporary directory and checked there by its own copy of check.mjs
# (which serves the directory it sits in), so nothing in the repository changes, with the gates'
# settings (--seconds 10 --steady 60):
#   at-226b7c92: pkg/web3d.js as the fifteenth critic judged it (git show 226b7c92), to see that
#     probe LR's new case (a renderer made in the page's restored listener ahead of web3d's) fails
#     there.
# Output in at-226b7c92/ beside this script, the home directory written <home>.
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
git -C "$web3d" show 226b7c92:web3d/js/web3d.js > "$old"
one at-226b7c92 "$old"
rm -f "$old"
