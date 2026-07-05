//! Service discovery: scan processes and Docker containers for PZ_TUNNEL.
//!
//! The value of PZ_TUNNEL must be a full domain name (after template
//! substitution). No implicit suffixes are added.
//!
//! - If it ends with `.portzero.local` (or `.local`) → local virtual overlay.
//! - Otherwise it must be a valid tunnel domain ending in `.tunnel.portzero.cloud`
//!   (or the configured base), under a username scope like `api.alice.tunnel.portzero.cloud`.
//!
//! Examples (full names required):
//!   PZ_TUNNEL=my-api.alice.tunnel.portzero.cloud
//!   PZ_TUNNEL=my-db-{branch}.portzero.local
//!   PZ_TUNNEL=web-{branch}.{username}.tunnel.portzero.cloud
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
mod docker;
mod process;

#[allow(unused_imports)]
use docker::{parse_docker_ports, scan_docker_containers, scan_network_containers};
#[allow(unused_imports)]
use process::{
    discover_process_ports, looks_like_repo_path, parse_extra_ports, parse_http_port_selection,
    resolve_tunnel_template, scan_network_processes, scan_process_env, scan_processes,
    select_http_port, template_substitutions, warn_if_port_like_rejected,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(unused_imports)]
use process::{parse_lsof_line, parse_lsof_stdout};

#[cfg(target_os = "macos")]
#[allow(unused_imports)]
use process::{parse_macos_ps_env_candidate, parse_macos_ps_env_candidates};

#[cfg(target_os = "linux")]
#[allow(unused_imports)]
use process::parse_proc_net_tcp_line;

pub use process::{enumerate_system_listeners, SystemListener};
#[cfg(target_os = "windows")]
#[allow(unused_imports)]
use process::{
    parse_windows_environment_block, parse_windows_netstat_line, parse_windows_netstat_stdout,
    parse_windows_netstat_stdout_by_pid, parse_windows_tcp_connection_line,
    parse_windows_tcp_connection_stdout,
};

/// The single environment variable used to tag services.
///
/// The value (after substitution) must be a full domain name:
/// - `*.portzero.local` → local overlay
/// - `*.<username>.tunnel.portzero.cloud` (or configured base) → cloud tunnel
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

/// Extract the name for the overlay from a full name like "my-db.portzero.local".
/// The input must already be a full domain (no implicit suffix added by us).
pub fn extract_local_label(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    let core = n
        .strip_suffix(".portzero.local")
        .or_else(|| n.strip_suffix(".local"))
        .unwrap_or(&n);
    sanitize_network_name(core)
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

#[derive(Debug, Clone, Default)]
struct ProcessTemplateContext {
    cwd: Option<PathBuf>,
    project_dir: Option<PathBuf>,
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
///
/// Returns the discovered services alongside any [`crate::notify::Issue`]s
/// found along the way (e.g. `PZ_TUNNEL` values that look like a cloud tunnel
/// request but are missing the required username scope), so callers can
/// surface both through `portzero status`.
pub async fn scan_all(
    account_id: Option<&str>,
    username: Option<&str>,
) -> (Vec<DiscoveredService>, Vec<crate::notify::Issue>) {
    let mut services = Vec::new();
    let mut issues = Vec::new();
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
        Ok(Ok((process_services, process_issues))) => {
            services.extend(process_services);
            issues.extend(process_issues);
        }
        Ok(Err(_)) => tracing::debug!("process scan panicked"),
        Err(_) => tracing::debug!("process scan timed out"),
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        scan_docker_containers(account_id, username),
    )
    .await
    {
        Ok((docker_services, docker_issues)) => {
            services.extend(docker_services);
            issues.extend(docker_issues);
        }
        Err(_) => tracing::debug!("Docker container scan timed out"),
    }
    (services, issues)
}

// ---------------------------------------------------------------------------

// Overlay network discovery (PZ_TUNNEL=*.portzero.local + port 0 support)
// ---------------------------------------------------------------------------

/// A service discovered for the local virtual overlay network.
#[derive(Debug, Clone)]
pub struct DiscoveredNetworkService {
    /// The local overlay name extracted from the PZ_TUNNEL value (e.g. "my-db"
    /// from "my-db.portzero.local", or "staging.portzero.net" from
    /// "staging.portzero.net.portzero.local").
    pub name: String,
    /// The actual host address we must proxy to (usually 127.0.0.1:random).
    pub real_addr: std::net::SocketAddr,
    /// The port that should be presented on the virtual side (e.g. 5432).
    /// For Docker this is the container port from the -p mapping.
    /// For plain processes we use the discovered listening port.
    pub service_port: u16,
    /// Best-effort backend protocol detection, used by the overlay to avoid
    /// raw-proxying browser TLS into plaintext HTTP services on port 443.
    pub backend_protocol: Option<protocol_detect::Canonical>,
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

    dedupe_network_services(out)
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
    Some(dedupe_network_services(out))
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
                backend_protocol: Some(protocol_detect::Canonical::Http),
                pid: *pid,
                source: ServiceSource::Process { cwd: None },
            });
        }
    }
}

/// Key used to deduplicate network services discovered from processes/containers.
type NetworkServiceDedupeKey = (
    String,
    String,
    BTreeMap<String, String>,
    std::net::SocketAddr,
    u16,
    String,
);

fn dedupe_network_services(
    services: Vec<DiscoveredNetworkService>,
) -> Vec<DiscoveredNetworkService> {
    let mut deduped: std::collections::BTreeMap<NetworkServiceDedupeKey, DiscoveredNetworkService> =
        std::collections::BTreeMap::new();

    for svc in services {
        let key = dedupe_network_service_key(&svc);
        match deduped.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(svc);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                // Keep the lowest PID so the dashboard remains stable across scans
                // when a process tree shares the same listening socket.
                if svc.pid < entry.get().pid {
                    entry.insert(svc);
                }
            }
        }
    }

    deduped.into_values().collect()
}

fn dedupe_network_service_key(svc: &DiscoveredNetworkService) -> NetworkServiceDedupeKey {
    (
        svc.name.clone(),
        svc.domain_template.clone(),
        svc.substitutions.clone(),
        svc.real_addr,
        svc.service_port,
        service_context_dedupe_key(svc),
    )
}

fn service_source_dedupe_key(source: &ServiceSource) -> String {
    match source {
        ServiceSource::Process { cwd } => format!("process:{cwd:?}"),
        ServiceSource::Container { id, .. } => format!("container:{id}"),
    }
}

fn service_context_dedupe_key(svc: &DiscoveredNetworkService) -> String {
    match &svc.source {
        ServiceSource::Process { cwd: Some(cwd) } => format!("process-cwd:{cwd:?}"),
        ServiceSource::Process { cwd: None } => format!("process-pid:{}", svc.pid),
        ServiceSource::Container { .. } => service_source_dedupe_key(&svc.source),
    }
}

fn sanitize_network_name(raw: &str) -> String {
    raw.split('.')
        .filter_map(|label| {
            let sanitized = label
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
                .trim_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_ascii_lowercase()
                .chars()
                .take(63)
                .collect::<String>();
            (!sanitized.is_empty()).then_some(sanitized)
        })
        .collect::<Vec<_>>()
        .join(".")
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

    #[cfg(target_os = "windows")]
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

    #[cfg(target_os = "windows")]
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

    #[cfg(target_os = "windows")]
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
        let expected = "spawned-child.alice.portzero.cloud";
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

    #[test]
    fn test_looks_like_repo_path() {
        assert!(looks_like_repo_path("/repo/app.jar"));
        assert!(looks_like_repo_path("./server/main.py"));
        assert!(looks_like_repo_path("dist/app.js"));
        assert!(looks_like_repo_path("app.jar"));
        assert!(!looks_like_repo_path("serve"));
    }

    #[test]
    fn test_template_substitutions_without_project_dir_do_not_use_daemon_cwd() {
        let substitutions = template_substitutions(None, None, None, None);
        assert_eq!(
            substitutions.get("branch").map(String::as_str),
            Some("unknown")
        );
        assert_eq!(
            substitutions.get("project").map(String::as_str),
            Some("unknown")
        );
        assert_eq!(
            substitutions.get("worktree").map(String::as_str),
            Some("unknown")
        );
    }

    #[test]
    fn test_extract_local_label_preserves_multi_label_prefix() {
        assert_eq!(
            extract_local_label("staging.portzero.net.portzero.local"),
            "staging.portzero.net"
        );
        assert_eq!(
            extract_local_label("Staging.PortZero.Net.PortZero.Local"),
            "staging.portzero.net"
        );
        assert_eq!(
            extract_local_label("bad_label.example.portzero.local"),
            "bad-label.example"
        );
    }

    #[test]
    fn test_dedupe_network_services_collapses_same_process_context() {
        let svc = DiscoveredNetworkService {
            name: "web".to_string(),
            domain_template: "web.portzero.local".to_string(),
            substitutions: BTreeMap::new(),
            real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5173)),
            service_port: 5173,
            backend_protocol: None,
            pid: 100,
            source: ServiceSource::Process {
                cwd: Some(PathBuf::from("/work/app")),
            },
        };

        let dup = DiscoveredNetworkService { ..svc.clone() };

        let deduped = dedupe_network_services(vec![svc, dup]);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].service_port, 5173);
    }

    #[test]
    fn test_dedupe_network_services_collapses_same_endpoint_in_same_worktree() {
        let a = DiscoveredNetworkService {
            name: "web".to_string(),
            domain_template: "web.portzero.local".to_string(),
            substitutions: BTreeMap::new(),
            real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5173)),
            service_port: 5173,
            backend_protocol: None,
            pid: 100,
            source: ServiceSource::Process {
                cwd: Some(PathBuf::from("/work/a")),
            },
        };

        let b = DiscoveredNetworkService {
            pid: 200,
            ..a.clone()
        };

        let deduped = dedupe_network_services(vec![a, b]);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].pid, 100);
    }

    #[test]
    fn test_dedupe_network_services_keeps_distinct_ports_in_same_worktree() {
        let a = DiscoveredNetworkService {
            name: "web".to_string(),
            domain_template: "web.portzero.local".to_string(),
            substitutions: BTreeMap::new(),
            real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5173)),
            service_port: 5173,
            backend_protocol: None,
            pid: 100,
            source: ServiceSource::Process {
                cwd: Some(PathBuf::from("/work/a")),
            },
        };

        let b = DiscoveredNetworkService {
            real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5174)),
            service_port: 5174,
            pid: 200,
            ..a.clone()
        };

        let deduped = dedupe_network_services(vec![a, b]);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn test_dedupe_network_services_keeps_distinct_pid_when_cwd_unknown() {
        let a = DiscoveredNetworkService {
            name: "web".to_string(),
            domain_template: "web.portzero.local".to_string(),
            substitutions: BTreeMap::new(),
            real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5173)),
            service_port: 5173,
            backend_protocol: None,
            pid: 100,
            source: ServiceSource::Process { cwd: None },
        };

        let b = DiscoveredNetworkService {
            pid: 200,
            ..a.clone()
        };

        let deduped = dedupe_network_services(vec![a, b]);
        assert_eq!(deduped.len(), 2);
    }
}
