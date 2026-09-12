//! Minimal bindings to `libasound`, declared here rather than pulled in as a
//! crate (`alsa-sys` needs `pkg-config` and a target ALSA install, which is
//! also what stops `cargo check --target aarch64-unknown-linux-gnu` from
//! working on a Mac). Only what this backend calls is declared.
//!
//! Constant values were read off dolores (alsa-lib 1.2.14, aarch64) with a C
//! probe, because ALSA's enums are implicit in the headers and can't be
//! grepped: `SND_PCM_ACCESS_RW_INTERLEAVED = 3`, `SND_PCM_STREAM_CAPTURE = 1`,
//! `S16_LE = 2`, `S32_LE = 10`, `FLOAT_LE = 14`, `S24_3LE = 32`.

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};

pub type SndPcm = c_void;
pub type SndPcmHwParams = c_void;
pub type SndPcmInfo = c_void;
pub type SndCtl = c_void;
pub type SndCtlCardInfo = c_void;
pub type Uframes = c_ulong;
pub type Sframes = c_long;

pub const STREAM_PLAYBACK: c_int = 0;
pub const STREAM_CAPTURE: c_int = 1;
pub const ACCESS_RW_INTERLEAVED: c_int = 3;
pub const NONBLOCK: c_int = 1;

pub const FORMAT_U8: c_int = 1;
pub const FORMAT_S16_LE: c_int = 2;
pub const FORMAT_S32_LE: c_int = 10;
pub const FORMAT_FLOAT_LE: c_int = 14;
pub const FORMAT_S24_3LE: c_int = 32;

pub const ENODEV: c_int = 19;
pub const EPIPE: c_int = 32;
pub const ESTRPIPE: c_int = 86;

#[link(name = "asound")]
extern "C" {
    pub fn snd_strerror(errnum: c_int) -> *const c_char;

    pub fn snd_pcm_open(
        pcm: *mut *mut SndPcm,
        name: *const c_char,
        stream: c_int,
        mode: c_int,
    ) -> c_int;
    pub fn snd_pcm_close(pcm: *mut SndPcm) -> c_int;
    pub fn snd_pcm_prepare(pcm: *mut SndPcm) -> c_int;
    pub fn snd_pcm_drop(pcm: *mut SndPcm) -> c_int;
    pub fn snd_pcm_readi(pcm: *mut SndPcm, buffer: *mut c_void, size: Uframes) -> Sframes;
    pub fn snd_pcm_writei(pcm: *mut SndPcm, buffer: *const c_void, size: Uframes) -> Sframes;
    pub fn snd_pcm_recover(pcm: *mut SndPcm, err: c_int, silent: c_int) -> c_int;

    pub fn snd_pcm_hw_params_malloc(ptr: *mut *mut SndPcmHwParams) -> c_int;
    pub fn snd_pcm_hw_params_free(obj: *mut SndPcmHwParams);
    pub fn snd_pcm_hw_params_any(pcm: *mut SndPcm, params: *mut SndPcmHwParams) -> c_int;
    pub fn snd_pcm_hw_params(pcm: *mut SndPcm, params: *mut SndPcmHwParams) -> c_int;
    pub fn snd_pcm_hw_params_set_access(
        pcm: *mut SndPcm,
        params: *mut SndPcmHwParams,
        access: c_int,
    ) -> c_int;
    pub fn snd_pcm_hw_params_set_format(
        pcm: *mut SndPcm,
        params: *mut SndPcmHwParams,
        format: c_int,
    ) -> c_int;
    pub fn snd_pcm_hw_params_test_format(
        pcm: *mut SndPcm,
        params: *mut SndPcmHwParams,
        format: c_int,
    ) -> c_int;
    pub fn snd_pcm_hw_params_set_channels(
        pcm: *mut SndPcm,
        params: *mut SndPcmHwParams,
        val: c_uint,
    ) -> c_int;
    pub fn snd_pcm_hw_params_get_channels_min(
        params: *const SndPcmHwParams,
        val: *mut c_uint,
    ) -> c_int;
    pub fn snd_pcm_hw_params_get_channels_max(
        params: *const SndPcmHwParams,
        val: *mut c_uint,
    ) -> c_int;
    pub fn snd_pcm_hw_params_set_rate(
        pcm: *mut SndPcm,
        params: *mut SndPcmHwParams,
        val: c_uint,
        dir: c_int,
    ) -> c_int;
    pub fn snd_pcm_hw_params_test_rate(
        pcm: *mut SndPcm,
        params: *mut SndPcmHwParams,
        val: c_uint,
        dir: c_int,
    ) -> c_int;
    pub fn snd_pcm_hw_params_get_rate_min(
        params: *const SndPcmHwParams,
        val: *mut c_uint,
        dir: *mut c_int,
    ) -> c_int;
    pub fn snd_pcm_hw_params_get_rate_max(
        params: *const SndPcmHwParams,
        val: *mut c_uint,
        dir: *mut c_int,
    ) -> c_int;
    pub fn snd_pcm_hw_params_set_rate_resample(
        pcm: *mut SndPcm,
        params: *mut SndPcmHwParams,
        val: c_uint,
    ) -> c_int;
    pub fn snd_pcm_hw_params_set_period_size_near(
        pcm: *mut SndPcm,
        params: *mut SndPcmHwParams,
        val: *mut Uframes,
        dir: *mut c_int,
    ) -> c_int;
    pub fn snd_pcm_hw_params_set_buffer_size_near(
        pcm: *mut SndPcm,
        params: *mut SndPcmHwParams,
        val: *mut Uframes,
    ) -> c_int;
    pub fn snd_pcm_hw_params_get_period_size(
        params: *const SndPcmHwParams,
        val: *mut Uframes,
        dir: *mut c_int,
    ) -> c_int;

    pub fn snd_card_next(card: *mut c_int) -> c_int;
    pub fn snd_ctl_open(ctl: *mut *mut SndCtl, name: *const c_char, mode: c_int) -> c_int;
    pub fn snd_ctl_close(ctl: *mut SndCtl) -> c_int;
    pub fn snd_ctl_card_info_malloc(ptr: *mut *mut SndCtlCardInfo) -> c_int;
    pub fn snd_ctl_card_info_free(obj: *mut SndCtlCardInfo);
    pub fn snd_ctl_card_info(ctl: *mut SndCtl, info: *mut SndCtlCardInfo) -> c_int;
    pub fn snd_ctl_card_info_get_name(info: *const SndCtlCardInfo) -> *const c_char;
    pub fn snd_ctl_pcm_next_device(ctl: *mut SndCtl, device: *mut c_int) -> c_int;
    pub fn snd_ctl_pcm_info(ctl: *mut SndCtl, info: *mut SndPcmInfo) -> c_int;

    pub fn snd_pcm_info_malloc(ptr: *mut *mut SndPcmInfo) -> c_int;
    pub fn snd_pcm_info_free(obj: *mut SndPcmInfo);
    pub fn snd_pcm_info_set_device(info: *mut SndPcmInfo, device: c_uint);
    pub fn snd_pcm_info_set_subdevice(info: *mut SndPcmInfo, subdevice: c_uint);
    pub fn snd_pcm_info_set_stream(info: *mut SndPcmInfo, stream: c_int);
    pub fn snd_pcm_info_get_card(info: *const SndPcmInfo) -> c_int;
    pub fn snd_pcm_info(pcm: *mut SndPcm, info: *mut SndPcmInfo) -> c_int;
}

/// ALSA's message for a negative return code.
pub fn error_text(code: c_int) -> String {
    // SAFETY: `snd_strerror` returns a static C string for any code.
    let text = unsafe { std::ffi::CStr::from_ptr(snd_strerror(code)) };
    text.to_string_lossy().into_owned()
}
