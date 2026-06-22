//! Discovery daemon loop: periodically scan for services and update routes.
//!
//! The daemon runs as a background process, scanning every few seconds for
//! processes and Docker containers with DEVENV_TUNNEL set. Changes are
//! persisted to `~/.devenv/daemon/routes.json`.
//!
//! When authenticated, the daemon also connects to the cloud edge and
//! registers/unregisters routes as they are discovered or removed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const RECONNECT_BASE_DELAY_SECS: u64 = 1;
const RECONNECT_MAX_DELAY_SECS: u64 = 300;
/// How often the daemon re-reads auth.json to pick up login/logout changes.
const AUTH_CHECK_INTERVAL_SECS: u64 = 30;
/// Refresh the JWT when it has less than 1 hour remaining.
const TOKEN_REFRESH_THRESHOLD_SECS: u64 = 3600;
/// How often to check if the token needs refreshing.
const TOKEN_REFRESH_CHECK_INTERVAL_SECS: u64 = 300;

struct ReconnectBackoff {
    next_attempt_at: Option<Instant>,
    current_delay: Duration,
}

impl ReconnectBackoff {
    fn new() -> Self {
        Self {
            next_attempt_at: None,
            current_delay: Duration::from_secs(RECONNECT_BASE_DELAY_SECS),
        }
    }

    fn is_due(&self) -> bool {
        self.next_attempt_at.is_none_or(|t| Instant::now() >= t)
    }

    fn on_failure(&mut self) {
        tracing::info!(
            "Will retry cloud reconnect in {}s",
            self.current_delay.as_secs()
        );
        self.next_attempt_at = Some(Instant::now() + self.current_delay);
        self.current_delay =
            (self.current_delay * 2).min(Duration::from_secs(RECONNECT_MAX_DELAY_SECS));
    }

    fn on_success(&mut self) {
        self.next_attempt_at = None;
        self.current_delay = Duration::from_secs(RECONNECT_BASE_DELAY_SECS);
    }
}

use anyhow::{Context, Result};
use devenv_tunnel_client::domain_router::DomainRouter;

use crate::auth::AuthConfig;
use crate::cloud::CloudConnector;
use crate::discovery::{self, DiscoveredNetworkService};
use crate::net::overlay::{OverlayConfig, OverlayNetwork};
use crate::net::service_table::ServiceTable;
use crate::route_table::{RouteChanges, RouteTable};

/// Grace period before removing a route after its process exits.
/// Allows for quick process restarts without flapping.
const PROCESS_EXIT_GRACE_SECS: u64 = 5;

/// Configuration for the discovery daemon.
pub struct DaemonConfig {
    /// Directory for daemon state files (routes.json, daemon.pid, daemon.log).
    pub state_dir: PathBuf,
    /// How often to scan for services, in seconds.
    pub scan_interval_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let state_dir = dirs::home_dir()
            .map(|h| h.join(".devenv").join("daemon"))
            .unwrap_or_else(|| PathBuf::from(".devenv/daemon"));

        Self {
            state_dir,
            scan_interval_secs: 2,
        }
    }
}

impl DaemonConfig {
    /// Path to the routes file.
    pub fn routes_path(&self) -> PathBuf {
        self.state_dir.join("routes.json")
    }

    /// Path to the PID file.
    pub fn pid_path(&self) -> PathBuf {
        self.state_dir.join("daemon.pid")
    }

    /// Path to the log file.
    pub fn log_path(&self) -> PathBuf {
        self.state_dir.join("daemon.log")
    }

    /// Path to the cloud connection state file.
    pub fn cloud_state_path(&self) -> PathBuf {
        self.state_dir.join("cloud_state.json")
    }
}

/// Tracks routes whose owning process has exited, giving them a grace period
/// before removal.
struct GracePeriodTracker {
    /// domain -> when the process was first noticed missing
    missing_since: HashMap<String, Instant>,
    grace_duration: Duration,
}

impl GracePeriodTracker {
    fn new() -> Self {
        Self {
            missing_since: HashMap::new(),
            grace_duration: Duration::from_secs(PROCESS_EXIT_GRACE_SECS),
        }
    }

    /// Record that a process for this domain is still missing.
    /// Returns true if the grace period has expired and the route should be removed.
    fn process_missing(&mut self, domain: &str) -> bool {
        let first_seen = self
            .missing_since
            .entry(domain.to_string())
            .or_insert_with(Instant::now);
        first_seen.elapsed() >= self.grace_duration
    }

    /// Record that a process for this domain is alive (clear grace period).
    fn process_alive(&mut self, domain: &str) {
        self.missing_since.remove(domain);
    }

    /// Clear all entries for domains that are no longer in the route table.
    fn prune(&mut self, active_domains: &[&String]) {
        self.missing_since
            .retain(|d, _| active_domains.contains(&d));
    }
}

/// Run the discovery loop until cancelled.
///
/// This is the main entry point for the daemon process. It:
/// 1. Writes a PID file
/// 2. Optionally connects to the cloud edge (if authenticated)
/// 3. Scans for services every `scan_interval_secs`
/// 4. Registers/unregisters routes with cloud as they change
/// 5. Checks for process exit with a grace period
pub async fn run_discovery_loop(config: &DaemonConfig) -> Result<()> {
    std::fs::create_dir_all(&config.state_dir).with_context(|| {
        format!(
            "Failed to create daemon state directory: {}\n\n\
             Check that you have write permissions to ~/.devenv/",
            config.state_dir.display()
        )
    })?;

    // Write PID file
    let pid = std::process::id();
    std::fs::write(config.pid_path(), pid.to_string()).with_context(|| {
        format!(
            "Failed to write PID file: {}\n\n\
             Is another discovery daemon already running? \
             Check with: devenv tunnel status",
            config.pid_path().display()
        )
    })?;

    tracing::info!("Discovery daemon started (PID {})", pid);
    tracing::info!(
        "Scanning every {}s, routes at {}",
        config.scan_interval_secs,
        config.routes_path().display()
    );

    // Set up domain router (shared with cloud connector)
    let domain_router = DomainRouter::new();

    // Load initial auth state and attempt cloud connection.
    let auth = AuthConfig::load();
    let mut current_token: Option<String> = auth.token.clone();
    let mut account_id: Option<String> = auth.account_id.clone();
    let mut username: Option<String> = auth.username.clone();
    // Blocks automatic reconnect attempts when the server has rejected our token.
    // Cleared when the token changes (i.e. the user re-authenticates).
    let mut auth_failed = false;
    let mut cloud: Option<CloudConnector> = None;

    if let Some(ref token) = current_token {
        tracing::info!("Auth token found, connecting to cloud edge");
        let mut connector = CloudConnector::new(token.clone());
        match connector.connect(domain_router.clone()).await {
            Ok(()) => {
                write_cloud_state(config, true, None);
                cloud = Some(connector);
            }
            Err(e) => {
                let err_msg = e.root_cause().to_string();
                tracing::warn!(
                    "Failed to connect to cloud edge: {}. Running in local-only mode.",
                    e
                );
                write_cloud_state(config, false, Some(err_msg));
            }
        }
    } else {
        tracing::info!("No auth token found, running in local-only mode");
        write_cloud_state(config, false, None);
    }

    let mut route_table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let mut grace_tracker = GracePeriodTracker::new();
    let mut reconnect_backoff = ReconnectBackoff::new();
    let interval = Duration::from_secs(config.scan_interval_secs);
    let mut next_auth_check = Instant::now() + Duration::from_secs(AUTH_CHECK_INTERVAL_SECS);
    let mut next_token_refresh_check =
        Instant::now() + Duration::from_secs(TOKEN_REFRESH_CHECK_INTERVAL_SECS);

    // Seed the domain router from any pre-existing routes
    for (domain, route) in &route_table.routes {
        domain_router.add_route(domain.clone(), route.port);
    }

    // The cloud connection is brand-new; register any routes that were
    // persisted from a previous daemon session so they are live immediately
    // without waiting for a scan cycle to detect a "change".
    if let Some(ref connector) = cloud {
        for (domain, route) in &route_table.routes {
            if let Err(e) = connector.register_route(domain, route.port).await {
                tracing::warn!(
                    "Failed to register pre-existing route {} on startup: {}",
                    domain,
                    e
                );
            }
        }
    }

    // Start the virtual overlay network (TUN + smoltcp stack + scoped DNS) for
    // services that use a full `*.devenv.local` name. This is independent of the
    // cloud tunnel: the overlay handles `.devenv.local`, cloud handles everything
    // else, so the two never conflict.
    //
    // Starting is best-effort: creating the TUN device requires root/CAP_NET_ADMIN
    // and will fail in unprivileged or CI environments. On failure we log a warning
    // and continue in cloud/local-only mode rather than aborting the daemon.
    let overlay: Option<OverlayNetwork> = match OverlayNetwork::start(OverlayConfig::default()).await
    {
        Ok(ov) => {
            tracing::info!("Virtual overlay network started (.devenv.local)");
            // Seed the overlay immediately so existing services are reachable
            // without waiting for the first scan cycle.
            refresh_overlay_services(&ov).await;
            Some(ov)
        }
        Err(e) => {
            tracing::warn!(
                "Virtual overlay network not started (continuing in cloud/local-only mode): {:#}. \
                 The overlay needs elevated privileges (root / CAP_NET_ADMIN) to create a TUN device.",
                e
            );
            None
        }
    };

    loop {
        // Check process liveness for existing routes
        let routes_snapshot: Vec<(String, u32)> = route_table
            .routes
            .iter()
            .map(|(d, r)| (d.clone(), r.pid))
            .collect();

        for (domain, route_pid) in &routes_snapshot {
            if is_process_alive(*route_pid) {
                grace_tracker.process_alive(domain);
            } else if grace_tracker.process_missing(domain) {
                tracing::info!(
                    "Process {} for route {} exited (grace period expired), removing route",
                    route_pid,
                    domain
                );
            }
        }

        // Discover services
        let mut discovered = discovery::scan_all(account_id.as_deref(), username.as_deref()).await;

        // Filter out services whose processes have exited and whose grace period
        // hasn't expired yet — keep them in the discovered list to avoid premature
        // removal.
        let grace_domains: Vec<String> = route_table
            .routes
            .iter()
            .filter(|(domain, route)| {
                !is_process_alive(route.pid) && !grace_tracker.process_missing(domain)
            })
            .map(|(_, route)| route.clone())
            .map(|route| route.domain.clone())
            .collect();

        // Re-inject grace-period routes into discovered set so they aren't
        // removed yet
        for domain in &grace_domains {
            if let Some(route) = route_table.routes.get(domain) {
                discovered.push(crate::discovery::DiscoveredService {
                    domain: route.domain.clone(),
                    port: route.port,
                    extra_ports: route.extra_ports.clone(),
                    pid: route.pid,
                    source: route.source.clone(),
                });
            }
        }

        let count = discovered.len();
        let changes = route_table.update(discovered);

        if changes.has_changes() {
            if let Err(e) = route_table.save(&config.routes_path()) {
                tracing::error!("Failed to save route table: {}", e);
            } else {
                log_changes(&changes);
            }

            // Update domain router
            update_domain_router(&domain_router, &changes);

            // Sync with cloud
            if let Some(ref connector) = cloud {
                sync_cloud_routes(connector, &changes).await;
            }
        }

        // Feed `.devenv.local` services into the virtual overlay. This is a
        // separate discovery pass from the cloud/route discovery above and does
        // not touch cloud route registration. No-op when the overlay failed to
        // start (unprivileged environment).
        if let Some(ref ov) = overlay {
            refresh_overlay_services(ov).await;
        }

        // Prune grace tracker
        let active_domains: Vec<&String> = route_table.routes.keys().collect();
        grace_tracker.prune(&active_domains);

        tracing::trace!(
            "Scan complete: {} services, {} routes",
            count,
            route_table.len()
        );

        // Periodic auth re-check: pick up login/logout without requiring a restart.
        if Instant::now() >= next_auth_check {
            next_auth_check = Instant::now() + Duration::from_secs(AUTH_CHECK_INTERVAL_SECS);
            let new_auth = AuthConfig::load();
            let new_token = new_auth.token.clone();
            if new_token != current_token {
                let had_cloud = cloud.is_some();
                current_token = new_token;
                account_id = new_auth.account_id.clone();
                username = new_auth.username.clone();
                auth_failed = false;
                cloud = None; // drop the existing connection; connect block below will re-establish
                reconnect_backoff = ReconnectBackoff::new();
                match &current_token {
                    Some(_) => tracing::info!("Auth token changed, will reconnect to cloud"),
                    None if had_cloud => {
                        tracing::info!("Logged out, disconnecting from cloud");
                        write_cloud_state(config, false, None);
                    }
                    None => {}
                }
            }
        }

        // Periodic JWT token refresh: renew the token before it expires so
        // long-running daemons don't silently lose cloud connectivity.
        if Instant::now() >= next_token_refresh_check {
            next_token_refresh_check =
                Instant::now() + Duration::from_secs(TOKEN_REFRESH_CHECK_INTERVAL_SECS);
            if current_token.is_some() {
                // Re-load auth config so we get the freshest token state.
                let current_auth = AuthConfig::load();
                if let Some(true) = current_auth.is_token_near_expiry(TOKEN_REFRESH_THRESHOLD_SECS)
                {
                    tracing::info!("JWT token is near expiry, attempting auto-refresh");
                    let mut refresh_auth = AuthConfig::load();
                    match refresh_auth.refresh_token().await {
                        Ok(()) => {
                            current_token = refresh_auth.token.clone();
                            if current_token.is_some() {
                                tracing::info!("JWT token refreshed, will reconnect to cloud");
                                cloud = None;
                                reconnect_backoff = ReconnectBackoff::new();
                                auth_failed = false;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to auto-refresh JWT token: {}", e);
                            // Don't block future attempts; the token may still be valid.
                        }
                    }
                }
            }
        }

        // Detect when the server rejected our token (expired or revoked).
        if let Some(ref connector) = cloud {
            if connector.is_auth_failed() {
                tracing::warn!(
                    "Edge server rejected our token (expired or revoked). \
                     Run `devenv tunnel login` to re-authenticate."
                );
                write_cloud_state(
                    config,
                    false,
                    Some(
                        "Authentication failed. Run `devenv tunnel login` to re-authenticate."
                            .to_string(),
                    ),
                );
                cloud = None;
                auth_failed = true;
            }
        }

        // Connect or reconnect whenever we have a token and are not blocked by an
        // auth failure.  This handles: initial connect retries, reconnects after
        // drops, and connecting after a fresh login while the daemon is running.
        if let Some(ref token_ref) = current_token {
            if !auth_failed {
                let needs_connect = match &cloud {
                    None => true,
                    Some(c) => !c.is_connected(),
                };
                if needs_connect && reconnect_backoff.is_due() {
                    let token = token_ref.clone();
                    let is_reconnect = cloud.is_some();
                    cloud = None; // drop any stale connector before creating the new one
                    tracing::info!(
                        "{}connecting to cloud edge...",
                        if is_reconnect { "Re" } else { "C" }
                    );
                    let mut connector = CloudConnector::new(token);
                    match connector.connect(domain_router.clone()).await {
                        Ok(()) => {
                            reconnect_backoff.on_success();
                            write_cloud_state(config, true, None);
                            for (domain, route) in &route_table.routes {
                                if let Err(e) = connector.register_route(domain, route.port).await {
                                    tracing::warn!("Failed to re-register route {}: {}", domain, e);
                                }
                            }
                            cloud = Some(connector);
                        }
                        Err(e) => {
                            let err_msg = e.root_cause().to_string();
                            tracing::warn!("Cloud connect failed: {}", e);
                            reconnect_backoff.on_failure();
                            write_cloud_state(config, false, Some(err_msg));
                        }
                    }
                }
            }
        }

        // Sleep until the next scan, but wake early on a shutdown signal so we
        // can tear the overlay (and its TUN/resolver config) down cleanly.
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = wait_for_shutdown_signal() => {
                tracing::info!("Shutdown signal received, stopping discovery daemon");
                break;
            }
        }
    }

    // Graceful shutdown: tear down the overlay (removes the scoped resolver
    // config and the TUN device) before exiting so we don't leave the system's
    // DNS pointed at a dead server. Best-effort — never blocks process exit.
    if let Some(ov) = overlay {
        ov.shutdown().await;
        tracing::info!("Virtual overlay network shut down");
    }

    remove_pid_file(config);
    tracing::info!("Discovery daemon stopped");

    Ok(())
}

/// Build an overlay `ServiceTable` from the services discovered for the local
/// virtual network (those whose `DEVENV_TUNNEL` value ends in `.devenv.local`).
///
/// This is pure (no TUN / no privileges required) so it can be unit-tested.
fn build_overlay_table(services: &[DiscoveredNetworkService]) -> ServiceTable {
    let mut table = ServiceTable::new();
    for svc in services {
        table.register(
            svc.name.clone(),
            svc.real_addr,
            svc.service_port,
            svc.pid,
        );
    }
    table
}

/// Best-effort: push the latest set of `.devenv.local` overlay services into the
/// running overlay network. Logs and ignores errors so a transient stack issue
/// never disrupts the (independent) cloud/local route flow.
async fn refresh_overlay_services(overlay: &OverlayNetwork) {
    let services = discovery::scan_network_services().await;
    let table = build_overlay_table(&services);
    if let Err(e) = overlay.update_services(table).await {
        tracing::warn!("Failed to update overlay services: {:#}", e);
    }
}

/// Wait for a termination signal (SIGTERM or Ctrl-C) so the loop can shut down
/// gracefully and tear down the overlay/TUN. Resolves when either fires.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to install SIGTERM handler: {}", e);
                // Fall back to ctrl_c only.
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Update the domain router with route changes.
fn update_domain_router(router: &DomainRouter, changes: &RouteChanges) {
    for route in &changes.added {
        router.add_route(route.domain.clone(), route.port);
    }
    for route in &changes.removed {
        router.remove_route(&route.domain);
    }
    for route in &changes.changed {
        router.add_route(route.domain.clone(), route.port);
    }
}

/// Sync route changes with the cloud connector.
async fn sync_cloud_routes(connector: &CloudConnector, changes: &RouteChanges) {
    for route in &changes.added {
        if let Err(e) = connector.register_route(&route.domain, route.port).await {
            tracing::warn!(
                "Failed to register route {} with cloud: {}",
                route.domain,
                e
            );
        }
    }
    for route in &changes.removed {
        if let Err(e) = connector.unregister_route(&route.domain).await {
            tracing::warn!(
                "Failed to unregister route {} from cloud: {}",
                route.domain,
                e
            );
        }
    }
    for route in &changes.changed {
        // Re-register with updated port
        if let Err(e) = connector.register_route(&route.domain, route.port).await {
            tracing::warn!("Failed to update route {} with cloud: {}", route.domain, e);
        }
    }
}

/// Write the current cloud connection state to a JSON file in the daemon state dir.
fn write_cloud_state(config: &DaemonConfig, connected: bool, error: Option<String>) {
    let path = config.cloud_state_path();
    let json = match error {
        Some(ref err) => {
            let escaped =
                serde_json::to_string(err).unwrap_or_else(|_| "\"unknown error\"".to_string());
            format!(r#"{{"connected":false,"error":{}}}"#, escaped)
        }
        None => format!(r#"{{"connected":{}}}"#, connected),
    };
    if let Err(e) = std::fs::write(&path, json) {
        tracing::warn!("Failed to write cloud state: {}", e);
    }
}

/// Read the cloud connection state written by the running daemon.
/// Returns None if the file doesn't exist or can't be parsed.
pub fn read_cloud_connected(config: &DaemonConfig) -> Option<bool> {
    let content = std::fs::read_to_string(config.cloud_state_path()).ok()?;
    if content.contains("\"connected\":true") {
        Some(true)
    } else if content.contains("\"connected\":false") {
        Some(false)
    } else {
        None
    }
}

/// Read the last connection error stored by the running daemon, if any.
pub fn read_cloud_error(config: &DaemonConfig) -> Option<String> {
    let content = std::fs::read_to_string(config.cloud_state_path()).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    val.get("error")?.as_str().map(|s| s.to_string())
}

/// Check if a process is still alive by PID.
fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    use sysinfo::{Pid, System};
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    sys.process(Pid::from_u32(pid)).is_some()
}

/// Log route changes to tracing.
fn log_changes(changes: &RouteChanges) {
    for route in &changes.added {
        tracing::info!(
            "Route added: {} -> {}:{} ({})",
            route.domain,
            route.host,
            route.port,
            route.source
        );
    }
    for route in &changes.removed {
        tracing::info!(
            "Route removed: {} (was {}:{})",
            route.domain,
            route.host,
            route.port
        );
    }
    for route in &changes.changed {
        tracing::info!(
            "Route changed: {} -> {}:{} ({})",
            route.domain,
            route.host,
            route.port,
            route.source
        );
    }
}

/// Read the daemon PID from the PID file. Returns None if not found or stale.
pub fn read_daemon_pid(config: &DaemonConfig) -> Option<u32> {
    let pid_path = config.pid_path();
    let content = std::fs::read_to_string(&pid_path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;

    if is_process_alive(pid) {
        Some(pid)
    } else {
        // Stale PID file, clean it up
        let _ = std::fs::remove_file(&pid_path);
        None
    }
}

/// Remove the PID file (on clean shutdown).
pub fn remove_pid_file(config: &DaemonConfig) {
    let _ = std::fs::remove_file(config.pid_path());
}

/// Stop the discovery daemon by sending SIGTERM (Unix) or terminating (Windows).
pub fn stop_daemon(config: &DaemonConfig) -> Result<()> {
    let pid = read_daemon_pid(config).ok_or_else(|| {
        anyhow::anyhow!(
            "Discovery daemon is not running.\n\n\
             Start it with: devenv tunnel start"
        )
    })?;

    #[cfg(unix)]
    {
        use std::process::Command;
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .context("Failed to send SIGTERM to daemon")?;
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("taskkill")
            .args(["/PID", &pid.to_string()])
            .status()
            .context("Failed to terminate daemon process")?;
    }

    remove_pid_file(config);

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_config_default() {
        let config = DaemonConfig::default();
        assert!(config
            .routes_path()
            .to_string_lossy()
            .contains("routes.json"));
        assert!(config.pid_path().to_string_lossy().contains("daemon.pid"));
        assert!(config.log_path().to_string_lossy().contains("daemon.log"));
        assert_eq!(config.scan_interval_secs, 2);
    }

    #[test]
    fn test_reconnect_backoff_initial_due() {
        let backoff = ReconnectBackoff::new();
        assert!(backoff.is_due());
    }

    #[test]
    fn test_reconnect_backoff_not_due_after_failure() {
        let mut backoff = ReconnectBackoff::new();
        backoff.on_failure();
        // Immediately after failure the timer hasn't elapsed yet
        assert!(!backoff.is_due());
    }

    #[test]
    fn test_reconnect_backoff_doubles_on_failure() {
        let mut backoff = ReconnectBackoff::new();
        assert_eq!(backoff.current_delay, Duration::from_secs(1));
        backoff.on_failure();
        assert_eq!(backoff.current_delay, Duration::from_secs(2));
        backoff.on_failure();
        assert_eq!(backoff.current_delay, Duration::from_secs(4));
    }

    #[test]
    fn test_reconnect_backoff_capped_at_max() {
        let mut backoff = ReconnectBackoff {
            next_attempt_at: None,
            current_delay: Duration::from_secs(RECONNECT_MAX_DELAY_SECS / 2 + 1),
        };
        backoff.on_failure();
        assert_eq!(
            backoff.current_delay,
            Duration::from_secs(RECONNECT_MAX_DELAY_SECS)
        );
        backoff.on_failure();
        assert_eq!(
            backoff.current_delay,
            Duration::from_secs(RECONNECT_MAX_DELAY_SECS)
        );
    }

    #[test]
    fn test_reconnect_backoff_resets_on_success() {
        let mut backoff = ReconnectBackoff::new();
        backoff.on_failure();
        backoff.on_failure();
        backoff.on_success();
        assert!(backoff.is_due());
        assert_eq!(
            backoff.current_delay,
            Duration::from_secs(RECONNECT_BASE_DELAY_SECS)
        );
    }

    #[test]
    fn test_grace_period_tracker() {
        let mut tracker = GracePeriodTracker {
            missing_since: HashMap::new(),
            grace_duration: Duration::from_millis(0), // instant expiry for testing
        };

        // First call: starts tracking, but with 0ms grace it expires immediately
        assert!(tracker.process_missing("test.devenv.tools"));

        // Mark alive clears it
        tracker.process_alive("test.devenv.tools");
        assert!(tracker.missing_since.is_empty());
    }

    #[test]
    fn test_grace_period_tracker_not_expired() {
        let mut tracker = GracePeriodTracker {
            missing_since: HashMap::new(),
            grace_duration: Duration::from_secs(60), // very long grace
        };

        // With a 60s grace, it should not expire on first check
        assert!(!tracker.process_missing("test.devenv.tools"));
    }

    #[test]
    fn test_grace_period_prune() {
        let mut tracker = GracePeriodTracker::new();
        tracker
            .missing_since
            .insert("old.devenv.tools".to_string(), Instant::now());
        tracker
            .missing_since
            .insert("current.devenv.tools".to_string(), Instant::now());

        let current = "current.devenv.tools".to_string();
        let active = vec![&current];
        tracker.prune(&active);

        assert!(!tracker.missing_since.contains_key("old.devenv.tools"));
        assert!(tracker.missing_since.contains_key("current.devenv.tools"));
    }

    #[test]
    fn test_is_process_alive_zero_pid() {
        assert!(!is_process_alive(0));
    }

    #[test]
    fn test_is_process_alive_current() {
        let current_pid = std::process::id();
        assert!(is_process_alive(current_pid));
    }

    #[test]
    fn test_is_process_alive_nonexistent() {
        // PID 99999999 is very unlikely to exist
        assert!(!is_process_alive(99_999_999));
    }

    #[test]
    fn test_update_domain_router() {
        use crate::discovery::ServiceSource;
        use crate::route_table::Route;

        let router = DomainRouter::new();
        let changes = RouteChanges {
            added: vec![Route {
                domain: "api.test.devenv.tools".to_string(),
                host: "127.0.0.1".to_string(),
                port: 8080,
                extra_ports: vec![],
                source: ServiceSource::Process { cwd: None },
                pid: 1,
                discovered_at: chrono::Utc::now(),
            }],
            removed: vec![],
            changed: vec![],
        };

        update_domain_router(&router, &changes);
        assert_eq!(router.resolve("api.test.devenv.tools"), Some(8080));
    }

    #[test]
    fn test_build_overlay_table_registers_services() {
        use crate::discovery::{DiscoveredNetworkService, ServiceSource};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let services = vec![
            DiscoveredNetworkService {
                name: "my-db".to_string(),
                real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 32768),
                service_port: 5432,
                pid: 100,
                source: ServiceSource::Process { cwd: None },
            },
            DiscoveredNetworkService {
                name: "my-api".to_string(),
                real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41000),
                service_port: 8080,
                pid: 101,
                source: ServiceSource::Process { cwd: None },
            },
        ];

        let table = build_overlay_table(&services);
        assert_eq!(table.len(), 2);

        let db = table.get("my-db").expect("my-db registered");
        assert_eq!(db.service_port, 5432);
        assert_eq!(db.real_addr.port(), 32768);
        assert_eq!(db.pid, 100);

        let api = table.get("my-api").expect("my-api registered");
        assert_eq!(api.service_port, 8080);
        // Distinct names receive distinct virtual IPs.
        assert_ne!(db.vip, api.vip);
    }

    #[test]
    fn test_build_overlay_table_empty() {
        let table = build_overlay_table(&[]);
        assert!(table.is_empty());
    }

    #[test]
    fn test_read_daemon_pid_no_file() {
        let config = DaemonConfig {
            state_dir: PathBuf::from("/nonexistent/path"),
            scan_interval_secs: 2,
        };
        assert!(read_daemon_pid(&config).is_none());
    }

    fn temp_config() -> (DaemonConfig, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = DaemonConfig {
            state_dir: dir.path().to_path_buf(),
            scan_interval_secs: 2,
        };
        (config, dir)
    }

    #[test]
    fn test_cloud_state_connected() {
        let (config, _dir) = temp_config();
        write_cloud_state(&config, true, None);
        assert_eq!(read_cloud_connected(&config), Some(true));
        assert_eq!(read_cloud_error(&config), None);
    }

    #[test]
    fn test_cloud_state_disconnected_no_error() {
        let (config, _dir) = temp_config();
        write_cloud_state(&config, false, None);
        assert_eq!(read_cloud_connected(&config), Some(false));
        assert_eq!(read_cloud_error(&config), None);
    }

    #[test]
    fn test_cloud_state_disconnected_with_error() {
        let (config, _dir) = temp_config();
        write_cloud_state(
            &config,
            false,
            Some("Connection refused (os error 111)".to_string()),
        );
        assert_eq!(read_cloud_connected(&config), Some(false));
        assert_eq!(
            read_cloud_error(&config),
            Some("Connection refused (os error 111)".to_string())
        );
    }

    #[test]
    fn test_cloud_state_error_with_special_chars() {
        let (config, _dir) = temp_config();
        write_cloud_state(
            &config,
            false,
            Some(r#"error with "quotes" and \backslash"#.to_string()),
        );
        assert_eq!(
            read_cloud_error(&config),
            Some(r#"error with "quotes" and \backslash"#.to_string())
        );
    }

    #[test]
    fn test_cloud_state_missing_file() {
        let config = DaemonConfig {
            state_dir: PathBuf::from("/nonexistent/path"),
            scan_interval_secs: 2,
        };
        assert_eq!(read_cloud_connected(&config), None);
        assert_eq!(read_cloud_error(&config), None);
    }
}
