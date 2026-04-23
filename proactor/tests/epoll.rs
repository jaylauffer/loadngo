#![cfg(target_os = "linux")]

use loadngo_proactor::{Completion, CompletionKind, EpollPort, Proactor, ReadinessEvent};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
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
fn epoll_dispatches_burst_enqueued_work_without_lost_wakeup() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
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
fn epoll_stop_wakes_and_ends_loop() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
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
fn epoll_dispatches_hup_for_registered_readiness() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();
    let (tx, rx) = mpsc::channel();
    let mut pipe_fds = [0; 2];

    let rc = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(rc, 0);

    handle
        .register_readable(pipe_fds[0], 88, move |readiness: ReadinessEvent| {
            tx.send(readiness.token).unwrap();
        })
        .unwrap();

    let worker = thread::spawn(move || proactor.run_once().unwrap());
    thread::sleep(Duration::from_millis(25));

    unsafe {
        libc::close(pipe_fds[1]);
    }

    let report = worker.join().unwrap();
    assert!(report.woke);
    assert_eq!(rx.recv_timeout(Duration::from_millis(100)).unwrap(), 88);

    handle.deregister_readable(pipe_fds[0], 88).unwrap();
    unsafe {
        libc::close(pipe_fds[0]);
    }
}

#[test]
fn epoll_stop_interrupts_continuous_readiness() {
    let proactor = Proactor::new(EpollPort::new().unwrap());
    let handle = proactor.handle();
    let mut pipe_fds = [0; 2];
    let ready_count = Arc::new(AtomicUsize::new(0));

    let rc = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(rc, 0);

    let ready_count_handler = Arc::clone(&ready_count);
    handle
        .register_readable(pipe_fds[0], 99, move |_readiness: ReadinessEvent| {
            ready_count_handler.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();

    let worker = thread::spawn(move || {
        let started = Instant::now();
        proactor.run_until_stopped().unwrap();
        started.elapsed()
    });

    let value = [1u8; 1];
    let written = unsafe { libc::write(pipe_fds[1], value.as_ptr() as *const _, value.len()) };
    assert_eq!(written, 1);

    let wait_started = Instant::now();
    while ready_count.load(Ordering::Relaxed) == 0 {
        thread::sleep(Duration::from_millis(1));
        assert!(wait_started.elapsed() < Duration::from_secs(1));
    }

    thread::sleep(Duration::from_millis(20));
    handle.stop().unwrap();

    let elapsed = worker.join().unwrap();
    assert!(elapsed < Duration::from_secs(1));
    assert!(ready_count.load(Ordering::Relaxed) > 0);

    handle.deregister_readable(pipe_fds[0], 99).unwrap();
    unsafe {
        libc::close(pipe_fds[0]);
        libc::close(pipe_fds[1]);
    }
}
