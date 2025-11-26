use data::netmsg::{BlobData, BlobStart, FileData, FileStart, Message, MessageType};

#[test]
fn file_start_roundtrip() {
    let msg = Message::TransferFileStart(FileStart {
        time: 1,
        filesize: 1234,
        filename: "foo.bin".into(),
    });
    let buf = msg.to_bytes(MessageType::TransferFileStart, false);
    let (header, parsed) = Message::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::TransferFileStart);
    match parsed {
        Message::TransferFileStart(f) => {
            assert_eq!(f.time, 1);
            assert_eq!(f.filesize, 1234);
            assert_eq!(f.filename, "foo.bin");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn file_data_roundtrip() {
    let msg = Message::TransferFileData(FileData {
        time: 2,
        seq: 5,
        data: vec![1, 2, 3, 4],
    });
    let buf = msg.to_bytes(MessageType::TransferFileData, true);
    let (header, parsed) = Message::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::TransferFileData);
    assert!(header.is_response);
    match parsed {
        Message::TransferFileData(f) => {
            assert_eq!(f.time, 2);
            assert_eq!(f.seq, 5);
            assert_eq!(f.data, vec![1, 2, 3, 4]);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn blob_data_roundtrip() {
    let msg = Message::TransferBlobData(BlobData {
        time: 3,
        seq: 7,
        data: vec![9, 8, 7],
    });
    let buf = msg.to_bytes(MessageType::TransferBlobData, false);
    let (header, parsed) = Message::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::TransferBlobData);
    assert!(!header.is_response);
    match parsed {
        Message::TransferBlobData(b) => {
            assert_eq!(b.time, 3);
            assert_eq!(b.seq, 7);
            assert_eq!(b.data, vec![9, 8, 7]);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn blob_start_roundtrip() {
    let msg = Message::TransferBlobStart(BlobStart { time: 4, len: 99 });
    let buf = msg.to_bytes(MessageType::TransferBlobStart, false);
    let (header, parsed) = Message::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::TransferBlobStart);
    match parsed {
        Message::TransferBlobStart(b) => {
            assert_eq!(b.time, 4);
            assert_eq!(b.len, 99);
        }
        _ => panic!("wrong variant"),
    }
}
