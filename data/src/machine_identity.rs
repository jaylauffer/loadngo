//! This machine's identity, as the C++ `Device::GetMachineHash` (`MachineId.cpp`) gave
//! it: an id the operating system keeps for the machine, not an environment variable.
//! `HOSTNAME` is not exported on macOS or in a Linux systemd session, so ids that hashed
//! it saw `unknown-host` on every machine.

use std::sync::OnceLock;

/// Bytes that identify this machine, read once per process: the OS machine id, else the
/// host name, else a fixed placeholder (ids then rely on the process id and clock).
pub fn identity() -> &'static [u8] {
    static IDENTITY: OnceLock<Vec<u8>> = OnceLock::new();
    IDENTITY.get_or_init(|| {
        platform_machine_id()
            .or_else(host_name)
            .unwrap_or_else(|| b"unknown-machine".to_vec())
    })
}

/// systemd's `/etc/machine-id`, or D-Bus's copy on systems without systemd.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn platform_machine_id() -> Option<Vec<u8>> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .map(|bytes| bytes.trim_ascii().to_vec())
        .find(|id| !id.is_empty())
}

/// The hardware UUID (`IOPlatformUUID`), through `gethostuuid`.
#[cfg(target_os = "macos")]
fn platform_machine_id() -> Option<Vec<u8>> {
    let mut uuid = [0u8; 16];
    let wait = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `uuid` is the 16-byte `uuid_t` gethostuuid writes; `wait` outlives the call.
    let status = unsafe { libc::gethostuuid(uuid.as_mut_ptr(), &wait) };
    (status == 0 && uuid != [0u8; 16]).then(|| uuid.to_vec())
}

/// `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid`, set when Windows is installed.
#[cfg(windows)]
fn platform_machine_id() -> Option<Vec<u8>> {
    use windows::core::w;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

    let mut buffer = [0u16; 128];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: `buffer` holds `size` bytes, and RegGetValueW writes at most that many.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Cryptography"),
            w!("MachineGuid"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let chars = (size as usize / 2).min(buffer.len());
    let guid = String::from_utf16_lossy(&buffer[..chars]);
    let guid = guid.trim_end_matches('\0').trim();
    (!guid.is_empty()).then(|| guid.as_bytes().to_vec())
}

/// iOS has no readable machine id outside UIKit; the host name stands in.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
fn platform_machine_id() -> Option<Vec<u8>> {
    None
}

#[cfg(unix)]
fn host_name() -> Option<Vec<u8>> {
    let mut buffer = [0u8; 256];
    // SAFETY: gethostname writes at most `buffer.len()` bytes into `buffer`.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return None;
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    (end > 0).then(|| buffer[..end].to_vec())
}

#[cfg(windows)]
fn host_name() -> Option<Vec<u8>> {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .map(String::into_bytes)
}

#[cfg(not(any(unix, windows)))]
fn host_name() -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_machine_has_an_identity_from_the_os() {
        // Every platform CI and the lab run on has one; the placeholder is a fallback.
        assert_ne!(identity(), b"unknown-machine");
        assert!(platform_machine_id().is_some() || cfg!(target_os = "ios"));
    }
}
