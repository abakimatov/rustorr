//! Network interfaces as Go's `net.Interfaces` and `Interface.Addrs` report
//! them, which both the Bonjour and the DLNA selection rules work on.

use std::{collections::BTreeMap, net::IpAddr};

use nix::{ifaddrs::getifaddrs, net::if_::InterfaceFlags};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub index: u32,
    pub up: bool,
    pub loopback: bool,
    pub multicast: bool,
    pub addresses: Vec<Address>,
}

/// An interface address with its prefix length, like Go's `*net.IPNet`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    pub ip: IpAddr,
    pub prefix: u8,
}

impl Address {
    /// `IPNet.Contains`.
    pub fn contains(&self, other: IpAddr) -> bool {
        match (self.ip, other) {
            (IpAddr::V4(network), IpAddr::V4(other)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(network) & mask == u32::from(other) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(other)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(network) & mask == u128::from(other) & mask
            }
            _ => false,
        }
    }
}

/// Every interface in index order.
pub fn list() -> Vec<Interface> {
    let Ok(entries) = getifaddrs() else {
        return Vec::new();
    };
    let mut interfaces: BTreeMap<u32, Interface> = BTreeMap::new();
    for entry in entries {
        let Ok(index) = nix::net::if_::if_nametoindex(entry.interface_name.as_str()) else {
            continue;
        };
        let interface = interfaces.entry(index).or_insert_with(|| Interface {
            name: entry.interface_name.clone(),
            index,
            up: entry.flags.contains(InterfaceFlags::IFF_UP),
            loopback: entry.flags.contains(InterfaceFlags::IFF_LOOPBACK),
            multicast: entry.flags.contains(InterfaceFlags::IFF_MULTICAST),
            addresses: Vec::new(),
        });
        let ip = entry.address.as_ref().and_then(|address| {
            address
                .as_sockaddr_in()
                .map(|v4| IpAddr::V4(v4.ip()))
                .or_else(|| address.as_sockaddr_in6().map(|v6| IpAddr::V6(v6.ip())))
        });
        let Some(ip) = ip else { continue };
        let prefix = entry
            .netmask
            .as_ref()
            .and_then(|mask| {
                mask.as_sockaddr_in()
                    .map(|v4| u32::from(v4.ip()).count_ones())
                    .or_else(|| {
                        mask.as_sockaddr_in6()
                            .map(|v6| u128::from(v6.ip()).count_ones())
                    })
            })
            .unwrap_or(if ip.is_ipv4() { 32 } else { 128 });
        interface.addresses.push(Address {
            ip,
            prefix: u8::try_from(prefix).unwrap_or(128),
        });
    }
    interfaces.into_values().collect()
}

/// `net.IP.IsLinkLocalUnicast`.
pub fn link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_contain_their_subnet_only() {
        let network = Address {
            ip: "172.31.252.4".parse().unwrap(),
            prefix: 24,
        };
        assert!(network.contains("172.31.252.30".parse().unwrap()));
        assert!(!network.contains("172.31.250.30".parse().unwrap()));
        assert!(!network.contains("::1".parse().unwrap()));
    }

    #[test]
    fn listing_includes_the_loopback_interface() {
        assert!(list().iter().any(|interface| interface.loopback));
    }
}
