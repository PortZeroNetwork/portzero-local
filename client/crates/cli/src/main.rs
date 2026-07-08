//! port-zero CLI: manage the local tunnel daemon and cloud integration.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use clap::{Parser, Subcommand};

mod api_client;
mod auth;
mod autostart;
mod browser;
mod daemon;
mod export;
mod setup;
mod team;
mod trust;
mod update;
mod wait;

#[derive(Parser)]
#[command(
    name = "portzero",
    version,
    about = "Expose local services via portzero.cloud tunnels"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the discovery daemon and tunnel connection.
    Start {
        /// Run in the foreground instead of daemonizing (used internally).
        #[arg(long, hide = true)]
        foreground: bool,

        /// Do not automatically open the dashboard in a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Stop the discovery daemon.
    Stop,
    /// Restart the discovery daemon.
    Restart,
    /// Show daemon and tunnel status.
    Status,

    /// Print the resolved URL for a tunnel domain (script-safe: only the URL
    /// is written to stdout).
    Url {
        /// The tunnel domain, e.g. `web.myapp.portzero.local` or
        /// `api.alice.tunnel.portzero.cloud`.
        domain: String,
    },
    /// Print `export NAME="URL"` lines for every discovered tunnel.
    Env {
        /// Append `NAME=URL` lines to `$GITHUB_ENV` instead of printing export
        /// lines (for use inside a GitHub Actions job).
        #[arg(long)]
        github: bool,
    },
    /// Block until a tunnel is up (readiness gate for CI and test runs).
    Wait {
        /// The tunnel domain, e.g. `web.myapp.portzero.local`.
        domain: String,
        /// Also poll the tunnel's health path until it returns 2xx. Health is
        /// polled automatically when the endpoint declares PZ_HEALTH_PATH.
        #[arg(long)]
        healthy: bool,
        /// Maximum seconds to wait before failing (default: 60).
        #[arg(long)]
        timeout: Option<u64>,
    },

    /// Run privileged first-run setup after package installation.
    #[command(alias = "post-install")]
    Setup,

    /// Log in to portzero.cloud (opens browser by default).
    Login {
        /// Use interactive terminal prompts instead of browser login.
        /// Useful on headless servers without a browser.
        #[arg(long)]
        interactive: bool,

        /// Email address (only used with --interactive, skips prompt).
        #[arg(long)]
        email: Option<String>,

        /// Display name (only used with --interactive, skips prompt).
        #[arg(long)]
        name: Option<String>,
    },
    /// Log out and remove stored credentials.
    Logout,
    /// Show the currently authenticated user.
    Whoami,

    /// Manage teams.
    #[command(subcommand)]
    Team(TeamCommand),

    /// Manage starting the daemon automatically at boot.
    #[command(subcommand)]
    Autostart(AutostartCommand),

    /// Manage the local CA certificate and OS trust store.
    #[command(subcommand)]
    Trust(TrustCommand),
}

#[derive(Subcommand)]
enum TrustCommand {
    /// Generate the local CA certificate (no root required).
    /// Run this before `trust install` so the cert exists when root reads it.
    Generate,
    /// Install the local CA into the OS trust store (requires root on Linux/macOS).
    Install,
    /// Remove the local CA from the OS trust store (requires root on Linux/macOS).
    Uninstall,
}

#[derive(Subcommand)]
enum AutostartCommand {
    /// Install the daemon as a system service that starts at boot.
    Enable,
    /// Remove the autostart system service.
    Disable,
    /// Show whether autostart is installed.
    Status,
}

#[derive(Subcommand)]
enum TeamCommand {
    /// List teams you belong to.
    List,
    /// Invite a user to your team.
    Invite {
        /// Email address to invite.
        email: String,
    },
    /// List members of the current team and their environments.
    Members,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    portzero_daemon::install_default_crypto_provider();

    let cli = Cli::parse();

    // Spawn update check in the background — it never blocks the command.
    let update_handle = tokio::spawn(update::check_for_update());

    match cli.command {
        Command::Start {
            foreground,
            no_browser,
        } => {
            if foreground {
                daemon::start_foreground().await?;
            } else {
                daemon::start(!no_browser)?;
            }
        }
        Command::Stop => daemon::stop()?,
        Command::Restart => daemon::restart()?,
        Command::Status => daemon::status().await?,
        Command::Url { domain } => export::url(&domain)?,
        Command::Env { github } => export::env(github)?,
        Command::Wait {
            domain,
            healthy,
            timeout,
        } => wait::wait(&domain, healthy, timeout).await?,
        Command::Setup => setup::run().await?,

        Command::Login {
            interactive,
            email,
            name,
        } => {
            auth::login(interactive, email, name).await?;
            daemon::restart()?;
        }
        Command::Logout => auth::logout()?,
        Command::Whoami => auth::whoami().await?,

        Command::Team(cmd) => match cmd {
            TeamCommand::List => team::list().await?,
            TeamCommand::Invite { email } => team::invite(&email).await?,
            TeamCommand::Members => team::members().await?,
        },
        Command::Autostart(cmd) => match cmd {
            AutostartCommand::Enable => autostart::enable()?,
            AutostartCommand::Disable => autostart::disable()?,
            AutostartCommand::Status => autostart::status()?,
        },
        Command::Trust(cmd) => match cmd {
            TrustCommand::Generate => trust::generate()?,
            TrustCommand::Install => trust::install()?,
            TrustCommand::Uninstall => trust::uninstall()?,
        },
    }

    // Wait briefly for the update check to print its notice (if any).
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), update_handle).await;

    Ok(())
}
