use data::{
    cas::{CasHash, CasStorage},
    p2pmsg::{FileStart, Message},
};
#[cfg(target_os = "linux")]
use loadngo_proactor::IoUringPort;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
use loadngo_proactor::KqueuePort;
use loadngo_proactor::{ChannelPort, Proactor};
use network::{p2p, Config, Network};
use std::{
    net::{SocketAddr, SocketAddrV6, UdpSocket},
    sync::atomic::{AtomicUsize, Ordering},
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};
use tempfile::tempdir;

fn recv_until_dispatch(
    sneakernet: &mut p2p::SneakerNet,
    network: &Network,
    store: &CasStorage,
) -> p2p::DispatchResult {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(dispatched) = sneakernet.recv_and_dispatch(network, store).unwrap() {
            return dispatched;
        }
        assert!(Instant::now() < deadline, "timed out waiting for dispatch");
        thread::sleep(Duration::from_millis(5));
    }
}

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
    let dispatched = recv_until_dispatch(&mut sneakernet, &server, &store);

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
        let dispatched = recv_until_dispatch(&mut sneakernet, &receiver, &store);
        last_events = dispatched.events;
    }

    assert_eq!(last_events, vec![p2p::Event::ContentStored(hash)]);
    assert_eq!(store.verified_read_all(hash).unwrap(), payload);
    assert!(!sneakernet
        .core()
        .has_content_transfer(sender.local_addr().unwrap(), 600));
}

#[test]
fn proactor_pump_drives_sneakernet_dispatch_and_storage() {
    let dir = tempdir().unwrap();
    let store = Arc::new(CasStorage::new(dir.path()).unwrap());
    let payload = b"abcdefghij";
    let hash = CasHash::digest(payload);

    let mut receiver = Network::new();
    receiver.init().unwrap();
    receiver.bind("127.0.0.1:0").unwrap();
    let receiver = Arc::new(receiver);
    let receiver_addr = receiver.local_addr().unwrap();

    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let proactor = Proactor::new(ChannelPort::new());
    let handle = proactor.handle();
    let stop_handle = handle.clone();
    let worker = thread::spawn(move || proactor.run_until_stopped().unwrap());
    let (tx, rx) = mpsc::channel();

    let _pump = p2p::ProactorSneakerNet::start_with_chunk_size(
        Arc::clone(&receiver),
        Arc::clone(&store),
        handle,
        Duration::from_millis(5),
        4,
        move |result| match result {
            Ok(dispatch) => {
                if dispatch.events == vec![p2p::Event::ContentStored(hash)] {
                    tx.send(Ok(())).unwrap();
                    stop_handle.stop().unwrap();
                }
            }
            Err(err) => {
                tx.send(Err(format!("{err:#}"))).unwrap();
                stop_handle.stop().unwrap();
            }
        },
    )
    .unwrap();

    let frames = [
        p2p::file_start(700, payload.len() as u64, hash),
        p2p::file_data(700, 0, &payload[0..4], false),
        p2p::file_data(700, 1, &payload[4..8], false),
        p2p::file_data(700, 2, &payload[8..10], false),
        p2p::file_end(700, false),
    ];

    for frame in frames {
        sender.send_to(&frame, receiver_addr).unwrap();
    }

    rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    worker.join().unwrap();
    assert_eq!(store.verified_read_all(hash).unwrap(), payload);
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
#[test]
fn registered_proactor_pump_drives_sneakernet_dispatch_and_storage() {
    let dir = tempdir().unwrap();
    let store = Arc::new(CasStorage::new(dir.path()).unwrap());
    let payload = b"abcdefghij";
    let hash = CasHash::digest(payload);

    let mut receiver = Network::new();
    receiver.init().unwrap();
    receiver.bind("127.0.0.1:0").unwrap();
    let receiver = Arc::new(receiver);
    let receiver_addr = receiver.local_addr().unwrap();

    #[cfg(target_os = "linux")]
    let proactor = Proactor::new(IoUringPort::new().unwrap());
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    let proactor = Proactor::new(KqueuePort::new().unwrap());

    let handle = proactor.handle();
    let stop_handle = handle.clone();
    let worker = thread::spawn(move || proactor.run_until_stopped().unwrap());
    let (tx, rx) = mpsc::channel();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();

    let _pump = p2p::ProactorSneakerNet::start_registered(
        Arc::clone(&receiver),
        Arc::clone(&store),
        handle,
        4,
        move |result| match result {
            Ok(dispatch) => {
                if dispatch.events == vec![p2p::Event::ContentStored(hash)] {
                    tx.send(Ok(())).unwrap();
                    stop_handle.stop().unwrap();
                }
            }
            Err(err) => {
                tx.send(Err(format!("{err:#}"))).unwrap();
                stop_handle.stop().unwrap();
            }
        },
    )
    .unwrap();

    let frames = [
        p2p::file_start(800, payload.len() as u64, hash),
        p2p::file_data(800, 0, &payload[0..4], false),
        p2p::file_data(800, 1, &payload[4..8], false),
        p2p::file_data(800, 2, &payload[8..10], false),
        p2p::file_end(800, false),
    ];

    for frame in frames {
        sender.send_to(&frame, receiver_addr).unwrap();
    }

    rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    worker.join().unwrap();
    assert_eq!(store.verified_read_all(hash).unwrap(), payload);
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
#[test]
fn registered_proactor_pump_handles_dual_stack_node_sockets() {
    let dir = tempdir().unwrap();
    let store = Arc::new(CasStorage::new(dir.path()).unwrap());
    let payload = b"abcdefghij";
    let hash = CasHash::digest(payload);
    store.add_content(payload).unwrap();

    let mut receiver = Network::with_config(Config::dual_stack(0));
    receiver.init().unwrap();
    let receiver = Arc::new(receiver);
    let addrs = receiver.local_addrs().unwrap();
    let v4_bound = addrs.iter().copied().find(SocketAddr::is_ipv4).unwrap();
    let v6_bound = addrs.iter().copied().find(SocketAddr::is_ipv6).unwrap();
    let v4_target = SocketAddr::new("127.0.0.1".parse().unwrap(), v4_bound.port());
    let v6_target = SocketAddr::V6(SocketAddrV6::new(
        "::1".parse().unwrap(),
        v6_bound.port(),
        0,
        0,
    ));

    #[cfg(target_os = "linux")]
    let proactor = Proactor::new(IoUringPort::new().unwrap());
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    let proactor = Proactor::new(KqueuePort::new().unwrap());

    let handle = proactor.handle();
    let stop_handle = handle.clone();
    let worker = thread::spawn(move || proactor.run_until_stopped().unwrap());
    let (tx, rx) = mpsc::channel();
    let seen = Arc::new(AtomicUsize::new(0));
    let seen_in_callback = Arc::clone(&seen);

    let _pump = p2p::ProactorSneakerNet::start_registered(
        Arc::clone(&receiver),
        Arc::clone(&store),
        handle,
        4,
        move |result| match result {
            Ok(dispatch) => {
                tx.send(dispatch.source).unwrap();
                if seen_in_callback.fetch_add(1, Ordering::SeqCst) + 1 >= 2 {
                    stop_handle.stop().unwrap();
                }
            }
            Err(err) => {
                panic!("dual-stack registered proactor pump failed: {err:#}");
            }
        },
    )
    .unwrap();

    let requester_v4 = UdpSocket::bind("127.0.0.1:0").unwrap();
    requester_v4
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let requester_v6 = UdpSocket::bind("[::1]:0").unwrap();
    requester_v6
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();

    let request = p2p::request_content(&[hash]);
    requester_v4.send_to(&request, v4_target).unwrap();
    requester_v6.send_to(&request, v6_target).unwrap();

    let mut buf = [0u8; 512];
    let _ = requester_v4.recv_from(&mut buf).unwrap();
    let _ = requester_v6.recv_from(&mut buf).unwrap();

    let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let second = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    worker.join().unwrap();

    assert!(first.is_ipv4() || second.is_ipv4());
    assert!(first.is_ipv6() || second.is_ipv6());
}
