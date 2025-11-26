//! Networking layer placeholder using the Windows APIs.

use anyhow::Result;
use data::{
    netmsg::{self, Header, Message, MessageType},
    Id, Participant,
};
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket},
    time::Duration,
};
use tracing::info;
use windows::Win32::Networking::WinSock::{WSAStartup, WSADATA};

const fn make_word(low: u8, high: u8) -> u16 {
    (low as u16) | ((high as u16) << 8)
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub multicast_v4: Option<Ipv4Addr>,
    pub multicast_iface_v4: Ipv4Addr,
    pub timeout: Duration,
    pub retries: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:0".parse().unwrap(),
            multicast_v4: None,
            multicast_iface_v4: Ipv4Addr::UNSPECIFIED,
            timeout: Duration::from_millis(500),
            retries: 3,
        }
    }
}

pub struct Network {
    initialized: bool,
    socket: Option<UdpSocket>,
    config: Config,
}

impl Network {
    pub fn new() -> Self {
        Self {
            initialized: false,
            socket: None,
            config: Config::default(),
        }
    }

    pub fn with_config(config: Config) -> Self {
        Self {
            initialized: false,
            socket: None,
            config,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
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
        if self.socket.is_none() {
            self.bind(self.config.bind_addr)?;
        }
        if let Some(group) = self.config.multicast_v4 {
            if let Some(sock) = &self.socket {
                sock.join_multicast_v4(&group, &self.config.multicast_iface_v4)?;
                info!(%group, iface=%self.config.multicast_iface_v4, "joined multicast group");
            }
        }
        Ok(())
    }

    /// Bind the underlying UDP socket (use "0.0.0.0:0" for ephemeral).
    pub fn bind<A: ToSocketAddrs>(&mut self, addr: A) -> Result<()> {
        if !self.initialized {
            anyhow::bail!("network not initialized");
        }
        let sock = UdpSocket::bind(addr)?;
        sock.set_nonblocking(false)?;
        sock.set_read_timeout(Some(self.config.timeout))?;
        sock.set_write_timeout(Some(self.config.timeout))?;
        self.socket = Some(sock);
        Ok(())
    }

    /// Send a raw frame to the target address.
    pub fn send_frame<A: ToSocketAddrs>(&self, target: A, frame: &[u8]) -> Result<usize> {
        let sock = self.socket.as_ref().ok_or_else(|| anyhow::anyhow!("socket not bound"))?;
        Ok(sock.send_to(frame, target)?)
    }

    /// Send with simple retry semantics.
    pub fn send_frame_with_retries<A: ToSocketAddrs>(
        &self,
        target: A,
        frame: &[u8],
    ) -> Result<usize> {
        let mut last_err = None;
        for _ in 0..self.config.retries {
            match self.send_frame(&target, frame) {
                Ok(n) => return Ok(n),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("send failed")))
    }

    /// Receive a raw frame into the provided buffer.
    pub fn recv_frame(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let sock = self.socket.as_ref().ok_or_else(|| anyhow::anyhow!("socket not bound"))?;
        Ok(sock.recv_from(buf)?)
    }

    /// Receive once and dispatch to a handler; returns Ok(false) on timeout.
    pub fn recv_and_dispatch<F>(&self, handler: &mut F) -> Result<bool>
    where
        F: FnMut(SocketAddr, Header, Message),
    {
        use std::io::ErrorKind;
        let sock = self.socket.as_ref().ok_or_else(|| anyhow::anyhow!("socket not bound"))?;
        let mut buf = [0u8; 64 * 1024];
        match sock.recv_from(&mut buf) {
            Ok((len, addr)) => {
                if let Some((hdr, msg)) = Message::from_bytes(&buf[..len]) {
                    handler(addr, hdr, msg);
                }
                Ok(true)
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Convenience: build and send a Message using the shared netmsg module.
    pub fn send_message<A: ToSocketAddrs>(
        &self,
        target: A,
        msg: Message,
        msg_type: MessageType,
        is_response: bool,
    ) -> Result<usize> {
        let frame = msg.to_bytes(msg_type, is_response);
        self.send_frame_with_retries(target, &frame)
    }

    pub fn register_participant(&self, participant: &Participant) {
        info!(ip = %participant.ip, "registering participant placeholder");
    }

    /// Legacy shim used by task/main: broadcast a simple sync request over multicast or bound port.
    pub fn send_sync_request(&self, since: Id) -> Result<usize> {
        let target = if let Some(group) = self.config.multicast_v4 {
            SocketAddr::V4(SocketAddrV4::new(group, self.config.bind_addr.port()))
        } else {
            self.config.bind_addr
        };
        let msg = Message::RequestUserTaskSynch(netmsg::UserTaskSynch { since });
        self.send_message(target, msg, MessageType::RequestUserTaskSynch, false)
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

    pub fn file_start(time: u64, filesize: u64, filename: &str) -> Vec<u8> {
        Message::TransferFileStart(netmsg::FileStart {
            time,
            filesize,
            filename: filename.to_string(),
        })
        .to_bytes(MessageType::TransferFileStart, false)
    }

    pub fn file_data(time: u64, seq: u32, data: &[u8], is_response: bool) -> Vec<u8> {
        Message::TransferFileData(netmsg::FileData {
            time,
            seq,
            data: data.to_vec(),
        })
        .to_bytes(MessageType::TransferFileData, is_response)
    }

    pub fn blob_start(time: u64, len: i32) -> Vec<u8> {
        Message::TransferBlobStart(netmsg::BlobStart { time, len })
            .to_bytes(MessageType::TransferBlobStart, false)
    }

    pub fn blob_data(time: u64, seq: u32, data: &[u8], is_response: bool) -> Vec<u8> {
        Message::TransferBlobData(netmsg::BlobData {
            time,
            seq,
            data: data.to_vec(),
        })
        .to_bytes(MessageType::TransferBlobData, is_response)
    }

    /// Parse any incoming buffer into header + message.
    pub fn parse_frame(buf: &[u8]) -> Option<(Header, Message)> {
        Message::from_bytes(buf)
    }
}
