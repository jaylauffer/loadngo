//! The Linux backend: ALSA directly, through `libasound`.
//!
//! See `loadngo/docs/AUDIO_BACKENDS.md`. Shaped to match the CoreAudio
//! backend's contract, with the differences ALSA forces:
//!
//! - **No callback model.** Each stream owns a thread blocking in
//!   `snd_pcm_readi`/`writei` one period at a time (`io.rs`).
//! - **Formats are the hardware's.** `hw:` parameters describe the converter,
//!   so there is no separate physical format to probe or switch (`formats.rs`).
//! - **Devices are cards plus PCM indices**, named by card (`devices.rs`).
//! - **Sample formats vary by device**, so the backend converts to and from
//!   `f32` (`pcm.rs`); the USB interface this was built against is `S16_LE`
//!   only, where CoreAudio always hands over float.

mod devices;
mod ffi;
mod formats;
mod io;
mod pcm;

pub(crate) use devices::list_devices;
pub(crate) use formats::{describe, probe_input_capabilities, set_input_physical_format};
pub(crate) use io::{open_input, open_output, Stream};

/// Period used when a caller expresses no preference: 1024 frames is ~21 ms at
/// 48 kHz, which every device this has met accepts, and `LiveMonitor` asks for
/// something smaller when latency matters.
pub(crate) const DEFAULT_PERIOD_FRAMES: u32 = 1_024;
