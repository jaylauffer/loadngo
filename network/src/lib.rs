//! Networking layer placeholder using the Windows APIs.
//!
//! ```rust
//! use data::cas::CasHash;
//! use data::p2pmsg::{Message, MessageType, RequestContent};
//! use network::p2p;
//!
//! let hash = CasHash::digest(b"voice line payload");
//! let frame = p2p::request_content(&[hash]);
//! let (header, message) = p2p::parse_frame(&frame).unwrap();
//!
//! assert_eq!(header.msg_type, MessageType::RequestContent);
//! assert!(!header.is_response);
//!
//! match message {
//!     Message::RequestContent(RequestContent::Request { hashes }) => {
//!         assert_eq!(hashes, vec![hash]);
//!     }
//!     other => panic!("unexpected message: {other:?}"),
//! }
//! ```

mod core;
pub mod model_service;
pub mod p2p;
pub mod task_runtime;

use anyhow::Result;
use data::{
    netmsg::{self, Header, Message, MessageType},
    p2pmsg, Id, Participant,
};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
use std::{
    io::ErrorKind,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs, UdpSocket},
    time::Duration,
};
use tracing::info;

pub use core::{BlobFinish, BlobKey, ContentEnd, ContentFinish, NetworkCore};

#[cfg(windows)]
const fn make_word(low: u8, high: u8) -> u16 {
    (low as u16) | ((high as u16) << 8)
}

#[cfg(windows)]
fn init_socket_runtime() -> Result<()> {
    use windows::Win32::Networking::WinSock::{WSAStartup, WSADATA};

    unsafe {
        let mut data = WSADATA::default();
        let ret = WSAStartup(make_word(2, 2), &mut data);
        if ret != 0 {
            anyhow::bail!("WSAStartup failed: {}", ret);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn init_socket_runtime() -> Result<()> {
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MulticastConfig {
    V4 {
        group: Ipv4Addr,
        interface: Ipv4Addr,
    },
    V6 {
        group: Ipv6Addr,
        interface: u32,
    },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub extra_bind_addrs: Vec<SocketAddr>,
    pub multicast: Vec<MulticastConfig>,
    pub multicast_target_port: Option<u16>,
    pub timeout: Duration,
    pub retries: usize,
}

impl Config {
    pub fn dual_stack(port: u16) -> Self {
        Self {
            bind_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)),
            extra_bind_addrs: vec![SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::UNSPECIFIED,
                port,
                0,
                0,
            ))],
            ..Self::default()
        }
    }

    pub fn bind_addrs(&self) -> Vec<SocketAddr> {
        let mut addrs = vec![self.bind_addr];
        for addr in self.extra_bind_addrs.iter().copied() {
            if !addrs.contains(&addr) {
                addrs.push(addr);
            }
        }
        addrs
    }

    pub fn sync_targets(&self) -> Vec<SocketAddr> {
        let target_port = self.multicast_target_port.unwrap_or(self.bind_addr.port());
        if self.multicast.is_empty() {
            return vec![self.bind_addr];
        }

        self.multicast
            .iter()
            .map(|multicast| match *multicast {
                MulticastConfig::V4 { group, .. } => {
                    SocketAddr::V4(SocketAddrV4::new(group, target_port))
                }
                MulticastConfig::V6 { group, interface } => {
                    SocketAddr::V6(SocketAddrV6::new(group, target_port, 0, interface))
                }
            })
            .collect()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:0".parse().unwrap(),
            extra_bind_addrs: Vec::new(),
            multicast: Vec::new(),
            multicast_target_port: None,
            timeout: Duration::from_millis(500),
            retries: 3,
        }
    }
}

struct BoundSocket {
    bind_addr: SocketAddr,
    socket: UdpSocket,
}

pub struct Network {
    initialized: bool,
    sockets: Vec<BoundSocket>,
    config: Config,
}

impl Network {
    pub fn new() -> Self {
        Self {
            initialized: false,
            sockets: Vec::new(),
            config: Config::default(),
        }
    }

    pub fn with_config(config: Config) -> Self {
        Self {
            initialized: false,
            sockets: Vec::new(),
            config,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.local_addrs()?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("socket not bound"))
    }

    pub fn local_addrs(&self) -> Result<Vec<SocketAddr>> {
        let sockets = self.bound_sockets()?;
        sockets
            .iter()
            .map(|entry| entry.socket.local_addr().map_err(anyhow::Error::from))
            .collect()
    }

    #[cfg(unix)]
    pub fn socket_fds(&self) -> Result<Vec<RawFd>> {
        let sockets = self.bound_sockets()?;
        Ok(sockets
            .iter()
            .map(|entry| entry.socket.as_raw_fd())
            .collect())
    }

    pub fn init(&mut self) -> Result<()> {
        init_socket_runtime()?;
        self.initialized = true;
        info!("network initialized");
        if self.sockets.is_empty() {
            self.bind_all(self.config.bind_addrs())?;
        }
        for multicast in self.config.multicast.iter().copied() {
            for entry in self.bound_sockets()? {
                match multicast {
                    MulticastConfig::V4 { group, interface } => {
                        if !entry.bind_addr.is_ipv4() {
                            continue;
                        }
                        entry.socket.join_multicast_v4(&group, &interface)?;
                        info!(%group, iface=%interface, "joined IPv4 multicast group");
                    }
                    MulticastConfig::V6 { group, interface } => {
                        if !entry.bind_addr.is_ipv6() {
                            continue;
                        }
                        entry.socket.join_multicast_v6(&group, interface)?;
                        info!(%group, iface=%interface, "joined IPv6 multicast group");
                    }
                }
            }
        }
        Ok(())
    }

    /// Bind the underlying UDP socket (use "0.0.0.0:0" for ephemeral).
    pub fn bind<A: ToSocketAddrs>(&mut self, addr: A) -> Result<()> {
        if !self.initialized {
            anyhow::bail!("network not initialized");
        }
        let addrs = addr.to_socket_addrs()?.collect::<Vec<_>>();
        let Some(first) = addrs.first().copied() else {
            anyhow::bail!("bind address resolution returned no candidates");
        };
        self.bind_all([first])
    }

    pub fn bind_all<I>(&mut self, addrs: I) -> Result<()>
    where
        I: IntoIterator<Item = SocketAddr>,
    {
        if !self.initialized {
            anyhow::bail!("network not initialized");
        }

        let addrs = addrs.into_iter().collect::<Vec<_>>();
        if addrs.is_empty() {
            anyhow::bail!("no bind addresses provided");
        }

        let mut sockets = Vec::with_capacity(addrs.len());
        for addr in addrs {
            sockets.push(BoundSocket {
                bind_addr: addr,
                socket: bind_socket(addr)?,
            });
        }
        self.sockets = sockets;
        Ok(())
    }

    /// Send a raw frame to the target address.
    pub fn send_frame<A: ToSocketAddrs>(&self, target: A, frame: &[u8]) -> Result<usize> {
        let targets = target.to_socket_addrs()?.collect::<Vec<_>>();
        let Some(target) = targets.first().copied() else {
            anyhow::bail!("target resolution returned no socket addresses");
        };
        self.send_frame_addr(target, frame)
    }

    /// Send with simple retry semantics.
    pub fn send_frame_with_retries<A: ToSocketAddrs>(
        &self,
        target: A,
        frame: &[u8],
    ) -> Result<usize> {
        let targets = target.to_socket_addrs()?.collect::<Vec<_>>();
        if targets.is_empty() {
            anyhow::bail!("target resolution returned no socket addresses");
        }

        let mut last_err = None;
        for _ in 0..self.config.retries {
            for target in targets.iter().copied() {
                match self.send_frame_addr(target, frame) {
                    Ok(n) => return Ok(n),
                    Err(e) => last_err = Some(e),
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("send failed")))
    }

    /// Receive a raw frame into the provided buffer.
    pub fn recv_frame(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        self.try_recv_frame(buf)?
            .ok_or_else(|| anyhow::anyhow!(std::io::Error::from(ErrorKind::WouldBlock)))
    }

    /// Receive once and dispatch to a handler; returns Ok(false) when no datagram is ready.
    pub fn recv_and_dispatch<F>(&self, handler: &mut F) -> Result<bool>
    where
        F: FnMut(SocketAddr, Header, Message),
    {
        let mut buf = [0u8; 64 * 1024];
        match self.try_recv_frame(&mut buf)? {
            Some((len, addr)) => {
                if let Some((hdr, msg)) = Message::from_bytes(&buf[..len]) {
                    handler(addr, hdr, msg);
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Drain all immediately available datagrams and dispatch the parsed messages.
    pub fn drain_and_dispatch<F>(&self, handler: &mut F) -> Result<usize>
    where
        F: FnMut(SocketAddr, Header, Message),
    {
        let mut buf = [0u8; 64 * 1024];
        let mut received = 0usize;
        loop {
            let mut progressed = false;
            for entry in self.bound_sockets()? {
                loop {
                    match entry.socket.recv_from(&mut buf) {
                        Ok((len, addr)) => {
                            progressed = true;
                            received += 1;
                            if let Some((hdr, msg)) = Message::from_bytes(&buf[..len]) {
                                handler(addr, hdr, msg);
                            }
                        }
                        Err(e)
                            if e.kind() == ErrorKind::WouldBlock
                                || e.kind() == ErrorKind::TimedOut =>
                        {
                            break;
                        }
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            if !progressed {
                return Ok(received);
            }
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

    pub fn send_p2p_message<A: ToSocketAddrs>(
        &self,
        target: A,
        msg: p2pmsg::Message,
        is_response: bool,
    ) -> Result<usize> {
        let frame = msg.to_bytes(is_response);
        self.send_frame_with_retries(target, &frame)
    }

    pub fn send_p2p_multicast_message(
        &self,
        msg: p2pmsg::Message,
        is_response: bool,
    ) -> Result<usize> {
        let frame = msg.to_bytes(is_response);
        let mut sent = 0usize;
        let mut last_err = None;
        for target in self.config.sync_targets() {
            match self.send_frame_with_retries(target, &frame) {
                Ok(bytes) => sent += bytes,
                Err(err) => last_err = Some(err),
            }
        }
        if sent > 0 {
            Ok(sent)
        } else {
            Err(last_err.unwrap_or_else(|| anyhow::anyhow!("multicast send had no targets")))
        }
    }

    pub fn register_participant(&self, participant: &Participant) {
        info!(ip = %participant.ip, "registering participant placeholder");
    }

    /// Legacy shim used by task/main: broadcast a simple sync request over multicast or bound port.
    pub fn send_sync_request(&self, since: Id) -> Result<usize> {
        let frame = Message::RequestUserTaskSynch(netmsg::UserTaskSynch { since })
            .to_bytes(MessageType::RequestUserTaskSynch, false);
        let mut sent = 0usize;
        let mut last_err = None;
        for target in self.config.sync_targets() {
            match self.send_frame_with_retries(target, &frame) {
                Ok(bytes) => sent += bytes,
                Err(err) => last_err = Some(err),
            }
        }
        if sent > 0 {
            Ok(sent)
        } else {
            Err(last_err.unwrap_or_else(|| anyhow::anyhow!("sync request had no targets")))
        }
    }

    pub fn recv_and_dispatch_p2p<F>(&self, handler: &mut F) -> Result<bool>
    where
        F: FnMut(SocketAddr, p2pmsg::Header, p2pmsg::Message),
    {
        let mut buf = [0u8; 64 * 1024];
        match self.try_recv_frame(&mut buf)? {
            Some((len, addr)) => {
                if let Some((hdr, msg)) = p2pmsg::Message::from_bytes(&buf[..len]) {
                    handler(addr, hdr, msg);
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Drain all immediately available datagrams and dispatch parsed p2p messages.
    pub fn drain_and_dispatch_p2p<F>(&self, handler: &mut F) -> Result<usize>
    where
        F: FnMut(SocketAddr, p2pmsg::Header, p2pmsg::Message),
    {
        let mut buf = [0u8; 64 * 1024];
        let mut received = 0usize;
        loop {
            let mut progressed = false;
            for entry in self.bound_sockets()? {
                loop {
                    match entry.socket.recv_from(&mut buf) {
                        Ok((len, addr)) => {
                            progressed = true;
                            received += 1;
                            if let Some((hdr, msg)) = p2pmsg::Message::from_bytes(&buf[..len]) {
                                handler(addr, hdr, msg);
                            }
                        }
                        Err(e)
                            if e.kind() == ErrorKind::WouldBlock
                                || e.kind() == ErrorKind::TimedOut =>
                        {
                            break;
                        }
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            if !progressed {
                return Ok(received);
            }
        }
    }

    fn bound_sockets(&self) -> Result<&[BoundSocket]> {
        if self.sockets.is_empty() {
            anyhow::bail!("socket not bound");
        }
        Ok(&self.sockets)
    }

    fn send_frame_addr(&self, target: SocketAddr, frame: &[u8]) -> Result<usize> {
        let socket = self.select_socket_for_target(target)?;
        Ok(socket.send_to(frame, target)?)
    }

    fn select_socket_for_target(&self, target: SocketAddr) -> Result<&UdpSocket> {
        let sockets = self.bound_sockets()?;
        if let Some(entry) = sockets
            .iter()
            .find(|entry| entry.bind_addr.is_ipv4() == target.is_ipv4())
        {
            return Ok(&entry.socket);
        }
        Ok(&sockets[0].socket)
    }

    fn try_recv_frame(&self, buf: &mut [u8]) -> Result<Option<(usize, SocketAddr)>> {
        for entry in self.bound_sockets()? {
            loop {
                match entry.socket.recv_from(buf) {
                    Ok(result) => return Ok(Some(result)),
                    Err(e)
                        if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
                    {
                        break;
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(None)
    }
}

fn bind_socket(addr: SocketAddr) -> Result<UdpSocket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
        socket.set_multicast_loop_v6(true)?;
    } else {
        socket.set_multicast_loop_v4(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&SockAddr::from(addr))?;
    Ok(socket.into())
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
