//! Opening and configuring a PCM, and converting between the device's sample
//! format and the `f32` frames the rest of the crate speaks.
//!
//! Devices are opened by their `hw:CARD,DEV` name so we get the hardware's own
//! format rather than whatever ALSA's plug layer would convert to -- the same
//! "no conversion we didn't ask for" rule as the CoreAudio backend. Where that
//! can't work, [`open_configured`] retries through `plughw:`, which is ALSA's
//! conversion layer: the Pi's HDMI output accepts only IEC958 subframes, so raw
//! access to it fails outright while `plughw:` plays ordinary PCM.

use std::ffi::{c_int, c_uint, CString};
use std::ptr;

use crate::capabilities::{SampleEncoding, SampleResolution};
use crate::error::AudioIoError;

use super::ffi;

/// Sample formats this backend converts, widest first.
pub(crate) const CANDIDATE_FORMATS: [Format; 5] = [
    Format::FloatLe,
    Format::S32Le,
    Format::S24_3Le,
    Format::S16Le,
    Format::U8,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    FloatLe,
    S32Le,
    S24_3Le,
    S16Le,
    U8,
}

impl Format {
    pub(crate) fn alsa(self) -> c_int {
        match self {
            Self::FloatLe => ffi::FORMAT_FLOAT_LE,
            Self::S32Le => ffi::FORMAT_S32_LE,
            Self::S24_3Le => ffi::FORMAT_S24_3LE,
            Self::S16Le => ffi::FORMAT_S16_LE,
            Self::U8 => ffi::FORMAT_U8,
        }
    }

    pub(crate) fn bytes(self) -> usize {
        match self {
            Self::FloatLe | Self::S32Le => 4,
            Self::S24_3Le => 3,
            Self::S16Le => 2,
            Self::U8 => 1,
        }
    }

    pub(crate) fn resolution(self) -> SampleResolution {
        match self {
            Self::FloatLe => SampleResolution::new(32, SampleEncoding::Float),
            Self::S32Le => SampleResolution::new(32, SampleEncoding::SignedInt),
            Self::S24_3Le => SampleResolution::new(24, SampleEncoding::SignedInt),
            Self::S16Le => SampleResolution::new(16, SampleEncoding::SignedInt),
            Self::U8 => SampleResolution::new(8, SampleEncoding::UnsignedInt),
        }
    }

    /// Device bytes -> `f32`, scaled by the same powers of two the rest of the
    /// crate uses, so integer samples round-trip exactly.
    pub(crate) fn decode(self, bytes: &[u8], out: &mut Vec<f32>) {
        out.clear();
        match self {
            Self::FloatLe => out.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            ),
            Self::S32Le => out.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| i32::from_le_bytes(*b) as f32 / 2_147_483_648.0),
            ),
            Self::S24_3Le => {
                out.extend(
                    bytes.as_chunks::<3>().0.iter().map(|b| {
                        (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0
                    }),
                )
            }
            Self::S16Le => out.extend(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| f32::from(i16::from_le_bytes(*b)) / 32_768.0),
            ),
            Self::U8 => out.extend(bytes.iter().map(|b| (f32::from(*b) - 128.0) / 128.0)),
        }
    }

    /// `f32` -> device bytes, clamped rather than wrapped.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn encode(self, samples: &[f32], out: &mut Vec<u8>) {
        out.clear();
        match self {
            Self::FloatLe => {
                for sample in samples {
                    out.extend_from_slice(&sample.clamp(-1.0, 1.0).to_le_bytes());
                }
            }
            Self::S32Le => {
                for sample in samples {
                    let code = (f64::from(*sample) * 2_147_483_648.0)
                        .clamp(-2_147_483_648.0, 2_147_483_647.0)
                        as i32;
                    out.extend_from_slice(&code.to_le_bytes());
                }
            }
            Self::S24_3Le => {
                for sample in samples {
                    let code =
                        (f64::from(*sample) * 8_388_608.0).clamp(-8_388_608.0, 8_388_607.0) as i32;
                    out.extend_from_slice(&code.to_le_bytes()[..3]);
                }
            }
            Self::S16Le => {
                for sample in samples {
                    let code = (f64::from(*sample) * 32_768.0).clamp(-32_768.0, 32_767.0) as i16;
                    out.extend_from_slice(&code.to_le_bytes());
                }
            }
            Self::U8 => {
                for sample in samples {
                    let code = (f64::from(*sample) * 128.0 + 128.0).clamp(0.0, 255.0) as u8;
                    out.push(code);
                }
            }
        }
    }
}

/// An open PCM handle. Closing is the owner's job (`Pcm::close`), because the
/// IO thread and the opener are different places in this backend.
pub(crate) struct Pcm(pub(crate) *mut ffi::SndPcm);

// SAFETY: an ALSA PCM handle is not thread-affine; this backend hands it to
// exactly one IO thread and never shares it.
unsafe impl Send for Pcm {}

impl Pcm {
    pub(crate) fn open(name: &str, capture: bool, nonblock: bool) -> Result<Self, AudioIoError> {
        let c_name = CString::new(name).map_err(|_| {
            AudioIoError::Backend(format!("device name {name:?} contains a NUL byte"))
        })?;
        let stream = if capture {
            ffi::STREAM_CAPTURE
        } else {
            ffi::STREAM_PLAYBACK
        };
        let mode = if nonblock { ffi::NONBLOCK } else { 0 };
        let mut pcm: *mut ffi::SndPcm = ptr::null_mut();
        // SAFETY: `c_name` is a valid C string and `pcm` a live out-pointer.
        let code = unsafe { ffi::snd_pcm_open(&mut pcm, c_name.as_ptr(), stream, mode) };
        if code < 0 || pcm.is_null() {
            return Err(AudioIoError::Stream(format!(
                "couldn't open {name}: {}",
                ffi::error_text(code)
            )));
        }
        Ok(Self(pcm))
    }

    pub(crate) fn close(self) {
        // SAFETY: `self.0` came from `snd_pcm_open` and is closed once.
        unsafe { ffi::snd_pcm_close(self.0) };
    }
}

/// A configured stream: what the device agreed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Configured {
    pub(crate) format: Format,
    pub(crate) channels: u16,
    pub(crate) sample_rate_hz: u32,
    pub(crate) period_frames: u32,
}

/// Hardware parameters, freed on drop.
pub(crate) struct HwParams(pub(crate) *mut ffi::SndPcmHwParams);

impl HwParams {
    pub(crate) fn any(pcm: &Pcm) -> Result<Self, AudioIoError> {
        let mut params: *mut ffi::SndPcmHwParams = ptr::null_mut();
        // SAFETY: out-pointer is live; `any` fills `params` from the device.
        let code = unsafe {
            let code = ffi::snd_pcm_hw_params_malloc(&mut params);
            if code < 0 {
                code
            } else {
                ffi::snd_pcm_hw_params_any(pcm.0, params)
            }
        };
        if code < 0 {
            if !params.is_null() {
                // SAFETY: allocated above.
                unsafe { ffi::snd_pcm_hw_params_free(params) };
            }
            return Err(AudioIoError::Stream(format!(
                "couldn't read hardware parameters: {}",
                ffi::error_text(code)
            )));
        }
        Ok(Self(params))
    }

    /// Formats the device accepts, widest first.
    pub(crate) fn supported_formats(&self, pcm: &Pcm) -> Vec<Format> {
        CANDIDATE_FORMATS
            .into_iter()
            // SAFETY: both handles are live for this call.
            .filter(|format| unsafe {
                ffi::snd_pcm_hw_params_test_format(pcm.0, self.0, format.alsa()) == 0
            })
            .collect()
    }

    pub(crate) fn supports_rate(&self, pcm: &Pcm, rate: u32) -> bool {
        // SAFETY: both handles are live for this call.
        unsafe { ffi::snd_pcm_hw_params_test_rate(pcm.0, self.0, rate as c_uint, 0) == 0 }
    }

    pub(crate) fn channel_range(&self) -> (u16, u16) {
        let (mut min, mut max): (c_uint, c_uint) = (0, 0);
        // SAFETY: both out-pointers are live.
        unsafe {
            ffi::snd_pcm_hw_params_get_channels_min(self.0, &mut min);
            ffi::snd_pcm_hw_params_get_channels_max(self.0, &mut max);
        }
        (
            u16::try_from(min).unwrap_or(1).max(1),
            u16::try_from(max).unwrap_or(2).max(1),
        )
    }

    pub(crate) fn rate_range(&self) -> (u32, u32) {
        let (mut min, mut max): (c_uint, c_uint) = (0, 0);
        let (mut dir_min, mut dir_max): (c_int, c_int) = (0, 0);
        // SAFETY: all out-pointers are live.
        unsafe {
            ffi::snd_pcm_hw_params_get_rate_min(self.0, &mut min, &mut dir_min);
            ffi::snd_pcm_hw_params_get_rate_max(self.0, &mut max, &mut dir_max);
        }
        (min, max)
    }
}

impl Drop for HwParams {
    fn drop(&mut self) {
        // SAFETY: allocated by `snd_pcm_hw_params_malloc`, freed once.
        unsafe { ffi::snd_pcm_hw_params_free(self.0) };
    }
}

/// Opens `pcm_name` and configures it, falling back to ALSA's plug layer when
/// the device speaks no format this backend converts (HDMI's IEC958). Returns
/// the handle and what the device agreed to.
pub(crate) fn open_configured(
    pcm_name: &str,
    capture: bool,
    nonblock: bool,
    period_frames: u32,
) -> Result<(Pcm, Configured), AudioIoError> {
    match open_and_configure(pcm_name, capture, nonblock, period_frames) {
        Ok(opened) => Ok(opened),
        Err(raw_error) => match plug_name(pcm_name) {
            // Report the raw device's error if the plug layer fails too: it
            // says what the hardware actually refused.
            Some(plug) => {
                open_and_configure(&plug, capture, nonblock, period_frames).map_err(|_| raw_error)
            }
            None => Err(raw_error),
        },
    }
}

fn open_and_configure(
    pcm_name: &str,
    capture: bool,
    nonblock: bool,
    period_frames: u32,
) -> Result<(Pcm, Configured), AudioIoError> {
    let pcm = Pcm::open(pcm_name, capture, nonblock)?;
    match configure(&pcm, None, period_frames) {
        Ok(configured) => Ok((pcm, configured)),
        Err(error) => {
            pcm.close();
            Err(error)
        }
    }
}

fn plug_name(pcm_name: &str) -> Option<String> {
    pcm_name
        .strip_prefix("hw:")
        .map(|device| format!("plughw:{device}"))
}

/// Configures `pcm` for interleaved access at the best format it offers,
/// `rate` (or its nearest supported standard rate), and `period_frames`.
pub(crate) fn configure(
    pcm: &Pcm,
    preferred_rate_hz: Option<u32>,
    period_frames: u32,
) -> Result<Configured, AudioIoError> {
    let params = HwParams::any(pcm)?;
    let formats = params.supported_formats(pcm);
    let Some(format) = formats.first().copied() else {
        return Err(AudioIoError::UnsupportedSampleFormat(
            "device offers no PCM format this backend converts".to_string(),
        ));
    };
    let (_, channels) = params.channel_range();
    let (rate_min, rate_max) = params.rate_range();
    let rate = preferred_rate_hz
        .filter(|rate| params.supports_rate(pcm, *rate))
        .or_else(|| {
            crate::capabilities::STANDARD_SAMPLE_RATES_HZ
                .into_iter()
                .rev()
                .find(|rate| params.supports_rate(pcm, *rate))
        })
        .unwrap_or(rate_max.min(rate_min.max(48_000)));

    let mut period: ffi::Uframes = ffi::Uframes::from(period_frames.max(16));
    let mut buffer: ffi::Uframes = period * 4;
    let mut dir: c_int = 0;
    // SAFETY: every pointer below is live, and `pcm`/`params` are this
    // device's own handles.
    let code = unsafe {
        let steps = [
            ffi::snd_pcm_hw_params_set_access(pcm.0, params.0, ffi::ACCESS_RW_INTERLEAVED),
            ffi::snd_pcm_hw_params_set_format(pcm.0, params.0, format.alsa()),
            ffi::snd_pcm_hw_params_set_channels(pcm.0, params.0, c_uint::from(channels)),
            // Never let ALSA silently resample: this backend wants the
            // hardware rate, and `DriftResampler` bridges any difference.
            ffi::snd_pcm_hw_params_set_rate_resample(pcm.0, params.0, 0),
            ffi::snd_pcm_hw_params_set_rate(pcm.0, params.0, rate as c_uint, 0),
            ffi::snd_pcm_hw_params_set_period_size_near(pcm.0, params.0, &mut period, &mut dir),
            ffi::snd_pcm_hw_params_set_buffer_size_near(pcm.0, params.0, &mut buffer),
            ffi::snd_pcm_hw_params(pcm.0, params.0),
        ];
        steps.into_iter().find(|code| *code < 0).unwrap_or(0)
    };
    if code < 0 {
        return Err(AudioIoError::Stream(format!(
            "couldn't configure the device at {rate} Hz, {channels} ch, {}: {}",
            format.resolution(),
            ffi::error_text(code)
        )));
    }

    let mut actual_period: ffi::Uframes = period;
    // SAFETY: `params` is configured and both pointers are live.
    unsafe { ffi::snd_pcm_hw_params_get_period_size(params.0, &mut actual_period, &mut dir) };
    // SAFETY: the device is configured; prepare makes it ready to run.
    let code = unsafe { ffi::snd_pcm_prepare(pcm.0) };
    if code < 0 {
        return Err(AudioIoError::Stream(format!(
            "couldn't prepare the device: {}",
            ffi::error_text(code)
        )));
    }

    Ok(Configured {
        format,
        channels,
        sample_rate_hz: rate,
        period_frames: u32::try_from(actual_period).unwrap_or(period_frames),
    })
}

#[cfg(test)]
mod tests {
    use super::Format;

    #[test]
    fn integer_formats_round_trip_through_f32() {
        for (format, codes) in [
            (Format::S16Le, vec![-32_768i32, -1, 0, 1, 32_767]),
            (Format::S24_3Le, vec![-8_388_608, -1, 0, 1, 8_388_607]),
            (Format::S32Le, vec![-2_147_483_648, 0, 2_147_483_647]),
        ] {
            let scale = match format {
                Format::S16Le => 32_768.0,
                Format::S24_3Le => 8_388_608.0,
                _ => 2_147_483_648.0,
            };
            #[allow(clippy::cast_possible_truncation)]
            let samples: Vec<f32> = codes
                .iter()
                .map(|c| (f64::from(*c) / scale) as f32)
                .collect();
            let mut bytes = Vec::new();
            format.encode(&samples, &mut bytes);
            assert_eq!(bytes.len(), samples.len() * format.bytes());
            let mut decoded = Vec::new();
            format.decode(&bytes, &mut decoded);
            #[allow(clippy::cast_possible_truncation)]
            let round: Vec<i32> = decoded
                .iter()
                .map(|value| (f64::from(*value) * scale).round() as i32)
                .collect();
            assert_eq!(round, codes, "{format:?}");
        }
    }

    #[test]
    fn encoding_clamps_rather_than_wrapping() {
        let mut bytes = Vec::new();
        Format::S16Le.encode(&[2.0, -2.0], &mut bytes);
        assert_eq!(bytes, [0xff, 0x7f, 0x00, 0x80]);
    }

    #[test]
    fn unsigned_eight_bit_centres_on_128() {
        let mut bytes = Vec::new();
        Format::U8.encode(&[0.0, 1.0, -1.0], &mut bytes);
        assert_eq!(bytes, [128, 255, 0]);
        let mut decoded = Vec::new();
        Format::U8.decode(&bytes, &mut decoded);
        assert!((decoded[0]).abs() < 0.01);
    }
}
