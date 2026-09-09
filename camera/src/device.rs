//! Camera enumeration, and the two per-platform details capture depends on:
//! which `ffmpeg` input format to use, and how a device identifier is spelled
//! on the command line.
//!
//! Enumeration is best-effort everywhere. A caller that already knows its
//! device (a saved setting, a `--device` flag) never has to enumerate at all.

use crate::CameraError;

/// A camera the host can open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraDevice {
    /// Platform-native identifier to put in [`crate::CaptureConfig::device`]:
    /// a `/dev/video*` path on Linux, an `avfoundation` index on macOS, a
    /// DirectShow device name on Windows.
    pub id: String,
    /// Human-readable name for a picker. Falls back to `id` when the
    /// platform offers nothing better.
    pub name: String,
}

/// The `ffmpeg` input format (`-f`) for this platform's cameras.
#[must_use]
pub fn input_format() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "v4l2"
    }
    #[cfg(target_os = "macos")]
    {
        "avfoundation"
    }
    #[cfg(windows)]
    {
        "dshow"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        compile_error!(
            "loadngo-camera has no ffmpeg input format for this platform; add one in device.rs"
        )
    }
}

/// How `device` is spelled after `-i`.
///
/// DirectShow wants `video=<name>`. AVFoundation takes `<video>:<audio>` and
/// will open a default microphone if the audio half is omitted, so it is
/// pinned to `:none` -- capture here is video-only, and grabbing an input
/// device nobody asked for is both surprising and a permissions prompt.
/// V4L2 takes the device path as-is.
#[must_use]
pub fn input_argument(device: &str) -> String {
    #[cfg(windows)]
    {
        if device.starts_with("video=") {
            device.to_string()
        } else {
            format!("video={device}")
        }
    }
    #[cfg(target_os = "macos")]
    {
        if device.contains(':') {
            device.to_string()
        } else {
            format!("{device}:none")
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        device.to_string()
    }
}

#[cfg(all(test, target_os = "macos"))]
mod input_argument_tests {
    use super::input_argument;

    #[test]
    fn pins_audio_off_unless_already_specified() {
        assert_eq!(input_argument("0"), "0:none");
        assert_eq!(input_argument("2:none"), "2:none");
    }
}

/// The `-framerate` to request before `-i`, given what the caller asked for.
///
/// V4L2 and DirectShow negotiate whatever you ask for, so the request passes
/// through. AVFoundation does not: it accepts only rates the device
/// explicitly advertises, and fails the entire capture with "Selected
/// framerate is not supported by the device" otherwise -- including for
/// ffmpeg's own default guess of 29.97 when no rate is given, so omitting
/// the flag is not a fix. macOS therefore snaps to the nearest rate from the
/// set every UVC webcam advertises. Any residual difference is absorbed by
/// the `fps=` output filter, which is what actually sets the preview rate.
#[must_use]
pub fn input_framerate(requested: u32) -> u32 {
    let requested = requested.max(1);
    #[cfg(target_os = "macos")]
    {
        const ADVERTISED: [u32; 6] = [30, 24, 20, 15, 10, 5];
        ADVERTISED
            .into_iter()
            .min_by_key(|rate| rate.abs_diff(requested))
            .unwrap_or(30)
    }
    #[cfg(not(target_os = "macos"))]
    {
        requested
    }
}

/// Cameras this host can see, best guess first.
pub fn list_devices() -> Result<Vec<CameraDevice>, CameraError> {
    platform::list_devices()
}

/// The camera a host should default to, if any.
pub fn default_device() -> Result<Option<CameraDevice>, CameraError> {
    Ok(list_devices()?.into_iter().next())
}

/// Runs `ffmpeg`'s device-listing probe, which reports on stderr and always
/// exits non-zero, so only the captured text matters.
#[cfg(any(target_os = "macos", windows))]
fn ffmpeg_list_output(args: &[&str]) -> Result<String, CameraError> {
    let output = std::process::Command::new("ffmpeg")
        .args(["-hide_banner"])
        .args(args)
        .output()
        .map_err(|err| CameraError::FfmpegUnavailable(err.to_string()))?;
    Ok(String::from_utf8_lossy(&output.stderr).into_owned())
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{CameraDevice, CameraError};
    use std::fs;
    use std::path::PathBuf;

    pub(super) fn list_devices() -> Result<Vec<CameraDevice>, CameraError> {
        let mut preferred = Vec::new();
        let mut fallback = Vec::new();
        let entries = fs::read_dir("/dev")
            .map_err(|err| CameraError::Enumerate(format!("cannot read /dev: {err}")))?;
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !name.starts_with("video") {
                continue;
            }
            let device = CameraDevice {
                id: format!("/dev/{name}"),
                name: label_for(&name),
            };
            if is_probably_camera(&device.name) {
                preferred.push(device);
            } else {
                fallback.push(device);
            }
        }
        preferred.sort_by(|a, b| a.id.cmp(&b.id));
        preferred.dedup();
        fallback.sort_by(|a, b| a.id.cmp(&b.id));
        fallback.dedup();
        // A Pi exposes a dozen ISP/codec nodes alongside any real webcam, so
        // only fall back to those when nothing looks like a camera at all.
        Ok(if preferred.is_empty() {
            fallback
        } else {
            preferred
        })
    }

    fn label_for(name: &str) -> String {
        let sysfs = PathBuf::from("/sys/class/video4linux")
            .join(name)
            .join("name");
        fs::read_to_string(sysfs)
            .map(|label| label.trim().to_string())
            .unwrap_or_else(|_| name.to_string())
    }

    fn is_probably_camera(label: &str) -> bool {
        let normalized = label.to_ascii_lowercase();
        !(normalized.contains("codec")
            || normalized.contains("isp")
            || normalized.contains("decoder")
            || normalized.contains("-dec")
            || normalized.contains("encoder")
            || normalized.contains("hevc")
            || normalized.contains("v4l2 loopback")
            || normalized.contains("bcm2835"))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{ffmpeg_list_output, CameraDevice, CameraError};

    pub(super) fn list_devices() -> Result<Vec<CameraDevice>, CameraError> {
        let text = ffmpeg_list_output(&["-f", "avfoundation", "-list_devices", "true", "-i", ""])?;
        Ok(parse(&text))
    }

    /// `avfoundation` lists video devices, then audio devices, as
    /// `[AVFoundation indev @ 0x...] [<index>] <name>`. Indices restart at 0
    /// for audio, so parsing must stop at the audio header or it will offer
    /// microphones as cameras.
    fn parse(text: &str) -> Vec<CameraDevice> {
        let mut devices = Vec::new();
        let mut in_video_section = false;
        for line in text.lines() {
            let Some((_, rest)) = line.split_once("] ") else {
                continue;
            };
            let rest = rest.trim();
            if rest.eq_ignore_ascii_case("AVFoundation video devices:") {
                in_video_section = true;
                continue;
            }
            if rest.eq_ignore_ascii_case("AVFoundation audio devices:") {
                break;
            }
            if !in_video_section {
                continue;
            }
            let Some(index_end) = rest.find(']') else {
                continue;
            };
            if !rest.starts_with('[') {
                continue;
            }
            let index = &rest[1..index_end];
            if index.parse::<u32>().is_err() {
                continue;
            }
            let name = rest[index_end + 1..].trim().to_string();
            // Screen-capture pseudo-devices are cameras to AVFoundation but
            // never what a webcam preview wants by default.
            if name.to_ascii_lowercase().contains("capture screen") {
                continue;
            }
            devices.push(CameraDevice {
                id: index.to_string(),
                name,
            });
        }
        devices
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_real_avfoundation_listing() {
            let text = "\
[AVFoundation indev @ 0x1] AVFoundation video devices:
[AVFoundation indev @ 0x1] [0] HD Pro Webcam C920
[AVFoundation indev @ 0x1] [1] Capture screen 0
[AVFoundation indev @ 0x1] AVFoundation audio devices:
[AVFoundation indev @ 0x1] [0] MOTIV Mix Virtual
";
            let devices = parse(text);
            assert_eq!(devices.len(), 1, "screen capture and audio must be skipped");
            assert_eq!(devices[0].id, "0");
            assert_eq!(devices[0].name, "HD Pro Webcam C920");
        }

        #[test]
        fn ignores_noise_without_a_video_section() {
            assert!(parse("nothing useful here\n").is_empty());
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::{ffmpeg_list_output, CameraDevice, CameraError};

    pub(super) fn list_devices() -> Result<Vec<CameraDevice>, CameraError> {
        let text = ffmpeg_list_output(&["-f", "dshow", "-list_devices", "true", "-i", "dummy"])?;
        Ok(parse(&text))
    }

    /// DirectShow lists `"<name>"` lines under a video header, each usually
    /// followed by an `Alternative name "@device_pnp_..."` line that must not
    /// be mistaken for another camera.
    fn parse(text: &str) -> Vec<CameraDevice> {
        let mut devices = Vec::new();
        let mut in_video_section = false;
        for line in text.lines() {
            let Some((_, rest)) = line.split_once("] ") else {
                continue;
            };
            let rest = rest.trim();
            let lowered = rest.to_ascii_lowercase();
            if lowered.starts_with("directshow video devices") {
                in_video_section = true;
                continue;
            }
            if lowered.starts_with("directshow audio devices") {
                break;
            }
            if !in_video_section || lowered.starts_with("alternative name") {
                continue;
            }
            let Some(name) = rest
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
            else {
                continue;
            };
            devices.push(CameraDevice {
                id: name.to_string(),
                name: name.to_string(),
            });
        }
        devices
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_directshow_listing() {
            let text = "\
[dshow @ 0x1] DirectShow video devices (some may be both video and audio devices)
[dshow @ 0x1]  \"Integrated Camera\"
[dshow @ 0x1]     Alternative name \"@device_pnp_\\\\?\\usb#vid\"
[dshow @ 0x1] DirectShow audio devices
[dshow @ 0x1]  \"Microphone\"
";
            let devices = parse(text);
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0].id, "Integrated Camera");
        }
    }
}
