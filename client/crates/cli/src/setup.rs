//! One-shot privileged setup for package managers and first-run instructions.

use std::io::Write;

use anyhow::{Context, Result};

use crate::{autostart, trust};

const HOSTS_PATH: &str = "/etc/hosts";
const DASHBOARD_HOSTS_LINE: &str = "10.254.0.2 portzero.local # portzero-local";
const DASHBOARD_HOST: &str = "portzero.local";
const DASHBOARD_IP: &str = "10.254.0.2";

/// Run the privileged setup steps that package managers should not execute
/// automatically: trust install, autostart install/start, and the dashboard
/// hosts pin needed for macOS `.local` behavior.
pub fn run() -> Result<()> {
    println!("PortZero setup will make these system changes:");
    println!("  - generate the local CA if it does not already exist");
    println!("  - install the local CA into available OS/browser trust stores");
    println!("  - install and start the PortZero autostart daemon");
    println!("  - ensure /etc/hosts contains: {DASHBOARD_HOSTS_LINE}");
    println!();

    println!("Generating local CA...");
    trust::generate()?;

    println!("Installing local CA trust...");
    trust::install()?;

    println!("Installing and starting autostart daemon...");
    autostart::enable()?;

    println!("Ensuring dashboard hosts entry...");
    ensure_dashboard_hosts_entry(HOSTS_PATH)?;

    println!();
    println!("Setup complete. Open http://portzero.local in your browser.");
    Ok(())
}

fn ensure_dashboard_hosts_entry(path: &str) -> Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    if has_expected_dashboard_hosts_entry(&content) {
        println!("Hosts entry already present: {DASHBOARD_HOSTS_LINE}");
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| {
            format!("Failed to open {path}. Re-run setup with administrator privileges.")
        })?;

    if !content.is_empty() && !content.ends_with('\n') {
        writeln!(file).with_context(|| format!("Failed to append newline to {path}"))?;
    }
    writeln!(file, "{DASHBOARD_HOSTS_LINE}")
        .with_context(|| format!("Failed to append PortZero hosts entry to {path}"))?;

    println!("Added hosts entry: {DASHBOARD_HOSTS_LINE}");
    Ok(())
}

fn has_expected_dashboard_hosts_entry(content: &str) -> bool {
    content.lines().any(|line| {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            return false;
        }
        let mut fields = line.split_whitespace();
        let Some(ip) = fields.next() else {
            return false;
        };
        ip == DASHBOARD_IP && fields.any(|name| name == DASHBOARD_HOST)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_expected_dashboard_hosts_entry_with_marker() {
        let hosts = "127.0.0.1 localhost\n10.254.0.2 portzero.local # portzero-local\n";
        assert!(has_expected_dashboard_hosts_entry(hosts));
    }

    #[test]
    fn detects_expected_dashboard_hosts_entry_without_marker() {
        let hosts = "10.254.0.2 api.portzero.local portzero.local\n";
        assert!(has_expected_dashboard_hosts_entry(hosts));
    }

    #[test]
    fn ignores_commented_dashboard_hosts_entry() {
        let hosts = "# 10.254.0.2 portzero.local # portzero-local\n";
        assert!(!has_expected_dashboard_hosts_entry(hosts));
    }

    #[test]
    fn rejects_wrong_dashboard_ip() {
        let hosts = "127.0.0.1 portzero.local\n";
        assert!(!has_expected_dashboard_hosts_entry(hosts));
    }
}
