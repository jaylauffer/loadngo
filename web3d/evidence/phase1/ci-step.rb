# The YAML gate for CI's wasm32 clippy step, which can run only on GitHub: gates.sh runs it on
# .github/workflows/ci.yml. It finds the step by its name, then checks what it runs and what could
# let it pass when its command fails:
# - it runs exactly two lines, the wasm32 target added and then the very clippy command the local
#   gate runs (WASM_CLIPPY), so neither a replaced command nor an added one (`exit 0` first, say)
#   passes;
# - neither the step nor its job sets `continue-on-error`, and neither the step, the job nor the
#   workflow sets a shell, so the step runs under GitHub's default, `bash -e`, which stops at the
#   first line that fails;
# - it runs on a matrix row that exists, in a workflow that runs on pushes and pull requests.
#   WASM_CLIPPY="cargo clippy ..." ruby ci-step.rb [path to the workflow]
# YAML 1.1 reads the key `on` as true. BSD-3-Clause; see the repository's LICENSE.
require "yaml"
path = ARGV[0] || ".github/workflows/ci.yml"
d = YAML.load_file(path)
job = d["jobs"]["check"]
name = "cargo clippy (web3d, wasm32)"
s = job["steps"].find { |x| x["name"] == name } or abort "no step named #{name}"
run = s["run"].to_s.lines.map(&:strip).reject(&:empty?)
want = ["rustup target add wasm32-unknown-unknown", ENV.fetch("WASM_CLIPPY")]
abort "the step must run exactly #{want.inspect}, not #{run.inspect}" unless run == want
abort "the step sets continue-on-error: #{s["continue-on-error"].inspect}" if s.key?("continue-on-error")
abort "the job sets continue-on-error: #{job["continue-on-error"].inspect}" if job.key?("continue-on-error")
abort "the step sets a shell: #{s["shell"].inspect}" if s.key?("shell")
[["the job", job], ["the workflow", d]].each do |what, x|
  shell = x.dig("defaults", "run", "shell")
  abort "#{what} sets a default shell: #{shell.inspect}" if shell
end
rows = job["strategy"]["matrix"]["include"].map { |m| m["os"] }
abort "the step runs on no matrix row: #{s["if"]}" unless s["if"] == "matrix.os == 'ubuntu-latest'" && rows.include?("ubuntu-latest")
on = d["on"] || d[true]
abort "the workflow does not run on push and pull_request: #{on.keys}" unless on.key?("push") && on.key?("pull_request")
puts "step: #{s["name"]}; if: #{s["if"]}; rows: #{rows.join(", ")}; runs exactly: #{run.join(" && ")}; no continue-on-error, no shell set; on: #{on.keys.join(", ")}"
