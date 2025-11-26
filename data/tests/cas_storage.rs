use data::cas::{CasFlags, CasHash, CasStorage};
use tempfile::tempdir;

#[test]
fn add_content_round_trips_and_verifies() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();

    let payload = b"hello cas";
    let (hash, inserted) = store.add_content(payload).unwrap();
    assert!(inserted);
    assert!(store.exists(hash).unwrap());
    assert_eq!(store.get_size(hash).unwrap(), Some(payload.len() as u32));
    assert_eq!(store.read_all(hash).unwrap(), payload);
    assert_eq!(store.verified_read_all(hash).unwrap(), payload);

    let (same_hash, inserted_again) = store.add_content(payload).unwrap();
    assert_eq!(same_hash, hash);
    assert!(!inserted_again);
}

#[test]
fn incomplete_write_verify_clears_flags() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();

    let payload = b"chunked payload over udp";
    let hash = CasHash::digest(payload);
    assert!(store.add_empty(hash, payload.len() as u32).unwrap());
    assert_eq!(store.flags(hash).unwrap(), Some(CasFlags::INCOMPLETE));

    store.write_incomplete(hash, 0, &payload[..7]).unwrap();
    store.write_incomplete(hash, 7, &payload[7..]).unwrap();
    assert!(store.verify(hash).unwrap());
    assert_eq!(store.flags(hash).unwrap(), Some(CasFlags::NORMAL));
    assert_eq!(store.verified_read_all(hash).unwrap(), payload);
}

#[test]
fn remove_marks_deleted_but_list_all_keeps_hash() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();

    let (hash, _) = store.add_content(b"delete-me").unwrap();
    assert!(store.remove(hash).unwrap());
    assert!(!store.exists(hash).unwrap());
    assert_eq!(store.flags(hash).unwrap(), Some(CasFlags::DELETED));
    let listed = store.list_all_content().unwrap();
    assert!(listed.contains(&hash));
}
