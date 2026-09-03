#![cfg(target_os = "android")]

use loadngo_proactor::{
    AcceptResult, Completion, CompletionKind, EpollPort, IoBuf, IoResult, Proactor, ReadinessEvent,
};
use std::io;
use std::net::{TcpListener, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn epoll_dispatches_enqueued_work() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();
    let (tx, rx) = mpsc::channel();

    handle
        .enqueue_work(move |completion: Completion| {
            tx.send((completion.kind, completion.bytes_transferred))
                .unwrap();
        })
        .unwrap();

    let report = proactor.run_once().unwrap();
    assert_eq!(report.dispatched_completions, 1);
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(100)).unwrap(),
        (CompletionKind::Job, 0)
    );
}

#[test]
fn epoll_wake_interrupts_blocking_poll() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();

    let worker = thread::spawn(move || {
        let started = Instant::now();
        let report = proactor.run_once().unwrap();
        (started.elapsed(), report)
    });

    thread::sleep(Duration::from_millis(25));
    handle.wake().unwrap();

    let (elapsed, report) = worker.join().unwrap();
    assert!(elapsed < Duration::from_secs(1));
    assert!(report.woke);
}

#[test]
fn epoll_dispatches_registered_readiness() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();
    let (tx, rx) = mpsc::channel();
    let mut pipe_fds = [0; 2];

    let rc = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(rc, 0);

    handle
        .register_readable(pipe_fds[0], 77, move |readiness: ReadinessEvent| {
            tx.send(readiness.token).unwrap();
        })
        .unwrap();

    let worker = thread::spawn(move || proactor.run_once().unwrap());
    thread::sleep(Duration::from_millis(25));

    let value = [1u8; 1];
    let written = unsafe { libc::write(pipe_fds[1], value.as_ptr() as *const _, value.len()) };
    assert_eq!(written, 1);

    let report = worker.join().unwrap();
    assert!(report.woke);
    assert_eq!(rx.recv_timeout(Duration::from_millis(100)).unwrap(), 77);

    handle.deregister_readable(pipe_fds[0], 77).unwrap();
    unsafe {
        libc::close(pipe_fds[0]);
        libc::close(pipe_fds[1]);
    }
}

#[test]
fn epoll_write_then_read_round_trip_a_real_file() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();

    let path = std::env::temp_dir().join(format!(
        "loadngo-proactor-epoll-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let fd = file.as_raw_fd();

    let (write_tx, write_rx) = mpsc::channel();
    handle
        .write(
            fd,
            IoBuf::from_vec(b"hello epoll".to_vec()),
            0,
            move |result: IoResult| {
                write_tx.send(result.map(|t| t.bytes_transferred)).unwrap();
            },
        )
        .unwrap();

    let mut dispatched = 0;
    let start = Instant::now();
    while dispatched < 1 {
        dispatched += proactor.run_ready().unwrap().dispatched_completions;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "write never completed"
        );
    }
    let written = write_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap()
        .unwrap();
    assert_eq!(written as usize, b"hello epoll".len());

    let (read_tx, read_rx) = mpsc::channel();
    handle
        .read(fd, IoBuf::with_capacity(64), 0, move |result: IoResult| {
            let transfer = result.unwrap();
            read_tx
                .send((transfer.bytes_transferred, transfer.buf.into_vec()))
                .unwrap();
        })
        .unwrap();

    let mut dispatched = 0;
    let start = Instant::now();
    while dispatched < 1 {
        dispatched += proactor.run_ready().unwrap().dispatched_completions;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "read never completed"
        );
    }
    let (n, buf) = read_rx.recv_timeout(Duration::from_millis(100)).unwrap();
    assert_eq!(n as usize, b"hello epoll".len());
    assert_eq!(&buf[..n as usize], b"hello epoll");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn epoll_recv_from_reports_the_real_sender_and_send_to_reaches_it() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();

    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let receiver_addr = receiver.local_addr().unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sender_addr = sender.local_addr().unwrap();
    let receiver_fd = receiver.as_raw_fd();
    let sender_fd = sender.as_raw_fd();

    let (recv_tx, recv_rx) = mpsc::channel();
    handle
        .recv_from(
            receiver_fd,
            IoBuf::with_capacity(64),
            move |result: IoResult| {
                let transfer = result.unwrap();
                recv_tx
                    .send((
                        transfer.bytes_transferred,
                        transfer.peer,
                        transfer.buf.into_vec(),
                    ))
                    .unwrap();
            },
        )
        .unwrap();

    let (send_tx, send_rx) = mpsc::channel();
    handle
        .send_to(
            sender_fd,
            IoBuf::from_vec(b"ping".to_vec()),
            receiver_addr,
            move |result: IoResult| {
                send_tx.send(result.map(|t| t.bytes_transferred)).unwrap();
            },
        )
        .unwrap();

    let mut dispatched = 0;
    let start = Instant::now();
    while dispatched < 2 {
        dispatched += proactor.run_ready().unwrap().dispatched_completions;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "send_to/recv_from never completed"
        );
    }

    let sent = send_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap()
        .unwrap();
    assert_eq!(sent as usize, 4);
    let (n, peer, buf) = recv_rx.recv_timeout(Duration::from_millis(100)).unwrap();
    assert_eq!(n as usize, 4);
    assert_eq!(&buf[..4], b"ping");
    assert_eq!(peer, Some(sender_addr));
}

#[test]
fn epoll_accept_reports_the_real_connecting_peer() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let listener_addr = listener.local_addr().unwrap();
    let listener_fd = listener.as_raw_fd();

    let (accept_tx, accept_rx) = mpsc::channel();
    handle
        .accept(listener_fd, move |result: AcceptResult| {
            let transfer = result.unwrap();
            accept_tx.send(transfer.peer).unwrap();
            unsafe {
                libc::close(transfer.new_fd);
            }
        })
        .unwrap();

    let client = std::net::TcpStream::connect(listener_addr).unwrap();
    let client_addr = client.local_addr().unwrap();

    let mut dispatched = 0;
    let start = Instant::now();
    while dispatched < 1 {
        dispatched += proactor.run_ready().unwrap().dispatched_completions;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "accept never completed"
        );
    }
    let peer = accept_rx.recv_timeout(Duration::from_millis(100)).unwrap();
    assert_eq!(peer, client_addr);
}

#[test]
fn epoll_connect_reaches_a_real_listener() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let listener_addr = listener.local_addr().unwrap();

    // A fresh, unconnected socket -- std has no API to create one of
    // these without also connecting it, so this goes straight to libc.
    let raw_fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    assert!(raw_fd >= 0);

    let (connect_tx, connect_rx) = mpsc::channel();
    handle
        .connect(raw_fd, listener_addr, move |result: io::Result<()>| {
            connect_tx.send(result).unwrap();
        })
        .unwrap();

    let mut dispatched = 0;
    let start = Instant::now();
    while dispatched < 1 {
        dispatched += proactor.run_ready().unwrap().dispatched_completions;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "connect never completed"
        );
    }
    connect_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap()
        .unwrap();

    unsafe {
        libc::close(raw_fd);
    }
}

#[test]
fn epoll_shutdown_drains_a_still_in_flight_op_instead_of_hanging() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();

    // A recv() on a socket nothing will ever write to -- guaranteed to
    // still be in flight when stop() is called, exercising the
    // begin_shutdown/cancel_io/shutdown_complete drain path rather than
    // just the already-covered "nothing in flight" no-op case.
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let fd = socket.as_raw_fd();
    handle
        .recv(fd, IoBuf::with_capacity(64), |_result| {
            // Expected to fire with a cancelled/error result during
            // shutdown draining; nothing to assert on the value here,
            // the real assertion is that shutdown finishes at all.
        })
        .unwrap();

    let worker = thread::spawn(move || {
        let started = Instant::now();
        proactor.run_until_stopped().unwrap();
        started.elapsed()
    });

    thread::sleep(Duration::from_millis(25));
    handle.stop().unwrap();

    let elapsed = worker.join().unwrap();
    assert!(
        elapsed < Duration::from_secs(5),
        "run_until_stopped did not drain the in-flight recv and return"
    );
}

/// Not covered by `tests/kqueue.rs`'s mirrored suite above: this exercises
/// the one real architectural difference between the two backends —
/// epoll registers interest *per fd*, not per direction, so a fd with
/// both a pending read and a pending write must resolve each
/// independently across two `poll()` calls rather than losing one. See
/// `epoll.rs`'s module doc and `resolve_fd_event`.
#[test]
fn epoll_resolves_simultaneous_read_and_write_waits_on_the_same_fd() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();

    let (a, b) = UnixStreamPair::connected();
    let fd = a.as_raw_fd();

    // `a` has nothing to read yet, so recv() goes to a real one-shot
    // wait; a fresh socket pair is immediately writable, so send() (also
    // registered on `fd`, the opposite direction) resolves on its first
    // optimistic attempt without ever needing epoll at all -- exercising
    // that both directions' bookkeeping on the *same* FdEntry coexist
    // correctly regardless of which one actually goes through a real
    // epoll round-trip.
    let (recv_tx, recv_rx) = mpsc::channel();
    handle
        .recv(fd, IoBuf::with_capacity(64), move |result: IoResult| {
            let transfer = result.unwrap();
            recv_tx.send(transfer.buf.into_vec()).unwrap();
        })
        .unwrap();

    let (send_tx, send_rx) = mpsc::channel();
    handle
        .send(
            fd,
            IoBuf::from_vec(b"pong".to_vec()),
            move |result: IoResult| {
                send_tx.send(result.map(|t| t.bytes_transferred)).unwrap();
            },
        )
        .unwrap();

    // send() should already be resolved (queued, no epoll wait needed);
    // write something to `b`'s end so `a`'s pending recv() has real data
    // to observe once its wait fires.
    let mut dispatched = 0;
    let start = Instant::now();
    while dispatched < 1 {
        dispatched += proactor.run_ready().unwrap().dispatched_completions;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "send never completed"
        );
    }
    let sent = send_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap()
        .unwrap();
    assert_eq!(sent as usize, 4);

    b.write_all(b"ping");

    let mut dispatched = 0;
    let start = Instant::now();
    while dispatched < 1 {
        dispatched += proactor.run_ready().unwrap().dispatched_completions;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "recv never completed"
        );
    }
    let received = recv_rx.recv_timeout(Duration::from_millis(100)).unwrap();
    assert_eq!(&received[..4], b"ping");
}

/// A minimal raw `AF_UNIX` `SOCK_STREAM` pair, since `std` has no direct
/// `socketpair` API and this test needs two connected, non-blocking-safe
/// endpoints without pulling in a real network round-trip. Each half owns
/// only its own fd -- deliberately not the peer's too, which would
/// double-close the same underlying fd once both halves drop.
struct UnixStreamPair {
    fd: i32,
}

impl UnixStreamPair {
    fn connected() -> (Self, Self) {
        let mut fds = [0i32; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0);
        (Self { fd: fds[0] }, Self { fd: fds[1] })
    }

    fn as_raw_fd(&self) -> i32 {
        self.fd
    }

    fn write_all(&self, data: &[u8]) {
        let written = unsafe { libc::write(self.fd, data.as_ptr().cast(), data.len()) };
        assert_eq!(written as usize, data.len());
    }
}

impl Drop for UnixStreamPair {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}
