//! Interim backend for Linux and Windows on `cpal`, until ALSA and WASAPI
//! backends replace it (see `loadngo/docs/AUDIO_BACKENDS.md`).
//!
//! `cpal::Stream` isn't `Send` on every host, so each stream lives on its own
//! keeper thread for its whole life. That thread blocks in a
//! `loadngo_proactor::Proactor<ChannelPort>` until the handle's `Drop` calls
//! `stop()` -- the ownership pattern `LiveMonitor` used before the backend
//! seam existed. Setup success or failure comes back over a plain `mpsc`
//! channel, because it has to resolve before there's a proactor to wait on.

use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use loadngo_proactor::{ChannelPort, Proactor, ProactorHandle};

use crate::backend::{Direction, InputCallback, OutputCallback, StreamFormat};
use crate::capabilities::{
    CurrentFormat, FormatRange, InputCapabilities, SampleEncoding, SampleResolution,
};
use crate::devices::AudioDeviceInfo;
use crate::error::AudioIoError;

pub(crate) struct Stream {
    proactor: ProactorHandle<ChannelPort>,
    worker: Option<JoinHandle<()>>,
    failure: Arc<Mutex<Option<String>>>,
}

impl Stream {
    pub(crate) fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|failure| failure.clone())
    }

    /// `cpal` doesn't report missed deadlines.
    pub(crate) fn overloads(&self) -> u64 {
        0
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.proactor.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn backend_error(error: impl std::fmt::Display) -> AudioIoError {
    AudioIoError::Backend(error.to_string())
}

pub(crate) fn list_devices(direction: Direction) -> Result<Vec<AudioDeviceInfo>, AudioIoError> {
    let host = cpal::default_host();
    let default_name = match direction {
        Direction::Input => host.default_input_device(),
        Direction::Output => host.default_output_device(),
    }
    .and_then(|device| device.name().ok());
    let devices = match direction {
        Direction::Input => host.input_devices(),
        Direction::Output => host.output_devices(),
    }
    .map_err(backend_error)?;
    Ok(devices
        .filter_map(|device| device.name().ok())
        .map(|name| {
            let is_default = default_name.as_deref() == Some(name.as_str());
            AudioDeviceInfo { name, is_default }
        })
        .collect())
}

fn resolve(
    host: &cpal::Host,
    direction: Direction,
    name: Option<&str>,
) -> Result<cpal::Device, AudioIoError> {
    match name {
        Some(name) => {
            let mut devices = match direction {
                Direction::Input => host.input_devices(),
                Direction::Output => host.output_devices(),
            }
            .map_err(backend_error)?;
            devices
                .find(|device| device.name().is_ok_and(|n| n == name))
                .ok_or_else(|| match direction {
                    Direction::Input => AudioIoError::InputDeviceNotFound(name.to_string()),
                    Direction::Output => AudioIoError::OutputDeviceNotFound(name.to_string()),
                })
        }
        None => match direction {
            Direction::Input => host
                .default_input_device()
                .ok_or(AudioIoError::NoDefaultInputDevice),
            Direction::Output => host
                .default_output_device()
                .ok_or(AudioIoError::NoDefaultOutputDevice),
        },
    }
}

fn default_config(
    device: &cpal::Device,
    direction: Direction,
) -> Result<cpal::SupportedStreamConfig, AudioIoError> {
    match direction {
        Direction::Input => device.default_input_config(),
        Direction::Output => device.default_output_config(),
    }
    .map_err(backend_error)
}

pub(crate) fn resolution(format: cpal::SampleFormat) -> Option<SampleResolution> {
    let encoding = if format.is_float() {
        SampleEncoding::Float
    } else if format.is_int() {
        SampleEncoding::SignedInt
    } else {
        SampleEncoding::UnsignedInt
    };
    let bits = u16::try_from(format.sample_size() * 8).ok()?;
    Some(SampleResolution::new(bits, encoding))
}

fn format_of(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    buffer_frames: Option<u32>,
) -> StreamFormat {
    StreamFormat {
        device_name: device
            .name()
            .unwrap_or_else(|_| "unknown device".to_string()),
        sample_rate_hz: config.sample_rate().0,
        channels: config.channels(),
        buffer_frames,
        resolution: resolution(config.sample_format()),
    }
}

pub(crate) fn describe(
    direction: Direction,
    name: Option<&str>,
) -> Result<StreamFormat, AudioIoError> {
    let host = cpal::default_host();
    let device = resolve(&host, direction, name)?;
    let config = default_config(&device, direction)?;
    Ok(format_of(&device, &config, None))
}

enum Callback {
    Input(InputCallback),
    Output(OutputCallback),
}

pub(crate) fn open_input(
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: InputCallback,
) -> Result<(Stream, StreamFormat), AudioIoError> {
    open(
        Direction::Input,
        name,
        preferred_buffer_frames,
        Callback::Input(callback),
    )
}

pub(crate) fn open_output(
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: OutputCallback,
) -> Result<(Stream, StreamFormat), AudioIoError> {
    open(
        Direction::Output,
        name,
        preferred_buffer_frames,
        Callback::Output(callback),
    )
}

fn open(
    direction: Direction,
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: Callback,
) -> Result<(Stream, StreamFormat), AudioIoError> {
    let name = name.map(str::to_owned);
    let failure = Arc::new(Mutex::new(None));
    let thread_failure = failure.clone();
    let proactor = Proactor::new(ChannelPort::new());
    let handle = proactor.handle();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<StreamFormat, AudioIoError>>();

    let worker = thread::Builder::new()
        .name(format!("loadngo-audio-io-{}", direction.label()))
        .spawn(move || {
            let built = build(
                direction,
                name.as_deref(),
                preferred_buffer_frames,
                callback,
                thread_failure,
            );
            match built {
                Ok((stream, format)) => {
                    if ready_tx.send(Ok(format)).is_err() {
                        return;
                    }
                    // Keeps the stream (and so the device I/O) alive until
                    // `Stream::drop` stops the proactor.
                    let _ = proactor.run_until_stopped();
                    drop(stream);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            }
        })
        .map_err(|error| AudioIoError::Thread(error.to_string()))?;

    let stream = Stream {
        proactor: handle,
        worker: Some(worker),
        failure,
    };
    match ready_rx.recv() {
        Ok(Ok(format)) => Ok((stream, format)),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(AudioIoError::WorkerExited),
    }
}

fn build(
    direction: Direction,
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: Callback,
    failure: Arc<Mutex<Option<String>>>,
) -> Result<(cpal::Stream, StreamFormat), AudioIoError> {
    let host = cpal::default_host();
    let device = resolve(&host, direction, name)?;
    let supported = default_config(&device, direction)?;
    let sample_format = supported.sample_format();
    let mut config: cpal::StreamConfig = supported.clone().into();
    if let Some(frames) = preferred_buffer_frames {
        config.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    let format = format_of(&device, &supported, preferred_buffer_frames);
    let on_error = move |error: cpal::StreamError| {
        if let Ok(mut failure) = failure.lock() {
            *failure = Some(error.to_string());
        }
    };
    let stream = match callback {
        Callback::Input(callback) => {
            build_input(&device, &config, sample_format, callback, on_error)?
        }
        Callback::Output(callback) => {
            build_output(&device, &config, sample_format, callback, on_error)?
        }
    };
    stream
        .play()
        .map_err(|error| AudioIoError::Stream(error.to_string()))?;
    Ok((stream, format))
}

fn build_input(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    mut callback: InputCallback,
    on_error: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, AudioIoError> {
    let channels = usize::from(config.channels.max(1));
    let mut float = Vec::new();
    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| callback(data, channels),
            on_error,
            None,
        ),
        // 2^15, not i16::MAX: the power-of-two scale CoreAudio uses, so
        // integer samples round-trip exactly on every platform.
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                float.clear();
                float.extend(data.iter().map(|&s| f32::from(s) / 32_768.0));
                callback(&float, channels);
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                float.clear();
                float.extend(data.iter().map(|&s| (f32::from(s) - 32_768.0) / 32_768.0));
                callback(&float, channels);
            },
            on_error,
            None,
        ),
        other => return Err(AudioIoError::UnsupportedSampleFormat(format!("{other:?}"))),
    };
    stream.map_err(|error| AudioIoError::Stream(error.to_string()))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn build_output(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    mut callback: OutputCallback,
    on_error: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, AudioIoError> {
    let channels = usize::from(config.channels.max(1));
    let mut float = Vec::new();
    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| callback(data, channels),
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                float.clear();
                float.resize(data.len(), 0.0);
                callback(&mut float, channels);
                for (out, value) in data.iter_mut().zip(&float) {
                    *out = (value.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
                }
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_output_stream(
            config,
            move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                float.clear();
                float.resize(data.len(), 0.0);
                callback(&mut float, channels);
                for (out, value) in data.iter_mut().zip(&float) {
                    *out = ((value.clamp(-1.0, 1.0) * 32_768.0) + 32_768.0) as u16;
                }
            },
            on_error,
            None,
        ),
        other => return Err(AudioIoError::UnsupportedSampleFormat(format!("{other:?}"))),
    };
    stream.map_err(|error| AudioIoError::Stream(error.to_string()))
}

/// `cpal` reports stream formats only; on Linux (ALSA `hw:` devices) those
/// are the hardware formats, while on Windows shared mode they're the mixer's.
pub(crate) fn probe_input_capabilities(
    device_name: Option<&str>,
) -> Result<InputCapabilities, AudioIoError> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let device = resolve(&host, Direction::Input, device_name)?;
    let name = device.name().map_err(backend_error)?;
    let default_config = default_config(&device, Direction::Input)?;
    let current_stream = CurrentFormat {
        sample_rate_hz: default_config.sample_rate().0,
        channels: default_config.channels(),
        resolution: resolution(default_config.sample_format()).ok_or_else(|| {
            AudioIoError::UnsupportedSampleFormat(format!("{:?}", default_config.sample_format()))
        })?,
    };
    let stream_formats = device
        .supported_input_configs()
        .map_err(backend_error)?
        .filter_map(|range| {
            Some(FormatRange {
                channels: range.channels(),
                min_sample_rate_hz: range.min_sample_rate().0,
                max_sample_rate_hz: range.max_sample_rate().0,
                resolution: resolution(range.sample_format())?,
            })
        })
        .collect();
    Ok(InputCapabilities {
        is_default: default_name.as_deref() == Some(name.as_str()),
        device_name: name,
        current_stream,
        stream_formats,
        current_physical: None,
        physical_formats: Vec::new(),
    })
}

pub(crate) fn set_input_physical_format(
    _device_name: &str,
    _format: &CurrentFormat,
) -> Result<(), AudioIoError> {
    Err(AudioIoError::UnsupportedOnPlatform(
        "changing a converter's physical format",
    ))
}
