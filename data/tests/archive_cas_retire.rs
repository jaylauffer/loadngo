//! A removal, signed, then purged: the old version keeps its record and still verifies.

use std::path::Path;
use std::process::Command;

use data::archive_cas::{ArchiveCasStorage, ArchiveEntry, ArchiveManifest};
use data::archive_cas_sign::sign_manifest_and_write;
use loadngo_pq_crypto::{default_registry, PqSchemeRegistry, SignatureSchemeId};

fn run(bin: &str, home: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(bin)
        .args(args)
        .env("HOME", home)
        .env_remove("USERPROFILE")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

#[test]
fn a_purged_version_keeps_its_manifest_and_signature_and_verifies_as_retired() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cas");
    let store = ArchiveCasStorage::new(&root).unwrap();
    let file = |path: &str, bytes: &[u8]| ArchiveEntry::File {
        path: path.into(),
        object: store.add_content(bytes).unwrap().object,
        modified_at_unix_secs: None,
    };
    let secret = file("secret.conf", b"PrivateKey = abc");
    let ArchiveEntry::File {
        object: secret_object,
        ..
    } = secret.clone()
    else {
        unreachable!()
    };
    let v1 =
        ArchiveManifest::new("docs", "Docs", 1, vec![secret, file("keep.txt", b"kept")]).unwrap();
    let (v1_path, _) = store.write_manifest(&v1).unwrap();
    let v2 = v1
        .with_entries_removed(&["secret.conf".to_string()], "leaked key", "jay", 2)
        .unwrap();
    let (v2_path, _) = store.write_manifest(&v2).unwrap();

    let (public, private) = default_registry()
        .get(&SignatureSchemeId::Dilithium2)
        .unwrap()
        .keygen()
        .unwrap();
    let key = dir.path().join("trusted.pub");
    std::fs::write(&key, hex::encode(public.to_bytes().unwrap())).unwrap();
    let (root_arg, key_arg) = (root.to_str().unwrap(), key.to_str().unwrap());
    let (v1_arg, v2_arg) = (v1_path.to_str().unwrap(), v2_path.to_str().unwrap());
    let purge = env!("CARGO_BIN_EXE_archive_cas_purge");
    let verify = env!("CARGO_BIN_EXE_archive_cas_verify");
    let home = dir.path();

    // Unsigned: nothing is retired.
    let (ok, out) = run(
        purge,
        home,
        &["--cas-root", root_arg, "--trusted-public-key", key_arg],
    );
    assert!(ok && out.contains("Not retired: docs"), "{out}");
    assert!(out.contains("Nothing to purge"), "{out}");

    sign_manifest_and_write(&store, &v2, "jay-test", &public, &private, 3).unwrap();
    let (ok, out) = run(
        purge,
        home,
        &["--cas-root", root_arg, "--trusted-public-key", key_arg],
    );
    assert!(ok && out.contains("manifest and signature kept"), "{out}");
    assert!(out.contains("removed file docs:secret.conf"), "{out}");
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("Purge plan "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .to_string();
    let (ok, out) = run(
        purge,
        home,
        &[
            "--cas-root",
            root_arg,
            "--trusted-public-key",
            key_arg,
            "--execute",
            &id,
        ],
    );
    assert!(ok && out.contains("Purged: 1 files (1 objects)"), "{out}");
    assert!(v1_path.exists(), "the retired version's manifest stays");
    assert!(!store.has_object(secret_object.hash));

    let (ok, out) = run(
        verify,
        home,
        &[
            "--cas-root",
            root_arg,
            "--manifest",
            v1_arg,
            "--trusted-public-key",
            key_arg,
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("Objects dropped on retirement: 1"), "{out}");
    assert!(out.contains("is signed by jay-test"), "{out}");
    let (ok, out) = run(
        verify,
        home,
        &["--cas-root", root_arg, "--manifest", v2_arg],
    );
    assert!(ok && out.contains("Blob verification: complete"), "{out}");
    // Without the trusted key the missing object is not excused.
    let (ok, out) = run(
        verify,
        home,
        &["--cas-root", root_arg, "--manifest", v1_arg],
    );
    assert!(
        !ok && out.contains("no later version is signed by the trusted key"),
        "{out}"
    );
}

#[test]
fn upgrade_moves_old_logs_into_a_v3_version_and_restore_gets_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cas");
    let store = ArchiveCasStorage::new(&root).unwrap();
    let object = store.add_content(b"kept").unwrap().object;
    let mut v2 = ArchiveManifest::new(
        "docs",
        "Docs",
        1,
        vec![ArchiveEntry::File {
            path: "keep.txt".into(),
            object,
            modified_at_unix_secs: None,
        }],
    )
    .unwrap();
    // A version written before v3, with a delete log beside it.
    v2.format = data::archive_cas::ARCHIVE_MANIFEST_FORMAT_V2.into();
    v2.supersedes_archive_root = Some(data::cas::CasHash::digest(b"long gone"));
    let (v2_path, _) = store.write_manifest(&v2).unwrap();
    let stem = v2_path.file_stem().unwrap().to_str().unwrap();
    let log = root
        .join("manifests")
        .join(format!("{stem}.delete-log.json"));
    let log_bytes = br#"{"format":"loadngo-archive-delete-log-v1","removed_paths":["x"]}"#;
    std::fs::write(&log, log_bytes).unwrap();

    let upgrade = env!("CARGO_BIN_EXE_archive_cas_upgrade");
    let verify = env!("CARGO_BIN_EXE_archive_cas_verify");
    let restore = env!("CARGO_BIN_EXE_archive_cas_restore");
    let home = dir.path();
    let (root_arg, log_arg) = (root.to_str().unwrap(), log.to_str().unwrap());
    let base = [
        "--cas-root",
        root_arg,
        "--archive",
        "docs",
        "--attach",
        log_arg,
    ];

    let (ok, out) = run(upgrade, home, &[&base[..], &["--dry-run"]].concat());
    assert!(ok && out.contains("Dry run: nothing written."), "{out}");
    assert!(log.exists());

    let (ok, out) = run(upgrade, home, &base);
    assert!(
        ok && out.contains("log files removed from manifests/: 1"),
        "{out}"
    );
    assert!(!log.exists(), "the log now lives in the store");
    let v3_path = out
        .lines()
        .find_map(|l| l.strip_prefix("New version: "))
        .unwrap()
        .to_string();
    let v3 = store.read_manifest(&v3_path).unwrap();
    assert_eq!(v3.format, data::archive_cas::ARCHIVE_MANIFEST_FORMAT_V3);
    assert_eq!(v3.entries, v2.entries);
    assert_eq!(v3.parents(), vec![v2.digest().unwrap()]);

    let (ok, out) = run(
        verify,
        home,
        &["--cas-root", root_arg, "--manifest", &v3_path],
    );
    assert!(ok && out.contains("Attachments verified"), "{out}");

    let back = dir.path().join("restored");
    let name = format!("{stem}.delete-log.json");
    let (ok, out) = run(
        restore,
        home,
        &[
            "--cas-root",
            root_arg,
            "--manifest",
            &v3_path,
            "--destination",
            back.to_str().unwrap(),
            "--path",
            &name,
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(std::fs::read(back.join(&name)).unwrap(), log_bytes);
}
