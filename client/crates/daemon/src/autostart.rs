//! Auto-start support: install/uninstall the daemon as a system service.
//!
//! - macOS: root LaunchDaemon plist in /Library/LaunchDaemons/ (runs as root,
//!   so the privileged overlay — utun + /etc/resolver + routes — actually comes
//!   up; install/uninstall therefore require `sudo`).
//! - Linux: systemd user unit in ~/.config/systemd/user/
//! - Windows: scheduled task (start at logon)

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Service label / unit name used across platforms.
#[allow(dead_code)]
const SERVICE_NAME: &str = "tools.devenv.daemon";

/// Install auto-start for the daemon.
///
/// After installation, the daemon starts automatically on user login.
pub fn install_autostart() -> Result<()> {
    let binary = find_daemon_binary()?;

    #[cfg(target_os = "macos")]
    install_launchd(&binary)?;

    #[cfg(target_os = "linux")]
    install_systemd(&binary)?;

    #[cfg(target_os = "windows")]
    install_windows_task(&binary)?;

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = binary;
        anyhow::bail!(
            "Auto-start is not supported on this platform.\n\n\
             You can still run the daemon manually with: devenv tunnel daemon"
        );
    }

    Ok(())
}

/// Uninstall auto-start for the daemon.
pub fn uninstall_autostart() -> Result<()> {
    #[cfg(target_os = "macos")]
    uninstall_launchd()?;

    #[cfg(target_os = "linux")]
    uninstall_systemd()?;

    #[cfg(target_os = "windows")]
    uninstall_windows_task()?;

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    anyhow::bail!("Auto-start is not supported on this platform.");

    Ok(())
}

/// Check whether auto-start is installed.
pub fn is_autostart_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        launchd_plist_path().exists()
    }

    #[cfg(target_os = "linux")]
    {
        systemd_unit_path().exists()
    }

    #[cfg(target_os = "windows")]
    {
        is_windows_task_installed()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    false
}

// ---------------------------------------------------------------------------
// Binary location
// ---------------------------------------------------------------------------

/// Find the path to the devenv-tunnel binary.
fn find_daemon_binary() -> Result<PathBuf> {
    // First: check if we're running as the binary ourselves
    let current_exe =
        std::env::current_exe().context("Failed to determine current executable path")?;

    // If the current exe looks like devenv-tunnel, use it
    if let Some(name) = current_exe.file_name() {
        let name_str = name.to_string_lossy();
        if name_str.starts_with("devenv-tunnel") {
            return Ok(current_exe);
        }
    }

    // Otherwise, search PATH
    if let Ok(path) = which("devenv-tunnel") {
        return Ok(path);
    }

    anyhow::bail!(
        "Could not find devenv-tunnel binary.\n\n\
         Ensure devenv is installed and on your PATH, then retry.\n\
         Install with: curl -fsSL https://devenv.tools/install.sh | sh"
    )
}

/// Simple which(1) equivalent.
fn which(name: &str) -> Result<PathBuf> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    for dir in path_env.split(':') {
        let candidate = PathBuf::from(dir).join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("{} not found in PATH", name)
}

// ---------------------------------------------------------------------------
// macOS: LaunchAgent
// ---------------------------------------------------------------------------

/// System LaunchDaemon directory. The daemon must run as **root** so the
/// privileged overlay (utun + `/etc/resolver` + routes) can come up, so the
/// plist lives in the system domain rather than the per-user LaunchAgents dir.
#[cfg(target_os = "macos")]
const LAUNCHDAEMONS_DIR: &str = "/Library/LaunchDaemons";

/// Root-writable log directory. A LaunchDaemon runs as root, whose home is not
/// the installing user's, so logs cannot live under `~/.devenv`.
#[cfg(target_os = "macos")]
const DAEMON_LOG_DIR: &str = "/Library/Logs/devenv";

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> PathBuf {
    PathBuf::from(LAUNCHDAEMONS_DIR).join(format!("{}.plist", SERVICE_NAME))
}

/// Build the LaunchDaemon plist XML. Pure and unit-testable (no I/O), so the
/// generated content can be asserted without root.
#[cfg(target_os = "macos")]
fn render_launchd_plist(binary: &str, log_dir: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_dir}/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/daemon.log</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        label = SERVICE_NAME,
        binary = binary,
        log_dir = log_dir,
    )
}

/// Fail with an actionable message if we are not root. Writing to
/// `/Library/LaunchDaemons` and bootstrapping the system domain both require it.
#[cfg(target_os = "macos")]
fn require_root(action: &str) -> Result<()> {
    // SAFETY: geteuid is always safe to call.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        anyhow::bail!(
            "Installing the devenv-tunnel autostart service as a root LaunchDaemon \
             requires administrator privileges to {action} {dir}.\n\n\
             Re-run this command with sudo, e.g.:\n    \
             sudo devenv-tunnel daemon {verb}",
            action = action,
            dir = LAUNCHDAEMONS_DIR,
            verb = if action.contains("remove") {
                "autostart-disable"
            } else {
                "autostart-enable"
            },
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_launchd(binary: &std::path::Path) -> Result<()> {
    require_root("write to")?;

    let plist_path = launchd_plist_path();
    let plist_dir = plist_path.parent().expect(
        "launchd plist path has no parent directory — this is a bug in launchd_plist_path()",
    );
    std::fs::create_dir_all(plist_dir)
        .with_context(|| format!("Failed to create {}", plist_dir.display()))?;

    let log_dir = PathBuf::from(DAEMON_LOG_DIR);
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("Failed to create log directory {}", log_dir.display()))?;

    let plist = render_launchd_plist(&binary.display().to_string(), DAEMON_LOG_DIR);

    std::fs::write(&plist_path, &plist).with_context(|| {
        format!(
            "Failed to write LaunchDaemon plist to {}",
            plist_path.display()
        )
    })?;

    // Modern launchctl: `bootstrap system <plist>` loads a system daemon
    // (`load -w` is deprecated for the system domain).
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", "system"])
        .arg(&plist_path)
        .status()
        .context("Failed to run launchctl bootstrap")?;

    if !status.success() {
        // Already-loaded is the common non-zero case; fall back to the legacy
        // verb so older systems still load the daemon.
        tracing::warn!(
            "launchctl bootstrap returned non-zero; the daemon may already be loaded. \
             Falling back to legacy `load -w`."
        );
        let _ = std::process::Command::new("launchctl")
            .args(["load", "-w"])
            .arg(&plist_path)
            .status();
    }

    tracing::info!("Installed LaunchDaemon: {}", plist_path.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<()> {
    require_root("remove from")?;

    let plist_path = launchd_plist_path();

    if plist_path.exists() {
        // Modern launchctl: `bootout system/<label>` unloads a system daemon
        // (`unload -w` is deprecated for the system domain).
        let status = std::process::Command::new("launchctl")
            .arg("bootout")
            .arg(format!("system/{}", SERVICE_NAME))
            .status();

        if !matches!(status, Ok(s) if s.success()) {
            // Fall back to the legacy verb if bootout isn't available / fails.
            let _ = std::process::Command::new("launchctl")
                .args(["unload", "-w"])
                .arg(&plist_path)
                .status();
        }

        std::fs::remove_file(&plist_path).with_context(|| {
            format!(
                "Failed to remove LaunchDaemon plist at {}",
                plist_path.display()
            )
        })?;

        tracing::info!("Uninstalled LaunchDaemon: {}", plist_path.display());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Linux: systemd user unit
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/systemd/user")
        .join("devenv-daemon.service")
}

#[cfg(target_os = "linux")]
fn install_systemd(binary: &std::path::Path) -> Result<()> {
    let unit_path = systemd_unit_path();
    let unit_dir = unit_path
        .parent()
        .expect("systemd unit path has no parent directory — this is a bug in systemd_unit_path()");
    std::fs::create_dir_all(unit_dir)?;

    let unit = format!(
        r#"[Unit]
Description=devenv discovery daemon
Documentation=https://devenv.tools/docs/daemon

[Service]
Type=simple
ExecStart={binary} daemon
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
"#,
        binary = binary.display(),
    );

    std::fs::write(&unit_path, &unit)
        .with_context(|| format!("Failed to write systemd unit to {}", unit_path.display()))?;

    // Reload and enable
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "devenv-daemon.service"])
        .status()
        .context("Failed to enable systemd unit")?;

    if !status.success() {
        tracing::warn!("systemctl enable returned non-zero; the unit may already be enabled");
    }

    tracing::info!("Installed systemd user unit: {}", unit_path.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd() -> Result<()> {
    let unit_path = systemd_unit_path();

    if unit_path.exists() {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "devenv-daemon.service"])
            .status();

        std::fs::remove_file(&unit_path)
            .with_context(|| format!("Failed to remove systemd unit at {}", unit_path.display()))?;

        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();

        tracing::info!("Uninstalled systemd user unit: {}", unit_path.display());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Windows: scheduled task
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn install_windows_task(binary: &PathBuf) -> Result<()> {
    use std::process::Command;

    let result = Command::new("schtasks")
        .args([
            "/Create",
            "/SC",
            "ONLOGON",
            "/TN",
            SERVICE_NAME,
            "/TR",
            &format!("\"{}\" daemon", binary.display()),
            "/RL",
            "LIMITED",
            "/F",
        ])
        .status()
        .context("Failed to create scheduled task")?;

    if !result.success() {
        anyhow::bail!(
            "Failed to create Windows scheduled task.\n\n\
             Try running as administrator, or create a shortcut in your \
             Startup folder manually."
        );
    }

    tracing::info!("Installed Windows scheduled task: {}", SERVICE_NAME);
    Ok(())
}

#[cfg(target_os = "windows")]
fn uninstall_windows_task() -> Result<()> {
    use std::process::Command;

    let result = Command::new("schtasks")
        .args(["/Delete", "/TN", SERVICE_NAME, "/F"])
        .status()
        .context("Failed to delete scheduled task")?;

    if !result.success() {
        tracing::warn!("schtasks delete returned non-zero; the task may not exist");
    }

    tracing::info!("Uninstalled Windows scheduled task: {}", SERVICE_NAME);
    Ok(())
}

#[cfg(target_os = "windows")]
fn is_windows_task_installed() -> bool {
    use std::process::Command;

    Command::new("schtasks")
        .args(["/Query", "/TN", SERVICE_NAME])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_autostart_installed_default() {
        // On a fresh system, autostart should not be installed
        // (unless the test machine happens to have it)
        let _result = is_autostart_installed();
        // Just ensure it doesn't panic
    }

    #[test]
    fn test_which_nonexistent() {
        let result = which("nonexistent-binary-xyz-123");
        assert!(result.is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_systemd_unit_path() {
        let path = systemd_unit_path();
        assert!(path.to_string_lossy().contains("systemd/user"));
        assert!(path.to_string_lossy().contains("devenv-daemon.service"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_launchd_plist_path() {
        let path = launchd_plist_path();
        // Must be a *system* LaunchDaemon (runs as root), not a user LaunchAgent.
        assert!(path.starts_with("/Library/LaunchDaemons"));
        assert!(!path.to_string_lossy().contains("LaunchAgents"));
        assert!(path.to_string_lossy().contains(SERVICE_NAME));
        assert!(path.to_string_lossy().ends_with(".plist"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_render_launchd_plist_is_well_formed_root_daemon() {
        let plist = render_launchd_plist("/usr/local/bin/devenv-tunnel", DAEMON_LOG_DIR);

        // Valid plist scaffolding.
        assert!(plist.contains("<!DOCTYPE plist"));
        assert!(plist.contains("<plist version=\"1.0\">"));
        assert!(plist.trim_end().ends_with("</plist>"));

        // Correct label + ProgramArguments [binary, "daemon"].
        assert!(plist.contains(&format!("<string>{SERVICE_NAME}</string>")));
        assert!(plist.contains("<string>/usr/local/bin/devenv-tunnel</string>"));
        assert!(plist.contains("<string>daemon</string>"));

        // Daemon keys.
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>ProcessType</key>"));
        assert!(plist.contains("<string>Background</string>"));

        // Logs go to a root-writable path, NOT the user's home.
        assert!(plist.contains("/Library/Logs/devenv/daemon.log"));
        assert!(!plist.contains("/.devenv"));
    }
}
