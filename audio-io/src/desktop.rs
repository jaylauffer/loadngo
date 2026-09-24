//! Session-managed playback, distinct from direct hardware monitoring.

use crate::{AudioIoError, OutputFormat};

/// Routing hints for desktop playback. System master/application volume is
/// applied by the desktop audio service, in addition to the caller's mix gain.
#[derive(Debug, Clone)]
pub struct DesktopOutputOptions {
    /// Human-readable application label shown in the desktop audio mixer.
    pub application_name: String,
    /// None follows the session manager. On Linux, Some is a PipeWire
    /// `node.name` or `object.serial`, not an ALSA card/display name.
    /// On other desktops it is the platform device name.
    pub target: Option<String>,
    /// Latency hint, not a guarantee about the negotiated callback size.
    pub preferred_buffer_frames: Option<u32>,
}

impl Default for DesktopOutputOptions {
    fn default() -> Self {
        Self {
            application_name: "Loadngo".into(),
            target: None,
            preferred_buffer_frames: None,
        }
    }
}

/// Owned desktop stream. Drop stops callbacks before releasing their state.
pub struct DesktopOutputStream {
    #[cfg(target_os = "linux")]
    stream: crate::backend::pipewire::Stream,
    #[cfg(not(target_os = "linux"))]
    stream: crate::OutputStream,
    format: OutputFormat,
}

impl DesktopOutputStream {
    #[must_use]
    pub fn format(&self) -> &OutputFormat {
        &self.format
    }

    #[must_use]
    pub fn sample_rate_hz(&self) -> u32 {
        self.format.sample_rate_hz
    }

    #[must_use]
    pub fn channels(&self) -> u16 {
        self.format.channels
    }

    /// Terminal stream failure, if any. No implicit raw-hardware fallback.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        self.stream.failure()
    }
}

/// Open session-managed output. The callback must be bounded, nonblocking
/// and allocation-free. Linux negotiates interleaved f32 stereo at 48 kHz;
/// PipeWire handles conversion/routing to the actual output device.
///
/// Linux initialization waits at most three seconds for a connected stream.
/// Run setup off the UI hot path. A missing server/output is an error, never
/// permission to bypass desktop volume by opening an ALSA device directly.
pub fn open_desktop_output_stream(
    options: &DesktopOutputOptions,
    fill: impl FnMut(&mut [f32], usize) + Send + 'static,
) -> Result<DesktopOutputStream, AudioIoError> {
    if options.application_name.trim().is_empty()
        || options.application_name.contains('\0')
        || options
            .target
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.contains('\0'))
        || options
            .preferred_buffer_frames
            .is_some_and(|n| !(16..=8192).contains(&n))
    {
        return Err(AudioIoError::Stream("invalid desktop audio options".into()));
    }
    #[cfg(target_os = "linux")]
    let (stream, format) = crate::backend::pipewire::open(options, Box::new(fill))?;
    #[cfg(not(target_os = "linux"))]
    let (stream, format) = {
        let stream = crate::open_output_stream(
            options.target.as_deref(),
            options.preferred_buffer_frames,
            fill,
        )?;
        let format = stream.format().clone();
        (stream, format)
    };
    Ok(DesktopOutputStream { stream, format })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_options_fail_before_opening_a_device() {
        for options in [
            DesktopOutputOptions {
                application_name: "".into(),
                ..Default::default()
            },
            DesktopOutputOptions {
                target: Some("bad\0target".into()),
                ..Default::default()
            },
            DesktopOutputOptions {
                preferred_buffer_frames: Some(0),
                ..Default::default()
            },
        ] {
            assert!(open_desktop_output_stream(&options, |_, _| {}).is_err());
        }
    }
}
