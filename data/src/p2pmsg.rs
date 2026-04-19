use crate::cas::CasHash;
use serde::{Deserialize, Serialize};

pub const PACKET_HDR: u32 = 0x6c6e6774;
pub const HDR_LEN: usize = 16;
pub const PING_LEN: usize = 8;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Empty = 0,
    UserIntroduction,
    UserDeparture,
    TransferBlobStart,
    TransferBlobEnd,
    TransferBlobData,
    TransferBlobMissed,
    TransferBlobComplete,
    Ping,
    EncodingBitset,
    RequestContent,
    TransferFileStart,
    TransferFileData,
    TransferFileEnd,
    TransferFileMissed,
    DeployComplete,
    TaskOffer,
    TaskAccept,
    TaskRequest,
    TaskStatus,
    TaskResult,
    TaskAck,
    MessageCount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub tag: u32,
    pub msg_type: MessageType,
    pub is_response: bool,
    pub length: u32,
}

impl Header {
    pub fn new(msg_type: MessageType, is_response: bool, length: u32) -> Self {
        Self {
            tag: PACKET_HDR,
            msg_type,
            is_response,
            length,
        }
    }

    pub fn to_bytes(self) -> [u8; HDR_LEN] {
        let mut buf = [0u8; HDR_LEN];
        buf[0..4].copy_from_slice(&self.tag.to_le_bytes());
        buf[4..8].copy_from_slice(&(self.msg_type as u32).to_le_bytes());
        buf[8..12].copy_from_slice(&(u32::from(self.is_response)).to_le_bytes());
        buf[12..16].copy_from_slice(&self.length.to_le_bytes());
        buf
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < HDR_LEN {
            return None;
        }
        let tag = u32::from_le_bytes(buf[0..4].try_into().ok()?);
        let msg_type = match u32::from_le_bytes(buf[4..8].try_into().ok()?) {
            0 => MessageType::Empty,
            1 => MessageType::UserIntroduction,
            2 => MessageType::UserDeparture,
            3 => MessageType::TransferBlobStart,
            4 => MessageType::TransferBlobEnd,
            5 => MessageType::TransferBlobData,
            6 => MessageType::TransferBlobMissed,
            7 => MessageType::TransferBlobComplete,
            8 => MessageType::Ping,
            9 => MessageType::EncodingBitset,
            10 => MessageType::RequestContent,
            11 => MessageType::TransferFileStart,
            12 => MessageType::TransferFileData,
            13 => MessageType::TransferFileEnd,
            14 => MessageType::TransferFileMissed,
            15 => MessageType::DeployComplete,
            16 => MessageType::TaskOffer,
            17 => MessageType::TaskAccept,
            18 => MessageType::TaskRequest,
            19 => MessageType::TaskStatus,
            20 => MessageType::TaskResult,
            21 => MessageType::TaskAck,
            22 => MessageType::MessageCount,
            _ => return None,
        };
        let is_response = u32::from_le_bytes(buf[8..12].try_into().ok()?) != 0;
        let length = u32::from_le_bytes(buf[12..16].try_into().ok()?);
        Some(Self {
            tag,
            msg_type,
            is_response,
            length,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStart {
    pub time: u64,
    pub filesize: u64,
    pub hash: CasHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileData {
    pub time: u64,
    pub seq: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEnd {
    pub time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMissed {
    pub time: u64,
    pub seq: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobStart {
    pub time: u64,
    pub len: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobData {
    pub time: u64,
    pub seq: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobEnd {
    pub time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMissed {
    pub time: u64,
    pub seq: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobComplete {
    pub time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    pub time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingBitset {
    Request {
        hash: CasHash,
    },
    Response {
        hash: CasHash,
        numbits: u32,
        data: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestContent {
    Request { hashes: Vec<CasHash> },
    Response { hash: CasHash, data: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOffer {
    pub offer_id: u64,
    pub request_id: u64,
    pub worker_node_id: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub capability_tags: Vec<String>,
    pub reply_endpoints: Vec<String>,
    pub estimated_duration_secs: Option<u64>,
    pub max_status_interval_secs: Option<u64>,
    pub note: Option<String>,
    pub artifact_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAccept {
    pub assignment_id: u64,
    pub request_id: u64,
    pub offer_id: u64,
    pub submitter_node_id: String,
    pub worker_node_id: String,
    pub accepted_at: u64,
    pub status_check_interval_secs: u64,
    pub expected_duration_secs: Option<u64>,
    pub expected_delivery_by: Option<u64>,
    pub submitter_reply_endpoint: Option<String>,
    pub success_criteria: Option<String>,
    pub artifact_hint: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRequest {
    pub request_id: u64,
    pub submitter_node_id: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub summary: String,
    pub capability_tags: Vec<String>,
    pub reply_endpoints: Vec<String>,
    pub requested_duration_secs: Option<u64>,
    pub success_criteria: Option<String>,
    pub artifact_hint: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStatus {
    pub assignment_id: u64,
    pub request_id: u64,
    pub offer_id: u64,
    pub worker_node_id: String,
    pub status_at: u64,
    pub state: String,
    pub next_check_in_by: Option<u64>,
    pub note: Option<String>,
    pub artifact_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub assignment_id: u64,
    pub request_id: u64,
    pub offer_id: u64,
    pub worker_node_id: String,
    pub submitted_at: u64,
    pub artifact_hint: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAck {
    pub assignment_id: u64,
    pub request_id: u64,
    pub offer_id: u64,
    pub submitter_node_id: String,
    pub acked_at: u64,
    pub accepted: bool,
    pub qcoin_tx_hint: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Empty,
    UserIntroduction(String),
    UserDeparture(String),
    TransferBlobStart(BlobStart),
    TransferBlobEnd(BlobEnd),
    TransferBlobData(BlobData),
    TransferBlobMissed(BlobMissed),
    TransferBlobComplete(BlobComplete),
    Ping(Ping),
    EncodingBitset(EncodingBitset),
    RequestContent(RequestContent),
    TransferFileStart(FileStart),
    TransferFileData(FileData),
    TransferFileEnd(FileEnd),
    TransferFileMissed(FileMissed),
    DeployComplete(Vec<u8>),
    TaskOffer(TaskOffer),
    TaskAccept(TaskAccept),
    TaskRequest(TaskRequest),
    TaskStatus(TaskStatus),
    TaskResult(TaskResult),
    TaskAck(TaskAck),
}

impl Message {
    pub fn message_type(&self) -> MessageType {
        match self {
            Message::Empty => MessageType::Empty,
            Message::UserIntroduction(_) => MessageType::UserIntroduction,
            Message::UserDeparture(_) => MessageType::UserDeparture,
            Message::TransferBlobStart(_) => MessageType::TransferBlobStart,
            Message::TransferBlobEnd(_) => MessageType::TransferBlobEnd,
            Message::TransferBlobData(_) => MessageType::TransferBlobData,
            Message::TransferBlobMissed(_) => MessageType::TransferBlobMissed,
            Message::TransferBlobComplete(_) => MessageType::TransferBlobComplete,
            Message::Ping(_) => MessageType::Ping,
            Message::EncodingBitset(_) => MessageType::EncodingBitset,
            Message::RequestContent(_) => MessageType::RequestContent,
            Message::TransferFileStart(_) => MessageType::TransferFileStart,
            Message::TransferFileData(_) => MessageType::TransferFileData,
            Message::TransferFileEnd(_) => MessageType::TransferFileEnd,
            Message::TransferFileMissed(_) => MessageType::TransferFileMissed,
            Message::DeployComplete(_) => MessageType::DeployComplete,
            Message::TaskOffer(_) => MessageType::TaskOffer,
            Message::TaskAccept(_) => MessageType::TaskAccept,
            Message::TaskRequest(_) => MessageType::TaskRequest,
            Message::TaskStatus(_) => MessageType::TaskStatus,
            Message::TaskResult(_) => MessageType::TaskResult,
            Message::TaskAck(_) => MessageType::TaskAck,
        }
    }

    pub fn to_bytes(&self, is_response: bool) -> Vec<u8> {
        let payload = match self {
            Message::Empty => Vec::new(),
            Message::UserIntroduction(name) | Message::UserDeparture(name) => {
                name.as_bytes().to_vec()
            }
            Message::TransferBlobStart(body) => {
                let mut buf = Vec::with_capacity(12);
                buf.extend_from_slice(&body.time.to_le_bytes());
                buf.extend_from_slice(&body.len.to_le_bytes());
                buf
            }
            Message::TransferBlobEnd(body) => body.time.to_le_bytes().to_vec(),
            Message::TransferBlobData(body) => {
                let mut buf = Vec::with_capacity(12 + body.data.len());
                buf.extend_from_slice(&body.time.to_le_bytes());
                buf.extend_from_slice(&body.seq.to_le_bytes());
                buf.extend_from_slice(&body.data);
                buf
            }
            Message::TransferBlobMissed(body) => {
                let mut buf = Vec::with_capacity(12);
                buf.extend_from_slice(&body.time.to_le_bytes());
                buf.extend_from_slice(&body.seq.to_le_bytes());
                buf
            }
            Message::TransferBlobComplete(body) => body.time.to_le_bytes().to_vec(),
            Message::Ping(body) => body.time.to_le_bytes().to_vec(),
            Message::EncodingBitset(body) => match body {
                EncodingBitset::Request { hash } => hash.as_bytes().to_vec(),
                EncodingBitset::Response {
                    hash,
                    numbits,
                    data,
                } => {
                    let mut buf = Vec::with_capacity(CasHash::LEN + 4 + data.len());
                    buf.extend_from_slice(hash.as_bytes());
                    buf.extend_from_slice(&numbits.to_le_bytes());
                    buf.extend_from_slice(data);
                    buf
                }
            },
            Message::RequestContent(body) => match body {
                RequestContent::Request { hashes } => {
                    let mut buf = Vec::with_capacity(hashes.len() * CasHash::LEN);
                    for hash in hashes {
                        buf.extend_from_slice(hash.as_bytes());
                    }
                    buf
                }
                RequestContent::Response { hash, data } => {
                    let mut buf = Vec::with_capacity(CasHash::LEN + data.len());
                    buf.extend_from_slice(hash.as_bytes());
                    buf.extend_from_slice(data);
                    buf
                }
            },
            Message::TransferFileStart(body) => {
                let mut buf = Vec::with_capacity(8 + 8 + CasHash::LEN);
                buf.extend_from_slice(&body.time.to_le_bytes());
                buf.extend_from_slice(&body.filesize.to_le_bytes());
                buf.extend_from_slice(body.hash.as_bytes());
                buf
            }
            Message::TransferFileData(body) => {
                let mut buf = Vec::with_capacity(12 + body.data.len());
                buf.extend_from_slice(&body.time.to_le_bytes());
                buf.extend_from_slice(&body.seq.to_le_bytes());
                buf.extend_from_slice(&body.data);
                buf
            }
            Message::TransferFileEnd(body) => body.time.to_le_bytes().to_vec(),
            Message::TransferFileMissed(body) => {
                let mut buf = Vec::with_capacity(12);
                buf.extend_from_slice(&body.time.to_le_bytes());
                buf.extend_from_slice(&body.seq.to_le_bytes());
                buf
            }
            Message::DeployComplete(data) => data.clone(),
            Message::TaskOffer(offer) => {
                serde_json::to_vec(offer).expect("task offer serialization should succeed")
            }
            Message::TaskAccept(accept) => {
                serde_json::to_vec(accept).expect("task accept serialization should succeed")
            }
            Message::TaskRequest(request) => {
                serde_json::to_vec(request).expect("task request serialization should succeed")
            }
            Message::TaskStatus(status) => {
                serde_json::to_vec(status).expect("task status serialization should succeed")
            }
            Message::TaskResult(result) => {
                serde_json::to_vec(result).expect("task result serialization should succeed")
            }
            Message::TaskAck(ack) => {
                serde_json::to_vec(ack).expect("task ack serialization should succeed")
            }
        };
        let header = Header::new(self.message_type(), is_response, payload.len() as u32);
        let mut out = header.to_bytes().to_vec();
        out.extend_from_slice(&payload);
        out
    }

    pub fn from_bytes(buf: &[u8]) -> Option<(Header, Message)> {
        let header = Header::from_bytes(buf)?;
        let body_len = header.length as usize;
        if buf.len() < HDR_LEN + body_len {
            return None;
        }
        let body = &buf[HDR_LEN..HDR_LEN + body_len];
        let message = match header.msg_type {
            MessageType::Empty => Message::Empty,
            MessageType::UserIntroduction => {
                Message::UserIntroduction(String::from_utf8_lossy(body).into_owned())
            }
            MessageType::UserDeparture => {
                Message::UserDeparture(String::from_utf8_lossy(body).into_owned())
            }
            MessageType::TransferBlobStart => {
                if body_len != 12 {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                let len = i32::from_le_bytes(body[8..12].try_into().ok()?);
                Message::TransferBlobStart(BlobStart { time, len })
            }
            MessageType::TransferBlobEnd => {
                if body_len != 8 {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                Message::TransferBlobEnd(BlobEnd { time })
            }
            MessageType::TransferBlobData => {
                if body_len < 12 {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                let seq = u32::from_le_bytes(body[8..12].try_into().ok()?);
                Message::TransferBlobData(BlobData {
                    time,
                    seq,
                    data: body[12..].to_vec(),
                })
            }
            MessageType::TransferBlobMissed => {
                if body_len != 12 {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                let seq = u32::from_le_bytes(body[8..12].try_into().ok()?);
                Message::TransferBlobMissed(BlobMissed { time, seq })
            }
            MessageType::TransferBlobComplete => {
                if body_len != 8 {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                Message::TransferBlobComplete(BlobComplete { time })
            }
            MessageType::Ping => {
                if body_len != PING_LEN {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                Message::Ping(Ping { time })
            }
            MessageType::EncodingBitset => {
                if header.is_response {
                    if body_len < CasHash::LEN + 4 {
                        return None;
                    }
                    let hash = parse_hash(&body[0..CasHash::LEN])?;
                    let numbits =
                        u32::from_le_bytes(body[CasHash::LEN..CasHash::LEN + 4].try_into().ok()?);
                    Message::EncodingBitset(EncodingBitset::Response {
                        hash,
                        numbits,
                        data: body[CasHash::LEN + 4..].to_vec(),
                    })
                } else {
                    if body_len != CasHash::LEN {
                        return None;
                    }
                    Message::EncodingBitset(EncodingBitset::Request {
                        hash: parse_hash(body)?,
                    })
                }
            }
            MessageType::RequestContent => {
                if header.is_response {
                    if body_len < CasHash::LEN {
                        return None;
                    }
                    let hash = parse_hash(&body[0..CasHash::LEN])?;
                    Message::RequestContent(RequestContent::Response {
                        hash,
                        data: body[CasHash::LEN..].to_vec(),
                    })
                } else {
                    if body_len % CasHash::LEN != 0 {
                        return None;
                    }
                    let mut hashes = Vec::with_capacity(body_len / CasHash::LEN);
                    for chunk in body.chunks(CasHash::LEN) {
                        hashes.push(parse_hash(chunk)?);
                    }
                    Message::RequestContent(RequestContent::Request { hashes })
                }
            }
            MessageType::TransferFileStart => {
                if body_len != 8 + 8 + CasHash::LEN {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                let filesize = u64::from_le_bytes(body[8..16].try_into().ok()?);
                let hash = parse_hash(&body[16..16 + CasHash::LEN])?;
                Message::TransferFileStart(FileStart {
                    time,
                    filesize,
                    hash,
                })
            }
            MessageType::TransferFileData => {
                if body_len < 12 {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                let seq = u32::from_le_bytes(body[8..12].try_into().ok()?);
                Message::TransferFileData(FileData {
                    time,
                    seq,
                    data: body[12..].to_vec(),
                })
            }
            MessageType::TransferFileEnd => {
                if body_len != 8 {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                Message::TransferFileEnd(FileEnd { time })
            }
            MessageType::TransferFileMissed => {
                if body_len != 12 {
                    return None;
                }
                let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                let seq = u32::from_le_bytes(body[8..12].try_into().ok()?);
                Message::TransferFileMissed(FileMissed { time, seq })
            }
            MessageType::DeployComplete => Message::DeployComplete(body.to_vec()),
            MessageType::TaskOffer => Message::TaskOffer(serde_json::from_slice(body).ok()?),
            MessageType::TaskAccept => Message::TaskAccept(serde_json::from_slice(body).ok()?),
            MessageType::TaskRequest => Message::TaskRequest(serde_json::from_slice(body).ok()?),
            MessageType::TaskStatus => Message::TaskStatus(serde_json::from_slice(body).ok()?),
            MessageType::TaskResult => Message::TaskResult(serde_json::from_slice(body).ok()?),
            MessageType::TaskAck => Message::TaskAck(serde_json::from_slice(body).ok()?),
            MessageType::MessageCount => return None,
        };
        Some((header, message))
    }
}

fn parse_hash(body: &[u8]) -> Option<CasHash> {
    CasHash::from_slice(body).ok()
}

#[cfg(test)]
mod tests {
    use super::{Message, TaskAccept, TaskAck, TaskOffer, TaskRequest, TaskResult, TaskStatus};

    #[test]
    fn task_offer_round_trips() {
        let offer = TaskOffer {
            offer_id: 1,
            request_id: 2,
            worker_node_id: "worker-a".to_string(),
            created_at: 100,
            expires_at: 140,
            capability_tags: vec!["codex".to_string(), "doc".to_string()],
            reply_endpoints: vec!["192.168.1.129:9850".to_string()],
            estimated_duration_secs: Some(600),
            max_status_interval_secs: Some(60),
            note: Some("can take this on the wired side".to_string()),
            artifact_hint: Some("docs/TASK_OFFER_PROTOCOL.md".to_string()),
        };

        let bytes = Message::TaskOffer(offer.clone()).to_bytes(false);
        let (_, decoded) = Message::from_bytes(&bytes).expect("task offer frame should parse");

        assert_eq!(decoded, Message::TaskOffer(offer));
    }

    #[test]
    fn task_accept_round_trips() {
        let accept = TaskAccept {
            assignment_id: 9,
            request_id: 10,
            offer_id: 10,
            submitter_node_id: "submitter-a".to_string(),
            worker_node_id: "worker-b".to_string(),
            accepted_at: 200,
            status_check_interval_secs: 30,
            expected_duration_secs: Some(900),
            expected_delivery_by: Some(1100),
            submitter_reply_endpoint: Some("[fd10:10:10::2]:9850".to_string()),
            success_criteria: Some("commit pushed and tests green".to_string()),
            artifact_hint: Some("docs/TASK_EXECUTION_TEST_PLAN.md".to_string()),
            note: Some("selected for execution".to_string()),
        };

        let bytes = Message::TaskAccept(accept.clone()).to_bytes(false);
        let (_, decoded) = Message::from_bytes(&bytes).expect("task accept frame should parse");

        assert_eq!(decoded, Message::TaskAccept(accept));
    }

    #[test]
    fn task_request_round_trips() {
        let request = TaskRequest {
            request_id: 99,
            submitter_node_id: "submitter-a".to_string(),
            created_at: 300,
            expires_at: 360,
            summary: "Review a patch and return acceptance criteria evidence".to_string(),
            capability_tags: vec!["codex".to_string(), "loadngo-task".to_string()],
            reply_endpoints: vec!["10.10.10.6:9850".to_string()],
            requested_duration_secs: Some(1200),
            success_criteria: Some("review notes posted with file references".to_string()),
            artifact_hint: Some("docs/TASK_OFFER_PROTOCOL.md".to_string()),
            note: Some("prefer the wired lab path".to_string()),
        };

        let bytes = Message::TaskRequest(request.clone()).to_bytes(false);
        let (_, decoded) = Message::from_bytes(&bytes).expect("task request frame should parse");

        assert_eq!(decoded, Message::TaskRequest(request));
    }

    #[test]
    fn task_status_round_trips() {
        let status = TaskStatus {
            assignment_id: 500,
            request_id: 99,
            offer_id: 77,
            worker_node_id: "worker-b".to_string(),
            status_at: 350,
            state: "in-progress".to_string(),
            next_check_in_by: Some(380),
            note: Some("drafting notes now".to_string()),
            artifact_hint: None,
        };

        let bytes = Message::TaskStatus(status.clone()).to_bytes(true);
        let (_, decoded) = Message::from_bytes(&bytes).expect("task status frame should parse");

        assert_eq!(decoded, Message::TaskStatus(status));
    }

    #[test]
    fn task_result_round_trips() {
        let result = TaskResult {
            assignment_id: 500,
            request_id: 99,
            offer_id: 77,
            worker_node_id: "worker-b".to_string(),
            submitted_at: 400,
            artifact_hint: Some("docs/TASK_EXECUTION_TEST_PLAN.md".to_string()),
            note: Some("all success criteria addressed".to_string()),
        };

        let bytes = Message::TaskResult(result.clone()).to_bytes(true);
        let (_, decoded) = Message::from_bytes(&bytes).expect("task result frame should parse");

        assert_eq!(decoded, Message::TaskResult(result));
    }

    #[test]
    fn task_ack_round_trips() {
        let ack = TaskAck {
            assignment_id: 500,
            request_id: 99,
            offer_id: 77,
            submitter_node_id: "submitter-a".to_string(),
            acked_at: 420,
            accepted: true,
            qcoin_tx_hint: Some("qcoin:tx:abc123".to_string()),
            note: Some("success criteria met and reward issued".to_string()),
        };

        let bytes = Message::TaskAck(ack.clone()).to_bytes(true);
        let (_, decoded) = Message::from_bytes(&bytes).expect("task ack frame should parse");

        assert_eq!(decoded, Message::TaskAck(ack));
    }
}
