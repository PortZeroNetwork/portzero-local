//! devenv-tunnel CLI: manage the local tunnel daemon and cloud integration.

use clap::{Parser, Subcommand};

mod api_client;
mod auth;
mod autostart;
mod daemon;
mod domains;
mod team;
mod update;

#[derive(Parser)]
#[command(
    name = "devenv-tunnel",
    version,
    about = "Expose local services via devenv.tools tunnels"
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
    },
    /// Stop the discovery daemon.
    Stop,
    /// Restart the discovery daemon.
    Restart,
    /// Show daemon and tunnel status.
    Status,

    /// Log in to devenv.tools (opens browser by default).
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

    /// Manage custom domains.
    #[command(subcommand)]
    Domains(DomainsCommand),

    /// Manage teams.
    #[command(subcommand)]
    Team(TeamCommand),

    /// Manage starting the daemon automatically at boot.
    #[command(subcommand)]
    Autostart(AutostartCommand),
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
enum DomainsCommand {
    /// List configured domains.
    List,
    /// Add a custom domain.
    Add {
        /// The domain pattern to add (e.g. "*.dev.example.com").
        domain: String,
    },
    /// Verify DNS for a domain.
    Verify {
        /// The domain to verify.
        domain: String,
    },
    /// Remove a custom domain.
    Remove {
        /// The domain to remove.
        domain: String,
    },
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

    let cli = Cli::parse();

    // Spawn update check in the background — it never blocks the command.
    let update_handle = tokio::spawn(update::check_for_update());

    match cli.command {
        Command::Start { foreground } => {
            if foreground {
                daemon::start_foreground().await?;
            } else {
                daemon::start()?;
            }
        }
        Command::Stop => daemon::stop()?,
        Command::Restart => daemon::restart()?,
        Command::Status => daemon::status().await?,

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

        Command::Domains(cmd) => match cmd {
            DomainsCommand::List => domains::list().await?,
            DomainsCommand::Add { domain } => domains::add(&domain).await?,
            DomainsCommand::Verify { domain } => domains::verify(&domain).await?,
            DomainsCommand::Remove { domain } => domains::remove(&domain).await?,
        },
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
    }

    // Wait briefly for the update check to print its notice (if any).
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), update_handle).await;

    Ok(())
}
