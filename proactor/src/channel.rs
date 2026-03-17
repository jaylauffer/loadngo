use crate::{CompletionEnvelope, CompletionPort, PollEvent};
use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Mutex;
use std::time::Duration;

pub struct ChannelPort {
    sender: Sender<ChannelMessage>,
    receiver: Mutex<Receiver<ChannelMessage>>,
}

enum ChannelMessage {
    Completion(CompletionEnvelope),
    Wake,
}

impl ChannelPort {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver: Mutex::new(receiver),
        }
    }
}

impl Default for ChannelPort {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionPort for ChannelPort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        self.sender
            .send(ChannelMessage::Completion(envelope))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "channel port closed"))
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        let receiver = self.receiver.lock().expect("channel receiver poisoned");
        let message = match timeout {
            Some(duration) if duration.is_zero() => match receiver.try_recv() {
                Ok(message) => Some(message),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "channel port disconnected",
                    ))
                }
            },
            Some(duration) => match receiver.recv_timeout(duration) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "channel port disconnected",
                    ))
                }
            },
            None => Some(
                receiver
                    .recv()
                    .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "channel port disconnected"))?,
            ),
        };

        Ok(match message {
            Some(ChannelMessage::Completion(envelope)) => PollEvent::Completion(envelope),
            Some(ChannelMessage::Wake) => PollEvent::Wake,
            None => PollEvent::Timeout,
        })
    }

    fn wake(&self) -> io::Result<()> {
        self.sender
            .send(ChannelMessage::Wake)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "channel port closed"))
    }
}
