#!/usr/bin/env bash
# The sixteenth critic's controls of the rendered check (a copy of the fifteenth critic's) (BSD-3-Clause; see the repository's LICENSE).
# Today's demo/ and probe page (with tools/three-reference.txt, which probe R0 reads), and pkg/ with
# one file swapped, copied to a temporary directory and checked there by its own copy of check.mjs
# (which serves the directory it sits in), so nothing in the repository changes, with the gates'
# settings (--seconds 10 --steady 60):
#   fix-pkg: pkg/web3d.js with critic 16's first trial (../fix-pkg/: the restored listener compares
#     the loss count the current renderer took its context at), to see that the 80 probes pass;
#   fix-pkg-2: the second trial (../fix-pkg-2/: that, and each waiting onContextRestored tagged with
#     the loss whose restore it waits for), the same;
#   fix-pkg-3: the third trial (../fix-pkg-3/: the first, and the restored listener leaves every hook
#     waiting when the context reads lost again by the time it runs), the same.
# Output in fix-pkg/, fix-pkg-2/ and fix-pkg-3/ beside this script, the home directory written
# <home>. Names after the port run only those.
#   PLAYWRIGHT_CORE=.../playwright-core/index.mjs bash run.sh <port> [name ...]
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
web3d="$(cd "$here/../../../.." && pwd)"
port="$1"
shift
names=("$@")
[ ${#names[@]} -gt 0 ] || names=(fix-pkg fix-pkg-2 fix-pkg-3)
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
for name in "${names[@]}"; do one "$name" "$here/../$name/web3d.js"; done
