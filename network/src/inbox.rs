//! Datagrams arriving on a [`Network`]'s sockets, received through the loadngo proactor.
//!
//! One `recv_from` stays posted on each bound socket, with a buffer that is handed back
//! in its completion and posted again, so a program waiting for a message sleeps in the
//! kernel (kqueue, epoll, io_uring or IOCP) until one arrives or its deadline passes.
//! The Task tools used to try a non-blocking receive and sleep 25 ms when nothing had
//! come, about 33 context switches a second while idle; `task-node` checked its sockets
//! from a 250 ms timer (2026-10-14, `task-records/2026-10-14-task-tools-stop-polling`).
//!
//! [`Inbox`] is for a program that waits in sequence: offers, then a result, then an
//! acknowledgement. A program that runs on its own proactor, as `task-node` does, posts
//! the receives itself with [`datagram`] and [`receive_again`].

use crate::Network;
use anyhow::{Context, Result};
use data::p2pmsg::{Header, Message};
use loadngo_proactor::{
    new_platform_proactor, IoBuf, IoResult, IoTransfer, PlatformPort, Proactor, RawFdCompat,
};
use std::{
    collections::VecDeque,
    io,
    marker::PhantomData,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Instant,
};

/// The largest datagram received: the most a UDP payload can be.
pub const DATAGRAM_BYTES: usize = 64 * 1024;

/// A message one datagram carried, and who sent it.
pub type Received = (SocketAddr, Header, Message);

type Arrivals = Arc<Mutex<VecDeque<(usize, IoResult)>>>;

/// The message in a completed receive, if it carried one, and the buffer to post the
/// next receive with: the same allocation, its full length restored.
#[must_use]
pub fn datagram(transfer: IoTransfer) -> (Option<Received>, IoBuf) {
    let len = transfer.bytes_transferred as usize;
    let mut bytes = transfer.buf.into_vec();
    let received = transfer.peer.and_then(|peer| {
        Message::from_bytes(&bytes[..len.min(bytes.len())])
            .map(|(header, message)| (peer, header, message))
    });
    bytes.resize(DATAGRAM_BYTES, 0);
    (received, IoBuf::from_vec(bytes))
}

/// Whether a failed receive only means "receive again": Windows reports a datagram this
/// socket sent earlier that was refused (ICMP port unreachable) on its next receive, and
/// a receive can be interrupted. Nothing arrived either way.
#[must_use]
pub fn receive_again(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::Interrupted
    )
}

/// Messages arriving on a [`Network`]'s sockets, waited for on the platform proactor.
///
/// Make one per process, after [`Network::init`], and keep it: on Windows a socket joins
/// one completion port for good.
pub struct Inbox<'n> {
    proactor: Proactor<PlatformPort>,
    sockets: Vec<RawFdCompat>,
    arrived: Arrivals,
    _network: PhantomData<&'n Network>,
}

impl<'n> Inbox<'n> {
    /// Posts a receive on each of `network`'s bound sockets.
    ///
    /// # Errors
    /// When the proactor cannot start, or a socket cannot be registered or received on.
    pub fn new(network: &'n Network) -> Result<Self> {
        let proactor = new_platform_proactor().context("cannot start the proactor")?;
        let handle = proactor.handle();
        let sockets = network
            .raw_sockets()?
            .into_iter()
            .map(|socket| handle.register(socket))
            .collect::<io::Result<Vec<_>>>()
            .context("cannot register a socket with the proactor")?;
        let inbox = Self {
            proactor,
            sockets,
            arrived: Arc::default(),
            _network: PhantomData,
        };
        for index in 0..inbox.sockets.len() {
            inbox.post(index, IoBuf::with_capacity(DATAGRAM_BYTES))?;
        }
        Ok(inbox)
    }

    fn post(&self, index: usize, buf: IoBuf) -> Result<()> {
        let arrived = Arc::clone(&self.arrived);
        self.proactor
            .handle()
            .recv_from(self.sockets[index], buf, move |result| {
                arrived
                    .lock()
                    .expect("inbox queue poisoned")
                    .push_back((index, result));
            })
            .context("cannot post a receive")?;
        Ok(())
    }

    /// The next message from any socket, or `None` once `deadline` passes with none.
    /// Datagrams that are not p2p messages are skipped.
    ///
    /// # Errors
    /// When a receive fails other than as [`receive_again`] allows, or the proactor does.
    pub fn next_until(&mut self, deadline: Instant) -> Result<Option<Received>> {
        loop {
            let next = self
                .arrived
                .lock()
                .expect("inbox queue poisoned")
                .pop_front();
            if let Some((index, result)) = next {
                match result {
                    Ok(transfer) => {
                        let (received, buf) = datagram(transfer);
                        self.post(index, buf)?;
                        if received.is_some() {
                            return Ok(received);
                        }
                    }
                    Err(error) if receive_again(&error) => {
                        self.post(index, IoBuf::with_capacity(DATAGRAM_BYTES))?;
                    }
                    Err(error) => return Err(error).context("a receive failed"),
                }
                continue;
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            self.proactor
                .run_once_until(deadline)
                .context("the proactor failed")?;
        }
    }
}

impl Drop for Inbox<'_> {
    /// Cancels the posted receives and waits for them, before their buffers and the
    /// network's sockets go.
    fn drop(&mut self) {
        let handle = self.proactor.handle();
        let _ = handle.stop();
        let _ = self.proactor.run_until_stopped();
        for socket in &self.sockets {
            handle.release(*socket);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Inbox;
    use crate::{task_runtime::task_network_config, Network};
    use data::p2pmsg::{Message, TaskStatus};
    use std::time::{Duration, Instant};

    #[test]
    fn an_inbox_receives_a_message_and_returns_none_at_its_deadline() {
        let mut network = Network::with_config(task_network_config(0, &[], &[]));
        network.init().unwrap();
        let port = network.local_addr().unwrap().port();
        let mut inbox = Inbox::new(&network).unwrap();

        let status = TaskStatus {
            assignment_id: 7,
            request_id: 11,
            offer_id: 13,
            worker_node_id: "worker".into(),
            status_at: 17,
            state: "running".into(),
            next_check_in_by: None,
            note: None,
            artifact_hint: None,
        };
        let sent = Message::TaskStatus(status.clone());
        for _ in 0..2 {
            network
                .send_p2p_message(("127.0.0.1", port), sent.clone(), false)
                .unwrap();
            let (_, _, received) = inbox
                .next_until(Instant::now() + Duration::from_secs(10))
                .unwrap()
                .expect("a message within the deadline");
            assert_eq!(received, sent);
        }

        let start = Instant::now();
        let none = inbox
            .next_until(start + Duration::from_millis(300))
            .unwrap();
        assert!(none.is_none());
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(300), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
    }
}
