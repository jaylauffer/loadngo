use data::netmsg::{Message, MessageType};
use network::netutil;
use std::net::UdpSocket;

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
