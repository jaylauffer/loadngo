use crate::{CompletionEnvelope, CompletionPort, PollEvent};
use libc::{
    c_int, close, kevent, kqueue, timespec, EVFILT_USER, EV_ADD, EV_CLEAR, EV_ENABLE, EV_ERROR,
    EV_RECEIPT, NOTE_TRIGGER,
};
use std::collections::VecDeque;
use std::io;
use std::mem::MaybeUninit;
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

    fn user_event(ident: usize, flags: u16, fflags: u32) -> kevent {
        kevent {
            ident: ident as _,
            filter: EVFILT_USER,
            flags,
            fflags,
            data: 0,
            udata: ptr::null_mut(),
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

        let result = unsafe { kevent(self.kq, ptr::null(), 0, &mut event, 1, timeout_ptr) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        if result == 0 {
            return Ok(PollEvent::Timeout);
        }
        if event.flags & EV_ERROR != 0 && event.data != 0 {
            return Err(io::Error::from_raw_os_error(event.data as i32));
        }

        if event.filter == EVFILT_USER {
            if event.ident as usize == QUEUE_IDENT {
                if let Some(envelope) = self.drain_completion() {
                    return Ok(PollEvent::Completion(envelope));
                }
                return Ok(PollEvent::Wake);
            }
            if event.ident as usize == WAKE_IDENT {
                return Ok(PollEvent::Wake);
            }
        }

        Ok(PollEvent::Wake)
    }

    fn wake(&self) -> io::Result<()> {
        self.trigger_user_event(WAKE_IDENT)
    }
}

impl Drop for KqueuePort {
    fn drop(&mut self) {
        unsafe {
            close(self.kq);
        }
    }
}
