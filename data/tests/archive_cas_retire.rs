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
    let (v2, _) = v1
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
