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

/// GET / — serve the developer status dashboard as a self-contained HTML shell.
/// The page fetches `/status.json` and renders the UI client-side, refreshing every 3 s.
pub async fn status_ui() -> Html<&'static str> {
    Html(STATUS_UI_HTML)
}

const STATUS_UI_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>portzero local</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
body{background:#0d1117;color:#c9d1d9;font-family:ui-monospace,'Courier New',monospace;font-size:13px;line-height:1.6}
a{color:#00ff88;text-decoration:none}
.header{display:flex;align-items:center;gap:10px;padding:10px 16px;border-bottom:1px solid #21262d;background:#161b22}
.logo{font-size:15px;font-weight:700;color:#00ff88;letter-spacing:.05em}
.badge{font-size:10px;font-weight:700;padding:2px 6px;border-radius:3px;background:#b8860b;color:#0d1117;letter-spacing:.08em}
.pid{color:#6e7681;font-size:11px}
.content{padding:16px;max-width:960px}
.section{margin-bottom:24px}
.section-title{color:#8b949e;font-size:11px;letter-spacing:.1em;text-transform:uppercase;margin-bottom:8px;border-bottom:1px solid #21262d;padding-bottom:4px}
table{width:100%;border-collapse:collapse;font-size:12px}
th{text-align:left;color:#8b949e;font-weight:400;padding:4px 8px;border-bottom:1px solid #21262d}
td{padding:4px 8px;border-bottom:1px solid #161b22;word-break:break-all}
tr:hover td{background:#161b22}
.empty{color:#6e7681;font-size:12px;padding:4px 0}
.dot{display:inline-block;width:7px;height:7px;border-radius:50%;margin-right:5px;vertical-align:middle}
.green{background:#3fb950}.red{background:#f85149}.yellow{background:#d29922}
.status-row{display:flex;align-items:center;gap:6px;margin-bottom:6px;font-size:12px}
/* diagnostics */
.diag{margin-bottom:6px;padding:8px 10px;border-radius:4px;border-left:3px solid}
.diag-critical{background:#2d1010;border-color:#f85149}
.diag-error{background:#1f1810;border-color:#d29922}
.diag-warning{background:#1a1a10;border-color:#ffd700}
.diag-info{background:#0d1520;border-color:#388bfd}
.diag-title{font-weight:600;margin-bottom:2px}
.diag-detail{color:#8b949e;font-size:12px;margin-bottom:4px}
.diag-fix{font-size:11px;color:#6e7681}
.diag-fix code{background:#161b22;padding:1px 4px;border-radius:2px;color:#c9d1d9}
.sev-critical{color:#f85149}.sev-error{color:#d29922}.sev-warning{color:#ffd700}.sev-info{color:#388bfd}
.no-issues{color:#3fb950;font-size:12px}
#root .loading{color:#6e7681;padding:32px 0}
.ts{color:#6e7681;font-size:10px;margin-left:auto}
</style>
</head>
<body>
<div class="header">
  <span class="logo">⬡ portzero</span>
  <span class="badge">LOCAL</span>
  <span class="pid" id="hdr-pid"></span>
  <span class="ts" id="hdr-ts"></span>
</div>
<div class="content" id="root"><p class="loading">connecting…</p></div>
<script>
function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;')}
function dot(ok){return '<span class="dot '+(ok?'green':'red')+'"></span>'}

function renderDiag(d){
  const sev=d.severity.toLowerCase();
  let fix='';
  if(d.fix){
    fix='<div class="diag-fix">'+esc(d.fix.description);
    if(d.fix.command) fix+=' — <code>'+esc(d.fix.command)+'</code>';
    fix+='</div>';
  }
  return '<div class="diag diag-'+sev+'"><div class="diag-title"><span class="sev-'+sev+'">['+sev.toUpperCase()+']</span> '+esc(d.title)+'</div>'
    +'<div class="diag-detail">'+esc(d.detail)+'</div>'+fix+'</div>';
}

function render(d){
  document.getElementById('hdr-pid').textContent=d.daemon_pid?'pid '+d.daemon_pid:'';
  document.getElementById('hdr-ts').textContent='updated '+new Date().toLocaleTimeString();

  let html='';

  // diagnostics
  html+='<div class="section"><div class="section-title">diagnostics</div>';
  const diags=d.diagnostics&&d.diagnostics.issues;
  if(!diags||diags.length===0){
    const ran=d.diagnostics?d.diagnostics.checks_run:0;
    html+='<p class="no-issues">✓ all '+(ran||'')+'checks passed</p>';
  } else {
    diags.forEach(function(i){html+=renderDiag(i);});
  }
  html+='</div>';

  // overlay status
  html+='<div class="section"><div class="section-title">local overlay</div>';
  html+='<div class="status-row">'+dot(d.overlay_active)+(d.overlay_active?'active':'inactive')+'</div>';
  if(d.local_services&&d.local_services.length>0){
    html+='<table><thead><tr><th>domain</th><th>real addr</th><th>pid</th></tr></thead><tbody>';
    d.local_services.forEach(function(s){
      html+='<tr><td>'+esc(s.domain)+'</td><td>'+esc(s.real_addr)+'</td><td>'+esc(s.pid)+'</td></tr>';
    });
    html+='</tbody></table>';
  } else {
    html+='<p class="empty">no local services</p>';
  }
  html+='</div>';

  // cloud tunnel
  html+='<div class="section"><div class="section-title">cloud tunnel</div>';
  html+='<div class="status-row">'+dot(d.cloud_connected)+(d.cloud_connected?'connected':'disconnected');
  if(d.cloud_error) html+=' <span style="color:#f85149;font-size:11px">— '+esc(d.cloud_error)+'</span>';
  html+='</div>';
  if(d.cloud_routes&&d.cloud_routes.length>0){
    html+='<table><thead><tr><th>domain</th><th>port</th><th>pid</th></tr></thead><tbody>';
    d.cloud_routes.forEach(function(r){
      html+='<tr><td>'+esc(r.domain)+'</td><td>'+esc(r.port)+'</td><td>'+esc(r.pid)+'</td></tr>';
    });
    html+='</tbody></table>';
  } else {
    html+='<p class="empty">no cloud routes</p>';
  }
  html+='</div>';

  // management registrations
  html+='<div class="section"><div class="section-title">api registrations</div>';
  if(d.management_registrations&&d.management_registrations.length>0){
    html+='<table><thead><tr><th>pid</th><th>local port</th><th>domain</th></tr></thead><tbody>';
    d.management_registrations.forEach(function(r){
      html+='<tr><td>'+esc(r.pid)+'</td><td>'+esc(r.local_port)+'</td><td>'+esc(r.domain)+'</td></tr>';
    });
    html+='</tbody></table>';
  } else {
    html+='<p class="empty">none</p>';
  }
  html+='</div>';

  document.getElementById('root').innerHTML=html;
}

async function load(){
  try{
    const r=await fetch('/status.json');
    if(r.ok) render(await r.json());
  }catch(e){
    document.getElementById('root').innerHTML='<p class="loading">waiting for daemon…</p>';
  }
}
load();
setInterval(load,3000);
</script>
</body>
</html>"#;

/// GET /status.json — machine-readable snapshot of all daemon state.
pub async fn status_json(State(state): State<AppState>) -> Json<serde_json::Value> {
    let daemon_pid = read_daemon_pid(&state.state_dir);
    let overlay = read_overlay_state(&state.state_dir);
    let routes = read_route_table(&state.state_dir);
    let (cloud_connected, cloud_error) = read_cloud_state(&state.state_dir);
    let diagnostics = crate::diagnostics::load_report(&state.state_dir);

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
        "diagnostics": diagnostics,
    }))
}

