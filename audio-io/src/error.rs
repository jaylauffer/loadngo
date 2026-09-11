//! Error type shared by the backends, [`crate::devices`], and [`crate::monitor`]. Kept in
//! its own module (rather than inline in `monitor.rs`) since both import it
//! independently.

#[derive(Debug, thiserror::Error)]
pub enum AudioIoError {
    #[error("input device '{0}' was not found")]
    InputDeviceNotFound(String),
    #[error("output device '{0}' was not found")]
    OutputDeviceNotFound(String),
    #[error("no default input device is available")]
    NoDefaultInputDevice,
    #[error("no default output device is available")]
    NoDefaultOutputDevice,
    #[error("unsupported sample format: {0}")]
    UnsupportedSampleFormat(String),
    #[error("audio device query failed: {0}")]
    Backend(String),
    #[error("audio stream setup failed: {0}")]
    Stream(String),
    #[error("failed to start the audio monitor worker thread: {0}")]
    Thread(String),
    #[error("the audio monitor worker thread exited before it signaled ready")]
    WorkerExited,
    #[error("the device does not offer the physical format {0}")]
    UnsupportedPhysicalFormat(String),
    #[error("{0} is not supported on this platform")]
    UnsupportedOnPlatform(&'static str),
}
