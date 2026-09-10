//! Platform-agnostic webcam capture for `loadngo` desktop hosts.
//!
//! Capture is `ffmpeg`-backed on every platform: one child process per
//! stream, emitting `rawvideo`/`rgba` frames on stdout at a fixed size, so
//! everything above the process boundary -- frame slicing, restart policy,
//! preview, saving -- is identical everywhere. Only two things genuinely
//! differ per platform, and both live in [`device`]:
//!
//! - **which input format `ffmpeg` needs** (`v4l2`, `avfoundation`, `dshow`)
//! - **how cameras are enumerated** (`/dev/video*` plus sysfs labels,
//!   `avfoundation`'s device list, DirectShow's device list)
//!
//! # Draining a stream
//!
//! [`CaptureStream::pump`] is the single entry point for turning bytes into
//! frames, and how you drive it is the one place the host still has to care
//! about the platform:
//!
//! - **Unix** (Linux, macOS): the capture pipe is non-blocking, so register
//!   [`CaptureStream::stdout_fd`] with a `loadngo_proactor::ReadinessPort`
//!   (`IoUringPort`, `KqueuePort`, `EpollPort`) and call `pump` on
//!   readiness. `pump` drains until the pipe would block. No extra threads.
//! - **Windows**: IOCP has no readiness model for an anonymous pipe --
//!   `ReadinessPort` is `RawFd`-based and `IocpPort` does not implement it
//!   -- so the pipe stays blocking, `pump` returns after a single read, and
//!   the caller runs it on a reader thread, handing frames back to the
//!   proactor with `ProactorHandle::enqueue_work`. Frames still arrive on
//!   the proactor thread, so consumer code is unchanged.
//!
//! This crate deliberately does not depend on `loadngo-proactor`: it hands
//! out a file descriptor and a pump, and lets the host decide how it is
//! driven. That keeps it usable from a non-proactor caller (see
//! `sng-rusty`'s motion detector, which only wants single frames).

use std::io::{ErrorKind, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

pub mod device;

pub use device::{default_device, list_devices, CameraDevice};

/// Frame size used when a caller does not specify one.
pub const DEFAULT_VIDEO_SIZE: &str = "1280x720";

/// Windows keeps the capture pipe blocking (see the module docs), so `pump`
/// must return after one read there instead of looping to `WouldBlock`.
const PUMP_READS_UNTIL_WOULD_BLOCK: bool = !cfg!(windows);

#[derive(Debug, thiserror::Error)]
pub enum CameraError {
    #[error("failed to start ffmpeg: {0}")]
    Spawn(String),
    #[error("ffmpeg is required for camera capture but was not usable: {0}")]
    FfmpegUnavailable(String),
    #[error("invalid video size {value:?}: {reason}")]
    VideoSize { value: String, reason: String },
    #[error("camera stream setup failed: {0}")]
    Stream(String),
    #[error("camera enumeration failed: {0}")]
    Enumerate(String),
    #[error("camera capture is not supported on this platform")]
    Unsupported,
}

/// Fails closed on platforms with no ffmpeg capture path (iOS, Android), so
/// no caller can reach [`device::input_format`] there. See
/// [`device::is_supported`].
fn ensure_supported() -> Result<(), CameraError> {
    if device::is_supported() {
        Ok(())
    } else {
        Err(CameraError::Unsupported)
    }
}

/// How to open a camera. `device` is a platform-native identifier as
/// produced by [`list_devices`] -- a `/dev/video*` path on Linux, an
/// `avfoundation` index or name on macOS, a DirectShow name on Windows --
/// so callers should carry [`CameraDevice::id`] through rather than
/// building one by hand.
#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub device: String,
    pub video_size: Option<String>,
    pub frame_rate: u32,
}

impl CaptureConfig {
    #[must_use]
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            video_size: None,
            frame_rate: 6,
        }
    }

    /// Frame dimensions this configuration will produce. Capture always
    /// scales to exactly this size, so the frame length is fixed and frames
    /// can be sliced out of the byte stream without parsing them.
    pub fn frame_dimensions(&self) -> Result<(u32, u32), CameraError> {
        let value = self
            .video_size
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(DEFAULT_VIDEO_SIZE);
        parse_video_size(value)
    }

    /// Bytes in one RGBA frame at [`Self::frame_dimensions`].
    pub fn frame_len(&self) -> Result<usize, CameraError> {
        let (width, height) = self.frame_dimensions()?;
        Ok(width as usize * height as usize * 4)
    }
}

/// Parses a `WIDTHxHEIGHT` spec.
pub fn parse_video_size(value: &str) -> Result<(u32, u32), CameraError> {
    let invalid = |reason: &str| CameraError::VideoSize {
        value: value.to_string(),
        reason: reason.to_string(),
    };
    let (width, height) = value
        .trim()
        .split_once('x')
        .ok_or_else(|| invalid("expected WIDTHxHEIGHT"))?;
    let width = width
        .parse::<u32>()
        .map_err(|_| invalid("width is not a number"))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| invalid("height is not a number"))?;
    if width == 0 || height == 0 {
        return Err(invalid("dimensions must be positive"));
    }
    Ok((width, height))
}

/// Base `ffmpeg` invocation for `config`, up to and including `-i <device>`.
/// Shared by streaming and single-frame capture so they can never drift on
/// input format or device naming.
fn base_command(config: &CaptureConfig) -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    command.args(["-fflags", "nobuffer"]);
    command.args(["-f", device::input_format()]);
    command.args([
        "-framerate",
        &device::input_framerate(config.frame_rate).to_string(),
    ]);
    if let Some(video_size) = config
        .video_size
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        command.args(["-video_size", video_size]);
    }
    command.args(["-i", &device::input_argument(&config.device)]);
    command
}

/// Captures exactly one frame and returns the encoded bytes in `format`
/// (`"png"`, `"mjpeg"`, ...). Used for non-streaming callers -- one-shot
/// capture, and periodic polling like `sng-rusty`'s motion detector -- so
/// they do not each reimplement the platform input flags.
pub fn capture_single_frame(config: &CaptureConfig, format: &str) -> Result<Vec<u8>, CameraError> {
    ensure_supported()?;
    let mut command = base_command(config);
    command.args(["-frames:v", "1", "-f", format, "pipe:1"]);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let output = command
        .output()
        .map_err(|err| CameraError::Spawn(err.to_string()))?;
    if !output.status.success() {
        return Err(CameraError::Stream(format!(
            "ffmpeg exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.is_empty() {
        return Err(CameraError::Stream(
            "ffmpeg produced no frame data".to_string(),
        ));
    }
    Ok(output.stdout)
}

/// Splits every complete frame out of `pending`, leaving any partial tail.
fn split_frames(pending: &mut Vec<u8>, frame_len: usize) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    if frame_len == 0 {
        return frames;
    }
    while pending.len() >= frame_len {
        let rest = pending.split_off(frame_len);
        frames.push(std::mem::replace(pending, rest));
    }
    frames
}

/// What one [`CaptureStream::pump`] produced.
#[derive(Debug, Default)]
pub struct PumpResult {
    /// Complete RGBA frames, oldest first.
    pub frames: Vec<Vec<u8>>,
    /// `Some` once the stream is finished: EOF or a read failure. The
    /// caller should tear the stream down and (if it wants) restart.
    pub ended: Option<String>,
}

/// Why a stream stopped, for diagnostics after [`CaptureStream::finish`].
#[derive(Debug)]
pub struct StreamOutcome {
    pub status_text: String,
    pub stderr_text: String,
    /// Whether the stream ever produced a frame. A stream that dies without
    /// one usually means a bad device or an unsupported size, rather than a
    /// transient failure worth retrying quietly.
    pub delivered_frame: bool,
}

/// A running `ffmpeg` capture producing fixed-size RGBA frames.
pub struct CaptureStream {
    child: Child,
    stdout: ChildStdout,
    frame_width: u32,
    frame_height: u32,
    frame_len: usize,
    pending: Vec<u8>,
    scratch: Vec<u8>,
    delivered_frame: bool,
    stderr_text: Arc<Mutex<String>>,
    stderr_join: Option<JoinHandle<()>>,
}

impl CaptureStream {
    /// Starts capture. On Unix the pipe is put into non-blocking mode so
    /// [`Self::pump`] can be driven from a readiness callback.
    pub fn start(config: &CaptureConfig) -> Result<Self, CameraError> {
        ensure_supported()?;
        let (frame_width, frame_height) = config.frame_dimensions()?;
        let frame_len = config.frame_len()?;

        let mut command = base_command(config);
        // Scale in ffmpeg so every frame is exactly `frame_len` bytes,
        // whatever the camera actually negotiated.
        let filter = format!(
            "fps={},scale={}x{}",
            config.frame_rate.max(1),
            frame_width,
            frame_height
        );
        command.args([
            "-an", "-vf", &filter, "-pix_fmt", "rgba", "-f", "rawvideo", "pipe:1",
        ]);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|err| CameraError::Spawn(err.to_string()))?;

        let stderr_text = Arc::new(Mutex::new(String::new()));
        let stderr_sink = Arc::clone(&stderr_text);
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| CameraError::Stream("ffmpeg stderr unavailable".to_string()))?;
        let stderr_join = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            if let Ok(mut sink) = stderr_sink.lock() {
                *sink = String::from_utf8_lossy(&bytes).trim().to_string();
            }
        });

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CameraError::Stream("ffmpeg stdout unavailable".to_string()))?;
        #[cfg(unix)]
        set_nonblocking(&stdout)?;

        Ok(Self {
            child,
            stdout,
            frame_width,
            frame_height,
            frame_len,
            pending: Vec::new(),
            scratch: vec![0u8; 64 * 1024],
            delivered_frame: false,
            stderr_text,
            stderr_join: Some(stderr_join),
        })
    }

    /// The capture pipe's descriptor, for registering readiness with a
    /// `loadngo_proactor::ReadinessPort`. Unix only -- Windows has no
    /// equivalent for anonymous pipes; see the module docs.
    #[cfg(unix)]
    #[must_use]
    pub fn stdout_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.stdout.as_raw_fd()
    }

    #[must_use]
    pub fn frame_dimensions(&self) -> (u32, u32) {
        (self.frame_width, self.frame_height)
    }

    #[must_use]
    pub fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// Reads whatever is available and slices out complete frames.
    ///
    /// On Unix this drains until the pipe would block, so it is safe to call
    /// from a readiness callback. On Windows it performs one blocking read
    /// and returns, so it is safe to call in a loop on a reader thread.
    pub fn pump(&mut self) -> PumpResult {
        let mut result = PumpResult::default();
        loop {
            match self.stdout.read(&mut self.scratch) {
                Ok(0) => {
                    result.ended =
                        Some("camera stream ended (ffmpeg closed its output)".to_string());
                    break;
                }
                Ok(read) => {
                    self.pending.extend_from_slice(&self.scratch[..read]);
                    let frames = split_frames(&mut self.pending, self.frame_len);
                    if !frames.is_empty() {
                        self.delivered_frame = true;
                    }
                    result.frames.extend(frames);
                    if !PUMP_READS_UNTIL_WOULD_BLOCK {
                        break;
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => {
                    result.ended = Some(format!("camera stream read failed: {err}"));
                    break;
                }
            }
        }
        result
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Reaps the child and collects its diagnostics.
    pub fn finish(mut self) -> StreamOutcome {
        let status_text = match self.child.wait() {
            Ok(status) => status.to_string(),
            Err(err) => format!("failed to wait for ffmpeg: {err}"),
        };
        if let Some(join) = self.stderr_join.take() {
            let _ = join.join();
        }
        let stderr_text = self
            .stderr_text
            .lock()
            .map(|text| text.clone())
            .unwrap_or_default();
        StreamOutcome {
            status_text,
            stderr_text,
            delivered_frame: self.delivered_frame,
        }
    }
}

#[cfg(unix)]
fn set_nonblocking(stdout: &ChildStdout) -> Result<(), CameraError> {
    use std::os::fd::AsRawFd;
    let fd = stdout.as_raw_fd();
    // SAFETY: `fd` is owned by `stdout` and open for the call's duration.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(CameraError::Stream(format!(
                "failed to query capture pipe flags: {}",
                std::io::Error::last_os_error()
            )));
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(CameraError::Stream(format!(
                "failed to mark capture pipe non-blocking: {}",
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video_size() {
        assert_eq!(parse_video_size("640x480").unwrap(), (640, 480));
        assert!(parse_video_size("640").is_err());
        assert!(parse_video_size("0x480").is_err());
        assert!(parse_video_size("axb").is_err());
    }

    #[test]
    fn frame_len_matches_dimensions() {
        let mut config = CaptureConfig::new("dummy");
        config.video_size = Some("4x2".to_string());
        assert_eq!(config.frame_dimensions().unwrap(), (4, 2));
        assert_eq!(config.frame_len().unwrap(), 32);
    }

    #[test]
    fn split_frames_waits_for_a_complete_frame() {
        let mut buffer = vec![1u8; 15];
        assert!(split_frames(&mut buffer, 16).is_empty());
        assert_eq!(buffer.len(), 15, "a partial frame must stay buffered");
    }

    #[test]
    fn split_frames_returns_whole_frames_and_keeps_the_tail() {
        let mut buffer = vec![7u8; 20];
        let frames = split_frames(&mut buffer, 16);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), 16);
        assert_eq!(buffer, vec![7u8; 4]);
    }

    #[test]
    fn split_frames_returns_several_at_once() {
        let mut buffer = vec![3u8; 40];
        assert_eq!(split_frames(&mut buffer, 16).len(), 2);
        assert_eq!(buffer.len(), 8);
    }

    #[test]
    fn default_video_size_is_used_when_unset() {
        let config = CaptureConfig::new("dummy");
        assert_eq!(
            config.frame_dimensions().unwrap(),
            parse_video_size(DEFAULT_VIDEO_SIZE).unwrap()
        );
    }
}
