//! `portzero wait <tunnel-domain>`: block until a tunnel is up (and, with
//! `--healthy` or a declared `PZ_HEALTH_PATH`, until its health path returns
//! 2xx). The readiness gate for CD smoke tests and Playwright `webServer`
//! blocks.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use portzero_daemon::discovery_loop::DaemonConfig;

use crate::export::{lookup_tunnel, TunnelUrl};

/// Default readiness timeout when `--timeout` is not given.
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// How often to re-check the tunnel / poll the health path.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// The outcome of a single readiness probe.
enum Probe {
    /// The tunnel is up (and healthy, if that was requested).
    Ready,
    /// The tunnel exists but is not ready yet (not discovered, health non-2xx,
    /// or awaiting cloud review). Carries a short reason for the timeout message.
    NotYet(String),
    /// The tunnel is paused at the edge (a cloud-side, edge-only pause). This is
    /// distinct from "dead": the backend may be fine, but the edge is holding
    /// traffic. Reported distinctly so callers don't confuse it with a crash.
    Paused,
}

/// `portzero wait <domain> [--healthy] [--timeout <secs>]`.
pub async fn wait(domain: &str, healthy: bool, timeout: Option<u64>) -> Result<()> {
    let config = DaemonConfig::load();
    let target = domain.trim().to_string();
    let timeout = Duration::from_secs(timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let deadline = Instant::now() + timeout;

    let client = reqwest::Client::builder()
        .no_proxy()
        .danger_accept_invalid_certs(false)
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))?;

    // Set on every non-ready probe; read only in the timeout branch. Every path
    // that reaches the timeout check first passes through the `NotYet` arm
    // (`Ready` returns, `Paused` bails), so it is always assigned by then.
    let mut last_reason: String;

    loop {
        match probe(&config, &client, &target, healthy).await {
            Probe::Ready => {
                println!("{target} is ready");
                return Ok(());
            }
            Probe::Paused => {
                // Fail fast and distinctly: waiting will not help a paused tunnel.
                bail!(
                    "Tunnel '{target}' is paused at the edge (edge-only pause, cloud-side \
                     feature). It is not dead — resume it in the dashboard \
                     (https://app.portzero.cloud) to continue. `portzero wait` will not \
                     block for a paused tunnel."
                );
            }
            Probe::NotYet(reason) => {
                last_reason = reason;
            }
        }

        if Instant::now() >= deadline {
            bail!(
                "Timed out after {}s waiting for '{target}' ({last_reason}). \
                 Is the process/container running with PZ_TUNNEL set{}? Check `portzero status`.",
                timeout.as_secs(),
                if healthy {
                    " and serving its health path"
                } else {
                    ""
                }
            );
        }

        // Sleep, but never overshoot the deadline.
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(POLL_INTERVAL.min(remaining)).await;
    }
}

/// One readiness probe: is the tunnel discovered, not paused, and (if required)
/// serving 2xx on its health path?
async fn probe(
    config: &DaemonConfig,
    client: &reqwest::Client,
    domain: &str,
    healthy: bool,
) -> Probe {
    // A paused status short-circuits everything (distinct from dead).
    if edge_status(config, domain).as_deref() == Some("paused") {
        return Probe::Paused;
    }

    let Some(tunnel) = lookup_tunnel(config, domain) else {
        return Probe::NotYet("tunnel not discovered yet".to_string());
    };

    // Cloud tunnels awaiting owner review are up locally but not publicly
    // routable; keep waiting and say so.
    if let Some(status) = edge_status(config, domain) {
        if status == "pending_review" {
            return Probe::NotYet("awaiting cloud review (pending_review)".to_string());
        }
        if status == "denied" {
            return Probe::NotYet("cloud review denied (denied)".to_string());
        }
    }

    // Health is polled when explicitly requested OR when the endpoint declared
    // a PZ_HEALTH_PATH. When --healthy is set without a declared path, default
    // to "/".
    let health_path = tunnel.health_path.clone();
    let must_poll = healthy || health_path.is_some();
    if !must_poll {
        return Probe::Ready;
    }

    let path = health_path.unwrap_or_else(|| "/".to_string());
    match poll_health(client, &tunnel, &path).await {
        Ok(true) => Probe::Ready,
        Ok(false) => Probe::NotYet(format!("health path {path} not yet 2xx")),
        Err(reason) => Probe::NotYet(reason),
    }
}

/// GET the tunnel's health URL; `Ok(true)` on a 2xx response.
async fn poll_health(
    client: &reqwest::Client,
    tunnel: &TunnelUrl,
    path: &str,
) -> Result<bool, String> {
    let url = format!("{}{}", tunnel.url.trim_end_matches('/'), path);
    match client.get(&url).send().await {
        Ok(resp) => Ok(resp.status().is_success()),
        Err(e) => Err(format!("health request to {url} failed: {e}")),
    }
}

/// Read this tunnel's cloud edge status (`published` / `pending_review` /
/// `denied` / `paused`) from `cloud_route_status.json`, if present.
///
/// `.portzero.local` overlay tunnels never carry an edge status, so this returns
/// `None` for them.
fn edge_status(config: &DaemonConfig, domain: &str) -> Option<String> {
    let path = config.cloud_route_status_path();
    let content = std::fs::read_to_string(path).ok()?;
    let map: std::collections::HashMap<String, String> = serde_json::from_str(&content).ok()?;
    map.into_iter()
        .find(|(d, _)| d.eq_ignore_ascii_case(domain))
        .map(|(_, status)| status)
}
