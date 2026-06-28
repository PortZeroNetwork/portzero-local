//! Local CA: generates and persists a self-signed CA and wildcard TLS cert
//! for the *.portzero.local zone.
//!
//! On first use (or when the wildcard cert nears expiry), generates:
//!   - A 10-year self-signed CA ("PortZero Local CA")
//!   - A 1-year wildcard cert for *.portzero.local signed by that CA
//!
//! Both are stored as PEM files in the platform data directory:
//!   - macOS:   ~/Library/Application Support/PortZero/
//!   - Linux:   ~/.local/share/PortZero/
//!   - Windows: %APPDATA%\PortZero\
//!
//! The CA cert path is exposed via [`LocalCa::ca_cert_path`] for the trust
//! store installer (see `tls::trust`).  The wildcard cert + key are consumed
//! by the rustls ServerConfig in the overlay stack (see `tls::server_config`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

const CA_CERT: &str = "ca.crt";
const WILDCARD_CERT: &str = "wildcard.crt";
const WILDCARD_KEY: &str = "wildcard.key";
const WILDCARD_EXPIRY: &str = "wildcard.expiry";

const CA_VALIDITY_DAYS: i64 = 365 * 10;
const WILDCARD_VALIDITY_DAYS: i64 = 365;
// Regenerate when fewer than this many days remain on the wildcard cert.
const RENEW_WITHIN_DAYS: i64 = 30;

/// PEM-encoded local CA and wildcard cert material for `*.portzero.local`.
#[derive(Clone)]
pub struct LocalCa {
    /// CA certificate PEM — install into OS trust stores.
    pub ca_cert_pem: String,
    /// Wildcard cert PEM for `*.portzero.local`.
    pub wildcard_cert_pem: String,
    /// Private key PEM for the wildcard cert.
    pub wildcard_key_pem: String,
}

impl LocalCa {
    /// Generate a fresh `LocalCa` in memory without writing anything to disk.
    ///
    /// Useful for tests and one-shot tooling where persistence is not wanted.
    /// For the daemon's normal lifecycle use [`load_or_create`].
    pub fn generate_ephemeral() -> Result<Self> {
        let (ca, _expiry) = generate()?;
        Ok(ca)
    }

    /// Load cert material from disk, or generate and persist fresh material if
    /// the files are absent or within [`RENEW_WITHIN_DAYS`] of expiry.
    pub fn load_or_create() -> Result<Self> {
        let dir = data_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create PortZero data dir: {}", dir.display()))?;

        if is_fresh(&dir) && has_browser_tls_usages(&dir) {
            match load(&dir) {
                Ok(ca) => return Ok(ca),
                Err(e) => {
                    tracing::warn!("failed to load existing TLS certs, regenerating: {e:#}");
                }
            }
        }

        tracing::info!("generating PortZero local CA and wildcard cert");
        let (ca, expiry) = generate()?;
        save(&ca, expiry, &dir)?;
        Ok(ca)
    }

    /// Absolute path to the CA certificate PEM file.
    ///
    /// Pass this to the OS trust store installer so it can reference the file
    /// on disk rather than the in-memory PEM bytes.
    pub fn ca_cert_path() -> Result<PathBuf> {
        Ok(data_dir()?.join(CA_CERT))
    }
}

fn data_dir() -> Result<PathBuf> {
    Ok(base_data_dir()?.join("PortZero"))
}

fn base_data_dir() -> Result<PathBuf> {
    #[cfg(unix)]
    if let Some(home) = sudo_user_home() {
        return Ok(home.join(".local/share"));
    }

    dirs::data_dir().context("cannot resolve platform data directory")
}

#[cfg(unix)]
fn sudo_user_home() -> Option<PathBuf> {
    let sudo_user = std::env::var("SUDO_USER").ok()?;
    sudo_user_home_for(&sudo_user)
}

#[cfg(unix)]
fn sudo_user_home_for(sudo_user: &str) -> Option<PathBuf> {
    if sudo_user.is_empty() || sudo_user == "root" {
        return None;
    }
    passwd_home(&sudo_user)
}

#[cfg(unix)]
fn passwd_home(username: &str) -> Option<PathBuf> {
    let content = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in content.lines() {
        let mut fields = line.splitn(7, ':');
        let name = fields.next()?;
        if name != username {
            continue;
        }
        let home = fields.nth(4)?;
        return Some(PathBuf::from(home));
    }
    None
}

/// Returns true when the persisted wildcard cert has more than
/// [`RENEW_WITHIN_DAYS`] of validity remaining.
fn is_fresh(dir: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(dir.join(WILDCARD_EXPIRY)) else {
        return false;
    };
    let Ok(expiry) = OffsetDateTime::parse(raw.trim(), &Rfc3339) else {
        return false;
    };
    expiry - OffsetDateTime::now_utc() > Duration::days(RENEW_WITHIN_DAYS)
}

fn has_browser_tls_usages(dir: &Path) -> bool {
    let Ok(ca_pem) = std::fs::read_to_string(dir.join(CA_CERT)) else {
        return false;
    };
    let Ok(leaf_pem) = std::fs::read_to_string(dir.join(WILDCARD_CERT)) else {
        return false;
    };
    certs_have_browser_tls_usages(&ca_pem, &leaf_pem)
}

fn certs_have_browser_tls_usages(ca_pem: &str, leaf_pem: &str) -> bool {
    let Ok(ca_params) = CertificateParams::from_ca_cert_pem(ca_pem) else {
        return false;
    };
    let Ok(leaf_params) = CertificateParams::from_ca_cert_pem(leaf_pem) else {
        return false;
    };

    ca_params.key_usages.contains(&KeyUsagePurpose::KeyCertSign)
        && ca_params.key_usages.contains(&KeyUsagePurpose::CrlSign)
        && ca_params
            .key_usages
            .contains(&KeyUsagePurpose::DigitalSignature)
        && leaf_params
            .key_usages
            .contains(&KeyUsagePurpose::DigitalSignature)
        && leaf_params
            .extended_key_usages
            .contains(&ExtendedKeyUsagePurpose::ServerAuth)
}

fn load(dir: &Path) -> Result<LocalCa> {
    Ok(LocalCa {
        ca_cert_pem: std::fs::read_to_string(dir.join(CA_CERT)).context("read ca.crt")?,
        wildcard_cert_pem: std::fs::read_to_string(dir.join(WILDCARD_CERT))
            .context("read wildcard.crt")?,
        wildcard_key_pem: std::fs::read_to_string(dir.join(WILDCARD_KEY))
            .context("read wildcard.key")?,
    })
}

fn save(ca: &LocalCa, expiry: OffsetDateTime, dir: &Path) -> Result<()> {
    // Key written first with restricted permissions; if this fails the cert
    // files are never written so the bundle stays consistent on retry.
    write_private(&dir.join(WILDCARD_KEY), ca.wildcard_key_pem.as_bytes())?;
    std::fs::write(dir.join(CA_CERT), &ca.ca_cert_pem).context("write ca.crt")?;
    std::fs::write(dir.join(WILDCARD_CERT), &ca.wildcard_cert_pem).context("write wildcard.crt")?;
    std::fs::write(
        dir.join(WILDCARD_EXPIRY),
        expiry.format(&Rfc3339).context("format expiry timestamp")?,
    )
    .context("write wildcard.expiry")?;
    Ok(())
}

fn generate() -> Result<(LocalCa, OffsetDateTime)> {
    let now = OffsetDateTime::now_utc();

    // --- CA (10-year self-signed) ---
    let mut ca_params = CertificateParams::new(vec![]).context("build CA cert params")?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    ca_params.distinguished_name = make_dn("PortZero Local CA", Some("PortZero"));
    ca_params.not_before = now;
    ca_params.not_after = now + Duration::days(CA_VALIDITY_DAYS);
    let ca_key = KeyPair::generate().context("generate CA keypair")?;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .context("self-sign CA certificate")?;

    // --- Wildcard cert (1-year, CA-signed) ---
    let wildcard_expiry = now + Duration::days(WILDCARD_VALIDITY_DAYS);
    let mut leaf_params = CertificateParams::new(vec![
        "*.portzero.local".to_string(),
        "portzero.local".to_string(),
    ])
    .context("build wildcard cert params")?;
    leaf_params.distinguished_name = make_dn("*.portzero.local", None);
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.not_before = now;
    leaf_params.not_after = wildcard_expiry;
    let leaf_key = KeyPair::generate().context("generate wildcard keypair")?;
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .context("sign wildcard certificate")?;

    Ok((
        LocalCa {
            ca_cert_pem: ca_cert.pem(),
            wildcard_cert_pem: leaf_cert.pem(),
            wildcard_key_pem: leaf_key.serialize_pem(),
        },
        wildcard_expiry,
    ))
}

fn make_dn(common_name: &str, org: Option<&str>) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    if let Some(o) = org {
        dn.push(DnType::OrganizationName, o);
    }
    dn
}

/// Write `content` to `path` with owner-only read/write permissions.
///
/// On Unix this is mode 0o600.  On other platforms falls back to a plain
/// write (Windows ACLs are handled separately by the trust store installer).
fn write_private(path: &Path, content: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("open {} for writing", path.display()))?
            .write_all(content)
            .with_context(|| format!("write {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_valid_pem() {
        let (ca, expiry) = generate().unwrap();
        assert!(ca.ca_cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.wildcard_cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.wildcard_key_pem.contains("BEGIN PRIVATE KEY"));
        assert!(expiry > OffsetDateTime::now_utc());
    }

    #[test]
    fn generate_adds_browser_tls_usages() {
        let Some(openssl) = openssl_available() else {
            eprintln!("skipping OpenSSL extension check: openssl not found");
            return;
        };

        let dir = tempfile::tempdir().unwrap();
        let (ca, _) = generate().unwrap();
        let ca_path = dir.path().join("ca.crt");
        let leaf_path = dir.path().join("wildcard.crt");
        std::fs::write(&ca_path, ca.ca_cert_pem).unwrap();
        std::fs::write(&leaf_path, ca.wildcard_cert_pem).unwrap();

        let ca_text = openssl_x509_text(&openssl, &ca_path);
        assert!(ca_text.contains("CA:TRUE"));
        assert!(ca_text.contains("Key Usage"));
        assert!(ca_text.contains("Certificate Sign"));
        assert!(ca_text.contains("CRL Sign"));
        assert!(ca_text.contains("Digital Signature"));

        let leaf_text = openssl_x509_text(&openssl, &leaf_path);
        assert!(leaf_text.contains("DNS:*.portzero.local"));
        assert!(leaf_text.contains("DNS:portzero.local"));
        assert!(leaf_text.contains("Key Usage"));
        assert!(leaf_text.contains("Digital Signature"));
        assert!(leaf_text.contains("Extended Key Usage"));
        assert!(leaf_text.contains("TLS Web Server Authentication"));
    }

    #[test]
    fn generated_certs_satisfy_browser_tls_usage_check() {
        let (ca, _) = generate().unwrap();
        assert!(certs_have_browser_tls_usages(
            &ca.ca_cert_pem,
            &ca.wildcard_cert_pem
        ));
    }

    #[test]
    fn legacy_certs_without_usages_are_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let (ca, expiry) = generate_legacy_without_usages().unwrap();
        save(&ca, expiry, dir.path()).unwrap();

        assert!(is_fresh(dir.path()), "legacy cert is fresh by date");
        assert!(
            !has_browser_tls_usages(dir.path()),
            "legacy cert must be regenerated despite a fresh expiry file"
        );
    }

    #[test]
    fn round_trip_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let (ca, expiry) = generate().unwrap();
        save(&ca, expiry, dir.path()).unwrap();
        assert!(is_fresh(dir.path()), "freshly saved cert should be fresh");
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.ca_cert_pem, ca.ca_cert_pem);
        assert_eq!(loaded.wildcard_cert_pem, ca.wildcard_cert_pem);
        assert_eq!(loaded.wildcard_key_pem, ca.wildcard_key_pem);
    }

    #[test]
    fn stale_when_expiry_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_fresh(dir.path()));
    }

    #[test]
    fn stale_when_expiry_in_past() {
        let dir = tempfile::tempdir().unwrap();
        let past = OffsetDateTime::now_utc() - Duration::days(1);
        std::fs::write(
            dir.path().join(WILDCARD_EXPIRY),
            past.format(&Rfc3339).unwrap(),
        )
        .unwrap();
        assert!(!is_fresh(dir.path()));
    }

    #[test]
    fn stale_when_expiry_within_renew_window() {
        let dir = tempfile::tempdir().unwrap();
        let soon = OffsetDateTime::now_utc() + Duration::days(RENEW_WITHIN_DAYS - 1);
        std::fs::write(
            dir.path().join(WILDCARD_EXPIRY),
            soon.format(&Rfc3339).unwrap(),
        )
        .unwrap();
        assert!(!is_fresh(dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn sudo_user_home_ignores_root() {
        assert_eq!(sudo_user_home_for(""), None);
        assert_eq!(sudo_user_home_for("root"), None);
    }

    #[cfg(unix)]
    #[test]
    fn sudo_user_home_uses_passwd_home() {
        let current_user = std::env::var("USER").unwrap();
        let home = sudo_user_home_for(&current_user).unwrap();
        assert_eq!(home, PathBuf::from(std::env::var("HOME").unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn private_key_file_has_restricted_permissions() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.pem");
        write_private(&path, b"test").unwrap();
        let mode = std::fs::metadata(&path).unwrap().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "key file must be owner-read/write only"
        );
    }

    fn openssl_available() -> Option<String> {
        std::process::Command::new("openssl")
            .arg("version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| "openssl".to_string())
    }

    fn openssl_x509_text(openssl: &str, path: &Path) -> String {
        let output = std::process::Command::new(openssl)
            .args(["x509", "-in"])
            .arg(path)
            .args(["-noout", "-text"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "openssl x509 failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn generate_legacy_without_usages() -> Result<(LocalCa, OffsetDateTime)> {
        let now = OffsetDateTime::now_utc();

        let mut ca_params = CertificateParams::new(vec![]).context("build CA cert params")?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.distinguished_name = make_dn("PortZero Local CA", Some("PortZero"));
        ca_params.not_before = now;
        ca_params.not_after = now + Duration::days(CA_VALIDITY_DAYS);
        let ca_key = KeyPair::generate().context("generate CA keypair")?;
        let ca_cert = ca_params
            .self_signed(&ca_key)
            .context("self-sign CA certificate")?;

        let wildcard_expiry = now + Duration::days(WILDCARD_VALIDITY_DAYS);
        let mut leaf_params = CertificateParams::new(vec![
            "*.portzero.local".to_string(),
            "portzero.local".to_string(),
        ])
        .context("build wildcard cert params")?;
        leaf_params.distinguished_name = make_dn("*.portzero.local", None);
        leaf_params.not_before = now;
        leaf_params.not_after = wildcard_expiry;
        let leaf_key = KeyPair::generate().context("generate wildcard keypair")?;
        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &ca_cert, &ca_key)
            .context("sign wildcard certificate")?;

        Ok((
            LocalCa {
                ca_cert_pem: ca_cert.pem(),
                wildcard_cert_pem: leaf_cert.pem(),
                wildcard_key_pem: leaf_key.serialize_pem(),
            },
            wildcard_expiry,
        ))
    }
}
