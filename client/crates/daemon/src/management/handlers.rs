//! Axum request handlers for the management API and status UI.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::management::pid_lookup;
use crate::management::port_verify;
use crate::management::server::{AppState, PortRegistration};
use crate::route_table::{OverlayState, RouteTable};

// ─── Request / response types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub ports: Vec<PortRegistration>,
}

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pid: u32,
    registered: usize,
}

#[derive(Debug, Serialize)]
pub struct DeregisterResponse {
    pid: u32,
    deregistered: bool,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pid: u32,
    ports: Vec<PortRegistration>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    error: &'static str,
    detail: String,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Resolve the source port from the connecting address to a PID.
/// Returns an error response tuple on failure.
fn resolve_pid(
    addr: &SocketAddr,
) -> Result<u32, (StatusCode, Json<ErrorResponse>)> {
    let source_port = addr.port();
    match pid_lookup::pid_for_source_port(source_port) {
        Some(pid) => Ok(pid),
        None => {
            tracing::warn!("management: could not identify caller on port {}", source_port);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "caller_unidentifiable",
                    detail: format!(
                        "Could not identify the PID for source port {}",
                        source_port
                    ),
                }),
            ))
        }
    }
}

/// Read daemon PID from `{state_dir}/daemon.pid`.  Returns `None` if missing/unreadable.
fn read_daemon_pid(state_dir: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(state_dir.join("daemon.pid"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// Read and parse `overlay.json`.  Returns a default (inactive, empty) state on any error.
fn read_overlay_state(state_dir: &std::path::Path) -> OverlayState {
    OverlayState::load(&state_dir.join("overlay.json")).unwrap_or_default()
}

/// Read and parse `routes.json`.  Returns an empty table on any error.
fn read_route_table(state_dir: &std::path::Path) -> RouteTable {
    RouteTable::load(&state_dir.join("routes.json")).unwrap_or_default()
}

/// Read `cloud_state.json` and extract `connected` + optional `error` fields.
fn read_cloud_state(state_dir: &std::path::Path) -> (bool, Option<String>) {
    let content = match std::fs::read_to_string(state_dir.join("cloud_state.json")) {
        Ok(c) => c,
        Err(_) => return (false, None),
    };
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return (false, None),
    };
    let connected = v.get("connected").and_then(|c| c.as_bool()).unwrap_or(false);
    let error = v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string());
    (connected, error)
}

// ─── Management API handlers ──────────────────────────────────────────────────

/// POST /v1/register — register a list of ports for the calling process.
pub async fn register(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), (StatusCode, Json<ErrorResponse>)> {
    let pid = resolve_pid(&addr)?;

    for reg in &body.ports {
        if !port_verify::pid_is_listening_on(pid, reg.local_port) {
            tracing::warn!(
                "management: PID {} not listening on port {}",
                pid,
                reg.local_port
            );
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: "port_not_listening",
                    detail: format!(
                        "PID {} is not listening on port {}",
                        pid, reg.local_port
                    ),
                }),
            ));
        }
    }

    let count = body.ports.len();
    state.store.write().await.insert(pid, body.ports);
    tracing::debug!("management: registered {} port(s) for PID {}", count, pid);

    Ok((StatusCode::OK, Json(RegisterResponse { pid, registered: count })))
}

/// DELETE /v1/register — remove the registration for the calling process.
pub async fn deregister(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<(StatusCode, Json<DeregisterResponse>), (StatusCode, Json<ErrorResponse>)> {
    let pid = resolve_pid(&addr)?;

    let removed = state.store.write().await.remove(&pid).is_some();
    if !removed {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "not_registered",
                detail: format!("PID {} has no active registration", pid),
            }),
        ));
    }

    tracing::debug!("management: deregistered PID {}", pid);
    Ok((StatusCode::OK, Json(DeregisterResponse { pid, deregistered: true })))
}

/// GET /v1/status — return the current registration for the calling process.
pub async fn status(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<(StatusCode, Json<StatusResponse>), (StatusCode, Json<ErrorResponse>)> {
    let pid = resolve_pid(&addr)?;

    let guard = state.store.read().await;
    match guard.get(&pid) {
        Some(ports) => {
            let ports = ports.clone();
            Ok((StatusCode::OK, Json(StatusResponse { pid, ports })))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "not_registered",
                detail: format!("PID {} has no active registration", pid),
            }),
        )),
    }
}

// ─── Status UI handlers ───────────────────────────────────────────────────────

/// GET / — serve the developer status dashboard as a self-contained HTML page.
pub async fn status_ui(State(state): State<AppState>) -> Html<String> {
    let daemon_pid = read_daemon_pid(&state.state_dir);
    let overlay = read_overlay_state(&state.state_dir);
    let routes = read_route_table(&state.state_dir);
    let (cloud_connected, cloud_error) = read_cloud_state(&state.state_dir);

    let registrations: Vec<(u32, Vec<PortRegistration>)> = {
        let guard = state.store.read().await;
        guard.iter().map(|(pid, regs)| (*pid, regs.clone())).collect()
    };

    // ── header ────────────────────────────────────────────────────────────────
    let pid_badge = match daemon_pid {
        Some(p) => format!(r#"<span class="pid">pid {p}</span>"#),
        None => r#"<span class="pid unavail">pid —</span>"#.to_string(),
    };

    // ── overlay status ────────────────────────────────────────────────────────
    let (overlay_dot, overlay_label) = if overlay.overlay_active {
        (r#"<span class="dot green"></span>"#, "active")
    } else {
        (r#"<span class="dot red"></span>"#, "inactive")
    };

    // ── local services table ──────────────────────────────────────────────────
    let local_services_html = if overlay.routes.is_empty() {
        r#"<p class="empty">no local services</p>"#.to_string()
    } else {
        let rows: String = overlay.routes.iter().map(|r| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                esc(&r.domain), esc(&r.real_addr), r.pid
            )
        }).collect();
        format!(
            "<table><thead><tr><th>domain</th><th>real_addr</th><th>pid</th></tr></thead>\
             <tbody>{rows}</tbody></table>"
        )
    };

    // ── cloud tunnel ──────────────────────────────────────────────────────────
    let (cloud_dot, cloud_label) = if cloud_connected {
        (r#"<span class="dot green"></span>"#, "connected")
    } else {
        (r#"<span class="dot red"></span>"#, "disconnected")
    };
    let cloud_error_html = match &cloud_error {
        Some(e) => format!(r#"<span class="cloud-err"> — {}</span>"#, esc(e)),
        None => String::new(),
    };

    // ── cloud routes table ────────────────────────────────────────────────────
    let cloud_routes_html = if routes.routes.is_empty() {
        r#"<p class="empty">no cloud routes</p>"#.to_string()
    } else {
        let mut sorted: Vec<_> = routes.routes.values().collect();
        sorted.sort_by(|a, b| a.domain.cmp(&b.domain));
        let rows: String = sorted.iter().map(|r| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                esc(&r.domain), r.port, r.pid
            )
        }).collect();
        format!(
            "<table><thead><tr><th>domain</th><th>port</th><th>pid</th></tr></thead>\
             <tbody>{rows}</tbody></table>"
        )
    };

    // ── management registrations table ────────────────────────────────────────
    let mgmt_html = if registrations.is_empty() {
        r#"<p class="empty">none</p>"#.to_string()
    } else {
        let mut rows = String::new();
        for (pid, regs) in &registrations {
            for reg in regs {
                rows.push_str(&format!(
                    "<tr><td>{pid}</td><td>{}</td><td>{}</td></tr>\n",
                    reg.local_port,
                    esc(&reg.domain)
                ));
            }
        }
        format!(
            "<table><thead><tr><th>pid</th><th>local_port</th><th>domain</th></tr></thead>\
             <tbody>{rows}</tbody></table>"
        )
    };

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="refresh" content="5">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>portzero local</title>
<style>
*, *::before, *::after {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{
  background: #0d1117;
  color: #c9d1d9;
  font-family: 'Courier New', Courier, monospace;
  font-size: 13px;
  line-height: 1.5;
}}
a {{ color: #00ff88; text-decoration: none; }}
/* header */
.header {{
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 16px;
  border-bottom: 1px solid #21262d;
  background: #161b22;
}}
.logo {{
  font-size: 16px;
  font-weight: bold;
  color: #00ff88;
  letter-spacing: 0.04em;
}}
.badge {{
  font-size: 10px;
  font-weight: bold;
  padding: 2px 6px;
  border-radius: 3px;
  background: #b8860b;
  color: #0d1117;
  letter-spacing: 0.06em;
}}
.pid {{
  font-size: 11px;
  color: #6e7681;
}}
.pid.unavail {{ color: #444; }}
/* main layout */
.main {{
  padding: 16px;
  max-width: 960px;
}}
/* sections */
.section {{
  margin-bottom: 20px;
}}
.section-title {{
  font-size: 11px;
  font-weight: bold;
  color: #00ff88;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  border-bottom: 1px solid #21262d;
  padding-bottom: 4px;
  margin-bottom: 8px;
}}
/* status line */
.status-line {{
  display: flex;
  align-items: center;
  gap: 6px;
}}
/* dot indicators */
.dot {{
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
}}
.dot.green {{ background: #00ff88; box-shadow: 0 0 4px #00ff88; }}
.dot.red   {{ background: #f85149; box-shadow: 0 0 4px #f85149; }}
/* tables */
table {{
  width: 100%;
  border-collapse: collapse;
  font-size: 12px;
}}
th {{
  text-align: left;
  color: #6e7681;
  font-weight: normal;
  padding: 3px 8px 3px 0;
  border-bottom: 1px solid #21262d;
}}
td {{
  padding: 3px 8px 3px 0;
  border-bottom: 1px solid #161b22;
  color: #c9d1d9;
}}
tr:last-child td {{ border-bottom: none; }}
/* misc */
.empty {{ color: #6e7681; font-size: 12px; }}
.cloud-err {{ color: #f85149; }}
.refresh-note {{
  font-size: 10px;
  color: #444;
  margin-top: 24px;
}}
</style>
</head>
<body>
<div class="header">
  <span class="logo">&#x2B22; portzero local</span>
  <span class="badge">LOCAL</span>
  {pid_badge}
</div>
<div class="main">

  <div class="section">
    <div class="section-title">overlay</div>
    <div class="status-line">{overlay_dot}<span>overlay: {overlay_label}</span></div>
  </div>

  <div class="section">
    <div class="section-title">local services</div>
    {local_services_html}
  </div>

  <div class="section">
    <div class="section-title">cloud tunnel</div>
    <div class="status-line">{cloud_dot}<span>{cloud_label}{cloud_error_html}</span></div>
  </div>

  <div class="section">
    <div class="section-title">cloud routes</div>
    {cloud_routes_html}
  </div>

  <div class="section">
    <div class="section-title">management registrations</div>
    {mgmt_html}
  </div>

  <p class="refresh-note">auto-refreshes every 5 s</p>
</div>
</body>
</html>
"#
    );

    Html(html)
}

/// GET /status.json — machine-readable snapshot of all daemon state.
pub async fn status_json(State(state): State<AppState>) -> Json<serde_json::Value> {
    let daemon_pid = read_daemon_pid(&state.state_dir);
    let overlay = read_overlay_state(&state.state_dir);
    let routes = read_route_table(&state.state_dir);
    let (cloud_connected, cloud_error) = read_cloud_state(&state.state_dir);

    let registrations_guard = state.store.read().await;
    let management_registrations: Vec<serde_json::Value> = registrations_guard
        .iter()
        .flat_map(|(pid, regs)| {
            regs.iter().map(move |r| {
                serde_json::json!({
                    "pid": pid,
                    "local_port": r.local_port,
                    "domain": r.domain,
                })
            })
        })
        .collect();

    let local_services: Vec<serde_json::Value> = overlay
        .routes
        .iter()
        .map(|r| {
            serde_json::json!({
                "domain": r.domain,
                "real_addr": r.real_addr,
                "pid": r.pid,
            })
        })
        .collect();

    let cloud_routes: Vec<serde_json::Value> = routes
        .routes
        .values()
        .map(|r| {
            serde_json::json!({
                "domain": r.domain,
                "port": r.port,
                "pid": r.pid,
            })
        })
        .collect();

    Json(serde_json::json!({
        "daemon_pid": daemon_pid,
        "overlay_active": overlay.overlay_active,
        "local_services": local_services,
        "cloud_connected": cloud_connected,
        "cloud_error": cloud_error,
        "cloud_routes": cloud_routes,
        "management_registrations": management_registrations,
    }))
}

// ─── HTML escaping ────────────────────────────────────────────────────────────

/// Minimal HTML entity escaping for untrusted strings rendered into HTML.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
