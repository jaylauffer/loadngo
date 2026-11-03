#!/usr/bin/env bash
# The phase-1 gates, all of them, recorded in gates.txt beside this script with the home directory
# written as <home>: fmt; clippy as CI runs it (the macOS row's workspace command and the wasm32
# step) and for the crate on wasm32, aarch64 Linux, Windows MSVC and the host; the tests, debug and
# release; build.sh twice in place and once from an empty target directory, compared byte for byte,
# with no local path in pkg/; the three.js reference check; the rendered check, which must pass; and
# the rendered check with --blind, which must fail. Uses the Mac's GPU: claim it first.
#   PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_DIR=.../node_modules/three \
#     bash web3d/evidence/phase1/gates.sh <port> <port for --blind>
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
web3d="$(cd "$here/../.." && pwd)"
ws="$(cd "$web3d/.." && pwd)"
out="$here/gates.txt"
port="$1"
blind_port="$2"
empty=""
red() { if [ -n "$empty" ]; then sed -e "s|$empty|<empty>|g" -e "s|$HOME|<home>|g"; else sed -e "s|$HOME|<home>|g"; fi; }
step() {
  { echo; echo "=================================================================="; echo "\$ ${*//$ws\//}"; } >> "$out"
  "$@" 2>&1 | red >> "$out"
  local code=${PIPESTATUS[0]}
  echo ">>> exit $code" >> "$out"
  echo "${*//$ws\//}: exit $code"
}
cd "$ws"
{
  echo "# loadngo-web3d phase 1 gates, after the ninth critic's round ($(date '+%Y-%m-%d %H:%M %z')); loadngo $(git rev-parse --short HEAD) ($(git branch --show-current)) plus uncommitted changes in $(git status --porcelain -- web3d docs/WEB3D.md | awk '{print $2}' | sed -e 's|/.*||' -e 's|^docs$|docs/WEB3D.md|' | sort -u | tr '\n' ' ')"
  echo "# $(rustc --version); $(cargo --version); $(wasm-opt --version); node $(node --version); macOS $(sw_vers -productVersion)"
  echo "# CARGO_TARGET_DIR is the workspace's own target/ except where a step says otherwise. PLAYWRIGHT_CORE and THREE_DIR (a three.js r180 package) are set in the environment."
} | red > "$out"
step cargo fmt --check
step cargo clippy --locked --workspace --all-targets --all-features --exclude loadngo-gfx-dx12 --exclude gui-win32 --exclude proactor-harness -- -D warnings
step cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings
step cargo clippy --locked --offline -p loadngo-web3d --target wasm32-unknown-unknown --all-targets --all-features -- -D warnings
step cargo clippy --locked --offline -p loadngo-web3d --target aarch64-unknown-linux-gnu --all-targets --all-features -- -D warnings
step cargo clippy --locked --offline -p loadngo-web3d --target x86_64-pc-windows-msvc --all-targets --all-features -- -D warnings
step cargo clippy --locked --offline -p loadngo-web3d --all-targets --all-features -- -D warnings
step cargo test --locked -p loadngo-web3d
step cargo test --locked -p loadngo-web3d --release

# Three builds: twice in place, once from an empty target directory. Each pkg/ is hashed; all three
# must match, and nothing in pkg/ may name this machine.
sums=()
for i in 1 2 3; do
  if [ "$i" = 3 ]; then
    empty="$(mktemp -d)"
    { echo; echo "== build $i: CARGO_TARGET_DIR is a new empty directory ($(ls -A "$empty" | wc -l | tr -d ' ') entries)"; } >> "$out"
    CARGO_TARGET_DIR="$empty" step web3d/build.sh
    rm -rf "$empty"
    empty=""
  else
    { echo; echo "== build $i: the workspace's target/"; } >> "$out"
    step web3d/build.sh
  fi
  step shasum -a 256 web3d/pkg/loadngo_web3d.wasm web3d/pkg/gl.js web3d/pkg/web3d.js web3d/pkg/web3d.d.ts
  sums+=("$(shasum -a 256 web3d/pkg/* | awk '{print $1}' | tr '\n' ' ')")
done
{
  echo; echo "=================================================================="
  if [ "${sums[0]}" = "${sums[1]}" ] && [ "${sums[1]}" = "${sums[2]}" ]; then echo "== the three builds' pkg/ files are byte-identical"; else echo "== THE BUILDS DIFFER"; fi
  echo "== local paths in pkg/ (strings -n 4 on the wasm; grep -a on every file): expect none"
  if strings -n 4 web3d/pkg/loadngo_web3d.wasm | grep -E "/Users/|/home/|$HOME|\.cargo|rustup" || grep -a -l -E "/Users/|/home/" web3d/pkg/*; then echo "FOUND"; else echo "none"; fi
} | red >> "$out"
step node web3d/tools/three-reference.mjs --check
step ruby -e 'require "yaml"; d=YAML.load_file(".github/workflows/ci.yml"); s=d["jobs"]["check"]["steps"].find{|x| x["name"]=="cargo clippy (web3d, wasm32)"}; p s'
# The rendered check, and its blind run; their logs are also kept beside their JSON.
step bash -c "set -o pipefail; node web3d/demo/check.mjs --port $port --out web3d/evidence/phase1 --seconds 10 --steady 60 2>&1 | tee web3d/evidence/phase1/rendered-check.log"
step bash -c "set -o pipefail; node web3d/demo/check.mjs --port $blind_port --out web3d/evidence/phase1/blind-control --seconds 10 --steady 60 --blind 2>&1 | tee web3d/evidence/phase1/blind-control/check-blind.log"
rm -f "$here"/blind-control/demo-*.png # the blind run's screenshots repeat the main run's
grep -l "$HOME" "$here/rendered-check.json" "$here/rendered-check.log" "$here/blind-control/rendered-check.json" "$here/blind-control/check-blind.log" 2>/dev/null | while read -r f; do sed -i '' -e "s|$HOME|<home>|g" "$f"; done
echo "wrote ${out//$HOME/<home>}"
