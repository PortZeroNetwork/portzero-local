//! Service table: maps DEVENV_NETWORK names to their backing endpoints and virtual IPs.
//!
//! A "service" here is something like:
//!   name = "my-db"
//!   real_addr = "127.0.0.1:32768"   (the discovered ephemeral port)
//!   service_port = 5432              (what clients connect to on the VIP)
//!   vip = 10.254.0.7

use std::collections::HashMap;
use std::net::SocketAddr;

use crate::net::virtual_ip::VirtualIpAllocator;
use smoltcp::wire::Ipv4Address;

/// A discovered network service reachable via the overlay.
#[derive(Debug, Clone)]
pub struct NetworkService {
    /// The DEVENV_NETWORK value (e.g. "my-db").
    pub name: String,
    /// Virtual IP assigned to this name.
    pub vip: Ipv4Address,
    /// The port clients are expected to connect to (e.g. 5432).
    /// This is usually the "well-known" or container port, not the ephemeral host port.
    pub service_port: u16,
    /// The actual address on the host that we proxy to.
    pub real_addr: SocketAddr,
    /// Owning PID (0 if unknown / container).
    pub pid: u32,
}

#[derive(Debug, Default, Clone)]
pub struct ServiceTable {
    allocator: VirtualIpAllocator,
    by_name: HashMap<String, NetworkService>,
    /// vip -> name for fast reverse lookup in the packet path
    vip_to_name: HashMap<Ipv4Address, String>,
}

impl ServiceTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or update a service.
    ///
    /// If the name already exists, we keep the same VIP but update the real_addr
    /// and service_port (handles process restarts or new port 0 assignment).
    pub fn register(&mut self, name: String, real_addr: SocketAddr, service_port: u16, pid: u32) -> NetworkService {
        let vip = if let Some(existing) = self.by_name.get(&name) {
            existing.vip
        } else {
            let std_ip = self.allocator.assign(&name);
            Ipv4Address::from_bytes(&std_ip.octets())
        };

        let svc = NetworkService {
            name: name.clone(),
            vip,
            service_port,
            real_addr,
            pid,
        };

        // Remove old reverse mapping if VIP changed (shouldn't for same name)
        if let Some(old) = self.by_name.get(&name) {
            if old.vip != vip {
                self.vip_to_name.remove(&old.vip);
            }
        }

        self.by_name.insert(name.clone(), svc.clone());
        self.vip_to_name.insert(vip, name);
        svc
    }

    /// Remove a service by name.
    pub fn unregister(&mut self, name: &str) -> Option<NetworkService> {
        if let Some(svc) = self.by_name.remove(name) {
            self.vip_to_name.remove(&svc.vip);
            // We keep the IP reserved in the allocator for stability across quick restarts.
            // Call self.allocator.release(name) only on explicit long-term removal if desired.
            Some(svc)
        } else {
            None
        }
    }

    /// Get by name.
    pub fn get(&self, name: &str) -> Option<&NetworkService> {
        self.by_name.get(name)
    }

    /// Get by virtual IP (used by the TCP stack to decide where to proxy).
    pub fn get_by_vip(&self, vip: Ipv4Address) -> Option<&NetworkService> {
        self.vip_to_name.get(&vip).and_then(|n| self.by_name.get(n))
    }

    /// Get by (vip, dst_port). We primarily key on VIP; the dst_port is validated
    /// against service_port for defense-in-depth.
    pub fn resolve_for_connect(&self, vip: Ipv4Address, dst_port: u16) -> Option<&NetworkService> {
        self.get_by_vip(vip).filter(|s| s.service_port == dst_port)
    }

    /// Iterate all services.
    pub fn all(&self) -> impl Iterator<Item = &NetworkService> {
        self.by_name.values()
    }

    /// Number of registered services.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}