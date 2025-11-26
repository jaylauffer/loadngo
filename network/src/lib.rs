//! Networking layer placeholder using the Windows APIs.

use anyhow::Result;
use data::{Id, Participant};
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
