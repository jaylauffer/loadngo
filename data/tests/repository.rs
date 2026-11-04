//! `data::repository`: versions, checkouts, commits, signatures, transfer and import.

use data::archive_cas::ArchiveChange;
use data::archive_cas_purge::{plan_purge, Sweep};
use data::cas::CasHash;
use data::repository::transfer::{list_heads, receive};
use data::repository::work_tree::{self, Change, CheckoutMarker, Target};
use data::repository::{live_objects, Repository, SignedHead, Signer, Trust};
use loadngo_pq_crypto::{default_registry, PqSchemeRegistry, SignatureSchemeId};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn signer(identity: &str) -> Signer {
    let registry = default_registry();
    let (public, private) = registry
        .get(&SignatureSchemeId::Dilithium2)
        .unwrap()
        .keygen()
        .unwrap();
    Signer {
        identity: identity.to_string(),
        public,
        private,
    }
}

fn trust(signer: &Signer) -> Trust {
    Trust::new(vec![signer.public.clone()])
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A tree with nested folders, an ignored folder, a build cache and a nested `.git`.
fn sample_tree(root: &Path) {
    write(&root.join("README.md"), "readme\n");
    write(&root.join("src/main.rs"), "fn main() {}\n");
    write(&root.join("src/deep/a.txt"), "a\n");
    write(&root.join(".gitignore"), "target\n*.log\n");
    write(&root.join("target/debug/out"), "build output\n");
    write(&root.join("notes.log"), "ignored\n");
    write(
        &root.join("cache/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    );
    write(&root.join("cache/blob"), "cached\n");
    write(&root.join(".git/config"), "[core]\n");
}

fn changes(status: &work_tree::Status) -> Vec<String> {
    status
        .changes
        .iter()
        .map(|change| format!("{} {}", change.kind(), change.path()))
        .collect()
}

#[test]
fn init_status_commit_and_checkout_round_trip() {
    let dir = tempdir().unwrap();
    let key = signer("test-key");
    let trusted = trust(&key);
    let repository = Repository::open(&dir.path().join("store"), "sample", true).unwrap();
    let work = dir.path().join("work");
    sample_tree(&work);

    let first = work_tree::init(
        &repository,
        "main",
        &work,
        "first",
        "tester",
        &key,
        &trusted,
    )
    .unwrap();
    let recorded: Vec<&str> = first.changes.iter().map(Change::path).collect();
    assert_eq!(
        recorded,
        [".gitignore", "README.md", "src/deep/a.txt", "src/main.rs"]
    );
    assert!(work_tree::status(&work).unwrap().changes.is_empty());

    // Change, create, delete, and (on unix) make one executable; one change is not named.
    write(&work.join("README.md"), "readme, changed\n");
    write(&work.join("src/new/b.txt"), "b\n");
    fs::remove_file(work.join("src/deep/a.txt")).unwrap();
    write(&work.join("src/main.rs"), "fn main() { println!(); }\n");
    write(&work.join("more.log"), "still ignored\n");
    let status = work_tree::status(&work).unwrap();
    assert_eq!(
        changes(&status),
        [
            "changed README.md",
            "deleted src/deep/a.txt",
            "changed src/main.rs",
            "created src/new/b.txt",
        ]
    );
    let second = work_tree::commit(
        &work,
        &["README.md".into(), "src/new".into(), "src/deep".into()],
        "second",
        "tester",
        &key,
        &trusted,
    )
    .unwrap();
    assert_eq!(second.parent, Some(first.version));
    assert_eq!(
        changes(&work_tree::status(&work).unwrap()),
        ["changed src/main.rs"]
    );
    assert_eq!(CheckoutMarker::read(&work).unwrap().version, second.version);

    let version = repository.version(second.version).unwrap();
    assert_eq!(
        version.changes,
        [
            ArchiveChange::Created {
                paths: vec!["src/new".into()]
            },
            ArchiveChange::Changed {
                paths: vec!["README.md".into()]
            },
            ArchiveChange::Deleted {
                paths: vec!["src/deep".into()]
            },
        ]
    );

    let copy = dir.path().join("copy");
    let report =
        work_tree::checkout(&repository, &Target::Branch("main".into()), &trusted, &copy).unwrap();
    assert_eq!(report.version, second.version);
    assert_eq!(
        fs::read_to_string(copy.join("README.md")).unwrap(),
        "readme, changed\n"
    );
    assert_eq!(
        fs::read_to_string(copy.join("src/new/b.txt")).unwrap(),
        "b\n"
    );
    assert_eq!(
        fs::read_to_string(copy.join("src/main.rs")).unwrap(),
        "fn main() {}\n"
    );
    assert!(!copy.join("src/deep").exists());
    assert!(
        !copy.join("target").exists()
            && !copy.join("cache").exists()
            && !copy.join(".git").exists()
    );
    assert!(work_tree::status(&copy).unwrap().changes.is_empty());

    let report = repository.verify("main", &trusted).unwrap();
    assert_eq!((report.versions, report.head), (2, second.version));

    // An old version checks out by hash; a commit to it is refused (no branch).
    let old = dir.path().join("old");
    work_tree::checkout(&repository, &Target::Version(first.version), &trusted, &old).unwrap();
    assert!(old.join("src/deep/a.txt").exists());
    write(&old.join("README.md"), "x\n");
    assert!(work_tree::commit(&old, &["README.md".into()], "m", "a", &key, &trusted).is_err());

    // A tree based on an older head cannot commit over a newer one.
    write(&copy.join("README.md"), "from the copy\n");
    work_tree::commit(
        &copy,
        &["README.md".into()],
        "third",
        "tester",
        &key,
        &trusted,
    )
    .unwrap();
    write(&work.join("src/main.rs"), "fn main() { 1; }\n");
    let error = work_tree::commit(
        &work,
        &["src/main.rs".into()],
        "late",
        "tester",
        &key,
        &trusted,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("moved since"), "{error}");
}

#[cfg(unix)]
#[test]
fn executable_bits_and_symlinks_are_kept() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let key = signer("test-key");
    let trusted = trust(&key);
    let repository = Repository::open(&dir.path().join("store"), "sample", true).unwrap();
    let work = dir.path().join("work");
    write(&work.join("run.sh"), "#!/bin/sh\n");
    fs::set_permissions(work.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    write(&work.join("data.txt"), "d\n");
    std::os::unix::fs::symlink("data.txt", work.join("link")).unwrap();
    work_tree::init(
        &repository,
        "main",
        &work,
        "first",
        "tester",
        &key,
        &trusted,
    )
    .unwrap();

    fs::set_permissions(work.join("data.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        changes(&work_tree::status(&work).unwrap()),
        ["changed data.txt"]
    );
    work_tree::commit(
        &work,
        &["data.txt".into()],
        "exec",
        "tester",
        &key,
        &trusted,
    )
    .unwrap();

    let copy = dir.path().join("copy");
    work_tree::checkout(&repository, &Target::Branch("main".into()), &trusted, &copy).unwrap();
    let mode = |name: &str| fs::metadata(copy.join(name)).unwrap().permissions().mode() & 0o111;
    assert_ne!(mode("run.sh"), 0);
    assert_ne!(mode("data.txt"), 0);
    assert_eq!(
        fs::read_link(copy.join("link")).unwrap(),
        Path::new("data.txt")
    );
    assert!(work_tree::status(&copy).unwrap().changes.is_empty());
}

#[test]
fn tampering_and_untrusted_keys_are_refused_leaving_nothing() {
    let dir = tempdir().unwrap();
    let key = signer("test-key");
    let trusted = trust(&key);
    let root = dir.path().join("store");
    let repository = Repository::open(&root, "sample", true).unwrap();
    let work = dir.path().join("work");
    write(&work.join("a.txt"), "the bytes of a\n");
    write(&work.join("b.txt"), "b\n");
    let first = work_tree::init(
        &repository,
        "main",
        &work,
        "first",
        "tester",
        &key,
        &trusted,
    )
    .unwrap();
    write(&work.join("b.txt"), "b2\n");
    work_tree::commit(&work, &["b.txt".into()], "second", "tester", &key, &trusted).unwrap();
    assert!(repository.verify("main", &trusted).is_ok());

    let refused = |trust: &Trust, name: &str| {
        let repository = Repository::open(&root, "sample", false).unwrap();
        assert!(
            repository.verify("main", trust).is_err(),
            "verify passed: {name}"
        );
        let destination = dir.path().join(name);
        assert!(
            work_tree::checkout(
                &repository,
                &Target::Branch("main".into()),
                trust,
                &destination
            )
            .is_err(),
            "checkout passed: {name}"
        );
        assert!(!destination.exists(), "{name} left a destination");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".checkout-"))
            .collect();
        assert!(leftovers.is_empty(), "{name} left a temporary folder");
    };

    // Another key.
    refused(&trust(&signer("other")), "other-key");

    // The head edited to name another version.
    let head_file = root.join("repositories/sample/heads/main.json");
    let original = fs::read_to_string(&head_file).unwrap();
    let head: SignedHead = serde_json::from_str(&original).unwrap();
    fs::write(
        &head_file,
        original.replace(&head.version.to_hex(), &first.version.to_hex()),
    )
    .unwrap();
    refused(&trusted, "edited-head");
    fs::write(&head_file, &original).unwrap();
    assert!(repository.verify("main", &trusted).is_ok());

    // One byte of a stored file changed.
    let object = CasHash::digest(b"the bytes of a\n");
    let path = repository.store().object_path(object);
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(&path, bytes).unwrap();
    refused(&trusted, "altered-object");
}

#[test]
fn a_stream_moves_a_branch_forward_only_and_checks_every_object() {
    let dir = tempdir().unwrap();
    let key = signer("test-key");
    let trusted = trust(&key);
    let source = Repository::open(&dir.path().join("a"), "sample", true).unwrap();
    let work = dir.path().join("work");
    write(&work.join("a.txt"), "a\n");
    let first = work_tree::init(&source, "main", &work, "first", "tester", &key, &trusted).unwrap();

    let stream_for = |known: &[CasHash]| {
        let (head, signed) = source.trusted_head("main", &trusted).unwrap();
        let plan = source.plan_send(head, known).unwrap();
        let mut stream = Vec::new();
        source.write_stream(&plan, &signed, &mut stream).unwrap();
        stream
    };
    let target = dir.path().join("b");
    let first_stream = stream_for(&[]);
    let report = receive(&target, &mut first_stream.as_slice(), &trusted).unwrap();
    assert!(report.moved);
    assert_eq!(
        list_heads(&target, "sample").unwrap(),
        format!("main\t{}\n", first.version)
    );

    write(&work.join("b.txt"), "b\n");
    let second =
        work_tree::commit(&work, &["b.txt".into()], "second", "tester", &key, &trusted).unwrap();
    let increment = stream_for(&[first.version]);
    let plan = source.plan_send(second.version, &[first.version]).unwrap();
    // One version, its root tree and the one new file: nothing the receiver holds.
    assert_eq!(
        (plan.versions.len(), plan.trees.len(), plan.files.len()),
        (1, 1, 1)
    );
    let report = receive(&target, &mut increment.as_slice(), &trusted).unwrap();
    assert_eq!((report.moved, report.head), (true, second.version));
    let received = Repository::open(&target, "sample", false).unwrap();
    assert_eq!(received.verify("main", &trusted).unwrap().versions, 2);

    // An older head is refused: the branch never moves back.
    let mut older = Vec::new();
    let plan = source.plan_send(first.version, &[]).unwrap();
    let old_head = SignedHead::sign("sample", "main", first.version, &key, 1).unwrap();
    source.write_stream(&plan, &old_head, &mut older).unwrap();
    assert!(receive(&target, &mut older.as_slice(), &trusted).is_err());

    // An untrusted signer is refused.
    assert!(receive(
        &dir.path().join("c"),
        &mut increment.clone().as_slice(),
        &trust(&signer("x"))
    )
    .is_err());

    // A changed byte inside an object is refused.
    let mut damaged = first_stream.clone();
    let at = damaged.len() / 2;
    damaged[at] ^= 1;
    assert!(receive(&dir.path().join("d"), &mut damaged.as_slice(), &trusted).is_err());
    assert!(Repository::open(&dir.path().join("d"), "sample", false)
        .map(|r| r.head("main").unwrap().is_none())
        .unwrap_or(true));
}

#[test]
fn store_upkeep_keeps_what_repositories_need() {
    let dir = tempdir().unwrap();
    let key = signer("test-key");
    let trusted = trust(&key);
    let root = dir.path().join("store");
    let repository = Repository::open(&root, "sample", true).unwrap();
    let work = dir.path().join("work");
    write(&work.join("a.txt"), "a\n");
    write(&work.join("sub/b.txt"), "b\n");
    work_tree::init(
        &repository,
        "main",
        &work,
        "first",
        "tester",
        &key,
        &trusted,
    )
    .unwrap();
    write(&work.join("a.txt"), "a2\n");
    work_tree::commit(&work, &["a.txt".into()], "second", "tester", &key, &trusted).unwrap();

    let live = live_objects(repository.store()).unwrap();
    let stored: Vec<CasHash> = repository
        .store()
        .list_objects()
        .unwrap()
        .into_iter()
        .map(|(hash, _)| hash)
        .collect();
    assert!(!stored.is_empty());
    assert!(stored.iter().all(|hash| live.contains(hash)));
    let plan = plan_purge(repository.store(), Sweep::Full, Some(&key.public), |_| {}).unwrap();
    assert!(
        plan.objects.is_empty(),
        "purge would delete {} objects",
        plan.objects.len()
    );
    assert!(repository.verify("main", &trusted).is_ok());
}

#[test]
fn invalid_names_are_refused() {
    use data::repository::{validate_name, Tree, TreeEntry};
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        ".git",
        ".GIT",
        ".loadngo-repo",
        "nul\0",
        "..\\..\\x",
        "C:x",
        ".git::$INDEX_ALLOCATION",
        "trailing.",
        "trailing ",
        "GIT~1",
        ".g\u{200c}it",
    ] {
        assert!(validate_name(bad).is_err(), "{bad:?}");
    }
    let object = data::archive_cas::ArchiveObject {
        hash: CasHash::digest(b"x"),
        size: 1,
    };
    let entry = |name: &str| TreeEntry::File {
        name: name.into(),
        object,
    };
    assert!(Tree::new(vec![entry("a"), entry("a")]).is_err());
    let tree = Tree::new(vec![entry("b"), entry("a")]).unwrap();
    assert_eq!(tree.entries[0].name(), "a");
}

#[cfg(unix)]
mod git {
    use super::*;
    use data::repository::import::import_git;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Tester")
            .env("GIT_AUTHOR_EMAIL", "tester@example.com")
            .env("GIT_COMMITTER_NAME", "Tester")
            .env("GIT_COMMITTER_EMAIL", "tester@example.com")
            .env("GIT_AUTHOR_DATE", "1793770000 +0700")
            .env("GIT_COMMITTER_DATE", "1793770000 +0700")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn sample_history(dir: &Path) -> Vec<String> {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        write(&dir.join("README.md"), "one\n");
        write(&dir.join("src/lib.rs"), "pub fn a() {}\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first"]);
        write(&dir.join("run.sh"), "#!/bin/sh\necho hi\n");
        fs::set_permissions(dir.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("README.md", dir.join("link")).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "exec and link"]);
        git(dir, &["mv", "src/lib.rs", "src/moved.rs"]);
        write(&dir.join("README.md"), "two\n");
        git(dir, &["commit", "-q", "-am", "move and change"]);
        git(dir, &["rm", "-q", "-r", "src"]);
        git(
            dir,
            &["commit", "-q", "-m", "delete a folder\n\nwith a body"],
        );
        git(dir, &["rev-list", "--first-parent", "main"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_send_after_a_merge_carries_both_sides() {
        let dir = tempdir().unwrap();
        let key = signer("test-key");
        let trusted = trust(&key);
        let source = dir.path().join("git");
        fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "-q", "-b", "main"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        write(&source.join("a.txt"), "a\n");
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-q", "-m", "A"]);
        git(&source, &["checkout", "-q", "-b", "side"]);
        write(&source.join("c.txt"), "c\n");
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-q", "-m", "C"]);
        git(&source, &["checkout", "-q", "main"]);
        write(&source.join("b.txt"), "b\n");
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-q", "-m", "B"]);
        git(&source, &["merge", "-q", "--no-ff", "-m", "M", "side"]);

        let repository = Repository::open(&dir.path().join("a"), "sample", true).unwrap();
        let send = |target: &Path, known: &[CasHash]| {
            let (head, signed) = repository.trusted_head("main", &trusted).unwrap();
            let mut stream = Vec::new();
            repository
                .write_stream(
                    &repository.plan_send(head, known).unwrap(),
                    &signed,
                    &mut stream,
                )
                .unwrap();
            receive(target, &mut stream.as_slice(), &trusted).unwrap()
        };
        let target = dir.path().join("b");
        let before = import_git(&repository, "main", &source, "main~1", &key, &trusted).unwrap();
        send(&target, &[]);
        let merged = import_git(&repository, "main", &source, "main", &key, &trusted).unwrap();
        assert_eq!(repository.version(merged.head).unwrap().parents.len(), 2);
        let report = send(&target, &[before.head]);
        assert_eq!(report.head, merged.head);
        let received = Repository::open(&target, "sample", false).unwrap();
        assert_eq!(received.verify("main", &trusted).unwrap().versions, 4);
    }

    #[test]
    fn a_git_history_imports_exactly_and_resumes() {
        let dir = tempdir().unwrap();
        let key = signer("test-key");
        let trusted = trust(&key);
        let source = dir.path().join("git");
        let commits = sample_history(&source);

        let whole = Repository::open(&dir.path().join("a"), "sample", true).unwrap();
        let report = import_git(&whole, "main", &source, "main", &key, &trusted).unwrap();
        assert_eq!(report.imported, 4);
        let history = whole.first_parent_history(report.head).unwrap();
        let origins: Vec<String> = history
            .iter()
            .map(|(_, v)| v.origin.as_ref().unwrap().commit.clone())
            .collect();
        assert_eq!(origins, commits);
        assert_eq!(history[0].1.message, "delete a folder\n\nwith a body\n");
        assert_eq!(history[0].1.subject(), "delete a folder");
        assert_eq!(history[0].1.author, "Tester <tester@example.com>");

        // Each version holds exactly what git's tree does.
        for (hash, version) in &history {
            let commit = &version.origin.as_ref().unwrap().commit;
            let leaves = whole.leaves(version.tree).unwrap();
            let listing = git(&source, &["ls-tree", "-r", commit]);
            assert_eq!(leaves.len(), listing.lines().count(), "{hash}");
            for line in listing.lines() {
                let (meta, path) = line.split_once('\t').unwrap();
                let mode = meta.split(' ').next().unwrap();
                let bytes = Command::new("git")
                    .arg("-C")
                    .arg(&source)
                    .args(["show", &format!("{commit}:{path}")])
                    .output()
                    .unwrap()
                    .stdout;
                match (&leaves[path], mode) {
                    (data::repository::Leaf::File(o), "100644")
                    | (data::repository::Leaf::Executable(o), "100755") => {
                        assert_eq!(o.hash, CasHash::digest(&bytes), "{commit}:{path}");
                    }
                    (data::repository::Leaf::Symlink(target), "120000") => {
                        let target_bytes = Command::new("git")
                            .arg("-C")
                            .arg(&source)
                            .args(["cat-file", "blob", &format!("{commit}:{path}")])
                            .output()
                            .unwrap()
                            .stdout;
                        assert_eq!(target.as_bytes(), target_bytes.as_slice());
                    }
                    (leaf, mode) => panic!("{commit}:{path} is {leaf:?} but git says {mode}"),
                }
            }
        }
        // The move was found.
        assert!(history[1].1.changes.iter().any(|change| matches!(
            change,
            ArchiveChange::Moved { moves } if moves.iter().any(|m| m.from == "src/lib.rs" && m.to == "src/moved.rs")
        )));

        // Resuming from an older commit gives the same versions.
        let resumed = Repository::open(&dir.path().join("b"), "sample", true).unwrap();
        let partial = import_git(&resumed, "main", &source, "main~2", &key, &trusted).unwrap();
        assert_eq!(partial.imported, 2);
        let rest = import_git(&resumed, "main", &source, "main", &key, &trusted).unwrap();
        assert_eq!((rest.imported, rest.head), (2, report.head));
        let again = import_git(&resumed, "main", &source, "main", &key, &trusted).unwrap();
        assert!(!again.moved);

        // The checkout of the head is git's tree.
        let copy = dir.path().join("copy");
        work_tree::checkout(&whole, &Target::Branch("main".into()), &trusted, &copy).unwrap();
        assert_eq!(fs::read_to_string(copy.join("README.md")).unwrap(), "two\n");
        assert_eq!(
            fs::read_link(copy.join("link")).unwrap(),
            Path::new("README.md")
        );
        assert!(!copy.join("src").exists());
        assert!(whole.verify("main", &trusted).is_ok());
    }
}

#[cfg(unix)]
#[test]
fn a_checkout_never_writes_through_a_symlink_the_tree_holds() {
    let dir = tempdir().unwrap();
    let key = signer("test-key");
    let trusted = trust(&key);
    let repository = Repository::open(&dir.path().join("store"), "sample", true).unwrap();
    let victim = dir.path().join("victim.txt");
    fs::write(&victim, "untouched\n").unwrap();
    let work = dir.path().join("work");
    write(&work.join("a.txt"), "a\n");
    // Names the old temporary-file scheme would have used, pointing outside the tree.
    for pid in [std::process::id(), std::process::id() + 1] {
        std::os::unix::fs::symlink(&victim, work.join(format!("..loadngo-repo.{pid}.partial")))
            .unwrap();
    }
    work_tree::init(
        &repository,
        "main",
        &work,
        "first",
        "tester",
        &key,
        &trusted,
    )
    .unwrap();
    let copy = dir.path().join("copy");
    work_tree::checkout(&repository, &Target::Branch("main".into()), &trusted, &copy).unwrap();
    write(&copy.join("a.txt"), "a2\n");
    work_tree::commit(&copy, &["a.txt".into()], "second", "tester", &key, &trusted).unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched\n");
    assert!(!fs::symlink_metadata(copy.join(".loadngo-repo"))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn a_key_the_trusted_keys_do_not_hold_never_signs_a_head() {
    let dir = tempdir().unwrap();
    let key = signer("test-key");
    let trusted = trust(&key);
    let stranger = signer("stranger");
    let repository = Repository::open(&dir.path().join("store"), "sample", true).unwrap();
    let work = dir.path().join("work");
    write(&work.join("a.txt"), "a\n");
    assert!(work_tree::init(
        &repository,
        "main",
        &work,
        "first",
        "tester",
        &stranger,
        &trusted
    )
    .is_err());
    assert!(repository.head("main").unwrap().is_none());
    assert!(!work.join(".loadngo-repo").exists());
    let first = work_tree::init(
        &repository,
        "main",
        &work,
        "first",
        "tester",
        &key,
        &trusted,
    )
    .unwrap();
    write(&work.join("a.txt"), "a2\n");
    assert!(
        work_tree::commit(&work, &["a.txt".into()], "m", "tester", &stranger, &trusted).is_err()
    );
    assert_eq!(
        repository.trusted_head("main", &trusted).unwrap().0,
        first.version
    );
    work_tree::commit(&work, &["a.txt".into()], "m", "tester", &key, &trusted).unwrap();
}

#[test]
fn store_upkeep_walks_each_repository_on_its_own() {
    // Both orders of names, so the folder listing's order cannot hide the case.
    for (first_id, second_id) in [("aa", "zz"), ("zz", "aa")] {
        let dir = tempdir().unwrap();
        let key = signer("test-key");
        let trusted = trust(&key);
        let root = dir.path().join("store");
        let first = Repository::open(&root, first_id, true).unwrap();
        let work = dir.path().join("first");
        write(&work.join("a.txt"), "a\n");
        write(&work.join("sub/b.txt"), "b\n");
        let report =
            work_tree::init(&first, "main", &work, "first", "tester", &key, &trusted).unwrap();
        // The other repository holds a file whose bytes are this one's version object.
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut first.store().open_object(report.version).unwrap(),
            &mut bytes,
        )
        .unwrap();
        let second = Repository::open(&root, second_id, true).unwrap();
        let other = dir.path().join("second");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("copy.json"), &bytes).unwrap();
        work_tree::init(&second, "main", &other, "first", "tester", &key, &trusted).unwrap();
        let live = live_objects(first.store()).unwrap();
        for (hash, _) in first.store().list_objects().unwrap() {
            assert!(
                live.contains(&hash),
                "{first_id} then {second_id}: {hash} would be deleted"
            );
        }
    }
}

#[test]
fn receive_replaces_a_damaged_copy_it_already_holds() {
    let dir = tempdir().unwrap();
    let key = signer("test-key");
    let trusted = trust(&key);
    let source = Repository::open(&dir.path().join("a"), "sample", true).unwrap();
    let work = dir.path().join("work");
    write(&work.join("a.txt"), "the bytes of a\n");
    work_tree::init(&source, "main", &work, "first", "tester", &key, &trusted).unwrap();
    let (head, signed) = source.trusted_head("main", &trusted).unwrap();
    let mut stream = Vec::new();
    source
        .write_stream(&source.plan_send(head, &[]).unwrap(), &signed, &mut stream)
        .unwrap();
    let target = dir.path().join("b");
    receive(&target, &mut stream.as_slice(), &trusted).unwrap();
    let received = Repository::open(&target, "sample", false).unwrap();
    let object = received
        .store()
        .object_path(CasHash::digest(b"the bytes of a\n"));
    let mut bytes = fs::read(&object).unwrap();
    bytes[0] ^= 1;
    fs::write(&object, bytes).unwrap();
    assert!(received.verify("main", &trusted).is_err());
    receive(&target, &mut stream.as_slice(), &trusted).unwrap();
    assert!(received.verify("main", &trusted).is_ok());
}
