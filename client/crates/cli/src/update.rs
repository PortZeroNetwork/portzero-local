//! Background update checker.
//!
//! After each CLI invocation we spawn a non-blocking check against the
//! `version.json` published as a GitHub Release asset.  If a newer version exists we print a
//! one-line notice to stderr.  The check is skipped if the `DEVENV_NO_UPDATE_CHECK`
//! env var is set, or if the last check was less than 24 hours ago.

use std::cmp::Ordering;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde::Deserialize;
use tokio::time::timeout;

const RELEASES_BASE_URL: &str = "https://github.com/LoumTechnologies/devenv-tunnel/releases";

/// Minimum interval between remote checks.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Timeout for the HTTP request so it never blocks the CLI noticeably.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Shape of the version manifest uploaded alongside release artifacts.
#[derive(Deserialize)]
struct VersionManifest {
    version: String,
}

/// Path to the timestamp file that records when we last checked.
fn last_check_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    Ok(home.join(".devenv").join(".last_update_check"))
}

/// Return true if enough time has elapsed since the last check.
fn should_check() -> bool {
    if std::env::var("DEVENV_NO_UPDATE_CHECK").is_ok() {
        return false;
    }

    let path = match last_check_path() {
        Ok(p) => p,
        Err(_) => return true,
    };

    let metadata = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return true,
    };

    let modified = match metadata.modified() {
        Ok(t) => t,
        Err(_) => return true,
    };

    SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO)
        >= CHECK_INTERVAL
}

/// Touch the timestamp file so we don't check again for 24 h.
fn record_check() {
    if let Ok(path) = last_check_path() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, []).ok();
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Option<String>,
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)) {
            Ordering::Equal => compare_prerelease(&self.prerelease, &other.prerelease),
            ordering => ordering,
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_prerelease(left: &Option<String>, right: &Option<String>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => compare_prerelease_identifiers(left, right),
    }
}

fn compare_prerelease_identifiers(left: &str, right: &str) -> Ordering {
    let mut left_parts = left.split('.');
    let mut right_parts = right.split('.');

    loop {
        match (left_parts.next(), right_parts.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => {
                let left_num = left.parse::<u64>();
                let right_num = right.parse::<u64>();
                let ordering = match (left_num, right_num) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => left.cmp(right),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

/// Parse a SemVer string, ignoring any leading `v`.
fn parse_semver(s: &str) -> Option<Version> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let (core, prerelease) = match s.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease.to_string())),
        None => (s, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
        prerelease,
    })
}

fn version_url() -> String {
    // `version.json` is uploaded as a GitHub Release asset by
    // .github/workflows/release.yml; GitHub's /releases/latest/download/<asset>
    // always resolves to the newest stable release. GitHub exposes no static
    // "latest prerelease" URL and this repo publishes no separate staging
    // channel, so all builds track the latest stable release.
    format!("{RELEASES_BASE_URL}/latest/download/version.json")
}

/// Run the update check.  Intended to be called with `tokio::spawn` so it
/// never delays the main command.
pub async fn check_for_update() {
    if !should_check() {
        return;
    }

    let current = env!("CARGO_PKG_VERSION");
    let Some(current_version) = parse_semver(current) else {
        return;
    };

    if let Ok(Ok(Some(latest))) =
        timeout(REQUEST_TIMEOUT, fetch_latest_version()).await
    {
        record_check();

        if let Some(latest_version) = parse_semver(&latest) {
            if latest_version > current_version {
                eprintln!(
                    "\x1b[33mA new version of devenv is available: {current} -> {latest}\x1b[0m"
                );
                eprintln!("\x1b[33mUpdate with: devenv update\x1b[0m");
            }
        }
    }
}

async fn fetch_latest_version() -> Result<Option<String>> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    let resp = client.get(version_url()).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let manifest: VersionManifest = resp.json().await?;
    Ok(Some(manifest.version))
}
