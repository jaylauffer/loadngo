#!/usr/bin/env bash
# The sixteenth critic's run of the phase-1 gates, by the same commands gates.sh runs, without
# overwriting gates.txt or the committed rendered-check files. Output goes to gates-critic16.txt
# beside this script, with the home directory written as <home>. The rendered check is run
# separately (see the critic16 README), into critic16/gates/rendered/.
#   bash web3d/evidence/phase1/critic16/gates/run-gates.sh
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ws="$(cd "$here/../../../../.." && pwd)"
out="$here/gates-critic16.txt"
red() { sed -e "s|${THREE_DIR:-/nonexistent}|<a three.js r180 package>|g" -e "s|$HOME|<home>|g" -e 's|/var/folders/[A-Za-z0-9_/.]*|<empty>|g'; }
failed=()
run() {
  { echo; echo "=================================================================="; echo "\$ $*"; } | red >> "$out"
  "$@" 2>&1 | red >> "$out"
  local code=${PIPESTATUS[0]}
  echo ">>> exit $code" >> "$out"
  echo "$*: exit $code" | red
  [ "$code" -eq 0 ] || failed+=("$*")
}
cd "$ws"
{
  echo "# critic16 gates, $(date '+%Y-%m-%d %H:%M %z'); loadngo $(git rev-parse --short HEAD) ($(git branch --show-current)); status: $(git status --porcelain -- web3d docs/WEB3D.md .github | grep -v "critic16" | wc -l | tr -d ' ') changed paths outside critic16"
  echo "# $(rustc --version); $(cargo --version); $(wasm-opt --version); node $(node --version)"
} | red > "$out"
run cargo fmt --check
run cargo clippy --locked --workspace --all-targets --all-features --exclude loadngo-gfx-dx12 --exclude gui-win32 --exclude proactor-harness -- -D warnings
run cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings
run cargo clippy --locked --offline -p loadngo-web3d --target wasm32-unknown-unknown --all-targets --all-features -- -D warnings
run cargo clippy --locked --offline -p loadngo-web3d --target aarch64-unknown-linux-gnu --all-targets --all-features -- -D warnings
run cargo clippy --locked --offline -p loadngo-web3d --target x86_64-pc-windows-msvc --all-targets --all-features -- -D warnings
run cargo clippy --locked --offline -p loadngo-web3d --all-targets --all-features -- -D warnings
run cargo test --locked -p loadngo-web3d
run cargo test --locked -p loadngo-web3d --release
sums=()
for i in 1 2 3; do
  if [ "$i" = 3 ]; then
    empty="$(mktemp -d)"
    echo "== build 3: empty CARGO_TARGET_DIR ($(ls -A "$empty" | wc -l | tr -d ' ') entries)" >> "$out"
    CARGO_TARGET_DIR="$empty" run web3d/build.sh
    rm -rf "$empty"
  else
    echo "== build $i: workspace target/" >> "$out"
    run web3d/build.sh
  fi
  s="$(shasum -a 256 web3d/pkg/* | red)"
  echo "$s" >> "$out"
  sums+=("$(echo "$s" | awk '{print $1}' | tr '\n' ' ')")
done
if [ "${sums[0]}" = "${sums[1]}" ] && [ "${sums[1]}" = "${sums[2]}" ]; then echo "== three builds byte-identical" >> "$out"; else echo "== BUILDS DIFFER" >> "$out"; failed+=("identical builds"); fi
echo "== local paths in pkg/ (strings -a -n 4 on the wasm, grep -a on all): expect none" >> "$out"
found="$( { strings -a -n 4 web3d/pkg/loadngo_web3d.wasm | grep -E "/Users/|/home/|$HOME|\.cargo|rustup|/private/|/var/folders"; grep -a -l -E "/Users/|/home/|/private/|/var/folders" web3d/pkg/*; } | red)"
if [ -n "$found" ]; then echo "FOUND: $found" >> "$out"; failed+=("local path"); else echo "none" >> "$out"; fi
echo "== wasm sha256 against the claimed 4d6594b5" >> "$out"
shasum -a 256 web3d/pkg/loadngo_web3d.wasm | awk '{print $1}' >> "$out"
run env THREE_DIR="${THREE_DIR:-}" node web3d/tools/three-reference.mjs --check
run env WASM_CLIPPY="cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings" ruby web3d/evidence/phase1/ci-step.rb .github/workflows/ci.yml
{ echo; if [ ${#failed[@]} -eq 0 ]; then echo "== every gate here held"; else echo "== DID NOT HOLD:"; printf '  %s\n' "${failed[@]}"; fi; } >> "$out"
echo "failed: ${#failed[@]}"
