#[allow(unused_imports)]
use super::*;

// Docker container scanning
// ---------------------------------------------------------------------------

pub(super) async fn scan_docker_containers(
    account_id: Option<&str>,
    username: Option<&str>,
) -> (Vec<DiscoveredService>, Vec<crate::notify::Issue>) {
    if !command_on_path("docker") {
        return (Vec::new(), Vec::new());
    }

    let acct = account_id.map(str::to_owned);
    let user = username.map(str::to_owned);
    match tokio::task::spawn_blocking(move || {
        scan_docker_containers_impl(acct.as_deref(), user.as_deref())
    })
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("spawn_blocking panicked")))
    {
        Ok(result) => result,
        Err(e) => {
            tracing::debug!(
                "Docker container scan failed (Docker may not be running): {}",
                e
            );
            (Vec::new(), Vec::new())
        }
    }
}

fn scan_docker_containers_impl(
    account_id: Option<&str>,
    username: Option<&str>,
) -> Result<(Vec<DiscoveredService>, Vec<crate::notify::Issue>)> {
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
        return Ok((Vec::new(), Vec::new()));
    }

    let mut services = Vec::new();
    let mut issues = Vec::new();
    for container_id in container_ids {
        if let Some(svc) = inspect_container(container_id, account_id, username, &mut issues)? {
            services.push(svc);
        }
    }

    Ok((services, issues))
}

fn inspect_container(
    container_id: &str,
    account_id: Option<&str>,
    username: Option<&str>,
    issues: &mut Vec<crate::notify::Issue>,
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

    let is_local = is_local_overlay_domain(raw_domain);
    if let Err(e) =
        portzero_domain::validate_username_placeholders(raw_domain, is_local, username.is_some())
    {
        tracing::warn!(
            container = name,
            template = raw_domain,
            error = %e,
            "PZ_TUNNEL template misuses {{local-username}}/{{cloud-username}}"
        );
        issues.push(crate::notify::Issue::InvalidUsernamePlaceholder {
            template: raw_domain.to_string(),
            reason: e,
            context: format!("container {name}"),
            requires_login: is_local,
        });
        return Ok(None);
    }

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
             Use a full name like web-mybranch.alice.tunnel.portzero.cloud"
        );
        issues.push(crate::notify::Issue::InvalidCloudTunnelScope {
            domain,
            reason: e,
            context: format!("container {name}"),
        });
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

pub(super) fn parse_docker_ports(ports_json: &str, selection: &HttpPortSelection) -> u16 {
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

pub(super) async fn scan_network_containers() -> Vec<DiscoveredNetworkService> {
    let mut services = match tokio::task::spawn_blocking(scan_network_containers_impl)
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("spawn_blocking panicked")))
    {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("Docker network scan failed: {}", e);
            return Vec::new();
        }
    };

    for svc in &mut services {
        if svc.service_port == 443 {
            svc.backend_protocol = protocol_detect::detect_canonical_cached(
                svc.real_addr,
                (svc.pid, svc.real_addr.port()),
            )
            .await;
        }
    }

    services
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
            backend_protocol: None,
            pid,
            source: ServiceSource::Container {
                id: id.to_string(),
                name: container_name,
            },
        });
    }

    Ok(out)
}

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
