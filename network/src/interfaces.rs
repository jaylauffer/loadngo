//! Local network interfaces, for choosing where to join a multicast group.
//!
//! Enumeration is per platform (`getifaddrs` on Unix, `GetAdaptersAddresses`
//! on Windows); choosing among the interfaces is plain code, the same on every
//! platform.

use std::io;
use std::net::{IpAddr, SocketAddr};

/// One address on a local interface that is up and can send multicast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceAddress {
    /// The interface index, as `join_multicast_v6` and IPv6 scope ids take it.
    pub index: u32,
    pub ip: IpAddr,
    pub is_loopback: bool,
}

/// Addresses on every local interface that is up and can send multicast.
pub fn multicast_interface_addresses() -> io::Result<Vec<InterfaceAddress>> {
    platform::multicast_interface_addresses()
}

/// The interfaces to join an IPv6 multicast group on, for a socket bound to
/// `bind_addr`; see [`select_ipv6_multicast_interfaces`]. Fails when there is
/// none.
pub fn ipv6_multicast_interfaces(bind_addr: SocketAddr) -> io::Result<Vec<u32>> {
    if let SocketAddr::V6(addr) = bind_addr {
        if addr.scope_id() != 0 {
            return Ok(vec![addr.scope_id()]);
        }
    }
    let candidates = multicast_interface_addresses()?;
    let selected = select_ipv6_multicast_interfaces(bind_addr, &candidates);
    if selected.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no IPv6 multicast-capable interfaces found",
        ));
    }
    Ok(selected)
}

/// Chooses interfaces for IPv6 multicast from `candidates`:
///
/// 1. an IPv6 bind address with a scope id names its interface;
/// 2. a bind address that is not unspecified selects the interfaces that own
///    it, of either family;
/// 3. otherwise, or when no interface owns it, every interface with an IPv6
///    address.
///
/// Within 2 and 3, loopback interfaces are chosen only when nothing else is.
pub fn select_ipv6_multicast_interfaces(
    bind_addr: SocketAddr,
    candidates: &[InterfaceAddress],
) -> Vec<u32> {
    if let SocketAddr::V6(addr) = bind_addr {
        if addr.scope_id() != 0 {
            return vec![addr.scope_id()];
        }
    }
    let bind_ip = bind_addr.ip();
    if !bind_ip.is_unspecified() {
        let owning = prefer_non_loopback(candidates.iter().filter(|c| c.ip == bind_ip));
        if !owning.is_empty() {
            return owning;
        }
    }
    prefer_non_loopback(candidates.iter().filter(|c| c.ip.is_ipv6()))
}

fn prefer_non_loopback<'a>(candidates: impl Iterator<Item = &'a InterfaceAddress>) -> Vec<u32> {
    let mut non_loopback = Vec::new();
    let mut loopback = Vec::new();
    for candidate in candidates {
        let target = if candidate.is_loopback {
            &mut loopback
        } else {
            &mut non_loopback
        };
        if !target.contains(&candidate.index) {
            target.push(candidate.index);
        }
    }
    if non_loopback.is_empty() {
        loopback
    } else {
        non_loopback
    }
}

#[cfg(unix)]
mod platform {
    use super::InterfaceAddress;
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    // libc's interface field and flag types differ between Unix targets, so the
    // casts are needed on some even where they are no-ops on others.
    #[allow(clippy::unnecessary_cast)]
    pub fn multicast_interface_addresses() -> io::Result<Vec<InterfaceAddress>> {
        let mut head = std::ptr::null_mut();
        // SAFETY: getifaddrs writes a list head we free below with freeifaddrs.
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut addresses = Vec::new();
        let mut cursor = head;
        while !cursor.is_null() {
            // SAFETY: cursor is a non-null node of the list getifaddrs returned.
            let entry = unsafe { &*cursor };
            cursor = entry.ifa_next;
            if entry.ifa_addr.is_null() {
                continue;
            }
            let flags = entry.ifa_flags as u32;
            if flags & libc::IFF_UP as u32 == 0 || flags & libc::IFF_MULTICAST as u32 == 0 {
                continue;
            }
            // SAFETY: ifa_addr is non-null and its family says which sockaddr it is.
            let ip = match unsafe { (*entry.ifa_addr).sa_family } as i32 {
                libc::AF_INET => {
                    let addr = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr)))
                }
                libc::AF_INET6 => {
                    let addr = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in6) };
                    IpAddr::V6(Ipv6Addr::from(addr.sin6_addr.s6_addr))
                }
                _ => continue,
            };
            // SAFETY: ifa_name is the interface's NUL-terminated name.
            let index = unsafe { libc::if_nametoindex(entry.ifa_name) };
            if index == 0 {
                continue;
            }
            addresses.push(InterfaceAddress {
                index,
                ip,
                is_loopback: flags & libc::IFF_LOOPBACK as u32 != 0,
            });
        }
        // SAFETY: head came from getifaddrs and is freed once.
        unsafe { libc::freeifaddrs(head) };
        Ok(addresses)
    }
}

#[cfg(windows)]
mod platform {
    use super::InterfaceAddress;
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_FRIENDLY_NAME, GAA_FLAG_SKIP_MULTICAST, IF_TYPE_SOFTWARE_LOOPBACK,
        IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_NO_MULTICAST,
    };
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6,
    };

    /// Microsoft's suggested first buffer; the call reports the size it needs
    /// when the adapter list is larger, and the list can grow in between.
    const FIRST_BUFFER_BYTES: u32 = 15 * 1024;
    const ATTEMPTS: usize = 4;

    pub fn multicast_interface_addresses() -> io::Result<Vec<InterfaceAddress>> {
        let flags = GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER
            | GAA_FLAG_SKIP_FRIENDLY_NAME;
        let mut size = FIRST_BUFFER_BYTES;
        for _ in 0..ATTEMPTS {
            // u64 elements keep the buffer aligned for IP_ADAPTER_ADDRESSES_LH.
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            // SAFETY: head points at `size` writable, suitably aligned bytes.
            let status = unsafe {
                GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(head), &mut size)
            };
            if status == ERROR_BUFFER_OVERFLOW.0 {
                continue;
            }
            if status != NO_ERROR.0 {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            // SAFETY: on success the buffer holds the adapter list starting at head.
            return Ok(unsafe { collect(head) });
        }
        Err(io::Error::other(
            "the adapter list kept growing while it was being read",
        ))
    }

    /// # Safety
    /// `head` must be the first adapter of a list `GetAdaptersAddresses` filled.
    unsafe fn collect(head: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<InterfaceAddress> {
        let mut addresses = Vec::new();
        let mut adapter = head;
        while let Some(entry) = unsafe { adapter.as_ref() } {
            adapter = entry.Next;
            let no_multicast = unsafe { entry.Anonymous2.Flags } & IP_ADAPTER_NO_MULTICAST != 0;
            if entry.OperStatus != IfOperStatusUp || no_multicast {
                continue;
            }
            // Current Windows gives an interface one index for both families;
            // prefer the IPv6 one, which is what multicast joins here use.
            let index = match entry.Ipv6IfIndex {
                0 => unsafe { entry.Anonymous1.Anonymous.IfIndex },
                index => index,
            };
            if index == 0 {
                continue;
            }
            let is_loopback = entry.IfType == IF_TYPE_SOFTWARE_LOOPBACK;
            let mut unicast = entry.FirstUnicastAddress;
            while let Some(address) = unsafe { unicast.as_ref() } {
                unicast = address.Next;
                let sockaddr = address.Address.lpSockaddr;
                let Some(family) = (unsafe { sockaddr.as_ref() }).map(|s| s.sa_family) else {
                    continue;
                };
                let ip = if family == AF_INET {
                    let addr = unsafe { &*sockaddr.cast::<SOCKADDR_IN>() };
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(unsafe {
                        addr.sin_addr.S_un.S_addr
                    })))
                } else if family == AF_INET6 {
                    let addr = unsafe { &*sockaddr.cast::<SOCKADDR_IN6>() };
                    IpAddr::V6(Ipv6Addr::from(unsafe { addr.sin6_addr.u.Byte }))
                } else {
                    continue;
                };
                addresses.push(InterfaceAddress {
                    index,
                    ip,
                    is_loopback,
                });
            }
        }
        addresses
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::InterfaceAddress;
    use std::io;

    pub fn multicast_interface_addresses() -> io::Result<Vec<InterfaceAddress>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "listing network interfaces is not implemented on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV6};

    fn address(index: u32, ip: &str, is_loopback: bool) -> InterfaceAddress {
        InterfaceAddress {
            index,
            ip: ip.parse().unwrap(),
            is_loopback,
        }
    }

    fn bind(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 5143)
    }

    #[test]
    fn bind_address_selects_the_non_loopback_interface_that_owns_it() {
        let candidates = [
            address(7, "10.0.0.5", false),
            address(16, "192.168.1.146", false),
            address(20, "::1", false),
        ];
        assert_eq!(
            select_ipv6_multicast_interfaces(bind("192.168.1.146"), &candidates),
            vec![16]
        );
    }

    #[test]
    fn loopback_is_chosen_only_when_nothing_else_owns_the_address() {
        let candidates = [
            address(1, "127.0.0.1", true),
            address(7, "192.168.1.146", false),
        ];
        assert_eq!(
            select_ipv6_multicast_interfaces(bind("127.0.0.1"), &candidates),
            vec![1]
        );
    }

    #[test]
    fn unspecified_or_unowned_bind_uses_every_ipv6_interface_once() {
        let candidates = [
            address(1, "::1", true),
            address(4, "fe80::1", false),
            address(4, "2001:db8::4", false),
            address(5, "10.0.0.5", false),
            address(6, "fe80::6", false),
        ];
        assert_eq!(
            select_ipv6_multicast_interfaces(bind("::"), &candidates),
            vec![4, 6]
        );
        assert_eq!(
            select_ipv6_multicast_interfaces(bind("0.0.0.0"), &candidates),
            vec![4, 6]
        );
        assert_eq!(
            select_ipv6_multicast_interfaces(bind("192.168.9.9"), &candidates),
            vec![4, 6]
        );
        assert_eq!(
            select_ipv6_multicast_interfaces(bind("::"), &[address(1, "::1", true)]),
            vec![1]
        );
        assert!(select_ipv6_multicast_interfaces(bind("::"), &[]).is_empty());
    }

    #[test]
    fn scope_id_names_the_interface_without_listing_any() {
        let scoped = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 5143, 0, 9));
        assert_eq!(
            select_ipv6_multicast_interfaces(scoped, &[address(4, "fe80::1", false)]),
            vec![9]
        );
        assert_eq!(ipv6_multicast_interfaces(scoped).unwrap(), vec![9]);
    }

    /// Runs the platform enumeration for real. What a machine has varies, so
    /// this checks consistency rather than a fixed list.
    #[test]
    fn this_machine_lists_its_multicast_interfaces() {
        let addresses = multicast_interface_addresses().expect("listing interfaces");
        eprintln!("multicast-capable interface addresses: {addresses:?}");
        assert!(addresses.iter().all(|address| address.index != 0));

        let unspecified = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 5143);
        let with_ipv6 = addresses
            .iter()
            .any(|address| address.ip.is_ipv6() && !address.is_loopback);
        if with_ipv6 {
            let chosen = ipv6_multicast_interfaces(unspecified).expect("an IPv6 interface");
            assert!(chosen.iter().all(|index| addresses
                .iter()
                .any(|address| address.index == *index && !address.is_loopback)));
        }
    }
}
