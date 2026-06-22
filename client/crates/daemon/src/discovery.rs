//! Service discovery: scan processes and Docker containers for DEVENV_TUNNEL.
//!
//! The value of DEVENV_TUNNEL must be a full domain name (after template
//! substitution). No implicit suffixes are added.
//!
//! - If it ends with `.devenv.local` (or `.local`) → local virtual overlay.
//! - Otherwise it must be a valid tunnel domain ending in `.tunnel.devenv.tools`
//!   (or the configured base, supporting namespacing like `api.alice.tunnel...`).
//!
//! Examples (full names required):
//!   DEVENV_TUNNEL=my-api.alice.tunnel.devenv.tools
//!   DEVENV_TUNNEL=my-db-{branch}.devenv.local
//!   DEVENV_TUNNEL=web-{branch}.tunnel.devenv.tools
//!
//! Cross-platform support:
//! - Linux: full (reads /proc/<pid>/environ and /proc/<pid>/fd for inode-based port scoping)
//! - macOS: full (uses `ps -p <pid> -wwwE` and `lsof`)
//! - Windows: containers only

use std::path::{Path, PathBuf};

use anyhow::Result;
use devenv_tunnel_domain::{validate_tunnel_domain, DomainContext};
use sysinfo::System;

/// The single environment variable used to tag services.
///
/// The value (after substitution) must be a full domain name:
/// - `*.devenv.local` → local overlay
/// - `*.(username.)tunnel.devenv.tools` (or configured base) → cloud tunnel
///
/// No implicit suffix is ever appended.
const ENV_VAR_NAME: &str = "DEVENV_TUNNEL";

/// Selects which HTTP port to forward. Defaults to CHOOSE_LOWEST if unset.
const ENV_HTTP_PORT_VAR: &str = "DEVENV_TUNNEL_HTTP_PORT";

/// Additional raw port mappings: `local:tunnel[;local:tunnel...]`
const ENV_PORTS_VAR: &str = "DEVENV_TUNNEL_PORTS";

/// Returns true if the (full) resolved name indicates the local virtual overlay
/// (must end with .devenv.local or .local).
///
/// The value in DEVENV_TUNNEL must be the complete name; no suffix is added.
pub fn is_local_overlay_domain(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n.ends_with(".devenv.local") || n == "devenv.local" || n.ends_with(".local")
}

/// Extract the label for the overlay from a full name like "my-db.devenv.local".
/// The input must already be a full domain (no implicit suffix added by us).
pub fn extract_local_label(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    let core = n
        .strip_suffix(".devenv.local")
        .or_else(|| n.strip_suffix(".local"))
        .unwrap_or(&n);
    let label = core.split('.').next().unwrap_or(core);
    sanitize_network_name(label)
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A discovered service with its domain, port, and origin.
#[derive(Debug, Clone)]
pub struct DiscoveredService {
    /// Resolved full DEVENV_TUNNEL value (must be a complete domain name).
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

        if is_local_overlay_domain(&domain) {
            tracing::debug!(
                pid = pid_u32,
                domain,
                "DEVENV_TUNNEL value ends in .local — routing to overlay path"
            );
            continue;
        }

        // For cloud tunnels we require a full domain name (no implicit suffix).
        if let Err(e) = validate_tunnel_domain(&domain) {
            tracing::warn!(
                pid = pid_u32,
                domain,
                error = %e,
                "DEVENV_TUNNEL value is not a valid full tunnel domain (and not .local). \
                 Provide the full name including suffix, e.g. my-api.alice.tunnel.devenv.tools"
            );
            continue;
        }

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
            "{{json .Config.Env}}||{{.Name}}||{{json .NetworkSettings.Ports}}||{{.State.Pid}}||{{json .Mounts}}||{{json .Config.Labels}}",
        ])
        .output()?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();

    let parts: Vec<&str> = stdout.splitn(6, "||").collect();
    if parts.len() < 4 {
        return Ok(None);
    }

    let env_json = parts[0];
    let name = parts[1].trim_start_matches('/');
    let ports_json = parts[2];
    let container_pid: u32 = parts[3].parse().unwrap_or(0);
    let mounts_json = if parts.len() > 4 { parts[4] } else { "[]" };
    let labels_json = if parts.len() > 5 { parts[5] } else { "{}" };

    let env_vars: Vec<String> = serde_json::from_str(env_json).unwrap_or_default();

    let raw = env_vars
        .iter()
        .find_map(|e| e.strip_prefix("DEVENV_TUNNEL="))
        .map(|v| v.to_string());

    let raw = match raw {
        Some(d) if !d.is_empty() => d,
        _ => return Ok(None),
    };

    // Discover a host-side project directory for template resolution (branch, worktree, etc.)
    // This makes DEVENV_TUNNEL=my-service-{branch} work for `docker run` (via bind mounts)
    // and `docker compose` (via labels + mounts).
    let project_dir = find_host_project_dir_for_container(mounts_json, labels_json);

    let domain = resolve_tunnel_template(&raw, project_dir.as_deref(), account_id, username);

    if is_local_overlay_domain(&domain) {
        tracing::debug!(
            container = name,
            domain,
            "value ends in .local — routing to overlay path"
        );
        return Ok(None);
    }

    if let Err(e) = validate_tunnel_domain(&domain) {
        tracing::warn!(
            container = name,
            domain,
            error = %e,
            "DEVENV_TUNNEL value on container is not a valid full tunnel domain (and not .local). \
             Use a full name like web-mybranch.tunnel.devenv.tools"
        );
        return Ok(None);
    }

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

/// Try to recover a host-side git project directory from a container's mounts
/// and labels. This enables correct `{branch}` / `{worktree}` resolution for
/// `DEVENV_TUNNEL` when using plain `docker run -v ...` or `docker compose`.
fn find_host_project_dir_for_container(mounts_json: &str, labels_json: &str) -> Option<PathBuf> {
    use devenv_tunnel_domain::find_git_project_dir;

    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. Docker Compose working dir label (very reliable when using compose)
    if let Ok(labels) = serde_json::from_str::<serde_json::Value>(labels_json) {
        if let Some(wd) = labels
            .get("com.docker.compose.project.working_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
        {
            candidates.push(wd);
        }
        // Also check other common labels users might set
        if let Some(custom) = labels.get("dev.devenv.project_dir").and_then(|v| v.as_str()) {
            candidates.push(PathBuf::from(custom));
        }
    }

    // 2. Bind mounts from the host (works for both compose and plain docker run -v)
    if let Ok(mounts) = serde_json::from_str::<serde_json::Value>(mounts_json) {
        if let Some(arr) = mounts.as_array() {
            for mount in arr {
                if mount.get("Type").and_then(|t| t.as_str()) == Some("bind") {
                    if let Some(source) = mount.get("Source").and_then(|s| s.as_str()) {
                        candidates.push(PathBuf::from(source));
                    }
                }
            }
        }
    }

    // Convert to slices for the domain helper
    let refs: Vec<&Path> = candidates.iter().map(|p| p.as_path()).collect();
    find_git_project_dir(&refs)
}

// ---------------------------------------------------------------------------
// Overlay network discovery (DEVENV_TUNNEL=*.devenv.local + port 0 support)
// ---------------------------------------------------------------------------

/// A service discovered for the local virtual overlay network.
#[derive(Debug, Clone)]
pub struct DiscoveredNetworkService {
    /// The label extracted from the DEVENV_TUNNEL value (e.g. "my-db"
    /// from "my-db.devenv.local").
    pub name: String,
    /// The actual host address we must proxy to (usually 127.0.0.1:random).
    pub real_addr: std::net::SocketAddr,
    /// The port that should be presented on the virtual side (e.g. 5432).
    /// For Docker this is the container port from the -p mapping.
    /// For plain processes we use the discovered listening port.
    pub service_port: u16,
    /// Owning PID.
    pub pid: u32,
    pub source: ServiceSource,
}

/// Scan for services that should participate in the virtual overlay.
///
/// Only `DEVENV_TUNNEL` values whose resolved name ends with `.devenv.local`
/// (or `.local`) are accepted. The full name must be provided (no implicit
/// suffix).
///
/// Supports templating, e.g.:
///   DEVENV_TUNNEL=my-db-{branch}.devenv.local
///   DEVENV_TUNNEL={service}-{worktree}.devenv.local
///
/// The label (left part) gets a stable virtual IP under .devenv.local.
/// The daemon should surface loud errors if the same name is claimed by
/// multiple distinct worktrees.
pub async fn scan_network_services() -> Vec<DiscoveredNetworkService> {
    let mut out = Vec::new();
    out.extend(scan_network_processes());
    out.extend(scan_network_containers().await);
    out
}

fn scan_network_processes() -> Vec<DiscoveredNetworkService> {
    use std::net::{IpAddr, SocketAddr};

    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let mut results = Vec::new();

    for (pid, process) in sys.processes() {
        let pid_u32 = pid.as_u32();
        if pid_u32 <= 1 {
            continue;
        }

        let raw = match scan_process_env(pid_u32, ENV_VAR_NAME) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };

        let process_cwd = process.cwd().map(|p| p.to_path_buf());
        let resolved = resolve_tunnel_template(&raw, process_cwd.as_deref(), None, None);

        // For the overlay we require an explicit .local suffix.
        // Bare names under DEVENV_TUNNEL go to the tunnel path.
        if !is_local_overlay_domain(&resolved) {
            continue;
        }

        let label = extract_local_label(&resolved);
        if label.is_empty() {
            continue;
        }

        let listening = discover_process_ports(pid_u32);
        if listening.is_empty() {
            continue;
        }

        // Pick a reasonable real port. Prefer a public one.
        let chosen = listening
            .iter()
            .find(|lp| lp.bind == BindAddr::Public)
            .or_else(|| listening.first());

        let port = match chosen {
            Some(lp) => lp.port,
            None => continue,
        };

        let real_addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);

        results.push(DiscoveredNetworkService {
            name: label,
            real_addr,
            service_port: port,
            pid: pid_u32,
            source: ServiceSource::Process {
                cwd: process_cwd,
            },
        });
    }

    results
}

async fn scan_network_containers() -> Vec<DiscoveredNetworkService> {
    match scan_network_containers_impl().await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("Docker network scan failed: {}", e);
            Vec::new()
        }
    }
}

async fn scan_network_containers_impl() -> anyhow::Result<Vec<DiscoveredNetworkService>> {
    use std::net::{IpAddr, SocketAddr};
    use std::process::Command;

    let output = Command::new("docker")
        .args(["ps", "--format", "{{.ID}}"])
        .output()?;

    if !output.status.success() {
        anyhow::bail!("docker ps failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let ids: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .collect();

    let mut out = Vec::new();

    for id in ids {
        let inspect = Command::new("docker")
            .args([
                "inspect",
                id,
                "--format",
                "{{json .Config.Env}}||{{.Name}}||{{json .NetworkSettings.Ports}}||{{.State.Pid}}||{{json .Mounts}}||{{json .Config.Labels}}",
            ])
            .output();

        let output = match inspect {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = stdout.trim().splitn(6, "||").collect();
        if parts.len() < 4 {
            continue;
        }

        let env_json = parts[0];
        let _name = parts[1].trim_start_matches('/');
        let ports_json = parts[2];
        let pid: u32 = parts[3].parse().unwrap_or(0);
        let mounts_json = if parts.len() > 4 { parts[4] } else { "[]" };
        let labels_json = if parts.len() > 5 { parts[5] } else { "{}" };

        let envs: Vec<String> = match serde_json::from_str(env_json) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only DEVENV_TUNNEL is used. Overlay participation requires the value
        // to end with .devenv.local (pure suffix-based detection).
        let raw_name = envs
            .iter()
            .find_map(|e| e.strip_prefix("DEVENV_TUNNEL="))
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let raw_name = match raw_name {
            Some(s) => s,
            None => continue,
        };

        // Use host-side git context (mounts/labels) for templating.
        let project_dir = find_host_project_dir_for_container(mounts_json, labels_json);
        let resolved_name = resolve_tunnel_template(&raw_name, project_dir.as_deref(), None, None);

        if !is_local_overlay_domain(&resolved_name) {
            continue;
        }

        let label = extract_local_label(&resolved_name);
        if label.is_empty() {
            continue;
        }

        // Parse docker ports to find a (container_port -> host_port) pair.
        // We want the container port as service_port.
        let (service_port, host_port) = parse_docker_port_mapping(ports_json).unwrap_or((0, 0));
        if host_port == 0 {
            // No published ports or still port 0 not yet assigned.
            continue;
        }

        let real_addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), host_port);
        let svc_port = if service_port > 0 { service_port } else { host_port };

        let container_name = label.clone();
        out.push(DiscoveredNetworkService {
            name: label,
            real_addr,
            service_port: svc_port,
            pid,
            source: ServiceSource::Container {
                id: id.to_string(),
                name: container_name,
            },
        });
    }

    Ok(out)
}

/// Very small parser for the docker ports JSON.
/// Returns (container_port, host_port) for the first mapped entry.
fn parse_docker_port_mapping(ports_json: &str) -> Option<(u16, u16)> {
    let val: serde_json::Value = serde_json::from_str(ports_json).ok()?;
    let obj = val.as_object()?;
    for (container_port_key, bindings) in obj {
        // container_port_key looks like "5432/tcp"
        let container_port: u16 = container_port_key
            .split('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if let Some(arr) = bindings.as_array() {
            for b in arr {
                if let Some(hp) = b.get("HostPort").and_then(|v| v.as_str()).and_then(|s| s.parse::<u16>().ok()) {
                    if hp > 0 {
                        return Some((container_port, hp));
                    }
                }
            }
        }
    }
    None
}

fn sanitize_network_name(raw: &str) -> String {
    // Allow only dns-safe simple labels for the network name.
    raw.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string()
        .to_lowercase()
        .chars()
        .take(63)
        .collect()
}

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
