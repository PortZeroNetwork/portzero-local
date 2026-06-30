//! Axum request handlers for the management API and status UI.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::Redirect;
use axum::response::{Html, IntoResponse};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::management::pid_lookup;
use crate::management::port_verify;
use crate::management::server::{AppState, PortRegistration};
use crate::route_table::{OverlayState, RouteTable};

const PORTZERO_MARK_JPEG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/management/assets/portzero-mark.jpg"
));
const PORTZERO_WORDMARK_JPEG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/management/assets/portzero-wordmark.jpg"
));

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
fn resolve_pid(addr: &SocketAddr) -> Result<u32, (StatusCode, Json<ErrorResponse>)> {
    let source_port = addr.port();
    match pid_lookup::pid_for_source_port(source_port) {
        Some(pid) => Ok(pid),
        None => {
            tracing::warn!(
                "management: could not identify caller on port {}",
                source_port
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "caller_unidentifiable",
                    detail: format!("Could not identify the PID for source port {}", source_port),
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
    let connected = v
        .get("connected")
        .and_then(|c| c.as_bool())
        .unwrap_or(false);
    let error = v
        .get("error")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());
    (connected, error)
}

fn command_on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            dir.join(format!("{program}.exe")).is_file()
        }
        #[cfg(not(windows))]
        {
            false
        }
    })
}

fn detected_languages() -> serde_json::Value {
    let languages = [
        ("typescript", "TypeScript", &["node", "nodejs", "npm"][..]),
        ("javascript", "JavaScript", &["node", "nodejs"][..]),
        ("python", "Python", &["python3", "python"][..]),
        ("java", "Java", &["java", "javac"][..]),
        ("go", "Go", &["go"][..]),
        ("rust", "Rust", &["cargo", "rustc"][..]),
        ("csharp", "C#", &["dotnet"][..]),
        ("php", "PHP", &["php", "composer"][..]),
        ("ruby", "Ruby", &["ruby", "bundle"][..]),
        (
            "cpp",
            "C/C++",
            &["cc", "gcc", "clang", "g++", "clang++"][..],
        ),
        ("swift", "Swift", &["swift"][..]),
        ("kotlin", "Kotlin", &["kotlin", "kotlinc"][..]),
        ("dart", "Dart", &["dart"][..]),
        ("elixir", "Elixir", &["elixir", "mix"][..]),
        ("scala", "Scala", &["scala", "sbt"][..]),
        ("r", "R", &["R", "Rscript"][..]),
        ("julia", "Julia", &["julia"][..]),
        ("lua", "Lua", &["lua", "luajit"][..]),
        ("perl", "Perl", &["perl"][..]),
        ("zig", "Zig", &["zig"][..]),
        ("haskell", "Haskell", &["ghc", "cabal", "stack"][..]),
        ("shell", "Shell", &["bash", "sh"][..]),
    ];

    let detected: Vec<serde_json::Value> = languages
        .iter()
        .map(|(id, label, tools)| {
            let installed_tools: Vec<&str> = tools
                .iter()
                .copied()
                .filter(|tool| command_on_path(tool))
                .collect();
            serde_json::json!({
                "id": id,
                "label": label,
                "installed": !installed_tools.is_empty(),
                "tools": installed_tools,
            })
        })
        .collect();

    let default_language = detected
        .iter()
        .find(|lang| {
            lang.get("id").and_then(|v| v.as_str()) != Some("shell")
                && lang
                    .get("installed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
        .and_then(|lang| lang.get("id").and_then(|v| v.as_str()))
        .unwrap_or("typescript");

    serde_json::json!({
        "default": default_language,
        "items": detected,
    })
}

fn display_substitutions(
    substitutions: &std::collections::BTreeMap<String, String>,
    source: &crate::discovery::ServiceSource,
) -> serde_json::Value {
    let mut values = substitutions.clone();
    if !values.contains_key("folder-name") {
        let folder_name = match source {
            crate::discovery::ServiceSource::Process { cwd } => cwd
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            crate::discovery::ServiceSource::Container { name, .. } => name.clone(),
        };
        values.insert("folder-name".to_string(), folder_name);
    }
    serde_json::json!(values)
}

fn local_service_link_url(domain: &str, service_port: u16) -> Option<String> {
    match service_port {
        80 | 443 => Some(format!("https://{domain}")),
        _ => None,
    }
}

fn has_dns_token(template: &str, value: &str) -> bool {
    if value.is_empty() {
        return false;
    }

    template.match_indices(value).any(|(start, _)| {
        let end = start + value.len();
        let before_is_boundary = template[..start]
            .chars()
            .next_back()
            .map(|c| !c.is_ascii_alphanumeric())
            .unwrap_or(true);
        let after_is_boundary = template[end..]
            .chars()
            .next()
            .map(|c| !c.is_ascii_alphanumeric())
            .unwrap_or(true);
        before_is_boundary && after_is_boundary
    })
}

fn substitution_alerts(
    domain_template: &str,
    substitutions: &std::collections::BTreeMap<String, String>,
) -> Vec<serde_json::Value> {
    const SUGGESTED_KEYS: &[&str] = &[
        "branch",
        "worktree",
        "project",
        "user",
        "machine",
        "uid",
        "username",
        "folder-name",
    ];

    let mut alerts = Vec::new();

    if domain_template.contains("{branch}")
        && substitutions.get("branch").map(String::as_str) == Some("unknown")
    {
        alerts.push(serde_json::json!({
            "severity": "info",
            "title": "Could not determine {branch}",
            "detail": "PZ_TUNNEL uses {branch}, but the daemon could not determine the current git branch, so it materialized the value as \"unknown\". Run the service from a git worktree with a checked-out branch, or use a literal tunnel name."
        }));
    }

    alerts.extend(SUGGESTED_KEYS
        .iter()
        .filter_map(|key| {
            let placeholder = format!("{{{key}}}");
            if domain_template.contains(&placeholder) {
                return None;
            }

            let value = substitutions.get(*key)?;
            if !has_dns_token(domain_template, value) {
                return None;
            }

            Some(serde_json::json!({
                "severity": "info",
                "title": format!("Suggest replacing \"{value}\" with \"{placeholder}\""),
                "detail": format!(
                    "PZ_TUNNEL supports variable substitution: write {placeholder} in the value and the daemon materializes it as \"{value}\" for this process. This keeps tunnel names accurate when the branch, worktree, user, or machine changes."
                ),
            }))
        }));

    alerts
}

fn duplicate_route_alert(domain: &str, duplicate_count: usize) -> serde_json::Value {
    serde_json::json!({
        "severity": "warning",
        "title": "Duplicate tunnel domain",
        "detail": format!(
            "{duplicate_count} tunnels materialized to {domain}. Change PZ_TUNNEL so each active tunnel has a unique domain."
        ),
    })
}

fn append_alert(row: &mut serde_json::Value, alert: serde_json::Value) {
    row.get_mut("alerts")
        .and_then(|alerts| alerts.as_array_mut())
        .expect("route rows always include an alerts array")
        .push(alert);
}

fn add_duplicate_route_alerts(
    local_services: &mut [serde_json::Value],
    cloud_routes: &mut [serde_json::Value],
) {
    #[derive(Clone, Copy)]
    enum RouteList {
        Local,
        Cloud,
    }

    let mut routes = Vec::new();
    for (index, row) in local_services.iter().enumerate() {
        if let Some(domain) = row.get("domain").and_then(|v| v.as_str()) {
            routes.push((RouteList::Local, index, domain.to_string()));
        }
    }
    for (index, row) in cloud_routes.iter().enumerate() {
        if let Some(domain) = row.get("domain").and_then(|v| v.as_str()) {
            routes.push((RouteList::Cloud, index, domain.to_string()));
        }
    }

    let mut counts = std::collections::HashMap::new();
    for (_, _, domain) in &routes {
        *counts.entry(domain.clone()).or_insert(0usize) += 1;
    }

    for (list, index, domain) in routes {
        let duplicate_count = counts.get(&domain).copied().unwrap_or(0);
        if duplicate_count < 2 {
            continue;
        }

        let alert = duplicate_route_alert(&domain, duplicate_count);
        match list {
            RouteList::Local => append_alert(&mut local_services[index], alert),
            RouteList::Cloud => append_alert(&mut cloud_routes[index], alert),
        }
    }
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
                    detail: format!("PID {} is not listening on port {}", pid, reg.local_port),
                }),
            ));
        }
    }

    let count = body.ports.len();
    state.store.write().await.insert(pid, body.ports);
    tracing::debug!("management: registered {} port(s) for PID {}", count, pid);

    Ok((
        StatusCode::OK,
        Json(RegisterResponse {
            pid,
            registered: count,
        }),
    ))
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
    Ok((
        StatusCode::OK,
        Json(DeregisterResponse {
            pid,
            deregistered: true,
        }),
    ))
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

/// GET /login — start browser login and redirect this tab to the cloud auth UI.
pub async fn start_login() -> impl IntoResponse {
    match crate::auth::start_browser_login_session().await {
        Ok(session) => {
            tracing::info!(
                callback_port = session.callback_port,
                "started dashboard browser login"
            );
            Redirect::temporary(&session.auth_url).into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to start dashboard browser login");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to start login flow: {error}"),
            )
                .into_response()
        }
    }
}

/// GET /assets/portzero-mark.jpg — favicon and compact brand mark.
pub async fn portzero_mark_asset() -> impl IntoResponse {
    ([("content-type", "image/jpeg")], PORTZERO_MARK_JPEG)
}

/// GET /assets/portzero-wordmark.jpg — full PortZero wordmark.
pub async fn portzero_wordmark_asset() -> impl IntoResponse {
    ([("content-type", "image/jpeg")], PORTZERO_WORDMARK_JPEG)
}

/// GET /openapi.json — read-only OpenAPI document for local process clients.
pub async fn openapi_json() -> impl IntoResponse {
    ([("content-type", "application/json")], OPENAPI_SPEC_JSON)
}

const OPENAPI_SPEC_JSON: &str = r##"{
  "openapi": "3.1.0",
  "info": {
    "title": "PortZero Local Management API",
    "version": "1.0.0",
    "description": "Local process API served by the PortZero daemon. This API is intended for local processes that are listening on ports, not for browser-origin request forwarding."
  },
  "servers": [
    {
      "url": "http://api.portzero.local",
      "description": "PortZero local management API"
    }
  ],
  "paths": {
    "/v1/register": {
      "post": {
        "summary": "Register local ports for the calling process",
        "description": "Registers one or more local port/domain mappings for the process that opened the TCP connection. Each call replaces any prior registration for that process.",
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/RegisterRequest"
              }
            }
          }
        },
        "responses": {
          "200": {
            "description": "Ports registered",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/RegisterResponse"
                }
              }
            }
          },
          "422": {
            "description": "The caller is not listening on a requested port",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          },
          "500": {
            "description": "The daemon could not identify the calling process",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          }
        }
      },
      "delete": {
        "summary": "Remove registrations for the calling process",
        "responses": {
          "200": {
            "description": "Registration removed",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/DeregisterResponse"
                }
              }
            }
          },
          "404": {
            "description": "No active registration exists for the caller",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          },
          "500": {
            "description": "The daemon could not identify the calling process",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          }
        }
      }
    },
    "/v1/status": {
      "get": {
        "summary": "Return registration state for the calling process",
        "responses": {
          "200": {
            "description": "Current registration for the caller",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/StatusResponse"
                }
              }
            }
          },
          "404": {
            "description": "No active registration exists for the caller",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          },
          "500": {
            "description": "The daemon could not identify the calling process",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "PortRegistration": {
        "type": "object",
        "required": ["local_port", "domain"],
        "properties": {
          "local_port": {
            "type": "integer",
            "minimum": 1,
            "maximum": 65535,
            "example": 3000
          },
          "domain": {
            "type": "string",
            "example": "web.portzero.local"
          }
        }
      },
      "RegisterRequest": {
        "type": "object",
        "required": ["ports"],
        "properties": {
          "ports": {
            "type": "array",
            "items": {
              "$ref": "#/components/schemas/PortRegistration"
            }
          }
        }
      },
      "RegisterResponse": {
        "type": "object",
        "required": ["pid", "registered"],
        "properties": {
          "pid": {
            "type": "integer",
            "format": "uint32"
          },
          "registered": {
            "type": "integer"
          }
        }
      },
      "DeregisterResponse": {
        "type": "object",
        "required": ["pid", "deregistered"],
        "properties": {
          "pid": {
            "type": "integer",
            "format": "uint32"
          },
          "deregistered": {
            "type": "boolean"
          }
        }
      },
      "StatusResponse": {
        "type": "object",
        "required": ["pid", "ports"],
        "properties": {
          "pid": {
            "type": "integer",
            "format": "uint32"
          },
          "ports": {
            "type": "array",
            "items": {
              "$ref": "#/components/schemas/PortRegistration"
            }
          }
        }
      },
      "ErrorResponse": {
        "type": "object",
        "required": ["error", "detail"],
        "properties": {
          "error": {
            "type": "string",
            "enum": ["caller_unidentifiable", "port_not_listening", "not_registered"]
          },
          "detail": {
            "type": "string"
          }
        }
      }
    }
  }
}"##;

const STATUS_UI_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>PortZero Local</title>
<link rel="icon" type="image/jpeg" href="/assets/portzero-mark.jpg">
<link rel="apple-touch-icon" href="/assets/portzero-mark.jpg">
<style>
:root{color-scheme:light dark;--bg:#fdfdfd;--fg:#1f2328;--muted:#6b7280;--soft:#f3f4f6;--line:#e5e7eb;--accent:#2563eb;--accent-soft:#dbeafe;--ok:#16833a;--warn:#a16207;--bad:#c2410c;--code:#111827;--code-fg:#f9fafb}
@media (prefers-color-scheme:dark){:root{--bg:#0f1115;--fg:#e5e7eb;--muted:#9ca3af;--soft:#171a21;--line:#2b303b;--accent:#7dd3fc;--accent-soft:#102a3a;--ok:#86efac;--warn:#facc15;--bad:#fb923c;--code:#05070a;--code-fg:#f3f4f6}}
*,*::before,*::after{box-sizing:border-box}
html{scroll-behavior:smooth}
body{margin:0;background:var(--bg);color:var(--fg);font-family:ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;font-size:16px;line-height:1.65}
a{color:inherit;text-decoration:none}
a:hover{color:var(--accent)}
code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
.shell{width:min(100% - 32px,1120px);margin:0 auto}
.topbar{position:sticky;top:0;z-index:3;background:color-mix(in srgb,var(--bg) 90%,transparent);backdrop-filter:blur(12px);border-bottom:1px solid var(--line)}
.nav{display:flex;align-items:center;gap:18px;min-height:62px}
.brand{display:inline-flex;align-items:center;gap:12px;font-size:18px;font-weight:800}
.brand img{display:block;height:28px;width:auto}
.navlinks{display:flex;gap:14px;margin-left:auto;color:var(--muted);font-size:14px}
.meta{color:var(--muted);font-size:13px;white-space:nowrap}
.hero{padding:58px 0 24px;display:grid;gap:18px}
.hero-brand{max-width:620px;width:100%;height:auto;display:block}
.eyebrow{color:var(--accent);font-size:13px;font-weight:700;margin:0 0 10px}
h1{font-size:clamp(34px,6vw,64px);line-height:1.05;margin:0 0 18px;max-width:760px}
.lede{max-width:680px;color:var(--muted);font-size:19px;margin:0}
.section{padding:30px 0;border-top:1px solid var(--line)}
.section-head{display:flex;align-items:end;gap:16px;justify-content:space-between;margin-bottom:18px}
h2{font-size:28px;line-height:1.2;margin:0}
.section-note{color:var(--muted);font-size:14px;margin:0}
.steps{display:grid;gap:12px}
.step{display:grid;grid-template-columns:40px minmax(0,1fr);gap:14px;padding:18px 0;border-bottom:1px solid var(--line)}
.step:last-child{border-bottom:0}
.num{display:grid;place-items:center;width:32px;height:32px;border-radius:50%;background:var(--accent-soft);color:var(--accent);font-weight:800;font-size:14px}
.step h3,.example h3{font-size:18px;margin:0 0 8px}
.placeholder{color:var(--muted);margin:0}
.code-row{margin-top:12px;background:var(--code);color:var(--code-fg);border-radius:8px;padding:12px 14px;overflow:auto;font-size:14px}
.examples{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:14px}
.example{border-top:2px solid var(--line);padding-top:14px}
.tag{display:inline-flex;align-items:center;border:1px solid var(--line);border-radius:999px;padding:2px 9px;color:var(--muted);font-size:12px;margin-bottom:10px}
.picker{display:flex;align-items:center;gap:10px;flex-wrap:wrap}
.picker select{appearance:none;border:1px solid var(--line);background:var(--bg);color:var(--fg);border-radius:8px;padding:8px 34px 8px 10px;font:inherit;font-size:14px}
.detected{color:var(--muted);font-size:13px}
.status-grid{display:grid;grid-template-columns:minmax(0,1fr);gap:28px}
.status-card{min-width:0}
.status-row{display:flex;align-items:center;gap:8px;color:var(--muted);font-size:14px;margin-bottom:12px}
.dot{width:9px;height:9px;border-radius:50%;background:var(--bad);flex:0 0 auto}
.dot.ok{background:var(--ok)}
.dot.warn{background:var(--warn)}
table{width:100%;border-collapse:collapse;font-size:14px}
th{text-align:left;color:var(--muted);font-weight:600;border-bottom:1px solid var(--line);padding:8px 6px}
td{border-bottom:1px solid var(--line);padding:8px 6px;vertical-align:top;word-break:break-word}
.domain-stack{display:grid;gap:2px}
.domain-stack code{font-size:13px}
.route-alerts{display:grid;gap:6px;margin-top:8px}
.route-alert{border-left:3px solid var(--warn);background:var(--soft);padding:7px 9px;font-size:13px}
.route-alert.info{border-left-color:var(--accent)}
.route-alert.warning{border-left-color:var(--warn)}
.route-alert-title{font-weight:700}
.route-alert-detail{color:var(--muted)}
.substitutions{display:flex;gap:6px;flex-wrap:wrap}
.substitutions code{background:var(--soft);border-radius:999px;padding:2px 7px;color:var(--fg);font-size:12px}
.empty{color:var(--muted);font-size:14px;margin:0}
.empty-action{display:flex;align-items:center;gap:12px;flex-wrap:wrap;color:var(--muted);font-size:14px}
.empty-action p{margin:0}
.button{display:inline-flex;align-items:center;justify-content:center;min-height:36px;border-radius:8px;background:var(--accent);color:var(--bg);font-weight:700;padding:6px 12px}
.button:hover{color:var(--bg)}
.diag{border-left:3px solid var(--line);padding:10px 0 10px 14px;margin-bottom:10px}
.diag-critical,.diag-error{border-color:var(--bad)}
.diag-warning{border-color:var(--warn)}
.diag-info{border-color:var(--accent)}
.diag-title{font-weight:700}
.diag-detail,.diag-fix{color:var(--muted);font-size:14px}
.diag-fix code{background:var(--soft);color:var(--fg);padding:1px 5px;border-radius:4px}
.no-issues{color:var(--ok);font-size:14px;margin:0}
.loading{color:var(--muted);padding:32px 0}
.endpoint-grid{display:grid;grid-template-columns:minmax(0,.9fr) minmax(0,1.1fr);gap:22px;align-items:start}
.endpoint-list{display:grid;gap:12px}
.endpoint{border-top:1px solid var(--line);padding-top:12px}
.endpoint code{display:block;color:var(--fg);font-size:15px;margin-top:4px;word-break:break-word}
.method{display:inline-flex;align-items:center;border-radius:4px;padding:1px 6px;background:var(--soft);font-weight:800;font-size:12px;color:var(--accent);margin-right:6px}
.api-note{color:var(--muted);margin:0 0 14px}
.spec-block{max-height:520px;overflow:auto;background:var(--code);color:var(--code-fg);border-radius:8px;padding:14px;font-size:12px;line-height:1.5}
.spec-block code{white-space:pre}
@media (prefers-reduced-motion:no-preference){.hero-brand,.brand img{animation:fade-in .45s ease-out both}}
@keyframes fade-in{from{opacity:0;transform:translateY(6px)}to{opacity:1;transform:translateY(0)}}
@media (max-width:760px){.nav{align-items:flex-start;flex-direction:column;gap:6px;padding:12px 0}.navlinks{margin-left:0;flex-wrap:wrap}.hero{padding-top:36px}.examples,.status-grid{grid-template-columns:1fr}.section-head{display:block}.step{grid-template-columns:1fr}.num{margin-bottom:4px}}
@media (max-width:860px){.endpoint-grid{grid-template-columns:1fr}}
</style>
</head>
<body>
<header class="topbar">
  <nav class="shell nav" aria-label="Main">
    <a class="brand" href="/">
      <img src="/assets/portzero-mark.jpg" alt="" aria-hidden="true">
      <span>PortZero</span>
    </a>
    <div class="navlinks">
      <a href="#getting-started">Getting Started</a>
      <a href="#api">API</a>
      <a href="#examples">Examples</a>
      <a href="#tunnels">Tunnels</a>
      <a href="#diagnostics">Diagnostics</a>
    </div>
    <span class="meta" id="hdr-meta"></span>
  </nav>
</header>
<main class="shell">
  <section class="hero" id="getting-started">
    <img class="hero-brand" src="/assets/portzero-wordmark.jpg" alt="PortZero">
    <p class="eyebrow">Local developer guide</p>
    <h1>Getting Started</h1>
    <p class="lede">Use <code>portzero.local</code> for the local dashboard. Local processes should call the management API at <code>http://api.portzero.local</code>.</p>
  </section>

  <section class="section" aria-labelledby="quickstart-title">
    <div class="section-head">
      <h2 id="quickstart-title">Quickstart</h2>
      <p class="section-note">Short steps first. Details can live below.</p>
    </div>
    <div class="steps">
      <article class="step">
        <span class="num">1</span>
        <div>
          <h3>Run something locally</h3>
          <p class="placeholder">TODO: one-liner for starting a tiny local service.</p>
          <pre class="code-row"><code>[TODO: command]</code></pre>
        </div>
      </article>
      <article class="step">
        <span class="num">2</span>
        <div>
          <h3>Point a .local name at it</h3>
          <p class="placeholder">TODO: one-liner for exposing that service through portzero.local DNS.</p>
          <pre class="code-row"><code>[TODO: command]</code></pre>
        </div>
      </article>
      <article class="step">
        <span class="num">3</span>
        <div>
          <h3>Open the local URL</h3>
          <p class="placeholder">TODO: final URL/check command.</p>
          <pre class="code-row"><code>[TODO: command]</code></pre>
        </div>
      </article>
    </div>
  </section>

  <section class="section" id="api" aria-labelledby="api-title">
    <div class="section-head">
      <h2 id="api-title">Local API</h2>
      <p class="section-note">For local processes only. This page does not send API requests.</p>
    </div>
    <div class="endpoint-grid">
      <div>
        <p class="api-note">Endpoint base URL:</p>
        <pre class="code-row"><code>http://api.portzero.local</code></pre>
        <div class="endpoint-list" aria-label="API routes">
          <div class="endpoint">
            <span class="method">POST</span><strong>Register ports</strong>
            <code>/v1/register</code>
          </div>
          <div class="endpoint">
            <span class="method">DELETE</span><strong>Deregister ports</strong>
            <code>/v1/register</code>
          </div>
          <div class="endpoint">
            <span class="method">GET</span><strong>Process registration status</strong>
            <code>/v1/status</code>
          </div>
        </div>
      </div>
      <div>
        <p class="api-note">OpenAPI 3.1 spec. The raw document is also available at <code>/openapi.json</code>.</p>
        <pre class="spec-block" aria-label="OpenAPI specification"><code>{
  "openapi": "3.1.0",
  "info": {
    "title": "PortZero Local Management API",
    "version": "1.0.0",
    "description": "Local process API served by the PortZero daemon. This API is intended for local processes that are listening on ports, not for browser-origin request forwarding."
  },
  "servers": [
    {
      "url": "http://api.portzero.local",
      "description": "PortZero local management API"
    }
  ],
  "paths": {
    "/v1/register": {
      "post": {
        "summary": "Register local ports for the calling process",
        "description": "Registers one or more local port/domain mappings for the process that opened the TCP connection. Each call replaces any prior registration for that process.",
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/RegisterRequest"
              }
            }
          }
        },
        "responses": {
          "200": {
            "description": "Ports registered",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/RegisterResponse"
                }
              }
            }
          },
          "422": {
            "description": "The caller is not listening on a requested port",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          },
          "500": {
            "description": "The daemon could not identify the calling process",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          }
        }
      },
      "delete": {
        "summary": "Remove registrations for the calling process",
        "responses": {
          "200": {
            "description": "Registration removed",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/DeregisterResponse"
                }
              }
            }
          },
          "404": {
            "description": "No active registration exists for the caller",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          },
          "500": {
            "description": "The daemon could not identify the calling process",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          }
        }
      }
    },
    "/v1/status": {
      "get": {
        "summary": "Return registration state for the calling process",
        "responses": {
          "200": {
            "description": "Current registration for the caller",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/StatusResponse"
                }
              }
            }
          },
          "404": {
            "description": "No active registration exists for the caller",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          },
          "500": {
            "description": "The daemon could not identify the calling process",
            "content": {
              "application/json": {
                "schema": {
                  "$ref": "#/components/schemas/ErrorResponse"
                }
              }
            }
          }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "PortRegistration": {
        "type": "object",
        "required": ["local_port", "domain"],
        "properties": {
          "local_port": {
            "type": "integer",
            "minimum": 1,
            "maximum": 65535,
            "example": 3000
          },
          "domain": {
            "type": "string",
            "example": "web.portzero.local"
          }
        }
      },
      "RegisterRequest": {
        "type": "object",
        "required": ["ports"],
        "properties": {
          "ports": {
            "type": "array",
            "items": {
              "$ref": "#/components/schemas/PortRegistration"
            }
          }
        }
      },
      "RegisterResponse": {
        "type": "object",
        "required": ["pid", "registered"],
        "properties": {
          "pid": {
            "type": "integer",
            "format": "uint32"
          },
          "registered": {
            "type": "integer"
          }
        }
      },
      "DeregisterResponse": {
        "type": "object",
        "required": ["pid", "deregistered"],
        "properties": {
          "pid": {
            "type": "integer",
            "format": "uint32"
          },
          "deregistered": {
            "type": "boolean"
          }
        }
      },
      "StatusResponse": {
        "type": "object",
        "required": ["pid", "ports"],
        "properties": {
          "pid": {
            "type": "integer",
            "format": "uint32"
          },
          "ports": {
            "type": "array",
            "items": {
              "$ref": "#/components/schemas/PortRegistration"
            }
          }
        }
      },
      "ErrorResponse": {
        "type": "object",
        "required": ["error", "detail"],
        "properties": {
          "error": {
            "type": "string",
            "enum": ["caller_unidentifiable", "port_not_listening", "not_registered"]
          },
          "detail": {
            "type": "string"
          }
        }
      }
    }
  }
}</code></pre>
      </div>
    </div>
  </section>

  <section class="section" id="examples" aria-labelledby="examples-title">
    <div class="section-head">
      <h2 id="examples-title">Examples</h2>
      <div class="picker">
        <label class="section-note" for="language-picker">Language</label>
        <select id="language-picker"></select>
        <span class="detected" id="language-detected"></span>
      </div>
    </div>
    <div class="examples" id="language-examples"></div>
  </section>

  <section class="section" id="tunnels" aria-labelledby="tunnels-title">
    <div class="section-head">
      <h2 id="tunnels-title">Tunnels</h2>
      <p class="section-note" id="hdr-ts"></p>
    </div>
    <div id="status-root"><p class="loading">Connecting to daemon...</p></div>
  </section>
</main>
<script>
const LANGUAGE_EXAMPLES={
  typescript:[['Run a server','TODO: TypeScript one-liner that starts a local service.'],['Expose it','TODO: TypeScript command with PZ_TUNNEL.'],['Download example','TODO: TypeScript downloadable example link.']],
  javascript:[['Run a server','TODO: JavaScript one-liner that starts a local service.'],['Expose it','TODO: JavaScript command with PZ_TUNNEL.'],['Download example','TODO: JavaScript downloadable example link.']],
  python:[['Run a server','TODO: Python one-liner that starts a local service.'],['Expose it','TODO: Python command with PZ_TUNNEL.'],['Download example','TODO: Python downloadable example link.']],
  java:[['Run a server','TODO: Java one-liner that starts a local service.'],['Expose it','TODO: Java command with PZ_TUNNEL.'],['Download example','TODO: Java downloadable example link.']],
  go:[['Run a server','TODO: Go one-liner that starts a local service.'],['Expose it','TODO: Go command with PZ_TUNNEL.'],['Download example','TODO: Go downloadable example link.']],
  rust:[['Run a server','TODO: Rust one-liner that starts a local service.'],['Expose it','TODO: Rust command with PZ_TUNNEL.'],['Download example','TODO: Rust downloadable example link.']],
  csharp:[['Run a server','TODO: C# one-liner that starts a local service.'],['Expose it','TODO: C# command with PZ_TUNNEL.'],['Download example','TODO: C# downloadable example link.']],
  php:[['Run a server','TODO: PHP one-liner that starts a local service.'],['Expose it','TODO: PHP command with PZ_TUNNEL.'],['Download example','TODO: PHP downloadable example link.']],
  ruby:[['Run a server','TODO: Ruby one-liner that starts a local service.'],['Expose it','TODO: Ruby command with PZ_TUNNEL.'],['Download example','TODO: Ruby downloadable example link.']],
  cpp:[['Run a server','TODO: C/C++ one-liner that starts a local service.'],['Expose it','TODO: C/C++ command with PZ_TUNNEL.'],['Download example','TODO: C/C++ downloadable example link.']],
  swift:[['Run a server','TODO: Swift one-liner that starts a local service.'],['Expose it','TODO: Swift command with PZ_TUNNEL.'],['Download example','TODO: Swift downloadable example link.']],
  kotlin:[['Run a server','TODO: Kotlin one-liner that starts a local service.'],['Expose it','TODO: Kotlin command with PZ_TUNNEL.'],['Download example','TODO: Kotlin downloadable example link.']],
  dart:[['Run a server','TODO: Dart one-liner that starts a local service.'],['Expose it','TODO: Dart command with PZ_TUNNEL.'],['Download example','TODO: Dart downloadable example link.']],
  elixir:[['Run a server','TODO: Elixir one-liner that starts a local service.'],['Expose it','TODO: Elixir command with PZ_TUNNEL.'],['Download example','TODO: Elixir downloadable example link.']],
  scala:[['Run a server','TODO: Scala one-liner that starts a local service.'],['Expose it','TODO: Scala command with PZ_TUNNEL.'],['Download example','TODO: Scala downloadable example link.']],
  r:[['Run a server','TODO: R one-liner that starts a local service.'],['Expose it','TODO: R command with PZ_TUNNEL.'],['Download example','TODO: R downloadable example link.']],
  julia:[['Run a server','TODO: Julia one-liner that starts a local service.'],['Expose it','TODO: Julia command with PZ_TUNNEL.'],['Download example','TODO: Julia downloadable example link.']],
  lua:[['Run a server','TODO: Lua one-liner that starts a local service.'],['Expose it','TODO: Lua command with PZ_TUNNEL.'],['Download example','TODO: Lua downloadable example link.']],
  perl:[['Run a server','TODO: Perl one-liner that starts a local service.'],['Expose it','TODO: Perl command with PZ_TUNNEL.'],['Download example','TODO: Perl downloadable example link.']],
  zig:[['Run a server','TODO: Zig one-liner that starts a local service.'],['Expose it','TODO: Zig command with PZ_TUNNEL.'],['Download example','TODO: Zig downloadable example link.']],
  haskell:[['Run a server','TODO: Haskell one-liner that starts a local service.'],['Expose it','TODO: Haskell command with PZ_TUNNEL.'],['Download example','TODO: Haskell downloadable example link.']],
  shell:[['Run a server','TODO: shell one-liner that starts a local service.'],['Expose it','TODO: shell command with PZ_TUNNEL.'],['Download example','TODO: shell downloadable example link.']]
};
let selectedLanguage=null;
let languagePickerReady=false;
function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;')}
function dot(ok){return '<span class="dot '+(ok?'ok':'')+'"></span>'}
function renderLanguageExamples(id){
  const picker=document.getElementById('language-picker');
  if(picker&&picker.value!==id) picker.value=id;
  selectedLanguage=id;
  const examples=LANGUAGE_EXAMPLES[id]||LANGUAGE_EXAMPLES.typescript;
  document.getElementById('language-examples').innerHTML=examples.map(function(ex){
    return '<article class="example"><span class="tag">'+esc(id)+'</span><h3>'+esc(ex[0])+'</h3><p class="placeholder">'+esc(ex[1])+'</p></article>';
  }).join('');
}
function renderLanguagePicker(languages){
  if(!languages||!languages.items) return;
  if(!selectedLanguage) selectedLanguage=languages.default||'typescript';
  const picker=document.getElementById('language-picker');
  if(!languagePickerReady){
    picker.innerHTML=languages.items.map(function(lang){
      return '<option value="'+esc(lang.id)+'">'+esc(lang.label)+(lang.installed?'':'')+'</option>';
    }).join('');
    picker.addEventListener('change',function(){renderLanguageExamples(picker.value);});
    languagePickerReady=true;
  }
  const current=languages.items.find(function(lang){return lang.id===selectedLanguage;});
  const detected=languages.items.filter(function(lang){return lang.installed;}).map(function(lang){return lang.label;});
  document.getElementById('language-detected').textContent=detected.length?'Detected: '+detected.slice(0,4).join(', '):'Defaulting to Node.js / TypeScript';
  renderLanguageExamples(current?selectedLanguage:(languages.default||'typescript'));
}
function renderSubstitutions(values){
  const entries=Object.entries(values||{});
  if(!entries.length) return '<span class="empty">none detected</span>';
  return '<div class="substitutions">'+entries.map(function(pair){
    return '<code>{'+esc(pair[0])+'}='+esc(pair[1])+'</code>';
  }).join('')+'</div>';
}
function domainCell(row){
  const domain=row.link_url?'<a href="'+esc(row.link_url)+'"><strong>'+esc(row.domain)+'</strong></a>':'<strong>'+esc(row.domain)+'</strong>';
  let alerts='';
  if(row.alerts&&row.alerts.length){
    alerts='<div class="route-alerts">'+row.alerts.map(function(alert){
      const sev=(alert.severity||'warning').toLowerCase();
      return '<div class="route-alert '+esc(sev)+'"><div class="route-alert-title">'+esc(alert.title)+'</div><div class="route-alert-detail">'+esc(alert.detail)+'</div></div>';
    }).join('')+'</div>';
  }
  return '<div class="domain-stack">'+domain+'<code>template: '+esc(row.domain_template||row.domain)+'</code><code>materialized: '+esc(row.domain)+'</code>'+alerts+'</div>';
}

function renderDiag(d){
  const sev=d.severity.toLowerCase();
  let fix='';
  if(d.fix){
    fix='<div class="diag-fix">'+esc(d.fix.description);
    if(d.fix.command) fix+=' - <code>'+esc(d.fix.command)+'</code>';
    fix+='</div>';
  }
  return '<div class="diag diag-'+sev+'"><div class="diag-title"><span class="sev-'+sev+'">['+sev.toUpperCase()+']</span> '+esc(d.title)+'</div>'
    +'<div class="diag-detail">'+esc(d.detail)+'</div>'+fix+'</div>';
}

function render(d){
  document.getElementById('hdr-meta').textContent=d.daemon_pid?'pid '+d.daemon_pid:'local daemon';
  document.getElementById('hdr-ts').textContent='Updated '+new Date().toLocaleTimeString();
  renderLanguagePicker(d.languages);

  let html='';

  html+='<div class="status-grid">';
  html+='<section class="status-card"><h3>Local .local services</h3>';
  html+='<div class="status-row">'+dot(d.overlay_active)+(d.overlay_active?'active':'inactive')+'</div>';
  if(d.local_services&&d.local_services.length>0){
    html+='<table><thead><tr><th>domain</th><th>substitutions</th><th>real addr</th><th>port</th><th>pid</th></tr></thead><tbody>';
    d.local_services.forEach(function(s){
      html+='<tr><td>'+domainCell(s)+'</td><td>'+renderSubstitutions(s.substitutions)+'</td><td>'+esc(s.real_addr)+'</td><td>'+esc(s.service_port)+'</td><td>'+esc(s.pid)+'</td></tr>';
    });
    html+='</tbody></table>';
  } else {
    html+='<p class="empty">no local services</p>';
  }
  html+='</section>';

  html+='<section class="status-card"><h3>Cloud routes</h3>';
  html+='<div class="status-row">'+dot(d.cloud_connected)+(d.cloud_connected?'connected':'disconnected');
  if(d.cloud_error) html+=' <span style="color:var(--bad);font-size:13px">- '+esc(d.cloud_error)+'</span>';
  html+='</div>';
  if(d.cloud_routes&&d.cloud_routes.length>0){
    html+='<table><thead><tr><th>domain</th><th>substitutions</th><th>port</th><th>pid</th></tr></thead><tbody>';
    d.cloud_routes.forEach(function(r){
      html+='<tr><td>'+domainCell(r)+'</td><td>'+renderSubstitutions(r.substitutions)+'</td><td>'+esc(r.port)+'</td><td>'+esc(r.pid)+'</td></tr>';
    });
    html+='</tbody></table>';
  } else {
    if(!d.auth_authenticated){
      html+='<div class="empty-action"><p>No cloud routes because you are not logged in.</p><a class="button" href="/login">Log in</a></div>';
    } else {
      html+='<p class="empty">no cloud routes</p>';
    }
  }
  html+='</section>';
  html+='</div>';

  html+='<section class="section"><div class="section-head"><h2>API registrations</h2></div>';
  if(d.management_registrations&&d.management_registrations.length>0){
    html+='<table><thead><tr><th>pid</th><th>local port</th><th>domain</th></tr></thead><tbody>';
    d.management_registrations.forEach(function(r){
      html+='<tr><td>'+esc(r.pid)+'</td><td>'+esc(r.local_port)+'</td><td>'+esc(r.domain)+'</td></tr>';
    });
    html+='</tbody></table>';
  } else {
    html+='<p class="empty">none</p>';
  }
  html+='</section>';

  html+='<section class="section" id="diagnostics"><div class="section-head"><h2>Diagnostics</h2></div>';
  const diags=d.diagnostics&&d.diagnostics.issues;
  if(!diags||diags.length===0){
    const ran=d.diagnostics?d.diagnostics.checks_run:0;
    html+='<p class="no-issues">all '+(ran||'')+' checks passed</p>';
  } else {
    diags.forEach(function(i){html+=renderDiag(i);});
  }
  html+='</section>';

  document.getElementById('status-root').innerHTML=html;
}

async function load(){
  try{
    const r=await fetch('/status.json');
    if(r.ok) render(await r.json());
  }catch(e){
    document.getElementById('status-root').innerHTML='<p class="loading">Waiting for daemon...</p>';
  }
}
load();
setInterval(load,3000);
</script>
</body>
</html>"##;

/// GET /status.json — machine-readable snapshot of all daemon state.
pub async fn status_json(State(state): State<AppState>) -> Json<serde_json::Value> {
    let daemon_pid = read_daemon_pid(&state.state_dir);
    let overlay = read_overlay_state(&state.state_dir);
    let routes = read_route_table(&state.state_dir);
    let (cloud_connected, cloud_error) = read_cloud_state(&state.state_dir);
    let auth_authenticated = crate::auth::AuthConfig::load().is_authenticated();
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

    let mut local_services: Vec<serde_json::Value> = overlay
        .routes
        .iter()
        .map(|r| {
            let domain_template = if r.domain_template.is_empty() {
                &r.domain
            } else {
                &r.domain_template
            };
            serde_json::json!({
                "domain": r.domain,
                "domain_template": domain_template,
                "substitutions": display_substitutions(&r.substitutions, &r.source),
                "alerts": substitution_alerts(domain_template, &r.substitutions),
                "real_addr": r.real_addr,
                "service_port": r.service_port,
                "link_url": local_service_link_url(&r.domain, r.service_port),
                "pid": r.pid,
            })
        })
        .collect();

    let mut cloud_routes: Vec<serde_json::Value> = routes
        .routes
        .values()
        .map(|r| {
            let domain_template = if r.domain_template.is_empty() {
                &r.domain
            } else {
                &r.domain_template
            };
            serde_json::json!({
                "domain": r.domain,
                "domain_template": domain_template,
                "substitutions": display_substitutions(&r.substitutions, &r.source),
                "alerts": substitution_alerts(domain_template, &r.substitutions),
                "port": r.port,
                "pid": r.pid,
            })
        })
        .collect();

    add_duplicate_route_alerts(&mut local_services, &mut cloud_routes);

    Json(serde_json::json!({
        "daemon_pid": daemon_pid,
        "overlay_active": overlay.overlay_active,
        "auth_authenticated": auth_authenticated,
        "local_services": local_services,
        "cloud_connected": cloud_connected,
        "cloud_error": cloud_error,
        "cloud_routes": cloud_routes,
        "management_registrations": management_registrations,
        "diagnostics": diagnostics,
        "languages": detected_languages(),
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        add_duplicate_route_alerts, has_dns_token, local_service_link_url, substitution_alerts,
    };
    use std::collections::BTreeMap;

    #[test]
    fn local_service_link_url_prefers_https_for_web_ports() {
        assert_eq!(
            local_service_link_url("web.portzero.local", 80).as_deref(),
            Some("https://web.portzero.local")
        );
        assert_eq!(
            local_service_link_url("staging.portzero.net.portzero.local", 443).as_deref(),
            Some("https://staging.portzero.net.portzero.local")
        );
        assert_eq!(
            local_service_link_url("api.portzero.local", 443).as_deref(),
            Some("https://api.portzero.local")
        );
    }

    #[test]
    fn local_service_link_url_skips_non_web_ports() {
        assert_eq!(local_service_link_url("db.portzero.local", 5432), None);
        assert_eq!(local_service_link_url("admin.portzero.local", 8080), None);
    }

    #[test]
    fn dns_token_matching_requires_boundaries() {
        assert!(has_dns_token("web-main.portzero.local", "main"));
        assert!(has_dns_token("web.feature-x.portzero.local", "feature-x"));
        assert!(!has_dns_token("web-maintenance.portzero.local", "main"));
    }

    #[test]
    fn substitution_alert_suggests_branch_placeholder_for_literal_value() {
        let substitutions = BTreeMap::from([
            ("branch".to_string(), "main".to_string()),
            ("worktree".to_string(), "portzero-local".to_string()),
        ]);

        let alerts = substitution_alerts("web-main.portzero.local", &substitutions);

        assert_eq!(alerts.len(), 1);
        assert_eq!(
            alerts[0].get("title").and_then(|v| v.as_str()),
            Some("Suggest replacing \"main\" with \"{branch}\"")
        );
        assert_eq!(
            alerts[0].get("severity").and_then(|v| v.as_str()),
            Some("info")
        );
        assert!(alerts[0]
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("PZ_TUNNEL supports variable substitution"));
    }

    #[test]
    fn substitution_alert_skips_existing_placeholder() {
        let substitutions = BTreeMap::from([("branch".to_string(), "main".to_string())]);

        let alerts = substitution_alerts("web-{branch}.portzero.local", &substitutions);

        assert!(alerts.is_empty());
    }

    #[test]
    fn substitution_alert_warns_when_branch_placeholder_resolves_to_unknown() {
        let substitutions = BTreeMap::from([("branch".to_string(), "unknown".to_string())]);

        let alerts = substitution_alerts("web-{branch}.portzero.local", &substitutions);

        assert_eq!(alerts.len(), 1);
        assert_eq!(
            alerts[0].get("title").and_then(|v| v.as_str()),
            Some("Could not determine {branch}")
        );
        assert_eq!(
            alerts[0].get("severity").and_then(|v| v.as_str()),
            Some("info")
        );
        assert!(alerts[0]
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("materialized the value as \"unknown\""));
    }

    #[test]
    fn duplicate_route_alerts_mark_every_matching_domain() {
        let mut local_services = vec![
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
        ];
        let mut cloud_routes = vec![serde_json::json!({
            "domain": "api.portzero.local",
            "port": 3000,
            "alerts": [],
        })];

        add_duplicate_route_alerts(&mut local_services, &mut cloud_routes);

        for row in local_services.iter().chain(cloud_routes.iter()) {
            let alerts = row.get("alerts").and_then(|v| v.as_array()).unwrap();
            assert_eq!(alerts.len(), 1);
            assert_eq!(
                alerts[0].get("title").and_then(|v| v.as_str()),
                Some("Duplicate tunnel domain")
            );
            assert_eq!(
                alerts[0].get("severity").and_then(|v| v.as_str()),
                Some("warning")
            );
            assert!(alerts[0]
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap()
                .contains("3 tunnels materialized to api.portzero.local"));
        }
    }

    #[test]
    fn duplicate_route_alerts_include_different_ports() {
        let mut local_services = vec![
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3001,
                "alerts": [],
            }),
        ];
        let mut cloud_routes = Vec::new();

        add_duplicate_route_alerts(&mut local_services, &mut cloud_routes);

        assert!(local_services
            .iter()
            .all(|row| { row.get("alerts").and_then(|v| v.as_array()).unwrap().len() == 1 }));
    }

    #[test]
    fn duplicate_route_alerts_skip_unique_domains() {
        let mut local_services = vec![
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
            serde_json::json!({
                "domain": "web.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
        ];
        let mut cloud_routes = Vec::new();

        add_duplicate_route_alerts(&mut local_services, &mut cloud_routes);

        assert!(local_services.iter().all(|row| row
            .get("alerts")
            .and_then(|v| v.as_array())
            .unwrap()
            .is_empty()));
    }
}
