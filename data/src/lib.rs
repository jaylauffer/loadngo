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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    /// Simple, process-local id generator. Replace with deterministic hashing if needed.
    pub fn generate_id() -> Id {
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
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

    #[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// Minimal task comparison helpers (placeholder until full logic is ported).
pub mod task_compare {
    use super::entity::Entity;
    use super::value::Value;
    use std::collections::HashMap;

    #[derive(Debug, Default)]
    pub struct Diff {
        pub added: Vec<String>,
        pub removed: Vec<String>,
        pub changed: HashMap<String, (Value, Value)>,
    }

    pub fn compare_properties(lhs: &Entity, rhs: &Entity) -> Diff {
        let mut diff = Diff::default();
        for (k, v) in lhs.properties.iter() {
            match rhs.properties.get(k) {
                None => diff.removed.push(k.clone()),
                Some(rv) if rv != v => {
                    diff.changed.insert(k.clone(), (v.clone(), rv.clone()));
                }
                _ => {}
            }
        }
        for k in rhs.properties.keys() {
            if !lhs.properties.contains_key(k) {
                diff.added.push(k.clone());
            }
        }
        diff
    }
}

/// Minimal undo/redo scaffolding; extend as commands are ported.
pub mod undo {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub enum Command {
        AddEntity { id: u64 },
        RemoveEntity { id: u64 },
        UpdateProperty { id: u64, key: String },
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

        pub fn undo(&mut self) -> Option<Command> {
            if let Some(cmd) = self.past.pop() {
                self.future.push(cmd.clone());
                Some(cmd)
            } else {
                None
            }
        }

        pub fn redo(&mut self) -> Option<Command> {
            if let Some(cmd) = self.future.pop() {
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
        pub properties: HashMap<String, Value>,
    }

    impl Task {
        pub fn new(entity: Entity, name: impl Into<String>) -> Self {
            Self {
                entity,
                name: name.into(),
                description: None,
                parent: None,
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
