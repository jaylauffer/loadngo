//! What an input device (typically an analog-to-digital converter such as
//! a USB instrument interface) can actually capture, for a UI that lets a
//! user explore it and for a recorder that wants to keep every bit the
//! converter delivers.
//!
//! Two different questions hide behind "what format is this device":
//!
//! - **Stream formats** are what the OS audio API will hand an application
//!   (`cpal`'s supported configs). On macOS these are *always* 32-bit float,
//!   because CoreAudio's HAL converts every device to its float "virtual
//!   format" before any app sees it -- so they say nothing about the
//!   converter itself.
//! - **Physical formats** are what the converter's hardware stream really
//!   runs at, e.g. 24-bit signed integer at 48 kHz. Only macOS exposes these
//!   separately today (`kAudioStreamPropertyAvailablePhysicalFormats`, see
//!   `backend/coreaudio/formats.rs`); on Linux, ALSA's stream formats already *are* the
//!   hardware formats for a `hw:` device, and on Windows the shared-mode
//!   mixer format hides the converter the same way CoreAudio does, without a
//!   physical-format query implemented here yet.
//!
//! [`InputCapabilities::capture_resolution`] resolves the two into the one
//! answer a recorder needs.

use crate::error::AudioIoError;

/// How a sample is encoded, independent of its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleEncoding {
    SignedInt,
    UnsignedInt,
    Float,
}

/// A sample encoding plus its width in bits, e.g. 24-bit signed integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleResolution {
    pub bits: u16,
    pub encoding: SampleEncoding,
}

impl SampleResolution {
    #[must_use]
    pub const fn new(bits: u16, encoding: SampleEncoding) -> Self {
        Self { bits, encoding }
    }
}

impl std::fmt::Display for SampleResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.encoding {
            SampleEncoding::SignedInt => "int",
            SampleEncoding::UnsignedInt => "unsigned int",
            SampleEncoding::Float => "float",
        };
        write!(f, "{}-bit {kind}", self.bits)
    }
}

/// One contiguous range of sample rates at a fixed channel count and
/// resolution. A device advertising discrete rates reports ranges whose
/// minimum equals their maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatRange {
    pub channels: u16,
    pub min_sample_rate_hz: u32,
    pub max_sample_rate_hz: u32,
    pub resolution: SampleResolution,
}

impl FormatRange {
    #[must_use]
    pub fn contains_rate(&self, sample_rate_hz: u32) -> bool {
        (self.min_sample_rate_hz..=self.max_sample_rate_hz).contains(&sample_rate_hz)
    }
}

/// A concrete format a device is running at right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentFormat {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub resolution: SampleResolution,
}

/// Where a [`InputCapabilities::capture_resolution`] answer came from, so a
/// UI can say how much to trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionSource {
    /// Read from the converter's hardware stream (macOS physical format).
    Physical,
    /// Only the OS stream format was available. On macOS and Windows this is
    /// the mixer's format, not necessarily the converter's.
    Stream,
}

/// Everything this crate can find out about one input device.
#[derive(Debug, Clone, PartialEq)]
pub struct InputCapabilities {
    pub device_name: String,
    pub is_default: bool,
    /// The stream format a capture opened right now would receive -- the
    /// device's current nominal rate, full channel count, and the OS stream
    /// encoding. This is what [`crate::LiveMonitor`] opens.
    pub current_stream: CurrentFormat,
    /// Every stream configuration the OS audio API will open.
    pub stream_formats: Vec<FormatRange>,
    /// The converter's current hardware format, where the platform exposes
    /// one separately from `current_stream` (macOS only today).
    pub current_physical: Option<CurrentFormat>,
    /// Every hardware format the converter supports (macOS only today).
    pub physical_formats: Vec<FormatRange>,
}

/// Sample rates worth listing individually when a device reports a
/// continuous range rather than discrete rates.
pub const STANDARD_SAMPLE_RATES_HZ: [u32; 10] = [
    8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 192_000,
];

impl InputCapabilities {
    /// The sample resolution a lossless recording of this device should
    /// use, and where that answer came from: the converter's physical
    /// format when known, otherwise the stream format.
    #[must_use]
    pub fn capture_resolution(&self) -> (SampleResolution, ResolutionSource) {
        match self.current_physical {
            Some(physical) => (physical.resolution, ResolutionSource::Physical),
            None => (self.current_stream.resolution, ResolutionSource::Stream),
        }
    }

    /// Every distinct sample rate the device supports across `ranges`,
    /// ascending. Discrete rates are reported as-is; continuous ranges
    /// contribute the [`STANDARD_SAMPLE_RATES_HZ`] they contain.
    #[must_use]
    pub fn sample_rates(ranges: &[FormatRange]) -> Vec<u32> {
        let mut rates = Vec::new();
        for range in ranges {
            if range.min_sample_rate_hz == range.max_sample_rate_hz {
                rates.push(range.min_sample_rate_hz);
            } else {
                rates.extend(
                    STANDARD_SAMPLE_RATES_HZ
                        .iter()
                        .copied()
                        .filter(|rate| range.contains_rate(*rate)),
                );
            }
        }
        rates.sort_unstable();
        rates.dedup();
        rates
    }

    /// Every distinct resolution across `ranges`, widest first.
    #[must_use]
    pub fn resolutions(ranges: &[FormatRange]) -> Vec<SampleResolution> {
        let mut resolutions: Vec<SampleResolution> = Vec::new();
        for range in ranges {
            if !resolutions.contains(&range.resolution) {
                resolutions.push(range.resolution);
            }
        }
        resolutions.sort_by_key(|resolution| std::cmp::Reverse(resolution.bits));
        resolutions
    }

    /// Every distinct channel count across `ranges`, ascending.
    #[must_use]
    pub fn channel_counts(ranges: &[FormatRange]) -> Vec<u16> {
        let mut counts: Vec<u16> = ranges.iter().map(|range| range.channels).collect();
        counts.sort_unstable();
        counts.dedup();
        counts
    }
}

/// Probes one input device. `None` probes the host's current default input.
///
/// Queries device properties only; it never opens a stream, so it is safe
/// to call while a [`crate::LiveMonitor`] is capturing from the same device.
pub fn probe_input_capabilities(
    device_name: Option<&str>,
) -> Result<InputCapabilities, AudioIoError> {
    crate::backend::platform::probe_input_capabilities(device_name)
}

/// Switches an input device's converter to one of its advertised
/// [`InputCapabilities::physical_formats`] (resolution plus sample rate).
///
/// This is a **system-wide, persistent** device setting -- the same one
/// Audio MIDI Setup's "Format" menu changes -- not a per-app stream option,
/// and it applies asynchronously: re-probe over the next few hundred
/// milliseconds to confirm. A running [`crate::LiveMonitor`] on the device
/// should be restarted afterwards if the sample rate changed. Only macOS
/// exposes physical formats today; elsewhere this returns
/// [`AudioIoError::UnsupportedOnPlatform`].
pub fn set_input_physical_format(
    device_name: &str,
    format: &CurrentFormat,
) -> Result<(), AudioIoError> {
    crate::backend::platform::set_input_physical_format(device_name, format)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(channels: u16, min: u32, max: u32, bits: u16) -> FormatRange {
        FormatRange {
            channels,
            min_sample_rate_hz: min,
            max_sample_rate_hz: max,
            resolution: SampleResolution::new(bits, SampleEncoding::SignedInt),
        }
    }

    fn capabilities(physical: Option<CurrentFormat>) -> InputCapabilities {
        InputCapabilities {
            device_name: "USB PnP Audio Device".to_string(),
            is_default: true,
            current_stream: CurrentFormat {
                sample_rate_hz: 48_000,
                channels: 2,
                resolution: SampleResolution::new(32, SampleEncoding::Float),
            },
            stream_formats: Vec::new(),
            current_physical: physical,
            physical_formats: Vec::new(),
        }
    }

    #[test]
    fn capture_resolution_prefers_the_physical_converter_format() {
        let physical = CurrentFormat {
            sample_rate_hz: 48_000,
            channels: 2,
            resolution: SampleResolution::new(24, SampleEncoding::SignedInt),
        };
        assert_eq!(
            capabilities(Some(physical)).capture_resolution(),
            (physical.resolution, ResolutionSource::Physical)
        );
    }

    #[test]
    fn capture_resolution_falls_back_to_the_stream_format() {
        assert_eq!(
            capabilities(None).capture_resolution(),
            (
                SampleResolution::new(32, SampleEncoding::Float),
                ResolutionSource::Stream
            )
        );
    }

    #[test]
    fn sample_rates_merge_discrete_and_continuous_ranges() {
        let ranges = [
            range(2, 44_100, 44_100, 24),
            range(2, 48_000, 48_000, 24),
            range(1, 40_000, 100_000, 16),
        ];
        assert_eq!(
            InputCapabilities::sample_rates(&ranges),
            vec![44_100, 48_000, 88_200, 96_000]
        );
    }

    #[test]
    fn resolutions_list_widest_first_without_duplicates() {
        let ranges = [
            range(2, 48_000, 48_000, 16),
            range(2, 48_000, 48_000, 24),
            range(1, 44_100, 44_100, 24),
        ];
        let bits: Vec<u16> = InputCapabilities::resolutions(&ranges)
            .iter()
            .map(|resolution| resolution.bits)
            .collect();
        assert_eq!(bits, vec![24, 16]);
    }
}
