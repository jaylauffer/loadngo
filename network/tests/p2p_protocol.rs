use data::{
    cas::{CasHash, CasStorage},
    p2pmsg::{FileData, FileEnd, FileStart, Message, RequestContent},
};
use network::{
    p2p::{Event, Protocol},
    NetworkCore,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tempfile::tempdir;

#[test]
fn request_content_emits_file_transfer_frames_for_exact_chunk_multiple() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let payload = b"abcdefgh";
    let (hash, _) = store.add_content(payload).unwrap();

    let requester = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7000);
    let mut protocol = Protocol::new();
    protocol.set_next_transfer_time(100);
    let mut core = NetworkCore::with_chunk_size(4);

    let result = protocol
        .handle_message(
            &mut core,
            requester,
            Message::RequestContent(RequestContent::Request { hashes: vec![hash] }),
            &store,
        )
        .unwrap();

    assert!(result.events.is_empty());
    assert_eq!(result.outbound.len(), 5);
    match &result.outbound[0] {
        Message::TransferFileStart(FileStart {
            time,
            filesize,
            hash: outbound_hash,
        }) => {
            assert_eq!(
                (*time, *filesize, *outbound_hash),
                (100, payload.len() as u64, hash)
            );
        }
        other => panic!("expected file start, got {other:?}"),
    }
    match &result.outbound[1] {
        Message::TransferFileData(FileData { time, seq, data }) => {
            assert_eq!((*time, *seq), (100, 0));
            assert_eq!(data, b"abcd");
        }
        other => panic!("expected first data frame, got {other:?}"),
    }
    match &result.outbound[2] {
        Message::TransferFileData(FileData { time, seq, data }) => {
            assert_eq!((*time, *seq), (100, 1));
            assert_eq!(data, b"efgh");
        }
        other => panic!("expected second data frame, got {other:?}"),
    }
    match &result.outbound[3] {
        Message::TransferFileData(FileData { time, seq, data }) => {
            assert_eq!((*time, *seq), (100, 2));
            assert!(data.is_empty());
        }
        other => panic!("expected trailing empty data frame, got {other:?}"),
    }
    match &result.outbound[4] {
        Message::TransferFileEnd(FileEnd { time }) => assert_eq!(*time, 100),
        other => panic!("expected file end, got {other:?}"),
    }
}

#[test]
fn request_content_round_trip_stores_payload_in_receiver_cas() {
    let sender_dir = tempdir().unwrap();
    let sender_store = CasStorage::new(sender_dir.path()).unwrap();
    let receiver_dir = tempdir().unwrap();
    let receiver_store = CasStorage::new(receiver_dir.path()).unwrap();
    let payload = b"abcdefghij";
    let hash = CasHash::digest(payload);
    sender_store.add_content(payload).unwrap();

    let sender = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7001);
    let receiver = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7002);
    let mut sender_protocol = Protocol::new();
    sender_protocol.set_next_transfer_time(200);
    let mut sender_core = NetworkCore::with_chunk_size(4);
    let mut receiver_protocol = Protocol::new();
    let mut receiver_core = NetworkCore::with_chunk_size(4);

    let sender_result = sender_protocol
        .handle_message(
            &mut sender_core,
            receiver,
            Message::RequestContent(RequestContent::Request { hashes: vec![hash] }),
            &sender_store,
        )
        .unwrap();

    let mut observed = Vec::new();
    for frame in sender_result.outbound {
        let receiver_result = receiver_protocol
            .handle_message(&mut receiver_core, sender, frame, &receiver_store)
            .unwrap();
        observed.extend(receiver_result.events);
    }

    assert_eq!(observed, vec![Event::ContentStored(hash)]);
    assert_eq!(receiver_store.verified_read_all(hash).unwrap(), payload);
}

#[test]
fn transfer_file_end_before_all_chunks_keeps_receiver_state_until_retry() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let payload = b"abcdefghij";
    let hash = CasHash::digest(payload);
    let sender = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7003);
    let mut protocol = Protocol::new();
    let mut core = NetworkCore::with_chunk_size(4);

    protocol
        .handle_message(
            &mut core,
            sender,
            Message::TransferFileStart(FileStart {
                time: 300,
                filesize: payload.len() as u64,
                hash,
            }),
            &store,
        )
        .unwrap();
    protocol
        .handle_message(
            &mut core,
            sender,
            Message::TransferFileData(FileData {
                time: 300,
                seq: 0,
                data: payload[0..4].to_vec(),
            }),
            &store,
        )
        .unwrap();

    let early_end = protocol
        .handle_message(
            &mut core,
            sender,
            Message::TransferFileEnd(FileEnd { time: 300 }),
            &store,
        )
        .unwrap();
    assert!(early_end.events.is_empty());
    assert!(core.has_content_transfer(sender, 300));

    protocol
        .handle_message(
            &mut core,
            sender,
            Message::TransferFileData(FileData {
                time: 300,
                seq: 1,
                data: payload[4..8].to_vec(),
            }),
            &store,
        )
        .unwrap();
    protocol
        .handle_message(
            &mut core,
            sender,
            Message::TransferFileData(FileData {
                time: 300,
                seq: 2,
                data: payload[8..10].to_vec(),
            }),
            &store,
        )
        .unwrap();

    let final_end = protocol
        .handle_message(
            &mut core,
            sender,
            Message::TransferFileEnd(FileEnd { time: 300 }),
            &store,
        )
        .unwrap();
    assert_eq!(final_end.events, vec![Event::ContentStored(hash)]);
    assert!(!core.has_content_transfer(sender, 300));
    assert_eq!(store.verified_read_all(hash).unwrap(), payload);
}
