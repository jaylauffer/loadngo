//! On-device speech: recognition from the microphone or a file, and synthesis to the
//! default output. macOS uses Apple's Speech framework with on-device recognition only
//! (nothing leaves the machine; the models run on the Neural Engine) and AVFAudio's
//! speech synthesizer. Other platforms report [`Error::Unsupported`].
//!
//! Recognition results arrive on the framework's own queue and are handed over through
//! a channel; [`Listener::next_utterance`] waits on it with a deadline (silence ends an
//! utterance), so nothing here polls or owns a timer thread.
#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod apple;

#[cfg(target_os = "macos")]
pub use apple::{request_authorization, transcribe_file, Listener, Speaker};

/// Why speech could not start or continue.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("speech is not supported on this platform")]
    Unsupported,
    #[error("speech recognition permission: {0}")]
    NotAuthorized(String),
    #[error("on-device recognition is not available for {0}")]
    NoOnDeviceModel(String),
    #[error("{0}")]
    Framework(String),
}
