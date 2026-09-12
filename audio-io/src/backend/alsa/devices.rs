//! Card and PCM enumeration through ALSA's control interface.
//!
//! Devices are listed per direction, named by their card (`USB PnP Audio
//! Device`), so the names match what a user sees elsewhere and what the
//! CoreAudio backend produces. A card with more than one PCM in a direction
//! gets its `hw:CARD,DEV` appended, since the card name alone is ambiguous.
//!
//! Which device is "default" is a question ALSA answers only through its
//! `default` PCM, so that gets opened non-blockingly once and asked which card
//! it landed on. If that fails, nothing is flagged rather than guessing.

use std::ffi::{c_int, c_uint, CStr, CString};
use std::ptr;

use crate::backend::Direction;
use crate::devices::AudioDeviceInfo;
use crate::error::AudioIoError;

use super::ffi;
use super::pcm::Pcm;

fn stream_of(direction: Direction) -> c_int {
    match direction {
        Direction::Input => ffi::STREAM_CAPTURE,
        Direction::Output => ffi::STREAM_PLAYBACK,
    }
}

/// `(card index, card name)` for every sound card present.
fn cards() -> Vec<(c_int, String)> {
    let mut cards = Vec::new();
    let mut card: c_int = -1;
    loop {
        // SAFETY: `card` is a live out-pointer; -1 starts the iteration.
        if unsafe { ffi::snd_card_next(&mut card) } < 0 || card < 0 {
            break;
        }
        if let Some(name) = card_name(card) {
            cards.push((card, name));
        }
    }
    cards
}

fn card_name(card: c_int) -> Option<String> {
    let ctl_name = CString::new(format!("hw:{card}")).ok()?;
    let mut ctl: *mut ffi::SndCtl = ptr::null_mut();
    // SAFETY: `ctl_name` is a valid C string; `ctl` is a live out-pointer.
    if unsafe { ffi::snd_ctl_open(&mut ctl, ctl_name.as_ptr(), 0) } < 0 || ctl.is_null() {
        return None;
    }
    let mut info: *mut ffi::SndCtlCardInfo = ptr::null_mut();
    // SAFETY: handles are live; the info block is freed before returning.
    let name = unsafe {
        let name = if ffi::snd_ctl_card_info_malloc(&mut info) == 0
            && ffi::snd_ctl_card_info(ctl, info) == 0
        {
            let text = ffi::snd_ctl_card_info_get_name(info);
            (!text.is_null()).then(|| CStr::from_ptr(text).to_string_lossy().into_owned())
        } else {
            None
        };
        if !info.is_null() {
            ffi::snd_ctl_card_info_free(info);
        }
        ffi::snd_ctl_close(ctl);
        name
    };
    name
}

/// PCM device indices on `card` that support `direction`.
fn pcm_devices(card: c_int, direction: Direction) -> Vec<c_int> {
    let Ok(ctl_name) = CString::new(format!("hw:{card}")) else {
        return Vec::new();
    };
    let mut ctl: *mut ffi::SndCtl = ptr::null_mut();
    // SAFETY: `ctl_name` is a valid C string; `ctl` is a live out-pointer.
    if unsafe { ffi::snd_ctl_open(&mut ctl, ctl_name.as_ptr(), 0) } < 0 || ctl.is_null() {
        return Vec::new();
    }
    let mut devices = Vec::new();
    let mut info: *mut ffi::SndPcmInfo = ptr::null_mut();
    // SAFETY: all handles are live and freed before returning; `snd_ctl_pcm_info`
    // reports whether the device supports the stream we set.
    unsafe {
        if ffi::snd_pcm_info_malloc(&mut info) == 0 {
            let mut device: c_int = -1;
            while ffi::snd_ctl_pcm_next_device(ctl, &mut device) == 0 && device >= 0 {
                ffi::snd_pcm_info_set_device(info, device as c_uint);
                ffi::snd_pcm_info_set_subdevice(info, 0);
                ffi::snd_pcm_info_set_stream(info, stream_of(direction));
                if ffi::snd_ctl_pcm_info(ctl, info) == 0 {
                    devices.push(device);
                }
            }
            ffi::snd_pcm_info_free(info);
        }
        ffi::snd_ctl_close(ctl);
    }
    devices
}

/// Opens ALSA's `default` PCM in `direction` to find out whether it exists at
/// all, and which card it lands on.
///
/// `None` means `default` has no slave in this direction, which is ordinary
/// rather than broken: a machine with no analogue input (any Pi) has a
/// `default` that plays but cannot capture. `Some(None)` means it opened but
/// belongs to no card -- a plugin PCM such as PipeWire's -- which is still
/// exactly what a caller should be routed through.
fn probe_default(direction: Direction) -> Option<Option<c_int>> {
    let pcm = Pcm::open("default", direction == Direction::Input, true).ok()?;
    let mut info: *mut ffi::SndPcmInfo = ptr::null_mut();
    // SAFETY: `pcm` is open for the duration; the info block is freed here.
    let card = unsafe {
        let card =
            if ffi::snd_pcm_info_malloc(&mut info) == 0 && ffi::snd_pcm_info(pcm.0, info) == 0 {
                Some(ffi::snd_pcm_info_get_card(info))
            } else {
                None
            };
        if !info.is_null() {
            ffi::snd_pcm_info_free(info);
        }
        card
    };
    pcm.close();
    Some(card.filter(|card| *card >= 0))
}

/// Every device in `direction`, with its `hw:` name for opening.
pub(crate) fn enumerate(direction: Direction) -> Vec<(AudioDeviceInfo, String)> {
    let default = probe_default(direction);
    let default_card = default.flatten();
    let mut listed = Vec::new();
    for (card, name) in cards() {
        let devices = pcm_devices(card, direction);
        let several = devices.len() > 1;
        for device in devices {
            let label = if several {
                format!("{name} (hw:{card},{device})")
            } else {
                name.clone()
            };
            listed.push((
                AudioDeviceInfo {
                    name: label,
                    is_default: default_card == Some(card),
                },
                format!("hw:{card},{device}"),
            ));
        }
    }
    // With no usable `default` in this direction, the first real device is the
    // only honest reading of "the default one", and saying so keeps a caller
    // from being told nothing is default while a working device sits listed.
    if default.is_none() {
        if let Some((info, _)) = listed.first_mut() {
            info.is_default = true;
        }
    }
    listed
}

pub(crate) fn list_devices(direction: Direction) -> Result<Vec<AudioDeviceInfo>, AudioIoError> {
    Ok(enumerate(direction)
        .into_iter()
        .map(|(info, _)| info)
        .collect())
}

/// The ALSA PCM name for `name` in `direction`; `None` means ALSA's own
/// `default` PCM, which is what a caller asking for "the default device"
/// means on Linux (and what PipeWire or dmix routes).
pub(crate) fn resolve(direction: Direction, name: Option<&str>) -> Result<String, AudioIoError> {
    let Some(name) = name else {
        return Ok(default_pcm(direction));
    };
    enumerate(direction)
        .into_iter()
        .find(|(info, _)| info.name == name)
        .map(|(_, pcm_name)| pcm_name)
        .ok_or_else(|| match direction {
            Direction::Input => AudioIoError::InputDeviceNotFound(name.to_string()),
            Direction::Output => AudioIoError::OutputDeviceNotFound(name.to_string()),
        })
}

/// What "no device named" opens in `direction`.
///
/// ALSA's own `default` PCM comes first when it works, because that is what
/// routes through PipeWire or dmix and so shares the device with everything
/// else on the machine. Unlike CoreAudio's default device, though, `default`
/// is a config alias that can have no slave in one direction, and delegating
/// to it there fails outright while a perfectly good device sits enumerated --
/// so fall back to the first of those.
fn default_pcm(direction: Direction) -> String {
    if probe_default(direction).is_some() {
        return "default".to_string();
    }
    enumerate(direction)
        .into_iter()
        .next()
        .map_or_else(|| "default".to_string(), |(_, pcm_name)| pcm_name)
}

/// The display name for a resolved PCM, for reporting back in `StreamFormat`.
pub(crate) fn display_name(direction: Direction, pcm_name: &str) -> String {
    enumerate(direction)
        .into_iter()
        .find(|(_, resolved)| resolved == pcm_name)
        .map_or_else(|| pcm_name.to_string(), |(info, _)| info.name)
}
