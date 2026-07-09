//! `portzero inspect`: a human-friendly view of the daemon's observed runtime
//! truth — discovered processes/containers, tunnel domains, health paths,
//! observed edges, and exercised routes. Text only; the machine-readable view is
//! the MCP server (`portzero mcp`).

use std::fmt::Write as _;

use anyhow::Result;

use portzero_daemon::discovery::ServiceSource;
use portzero_daemon::discovery_loop::{read_daemon_pid, DaemonConfig};
use portzero_daemon::observations::Observations;
use portzero_daemon::route_table::{OverlayState, RouteTable};

use crate::export::tunnel_url;

/// `portzero inspect` — render the runtime truth as readable text.
pub fn inspect() -> Result<()> {
    let config = DaemonConfig::load();
    print!("{}", render(&config));
    Ok(())
}

/// Build the full inspect report as a string (pure, given the config paths).
fn render(config: &DaemonConfig) -> String {
    let running = read_daemon_pid(config).is_some();
    let table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();
    let obs = Observations::load(&config.observations_path());

    let mut out = String::new();

    let _ = writeln!(out, "portzero inspect — observed runtime truth");
    if running {
        let _ = writeln!(out, "Daemon: running");
    } else {
        let _ = writeln!(
            out,
            "Daemon: stopped (showing last-known state; run `portzero start` for live data)"
        );
    }
    out.push('\n');

    // --- Tunnels (cloud routes + local overlay) ---
    let _ = writeln!(out, "TUNNELS");
    if table.routes.is_empty() && overlay.routes.is_empty() {
        let _ = writeln!(
            out,
            "  (none discovered — set PZ_TUNNEL on a process or container)"
        );
    } else {
        let mut rows: Vec<(String, String, String, String)> = Vec::new();
        for r in table.routes.values() {
            rows.push((
                r.domain.clone(),
                tunnel_url(&r.domain, r.port),
                r.health_path.clone().unwrap_or_else(|| "-".to_string()),
                describe_source(&r.source, r.pid),
            ));
        }
        for r in &overlay.routes {
            rows.push((
                r.domain.clone(),
                tunnel_url(&r.domain, r.service_port),
                r.health_path.clone().unwrap_or_else(|| "-".to_string()),
                describe_source(&r.source, r.pid),
            ));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
        for (domain, url, health, source) in &rows {
            let _ = writeln!(out, "  {domain}");
            let _ = writeln!(out, "      url:    {url}");
            let _ = writeln!(out, "      health: {health}");
            let _ = writeln!(out, "      source: {source}");
        }
    }
    out.push('\n');

    // --- Observed edges ---
    let _ = writeln!(out, "OBSERVED EDGES (who-talks-to-whom)");
    if obs.edges.is_empty() {
        let _ = writeln!(out, "  (none observed yet)");
    } else {
        for e in &obs.edges {
            let from = e.from.as_deref().unwrap_or("(external)");
            let _ = writeln!(
                out,
                "  {from} -> {to}  [{proto}, {count} req]",
                to = e.to,
                proto = e.protocol,
                count = e.request_count,
            );
        }
    }
    out.push('\n');

    // --- Exercised routes ---
    let _ = writeln!(out, "EXERCISED ROUTES (smoke-test inventory)");
    if obs.routes.is_empty() {
        let _ = writeln!(out, "  (none observed yet)");
    } else {
        // Group by tunnel domain for readability.
        let mut by_domain: std::collections::BTreeMap<&str, Vec<&_>> =
            std::collections::BTreeMap::new();
        for r in &obs.routes {
            by_domain.entry(r.domain.as_str()).or_default().push(r);
        }
        for (domain, mut routes) in by_domain {
            routes.sort_by(|a, b| (&a.method, &a.path).cmp(&(&b.method, &b.path)));
            let _ = writeln!(out, "  {domain}");
            for r in routes {
                let tests = if r.tests.is_empty() {
                    String::new()
                } else {
                    format!("  (tests: {})", r.tests.join(", "))
                };
                let _ = writeln!(
                    out,
                    "      {method:<6} {path}  x{count}{tests}",
                    method = r.method,
                    path = r.path,
                    count = r.count,
                );
            }
        }
    }
    out.push('\n');

    let _ = writeln!(
        out,
        "Note: only traffic addressed via tunnel names is observed. \
         Container-to-container traffic over compose-internal DNS (e.g. http://db:5432 \
         between services on the same compose network) bypasses the daemon and is not \
         shown here."
    );

    out
}

/// A short human description of where a tunnel's backend was discovered.
fn describe_source(source: &ServiceSource, pid: u32) -> String {
    match source {
        ServiceSource::Process { cwd } => match cwd {
            Some(dir) => format!("process pid {pid} ({})", dir.display()),
            None => format!("process pid {pid}"),
        },
        ServiceSource::Container { id, name } => {
            format!("container {name} ({})", &id[..id.len().min(12)])
        }
    }
}
