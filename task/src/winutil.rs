use std::os::windows::ffi::OsStrExt;

use windows::Win32::UI::WindowsAndMessaging::WM_APP;

pub const WM_SPLITTERREPOS: u32 = WM_APP + 1220;

pub fn to_wstring(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn MAKELONG(lo: i32, hi: i32) -> i32 {
    ((hi & 0xffff) << 16) | (lo & 0xffff)
}
