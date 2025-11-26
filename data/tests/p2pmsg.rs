use data::{
    cas::CasHash,
    p2pmsg::{EncodingBitset, FileStart, Message, MessageType, RequestContent},
};

fn sample_hash(seed: u8) -> CasHash {
    let mut bytes = [0u8; CasHash::LEN];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    CasHash::from_bytes(bytes)
}

#[test]
fn p2p_file_start_roundtrip_uses_hash_payload() {
    let hash = sample_hash(0x10);
    let msg = Message::TransferFileStart(FileStart {
        time: 7,
        filesize: 1234,
        hash,
    });
    let buf = msg.to_bytes(false);
    let (header, parsed) = Message::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::TransferFileStart);
    assert!(!header.is_response);
    match parsed {
        Message::TransferFileStart(body) => {
            assert_eq!(body.time, 7);
            assert_eq!(body.filesize, 1234);
            assert_eq!(body.hash, hash);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn p2p_request_content_roundtrip_supports_request_and_response() {
    let left = sample_hash(0x20);
    let right = sample_hash(0x40);
    let request = Message::RequestContent(RequestContent::Request {
        hashes: vec![left, right],
    });
    let request_buf = request.to_bytes(false);
    let (request_header, parsed_request) = Message::from_bytes(&request_buf).unwrap();
    assert_eq!(request_header.msg_type, MessageType::RequestContent);
    assert!(!request_header.is_response);
    match parsed_request {
        Message::RequestContent(RequestContent::Request { hashes }) => {
            assert_eq!(hashes, vec![left, right]);
        }
        other => panic!("wrong request variant: {other:?}"),
    }

    let response = Message::RequestContent(RequestContent::Response {
        hash: left,
        data: b"content payload".to_vec(),
    });
    let response_buf = response.to_bytes(true);
    let (response_header, parsed_response) = Message::from_bytes(&response_buf).unwrap();
    assert_eq!(response_header.msg_type, MessageType::RequestContent);
    assert!(response_header.is_response);
    match parsed_response {
        Message::RequestContent(RequestContent::Response { hash, data }) => {
            assert_eq!(hash, left);
            assert_eq!(data, b"content payload");
        }
        other => panic!("wrong response variant: {other:?}"),
    }
}

#[test]
fn p2p_encoding_bitset_roundtrip_supports_request_and_response() {
    let hash = sample_hash(0x50);
    let request = Message::EncodingBitset(EncodingBitset::Request { hash });
    let request_buf = request.to_bytes(false);
    let (_, parsed_request) = Message::from_bytes(&request_buf).unwrap();
    match parsed_request {
        Message::EncodingBitset(EncodingBitset::Request { hash: parsed }) => {
            assert_eq!(parsed, hash);
        }
        other => panic!("wrong request variant: {other:?}"),
    }

    let response = Message::EncodingBitset(EncodingBitset::Response {
        hash,
        numbits: 19,
        data: vec![0xaa, 0xbb, 0xcc],
    });
    let response_buf = response.to_bytes(true);
    let (_, parsed_response) = Message::from_bytes(&response_buf).unwrap();
    match parsed_response {
        Message::EncodingBitset(EncodingBitset::Response {
            hash: parsed,
            numbits,
            data,
        }) => {
            assert_eq!(parsed, hash);
            assert_eq!(numbits, 19);
            assert_eq!(data, vec![0xaa, 0xbb, 0xcc]);
        }
        other => panic!("wrong response variant: {other:?}"),
    }
}
