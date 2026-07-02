#[allow(unused_imports)]
use super::*;

// Process scanning
// ---------------------------------------------------------------------------

pub(super) fn scan_processes(
    account_id: Option<&str>,
    username: Option<&str>,
) -> Vec<DiscoveredService> {
    #[cfg(target_os = "windows")]
    {
        scan_processes_windows(account_id, username)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let mut services = Vec::new();

        for pid in sys.processes().keys() {
            let pid_u32 = pid.as_u32();
            if pid_u32 <= 1 {
                continue;
            }

            let raw = match scan_process_env(pid_u32, ENV_VAR_NAME) {
                Some(v) if !v.is_empty() => v,
                _ => continue,
            };

            let process_context = process_template_context(&sys, pid_u32);
            // Strip an optional canonical `:port` before classification/validation.
            // On the cloud path the edge assigns the URL, so the port is ignored.
            let (raw_domain, _canonical_port) = split_tunnel_port(&raw);
            warn_if_port_like_rejected(&raw, _canonical_port, pid_u32);
            let substitutions = template_substitutions(
                process_context.project_dir.as_deref(),
                account_id,
                username,
                None,
            );
            let domain = resolve_tunnel_template(
                raw_domain,
                process_context.project_dir.as_deref(),
                account_id,
                username,
            );

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
                     Provide the full name including suffix, e.g. my-api.alice.portzero.cloud"
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
                source: ServiceSource::Process {
                    cwd: process_context.cwd,
                },
            });
        }

        services
    }
}

#[cfg(target_os = "windows")]
pub(super) fn scan_processes_windows(
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

        let process_context = process_template_context(&sys, pid_u32);
        let (raw_domain, _canonical_port) = split_tunnel_port(&raw);
        warn_if_port_like_rejected(&raw, _canonical_port, pid_u32);
        let substitutions = template_substitutions(
            process_context.project_dir.as_deref(),
            account_id,
            username,
            None,
        );
        let domain = resolve_tunnel_template(
            raw_domain,
            process_context.project_dir.as_deref(),
            account_id,
            username,
        );

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
                 Provide the full name including suffix, e.g. my-api.alice.portzero.cloud"
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
            source: ServiceSource::Process {
                cwd: process_context.cwd,
            },
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
pub(super) fn warn_if_port_like_rejected(raw: &str, canonical: Option<u16>, pid: u32) {
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
pub(super) fn resolve_tunnel_template(
    raw: &str,
    project_dir: Option<&Path>,
    account_id: Option<&str>,
    username: Option<&str>,
) -> String {
    if !raw.contains('{') {
        return raw.to_string();
    }
    let ctx = DomainContext::from_optional_environment("", project_dir, account_id, username);
    ctx.resolve(raw)
}

pub(super) fn template_substitutions(
    project_dir: Option<&Path>,
    account_id: Option<&str>,
    username: Option<&str>,
    source_name: Option<&str>,
) -> BTreeMap<String, String> {
    let ctx = DomainContext::from_optional_environment("", project_dir, account_id, username);
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

fn process_template_context(sys: &System, pid: u32) -> ProcessTemplateContext {
    let mut context = ProcessTemplateContext::default();
    let mut repo_candidates = Vec::new();
    let mut current = Some(sysinfo::Pid::from_u32(pid));
    let mut depth = 0usize;

    while let Some(current_pid) = current {
        if depth >= 8 {
            break;
        }
        let Some(process) = sys.process(current_pid) else {
            break;
        };

        let lineage_pid = current_pid.as_u32();

        if let Some(cwd) = process
            .cwd()
            .map(|p| p.to_path_buf())
            .filter(|p| p.is_dir())
        {
            if context.cwd.is_none() {
                context.cwd = Some(cwd.clone());
            }
            repo_candidates.push(cwd);
        }

        if let Some(env_pwd) = scan_process_env(lineage_pid, "PWD")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
        {
            if context.cwd.is_none() {
                context.cwd = Some(env_pwd.clone());
            }
            repo_candidates.push(env_pwd);
        }

        collect_process_repo_candidates(process, context.cwd.as_deref(), &mut repo_candidates);

        current = process.parent();
        depth += 1;
    }

    context.project_dir = first_git_project_dir(&repo_candidates);
    context
}

fn first_git_project_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    for candidate in candidates {
        if let Some((root, _)) = portzero_domain::find_git_root(candidate) {
            return Some(root.to_path_buf());
        }
    }
    None
}

fn collect_process_repo_candidates(
    process: &sysinfo::Process,
    base_dir: Option<&Path>,
    out: &mut Vec<PathBuf>,
) {
    if let Some(exe) = process.exe() {
        push_repo_candidate(out, exe);
    }

    let cmd = process.cmd();
    let launcher = process.name().to_string_lossy().to_ascii_lowercase();

    let mut i = 0usize;
    while i < cmd.len() {
        let arg = cmd[i].to_string_lossy();
        match arg.as_ref() {
            "-jar" | "--manifest-path" | "--project-dir" | "--project" | "--file" | "--config" => {
                if let Some(next) = cmd.get(i + 1) {
                    push_repo_candidate_arg(out, next, base_dir);
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    if launcher.contains("java") || launcher.contains("kotlin") {
        for pair in cmd.windows(2) {
            if pair[0].to_string_lossy() == "-jar" {
                push_repo_candidate_arg(out, &pair[1], base_dir);
            }
        }
    } else if launcher.contains("python")
        || launcher.contains("node")
        || launcher.contains("bun")
        || launcher.contains("deno")
        || launcher.contains("ruby")
        || launcher.contains("php")
        || launcher.contains("perl")
        || launcher.contains("dotnet")
    {
        if let Some(script_arg) = cmd
            .iter()
            .skip(1)
            .find(|arg| !arg.to_string_lossy().starts_with('-'))
        {
            push_repo_candidate_arg(out, script_arg, base_dir);
        }
    }

    for arg in cmd.iter().skip(1) {
        let text = arg.to_string_lossy();
        if text.starts_with('-') || text.is_empty() {
            continue;
        }
        if looks_like_repo_path(&text) {
            push_repo_candidate_arg(out, arg, base_dir);
        }
    }
}

fn push_repo_candidate_arg(out: &mut Vec<PathBuf>, arg: &std::ffi::OsStr, base_dir: Option<&Path>) {
    let path = PathBuf::from(arg);
    if path.is_absolute() {
        push_repo_candidate(out, &path);
    } else if let Some(base_dir) = base_dir {
        push_repo_candidate(out, &base_dir.join(path));
    }
}

fn push_repo_candidate(out: &mut Vec<PathBuf>, path: &Path) {
    let candidate = if path.is_dir() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent() {
        parent.to_path_buf()
    } else {
        return;
    };

    if candidate.as_os_str().is_empty() {
        return;
    }

    out.push(candidate);
}

pub(super) fn looks_like_repo_path(value: &str) -> bool {
    if value.starts_with('/') || value.starts_with("./") || value.starts_with("../") {
        return true;
    }

    let path = Path::new(value);
    if path.components().count() > 1 {
        return true;
    }

    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(
            "jar"
                | "class"
                | "js"
                | "mjs"
                | "cjs"
                | "ts"
                | "tsx"
                | "py"
                | "rb"
                | "php"
                | "pl"
                | "sh"
                | "dll"
                | "exe"
                | "go"
        )
    )
}

// ---------------------------------------------------------------------------
// Port selection logic
// ---------------------------------------------------------------------------

/// Parse PZ_TUNNEL_HTTP_PORT into a selection strategy.
pub(super) fn parse_http_port_selection(val: &str) -> HttpPortSelection {
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
pub(super) fn parse_extra_ports(val: &str) -> Vec<PortMapping> {
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
pub(super) fn select_http_port(
    ports: &[ListeningPort],
    selection: &HttpPortSelection,
) -> SelectedPort {
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
pub(super) fn discover_process_ports(pid: u32) -> Vec<ListeningPort> {
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
pub(super) fn parse_proc_net_tcp_line(
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
pub(super) fn parse_lsof_stdout(stdout: &str) -> Vec<ListeningPort> {
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
pub(super) fn parse_lsof_line(line: &str) -> Option<ListeningPort> {
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
pub(super) fn parse_windows_tcp_connection_stdout(stdout: &str) -> Vec<ListeningPort> {
    stdout
        .lines()
        .filter_map(parse_windows_tcp_connection_line)
        .collect()
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) fn parse_windows_tcp_connection_line(line: &str) -> Option<ListeningPort> {
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
pub(super) fn parse_windows_netstat_stdout(stdout: &str, pid: u32) -> Vec<ListeningPort> {
    stdout
        .lines()
        .filter_map(|line| parse_windows_netstat_line(line, pid))
        .collect()
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) fn parse_windows_netstat_stdout_by_pid(
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
pub(super) fn parse_windows_netstat_line(line: &str, pid: u32) -> Option<ListeningPort> {
    let (line_pid, port) = parse_windows_netstat_line_any_pid(line)?;
    if line_pid == pid {
        Some(port)
    } else {
        None
    }
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) fn parse_windows_netstat_line_any_pid(line: &str) -> Option<(u32, ListeningPort)> {
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
pub(super) fn parse_windows_local_address_port(local: &str) -> Option<ListeningPort> {
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

pub(super) fn scan_process_env(pid: u32, var_name: &str) -> Option<String> {
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
pub(super) fn parse_windows_environment_block(words: &[u16]) -> Vec<String> {
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

        for pid in sys.processes().keys() {
            let pid_u32 = pid.as_u32();
            if pid_u32 <= 1 {
                continue;
            }

            let raw = match scan_process_env(pid_u32, ENV_VAR_NAME) {
                Some(v) if !v.is_empty() => v,
                _ => continue,
            };

            let process_context = process_template_context(&sys, pid_u32);
            let (raw_domain, canonical_port) = split_tunnel_port(&raw);
            warn_if_port_like_rejected(&raw, canonical_port, pid_u32);
            let substitutions =
                template_substitutions(process_context.project_dir.as_deref(), None, None, None);
            let resolved = resolve_tunnel_template(
                raw_domain,
                process_context.project_dir.as_deref(),
                None,
                None,
            );

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
                cwd: process_context.cwd,
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

        let process_context = process_template_context(&sys, pid_u32);
        let (raw_domain, canonical_port) = split_tunnel_port(&raw);
        warn_if_port_like_rejected(&raw, canonical_port, pid_u32);
        let substitutions =
            template_substitutions(process_context.project_dir.as_deref(), None, None, None);
        let resolved = resolve_tunnel_template(
            raw_domain,
            process_context.project_dir.as_deref(),
            None,
            None,
        );

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
            cwd: process_context.cwd,
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

        let process_context = process_template_context(&sys, pid_u32);
        let (raw_domain, canonical_port) = split_tunnel_port(&raw);
        warn_if_port_like_rejected(&raw, canonical_port, pid_u32);
        let substitutions =
            template_substitutions(process_context.project_dir.as_deref(), None, None, None);
        let resolved = resolve_tunnel_template(
            raw_domain,
            process_context.project_dir.as_deref(),
            None,
            None,
        );

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
            cwd: process_context.cwd,
        });
    }

    candidates
}

pub(super) async fn scan_network_processes(
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
        let detected = if c.needs_probe || c.port == 443 {
            protocol_detect::detect_canonical_cached(c.real_addr, (c.pid, c.port)).await
        } else {
            None
        };

        let service_port = if c.needs_probe {
            detected
                .map(protocol_detect::Canonical::port)
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
            backend_protocol: detected,
            pid: c.pid,
            source: ServiceSource::Process { cwd: c.cwd },
        });
    }

    results
}
