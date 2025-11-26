//! Core data models for the Rust port of loadngo Task.
//!
//! This is an incremental, idiomatic reimplementation of the C++ types in
//! `Task/Data/*`. It focuses on the shapes and identifiers needed by the Task
//! and Network layers; functionality will be expanded as more features are ported.

use std::time::{SystemTime, UNIX_EPOCH};

pub use hash::fnv1a;
pub use model_utils::generate_id;
pub use types::{Atom, Duration, Id, Ip, TimeStamp};
pub use sync::{Discrepancy, Participant, Sync};

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
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use crate::generate_id;

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
    use serde::{Deserialize, Serialize};
    use super::types::Id;

    pub const PACKET_HDR: u32 = 0x6c6e6774;
    pub const HDR_LEN: usize = 16;

    #[repr(u32)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
        pub is_response: u32,
        pub length: u32,
    }

    impl Header {
        pub fn new(msg_type: MessageType, is_response: bool, length: u32) -> Self {
            Self {
                tag: PACKET_HDR,
                msg_type,
                is_response: if is_response { 1 } else { 0 },
                length,
            }
        }

        pub fn to_bytes(self) -> [u8; HDR_LEN] {
            let mut buf = [0u8; HDR_LEN];
            buf[0..4].copy_from_slice(&self.tag.to_le_bytes());
            buf[4..8].copy_from_slice(&(self.msg_type as u32).to_le_bytes());
            buf[8..12].copy_from_slice(&self.is_response.to_le_bytes());
            buf[12..16].copy_from_slice(&self.length.to_le_bytes());
            buf
        }

        pub fn from_bytes(buf: &[u8]) -> Option<Self> {
            if buf.len() < HDR_LEN {
                return None;
            }
            let tag = u32::from_le_bytes(buf[0..4].try_into().ok()?);
            let msg_type_val = u32::from_le_bytes(buf[4..8].try_into().ok()?);
            let is_response = u32::from_le_bytes(buf[8..12].try_into().ok()?);
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

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SyncRequest {
        pub sync_id: Id,
        pub is_response: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct EntityRequest {
        pub origin_id: Id,
        pub sync_id: Id,
        pub doid: Id,
        pub is_response: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SuggestConsolidation {
        pub sync_id: Id,
        pub consolidated_id: Id,
        pub is_response: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub enum Message {
        Sync(SyncRequest),
        Entity(EntityRequest),
        Properties(EntityRequest),
        Move(EntityRequest),
        Delete(EntityRequest),
        Suggest(SuggestConsolidation),
    }

    impl Message {
        pub fn to_bytes(&self, msg_type: MessageType) -> Vec<u8> {
            let mut payload = Vec::new();
            match self {
                Message::Sync(s) => {
                    payload.extend_from_slice(&s.sync_id.to_le_bytes());
                    payload.push(if s.is_response { 1 } else { 0 });
                }
                Message::Entity(e)
                | Message::Properties(e)
                | Message::Move(e)
                | Message::Delete(e) => {
                    payload.extend_from_slice(&e.origin_id.to_le_bytes());
                    payload.extend_from_slice(&e.sync_id.to_le_bytes());
                    payload.extend_from_slice(&e.doid.to_le_bytes());
                    payload.push(if e.is_response { 1 } else { 0 });
                }
                Message::Suggest(s) => {
                    payload.extend_from_slice(&s.sync_id.to_le_bytes());
                    payload.extend_from_slice(&s.consolidated_id.to_le_bytes());
                    payload.push(if s.is_response { 1 } else { 0 });
                }
            }
            let header = Header::new(msg_type, matches!(self, Message::Sync(SyncRequest { is_response: true, .. }) | Message::Entity(EntityRequest { is_response: true, .. }) | Message::Properties(EntityRequest { is_response: true, .. }) | Message::Move(EntityRequest { is_response: true, .. }) | Message::Delete(EntityRequest { is_response: true, .. }) | Message::Suggest(SuggestConsolidation { is_response: true, .. })), (HDR_LEN + payload.len()) as u32);
            let mut buf = header.to_bytes().to_vec();
            buf.extend_from_slice(&payload);
            buf
        }

        pub fn from_bytes(buf: &[u8], msg_type: MessageType) -> Option<Self> {
            if buf.len() < HDR_LEN {
                return None;
            }
            let body = &buf[HDR_LEN..];
            match msg_type {
                MessageType::RequestSyncParticipants => {
                    if body.len() < 9 {
                        return None;
                    }
                    let sync_id = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    let is_response = body[8] != 0;
                    Some(Message::Sync(SyncRequest { sync_id, is_response }))
                }
                MessageType::EntityInfo
                | MessageType::EntityMove
                | MessageType::EntityDelete
                | MessageType::PropertyInfo => {
                    if body.len() < 25 {
                        return None;
                    }
                    let origin_id = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    let sync_id = Id::from_le_bytes(body[8..16].try_into().ok()?);
                    let doid = Id::from_le_bytes(body[16..24].try_into().ok()?);
                    let is_response = body[24] != 0;
                    Some(Message::Entity(EntityRequest {
                        origin_id,
                        sync_id,
                        doid,
                        is_response,
                    }))
                }
                MessageType::SuggestConsolidation => {
                    if body.len() < 17 {
                        return None;
                    }
                    let sync_id = Id::from_le_bytes(body[0..8].try_into().ok()?);
                    let consolidated_id = Id::from_le_bytes(body[8..16].try_into().ok()?);
                    let is_response = body[16] != 0;
                    Some(Message::Suggest(SuggestConsolidation {
                        sync_id,
                        consolidated_id,
                        is_response,
                    }))
                }
                _ => None,
            }
        }
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
                fields: [SortField::DueDate, SortField::Priority, SortField::StartDate],
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
                    SortField::EstimatedDuration => compare_u64(lhs.estimated_duration, rhs.estimated_duration),
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
        AddTask { task: Task },
        RemoveTask { task: Task },
        UpdateProperty { id: u64, key: String, old: Value, new: Value },
    }

    #[derive(Debug, Default, Clone, Serialize, Deserialize)]
    pub struct UndoStack {
        pub past: Vec<Command>,
        pub future: Vec<Command>,
    }

    impl UndoStack {
        pub fn push(&mut self, cmd: Command) {
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
                match &cmd {
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
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TimeEntry {
        pub entity: Entity,
        pub task_id: Id,
        pub duration: u64,
        pub notes: Option<String>,
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
