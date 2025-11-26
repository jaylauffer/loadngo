//! Networking layer placeholder using the Windows APIs.

use anyhow::Result;
use data::{
    netmsg::{self, Header, Message, MessageType},
    Id, Participant,
};
use tracing::info;
use windows::Win32::Networking::WinSock::{WSAStartup, WSADATA};

const fn make_word(low: u8, high: u8) -> u16 {
    (low as u16) | ((high as u16) << 8)
}

pub struct Network {
    initialized: bool,
}

impl Network {
    pub fn new() -> Self {
        Self { initialized: false }
    }

    pub fn init(&mut self) -> Result<()> {
        unsafe {
            let mut data = WSADATA::default();
            let ret = WSAStartup(make_word(2, 2), &mut data);
            if ret != 0 {
                anyhow::bail!("WSAStartup failed: {}", ret);
            }
        }
        self.initialized = true;
        info!("network initialized");
        Ok(())
    }

    pub fn send_sync_request(&self, _sync_id: Id) -> Result<()> {
        // TODO: implement real multicast send once message formats are ported.
        if !self.initialized {
            anyhow::bail!("network not initialized");
        }
        info!("send_sync_request placeholder");
        Ok(())
    }

    pub fn register_participant(&self, participant: &Participant) {
        info!(ip = %participant.ip, "registering participant placeholder");
    }
}

/// Helpers that build legacy-compliant network frames using the shared netmsg module.
pub mod netutil {
    use super::*;

    /// Build a UserIntroduction frame (legacy wire format).
    pub fn user_intro(
        machine_id: Id,
        user_key: [u8; 64],
        name: &str,
        device: &str,
        is_response: bool,
    ) -> Vec<u8> {
        let msg = Message::UserIntroduction(netmsg::UserIntroduction {
            machine_id,
            user_key,
            name: name.to_string(),
            device: device.to_string(),
        });
        msg.to_bytes(MessageType::UserIntroduction, is_response)
    }

    /// Build a ReportDiscrepancies frame for the provided discrepancies.
    pub fn discrepancies(
        sync_id: Id,
        entries: Vec<netmsg::Discrepancy>,
        is_response: bool,
    ) -> Vec<u8> {
        let msg = Message::ReportDiscrepancies(netmsg::DiscrepanciesReport {
            sync_id,
            discrepancies: entries,
        });
        msg.to_bytes(MessageType::ReportDiscrepancies, is_response)
    }

    /// Build a RequestMoveChain frame with arbitrary payload data (XML in the legacy code).
    pub fn move_chain(
        origin_id: Id,
        sync_id: Id,
        doid: Id,
        data: &[u8],
        is_response: bool,
    ) -> Vec<u8> {
        let msg = Message::RequestMoveChain(netmsg::EntityPayload {
            origin_id,
            sync_id,
            doid,
            data: data.to_vec(),
        });
        msg.to_bytes(MessageType::RequestMoveChain, is_response)
    }

    /// Parse any incoming buffer into header + message.
    pub fn parse_frame(buf: &[u8]) -> Option<(Header, Message)> {
        Message::from_bytes(buf)
    }
}
