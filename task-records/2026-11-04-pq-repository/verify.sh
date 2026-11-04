#!/bin/bash
# The submitter's check for the Task "loadngo in our PQ version control" (Jay, 2026-11-04,
# after "how are we on building our PQ git replacement?": "go, make this work, loadngo,
# needs you"). Committed and pushed before the work started. task_submitter runs it with
# LOADNGO_TASK_ARTIFACT_HINT and LOADNGO_TASK_ARTIFACT_COPY_PATH. Exit 0 accepts the work.
#
# Before: loadngo's history is kept only in git and on GitHub. The pudding CAS signs
# versions of the board and the Task records, but each version is one flat list of files
# without executable bits, and nothing checks a whole version out as a working tree,
# commits from one, or sends versions to another machine. archive_cas_gc deletes every
# object no archive manifest lists.
#
# What the result must name:
#   commit: <40 hex>    the loadngo commit that adds the tool
#   dolores: <40 hex>   the commit dolores's ~/pudding/loadngo is at; it contains the tool
#
# Success criteria:
#  1. The commit is on GitHub's dev and adds data/src/repository.rs and
#     data/src/bin/loadngo-repo.rs.
#  2. HEAD of this checkout contains it with data/ unchanged; fmt, strict clippy of data
#     and the data tests pass.
#  3. GitHub's CI passed on all four platforms for the commit or a later dev commit.
#  4. Import: `loadngo-repo import` of GitHub's dev into a new store makes one version per
#     git commit; `log` lists them newest first in git's order, each naming its git commit
#     and, as its parent, the version of its git parent. Importing dev~7 and then dev into
#     another new store lists exactly the same versions (content-addressed; imports resume).
#  5. Fidelity: every version checks out (`checkout --version`) to exactly git's tree for
#     its commit: `git archive` of the commit and the checkout hold the same files with
#     the same bytes, the same executable files and the same symlinks.
#  6. Signatures: `verify` of the branch passes with this Mac's key (jay-macmini). `verify`
#     fails, and so does `checkout` of the branch, leaving nothing at its destination, when
#     (a) one stored file object is altered by one byte, (b) the head is edited to name
#     another version, or (c) the trusted key is a different one.
#  7. Commit: in a checkout of the head, a file is changed, one created in a new folder,
#     one deleted, one made executable, one created under the ignored target/ and one more
#     changed. `status` lists those five changes and not the ignored file. `commit` naming
#     the first four paths makes a signed version whose parent is the old head and whose
#     author is the one given; afterwards `status` lists only the unnamed change, `verify`
#     passes, and a fresh checkout of the new head matches the working tree apart from the
#     unnamed change and target/.
#  8. Store upkeep: `archive_cas_gc --execute` on the store keeps everything the
#     repository needs: `verify` still passes afterwards.
#  9. Send: `send --to` another new store on this Mac gives a store whose `verify` passes.
#     `send --to-ssh` to a new store on dolores, received by the tool built there at the
#     named commit, gives a store that verifies there; its head checks out on dolores to
#     exactly git's tree, with no .git, and loadngo-repo builds from that tree
#     (`cargo build --locked --release -p data --bin loadngo-repo`) and runs.
set -u
loadngo=/Users/jay/pudding/loadngo-claudette
dolores=jay@10.10.10.6
fail() {
    echo "verify: FAIL: $*" >&2
    exit 1
}
pass() { echo "verify: ok: $*"; }
on() { ssh -o BatchMode=yes -o ConnectTimeout=20 "$dolores" "$@"; }

result=${LOADNGO_TASK_ARTIFACT_HINT:?no artifact hint}
copy=${LOADNGO_TASK_ARTIFACT_COPY_PATH:?no artifact copy path}
[ -f "$result" ] || fail "no result file at $result"
cp "$result" "$copy" || fail "cannot copy the result to $copy"
field() { sed -n "s/^$1: *//p" "$copy" | head -1; }
commit=$(field commit)
remote_head=$(field dolores)
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "the result names no commit"
[[ $remote_head =~ ^[0-9a-f]{40}$ ]] || fail "the result names no dolores commit"

# 1. On GitHub.
cd "$loadngo" || fail "no $loadngo"
git fetch -q github dev || fail "cannot fetch github dev"
dev=$(git rev-parse github/dev) || fail "no github/dev"
git merge-base --is-ancestor "$commit" "$dev" || fail "$commit is not on github/dev"
added=$(git diff --name-status --diff-filter=A "$commit^" "$commit")
for path in data/src/repository.rs data/src/bin/loadngo-repo.rs; do
    grep -Eq "^A[[:space:]]+$path$" <<<"$added" || fail "$commit does not add $path"
done
pass "1: $commit is on github/dev and adds data/src/repository.rs and data/src/bin/loadngo-repo.rs"

# 2. The gates.
git merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git diff --quiet HEAD -- data || fail "data/ has uncommitted changes"
cargo fmt --all -- --check >/dev/null || fail "cargo fmt --check"
cargo clippy -q -p data --all-targets --all-features -- -D warnings || fail "clippy"
cargo test -q -p data >/dev/null 2>&1 || fail "data tests"
pass "2: fmt, clippy and the data tests pass at $(git rev-parse --short HEAD)"

# 3. CI on the four platforms.
ci=
while read -r id sha conclusion; do
    [ "$conclusion" = success ] || continue
    if git merge-base --is-ancestor "$commit" "$sha" 2>/dev/null; then
        ci=$id
        break
    fi
done < <(gh run list --repo jaylauffer/loadngo --branch dev --workflow CI --limit 30 \
    --json databaseId,headSha,conclusion --jq '.[] | "\(.databaseId) \(.headSha) \(.conclusion)"')
[ -n "$ci" ] || fail "no successful CI run on dev contains $commit"
n=$(gh run view "$ci" --repo jaylauffer/loadngo --json jobs --jq '[.jobs[] | select(.conclusion == "success")] | length')
[ "$n" -ge 4 ] || fail "CI run $ci has $n successful jobs, not 4"
pass "3: CI run $ci passed ($n jobs) and contains $commit"

# The tools, built from HEAD.
cargo build -q --release -p data --bin loadngo-repo --bin archive_cas_gc || fail "cannot build loadngo-repo and archive_cas_gc"
cargo build -q --release -p loadngo-pq-auth --bin loadngo_pq_auth || fail "cannot build loadngo_pq_auth"
repo=$loadngo/target/release/loadngo-repo
s=$(mktemp -d)
rdir=
cleanup() {
    if [ -n "$rdir" ]; then on "rm -rf ~/$rdir" || echo "verify: could not remove ~/$rdir on dolores" >&2; fi
}
trap cleanup EXIT
echo "verify: scratch $s"

# 4. Import.
"$repo" import --store "$s/a" --repository loadngo --branch dev --from-git "$loadngo" --git-ref "$dev" \
    >"$s/import-a.log" 2>&1 || fail "import: $(tail -3 "$s/import-a.log")"
"$repo" log --store "$s/a" --repository loadngo --branch dev >"$s/log-a.tsv" 2>"$s/log-a.err" ||
    fail "log: $(tail -3 "$s/log-a.err")"
count=$(git rev-list --count "$dev")
listed=$(wc -l <"$s/log-a.tsv" | tr -d ' ')
[ "$listed" = "$count" ] || fail "log lists $listed versions; git has $count commits"
cut -f2 "$s/log-a.tsv" >"$s/log-a.git"
git rev-list --first-parent "$dev" >"$s/git.order"
cmp -s "$s/log-a.git" "$s/git.order" || fail "log does not list git's commits in git's order"
# Line i's parent (column 3) is line i+1's version (column 1); the first commit has none.
awk -F'\t' 'NR > 1 && prev != $1 { bad = 1 } { prev = $3 } END { exit (bad || prev != "-") }' "$s/log-a.tsv" ||
    fail "a version's parent is not the version of its git parent"
head_version=$(head -1 "$s/log-a.tsv" | cut -f1)
[[ $head_version =~ ^[0-9a-f]{64}$ ]] || fail "log names no head version: $head_version"
"$repo" import --store "$s/c" --repository loadngo --branch dev --from-git "$loadngo" --git-ref "$dev~7" \
    >"$s/import-c1.log" 2>&1 || fail "import of dev~7: $(tail -3 "$s/import-c1.log")"
"$repo" import --store "$s/c" --repository loadngo --branch dev --from-git "$loadngo" --git-ref "$dev" \
    >"$s/import-c2.log" 2>&1 || fail "import of dev after dev~7: $(tail -3 "$s/import-c2.log")"
"$repo" log --store "$s/c" --repository loadngo --branch dev >"$s/log-c.tsv" 2>&1 || fail "log of the second store"
cmp -s "$s/log-a.tsv" "$s/log-c.tsv" || fail "importing dev~7 and then dev does not list the same versions as importing dev"
pass "4: $count versions in git's order with git's parents; dev~7 then dev lists the same versions, head $head_version"
du -sh "$s/a" | sed 's/^/verify: store size: /'

# 5. Every version checks out to git's tree.
snapshot() { # directory, output prefix: executable files and symlinks with their targets
    (cd "$1" && find . -type f -perm -u+x | LC_ALL=C sort) >"$2.exec"
    (cd "$1" && find . -type l | LC_ALL=C sort | while IFS= read -r link; do
        printf '%s -> %s\n' "$link" "$(readlink "$link")"
    done) >"$2.links"
}
checked=0
while IFS=$'\t' read -r version gitcommit _; do
    rm -rf "$s/co" "$s/ga"
    mkdir "$s/ga"
    "$repo" checkout --store "$s/a" --repository loadngo --version "$version" --destination "$s/co" \
        >/dev/null 2>"$s/co.err" || fail "checkout of $version (git $gitcommit): $(tail -3 "$s/co.err")"
    git archive "$gitcommit" | tar -x -C "$s/ga" || fail "git archive $gitcommit"
    diff -r --no-dereference -x .loadngo-repo "$s/ga" "$s/co" >"$s/diff.txt" 2>&1 ||
        fail "version $version differs from git $gitcommit: $(head -5 "$s/diff.txt")"
    snapshot "$s/ga" "$s/ga"
    snapshot "$s/co" "$s/co"
    cmp -s "$s/ga.exec" "$s/co.exec" || fail "version $version: executable files differ from git $gitcommit"
    cmp -s "$s/ga.links" "$s/co.links" || fail "version $version: symlinks differ from git $gitcommit"
    checked=$((checked + 1))
done <"$s/log-a.tsv"
[ "$checked" = "$count" ] || fail "checked $checked of $count versions"
rm -rf "$s/co" "$s/ga"
pass "5: all $checked versions check out to exactly git's tree (bytes, executable files, symlinks)"

# 6. Signatures.
"$repo" verify --store "$s/a" --repository loadngo --branch dev >"$s/verify-a.log" 2>&1 ||
    fail "verify: $(tail -3 "$s/verify-a.log")"
"$loadngo/target/release/loadngo_pq_auth" keygen --public-key "$s/other.pub" --private-key "$s/other.key" \
    >/dev/null || fail "keygen"
refused() { # name, store, extra flags
    local name=$1 store=$2
    shift 2
    if "$repo" verify --store "$store" --repository loadngo --branch dev "$@" >"$s/$name-verify.log" 2>&1; then
        fail "verify passed with $name"
    fi
    if "$repo" checkout --store "$store" --repository loadngo --branch dev --destination "$s/$name-co" "$@" \
        >"$s/$name-checkout.log" 2>&1; then
        fail "checkout passed with $name"
    fi
    [ ! -e "$s/$name-co" ] || fail "the refused checkout with $name left $s/$name-co"
}
cp -R "$s/a" "$s/altered-object"
git show "$dev:.gitignore" >"$s/gitignore.bytes" || fail "git show .gitignore"
size=$(wc -c <"$s/gitignore.bytes" | tr -d ' ')
object=
while IFS= read -r candidate; do
    if cmp -s "$candidate" "$s/gitignore.bytes"; then
        object=$candidate
        break
    fi
done < <(find "$s/altered-object/objects" -type f -size "${size}c")
[ -n "$object" ] || fail "cannot find the stored object of .gitignore"
chmod u+w "$object" && printf 'X' | dd of="$object" bs=1 seek=0 conv=notrunc 2>/dev/null || fail "cannot alter $object"
refused altered-object "$s/altered-object"
cp -R "$s/a" "$s/edited-head"
parent=$(sed -n 2p "$s/log-a.tsv" | cut -f1)
head_file=$s/edited-head/repositories/loadngo/heads/dev.json
grep -q "$head_version" "$head_file" || fail "$head_file does not name the head version"
sed -i '' "s/$head_version/$parent/g" "$head_file" || fail "cannot edit $head_file"
refused edited-head "$s/edited-head"
refused other-key "$s/a" --trusted-public-key "$s/other.pub"
pass "6: verify passes; verify and checkout refuse an altered object, an edited head and another key, leaving nothing"

# 7. Commit from a working tree.
"$repo" checkout --store "$s/a" --repository loadngo --branch dev --destination "$s/work" >/dev/null 2>"$s/work.err" ||
    fail "checkout of the head: $(tail -3 "$s/work.err")"
(
    cd "$s/work" || exit 1
    printf 'one more line\n' >>docs/ARCHIVE_CAS.md &&
        mkdir new-folder && printf 'new\n' >new-folder/new-file.txt &&
        rm docs/CAS_DRIVE_CLEANUP.md &&
        chmod +x README.md &&
        mkdir -p target && printf 'ignored\n' >target/ignored.txt &&
        printf 'not named\n' >>LICENSE
) || fail "cannot make the working-tree changes"
printf 'changed\tLICENSE\nchanged\tREADME.md\nchanged\tdocs/ARCHIVE_CAS.md\ncreated\tnew-folder/new-file.txt\ndeleted\tdocs/CAS_DRIVE_CLEANUP.md\n' |
    LC_ALL=C sort >"$s/status.expected"
"$repo" status --tree "$s/work" >"$s/status.raw" 2>"$s/status.err" || fail "status: $(tail -3 "$s/status.err")"
LC_ALL=C sort "$s/status.raw" >"$s/status.before"
cmp -s "$s/status.before" "$s/status.expected" || fail "status before the commit: $(cat "$s/status.before")"
"$repo" commit --tree "$s/work" --path docs/ARCHIVE_CAS.md --path new-folder/new-file.txt \
    --path docs/CAS_DRIVE_CLEANUP.md --path README.md --message "verify: four named changes" \
    --author "task check" >"$s/commit.log" 2>&1 || fail "commit: $(tail -3 "$s/commit.log")"
"$repo" log --store "$s/a" --repository loadngo --branch dev >"$s/log-after.tsv" 2>&1 || fail "log after the commit"
IFS=$'\t' read -r new_head new_git new_parent _ new_author _ < <(head -1 "$s/log-after.tsv")
[ "$new_parent" = "$head_version" ] || fail "the new version's parent is $new_parent, not the old head"
[ "$new_git" = "-" ] || fail "the new version names a git commit: $new_git"
[ "$new_author" = "task check" ] || fail "the new version's author is $new_author"
[ "$new_head" != "$head_version" ] || fail "the head did not move"
"$repo" status --tree "$s/work" >"$s/status.after" 2>&1 || fail "status after the commit"
[ "$(cat "$s/status.after")" = "$(printf 'changed\tLICENSE')" ] || fail "status after the commit: $(cat "$s/status.after")"
"$repo" verify --store "$s/a" --repository loadngo --branch dev >"$s/verify-after.log" 2>&1 ||
    fail "verify after the commit: $(tail -3 "$s/verify-after.log")"
"$repo" checkout --store "$s/a" --repository loadngo --branch dev --destination "$s/fresh" >/dev/null 2>&1 ||
    fail "checkout of the new head"
diff -rq --no-dereference -x .loadngo-repo -x target "$s/work" "$s/fresh" >"$s/fresh.diff" 2>&1
[ "$(cat "$s/fresh.diff")" = "Files $s/work/LICENSE and $s/fresh/LICENSE differ" ] ||
    fail "the fresh checkout of the new head: $(head -5 "$s/fresh.diff")"
[ -x "$s/fresh/README.md" ] || fail "README.md is not executable in the new head"
pass "7: status lists the five changes, not the ignored file; commit of four makes $new_head on $head_version"

# 8. archive_cas_gc keeps the repository.
"$loadngo/target/release/archive_cas_gc" --cas-root "$s/a" --execute >"$s/gc.log" 2>&1 ||
    fail "archive_cas_gc: $(tail -3 "$s/gc.log")"
"$repo" verify --store "$s/a" --repository loadngo --branch dev >"$s/verify-gc.log" 2>&1 ||
    fail "verify after archive_cas_gc: $(tail -3 "$s/verify-gc.log")"
pass "8: verify passes after archive_cas_gc --execute ($(grep -m1 -i 'orphaned' "$s/gc.log"))"

# 9. Send, on this Mac and to dolores.
"$repo" send --store "$s/a" --repository loadngo --branch dev --to "$s/b" >"$s/send-b.log" 2>&1 ||
    fail "send --to: $(tail -3 "$s/send-b.log")"
"$repo" verify --store "$s/b" --repository loadngo --branch dev >"$s/verify-b.log" 2>&1 ||
    fail "verify of the sent store: $(tail -3 "$s/verify-b.log")"
on true || fail "cannot reach dolores"
[ "$(on 'git -C ~/pudding/loadngo rev-parse HEAD')" = "$remote_head" ] || fail "dolores's ~/pudding/loadngo is not at $remote_head"
on "git -C ~/pudding/loadngo merge-base --is-ancestor $commit HEAD" || fail "dolores's checkout does not contain $commit"
on 'git -C ~/pudding/loadngo diff --quiet HEAD -- data' || fail "dolores's data/ has uncommitted changes"
on 'cd ~/pudding/loadngo && ~/.cargo/bin/cargo build -q --release -p data --bin loadngo-repo' \
    >"$s/dolores-tool.log" 2>&1 || fail "cannot build loadngo-repo on dolores: $(tail -3 "$s/dolores-tool.log")"
rdir=loadngo-repo-check-$(date +%s)
on "mkdir ~/$rdir" || fail "cannot make ~/$rdir on dolores"
scp -q -o BatchMode=yes "$HOME/.loadngo/keys/jay-macmini.dilithium2.pub" "$dolores:$rdir/" ||
    fail "cannot copy the public key to dolores"
"$repo" send --store "$s/c" --repository loadngo --branch dev --to-ssh "$dolores" --remote-store "$rdir/store" \
    --remote-program pudding/loadngo/target/release/loadngo-repo \
    --remote-trusted-public-key "$rdir/jay-macmini.dilithium2.pub" >"$s/send-dolores.log" 2>&1 ||
    fail "send --to-ssh: $(tail -3 "$s/send-dolores.log")"
rtool='~/pudding/loadngo/target/release/loadngo-repo'
on "cd ~/$rdir && $rtool verify --store store --repository loadngo --branch dev --trusted-public-key jay-macmini.dilithium2.pub" \
    >"$s/dolores-verify.log" 2>&1 || fail "verify on dolores: $(tail -3 "$s/dolores-verify.log")"
on "cd ~/$rdir && $rtool checkout --store store --repository loadngo --branch dev --destination tree --trusted-public-key jay-macmini.dilithium2.pub >/dev/null &&
    git -C ~/pudding/loadngo fetch -q github dev && mkdir git && git -C ~/pudding/loadngo archive $dev | tar -x -C git &&
    diff -r --no-dereference -x .loadngo-repo git tree &&
    (cd git && find . -type f -perm -u+x | LC_ALL=C sort) >git.exec && (cd tree && find . -type f -perm -u+x | LC_ALL=C sort) >tree.exec &&
    cmp git.exec tree.exec && [ ! -e tree/.git ]" >"$s/dolores-checkout.log" 2>&1 ||
    fail "the checkout on dolores is not git's tree: $(tail -5 "$s/dolores-checkout.log")"
on "cd ~/$rdir/tree && CARGO_TARGET_DIR=../target ~/.cargo/bin/cargo build -q --locked --release -p data --bin loadngo-repo &&
    ../target/release/loadngo-repo --help | head -1" >"$s/dolores-build.log" 2>&1 ||
    fail "cannot build loadngo-repo from the checked-out tree on dolores: $(tail -5 "$s/dolores-build.log")"
grep -q '^loadngo-repo' "$s/dolores-build.log" || fail "the tool built from the checked-out tree did not run: $(tail -3 "$s/dolores-build.log")"
pass "9: sent to a store on this Mac and to dolores; dolores verified it, checked out git's tree and built loadngo-repo from it"
rm -rf "$s"
echo "verify: accepted"
