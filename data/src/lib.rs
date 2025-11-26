//! Core data models for the Rust port of loadngo.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub type Id = u64;
pub type TimeStamp = u64;

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

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Sync {
    pub consolidated_id: Option<Id>,
    pub participants: Vec<Participant>,
}

impl Sync {
    pub fn add_participant(&mut self, participant: Participant) {
        self.participants.push(participant);
    }

    pub fn get_participant_by_ip(&self, ip: &str) -> Option<&Participant> {
        self.participants.iter().find(|p| p.ip == ip)
    }
}

pub fn now_timestamp() -> TimeStamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
