//! Free space on the volume holding a path, for writers that must not fill a disk. The
//! Archive CAS tools keep a floor below which they refuse to write (`docs/ARCHIVE_CAS.md`,
//! "Free space"). Jay, 2026-10-14: monitor disk space and never reach the limit; the Mac's
//! internal disk had reached 94% that morning.

use anyhow::{bail, Result};
use std::io;
use std::path::Path;

const GIB: u64 = 1 << 30;

/// Space on one volume, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Space {
    /// What this process may still write.
    pub available: u64,
    pub total: u64,
}

/// The default floor for a volume of `total` bytes: the larger of 10 GiB and 5% of it.
#[must_use]
pub fn default_floor(total: u64) -> u64 {
    (10 * GIB).max(total / 20)
}

/// Bytes in `gib` GiB, for a `--min-free-gib` option.
#[must_use]
pub fn gib(gib: u64) -> u64 {
    gib.saturating_mul(GIB)
}

/// The environment variable that replaces the default floor, in whole GiB (CI sets 0).
pub const MIN_FREE_ENV: &str = "LOADNGO_CAS_MIN_FREE_GIB";

/// The floor `LOADNGO_CAS_MIN_FREE_GIB` sets, if it is set to a whole number of GiB.
#[must_use]
pub fn floor_from_env() -> Option<u64> {
    std::env::var(MIN_FREE_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .map(gib)
}

/// Reads a `--min-free-gib` value: whole GiB to keep free on the volume.
///
/// # Errors
/// When the value is missing or not a whole number.
pub fn parse_gib(value: Option<String>) -> Result<u64> {
    let value = value.ok_or_else(|| anyhow::anyhow!("--min-free-gib needs a value"))?;
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("--min-free-gib takes whole GiB, not {value:?}"))
}

/// The space on the volume holding `path`, or the nearest existing directory above it when
/// `path` does not exist yet.
///
/// # Errors
/// When no ancestor exists or the system cannot report the volume's space.
pub fn space(path: &Path) -> io::Result<Space> {
    let mut at = path;
    while !at.exists() {
        at = at.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no existing directory above {}", path.display()),
            )
        })?;
        if at.as_os_str().is_empty() {
            at = Path::new(".");
        }
    }
    volume_space(at)
}

/// Refuses a write of `bytes` to the volume holding `path` when it would leave less than
/// `floor` free (`None`: [`default_floor`]).
///
/// # Errors
/// When the write would cross the floor, or the space cannot be read.
pub fn ensure_room(path: &Path, bytes: u64, floor: Option<u64>) -> Result<()> {
    let space = space(path)?;
    check(
        path,
        bytes,
        floor.unwrap_or_else(|| default_floor(space.total)),
        space,
    )
}

fn check(path: &Path, bytes: u64, floor: u64, space: Space) -> Result<()> {
    let left = space.available.saturating_sub(bytes);
    if left < floor {
        bail!(
            "not enough free space for {}: writing {} would leave {} free of {} on its volume, \
             below the floor of {} (the larger of 10 GiB and 5% of the volume unless \
             --min-free-gib sets another)",
            path.display(),
            human(bytes),
            human(left),
            human(space.total),
            human(floor)
        );
    }
    Ok(())
}

/// Bytes as GiB with one decimal, or MiB below a GiB.
fn human(bytes: u64) -> String {
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1 << 20) as f64)
    }
}

// statvfs's fields are u64 on some systems and u32 on others (macOS's block counts).
#[cfg(unix)]
#[allow(clippy::useless_conversion)]
fn volume_space(path: &Path) -> io::Result<Space> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path holds a NUL byte"))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: name is a NUL-terminated path and stat is a valid statvfs to write into.
    if unsafe { libc::statvfs(name.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let block = u64::from(stat.f_frsize);
    Ok(Space {
        available: u64::from(stat.f_bavail).saturating_mul(block),
        total: u64::from(stat.f_blocks).saturating_mul(block),
    })
}

#[cfg(windows)]
fn volume_space(path: &Path) -> io::Result<Space> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let (mut available, mut total) = (0u64, 0u64);
    // SAFETY: wide is NUL-terminated and both outputs are valid u64s to write into.
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut available),
            Some(&mut total),
            None,
        )
    }
    .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(Space { available, total })
}

#[cfg(not(any(unix, windows)))]
fn volume_space(_path: &Path) -> io::Result<Space> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "free space is not known on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::{check, default_floor, space, Space, GIB};
    use std::path::Path;

    #[test]
    fn the_floor_is_the_larger_of_ten_gib_and_five_percent() {
        assert_eq!(default_floor(100 * GIB), 10 * GIB);
        assert_eq!(default_floor(926 * GIB), 926 * GIB / 20);
    }

    #[test]
    fn a_write_that_would_cross_the_floor_is_refused_and_says_why() {
        let space = Space {
            available: 60 * GIB,
            total: 1000 * GIB,
        };
        let floor = default_floor(space.total);
        assert!(check(Path::new("/v/a"), 5 * GIB, floor, space).is_ok());
        let error = check(Path::new("/v/a"), 12 * GIB, floor, space)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not enough free space"), "{error}");
        assert!(error.contains("48.0 GiB free of 1000.0 GiB"), "{error}");
        assert!(error.contains("floor of 50.0 GiB"), "{error}");
    }

    #[test]
    fn the_space_of_a_path_not_yet_made_is_its_nearest_directory_s() {
        let dir = tempfile::tempdir().unwrap();
        let here = space(dir.path()).unwrap();
        let later = space(&dir.path().join("not/yet/made.bin")).unwrap();
        assert!(here.total > 0 && here.available <= here.total);
        assert_eq!(here.total, later.total);
    }
}
