#!/usr/bin/env bash
# The YAML gate (../../ci-step.rb) against the real workflow and against copies of it, each changed
# one way, made in a temporary directory outside the repository: the real one must pass, every
# changed copy must fail. Writes results.txt beside this script.
#   bash run.sh   (from anywhere; uses ruby)
# BSD-3-Clause; see the repository's LICENSE.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ws="$(cd "$here/../../../../.." && pwd)"
gate="$ws/web3d/evidence/phase1/ci-step.rb"
real="$ws/.github/workflows/ci.yml"
export WASM_CLIPPY="cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings"
tmp="$(mktemp -d)"
out="$here/results.txt"
# Each variant: a name, and a ruby expression that changes the parsed workflow `d` (its step is `s`).
variants=(
  "real|"
  "exit-0-first|s['run'] = \"exit 0\n\" + s['run']"
  "true-after|s['run'] = s['run'] + \"true\n\""
  "or-true|s['run'] = s['run'].sub('-D warnings', '-D warnings || true')"
  "no-all-targets|s['run'] = s['run'].sub(' --all-targets', '')"
  "step-continue-on-error|s['continue-on-error'] = true"
  "job-continue-on-error|d['jobs']['check']['continue-on-error'] = true"
  "step-shell|s['shell'] = 'bash {0}'"
  "workflow-default-shell|d['defaults'] = { 'run' => { 'shell' => 'bash {0}' } }"
)
fails=0
{
  echo "# The YAML gate, ci-step.rb, on the real workflow and on changed copies ($(date '+%Y-%m-%d %H:%M %z'))"
  for v in "${variants[@]}"; do
    name="${v%%|*}"; change="${v#*|}"
    file="$real"
    if [ -n "$change" ]; then
      file="$tmp/$name.yml"
      ruby -ryaml -e "d = YAML.load_file(ARGV[0]); s = d['jobs']['check']['steps'].find { |x| x['name'] == 'cargo clippy (web3d, wasm32)' }; $change; File.write(ARGV[1], d.to_yaml)" "$real" "$file"
    fi
    msg="$(ruby "$gate" "$file" 2>&1)"; code=$?
    want=1; [ "$name" = real ] && want=0
    ok=no; { [ "$want" = 0 ] && [ "$code" = 0 ]; } || { [ "$want" = 1 ] && [ "$code" != 0 ]; } && ok=yes
    [ $ok = yes ] || fails=$((fails + 1))
    echo "$name: exit $code ($([ "$want" = 0 ] && echo "must pass" || echo "must fail"): $([ $ok = yes ] && echo held || echo DID NOT HOLD)): ${msg//$ws\//}"
  done
  echo "variants that did not hold: $fails"
} > "$out"
rm -rf "$tmp"
cat "$out"
[ "$fails" = 0 ]
