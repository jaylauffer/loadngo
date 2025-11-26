use data::netmsg::{Discrepancy, EntityPayload, Message, MessageType, UserIntroduction};
use network::netutil;

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn user_intro_matches_legacy_layout() {
    // Arrange test values that mirror the C++ MakeUserIntroMsg layout.
    let mut user_key = [0u8; 64];
    for (i, b) in user_key.iter_mut().enumerate() {
        *b = (i as u8).wrapping_add(1);
    }
    let frame = netutil::user_intro(0x0102_0304_0506_0708, user_key, "alice", "deviceX", false);

    // Manually build the expected buffer per NetUtil.cpp (tag, type, resp, len, then payload).
    let mut expected = Vec::new();
    expected.extend_from_slice(&0x6c6e6774u32.to_le_bytes()); // tag
    expected.extend_from_slice(&(MessageType::UserIntroduction as u32).to_le_bytes());
    expected.extend_from_slice(&0u32.to_le_bytes()); // is_response
    let payload_len = 8 + 64 + 5 + 1 + 7; // ids + hash + name + nul + device
    expected.extend_from_slice(&(payload_len as u32).to_le_bytes());
    expected.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
    expected.extend_from_slice(&user_key);
    expected.extend_from_slice(b"alice");
    expected.push(0);
    expected.extend_from_slice(b"deviceX");

    assert_eq!(hex(&frame), hex(&expected));

    let (_hdr, parsed) = Message::from_bytes(&frame).expect("parse intro");
    match parsed {
        Message::UserIntroduction(UserIntroduction {
            machine_id,
            user_key: parsed_key,
            name,
            device,
        }) => {
            assert_eq!(machine_id, 0x0102_0304_0506_0708);
            assert_eq!(parsed_key, user_key);
            assert_eq!(name, "alice");
            assert_eq!(device, "deviceX");
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn discrepancies_match_legacy_layout() {
    let entries = vec![
        Discrepancy {
            sync_id: 11,
            origin_id: 22,
            foreign_id: 33,
            local_id: 44,
            discrepancy: 55,
        },
        Discrepancy {
            sync_id: 66,
            origin_id: 77,
            foreign_id: 88,
            local_id: 99,
            discrepancy: 111,
        },
    ];
    let frame = netutil::discrepancies(999, entries.clone(), true);

    let mut expected = Vec::new();
    expected.extend_from_slice(&0x6c6e6774u32.to_le_bytes());
    expected.extend_from_slice(&(MessageType::ReportDiscrepancies as u32).to_le_bytes());
    expected.extend_from_slice(&1u32.to_le_bytes()); // response
                                                     // payload: syncid + 2 discrepancy structs (5 u64 each)
    let payload_len = 8 + (entries.len() * 5 * 8);
    expected.extend_from_slice(&(payload_len as u32).to_le_bytes());
    expected.extend_from_slice(&999u64.to_le_bytes());
    for d in &entries {
        expected.extend_from_slice(&d.sync_id.to_le_bytes());
        expected.extend_from_slice(&d.origin_id.to_le_bytes());
        expected.extend_from_slice(&d.foreign_id.to_le_bytes());
        expected.extend_from_slice(&d.local_id.to_le_bytes());
        expected.extend_from_slice(&d.discrepancy.to_le_bytes());
    }

    assert_eq!(hex(&frame), hex(&expected));

    let (_hdr, parsed) = Message::from_bytes(&frame).expect("parse discrepancies");
    match parsed {
        Message::ReportDiscrepancies(report) => {
            assert_eq!(report.sync_id, 999);
            assert_eq!(report.discrepancies.len(), 2);
            assert_eq!(report.discrepancies[0].local_id, 44);
            assert_eq!(report.discrepancies[1].discrepancy, 111);
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}

#[test]
fn move_chain_matches_legacy_layout() {
    let payload = b"<xml move-chain/>";
    let frame = netutil::move_chain(1, 2, 3, payload, false);

    let mut expected = Vec::new();
    expected.extend_from_slice(&0x6c6e6774u32.to_le_bytes());
    expected.extend_from_slice(&(MessageType::RequestMoveChain as u32).to_le_bytes());
    expected.extend_from_slice(&0u32.to_le_bytes());
    let payload_len = 24 + payload.len();
    expected.extend_from_slice(&(payload_len as u32).to_le_bytes());
    expected.extend_from_slice(&1u64.to_le_bytes());
    expected.extend_from_slice(&2u64.to_le_bytes());
    expected.extend_from_slice(&3u64.to_le_bytes());
    expected.extend_from_slice(payload);

    assert_eq!(hex(&frame), hex(&expected));

    let (_hdr, parsed) = Message::from_bytes(&frame).expect("parse move-chain");
    match parsed {
        Message::RequestMoveChain(EntityPayload {
            origin_id,
            sync_id,
            doid,
            data,
        }) => {
            assert_eq!((origin_id, sync_id, doid), (1, 2, 3));
            assert_eq!(data, payload);
        }
        other => panic!("unexpected parsed variant: {other:?}"),
    }
}
