#!/bin/bash
# The submitter's check for the Task "espeak-ng-rs: a live MBROLA test, Rust against the C
# oracle" (Jay, 2026-10-29: "complete the port"). Committed and pushed before the work
# started. task_submitter runs it with LOADNGO_TASK_ARTIFACT_HINT and
# LOADNGO_TASK_ARTIFACT_COPY_PATH. Exit 0 accepts.
#
# Task 17 moved the MBROLA wrapper to Rust (rust/mbrowrap_compat.rs: on Unix it starts the
# mbrola program and talks to it over pipes). Nothing runs that path against a real mbrola:
# no CI lane installs it, and tests/mbrola.test, upstream's live test (it speaks "Bonjour"
# with the mb-fr4 voice and compares the WAV's SHA-1), prints "checking if MBROLA is
# installed ... no" and exits 0 when mbrola is missing, so it passes without testing.
#
# What the result must name:
#   commit: <40 hex>   the last espeak-ng-rs commit of the work
#
# Success criteria:
#  1. The commit is on GitHub's dev, and HEAD contains it with tracked files unchanged.
#  2. At the commit, tests/mbrola.test fails (exit status not 0) when ESPEAK_REQUIRE_MBROLA=1
#     is set and mbrola is not installed (as on this Mac), instead of passing untested.
#  3. On GitHub's dev, for the commit or a later commit containing it, a successful "Rust
#     port" run whose Linux shared job (the Rust engine with USE_MBROLA=ON) and a
#     successful "C oracle" run whose Linux job (the C engine) both log, from the same
#     tests/mbrola.test with ESPEAK_REQUIRE_MBROLA=1, "testing mb-fr4 Bonjour" and
#     "testing mb-fr4+announcer Bonjour", and neither logs "was not tested". The same
#     expected SHA-1 values then hold for both engines.
#  4. On GitHub's dev, for the commit or a later commit containing it, each workflow has
#     a successful run with at least this many successful jobs: "Rust port" 7, "CI" 57,
#     "Windows" 8, "windows-msbuild" 2, "Android" 3, "Dist" 3, "C oracle" 2.
set -u
repo=/Users/jay/pudding/espeak-ng-rs
gh_repo=jaylauffer/espeak-ng-rs
fail() { echo "verify: FAIL: $*" >&2; exit 1; }
pass() { echo "verify: ok: $*"; }

result=${LOADNGO_TASK_ARTIFACT_HINT:?no artifact hint}
copy=${LOADNGO_TASK_ARTIFACT_COPY_PATH:?no artifact copy path}
[ -f "$result" ] || fail "no result file at $result"
cp "$result" "$copy" || fail "cannot copy the result to $copy"
commit=$(sed -n 's/^commit: *//p' "$copy" | head -1)
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "the result names no commit"

cd "$repo" || fail "no $repo"
git fetch -q origin dev || fail "cannot fetch origin dev"
git merge-base --is-ancestor "$commit" origin/dev || fail "$commit is not on origin/dev"
git merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git diff --quiet HEAD || fail "tracked files have uncommitted changes"
pass "1: $commit on origin/dev; HEAD contains it, tracked files unchanged"

command -v mbrola >/dev/null && fail "mbrola is installed here; criterion 2 needs a machine without it"
work=$(mktemp -d /private/tmp/espeak-task19.XXXXXX) || fail "no scratch directory"
git worktree add -q --detach "$work/src" "$commit" || fail "cannot check out $commit"
trap 'git -C "$repo" worktree remove --force "$work/src" >/dev/null 2>&1' EXIT
mkdir -p "$work/run"
(cd "$work/run" && ESPEAK_REQUIRE_MBROLA=1 sh "$work/src/tests/mbrola.test") >"$work/required.log" 2>&1 \
    && fail "mbrola.test passed without mbrola under ESPEAK_REQUIRE_MBROLA=1: $(tr '\n' ' ' <"$work/required.log")"
pass "2: without mbrola, the required test fails: $(tail -1 "$work/required.log")"

# live <workflow> <job name pattern>: a successful run containing the commit whose job logs
# both mb-fr4 cases and nothing untested.
live() {
    local w=$1 pattern=$2 id sha job log
    for id in $(gh run list -R "$gh_repo" -b dev -w "$w" -L 40 \
            --json databaseId,conclusion --jq '.[] | select(.conclusion=="success") | .databaseId'); do
        sha=$(gh run view "$id" -R "$gh_repo" --json headSha --jq .headSha)
        git merge-base --is-ancestor "$commit" "$sha" 2>/dev/null || continue
        job=$(gh run view "$id" -R "$gh_repo" --json jobs \
            --jq ".jobs[] | select(.conclusion==\"success\" and (.name | test(\"$pattern\"))) | .databaseId" | head -1)
        [ -n "$job" ] || continue
        log=$(gh run view -R "$gh_repo" --job "$job" --log 2>/dev/null) || continue
        grep -q 'testing mb-fr4 Bonjour' <<<"$log" || continue
        grep -q 'testing mb-fr4+announcer Bonjour' <<<"$log" || continue
        grep -q 'was not tested' <<<"$log" && continue
        grep -q 'ESPEAK_REQUIRE_MBROLA' <<<"$log" || continue
        echo "run $id job $job at ${sha:0:8}"; return 0
    done
    return 1
}
r=$(live "Rust port" '^speech-parity \\(ubuntu-latest, ON\\)$') || fail "no Rust port run containing $commit tested MBROLA live"
c=$(live "C oracle" '^c-engine \\(ubuntu-latest\\)$') || fail "no C oracle run containing $commit tested MBROLA live"
pass "3: live MBROLA, Rust engine: $r; C engine: $c"

for spec in "Rust port:7" "CI:57" "Windows:8" "windows-msbuild:2" "Android:3" "Dist:3" "C oracle:2"; do
    w=${spec%:*}; need=${spec##*:}; ok=""
    for id in $(gh run list -R "$gh_repo" -b dev -w "$w" -L 40 \
            --json databaseId,conclusion --jq '.[] | select(.conclusion=="success") | .databaseId'); do
        sha=$(gh run view "$id" -R "$gh_repo" --json headSha --jq .headSha)
        git merge-base --is-ancestor "$commit" "$sha" 2>/dev/null || continue
        jobs=$(gh run view "$id" -R "$gh_repo" --json jobs \
            --jq '[.jobs[] | select(.conclusion=="success")] | length')
        [ "$jobs" -ge "$need" ] && { ok="$id ($jobs jobs at ${sha:0:8})"; break; }
    done
    [ -n "$ok" ] || fail "no successful \"$w\" run on dev with $need jobs contains $commit"
    pass "4: $w: run $ok"
done
echo "verify: accepted"
