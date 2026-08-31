use crate::{CompletionEnvelope, CompletionPort, PollEvent};
use std::collections::VecDeque;
use std::io;
use std::ptr;
use std::sync::Mutex;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, PostQueuedCompletionStatus, OVERLAPPED,
};

const QUEUE_KEY: usize = 1;
const WAKE_KEY: usize = 2;
const WAIT_TIMEOUT_ERROR: u32 = 258;
const INFINITE_TIMEOUT_MS: u32 = u32::MAX;

pub struct IocpPort {
    handle: HANDLE,
    queue: Mutex<VecDeque<CompletionEnvelope>>,
}

// SAFETY: `HANDLE` wraps a raw `*mut c_void`, which is `!Send`/`!Sync` by
// default -- but a Windows I/O completion port handle is specifically
// designed to be shared and called into (`GetQueuedCompletionStatus`,
// `PostQueuedCompletionStatus`) from multiple threads concurrently; that
// concurrent use is the documented, intended way to use one, not a
// hazard this needs to guard against. Found (pre-existing, not introduced
// by this change) via `cargo check --target x86_64-pc-windows-msvc`,
// which is otherwise unavailable without a real Windows machine to build
// on -- without these impls, `IocpPort` cannot satisfy `CompletionPort:
// Send + Sync` at all, so this crate has likely never actually compiled
// for Windows successfully.
unsafe impl Send for IocpPort {}
unsafe impl Sync for IocpPort {}

impl IocpPort {
    pub fn new() -> io::Result<Self> {
        let handle = unsafe {
            CreateIoCompletionPort(INVALID_HANDLE_VALUE, HANDLE::default(), 0, 0)
                .map_err(|err| io::Error::other(err.to_string()))?
        };
        Ok(Self {
            handle,
            queue: Mutex::new(VecDeque::new()),
        })
    }

    fn drain_completion(&self) -> Option<CompletionEnvelope> {
        self.queue
            .lock()
            .expect("iocp completion queue poisoned")
            .pop_front()
    }

    fn duration_to_timeout_ms(duration: Option<Duration>) -> u32 {
        match duration {
            Some(duration) => duration.as_millis().min(u32::MAX as u128) as u32,
            None => INFINITE_TIMEOUT_MS,
        }
    }
}

impl CompletionPort for IocpPort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        self.queue
            .lock()
            .expect("iocp completion queue poisoned")
            .push_back(envelope);
        unsafe {
            PostQueuedCompletionStatus(self.handle, 0, QUEUE_KEY, None)
                .map_err(|err| io::Error::other(err.to_string()))
        }
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        if let Some(envelope) = self.drain_completion() {
            return Ok(PollEvent::Completion(envelope));
        }

        let mut bytes_transferred = 0u32;
        let mut completion_key = 0usize;
        let mut overlapped: *mut OVERLAPPED = ptr::null_mut();
        let timeout_ms = Self::duration_to_timeout_ms(timeout);

        let result = unsafe {
            GetQueuedCompletionStatus(
                self.handle,
                &mut bytes_transferred,
                &mut completion_key,
                &mut overlapped,
                timeout_ms,
            )
        };

        if let Err(err) = result {
            let last_error = unsafe { GetLastError().0 };
            if last_error == WAIT_TIMEOUT_ERROR {
                return Ok(PollEvent::Timeout);
            }
            return Err(io::Error::other(err.to_string()));
        }

        match completion_key {
            QUEUE_KEY => {
                if let Some(envelope) = self.drain_completion() {
                    Ok(PollEvent::Completion(envelope))
                } else {
                    Ok(PollEvent::Wake)
                }
            }
            WAKE_KEY => Ok(PollEvent::Wake),
            _ => Ok(PollEvent::Wake),
        }
    }

    fn wake(&self) -> io::Result<()> {
        unsafe {
            PostQueuedCompletionStatus(self.handle, 0, WAKE_KEY, None)
                .map_err(|err| io::Error::other(err.to_string()))
        }
    }
}

impl Drop for IocpPort {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
