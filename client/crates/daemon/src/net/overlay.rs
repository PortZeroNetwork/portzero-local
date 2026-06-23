//! High-level overlay network manager.
//!
//! Starts the TUN device, the smoltcp TCP stack, and the embedded DNS server.
//! It receives service updates (from discovery) and keeps the virtual network
//! in sync with services that set a full `*.devenv.local` name via DEVENV_TUNNEL.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::net::dns::OverlayDnsServer;
use crate::net::resolver_config;
use crate::net::service_table::ServiceTable;
use crate::net::stack::VirtualStack;
use crate::net::tun_device::{TunConfig, TunDevice};
#[cfg(not(target_os = "macos"))]
use crate::net::virtual_ip::gateway_ip;

/// macOS loopback port for the embedded DNS server. A high, non-privileged
/// port that won't collide with anything; change here if it ever conflicts.
/// (Must NOT be 53 — the macOS `/etc/resolver` file carries the port, so any
/// port works as long as it matches what the server binds.)
#[cfg(target_os = "macos")]
const MACOS_DNS_LOOPBACK_PORT: u16 = 10053;

/// The default address the embedded DNS server listens on AND that the scoped
/// OS resolver is pointed at.
///
/// **Linux / others:** the TUN **gateway** address on port **53**
/// (e.g. `10.254.0.1:53`). systemd-resolved sends per-link DNS queries via the
/// TUN link (`deven0`), where loopback (`127.0.0.1`) is unreachable but the
/// gateway — the TUN's own address — is reachable; and resolve1's `SetLinkDNS`
/// carries no port, so resolved always queries port 53 (hence the server must
/// listen on 53, not 5300). The dnsmasq fallback path carries the port too and
/// reaches the gateway:53 just the same.
///
/// **macOS exception:** the utun is point-to-point and has NO local route for
/// the gateway IP, so a packet to `10.254.0.1:53` routes OUT the tunnel instead
/// of reaching the local `UdpSocket` (queries time out — see task-33). macOS
/// therefore binds to loopback (`127.0.0.1:<MACOS_DNS_LOOPBACK_PORT>`), which is
/// always locally deliverable; the `/etc/resolver/devenv.local` file carries the
/// matching `port` line so the system resolver finds it.
fn default_dns_listen() -> SocketAddr {
    #[cfg(target_os = "macos")]
    {
        SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), MACOS_DNS_LOOPBACK_PORT)
    }
    #[cfg(not(target_os = "macos"))]
    {
        SocketAddr::new(IpAddr::V4(gateway_ip()), 53)
    }
}

/// Configuration for the overlay network.
#[derive(Debug, Clone)]
pub struct OverlayConfig {
    /// Address the embedded DNS server listens on AND that the scoped OS resolver
    /// is pointed at. See [`default_dns_listen`] for the per-platform default and
    /// the macOS loopback exception.
    pub dns_listen: SocketAddr,
    /// TUN device configuration.
    pub tun: TunConfig,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            dns_listen: default_dns_listen(),
            tun: TunConfig::default(),
        }
    }
}

/// The running overlay network.
pub struct OverlayNetwork {
    services: Arc<RwLock<ServiceTable>>,
    stack: VirtualStack,
    dns_task: tokio::task::JoinHandle<()>,
    /// Name of the overlay's TUN link (e.g. `deven0`). The scoped resolver is
    /// attached to this real link, so teardown must revert the same one.
    link_name: String,
}

impl OverlayNetwork {
    /// Start the overlay (TUN + TCP stack + DNS).
    ///
    /// This must be called with sufficient privileges.
    pub async fn start(config: OverlayConfig) -> Result<Self> {
        // Create the TUN device first (this may require root).
        let tun = TunDevice::create(&config.tun)?;
        // Capture the actual link name (the kernel may pick a different unit
        // number than requested) before the device is moved into the stack. The
        // scoped resolver is attached to THIS real link, not `lo`.
        let link_name = tun.name().to_string();

        // Shared service table
        let services: Arc<RwLock<ServiceTable>> = Arc::new(RwLock::new(ServiceTable::new()));

        // Start the TCP stack
        let initial = ServiceTable::new();
        let stack = VirtualStack::spawn(tun, initial).await?;

        // Start DNS server
        let dns_server = OverlayDnsServer::new(services.clone(), config.dns_listen);
        let dns_task = tokio::spawn(async move {
            if let Err(e) = dns_server.run().await {
                tracing::error!("overlay DNS server exited: {}", e);
            }
        });

        // Install scoped OS resolver — routes *.devenv.local to our DNS server.
        // Attached to the TUN link (created above), so on Linux this works even
        // when systemd-networkd is absent. Log errors but do not abort startup;
        // the overlay still works for services that manually configure DNS.
        if let Err(e) = resolver_config::install(config.dns_listen, &link_name).await {
            tracing::warn!("scoped resolver setup failed (may need elevated privileges): {:#}", e);
        }

        Ok(Self {
            services,
            stack,
            dns_task,
            link_name,
        })
    }

    /// Push an updated service table into the overlay (called by discovery).
    pub async fn update_services(&self, table: ServiceTable) -> Result<()> {
        // Update DNS view
        {
            let mut guard = self.services.write().await;
            *guard = table.clone();
        }

        // Tell the TCP stack
        self.stack.update_services(table).await?;
        Ok(())
    }

    /// Shutdown the overlay components.
    pub async fn shutdown(self) {
        // Remove the scoped OS resolver before tearing down DNS.
        if let Err(e) = resolver_config::uninstall(&self.link_name).await {
            tracing::warn!("scoped resolver teardown failed: {:#}", e);
        }

        let _ = self.stack.shutdown().await;
        self.dns_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On macOS the default DNS server must bind to loopback (not the overlay
    /// gateway), because the point-to-point utun has no local route for the
    /// gateway IP — so a query to `10.254.0.1:53` routes into the tunnel and the
    /// real `UdpSocket` never sees it (task-33).
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_dns_listen_is_loopback() {
        let addr = default_dns_listen();
        assert_eq!(addr.ip(), IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        assert_ne!(
            addr.ip(),
            IpAddr::V4(gateway_ip_for_test()),
            "must not bind the DNS server to the unreachable overlay gateway"
        );
        assert_eq!(addr.port(), MACOS_DNS_LOOPBACK_PORT);
        assert_ne!(addr.port(), 53, "must not use privileged port 53 on macOS");
    }

    /// The scoped `/etc/resolver/devenv.local` file written from the macOS
    /// default must point the system resolver at that same loopback address,
    /// including the matching `port` line.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_resolver_file_points_at_loopback_default() {
        let addr = default_dns_listen();
        let content = crate::net::resolver_config::macos_resolver_file_content(addr);
        assert!(
            content.contains("nameserver 127.0.0.1\n"),
            "resolver file must point at loopback: {content}"
        );
        assert!(
            content.contains(&format!("port {MACOS_DNS_LOOPBACK_PORT}\n")),
            "resolver file must carry the loopback port: {content}"
        );
    }

    /// On Linux (and other non-macOS targets) the default is unchanged: the
    /// overlay gateway on port 53, which the kernel makes locally reachable via
    /// the interface route.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_default_dns_listen_is_gateway_53() {
        let addr = default_dns_listen();
        assert_eq!(addr, SocketAddr::new(IpAddr::V4(gateway_ip()), 53));
    }

    /// The gateway IP, for asserting the macOS default does NOT use it.
    #[cfg(target_os = "macos")]
    fn gateway_ip_for_test() -> std::net::Ipv4Addr {
        crate::net::virtual_ip::gateway_ip()
    }
}