use data::netmsg::{
    BlobData, BlobStart, FileData, FileStart, Header, Message, MessageType,
};

#[test]
fn file_start_roundtrip() {
    let msg = Message::FileStart(FileStart {
        time: 1,
        filesize: 1234,
        filename: "foo.bin".into(),
    });
    let buf = msg.to_bytes(MessageType::TransferFileStart);
    let header = Header::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::TransferFileStart);
    let parsed = Message::from_bytes(&buf, header.msg_type).unwrap();
    match parsed {
        Message::FileStart(f) => {
            assert_eq!(f.time, 1);
            assert_eq!(f.filesize, 1234);
            assert_eq!(f.filename, "foo.bin");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn file_data_roundtrip() {
    let msg = Message::FileData(FileData {
        time: 2,
        seq: 5,
        data: vec![1, 2, 3, 4],
        is_response: true,
    });
    let buf = msg.to_bytes(MessageType::TransferFileData);
    let header = Header::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::TransferFileData);
    let parsed = Message::from_bytes(&buf, header.msg_type).unwrap();
    match parsed {
        Message::FileData(f) => {
            assert_eq!(f.time, 2);
            assert_eq!(f.seq, 5);
            assert_eq!(f.data, vec![1, 2, 3, 4]);
            assert!(f.is_response);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn blob_data_roundtrip() {
    let msg = Message::BlobData(BlobData {
        time: 3,
        seq: 7,
        data: vec![9, 8, 7],
        is_response: false,
    });
    let buf = msg.to_bytes(MessageType::TransferBlobData);
    let header = Header::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::TransferBlobData);
    let parsed = Message::from_bytes(&buf, header.msg_type).unwrap();
    match parsed {
        Message::BlobData(b) => {
            assert_eq!(b.time, 3);
            assert_eq!(b.seq, 7);
            assert_eq!(b.data, vec![9, 8, 7]);
            assert!(!b.is_response);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn blob_start_roundtrip() {
    let msg = Message::BlobStart(BlobStart { time: 4, len: 99 });
    let buf = msg.to_bytes(MessageType::TransferBlobStart);
    let header = Header::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::TransferBlobStart);
    let parsed = Message::from_bytes(&buf, header.msg_type).unwrap();
    match parsed {
        Message::BlobStart(b) => {
            assert_eq!(b.time, 4);
            assert_eq!(b.len, 99);
        }
        _ => panic!("wrong variant"),
    }
}
