//! Exercise default SIGPIPE in a subprocess; never mutate the test host's policy.
#![cfg(unix)]
use loadngo_proactor::{CompletionKind, IoBuf, IoPort, IoResult, Proactor};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

#[test]
fn sends_to_closed_peers_report_errors_without_sigpipe() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sigpipe_child", "--nocapture"])
        .env("LOADNGO_SIGPIPE_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child status {:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sigpipe_child() {
    if std::env::var_os("LOADNGO_SIGPIPE_CHILD").is_none() {
        return;
    }
    // SAFETY: isolated child process; this deliberately restores the embedding
    // C application's default policy without changing the parent test process.
    unsafe {
        assert_ne!(libc::signal(libc::SIGPIPE, libc::SIG_DFL), libc::SIG_ERR);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        check(loadngo_proactor::EpollPort::new().unwrap(), false);
        check(loadngo_proactor::EpollPort::new().unwrap(), true);
    }
    #[cfg(target_os = "linux")]
    if let Ok(port) = loadngo_proactor::IoUringPort::new() {
        check(port, false);
        check(loadngo_proactor::IoUringPort::new().unwrap(), true);
    } else {
        eprintln!("io_uring unavailable; epoll remains mandatory");
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        check(loadngo_proactor::KqueuePort::new().unwrap(), false);
        check(loadngo_proactor::KqueuePort::new().unwrap(), true);
    }
}

fn check<P: IoPort>(port: P, deferred: bool) {
    let host = Proactor::new(port);
    let handle = host.handle();
    let (sender, peer) = UnixStream::pair().unwrap();
    sender.set_nonblocking(true).unwrap();
    if deferred {
        // Fill without SIGPIPE, so the proactor must arm a writable wait. The
        // queue is finite; this setup stops at actual backpressure, no sleeps.
        let bytes = [0u8; 4096];
        loop {
            // SAFETY: live socket and initialized buffer; result checked below.
            let count = unsafe {
                libc::send(
                    sender.as_raw_fd(),
                    bytes.as_ptr().cast(),
                    bytes.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            if count < 0 {
                assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
                break;
            }
            assert!(count > 0);
        }
    } else {
        drop(peer);
        return submit_and_drive(host, sender);
    }
    let seen = Arc::new(Mutex::new(None));
    let result = Arc::clone(&seen);
    handle
        .send(
            sender.as_raw_fd(),
            IoBuf::from_vec(vec![1]),
            move |r: IoResult| {
                *result.lock().unwrap() = Some(r.map(|_| ()));
            },
        )
        .unwrap();
    assert!(seen.lock().unwrap().is_none());
    drop(peer);
    drive(&host, seen);
}

fn submit_and_drive<P: IoPort>(host: Proactor<P>, sender: UnixStream) {
    let seen = Arc::new(Mutex::new(None));
    let result = Arc::clone(&seen);
    host.handle()
        .send(
            sender.as_raw_fd(),
            IoBuf::from_vec(vec![1]),
            move |r: IoResult| {
                *result.lock().unwrap() = Some(r.map(|_| ()));
            },
        )
        .unwrap();
    drive(&host, seen);
}

fn drive<P: IoPort>(host: &Proactor<P>, seen: Arc<Mutex<Option<io::Result<()>>>>) {
    let deadline = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::clone(&deadline);
    host.handle()
        .defer_for(
            Duration::from_secs(5),
            CompletionKind::Timer,
            0,
            move |_| {
                timed_out.store(true, Ordering::Release);
            },
        )
        .unwrap();
    while seen.lock().unwrap().is_none() && !deadline.load(Ordering::Acquire) {
        host.run_once().unwrap();
    }
    let error = seen
        .lock()
        .unwrap()
        .take()
        .expect("send did not complete before deadline")
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
        ),
        "{error}"
    );
}
