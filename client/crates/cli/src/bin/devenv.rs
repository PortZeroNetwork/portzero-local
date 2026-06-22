//! `devenv` — plugin dispatcher, à la git.
//!
//! `devenv <subcommand> [args...]` looks for a `devenv-<subcommand>` binary in
//! PATH and exec-replaces the current process with it (Unix) or spawns and
//! waits (Windows).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::{env, process};

const RELEASES_BASE_URL: &str = "https://github.com/LoumTechnologies/devenv-tunnel/releases";
const INSTALL_SCRIPT: &str = "curl -fsSL https://devenv.tools/install.sh | sh";

#[tokio::main]
async fn main() {
    let mut argv = env::args();
    argv.next(); // skip argv[0]
    let args: Vec<String> = argv.collect();

    let subcommand = match args.first() {
        Some(s) => s.as_str(),
        None => {
            print_help();
            process::exit(0);
        }
    };

    match subcommand {
        "--help" | "-h" | "help" => {
            print_help();
            process::exit(0);
        }
        "--version" | "-V" => {
            println!("devenv {}", env!("CARGO_PKG_VERSION"));
            process::exit(0);
        }
        "update" => {
            do_update().await;
            process::exit(0);
        }
        _ => {}
    }

    let plugin_name = format!("devenv-{subcommand}");

    match find_in_path(&plugin_name) {
        Some(path) => exec_plugin(&path, &args[1..]),
        None => {
            eprintln!("error: '{subcommand}' is not a devenv command (plugin '{plugin_name}' not found in PATH)");
            eprintln!();
            write_plugins(&mut std::io::stderr());
            process::exit(1);
        }
    }
}

async fn do_update() {
    let current = env!("CARGO_PKG_VERSION");
    let current_version = parse_semver(current);

    match fetch_latest_version().await {
        Ok(latest) => match (current_version, parse_semver(&latest)) {
            (Some(cur), Some(lat)) if lat <= cur => {
                println!("Already up to date ({current}).");
                return;
            }
            (Some(_), Some(_)) => {
                println!("Updating devenv {current} -> {latest}...");
            }
            _ => {
                println!("Updating devenv...");
            }
        },
        Err(_) => {
            println!("Could not check latest version. Updating anyway...");
        }
    }

    let status = Command::new("sh")
        .arg("-c")
        .arg(INSTALL_SCRIPT)
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("error: failed to run install script: {e}");
            process::exit(1);
        }
    }
}

async fn fetch_latest_version() -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let resp = client.get(version_url()).send().await?;
    anyhow::ensure!(resp.status().is_success(), "version check failed");
    let manifest: serde_json::Value = resp.json().await?;
    manifest["version"]
        .as_str()
        .map(|s| s.to_owned())
        .ok_or_else(|| anyhow::anyhow!("missing version field"))
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
    // Served from this repo's GitHub Releases (see note in src/update.rs):
    // `version.json` is a release asset and /releases/latest/download/ resolves
    // to the newest stable release.
    format!("{RELEASES_BASE_URL}/latest/download/version.json")
}

#[cfg(unix)]
fn exec_plugin(path: &PathBuf, args: &[String]) -> ! {
    use std::os::unix::process::CommandExt;
    let err = Command::new(path).args(args).exec();
    eprintln!("error: failed to execute {}: {err}", path.display());
    process::exit(1);
}

#[cfg(not(unix))]
fn exec_plugin(path: &PathBuf, args: &[String]) -> ! {
    match Command::new(path).args(args).status() {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("error: failed to execute {}: {err}", path.display());
            process::exit(1);
        }
    }
}

fn is_executable(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH").unwrap_or_default();
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(name);

        // On Windows, also try with .exe extension
        #[cfg(windows)]
        let candidate = if candidate.extension().is_none() {
            let with_exe = candidate.with_extension("exe");
            if with_exe.exists() {
                with_exe
            } else {
                candidate
            }
        } else {
            candidate
        };

        if let Ok(meta) = candidate.metadata() {
            if is_executable(&meta) {
                return Some(candidate);
            }
        }
    }
    None
}

fn available_plugins() -> Vec<String> {
    let path_var = env::var_os("PATH").unwrap_or_default();
    let mut seen = HashSet::new();
    let mut plugins = Vec::new();
    for dir in env::split_paths(&path_var) {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                // Strip .exe on Windows for display purposes
                let name_str = if cfg!(windows) {
                    name_str
                        .strip_suffix(".exe")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| name_str.into_owned())
                } else {
                    name_str.into_owned()
                };
                if let Some(sub) = name_str.strip_prefix("devenv-") {
                    if seen.insert(sub.to_string()) {
                        if let Ok(meta) = entry.metadata() {
                            if is_executable(&meta) {
                                plugins.push(sub.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    plugins.sort();
    plugins
}

fn write_plugins(out: &mut dyn std::io::Write) {
    let plugins = available_plugins();
    if plugins.is_empty() {
        writeln!(out, "No devenv plugins found in PATH.").ok();
    } else {
        writeln!(out, "Available subcommands:").ok();
        for p in &plugins {
            writeln!(out, "  {p}").ok();
        }
    }
}

fn print_help() {
    println!("devenv - developer environment tools");
    println!();
    println!("USAGE:");
    println!("  devenv <subcommand> [args...]");
    println!();
    println!("Built-in commands:");
    println!("  update    Update devenv to the latest version");
    println!("  help      Show this help message");
    println!("  --version Print version");
    println!();
    write_plugins(&mut std::io::stdout());
}
