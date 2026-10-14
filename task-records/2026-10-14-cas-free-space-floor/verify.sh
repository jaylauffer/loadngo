#!/bin/bash
# The submitter's check for the Task "CAS writes keep a free-space floor" (Jay, 2026-10-14:
# "continue to monitor our disk space, and ensure we do not reach the limit, while
# continuing on this path of PQ git"). Committed and pushed before the work started.
# task_submitter runs it with LOADNGO_TASK_ARTIFACT_HINT and LOADNGO_TASK_ARTIFACT_COPY_PATH.
# Exit 0 accepts the work.
#
# Before: the Archive CAS tools wrote objects (ingest, add, compress, restore) without
# looking at free space; a large ingest or restore could fill a volume. The Mac's internal
# disk was at 94% (56 GiB free) before Jay's cleanup on 10-14.
#
# What the result must name:
#   commit: <40 hex>   the loadngo commit with the change
#   test: <name>       a data test for the floor (refusal leaves nothing written)
#
# Success criteria:
#  1. The commit is on GitHub's dev and changes data/src/archive_cas.rs and adds
#     data/src/disk.rs.
#  2. HEAD contains it with data/ unchanged; fmt, strict clippy and the data tests pass,
#     and the named test runs and passes.
#  3. GitHub's CI passed on all four platforms (Windows included: the free-space query
#     there is its own code) for the commit or a later dev commit.
#  4. With the tools built from HEAD, in a scratch CAS: given --min-free-gib 1000000 (a floor
#     above any disk), archive_cas_ingest, archive_cas_add and archive_cas_restore each
#     exit non-zero with a message naming the free space and the floor, and write nothing:
#     no new manifest or object, no restored file. Without the flag the same add and
#     restore succeed (the default floor, the larger of 10 GiB and 5% of the volume,
#     leaves room on this Mac).
set -u
loadngo=/Users/jay/pudding/loadngo
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
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "the result names no commit"
[ -n "$test_name" ] || fail "the result names no test"

# 1. On GitHub.
cd "$loadngo" || fail "no $loadngo"
git fetch -q github dev || fail "cannot fetch github dev"
git merge-base --is-ancestor "$commit" github/dev || fail "$commit is not on github/dev"
status=$(git diff --name-status "$commit^" "$commit")
grep -Eq "^M[[:space:]]+data/src/archive_cas.rs$" <<<"$status" || fail "$commit does not change data/src/archive_cas.rs"
grep -Eq "^A[[:space:]]+data/src/disk.rs$" <<<"$status" || fail "$commit does not add data/src/disk.rs"
pass "1: $commit on github/dev, changes archive_cas.rs and adds disk.rs"

# 2. The gates.
git merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git diff --quiet HEAD -- data || fail "data/ has uncommitted changes"
cargo fmt --all -- --check >/dev/null || fail "cargo fmt --check"
cargo clippy -q -p data --all-targets --all-features -- -D warnings || fail "clippy"
cargo test -q -p data >/dev/null 2>&1 || fail "data tests"
one=$(cargo test -q -p data "$test_name" 2>&1)
grep -q "test result: ok. [1-9]" <<<"$one" || fail "$test_name did not run and pass: $one"
pass "2: fmt, clippy, data tests and $test_name pass at $(git rev-parse --short HEAD)"

# 3. CI on the four platforms.
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

# 4. The floor, live.
cargo build -q --release -p data --bin archive_cas_ingest --bin archive_cas_add --bin archive_cas_restore ||
    fail "cannot build the archive tools"
bin=$loadngo/target/release
scratch=$(mktemp -d)
mkdir -p "$scratch/src/folder"
printf 'some bytes\n' >"$scratch/src/folder/a.txt"
count() { find "$scratch/cas/objects" -type f 2>/dev/null | wc -l | tr -d ' '; }
manifests() { ls "$scratch/cas/manifests"/*.json 2>/dev/null | grep -v signature | wc -l | tr -d ' '; }
huge=1000000

# Ingest refused, then allowed.
if "$bin/archive_cas_ingest" --source "$scratch/src" --cas-root "$scratch/cas" --archive-id floor \
    --min-free-gib "$huge" >"$scratch/ingest-refused.log" 2>&1; then
    fail "ingest was not refused with a floor above the disk"
fi
grep -qi "free" "$scratch/ingest-refused.log" || fail "the ingest refusal does not name the free space: $(tail -2 "$scratch/ingest-refused.log")"
[ "$(manifests)" = 0 ] || fail "the refused ingest wrote a manifest"
"$bin/archive_cas_ingest" --source "$scratch/src" --cas-root "$scratch/cas" --archive-id floor \
    >"$scratch/ingest.log" 2>&1 || fail "ingest failed under the default floor: $(tail -2 "$scratch/ingest.log")"
before_objects=$(count)
before_manifests=$(manifests)

# Add refused, then allowed.
printf 'more bytes\n' >"$scratch/extra.txt"
if "$bin/archive_cas_add" --cas-root "$scratch/cas" --archive floor --add "$scratch/extra.txt" \
    --reason check --actor verify --min-free-gib "$huge" >"$scratch/add-refused.log" 2>&1; then
    fail "add was not refused with a floor above the disk"
fi
grep -qi "free" "$scratch/add-refused.log" || fail "the add refusal does not name the free space: $(tail -2 "$scratch/add-refused.log")"
[ "$(count)" = "$before_objects" ] && [ "$(manifests)" = "$before_manifests" ] || fail "the refused add wrote an object or a manifest"
"$bin/archive_cas_add" --cas-root "$scratch/cas" --archive floor --add "$scratch/extra.txt" \
    --reason check --actor verify >"$scratch/add.log" 2>&1 || fail "add failed under the default floor: $(tail -2 "$scratch/add.log")"

# Restore refused, then allowed.
manifest=$(ls -t "$scratch/cas/manifests"/floor-*.json | grep -v signature | head -1)
if "$bin/archive_cas_restore" --cas-root "$scratch/cas" --manifest "$manifest" --destination "$scratch/out-refused" \
    --path folder/a.txt --min-free-gib "$huge" >"$scratch/restore-refused.log" 2>&1; then
    fail "restore was not refused with a floor above the disk"
fi
grep -qi "free" "$scratch/restore-refused.log" || fail "the restore refusal does not name the free space: $(tail -2 "$scratch/restore-refused.log")"
[ ! -e "$scratch/out-refused/folder/a.txt" ] || fail "the refused restore wrote the file"
"$bin/archive_cas_restore" --cas-root "$scratch/cas" --manifest "$manifest" --destination "$scratch/out" \
    --path folder/a.txt >/dev/null 2>&1 || fail "restore failed under the default floor"
[ "$(cat "$scratch/out/folder/a.txt")" = "some bytes" ] || fail "the restored file is wrong"
pass "4: ingest, add and restore refused above the floor with nothing written; allowed under the default floor"
echo "verify: accepted"
