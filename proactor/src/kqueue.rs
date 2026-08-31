use crate::{CompletionEnvelope, CompletionPort, PollEvent, ReadinessEvent, ReadinessPort};
use libc::{
    c_int, close, kevent, kqueue, timespec, EVFILT_READ, EVFILT_USER, EV_ADD, EV_CLEAR, EV_DELETE,
    EV_ENABLE, EV_ERROR, EV_RECEIPT, NOTE_TRIGGER,
};
use std::collections::VecDeque;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::RawFd;
use std::ptr;
use std::sync::Mutex;
use std::time::Duration;

const QUEUE_IDENT: usize = 1;
const WAKE_IDENT: usize = 2;

pub struct KqueuePort {
    kq: c_int,
    queue: Mutex<VecDeque<CompletionEnvelope>>,
}

impl KqueuePort {
    pub fn new() -> io::Result<Self> {
        let kq = unsafe { kqueue() };
        if kq == -1 {
            return Err(io::Error::last_os_error());
        }

        let port = Self {
            kq,
            queue: Mutex::new(VecDeque::new()),
        };
        if let Err(err) = port.register_user_event(QUEUE_IDENT) {
            unsafe {
                close(kq);
            }
            return Err(err);
        }
        if let Err(err) = port.register_user_event(WAKE_IDENT) {
            unsafe {
                close(kq);
            }
            return Err(err);
        }
        Ok(port)
    }

    fn register_user_event(&self, ident: usize) -> io::Result<()> {
        let change = Self::user_event(ident, EV_ADD | EV_ENABLE | EV_CLEAR | EV_RECEIPT, 0);
        let mut receipt = Self::empty_event();
        let result = unsafe { kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Self::check_receipt(&receipt)
    }

    fn trigger_user_event(&self, ident: usize) -> io::Result<()> {
        let change = Self::user_event(ident, EV_ADD | EV_RECEIPT, NOTE_TRIGGER);
        let mut receipt = Self::empty_event();
        let result = unsafe { kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Self::check_receipt(&receipt)
    }

    fn drain_completion(&self) -> Option<CompletionEnvelope> {
        self.queue
            .lock()
            .expect("kqueue completion queue poisoned")
            .pop_front()
    }

    fn user_event(ident: usize, flags: impl Into<u32>, fflags: u32) -> kevent {
        kevent {
            ident: ident as _,
            filter: EVFILT_USER,
            flags: flags.into() as _,
            fflags,
            data: 0,
            udata: ptr::null_mut(),
        }
    }

    fn read_event(fd: RawFd, flags: impl Into<u32>, token: u64) -> kevent {
        kevent {
            ident: fd as _,
            filter: EVFILT_READ,
            flags: flags.into() as _,
            fflags: 0,
            data: 0,
            udata: token as usize as *mut _,
        }
    }

    fn empty_event() -> kevent {
        unsafe { MaybeUninit::<kevent>::zeroed().assume_init() }
    }

    fn check_receipt(event: &kevent) -> io::Result<()> {
        if event.flags & EV_ERROR != 0 && event.data != 0 {
            Err(io::Error::from_raw_os_error(event.data as i32))
        } else {
            Ok(())
        }
    }

    fn duration_to_timespec(duration: Duration) -> timespec {
        timespec {
            tv_sec: duration.as_secs() as _,
            tv_nsec: duration.subsec_nanos() as _,
        }
    }
}

impl CompletionPort for KqueuePort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        self.queue
            .lock()
            .expect("kqueue completion queue poisoned")
            .push_back(envelope);
        self.trigger_user_event(QUEUE_IDENT)
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        if let Some(envelope) = self.drain_completion() {
            return Ok(PollEvent::Completion(envelope));
        }

        let mut event = Self::empty_event();
        let mut timeout_storage = timeout.map(Self::duration_to_timespec);
        let timeout_ptr = timeout_storage
            .as_mut()
            .map_or(ptr::null(), |timespec| timespec as *mut _ as *const _);

        // Retries on EINTR: a stray signal delivered to this thread during
        // the blocking kevent() call (ptrace attach, SIGCHLD from an
        // unrelated part of the process, etc.) must not be allowed to
        // propagate as an error here -- see uring.rs's poll() for the
        // matching fix and how this was actually found (proactor-harness's
        // throughput bench, via an strace attach). Retrying the wait
        // itself is correct and doesn't need to resubmit anything: unlike
        // io_uring, kqueue has no separate "submission" step for this
        // call, so there's nothing that could have partially landed.
        let result = loop {
            let attempt = unsafe { kevent(self.kq, ptr::null(), 0, &mut event, 1, timeout_ptr) };
            if attempt == -1 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            break attempt;
        };
        if result == 0 {
            return Ok(PollEvent::Timeout);
        }
        if event.flags & EV_ERROR != 0 && event.data != 0 {
            return Err(io::Error::from_raw_os_error(event.data as i32));
        }

        if event.filter == EVFILT_USER {
            if event.ident == QUEUE_IDENT {
                if let Some(envelope) = self.drain_completion() {
                    return Ok(PollEvent::Completion(envelope));
                }
                return Ok(PollEvent::Wake);
            }
            if event.ident == WAKE_IDENT {
                return Ok(PollEvent::Wake);
            }
        }

        if event.filter == EVFILT_READ {
            return Ok(PollEvent::Readiness(ReadinessEvent {
                token: event.udata as usize as u64,
            }));
        }

        Ok(PollEvent::Wake)
    }

    fn wake(&self) -> io::Result<()> {
        self.trigger_user_event(WAKE_IDENT)
    }
}

impl ReadinessPort for KqueuePort {
    fn register_readable(&self, fd: RawFd, token: u64) -> io::Result<()> {
        let change = Self::read_event(fd, EV_ADD | EV_ENABLE | EV_CLEAR | EV_RECEIPT, token);
        let mut receipt = Self::empty_event();
        let result = unsafe { kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Self::check_receipt(&receipt)
    }

    fn deregister(&self, fd: RawFd) -> io::Result<()> {
        let change = Self::read_event(fd, EV_DELETE | EV_RECEIPT, 0);
        let mut receipt = Self::empty_event();
        let result = unsafe { kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Self::check_receipt(&receipt)
    }
}

impl Drop for KqueuePort {
    fn drop(&mut self) {
        unsafe {
            close(self.kq);
        }
    }
}
