#!/usr/bin/env bash
# The phase-1 gates, all of them, recorded in gates.txt beside this script with the home directory
# written as <home>: fmt; clippy as CI runs it (the macOS row's workspace command and the wasm32
# step) and for the crate on wasm32, aarch64 Linux, Windows MSVC and the host; the tests, debug and
# release; build.sh twice in place and once from an empty target directory, compared byte for byte,
# with no local path in pkg/; the three.js reference check; the CI step's YAML; the rendered check,
# which must pass; and the rendered check with --blind, which must fail on its positive control
# alone, which check.mjs reports as exit 3 (any other failure, such as a port taken or Chrome
# missing, exits 1, and is not counted as the blind run holding). Each gate is judged by its exit
# status (and the builds by their hashes); the script exits 1 if any gate did not hold, and the last
# lines of gates.txt name each one that did not. It does not run the critics' probe scripts:
# critic11-rerun/rerun-critics.sh does. Uses the Mac's GPU: claim it first.
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
failed=()
red() { if [ -n "$empty" ]; then sed -e "s|$empty|<empty>|g" -e "s|$HOME|<home>|g"; else sed -e "s|$HOME|<home>|g"; fi; }
# Runs a gate and records it, expecting the exit status given first: `step` expects 0,
# `step_blind` (the blind run) expects exactly 3, check.mjs's code for "only the positive control
# failed".
run() {
  local want="$1"
  shift
  { echo; echo "=================================================================="; echo "\$ ${*//$ws\//}"; } >> "$out"
  "$@" 2>&1 | red >> "$out"
  local code=${PIPESTATUS[0]}
  local held=no
  [ "$code" -eq "$want" ] && held=yes
  echo ">>> exit $code (must be $want: $([ $held = yes ] && echo held || echo DID NOT HOLD))" >> "$out"
  echo "${*//$ws\//}: exit $code, $([ $held = yes ] && echo held || echo DID NOT HOLD)"
  [ $held = yes ] || failed+=("${*//$ws\//}")
}
step() { run 0 "$@"; }
step_blind() { run 3 "$@"; }
cd "$ws"
changes="$(git status --porcelain -- web3d docs/WEB3D.md | awk '{print $2}' | sed -e 's|^\(web3d/[^/]*\)/.*|\1|' | sort -u | tr '\n' ' ')"
{
  echo "# loadngo-web3d phase 1 gates, $(date '+%Y-%m-%d %H:%M %z'); run on the working tree of loadngo $(git rev-parse --short HEAD) ($(git branch --show-current)); $([ -n "$changes" ] && echo "changed from that commit: $changes" || echo "no changes from that commit")"
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
identical=no
[ "${sums[0]}" = "${sums[1]}" ] && [ "${sums[1]}" = "${sums[2]}" ] && identical=yes
{
  echo; echo "=================================================================="
  if [ $identical = yes ]; then echo "== the three builds' pkg/ files are byte-identical"; else echo "== THE BUILDS DIFFER"; fi
  echo "== local paths in pkg/ (strings -n 4 on the wasm; grep -a on every file): expect none"
} >> "$out"
[ $identical = yes ] || failed+=("the three builds are byte-identical")
found="$( { strings -n 4 web3d/pkg/loadngo_web3d.wasm | grep -E "/Users/|/home/|$HOME|\.cargo|rustup"; grep -a -l -E "/Users/|/home/" web3d/pkg/*; } | red)"
if [ -n "$found" ]; then
  { echo "FOUND:"; echo "$found"; } >> "$out"
  failed+=("no local path in pkg/")
else
  echo "none" >> "$out"
fi
step node web3d/tools/three-reference.mjs --check
step ruby -e 'require "yaml"; d=YAML.load_file(".github/workflows/ci.yml"); s=d["jobs"]["check"]["steps"].find{|x| x["name"]=="cargo clippy (web3d, wasm32)"}; abort "no such step" unless s; p s'
# The rendered check, which must pass, and its blind run, which must fail on the control alone (exit
# 3); their logs are also kept beside their JSON.
step bash -c "set -o pipefail; node web3d/demo/check.mjs --port $port --out web3d/evidence/phase1 --seconds 10 --steady 60 2>&1 | tee web3d/evidence/phase1/rendered-check.log"
step_blind bash -c "set -o pipefail; node web3d/demo/check.mjs --port $blind_port --out web3d/evidence/phase1/blind-control --seconds 10 --steady 60 --blind 2>&1 | tee web3d/evidence/phase1/blind-control/check-blind.log"
rm -f "$here"/blind-control/demo-*.png # the blind run's screenshots repeat the main run's
grep -l "$HOME" "$here/rendered-check.json" "$here/rendered-check.log" "$here/blind-control/rendered-check.json" "$here/blind-control/check-blind.log" 2>/dev/null | while read -r f; do sed -i '' -e "s|$HOME|<home>|g" "$f"; done
{
  echo; echo "=================================================================="
  if [ ${#failed[@]} -eq 0 ]; then
    echo "== every gate held"
  else
    echo "== GATES THAT DID NOT HOLD:"
    printf '  %s\n' "${failed[@]}"
  fi
} | red >> "$out"
echo "wrote ${out//$HOME/<home>}; $([ ${#failed[@]} -eq 0 ] && echo "every gate held" || echo "${#failed[@]} did not hold")"
[ ${#failed[@]} -eq 0 ]
