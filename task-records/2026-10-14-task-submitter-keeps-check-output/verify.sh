#!/bin/bash
# The submitter's check for the Task "task_submitter keeps the check's output" (Jay,
# 2026-10-14). Committed and pushed before the work started. task_submitter runs it with
# LOADNGO_TASK_ARTIFACT_HINT (the worker's result) and LOADNGO_TASK_ARTIFACT_COPY_PATH
# (the copy the receipt hashes). Exit 0 accepts the work.
#
# Until now the submitter printed only "success=true/false" for its check and kept
# nothing of what the check said, and wrote no record at all for rejected work.
#
# What the result must name:
#   commit: <40 hex>      the loadngo commit with the change
#
# Success criteria:
#  1. The commit is on GitHub's dev and changes network/src/bin/task_submitter.rs.
#  2. The main checkout's HEAD contains it with network/ unchanged; fmt, strict clippy and
#     the tests of the network crate pass there.
#  3. Two real Task cycles on loopback, run here with task_submitter and task_worker built
#     from HEAD, kept in demo/ beside the copy:
#     - a check that fails (exit 3, one line on stderr): the submitter exits non-zero;
#       its closure record (--receipt-path) has "accepted": false, no receipt, and a
#       "verification" with "exit_code": 3 and that line in "stderr"; the submitter's log
#       has the line; the worker's TaskAck note carries it.
#     - a check that passes (prints "checked"): the record has "accepted": true, a
#       receipt with its commitment, and a "verification" with "exit_code": 0 and
#       "checked" in "stdout".
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

# 1. On GitHub, changing the submitter.
git -C "$repo" fetch -q github dev || fail "cannot fetch github dev"
git -C "$repo" merge-base --is-ancestor "$commit" github/dev || fail "$commit is not on github/dev"
git -C "$repo" diff --name-only "$commit^" "$commit" | grep -qx network/src/bin/task_submitter.rs ||
    fail "$commit does not change network/src/bin/task_submitter.rs"
pass "1: $commit on github/dev, changes task_submitter.rs"

# 2. The gates, run here.
git -C "$repo" merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git -C "$repo" diff --quiet HEAD -- network || fail "network/ has uncommitted changes"
cd "$repo" || fail "no $repo"
cargo fmt --all -- --check >/dev/null || fail "cargo fmt --check"
cargo clippy -q -p network --all-targets --all-features -- -D warnings || fail "clippy"
cargo test -q -p network >/dev/null 2>&1 || fail "network tests"
pass "2: fmt, clippy and network tests pass at $(git rev-parse --short HEAD)"

# 3. Two cycles with the binaries built from HEAD. The worker gets two seconds to bind
# before the submitter sends its one request (the worker prints nothing until then).
cargo build -q -p network --bin task_submitter --bin task_worker || fail "cannot build the Task binaries"
demo=$(dirname "$copy")/demo
rm -rf "$demo"
cycle() { # name, port base, check command
    local dir=$demo/$1 wp=$(($2 + 1)) sp=$2
    mkdir -p "$dir"
    "$repo/target/debug/task_worker" --worker-node-id demo-worker \
        --reply-endpoint 127.0.0.1:$wp --bind-port $wp --multicast-v6 'ff02::4c4e:2%1' \
        --capability demo --artifact-hint "$dir/artifact.txt" \
        --listen-seconds 30 --ack-timeout-seconds 120 \
        --execute-command "printf 'demo artifact\n' > \"\$LOADNGO_TASK_ARTIFACT_HINT\"" \
        >"$dir/worker.log" 2>&1 &
    local worker=$!
    sleep 2
    "$repo/target/debug/task_submitter" --multicast-port $wp --submitter-node-id demo-submitter \
        --reply-endpoint 127.0.0.1:$sp --bind-port $sp --request-target 127.0.0.1:$wp \
        --multicast-v6 'ff02::4c4e:2%1' --capability demo \
        --summary "demo of the kept check output ($1)" \
        --artifact-hint "$dir/artifact.txt" --artifact-copy-path "$dir/artifact-copy.txt" \
        --receipt-path "$dir/closure.json" --offer-timeout-seconds 3 --result-timeout-seconds 60 \
        --verify-command "$3" >"$dir/submitter.log" 2>&1
    echo $? >"$dir/submitter.exit"
    wait $worker
}
cycle rejected 47940 "echo 'check: the artifact names no commit' >&2; exit 3"
cycle accepted 47942 "cp \"\$LOADNGO_TASK_ARTIFACT_HINT\" \"\$LOADNGO_TASK_ARTIFACT_COPY_PATH\" && echo checked"

python3 -I - "$demo" <<'EOF' || fail "the demo cycles do not show the check's output kept"
import json, sys
demo = sys.argv[1]
line = "check: the artifact names no commit"
def read(name, file):
    return open(f"{demo}/{name}/{file}").read()
r = json.loads(read("rejected", "closure.json"))
v = r["verification"]
assert read("rejected", "submitter.exit").strip() != "0", "rejected: submitter exited 0"
assert r["accepted"] is False and r.get("receipt") is None, "rejected: record says accepted"
assert v["exit_code"] == 3 and line in v["stderr"], f"rejected: verification {v}"
assert line in read("rejected", "submitter.log"), "rejected: line not in the submitter log"
ack = [l for l in read("rejected", "worker.log").splitlines() if l.startswith("task_worker_ack")]
assert ack and "accepted=false" in ack[0] and line in ack[0], f"rejected: worker ack {ack}"
a = json.loads(read("accepted", "closure.json"))
v = a["verification"]
assert read("accepted", "submitter.exit").strip() == "0", "accepted: submitter failed"
assert a["accepted"] is True and a["receipt"] and a["receipt_commitment_hex"], "accepted: no receipt"
assert v["exit_code"] == 0 and "checked" in v["stdout"], f"accepted: verification {v}"
print("verify: ok: 3: rejected and accepted cycles keep the check's output")
EOF
echo "verify: accepted"
