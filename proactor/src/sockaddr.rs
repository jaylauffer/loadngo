//! The one place raw OS address representations become loadngo types.
//!
//! ## Why this module exists
//!
//! `loadngo-proactor` is the platform seam for networking in this
//! workspace: no crate above it names a `libc` or `windows` address type.
//! `network/` works in `std::net` and `socket2` types, `host-desktop`
//! touches none of this at all. That boundary is worth keeping, and this
//! module is where it is actually enforced -- [`crate::io_port`] defines
//! [`PeerAddr`] and the rest of the public surface without mentioning
//! `libc` at all, and the backends convert through here.
//!
//! ## Why the conversions are split by platform rather than cast inline
//!
//! The address types genuinely differ in *shape*, not just in name:
//!
//! | type | Linux / Android | macOS / BSD |
//! | --- | --- | --- |
//! | `sa_family_t` | `u16` | `u8` |
//! | `c_char` (in `sun_path`) | unsigned on aarch64 | signed |
//!
//! Written inline in shared code, that produces an expression no lint
//! configuration can be happy with on every target at once: `as u16`
//! is flagged `unnecessary_cast` where the type is already `u16`, and
//! `u16::from` is flagged `useless_conversion` in exactly the same place,
//! while both are *required* on the other platform. The first cut at this
//! carried an `#[allow]` for precisely that reason.
//!
//! Splitting the one primitive that differs into per-target functions
//! removes the ambiguity instead of suppressing it: each body is compiled
//! only where its types are concrete, so each is unambiguously correct
//! and neither lint fires. Adding a platform means adding a branch here,
//! not touching shared code.

use crate::PeerAddr;
use std::path::PathBuf;

/// The address family, widened to a single canonical type.
///
/// `sa_family_t` is `u16` on Linux and Android, so this is the identity.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[inline]
fn family_of(storage: &libc::sockaddr_storage) -> u16 {
    storage.ss_family
}

/// The address family, widened to a single canonical type.
///
/// `sa_family_t` is `u8` on macOS and the BSDs, so this genuinely widens.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
#[inline]
fn family_of(storage: &libc::sockaddr_storage) -> u16 {
    u16::from(storage.ss_family)
}

/// Decodes a kernel-filled `sockaddr_storage` into a [`PeerAddr`].
///
/// Shared by every Unix backend (`uring`, `kqueue`, `epoll`). It was
/// previously copied into each of them, and every copy carried the same
/// descriptor-leaking bug -- see `docs/PROACTOR_IOPORT_DEFECTS.md`.
///
/// `AF_UNIX` is checked *before* handing the storage to `socket2`, which
/// only decodes IP families and would otherwise report a perfectly normal
/// Unix peer as undecodable.
pub(crate) fn peer_addr_from_storage(
    storage: &libc::sockaddr_storage,
    len: libc::socklen_t,
) -> PeerAddr {
    let family = family_of(storage);

    if family == libc::AF_UNIX as u16 {
        return PeerAddr::Unix {
            path: unix_path_from_storage(storage, len),
        };
    }

    match unsafe { socket2::SockAddr::new(*storage, len) }.as_socket() {
        Some(addr) => PeerAddr::Ip(addr),
        None => PeerAddr::Unknown { family },
    }
}

/// Extracts the filesystem path from an `AF_UNIX` `sockaddr_un`, or
/// `None` when there isn't one.
///
/// Two distinct cases produce `None`, and both are normal rather than
/// errors:
///
/// - `len` no larger than the family field: an *unnamed* socket. This is
///   the usual case for an accepted peer, since a connecting client
///   rarely calls `bind`.
/// - a leading NUL in `sun_path`: Linux's abstract namespace, which has a
///   name but not a filesystem path.
fn unix_path_from_storage(
    storage: &libc::sockaddr_storage,
    len: libc::socklen_t,
) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let family_len = std::mem::size_of::<libc::sa_family_t>() as libc::socklen_t;
    if len <= family_len {
        return None;
    }

    // SAFETY: the caller checked the family is AF_UNIX, so the storage
    // really is a `sockaddr_un`, and `sockaddr_storage` is defined to be
    // large enough and suitably aligned for any address family.
    let sun = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_un>() };

    // `sun_path` is `[c_char]`, signed on darwin and unsigned on aarch64
    // Linux. Reinterpret the whole run as bytes once, rather than a
    // per-element cast that is redundant on one target and required on
    // another.
    //
    // SAFETY: `c_char` and `u8` have identical size and alignment on every
    // supported target; this changes only the signedness of the view, and
    // the slice does not outlive `sun`'s borrow.
    let path_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(sun.sun_path.as_ptr().cast::<u8>(), sun.sun_path.len())
    };

    let path_len = (len - family_len) as usize;
    let raw = &path_bytes[..path_len.min(path_bytes.len())];
    if raw.first().is_none_or(|first| *first == 0) {
        return None;
    }

    let bytes: Vec<u8> = raw.iter().copied().take_while(|byte| *byte != 0).collect();
    if bytes.is_empty() {
        return None;
    }

    Some(PathBuf::from(OsString::from_vec(bytes)))
}
