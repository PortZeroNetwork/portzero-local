//! Diagnostics module: runs health checks across Binary, Network, DNS, TLS,
//! Auth, and System categories and produces a serializable report.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};

// ─── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixKind {
    Auto,
    Confirm,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fix {
    pub kind: FixKind,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub id: String,
    pub severity: Severity,
    pub category: String,
    pub title: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiagnosticsReport {
    pub generated_at: String, // RFC3339 via chrono
    pub checks_run: usize,
    pub issues: Vec<Diagnostic>, // sorted: Critical first, then Error, Warning, Info
}

// ─── Individual checks ────────────────────────────────────────────────────────

fn check_binary_exists() -> Option<Diagnostic> {
    let exe = std::env::current_exe().ok()?;
    if std::fs::metadata(&exe).is_err() {
        tracing::debug!("check_binary_exists: binary not found at {}", exe.display());
        Some(Diagnostic {
            id: "binary_missing".into(),
            severity: Severity::Critical,
            category: "binary".into(),
            title: "portzero binary not found at its own path".to_string(),
            detail: format!("The binary was expected at {} but could not be accessed.", exe.display()),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Antivirus may have quarantined or deleted the file. Add the portzero installation directory to your antivirus exclusions.".to_string(),
                command: None,
            }),
        })
    } else {
        None
    }
}

fn check_multiple_instances() -> Option<Diagnostic> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let count = sys
        .processes()
        .values()
        .filter(|p| {
            let name = p.name().to_string_lossy();
            name.contains("portzero") || name.contains("portzero-daemon")
        })
        .count();

    if count > 1 {
        tracing::debug!("check_multiple_instances: {} instances found", count);
        Some(Diagnostic {
            id: "multiple_instances".into(),
            severity: Severity::Warning,
            category: "binary".into(),
            title: "Multiple portzero instances detected".to_string(),
            detail: format!(
                "{} processes named 'portzero' are running. This can cause route conflicts.",
                count
            ),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Kill all but the newest instance.".to_string(),
                command: Some("pkill -o portzero  # kill all but the newest".to_string()),
            }),
        })
    } else {
        None
    }
}

fn check_state_dir_writable(state_dir: &Path) -> Option<Diagnostic> {
    let test_path = state_dir.join(".write_test");
    match std::fs::write(&test_path, b"ok") {
        Ok(_) => {
            let _ = std::fs::remove_file(&test_path);
            None
        }
        Err(e) => {
            tracing::debug!("check_state_dir_writable: {}: {}", state_dir.display(), e);
            Some(Diagnostic {
                id: "state_dir_unwritable".into(),
                severity: Severity::Error,
                category: "system".into(),
                title: "State directory is not writable".to_string(),
                detail: format!(
                    "Cannot write to {}: {}",
                    state_dir.display(),
                    e
                ),
                fix: Some(Fix {
                    kind: FixKind::Manual,
                    description: "Check permissions on the state directory".to_string(),
                    command: None,
                }),
            })
        }
    }
}

fn check_state_files_valid(state_dir: &Path) -> Option<Diagnostic> {
    let files = ["routes.json", "overlay.json", "cloud_state.json"];
    let mut failed: Vec<&str> = Vec::new();

    for name in &files {
        let path = state_dir.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if serde_json::from_str::<serde_json::Value>(&content).is_err() {
                tracing::debug!("check_state_files_valid: {} failed to parse", name);
                failed.push(name);
            }
        }
        // Missing files are fine — skip them
    }

    if failed.is_empty() {
        None
    } else {
        Some(Diagnostic {
            id: "state_file_corrupt".into(),
            severity: Severity::Warning,
            category: "system".into(),
            title: "State file corrupted".to_string(),
            detail: format!("The following state files failed to parse: {}", failed.join(", ")),
            fix: Some(Fix {
                kind: FixKind::Auto,
                description: "Restart the daemon to reinitialize state files".to_string(),
                command: Some("portzero restart".to_string()),
            }),
        })
    }
}

#[cfg(unix)]
fn check_fd_limit() -> Option<Diagnostic> {
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let ret = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
    if ret != 0 {
        tracing::warn!("check_fd_limit: getrlimit failed");
        return None;
    }
    let soft = rlim.rlim_cur;
    if soft < 1024 {
        tracing::debug!("check_fd_limit: soft limit is {}", soft);
        Some(Diagnostic {
            id: "fd_limit_low".into(),
            severity: Severity::Warning,
            category: "system".into(),
            title: "File descriptor limit is low".to_string(),
            detail: format!("Current soft limit: {}. Recommended: ≥1024.", soft),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Increase the file descriptor limit.".to_string(),
                command: Some(
                    "ulimit -n 65536  # add to ~/.bashrc or /etc/security/limits.conf".to_string(),
                ),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(unix))]
fn check_fd_limit() -> Option<Diagnostic> {
    None
}

fn check_etc_hosts_override() -> Option<Diagnostic> {
    #[cfg(unix)]
    let hosts_path = "/etc/hosts";
    #[cfg(windows)]
    let hosts_path = r"C:\Windows\System32\drivers\etc\hosts";

    let content = match std::fs::read_to_string(hosts_path) {
        Ok(c) => c,
        Err(_) => return None,
    };

    let offending: Vec<String> = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with('#') && trimmed.contains("portzero.local")
        })
        .map(|l| l.to_string())
        .collect();

    if offending.is_empty() {
        None
    } else {
        tracing::debug!("check_etc_hosts_override: found {} offending lines", offending.len());
        Some(Diagnostic {
            id: "etc_hosts_override".into(),
            severity: Severity::Warning,
            category: "dns".into(),
            title: "/etc/hosts overrides portzero.local".to_string(),
            detail: format!(
                "The following lines in {} override portzero.local:\n{}",
                hosts_path,
                offending.join("\n")
            ),
            fix: Some(Fix {
                kind: FixKind::Confirm,
                description: "Remove the portzero.local entries from /etc/hosts".to_string(),
                command: Some(r"sudo sed -i '/portzero\.local/d' /etc/hosts".to_string()),
            }),
        })
    }
}

#[cfg(target_os = "linux")]
fn check_dev_net_tun() -> Option<Diagnostic> {
    if std::fs::metadata("/dev/net/tun").is_err() {
        tracing::debug!("check_dev_net_tun: /dev/net/tun not available");
        Some(Diagnostic {
            id: "dev_net_tun_missing".into(),
            severity: Severity::Critical,
            category: "network".into(),
            title: "/dev/net/tun is not available".to_string(),
            detail: "The TUN kernel module is required to create virtual network devices.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Load the tun kernel module.".to_string(),
                command: Some("sudo modprobe tun".to_string()),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn check_dev_net_tun() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "linux")]
fn check_cap_net_admin() -> Option<Diagnostic> {
    let content = match std::fs::read_to_string("/proc/self/status") {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("check_cap_net_admin: failed to read /proc/self/status: {e}");
            return None;
        }
    };

    let cap_eff_hex = content.lines().find_map(|line| {
        line.strip_prefix("CapEff:")
            .map(|v| v.trim().to_string())
    });

    let cap_eff = match cap_eff_hex
        .as_deref()
        .and_then(|h| u64::from_str_radix(h, 16).ok())
    {
        Some(v) => v,
        None => {
            tracing::warn!("check_cap_net_admin: could not parse CapEff");
            return None;
        }
    };

    const CAP_NET_ADMIN: u64 = 1 << 12;
    if cap_eff & CAP_NET_ADMIN == 0 {
        tracing::debug!("check_cap_net_admin: CAP_NET_ADMIN not set (CapEff={:#x})", cap_eff);
        let exe_path = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/path/to/portzero".to_string());
        Some(Diagnostic {
            id: "cap_net_admin_missing".into(),
            severity: Severity::Critical,
            category: "network".into(),
            title: "CAP_NET_ADMIN capability is not set".to_string(),
            detail: "The daemon cannot create TUN devices without CAP_NET_ADMIN.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Grant CAP_NET_ADMIN to the binary.".to_string(),
                command: Some(format!("sudo setcap CAP_NET_ADMIN+ep {}", exe_path)),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn check_cap_net_admin() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "linux")]
fn check_ptrace_scope() -> Option<Diagnostic> {
    let content = match std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope") {
        Ok(c) => c,
        Err(_) => return None, // file absent on non-Yama kernels
    };

    let value: u32 = match content.trim().parse() {
        Ok(v) => v,
        Err(_) => return None,
    };

    if value >= 2 {
        tracing::debug!("check_ptrace_scope: ptrace_scope={}", value);
        Some(Diagnostic {
            id: "ptrace_scope_restricted".into(),
            severity: Severity::Warning,
            category: "system".into(),
            title: "ptrace_scope restricts process environment reading".to_string(),
            detail: format!(
                "ptrace_scope={}. The daemon cannot read PZ_TUNNEL from other processes. Values ≥2 require processes to opt in.",
                value
            ),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Relax the ptrace scope to allow environment variable reading.".to_string(),
                command: Some(
                    "echo 1 | sudo tee /proc/sys/kernel/yama/ptrace_scope".to_string(),
                ),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn check_ptrace_scope() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "linux")]
fn check_avahi_conflict() -> Option<Diagnostic> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let found = sys
        .processes()
        .values()
        .any(|p| p.name().to_string_lossy() == "avahi-daemon");

    if found {
        tracing::debug!("check_avahi_conflict: avahi-daemon is running");
        Some(Diagnostic {
            id: "avahi_conflict".into(),
            severity: Severity::Warning,
            category: "dns".into(),
            title: "avahi-daemon may intercept .local DNS queries".to_string(),
            detail: "avahi-daemon handles mDNS for .local domains and may respond before the portzero resolver.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Stop avahi-daemon or configure it to not handle portzero.local".to_string(),
                command: Some("sudo systemctl stop avahi-daemon".to_string()),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn check_avahi_conflict() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "linux")]
fn check_systemd_resolved() -> Option<Diagnostic> {
    let dir_exists = std::fs::metadata("/run/systemd/resolve/").is_ok();
    let resolv_exists = std::fs::metadata("/run/systemd/resolve/resolv.conf").is_ok();

    if !dir_exists && !resolv_exists {
        tracing::debug!("check_systemd_resolved: systemd-resolved not running");
        Some(Diagnostic {
            id: "systemd_resolved_not_running".into(),
            severity: Severity::Error,
            category: "dns".into(),
            title: "systemd-resolved is not running".to_string(),
            detail: "portzero uses systemd-resolved to register the scoped .portzero.local resolver. Without it, .portzero.local names will not resolve.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Enable and start systemd-resolved.".to_string(),
                command: Some("sudo systemctl enable --now systemd-resolved".to_string()),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn check_systemd_resolved() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "macos")]
fn check_macos_resolver_file() -> Option<Diagnostic> {
    if std::fs::metadata("/etc/resolver/portzero.local").is_err() {
        tracing::debug!("check_macos_resolver_file: /etc/resolver/portzero.local missing");
        Some(Diagnostic {
            id: "macos_resolver_missing".into(),
            severity: Severity::Error,
            category: "dns".into(),
            title: "/etc/resolver/portzero.local is missing".to_string(),
            detail: "macOS uses this file to route .portzero.local DNS queries to the portzero resolver.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Confirm,
                description: "Restart the portzero daemon with admin privileges to recreate the resolver file".to_string(),
                command: Some("sudo portzero restart".to_string()),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn check_macos_resolver_file() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "windows")]
fn check_wintun_present() -> Option<Diagnostic> {
    let missing = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wintun.dll")))
        .map(|dll| !dll.exists())
        .unwrap_or(false);

    if missing {
        tracing::debug!("check_wintun_present: wintun.dll not found");
        Some(Diagnostic {
            id: "wintun_missing".into(),
            severity: Severity::Critical,
            category: "network".into(),
            title: "wintun.dll is missing".to_string(),
            detail: "portzero requires Wintun to create the virtual network adapter on Windows.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Re-run the portzero installer, or download wintun.dll from https://wintun.net and place it next to portzero.exe".to_string(),
                command: None,
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "windows"))]
fn check_wintun_present() -> Option<Diagnostic> {
    None
}

fn check_conflicting_vpn_software() -> Option<Diagnostic> {
    const VPN_NAMES: &[&str] = &[
        "tailscaled",
        "tailscale",
        "openconnect",
        "vpnc",
        "openvpn",
        "GlobalProtect",
        "PanGPS",
        "ciscovpn",
        "vpnagentd",
        "ZscalerApp",
        "wg-quick",
        "Pulse Secure",
        "PulseSecureService",
        "ivpnagent",
    ];

    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let detected: Vec<String> = sys
        .processes()
        .values()
        .filter_map(|p| {
            let name = p.name().to_string_lossy();
            if VPN_NAMES.iter().any(|vpn| name.contains(vpn)) {
                Some(name.into_owned())
            } else {
                None
            }
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if detected.is_empty() {
        None
    } else {
        tracing::debug!("check_conflicting_vpn_software: detected {:?}", detected);
        let mut sorted = detected;
        sorted.sort();
        Some(Diagnostic {
            id: "vpn_software_detected".into(),
            severity: Severity::Info,
            category: "network".into(),
            title: "VPN software is running".to_string(),
            detail: format!(
                "Detected: {}. VPNs may conflict with the 10.254.0.0/16 overlay range or intercept DNS.",
                sorted.join(", ")
            ),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "If .portzero.local names don't resolve, check your VPN's split-tunneling settings to exclude 10.254.0.0/16".to_string(),
                command: None,
            }),
        })
    }
}

fn check_auth_token(state_dir: &Path) -> Option<Diagnostic> {
    // config_dir is ~/.portzero/ (parent of the daemon's state_dir ~/.portzero/daemon/)
    let config_dir = state_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| state_dir.to_path_buf());

    let auth = crate::auth::AuthConfig::load_from(&config_dir);

    match auth.token {
        None => {
            tracing::debug!("check_auth_token: no token present");
            Some(Diagnostic {
                id: "auth_not_logged_in".into(),
                severity: Severity::Info,
                category: "auth".into(),
                title: "Not logged in to portzero.cloud".to_string(),
                detail: "Cloud tunnels are unavailable. Run `portzero login` to enable them."
                    .to_string(),
                fix: Some(Fix {
                    kind: FixKind::Manual,
                    description: "Log in to enable cloud tunnel support.".to_string(),
                    command: Some("portzero login".to_string()),
                }),
            })
        }
        Some(token) => {
            // Decode JWT payload (index 1) to get `exp`
            let exp: Option<u64> = token.split('.').nth(1).and_then(|b64| {
                let bytes = URL_SAFE_NO_PAD.decode(b64).ok()?;
                let val: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                val.get("exp")?.as_u64()
            });

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            match exp {
                None => None, // can't decode expiry — assume valid
                Some(exp) if exp < now => {
                    tracing::debug!("check_auth_token: token expired at {}", exp);
                    Some(Diagnostic {
                        id: "auth_token_expired".into(),
                        severity: Severity::Warning,
                        category: "auth".into(),
                        title: "Cloud auth token has expired".to_string(),
                        detail: "The stored auth token is no longer valid. Re-authenticate to restore cloud tunnel access.".to_string(),
                        fix: Some(Fix {
                            kind: FixKind::Manual,
                            description: "Re-authenticate with portzero.cloud.".to_string(),
                            command: Some("portzero login".to_string()),
                        }),
                    })
                }
                Some(exp) if exp < now + 86400 => {
                    tracing::debug!("check_auth_token: token expires soon at {}", exp);
                    Some(Diagnostic {
                        id: "auth_token_expired".into(),
                        severity: Severity::Warning,
                        category: "auth".into(),
                        title: "Cloud auth token expires soon (within 24 hours)".to_string(),
                        detail: "Re-authenticate soon to avoid losing cloud tunnel access.".to_string(),
                        fix: Some(Fix {
                            kind: FixKind::Manual,
                            description: "Re-authenticate with portzero.cloud.".to_string(),
                            command: Some("portzero login".to_string()),
                        }),
                    })
                }
                Some(_) => None, // token is valid and not expiring soon
            }
        }
    }
}

fn check_ca_cert_exists() -> Option<Diagnostic> {
    match crate::tls::ca::LocalCa::ca_cert_path() {
        Ok(path) if !path.exists() => {
            tracing::debug!("check_ca_cert_exists: CA cert missing at {}", path.display());
            Some(Diagnostic {
                id: "ca_cert_missing".into(),
                severity: Severity::Error,
                category: "tls".into(),
                title: "Local CA certificate file is missing".to_string(),
                detail: "The wildcard cert for *.portzero.local cannot be served.".to_string(),
                fix: Some(Fix {
                    kind: FixKind::Confirm,
                    description: "Restart the daemon to regenerate the CA certificate".to_string(),
                    command: Some("portzero restart".to_string()),
                }),
            })
        }
        Err(e) => {
            tracing::warn!("check_ca_cert_exists: could not determine CA cert path: {e}");
            None
        }
        Ok(_) => None,
    }
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Run all diagnostic checks and return a report.
///
/// Blocking I/O is executed via [`tokio::task::spawn_blocking`] to keep the
/// async executor free.
pub async fn run_diagnostics(state_dir: &std::path::Path) -> DiagnosticsReport {
    let state_dir: PathBuf = state_dir.to_path_buf();

    let (issues, checks_run) = tokio::task::spawn_blocking(move || {
        let mut out: Vec<Diagnostic> = Vec::new();
        let mut checks_run: usize = 0;

        macro_rules! run {
            ($check:expr) => {{
                checks_run += 1;
                if let Some(d) = $check {
                    out.push(d);
                }
            }};
        }

        run!(check_binary_exists());
        run!(check_multiple_instances());
        run!(check_state_dir_writable(&state_dir));
        run!(check_state_files_valid(&state_dir));
        run!(check_fd_limit());
        run!(check_etc_hosts_override());
        run!(check_dev_net_tun());
        run!(check_cap_net_admin());
        run!(check_ptrace_scope());
        run!(check_avahi_conflict());
        run!(check_systemd_resolved());
        run!(check_macos_resolver_file());
        run!(check_wintun_present());
        run!(check_conflicting_vpn_software());
        run!(check_auth_token(&state_dir));
        run!(check_ca_cert_exists());

        out.sort_by(|a, b| a.severity.cmp(&b.severity));
        (out, checks_run)
    })
    .await
    .unwrap_or_else(|_| (Vec::new(), 0));

    DiagnosticsReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        checks_run,
        issues,
    }
}

/// Persist a diagnostics report to `<state_dir>/diagnostics.json`.
pub fn save_report(report: &DiagnosticsReport, state_dir: &std::path::Path) {
    let path = state_dir.join("diagnostics.json");
    match serde_json::to_string_pretty(report) {
        Ok(json) => {
            let _ = std::fs::write(&path, json);
        }
        Err(e) => tracing::warn!("failed to serialize diagnostics: {e}"),
    }
}

/// Load the most recently persisted diagnostics report from `<state_dir>/diagnostics.json`.
pub fn load_report(state_dir: &std::path::Path) -> Option<DiagnosticsReport> {
    let content = std::fs::read_to_string(state_dir.join("diagnostics.json")).ok()?;
    serde_json::from_str(&content).ok()
}
