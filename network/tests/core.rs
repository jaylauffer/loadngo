use data::{
    cas::{CasFlags, CasHash, CasStorage},
    model_utils::now_timestamp,
    Participant,
};
use network::{BlobFinish, ContentEnd, ContentFinish, NetworkCore};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tempfile::tempdir;

#[test]
fn participant_registry_tracks_latest_view() {
    let mut core = NetworkCore::new();
    let participant = Participant::new(11, 22, "10.0.0.5", now_timestamp());
    core.register_participant(participant.clone());

    let found = core.participant("10.0.0.5").unwrap();
    assert_eq!(found.user_id, participant.user_id);
    assert_eq!(found.machine_id, participant.machine_id);
}

#[test]
fn blob_finish_reports_missing_sequences() {
    let mut core = NetworkCore::with_chunk_size(4);
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444);
    core.start_blob(source, 55, 12);
    core.push_blob_data(source, 55, 0, b"abcd").unwrap();
    core.push_blob_data(source, 55, 2, b"ijkl").unwrap();

    match core.finish_blob(source, 55).unwrap() {
        BlobFinish::Missing(missing) => assert_eq!(missing, vec![1]),
        other => panic!("expected missing chunk report, got {other:?}"),
    }
}

#[test]
fn completed_blob_can_be_stored_in_cas() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let mut core = NetworkCore::with_chunk_size(4);
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5555);
    let payload = b"abcdefghijkl";

    core.start_blob(source, 77, payload.len());
    core.push_blob_data(source, 77, 0, &payload[0..4]).unwrap();
    core.push_blob_data(source, 77, 1, &payload[4..8]).unwrap();
    core.push_blob_data(source, 77, 2, &payload[8..12]).unwrap();

    let expected = CasHash::digest(payload);
    match core.finish_blob_into_cas(source, 77, &store).unwrap() {
        BlobFinish::Stored(hash) => {
            assert_eq!(hash, expected);
            assert_eq!(store.verified_read_all(hash).unwrap(), payload);
        }
        other => panic!("expected CAS-backed completion, got {other:?}"),
    }
}

#[test]
fn content_transfer_streams_into_cas_and_verifies() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let mut core = NetworkCore::with_chunk_size(4);
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6000);
    let payload = b"abcdefghijkl";
    let hash = CasHash::digest(payload);

    core.start_content_transfer(source, 88, hash, payload.len(), &store)
        .unwrap();
    core.push_content_data(source, 88, 0, &payload[0..4], &store)
        .unwrap();
    core.push_content_data(source, 88, 1, &payload[4..8], &store)
        .unwrap();
    core.push_content_data(source, 88, 2, &payload[8..12], &store)
        .unwrap();

    match core.finish_content_transfer(source, 88, &store).unwrap() {
        ContentFinish::Stored(stored) => {
            assert_eq!(stored, hash);
            assert_eq!(store.flags(hash).unwrap(), Some(CasFlags::NORMAL));
            assert_eq!(store.verified_read_all(hash).unwrap(), payload);
        }
        other => panic!("expected verified CAS content, got {other:?}"),
    }
}

#[test]
fn content_transfer_reports_missing_chunks_and_keeps_incomplete_entry() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let mut core = NetworkCore::with_chunk_size(4);
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6001);
    let payload = b"abcdefghijkl";
    let hash = CasHash::digest(payload);

    core.start_content_transfer(source, 89, hash, payload.len(), &store)
        .unwrap();
    core.push_content_data(source, 89, 0, &payload[0..4], &store)
        .unwrap();
    core.push_content_data(source, 89, 2, &payload[8..12], &store)
        .unwrap();

    match core.finish_content_transfer(source, 89, &store).unwrap() {
        ContentFinish::Missing(missing) => assert_eq!(missing, vec![1]),
        other => panic!("expected missing content chunk report, got {other:?}"),
    }
    assert_eq!(store.flags(hash).unwrap(), Some(CasFlags::INCOMPLETE));
}

#[test]
fn duplicate_content_chunks_are_idempotent_when_bytes_match() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let mut core = NetworkCore::with_chunk_size(4);
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6002);
    let payload = b"abcdefgh";
    let hash = CasHash::digest(payload);

    core.start_content_transfer(source, 90, hash, payload.len(), &store)
        .unwrap();
    core.push_content_data(source, 90, 0, &payload[0..4], &store)
        .unwrap();
    core.push_content_data(source, 90, 0, &payload[0..4], &store)
        .unwrap();
    core.push_content_data(source, 90, 1, &payload[4..8], &store)
        .unwrap();

    match core.finish_content_transfer(source, 90, &store).unwrap() {
        ContentFinish::Stored(stored) => {
            assert_eq!(stored, hash);
            assert_eq!(store.verified_read_all(stored).unwrap(), payload);
        }
        other => panic!("expected duplicate-tolerant completion, got {other:?}"),
    }
}

#[test]
fn content_transfer_marks_corrupt_payloads_in_cas() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let mut core = NetworkCore::with_chunk_size(4);
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6003);
    let payload = b"abcdefghijkl";
    let hash = CasHash::digest(payload);

    core.start_content_transfer(source, 91, hash, payload.len(), &store)
        .unwrap();
    core.push_content_data(source, 91, 0, &payload[0..4], &store)
        .unwrap();
    core.push_content_data(source, 91, 1, b"WXYZ", &store)
        .unwrap();
    core.push_content_data(source, 91, 2, &payload[8..12], &store)
        .unwrap();

    match core.finish_content_transfer(source, 91, &store).unwrap() {
        ContentFinish::Corrupt(corrupt) => assert_eq!(corrupt, hash),
        other => panic!("expected CAS corruption result, got {other:?}"),
    }

    let flags = store.flags(hash).unwrap().unwrap();
    assert!(flags.contains(CasFlags::INCOMPLETE));
    assert!(flags.contains(CasFlags::CORRUPT));
}

#[test]
fn incomplete_end_keeps_content_transfer_alive_for_late_chunks() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let mut core = NetworkCore::with_chunk_size(4);
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6004);
    let payload = b"abcdefgh";
    let hash = CasHash::digest(payload);

    core.start_content_transfer(source, 92, hash, payload.len(), &store)
        .unwrap();
    core.push_content_data(source, 92, 0, &payload[0..4], &store)
        .unwrap();

    match core.end_content_transfer(source, 92, &store).unwrap() {
        ContentEnd::Incomplete(missing) => assert_eq!(missing, vec![1]),
        other => panic!("expected incomplete transfer, got {other:?}"),
    }
    assert!(core.has_content_transfer(source, 92));

    core.push_content_data(source, 92, 1, &payload[4..8], &store)
        .unwrap();
    match core.end_content_transfer(source, 92, &store).unwrap() {
        ContentEnd::Stored(stored) => assert_eq!(stored, hash),
        other => panic!("expected completed transfer after retry, got {other:?}"),
    }
    assert!(!core.has_content_transfer(source, 92));
    assert_eq!(store.verified_read_all(hash).unwrap(), payload);
}
