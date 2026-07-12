//! The platform-agnostic tray controller: owns the tray icon and translates
//! menu clicks and periodic refreshes into state reads and actions. The platform
//! layers construct one of these on their event-loop thread and drive it; they
//! contain no menu or state logic themselves.

use std::time::Duration;

use anyhow::{Context, Result};
use portzero_daemon::discovery_loop::DaemonConfig;
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::actions;
use crate::icon;
use crate::menu::{self, Action, MenuModel};
use crate::state::Snapshot;

/// How often the tray re-reads daemon state and rebuilds its menu. State reads
/// are just a handful of small file reads, so this is cheap to run often.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// What the platform loop should do after handling an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// Keep running.
    Continue,
    /// The user asked to quit the tray.
    Quit,
}

pub struct Controller {
    config: DaemonConfig,
    tray: TrayIcon,
    model: MenuModel,
    /// Whether we've already tried to auto-launch a stopped daemon once this
    /// session, so we don't fight a user who deliberately stopped it.
    auto_started: bool,
}

impl Controller {
    /// Build the tray icon from the current daemon state. Must be called on the
    /// event-loop thread (a `tray-icon` requirement on every platform).
    pub fn new() -> Result<Self> {
        let config = DaemonConfig::load();
        let snapshot = Snapshot::read(&config);
        let (menu, model) = menu::build(&snapshot);

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(&snapshot.summary)
            .with_icon(icon::for_health(snapshot.health))
            .build()
            .context("failed to create the system tray icon")?;

        Ok(Self {
            config,
            tray,
            model,
            auto_started: false,
        })
    }

    /// If the daemon is not running and we haven't already tried once, launch it.
    /// This delivers the "install it and it just works" behaviour: the tray comes
    /// up, notices the daemon is down, and starts it.
    pub fn maybe_autostart_daemon(&mut self) {
        if self.auto_started {
            return;
        }
        let snapshot = Snapshot::read(&self.config);
        if !snapshot.running {
            self.auto_started = true;
            tracing::info!("daemon is not running; launching `portzero start`");
            if let Err(e) = actions::start_daemon() {
                tracing::warn!("failed to auto-start daemon: {e:#}");
            }
        }
    }

    /// Re-read daemon state and rebuild the icon, tooltip, and menu.
    pub fn refresh(&mut self) {
        // Reload the config each tick so an HTTPS toggle we (or the CLI) wrote to
        // config.toml is reflected, and the state dir stays authoritative.
        self.config = DaemonConfig::load();
        let snapshot = Snapshot::read(&self.config);
        let (new_menu, new_model) = menu::build(&snapshot);

        if let Err(e) = self.tray.set_icon(Some(icon::for_health(snapshot.health))) {
            tracing::debug!("failed to update tray icon: {e:#}");
        }
        let _ = self.tray.set_tooltip(Some(&snapshot.summary));
        self.tray.set_menu(Some(Box::new(new_menu)));
        self.model = new_model;
    }

    /// Handle a menu click by its item id. Returns whether the loop should quit.
    pub fn handle_menu(&mut self, id: &str) -> Dispatch {
        let Some(action) = self.model.get(id).cloned() else {
            return Dispatch::Continue;
        };

        match action {
            Action::Quit => return Dispatch::Quit,
            Action::Refresh => {}
            Action::Start => log_err("start daemon", actions::start_daemon()),
            Action::Stop => log_err("stop daemon", actions::stop_daemon()),
            Action::Restart => log_err("restart daemon", actions::restart_daemon()),
            Action::OpenUrl(url) => log_err("open url", actions::open_url(&url)),
            Action::ToggleHttps(enabled) => log_err(
                "set https policy",
                actions::set_https_enabled(&self.config, enabled),
            ),
        }

        // Reflect the new state immediately. Daemon start/stop take a moment to
        // settle in the pid file; the periodic refresh catches the final state.
        self.refresh();
        Dispatch::Continue
    }
}

fn log_err(what: &str, result: Result<()>) {
    if let Err(e) = result {
        tracing::warn!("{what} failed: {e:#}");
    }
}
