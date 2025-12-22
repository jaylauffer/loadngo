//! Core data models for the Rust port of loadngo Task.
//!
//! This is an incremental, idiomatic reimplementation of the C++ types in
//! `Task/Data/*`. It focuses on the shapes and identifiers needed by the Task
//! and Network layers; functionality will be expanded as more features are ported.

use std::time::{SystemTime, UNIX_EPOCH};

pub use hash::fnv1a;
pub use model_utils::generate_id;
pub use sync::{Discrepancy, Participant, Sync};
pub use types::{Atom, Duration, Id, Ip, TimeStamp};
pub mod action;
pub mod clipboard;
pub mod config;
pub mod crypto;
pub mod data_object;
pub mod file_manager;
pub mod listener;
pub mod service;

pub mod types {
    use serde::{Deserialize, Serialize};

    /// Core identifier types (mirrors `types.h`).
    pub type Id = u64;
    pub type TimeStamp = u64;
    pub type Duration = u64;
    pub type Atom = u64;

    /// Placeholder for IP representations; we currently normalize to strings.
    #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Ip(pub String);

    impl From<&str> for Ip {
        fn from(s: &str) -> Self {
            Ip(s.to_string())
        }
    }

    impl From<String> for Ip {
        fn from(s: String) -> Self {
            Ip(s)
        }
    }
}

pub mod value {
    use serde::{Deserialize, Serialize};

    /// Rough equivalent of `value_t` (string/boolean/u64).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", content = "value")]
    pub enum Value {
        Bool(bool),
        U64(u64),
        Str(String),
        Null,
    }

    impl Default for Value {
        fn default() -> Self {
            Value::Null
        }
    }

    impl From<bool> for Value {
        fn from(v: bool) -> Self {
            Value::Bool(v)
        }
    }

    impl From<u64> for Value {
        fn from(v: u64) -> Self {
            Value::U64(v)
        }
    }

    impl From<String> for Value {
        fn from(v: String) -> Self {
            Value::Str(v)
        }
    }

    impl From<&str> for Value {
        fn from(v: &str) -> Self {
            Value::Str(v.to_string())
        }
    }
}

/// Helpers that mirror bits of ModelUtils and util usage in the C++ code.
pub mod model_utils {
    use super::types::{Id, TimeStamp};
    use crate::hash::fnv1a;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    pub const UNITS_PER_HOUR: u64 = 36_000_000_000;
    pub const UNITS_PER_MINUTE: u64 = UNITS_PER_HOUR / 60;

    /// Generate an id using hostname, username, process-local counter, and timestamp.
    pub fn generate_id() -> Id {
        let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let host = env::var("COMPUTERNAME")
            .or_else(|_| env::var("HOSTNAME"))
            .unwrap_or_else(|_| "unknown-host".to_string());
        let user = env::var("USERNAME")
            .or_else(|_| env::var("USER"))
            .unwrap_or_else(|_| "unknown-user".to_string());
        let now = now_timestamp();

        let mut acc = fnv1a(host.as_bytes(), 0xcbf29ce484222325);
        acc = fnv1a(user.as_bytes(), acc);
        acc = fnv1a(&now.to_le_bytes(), acc);
        acc = fnv1a(&counter.to_le_bytes(), acc);
        acc
    }

    pub fn now_timestamp() -> TimeStamp {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    }
}

/// FNV-1a 64-bit hash, matching the usage in the C++ codebase.
pub mod hash {
    pub fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
        const FNV_PRIME: u64 = 1099511628211;
        let mut hash = seed;
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }
}

pub mod entity {
    use super::hash::fnv1a;
    use super::types::{Id, TimeStamp};
    use super::value::Value;
    use crate::generate_id;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    /// Minimal stand-in for the C++ Entity base class.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Entity {
        pub id: Id,
        pub origin_id: Id,
        pub owner: String,
        pub created: TimeStamp,
        pub machine_id: Option<Id>,
        pub user_id: Option<Id>,
        pub properties: HashMap<String, Value>,
        pub children: Vec<Id>,
    }

    impl Entity {
        pub fn new(id: Id, origin_id: Id, owner: impl Into<String>, created: TimeStamp) -> Self {
            Self {
                id,
                origin_id,
                owner: owner.into(),
                created,
                machine_id: None,
                user_id: None,
                properties: HashMap::new(),
                children: Vec::new(),
            }
        }

        pub fn with_machine_user(
            id: Id,
            origin_id: Id,
            owner: impl Into<String>,
            created: TimeStamp,
            machine_id: Id,
            user_id: Id,
        ) -> Self {
            Self {
                machine_id: Some(machine_id),
                user_id: Some(user_id),
                ..Self::new(id, origin_id, owner, created)
            }
        }

        /// Generate a new Entity with fresh ids (used when porting creation logic).
        pub fn spawn(
            owner: impl Into<String>,
            machine_id: Id,
            user_id: Id,
            created: TimeStamp,
        ) -> Self {
            let origin_id = generate_id();
            let id = generate_id();
            Self::with_machine_user(id, origin_id, owner, created, machine_id, user_id)
        }

        pub fn set_property(&mut self, key: impl Into<String>, value: Value) {
            self.properties.insert(key.into(), value);
        }

        pub fn get_property(&self, key: &str) -> Option<&Value> {
            self.properties.get(key)
        }

        pub fn add_child(&mut self, child: Id) {
            self.children.push(child);
        }

        pub fn property_hash(&self) -> u64 {
            let mut acc: u64 = 0xcbf29ce484222325;
            for (k, v) in self.properties.iter() {
                acc = fnv1a(k.as_bytes(), acc);
                let val_bytes_owned;
                let val_bytes = match v {
                    super::value::Value::Bool(b) => {
                        if *b {
                            &[1u8][..]
                        } else {
                            &[0u8][..]
                        }
                    }
                    super::value::Value::U64(n) => {
                        val_bytes_owned = n.to_le_bytes();
                        val_bytes_owned.as_slice()
                    }
                    super::value::Value::Str(s) => s.as_bytes(),
                    super::value::Value::Null => &[0u8][..],
                };
                acc = fnv1a(val_bytes, acc);
            }
            acc
        }

        /// Simple combined hash for the entity id, origin, and properties.
        pub fn hash(&self) -> u64 {
            let mut acc = fnv1a(&self.id.to_le_bytes(), 0xcbf29ce484222325);
            acc = fnv1a(&self.origin_id.to_le_bytes(), acc);
            acc = fnv1a(self.owner.as_bytes(), acc);
            acc = fnv1a(&self.created.to_le_bytes(), acc);
            acc ^ self.property_hash()
        }
    }
}

pub mod sync {
    use super::hash::fnv1a;
    use super::types::{Id, TimeStamp};
    use super::value::Value;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Participant {
        pub user_id: Id,
        pub machine_id: Id,
        pub ip: String,
        pub when: TimeStamp,
    }

    impl Participant {
        pub fn new<S: Into<String>>(user_id: Id, machine_id: Id, ip: S, when: TimeStamp) -> Self {
            Self {
                user_id,
                machine_id,
                ip: ip.into(),
                when,
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Discrepancy {
        pub sync_id: Id,
        pub entity_origin_id: Id,
        pub foreign_id: Id,
        pub local_id: Id,
        pub discrepancy: Id,
        pub description: Option<String>,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    pub struct MoveChain {
        pub moves: Vec<Id>,
    }

    impl MoveChain {
        pub fn add_move(&mut self, id: Id) {
            self.moves.push(id);
        }
    }

    #[derive(Debug, Default, Clone, Serialize, Deserialize)]
    pub struct Sync {
        pub sync_id: Id,
        pub consolidated_id: Option<Id>,
        pub participants: HashMap<String, Participant>, // keyed by ip string
        pub properties: HashMap<String, Value>,
    }

    impl Sync {
        pub fn new(sync_id: Id) -> Self {
            Self {
                sync_id,
                consolidated_id: None,
                participants: HashMap::new(),
                properties: HashMap::new(),
            }
        }

        pub fn add_participant(&mut self, participant: Participant) {
            self.participants
                .insert(participant.ip.clone(), participant);
        }

        pub fn get_participant(&self, ip: &str) -> Option<&Participant> {
            self.participants.get(ip)
        }

        pub fn set_property(&mut self, key: impl Into<String>, value: Value) {
            self.properties.insert(key.into(), value);
        }

        pub fn record_discrepancy(
            &mut self,
            origin: Id,
            foreign: Id,
            local: Id,
            code: Id,
        ) -> Discrepancy {
            Discrepancy {
                sync_id: self.sync_id,
                entity_origin_id: origin,
                foreign_id: foreign,
                local_id: local,
                discrepancy: code,
                description: None,
            }
        }

        pub fn build_move_chain(&self, ids: &[Id]) -> MoveChain {
            let mut chain = MoveChain::default();
            for id in ids {
                chain.add_move(*id);
            }
            chain
        }

        pub fn hash(&self) -> u64 {
            let mut acc = fnv1a(&self.sync_id.to_le_bytes(), 0xcbf29ce484222325);
            if let Some(cid) = self.consolidated_id {
                acc = fnv1a(&cid.to_le_bytes(), acc);
            }
            for (k, v) in self.properties.iter() {
                acc = fnv1a(k.as_bytes(), acc);
                let val_bytes_owned;
                let val_bytes = match v {
                    super::value::Value::Bool(b) => {
                        if *b {
                            &[1u8][..]
                        } else {
                            &[0u8][..]
                        }
                    }
                    super::value::Value::U64(n) => {
                        val_bytes_owned = n.to_le_bytes();
                        val_bytes_owned.as_slice()
                    }
                    super::value::Value::Str(s) => s.as_bytes(),
                    super::value::Value::Null => &[0u8][..],
                };
                acc = fnv1a(val_bytes, acc);
            }
            acc
        }
    }
}

/// Network message shapes (serde-serializable) that align with NetUtil.h.
pub mod netmsg {
    use super::types::Id;

    pub const PACKET_HDR: u32 = 0x6c6e6774;
    pub const HDR_LEN: usize = 16;

    #[repr(u32)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MessageType {
        Empty = 0,
        UserIntroduction,
        UserDeparture,
        RequestGroupTaskSynch,
        RequestUserTaskSynch,
        RequestSyncParticipants,
        RequestProperties,
        RequestTask,
        RequestMoveChain,
        ReportDiscrepancies,
        EntityInfo,
        EntityMove,
        EntityDelete,
        PropertyInfo,
        SuggestConsolidation,
        ConcludeSync,
        Chat,
        PrivateChat,
        TransferFileStart,
        TransferFileEnd,
        TransferFileData,
        TransferFileMissed,
        TransferBlobStart,
        TransferBlobEnd,
        TransferBlobData,
        TransferBlobMissed,
        TransferBlobComplete,
        RequestHashes,
        RequestTasks,
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
            buf[8..12].copy_from_slice(&(if self.is_response { 1u32 } else { 0u32 }).to_le_bytes());
            buf[12..16].copy_from_slice(&self.length.to_le_bytes());
            buf
        }

        pub fn from_bytes(buf: &[u8]) -> Option<Self> {
            if buf.len() < HDR_LEN {
                return None;
            }
            let tag = u32::from_le_bytes(buf[0..4].try_into().ok()?);
            let msg_type_val = u32::from_le_bytes(buf[4..8].try_into().ok()?);
            let is_response = u32::from_le_bytes(buf[8..12].try_into().ok()?) != 0;
            let length = u32::from_le_bytes(buf[12..16].try_into().ok()?);
            let msg_type = match msg_type_val {
                0 => MessageType::Empty,
                1 => MessageType::UserIntroduction,
                2 => MessageType::UserDeparture,
                3 => MessageType::RequestGroupTaskSynch,
                4 => MessageType::RequestUserTaskSynch,
                5 => MessageType::RequestSyncParticipants,
                6 => MessageType::RequestProperties,
                7 => MessageType::RequestTask,
                8 => MessageType::RequestMoveChain,
                9 => MessageType::ReportDiscrepancies,
                10 => MessageType::EntityInfo,
                11 => MessageType::EntityMove,
                12 => MessageType::EntityDelete,
                13 => MessageType::PropertyInfo,
                14 => MessageType::SuggestConsolidation,
                15 => MessageType::ConcludeSync,
                16 => MessageType::Chat,
                17 => MessageType::PrivateChat,
                18 => MessageType::TransferFileStart,
                19 => MessageType::TransferFileEnd,
                20 => MessageType::TransferFileData,
                21 => MessageType::TransferFileMissed,
                22 => MessageType::TransferBlobStart,
                23 => MessageType::TransferBlobEnd,
                24 => MessageType::TransferBlobData,
                25 => MessageType::TransferBlobMissed,
                26 => MessageType::TransferBlobComplete,
                27 => MessageType::RequestHashes,
                28 => MessageType::RequestTasks,
                29 => MessageType::MessageCount,
                _ => return None,
            };
            Some(Self {
                tag,
                msg_type,
                is_response,
                length,
            })
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct UserIntroduction {
        pub machine_id: Id,
        pub user_key: [u8; 64],
        pub name: String,
        pub device: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct UserDeparture {
        pub machine_id: Id,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SyncParticipants {
        pub sync_time: Id,
        pub user_id: Id,
        pub machine_id: Id,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct GroupTaskSynch {
        pub task_ids: Vec<Id>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct UserTaskSynch {
        pub since: Id,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct EntityPayload {
        pub origin_id: Id,
        pub sync_id: Id,
        pub doid: Id,
        pub data: Vec<u8>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SuggestConsolidation {
        pub sync_id: Id,
        pub consolidated_id: Id,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Discrepancy {
        pub sync_id: Id,
        pub origin_id: Id,
        pub foreign_id: Id,
        pub local_id: Id,
        pub discrepancy: Id,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DiscrepanciesReport {
        pub sync_id: Id,
        pub discrepancies: Vec<Discrepancy>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FileStart {
        pub time: u64,
        pub filesize: u64,
        pub filename: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FileEnd {
        pub time: u64,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FileData {
        pub time: u64,
        pub seq: u32,
        pub data: Vec<u8>,
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
    pub struct BlobEnd {
        pub time: u64,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BlobData {
        pub time: u64,
        pub seq: u32,
        pub data: Vec<u8>,
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
    pub struct BulkIds {
        pub ids: Vec<Id>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Message {
        Empty,
        UserIntroduction(UserIntroduction),
        UserDeparture(UserDeparture),
        RequestGroupTaskSynch(GroupTaskSynch),
        RequestUserTaskSynch(UserTaskSynch),
        RequestSyncParticipants(SyncParticipants),
        RequestProperties(BulkIds),
        RequestTask(BulkIds),
        RequestMoveChain(EntityPayload),
        ReportDiscrepancies(DiscrepanciesReport),
        EntityInfo(EntityPayload),
        EntityMove(EntityPayload),
        EntityDelete(EntityPayload),
        PropertyInfo(EntityPayload),
        SuggestConsolidation(SuggestConsolidation),
        ConcludeSync { sync_id: Id, consolidated_id: Id },
        Chat(Vec<u8>),
        PrivateChat(Vec<u8>),
        TransferFileStart(FileStart),
        TransferFileEnd(FileEnd),
        TransferFileData(FileData),
        TransferFileMissed(FileMissed),
        TransferBlobStart(BlobStart),
        TransferBlobEnd(BlobEnd),
        TransferBlobData(BlobData),
        TransferBlobMissed(BlobMissed),
        TransferBlobComplete(BlobComplete),
        RequestHashes(BulkIds),
        RequestTasks(BulkIds),
    }

    impl Message {
        pub fn to_bytes(&self, msg_type: MessageType, is_response: bool) -> Vec<u8> {
            let payload: Vec<u8> = match msg_type {
                MessageType::Empty => Vec::new(),
                MessageType::UserIntroduction => {
                    if let Message::UserIntroduction(body) = self {
                        let mut buf =
                            Vec::with_capacity(8 + 64 + body.name.len() + 1 + body.device.len());
                        buf.extend_from_slice(&body.machine_id.to_le_bytes());
                        buf.extend_from_slice(&body.user_key);
                        buf.extend_from_slice(body.name.as_bytes());
                        buf.push(0);
                        buf.extend_from_slice(body.device.as_bytes());
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::UserDeparture => {
                    if let Message::UserDeparture(body) = self {
                        body.machine_id.to_le_bytes().to_vec()
                    } else {
                        Vec::new()
                    }
                }
                MessageType::RequestGroupTaskSynch => {
                    if let Message::RequestGroupTaskSynch(body) = self {
                        let mut buf = Vec::with_capacity(body.task_ids.len() * 8);
                        for id in &body.task_ids {
                            buf.extend_from_slice(&id.to_le_bytes());
                        }
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::RequestUserTaskSynch => {
                    if let Message::RequestUserTaskSynch(body) = self {
                        body.since.to_le_bytes().to_vec()
                    } else {
                        Vec::new()
                    }
                }
                MessageType::RequestSyncParticipants => {
                    if let Message::RequestSyncParticipants(body) = self {
                        let mut buf = Vec::with_capacity(24);
                        buf.extend_from_slice(&body.sync_time.to_le_bytes());
                        buf.extend_from_slice(&body.user_id.to_le_bytes());
                        buf.extend_from_slice(&body.machine_id.to_le_bytes());
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::RequestProperties => {
                    if let Message::RequestProperties(ids) = self {
                        pack_ids(&ids.ids)
                    } else {
                        Vec::new()
                    }
                }
                MessageType::RequestTask => {
                    if let Message::RequestTask(ids) = self {
                        pack_ids(&ids.ids)
                    } else {
                        Vec::new()
                    }
                }
                MessageType::RequestMoveChain => {
                    if let Message::RequestMoveChain(body) = self {
                        pack_entity(body)
                    } else {
                        Vec::new()
                    }
                }
                MessageType::ReportDiscrepancies => {
                    if let Message::ReportDiscrepancies(report) = self {
                        let mut buf = Vec::with_capacity(8 + report.discrepancies.len() * 40);
                        buf.extend_from_slice(&report.sync_id.to_le_bytes());
                        for d in &report.discrepancies {
                            buf.extend_from_slice(&d.sync_id.to_le_bytes());
                            buf.extend_from_slice(&d.origin_id.to_le_bytes());
                            buf.extend_from_slice(&d.foreign_id.to_le_bytes());
                            buf.extend_from_slice(&d.local_id.to_le_bytes());
                            buf.extend_from_slice(&d.discrepancy.to_le_bytes());
                        }
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::EntityInfo => {
                    if let Message::EntityInfo(body) = self {
                        pack_entity(body)
                    } else {
                        Vec::new()
                    }
                }
                MessageType::EntityMove => {
                    if let Message::EntityMove(body) = self {
                        pack_entity(body)
                    } else {
                        Vec::new()
                    }
                }
                MessageType::EntityDelete => {
                    if let Message::EntityDelete(body) = self {
                        pack_entity(body)
                    } else {
                        Vec::new()
                    }
                }
                MessageType::PropertyInfo => {
                    if let Message::PropertyInfo(body) = self {
                        pack_entity(body)
                    } else {
                        Vec::new()
                    }
                }
                MessageType::SuggestConsolidation => {
                    if let Message::SuggestConsolidation(body) = self {
                        let mut buf = Vec::with_capacity(16);
                        buf.extend_from_slice(&body.sync_id.to_le_bytes());
                        buf.extend_from_slice(&body.consolidated_id.to_le_bytes());
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::ConcludeSync => {
                    if let Message::ConcludeSync {
                        sync_id,
                        consolidated_id,
                    } = self
                    {
                        let mut buf = Vec::with_capacity(16);
                        buf.extend_from_slice(&sync_id.to_le_bytes());
                        buf.extend_from_slice(&consolidated_id.to_le_bytes());
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::Chat => {
                    if let Message::Chat(text) = self {
                        text.clone()
                    } else {
                        Vec::new()
                    }
                }
                MessageType::PrivateChat => {
                    if let Message::PrivateChat(text) = self {
                        text.clone()
                    } else {
                        Vec::new()
                    }
                }
                MessageType::TransferFileStart => {
                    if let Message::TransferFileStart(body) = self {
                        let mut buf = Vec::with_capacity(16 + body.filename.len());
                        buf.extend_from_slice(&body.time.to_le_bytes());
                        buf.extend_from_slice(&body.filesize.to_le_bytes());
                        buf.extend_from_slice(body.filename.as_bytes());
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::TransferFileEnd => {
                    if let Message::TransferFileEnd(body) = self {
                        body.time.to_le_bytes().to_vec()
                    } else {
                        Vec::new()
                    }
                }
                MessageType::TransferFileData => {
                    if let Message::TransferFileData(body) = self {
                        let mut buf = Vec::with_capacity(12 + body.data.len());
                        buf.extend_from_slice(&body.time.to_le_bytes());
                        buf.extend_from_slice(&body.seq.to_le_bytes());
                        buf.extend_from_slice(&body.data);
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::TransferFileMissed => {
                    if let Message::TransferFileMissed(body) = self {
                        let mut buf = Vec::with_capacity(12);
                        buf.extend_from_slice(&body.time.to_le_bytes());
                        buf.extend_from_slice(&body.seq.to_le_bytes());
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::TransferBlobStart => {
                    if let Message::TransferBlobStart(body) = self {
                        let mut buf = Vec::with_capacity(12);
                        buf.extend_from_slice(&body.time.to_le_bytes());
                        buf.extend_from_slice(&body.len.to_le_bytes());
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::TransferBlobEnd => {
                    if let Message::TransferBlobEnd(body) = self {
                        body.time.to_le_bytes().to_vec()
                    } else {
                        Vec::new()
                    }
                }
                MessageType::TransferBlobData => {
                    if let Message::TransferBlobData(body) = self {
                        let mut buf = Vec::with_capacity(12 + body.data.len());
                        buf.extend_from_slice(&body.time.to_le_bytes());
                        buf.extend_from_slice(&body.seq.to_le_bytes());
                        buf.extend_from_slice(&body.data);
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::TransferBlobMissed => {
                    if let Message::TransferBlobMissed(body) = self {
                        let mut buf = Vec::with_capacity(12);
                        buf.extend_from_slice(&body.time.to_le_bytes());
                        buf.extend_from_slice(&body.seq.to_le_bytes());
                        buf
                    } else {
                        Vec::new()
                    }
                }
                MessageType::TransferBlobComplete => {
                    if let Message::TransferBlobComplete(body) = self {
                        body.time.to_le_bytes().to_vec()
                    } else {
                        Vec::new()
                    }
                }
                MessageType::RequestHashes => {
                    if let Message::RequestHashes(ids) = self {
                        pack_ids(&ids.ids)
                    } else {
                        Vec::new()
                    }
                }
                MessageType::RequestTasks => {
                    if let Message::RequestTasks(ids) = self {
                        pack_ids(&ids.ids)
                    } else {
                        Vec::new()
                    }
                }
                MessageType::MessageCount => Vec::new(),
            };
            let header = Header::new(msg_type, is_response, payload.len() as u32);
            let mut buf = header.to_bytes().to_vec();
            buf.extend_from_slice(&payload);
            buf
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
                    if body_len < 8 + 64 + 1 {
                        return None;
                    }
                    let machine_id = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    let mut key = [0u8; 64];
                    key.copy_from_slice(&body[8..72]);
                    let rest = &body[72..];
                    let split = rest.iter().position(|b| *b == 0)?;
                    let name = String::from_utf8_lossy(&rest[..split]).into_owned();
                    let device = String::from_utf8_lossy(&rest[split + 1..]).into_owned();
                    Message::UserIntroduction(UserIntroduction {
                        machine_id,
                        user_key: key,
                        name,
                        device,
                    })
                }
                MessageType::UserDeparture => {
                    if body_len < 8 {
                        return None;
                    }
                    let machine_id = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    Message::UserDeparture(UserDeparture { machine_id })
                }
                MessageType::RequestGroupTaskSynch => {
                    let ids = parse_bulk_ids(body)?;
                    Message::RequestGroupTaskSynch(GroupTaskSynch { task_ids: ids })
                }
                MessageType::RequestUserTaskSynch => {
                    if body_len < 8 {
                        return None;
                    }
                    let since = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    Message::RequestUserTaskSynch(UserTaskSynch { since })
                }
                MessageType::RequestSyncParticipants => {
                    if body_len < 24 {
                        return None;
                    }
                    let sync_time = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    let user_id = Id::from_le_bytes(body[8..16].try_into().ok()?);
                    let machine_id = Id::from_le_bytes(body[16..24].try_into().ok()?);
                    Message::RequestSyncParticipants(SyncParticipants {
                        sync_time,
                        user_id,
                        machine_id,
                    })
                }
                MessageType::RequestProperties => {
                    let ids = parse_bulk_ids(body)?;
                    Message::RequestProperties(BulkIds { ids })
                }
                MessageType::RequestTask => {
                    let ids = parse_bulk_ids(body)?;
                    Message::RequestTask(BulkIds { ids })
                }
                MessageType::RequestMoveChain => {
                    let payload = parse_entity(body)?;
                    Message::RequestMoveChain(payload)
                }
                MessageType::ReportDiscrepancies => {
                    if body_len < 8 {
                        return None;
                    }
                    let sync_id = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    let rest = &body[8..];
                    if rest.len() % 40 != 0 {
                        return None;
                    }
                    let mut discrepancies = Vec::new();
                    for chunk in rest.chunks(40) {
                        let syncid = Id::from_le_bytes(chunk[0..8].try_into().ok()?);
                        let origin_id = Id::from_le_bytes(chunk[8..16].try_into().ok()?);
                        let foreign_id = Id::from_le_bytes(chunk[16..24].try_into().ok()?);
                        let local_id = Id::from_le_bytes(chunk[24..32].try_into().ok()?);
                        let discrepancy = Id::from_le_bytes(chunk[32..40].try_into().ok()?);
                        discrepancies.push(Discrepancy {
                            sync_id: syncid,
                            origin_id,
                            foreign_id,
                            local_id,
                            discrepancy,
                        });
                    }
                    Message::ReportDiscrepancies(DiscrepanciesReport {
                        sync_id,
                        discrepancies,
                    })
                }
                MessageType::EntityInfo => Message::EntityInfo(parse_entity(body)?),
                MessageType::EntityMove => Message::EntityMove(parse_entity(body)?),
                MessageType::EntityDelete => Message::EntityDelete(parse_entity(body)?),
                MessageType::PropertyInfo => Message::PropertyInfo(parse_entity(body)?),
                MessageType::SuggestConsolidation => {
                    if body_len < 16 {
                        return None;
                    }
                    let sync_id = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    let consolidated_id = Id::from_le_bytes(body[8..16].try_into().ok()?);
                    Message::SuggestConsolidation(SuggestConsolidation {
                        sync_id,
                        consolidated_id,
                    })
                }
                MessageType::ConcludeSync => {
                    if body_len < 16 {
                        return None;
                    }
                    let sync_id = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    let consolidated_id = Id::from_le_bytes(body[8..16].try_into().ok()?);
                    Message::ConcludeSync {
                        sync_id,
                        consolidated_id,
                    }
                }
                MessageType::Chat => Message::Chat(body.to_vec()),
                MessageType::PrivateChat => Message::PrivateChat(body.to_vec()),
                MessageType::TransferFileStart => {
                    if body_len < 16 {
                        return None;
                    }
                    let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                    let filesize = u64::from_le_bytes(body[8..16].try_into().ok()?);
                    let filename = String::from_utf8_lossy(&body[16..]).into_owned();
                    Message::TransferFileStart(FileStart {
                        time,
                        filesize,
                        filename,
                    })
                }
                MessageType::TransferFileEnd => {
                    if body_len < 8 {
                        return None;
                    }
                    let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                    Message::TransferFileEnd(FileEnd { time })
                }
                MessageType::TransferFileData => {
                    if body_len < 12 {
                        return None;
                    }
                    let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                    let seq = u32::from_le_bytes(body[8..12].try_into().ok()?);
                    let data = body[12..].to_vec();
                    Message::TransferFileData(FileData { time, seq, data })
                }
                MessageType::TransferFileMissed => {
                    if body_len < 12 {
                        return None;
                    }
                    let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                    let seq = u32::from_le_bytes(body[8..12].try_into().ok()?);
                    Message::TransferFileMissed(FileMissed { time, seq })
                }
                MessageType::TransferBlobStart => {
                    if body_len < 12 {
                        return None;
                    }
                    let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                    let len = i32::from_le_bytes(body[8..12].try_into().ok()?);
                    Message::TransferBlobStart(BlobStart { time, len })
                }
                MessageType::TransferBlobEnd => {
                    if body_len < 8 {
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
                    let data = body[12..].to_vec();
                    Message::TransferBlobData(BlobData { time, seq, data })
                }
                MessageType::TransferBlobMissed => {
                    if body_len < 12 {
                        return None;
                    }
                    let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                    let seq = u32::from_le_bytes(body[8..12].try_into().ok()?);
                    Message::TransferBlobMissed(BlobMissed { time, seq })
                }
                MessageType::TransferBlobComplete => {
                    if body_len < 8 {
                        return None;
                    }
                    let time = u64::from_le_bytes(body[0..8].try_into().ok()?);
                    Message::TransferBlobComplete(BlobComplete { time })
                }
                MessageType::RequestHashes => {
                    let ids = parse_bulk_ids(body)?;
                    Message::RequestHashes(BulkIds { ids })
                }
                MessageType::RequestTasks => {
                    let ids = parse_bulk_ids(body)?;
                    Message::RequestTasks(BulkIds { ids })
                }
                MessageType::MessageCount => return None,
            };
            Some((header, message))
        }
    }

    fn pack_ids(ids: &[Id]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(ids.len() * 8);
        for id in ids {
            buf.extend_from_slice(&id.to_le_bytes());
        }
        buf
    }

    fn parse_bulk_ids(body: &[u8]) -> Option<Vec<Id>> {
        if body.len() % 8 != 0 {
            return None;
        }
        let mut ids = Vec::with_capacity(body.len() / 8);
        for chunk in body.chunks(8) {
            ids.push(Id::from_le_bytes(chunk.try_into().ok()?));
        }
        Some(ids)
    }

    fn pack_entity(body: &EntityPayload) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24 + body.data.len());
        buf.extend_from_slice(&body.origin_id.to_le_bytes());
        buf.extend_from_slice(&body.sync_id.to_le_bytes());
        buf.extend_from_slice(&body.doid.to_le_bytes());
        buf.extend_from_slice(&body.data);
        buf
    }

    fn parse_entity(body: &[u8]) -> Option<EntityPayload> {
        if body.len() < 24 {
            return None;
        }
        let origin_id = Id::from_le_bytes(body[0..8].try_into().ok()?);
        let sync_id = Id::from_le_bytes(body[8..16].try_into().ok()?);
        let doid = Id::from_le_bytes(body[16..24].try_into().ok()?);
        let data = body[24..].to_vec();
        Some(EntityPayload {
            origin_id,
            sync_id,
            doid,
            data,
        })
    }
}

/// Minimal task comparison helpers (placeholder until full logic is ported).
pub mod task_compare {
    use super::hash::fnv1a;
    use super::task::Task;
    use std::cmp::Ordering;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SortField {
        TaskId,
        DueDate,
        Priority,
        CreateDate,
        EstimatedDuration,
        StartDate,
        TaskTitle,
    }

    impl Default for SortField {
        fn default() -> Self {
            SortField::DueDate
        }
    }

    #[derive(Debug, Clone)]
    pub struct TaskComparator {
        fields: [SortField; 3],
    }

    impl Default for TaskComparator {
        fn default() -> Self {
            Self {
                fields: [
                    SortField::DueDate,
                    SortField::Priority,
                    SortField::StartDate,
                ],
            }
        }
    }

    impl TaskComparator {
        pub fn set_sort_field(&mut self, pos: usize, field: SortField) {
            if pos < self.fields.len() {
                self.fields[pos] = field;
            }
        }

        pub fn compare(&self, lhs: &Task, rhs: &Task) -> Ordering {
            for field in self.fields.iter() {
                let ord = match field {
                    SortField::TaskId => compare_u64(lhs.entity.id, rhs.entity.id),
                    SortField::DueDate => compare_u64(lhs.due_date, rhs.due_date),
                    SortField::Priority => lhs.priority.cmp(&rhs.priority),
                    SortField::CreateDate => compare_u64(lhs.entity.created, rhs.entity.created),
                    SortField::EstimatedDuration => {
                        compare_u64(lhs.estimated_duration, rhs.estimated_duration)
                    }
                    SortField::StartDate => compare_u64(lhs.scheduled_start, rhs.scheduled_start),
                    SortField::TaskTitle => compare_str(
                        lhs.properties
                            .get("title")
                            .and_then(as_str)
                            .unwrap_or(&lhs.name),
                        rhs.properties
                            .get("title")
                            .and_then(as_str)
                            .unwrap_or(&rhs.name),
                    ),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            // fallback to hash to avoid equality
            compare_u64(task_hash(lhs), task_hash(rhs))
        }
    }

    fn compare_u64(a: u64, b: u64) -> Ordering {
        if a == b {
            Ordering::Equal
        } else if a == 0 {
            Ordering::Greater
        } else if b == 0 {
            Ordering::Less
        } else if a > b {
            Ordering::Greater
        } else {
            Ordering::Less
        }
    }

    fn compare_str(a: &str, b: &str) -> Ordering {
        a.cmp(b)
    }

    fn as_str(v: &super::value::Value) -> Option<&str> {
        if let super::value::Value::Str(s) = v {
            Some(s.as_str())
        } else {
            None
        }
    }

    fn task_hash(task: &Task) -> u64 {
        let mut acc = fnv1a(&task.entity.id.to_le_bytes(), 0xcbf29ce484222325);
        acc ^= task.entity.hash();
        acc
    }
}

/// Minimal undo/redo scaffolding; extend as commands are ported.
pub mod undo {
    use crate::task::Task;
    use crate::value::Value;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub enum Command {
        AddTask {
            task: Task,
        },
        RemoveTask {
            task: Task,
        },
        UpdateProperty {
            id: u64,
            key: String,
            old: Value,
            new: Value,
        },
    }

    #[derive(Debug, Default, Clone, Serialize, Deserialize)]
    pub struct UndoStack {
        pub past: Vec<Command>,
        pub future: Vec<Command>,
    }

    impl UndoStack {
        fn apply_cmd(cmd: &Command, tasks: &mut HashMap<u64, Task>) {
            match cmd {
                Command::AddTask { task } => {
                    tasks.insert(task.entity.id, task.clone());
                }
                Command::RemoveTask { task } => {
                    tasks.remove(&task.entity.id);
                }
                Command::UpdateProperty { id, key, new, .. } => {
                    if let Some(task) = tasks.get_mut(id) {
                        task.properties.insert(key.clone(), new.clone());
                    }
                }
            }
        }

        pub fn apply(&mut self, cmd: Command, tasks: &mut HashMap<u64, Task>) {
            Self::apply_cmd(&cmd, tasks);
            self.past.push(cmd);
            self.future.clear();
        }

        pub fn undo(&mut self, tasks: &mut HashMap<u64, Task>) -> Option<Command> {
            if let Some(cmd) = self.past.pop() {
                match &cmd {
                    Command::AddTask { task } => {
                        tasks.remove(&task.entity.id);
                    }
                    Command::RemoveTask { task } => {
                        tasks.insert(task.entity.id, task.clone());
                    }
                    Command::UpdateProperty { id, key, old, .. } => {
                        if let Some(task) = tasks.get_mut(id) {
                            task.properties.insert(key.clone(), old.clone());
                        }
                    }
                }
                self.future.push(cmd.clone());
                Some(cmd)
            } else {
                None
            }
        }

        pub fn redo(&mut self, tasks: &mut HashMap<u64, Task>) -> Option<Command> {
            if let Some(cmd) = self.future.pop() {
                Self::apply_cmd(&cmd, tasks);
                self.past.push(cmd.clone());
                Some(cmd)
            } else {
                None
            }
        }
    }
}

pub mod user {
    use super::entity::Entity;
    use super::types::Id;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct User {
        pub entity: Entity,
        pub display_name: String,
    }

    impl User {
        pub fn new(entity: Entity, display_name: impl Into<String>) -> Self {
            Self {
                entity,
                display_name: display_name.into(),
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct MachineId {
        pub id: Id,
        pub description: String,
    }
}

pub mod task {
    use super::entity::Entity;
    use super::model_utils::generate_id;
    use super::types::Id;
    use super::value::Value;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Task {
        pub entity: Entity,
        pub name: String,
        pub description: Option<String>,
        pub parent: Option<Id>,
        pub priority: i32,
        pub due_date: u64,
        pub scheduled_start: u64,
        pub estimated_duration: u64,
        pub properties: HashMap<String, Value>,
    }

    impl Task {
        pub fn new(entity: Entity, name: impl Into<String>) -> Self {
            Self {
                entity,
                name: name.into(),
                description: None,
                parent: None,
                priority: 0,
                due_date: 0,
                scheduled_start: 0,
                estimated_duration: 0,
                properties: HashMap::new(),
            }
        }

        /// Convenience helper for creating a bare task with generated ids.
        pub fn spawn(
            name: impl Into<String>,
            owner: impl Into<String>,
            machine_id: Id,
            user_id: Id,
            created: u64,
        ) -> Self {
            let entity = Entity::with_machine_user(
                generate_id(),
                generate_id(),
                owner,
                created,
                machine_id,
                user_id,
            );
            Task::new(entity, name)
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TimeEntry {
        pub entity: Entity,
        pub task_id: Id,
        pub duration: u64,
        pub start: u64,
        pub stop: u64,
        pub title: String,
        pub notes: Option<String>,
    }
}

/// JSON persistence helpers for task files (deterministic ordering).
pub mod persistence {
    use super::model_utils::now_timestamp;
    use super::task::Task;
    use super::types::{Id, TimeStamp};
    use super::value::Value;
    use anyhow::Result;
    use serde::{Deserialize, Serialize};
    use std::collections::{BTreeMap, HashMap};
    use std::fs::File;
    use std::path::Path;

    #[derive(Debug, Serialize, Deserialize)]
    pub struct TaskFile {
        pub version: u32,
        pub generated_at: TimeStamp,
        pub tasks: Vec<SerdeTask>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SerdeTask {
        pub entity: super::entity::Entity,
        pub name: String,
        pub description: Option<String>,
        pub parent: Option<Id>,
        pub priority: i32,
        pub due_date: u64,
        pub scheduled_start: u64,
        pub estimated_duration: u64,
        pub properties: BTreeMap<String, Value>,
    }

    impl From<&Task> for SerdeTask {
        fn from(t: &Task) -> Self {
            let mut props = BTreeMap::new();
            for (k, v) in t.properties.iter() {
                props.insert(k.clone(), v.clone());
            }
            SerdeTask {
                entity: t.entity.clone(),
                name: t.name.clone(),
                description: t.description.clone(),
                parent: t.parent,
                priority: t.priority,
                due_date: t.due_date,
                scheduled_start: t.scheduled_start,
                estimated_duration: t.estimated_duration,
                properties: props,
            }
        }
    }

    impl From<SerdeTask> for Task {
        fn from(t: SerdeTask) -> Self {
            let mut props = HashMap::new();
            for (k, v) in t.properties.into_iter() {
                props.insert(k, v);
            }
            Task {
                entity: t.entity,
                name: t.name,
                description: t.description,
                parent: t.parent,
                priority: t.priority,
                due_date: t.due_date,
                scheduled_start: t.scheduled_start,
                estimated_duration: t.estimated_duration,
                properties: props,
            }
        }
    }

    /// Write tasks to a deterministic JSON file (sorted by id/name, properties sorted by key).
    pub fn write_task_file(path: impl AsRef<Path>, tasks: &[Task]) -> Result<()> {
        let json = tasks_to_json(tasks)?;
        let writer = File::create(path)?;
        serde_json::to_writer_pretty(writer, &json)?;
        Ok(())
    }

    /// Read tasks from a JSON file written by `write_task_file`.
    pub fn read_task_file(path: impl AsRef<Path>) -> Result<Vec<Task>> {
        let reader = File::open(path)?;
        let json: TaskFile = serde_json::from_reader(reader)?;
        let mut tasks: Vec<Task> = json.tasks.into_iter().map(Task::from).collect();
        tasks.sort_by(|a, b| a.entity.id.cmp(&b.entity.id).then(a.name.cmp(&b.name)));
        Ok(tasks)
    }

    /// Convert tasks into deterministic JSON payload (sorted by id/name/properties).
    pub fn tasks_to_json(tasks: &[Task]) -> Result<TaskFile> {
        let mut ordered: Vec<SerdeTask> = tasks.iter().map(SerdeTask::from).collect();
        ordered.sort_by(|a, b| a.entity.id.cmp(&b.entity.id).then(a.name.cmp(&b.name)));
        Ok(TaskFile {
            version: 1,
            generated_at: now_timestamp(),
            tasks: ordered,
        })
    }

    /// Convenience: produce a pretty JSON string for clipboard/export.
    pub fn tasks_to_json_string(tasks: &[Task]) -> Result<String> {
        let json = tasks_to_json(tasks)?;
        Ok(serde_json::to_string_pretty(&json)?)
    }

    /// Parse tasks from a JSON string.
    pub fn tasks_from_json_str(s: &str) -> Result<Vec<Task>> {
        let json: TaskFile = serde_json::from_str(s)?;
        let mut tasks: Vec<Task> = json.tasks.into_iter().map(Task::from).collect();
        tasks.sort_by(|a, b| a.entity.id.cmp(&b.entity.id).then(a.name.cmp(&b.name)));
        Ok(tasks)
    }
}

pub mod annotation {
    use super::entity::Entity;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Annotation {
        pub entity: Entity,
        pub text: String,
    }
}

pub fn now_timestamp() -> types::TimeStamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
