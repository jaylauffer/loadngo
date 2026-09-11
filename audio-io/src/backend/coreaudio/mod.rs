//! The macOS backend: CoreAudio's HAL, directly. See
//! `loadngo/docs/AUDIO_BACKENDS.md` for why this replaced `cpal` here.

mod devices;
mod formats;
mod hal;
mod io;

pub(crate) use devices::{describe, list_devices};
pub(crate) use formats::{probe_input_capabilities, set_input_physical_format};
pub(crate) use io::{open_input, open_output, Stream};
