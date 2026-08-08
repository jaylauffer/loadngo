use crate::{CompletionEnvelope, CompletionPort, PollEvent};
use io_uring::{opcode, types, IoUring};
use libc::{c_int, timespec, POLLERR, POLLHUP, POLLIN, POLLRDHUP};
use std::collections::VecDeque;
use std::io;
use std::os::fd::RawFd;
use std::sync::Mutex;
use std::time::Duration;

const QUEUE_TOKEN: u64 = 1;
const WAKE_TOKEN: u64 = 2;
const MAX_EVENTS: usize = 256;

pub struct IoUringPort {
    ring: Mutex<IoUring>,
    queue: Mutex<VecDeque<CompletionEnvelope>>,
    wake_fd: RawFd,  // eventfd for waking
}

impl IoUringPort {
    pub fn new() -> io::Result<Self> {
        let ring = IoUring::new(MAX_EVENTS as u32)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let wake_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if wake_fd == -1 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            ring: Mutex::new(ring),
            queue: Mutex::new(VecDeque::new()),
            wake_fd,
        })
    }

    fn drain_completion(&self) -> Option<CompletionEnvelope> {
        self.queue
            .lock()
            .expect("io_uring completion queue poisoned")
            .pop_front()
    }

    fn duration_to_timespec(duration: Duration) -> timespec {
        timespec {
            tv_sec: duration.as_secs() as _,
            tv_nsec: duration.subsec_nanos() as _,
        }
    }

    fn signal_wake(&self, token: u64) -> io::Result<()> {
        let mut ring = self.ring.lock().map_err(|e| 
            io::Error::new(io::ErrorKind::Other, format!("lock poisoned: {}", e))
        )?;

        let noop = opcode::Nop::new()
            .build()
            .user_data(token);

        unsafe {
            ring.submission()
                .push(&noop)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "submission queue full"))?;
        }

        ring.submit()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        Ok(())
    }

        fn signal_eventfd(fd: RawFd) -> io::Result<()> {
        let value: u64 = 1;
        let rc = unsafe {
            libc::write(fd, &value as *const u64 as *const libc::c_void, std::mem::size_of::<u64>())
        };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn clear_eventfd(fd: RawFd) -> io::Result<()> {
        loop {
            let mut value = 0u64;
            let rc = unsafe {
                libc::read(fd, &mut value as *mut u64 as *mut libc::c_void, std::mem::size_of::<u64>())
            };
            if rc == -1 {
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EAGAIN) => return Ok(()),
                    Some(libc::EINTR) => continue,
                    _ => return Err(err),
                }
            }
            if rc == 0 {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
        }
    }
}

impl CompletionPort for IoUringPort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        self.queue
            .lock()
            .expect("io_uring completion queue poisoned")
            .push_back(envelope);
        
        // Signal via IORING_OP_NOP with QUEUE_TOKEN
        self.signal_wake(QUEUE_TOKEN)
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        if let Some(envelope) = self.drain_completion() {
            return Ok(PollEvent::Completion(envelope));
        }

        let mut ring = self.ring.lock().map_err(|e|
            io::Error::new(io::ErrorKind::Other, format!("lock poisoned: {}", e))
        )?;

        // Register eventfd for poll if not already registered
        // We need to submit a POLL_ADD for wake_fd
        let poll_op = opcode::PollAdd::new(types::Fd(self.wake_fd), libc::POLLIN as u32)
            .build()
            .user_data(WAKE_TOKEN);

        unsafe {
            ring.submission()
                .push(&poll_op)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "submission queue full"))?;
        }

        ring.submit()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        // Wait for events
        let result = if let Some(duration) = timeout {
            let ts = Self::duration_to_timespec(duration);
            let uring_ts = types::Timespec::new()
                .sec(ts.tv_sec as _)
                .nsec(ts.tv_nsec as _);
            let args = types::SubmitArgs::new().timespec(&uring_ts);
            ring.submitter().submit_with_args(1, &args)
        } else {
            ring.submit_and_wait(1)
        };

        match result {
            Ok(_) => {},
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                return Ok(PollEvent::Timeout);
            }
            Err(e) => return Err(e),
        }

        // Process completion queue
        let mut cq = ring.completion();
        if let Some(cqe) = cq.next() {
            match cqe.user_data() {
                QUEUE_TOKEN => {
                    if let Some(envelope) = self.drain_completion() {
                        Ok(PollEvent::Completion(envelope))
                    } else {
                        Ok(PollEvent::Wake)
                    }
                }
                WAKE_TOKEN => {
                    Self::clear_eventfd(self.wake_fd)?;
                    Ok(PollEvent::Wake)
                }
                _ => Ok(PollEvent::Wake),
            }
        } else {
            Ok(PollEvent::Timeout)
        }
    }

    fn wake(&self) -> io::Result<()> {
        Self::signal_eventfd(self.wake_fd)
    }
}