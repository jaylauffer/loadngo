#!/usr/bin/env bash
# A copy of ../../critic14/yaml-gate/run.sh, changed only to write critic14-results.txt here.
# Critic 14: more changed copies of .github/workflows/ci.yml, each of which keeps the wasm32 clippy
# step from failing when clippy finds a lint (or keeps it from running at all), run through the
# CI-step gate (../../ci-step.rb). Copies are made in a temporary directory and removed. A copy
# "passes the gate" when ci-step.rb exits 0. Writes results.txt beside this script.
#   bash web3d/evidence/phase1/critic14/yaml-gate/run.sh   (from the loadngo workspace)
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ws="$(cd "$here/../../../../.." && pwd)"
gate="$ws/web3d/evidence/phase1/ci-step.rb"
export WASM_CLIPPY="cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings"
tmp="$(mktemp -d)"
out="$here/critic14-results.txt"
: > "$out"
try() {
  local name="$1" script="$2"
  ruby -ryaml -e "$script" "$ws/.github/workflows/ci.yml" "$tmp/$name.yml"
  if ruby "$gate" "$tmp/$name.yml" > "$tmp/o.txt" 2>&1; then r="PASSES the gate"; else r="fails the gate: $(tail -1 "$tmp/o.txt" | cut -c1-140)"; fi
  echo "$name: $r" | tee -a "$out"
}
# Each edit loads the workflow, changes one thing, and writes it back.
L='d = YAML.load_file(ARGV[0]); j = d["jobs"]["check"]; s = j["steps"].find { |x| x["name"] == "cargo clippy (web3d, wasm32)" };'
W='File.write(ARGV[1], d.to_yaml)'
try real "$L $W"
try step-env-cap-lints "$L s[\"env\"] = { \"RUSTFLAGS\" => \"--cap-lints warn\" }; $W"
try job-env-cap-lints "$L j[\"env\"] = { \"RUSTFLAGS\" => \"--cap-lints warn\" }; $W"
try workflow-env-cap-lints "$L d[\"env\"][\"RUSTFLAGS\"] = \"--cap-lints warn\"; $W"
try earlier-step-sets-github-env "$L i = j[\"steps\"].index(s); j[\"steps\"].insert(i, { \"name\" => \"x\", \"run\" => \"echo RUSTFLAGS=--cap-lints warn >> \\\"\$GITHUB_ENV\\\"\" }); $W"
try job-if-false "$L j[\"if\"] = false; $W"
try push-and-pr-on-no-branch "$L on = d[\"on\"] || d[true]; on[\"push\"][\"branches\"] = [\"no-such-branch\"]; on[\"pull_request\"][\"branches\"] = [\"no-such-branch\"]; $W"
try paths-ignore-everything "$L on = d[\"on\"] || d[true]; on[\"push\"][\"paths-ignore\"] = [\"**\"]; on[\"pull_request\"][\"paths-ignore\"] = [\"**\"]; $W"
rm -rf "$tmp"
