//! High-level overlay network manager.
//!
//! Starts the TUN device, the smoltcp TCP stack, and the embedded DNS server.
//! It receives service updates (from discovery) and keeps the virtual network
//! in sync with services that set a full `*.devenv.local` name via DEVENV_TUNNEL.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::net::dns::OverlayDnsServer;
use crate::net::service_table::ServiceTable;
use crate::net::stack::VirtualStack;
use crate::net::tun_device::{TunConfig, TunDevice};

/// Configuration for the overlay network.
#[derive(Debug, Clone)]
pub struct OverlayConfig {
    /// Address the embedded DNS server should listen on.
    /// Usually 127.0.0.53 or 127.0.0.1:53 (the latter requires privileges).
    pub dns_listen: SocketAddr,
    /// TUN device configuration.
    pub tun: TunConfig,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            // Common safe local address for a scoped resolver.
            // On macOS/Linux we will point /etc/resolver or systemd-resolved at this.
            dns_listen: "127.0.0.1:5300".parse().unwrap(),
            tun: TunConfig::default(),
        }
    }
}

/// The running overlay network.
pub struct OverlayNetwork {
    services: Arc<RwLock<ServiceTable>>,
    stack: VirtualStack,
    dns_task: tokio::task::JoinHandle<()>,
}

impl OverlayNetwork {
    /// Start the overlay (TUN + TCP stack + DNS).
    ///
    /// This must be called with sufficient privileges.
    pub async fn start(config: OverlayConfig) -> Result<Self> {
        // Create the TUN device first (this may require root).
        let tun = TunDevice::create(&config.tun)?;

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

        Ok(Self {
            services,
            stack,
            dns_task,
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
        let _ = self.stack.shutdown().await;
        self.dns_task.abort();
    }
}