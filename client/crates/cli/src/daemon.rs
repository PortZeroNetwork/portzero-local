//! Daemon control commands: start, stop, status, restart.

use std::time::Duration;

use anyhow::{Context, Result};

use devenv_tunnel_daemon::discovery_loop::{
    read_cloud_connected, read_cloud_error, read_daemon_pid, DaemonConfig,
};
use devenv_tunnel_daemon::notify::read_issues;
use devenv_tunnel_daemon::route_table::RouteTable;

use crate::auth::AuthConfig;

/// Start the discovery daemon in the background.
///
/// If the daemon is already running, prints its PID and exits.
/// Otherwise spawns a new background process.
pub fn start() -> Result<()> {
    let config = DaemonConfig::default();

    // Check if already running.
    if let Some(pid) = read_daemon_pid(&config) {
        println!("Daemon is already running (PID {pid}).");
        return Ok(());
    }

    // Ensure state directory exists.
    std::fs::create_dir_all(&config.state_dir).with_context(|| {
        format!(
            "Failed to create daemon state directory: {}\n\n\
             Check that you have write permissions to ~/.devenv/",
            config.state_dir.display()
        )
    })?;

    // Find our own executable so we can re-invoke with `start --foreground` style,
    // but since the skeleton uses `run_discovery_loop` directly, we spawn the
    // current binary with an internal flag.
    let exe = std::env::current_exe().with_context(|| {
        "Could not determine the path to the devenv-tunnel binary.\n\n\
         Try running with an absolute path, e.g. /usr/local/bin/devenv-tunnel start"
    })?;

    let log_path = config.log_path();

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| {
            format!(
                "Failed to open daemon log file: {}\n\n\
                 Check permissions on ~/.devenv/daemon/",
                log_path.display()
            )
        })?;

    let stderr_file = log_file
        .try_clone()
        .context("Failed to clone log file handle")?;

    let child = std::process::Command::new(&exe)
        .arg("start")
        .arg("--foreground")
        .stdout(log_file)
        .stderr(stderr_file)
        .stdin(std::process::Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "Failed to spawn daemon process from: {}\n\n\
                 Is the devenv-tunnel binary executable?",
                exe.display()
            )
        })?;

    let child_pid = child.id();

    // Wait briefly for the daemon to write its PID file, confirming startup.
    let mut started = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        if read_daemon_pid(&config).is_some() {
            started = true;
            break;
        }
    }

    if started {
        let auth_status = if AuthConfig::load().is_ok() {
            "authenticated (tunnel will connect to cloud)"
        } else {
            "not authenticated (local-only mode, run `devenv tunnel login` for cloud tunnels)"
        };

        println!("Daemon started (PID {child_pid}).");
        println!("Auth: {auth_status}");
        println!("Log:  {}", log_path.display());
    } else {
        println!(
            "Daemon process spawned (PID {child_pid}) but did not confirm startup.\n\
             Check the log for errors: {}",
            log_path.display()
        );
    }

    Ok(())
}

/// Run the daemon in the foreground (called internally by `start --foreground`).
pub async fn start_foreground() -> Result<()> {
    let config = DaemonConfig::default();
    devenv_tunnel_daemon::discovery_loop::run_discovery_loop(&config).await
}

/// Stop the daemon.
pub fn stop() -> Result<()> {
    let config = DaemonConfig::default();
    devenv_tunnel_daemon::discovery_loop::stop_daemon(&config)?;
    println!("Daemon stopped.");
    Ok(())
}

/// Show daemon and route status.
pub async fn status() -> Result<()> {
    let config = DaemonConfig::default();

    match read_daemon_pid(&config) {
        Some(pid) => {
            println!("Daemon: running (PID {pid})");
        }
        None => {
            println!("Daemon: stopped");
            println!("\nRun `devenv tunnel start` to begin discovering services.");
            return Ok(());
        }
    }

    print_issues(&config);

    // Auth / tunnel status — verify the token against the API so we catch
    // expired sessions instead of showing a stale "logged in" from the local file.
    match AuthConfig::load() {
        Ok(auth) => {
            let token_ok = verify_token().await;
            match token_ok {
                TokenStatus::Valid => {
                    println!("Auth:   logged in as {}", auth.email);
                }
                TokenStatus::Expired => {
                    println!("Auth:   session expired — run `devenv tunnel login`");
                    println!("Tunnel: routes will return 502 until you re-authenticate");
                    print_routes(&config);
                    return Ok(());
                }
                TokenStatus::Unreachable => {
                    println!(
                        "Auth:   logged in as {} (could not verify — offline?)",
                        auth.email
                    );
                }
            }
            let edge_display = std::env::var("DEVENV_TOOLS_EDGE_URL")
                .unwrap_or_else(|_| "wss://edge.devenv.tools/tunnel".to_string());
            let cloud_err = read_cloud_error(&config);
            let is_auth_err = cloud_err
                .as_deref()
                .map(|e| e.contains("Authentication failed"))
                .unwrap_or(false);
            match read_cloud_connected(&config) {
                Some(true) => println!("Tunnel: connected to {edge_display}"),
                Some(false) if is_auth_err => {
                    println!(
                        "Tunnel: disconnected (auth token rejected — run `devenv tunnel login`)"
                    );
                }
                Some(false) => {
                    if let Some(err) = cloud_err {
                        println!("Tunnel: disconnected — {}", err);
                    } else {
                        println!("Tunnel: disconnected — cannot reach {edge_display}");
                    }
                }
                None => println!("Tunnel: connecting to {edge_display}"),
            }
        }
        Err(_) => {
            println!("Auth:   not logged in");
            println!("Tunnel: disabled (run `devenv tunnel login` for cloud tunnels)");
        }
    }

    print_routes(&config);

    Ok(())
}

/// Surface any current visibility issues (e.g. duplicate `.devenv.local` names
/// claimed by multiple worktrees) recorded by the running daemon.
fn print_issues(config: &DaemonConfig) {
    let state = read_issues(&config.issues_path());
    if state.is_empty() {
        return;
    }

    println!();
    println!(
        "Issues: {} problem(s) detected — see fixes below:",
        state.issues.len()
    );
    for issue in &state.issues {
        println!("  ! {}", issue.summary());
        println!("    fix: {}", issue.fix_hint());
    }
}

fn print_routes(config: &DaemonConfig) {
    let routes_path = config.routes_path();
    let table = RouteTable::load(&routes_path).unwrap_or_default();

    if table.is_empty() {
        println!("\nNo routes discovered yet.");
        println!("Set DEVENV_TUNNEL on a process or Docker container to expose it.");
        return;
    }

    println!();

    let mut max_domain = "DOMAIN".len();
    let mut max_port = "PORT".len();

    for route in table.routes.values() {
        max_domain = max_domain.max(route.domain.len());
        max_port = max_port.max(route.port.to_string().len());
    }

    println!(
        "{:<domain_w$}  {:<port_w$}  SOURCE",
        "DOMAIN",
        "PORT",
        domain_w = max_domain,
        port_w = max_port,
    );

    let mut routes: Vec<_> = table.routes.values().collect();
    routes.sort_by_key(|r| &r.domain);

    for route in routes {
        let source_str = match &route.source {
            devenv_tunnel_daemon::discovery::ServiceSource::Process { .. } => {
                format!("PID {}", route.pid)
            }
            devenv_tunnel_daemon::discovery::ServiceSource::Container { id, .. } => {
                format!("container {}", &id[..id.len().min(12)])
            }
        };

        println!(
            "{:<domain_w$}  {:<port_w$}  {}",
            route.domain,
            route.port,
            source_str,
            domain_w = max_domain,
            port_w = max_port,
        );
    }
}

enum TokenStatus {
    Valid,
    Expired,
    Unreachable,
}

async fn verify_token() -> TokenStatus {
    use crate::api_client::ApiClient;
    use std::time::Duration;

    let client = ApiClient::new();
    let result = tokio::time::timeout(Duration::from_secs(5), client.get("/auth/me")).await;

    match result {
        Ok(Ok(resp)) => {
            if resp.status().as_u16() == 401 {
                TokenStatus::Expired
            } else if resp.status().is_success() {
                TokenStatus::Valid
            } else {
                TokenStatus::Unreachable
            }
        }
        Ok(Err(_)) | Err(_) => TokenStatus::Unreachable,
    }
}

/// Restart the daemon (stop + start).
pub fn restart() -> Result<()> {
    let config = DaemonConfig::default();

    // Stop if running, ignore error if not.
    if read_daemon_pid(&config).is_some() {
        devenv_tunnel_daemon::discovery_loop::stop_daemon(&config).ok();
        // Brief pause to let the process exit.
        std::thread::sleep(Duration::from_millis(500));
    }

    start()
}
