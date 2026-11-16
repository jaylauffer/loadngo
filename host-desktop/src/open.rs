//! Opening a web address, a file or a folder with whatever the system uses
//! for it: the default browser for an address, the default app for a file,
//! the file manager for a folder. What a double-click would do.
//!
//! Blocks until the system has taken the request (it starts the system's
//! opener and waits for it to hand off), so call it off the frame thread: an
//! offloaded job.

/// Opens `target`, an address such as `https://example.org/` or a path.
/// Platforms without an opener say so in the error.
pub fn open_in_system(target: &str) -> Result<(), String> {
    if target.trim().is_empty() {
        return Err("nothing to open".to_string());
    }
    imp::open(target)
}

/// Runs the system's opener program and reports why it refused, if it did.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_opener(program: &str, target: &str) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim();
    if said.is_empty() {
        Err(format!("{program} could not open {target}"))
    } else {
        Err(said.to_string())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    pub fn open(target: &str) -> Result<(), String> {
        super::run_opener("/usr/bin/open", target)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn open(target: &str) -> Result<(), String> {
        super::run_opener("xdg-open", target)
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    pub fn open(target: &str) -> Result<(), String> {
        let mut wide: Vec<u16> = std::ffi::OsStr::new(target).encode_wide().collect();
        wide.push(0);
        // SAFETY: both strings are NUL-terminated and outlive the call; the
        // shell copies what it keeps.
        let result = unsafe {
            ShellExecuteW(
                HWND::default(),
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        // The shell reports success as a value above 32.
        let code = result.0 as isize;
        if code > 32 {
            Ok(())
        } else {
            Err(format!("the shell could not open {target} (code {code})"))
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    pub fn open(_target: &str) -> Result<(), String> {
        Err("this platform has no opener".to_string())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nothing_is_not_opened() {
        assert!(super::open_in_system("  ").is_err());
    }
}
