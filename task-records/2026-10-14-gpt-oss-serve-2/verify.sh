#!/bin/bash
# The submitter's check for the Task "the model service runs on loadngo inference" (Jay,
# 2026-10-14: "We don't need a llama-server, we have loadngo inference, remove the noise
# and wire it up"). Committed and pushed before the work started. task_submitter runs it
# with LOADNGO_TASK_ARTIFACT_HINT (the worker's result) and LOADNGO_TASK_ARTIFACT_COPY_PATH
# (the copy the receipt hashes). Exit 0 accepts the work.
#
# Second run. The first (task-records/2026-10-14-gpt-oss-serve, intent e935c46c, pushed
# before the work) was rejected because its llama grep matched its own verify.sh. This
# copy was written after the work and differs from it only in that grep, which now leaves
# out task-records/, and in this paragraph.
#
# Before: network::model_service restored gpt-oss-20b from the Archive CAS by hash, then
# started Homebrew's llama-server, fell back from Metal to the CPU, read its logs and
# polled its HTTP health check every 250 ms; zhoenus_head_model ran it for the Zhoenus
# talking head, which never called it.
#
# What the result must name:
#   commit: <40 hex>   the last loadngo commit of the work
#
# Success criteria:
#  1. The commit is on GitHub's dev. At it, network/src/model_service.rs and
#     network/src/bin/zhoenus_head_model.rs are gone, gpt-oss/src/bin/gpt_oss_serve.rs
#     exists, and no Rust, TOML or shell source and no README mentions llama.
#  2. HEAD contains the commit with the touched crates unchanged; fmt, strict clippy and
#     the tests of data, loadngo-inference, loadngo-gpt-oss and network pass, including at
#     least three model-store tests in data (restore once then serve offline, a
#     substituted copy refused, a root without the model refused).
#  3. GitHub's CI passed on all four platforms for the commit or a later dev commit.
#  4. On this Mac, `gpt_oss_serve --socket <dir>/node/gpt-oss.sock` (release build):
#     - verifies the model by hash and prints a line starting
#       "gpt_oss_serve_listening" once it accepts calls; the socket's directory is 0700;
#     - answers one JSON line per call on the Unix socket:
#         {"call_id": N, "capability": "chat", "deadline_ms": MS,
#          "body": {"instructions": TEXT, "message": TEXT}}
#       with {"call_id": N, "status": S, "body": {...}}: a talking-head question gets
#       status "ok" and a non-empty body.text within its 120 s deadline, and a second one
#       does too; deadline_ms 0 gets "expired"; an unknown capability and a line that is
#       not JSON get "error";
#     - doing nothing for 10 s after answering, makes at most 20 context switches (top);
#     - on SIGTERM exits 0 within 10 s and removes its socket.
set -u
repo=/Users/jay/pudding/loadngo
fail() {
    echo "verify: FAIL: $*" >&2
    exit 1
}
pass() { echo "verify: ok: $*"; }

result=${LOADNGO_TASK_ARTIFACT_HINT:?no artifact hint}
copy=${LOADNGO_TASK_ARTIFACT_COPY_PATH:?no artifact copy path}
[ -f "$result" ] || fail "no result file at $result"
cp "$result" "$copy" || fail "cannot copy the result to $copy"
commit=$(sed -n 's/^commit: *//p' "$copy" | head -1)
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "the result names no commit (a line 'commit: <40 hex>')"

# 1. On GitHub, the old service gone, no llama.
cd "$repo" || fail "no $repo"
git fetch -q github dev || fail "cannot fetch github dev"
git merge-base --is-ancestor "$commit" github/dev || fail "$commit is not on github/dev"
for gone in network/src/model_service.rs network/src/bin/zhoenus_head_model.rs; do
    if git cat-file -e "$commit:$gone" 2>/dev/null; then fail "$gone is still there at $commit"; fi
done
git cat-file -e "$commit:gpt-oss/src/bin/gpt_oss_serve.rs" || fail "no gpt-oss/src/bin/gpt_oss_serve.rs at $commit"
if git grep -n -i llama "$commit" -- '*.rs' '*.toml' '*.sh' '*README.md' ':!task-records'; then
    fail "llama is still mentioned in source or a README at $commit"
fi
pass "1: $commit on github/dev; model_service and zhoenus_head_model gone, gpt_oss_serve there, no llama"

# 2. The gates.
git merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git diff --quiet HEAD -- data inference gpt-oss network || fail "uncommitted changes in the touched crates"
crates=(-p data -p loadngo-inference -p loadngo-gpt-oss -p network)
cargo fmt --all -- --check >/dev/null || fail "cargo fmt --check"
cargo clippy -q "${crates[@]}" --all-targets --all-features -- -D warnings || fail "clippy"
cargo test -q "${crates[@]}" >/dev/null 2>&1 || fail "tests"
store=$(cargo test -q -p data model_store 2>&1)
grep -Eq "test result: ok. ([3-9]|[1-9][0-9]+) passed" <<<"$store" || fail "fewer than three model-store tests ran and passed: $store"
pass "2: fmt, clippy and tests pass at $(git rev-parse --short HEAD), model-store tests included"

# 3. CI on the four platforms, for the commit or a descendant on dev.
ci=
while read -r id sha conclusion; do
    [ "$conclusion" = success ] || continue
    if git merge-base --is-ancestor "$commit" "$sha" 2>/dev/null; then ci=$id; break; fi
done < <(gh run list --repo jaylauffer/loadngo --branch dev --workflow CI --limit 30 \
    --json databaseId,headSha,conclusion --jq '.[] | "\(.databaseId) \(.headSha) \(.conclusion)"')
[ -n "$ci" ] || fail "no successful CI run on dev contains $commit"
n=$(gh run view "$ci" --repo jaylauffer/loadngo --json jobs --jq '[.jobs[] | select(.conclusion == "success")] | length')
[ "$n" -ge 4 ] || fail "CI run $ci has $n successful jobs, not 4"
pass "3: CI run $ci passed ($n jobs) and contains $commit"

# 4. The server, live.
cargo build -q --release -p loadngo-gpt-oss --bin gpt_oss_serve || fail "cannot build gpt_oss_serve"
demo=$(dirname "$copy")/demo
mkdir -p "$demo"
python3 -I - "$repo/target/release/gpt_oss_serve" "$demo" <<'EOF' || fail "the live server check failed"
import json, os, signal, socket, stat, subprocess, sys, tempfile, threading, time

server, demo = sys.argv[1], sys.argv[2]
home = tempfile.mkdtemp(prefix="gpt-oss-serve-")
path = os.path.join(home, "node", "gpt-oss.sock")
log = open(os.path.join(demo, "server.log"), "w")
p = subprocess.Popen([server, "--socket", path], stdout=subprocess.PIPE, stderr=log, text=True)
watchdog = threading.Timer(600, p.kill)  # a hung server fails the check instead of hanging it
watchdog.daemon = True
watchdog.start()
ready = False
for line in p.stdout:
    log.write(line); log.flush()
    if line.startswith("gpt_oss_serve_listening"):
        ready = True
        break
assert ready, "the server never printed gpt_oss_serve_listening"
mode = stat.S_IMODE(os.stat(os.path.dirname(path)).st_mode)
assert mode == 0o700, f"the socket's directory is {oct(mode)}, not 0700"

def call(line, timeout=150):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(path)
    s.sendall(line.encode() + b"\n")
    data = b""
    while not data.endswith(b"\n"):
        chunk = s.recv(65536)
        if not chunk:
            break
        data += chunk
    s.close()
    return json.loads(data)

calls = []
instructions = ("You are the talking head above the base in Zhoenus, a game where the player "
                "flies a ship and saves flyers. Answer in one or two short sentences.")
for i, question in enumerate(["How do I save the flyers?", "What happens when my battery runs low?"]):
    request = {"call_id": 10 + i, "capability": "chat", "deadline_ms": 120000,
               "body": {"instructions": instructions, "message": question}}
    start = time.monotonic()
    reply = call(json.dumps(request))
    seconds = time.monotonic() - start
    calls.append({"request": request, "reply": reply, "seconds": round(seconds, 2)})
    assert reply.get("call_id") == 10 + i and reply.get("status") == "ok", reply
    assert reply["body"]["text"].strip(), reply
    assert seconds < 120, seconds
expired = call(json.dumps({"call_id": 20, "capability": "chat", "deadline_ms": 0,
                           "body": {"message": "Hello?"}}))
assert expired.get("status") == "expired", expired
unknown = call(json.dumps({"call_id": 21, "capability": "paint", "deadline_ms": 5000, "body": {}}))
assert unknown.get("status") == "error", unknown
garbage = call("this is not json")
assert garbage.get("status") == "error", garbage
calls += [{"reply": expired}, {"reply": unknown}, {"reply": garbage}]

top = subprocess.run(["top", "-l", "2", "-s", "10", "-pid", str(p.pid), "-stats", "pid,csw"],
                     capture_output=True, text=True).stdout
samples = [int(l.split()[1].rstrip("+")) for l in top.splitlines() if l.strip().startswith(str(p.pid))]
assert len(samples) == 2, top
idle = samples[1] - samples[0]
assert idle <= 20, f"{idle} context switches in 10 s doing nothing"

p.send_signal(signal.SIGTERM)
code = p.wait(timeout=10)
assert code == 0, f"exit status {code} after SIGTERM"
assert not os.path.exists(path), "the socket is still there after SIGTERM"
json.dump({"calls": calls, "idle_context_switches_10s": idle, "exit": code},
          open(os.path.join(demo, "calls.json"), "w"), indent=1)
print(f"verify: ok: 4: two answers ({calls[0]['seconds']} s, {calls[1]['seconds']} s), "
      f"expired and errors as asked, {idle} context switches idle in 10 s, clean SIGTERM")
EOF
echo "verify: accepted"
