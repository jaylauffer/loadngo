#![cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]

use loadngo_proactor::{
    AcceptResult, Completion, CompletionKind, IoBuf, IoResult, KqueuePort, Proactor, ReadinessEvent,
};
use std::io;
use std::net::{TcpListener, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn kqueue_dispatches_enqueued_work() {
    let proactor = Proactor::new(KqueuePort::new().unwrap());
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
fn kqueue_wake_interrupts_blocking_poll() {
    let proactor = Proactor::new(KqueuePort::new().unwrap());
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
fn kqueue_dispatches_registered_readiness() {
    let proactor = Proactor::new(KqueuePort::new().unwrap());
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
fn kqueue_write_then_read_round_trip_a_real_file() {
    let proactor = Proactor::new(KqueuePort::new().unwrap());
    let handle = proactor.handle();

    let path = std::env::temp_dir().join(format!(
        "loadngo-proactor-kqueue-test-{}-{:?}",
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
            IoBuf::from_vec(b"hello kqueue".to_vec()),
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
    assert_eq!(written as usize, b"hello kqueue".len());

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
    assert_eq!(n as usize, b"hello kqueue".len());
    assert_eq!(&buf[..n as usize], b"hello kqueue");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn kqueue_recv_from_reports_the_real_sender_and_send_to_reaches_it() {
    let proactor = Proactor::new(KqueuePort::new().unwrap());
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
fn kqueue_accept_reports_the_real_connecting_peer() {
    let proactor = Proactor::new(KqueuePort::new().unwrap());
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
fn kqueue_connect_reaches_a_real_listener() {
    let proactor = Proactor::new(KqueuePort::new().unwrap());
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
fn kqueue_shutdown_drains_a_still_in_flight_op_instead_of_hanging() {
    let proactor = Proactor::new(KqueuePort::new().unwrap());
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
