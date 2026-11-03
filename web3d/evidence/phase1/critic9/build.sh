#!/usr/bin/env bash
# Ninth critic: build.sh twice in place and once from an empty target directory; compare pkg/.
set -u
cd "$(dirname "$0")/../../.."   # web3d/
red() { sed -e "s|$HOME|~|g"; }
echo "== pkg found before building:"; shasum -a 256 pkg/* | red
for i in 1 2; do
  echo; echo "== build $i (workspace target dir)"
  ./build.sh 2>&1 | red; echo "exit ${PIPESTATUS[0]}"
  shasum -a 256 pkg/* | red
done
empty="$(mktemp -d)"
echo; echo "== build 3 (empty target dir: a new mktemp -d)"
ls -A "$empty" | wc -l | sed 's/^/entries before: /'
CARGO_TARGET_DIR="$empty" ./build.sh 2>&1 | red | sed -e "s|$empty|<empty>|g"; echo "exit ${PIPESTATUS[0]}"
shasum -a 256 pkg/* | red
echo; echo "== local paths in pkg (strings -n 4, grep -a): expect none"
strings -n 4 pkg/loadngo_web3d.wasm | grep -E "/Users/|/home/|$HOME|\.cargo|rustup" || echo none
grep -a -c -E "/Users/|/home/" pkg/* || true
echo "== remapped prefixes seen in the wasm:"; strings -n 6 pkg/loadngo_web3d.wasm | grep -oE "^/(rustc|loadngo|cargo)/[^ ]{0,40}" | sort -u | head
rm -rf "$empty"
