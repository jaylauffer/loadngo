use windows::Win32::UI::WindowsAndMessaging::MAKELONG as WM_MAKELONG;

pub fn to_wstring(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn MAKELONG(lo: i32, hi: i32) -> i32 {
    WM_MAKELONG(lo as u16, hi as u16) as i32
}
