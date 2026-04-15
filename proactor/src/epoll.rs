use crate::{CompletionEnvelope, CompletionPort, PollEvent};
use libc::{
    c_int, close, epoll_create1, epoll_ctl, epoll_event, epoll_wait, eventfd, read, write,
    EFD_CLOEXEC, EFD_NONBLOCK, EINTR, EINVAL, EPOLLIN, EPOLL_CLOEXEC, EPOLL_CTL_ADD,
};
use std::collections::VecDeque;
use std::io;
use std::mem::MaybeUninit;
use std::sync::Mutex;
use std::time::Duration;

const QUEUE_TOKEN: u64 = 1;
const WAKE_TOKEN: u64 = 2;
const MAX_EVENTS: usize = 1;

pub struct EpollPort {
    epoll_fd: c_int,
    queue_fd: c_int,
    wake_fd: c_int,
    queue: Mutex<VecDeque<CompletionEnvelope>>,
}

impl EpollPort {
    pub fn new() -> io::Result<Self> {
        let epoll_fd = unsafe { epoll_create1(EPOLL_CLOEXEC) };
        if epoll_fd == -1 {
            return Err(io::Error::last_os_error());
        }

        let queue_fd = match Self::create_eventfd() {
            Ok(fd) => fd,
            Err(err) => {
                unsafe {
                    close(epoll_fd);
                }
                return Err(err);
            }
        };

        let wake_fd = match Self::create_eventfd() {
            Ok(fd) => fd,
            Err(err) => {
                unsafe {
                    close(queue_fd);
                    close(epoll_fd);
                }
                return Err(err);
            }
        };

        let port = Self {
            epoll_fd,
            queue_fd,
            wake_fd,
            queue: Mutex::new(VecDeque::new()),
        };

        if let Err(err) = port.register_fd(queue_fd, QUEUE_TOKEN) {
            unsafe {
                close(wake_fd);
                close(queue_fd);
                close(epoll_fd);
            }
            return Err(err);
        }

        if let Err(err) = port.register_fd(wake_fd, WAKE_TOKEN) {
            unsafe {
                close(wake_fd);
                close(queue_fd);
                close(epoll_fd);
            }
            return Err(err);
        }

        Ok(port)
    }

    fn create_eventfd() -> io::Result<c_int> {
        let fd = unsafe { eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK) };
        if fd == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(fd)
        }
    }

    fn register_fd(&self, fd: c_int, token: u64) -> io::Result<()> {
        let mut event = Self::event(EPOLLIN as u32, token);
        let rc = unsafe { epoll_ctl(self.epoll_fd, EPOLL_CTL_ADD, fd, &mut event) };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn drain_completion(&self) -> Option<CompletionEnvelope> {
        self.queue
            .lock()
            .expect("epoll completion queue poisoned")
            .pop_front()
    }

    fn signal(fd: c_int) -> io::Result<()> {
        let value: u64 = 1;
        let rc = unsafe {
            write(
                fd,
                &value as *const u64 as *const libc::c_void,
                std::mem::size_of::<u64>(),
            )
        };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn clear_signal(fd: c_int) -> io::Result<()> {
        loop {
            let mut value = 0u64;
            let rc = unsafe {
                read(
                    fd,
                    &mut value as *mut u64 as *mut libc::c_void,
                    std::mem::size_of::<u64>(),
                )
            };
            if rc == -1 {
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EAGAIN) => return Ok(()),
                    Some(EINTR) => continue,
                    _ => return Err(err),
                }
            }
            if rc == 0 {
                return Err(io::Error::from_raw_os_error(EINVAL));
            }
        }
    }

    fn timeout_to_epoll_ms(timeout: Option<Duration>) -> c_int {
        match timeout {
            None => -1,
            Some(duration) => duration.as_millis().min(c_int::MAX as u128) as c_int,
        }
    }

    fn event(events: u32, token: u64) -> epoll_event {
        let mut event = unsafe { MaybeUninit::<epoll_event>::zeroed().assume_init() };
        event.events = events;
        event.u64 = token;
        event
    }
}

impl CompletionPort for EpollPort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        self.queue
            .lock()
            .expect("epoll completion queue poisoned")
            .push_back(envelope);
        Self::signal(self.queue_fd)
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        if let Some(envelope) = self.drain_completion() {
            return Ok(PollEvent::Completion(envelope));
        }

        let mut events = [Self::event(0, 0); MAX_EVENTS];
        let timeout_ms = Self::timeout_to_epoll_ms(timeout);

        loop {
            let rc = unsafe {
                epoll_wait(
                    self.epoll_fd,
                    events.as_mut_ptr(),
                    MAX_EVENTS as c_int,
                    timeout_ms,
                )
            };

            if rc == -1 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(EINTR) {
                    continue;
                }
                return Err(err);
            }

            if rc == 0 {
                return Ok(PollEvent::Timeout);
            }

            let event = events[0];
            match event.u64 {
                QUEUE_TOKEN => {
                    Self::clear_signal(self.queue_fd)?;
                    if let Some(envelope) = self.drain_completion() {
                        return Ok(PollEvent::Completion(envelope));
                    }
                    return Ok(PollEvent::Wake);
                }
                WAKE_TOKEN => {
                    Self::clear_signal(self.wake_fd)?;
                    return Ok(PollEvent::Wake);
                }
                _ => return Ok(PollEvent::Wake),
            }
        }
    }

    fn wake(&self) -> io::Result<()> {
        Self::signal(self.wake_fd)
    }
}

impl Drop for EpollPort {
    fn drop(&mut self) {
        unsafe {
            let _ = close(self.wake_fd);
            let _ = close(self.queue_fd);
            let _ = close(self.epoll_fd);
        }
    }
}
