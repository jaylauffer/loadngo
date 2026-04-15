use crate::{ContentEnd, Network, NetworkCore};
use anyhow::Result;
use data::{
    cas::{CasHash, CasStorage},
    p2pmsg::{
        self, EncodingBitset, FileData, FileEnd, FileMissed, FileStart, Message, Ping,
        RequestContent,
    },
};
use loadngo_proactor::{CompletionKind, CompletionPort, ProactorHandle};
#[cfg(unix)]
use loadngo_proactor::{ReadinessEvent, ReadinessPort};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

pub fn user_intro(name: &str, is_response: bool) -> Vec<u8> {
    Message::UserIntroduction(name.to_string()).to_bytes(is_response)
}

pub fn user_depart(name: &str) -> Vec<u8> {
    Message::UserDeparture(name.to_string()).to_bytes(false)
}

pub fn file_start(time: u64, filesize: u64, hash: CasHash) -> Vec<u8> {
    Message::TransferFileStart(FileStart {
        time,
        filesize,
        hash,
    })
    .to_bytes(false)
}

pub fn file_data(time: u64, seq: u32, data: &[u8], is_response: bool) -> Vec<u8> {
    Message::TransferFileData(FileData {
        time,
        seq,
        data: data.to_vec(),
    })
    .to_bytes(is_response)
}

pub fn file_end(time: u64, is_response: bool) -> Vec<u8> {
    Message::TransferFileEnd(FileEnd { time }).to_bytes(is_response)
}

pub fn file_missed(time: u64, seq: u32) -> Vec<u8> {
    Message::TransferFileMissed(FileMissed { time, seq }).to_bytes(false)
}

pub fn request_bitset(hash: CasHash) -> Vec<u8> {
    Message::EncodingBitset(EncodingBitset::Request { hash }).to_bytes(false)
}

pub fn encoding_bitset(hash: CasHash, numbits: u32, data: &[u8]) -> Vec<u8> {
    Message::EncodingBitset(EncodingBitset::Response {
        hash,
        numbits,
        data: data.to_vec(),
    })
    .to_bytes(true)
}

pub fn request_content(hashes: &[CasHash]) -> Vec<u8> {
    Message::RequestContent(RequestContent::Request {
        hashes: hashes.to_vec(),
    })
    .to_bytes(false)
}

pub fn send_content(hash: CasHash, data: &[u8]) -> Vec<u8> {
    Message::RequestContent(RequestContent::Response {
        hash,
        data: data.to_vec(),
    })
    .to_bytes(true)
}

pub fn ping(time: u64) -> Vec<u8> {
    Message::Ping(Ping { time }).to_bytes(false)
}

pub fn ping_response(ping_payload: &[u8; p2pmsg::PING_LEN]) -> Vec<u8> {
    let time = u64::from_le_bytes(*ping_payload);
    Message::Ping(Ping { time }).to_bytes(true)
}

pub fn parse_frame(buf: &[u8]) -> Option<(p2pmsg::Header, p2pmsg::Message)> {
    p2pmsg::Message::from_bytes(buf)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    ContentStored(CasHash),
    ContentCorrupt(CasHash),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HandleResult {
    pub outbound: Vec<Message>,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchResult {
    pub source: SocketAddr,
    pub header: p2pmsg::Header,
    pub outbound: Vec<Message>,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone)]
pub struct Protocol {
    next_transfer_time: u64,
}

impl Protocol {
    pub fn new() -> Self {
        Self {
            next_transfer_time: 1,
        }
    }

    pub fn set_next_transfer_time(&mut self, next_transfer_time: u64) {
        self.next_transfer_time = next_transfer_time;
    }

    pub fn handle_message(
        &mut self,
        core: &mut NetworkCore,
        source: SocketAddr,
        message: Message,
        storage: &CasStorage,
    ) -> Result<HandleResult> {
        let mut result = HandleResult::default();
        match message {
            Message::RequestContent(RequestContent::Request { hashes }) => {
                for hash in hashes {
                    result.outbound.extend(self.build_file_transfer(
                        hash,
                        storage,
                        core.chunk_size(),
                    )?);
                }
            }
            Message::RequestContent(RequestContent::Response { hash, data }) => {
                storage.verify_and_add(hash, &data)?;
                result.events.push(Event::ContentStored(hash));
            }
            Message::TransferFileStart(FileStart {
                time,
                filesize,
                hash,
            }) => {
                core.start_content_transfer(source, time, hash, filesize as usize, storage)?;
            }
            Message::TransferFileData(FileData { time, seq, data }) => {
                core.push_content_data(source, time, seq, &data, storage)?;
            }
            Message::TransferFileEnd(FileEnd { time }) => {
                match core.end_content_transfer(source, time, storage)? {
                    ContentEnd::Stored(hash) => result.events.push(Event::ContentStored(hash)),
                    ContentEnd::Corrupt(hash) => result.events.push(Event::ContentCorrupt(hash)),
                    ContentEnd::Incomplete(_) => {}
                }
            }
            Message::TransferFileMissed(FileMissed { .. }) => {}
            _ => {}
        }
        Ok(result)
    }

    fn build_file_transfer(
        &mut self,
        hash: CasHash,
        storage: &CasStorage,
        chunk_size: usize,
    ) -> Result<Vec<Message>> {
        let Some(filesize) = storage.get_size(hash)? else {
            return Ok(Vec::new());
        };
        if filesize == 0 {
            return Ok(Vec::new());
        }

        let time = self.next_transfer_time;
        self.next_transfer_time = self.next_transfer_time.wrapping_add(1);

        let mut frames = Vec::new();
        frames.push(Message::TransferFileStart(FileStart {
            time,
            filesize: u64::from(filesize),
            hash,
        }));

        let mut seq = 0u32;
        let mut offset = 0u32;
        loop {
            let amount = (filesize - offset).min(chunk_size as u32);
            let data = storage.read(hash, offset, amount)?;
            frames.push(Message::TransferFileData(FileData { time, seq, data }));
            seq += 1;
            offset = offset.saturating_add(amount);

            if amount != chunk_size as u32 {
                break;
            }
        }

        frames.push(Message::TransferFileEnd(FileEnd { time }));
        Ok(frames)
    }
}

impl Default for Protocol {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct SneakerNet {
    protocol: Protocol,
    core: NetworkCore,
}

impl SneakerNet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_chunk_size(chunk_size: usize) -> Self {
        Self {
            protocol: Protocol::default(),
            core: NetworkCore::with_chunk_size(chunk_size),
        }
    }

    pub fn protocol(&self) -> &Protocol {
        &self.protocol
    }

    pub fn protocol_mut(&mut self) -> &mut Protocol {
        &mut self.protocol
    }

    pub fn core(&self) -> &NetworkCore {
        &self.core
    }

    pub fn core_mut(&mut self) -> &mut NetworkCore {
        &mut self.core
    }

    pub fn recv_and_dispatch(
        &mut self,
        network: &Network,
        storage: &CasStorage,
    ) -> Result<Option<DispatchResult>> {
        let mut captured = None;
        let received = network.recv_and_dispatch_p2p(&mut |source, header, message| {
            captured = Some((source, header, message));
        })?;
        if !received {
            return Ok(None);
        }

        let Some((source, header, message)) = captured else {
            return Ok(None);
        };
        self.dispatch_message(network, source, header, message, storage)
            .map(Some)
    }

    pub fn dispatch_message(
        &mut self,
        network: &Network,
        source: SocketAddr,
        header: p2pmsg::Header,
        message: Message,
        storage: &CasStorage,
    ) -> Result<DispatchResult> {
        let handled = self
            .protocol
            .handle_message(&mut self.core, source, message, storage)?;
        for outbound in handled.outbound.iter().cloned() {
            network.send_p2p_message(source, outbound.clone(), response_flag(&outbound))?;
        }
        Ok(DispatchResult {
            source,
            header,
            outbound: handled.outbound,
            events: handled.events,
        })
    }
}

type DispatchCallback = Box<dyn FnMut(Result<DispatchResult>) + Send + 'static>;
type CleanupCallback = Box<dyn FnMut() + Send + 'static>;

#[cfg(unix)]
const SNEAKERNET_READINESS_TOKEN: u64 = 0x4c4e_475f_4e45_5431;

pub struct ProactorSneakerNet<P>
where
    P: CompletionPort,
{
    inner: Arc<ProactorSneakerNetInner<P>>,
}

struct ProactorSneakerNetInner<P>
where
    P: CompletionPort,
{
    network: Arc<Network>,
    storage: Arc<CasStorage>,
    sneakernet: Mutex<SneakerNet>,
    handle: ProactorHandle<P>,
    idle_interval: Duration,
    on_dispatch: Mutex<DispatchCallback>,
    cleanup: Mutex<Option<CleanupCallback>>,
}

impl<P> ProactorSneakerNet<P>
where
    P: CompletionPort,
{
    pub fn start(
        network: Arc<Network>,
        storage: Arc<CasStorage>,
        handle: ProactorHandle<P>,
        idle_interval: Duration,
        on_dispatch: impl FnMut(Result<DispatchResult>) + Send + 'static,
    ) -> Result<Self> {
        Self::start_with_chunk_size(
            network,
            storage,
            handle,
            idle_interval,
            16 * 1024,
            on_dispatch,
        )
    }

    pub fn start_with_chunk_size(
        network: Arc<Network>,
        storage: Arc<CasStorage>,
        handle: ProactorHandle<P>,
        idle_interval: Duration,
        chunk_size: usize,
        on_dispatch: impl FnMut(Result<DispatchResult>) + Send + 'static,
    ) -> Result<Self> {
        let inner = Arc::new(ProactorSneakerNetInner {
            network,
            storage,
            sneakernet: Mutex::new(SneakerNet::with_chunk_size(chunk_size)),
            handle,
            idle_interval: if idle_interval.is_zero() {
                Duration::from_millis(1)
            } else {
                idle_interval
            },
            on_dispatch: Mutex::new(Box::new(on_dispatch)),
            cleanup: Mutex::new(None),
        });
        inner.schedule(Duration::ZERO)?;
        Ok(Self { inner })
    }

    pub fn schedule_now(&self) -> Result<()> {
        self.inner.schedule(Duration::ZERO)
    }

    #[cfg(unix)]
    pub fn start_registered(
        network: Arc<Network>,
        storage: Arc<CasStorage>,
        handle: ProactorHandle<P>,
        chunk_size: usize,
        on_dispatch: impl FnMut(Result<DispatchResult>) + Send + 'static,
    ) -> Result<Self>
    where
        P: ReadinessPort,
    {
        let socket_fds = network.socket_fds()?;
        let cleanup_fds = socket_fds.clone();
        let cleanup_handle = handle.clone();
        let inner = Arc::new(ProactorSneakerNetInner {
            network,
            storage,
            sneakernet: Mutex::new(SneakerNet::with_chunk_size(chunk_size)),
            handle,
            idle_interval: Duration::from_millis(1),
            on_dispatch: Mutex::new(Box::new(on_dispatch)),
            cleanup: Mutex::new(Some(Box::new(move || {
                for (index, fd) in cleanup_fds.iter().copied().enumerate() {
                    let _ = cleanup_handle
                        .deregister_readable(fd, SNEAKERNET_READINESS_TOKEN + index as u64);
                }
            }))),
        });

        for (index, fd) in socket_fds.into_iter().enumerate() {
            let driver = Arc::clone(&inner);
            inner.handle.register_readable(
                fd,
                SNEAKERNET_READINESS_TOKEN + index as u64,
                move |_readiness: ReadinessEvent| {
                    driver.drain_and_report();
                },
            )?;
        }

        Ok(Self { inner })
    }
}

impl<P> Drop for ProactorSneakerNet<P>
where
    P: CompletionPort,
{
    fn drop(&mut self) {
        if let Some(mut cleanup) = self
            .inner
            .cleanup
            .lock()
            .expect("proactor sneaker net cleanup lock poisoned")
            .take()
        {
            cleanup();
        }
    }
}

impl<P> ProactorSneakerNetInner<P>
where
    P: CompletionPort,
{
    fn schedule(self: &Arc<Self>, delay: Duration) -> Result<()> {
        let driver = Arc::clone(self);
        self.handle
            .defer_for(delay, CompletionKind::Net, 0, move |_| {
                driver.run();
            })?;
        Ok(())
    }

    fn run(self: Arc<Self>) {
        let drained = self.drain_and_report();

        if !self.handle.is_running() {
            return;
        }

        let delay = if drained == 0 {
            self.idle_interval
        } else {
            Duration::ZERO
        };
        if let Err(err) = self.schedule(delay) {
            self.report(Err(err));
        }
    }

    fn drain_and_report(&self) -> usize {
        match self.drain_dispatches() {
            Ok(drained) => drained,
            Err(err) => {
                self.report(Err(err));
                0
            }
        }
    }

    fn drain_dispatches(&self) -> Result<usize> {
        self.network
            .drain_and_dispatch_p2p(&mut |source, header, message| {
                let result = {
                    let mut sneakernet = self
                        .sneakernet
                        .lock()
                        .expect("sneakernet state lock poisoned");
                    sneakernet.dispatch_message(
                        &self.network,
                        source,
                        header,
                        message,
                        &self.storage,
                    )
                };
                self.report(result);
            })
    }

    fn report(&self, result: Result<DispatchResult>) {
        let mut on_dispatch = self
            .on_dispatch
            .lock()
            .expect("proactor sneaker net callback lock poisoned");
        (on_dispatch)(result);
    }
}

fn response_flag(message: &Message) -> bool {
    matches!(
        message,
        Message::EncodingBitset(EncodingBitset::Response { .. })
            | Message::RequestContent(RequestContent::Response { .. })
    )
}
