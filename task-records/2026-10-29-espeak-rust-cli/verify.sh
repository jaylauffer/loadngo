#!/bin/bash
# The submitter's check for the Task "espeak-ng-rs: the espeak-ng command-line program
# is Rust" (Jay, 2026-10-29: "yes takeover, and rust should be the default", taking over
# Codex's stalled 10-14 CLI stage). Committed and pushed before the work started.
# task_submitter runs it with LOADNGO_TASK_ARTIFACT_HINT and LOADNGO_TASK_ARTIFACT_COPY_PATH.
# Exit 0 accepts the work.
#
# With USE_RUST_CORE=ON the espeak-ng executable is built from a small C entry
# (src/rust_cli.c: getopt and the standard streams) and Rust (rust/cli_native.rs,
# rust/cli_io.rs): options, help, voice listing, input framing, WAV writing. The
# original src/espeak-ng.c stays unchanged as a test-only oracle
# (espeak-ng-cli-reference), and tests/rust_cli_parity.py compares the two programs.
# Codex's draft (uncommitted, 10-14) passed 174 of 175 cases: with -f FILE and
# --stdin both given, the original reads stdin and the draft read the file.
#
# What the result must name:
#   commit: <40 hex>   the last espeak-ng-rs commit of the work
#
# Success criteria:
#  1. The commit is on GitHub's dev. At it, src/CMakeLists.txt builds espeak-ng-bin from
#     rust_cli.c under USE_RUST_CORE, rust/cli_native.rs and rust/cli_io.rs exist, and
#     src/espeak-ng.c is identical to the oracle commit ba90c8e9.
#  2. HEAD contains it with rust/, src/ and tests/ unchanged; cargo fmt --check, strict
#     clippy (all targets, all features), the native-host library clippy for this Mac and
#     for x86_64-pc-windows-msvc, and tools/generate_rust_{tables,languages}.py --check pass.
#  3. A fresh static, synchronous Rust-core build (no async, MBROLA, pcaudio or sonic)
#     links espeak-ng-bin from rust_cli.c only, and rust_cli_parity passes every case,
#     at least 175.
#  4. A fresh shared Rust-core build with async and MBROLA (CI's shared lane) does the
#     same.
#  5. GitHub's "Rust port" workflow (3 cargo jobs, 4 CMake/CTest jobs on Linux and macOS)
#     succeeded for the commit or a later dev commit containing it.
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

# 1. On GitHub, with the shape the intent describes.
cd "$repo" || fail "no $repo"
git fetch -q origin dev || fail "cannot fetch origin dev"
git merge-base --is-ancestor "$commit" origin/dev || fail "$commit is not on origin/dev"
git show "$commit:src/CMakeLists.txt" | grep -q 'add_executable(espeak-ng-bin rust_cli.c)' \
    || fail "src/CMakeLists.txt at $commit does not build espeak-ng-bin from rust_cli.c"
for f in rust/cli_native.rs rust/cli_io.rs src/rust_cli.c; do
    git cat-file -e "$commit:$f" || fail "no $f at $commit"
done
git diff --quiet ba90c8e9 "$commit" -- src/espeak-ng.c || fail "src/espeak-ng.c differs from the oracle ba90c8e9"
pass "1: $commit on origin/dev; espeak-ng-bin from rust_cli.c; src/espeak-ng.c unchanged"

# 2. The gates.
git merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git diff --quiet HEAD -- rust src tests || fail "rust/, src/ or tests/ have uncommitted changes"
cargo fmt --check >/dev/null || fail "cargo fmt --check"
cargo clippy -q --locked --all-targets --all-features -- -D warnings || fail "clippy"
ESPEAK_NATIVE_HOST=1 cargo clippy -q --locked --lib --all-features -- -D warnings || fail "native-host clippy"
ESPEAK_NATIVE_HOST=1 cargo clippy -q --locked --lib --all-features --target x86_64-pc-windows-msvc -- -D warnings \
    || fail "native-host clippy for Windows"
python3 tools/generate_rust_tables.py --check >/dev/null || fail "generated tables are stale"
python3 tools/generate_rust_languages.py --check >/dev/null || fail "generated languages are stale"
pass "2: fmt, clippy (all targets; native host on macOS and Windows), generated sources"

# 3 and 4. Fresh builds; the parity test.
work=$(mktemp -d /private/tmp/espeak-task9.XXXXXX) || fail "no scratch directory"
lane() {
    local name=$1; shift
    local b="$work/$name"
    cmake -S "$repo" -B "$b" -DUSE_RUST_CORE=ON -DUSE_LIBPCAUDIO=OFF -DUSE_LIBSONIC=OFF "$@" \
        >"$work/$name-configure.log" 2>&1 || fail "$name: configure (log $work/$name-configure.log)"
    cmake --build "$b" -j 4 >"$work/$name-build.log" 2>&1 || fail "$name: build (log $work/$name-build.log)"
    local objs
    objs=$(cd "$b/src/CMakeFiles/espeak-ng-bin.dir" && ls ./*.o | tr '\n' ' ')
    [ "$objs" = "./rust_cli.c.o " ] || fail "$name: espeak-ng-bin objects are '$objs', not rust_cli.c.o alone"
    # -V: ctest prints a passing test's output (the summary line) only when verbose,
    # with a "<test number>: " prefix. The first run used --output-on-failure and
    # was rejected for that alone.
    (cd "$b" && ctest -R '^rust_cli_parity$' -V) >"$work/$name-ctest.log" 2>&1 \
        || { tail -20 "$work/$name-ctest.log" >&2; fail "$name: rust_cli_parity"; }
    local line
    line=$(grep -m1 -E '^([0-9]+: )?CLI parity: [0-9]+/[0-9]+ cases passed' "$work/$name-ctest.log" \
        | sed -E 's/^[0-9]+: //')
    [[ $line =~ ^CLI\ parity:\ ([0-9]+)/([0-9]+) ]] || fail "$name: no parity summary"
    [ "${BASH_REMATCH[1]}" = "${BASH_REMATCH[2]}" ] && [ "${BASH_REMATCH[2]}" -ge 175 ] \
        || fail "$name: $line"
    pass "$name: espeak-ng-bin from rust_cli.c.o; $line"
}
lane static -DBUILD_SHARED_LIBS=OFF -DUSE_ASYNC=OFF -DUSE_MBROLA=OFF
pass "3: static synchronous lane"
lane shared -DBUILD_SHARED_LIBS=ON -DUSE_ASYNC=ON -DUSE_MBROLA=ON
pass "4: shared async MBROLA lane"

# 5. GitHub CI.
ok=""
for sha in $(gh run list -R jaylauffer/espeak-ng-rs -b dev -w "Rust port" -L 30 \
        --json headSha,conclusion --jq '.[] | select(.conclusion=="success") | .headSha'); do
    git merge-base --is-ancestor "$commit" "$sha" 2>/dev/null && { ok=$sha; break; }
done
[ -n "$ok" ] || fail "no successful Rust port run on dev contains $commit"
pass "5: Rust port workflow succeeded at $ok"
echo "verify: accepted"
