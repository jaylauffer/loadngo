#!/usr/bin/env bash
# The YAML gate (../../ci-step.rb) against the real workflow and against copies of it, each changed
# one way, made in a temporary directory outside the repository. The real workflow and the positive
# controls must pass (exit 0). Every other copy must be refused: exit 2 AND a `ci-step: refused:`
# line holding the reason named for that row; exit 1 (a Ruby error), any other exit, or another
# reason does not count. Three broken copies of the gate (a missing library, a syntax error, a
# refusal with the wrong reason) run on a copy the real gate refuses, and the verdict this script
# gives each must be "not a refusal" or "another reason": they show a broken gate cannot pass for a
# refusal here. Writes results.txt beside this script, with the repository path, the temporary
# directory and the home directory left out; exits 0 only if every row held.
#   bash web3d/evidence/phase1/critic14-rerun/yaml-gate/run.sh   (from anywhere; uses ruby and git)
# BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ws="$(cd "$here/../../../../.." && pwd)"
gate="$ws/web3d/evidence/phase1/ci-step.rb"
real="$ws/.github/workflows/ci.yml"
export WASM_CLIPPY="cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
out="$here/results.txt"
rows=0
held=0
red() { sed -e "s|$tmp|<tmp>|g" -e "s|$ws/||g" -e "s|$HOME|<home>|g"; }

# make <name> <ruby>: a copy of the workflow with one change. The ruby sees the parsed workflow `d`,
# its job `j`, the step `s` and the triggers `on`.
make() {
  ruby -ryaml -e "d = YAML.load_file(ARGV[0]); j = d['jobs']['check']; s = j['steps'].find { |x| x['name'] == 'cargo clippy (web3d, wasm32)' }; on = d['on'] || d[true]; $2; File.write(ARGV[1], d.to_yaml)" "$real" "$tmp/$1.yml" \
    || { echo "could not make $1" >&2; exit 1; }
}

# verdict <gate> <workflow> <reason>: pass, refused (exit 2 with the reason), another-reason (exit 2
# without it) or not-a-refusal (any other exit). Leaves the gate's first output line in $line.
verdict() {
  local o code
  o="$(ruby "$1" "$2" 2>&1)"; code=$?
  local refusal
  refusal="$(printf '%s\n' "$o" | grep -m1 -F 'ci-step: refused: ')"
  line="exit $code: $( { [ -n "$refusal" ] && echo "$refusal"; } || printf '%s\n' "$o" | head -1)"
  if [ "$code" = 0 ]; then v=pass
  elif [ "$code" = 2 ] && [ -n "$refusal" ] && [[ "$refusal" == *"$3"* ]]; then v=refused
  elif [ "$code" = 2 ] && [ -n "$refusal" ]; then v=another-reason
  else v=not-a-refusal
  fi
}

# row <name> <wanted verdict> <reason> <workflow> [gate]: one row of results.txt.
row() {
  local name="$1" want="$2" reason="$3" file="$4" g="${5:-$gate}"
  verdict "$g" "$file" "$reason"
  rows=$((rows + 1))
  local ok="DID NOT HOLD"
  [ "$v" = "$want" ] && { ok=held; held=$((held + 1)); }
  local must="must pass"
  [ "$want" = refused ] && must="must be refused for \"$reason\""
  [ "$want" = not-a-refusal ] && must="broken gate: must be judged not-a-refusal"
  [ "$want" = another-reason ] && must="broken gate: must be judged another-reason, not \"$reason\""
  echo "$name: $v ($must: $ok): $line" | red
}
pass() { make "$1" "$2"; row "$1" pass "" "$tmp/$1.yml"; }
refused() { make "$1" "$3"; row "$1" refused "$2" "$tmp/$1.yml"; }

{
  echo "# The YAML gate, ci-step.rb, on the real workflow and on changed copies ($(date '+%Y-%m-%d %H:%M %z'))"
  echo "## Must pass"
  row real pass "" "$real"
  pass round-trip "nil"
  pass allowed-env-added "s['env'] = { 'CARGO_TERM_COLOR' => 'never' }; j['env'] = { 'LOADNGO_CAS_MIN_FREE_GIB' => 1 }"
  pass harmless-paths-ignore "on['push']['paths-ignore'] = ['docs/**', '*.md']; on['pull_request']['paths-ignore'] = ['docs/**', '*.md']"

  echo "## Round 13's eight"
  refused exit-0-first "the step must run exactly" "s['run'] = \"exit 0\n\" + s['run']"
  refused true-after "the step must run exactly" "s['run'] = s['run'] + \"true\n\""
  refused or-true "the step must run exactly" "s['run'] = s['run'].sub('-D warnings', '-D warnings || true')"
  refused no-all-targets "the step must run exactly" "s['run'] = s['run'].sub(' --all-targets', '')"
  refused step-continue-on-error "the step sets continue-on-error" "s['continue-on-error'] = true"
  refused job-continue-on-error "the job sets continue-on-error" "j['continue-on-error'] = true"
  refused step-shell "the step sets a shell" "s['shell'] = 'bash {0}'"
  refused workflow-default-shell "the workflow sets a default shell" "d['defaults'] = { 'run' => { 'shell' => 'bash {0}' } }"

  echo "## Critic 14's seven"
  refused step-env-cap-lints "the step sets env RUSTFLAGS" "s['env'] = { 'RUSTFLAGS' => '--cap-lints warn' }"
  refused job-env-cap-lints "the job sets env RUSTFLAGS" "j['env'] = { 'RUSTFLAGS' => '--cap-lints warn' }"
  refused workflow-env-cap-lints "the workflow sets env RUSTFLAGS" "d['env']['RUSTFLAGS'] = '--cap-lints warn'"
  refused earlier-step-sets-github-env "mentions GITHUB_ENV" "j['steps'].insert(j['steps'].index(s), { 'name' => 'x', 'run' => 'echo RUSTFLAGS=--cap-lints warn >> \"\$GITHUB_ENV\"' })"
  refused job-if-false "the job sets if: false" "j['if'] = false"
  refused push-and-pr-on-no-branch "push has the filter branches" "on['push']['branches'] = ['no-such-branch']; on['pull_request']['branches'] = ['no-such-branch']"
  refused paths-ignore-everything "push paths-ignore \"**\" matches" "on['push']['paths-ignore'] = ['**']; on['pull_request']['paths-ignore'] = ['**']"

  echo "## One per new rule"
  refused earlier-step-writes-github-path "mentions GITHUB_PATH" "j['steps'].insert(j['steps'].index(s), { 'name' => 'x', 'run' => 'echo \"\$HOME/bin\" >> \"\$GITHUB_PATH\"' })"
  refused job-env-cargo-encoded-rustflags "the job sets env CARGO_ENCODED_RUSTFLAGS" "j['env'] = { 'CARGO_ENCODED_RUSTFLAGS' => \"--cap-lints\x1fwarn\" }"
  refused step-env-cargo-build-rustflags "the step sets env CARGO_BUILD_RUSTFLAGS" "s['env'] = { 'CARGO_BUILD_RUSTFLAGS' => '--cap-lints warn' }"
  refused step-env-target-rustflags "the step sets env CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS" "s['env'] = { 'CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS' => '--cap-lints warn' }"
  refused workflow-env-rustc-wrapper "the workflow sets env RUSTC_WRAPPER" "d['env']['RUSTC_WRAPPER'] = '/bin/true'"
  refused workflow-env-bash-env "the workflow sets env BASH_ENV" "d['env']['BASH_ENV'] = 'x.sh'"
  refused push-paths-filter "push has the filter paths" "on['push'].delete('paths-ignore'); on['push']['paths'] = ['docs/**']"
  refused pr-types-filter "pull_request has the filter types" "on['pull_request']['types'] = ['labeled']"
  refused pr-branches-ignore "pull_request has the filter branches-ignore" "on['pull_request']['branches-ignore'] = ['**']"
  refused push-tags-filter "push has the filter tags" "on['push']['tags'] = ['v*']"
  refused pr-paths-ignore-cargo-lock "pull_request paths-ignore \"Cargo.lock\" matches Cargo.lock" "on['pull_request']['paths-ignore'] << 'Cargo.lock'"
  refused push-paths-ignore-rs "web3d/src/web.rs" "on['push']['paths-ignore'] << '**/*.rs'"
  refused job-if-always "the job sets if: \"always()\"" "j['if'] = 'always()'"

  echo "## The pattern matcher"
  refused paths-ignore-star "push paths-ignore \"*\" matches .github/workflows/ci.yml, Cargo.lock, Cargo.toml" "on['push']['paths-ignore'] << '*'"
  refused paths-ignore-web3d "push paths-ignore \"web3d/**\" matches web3d/Cargo.toml, web3d/src/gl.rs" "on['push']['paths-ignore'] << 'web3d/**'"
  refused paths-ignore-cargo-star "push paths-ignore \"Cargo.*\" matches Cargo.lock, Cargo.toml, web3d/Cargo.toml" "on['push']['paths-ignore'] << 'Cargo.*'"
  refused paths-ignore-workflow "pull_request paths-ignore \".github/**\" matches .github/workflows/ci.yml" "on['pull_request']['paths-ignore'] << '.github/**'"
  refused paths-ignore-question-one-char "push paths-ignore \"Cargo.loc?\" matches Cargo.lock" "on['push']['paths-ignore'] << 'Cargo.loc?'"
  refused paths-ignore-question-optional "push paths-ignore \"web3d/src/web.rss?\" matches web3d/src/web.rs" "on['push']['paths-ignore'] << 'web3d/src/web.rss?'"
  refused paths-ignore-class "push paths-ignore \"web3d/src/[w]eb.rs\" matches web3d/src/web.rs" "on['push']['paths-ignore'] << 'web3d/src/[w]eb.rs'"
  refused paths-ignore-negation-not-credited "push paths-ignore \"**\" matches" "on['push']['paths-ignore'] = ['**', '!web3d/**']"
  refused paths-ignore-braces "has a character the gate does not model" "on['push']['paths-ignore'] << 'web3d/{src,js}/**'"

  echo "## The other checks"
  refused step-renamed "no step named" "s['name'] = 'clippy (wasm32)'"
  refused step-working-directory "the step runs in \"old\"" "s['working-directory'] = 'old'"
  refused step-other-row "the step runs on no matrix row" "s['if'] = \"matrix.os == 'windows-latest'\""
  refused no-push "the workflow does not run on push and pull_request" "on.delete('push')"
  refused job-container-env "the job has container" "j['container'] = { 'image' => 'rust:latest', 'env' => { 'RUSTFLAGS' => '--cap-lints warn' } }"
  refused job-needs "the job has needs" "j['needs'] = ['setup']"
  refused job-runs-on "the job runs on \"self-hosted\"" "j['runs-on'] = 'self-hosted'"
  refused checkout-old-commit "the job uses \"actions/checkout@v4\"" "j['steps'].find { |x| x['uses'] == 'actions/checkout@v4' }['with']['ref'] = '0' * 40"
  refused other-action "the job uses \"example/set-env@v1\"" "j['steps'].insert(j['steps'].index(s), { 'uses' => 'example/set-env@v1' })"

  echo "## Broken copies of the gate, on step-env-cap-lints (which the real gate refuses)"
  ruby -e 'File.write(ARGV[1], File.read(ARGV[0]).sub("require \"yaml\"\n", "require \"yaml\"\nrequire \"no-such-library\"\n"))' "$gate" "$tmp/gate-load-error.rb"
  ruby -e 'File.write(ARGV[1], File.read(ARGV[0]) + "end\n")' "$gate" "$tmp/gate-syntax-error.rb"
  ruby -e 'File.write(ARGV[1], File.read(ARGV[0]).sub(/^def refuse.*?^end\n/m) { |m| m + "refuse \"a reason no row names\"\n" })' "$gate" "$tmp/gate-wrong-reason.rb"
  for g in load-error syntax-error; do
    row "gate-$g" not-a-refusal "the step sets env RUSTFLAGS" "$tmp/step-env-cap-lints.yml" "$tmp/gate-$g.rb"
  done
  row gate-wrong-reason another-reason "the step sets env RUSTFLAGS" "$tmp/step-env-cap-lints.yml" "$tmp/gate-wrong-reason.rb"

  echo "rows that held: $held of $rows"
} > "$out"
cat "$out"
[ "$held" = "$rows" ]
