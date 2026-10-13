#!/bin/bash
# The submitter's check for the Task "the Task tools stop polling their sockets" (Jay,
# 2026-10-14: "fix the 25ms polling issue"). Committed and pushed before the work started.
# task_submitter runs it with LOADNGO_TASK_ARTIFACT_HINT (the worker's result) and
# LOADNGO_TASK_ARTIFACT_COPY_PATH (the copy the receipt hashes). Exit 0 accepts the work.
#
# Before: task_worker, task_submitter, task_offer and task_request tried a non-blocking
# receive and slept 25 ms when nothing had come; task-node checked its sockets from a
# proactor timer every 250 ms (--idle-interval-millis). Measured at intent time with
# /usr/bin/time -l on the Mac mini, doing nothing:
#   task_worker, 5 s listening:            163 context switches
#   task_submitter, 5 s waiting for offers: 167
#   task-node, 10 s running:                 53
#
# What the result must name:
#   commit: <40 hex>   the loadngo commit with the change
#   test: <name>       a network crate test that receives a datagram through the new
#                      proactor path and sees a deadline pass with nothing received
#
# Success criteria:
#  1. The commit is on GitHub's dev and changes network/src/bin/task_worker.rs and
#     network/src/bin/task-node.rs.
#  2. No `thread::sleep` is left in network/src/bin/. task-node still accepts
#     --idle-interval-millis (dolores's launcher passes it).
#  3. At HEAD (containing the commit, network/ unchanged): fmt, strict clippy and the
#     network tests pass, and the named test runs and passes.
#  4. GitHub's CI passed on all four platforms (macOS, Linux x86-64 and arm64, Windows)
#     for the commit or a later dev commit containing it.
#  5. With the binaries built from HEAD, on this Mac:
#     - doing nothing, each makes at most 20 context switches (task_worker 5 s,
#       task_submitter 5 s waiting for offers, task-node 10 s), and the worker and the
#       submitter still end at their deadlines (4.5 to 7 s);
#     - a full loopback cycle is accepted with task_worker, and another with task-node.
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
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "the result names no commit (a line 'commit: <40 hex>')"
[ -n "$test_name" ] || fail "the result names no test"

# 1. On GitHub.
git -C "$repo" fetch -q github dev || fail "cannot fetch github dev"
git -C "$repo" merge-base --is-ancestor "$commit" github/dev || fail "$commit is not on github/dev"
changed=$(git -C "$repo" diff --name-only "$commit^" "$commit")
for f in network/src/bin/task_worker.rs network/src/bin/task-node.rs; do
    grep -qx "$f" <<<"$changed" || fail "$commit does not change $f"
done
pass "1: $commit on github/dev, changes task_worker.rs and task-node.rs"

# 2. No sleeps; the old flag still accepted.
git -C "$repo" merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git -C "$repo" diff --quiet HEAD -- network || fail "network/ has uncommitted changes"
cd "$repo" || fail "no $repo"
if grep -n "thread::sleep" network/src/bin/*.rs; then fail "thread::sleep left in network/src/bin"; fi
grep -q -- "--idle-interval-millis" network/src/bin/task-node.rs || fail "task-node no longer accepts --idle-interval-millis"
pass "2: no thread::sleep in network/src/bin; task-node accepts --idle-interval-millis"

# 3. The gates.
cargo fmt --all -- --check >/dev/null || fail "cargo fmt --check"
cargo clippy -q -p network --all-targets --all-features -- -D warnings || fail "clippy"
cargo test -q -p network >/dev/null 2>&1 || fail "network tests"
one=$(cargo test -q -p network "$test_name" 2>&1)
grep -q "test result: ok. [1-9]" <<<"$one" || fail "$test_name did not run and pass: $one"
pass "3: fmt, clippy, network tests and $test_name pass at $(git rev-parse --short HEAD)"

# 4. CI on the four platforms, for the commit or a descendant on dev.
ci_ok=
while read -r sha conclusion; do
    [ "$conclusion" = success ] || continue
    if git merge-base --is-ancestor "$commit" "$sha" 2>/dev/null; then ci_ok=$sha; break; fi
done < <(gh run list --repo jaylauffer/loadngo --branch dev --workflow CI --limit 30 \
    --json headSha,conclusion --jq '.[] | "\(.headSha) \(.conclusion)"')
[ -n "$ci_ok" ] || fail "no successful CI run on dev contains $commit"
jobs=$(gh run list --repo jaylauffer/loadngo --branch dev --workflow CI --limit 30 --json headSha,databaseId \
    --jq ".[] | select(.headSha == \"$ci_ok\") | .databaseId" | head -1)
n=$(gh run view "$jobs" --repo jaylauffer/loadngo --json jobs --jq '[.jobs[] | select(.conclusion == "success")] | length')
[ "$n" -ge 4 ] || fail "CI run $jobs for $ci_ok has $n successful jobs, not 4"
pass "4: CI run $jobs passed ($n jobs) at ${ci_ok:0:8}, which contains $commit"

# 5. The binaries from HEAD, on this Mac.
cargo build -q -p network --bin task_submitter --bin task_worker --bin task-node || fail "cannot build the Task binaries"
bin=$repo/target/debug
demo=$(dirname "$copy")/demo
rm -rf "$demo"
mkdir -p "$demo"
group='ff02::4c4e:4%1'
# switches LOG: voluntary + involuntary context switches from /usr/bin/time -l.
switches() { awk '/context switches/ { n += $1 } END { print n + 0 }' "$1"; }
seconds() { awk '/ real / { print $1 }' "$1"; }

/usr/bin/time -l "$bin/task_worker" --worker-node-id idle-worker --reply-endpoint 127.0.0.1:47961 \
    --bind-port 47961 --multicast-v6 "$group" --capability none --listen-seconds 5 \
    --execute-command true >"$demo/idle-worker.log" 2>&1
/usr/bin/time -l "$bin/task_submitter" --multicast-port 47963 --submitter-node-id idle-submitter \
    --reply-endpoint 127.0.0.1:47962 --bind-port 47962 --multicast-v6 "$group" --summary idle \
    --verify-command true --offer-timeout-seconds 5 >"$demo/idle-submitter.log" 2>&1
/usr/bin/time -l "$bin/task-node" --worker-node-id idle-node --reply-endpoint 127.0.0.1:47964 \
    --bind-port 47964 --multicast-v6 "$group" --capability none --execute-command true \
    --run-seconds 10 >"$demo/idle-node.log" 2>&1
for name in idle-worker idle-submitter idle-node; do
    n=$(switches "$demo/$name.log")
    [ "$n" -le 20 ] || fail "$name made $n context switches doing nothing (at most 20)"
    pass "5: $name made $n context switches in $(seconds "$demo/$name.log") s doing nothing"
done
grep -q task_worker_timeout "$demo/idle-worker.log" || fail "the idle worker did not end at its deadline"
grep -q "no task offers" "$demo/idle-submitter.log" || fail "the idle submitter did not end at its deadline"
for name in idle-worker idle-submitter; do
    awk -v s="$(seconds "$demo/$name.log")" 'BEGIN { exit !(s >= 4.5 && s <= 7) }' ||
        fail "$name ended after $(seconds "$demo/$name.log") s, not 4.5 to 7"
done
pass "5: the idle worker and submitter ended at their 5 s deadlines"

cycle() { # name, port base, worker binary, its extra flags
    local dir=$demo/$1 wp=$(($2 + 1)) sp=$2 worker_bin=$3 extra=$4
    mkdir -p "$dir"
    # shellcheck disable=SC2086
    "$bin/$worker_bin" --worker-node-id "demo-$1" --reply-endpoint 127.0.0.1:$wp --bind-port $wp \
        --multicast-v6 "$group" --capability demo --artifact-hint "$dir/artifact.txt" $extra \
        --execute-command "printf 'demo artifact\n' > \"\$LOADNGO_TASK_ARTIFACT_HINT\"" \
        >"$dir/worker.log" 2>&1 &
    local worker=$!
    sleep 2
    "$bin/task_submitter" --multicast-port $wp --submitter-node-id demo-submitter \
        --reply-endpoint 127.0.0.1:$sp --bind-port $sp --request-target 127.0.0.1:$wp \
        --multicast-v6 "$group" --capability demo --summary "polling-free cycle ($1)" \
        --artifact-hint "$dir/artifact.txt" --artifact-copy-path "$dir/artifact-copy.txt" \
        --receipt-path "$dir/closure.json" --offer-timeout-seconds 3 --result-timeout-seconds 60 \
        --verify-command "cp \"\$LOADNGO_TASK_ARTIFACT_HINT\" \"\$LOADNGO_TASK_ARTIFACT_COPY_PATH\" && echo checked" \
        >"$dir/submitter.log" 2>&1
    echo $? >"$dir/submitter.exit"
    wait $worker
    [ "$(cat "$dir/submitter.exit")" = 0 ] || fail "$1 cycle: the submitter failed"
    grep -q '"accepted": true' "$dir/closure.json" || fail "$1 cycle: not accepted"
    pass "5: a loopback cycle with $worker_bin was accepted"
}
cycle worker 47970 task_worker "--listen-seconds 30 --ack-timeout-seconds 60"
cycle node 47972 task-node "--run-seconds 15 --ack-timeout-seconds 60"
echo "verify: accepted"
