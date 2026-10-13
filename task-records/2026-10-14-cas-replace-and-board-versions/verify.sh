#!/bin/bash
# The submitter's check for the Task "a replace is one changed version; the board and the
# Task records go into the pudding CAS" (Jay, 2026-10-14: the post-quantum version
# control we are building, not git; sign with this Mac's key). Committed and pushed
# before the work started. task_submitter runs it with LOADNGO_TASK_ARTIFACT_HINT (the
# worker's result) and LOADNGO_TASK_ARTIFACT_COPY_PATH. Exit 0 accepts the work.
#
# Before: archive_cas_add refused a path already in the archive, so updating a file took a
# remove version and an add version; the agent board and the Task records were in no
# archive at all.
#
# What the result must name:
#   commit: <40 hex>        the loadngo commit with the change
#   test: <name>            the data test for a replace
#   manifest: <file name>   the pudding-20260917 version holding the board and records
#   board-copy: <path>      the board as archived (a copy beside this record)
#
# Success criteria:
#  1. The commit is on GitHub's dev and changes data/src/archive_cas.rs and
#     data/src/bin/archive_cas_add.rs.
#  2. HEAD contains it with data/ unchanged; fmt, strict clippy and the data tests pass,
#     and the named test runs and passes.
#  3. GitHub's CI passed on all four platforms for the commit or a later dev commit.
#  4. In a scratch CAS made here: a folder (a.txt, b.txt) is added; then with a.txt
#     changed, b.txt the same and c.txt new, the same add is refused without --replace,
#     and with --replace makes exactly one new version whose parent is the previous one
#     and whose records are "changed" [a.txt] and "created" [c.txt] only; a.txt restores
#     to its new bytes.
#  5. In /Volumes/Zhoenus II/pudding-cas, the named manifest descends from 6a5b6290 (the
#     signed 10-04 version); between them the records name AGENT-BOARD.md as changed and
#     agent-board-archive/2026-10-14-before-condensing.md and files under
#     loadngo/task-records/ as created; its signature verifies against
#     ~/.loadngo/keys/jay-macmini.dilithium2.pub; and AGENT-BOARD.md restored from it is
#     byte for byte the board copy.
set -u
loadngo=/Users/jay/pudding/loadngo
cas="/Volumes/Zhoenus II/pudding-cas"
key=$HOME/.loadngo/keys/jay-macmini.dilithium2.pub
base=6a5b6290edb2bf426fb1ba00847bf4629be00ee47c788b3474f49bbf0fd57c0d
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
manifest=$(field manifest)
board_copy=$(field board-copy)
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "the result names no commit"
[ -n "$test_name" ] && [ -n "$manifest" ] && [ -n "$board_copy" ] || fail "the result lacks test, manifest or board-copy"

# 1. On GitHub.
cd "$loadngo" || fail "no $loadngo"
git fetch -q github dev || fail "cannot fetch github dev"
git merge-base --is-ancestor "$commit" github/dev || fail "$commit is not on github/dev"
changed=$(git diff --name-only "$commit^" "$commit")
for f in data/src/archive_cas.rs data/src/bin/archive_cas_add.rs; do
    grep -qx "$f" <<<"$changed" || fail "$commit does not change $f"
done
pass "1: $commit on github/dev, changes archive_cas.rs and archive_cas_add.rs"

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

# 4. A replace in a scratch CAS.
cargo build -q --release -p data --bin archive_cas_add --bin archive_cas_ingest --bin archive_cas_restore ||
    fail "cannot build the archive tools"
bin=$loadngo/target/release
scratch=$(mktemp -d)
mkdir -p "$scratch/src/folder" "$scratch/cas/manifests"
printf 'first a\n' >"$scratch/src/folder/a.txt"
printf 'same b\n' >"$scratch/src/folder/b.txt"
"$bin/archive_cas_ingest" --cas-root "$scratch/cas" --archive-id scratch --source "$scratch/src" \
    >"$scratch/ingest.log" 2>&1 || fail "cannot ingest the scratch folder: $(tail -3 "$scratch/ingest.log")"
first=$(ls "$scratch/cas/manifests"/scratch-*.json | grep -v signature)
[ "$(wc -l <<<"$first")" -eq 1 ] || fail "the ingest made more than one version"
printf 'second a\n' >"$scratch/src/folder/a.txt"
printf 'new c\n' >"$scratch/src/folder/c.txt"
if "$bin/archive_cas_add" --cas-root "$scratch/cas" --archive scratch --add "$scratch/src/folder" \
    --reason check --actor verify >/dev/null 2>&1; then
    fail "the add without --replace was not refused"
fi
"$bin/archive_cas_add" --cas-root "$scratch/cas" --archive scratch --add "$scratch/src/folder" --replace \
    --reason check --actor verify >"$scratch/replace.log" 2>&1 || fail "the replace failed: $(tail -3 "$scratch/replace.log")"
second=$(ls "$scratch/cas/manifests"/scratch-*.json | grep -v signature | grep -vxF "$first")
[ -n "$second" ] && [ "$(wc -l <<<"$second")" -eq 1 ] || fail "the replace did not make exactly one new version"
python3 -I - "$first" "$second" <<'EOF' || fail "the replace version is not one changed and one created record"
import json, os, sys
first, second = sys.argv[1], sys.argv[2]
new = json.load(open(second))
first_digest = os.path.basename(first).rsplit("-", 1)[1].removesuffix(".json")
assert new["parents"] == [first_digest], (new["parents"], first_digest)
kinds = sorted((r["change"]["kind"], tuple(r["change"]["paths"])) for r in new["records"])
assert kinds == [("changed", ("folder/a.txt",)), ("created", ("folder/c.txt",))], kinds
EOF
"$bin/archive_cas_restore" --cas-root "$scratch/cas" --manifest "$second" --destination "$scratch/out" \
    --path folder/a.txt >/dev/null 2>&1 || fail "cannot restore a.txt"
[ "$(cat "$scratch/out/folder/a.txt")" = "second a" ] || fail "a.txt did not restore to its new bytes"
pass "4: without --replace refused; with it one version: changed [a.txt], created [c.txt]; a.txt restores new"

# 5. The pudding CAS.
manifest_path="$cas/manifests/$manifest"
[ -f "$manifest_path" ] || fail "no $manifest_path (is Zhoenus II attached?)"
python3 -I - "$cas/manifests" "$manifest" "$base" <<'EOF' || fail "the version's lineage or records are not as required"
import json, os, sys
manifests, name, base = sys.argv[1:]
changed, created = set(), set()
current, steps = name, 0
while True:
    m = json.load(open(os.path.join(manifests, current)))
    for r in m.get("records", []):
        kind, paths = r["change"]["kind"], r["change"]["paths"]
        (changed if kind == "changed" else created if kind == "created" else set()).update(paths)
    parents = m.get("parents", [])
    assert parents, f"{current} has no parent before reaching {base}"
    if parents[0] == base:
        break
    current = f"{m['archive_id']}-{parents[0]}.json"
    steps += 1
    assert steps < 20, "more than 20 versions above the base"
assert "AGENT-BOARD.md" in changed, changed
assert "agent-board-archive/2026-10-14-before-condensing.md" in created, sorted(created)[:20]
assert any(p.startswith("loadngo/task-records/") for p in created), sorted(created)[:20]
print(f"verify: ok: 5: {name} descends from {base[:8]}; board changed, its archive and task records created")
EOF
signature="${manifest_path%.json}.signature.json"
cargo build -q --release -p data --bin archive_cas_sign || fail "cannot build archive_cas_sign"
"$bin/archive_cas_sign" verify --cas-root "$cas" --signature "$signature" --trusted-public-key "$key" \
    >"$scratch/sign-verify.log" 2>&1 || fail "the signature does not verify: $(tail -3 "$scratch/sign-verify.log")"
"$bin/archive_cas_restore" --cas-root "$cas" --manifest "$manifest_path" --destination "$scratch/board" \
    --path AGENT-BOARD.md >/dev/null 2>&1 || fail "cannot restore AGENT-BOARD.md from $manifest"
cmp -s "$scratch/board/AGENT-BOARD.md" "$board_copy" || fail "the archived board differs from $board_copy"
pass "5: signed by jay-macmini (verified), and the archived board is the board copy"
echo "verify: accepted"
