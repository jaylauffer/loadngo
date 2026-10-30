#!/bin/bash
# The submitter's check for the Task "espeak-ng-rs: the engine's mutable state moves from C
# to Rust" (Jay, 2026-10-29: "complete the port"; the port is finished when Rust is the
# engine and the default build, the C API stays as a layer over it, and the front ends stay
# for now). Committed and pushed before the work started. task_submitter runs it with
# LOADNGO_TASK_ARTIFACT_HINT and LOADNGO_TASK_ARTIFACT_COPY_PATH. Exit 0 accepts.
#
# After Task 17 every library and CLI source held only bridges to Rust (c_inventory: 0 lines
# of C logic in both configurations), but the C files still defined the engine's mutable
# state. At espeak-ng-rs 72a8c53b, nm over the C objects of the espeak-ng library and CLI
# lists 66 symbols in writable data sections, the same 66 in the measured and the
# asynchronous and MBROLA configurations: translate.c 43 (translator, option_*, skip_*,
# word_phonemes, the clause source...), voices.c 10 (voice, voicedata, voices_list,
# tone_points, formant_rate...), speech.c 5, soundicon.c 3, readclause.c 3, dictionary.c 2.
# Read-only tables (__DATA,__const and __TEXT) are not state and may stay in C.
#
# Once Rust defines them, a shared library on Linux would export names such as "voice" and
# "translator" (a Rust static archive's symbols have default visibility), and a program that
# defines the same name would take over the engine's copy. So the shared library must export
# only the public API: espeak_* and espeak_ng_* functions (an upper-case letter after the
# prefix), the text decoder functions of espeak-ng/encoding.h, and espeak_rs_cli_main for
# the command-line program.
#
# What the result must name:
#   commit: <40 hex>   the last espeak-ng-rs commit of the work
#
# Success criteria:
#  1. The commit is on GitHub's dev, and HEAD contains it with tracked files unchanged.
#  2. At the commit, in a fresh measured configuration and a fresh asynchronous and MBROLA
#     configuration (-DUSE_ASYNC=ON -DUSE_MBROLA=ON), with the espeak-ng library and CLI
#     built: no C object of the library or CLI defines a symbol in a writable data section
#     (__DATA,__data, __DATA,__bss, __DATA,__common or common), and c_inventory still
#     reports 0 lines of C or mixed logic in both.
#  3. At the commit, a fresh shared build (-DBUILD_SHARED_LIBS=ON -DUSE_ASYNC=ON
#     -DUSE_MBROLA=ON) gives a libespeak-ng.dylib that exports only the public API above,
#     the CLI built against it runs (--version), and the "Rust port" workflow checks the
#     exports of libespeak-ng.so on Linux.
#  4. A fresh default build passes every CTest (at least 131), and a fresh
#     -DUSE_RUST_CORE=OFF build (the C oracle) passes every CTest.
#  5. On GitHub's dev, for the commit or a later commit containing it, each workflow has
#     a successful run with at least this many successful jobs: "Rust port" 7, "CI" 57,
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

cd "$repo" || fail "no $repo"
git fetch -q origin dev || fail "cannot fetch origin dev"
git merge-base --is-ancestor "$commit" origin/dev || fail "$commit is not on origin/dev"
git merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git diff --quiet HEAD || fail "tracked files have uncommitted changes"
pass "1: $commit on origin/dev; HEAD contains it, tracked files unchanged"

work=$(mktemp -d /private/tmp/espeak-task18.XXXXXX) || fail "no scratch directory"
git worktree add -q --detach "$work/src" "$commit" || fail "cannot check out $commit"
trap 'git -C "$repo" worktree remove --force "$work/src" >/dev/null 2>&1' EXIT

# One line per writable data symbol a C object of the library or CLI defines.
writable() {
    find "$1/src" -name '*.c.o' \( -path '*espeak-ng.dir*' -o -path '*espeak-ng-bin.dir*' \) | sort |
    while read -r o; do
        nm -m "$o" | awk -v f="$(basename "$o" .c.o)" '
            /undefined/ { next }
            /\(common\)/ { print f ": " $NF; next }
            /\(__DATA,__(data|bss|common)\)/ { if ($NF !~ /^(ltmp|l_|L)/) print f ": " $NF }'
    done
}
for spec in "measured:-DUSE_ASYNC=OFF -DUSE_MBROLA=OFF" "async-mbrola:-DUSE_ASYNC=ON -DUSE_MBROLA=ON"; do
    name=${spec%%:*}; options=${spec#*:}; b="$work/$name"
    cmake -S "$work/src" -B "$b" -DUSE_RUST_CORE=ON $options -DUSE_LIBSONIC=OFF \
        -DUSE_LIBPCAUDIO=OFF -DCMAKE_EXPORT_COMPILE_COMMANDS=ON >"$b.log" 2>&1 \
        || fail "$name: configure (log $b.log)"
    cmake --build "$b" -j 4 --target espeak-ng espeak-ng-bin >>"$b.log" 2>&1 || fail "$name: build (log $b.log)"
    n=$(find "$b/src" -name '*.c.o' -path '*espeak-ng.dir*' | wc -l)
    [ "$n" -ge 20 ] || fail "$name: only $n library objects found"
    state=$(writable "$b")
    [ -z "$state" ] || fail "$name: C objects still define writable data: $(tr '\n' ' ' <<<"$state")"
    python3 "$work/src/tools/c_inventory.py" "$b" >"$b.txt" 2>&1 || fail "c_inventory $name (log $b.txt)"
    total=$(awk -F'; ' '/ lines of C logic/ {split($3, a, " "); s += a[1]} END {print s+0}' "$b.txt")
    [ "$total" -eq 0 ] || fail "$name: $total lines of C logic, not 0"
done
pass "2: no writable data in the library's or CLI's C objects, and no C logic, in either configuration"

b="$work/shared"
cmake -S "$work/src" -B "$b" -DUSE_RUST_CORE=ON -DBUILD_SHARED_LIBS=ON -DUSE_ASYNC=ON -DUSE_MBROLA=ON \
    -DUSE_LIBSONIC=OFF -DUSE_LIBPCAUDIO=OFF >"$b.log" 2>&1 || fail "shared: configure (log $b.log)"
cmake --build "$b" -j 4 --target espeak-ng espeak-ng-bin >>"$b.log" 2>&1 || fail "shared: build (log $b.log)"
lib=$(find "$b/src/libespeak-ng" -maxdepth 1 -name 'libespeak-ng*.dylib' -type f | head -1)
[ -n "$lib" ] || fail "shared: no libespeak-ng dylib"
exports=$(nm -gU "$lib" | awk '{print $3}')
[ "$(wc -l <<<"$exports")" -ge 50 ] || fail "shared: only $(wc -l <<<"$exports") exports; the public API is missing"
extra=$(grep -vE '^_(espeak_[A-Z]|espeak_ng_[A-Z]|create_text_decoder$|destroy_text_decoder$|text_decoder_[a-z_]+$|espeak_rs_cli_main$)' <<<"$exports")
[ -z "$extra" ] || fail "shared: the dylib exports more than the public API: $(head -20 <<<"$extra" | tr '\n' ' ')"
"$b/src/espeak-ng" --version >"$b.version" 2>&1 || fail "shared: the CLI does not run against the library ($(head -3 "$b.version" | tr '\n' ' '))"
grep -q 'nm -D --defined-only' "$work/src/.github/workflows/rust.yml" \
    || fail "the Rust port workflow does not check the Linux library's exports"
pass "3: the shared library exports only the public API ($(wc -l <<<"$exports") symbols)"

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
pass "4: default build $s; C oracle $s2"

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
