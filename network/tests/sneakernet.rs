use data::{
    cas::{CasHash, CasStorage},
    p2pmsg::{FileStart, Message},
};
use network::{p2p, Network};
use std::{net::UdpSocket, time::Duration};
use tempfile::tempdir;

#[test]
fn sneakernet_dispatches_request_content_and_sends_file_frames() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let payload = b"abcdefghij";
    let hash = CasHash::digest(payload);
    store.add_content(payload).unwrap();

    let mut server = Network::new();
    server.init().unwrap();
    server.bind("127.0.0.1:0").unwrap();
    let server_addr = server.local_addr().unwrap();

    let requester = UdpSocket::bind("127.0.0.1:0").unwrap();
    requester
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    let request = p2p::request_content(&[hash]);
    requester.send_to(&request, server_addr).unwrap();

    let mut sneakernet = p2p::SneakerNet::with_chunk_size(4);
    sneakernet.protocol_mut().set_next_transfer_time(500);
    let dispatched = sneakernet
        .recv_and_dispatch(&server, &store)
        .unwrap()
        .unwrap();

    assert_eq!(dispatched.source, requester.local_addr().unwrap());
    assert!(dispatched.events.is_empty());
    assert_eq!(dispatched.outbound.len(), 5);
    match &dispatched.outbound[0] {
        Message::TransferFileStart(FileStart {
            time,
            filesize,
            hash: outbound_hash,
        }) => {
            assert_eq!(
                (*time, *filesize, *outbound_hash),
                (500, payload.len() as u64, hash)
            );
        }
        other => panic!("expected file start, got {other:?}"),
    }

    let mut received = Vec::new();
    for _ in 0..5 {
        let mut buf = [0u8; 512];
        let (len, _) = requester.recv_from(&mut buf).unwrap();
        let (_hdr, message) = p2p::parse_frame(&buf[..len]).unwrap();
        received.push(message);
    }

    assert!(matches!(
        received.first(),
        Some(Message::TransferFileStart(_))
    ));
    assert!(matches!(received.last(), Some(Message::TransferFileEnd(_))));
}

#[test]
fn sneakernet_dispatches_incoming_transfer_into_cas() {
    let dir = tempdir().unwrap();
    let store = CasStorage::new(dir.path()).unwrap();
    let payload = b"abcdefghij";
    let hash = CasHash::digest(payload);

    let mut receiver = Network::new();
    receiver.init().unwrap();
    receiver.bind("127.0.0.1:0").unwrap();
    let receiver_addr = receiver.local_addr().unwrap();

    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    sender
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    let mut sneakernet = p2p::SneakerNet::with_chunk_size(4);

    let frames = [
        p2p::file_start(600, payload.len() as u64, hash),
        p2p::file_data(600, 0, &payload[0..4], false),
        p2p::file_data(600, 1, &payload[4..8], false),
        p2p::file_data(600, 2, &payload[8..10], false),
        p2p::file_end(600, false),
    ];

    let mut last_events = Vec::new();
    for frame in frames {
        sender.send_to(&frame, receiver_addr).unwrap();
        let dispatched = sneakernet
            .recv_and_dispatch(&receiver, &store)
            .unwrap()
            .unwrap();
        last_events = dispatched.events;
    }

    assert_eq!(last_events, vec![p2p::Event::ContentStored(hash)]);
    assert_eq!(store.verified_read_all(hash).unwrap(), payload);
    assert!(!sneakernet
        .core()
        .has_content_transfer(sender.local_addr().unwrap(), 600));
}
