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
use crate::net::virtual_ip::gateway_ip;

/// Configuration for the overlay network.
#[derive(Debug, Clone)]
pub struct OverlayConfig {
    /// Address the embedded DNS server listens on AND that the scoped OS resolver
    /// is pointed at. This must be the TUN **gateway** address on port **53**
    /// (e.g. `10.254.0.1:53`). systemd-resolved sends per-link DNS queries via the
    /// TUN link (`deven0`), where loopback (`127.0.0.1`) is unreachable but the
    /// gateway — the TUN's own address — is reachable; and resolve1's `SetLinkDNS`
    /// carries no port, so resolved always queries port 53 (hence the server must
    /// listen on 53, not 5300). The macOS (`/etc/resolver`) and dnsmasq fallback
    /// paths carry the port too and reach the gateway:53 just the same.
    pub dns_listen: SocketAddr,
    /// TUN device configuration.
    pub tun: TunConfig,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            // The embedded DNS server lives on the overlay gateway at :53 so that
            // systemd-resolved (per-link, port-less SetLinkDNS) can reach it via
            // the TUN link. The daemon already runs as root to create the TUN, so
            // binding :53 on this dedicated address is fine (it does not collide
            // with systemd-resolved's own 127.0.0.53:53 stub).
            dns_listen: SocketAddr::new(IpAddr::V4(gateway_ip()), 53),
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