//! Discovery daemon loop: periodically scan for services and update routes.
//!
//! The daemon runs as a background process, scanning every few seconds for
//! processes and Docker containers with PZ_TUNNEL set. Changes are
//! persisted to `~/.portzero/daemon/routes.json`.
//!
//! When authenticated, the daemon also connects to the cloud edge and
//! registers/unregisters routes as they are discovered or removed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

const RECONNECT_BASE_DELAY_SECS: u64 = 1;
const RECONNECT_MAX_DELAY_SECS: u64 = 300;
/// How often the daemon re-reads auth.json to pick up login/logout changes.
const AUTH_CHECK_INTERVAL_SECS: u64 = 30;
/// Refresh the JWT when it has less than 1 hour remaining.
const TOKEN_REFRESH_THRESHOLD_SECS: u64 = 3600;
/// How often to check if the token needs refreshing.
const TOKEN_REFRESH_CHECK_INTERVAL_SECS: u64 = 300;
/// How often to poll ~/.portzero/config.toml for external changes (or writes
/// performed via the local management API) and hot-reload the effective values.
/// A short interval makes `scan_interval`, overlay https policy, and dns
/// first-hit policy updates visible quickly without requiring a daemon restart.
const CONFIG_RELOAD_INTERVAL_SECS: u64 = 5;
const OVERLAY_REFRESH_TIMEOUT: Duration = Duration::from_secs(3);

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
use portzero_tunnel_client::domain_router::DomainRouter;

use crate::auth::AuthConfig;
use crate::cloud::CloudConnector;
use crate::discovery::{self, DiscoveredNetworkService};
use crate::docker_events::{self, ConflictRegistry};
use crate::net::dns::DnsFirstHitPolicy;
use crate::net::overlay::{OverlayConfig, OverlayNetwork, ResolverCheck};
use crate::net::service_table::ServiceTable;
use crate::net::stack::OverlayHttpsPolicy;
use crate::notify::{self, IssuesState};
use crate::route_table::{OverlayRoute, OverlayState, RouteChanges, RouteTable};

/// Grace period before removing a route after its process exits.
/// Allows for quick process restarts without flapping.
const PROCESS_EXIT_GRACE_SECS: u64 = 5;

/// Configuration for the discovery daemon.
#[derive(Clone)]
pub struct DaemonConfig {
    /// Directory for daemon state files (routes.json, daemon.pid, daemon.log).
    pub state_dir: PathBuf,
    /// How often to scan for services, in seconds.
    pub scan_interval_secs: u64,
    /// HTTPS behavior for `.portzero.local` overlay services.
    pub overlay_https: OverlayHttpsPolicy,
    /// How DNS handles first hits for unknown `.portzero.local` services.
    pub dns_first_hit_policy: DnsFirstHitPolicy,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let state_dir = dirs::home_dir()
            .map(|h| h.join(".portzero").join("daemon"))
            .unwrap_or_else(|| PathBuf::from(".portzero/daemon"));

        Self {
            state_dir,
            scan_interval_secs: 2,
            overlay_https: OverlayHttpsPolicy::default(),
            dns_first_hit_policy: DnsFirstHitPolicy::default(),
        }
    }
}

impl DaemonConfig {
    /// Load daemon configuration from `~/.portzero/config.toml`, falling back to
    /// defaults when the file is absent or invalid.
    pub fn load() -> Self {
        let mut config = Self::default();
        let path = config.config_path();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return config;
        };

        match toml::from_str::<FileConfig>(&raw) {
            Ok(file) => file.apply_to(&mut config),
            Err(e) => tracing::warn!("Ignoring invalid daemon config {}: {e}", path.display()),
        }
        config
    }

    /// Path to the user-editable TOML config file.
    pub fn config_path(&self) -> PathBuf {
        self.state_dir
            .parent()
            .map(|p| p.join("config.toml"))
            .unwrap_or_else(|| PathBuf::from(".portzero/config.toml"))
    }

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

    /// Path to the per-cloud-route review status file (domain → status),
    /// written by the cloud connector and read by the local dashboard.
    pub fn cloud_route_status_path(&self) -> PathBuf {
        self.state_dir.join("cloud_route_status.json")
    }

    /// Path to the visibility "issues" state file (duplicate names, etc.).
    pub fn issues_path(&self) -> PathBuf {
        self.state_dir.join("issues.json")
    }

    /// Path to the overlay services state file (read by `portzero status`).
    pub fn overlay_path(&self) -> PathBuf {
        self.state_dir.join("overlay.json")
    }

    /// Path to the observed runtime-truth file (observed edges + exercised
    /// routes), read by `portzero inspect` and the MCP server.
    pub fn observations_path(&self) -> PathBuf {
        self.state_dir.join("observations.json")
    }

    /// Path to the diagnostics report file.
    pub fn diagnostics_path(&self) -> PathBuf {
        self.state_dir.join("diagnostics.json")
    }

    /// Write (or update) only the HTTPS policy section in the config file (next to state dir,
    /// typically `~/.portzero/config.toml`). Preserves other existing keys/sections using
    /// a TOML value merge.
    pub fn write_https_policy(&self, policy: OverlayHttpsPolicy) -> Result<()> {
        let path = self.config_path();

        let mut root: toml::Value = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(raw) => {
                    toml::from_str(&raw).unwrap_or_else(|_| toml::Value::Table(Default::default()))
                }
                Err(_) => toml::Value::Table(Default::default()),
            }
        } else {
            toml::Value::Table(Default::default())
        };

        // Ensure [overlay.https] table exists and set the three keys.
        if let Some(tbl) = root.as_table_mut() {
            let overlay = tbl
                .entry("overlay".to_owned())
                .or_insert(toml::Value::Table(Default::default()));
            if let Some(ov) = overlay.as_table_mut() {
                let https = ov
                    .entry("https".to_owned())
                    .or_insert(toml::Value::Table(Default::default()));
                if let Some(h) = https.as_table_mut() {
                    h.insert(
                        "enable_for_port_80".to_owned(),
                        toml::Value::Boolean(policy.enable_for_port_80),
                    );
                    h.insert(
                        "redirect_port_80".to_owned(),
                        toml::Value::Boolean(policy.redirect_port_80),
                    );
                    h.insert(
                        "passthrough_port_443".to_owned(),
                        toml::Value::Boolean(policy.passthrough_port_443),
                    );
                }
            }
        }

        let serialized =
            toml::to_string_pretty(&root).context("serializing config.toml for https policy")?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir for {}", path.display()))?;
        }
        std::fs::write(&path, serialized)
            .with_context(|| format!("writing https policy to {}", path.display()))?;
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    daemon: Option<FileDaemonConfig>,
    overlay: Option<FileOverlayConfig>,
}

impl FileConfig {
    fn apply_to(self, config: &mut DaemonConfig) {
        if let Some(daemon) = self.daemon {
            if let Some(scan_interval_secs) = daemon.scan_interval_secs {
                config.scan_interval_secs = scan_interval_secs;
            }
        }
        if let Some(overlay) = self.overlay {
            overlay.apply_to(config);
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileDaemonConfig {
    scan_interval_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct FileOverlayConfig {
    https: Option<FileOverlayHttpsConfig>,
    dns_first_hit_policy: Option<DnsFirstHitPolicy>,
}

impl FileOverlayConfig {
    fn apply_to(self, config: &mut DaemonConfig) {
        if let Some(policy) = self.dns_first_hit_policy {
            config.dns_first_hit_policy = policy;
        }
        if let Some(https) = self.https {
            if let Some(v) = https.enable_for_port_80 {
                config.overlay_https.enable_for_port_80 = v;
            }
            if let Some(v) = https.redirect_port_80 {
                config.overlay_https.redirect_port_80 = v;
            }
            if let Some(v) = https.passthrough_port_443 {
                config.overlay_https.passthrough_port_443 = v;
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileOverlayHttpsConfig {
    enable_for_port_80: Option<bool>,
    redirect_port_80: Option<bool>,
    passthrough_port_443: Option<bool>,
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
    crate::install_default_crypto_provider();

    std::fs::create_dir_all(&config.state_dir).with_context(|| {
        format!(
            "Failed to create daemon state directory: {}\n\n\
             Check that you have write permissions to ~/.portzero/",
            config.state_dir.display()
        )
    })?;

    // Ensure we are the sole discovery daemon. If another live daemon is already
    // running, take over from it (SIGTERM, wait for it to release the TUN/DNS
    // resources, escalate to SIGKILL if it overstays) before claiming the PID
    // file. This runs BEFORE OverlayNetwork::start so the old daemon has released
    // the TUN device and DNS port by the time we create ours.
    acquire_singleton_or_take_over(config)?;

    let mut config = config.clone();

    let pid = std::process::id();
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

    // Records observed edges + exercised routes from forwarded cloud traffic
    // (task-63), persisted to observations.json for `portzero inspect` and the
    // MCP server. Shared with every cloud connector we create.
    let observations = Arc::new(crate::observations::ObservationStore::new(
        &config.state_dir,
    ));

    if let Some(ref token) = current_token {
        tracing::info!("Auth token found, connecting to cloud edge");
        let mut connector = CloudConnector::new(token.clone());
        connector.set_status_path(config.cloud_route_status_path());
        connector.set_observations(observations.clone());
        match connector.connect(domain_router.clone()).await {
            Ok(()) => {
                let p = connector.plan();
                let c = connector.can_use_cloud_tunnels();
                let m = connector.status_message();
                write_cloud_state(&config, true, None, p, c, m);
                cloud = Some(connector);
            }
            Err(e) => {
                let err_msg = e.root_cause().to_string();
                tracing::warn!(
                    "Failed to connect to cloud edge: {}. Running in local-only mode.",
                    e
                );
                write_cloud_state(&config, false, Some(err_msg), None, None, None);
            }
        }
    } else {
        tracing::info!("No auth token found, running in local-only mode");
        write_cloud_state(&config, false, None, None, None, None);
    }

    let mut route_table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let mut grace_tracker = GracePeriodTracker::new();
    let mut reconnect_backoff = ReconnectBackoff::new();
    let mut next_auth_check = Instant::now() + Duration::from_secs(AUTH_CHECK_INTERVAL_SECS);
    let mut next_token_refresh_check =
        Instant::now() + Duration::from_secs(TOKEN_REFRESH_CHECK_INTERVAL_SECS);
    let mut next_config_check = Instant::now() + Duration::from_secs(CONFIG_RELOAD_INTERVAL_SECS);
    let mut last_config_mtime: Option<std::time::SystemTime> =
        std::fs::metadata(config.config_path())
            .ok()
            .and_then(|m| m.modified().ok());

    // Seed the domain router from any pre-existing routes
    for (domain, route) in &route_table.routes {
        domain_router.add_route(domain.clone(), route.port);
    }

    // The cloud connection is brand-new; register any routes that were
    // persisted from a previous daemon session so they are live immediately
    // without waiting for a scan cycle to detect a "change".
    if let Some(ref connector) = cloud {
        for (domain, route) in &route_table.routes {
            if let Err(e) = connector
                .register_route(
                    domain,
                    route.port,
                    Some(crate::cloud::build_route_metadata(route)),
                )
                .await
            {
                tracing::warn!(
                    "Failed to register pre-existing route {} on startup: {}",
                    domain,
                    e
                );
            }
        }
    }

    // Start the virtual overlay network (TUN + smoltcp stack + scoped DNS) for
    // services that use a full `*.portzero.local` name. This is independent of the
    // cloud tunnel: the overlay handles `.portzero.local`, cloud handles everything
    // else, so the two never conflict.
    //
    // Starting is best-effort: creating the TUN device requires root/CAP_NET_ADMIN
    // and will fail in unprivileged or CI environments. On failure we log a warning
    // and continue in cloud/local-only mode rather than aborting the daemon.
    // Tracks the last issue set we logged loudly / notified about, so repeated
    // scans of the same problem don't spam the log or the desktop. Seeded from
    // any state a previous daemon left behind so a restart doesn't re-notify for
    // an unchanged, still-present issue.
    let mut notified_issues = notify::read_issues(&config.issues_path());

    // Robust Docker event monitor (task-8): watches `docker events` in real time
    // for container start/die so we can (a) surface host-port-bind conflicts that
    // the periodic poll would miss (a failed start dies before the next scan) and
    // (b) rescan immediately instead of waiting the full poll interval. The
    // `ConflictRegistry` collects conflict issues the monitor finds so they can be
    // merged into the per-scan issue set below. `docker_rescan` lets the monitor
    // wake the loop early; `docker_shutdown` tears the monitor down cleanly.
    let conflicts = ConflictRegistry::new();
    let docker_rescan = Arc::new(Notify::new());
    let docker_shutdown = Arc::new(Notify::new());

    // Lets the embedded DNS server wake the discovery loop the instant it sees a
    // query for an unknown `*.portzero.local` name, so a just-started service is
    // discovered immediately instead of waiting for the next poll cycle.
    let dns_rescan = Arc::new(Notify::new());
    let docker_monitor = tokio::spawn(docker_events::run_event_monitor(
        conflicts.clone(),
        docker_rescan.clone(),
        docker_shutdown.clone(),
    ));

    let (mgmt_port, mgmt_store) =
        match crate::management::ManagementServer::bind(config.state_dir.clone()).await {
            Ok((mgmt_server, mgmt_listener)) => {
                let mgmt_port = mgmt_server.bound_port;
                let mgmt_store = mgmt_server.store.clone();
                tokio::spawn(mgmt_server.serve(mgmt_listener));
                (mgmt_port, mgmt_store)
            }
            Err(e) => {
                tracing::warn!(
                    "Management API server could not bind; continuing without local API: {e:#}"
                );
                (0, Arc::new(tokio::sync::RwLock::new(HashMap::new())))
            }
        };

    let overlay: Option<Arc<OverlayNetwork>> = match OverlayNetwork::start(
        OverlayConfig {
            https_policy: config.overlay_https,
            dns_first_hit_policy: config.dns_first_hit_policy,
            ..OverlayConfig::default()
        },
        dns_rescan.clone(),
    )
    .await
    {
        Ok(ov) => {
            tracing::info!("Virtual overlay network started (.portzero.local)");
            // Seed the overlay immediately so existing services are reachable
            // without waiting for the first scan cycle.
            let (overlay_issues, overlay_services) = match tokio::time::timeout(
                OVERLAY_REFRESH_TIMEOUT,
                refresh_overlay_services(&ov, mgmt_port, &mgmt_store),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!("Initial overlay service refresh timed out; continuing");
                    (Vec::new(), Vec::new())
                }
            };
            write_overlay_state(&config, &overlay_services, true);
            let docker_conflicts = conflicts.snapshot().await;
            gather_and_publish_issues(
                &config,
                &route_table,
                overlay_issues,
                &overlay_services,
                docker_conflicts,
                Vec::new(),
                &mut notified_issues,
            )
            .await;
            Some(Arc::new(ov))
        }
        Err(e) => {
            tracing::warn!(
                "Virtual overlay network not started (continuing in cloud/local-only mode): {:#}. \
                 The overlay needs elevated privileges (root / CAP_NET_ADMIN) to create a TUN device.",
                e
            );
            write_overlay_state(&config, &[], false);
            None
        }
    };

    // Run an initial diagnostics scan so `status_json` has data from the moment
    // the daemon starts, without waiting for the first 30-second periodic check.
    {
        let diag = crate::diagnostics::run_diagnostics(&config.state_dir).await;
        crate::diagnostics::save_report(&diag, &config.state_dir);
        tracing::info!(
            "diagnostics: {} check(s) run, {} issue(s) found",
            diag.checks_run,
            diag.issues.len()
        );
    }

    // Long-lived shutdown future. We `select!` the ENTIRE loop body against this
    // every iteration so a SIGTERM/Ctrl-C is honoured immediately — even in the
    // middle of a (potentially slow, subprocess-heavy) scan — rather than only
    // in the gap between scans. `Box::pin` keeps the same future alive across
    // iterations so a signal that arrives mid-iteration is not lost when the
    // branch's local future is dropped.
    let mut shutdown = Box::pin(wait_for_shutdown_signal());

    let mut diag_scan_counter: u32 = 0;
    // Whether we have already fired a desktop notification for the current
    // "resolver removed" episode. Reset once the resolver is healthy again so a
    // future removal notifies afresh (rather than once per 30s check).
    let mut resolver_repair_notified = false;
    // The set of post-setup regressions (sorted diagnostic IDs) we last notified
    // about, so a still-present, unchanged problem doesn't re-notify every 30s.
    // Reset to empty once everything setup-related is healthy again.
    let mut post_setup_notified: Vec<String> = Vec::new();
    let overlay_fast_refresh = overlay.as_ref().map(|ov| {
        tokio::spawn(run_dns_fast_overlay_refresh(
            config.clone(),
            ov.clone(),
            mgmt_port,
            mgmt_store.clone(),
            dns_rescan.clone(),
        ))
    });

    loop {
        // The per-iteration work (scan + cloud sync + sleep) lives in this async
        // block so it can be raced against `shutdown` at the top level. If the
        // signal fires, this future is dropped at its next `.await` point and we
        // break out to teardown.
        let iteration = async {
            // Re-run diagnostics every ~30s (every 15 × 2s scan cycles).
            diag_scan_counter = diag_scan_counter.wrapping_add(1);
            if diag_scan_counter.is_multiple_of(15) {
                let diag = crate::diagnostics::run_diagnostics(&config.state_dir).await;
                crate::diagnostics::save_report(&diag, &config.state_dir);

                // Detect out-of-band removal of the scoped `.portzero.local`
                // resolver (e.g. a user or another tool deleted
                // `/etc/resolver/portzero.local`) and repair it, surfacing a
                // one-shot notification so the user knows it happened.
                if let Some(ref ov) = overlay {
                    match ov.ensure_resolver_installed().await {
                        ResolverCheck::Repaired => {
                            if !resolver_repair_notified {
                                notify::send_notification(
                                    "portzero: DNS resolver restored",
                                    "The .portzero.local resolver was removed and has \
                                     been recreated automatically.",
                                );
                                resolver_repair_notified = true;
                            }
                        }
                        ResolverCheck::RepairFailed => {
                            if !resolver_repair_notified {
                                notify::send_notification(
                                    "portzero: DNS resolver missing",
                                    "The .portzero.local resolver was removed and could \
                                     not be recreated. Run `sudo portzero setup` to fix it.",
                                );
                                resolver_repair_notified = true;
                            }
                        }
                        ResolverCheck::Ok => resolver_repair_notified = false,
                    }
                }

                // Alarm on any OTHER post-setup step that has regressed
                // out-of-band (CA trust removed, capabilities dropped, the
                // dashboard hosts pin deleted, the autostart service removed,
                // …). The diagnostics pass above already detected these; here we
                // turn the setup-related ones into a single desktop notification
                // pointing at the fix. Gated on the overlay having started (a
                // real privileged install) so a dev running the daemon in the
                // foreground is never nagged.
                if overlay.is_some() {
                    notify_post_setup_regressions(&diag, &mut post_setup_notified);
                }
            }

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
            let (mut discovered, cloud_scope_issues) =
                discovery::scan_all(account_id.as_deref(), username.as_deref()).await;

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
                        domain_template: route.domain_template.clone(),
                        substitutions: route.substitutions.clone(),
                        port: route.port,
                        extra_ports: route.extra_ports.clone(),
                        health_path: route.health_path.clone(),
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

            // Feed `.portzero.local` services into the virtual overlay. This is a
            // separate discovery pass from the cloud/route discovery above and does
            // not touch cloud tunnel registration. No-op when the overlay failed to
            // start (unprivileged environment).
            //
            // Also run the visibility checks (duplicate-name detection + legacy
            // listener monitoring) and publish the combined issue set once per scan.
            // Legacy monitoring runs regardless of whether the overlay started, so
            // when the overlay is unavailable we still scan with empty overlay data.
            let (overlay_issues, overlay_services) = if let Some(ref ov) = overlay {
                match tokio::time::timeout(
                    OVERLAY_REFRESH_TIMEOUT,
                    refresh_overlay_services(ov, mgmt_port, &mgmt_store),
                )
                .await
                {
                    Ok(result) => {
                        write_overlay_state(&config, &result.1, true);
                        result
                    }
                    Err(_) => {
                        tracing::warn!("Overlay service refresh timed out; keeping previous state");
                        (Vec::new(), Vec::new())
                    }
                }
            } else {
                // Overlay TUN is not running (insufficient privileges), but still scan
                // so that `portzero status` can surface discovered .local services.
                let services = match tokio::time::timeout(
                    OVERLAY_REFRESH_TIMEOUT,
                    discovery::scan_network_services(&mgmt_store),
                )
                .await
                {
                    Ok(services) => services,
                    Err(_) => {
                        tracing::warn!("Overlay service scan timed out; keeping previous state");
                        Vec::new()
                    }
                };
                let issues = notify::detect_duplicate_names(&services);
                write_overlay_state(&config, &services, false);
                (issues, services)
            };
            let docker_conflicts = conflicts.snapshot().await;
            gather_and_publish_issues(
                &config,
                &route_table,
                overlay_issues,
                &overlay_services,
                docker_conflicts,
                cloud_scope_issues,
                &mut notified_issues,
            )
            .await;

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
                            write_cloud_state(&config, false, None, None, None, None);
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
                    if let Some(true) =
                        current_auth.is_token_near_expiry(TOKEN_REFRESH_THRESHOLD_SECS)
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

            // Poll config file for changes (scan interval, overlay https policy,
            // dns first-hit policy). Changes are applied without restarting the
            // daemon and without dropping any active tunnel or overlay connections.
            if Instant::now() >= next_config_check {
                next_config_check =
                    Instant::now() + Duration::from_secs(CONFIG_RELOAD_INTERVAL_SECS);
                let path = config.config_path();
                let current_mtime = std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok());
                if current_mtime.is_some() && current_mtime != last_config_mtime
                    || (last_config_mtime.is_some() && current_mtime.is_none())
                {
                    let reloaded = DaemonConfig::load();
                    let changed_scan = reloaded.scan_interval_secs != config.scan_interval_secs;
                    let changed_https = reloaded.overlay_https != config.overlay_https;
                    let changed_dns = reloaded.dns_first_hit_policy != config.dns_first_hit_policy;

                    if changed_scan || changed_https || changed_dns {
                        tracing::info!("daemon config file changed; reloading");
                    }
                    if changed_scan {
                        tracing::info!(
                            "scan_interval_secs now {} (was {})",
                            reloaded.scan_interval_secs,
                            config.scan_interval_secs
                        );
                    }
                    if changed_https {
                        tracing::info!("overlay https policy updated");
                        if let Some(ref ov) = overlay {
                            if let Err(e) = ov.update_https_policy(reloaded.overlay_https).await {
                                tracing::warn!(?e, "failed to push https policy to overlay stack");
                            }
                        }
                    }
                    if changed_dns {
                        tracing::info!("dns_first_hit_policy updated");
                        if let Some(ref ov) = overlay {
                            ov.update_dns_first_hit_policy(reloaded.dns_first_hit_policy);
                        }
                    }
                    config = reloaded;
                    last_config_mtime = current_mtime;
                } else if current_mtime.is_some() {
                    last_config_mtime = current_mtime;
                }
            }

            // Snapshot current plan and any status message (e.g. plan limit) from a live connector
            // into cloud_state.json so that `portzero status` and the web UI see fresh upsell info
            // without waiting for a reconnect.
            if let Some(ref connector) = cloud {
                if connector.is_connected() {
                    let p = connector.plan();
                    let c = connector.can_use_cloud_tunnels();
                    let m = connector.status_message();
                    // Only rewrite if we have something new to say (plan or message)
                    if p.is_some() || m.is_some() {
                        // Preserve previous error if any
                        let prev_err = read_cloud_error(&config);
                        write_cloud_state(&config, true, prev_err, p, c, m);
                    }
                }
            }

            // Detect when the server rejected our token (expired or revoked).
            if let Some(ref connector) = cloud {
                if connector.is_auth_failed() {
                    tracing::warn!(
                        "Edge server rejected our token (expired or revoked). \
                     Run `portzero login` to re-authenticate."
                    );
                    write_cloud_state(
                        &config,
                        false,
                        Some(
                            "Authentication failed. Run `portzero login` to re-authenticate."
                                .to_string(),
                        ),
                        None,
                        None,
                        None,
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
                        connector.set_status_path(config.cloud_route_status_path());
                        connector.set_observations(observations.clone());
                        match connector.connect(domain_router.clone()).await {
                            Ok(()) => {
                                reconnect_backoff.on_success();
                                let p = connector.plan();
                                let c = connector.can_use_cloud_tunnels();
                                let m = connector.status_message();
                                write_cloud_state(&config, true, None, p, c, m);
                                for (domain, route) in &route_table.routes {
                                    if let Err(e) = connector
                                        .register_route(
                                            domain,
                                            route.port,
                                            Some(crate::cloud::build_route_metadata(route)),
                                        )
                                        .await
                                    {
                                        tracing::warn!(
                                            "Failed to re-register route {}: {}",
                                            domain,
                                            e
                                        );
                                    }
                                }
                                cloud = Some(connector);
                            }
                            Err(e) => {
                                let err_msg = e.root_cause().to_string();
                                tracing::warn!("Cloud connect failed: {}", e);
                                reconnect_backoff.on_failure();
                                write_cloud_state(&config, false, Some(err_msg), None, None, None);
                            }
                        }
                    }
                }
            }

            // Sleep until the next scan, waking early when the Docker event monitor
            // sees a relevant container start/die so a new (or vanished) container is
            // reflected promptly instead of waiting the full poll interval. The
            // shutdown signal is handled one level up (the top-level `select!`
            // below), so it can interrupt this sleep AND any of the scan work above.
            // Uses the (possibly reloaded) scan interval.
            let sleep_dur = Duration::from_secs(config.scan_interval_secs);
            tokio::select! {
                _ = tokio::time::sleep(sleep_dur) => {}
                _ = docker_rescan.notified() => {
                    tracing::trace!("Docker event triggered an immediate rescan");
                }
                _ = dns_rescan.notified() => {
                    tracing::trace!("DNS miss on an unknown subdomain triggered an immediate rescan");
                }
            }
        }; // end of `iteration` async block

        tokio::select! {
            _ = iteration => {}
            _ = &mut shutdown => {
                tracing::info!("Shutdown signal received, stopping discovery daemon");
                break;
            }
        }
    }

    // We have begun shutting down. From here on, a SECOND signal (or a stalled
    // graceful teardown) must force the process to exit so the operator never
    // has to `pkill`. Arm a watchdog that hard-exits on either condition.
    spawn_shutdown_watchdog();

    // Stop the Docker event monitor cleanly alongside the overlay teardown.
    docker_shutdown.notify_one();
    docker_monitor.abort();
    if let Some(task) = overlay_fast_refresh {
        task.abort();
    }

    // Graceful shutdown: tear down the overlay (removes the scoped resolver
    // config and the TUN device) before exiting so we don't leave the system's
    // DNS pointed at a dead server. Best-effort and time-bounded — if teardown
    // wedges (e.g. a blocking resolver uninstall), the watchdog above forces
    // exit, but we also cap the overlay teardown directly so the common case
    // returns promptly.
    if let Some(ov) = overlay {
        match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, ov.shutdown()).await {
            Ok(()) => tracing::info!("Virtual overlay network shut down"),
            Err(_) => tracing::warn!(
                "Overlay shutdown did not complete within {}s; continuing exit",
                GRACEFUL_SHUTDOWN_TIMEOUT.as_secs()
            ),
        }
    }

    // Persist any observed edges / exercised routes that were buffered since the
    // last throttled flush, so the final snapshot on disk is complete.
    observations.flush();

    remove_pid_file(&config);
    tracing::info!("Discovery daemon stopped");

    Ok(())
}

/// Hard upper bound on how long the graceful teardown may take before we force
/// the process to exit. Keeps Ctrl-C / SIGTERM responsive even if a teardown
/// step wedges.
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Arm a safety net so shutdown can never wedge the process.
///
/// A hard deadline (`GRACEFUL_SHUTDOWN_TIMEOUT`) forces `process::exit(0)` even
/// if tokio's blocking-thread pool is still draining (e.g. an in-flight
/// spawn_blocking scan). Uses a plain OS thread so it survives the tokio runtime
/// being torn down when the `main()` future returns.
fn spawn_shutdown_watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(GRACEFUL_SHUTDOWN_TIMEOUT);
        tracing::warn!(
            "Graceful shutdown exceeded {}s, forcing exit",
            GRACEFUL_SHUTDOWN_TIMEOUT.as_secs()
        );
        std::process::exit(0);
    });
}

/// Persist discovered overlay services to `overlay.json` so `portzero status` can read them.
fn write_overlay_state(
    config: &DaemonConfig,
    services: &[DiscoveredNetworkService],
    overlay_active: bool,
) {
    let routes = services
        .iter()
        .map(|s| OverlayRoute {
            domain: format!("{}.portzero.local", s.name),
            domain_template: s.domain_template.clone(),
            substitutions: s.substitutions.clone(),
            service_port: s.service_port,
            real_addr: s.real_addr.to_string(),
            health_path: s.health_path.clone(),
            pid: s.pid,
            source: s.source.clone(),
        })
        .collect();
    let state = OverlayState {
        overlay_active,
        routes,
    };
    if let Err(e) = state.save(&config.overlay_path()) {
        tracing::warn!("Failed to save overlay state: {}", e);
    }
}

/// Build an overlay `ServiceTable` from the services discovered for the local
/// virtual network (those whose `PZ_TUNNEL` value ends in `.portzero.local`).
///
/// When `mgmt_port != 0` the management service ("portzero") is injected as the
/// first entry, mapping to `127.0.0.1:<mgmt_port>` on virtual port 80.
///
/// This is pure (no TUN / no privileges required) so it can be unit-tested.
fn build_overlay_table(services: &[DiscoveredNetworkService], mgmt_port: u16) -> ServiceTable {
    let mut table = ServiceTable::new();
    if mgmt_port != 0 {
        let backend = std::net::SocketAddr::from(([127, 0, 0, 1], mgmt_port));
        table.register("portzero".to_string(), backend, 80, 0);
        table.register("portzero-api".to_string(), backend, 80, 0);
    }
    for svc in services {
        table.register_with_backend_protocol(
            svc.name.clone(),
            svc.real_addr,
            svc.service_port,
            svc.pid,
            svc.backend_protocol,
        );
    }
    table
}

/// Best-effort: push the latest set of `.portzero.local` overlay services into the
/// running overlay network. Logs and ignores errors so a transient stack issue
/// never disrupts the (independent) cloud/local route flow.
///
/// Returns the duplicate-name visibility issues detected over the freshly
/// scanned services *and* the scanned overlay service list itself (so the caller
/// can build a managed-context for legacy-listener monitoring without scanning
/// twice). The caller merges these issues with other sources and publishes them
/// once per scan.
async fn refresh_overlay_services(
    overlay: &OverlayNetwork,
    mgmt_port: u16,
    mgmt_store: &crate::management::RegistrationStore,
) -> (Vec<notify::Issue>, Vec<DiscoveredNetworkService>) {
    let services = discovery::scan_network_services(mgmt_store).await;
    let issues = notify::detect_duplicate_names(&services);
    let table = build_overlay_table(&services, mgmt_port);
    if let Err(e) = overlay.update_services(table).await {
        tracing::warn!("Failed to update overlay services: {:#}", e);
    }
    (issues, services)
}

/// Low-latency path for first DNS hits on unknown `*.portzero.local` names.
///
/// The main discovery loop performs full reconciliation, including Docker scans,
/// cloud sync, diagnostics, and issue publishing. That is correct but too much
/// work to put on the critical path of a browser's first DNS lookup. This task
/// reacts to DNS misses independently and refreshes only local process overlay
/// routes, which is the common `PZ_TUNNEL=foo.portzero.local cargo run` case.
async fn run_dns_fast_overlay_refresh(
    config: DaemonConfig,
    overlay: Arc<OverlayNetwork>,
    mgmt_port: u16,
    mgmt_store: crate::management::RegistrationStore,
    dns_rescan: Arc<Notify>,
) {
    loop {
        dns_rescan.notified().await;

        let Some(services) = discovery::scan_network_process_services(&mgmt_store).await else {
            continue;
        };
        let table = build_overlay_table(&services, mgmt_port);
        if let Err(e) = overlay.update_services(table).await {
            tracing::warn!("Fast overlay refresh failed to update services: {e:#}");
            continue;
        }
        write_overlay_state(&config, &services, true);
        tracing::trace!(
            "Fast overlay refresh complete: {} process service(s)",
            services.len()
        );
    }
}

/// Gather all current issues from every source and publish them once.
///
/// Sources:
///  - duplicate `.portzero.local` names (from the overlay scan, when the overlay
///    is running),
///  - legacy listeners: processes serving common/managed ports directly,
///    bypassing port-zero.
///
/// `overlay_issues` / `overlay_services` come from a prior overlay refresh (or
/// are empty when the overlay isn't running). Best-effort and non-fatal.
async fn gather_and_publish_issues(
    config: &DaemonConfig,
    route_table: &RouteTable,
    overlay_issues: Vec<notify::Issue>,
    overlay_services: &[DiscoveredNetworkService],
    docker_conflicts: Vec<notify::Issue>,
    cloud_scope_issues: Vec<notify::Issue>,
    notified_issues: &mut IssuesState,
) {
    let managed = build_managed_context(route_table, overlay_services);
    // enumerate_system_listeners() does a full /proc scan + per-pid fd reads —
    // blocking. Run it on the spawn_blocking pool so the async executor stays
    // free to handle shutdown signals and other tasks.
    let legacy =
        tokio::task::spawn_blocking(move || crate::legacy_monitor::scan_legacy_listeners(&managed))
            .await
            .unwrap_or_default();
    let mut all = overlay_issues;
    all.extend(legacy);
    // Real-time Docker port-bind conflicts caught by the event monitor (task-8).
    all.extend(docker_conflicts);
    // Invalidly-scoped PZ_TUNNEL cloud tunnel domains found on this scan
    // (e.g. myservice.portzero.cloud instead of myservice.<cloud-username>.tunnel.portzero.cloud).
    all.extend(cloud_scope_issues);
    publish_issues(all, config, notified_issues);
}

/// Build the set of ports/cwds the daemon already manages (route table routes
/// plus overlay services), so the legacy-listener monitor never flags a service
/// we are already tunneling. Pure given its inputs.
fn build_managed_context(
    route_table: &RouteTable,
    overlay_services: &[DiscoveredNetworkService],
) -> crate::legacy_monitor::ManagedContext {
    use crate::discovery::ServiceSource;
    let mut managed = crate::legacy_monitor::ManagedContext::new();
    for route in route_table.routes.values() {
        if route.port != 0 {
            managed.ports.insert(route.port);
        }
        if let ServiceSource::Process { cwd: Some(cwd) } = &route.source {
            managed.cwds.insert(cwd.clone());
        }
    }
    for svc in overlay_services {
        managed.ports.insert(svc.real_addr.port());
        if let ServiceSource::Process { cwd: Some(cwd) } = &svc.source {
            managed.cwds.insert(cwd.clone());
        }
    }
    managed
}

/// Persist the full set of current issues (from all sources) so
/// `portzero status` can surface them, and — only when the set changes
/// since the last scan — log actionable guidance and fire a single native
/// notification. The change-gating prevents flooding the log/desktop every scan.
fn publish_issues(
    issues: Vec<notify::Issue>,
    config: &DaemonConfig,
    notified_issues: &mut IssuesState,
) {
    let current = IssuesState {
        issues: issues.clone(),
    };

    // Persist the current state so `portzero status` can surface it.
    notify::write_issues(&config.issues_path(), &current);

    if current == *notified_issues {
        return;
    }

    if issues.is_empty() {
        tracing::info!("All previously reported tunnel issues are now resolved");
    } else {
        for issue in &issues {
            tracing::warn!("{} — {}", issue.summary(), issue.fix_hint());
        }
        // One consolidated notification covering all current issues.
        let first = &issues[0];
        let title = if issues.len() == 1 {
            "portzero: issue detected".to_string()
        } else {
            format!("portzero: {} issues", issues.len())
        };
        let body = format!("{}\n{}", first.summary(), first.fix_hint());
        notify::send_notification(&title, &body);
    }

    *notified_issues = current;
}

/// Fire a single desktop notification when the set of post-setup regressions
/// changes to a non-empty set, pointing the user at the fix.
///
/// A "post-setup regression" is a step `portzero setup` / the installer
/// configured that has since been removed or broken out-of-band (CA trust
/// removed, Linux capabilities dropped, the dashboard hosts pin deleted, the
/// autostart service removed, …). These are already detected by the diagnostics
/// pass and surfaced in `portzero status`; this turns the setup-related ones
/// into a loud, actionable alert.
///
/// De-duplicated exactly like the resolver self-heal: we re-notify only when the
/// set of problems *changes* (mirroring `publish_issues`), and reset silently
/// once everything setup-related is healthy again — so a still-present, unchanged
/// problem does not nag every 30s. Best-effort and non-fatal.
fn notify_post_setup_regressions(
    diag: &crate::diagnostics::DiagnosticsReport,
    last_notified: &mut Vec<String>,
) {
    let mut regressions: Vec<&crate::diagnostics::Diagnostic> = diag
        .issues
        .iter()
        .filter(|d| crate::diagnostics::is_post_setup_regression(&d.id))
        .collect();
    // Stable, severity-then-id ordering so the "first" issue we headline and the
    // change-detection key are both deterministic across scans.
    regressions.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.id.cmp(&b.id)));
    let current_ids: Vec<String> = regressions.iter().map(|d| d.id.clone()).collect();

    if current_ids == *last_notified {
        return;
    }

    if let Some(first) = regressions.first() {
        for d in &regressions {
            tracing::warn!("post-setup regression: {} ({})", d.title, d.id);
        }
        let title = if regressions.len() == 1 {
            "portzero: setup problem detected".to_string()
        } else {
            format!("portzero: {} setup problems", regressions.len())
        };
        // Prefer the diagnostic's concrete fix command, falling back to its
        // description, and finally to the generic setup command.
        let fix = first
            .fix
            .as_ref()
            .map(|f| f.command.clone().unwrap_or_else(|| f.description.clone()))
            .unwrap_or_else(|| "Run `sudo portzero setup` to fix it.".to_string());
        let body = format!("{}\n{}", first.title, fix);
        notify::send_notification(&title, &body);
    } else {
        tracing::info!("All previously reported post-setup regressions are now resolved");
    }

    *last_notified = current_ids;
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
        if let Err(e) = connector
            .register_route(
                &route.domain,
                route.port,
                Some(crate::cloud::build_route_metadata(route)),
            )
            .await
        {
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
        if let Err(e) = connector
            .register_route(
                &route.domain,
                route.port,
                Some(crate::cloud::build_route_metadata(route)),
            )
            .await
        {
            tracing::warn!("Failed to update route {} with cloud: {}", route.domain, e);
        }
    }
}

/// Serializable cloud connection state persisted to cloud_state.json.
/// Extended to carry plan (for upsell prompts) and a user-facing status_message
/// (e.g. when the edge rejects a cloud route due to PlanLimitExceeded).
#[derive(Serialize, Deserialize, Default, Clone)]
struct CloudStateFile {
    connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    can_use_cloud_tunnels: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// Write the current cloud connection state (and plan / user message) to disk.
fn write_cloud_state(
    config: &DaemonConfig,
    connected: bool,
    error: Option<String>,
    plan: Option<String>,
    can_use_cloud_tunnels: Option<bool>,
    message: Option<String>,
) {
    let path = config.cloud_state_path();
    let state = CloudStateFile {
        connected,
        plan,
        can_use_cloud_tunnels,
        error,
        message,
    };
    match serde_json::to_string(&state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("Failed to write cloud state: {}", e);
            }
        }
        Err(e) => tracing::warn!("Failed to serialize cloud state: {}", e),
    }
}

/// Read the full persisted cloud state (internal helper).
fn read_full_cloud_state(config: &DaemonConfig) -> CloudStateFile {
    read_full_cloud_state_from_path(&config.cloud_state_path())
}

fn read_full_cloud_state_from_path(p: &std::path::Path) -> CloudStateFile {
    let content = match std::fs::read_to_string(p) {
        Ok(c) => c,
        Err(_) => return CloudStateFile::default(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

/// Read the cloud connection state written by the running daemon.
/// Returns None if the file doesn't exist or can't be parsed.
pub fn read_cloud_connected(config: &DaemonConfig) -> Option<bool> {
    read_cloud_connected_from_path(&config.cloud_state_path())
}

fn read_cloud_connected_from_path(path: &std::path::Path) -> Option<bool> {
    let content = std::fs::read_to_string(path).ok()?;
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
    read_full_cloud_state(config).error
}

/// Read the plan reported by the edge (e.g. "free", "pro").
pub fn read_cloud_plan(config: &DaemonConfig) -> Option<String> {
    read_full_cloud_state(config).plan
}

/// Convenience path-based reader (used by diagnostics which only has state_dir).
pub fn read_cloud_plan_from_path(state_dir: &std::path::Path) -> Option<String> {
    read_full_cloud_state_from_path(&state_dir.join("cloud_state.json")).plan
}

/// Read whether the account can create cloud tunnels right now (own plan or
/// a paid team it belongs to), as last reported by the edge on Welcome.
pub fn read_cloud_can_use_tunnels(config: &DaemonConfig) -> Option<bool> {
    read_full_cloud_state(config).can_use_cloud_tunnels
}

/// Convenience path-based reader (used by diagnostics which only has state_dir).
pub fn read_cloud_can_use_tunnels_from_path(state_dir: &std::path::Path) -> Option<bool> {
    read_full_cloud_state_from_path(&state_dir.join("cloud_state.json")).can_use_cloud_tunnels
}

/// Read the latest user-facing status message (plan limits, upsell, etc.).
pub fn read_cloud_message(config: &DaemonConfig) -> Option<String> {
    read_full_cloud_state(config).message
}

/// Convenience path-based reader.
pub fn read_cloud_message_from_path(state_dir: &std::path::Path) -> Option<String> {
    read_full_cloud_state_from_path(&state_dir.join("cloud_state.json")).message
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

/// Send a stop signal to `pid`. With `force`, escalates to SIGKILL (`taskkill /F`
/// on Windows); otherwise a graceful SIGTERM (Unix). Best-effort: only a failure
/// to launch the kill command surfaces as an error.
fn signal_pid(pid: u32, force: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::process::Command;
        let sig = if force { "-KILL" } else { "-TERM" };
        Command::new("kill")
            .args([sig, &pid.to_string()])
            .status()
            .with_context(|| format!("Failed to signal daemon process {pid}"))?;
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        // Windows has no graceful console signal we can reliably deliver to a
        // detached process, so we terminate it directly in both cases.
        let _ = force;
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status()
            .with_context(|| format!("Failed to terminate daemon process {pid}"))?;
    }
    Ok(())
}

/// How long to wait for an existing daemon to exit after we ask it to stop,
/// before escalating to SIGKILL. Slightly longer than the old daemon's own
/// graceful-teardown budget ([`GRACEFUL_SHUTDOWN_TIMEOUT`] = 5s) plus its
/// shutdown watchdog, so a cleanly-exiting daemon is never force-killed.
const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(8);

/// Ensure this process is the sole discovery daemon, taking over from any
/// existing one, then claim the PID file.
///
/// If a live daemon is recorded in the PID file (and it isn't us), we ask it to
/// stop, wait for it to exit and release the TUN/DNS resources, and escalate to
/// SIGKILL if it overstays [`TAKEOVER_TIMEOUT`]. This makes `start --foreground`
/// — and therefore systemd restarts, `just install`, and manual launches —
/// self-correcting: the newest daemon always wins, instead of silently running
/// alongside an orphaned older one.
fn acquire_singleton_or_take_over(config: &DaemonConfig) -> Result<()> {
    let me = std::process::id();

    if let Some(other) = read_daemon_pid(config) {
        if other != me {
            tracing::warn!(
                "Another discovery daemon (PID {other}) is already running; taking over"
            );
            if let Err(e) = signal_pid(other, false) {
                tracing::warn!("Failed to signal existing daemon {other}: {e:#}");
            }

            let deadline = Instant::now() + TAKEOVER_TIMEOUT;
            while crate::management::pid_lookup::pid_is_alive(other) {
                if Instant::now() >= deadline {
                    tracing::warn!(
                        "Existing daemon {other} did not exit within {}s; sending SIGKILL",
                        TAKEOVER_TIMEOUT.as_secs()
                    );
                    let _ = signal_pid(other, true);
                    // Give the kernel a moment to reap it and release the TUN/port.
                    std::thread::sleep(Duration::from_millis(500));
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            tracing::info!("Previous daemon {other} has exited; claiming ownership");
        }
    }

    std::fs::write(config.pid_path(), me.to_string()).with_context(|| {
        format!(
            "Failed to write PID file: {}\n\n\
             Check write permissions on the daemon state directory.",
            config.pid_path().display()
        )
    })?;
    Ok(())
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
             Start it with: portzero start"
        )
    })?;

    signal_pid(pid, false)?;

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
        assert!(tracker.process_missing("test.portzero.cloud"));

        // Mark alive clears it
        tracker.process_alive("test.portzero.cloud");
        assert!(tracker.missing_since.is_empty());
    }

    #[test]
    fn test_grace_period_tracker_not_expired() {
        let mut tracker = GracePeriodTracker {
            missing_since: HashMap::new(),
            grace_duration: Duration::from_secs(60), // very long grace
        };

        // With a 60s grace, it should not expire on first check
        assert!(!tracker.process_missing("test.portzero.cloud"));
    }

    #[test]
    fn test_grace_period_prune() {
        let mut tracker = GracePeriodTracker::new();
        tracker
            .missing_since
            .insert("old.portzero.cloud".to_string(), Instant::now());
        tracker
            .missing_since
            .insert("current.portzero.cloud".to_string(), Instant::now());

        let current = "current.portzero.cloud".to_string();
        let active = vec![&current];
        tracker.prune(&active);

        assert!(!tracker.missing_since.contains_key("old.portzero.cloud"));
        assert!(tracker.missing_since.contains_key("current.portzero.cloud"));
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
                domain: "api.test.portzero.cloud".to_string(),
                domain_template: "api.test.portzero.cloud".to_string(),
                substitutions: Default::default(),
                host: "127.0.0.1".to_string(),
                port: 8080,
                extra_ports: vec![],
                health_path: None,
                source: ServiceSource::Process { cwd: None },
                pid: 1,
                discovered_at: chrono::Utc::now(),
            }],
            removed: vec![],
            changed: vec![],
        };

        update_domain_router(&router, &changes);
        assert_eq!(router.resolve("api.test.portzero.cloud"), Some(8080));
    }

    #[test]
    fn test_build_overlay_table_registers_services() {
        use crate::discovery::{DiscoveredNetworkService, ServiceSource};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let services = vec![
            DiscoveredNetworkService {
                name: "my-db".to_string(),
                domain_template: "my-db.portzero.local".to_string(),
                substitutions: Default::default(),
                real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 32768),
                service_port: 5432,
                backend_protocol: None,
                pid: 100,
                source: ServiceSource::Process { cwd: None },
                health_path: None,
            },
            DiscoveredNetworkService {
                name: "my-api".to_string(),
                domain_template: "my-api.portzero.local".to_string(),
                substitutions: Default::default(),
                real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41000),
                service_port: 8080,
                backend_protocol: None,
                pid: 101,
                source: ServiceSource::Process { cwd: None },
                health_path: None,
            },
        ];

        let table = build_overlay_table(&services, 0);
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
    fn test_build_overlay_table_keeps_multi_label_names_distinct() {
        use crate::discovery::{DiscoveredNetworkService, ServiceSource};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let services = vec![
            DiscoveredNetworkService {
                name: "staging.portzero.net".to_string(),
                domain_template: "staging.portzero.net.portzero.local".to_string(),
                substitutions: Default::default(),
                real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 32768),
                service_port: 443,
                backend_protocol: None,
                pid: 100,
                source: ServiceSource::Process { cwd: None },
                health_path: None,
            },
            DiscoveredNetworkService {
                name: "staging".to_string(),
                domain_template: "staging.portzero.local".to_string(),
                substitutions: Default::default(),
                real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41000),
                service_port: 443,
                backend_protocol: None,
                pid: 101,
                source: ServiceSource::Process { cwd: None },
                health_path: None,
            },
        ];

        let table = build_overlay_table(&services, 0);
        let nested = table
            .get("staging.portzero.net")
            .expect("nested service registered");
        let short = table.get("staging").expect("short service registered");
        assert_ne!(nested.vip, short.vip);
    }

    #[test]
    fn test_write_overlay_state_preserves_multi_label_domain() {
        use crate::discovery::{DiscoveredNetworkService, ServiceSource};
        use crate::route_table::OverlayState;

        let (config, _dir) = temp_config();
        let services = vec![DiscoveredNetworkService {
            name: "staging.portzero.net".to_string(),
            domain_template: "staging.portzero.net.portzero.local".to_string(),
            substitutions: Default::default(),
            real_addr: "127.0.0.1:32768".parse().unwrap(),
            service_port: 443,
            backend_protocol: None,
            pid: 100,
            source: ServiceSource::Process { cwd: None },
            health_path: None,
        }];

        write_overlay_state(&config, &services, true);

        let state = OverlayState::load(&config.overlay_path()).expect("overlay state");
        assert!(state.overlay_active);
        assert_eq!(state.routes.len(), 1);
        assert_eq!(
            state.routes[0].domain,
            "staging.portzero.net.portzero.local"
        );
    }

    #[test]
    fn test_build_overlay_table_empty() {
        let table = build_overlay_table(&[], 0);
        assert!(table.is_empty());
    }

    #[test]
    fn test_read_daemon_pid_no_file() {
        let config = DaemonConfig {
            state_dir: PathBuf::from("/nonexistent/path"),
            scan_interval_secs: 2,
            overlay_https: OverlayHttpsPolicy::default(),
            dns_first_hit_policy: DnsFirstHitPolicy::default(),
        };
        assert!(read_daemon_pid(&config).is_none());
    }

    fn temp_config() -> (DaemonConfig, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = DaemonConfig {
            state_dir: dir.path().to_path_buf(),
            scan_interval_secs: 2,
            overlay_https: OverlayHttpsPolicy::default(),
            dns_first_hit_policy: DnsFirstHitPolicy::default(),
        };
        (config, dir)
    }

    #[test]
    fn test_cloud_state_connected() {
        let (config, _dir) = temp_config();
        write_cloud_state(&config, true, None, None, None, None);
        assert_eq!(read_cloud_connected(&config), Some(true));
        assert_eq!(read_cloud_error(&config), None);
    }

    #[test]
    fn test_cloud_state_disconnected_no_error() {
        let (config, _dir) = temp_config();
        write_cloud_state(&config, false, None, None, None, None);
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
            overlay_https: OverlayHttpsPolicy::default(),
            dns_first_hit_policy: DnsFirstHitPolicy::default(),
        };
        assert_eq!(read_cloud_connected(&config), None);
        assert_eq!(read_cloud_error(&config), None);
    }

    #[test]
    fn test_cloud_state_with_plan_and_message() {
        let (config, _dir) = temp_config();
        write_cloud_state(
            &config,
            true,
            None,
            Some("free".to_string()),
            Some(false),
            Some("Your plan does not include cloud tunnels.".to_string()),
        );
        assert_eq!(read_cloud_connected(&config), Some(true));
        assert_eq!(read_cloud_plan(&config), Some("free".to_string()));
        assert_eq!(read_cloud_can_use_tunnels(&config), Some(false));
        assert_eq!(
            read_cloud_message(&config),
            Some("Your plan does not include cloud tunnels.".to_string())
        );
        assert_eq!(read_cloud_error(&config), None);
    }

    #[test]
    fn test_file_config_overrides_https_policy() {
        let file: FileConfig = toml::from_str(
            r#"
            [overlay.https]
            enable_for_port_80 = false
            redirect_port_80 = false
            passthrough_port_443 = true
            "#,
        )
        .unwrap();
        let mut config = DaemonConfig::default();
        file.apply_to(&mut config);

        assert!(!config.overlay_https.enable_for_port_80);
        assert!(!config.overlay_https.redirect_port_80);
        assert!(config.overlay_https.passthrough_port_443);
    }

    #[test]
    fn test_file_config_overrides_dns_first_hit_policy() {
        let file: FileConfig = toml::from_str(
            r#"
            [overlay]
            dns_first_hit_policy = "proactive-vip"
            "#,
        )
        .unwrap();
        let mut config = DaemonConfig::default();
        file.apply_to(&mut config);

        assert_eq!(config.dns_first_hit_policy, DnsFirstHitPolicy::ProactiveVip);
    }

    #[test]
    fn test_write_https_policy_roundtrips_via_file() {
        let (cfg, _dir) = temp_config();
        // config.toml lives in parent of the (temp) state dir
        let policy = OverlayHttpsPolicy {
            enable_for_port_80: false,
            redirect_port_80: true,
            passthrough_port_443: false,
        };
        cfg.write_https_policy(policy)
            .expect("write should succeed for temp dir");

        // Load by pointing a fresh default? Simulate: parse the sibling config.toml directly
        let cfg_path = cfg.config_path();
        let raw = std::fs::read_to_string(&cfg_path).expect("config file written");
        let parsed: FileConfig = toml::from_str(&raw).unwrap();
        let mut loaded = DaemonConfig::default();
        parsed.apply_to(&mut loaded);

        assert!(!loaded.overlay_https.enable_for_port_80);
        assert!(loaded.overlay_https.redirect_port_80);
        assert!(!loaded.overlay_https.passthrough_port_443);
    }
}
