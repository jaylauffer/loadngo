#!/bin/bash
# The submitter's check for the Task "espeak-ng-rs: translate.c's C logic moves to Rust" (Jay, 2026-10-29: "complete the port"; the port is finished when Rust is
# the engine and the default build, the C API stays as a layer over it, and the front ends
# stay for now). Committed and pushed before the work started. task_submitter runs it with
# LOADNGO_TASK_ARTIFACT_HINT and LOADNGO_TASK_ARTIFACT_COPY_PATH. Exit 0 accepts.
#
# tools/c_inventory.py preprocesses every library and CLI source exactly as a build
# compiles it and lists each surviving function as C logic, mixed (C around Rust calls)
# or a bridge to Rust. In the measured configuration (docs/REMAINING_PORT.md) it counted
# 395 lines of C and mixed logic on 2026-10-30 after Task 15, all of them in translate.c
# (the alternate translators, the clause, word-to-list, replacement, character and text
# control hosts' callbacks).
#
# What the result must name:
#   commit: <40 hex>   the last espeak-ng-rs commit of the work
#
# Success criteria:
#  1. The commit is on GitHub's dev, and HEAD contains it with tracked files unchanged.
#  2. At the commit, c_inventory over a fresh measured configuration reports 0 lines of
#     C or mixed logic in translate.c, and 0 in total: every library and CLI source of
#     the measured configuration holds only bridges to Rust.
#  3. A fresh default build passes every CTest (at least 131), and a fresh
#     -DUSE_RUST_CORE=OFF build (the C oracle) passes every CTest.
#  4. On GitHub's dev, for the commit or a later commit containing it, each workflow has
#     a successful run with at least this many successful jobs: "Rust port" 7, "CI" 57,
#     "Windows" 8, "windows-msbuild" 2, "Android" 3, "Dist" 3, "C oracle" 2.
#
# Revised on 2026-10-30, and pushed before this Task's result is checked again: the
# first version required at least 133 CTests of the default build. That count was the
# MBROLA-on gate build's; this Mac's default build has USE_MBROLA off and runs 131 (no
# mbrola or rust_mbrola_host test), and Task 11's first result was rejected on it.
# Nothing else changed.
set -u
repo=/Users/jay/pudding/espeak-ng-rs
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

work=$(mktemp -d /private/tmp/espeak-task16.XXXXXX) || fail "no scratch directory"
git worktree add -q --detach "$work/src" "$commit" || fail "cannot check out $commit"
trap 'git -C "$repo" worktree remove --force "$work/src" >/dev/null 2>&1' EXIT
cmake -S "$work/src" -B "$work/inventory" -DUSE_RUST_CORE=ON -DUSE_ASYNC=OFF -DUSE_MBROLA=OFF \
    -DUSE_LIBSONIC=OFF -DUSE_LIBPCAUDIO=OFF -DCMAKE_EXPORT_COMPILE_COMMANDS=ON \
    >"$work/inventory.log" 2>&1 || fail "inventory configure (log $work/inventory.log)"
python3 "$work/src/tools/c_inventory.py" "$work/inventory" >"$work/inventory.txt" 2>&1 \
    || fail "c_inventory (log $work/inventory.txt)"
for f in src/libespeak-ng/translate.c; do
    line=$(grep -F "$f: " "$work/inventory.txt")
    if [ -n "$line" ]; then
        n=$(sed -E 's/.*; ([0-9]+) lines of C logic.*/\1/' <<<"$line")
        [ "$n" = 0 ] || fail "$f still has $n lines of C logic: $line"
    fi
done
total=$(awk -F'; ' '/ lines of C logic/ {split($3, a, " "); s += a[1]} END {print s+0}' "$work/inventory.txt")
[ "$total" -eq 0 ] || fail "$total lines of C logic in total, not 0"
pass "2: translate.c holds no C logic; $total lines in total"

build() {
    local name=$1; shift
    local b="$work/$name"
    cmake -S "$work/src" -B "$b" "$@" >"$work/$name-configure.log" 2>&1 || fail "$name: configure (log $work/$name-configure.log)"
    cmake --build "$b" -j 4 >"$work/$name-build.log" 2>&1 || fail "$name: build (log $work/$name-build.log)"
    (cd "$b" && ctest -j 3 --output-on-failure) >"$work/$name-ctest.log" 2>&1 \
        || { tail -20 "$work/$name-ctest.log" >&2; fail "$name: ctest"; }
    grep -m1 -E '^100% tests passed(, 0 tests failed)? out of [0-9]+' "$work/$name-ctest.log" \
        || fail "$name: no ctest summary"
}
s=$(build default) || exit 1
n=$(sed -E 's/.* out of ([0-9]+).*/\1/' <<<"$s")
[ "$n" -ge 131 ] || fail "default build ran $n CTests, fewer than 131"
s2=$(build c-oracle -DUSE_RUST_CORE=OFF) || exit 1
pass "3: default build $s; C oracle $s2"

for spec in "Rust port:7" "CI:57" "Windows:8" "windows-msbuild:2" "Android:3" "Dist:3" "C oracle:2"; do
    w=${spec%:*}; need=${spec##*:}; ok=""
    for id in $(gh run list -R jaylauffer/espeak-ng-rs -b dev -w "$w" -L 40 \
            --json databaseId,conclusion --jq '.[] | select(.conclusion=="success") | .databaseId'); do
        sha=$(gh run view "$id" -R jaylauffer/espeak-ng-rs --json headSha --jq .headSha)
        git merge-base --is-ancestor "$commit" "$sha" 2>/dev/null || continue
        jobs=$(gh run view "$id" -R jaylauffer/espeak-ng-rs --json jobs \
            --jq '[.jobs[] | select(.conclusion=="success")] | length')
        [ "$jobs" -ge "$need" ] && { ok="$id ($jobs jobs at ${sha:0:8})"; break; }
    done
    [ -n "$ok" ] || fail "no successful \"$w\" run on dev with $need jobs contains $commit"
    pass "4: $w: run $ok"
done
echo "verify: accepted"
