//! TUN device handling (cross-platform).
//!
//! Uses the `tun` crate. Provides a simple async interface returning L3 packets.
//!
//! Responsibilities:
//!  - create and configure a TUN interface on macOS / Linux / Windows,
//!    assigning the gateway IPv4 address and netmask for the virtual subnet,
//!  - ensure the OS routes the whole virtual subnet (`10.254.0.0/16`) into the
//!    interface,
//!  - tear down any route the daemon explicitly added when the device is
//!    dropped.
//!
//! Creating a TUN device and modifying the routing table both require
//! root / `CAP_NET_ADMIN` (or the appropriate entitlement on macOS / Windows).
//! Route management is therefore *best-effort and non-fatal*: a missing route is
//! logged but does not abort startup, mirroring how the rest of the overlay
//! treats privileged operations.

use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tun::{create_as_async, AsyncDevice, Configuration, Device};

/// Prefix length of the virtual subnet (10.254.0.0/16).
const VNET_PREFIX_LEN: u8 = 16;

/// Configuration for the virtual TUN interface.
#[derive(Debug, Clone)]
pub struct TunConfig {
    /// Suggested device name (platform dependent). `None` lets each platform
    /// pick a sensible default (see [`default_device_name`]).
    ///
    /// macOS: `utun` lets the kernel choose the next free `utunN`.
    /// Linux: `deven0` (or any `tunN`-style name).
    /// Windows: the wintun adapter name.
    pub name: Option<String>,
    /// IPv4 address to assign to the interface (our side).
    /// This is typically the gateway 10.254.0.1 .
    pub address: Ipv4Addr,
    /// Netmask.
    pub netmask: Ipv4Addr,
    /// MTU. 1500 is safe.
    pub mtu: u32,
}

impl Default for TunConfig {
    fn default() -> Self {
        Self {
            // `None` => let the platform choose (kernel-assigned utunN on macOS,
            // `deven0` on Linux/Windows). Do not hardcode a specific unit number
            // since it may already be in use.
            name: None,
            address: crate::net::virtual_ip::gateway_ip(),
            netmask: Ipv4Addr::new(255, 255, 0, 0),
            mtu: 1500,
        }
    }
}

/// Pick a sensible default interface name for the current platform when the
/// caller did not request one.
///
/// On macOS the kernel only accepts `utun`-prefixed names and assigns the unit
/// number itself, so we request the bare prefix `"utun"`. On Linux and Windows
/// we use a stable, descriptive name.
fn default_device_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "utun"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "deven0"
    }
}

/// The virtual subnet in CIDR form (e.g. `10.254.0.0/16`), derived from the
/// gateway address and the subnet prefix length. Pure — used for route commands.
fn subnet_cidr(gateway: Ipv4Addr, prefix_len: u8) -> String {
    let o = gateway.octets();
    // Zero out the host bits below the prefix. For /16 this yields the
    // network address 10.254.0.0.
    let mask: u32 = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len as u32)
    };
    let net = u32::from_be_bytes(o) & mask;
    let net = Ipv4Addr::from(net.to_be_bytes());
    format!("{net}/{prefix_len}")
}

/// Generate the OS command + arguments to add a route for `cidr` via the named
/// interface. Returns `None` on platforms where the kernel installs the
/// connected route automatically and no manual command is needed.
///
/// Pure and side-effect free so it can be unit-tested unprivileged.
fn route_add_command(cidr: &str, iface: &str) -> Option<(&'static str, Vec<String>)> {
    #[cfg(target_os = "linux")]
    {
        Some((
            "ip",
            vec![
                "route".into(),
                "add".into(),
                cidr.into(),
                "dev".into(),
                iface.into(),
            ],
        ))
    }
    #[cfg(target_os = "macos")]
    {
        // `route -n add -net <cidr> -interface <iface>`
        Some((
            "route",
            vec![
                "-n".into(),
                "add".into(),
                "-net".into(),
                cidr.into(),
                "-interface".into(),
                iface.into(),
            ],
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // On Windows the wintun adapter's assigned address installs the
        // connected route; no explicit command issued here.
        let _ = (cidr, iface);
        None
    }
}

/// Generate the OS command + arguments to delete the route previously added by
/// [`route_add_command`]. Mirrors it platform-for-platform.
///
/// Pure and side-effect free so it can be unit-tested unprivileged.
fn route_del_command(cidr: &str, iface: &str) -> Option<(&'static str, Vec<String>)> {
    #[cfg(target_os = "linux")]
    {
        Some((
            "ip",
            vec![
                "route".into(),
                "del".into(),
                cidr.into(),
                "dev".into(),
                iface.into(),
            ],
        ))
    }
    #[cfg(target_os = "macos")]
    {
        Some((
            "route",
            vec![
                "-n".into(),
                "delete".into(),
                "-net".into(),
                cidr.into(),
                "-interface".into(),
                iface.into(),
            ],
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (cidr, iface);
        None
    }
}

/// Best-effort: run a route command, logging the outcome. Never returns an
/// error — route management is non-fatal.
fn run_route_command(action: &str, program: &str, args: &[String]) -> bool {
    match std::process::Command::new(program).args(args).output() {
        Ok(out) if out.status.success() => {
            tracing::info!("route {action} succeeded: {program} {}", args.join(" "));
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!(
                "route {action} failed ({program} {}): {}",
                args.join(" "),
                stderr.trim()
            );
            false
        }
        Err(e) => {
            tracing::warn!(
                "route {action} could not run ({program} {}): {e}",
                args.join(" ")
            );
            false
        }
    }
}

/// A handle to an async TUN device.
pub struct TunDevice {
    /// `Option` so the device can be moved out in [`TunDevice::split`] despite
    /// the `Drop` impl. Always `Some` until split/drop.
    dev: Option<AsyncDevice>,
    name: String,
    /// The subnet CIDR routed into this interface, set when the daemon explicitly
    /// added an OS route that it is responsible for removing on teardown.
    added_route: Option<String>,
}

impl TunDevice {
    /// Create and configure the TUN device, assign its IPv4 address/netmask, and
    /// ensure the virtual subnet is routed into it.
    ///
    /// On macOS this will typically create a `utunN` interface (kernel-chosen).
    /// Requires root / appropriate entitlements on macOS and Windows, and
    /// `CAP_NET_ADMIN`/root on Linux.
    pub fn create(config: &TunConfig) -> Result<Self> {
        let mut tuncfg = Configuration::default();

        let requested_name = config
            .name
            .clone()
            .unwrap_or_else(|| default_device_name().to_string());
        tuncfg.name(&requested_name);

        tuncfg.address(config.address);
        tuncfg.netmask(config.netmask);
        tuncfg.mtu(config.mtu.try_into().unwrap_or(1500));
        tuncfg.up();

        // Layer 3 (we want raw IP packets, not ethernet frames).
        tuncfg.layer(tun::Layer::L3);

        let dev =
            create_as_async(&tuncfg).context("failed to create TUN device (are you root?)")?;

        // Ask the device what its actual name is (the kernel may have chosen a
        // different unit number, e.g. utun7 instead of the requested utun).
        let actual_name = dev
            .get_ref()
            .name()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| requested_name.clone());

        tracing::info!(
            "TUN device created: name={} address={}/{} mtu={}",
            actual_name,
            config.address,
            VNET_PREFIX_LEN,
            config.mtu
        );

        // Ensure the whole virtual subnet routes into this interface. On most
        // platforms assigning the /16 address already installs a connected
        // route; we additionally issue an explicit route command where the
        // platform needs it (Linux/macOS) so the subnet is reliably reachable.
        // This is best-effort: failures are logged, not fatal.
        let cidr = subnet_cidr(config.address, VNET_PREFIX_LEN);
        let mut added_route = None;
        if let Some((program, args)) = route_add_command(&cidr, &actual_name) {
            if run_route_command("add", program, &args) {
                // Only record routes we successfully installed, so teardown only
                // removes what we own. (A pre-existing connected route is left
                // untouched.)
                added_route = Some(cidr.clone());
            }
        }

        Ok(Self {
            dev: Some(dev),
            name: actual_name,
            added_route,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Read one L3 packet from the TUN.
    /// Returns the number of bytes read.
    pub async fn read_packet(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.dev
            .as_mut()
            .expect("TUN device used after split")
            .read(buf)
            .await
    }

    /// Write one L3 packet to the TUN.
    pub async fn write_packet(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.dev
            .as_mut()
            .expect("TUN device used after split")
            .write_all(buf)
            .await
    }

    /// Split into read and write halves for concurrent use.
    ///
    /// This consumes the [`TunDevice`]. Ownership of the explicitly-added route
    /// is transferred into the [`TunWriter`] half, which removes it on drop so
    /// teardown still happens after a split.
    pub fn split(mut self) -> (TunReader, TunWriter) {
        // Move the route bookkeeping out before the device is consumed so the
        // `Drop` impl on `self` does not run teardown prematurely.
        let added_route = self.added_route.take();
        let name = self.name.clone();
        let dev = self
            .dev
            .take()
            .expect("TUN device already split or dropped");
        let (r, w) = tokio::io::split(dev);
        (
            TunReader {
                inner: Arc::new(tokio::sync::Mutex::new(r)),
                _name: name.clone(),
            },
            TunWriter {
                inner: Arc::new(tokio::sync::Mutex::new(w)),
                route_guard: RouteGuard { name, added_route },
            },
        )
    }
}

impl Drop for TunDevice {
    fn drop(&mut self) {
        remove_added_route(&self.name, self.added_route.take());
    }
}

/// Owns the responsibility of removing the explicitly-added OS route. Held by
/// the [`TunWriter`] after a [`TunDevice::split`] so teardown happens exactly
/// once (the writer outlives the route just like the device did).
struct RouteGuard {
    name: String,
    added_route: Option<String>,
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        remove_added_route(&self.name, self.added_route.take());
    }
}

/// Best-effort removal of a route the daemon previously installed. Routes the
/// kernel auto-installed for the connected /16 vanish with the interface, so we
/// only ever remove what we explicitly added.
fn remove_added_route(iface: &str, added_route: Option<String>) {
    let Some(cidr) = added_route else {
        return;
    };
    if let Some((program, args)) = route_del_command(&cidr, iface) {
        run_route_command("del", program, &args);
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
    /// Removes the daemon-installed route when this writer (the last surviving
    /// handle to the device after a split) is dropped.
    #[allow(dead_code)]
    route_guard: RouteGuard,
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
    tracing::info!("Linux: the daemon must have CAP_NET_ADMIN or run as root to create /dev/net/tun devices and modify routes.");
}

#[cfg(target_os = "windows")]
pub fn platform_setup_hints() {
    tracing::info!("Windows: wintun.dll must be present next to the binary or in PATH.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnet_cidr_zeroes_host_bits() {
        // Gateway is 10.254.0.1; the /16 network is 10.254.0.0.
        let gw = Ipv4Addr::new(10, 254, 0, 1);
        assert_eq!(subnet_cidr(gw, 16), "10.254.0.0/16");
    }

    #[test]
    fn subnet_cidr_matches_virtual_gateway() {
        let gw = crate::net::virtual_ip::gateway_ip();
        assert_eq!(subnet_cidr(gw, VNET_PREFIX_LEN), "10.254.0.0/16");
    }

    #[test]
    fn subnet_cidr_other_prefixes() {
        let gw = Ipv4Addr::new(192, 168, 5, 37);
        assert_eq!(subnet_cidr(gw, 24), "192.168.5.0/24");
        assert_eq!(subnet_cidr(gw, 8), "192.0.0.0/8");
        assert_eq!(subnet_cidr(gw, 32), "192.168.5.37/32");
    }

    #[test]
    fn default_device_name_is_platform_appropriate() {
        let name = default_device_name();
        #[cfg(target_os = "macos")]
        assert_eq!(name, "utun");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(name, "deven0");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn route_add_command_linux() {
        let (prog, args) = route_add_command("10.254.0.0/16", "deven0").unwrap();
        assert_eq!(prog, "ip");
        assert_eq!(args, vec!["route", "add", "10.254.0.0/16", "dev", "deven0"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn route_del_command_linux() {
        let (prog, args) = route_del_command("10.254.0.0/16", "deven0").unwrap();
        assert_eq!(prog, "ip");
        assert_eq!(args, vec!["route", "del", "10.254.0.0/16", "dev", "deven0"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn route_add_command_macos() {
        let (prog, args) = route_add_command("10.254.0.0/16", "utun7").unwrap();
        assert_eq!(prog, "route");
        assert_eq!(
            args,
            vec!["-n", "add", "-net", "10.254.0.0/16", "-interface", "utun7"]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn route_del_command_macos() {
        let (prog, args) = route_del_command("10.254.0.0/16", "utun7").unwrap();
        assert_eq!(prog, "route");
        assert_eq!(
            args,
            vec![
                "-n",
                "delete",
                "-net",
                "10.254.0.0/16",
                "-interface",
                "utun7"
            ]
        );
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn route_commands_none_on_other_platforms() {
        assert!(route_add_command("10.254.0.0/16", "deven0").is_none());
        assert!(route_del_command("10.254.0.0/16", "deven0").is_none());
    }
}
