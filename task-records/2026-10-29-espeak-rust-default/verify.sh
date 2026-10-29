#!/bin/bash
# The submitter's check for the Task "espeak-ng-rs: the Rust core is the default build"
# (Jay, 2026-10-29: "rust should be the default"). Committed and pushed before the work
# started. task_submitter runs it with LOADNGO_TASK_ARTIFACT_HINT and
# LOADNGO_TASK_ARTIFACT_COPY_PATH. Exit 0 accepts the work.
#
# Today USE_RUST_CORE defaults OFF: a plain CMake build, the Visual Studio solution, the
# Android app and every upstream workflow (CI's 57-lane matrix of architectures, compilers
# and sanitizers; Windows; windows-msbuild; Android; Dist) build the C engine. Only the
# "Rust port" workflow builds Rust, on Linux and macOS. After this Task, Rust is what all of
# them build, and the C engine stays buildable on request (-DUSE_RUST_CORE=OFF) as the
# oracle, checked by its own workflow.
#
# What the result must name:
#   commit: <40 hex>   the last espeak-ng-rs commit of the work
#
# Success criteria:
#  1. The commit is on GitHub's dev. At it, CMakeLists.txt's USE_RUST_CORE option defaults
#     ON; .github/workflows/c-oracle.yml (workflow "C oracle") configures with
#     -DUSE_RUST_CORE=OFF and runs ctest; no other workflow, and nothing under android/,
#     cmake/, src/ or the top CMakeLists.txt, turns USE_RUST_CORE off.
#  2. The Visual Studio solution builds the Rust core: at the commit,
#     src/windows/libespeak-ng.vcxproj defines USE_RUST_CORE and links espeak_ng_rs.lib,
#     and src/windows/espeak-ng.vcxproj compiles rust_cli.c, not espeak-ng.c.
#  3. HEAD contains the commit with the repo's tracked files unchanged. On this Mac a fresh
#     `cmake -S . -B <dir>` with no options caches USE_RUST_CORE:BOOL=ON, builds, links
#     espeak-ng-bin from rust_cli.c alone, and passes every CTest (at least 129).
#  4. A fresh -DUSE_RUST_CORE=OFF build (the C oracle) builds and passes every CTest.
#  5. On GitHub's dev, for the commit or a later commit containing it, each workflow has a
#     successful run with at least this many successful jobs: "Rust port" 7, "CI" 57,
#     "Windows" 8, "windows-msbuild" 2, "Android" 3, "Dist" 3, "C oracle" 2.
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

# 1. On GitHub; Rust by default; the C build kept as the oracle, and only there.
cd "$repo" || fail "no $repo"
git fetch -q origin dev || fail "cannot fetch origin dev"
git merge-base --is-ancestor "$commit" origin/dev || fail "$commit is not on origin/dev"
git show "$commit:CMakeLists.txt" | grep -qE '^option\(USE_RUST_CORE "[^"]*" ON\)' \
    || fail "USE_RUST_CORE does not default ON at $commit"
oracle=$(git show "$commit:.github/workflows/c-oracle.yml" 2>/dev/null) || fail "no c-oracle.yml at $commit"
grep -q '^name: C oracle$' <<<"$oracle" || fail "c-oracle.yml is not named \"C oracle\""
grep -qE 'USE_RUST_CORE(:BOOL)?=OFF' <<<"$oracle" || fail "c-oracle.yml does not turn USE_RUST_CORE off"
grep -q 'ctest' <<<"$oracle" || fail "c-oracle.yml runs no ctest"
others=$(git grep -n -E 'USE_RUST_CORE(:BOOL)?=OFF|USE_RUST_CORE +OFF|USE_RUST_CORE"? *, *"?OFF' "$commit" -- \
    .github/workflows android cmake src CMakeLists.txt | grep -v ':.github/workflows/c-oracle.yml:')
[ -z "$others" ] || fail "USE_RUST_CORE is turned off elsewhere: $others"
pass "1: $commit on origin/dev; USE_RUST_CORE defaults ON; only c-oracle.yml turns it off"

# 2. The Visual Studio solution.
lib=$(git show "$commit:src/windows/libespeak-ng.vcxproj") || fail "no libespeak-ng.vcxproj"
grep -q 'USE_RUST_CORE' <<<"$lib" || fail "libespeak-ng.vcxproj does not define USE_RUST_CORE"
grep -q 'espeak_ng_rs.lib' <<<"$lib" || fail "libespeak-ng.vcxproj does not link espeak_ng_rs.lib"
exe=$(git show "$commit:src/windows/espeak-ng.vcxproj") || fail "no espeak-ng.vcxproj"
grep -qE 'ClCompile Include="[^"]*rust_cli\.c"' <<<"$exe" || fail "espeak-ng.vcxproj does not compile rust_cli.c"
! grep -qE 'ClCompile Include="[^"]*espeak-ng\.c"' <<<"$exe" || fail "espeak-ng.vcxproj still compiles espeak-ng.c"
pass "2: the Visual Studio projects build and link the Rust core and the Rust CLI"

# 3 and 4. Fresh builds on this Mac.
git merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git diff --quiet HEAD || fail "tracked files have uncommitted changes"
work=$(mktemp -d /private/tmp/espeak-task10.XXXXXX) || fail "no scratch directory"
build() {
    local name=$1; shift
    local b="$work/$name"
    cmake -S "$repo" -B "$b" "$@" >"$work/$name-configure.log" 2>&1 || fail "$name: configure (log $work/$name-configure.log)"
    cmake --build "$b" -j 4 >"$work/$name-build.log" 2>&1 || fail "$name: build (log $work/$name-build.log)"
    (cd "$b" && ctest -j 3 --output-on-failure) >"$work/$name-ctest.log" 2>&1 \
        || { tail -20 "$work/$name-ctest.log" >&2; fail "$name: ctest"; }
    local summary
    # CMake 4 prints "100% tests passed out of N"; older versions add ", 0 tests failed".
    summary=$(grep -m1 -E '^100% tests passed(, 0 tests failed)? out of [0-9]+' "$work/$name-ctest.log") \
        || fail "$name: no ctest summary"
    echo "$summary"
}
s=$(build default) || exit 1
grep -q '^USE_RUST_CORE:BOOL=ON$' "$work/default/CMakeCache.txt" || fail "default build did not cache USE_RUST_CORE:BOOL=ON"
objs=$(cd "$work/default/src/CMakeFiles/espeak-ng-bin.dir" && ls ./*.o | tr '\n' ' ')
[ "$objs" = "./rust_cli.c.o " ] || fail "default espeak-ng-bin objects are '$objs', not rust_cli.c.o alone"
n=$(sed -E 's/.* out of ([0-9]+).*/\1/' <<<"$s")
[ "$n" -ge 129 ] || fail "default build ran $n CTests, fewer than 129"
pass "3: default build is the Rust core; espeak-ng-bin from rust_cli.c.o; $s"
s=$(build c-oracle -DUSE_RUST_CORE=OFF) || exit 1
grep -q '^USE_RUST_CORE:BOOL=OFF$' "$work/c-oracle/CMakeCache.txt" || fail "oracle build did not cache USE_RUST_CORE:BOOL=OFF"
pass "4: C oracle build; $s"

# 5. GitHub workflows.
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
    pass "5: $w: run $ok"
done
echo "verify: accepted"
