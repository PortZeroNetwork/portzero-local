//! TUN device handling (cross-platform).
//!
//! Uses the `tun` crate. Provides a simple async interface returning L3 packets.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tun::{create_as_async, AsyncDevice, Configuration, Device};

/// Configuration for the virtual TUN interface.
#[derive(Debug, Clone)]
pub struct TunConfig {
    /// Suggested device name (platform dependent).
    /// macOS: utunX (kernel chooses if "utun" or empty)
    /// Linux: "tun0", "deven0" etc.
    /// Windows: adapter name (wintun).
    pub name: Option<String>,
    /// IPv4 address to assign to the interface (our side).
    /// This is typically the gateway 10.254.0.1 .
    pub address: std::net::Ipv4Addr,
    /// Netmask.
    pub netmask: std::net::Ipv4Addr,
    /// MTU. 1500 is safe.
    pub mtu: u32,
}

impl Default for TunConfig {
    fn default() -> Self {
        Self {
            name: Some("utun5".to_string()), // common on mac dev setups; ignored or adjusted on other OS
            address: crate::net::virtual_ip::gateway_ip(),
            netmask: std::net::Ipv4Addr::new(255, 255, 0, 0),
            mtu: 1500,
        }
    }
}

/// A handle to an async TUN device.
pub struct TunDevice {
    dev: AsyncDevice,
    name: String,
}

impl TunDevice {
    /// Create and configure the TUN device.
    ///
    /// On macOS this will typically create a utun interface.
    /// Requires root / appropriate entitlements on macOS and Windows.
    pub fn create(config: &TunConfig) -> Result<Self> {
        let mut tuncfg = Configuration::default();

        if let Some(ref n) = config.name {
            // The tun crate accepts the name; on macOS you usually request "utun" or leave it.
            tuncfg.name(n);
        }

        tuncfg.address(config.address);
        tuncfg.netmask(config.netmask);
        tuncfg.mtu(config.mtu.try_into().unwrap_or(1500));
        tuncfg.up();

        // Layer 3 (we want raw IP packets, not ethernet frames).
        tuncfg.layer(tun::Layer::L3);

        let dev = create_as_async(&tuncfg).context("failed to create TUN device (are you root?)")?;

        // Best effort: ask the device what its actual name is.
        let actual_name = dev.get_ref()
            .name()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| {
                config.name.clone().unwrap_or_else(|| "tun0".to_string())
            });

        tracing::info!(
            "TUN device created: name={} address={}/16 mtu={}",
            actual_name,
            config.address,
            config.mtu
        );

        Ok(Self {
            dev,
            name: actual_name,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Read one L3 packet from the TUN.
    /// Returns the number of bytes read.
    pub async fn read_packet(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.dev.read(buf).await
    }

    /// Write one L3 packet to the TUN.
    pub async fn write_packet(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.dev.write_all(buf).await
    }

    /// Split into read and write halves for concurrent use.
    pub fn split(self) -> (TunReader, TunWriter) {
        let (r, w) = tokio::io::split(self.dev);
        (
            TunReader {
                inner: Arc::new(tokio::sync::Mutex::new(r)),
                _name: self.name.clone(),
            },
            TunWriter {
                inner: Arc::new(tokio::sync::Mutex::new(w)),
            },
        )
    }
}

pub struct TunReader {
    inner: Arc<tokio::sync::Mutex<tokio::io::ReadHalf<AsyncDevice>>>,
    _name: String,
}

impl TunReader {
    pub async fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut guard = self.inner.lock().await;
        guard.read(buf).await
    }
}

pub struct TunWriter {
    inner: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<AsyncDevice>>>,
}

impl TunWriter {
    pub async fn write(&self, buf: &[u8]) -> std::io::Result<()> {
        let mut guard = self.inner.lock().await;
        guard.write_all(buf).await
    }
}

// Convenience: platform specific notes for privileged setup.
#[cfg(target_os = "macos")]
pub fn platform_setup_hints() {
    tracing::info!("macOS: ensure the binary runs as root or has the com.apple.developer.networking.networkextension entitlement for System Extension utun.");
}

#[cfg(target_os = "linux")]
pub fn platform_setup_hints() {
    tracing::info!("Linux: the daemon must have CAP_NET_ADMIN or run as root to create /dev/net/tun devices.");
}

#[cfg(target_os = "windows")]
pub fn platform_setup_hints() {
    tracing::info!("Windows: wintun.dll must be present next to the binary or in PATH.");
}