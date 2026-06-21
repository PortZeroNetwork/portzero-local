//! User-space TCP stack (smoltcp over TUN).
//!
//! NOTE: The low-level packet loop is under active integration with smoltcp 0.11 + tun 0.6.
//! The public API (VirtualStack) is stable and the rest of the system can already
//! feed it ServiceTable updates. The actual packet pump will be completed in a follow-up
//! patch that nails the exact Device / Socket API for the chosen smoltcp version.

use anyhow::Result;
use tokio::sync::mpsc;

use crate::net::service_table::ServiceTable;

pub enum StackCommand {
    UpdateServices(ServiceTable),
    Shutdown,
}

pub struct VirtualStack {
    cmd_tx: mpsc::Sender<StackCommand>,
}

impl VirtualStack {
    pub async fn spawn(_tun: crate::net::tun_device::TunDevice, _initial: ServiceTable) -> Result<Self> {
        let (tx, mut rx) = mpsc::channel::<StackCommand>(8);
        // Stub task that just drains commands (real implementation will own the tun + smoltcp iface here).
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if matches!(cmd, StackCommand::Shutdown) {
                    break;
                }
            }
        });
        Ok(Self { cmd_tx: tx })
    }

    pub async fn update_services(&self, table: ServiceTable) -> Result<()> {
        // In the real impl we would forward to the inner loop.
        let _ = self.cmd_tx.send(StackCommand::UpdateServices(table)).await;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        let _ = self.cmd_tx.send(StackCommand::Shutdown).await;
        Ok(())
    }
}