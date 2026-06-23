//! Auto-start support: install/uninstall the daemon as a system service.
//!
//! - macOS: LaunchAgent plist in ~/Library/LaunchAgents/
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

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", SERVICE_NAME))
}

#[cfg(target_os = "macos")]
fn install_launchd(binary: &std::path::Path) -> Result<()> {
    let plist_path = launchd_plist_path();
    let plist_dir = plist_path.parent().expect(
        "launchd plist path has no parent directory — this is a bug in launchd_plist_path()",
    );
    std::fs::create_dir_all(plist_dir)?;

    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".devenv/daemon");
    std::fs::create_dir_all(&log_dir)?;

    let plist = format!(
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
        binary = binary.display(),
        log_dir = log_dir.display(),
    );

    std::fs::write(&plist_path, &plist).with_context(|| {
        format!(
            "Failed to write LaunchAgent plist to {}",
            plist_path.display()
        )
    })?;

    // Load the agent
    let status = std::process::Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .status()
        .context("Failed to run launchctl load")?;

    if !status.success() {
        tracing::warn!("launchctl load returned non-zero; the agent may already be loaded");
    }

    tracing::info!("Installed LaunchAgent: {}", plist_path.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<()> {
    let plist_path = launchd_plist_path();

    if plist_path.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&plist_path)
            .status();

        std::fs::remove_file(&plist_path).with_context(|| {
            format!(
                "Failed to remove LaunchAgent plist at {}",
                plist_path.display()
            )
        })?;

        tracing::info!("Uninstalled LaunchAgent: {}", plist_path.display());
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
        assert!(path.to_string_lossy().contains("LaunchAgents"));
        assert!(path.to_string_lossy().contains(SERVICE_NAME));
    }
}
