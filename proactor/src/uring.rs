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
}

impl IoUringPort {
    pub fn new() -> io::Result<Self> {
        let ring = IoUring::new(MAX_EVENTS as u32)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        Ok(Self {
            ring: Mutex::new(ring),
            queue: Mutex::new(VecDeque::new()),
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
        // Check queue first (non-blocking)
        if let Some(envelope) = self.drain_completion() {
            return Ok(PollEvent::Completion(envelope));
        }

        let mut ring = self.ring.lock().map_err(|e|
            io::Error::new(io::ErrorKind::Other, format!("lock poisoned: {}", e))
        )?;

        // Wait for completion events
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

        // Process completion queue entries
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
                WAKE_TOKEN => Ok(PollEvent::Wake),
                _ => Ok(PollEvent::Wake),
            }
        } else {
            Ok(PollEvent::Timeout)
        }
    }

    fn wake(&self) -> io::Result<()> {
        self.signal_wake(WAKE_TOKEN)
    }
}