//! CLI handlers for CA certificate generation and OS trust store management.

use portzero_daemon::tls::{trust, LocalCa};

/// Generate the local CA certificate and key, persisting them to the platform
/// data directory.  Safe to call multiple times — existing certs are kept.
/// Does not require elevated privileges.
pub fn generate() -> anyhow::Result<()> {
    let _ca = LocalCa::load_or_create()?;
    let path = LocalCa::ca_cert_path()?;
    println!("CA certificate ready: {}", path.display());
    Ok(())
}

/// Install the local CA into the OS trust store so browsers and system tools
/// accept `*.portzero.local` without certificate warnings.
///
/// Requires elevated privileges on Linux (`sudo -E`) and macOS (`sudo`).
/// On Linux, run `portzero trust generate` first (as the regular user) so the
/// certificate exists before this command is invoked as root.
pub fn install() -> anyhow::Result<()> {
    let path = LocalCa::ca_cert_path()?;
    if !path.exists() {
        anyhow::bail!(
            "CA certificate not found at {}. Run `portzero trust generate` first (without sudo).",
            path.display()
        );
    }
    trust::install(&path)?;
    println!("CA certificate installed to system trust store.");
    Ok(())
}

/// Remove the local CA from the OS trust store.
///
/// Requires elevated privileges on Linux and macOS.
pub fn uninstall() -> anyhow::Result<()> {
    trust::uninstall()?;
    println!("CA certificate removed from system trust store.");
    Ok(())
}
