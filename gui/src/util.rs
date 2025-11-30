use std::os::windows::ffi::OsStrExt;

/// Convert a Rust string into a null-terminated UTF-16 buffer.
pub fn to_wstring(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
