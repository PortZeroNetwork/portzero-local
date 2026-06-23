//! Virtual IP allocator for the overlay network.
//!
//! We use a non-routable documentation/example block:
//!   10.254.0.0/16
//!
//! Each full `*.devenv.local` name from DEVENV_TUNNEL gets a stable IP
//! for the lifetime of the daemon.

use std::collections::HashMap;
use std::net::Ipv4Addr;

/// The virtual network base (10.254.0.0/16).
const VNET_PREFIX: [u8; 2] = [10, 254];
const VNET_GATEWAY_OCTET3: u8 = 0;
const VNET_GATEWAY_OCTET4: u8 = 1; // 10.254.0.1 reserved for future gateway

/// Allocator for virtual IPs inside 10.254.0.0/16.
#[derive(Debug, Clone)]
pub struct VirtualIpAllocator {
    next: u16,
    name_to_ip: HashMap<String, Ipv4Addr>,
    ip_to_name: HashMap<Ipv4Addr, String>,
}

impl Default for VirtualIpAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualIpAllocator {
    pub fn new() -> Self {
        Self {
            next: 2, // start at 10.254.0.2
            name_to_ip: HashMap::new(),
            ip_to_name: HashMap::new(),
        }
    }

    /// Get or allocate a virtual IP for a given network name.
    /// The same name always yields the same IP while the allocator lives.
    pub fn assign(&mut self, name: &str) -> Ipv4Addr {
        if let Some(&ip) = self.name_to_ip.get(name) {
            return ip;
        }

        // Find a free slot (very simple linear scan; 64k is tiny).
        let mut candidate = self.next;
        loop {
            let ip = Ipv4Addr::new(VNET_PREFIX[0], VNET_PREFIX[1], (candidate >> 8) as u8, (candidate & 0xff) as u8);

            if self.is_reserved(ip) || self.ip_to_name.contains_key(&ip) {
                candidate = candidate.wrapping_add(1);
                if candidate == 0 {
                    candidate = 2;
                }
                continue;
            }

            self.name_to_ip.insert(name.to_string(), ip);
            self.ip_to_name.insert(ip, name.to_string());
            self.next = candidate.wrapping_add(1);
            if self.next == 0 {
                self.next = 2;
            }
            return ip;
        }
    }

    /// Release a name (and its IP) if present.
    pub fn release(&mut self, name: &str) {
        if let Some(ip) = self.name_to_ip.remove(name) {
            self.ip_to_name.remove(&ip);
        }
    }

    /// Lookup by name.
    pub fn lookup_name(&self, name: &str) -> Option<Ipv4Addr> {
        self.name_to_ip.get(name).copied()
    }

    /// Reverse lookup by IP.
    pub fn lookup_ip(&self, ip: Ipv4Addr) -> Option<&str> {
        self.ip_to_name.get(&ip).map(|s| s.as_str())
    }

    fn is_reserved(&self, ip: Ipv4Addr) -> bool {
        let o = ip.octets();
        // 10.254.0.0 and 10.254.0.1 and broadcast-ish 10.254.255.255
        (o[2] == 0 && o[3] <= 1) || (o[2] == 255 && o[3] == 255)
    }
}

/// Return the conventional gateway address for the virtual net (if ever needed).
pub fn gateway_ip() -> Ipv4Addr {
    Ipv4Addr::new(VNET_PREFIX[0], VNET_PREFIX[1], VNET_GATEWAY_OCTET3, VNET_GATEWAY_OCTET4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_stable_ip() {
        let mut alloc = VirtualIpAllocator::new();
        let a = alloc.assign("my-db");
        let b = alloc.assign("my-db");
        assert_eq!(a, b);
        assert!(a.octets()[0] == 10 && a.octets()[1] == 254);
    }

    #[test]
    fn different_names_get_different_ips() {
        let mut alloc = VirtualIpAllocator::new();
        let a = alloc.assign("db");
        let b = alloc.assign("api");
        assert_ne!(a, b);
    }

    #[test]
    fn release_frees_ip() {
        let mut alloc = VirtualIpAllocator::new();
        let _ip = alloc.assign("temp");
        alloc.release("temp");
        let _ip2 = alloc.assign("other");
        // We don't guarantee immediate reuse, but lookup must be gone
        assert!(alloc.lookup_name("temp").is_none());
    }
}