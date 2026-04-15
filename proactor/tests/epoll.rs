#![cfg(target_os = "linux")]

use loadngo_proactor::{Completion, CompletionKind, EpollPort, Proactor, ReadinessEvent};
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
