#!/bin/bash
# The submitter's check for the Task "gpt-oss: the first answer when the refusal note is
# ignored" (Jay, 2026-10-14). Committed and pushed before the work started, so the
# criteria cannot be fitted to the result afterwards. task_submitter runs it with
# LOADNGO_TASK_ARTIFACT_HINT (the worker's result file) and LOADNGO_TASK_ARTIFACT_COPY_PATH
# (where the submitter keeps a copy, hashed into the receipt). Exit 0 accepts the work.
#
# What the result must name, one per line:
#   commit: <40 hex>      the loadngo commit with the change
#   test: <name>          the loop test that covers both cases
#   live: <path>.jsonl    a gpt-oss chat run after the commit, its .state.json beside it
#
# Success criteria:
#  1. The commit is on GitHub's dev and changes inference/src/agent/mod.rs and
#     inference/src/agent/tests.rs; the named test is in tests.rs at that commit.
#  2. The main checkout's HEAD contains the commit with inference/ unchanged, and there
#     fmt, strict clippy for loadngo-inference, the agent tests and the named test pass.
#  3. In the live chat, a refusal note ("[check] ...") was followed by a reply with no
#     tool call, the loop said it kept the first answer, and the answer saved in the
#     chat's history is the reply from before the note.
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
field() { sed -n "s/^$1: *//p" "$copy" | head -1; }

commit=$(field commit)
test_name=$(field test)
live=$(field live)
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "the result names no commit (a line 'commit: <40 hex>')"
[ -n "$test_name" ] || fail "the result names no test"
[ -n "$live" ] || fail "the result names no live chat"

# 1. On GitHub, changing the loop and its tests.
git -C "$repo" fetch -q github dev || fail "cannot fetch github dev"
git -C "$repo" merge-base --is-ancestor "$commit" github/dev || fail "$commit is not on github/dev"
changed=$(git -C "$repo" diff --name-only "$commit^" "$commit")
grep -qx inference/src/agent/mod.rs <<<"$changed" || fail "$commit does not change inference/src/agent/mod.rs"
grep -qx inference/src/agent/tests.rs <<<"$changed" || fail "$commit does not change inference/src/agent/tests.rs"
git -C "$repo" show "$commit:inference/src/agent/tests.rs" | grep -q "^fn $test_name()" ||
    fail "test $test_name is not in tests.rs at $commit"
pass "1: $commit on github/dev, changes the loop and its tests, has $test_name"

# 2. The gates, run here.
git -C "$repo" merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git -C "$repo" diff --quiet HEAD -- inference || fail "inference/ has uncommitted changes"
cd "$repo" || fail "no $repo"
cargo fmt --all -- --check >/dev/null || fail "cargo fmt --check"
cargo clippy -q -p loadngo-inference --all-targets --all-features -- -D warnings || fail "clippy"
cargo test -q -p loadngo-inference --features agent --lib agent:: >/dev/null || fail "agent tests"
one=$(cargo test -q -p loadngo-inference --features agent --lib "agent::tests::$test_name" -- --exact 2>&1)
grep -q "test result: ok. 1 passed" <<<"$one" || fail "$test_name did not run and pass: $one"
pass "2: fmt, clippy, agent tests and $test_name pass at $(git rev-parse --short HEAD)"

# 3. The live chat.
state=${live%.jsonl}.state.json
[ -f "$live" ] && [ -f "$state" ] || fail "no live chat at $live with its .state.json"
python3 -I - "$live" "$state" <<'EOF' || fail "the live chat does not show the first answer kept"
import json, sys
events = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
answer = json.load(open(sys.argv[2]))["history"][-1]["answer"]
for i, e in enumerate(events):
    if e.get("event") == "note" and e["text"].startswith("[check] the answer"):
        before = [x for x in events[:i] if x.get("event") == "reply"]
        after = [x for x in events[i + 1:] if x.get("event") in ("reply", "note")]
        if (before and len(after) >= 2 and after[0].get("event") == "reply"
                and not after[0].get("calls")
                and after[1].get("event") == "note" and "first answer" in after[1]["text"]
                and answer == before[-1]["text"] and answer != after[0]["text"]):
            print("verify: ok: 3: the note was not acted on; the history keeps the first answer")
            sys.exit(0)
sys.exit(1)
EOF
echo "verify: accepted"
