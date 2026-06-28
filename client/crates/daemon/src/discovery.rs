//! Service discovery: scan processes and Docker containers for PZ_TUNNEL.
//!
//! The value of PZ_TUNNEL must be a full domain name (after template
//! substitution). No implicit suffixes are added.
//!
//! - If it ends with `.portzero.local` (or `.local`) → local virtual overlay.
//! - Otherwise it must be a valid tunnel domain ending in `.tunnel.portzero.cloud`
//!   (or the configured base, supporting namespacing like `api.alice.tunnel...`).
//!
//! Examples (full names required):
//!   PZ_TUNNEL=my-api.alice.tunnel.portzero.cloud
//!   PZ_TUNNEL=my-db-{branch}.portzero.local
//!   PZ_TUNNEL=web-{branch}.tunnel.portzero.cloud
//!
//! Cross-platform support:
//! - Linux: full (reads /proc/<pid>/environ and /proc/<pid>/fd for inode-based port scoping)
//! - macOS: full (uses `ps -p <pid> -wwwE` and `lsof`)
//! - Windows: full for same-bitness/readable processes (reads the target PEB
//!   environment block and uses `Get-NetTCPConnection` for port ownership)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use portzero_domain::{split_tunnel_port, validate_tunnel_domain, DomainContext};
use sysinfo::System;

use crate::protocol_detect;

/// The single environment variable used to tag services.
///
/// The value (after substitution) must be a full domain name:
/// - `*.portzero.local` → local overlay
/// - `*.(username.)tunnel.portzero.cloud` (or configured base) → cloud tunnel
///
/// No implicit suffix is ever appended.
const ENV_VAR_NAME: &str = "PZ_TUNNEL";

/// Selects which HTTP port to forward. Defaults to CHOOSE_LOWEST if unset.
const ENV_HTTP_PORT_VAR: &str = "PZ_TUNNEL_HTTP_PORT";

/// Additional raw port mappings: `local:tunnel[;local:tunnel...]`
const ENV_PORTS_VAR: &str = "PZ_TUNNEL_PORTS";

/// Opt-out for active protocol probing (task-17). When set on the target
/// process (or on the daemon's own env) probing is skipped entirely.
const ENV_NO_PROBE_VAR: &str = "PZ_TUNNEL_NO_PROBE";

/// Returns true if the (full) resolved name indicates the local virtual overlay
/// (must end with .portzero.local or .local).
///
/// The value in PZ_TUNNEL must be the complete name; no suffix is added.
pub fn is_local_overlay_domain(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n.ends_with(".portzero.local") || n == "portzero.local" || n.ends_with(".local")
}

/// Extract the label for the overlay from a full name like "my-db.portzero.local".
/// The input must already be a full domain (no implicit suffix added by us).
pub fn extract_local_label(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    let core = n
        .strip_suffix(".portzero.local")
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
    /// Resolved full PZ_TUNNEL value (must be a complete domain name).
    pub domain: String,
    /// Original PZ_TUNNEL domain value before template substitution.
    pub domain_template: String,
    /// Values available for template substitution when this service was found.
    pub substitutions: BTreeMap<String, String>,
    /// Selected HTTP port (0 if not determinable).
    pub port: u16,
    /// Additional port mappings from PZ_TUNNEL_PORTS.
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListeningPort {
    port: u16,
    bind: BindAddr,
}

/// How the HTTP port is chosen when PZ_TUNNEL_HTTP_PORT is set.
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

/// Scan all processes and Docker containers for PZ_TUNNEL.
pub async fn scan_all(account_id: Option<&str>, username: Option<&str>) -> Vec<DiscoveredService> {
    let mut services = Vec::new();
    // `scan_processes` does a full sysinfo refresh and shells out (`ps`/`lsof`)
    // for every process system-wide, all synchronously. Run it on a blocking
    // thread so the async task yields control — otherwise the executor thread is
    // held for the whole (potentially multi-second) scan and the daemon's
    // shutdown-signal future never gets polled mid-scan.
    let acct = account_id.map(str::to_owned);
    let user = username.map(str::to_owned);
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::task::spawn_blocking(move || scan_processes(acct.as_deref(), user.as_deref())),
    )
    .await
    {
        Ok(Ok(process_services)) => services.extend(process_services),
        Ok(Err(_)) => tracing::debug!("process scan panicked"),
        Err(_) => tracing::debug!("process scan timed out"),
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        scan_docker_containers(account_id, username),
    )
    .await
    {
        Ok(docker_services) => services.extend(docker_services),
        Err(_) => tracing::debug!("Docker container scan timed out"),
    }
    services
}

// ---------------------------------------------------------------------------
// Process scanning
// ---------------------------------------------------------------------------

fn scan_processes(account_id: Option<&str>, username: Option<&str>) -> Vec<DiscoveredService> {
    #[cfg(target_os = "windows")]
    {
        scan_processes_windows(account_id, username)
    }

    #[cfg(not(target_os = "windows"))]
    {
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
            // Strip an optional canonical `:port` before classification/validation.
            // On the cloud path the edge assigns the URL, so the port is ignored.
            let (raw_domain, _canonical_port) = split_tunnel_port(&raw);
            warn_if_port_like_rejected(&raw, _canonical_port, pid_u32);
            let substitutions = template_substitutions(cwd.as_deref(), account_id, username, None);
            let domain = resolve_tunnel_template(raw_domain, cwd.as_deref(), account_id, username);

            if is_local_overlay_domain(&domain) {
                tracing::debug!(
                    pid = pid_u32,
                    domain,
                    "PZ_TUNNEL value ends in .local — routing to overlay path"
                );
                continue;
            }

            // For cloud tunnels we require a full domain name (no implicit suffix).
            if let Err(e) = validate_tunnel_domain(&domain) {
                tracing::warn!(
                    pid = pid_u32,
                    domain,
                    error = %e,
                    "PZ_TUNNEL value is not a valid full tunnel domain (and not .local). \
                     Provide the full name including suffix, e.g. my-api.alice.tunnel.portzero.cloud"
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
                        "PZ_TUNNEL_HTTP_PORT specifies a port not owned by this process; ignoring"
                    );
                    continue;
                }
                // No listening ports on this process. This is expected when a
                // parent shell or launcher sets PZ_TUNNEL so that a child
                // process inherits it — the parent itself has nothing to forward.
                SelectedPort::NoneListening => {
                    tracing::debug!(
                        pid = pid_u32,
                        "skipping process with PZ_TUNNEL but no listening ports \
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
                        "PZ_TUNNEL_PORTS entry references a port not owned by this process; skipping"
                    );
                    false
                }
            })
            .collect();

            services.push(DiscoveredService {
                domain,
                domain_template: raw_domain.to_string(),
                substitutions,
                port,
                extra_ports,
                pid: pid_u32,
                source: ServiceSource::Process { cwd },
            });
        }

        services
    }
}

#[cfg(target_os = "windows")]
fn scan_processes_windows(
    account_id: Option<&str>,
    username: Option<&str>,
) -> Vec<DiscoveredService> {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let listening_by_pid = discover_all_ports_windows_by_pid();
    let mut services = Vec::new();

    for (pid_u32, listening) in listening_by_pid {
        if pid_u32 <= 1 || listening.is_empty() {
            continue;
        }

        let raw = match scan_process_env(pid_u32, ENV_VAR_NAME) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };

        let cwd = sys
            .process(sysinfo::Pid::from_u32(pid_u32))
            .and_then(|process| process.cwd().map(|p| p.to_path_buf()));
        let (raw_domain, _canonical_port) = split_tunnel_port(&raw);
        warn_if_port_like_rejected(&raw, _canonical_port, pid_u32);
        let substitutions = template_substitutions(cwd.as_deref(), account_id, username, None);
        let domain = resolve_tunnel_template(raw_domain, cwd.as_deref(), account_id, username);

        if is_local_overlay_domain(&domain) {
            tracing::debug!(
                pid = pid_u32,
                domain,
                "PZ_TUNNEL value ends in .local — routing to overlay path"
            );
            continue;
        }

        if let Err(e) = validate_tunnel_domain(&domain) {
            tracing::warn!(
                pid = pid_u32,
                domain,
                error = %e,
                "PZ_TUNNEL value is not a valid full tunnel domain (and not .local). \
                 Provide the full name including suffix, e.g. my-api.alice.tunnel.portzero.cloud"
            );
            continue;
        }

        let http_selection = scan_process_env(pid_u32, ENV_HTTP_PORT_VAR)
            .map(|v| parse_http_port_selection(&v))
            .unwrap_or(HttpPortSelection::ChooseLowest);

        let extra_ports = scan_process_env(pid_u32, ENV_PORTS_VAR)
            .map(|v| parse_extra_ports(&v))
            .unwrap_or_default();

        let port = match select_http_port(&listening, &http_selection) {
            SelectedPort::Found(p) => p,
            SelectedPort::ExplicitNotOwned(requested) => {
                tracing::warn!(
                    pid = pid_u32,
                    requested_port = requested,
                    "PZ_TUNNEL_HTTP_PORT specifies a port not owned by this process; ignoring"
                );
                continue;
            }
            SelectedPort::NoneListening => continue,
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
                        "PZ_TUNNEL_PORTS entry references a port not owned by this process; skipping"
                    );
                    false
                }
            })
            .collect();

        services.push(DiscoveredService {
            domain,
            domain_template: raw_domain.to_string(),
            substitutions,
            port,
            extra_ports,
            pid: pid_u32,
            source: ServiceSource::Process { cwd },
        });
    }

    services
}

/// Emit a warning when a `PZ_TUNNEL` value had a trailing `:something` that
/// LOOKED like a canonical port but was rejected by [`split_tunnel_port`]
/// (out of range or zero — i.e. an all-digit segment that is not `1..=65535`).
///
/// We deliberately only warn on the all-numeric case: a non-numeric trailing
/// segment after `:` is almost certainly part of the value itself (or a typo we
/// can't distinguish), and warning on it would be noisy. When the port parsed
/// fine (`canonical` is `Some`) we say nothing.
fn warn_if_port_like_rejected(raw: &str, canonical: Option<u16>, pid: u32) {
    if canonical.is_some() {
        return;
    }
    if let Some((_, tail)) = raw.rsplit_once(':') {
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            tracing::warn!(
                pid,
                value = raw,
                rejected_port = tail,
                "PZ_TUNNEL has a trailing ':<port>' that is not a valid port \
                 (must be 1..=65535); ignoring the port and using the whole value as the domain"
            );
        }
    }
}

/// Resolve template variables in a PZ_TUNNEL value.
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

fn template_substitutions(
    project_dir: Option<&Path>,
    account_id: Option<&str>,
    username: Option<&str>,
    source_name: Option<&str>,
) -> BTreeMap<String, String> {
    let dir = project_dir.unwrap_or(Path::new("."));
    let ctx = DomainContext::from_environment("", dir, account_id, username);
    let folder_name = source_name
        .map(|s| s.trim_start_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            project_dir
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    let mut values = BTreeMap::new();
    values.insert("branch".to_string(), ctx.branch);
    values.insert(
        "worktree".to_string(),
        ctx.worktree.unwrap_or_else(|| "unknown".to_string()),
    );
    values.insert("folder-name".to_string(), folder_name);
    values.insert("project".to_string(), ctx.project);
    values.insert("user".to_string(), ctx.user);
    values.insert("machine".to_string(), ctx.machine);
    values.insert(
        "uid".to_string(),
        ctx.uid.unwrap_or_else(|| "unknown".to_string()),
    );
    values.insert(
        "username".to_string(),
        ctx.username.unwrap_or_else(|| "unknown".to_string()),
    );
    values
}

// ---------------------------------------------------------------------------
// Port selection logic
// ---------------------------------------------------------------------------

/// Parse PZ_TUNNEL_HTTP_PORT into a selection strategy.
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

/// Parse PZ_TUNNEL_PORTS into a list of port mappings.
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

    #[cfg(target_os = "windows")]
    {
        return discover_ports_windows(pid);
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

    // `-a` ANDs the selection criteria. Without it, lsof ORs `-iTCP -sTCP:LISTEN`
    // with `-p <pid>`, returning EVERY listening TCP socket on the system unioned
    // with this pid's sockets — so a process would appear to listen on ports it
    // doesn't own (e.g. sshd's *:22), mismapping the service backend.
    let output = match Command::new("lsof")
        .args(["-a", "-iTCP", "-sTCP:LISTEN", "-nP", "-p", &pid.to_string()])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_lsof_stdout(&stdout)
}

/// Parse the full stdout of `lsof -a -iTCP -sTCP:LISTEN -nP -p <pid>` into the
/// set of listening ports. Pure (no process spawning) so it is unit-testable.
///
/// The first line is the `lsof` header (`COMMAND PID USER …`) and is skipped.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn parse_lsof_stdout(stdout: &str) -> Vec<ListeningPort> {
    stdout.lines().skip(1).filter_map(parse_lsof_line).collect()
}

/// Parse a single `lsof` output line into a [`ListeningPort`], if it carries a
/// listening address/port.
///
/// With `-sTCP:LISTEN` the NAME column is printed as e.g.
/// `127.0.0.1:50706 (LISTEN)`, so the trailing whitespace token is `(LISTEN)`
/// rather than the address. We therefore scan every whitespace token and pick
/// the address token = the one whose `rsplit_once(':')` yields a parseable
/// `u16` port. This naturally skips `(LISTEN)`, `TCP`, `0t0`, and other columns.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn parse_lsof_line(line: &str) -> Option<ListeningPort> {
    for token in line.split_whitespace() {
        // token is like "*:8080", "127.0.0.1:50706", "[::]:8080", "[::1]:57889"
        let Some((addr, port_str)) = token.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let bind = if addr == "*" || addr == "0.0.0.0" || addr == "[::]" {
            BindAddr::Public
        } else {
            BindAddr::Loopback
        };
        return Some(ListeningPort { port, bind });
    }
    None
}

#[cfg(target_os = "windows")]
fn discover_ports_windows(pid: u32) -> Vec<ListeningPort> {
    use std::process::Command;

    let script = format!(
        r#"
$ErrorActionPreference = 'SilentlyContinue'
Get-NetTCPConnection -State Listen -OwningProcess {pid} |
  ForEach-Object {{ "$($_.LocalAddress)|$($_.LocalPort)" }}
"#
    );

    let output = match Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return discover_ports_windows_netstat(pid);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let ports = parse_windows_tcp_connection_stdout(&stdout);
    if ports.is_empty() {
        discover_ports_windows_netstat(pid)
    } else {
        ports
    }
}

#[cfg(target_os = "windows")]
fn discover_ports_windows_netstat(pid: u32) -> Vec<ListeningPort> {
    use std::process::Command;

    let output = match Command::new("netstat").args(["-ano", "-p", "tcp"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_windows_netstat_stdout(&stdout, pid)
}

#[cfg(target_os = "windows")]
fn discover_all_ports_windows_by_pid() -> std::collections::HashMap<u32, Vec<ListeningPort>> {
    use std::process::Command;

    let output = match Command::new("netstat").args(["-ano", "-p", "tcp"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return std::collections::HashMap::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_windows_netstat_stdout_by_pid(&stdout)
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_windows_tcp_connection_stdout(stdout: &str) -> Vec<ListeningPort> {
    stdout
        .lines()
        .filter_map(parse_windows_tcp_connection_line)
        .collect()
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_windows_tcp_connection_line(line: &str) -> Option<ListeningPort> {
    let (addr, port_str) = line.trim().split_once('|')?;
    let port = port_str.trim().parse::<u16>().ok()?;
    if port == 0 {
        return None;
    }

    let addr = addr.trim();
    let bind = if addr == "0.0.0.0" || addr == "::" || addr == "*" {
        BindAddr::Public
    } else {
        BindAddr::Loopback
    };

    Some(ListeningPort { port, bind })
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_windows_netstat_stdout(stdout: &str, pid: u32) -> Vec<ListeningPort> {
    stdout
        .lines()
        .filter_map(|line| parse_windows_netstat_line(line, pid))
        .collect()
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_windows_netstat_stdout_by_pid(
    stdout: &str,
) -> std::collections::HashMap<u32, Vec<ListeningPort>> {
    let mut out: std::collections::HashMap<u32, Vec<ListeningPort>> =
        std::collections::HashMap::new();
    for line in stdout.lines() {
        if let Some((pid, port)) = parse_windows_netstat_line_any_pid(line) {
            out.entry(pid).or_default().push(port);
        }
    }
    out
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_windows_netstat_line(line: &str, pid: u32) -> Option<ListeningPort> {
    let (line_pid, port) = parse_windows_netstat_line_any_pid(line)?;
    if line_pid == pid {
        Some(port)
    } else {
        None
    }
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_windows_netstat_line_any_pid(line: &str) -> Option<(u32, ListeningPort)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 5 || !parts[0].eq_ignore_ascii_case("TCP") {
        return None;
    }
    if !parts[3].eq_ignore_ascii_case("LISTENING") {
        return None;
    }

    let pid = parts[4].parse::<u32>().ok()?;
    let port = parse_windows_local_address_port(parts[1])?;
    Some((pid, port))
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_windows_local_address_port(local: &str) -> Option<ListeningPort> {
    let (addr, port_str) = if let Some(rest) = local.strip_prefix('[') {
        let (addr, tail) = rest.split_once("]:")?;
        (addr, tail)
    } else {
        local.rsplit_once(':')?
    };

    let port = port_str.trim().parse::<u16>().ok()?;
    if port == 0 {
        return None;
    }

    let bind = if addr == "0.0.0.0" || addr == "::" || addr == "*" {
        BindAddr::Public
    } else {
        BindAddr::Loopback
    };

    Some(ListeningPort { port, bind })
}

// ---------------------------------------------------------------------------
// System-wide listener enumeration (for legacy-port monitoring)
// ---------------------------------------------------------------------------

/// A TCP listener observed system-wide, attributed to its owning process.
///
/// Used by the legacy-port monitor to find processes that serve a port directly
/// (bypassing port-zero). Carries enough context (pid, cwd, whether
/// `PZ_TUNNEL` is set) for the monitor's *pure* comparison logic to decide
/// whether the listener is "legacy".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemListener {
    /// The TCP port being listened on.
    pub port: u16,
    /// Owning process id.
    pub pid: u32,
    /// Working directory of the owning process, if discoverable.
    pub cwd: Option<PathBuf>,
    /// Whether the owning process has `PZ_TUNNEL` set (i.e. it is already
    /// managed by us and must NOT be flagged as legacy).
    pub has_port_zero: bool,
}

/// Enumerate every process's listening TCP ports system-wide, attributed to the
/// owning process (pid, cwd, whether `PZ_TUNNEL` is set).
///
/// This is the single public entry point the legacy-port monitor uses; it
/// reuses the existing per-OS [`discover_process_ports`] enumeration rather than
/// duplicating any netstat/lsof/proc parsing. Best-effort and cross-platform:
/// on platforms where per-process port discovery is unavailable it simply
/// returns an empty list.
pub fn enumerate_system_listeners() -> Vec<SystemListener> {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let mut out = Vec::new();
    for (pid, process) in sys.processes() {
        let pid_u32 = pid.as_u32();
        if pid_u32 <= 1 {
            continue;
        }

        let listening = discover_process_ports(pid_u32);
        if listening.is_empty() {
            continue;
        }

        let cwd = process.cwd().map(|p| p.to_path_buf());
        let has_port_zero = scan_process_env(pid_u32, ENV_VAR_NAME)
            .map(|v| !v.is_empty())
            .unwrap_or(false);

        for lp in listening {
            out.push(SystemListener {
                port: lp.port,
                pid: pid_u32,
                cwd: cwd.clone(),
                has_port_zero,
            });
        }
    }

    out
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
    #[cfg(target_os = "windows")]
    {
        scan_process_env_windows(pid, var_name)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
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

#[cfg(target_os = "windows")]
fn scan_process_env_windows(pid: u32, var_name: &str) -> Option<String> {
    let entries = read_windows_process_environment(pid).ok()?;
    let prefix = format!("{var_name}=");
    entries.into_iter().find_map(|entry| {
        if entry.len() >= prefix.len() && entry[..prefix.len()].eq_ignore_ascii_case(&prefix) {
            Some(entry[prefix.len()..].to_string())
        } else {
            None
        }
    })
}

#[cfg(target_os = "windows")]
fn read_windows_process_environment(pid: u32) -> Result<Vec<String>> {
    use anyhow::Context;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    #[repr(C)]
    struct ProcessBasicInformation {
        reserved1: *mut c_void,
        peb_base_address: *mut c_void,
        reserved2: [*mut c_void; 2],
        unique_process_id: usize,
        reserved3: *mut c_void,
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            process_handle: HANDLE,
            process_information_class: u32,
            process_information: *mut c_void,
            process_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    const PROCESS_BASIC_INFORMATION_CLASS: u32 = 0;
    const STATUS_SUCCESS: i32 = 0;

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    unsafe fn read_usize(process: HANDLE, address: usize) -> Result<usize> {
        let mut value = 0usize;
        let mut bytes_read = 0usize;
        let ok = ReadProcessMemory(
            process,
            address as *const c_void,
            &mut value as *mut usize as *mut c_void,
            std::mem::size_of::<usize>(),
            &mut bytes_read,
        );
        if ok == 0 || bytes_read != std::mem::size_of::<usize>() {
            anyhow::bail!("ReadProcessMemory failed at 0x{address:x}");
        }
        Ok(value)
    }

    unsafe fn read_env_block(process: HANDLE, address: usize) -> Result<Vec<u16>> {
        const CHUNK_BYTES: usize = 4096;
        const MAX_BYTES: usize = 4 * 1024 * 1024;

        let mut bytes = Vec::new();
        let mut offset = 0usize;
        while offset < MAX_BYTES {
            let mut chunk = [0u8; CHUNK_BYTES];
            let mut bytes_read = 0usize;
            let ok = ReadProcessMemory(
                process,
                (address + offset) as *const c_void,
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len(),
                &mut bytes_read,
            );
            if ok == 0 || bytes_read == 0 {
                break;
            }

            bytes.extend_from_slice(&chunk[..bytes_read]);
            if bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| pair == [0, 0])
            {
                break;
            }

            offset += bytes_read;
        }

        let words = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        Ok(words)
    }

    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if handle.is_null() {
        anyhow::bail!("OpenProcess failed for pid {pid}");
    }
    let handle = Handle(handle);

    let mut pbi = ProcessBasicInformation {
        reserved1: std::ptr::null_mut(),
        peb_base_address: std::ptr::null_mut(),
        reserved2: [std::ptr::null_mut(); 2],
        unique_process_id: 0,
        reserved3: std::ptr::null_mut(),
    };
    let status = unsafe {
        NtQueryInformationProcess(
            handle.0,
            PROCESS_BASIC_INFORMATION_CLASS,
            &mut pbi as *mut ProcessBasicInformation as *mut c_void,
            std::mem::size_of::<ProcessBasicInformation>() as u32,
            std::ptr::null_mut(),
        )
    };
    if status != STATUS_SUCCESS {
        anyhow::bail!("NtQueryInformationProcess failed with status 0x{status:x}");
    }

    #[cfg(target_pointer_width = "64")]
    const PEB_PROCESS_PARAMETERS_OFFSET: usize = 0x20;
    #[cfg(target_pointer_width = "32")]
    const PEB_PROCESS_PARAMETERS_OFFSET: usize = 0x10;
    #[cfg(target_pointer_width = "64")]
    const RTL_ENVIRONMENT_OFFSET: usize = 0x80;
    #[cfg(target_pointer_width = "32")]
    const RTL_ENVIRONMENT_OFFSET: usize = 0x48;

    let peb = pbi.peb_base_address as usize;
    let process_parameters = unsafe { read_usize(handle.0, peb + PEB_PROCESS_PARAMETERS_OFFSET) }
        .context("reading PEB process parameters")?;
    if process_parameters == 0 {
        return Ok(Vec::new());
    }

    let environment = unsafe { read_usize(handle.0, process_parameters + RTL_ENVIRONMENT_OFFSET) }
        .context("reading process environment pointer")?;
    if environment == 0 {
        return Ok(Vec::new());
    }

    let words = unsafe { read_env_block(handle.0, environment) }
        .context("reading process environment block")?;
    Ok(parse_windows_environment_block(&words))
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_windows_environment_block(words: &[u16]) -> Vec<String> {
    let mut entries = Vec::new();
    let mut start = 0usize;
    for (idx, word) in words.iter().enumerate() {
        if *word != 0 {
            continue;
        }
        if idx == start {
            break;
        }
        entries.push(String::from_utf16_lossy(&words[start..idx]));
        start = idx + 1;
    }
    entries
}

// ---------------------------------------------------------------------------
// Docker container scanning
// ---------------------------------------------------------------------------

async fn scan_docker_containers(
    account_id: Option<&str>,
    username: Option<&str>,
) -> Vec<DiscoveredService> {
    if !command_on_path("docker") {
        return Vec::new();
    }

    let acct = account_id.map(str::to_owned);
    let user = username.map(str::to_owned);
    match tokio::task::spawn_blocking(move || {
        scan_docker_containers_impl(acct.as_deref(), user.as_deref())
    })
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("spawn_blocking panicked")))
    {
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

fn scan_docker_containers_impl(
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

    // State.Pid == 0 means the container is stopped/exited. This can happen in
    // a race between docker ps (lists running IDs) and docker inspect (runs
    // slightly later after the container stops). Ignore such containers.
    if container_pid == 0 {
        return Ok(None);
    }

    let env_vars: Vec<String> = serde_json::from_str(env_json).unwrap_or_default();

    let tunnel_prefix = format!("{ENV_VAR_NAME}=");
    let raw = env_vars
        .iter()
        .find_map(|e| e.strip_prefix(&tunnel_prefix))
        .map(|v| v.to_string());

    let raw = match raw {
        Some(d) if !d.is_empty() => d,
        _ => return Ok(None),
    };

    // Discover a host-side project directory for template resolution (branch, worktree, etc.)
    // This makes PZ_TUNNEL=my-service-{branch} work for `docker run` (via bind mounts)
    // and `docker compose` (via labels + mounts).
    let project_dir = find_host_project_dir_for_container(mounts_json, labels_json);

    // Strip an optional canonical `:port` before classification/validation.
    // On the cloud path the edge assigns the URL, so the port is ignored.
    let (raw_domain, canonical_port) = split_tunnel_port(&raw);
    warn_if_port_like_rejected(&raw, canonical_port, container_pid);
    let substitutions =
        template_substitutions(project_dir.as_deref(), account_id, username, Some(name));
    let domain = resolve_tunnel_template(raw_domain, project_dir.as_deref(), account_id, username);

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
            "PZ_TUNNEL value on container is not a valid full tunnel domain (and not .local). \
             Use a full name like web-mybranch.tunnel.portzero.cloud"
        );
        return Ok(None);
    }

    let http_selection = env_vars
        .iter()
        .find_map(|e| e.strip_prefix("PZ_TUNNEL_HTTP_PORT="))
        .map(parse_http_port_selection)
        .unwrap_or(HttpPortSelection::ChooseLowest);

    let extra_ports = env_vars
        .iter()
        .find_map(|e| e.strip_prefix("PZ_TUNNEL_PORTS="))
        .map(parse_extra_ports)
        .unwrap_or_default();

    let port = match http_selection {
        HttpPortSelection::Explicit(p) => p,
        _ => parse_docker_ports(ports_json, &http_selection),
    };

    Ok(Some(DiscoveredService {
        domain,
        domain_template: raw_domain.to_string(),
        substitutions,
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
/// `PZ_TUNNEL` when using plain `docker run -v ...` or `docker compose`.
fn find_host_project_dir_for_container(mounts_json: &str, labels_json: &str) -> Option<PathBuf> {
    use portzero_domain::find_git_project_dir;

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
        if let Some(custom) = labels
            .get("dev.portzero.project_dir")
            .and_then(|v| v.as_str())
        {
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
// Overlay network discovery (PZ_TUNNEL=*.portzero.local + port 0 support)
// ---------------------------------------------------------------------------

/// A service discovered for the local virtual overlay network.
#[derive(Debug, Clone)]
pub struct DiscoveredNetworkService {
    /// The label extracted from the PZ_TUNNEL value (e.g. "my-db"
    /// from "my-db.portzero.local").
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
    /// Original PZ_TUNNEL domain value before template substitution.
    pub domain_template: String,
    /// Values available for template substitution when this service was found.
    pub substitutions: BTreeMap<String, String>,
}

/// Scan for services that should participate in the virtual overlay.
///
/// Only `PZ_TUNNEL` values whose resolved name ends with `.portzero.local`
/// (or `.local`) are accepted. The full name must be provided (no implicit
/// suffix).
///
/// Supports templating, e.g.:
///   PZ_TUNNEL=my-db-{branch}.portzero.local
///   PZ_TUNNEL={service}-{worktree}.portzero.local
///
/// The label (left part) gets a stable virtual IP under .portzero.local.
/// The daemon should surface loud errors if the same name is claimed by
/// multiple distinct worktrees.
pub async fn scan_network_services(
    mgmt_store: &crate::management::RegistrationStore,
) -> Vec<DiscoveredNetworkService> {
    // Snapshot the registration store so we can filter out PIDs that have
    // already registered through the management API and synthesize overlay
    // entries for them instead.
    let registrations = mgmt_store.read().await.clone();
    let registered_pids: std::collections::HashSet<u32> = registrations.keys().copied().collect();

    let mut out = Vec::new();
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        scan_network_processes(&registered_pids),
    )
    .await
    {
        Ok(process_services) => out.extend(process_services),
        Err(_) => tracing::debug!("overlay process scan timed out"),
    }
    match tokio::time::timeout(std::time::Duration::from_secs(2), scan_network_containers()).await {
        Ok(container_services) => {
            // Also filter any container service that happens to share a PID
            // with a management-registered process.
            for svc in container_services {
                if !registered_pids.contains(&svc.pid) {
                    out.push(svc);
                }
            }
        }
        Err(_) => tracing::debug!("Docker network scan timed out"),
    }

    append_registered_network_services(&mut out, &registrations);

    out
}

/// Fast overlay-only scan used on first DNS hit for an unknown
/// `*.portzero.local` name.
///
/// This intentionally skips Docker and issue detection so a browser/curl lookup
/// can be satisfied as soon as the local process appears, while the normal
/// periodic scan still performs the full reconciliation.
pub async fn scan_network_process_services(
    mgmt_store: &crate::management::RegistrationStore,
) -> Option<Vec<DiscoveredNetworkService>> {
    let registrations = mgmt_store.read().await.clone();
    let registered_pids: std::collections::HashSet<u32> = registrations.keys().copied().collect();

    let mut out = match tokio::time::timeout(
        std::time::Duration::from_millis(750),
        scan_network_processes(&registered_pids),
    )
    .await
    {
        Ok(process_services) => process_services,
        Err(_) => {
            tracing::debug!("fast overlay process scan timed out");
            return None;
        }
    };

    append_registered_network_services(&mut out, &registrations);
    Some(out)
}

fn append_registered_network_services(
    out: &mut Vec<DiscoveredNetworkService>,
    registrations: &std::collections::HashMap<u32, Vec<crate::management::PortRegistration>>,
) {
    // Synthesize overlay entries for management-registered PIDs that are still
    // alive. These bypass the PZ_TUNNEL env-var path and protocol probing.
    for (pid, regs) in registrations {
        if !crate::management::pid_lookup::pid_is_alive(*pid) {
            continue;
        }
        for reg in regs {
            if !is_local_overlay_domain(&reg.domain) {
                continue;
            }
            out.push(DiscoveredNetworkService {
                name: extract_local_label(&reg.domain),
                domain_template: reg.domain.clone(),
                substitutions: BTreeMap::new(),
                real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], reg.local_port)),
                service_port: 80,
                pid: *pid,
                source: ServiceSource::Process { cwd: None },
            });
        }
    }
}

struct NetProcessCandidate {
    label: String,
    domain_template: String,
    substitutions: BTreeMap<String, String>,
    real_addr: std::net::SocketAddr,
    port: u16,
    needs_probe: bool,
    pid: u32,
    cwd: Option<PathBuf>,
}

// Blocking half: sysinfo refresh + per-process ps/lsof calls. Must not .await.
fn scan_network_processes_sync() -> Vec<NetProcessCandidate> {
    #[cfg(target_os = "windows")]
    {
        scan_network_processes_sync_windows()
    }

    #[cfg(target_os = "macos")]
    {
        scan_network_processes_sync_macos()
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        use std::net::{IpAddr, SocketAddr};

        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let daemon_no_probe = std::env::var(ENV_NO_PROBE_VAR).ok();
        let mut candidates = Vec::new();

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
            let (raw_domain, canonical_port) = split_tunnel_port(&raw);
            warn_if_port_like_rejected(&raw, canonical_port, pid_u32);
            let substitutions = template_substitutions(process_cwd.as_deref(), None, None, None);
            let resolved = resolve_tunnel_template(raw_domain, process_cwd.as_deref(), None, None);

            if !is_local_overlay_domain(&resolved) {
                continue;
            }

            if resolved.eq_ignore_ascii_case("portzero.local") {
                tracing::warn!(
                    "PZ_TUNNEL=portzero.local is reserved by the portzero daemon; ignoring process {}",
                    pid_u32
                );
                continue;
            }

            if resolved.eq_ignore_ascii_case("api.portzero.local") {
                tracing::warn!(
                    "PZ_TUNNEL=api.portzero.local is reserved by the portzero daemon; ignoring process {}",
                    pid_u32
                );
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

            let chosen = listening
                .iter()
                .find(|lp| lp.bind == BindAddr::Public)
                .or_else(|| listening.first());

            let port = match chosen {
                Some(lp) => lp.port,
                None => continue,
            };

            let real_addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);

            let (port, needs_probe) = match canonical_port {
                Some(explicit) => (explicit, false),
                None => {
                    let per_process = scan_process_env(pid_u32, ENV_NO_PROBE_VAR);
                    let no_probe = protocol_detect::probing_disabled(
                        per_process.as_deref(),
                        daemon_no_probe.as_deref(),
                    );
                    (port, !no_probe)
                }
            };

            candidates.push(NetProcessCandidate {
                label,
                domain_template: raw_domain.to_string(),
                substitutions,
                real_addr,
                port,
                needs_probe,
                pid: pid_u32,
                cwd: process_cwd,
            });
        }

        candidates
    }
}

#[cfg(target_os = "macos")]
fn scan_network_processes_sync_macos() -> Vec<NetProcessCandidate> {
    use std::net::{IpAddr, SocketAddr};
    use std::process::Command;

    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let output = match Command::new("ps")
        .args(["-axo", "pid=,command=", "-wwwE"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let daemon_no_probe = std::env::var(ENV_NO_PROBE_VAR).ok();
    let mut candidates = Vec::new();

    for (pid_u32, raw) in parse_macos_ps_env_candidates(&stdout, ENV_VAR_NAME) {
        if pid_u32 <= 1 {
            continue;
        }

        let process_cwd = sys
            .process(sysinfo::Pid::from_u32(pid_u32))
            .and_then(|process| process.cwd().map(|p| p.to_path_buf()));
        let (raw_domain, canonical_port) = split_tunnel_port(&raw);
        warn_if_port_like_rejected(&raw, canonical_port, pid_u32);
        let substitutions = template_substitutions(process_cwd.as_deref(), None, None, None);
        let resolved = resolve_tunnel_template(raw_domain, process_cwd.as_deref(), None, None);

        if !is_local_overlay_domain(&resolved) {
            continue;
        }

        if resolved.eq_ignore_ascii_case("portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid_u32
            );
            continue;
        }

        if resolved.eq_ignore_ascii_case("api.portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=api.portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid_u32
            );
            continue;
        }

        let label = extract_local_label(&resolved);
        if label.is_empty() {
            continue;
        }

        // We only invoke lsof for processes that actually advertise PZ_TUNNEL.
        // The previous macOS path invoked `ps` once per process and commonly
        // exceeded the overlay scan timeout before reaching the real service.
        let listening = discover_process_ports(pid_u32);
        if listening.is_empty() {
            continue;
        }

        let chosen = listening
            .iter()
            .find(|lp| lp.bind == BindAddr::Public)
            .or_else(|| listening.first());

        let port = match chosen {
            Some(lp) => lp.port,
            None => continue,
        };

        let real_addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);

        let (port, needs_probe) = match canonical_port {
            Some(explicit) => (explicit, false),
            None => {
                let per_process = scan_process_env(pid_u32, ENV_NO_PROBE_VAR);
                let no_probe = protocol_detect::probing_disabled(
                    per_process.as_deref(),
                    daemon_no_probe.as_deref(),
                );
                (port, !no_probe)
            }
        };

        candidates.push(NetProcessCandidate {
            label,
            domain_template: raw_domain.to_string(),
            substitutions,
            real_addr,
            port,
            needs_probe,
            pid: pid_u32,
            cwd: process_cwd,
        });
    }

    candidates
}

#[cfg(target_os = "macos")]
fn parse_macos_ps_env_candidates(stdout: &str, var_name: &str) -> Vec<(u32, String)> {
    stdout
        .lines()
        .filter_map(|line| parse_macos_ps_env_candidate(line, var_name))
        .collect()
}

#[cfg(target_os = "macos")]
fn parse_macos_ps_env_candidate(line: &str, var_name: &str) -> Option<(u32, String)> {
    let trimmed = line.trim_start();
    let (pid, rest) = trimmed.split_once(char::is_whitespace)?;
    let pid = pid.parse::<u32>().ok()?;
    let prefix = format!("{var_name}=");
    let value = rest
        .split_whitespace()
        .find_map(|token| token.strip_prefix(&prefix))?;
    if value.is_empty() {
        return None;
    }
    Some((pid, value.to_string()))
}

#[cfg(target_os = "windows")]
fn scan_network_processes_sync_windows() -> Vec<NetProcessCandidate> {
    use std::net::{IpAddr, SocketAddr};

    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let daemon_no_probe = std::env::var(ENV_NO_PROBE_VAR).ok();
    let listening_by_pid = discover_all_ports_windows_by_pid();
    let mut candidates = Vec::new();

    for (pid_u32, listening) in listening_by_pid {
        if pid_u32 <= 1 || listening.is_empty() {
            continue;
        }

        let raw = match scan_process_env(pid_u32, ENV_VAR_NAME) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };

        let process_cwd = sys
            .process(sysinfo::Pid::from_u32(pid_u32))
            .and_then(|process| process.cwd().map(|p| p.to_path_buf()));
        let (raw_domain, canonical_port) = split_tunnel_port(&raw);
        warn_if_port_like_rejected(&raw, canonical_port, pid_u32);
        let substitutions = template_substitutions(process_cwd.as_deref(), None, None, None);
        let resolved = resolve_tunnel_template(raw_domain, process_cwd.as_deref(), None, None);

        if !is_local_overlay_domain(&resolved) {
            continue;
        }

        if resolved.eq_ignore_ascii_case("portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid_u32
            );
            continue;
        }

        if resolved.eq_ignore_ascii_case("api.portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=api.portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid_u32
            );
            continue;
        }

        let label = extract_local_label(&resolved);
        if label.is_empty() {
            continue;
        }

        let chosen = listening
            .iter()
            .find(|lp| lp.bind == BindAddr::Public)
            .or_else(|| listening.first());

        let port = match chosen {
            Some(lp) => lp.port,
            None => continue,
        };

        let real_addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);

        let (port, needs_probe) = match canonical_port {
            Some(explicit) => (explicit, false),
            None => {
                let per_process = scan_process_env(pid_u32, ENV_NO_PROBE_VAR);
                let no_probe = protocol_detect::probing_disabled(
                    per_process.as_deref(),
                    daemon_no_probe.as_deref(),
                );
                (port, !no_probe)
            }
        };

        candidates.push(NetProcessCandidate {
            label,
            domain_template: raw_domain.to_string(),
            substitutions,
            real_addr,
            port,
            needs_probe,
            pid: pid_u32,
            cwd: process_cwd,
        });
    }

    candidates
}

async fn scan_network_processes(
    registered_pids: &std::collections::HashSet<u32>,
) -> Vec<DiscoveredNetworkService> {
    // Run the blocking scan (sysinfo + per-process ps/lsof) on the blocking
    // thread pool so the async executor is free to handle shutdown signals and
    // other tasks while the scan runs.
    let candidates = tokio::task::spawn_blocking(scan_network_processes_sync)
        .await
        .unwrap_or_default();

    let mut results = Vec::new();
    for c in candidates {
        // Management-registered PIDs are synthesized separately from the
        // registration store; skip them here to avoid double-registration.
        if registered_pids.contains(&c.pid) {
            continue;
        }

        // Precedence (task-16 + task-17):
        //   explicit `:port`  >  detected canonical (HTTP→80 / TLS→443)  >  ephemeral.
        let service_port = if c.needs_probe {
            protocol_detect::detect_cached(c.real_addr, (c.pid, c.port))
                .await
                .unwrap_or(c.port)
        } else {
            c.port
        };

        results.push(DiscoveredNetworkService {
            name: c.label,
            domain_template: c.domain_template,
            substitutions: c.substitutions,
            real_addr: c.real_addr,
            service_port,
            pid: c.pid,
            source: ServiceSource::Process { cwd: c.cwd },
        });
    }

    results
}

async fn scan_network_containers() -> Vec<DiscoveredNetworkService> {
    match tokio::task::spawn_blocking(scan_network_containers_impl)
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("spawn_blocking panicked")))
    {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("Docker network scan failed: {}", e);
            Vec::new()
        }
    }
}

fn scan_network_containers_impl() -> anyhow::Result<Vec<DiscoveredNetworkService>> {
    use std::net::{IpAddr, SocketAddr};
    use std::process::Command;

    if !command_on_path("docker") {
        return Ok(Vec::new());
    }

    let output = Command::new("docker")
        .args(["ps", "--format", "{{.ID}}"])
        .output()?;

    if !output.status.success() {
        anyhow::bail!("docker ps failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let ids: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

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

        // State.Pid == 0 → container stopped/exited (race between docker ps and inspect).
        if pid == 0 {
            continue;
        }

        let envs: Vec<String> = match serde_json::from_str(env_json) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only PZ_TUNNEL is used. Overlay participation requires the value
        // to end with .portzero.local (pure suffix-based detection).
        let tunnel_prefix = format!("{ENV_VAR_NAME}=");
        let raw_name = envs
            .iter()
            .find_map(|e| e.strip_prefix(&tunnel_prefix))
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let raw_name = match raw_name {
            Some(s) => s,
            None => continue,
        };

        // Use host-side git context (mounts/labels) for templating.
        // Strip an optional canonical `:port` (the VIP listen port) first.
        let (raw_domain, canonical_port) = split_tunnel_port(&raw_name);
        warn_if_port_like_rejected(&raw_name, canonical_port, pid);
        let project_dir = find_host_project_dir_for_container(mounts_json, labels_json);
        let substitutions = template_substitutions(project_dir.as_deref(), None, None, Some(_name));
        let resolved_name = resolve_tunnel_template(raw_domain, project_dir.as_deref(), None, None);

        if !is_local_overlay_domain(&resolved_name) {
            continue;
        }

        if resolved_name.eq_ignore_ascii_case("portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid
            );
            continue;
        }

        if resolved_name.eq_ignore_ascii_case("api.portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=api.portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid
            );
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
        // An explicit canonical port wins; otherwise prefer the container port,
        // falling back to the host port.
        let svc_port = canonical_port.unwrap_or(if service_port > 0 {
            service_port
        } else {
            host_port
        });

        let container_name = label.clone();
        out.push(DiscoveredNetworkService {
            name: label,
            domain_template: raw_domain.to_string(),
            substitutions,
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
                if let Some(hp) = b
                    .get("HostPort")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u16>().ok())
                {
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
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string()
        .to_lowercase()
        .chars()
        .take(63)
        .collect()
}

fn command_on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };

    #[cfg(windows)]
    let candidates: Vec<String> = {
        let pathext = std::env::var_os("PATHEXT")
            .and_then(|v| v.into_string().ok())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
        let has_ext = std::path::Path::new(program).extension().is_some();
        if has_ext {
            vec![program.to_string()]
        } else {
            pathext
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(|ext| format!("{program}{ext}"))
                .chain(std::iter::once(program.to_string()))
                .collect()
        }
    };

    #[cfg(not(windows))]
    let candidates: Vec<String> = vec![program.to_string()];

    std::env::split_paths(&paths).any(|dir| {
        candidates
            .iter()
            .any(|candidate| dir.join(candidate).is_file())
    })
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

    #[cfg(target_os = "macos")]
    #[test]
    fn test_parse_macos_ps_env_candidate() {
        let line = "64039 target/debug/portzero-rust-local-process CARGO=/Users/loumtech/.cargo/bin/cargo PWD=/tmp/project PZ_TUNNEL=rust-demo.portzero.local:80 RUST_RECURSION_COUNT=1";
        assert_eq!(
            parse_macos_ps_env_candidate(line, ENV_VAR_NAME),
            Some((64039, "rust-demo.portzero.local:80".to_string()))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_parse_macos_ps_env_candidate_ignores_missing_var() {
        let line = "70462 /Users/loumtech/.cargo/bin/portzero start --foreground";
        assert_eq!(parse_macos_ps_env_candidate(line, ENV_VAR_NAME), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_parse_lsof_line_ipv4_loopback() {
        let p = parse_lsof_line(
            "Python    32520 loumtech    3u  IPv4 0xc2df...      0t0  TCP 127.0.0.1:50706 (LISTEN)",
        )
        .expect("should parse a port");
        assert_eq!(p.port, 50706);
        assert_eq!(p.bind, BindAddr::Loopback);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_parse_lsof_line_public_forms() {
        let star =
            parse_lsof_line("node 100 u 5u IPv4 0x0 0t0 TCP *:8080 (LISTEN)").expect("star parses");
        assert_eq!(star.port, 8080);
        assert_eq!(star.bind, BindAddr::Public);

        let any = parse_lsof_line("node 100 u 5u IPv4 0x0 0t0 TCP 0.0.0.0:8080 (LISTEN)")
            .expect("0.0.0.0 parses");
        assert_eq!(any.port, 8080);
        assert_eq!(any.bind, BindAddr::Public);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_parse_lsof_line_ipv6() {
        let loopback = parse_lsof_line(
            "Parallels  7174 loumtech   13u  IPv6 0x1e18...      0t0  TCP [::1]:57889 (LISTEN)",
        )
        .expect("[::1] parses");
        assert_eq!(loopback.port, 57889);
        assert_eq!(loopback.bind, BindAddr::Loopback);

        let public = parse_lsof_line("node 100 u 5u IPv6 0x0 0t0 TCP [::]:8080 (LISTEN)")
            .expect("[::] parses");
        assert_eq!(public.port, 8080);
        assert_eq!(public.bind, BindAddr::Public);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_parse_lsof_line_no_address() {
        // A line with no address token (just a state) must not panic or produce a port.
        assert_eq!(parse_lsof_line("(LISTEN)"), None);
        assert_eq!(parse_lsof_line(""), None);
        // Header-ish / non-address tokens only.
        assert_eq!(parse_lsof_line("COMMAND PID USER FD TYPE"), None);
        // Port 0 is filtered.
        assert_eq!(
            parse_lsof_line("node 1 u 5u IPv4 0x0 0t0 TCP *:0 (LISTEN)"),
            None
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_parse_lsof_stdout_skips_header() {
        let stdout = "COMMAND     PID     USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME\n\
            Python    32520 loumtech    3u  IPv4 0xc2df...      0t0  TCP 127.0.0.1:50706 (LISTEN)\n\
            Parallels  7174 loumtech   13u  IPv6 0x1e18...      0t0  TCP [::1]:57889 (LISTEN)\n\
            ollama      580 loumtech    3u  IPv4 0xd11a...      0t0  TCP 127.0.0.1:11434 (LISTEN)\n";
        let ports = parse_lsof_stdout(stdout);
        assert_eq!(
            ports,
            vec![
                ListeningPort {
                    port: 50706,
                    bind: BindAddr::Loopback
                },
                ListeningPort {
                    port: 57889,
                    bind: BindAddr::Loopback
                },
                ListeningPort {
                    port: 11434,
                    bind: BindAddr::Loopback
                },
            ]
        );
    }

    #[test]
    fn test_parse_windows_tcp_connection_lines() {
        assert_eq!(
            parse_windows_tcp_connection_line("0.0.0.0|8080"),
            Some(ListeningPort {
                port: 8080,
                bind: BindAddr::Public,
            })
        );
        assert_eq!(
            parse_windows_tcp_connection_line("::|3000"),
            Some(ListeningPort {
                port: 3000,
                bind: BindAddr::Public,
            })
        );
        assert_eq!(
            parse_windows_tcp_connection_line("127.0.0.1|5173"),
            Some(ListeningPort {
                port: 5173,
                bind: BindAddr::Loopback,
            })
        );
        assert_eq!(parse_windows_tcp_connection_line("127.0.0.1|0"), None);
        assert_eq!(parse_windows_tcp_connection_line("bad"), None);
    }

    #[test]
    fn test_parse_windows_netstat_lines() {
        assert_eq!(
            parse_windows_netstat_line(
                "  TCP    127.0.0.1:5173         0.0.0.0:0              LISTENING       1234",
                1234,
            ),
            Some(ListeningPort {
                port: 5173,
                bind: BindAddr::Loopback,
            })
        );
        assert_eq!(
            parse_windows_netstat_line(
                "  TCP    0.0.0.0:8080           0.0.0.0:0              LISTENING       1234",
                1234,
            ),
            Some(ListeningPort {
                port: 8080,
                bind: BindAddr::Public,
            })
        );
        assert_eq!(
            parse_windows_netstat_line(
                "  TCP    [::]:3000              [::]:0                 LISTENING       1234",
                1234,
            ),
            Some(ListeningPort {
                port: 3000,
                bind: BindAddr::Public,
            })
        );
        assert_eq!(
            parse_windows_netstat_line(
                "  TCP    127.0.0.1:5173         0.0.0.0:0              ESTABLISHED     1234",
                1234,
            ),
            None
        );
        assert_eq!(
            parse_windows_netstat_line(
                "  TCP    127.0.0.1:5173         0.0.0.0:0              LISTENING       9999",
                1234,
            ),
            None
        );
    }

    #[test]
    fn test_parse_windows_environment_block() {
        let mut words: Vec<u16> = "Path=C:\\Windows\0PZ_TUNNEL=api.portzero.local\0\0"
            .encode_utf16()
            .collect();
        assert_eq!(
            parse_windows_environment_block(&words),
            vec![
                "Path=C:\\Windows".to_string(),
                "PZ_TUNNEL=api.portzero.local".to_string()
            ]
        );

        words.clear();
        words.extend("\0".encode_utf16());
        assert!(parse_windows_environment_block(&words).is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_scan_process_env_windows_spawned_child() {
        let expected = "spawned-child.tunnel.portzero.cloud";
        let mut child = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .env("PZ_TUNNEL", expected)
            .spawn()
            .expect("spawn child process");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut result = None;
        while std::time::Instant::now() < deadline {
            result = scan_process_env(child.id(), ENV_VAR_NAME);
            if result.as_deref() == Some(expected) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(result.as_deref(), Some(expected));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_discover_ports_windows_current_process_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral listener");
        let port = listener.local_addr().expect("listener address").port();

        let ports = discover_process_ports(std::process::id());

        assert!(
            ports.iter().any(|p| p.port == port),
            "expected to discover listener port {port}, got {ports:?}"
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
