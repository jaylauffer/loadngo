//! Moving a file or folder to the system's Trash (Recycle Bin), so a delete
//! can be undone from there. Blocks briefly; call it off the frame thread
//! (an offloaded job).

use std::path::Path;

/// Moves `path` to the Trash. Platforms without one say so in the error.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    imp::move_to_trash(path)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;

    use objc2::{class, msg_send, rc::autoreleasepool, runtime::AnyObject};

    pub fn move_to_trash(path: &Path) -> Result<(), String> {
        let text = path
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))?;
        let text = std::ffi::CString::new(text).map_err(|error| error.to_string())?;
        autoreleasepool(|_| unsafe {
            // NSFileManager is safe to use from any thread.
            let string: *mut AnyObject =
                msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()];
            let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: string];
            let manager: *mut AnyObject = msg_send![class!(NSFileManager), defaultManager];
            let mut error: *mut AnyObject = std::ptr::null_mut();
            let moved: bool = msg_send![
                manager,
                trashItemAtURL: url,
                resultingItemURL: std::ptr::null_mut::<*mut AnyObject>(),
                error: &mut error
            ];
            if moved {
                return Ok(());
            }
            let message = if error.is_null() {
                "the Trash refused it".to_string()
            } else {
                let description: *mut AnyObject = msg_send![error, localizedDescription];
                let utf8: *const std::ffi::c_char = msg_send![description, UTF8String];
                if utf8.is_null() {
                    "the Trash refused it".to_string()
                } else {
                    std::ffi::CStr::from_ptr(utf8)
                        .to_string_lossy()
                        .into_owned()
                }
            };
            Err(message)
        })
    }
}

/// The freedesktop.org Trash in the user's data folder: the item moves to
/// `Trash/files` with a `.trashinfo` record beside it in `Trash/info`, as
/// file managers expect. A file on another filesystem cannot be moved
/// there and is reported, not deleted.
#[cfg(target_os = "linux")]
mod imp {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn trash_dir() -> Result<PathBuf, String> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .ok_or("no home folder for the Trash")?;
        Ok(data.join("Trash"))
    }

    /// `YYYY-MM-DDThh:mm:ss` for a time in seconds since 1970 (UTC).
    fn deletion_date(secs: u64) -> String {
        let days = (secs / 86_400) as i64;
        let rest = secs % 86_400;
        // Civil date from days since 1970 (Howard Hinnant's algorithm).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
            rest / 3600,
            rest / 60 % 60,
            rest % 60
        )
    }

    fn encode(path: &Path) -> String {
        let mut out = String::new();
        for byte in path.to_string_lossy().bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }

    pub fn move_to_trash(path: &Path) -> Result<(), String> {
        let path = fs::canonicalize(path).map_err(|error| error.to_string())?;
        let trash = trash_dir()?;
        let files = trash.join("files");
        let info = trash.join("info");
        fs::create_dir_all(&files).map_err(|error| error.to_string())?;
        fs::create_dir_all(&info).map_err(|error| error.to_string())?;
        let name = path
            .file_name()
            .ok_or("nothing to move")?
            .to_string_lossy()
            .into_owned();
        // A free name in the Trash: "name", then "name.2", "name.3", ...
        let mut candidate = name.clone();
        let mut counter = 1;
        while files.join(&candidate).exists()
            || info.join(format!("{candidate}.trashinfo")).exists()
        {
            counter += 1;
            candidate = format!("{name}.{counter}");
        }
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let record = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            encode(&path),
            deletion_date(secs)
        );
        let info_path = info.join(format!("{candidate}.trashinfo"));
        fs::write(&info_path, record).map_err(|error| error.to_string())?;
        if let Err(error) = fs::rename(&path, files.join(&candidate)) {
            let _ = fs::remove_file(&info_path);
            return Err(if error.raw_os_error() == Some(libc::EXDEV) {
                "it is on another drive than the Trash".to_string()
            } else {
                error.to_string()
            });
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn deletion_dates_are_civil_dates() {
            assert_eq!(super::deletion_date(0), "1970-01-01T00:00:00");
            assert_eq!(super::deletion_date(1_760_140_800), "2025-10-11T00:00:00");
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
        SHFILEOPSTRUCTW,
    };

    pub fn move_to_trash(path: &Path) -> Result<(), String> {
        // The shell takes a list of paths ending in two NULs.
        let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
        from.push(0);
        from.push(0);
        let mut operation = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: PCWSTR(from.as_ptr()),
            fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
            ..Default::default()
        };
        let result = unsafe { SHFileOperationW(&mut operation) };
        if result == 0 && !operation.fAnyOperationsAborted.as_bool() {
            Ok(())
        } else {
            Err(format!("the Recycle Bin refused it (code {result})"))
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    use std::path::Path;

    pub fn move_to_trash(_path: &Path) -> Result<(), String> {
        Err("this platform has no Trash".to_string())
    }
}

/// Touches the real Trash, so it only runs when asked:
/// `cargo test -p loadngo-host-desktop --lib trash -- --ignored`.
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use std::path::PathBuf;

    fn trashed_copy(name: &str) -> PathBuf {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        if cfg!(target_os = "macos") {
            home.join(".Trash").join(name)
        } else {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"))
                .join("Trash/files")
                .join(name)
        }
    }

    #[test]
    #[ignore]
    fn a_scratch_file_goes_to_the_trash_and_is_removed_from_it_again() {
        let dir = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
        let name = format!("loadngo-trash-test-{}.txt", std::process::id());
        let file = dir.path().join(&name);
        std::fs::write(&file, "scratch").unwrap();
        super::move_to_trash(&file).unwrap();
        assert!(!file.exists());
        let copy = trashed_copy(&name);
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "scratch");
        std::fs::remove_file(&copy).unwrap();
        if cfg!(target_os = "linux") {
            let info = copy
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("info")
                .join(format!("{name}.trashinfo"));
            assert!(std::fs::read_to_string(&info).unwrap().contains("Path=/"));
            std::fs::remove_file(info).unwrap();
        }
    }
}
