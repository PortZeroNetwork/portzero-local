//! Scoped OS DNS resolver configuration for `*.devenv.local`.
//!
//! Points the OS at our embedded DNS server for ONLY the `devenv.local` domain,
//! without hijacking the whole system resolver. Three platforms are supported:
//!
//! - **macOS**: writes `/etc/resolver/devenv.local` (requires privileges).
//! - **Linux**: calls `resolvectl dns` and `resolvectl domain` (systemd-resolved).
//! - **Windows**: calls `Add-DnsClientNrptRule` via PowerShell.
//!
//! On unsupported platforms (other Unix platforms) the calls compile to no-ops with a
//! logged warning.
//!
//! # Design for testability
//!
//! Privilege-requiring writes are done by [`install`] / [`uninstall`].  All the
//! pure content-generation helpers (`macos_resolver_file_content`,
//! `resolvectl_setup_args`, …) are `pub(crate)` so they can be unit-tested
//! without touching the real system.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tracing::info;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
use tracing::warn;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Install the scoped resolver for `*.devenv.local`, routing queries to
/// `dns_addr` (our embedded DNS server).
///
/// May require elevated privileges on macOS (needs write access to
/// `/etc/resolver/`). On Linux, `resolvectl` talks to systemd-resolved over
/// D-Bus and usually does not require root when called by a service. On
/// Windows, `Add-DnsClientNrptRule` requires Administrator.
pub async fn install(dns_addr: SocketAddr) -> Result<()> {
    install_impl(dns_addr)
}

/// Remove the scoped resolver configuration installed by [`install`].
pub async fn uninstall() -> Result<()> {
    uninstall_impl()
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn install_impl(dns_addr: SocketAddr) -> Result<()> {
    use std::fs;
    use std::path::Path;

    let dir = Path::new("/etc/resolver");
    if !dir.exists() {
        fs::create_dir_all(dir).context("creating /etc/resolver")?;
    }
    let path = dir.join("devenv.local");
    let content = macos_resolver_file_content(dns_addr);
    fs::write(&path, content)
        .with_context(|| format!("writing {}", path.display()))?;
    info!("macOS: wrote scoped resolver {}", path.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_impl() -> Result<()> {
    use std::path::Path;
    let path = Path::new("/etc/resolver/devenv.local");
    if path.exists() {
        std::fs::remove_file(path).context("removing /etc/resolver/devenv.local")?;
        info!("macOS: removed scoped resolver /etc/resolver/devenv.local");
    }
    Ok(())
}

/// Generate the content of `/etc/resolver/devenv.local`.
/// The `port` line is needed when the server listens on a non-standard port.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn macos_resolver_file_content(dns_addr: SocketAddr) -> String {
    let mut out = format!("# Managed by devenv-tunnel — do not edit by hand.\n");
    out.push_str(&format!("nameserver {}\n", dns_addr.ip()));
    if dns_addr.port() != 53 {
        out.push_str(&format!("port {}\n", dns_addr.port()));
    }
    out
}

// ---------------------------------------------------------------------------
// Linux (systemd-resolved via resolvectl)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn install_impl(dns_addr: SocketAddr) -> Result<()> {
    // We need a network interface name to attach the resolver to.
    // Using the loopback interface here would not actually scope the resolver.
    // systemd-resolved requires a link to attach per-link DNS settings.
    // The idiomatic approach is to use a dummy/loopback link or the TUN link.
    // For maximum compatibility we create a virtual dummy link and use that,
    // but that requires `ip` commands.  A simpler approach supported by
    // systemd-resolved is to use the global DNS stub + split-DNS via `resolvectl`.
    //
    // We use the loopback interface index as the attachment point, then set
    // the domain restriction to "~devenv.local" (tilde prefix = routing domain
    // only, never search domain) and the DNS server.
    //
    // This requires systemd-resolved to be active. We fail gracefully when it
    // is not.

    let link = loopback_link_name()?;
    run_resolvectl(&resolvectl_dns_args(&link, dns_addr))
        .context("resolvectl dns")?;
    run_resolvectl(&resolvectl_domain_args(&link))
        .context("resolvectl domain")?;
    info!("Linux: configured systemd-resolved for devenv.local via link {}", link);
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_impl() -> Result<()> {
    let link = loopback_link_name()?;
    // Revert the link-level DNS and domain settings to defaults.
    run_resolvectl(&["revert", &link]).context("resolvectl revert")?;
    info!("Linux: reverted systemd-resolved settings for link {}", link);
    Ok(())
}

/// Returns the resolvectl args to set the DNS server for a link.
/// Exported for testing.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn resolvectl_dns_args(link: &str, dns_addr: SocketAddr) -> Vec<String> {
    // resolvectl dns <link> <ip>:<port>  (port supported since systemd 245)
    let addr_str = format!("{}:{}", dns_addr.ip(), dns_addr.port());
    vec!["dns".to_string(), link.to_string(), addr_str]
}

/// Returns the resolvectl args to restrict a link to the devenv.local domain.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn resolvectl_domain_args(link: &str) -> Vec<String> {
    // ~devenv.local means "routing domain only" — queries for devenv.local go
    // here but it is not used as a search domain.
    vec!["domain".to_string(), link.to_string(), "~devenv.local".to_string()]
}

#[cfg(target_os = "linux")]
fn loopback_link_name() -> Result<String> {
    // We use a stable dummy loopback device name.  The actual loopback ("lo")
    // works for local-only listeners on 127.0.0.1, which is exactly our case.
    Ok("lo".to_string())
}

#[cfg(target_os = "linux")]
fn run_resolvectl(args: &[impl AsRef<std::ffi::OsStr>]) -> Result<()> {
    let status = std::process::Command::new("resolvectl")
        .args(args)
        .status()
        .context("running resolvectl (is systemd-resolved active?)")?;
    if !status.success() {
        anyhow::bail!("resolvectl exited with {}", status);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn install_impl(dns_addr: SocketAddr) -> Result<()> {
    let script = powershell_add_nrpt_script(dns_addr);
    run_powershell(&script).context("Add-DnsClientNrptRule")?;
    info!("Windows: added NRPT rule for devenv.local -> {}", dns_addr);
    Ok(())
}

#[cfg(target_os = "windows")]
fn uninstall_impl() -> Result<()> {
    let script = powershell_remove_nrpt_script();
    run_powershell(&script).context("Remove-DnsClientNrptRule")?;
    info!("Windows: removed NRPT rule for devenv.local");
    Ok(())
}

/// PowerShell snippet that adds (or replaces) the NRPT rule.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn powershell_add_nrpt_script(dns_addr: SocketAddr) -> String {
    // Remove any existing rule for this namespace first to make it idempotent.
    // NRPT requires the namespace with a leading dot.
    format!(
        r#"
Get-DnsClientNrptRule | Where-Object {{ $_.Namespace -eq '.devenv.local' }} | Remove-DnsClientNrptRule -Force -ErrorAction SilentlyContinue
Add-DnsClientNrptRule -Namespace '.devenv.local' -NameServers '{ip}' -Comment 'devenv-tunnel managed'
"#,
        ip = dns_addr.ip()
    )
}

/// PowerShell snippet that removes the NRPT rule.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn powershell_remove_nrpt_script() -> String {
    r#"
Get-DnsClientNrptRule | Where-Object { $_.Namespace -eq '.devenv.local' } | Remove-DnsClientNrptRule -Force -ErrorAction SilentlyContinue
"#
    .to_string()
}

#[cfg(target_os = "windows")]
fn run_powershell(script: &str) -> Result<()> {
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .status()
        .context("running powershell.exe")?;
    if !status.success() {
        anyhow::bail!("PowerShell exited with {}", status);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Unsupported platforms — compile to no-ops with a warning
// ---------------------------------------------------------------------------

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn install_impl(dns_addr: SocketAddr) -> Result<()> {
    warn!(
        "scoped resolver not supported on this platform; \
         configure your OS to send *.devenv.local queries to {}",
        dns_addr
    );
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn uninstall_impl() -> Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests — purely functional; never write to the real system
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn addr(ip: &str, port: u16) -> SocketAddr {
        SocketAddr::new(ip.parse::<IpAddr>().unwrap(), port)
    }

    // --- macOS ---

    #[test]
    fn macos_content_standard_port() {
        let content = macos_resolver_file_content(addr("127.0.0.1", 53));
        assert!(content.contains("nameserver 127.0.0.1\n"), "missing nameserver line");
        assert!(!content.contains("port"), "should not emit port line for port 53");
    }

    #[test]
    fn macos_content_custom_port() {
        let content = macos_resolver_file_content(addr("127.0.0.1", 5300));
        assert!(content.contains("nameserver 127.0.0.1\n"));
        assert!(content.contains("port 5300\n"), "should emit port line for non-53 port");
    }

    #[test]
    fn macos_content_comment() {
        let content = macos_resolver_file_content(addr("127.0.0.1", 5300));
        assert!(content.contains("devenv-tunnel"), "should have management comment");
    }

    // --- Linux ---

    #[test]
    fn resolvectl_dns_args_format() {
        let args = resolvectl_dns_args("lo", addr("127.0.0.1", 5300));
        assert_eq!(args[0], "dns");
        assert_eq!(args[1], "lo");
        assert_eq!(args[2], "127.0.0.1:5300");
    }

    #[test]
    fn resolvectl_domain_args_format() {
        let args = resolvectl_domain_args("lo");
        assert_eq!(args[0], "domain");
        assert_eq!(args[1], "lo");
        assert_eq!(args[2], "~devenv.local", "must use routing-domain tilde prefix");
    }

    #[test]
    fn resolvectl_domain_not_search_domain() {
        // The domain must start with '~' so it is a routing domain, not a
        // search domain that would affect general hostname resolution.
        let args = resolvectl_domain_args("eth0");
        assert!(
            args[2].starts_with('~'),
            "domain must start with '~' to be a routing domain"
        );
    }

    // --- Windows ---

    #[test]
    fn powershell_add_script_contains_namespace() {
        let script = powershell_add_nrpt_script(addr("127.0.0.1", 5300));
        assert!(script.contains(".devenv.local"), "must reference .devenv.local namespace");
        assert!(script.contains("127.0.0.1"), "must reference the DNS server IP");
        assert!(script.contains("Add-DnsClientNrptRule"), "must call Add-DnsClientNrptRule");
    }

    #[test]
    fn powershell_add_script_idempotent() {
        // The script must remove any existing rule before adding, for idempotency.
        let script = powershell_add_nrpt_script(addr("127.0.0.1", 5300));
        assert!(
            script.contains("Remove-DnsClientNrptRule"),
            "must clean up existing rule before adding"
        );
    }

    #[test]
    fn powershell_remove_script_targets_namespace() {
        let script = powershell_remove_nrpt_script();
        assert!(script.contains(".devenv.local"));
        assert!(script.contains("Remove-DnsClientNrptRule"));
    }

    // --- Scope guard: only devenv.local ---

    #[test]
    fn macos_does_not_mention_other_domains() {
        let content = macos_resolver_file_content(addr("127.0.0.1", 5300));
        // The resolver file contains no wildcard that would affect other domains.
        assert!(
            !content.contains("search"),
            "resolver file must not add search domains"
        );
    }

    #[test]
    fn resolvectl_args_scope_only_devenv_local() {
        let domain_args = resolvectl_domain_args("lo");
        // Only one domain listed, and it must be ~devenv.local.
        assert_eq!(domain_args.len(), 3, "exactly [domain, link, ~devenv.local]");
        assert_eq!(domain_args[2], "~devenv.local");
    }
}
