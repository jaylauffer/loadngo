# The YAML gate for CI's wasm32 clippy step, which can run only on GitHub: gates.sh runs it on
# .github/workflows/ci.yml. It finds the step by its name in the job `check`, then refuses the
# workflow unless all of these hold, so that the step can neither pass when its command fails nor
# be skipped:
# - the step runs exactly two lines, the wasm32 target added and then the very clippy command the
#   local gate runs (WASM_CLIPPY), so neither a replaced command nor an added one (`exit 0` first,
#   say) passes; and it runs them in the checkout (working-directory: loadngo);
# - neither the step nor its job sets `continue-on-error`, and neither the step, the job nor the
#   workflow sets a shell, so the step runs under GitHub's default, `bash -e`, which stops at the
#   first line that fails;
# - the step, the job and the workflow set no `env` key outside ENV_OK, so none sets RUSTFLAGS,
#   CARGO_ENCODED_RUSTFLAGS, CARGO_BUILD_RUSTFLAGS, RUSTC_WRAPPER, BASH_ENV or the like; and no
#   `run` anywhere in the workflow names GITHUB_ENV or GITHUB_PATH, through which an earlier step
#   could set them for later steps;
# - the job has no `if`, and no key outside JOB_KEYS (so no `needs`, `container`, `services`);
#   it runs on `${{ matrix.os }}`; and every action it uses is in USES_OK, inputs (`with`) included,
#   so the checkout cannot be pinned to an old commit;
# - the step's `if` picks the ubuntu-latest matrix row, and that row exists;
# - the workflow runs on push and pull_request, neither filtered by anything but `paths-ignore`, and
#   no paths-ignore pattern matches a file the step reads (web3d's tracked sources and manifest, the
#   root Cargo.toml and Cargo.lock) or this workflow file, as git tracks them when the gate runs.
#   Patterns are read generously (glob_re), so one that GitHub might read as matching is refused.
# A refusal prints `ci-step: refused: <reason>` to stderr and exits 2. Anything else that stops it
# (a Ruby error, WASM_CLIPPY unset, git failing) exits 1, so a broken gate never looks like a
# refusal. A pass prints one line, what it found, and exits 0.
#   WASM_CLIPPY="cargo clippy ..." ruby ci-step.rb [path to the workflow]
# YAML 1.1 reads the key `on` as true. BSD-3-Clause; see the repository's LICENSE.
require "yaml"

def refuse(reason)
  warn "ci-step: refused: #{reason}"
  exit 2
end

# The allow-lists: what the workflow uses today (and, for the job, `env`, whose names ENV_OK limits).
ENV_OK = %w[CARGO_TERM_COLOR LOADNGO_CAS_MIN_FREE_GIB].freeze
JOB_KEYS = %w[name runs-on timeout-minutes strategy steps env].freeze
USES_OK = [
  { "uses" => "actions/checkout@v4", "with" => { "path" => "loadngo" } },
  { "uses" => "Swatinem/rust-cache@v2", "with" => { "workspaces" => "loadngo" } },
].freeze

# A GitHub filter pattern as a regex that matches at least what GitHub matches: `*` within a path
# segment, `**` across `/` (`**/` also matches nothing), `[...]` a class (a negated one as any
# character), `?` both as any one character and as "the character before it, or not", and `+` both
# as itself and as "one or more of the character before it"; case is ignored, and a leading `/`
# dropped. Characters it does not model (`{ } \ ( ) |`) are refused.
def glob_re(pat, where)
  refuse "#{where} #{pat.inspect} has a character the gate does not model" if pat =~ /[{}\\()|]/
  p = pat.sub(%r{\A/}, "")
  atoms = []
  i = 0
  while i < p.size
    c = p[i]
    if c == "*" && p[i + 1] == "*"
      i += 1 while p[i + 1] == "*"
      if p[i + 1] == "/"
        atoms << "(?:.*/)?"
        i += 1
      else
        atoms << ".*"
      end
    elsif c == "*"
      atoms << "[^/]*"
    elsif c == "?" || c == "+"
      atoms << "(?:#{atoms.pop})#{c}" unless atoms.empty?
      atoms << (c == "?" ? "[^/]?" : "\\+?")
    elsif c == "["
      j = p.index("]", i + 1)
      body = j && p[i + 1...j]
      refuse "#{where} #{pat.inspect} has a class the gate does not model" unless body =~ /\A[!^]?[A-Za-z0-9._-]+\z/
      atoms << (body =~ /\A[!^]/ ? "[^/]" : "[#{body}]")
      i = j
    else
      atoms << Regexp.escape(c)
    end
    i += 1
  end
  Regexp.new("\\A#{atoms.join}\\z", Regexp::IGNORECASE)
rescue RegexpError => e
  refuse "#{where} #{pat.inspect} cannot be read: #{e.message}"
end

# The file and every tail of it after a `/`: a pattern is also tried on those, in case GitHub reads
# it relative to any directory (its documentation says it does not; this only refuses more).
def tails(file)
  parts = file.split("/")
  (0...parts.size).map { |k| parts[k..-1].join("/") }
end

path = ARGV[0] || ".github/workflows/ci.yml"
d = YAML.load_file(path)
job = d["jobs"]["check"]
name = "cargo clippy (web3d, wasm32)"
s = job["steps"].find { |x| x["name"] == name } or refuse "no step named #{name}"
run = s["run"].to_s.lines.map(&:strip).reject(&:empty?)
want = ["rustup target add wasm32-unknown-unknown", ENV.fetch("WASM_CLIPPY")]
refuse "the step must run exactly #{want.inspect}, not #{run.inspect}" unless run == want
refuse "the step runs in #{s["working-directory"].inspect}, not the checkout, loadngo" unless s["working-directory"] == "loadngo"
refuse "the step sets continue-on-error: #{s["continue-on-error"].inspect}" if s.key?("continue-on-error")
refuse "the job sets continue-on-error: #{job["continue-on-error"].inspect}" if job.key?("continue-on-error")
refuse "the step sets a shell: #{s["shell"].inspect}" if s.key?("shell")
[["the job", job], ["the workflow", d]].each do |what, x|
  shell = x.dig("defaults", "run", "shell")
  refuse "#{what} sets a default shell: #{shell.inspect}" if shell
end

env = []
[["the step", s], ["the job", job], ["the workflow", d]].each do |what, x|
  next unless x.key?("env")
  refuse "#{what} sets env to #{x["env"].inspect}, not a map of names" unless x["env"].is_a?(Hash)
  bad = x["env"].keys.map(&:to_s) - ENV_OK
  refuse "#{what} sets env #{bad.join(", ")}, outside the allow-list (#{ENV_OK.join(", ")})" unless bad.empty?
  env |= x["env"].keys
end
d["jobs"].each do |id, j|
  Array(j["steps"]).each do |x|
    word = x["run"].to_s[/GITHUB_(ENV|PATH)/i]
    refuse "a run in job #{id} (step #{(x["name"] || x["uses"]).inspect}) mentions #{word}" if word
  end
end

refuse "the job sets if: #{job["if"].inspect}" if job.key?("if")
extra = job.keys - JOB_KEYS
refuse "the job has #{extra.join(", ")}; only #{JOB_KEYS.join(", ")} are allowed" unless extra.empty?
refuse "the job runs on #{job["runs-on"].inspect}, not ${{ matrix.os }}" unless job["runs-on"] == "${{ matrix.os }}"
job["steps"].each do |x|
  next unless x.key?("uses")
  next if USES_OK.include?("uses" => x["uses"], "with" => x["with"])
  refuse "the job uses #{x["uses"].inspect} with #{x["with"].inspect}, not one of the allowed actions and inputs"
end

rows = job["strategy"]["matrix"]["include"].map { |m| m["os"] }
refuse "the step runs on no matrix row: #{s["if"]}" unless s["if"] == "matrix.os == 'ubuntu-latest'" && rows.include?("ubuntu-latest")

on = d.key?("on") ? d["on"] : d[true]
on = Array(on).map { |e| [e, nil] }.to_h unless on.is_a?(Hash) # `on: push` or `on: [push, ...]`
refuse "the workflow does not run on push and pull_request: #{on.keys}" unless on.key?("push") && on.key?("pull_request")

# The files a paths-ignore pattern must not match. (web3d has no tests, examples, benches or build
# script today; they are listed so that they count once they exist.)
root = File.expand_path("../../..", __dir__)
specs = %w[web3d/src web3d/tests web3d/examples web3d/benches web3d/build.rs web3d/Cargo.toml
           Cargo.toml Cargo.lock .github/workflows/ci.yml]
files = IO.popen(["git", "-C", root, "ls-files", "-z", "--", *specs], &:read).split("\0").sort
abort "ci-step: error: git ls-files failed in #{root}" unless $?.success?
must = %w[web3d/Cargo.toml Cargo.toml Cargo.lock .github/workflows/ci.yml]
abort "ci-step: error: git does not track #{(must - files).join(", ")}" unless (must - files).empty?
abort "ci-step: error: git tracks no file under web3d/src" if files.none? { |f| f.start_with?("web3d/src/") }
ignored = {}
%w[push pull_request].each do |ev|
  f = on[ev] || {}
  refuse "#{ev} is #{f.inspect}, not a map of filters" unless f.is_a?(Hash)
  (f.keys - ["paths-ignore"]).each { |k| refuse "#{ev} has the filter #{k}; only paths-ignore is allowed" }
  ignored[ev] = Array(f["paths-ignore"]).map(&:to_s)
  ignored[ev].each do |pat|
    next if pat.start_with?("!") # a negated pattern can only bring files back
    re = glob_re(pat, "#{ev} paths-ignore")
    hit = files.select { |file| tails(file).any? { |t| re.match?(t) } }
    refuse "#{ev} paths-ignore #{pat.inspect} matches #{hit.join(", ")}" unless hit.empty?
  end
end

uses = job["steps"].map { |x| x["uses"] }.compact
puts "step: #{s["name"]}; if: #{s["if"]}; rows: #{rows.join(", ")}; runs exactly: #{run.join(" && ")}; " \
     "in #{s["working-directory"]}; no continue-on-error, no shell set; env: #{env.join(", ")}; " \
     "no run names GITHUB_ENV or GITHUB_PATH; job keys: #{job.keys.join(", ")}; actions: #{uses.join(", ")}; " \
     "on: #{on.keys.join(", ")}; paths-ignore: #{ignored.map { |ev, ps| "#{ev} #{ps.inspect}" }.join(", ")}, " \
     "matching none of the #{files.size} files kept"
