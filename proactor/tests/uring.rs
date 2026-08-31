#![cfg(target_os = "linux")]

use loadngo_proactor::{
    AcceptResult, Completion, CompletionKind, IoBuf, IoResult, IoUringPort, Proactor,
    ReadinessEvent,
};
use std::net::{TcpListener, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn uring_dispatches_enqueued_work() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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
fn uring_dispatches_burst_enqueued_work_without_lost_wakeup() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
    let handle = proactor.handle();
    let (tx, rx) = mpsc::channel();

    for value in [1u8, 2, 3] {
        let tx = tx.clone();
        handle
            .enqueue_work(move |_completion: Completion| {
                tx.send(value).unwrap();
            })
            .unwrap();
    }
    drop(tx);

    let start = Instant::now();
    let mut dispatched = 0;
    while dispatched < 3 {
        let report = proactor.run_ready().unwrap();
        dispatched += report.dispatched_completions;
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    assert_eq!(rx.recv_timeout(Duration::from_millis(100)).unwrap(), 1);
    assert_eq!(rx.recv_timeout(Duration::from_millis(100)).unwrap(), 2);
    assert_eq!(rx.recv_timeout(Duration::from_millis(100)).unwrap(), 3);
}

#[test]
fn uring_wake_interrupts_blocking_poll() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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
fn uring_stop_wakes_and_ends_loop() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
    let handle = proactor.handle();

    let worker = thread::spawn(move || {
        let started = Instant::now();
        proactor.run_until_stopped().unwrap();
        started.elapsed()
    });

    thread::sleep(Duration::from_millis(25));
    handle.stop().unwrap();

    let elapsed = worker.join().unwrap();
    assert!(elapsed < Duration::from_secs(1));
    assert!(!handle.is_running());
}

#[test]
fn uring_dispatches_registered_readiness() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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
fn uring_write_then_read_round_trip_a_real_file() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
    let handle = proactor.handle();

    let path = std::env::temp_dir().join(format!(
        "loadngo-proactor-uring-test-{}-{:?}",
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
            IoBuf::from_vec(b"hello io_uring".to_vec()),
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
    assert_eq!(written as usize, b"hello io_uring".len());

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
    assert_eq!(n as usize, b"hello io_uring".len());
    assert_eq!(&buf[..n as usize], b"hello io_uring");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn uring_recv_from_reports_the_real_sender_and_send_to_reaches_it() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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
fn uring_accept_reports_the_real_connecting_peer() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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
fn uring_shutdown_drains_a_still_in_flight_op_instead_of_hanging() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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
