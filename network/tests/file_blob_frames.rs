use data::netmsg::{Message, MessageType};
use network::{netutil, Config, MulticastConfig};
use std::net::{SocketAddr, SocketAddrV6, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn file_frames_match_legacy_layout() {
    let frame = netutil::file_start(10, 1234, "foo.txt");
    let mut expected = Vec::new();
    expected.extend_from_slice(&0x6c6e6774u32.to_le_bytes());
    expected.extend_from_slice(&(MessageType::TransferFileStart as u32).to_le_bytes());
    expected.extend_from_slice(&0u32.to_le_bytes());
    let payload_len = 8 + 8 + 7;
    expected.extend_from_slice(&(payload_len as u32).to_le_bytes());
    expected.extend_from_slice(&10u64.to_le_bytes());
    expected.extend_from_slice(&1234u64.to_le_bytes());
    expected.extend_from_slice(b"foo.txt");
    assert_eq!(hex(&frame), hex(&expected));

    // FileData with response bit set (only in header).
    let data_frame = netutil::file_data(20, 3, b"\x01\x02\x03", true);
    let (_hdr, parsed) = Message::from_bytes(&data_frame).expect("parse filedata");
    match parsed {
        Message::TransferFileData(fd) => {
            assert_eq!(fd.time, 20);
            assert_eq!(fd.seq, 3);
            assert_eq!(fd.data, b"\x01\x02\x03");
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn blob_frames_match_legacy_layout() {
    let start = netutil::blob_start(5, 4096);
    let mut expected = Vec::new();
    expected.extend_from_slice(&0x6c6e6774u32.to_le_bytes());
    expected.extend_from_slice(&(MessageType::TransferBlobStart as u32).to_le_bytes());
    expected.extend_from_slice(&0u32.to_le_bytes());
    let payload_len = 8 + 4;
    expected.extend_from_slice(&(payload_len as u32).to_le_bytes());
    expected.extend_from_slice(&5u64.to_le_bytes());
    expected.extend_from_slice(&4096i32.to_le_bytes());
    assert_eq!(hex(&start), hex(&expected));

    let data_frame = netutil::blob_data(9, 7, b"blob", false);
    let (_hdr, parsed) = Message::from_bytes(&data_frame).expect("parse blobdata");
    match parsed {
        Message::TransferBlobData(bd) => {
            assert_eq!(bd.time, 9);
            assert_eq!(bd.seq, 7);
            assert_eq!(bd.data, b"blob");
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn send_receive_round_trip() {
    let mut net = network::Network::new();
    net.init().unwrap();
    net.bind("127.0.0.1:0").unwrap();

    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = receiver.local_addr().unwrap();

    let frame = netutil::move_chain(1, 2, 3, b"<move/>", false);
    net.send_frame(target, &frame).unwrap();

    let mut buf = [0u8; 512];
    let (len, _src) = receiver.recv_from(&mut buf).unwrap();
    let (hdr, msg) = netutil::parse_frame(&buf[..len]).expect("parse frame");
    assert_eq!(hdr.msg_type, MessageType::RequestMoveChain);
    match msg {
        Message::RequestMoveChain(body) => {
            assert_eq!((body.origin_id, body.sync_id, body.doid), (1, 2, 3));
            assert_eq!(body.data, b"<move/>");
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn send_receive_round_trip_ipv6() {
    let mut net = network::Network::with_config(Config {
        bind_addr: "[::1]:0".parse().unwrap(),
        ..Config::default()
    });
    net.init().unwrap();
    net.bind("[::1]:0").unwrap();

    let receiver = UdpSocket::bind("[::1]:0").unwrap();
    let target = receiver.local_addr().unwrap();

    let frame = netutil::move_chain(1, 2, 3, b"<move/>", false);
    net.send_frame(target, &frame).unwrap();

    let mut buf = [0u8; 512];
    let (len, _src) = receiver.recv_from(&mut buf).unwrap();
    let (hdr, msg) = netutil::parse_frame(&buf[..len]).expect("parse frame");
    assert_eq!(hdr.msg_type, MessageType::RequestMoveChain);
    match msg {
        Message::RequestMoveChain(body) => {
            assert_eq!((body.origin_id, body.sync_id, body.doid), (1, 2, 3));
            assert_eq!(body.data, b"<move/>");
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn sync_target_uses_ipv6_multicast_group_when_configured() {
    let config = Config {
        bind_addr: "[::]:4040".parse().unwrap(),
        multicast: vec![MulticastConfig::V6 {
            group: "ff12::1234".parse().unwrap(),
            interface: 7,
        }],
        ..Config::default()
    };

    let targets = config.sync_targets();
    assert_eq!(targets.len(), 1);
    let target = targets[0];
    assert_eq!(
        target,
        SocketAddr::V6(SocketAddrV6::new("ff12::1234".parse().unwrap(), 4040, 0, 7))
    );
}

#[test]
fn dual_stack_config_exposes_v4_and_v6_bind_addrs() {
    let config = Config::dual_stack(4040);
    let addrs = config.bind_addrs();
    assert_eq!(addrs.len(), 2);
    assert!(addrs.iter().any(SocketAddr::is_ipv4));
    assert!(addrs.iter().any(SocketAddr::is_ipv6));
}

#[test]
fn dual_stack_node_receives_on_v4_and_v6_sockets() {
    let mut net = network::Network::with_config(Config::dual_stack(0));
    net.init().unwrap();
    let addrs = net.local_addrs().unwrap();
    assert_eq!(addrs.len(), 2);

    let v4_addr = addrs.iter().copied().find(SocketAddr::is_ipv4).unwrap();
    let v6_addr = addrs.iter().copied().find(SocketAddr::is_ipv6).unwrap();
    let v4_target = SocketAddr::new("127.0.0.1".parse().unwrap(), v4_addr.port());
    let v6_target = SocketAddr::V6(SocketAddrV6::new(
        "::1".parse().unwrap(),
        v6_addr.port(),
        0,
        0,
    ));
    let sender_v4 = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sender_v6 = UdpSocket::bind("[::1]:0").unwrap();
    let frame_v4 = netutil::move_chain(1, 2, 3, b"v4", false);
    let frame_v6 = netutil::move_chain(4, 5, 6, b"v6", false);

    sender_v4.send_to(&frame_v4, v4_target).unwrap();
    sender_v6.send_to(&frame_v6, v6_target).unwrap();

    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(1);
    let drained = loop {
        let drained = net
            .drain_and_dispatch(&mut |source, _header, _message| {
                seen.push(source);
            })
            .unwrap();
        if drained >= 2 {
            break drained;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for dual-stack delivery"
        );
        thread::sleep(Duration::from_millis(5));
    };

    assert_eq!(drained, 2);
    assert!(seen.iter().any(SocketAddr::is_ipv4));
    assert!(seen.iter().any(SocketAddr::is_ipv6));
}
