//! `portzero mcp`: a Model Context Protocol server (JSON-RPC 2.0 over stdio)
//! that exposes the daemon's observed runtime truth to AI coding agents.
//!
//! It speaks newline-delimited JSON-RPC on stdin/stdout — the MCP stdio
//! transport. An agent launches `portzero mcp` as a subprocess and calls the
//! tools below to learn what is actually running:
//!
//! - `overview`          — everything at once (services, tunnels, edges, routes)
//! - `list_services`     — discovered processes/containers, their ports, images
//! - `list_tunnels`      — tunnel domains (local + cloud), URLs, health paths
//! - `observed_edges`    — who-talks-to-whom dependency edges
//! - `exercised_routes`  — HTTP routes actually hit per tunnel (smoke-test list)
//!
//! All data is read fresh from the daemon's state files on each call, so it
//! reflects current truth. See `portzero inspect` for the human-readable view.

use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::{json, Value};

use portzero_daemon::discovery::ServiceSource;
use portzero_daemon::discovery_loop::{read_daemon_pid, DaemonConfig};
use portzero_daemon::observations::Observations;
use portzero_daemon::route_table::{OverlayState, RouteTable};

use crate::export::tunnel_url;

/// MCP protocol revision we implement (a widely-supported stable revision).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the MCP stdio server until stdin closes.
pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                // Parse error — id unknown, per JSON-RPC use null.
                write_message(
                    &mut stdout,
                    &error_response(Value::Null, -32700, &format!("parse error: {e}")),
                )?;
                continue;
            }
        };

        // Notifications have no `id` and get no response.
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");

        if id.is_none() {
            // e.g. notifications/initialized — nothing to reply.
            continue;
        }
        let id = id.unwrap();

        let response = match handle_method(method, request.get("params")) {
            Ok(result) => success_response(id, result),
            Err(err) => error_response(id, -32601, &err),
        };
        write_message(&mut stdout, &response)?;
    }

    Ok(())
}

/// Dispatch a JSON-RPC method to its handler. `Err(message)` becomes a
/// JSON-RPC error response.
fn handle_method(method: &str, params: Option<&Value>) -> Result<Value, String> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "portzero", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Runtime truth from the portzero daemon: discovered \
                services/containers, tunnel domains, health paths, observed \
                dependency edges, and exercised HTTP routes. Only traffic addressed \
                via tunnel names is observed."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => {
            let params = params.ok_or_else(|| "missing params".to_string())?;
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing tool name".to_string())?;
            call_tool(name)
        }
        other => Err(format!("method not found: {other}")),
    }
}

/// The advertised tool catalog. All tools take no arguments.
fn tool_list() -> Value {
    let no_args = json!({ "type": "object", "properties": {} });
    json!([
        {
            "name": "overview",
            "description": "Everything the daemon knows at once: discovered services, \
                tunnel domains + health paths, observed dependency edges, and exercised routes.",
            "inputSchema": no_args,
        },
        {
            "name": "list_services",
            "description": "Discovered processes and Docker containers with PZ_TUNNEL: \
                their tunnel domain, listening/forwarded port, source (process pid or \
                container image/name), and health path.",
            "inputSchema": no_args,
        },
        {
            "name": "list_tunnels",
            "description": "Tunnel domains (local overlay and cloud), their resolved URLs, \
                health paths, and cloud review status.",
            "inputSchema": no_args,
        },
        {
            "name": "observed_edges",
            "description": "Observed who-talks-to-whom dependency edges between tunnels \
                (protocol, request count, last seen).",
            "inputSchema": no_args,
        },
        {
            "name": "exercised_routes",
            "description": "HTTP routes actually exercised per tunnel (method, path, count, \
                and X-PZ-Test attributions) — a ready-made smoke-test inventory.",
            "inputSchema": no_args,
        },
    ])
}

/// Execute a tool by name, returning a `tools/call` result envelope.
fn call_tool(name: &str) -> Result<Value, String> {
    let config = DaemonConfig::load();
    let value = match name {
        "overview" => overview(&config),
        "list_services" => json!({ "services": services(&config) }),
        "list_tunnels" => json!({ "tunnels": tunnels(&config) }),
        "observed_edges" => json!({ "edges": edges(&config) }),
        "exercised_routes" => json!({ "routes": routes(&config) }),
        other => return Err(format!("unknown tool: {other}")),
    };

    // MCP tools return content blocks; we return the JSON as a text block.
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    Ok(json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": false,
    }))
}

// ---------------------------------------------------------------------------
// Data gathering (shared shapes for every tool)
// ---------------------------------------------------------------------------

fn overview(config: &DaemonConfig) -> Value {
    json!({
        "daemon_running": read_daemon_pid(config).is_some(),
        "services": services(config),
        "tunnels": tunnels(config),
        "observed_edges": edges(config),
        "exercised_routes": routes(config),
        "observability_caveat":
            "Only traffic addressed via tunnel names is observed. Container-to-container \
             traffic over compose-internal DNS bypasses the daemon and is not recorded.",
    })
}

fn source_json(source: &ServiceSource, pid: u32) -> Value {
    match source {
        ServiceSource::Process { cwd } => json!({
            "type": "process",
            "pid": pid,
            "cwd": cwd.as_ref().map(|c| c.display().to_string()),
        }),
        ServiceSource::Container { id, name } => json!({
            "type": "container",
            "id": id,
            "name": name,
        }),
    }
}

fn services(config: &DaemonConfig) -> Vec<Value> {
    let table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();

    let mut out = Vec::new();
    for r in table.routes.values() {
        out.push(json!({
            "domain": r.domain,
            "kind": "cloud",
            "port": r.port,
            "health_path": r.health_path,
            "source": source_json(&r.source, r.pid),
        }));
    }
    for r in &overlay.routes {
        out.push(json!({
            "domain": r.domain,
            "kind": "local",
            "port": r.service_port,
            "real_addr": r.real_addr,
            "health_path": r.health_path,
            "source": source_json(&r.source, r.pid),
        }));
    }
    out.sort_by(|a, b| domain_of(a).cmp(domain_of(b)));
    out
}

fn tunnels(config: &DaemonConfig) -> Vec<Value> {
    let table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();
    let statuses = read_cloud_route_statuses(config);

    let mut out = Vec::new();
    for r in table.routes.values() {
        out.push(json!({
            "domain": r.domain,
            "kind": "cloud",
            "url": tunnel_url(&r.domain, r.port),
            "health_path": r.health_path,
            "status": statuses.get(&r.domain).cloned().unwrap_or_else(|| "published".to_string()),
        }));
    }
    for r in &overlay.routes {
        out.push(json!({
            "domain": r.domain,
            "kind": "local",
            "url": tunnel_url(&r.domain, r.service_port),
            "health_path": r.health_path,
            "status": "local",
        }));
    }
    out.sort_by(|a, b| domain_of(a).cmp(domain_of(b)));
    out.dedup_by(|a, b| domain_of(a).eq_ignore_ascii_case(domain_of(b)));
    out
}

fn edges(config: &DaemonConfig) -> Vec<Value> {
    Observations::load(&config.observations_path())
        .edges
        .into_iter()
        .map(|e| {
            json!({
                "from": e.from,
                "to": e.to,
                "protocol": e.protocol,
                "request_count": e.request_count,
                "last_seen": e.last_seen,
            })
        })
        .collect()
}

fn routes(config: &DaemonConfig) -> Vec<Value> {
    Observations::load(&config.observations_path())
        .routes
        .into_iter()
        .map(|r| {
            json!({
                "domain": r.domain,
                "method": r.method,
                "path": r.path,
                "count": r.count,
                "tests": r.tests,
                "last_seen": r.last_seen,
            })
        })
        .collect()
}

fn domain_of(v: &Value) -> &str {
    v.get("domain").and_then(Value::as_str).unwrap_or("")
}

fn read_cloud_route_statuses(config: &DaemonConfig) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(config.cloud_route_status_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// JSON-RPC framing
// ---------------------------------------------------------------------------

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn write_message(out: &mut impl Write, msg: &Value) -> Result<()> {
    // Newline-delimited JSON: one compact object per line.
    let line = serde_json::to_string(msg)?;
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize_reports_tools_capability() {
        let result = handle_method("initialize", None).unwrap();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "portzero");
    }

    #[test]
    fn test_tools_list_advertises_expected_tools() {
        let result = handle_method("tools/list", None).unwrap();
        let names: Vec<&str> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "overview",
            "list_services",
            "list_tunnels",
            "observed_edges",
            "exercised_routes",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
    }

    #[test]
    fn test_tools_call_returns_text_content() {
        let params = json!({ "name": "observed_edges", "arguments": {} });
        let result = handle_method("tools/call", Some(&params)).unwrap();
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["type"], "text");
        assert!(result["content"][0]["text"].is_string());
    }

    #[test]
    fn test_unknown_method_is_error() {
        assert!(handle_method("does/not/exist", None).is_err());
    }

    #[test]
    fn test_unknown_tool_is_error() {
        let params = json!({ "name": "bogus" });
        assert!(handle_method("tools/call", Some(&params)).is_err());
    }
}
