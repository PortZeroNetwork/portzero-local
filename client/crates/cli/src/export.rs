//! `portzero url` and `portzero env`: export discovered tunnel URLs so test
//! processes and CI jobs can learn where a tunnel lives.
//!
//! These are the script-friendly escape hatch for non-Playwright consumers; the
//! Playwright fixture package talks to the daemon directly and is the preferred
//! integration.

use anyhow::{bail, Context, Result};

use portzero_daemon::discovery::is_local_overlay_domain;
use portzero_daemon::discovery_loop::DaemonConfig;
use portzero_daemon::route_table::{OverlayState, RouteTable};

/// A discovered tunnel and its resolved URL.
struct TunnelUrl {
    domain: String,
    url: String,
}

/// Resolve the URL a client should use to reach a tunnel.
///
/// - Cloud tunnels (anything not ending in `.portzero.local` / `.local`) are
///   served over HTTPS by the edge; the local backend port is irrelevant to the
///   public URL, so no port is emitted.
/// - Local overlay tunnels are reached by name on the virtual IP. Port 443 is
///   HTTPS, port 80 (or an unknown 0) is HTTP with no explicit port, and any
///   other port is emitted explicitly as `http://<domain>:<port>`.
pub fn tunnel_url(domain: &str, service_port: u16) -> String {
    if is_local_overlay_domain(domain) {
        match service_port {
            443 => format!("https://{domain}"),
            80 | 0 => format!("http://{domain}"),
            p => format!("http://{domain}:{p}"),
        }
    } else {
        format!("https://{domain}")
    }
}

/// Derive a stable, shell-safe environment variable name for a tunnel domain.
///
/// `web.portzero.local` → `PZ_URL_WEB_PORTZERO_LOCAL`.
pub fn env_var_name(domain: &str) -> String {
    let sanitized: String = domain
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("PZ_URL_{sanitized}")
}

/// Collect every discovered tunnel (cloud routes + local overlay routes) with
/// its resolved URL, sorted by domain for stable output.
fn discovered_tunnels(config: &DaemonConfig) -> Vec<TunnelUrl> {
    let table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();

    let mut out: Vec<TunnelUrl> = Vec::new();
    for route in table.routes.values() {
        out.push(TunnelUrl {
            url: tunnel_url(&route.domain, route.port),
            domain: route.domain.clone(),
        });
    }
    for ov in &overlay.routes {
        out.push(TunnelUrl {
            url: tunnel_url(&ov.domain, ov.service_port),
            domain: ov.domain.clone(),
        });
    }

    out.sort_by(|a, b| a.domain.cmp(&b.domain));
    out.dedup_by(|a, b| a.domain.eq_ignore_ascii_case(&b.domain));
    out
}

/// `portzero url <domain>` — print exactly the resolved URL on stdout.
///
/// Nothing else is written to stdout so the output is safe to capture in a
/// script (`BASE_URL=$(portzero url web.myapp.portzero.local)`). Errors go to
/// stderr and the process exits non-zero.
pub fn url(domain: &str) -> Result<()> {
    let config = DaemonConfig::load();
    let target = domain.trim();

    let tunnels = discovered_tunnels(&config);
    if let Some(t) = tunnels
        .iter()
        .find(|t| t.domain.eq_ignore_ascii_case(target))
    {
        // Exactly the URL, nothing else.
        println!("{}", t.url);
        return Ok(());
    }

    // Not found: give an actionable, stderr-only diagnostic.
    if tunnels.is_empty() {
        bail!(
            "No tunnel named '{target}' is known to the daemon (no tunnels are currently \
             discovered). Is the daemon running (`portzero status`) and is PZ_TUNNEL set on \
             the process/container?"
        );
    }
    let known: Vec<&str> = tunnels.iter().map(|t| t.domain.as_str()).collect();
    bail!(
        "No tunnel named '{target}' is known to the daemon. Discovered tunnels: {}.",
        known.join(", ")
    );
}

/// `portzero env [--github]` — export every discovered tunnel's URL.
///
/// Without `--github`, prints `export NAME="URL"` lines to stdout for
/// `eval "$(portzero env)"`. With `--github`, appends `NAME=URL` lines to the
/// file named by `$GITHUB_ENV` so the values are exported to subsequent steps
/// of a GitHub Actions job.
pub fn env(github: bool) -> Result<()> {
    let config = DaemonConfig::load();
    let tunnels = discovered_tunnels(&config);

    if github {
        let github_env = std::env::var("GITHUB_ENV").context(
            "GITHUB_ENV is not set — `portzero env --github` only works inside a GitHub \
             Actions job. Use `portzero env` (without --github) elsewhere.",
        )?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&github_env)
            .with_context(|| format!("Failed to open $GITHUB_ENV file: {github_env}"))?;
        for t in &tunnels {
            // GitHub's environment-file format is `NAME=value`, one per line.
            writeln!(file, "{}={}", env_var_name(&t.domain), t.url)
                .with_context(|| format!("Failed to write to $GITHUB_ENV file: {github_env}"))?;
        }
        eprintln!("Exported {} tunnel URL(s) to $GITHUB_ENV.", tunnels.len());
        return Ok(());
    }

    if tunnels.is_empty() {
        eprintln!(
            "# No tunnels discovered yet. Is the daemon running and PZ_TUNNEL set? \
             (`portzero status`)"
        );
        return Ok(());
    }
    for t in &tunnels {
        println!("export {}=\"{}\"", env_var_name(&t.domain), t.url);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tunnel_url_cloud_is_https_no_port() {
        assert_eq!(
            tunnel_url("api.alice.tunnel.portzero.cloud", 8080),
            "https://api.alice.tunnel.portzero.cloud"
        );
    }

    #[test]
    fn test_tunnel_url_local_ports() {
        assert_eq!(
            tunnel_url("web.portzero.local", 80),
            "http://web.portzero.local"
        );
        assert_eq!(
            tunnel_url("web.portzero.local", 0),
            "http://web.portzero.local"
        );
        assert_eq!(
            tunnel_url("secure.portzero.local", 443),
            "https://secure.portzero.local"
        );
        assert_eq!(
            tunnel_url("db.portzero.local", 5432),
            "http://db.portzero.local:5432"
        );
    }

    #[test]
    fn test_env_var_name() {
        assert_eq!(
            env_var_name("web.portzero.local"),
            "PZ_URL_WEB_PORTZERO_LOCAL"
        );
        assert_eq!(
            env_var_name("api-x.alice.tunnel.portzero.cloud"),
            "PZ_URL_API_X_ALICE_TUNNEL_PORTZERO_CLOUD"
        );
    }
}
