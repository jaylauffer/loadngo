use data::{
    cas::CasHash,
    p2pmsg::{self, EncodingBitset, Message, MessageType, RequestContent, TaskAccept, TaskOffer},
};
use network::p2p;

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join("")
}

fn sample_hash(seed: u8) -> CasHash {
    let mut bytes = [0u8; CasHash::LEN];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    CasHash::from_bytes(bytes)
}

#[test]
fn p2p_file_start_matches_legacy_layout() {
    let hash = sample_hash(0x30);
    let frame = p2p::file_start(10, 1234, hash);

    let mut expected = Vec::new();
    expected.extend_from_slice(&p2pmsg::PACKET_HDR.to_le_bytes());
    expected.extend_from_slice(&(MessageType::TransferFileStart as u32).to_le_bytes());
    expected.extend_from_slice(&0u32.to_le_bytes());
    let payload_len = 8 + 8 + CasHash::LEN;
    expected.extend_from_slice(&(payload_len as u32).to_le_bytes());
    expected.extend_from_slice(&10u64.to_le_bytes());
    expected.extend_from_slice(&1234u64.to_le_bytes());
    expected.extend_from_slice(hash.as_bytes());

    assert_eq!(hex(&frame), hex(&expected));

    let (_hdr, parsed) = p2p::parse_frame(&frame).expect("parse p2p file start");
    match parsed {
        Message::TransferFileStart(body) => {
            assert_eq!(body.time, 10);
            assert_eq!(body.filesize, 1234);
            assert_eq!(body.hash, hash);
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn p2p_request_content_matches_legacy_layout() {
    let left = sample_hash(0x40);
    let right = sample_hash(0x60);
    let frame = p2p::request_content(&[left, right]);

    let mut expected = Vec::new();
    expected.extend_from_slice(&p2pmsg::PACKET_HDR.to_le_bytes());
    expected.extend_from_slice(&(MessageType::RequestContent as u32).to_le_bytes());
    expected.extend_from_slice(&0u32.to_le_bytes());
    let payload_len = 2 * CasHash::LEN;
    expected.extend_from_slice(&(payload_len as u32).to_le_bytes());
    expected.extend_from_slice(left.as_bytes());
    expected.extend_from_slice(right.as_bytes());

    assert_eq!(hex(&frame), hex(&expected));

    let (_hdr, parsed) = p2p::parse_frame(&frame).expect("parse request content");
    match parsed {
        Message::RequestContent(RequestContent::Request { hashes }) => {
            assert_eq!(hashes, vec![left, right]);
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn p2p_request_content_response_matches_legacy_layout() {
    let hash = sample_hash(0x70);
    let payload = b"abc123";
    let frame = p2p::send_content(hash, payload);

    let mut expected = Vec::new();
    expected.extend_from_slice(&p2pmsg::PACKET_HDR.to_le_bytes());
    expected.extend_from_slice(&(MessageType::RequestContent as u32).to_le_bytes());
    expected.extend_from_slice(&1u32.to_le_bytes());
    let payload_len = CasHash::LEN + payload.len();
    expected.extend_from_slice(&(payload_len as u32).to_le_bytes());
    expected.extend_from_slice(hash.as_bytes());
    expected.extend_from_slice(payload);

    assert_eq!(hex(&frame), hex(&expected));

    let (_hdr, parsed) = p2p::parse_frame(&frame).expect("parse send content");
    match parsed {
        Message::RequestContent(RequestContent::Response { hash: parsed, data }) => {
            assert_eq!(parsed, hash);
            assert_eq!(data, payload);
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn p2p_encoding_bitset_matches_legacy_layout() {
    let hash = sample_hash(0x80);
    let bitset = vec![0xde, 0xad, 0xbe, 0xef];
    let frame = p2p::encoding_bitset(hash, 23, &bitset);

    let mut expected = Vec::new();
    expected.extend_from_slice(&p2pmsg::PACKET_HDR.to_le_bytes());
    expected.extend_from_slice(&(MessageType::EncodingBitset as u32).to_le_bytes());
    expected.extend_from_slice(&1u32.to_le_bytes());
    let payload_len = CasHash::LEN + 4 + bitset.len();
    expected.extend_from_slice(&(payload_len as u32).to_le_bytes());
    expected.extend_from_slice(hash.as_bytes());
    expected.extend_from_slice(&23u32.to_le_bytes());
    expected.extend_from_slice(&bitset);

    assert_eq!(hex(&frame), hex(&expected));

    let (_hdr, parsed) = p2p::parse_frame(&frame).expect("parse encoding bitset");
    match parsed {
        Message::EncodingBitset(EncodingBitset::Response {
            hash: parsed,
            numbits,
            data,
        }) => {
            assert_eq!(parsed, hash);
            assert_eq!(numbits, 23);
            assert_eq!(data, bitset);
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn p2p_task_offer_round_trips() {
    let offer = TaskOffer {
        offer_id: 10,
        task_id: 20,
        offerer_node_id: "node-a".to_string(),
        created_at: 100,
        expires_at: 160,
        summary: "Publish a task receipt".to_string(),
        capability_tags: vec!["codex".to_string(), "docs".to_string()],
        reply_endpoints: vec!["192.168.1.129:9850".to_string()],
        artifact_hint: Some("docs/TASK_OFFER_PROTOCOL.md".to_string()),
    };
    let frame = p2p::task_offer(offer.clone());

    let (header, parsed) = p2p::parse_frame(&frame).expect("parse task offer");
    assert_eq!(header.msg_type, MessageType::TaskOffer);
    assert!(!header.is_response);
    assert_eq!(parsed, Message::TaskOffer(offer));
}

#[test]
fn p2p_task_accept_round_trips() {
    let accept = TaskAccept {
        offer_id: 10,
        task_id: 20,
        worker_node_id: "node-b".to_string(),
        accepted_at: 130,
        note: Some("available".to_string()),
    };
    let frame = p2p::task_accept(accept.clone());

    let (header, parsed) = p2p::parse_frame(&frame).expect("parse task accept");
    assert_eq!(header.msg_type, MessageType::TaskAccept);
    assert!(header.is_response);
    assert_eq!(parsed, Message::TaskAccept(accept));
}
