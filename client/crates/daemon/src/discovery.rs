//! Service discovery: scan processes and Docker containers for DEVENV_TUNNEL.
//!
//! Cross-platform support:
//! - Linux: full (reads /proc/<pid>/environ and /proc/<pid>/fd for inode-based port scoping)
//! - macOS: full (uses `ps -p <pid> -wwwE` and `lsof`)
//! - Windows: containers only

use std::path::{Path, PathBuf};

use anyhow::Result;
use devenv_tunnel_domain::DomainContext;
use sysinfo::System;

/// The environment variable that tags a process for tunneling.
const ENV_VAR_NAME: &str = "DEVENV_TUNNEL";

/// Selects which HTTP port to forward. Defaults to CHOOSE_LOWEST if unset.
const ENV_HTTP_PORT_VAR: &str = "DEVENV_TUNNEL_HTTP_PORT";

/// Additional raw port mappings: `local:tunnel[;local:tunnel...]`
const ENV_PORTS_VAR: &str = "DEVENV_TUNNEL_PORTS";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A discovered service with its domain, port, and origin.
#[derive(Debug, Clone)]
pub struct DiscoveredService {
    /// Resolved DEVENV_TUNNEL value (the domain name).
    pub domain: String,
    /// Selected HTTP port (0 if not determinable).
    pub port: u16,
    /// Additional port mappings from DEVENV_TUNNEL_PORTS.
    pub extra_ports: Vec<PortMapping>,
    /// Process ID that owns the service.
    pub pid: u32,
    /// How the service was discovered.
    pub source: ServiceSource,
}

/// A local-port → tunnel-port mapping for raw (non-HTTP) forwarding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortMapping {
    pub local_port: u16,
    pub tunnel_port: u16,
}

/// Where the service was discovered.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ServiceSource {
    /// A local OS process.
    Process {
        /// Working directory of the process, if available.
        cwd: Option<PathBuf>,
    },
    /// A Docker container.
    Container {
        /// Container ID (short hash).
        id: String,
        /// Container name.
        name: String,
    },
}

impl std::fmt::Display for ServiceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceSource::Process { cwd } => {
                if let Some(dir) = cwd {
                    let display = if let Some(home) = dirs::home_dir() {
                        if let Ok(rel) = dir.strip_prefix(&home) {
                            format!("~/{}", rel.display())
                        } else {
                            dir.display().to_string()
                        }
                    } else {
                        dir.display().to_string()
                    };
                    write!(f, "({})", display)
                } else {
                    write!(f, "(unknown dir)")
                }
            }
            ServiceSource::Container { name, .. } => {
                write!(f, "container {}", name)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal port discovery types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum BindAddr {
    /// Bound to all interfaces (0.0.0.0 / ::).
    Public,
    /// Bound to loopback only (127.x.x.x / ::1).
    Loopback,
}

#[derive(Debug, Clone)]
struct ListeningPort {
    port: u16,
    bind: BindAddr,
}

/// How the HTTP port is chosen when DEVENV_TUNNEL_HTTP_PORT is set.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HttpPortSelection {
    /// Use this exact port number.
    Explicit(u16),
    /// Pick the lowest-numbered public port; fall back to lowest loopback.
    ChooseLowest,
    /// Pick the highest-numbered public port; fall back to highest loopback.
    ChooseHighest,
}

/// The result of selecting an HTTP port for a process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedPort {
    /// A port was found and the process owns it.
    Found(u16),
    /// An explicit port was requested but the process does not own it.
    ExplicitNotOwned(u16),
    /// No listening port could be found (process hasn't bound one yet, or
    /// port enumeration failed).
    NoneListening,
}

// ---------------------------------------------------------------------------
// Unified scanner
// ---------------------------------------------------------------------------

/// Scan all processes and Docker containers for DEVENV_TUNNEL.
pub async fn scan_all(account_id: Option<&str>, username: Option<&str>) -> Vec<DiscoveredService> {
    let mut services = Vec::new();
    services.extend(scan_processes(account_id, username));
    services.extend(scan_docker_containers(account_id, username).await);
    services
}

// ---------------------------------------------------------------------------
// Process scanning
// ---------------------------------------------------------------------------

fn scan_processes(account_id: Option<&str>, username: Option<&str>) -> Vec<DiscoveredService> {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let mut services = Vec::new();

    for (pid, process) in sys.processes() {
        let pid_u32 = pid.as_u32();
        if pid_u32 <= 1 {
            continue;
        }

        let raw = match scan_process_env(pid_u32, ENV_VAR_NAME) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };

        let cwd = process.cwd().map(|p| p.to_path_buf());
        let domain = resolve_tunnel_template(&raw, cwd.as_deref(), account_id, username);

        let http_selection = scan_process_env(pid_u32, ENV_HTTP_PORT_VAR)
            .map(|v| parse_http_port_selection(&v))
            .unwrap_or(HttpPortSelection::ChooseLowest);

        let extra_ports = scan_process_env(pid_u32, ENV_PORTS_VAR)
            .map(|v| parse_extra_ports(&v))
            .unwrap_or_default();

        let listening = discover_process_ports(pid_u32);

        let port = match select_http_port(&listening, &http_selection) {
            SelectedPort::Found(p) => p,
            SelectedPort::ExplicitNotOwned(requested) => {
                tracing::warn!(
                    pid = pid_u32,
                    requested_port = requested,
                    "DEVENV_TUNNEL_HTTP_PORT specifies a port not owned by this process; ignoring"
                );
                continue;
            }
            // No listening ports on this process. This is expected when a
            // parent shell or launcher sets DEVENV_TUNNEL so that a child
            // process inherits it — the parent itself has nothing to forward.
            SelectedPort::NoneListening => {
                tracing::debug!(
                    pid = pid_u32,
                    "skipping process with DEVENV_TUNNEL but no listening ports \
                     (likely a parent process passing the variable to its child)"
                );
                continue;
            }
        };

        let owned_ports: std::collections::HashSet<u16> =
            listening.iter().map(|lp| lp.port).collect();
        let extra_ports: Vec<PortMapping> = extra_ports
            .into_iter()
            .filter(|m| {
                if owned_ports.contains(&m.local_port) {
                    true
                } else {
                    tracing::warn!(
                        pid = pid_u32,
                        local_port = m.local_port,
                        "DEVENV_TUNNEL_PORTS entry references a port not owned by this process; skipping"
                    );
                    false
                }
            })
            .collect();

        services.push(DiscoveredService {
            domain,
            port,
            extra_ports,
            pid: pid_u32,
            source: ServiceSource::Process { cwd },
        });
    }

    services
}

/// Resolve template variables in a DEVENV_TUNNEL value.
fn resolve_tunnel_template(
    raw: &str,
    project_dir: Option<&Path>,
    account_id: Option<&str>,
    username: Option<&str>,
) -> String {
    if !raw.contains('{') {
        return raw.to_string();
    }
    let dir = project_dir.unwrap_or(Path::new("."));
    let ctx = DomainContext::from_environment("", dir, account_id, username);
    ctx.resolve(raw)
}

// ---------------------------------------------------------------------------
// Port selection logic
// ---------------------------------------------------------------------------

/// Parse DEVENV_TUNNEL_HTTP_PORT into a selection strategy.
fn parse_http_port_selection(val: &str) -> HttpPortSelection {
    match val.trim() {
        "" | "CHOOSE_LOWEST" => HttpPortSelection::ChooseLowest,
        "CHOOSE_HIGHEST" => HttpPortSelection::ChooseHighest,
        other => match other.parse::<u16>() {
            Ok(port) => HttpPortSelection::Explicit(port),
            Err(_) => HttpPortSelection::ChooseLowest,
        },
    }
}

/// Parse DEVENV_TUNNEL_PORTS into a list of port mappings.
///
/// Format: `local:tunnel[;local:tunnel...]`  e.g. `9222:9222;9300:9300`
fn parse_extra_ports(val: &str) -> Vec<PortMapping> {
    val.split(';')
        .filter_map(|entry| {
            let mut parts = entry.trim().splitn(2, ':');
            let local: u16 = parts.next()?.trim().parse().ok()?;
            let tunnel: u16 = parts.next()?.trim().parse().ok()?;
            Some(PortMapping {
                local_port: local,
                tunnel_port: tunnel,
            })
        })
        .collect()
}

/// Choose the HTTP port from the list of listening ports.
///
/// Prefers ports bound to 0.0.0.0 over loopback-only. Falls back to loopback
/// when no public-facing port is found.
///
/// For `Explicit`, the requested port must be in `ports` (owned by the
/// process); otherwise returns `ExplicitNotOwned`.
fn select_http_port(ports: &[ListeningPort], selection: &HttpPortSelection) -> SelectedPort {
    match selection {
        HttpPortSelection::Explicit(p) => {
            if ports.iter().any(|lp| lp.port == *p) {
                SelectedPort::Found(*p)
            } else {
                SelectedPort::ExplicitNotOwned(*p)
            }
        }
        HttpPortSelection::ChooseLowest => {
            let port = ports
                .iter()
                .filter(|p| p.bind == BindAddr::Public)
                .map(|p| p.port)
                .min()
                .or_else(|| ports.iter().map(|p| p.port).min());
            match port {
                Some(p) => SelectedPort::Found(p),
                None => SelectedPort::NoneListening,
            }
        }
        HttpPortSelection::ChooseHighest => {
            let port = ports
                .iter()
                .filter(|p| p.bind == BindAddr::Public)
                .map(|p| p.port)
                .max()
                .or_else(|| ports.iter().map(|p| p.port).max());
            match port {
                Some(p) => SelectedPort::Found(p),
                None => SelectedPort::NoneListening,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Port enumeration
// ---------------------------------------------------------------------------

/// Return all TCP ports the process is actively listening on, with bind addresses.
fn discover_process_ports(pid: u32) -> Vec<ListeningPort> {
    #[cfg(target_os = "linux")]
    {
        return discover_ports_linux(pid);
    }

    #[cfg(target_os = "macos")]
    {
        return discover_ports_lsof(pid);
    }

    #[allow(unreachable_code)]
    Vec::new()
}

#[cfg(target_os = "linux")]
fn discover_ports_linux(pid: u32) -> Vec<ListeningPort> {
    // Collect socket inodes owned by this process via /proc/<pid>/fd/
    let mut owned_inodes = std::collections::HashSet::new();
    let fd_dir = format!("/proc/{}/fd", pid);
    if let Ok(entries) = std::fs::read_dir(&fd_dir) {
        for entry in entries.flatten() {
            if let Ok(target) = std::fs::read_link(entry.path()) {
                let s = target.to_string_lossy();
                if let Some(inode_str) =
                    s.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']'))
                {
                    if let Ok(inode) = inode_str.parse::<u64>() {
                        owned_inodes.insert(inode);
                    }
                }
            }
        }
    }

    // If we can't read fd (permissions), fall back to lsof
    if owned_inodes.is_empty() {
        return discover_ports_lsof(pid);
    }

    let tcp_path = format!("/proc/{}/net/tcp", pid);
    let tcp6_path = format!("/proc/{}/net/tcp6", pid);
    let mut ports = Vec::new();

    for path in [&tcp_path, &tcp6_path] {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines().skip(1) {
                if let Some(p) = parse_proc_net_tcp_line(line, &owned_inodes) {
                    ports.push(p);
                }
            }
        }
    }

    if ports.is_empty() {
        discover_ports_lsof(pid)
    } else {
        ports
    }
}

/// Parse one line from /proc/<pid>/net/tcp or tcp6.
///
/// Returns Some only for LISTEN (state 0A) sockets whose inode is in `owned`.
/// Columns: idx local_addr remote_addr state ... inode
#[cfg(target_os = "linux")]
fn parse_proc_net_tcp_line(
    line: &str,
    owned: &std::collections::HashSet<u64>,
) -> Option<ListeningPort> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 10 {
        return None;
    }
    // state must be 0A (LISTEN)
    if parts[3] != "0A" {
        return None;
    }
    let inode: u64 = parts[9].parse().ok()?;
    if !owned.contains(&inode) {
        return None;
    }
    let local_addr = parts[1];
    let (addr_hex, port_hex) = local_addr.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if port == 0 {
        return None;
    }
    // All-zero address hex means bound to all interfaces (0.0.0.0 or ::)
    let bind = if addr_hex.chars().all(|c| c == '0') {
        BindAddr::Public
    } else {
        BindAddr::Loopback
    };
    Some(ListeningPort { port, bind })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn discover_ports_lsof(pid: u32) -> Vec<ListeningPort> {
    use std::process::Command;

    let output = match Command::new("lsof")
        .args(["-iTCP", "-sTCP:LISTEN", "-nP", "-p", &pid.to_string()])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ports = Vec::new();

    for line in stdout.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(name) = parts.last() {
            // name is like "*:8080" or "127.0.0.1:8080" or "[::]:8080"
            if let Some((addr, port_str)) = name.rsplit_once(':') {
                if let Ok(port) = port_str.parse::<u16>() {
                    if port > 0 {
                        let bind = if addr == "*" || addr == "0.0.0.0" || addr == "[::]" {
                            BindAddr::Public
                        } else {
                            BindAddr::Loopback
                        };
                        ports.push(ListeningPort { port, bind });
                    }
                }
            }
        }
    }

    ports
}

// ---------------------------------------------------------------------------
// Platform-specific environment variable reading
// ---------------------------------------------------------------------------

fn scan_process_env(pid: u32, var_name: &str) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        scan_process_env_linux(pid, var_name)
    }
    #[cfg(target_os = "macos")]
    {
        scan_process_env_macos(pid, var_name)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (pid, var_name);
        None
    }
}

#[cfg(target_os = "linux")]
fn scan_process_env_linux(pid: u32, var_name: &str) -> Option<String> {
    let environ_path = format!("/proc/{}/environ", pid);
    let data = std::fs::read(&environ_path).ok()?;

    let prefix = format!("{}=", var_name);
    for entry in data.split(|&b| b == 0) {
        if let Ok(s) = std::str::from_utf8(entry) {
            if let Some(value) = s.strip_prefix(&prefix) {
                return Some(value.to_string());
            }
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn scan_process_env_macos(pid: u32, var_name: &str) -> Option<String> {
    use std::process::Command;

    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-wwwE", "-o", "command="])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let prefix = format!("{}=", var_name);

    for token in stdout.split_whitespace() {
        if let Some(value) = token.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Docker container scanning
// ---------------------------------------------------------------------------

async fn scan_docker_containers(
    account_id: Option<&str>,
    username: Option<&str>,
) -> Vec<DiscoveredService> {
    match scan_docker_containers_impl(account_id, username).await {
        Ok(services) => services,
        Err(e) => {
            tracing::debug!(
                "Docker container scan failed (Docker may not be running): {}",
                e
            );
            Vec::new()
        }
    }
}

async fn scan_docker_containers_impl(
    account_id: Option<&str>,
    username: Option<&str>,
) -> Result<Vec<DiscoveredService>> {
    use std::process::Command;

    let output = Command::new("docker")
        .args(["ps", "--format", "{{.ID}}"])
        .output()?;

    if !output.status.success() {
        anyhow::bail!("docker ps failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let container_ids: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    if container_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut services = Vec::new();
    for container_id in container_ids {
        if let Some(svc) = inspect_container(container_id, account_id, username)? {
            services.push(svc);
        }
    }

    Ok(services)
}

fn inspect_container(
    container_id: &str,
    account_id: Option<&str>,
    username: Option<&str>,
) -> Result<Option<DiscoveredService>> {
    use std::process::Command;

    let output = Command::new("docker")
        .args([
            "inspect",
            container_id,
            "--format",
            "{{json .Config.Env}}||{{.Name}}||{{json .NetworkSettings.Ports}}||{{.State.Pid}}",
        ])
        .output()?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();

    let parts: Vec<&str> = stdout.splitn(4, "||").collect();
    if parts.len() < 4 {
        return Ok(None);
    }

    let env_json = parts[0];
    let name = parts[1].trim_start_matches('/');
    let ports_json = parts[2];
    let container_pid: u32 = parts[3].parse().unwrap_or(0);

    let env_vars: Vec<String> = serde_json::from_str(env_json).unwrap_or_default();

    let raw = env_vars
        .iter()
        .find_map(|e| e.strip_prefix("DEVENV_TUNNEL="))
        .map(|v| v.to_string());
    let raw = match raw {
        Some(d) if !d.is_empty() => d,
        _ => return Ok(None),
    };

    let domain = resolve_tunnel_template(&raw, None, account_id, username);

    let http_selection = env_vars
        .iter()
        .find_map(|e| e.strip_prefix("DEVENV_TUNNEL_HTTP_PORT="))
        .map(parse_http_port_selection)
        .unwrap_or(HttpPortSelection::ChooseLowest);

    let extra_ports = env_vars
        .iter()
        .find_map(|e| e.strip_prefix("DEVENV_TUNNEL_PORTS="))
        .map(parse_extra_ports)
        .unwrap_or_default();

    let port = match http_selection {
        HttpPortSelection::Explicit(p) => p,
        _ => parse_docker_ports(ports_json, &http_selection),
    };

    Ok(Some(DiscoveredService {
        domain,
        port,
        extra_ports,
        pid: container_pid,
        source: ServiceSource::Container {
            id: container_id.to_string(),
            name: name.to_string(),
        },
    }))
}

fn parse_docker_ports(ports_json: &str, selection: &HttpPortSelection) -> u16 {
    let ports: serde_json::Value = match serde_json::from_str(ports_json) {
        Ok(v) => v,
        Err(_) => return 0,
    };

    let obj = match ports.as_object() {
        Some(o) => o,
        None => return 0,
    };

    let mut found: Vec<u16> = Vec::new();
    for (_container_port, bindings) in obj {
        if let Some(arr) = bindings.as_array() {
            for binding in arr {
                if let Some(port_str) = binding.get("HostPort").and_then(|v| v.as_str()) {
                    if let Ok(port) = port_str.parse::<u16>() {
                        if port > 0 {
                            found.push(port);
                        }
                    }
                }
            }
        }
    }

    match selection {
        HttpPortSelection::ChooseHighest => found.into_iter().max().unwrap_or(0),
        _ => found.into_iter().min().unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_http_port_selection() {
        assert_eq!(
            parse_http_port_selection(""),
            HttpPortSelection::ChooseLowest
        );
        assert_eq!(
            parse_http_port_selection("CHOOSE_LOWEST"),
            HttpPortSelection::ChooseLowest
        );
        assert_eq!(
            parse_http_port_selection("CHOOSE_HIGHEST"),
            HttpPortSelection::ChooseHighest
        );
        assert_eq!(
            parse_http_port_selection("8080"),
            HttpPortSelection::Explicit(8080)
        );
        assert_eq!(
            parse_http_port_selection("3000"),
            HttpPortSelection::Explicit(3000)
        );
        // Invalid falls back to ChooseLowest
        assert_eq!(
            parse_http_port_selection("notaport"),
            HttpPortSelection::ChooseLowest
        );
    }

    #[test]
    fn test_parse_extra_ports() {
        let mappings = parse_extra_ports("9222:9222;9300:9300");
        assert_eq!(mappings.len(), 2);
        assert_eq!(
            mappings[0],
            PortMapping {
                local_port: 9222,
                tunnel_port: 9222
            }
        );
        assert_eq!(
            mappings[1],
            PortMapping {
                local_port: 9300,
                tunnel_port: 9300
            }
        );
    }

    #[test]
    fn test_parse_extra_ports_single() {
        let mappings = parse_extra_ports("5432:5432");
        assert_eq!(mappings.len(), 1);
        assert_eq!(
            mappings[0],
            PortMapping {
                local_port: 5432,
                tunnel_port: 5432
            }
        );
    }

    #[test]
    fn test_parse_extra_ports_empty() {
        assert!(parse_extra_ports("").is_empty());
        assert!(parse_extra_ports("  ").is_empty());
    }

    #[test]
    fn test_select_http_port_explicit_owned() {
        let ports = vec![
            ListeningPort {
                port: 3000,
                bind: BindAddr::Public,
            },
            ListeningPort {
                port: 8080,
                bind: BindAddr::Loopback,
            },
        ];
        assert_eq!(
            select_http_port(&ports, &HttpPortSelection::Explicit(3000)),
            SelectedPort::Found(3000)
        );
        assert_eq!(
            select_http_port(&ports, &HttpPortSelection::Explicit(8080)),
            SelectedPort::Found(8080)
        );
    }

    #[test]
    fn test_select_http_port_explicit_not_owned() {
        let ports = vec![
            ListeningPort {
                port: 3000,
                bind: BindAddr::Public,
            },
            ListeningPort {
                port: 8080,
                bind: BindAddr::Loopback,
            },
        ];
        assert_eq!(
            select_http_port(&ports, &HttpPortSelection::Explicit(9000)),
            SelectedPort::ExplicitNotOwned(9000)
        );
    }

    #[test]
    fn test_select_http_port_explicit_no_listening_ports() {
        assert_eq!(
            select_http_port(&[], &HttpPortSelection::Explicit(8080)),
            SelectedPort::ExplicitNotOwned(8080)
        );
    }

    #[test]
    fn test_select_http_port_prefers_public() {
        let ports = vec![
            ListeningPort {
                port: 9229,
                bind: BindAddr::Loopback,
            }, // debugger
            ListeningPort {
                port: 3000,
                bind: BindAddr::Public,
            },
        ];
        assert_eq!(
            select_http_port(&ports, &HttpPortSelection::ChooseLowest),
            SelectedPort::Found(3000)
        );
    }

    #[test]
    fn test_select_http_port_falls_back_to_loopback() {
        let ports = vec![
            ListeningPort {
                port: 8080,
                bind: BindAddr::Loopback,
            },
            ListeningPort {
                port: 3000,
                bind: BindAddr::Loopback,
            },
        ];
        assert_eq!(
            select_http_port(&ports, &HttpPortSelection::ChooseLowest),
            SelectedPort::Found(3000)
        );
    }

    #[test]
    fn test_select_http_port_choose_highest_public() {
        let ports = vec![
            ListeningPort {
                port: 3000,
                bind: BindAddr::Public,
            },
            ListeningPort {
                port: 8080,
                bind: BindAddr::Public,
            },
            ListeningPort {
                port: 9229,
                bind: BindAddr::Loopback,
            },
        ];
        assert_eq!(
            select_http_port(&ports, &HttpPortSelection::ChooseHighest),
            SelectedPort::Found(8080)
        );
    }

    #[test]
    fn test_select_http_port_no_listening_ports() {
        assert_eq!(
            select_http_port(&[], &HttpPortSelection::ChooseLowest),
            SelectedPort::NoneListening
        );
        assert_eq!(
            select_http_port(&[], &HttpPortSelection::ChooseHighest),
            SelectedPort::NoneListening
        );
    }

    #[test]
    fn test_parse_docker_ports_typical() {
        let json = r#"{"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"58321"}]}"#;
        assert_eq!(
            parse_docker_ports(json, &HttpPortSelection::ChooseLowest),
            58321
        );
    }

    #[test]
    fn test_parse_docker_ports_choose_highest() {
        let json = r#"{"3000/tcp":[{"HostIp":"0.0.0.0","HostPort":"3000"}],"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"8080"}]}"#;
        assert_eq!(
            parse_docker_ports(json, &HttpPortSelection::ChooseHighest),
            8080
        );
    }

    #[test]
    fn test_parse_docker_ports_empty() {
        assert_eq!(
            parse_docker_ports("{}", &HttpPortSelection::ChooseLowest),
            0
        );
        assert_eq!(
            parse_docker_ports("null", &HttpPortSelection::ChooseLowest),
            0
        );
    }

    #[test]
    fn test_parse_docker_ports_no_bindings() {
        let json = r#"{"8080/tcp":null}"#;
        assert_eq!(
            parse_docker_ports(json, &HttpPortSelection::ChooseLowest),
            0
        );
    }

    #[test]
    fn test_service_source_display_process() {
        let src = ServiceSource::Process {
            cwd: Some(PathBuf::from("/tmp/myapp")),
        };
        let display = format!("{}", src);
        assert!(display.contains("/tmp/myapp"));
    }

    #[test]
    fn test_service_source_display_container() {
        let src = ServiceSource::Container {
            id: "abc123".to_string(),
            name: "myapp-postgres-1".to_string(),
        };
        assert_eq!(format!("{}", src), "container myapp-postgres-1");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_proc_net_tcp_listen_line_public() {
        // 0A = LISTEN, local addr 00000000:1F90 = 0.0.0.0:8080, inode = 12345
        let line = "   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0";
        let mut inodes = std::collections::HashSet::new();
        inodes.insert(12345u64);
        let result = parse_proc_net_tcp_line(line, &inodes);
        assert!(result.is_some());
        let p = result.unwrap();
        assert_eq!(p.port, 8080);
        assert_eq!(p.bind, BindAddr::Public);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_proc_net_tcp_listen_line_loopback() {
        // local addr 0100007F:1F90 = 127.0.0.1:8080
        let line = "   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 99 1 0000000000000000 100 0 0 10 0";
        let mut inodes = std::collections::HashSet::new();
        inodes.insert(99u64);
        let result = parse_proc_net_tcp_line(line, &inodes);
        assert!(result.is_some());
        let p = result.unwrap();
        assert_eq!(p.port, 8080);
        assert_eq!(p.bind, BindAddr::Loopback);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_proc_net_tcp_wrong_inode_skipped() {
        let line = "   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0";
        let inodes = std::collections::HashSet::new(); // empty — doesn't own this socket
        assert!(parse_proc_net_tcp_line(line, &inodes).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_proc_net_tcp_established_skipped() {
        // state 01 = ESTABLISHED, not LISTEN
        let line = "   1: 0100007F:1F90 0100007F:C000 01 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0";
        let mut inodes = std::collections::HashSet::new();
        inodes.insert(12345u64);
        assert!(parse_proc_net_tcp_line(line, &inodes).is_none());
    }
}
