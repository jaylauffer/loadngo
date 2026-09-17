use data::archive_cas::{
    ArchiveCasStorage, ArchiveEntry, ArchiveManifest, ArchiveObject, ARCHIVE_MANIFEST_FORMAT_V2,
};
use data::cas::CasHash;
use tempfile::tempdir;

#[test]
fn streaming_ingest_deduplicates_and_supports_range_reads() {
    let directory = tempdir().unwrap();
    let store = ArchiveCasStorage::with_buffer_size(directory.path().join("cas"), 4_093).unwrap();
    let source = directory.path().join("source.bin");
    let copy = directory.path().join("copy.bin");
    let content = (0..(3 * 1024 * 1024 + 17))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    std::fs::write(&source, &content).unwrap();
    std::fs::write(&copy, &content).unwrap();

    let first = store.ingest_file(&source, "source.bin").unwrap();
    assert!(first.inserted);
    assert_eq!(first.object.size, content.len() as u64);
    store.verify_object(first.object).unwrap();
    assert_eq!(
        store.read_range(first.object.hash, 1_000_000, 91).unwrap(),
        content[1_000_000..1_000_091]
    );

    let second = store.ingest_file(&copy, "copy.bin").unwrap();
    assert!(!second.inserted);
    assert_eq!(second.object, first.object);
}

#[test]
fn manifest_explicitly_marks_an_unreadable_source_entry_incomplete() {
    let manifest = ArchiveManifest::new(
        "example-archive",
        "Example read-only source",
        1_700_000_000,
        vec![ArchiveEntry::Unreadable {
            path: "lost-entry.ipa".to_string(),
            operation: "stat".to_string(),
            error: "No such file or directory (os error 2)".to_string(),
        }],
    )
    .unwrap();

    assert_eq!(manifest.format, ARCHIVE_MANIFEST_FORMAT_V2);
    assert!(!manifest.is_complete());
    assert_eq!(manifest.unreadable_entry_count(), 1);
    assert!(manifest
        .canonical_bytes()
        .unwrap()
        .windows(10)
        .any(|bytes| bytes == b"unreadable"));
}

#[test]
fn owner_approved_exclusion_supersedes_an_unreadable_manifest_root() {
    let unresolved = ArchiveManifest::new(
        "example-archive",
        "Example read-only source",
        1_700_000_000,
        vec![ArchiveEntry::Unreadable {
            path: "lost-entry.ipa".to_string(),
            operation: "stat".to_string(),
            error: "No such file or directory (os error 2)".to_string(),
        }],
    )
    .unwrap();

    let amended = unresolved
        .with_owner_approved_exclusion(
            "lost-entry.ipa",
            "owner-approved source exclusion",
            1_700_000_001,
        )
        .unwrap();

    assert!(amended.is_complete());
    assert_eq!(amended.excluded_entry_count(), 1);
    assert_eq!(
        amended.supersedes_archive_root,
        Some(unresolved.digest().unwrap())
    );
    assert!(matches!(
        amended.entries.as_slice(),
        [ArchiveEntry::Excluded { path, reason }]
            if path == "lost-entry.ipa" && reason == "owner-approved source exclusion"
    ));
}

#[test]
fn manifest_is_canonical_and_is_stored_as_a_cas_root() {
    let directory = tempdir().unwrap();
    let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
    let object = ArchiveObject {
        hash: CasHash::digest(b"object"),
        size: 6,
    };
    let manifest = ArchiveManifest::new(
        "example-archive",
        "Example read-only source",
        1_700_000_000,
        vec![
            ArchiveEntry::File {
                path: "z/file.bin".to_string(),
                object,
                modified_at_unix_secs: Some(1_699_999_999),
            },
            ArchiveEntry::Directory {
                path: "a".to_string(),
                modified_at_unix_secs: None,
            },
        ],
    )
    .unwrap();
    assert_eq!(manifest.entries[0].path(), "a");

    let (manifest_path, manifest_object) = store.write_manifest(&manifest).unwrap();
    assert_eq!(manifest_object.hash, manifest.digest().unwrap());
    store.verify_object(manifest_object).unwrap();
    assert_eq!(store.read_manifest(&manifest_path).unwrap(), manifest);

    assert!(ArchiveManifest::new(
        "example-archive",
        "source",
        1,
        vec![ArchiveEntry::Directory {
            path: "a/../b".to_string(),
            modified_at_unix_secs: None,
        }],
    )
    .is_err());
}
