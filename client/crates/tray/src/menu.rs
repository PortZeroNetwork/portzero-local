//! The tray menu — built once here and used unchanged on every platform, so the
//! menu is identical across macOS, Windows, and Linux by construction (the
//! platform layers only own the event loop, never the menu).
//!
//! Building the menu also produces a [`MenuModel`]: a map from each item's id to
//! the [`Action`] it triggers, so the controller can dispatch a click without
//! re-deriving what each item meant.

use std::collections::HashMap;

use muda::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

use crate::actions::DASHBOARD_URL;
use crate::state::{Health, Snapshot};

/// Something the user asked the tray to do by clicking a menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    Refresh,
    Quit,
    OpenUrl(String),
    /// Set the "enable HTTPS for HTTP tunnels" policy to this value.
    ToggleHttps(bool),
}

// Stable ids for the fixed items. Tunnel items get generated `tunnel:<n>` ids.
const ID_START: &str = "daemon:start";
const ID_STOP: &str = "daemon:stop";
const ID_RESTART: &str = "daemon:restart";
const ID_DASHBOARD: &str = "open:dashboard";
const ID_HTTPS: &str = "config:https";
const ID_REFRESH: &str = "app:refresh";
const ID_QUIT: &str = "app:quit";
const ID_DASHBOARD_DETAILS: &str = "open:dashboard-details";

/// Maps menu-item ids to the action they perform.
pub type MenuModel = HashMap<String, Action>;

/// Build the full tray menu from a snapshot, returning the menu plus the id →
/// action model the controller dispatches against.
pub fn build(snapshot: &Snapshot) -> (Menu, MenuModel) {
    let menu = Menu::new();
    let mut model: MenuModel = HashMap::new();

    // Header — a disabled one-line status summary with a health glyph.
    let glyph = match snapshot.health {
        Health::Ok => "●",
        Health::Degraded => "▲",
        Health::Down => "■",
    };
    append_disabled(&menu, &format!("{glyph}  {}", snapshot.summary));
    append_separator(&menu);

    // Dashboard.
    append_action(
        &menu,
        &mut model,
        ID_DASHBOARD,
        "Open Dashboard",
        true,
        Action::OpenUrl(DASHBOARD_URL.to_string()),
    );
    append_separator(&menu);

    // Daemon lifecycle — contextual to whether it is running.
    if snapshot.running {
        append_action(
            &menu,
            &mut model,
            ID_RESTART,
            "Restart Daemon",
            true,
            Action::Restart,
        );
        append_action(
            &menu,
            &mut model,
            ID_STOP,
            "Stop Daemon",
            true,
            Action::Stop,
        );
    } else {
        append_action(
            &menu,
            &mut model,
            ID_START,
            "Start Daemon",
            true,
            Action::Start,
        );
    }
    append_separator(&menu);

    // Tunnels submenu.
    let tunnels = Submenu::new(format!("Tunnels ({})", snapshot.tunnels.len()), true);
    let _ = menu.append(&tunnels);
    if snapshot.tunnels.is_empty() {
        append_disabled_sub(&tunnels, "No tunnels discovered yet");
    } else {
        for (i, t) in snapshot.tunnels.iter().enumerate() {
            let scheme = if t.https { "https" } else { "http" };
            let kind = if t.cloud { "cloud" } else { "local" };
            let id = format!("tunnel:{i}");
            let item = MenuItem::with_id(
                MenuId(id.clone()),
                format!("{}   ·   {scheme} · {kind}", t.domain),
                true,
                None,
            );
            let _ = tunnels.append(&item);
            model.insert(id, Action::OpenUrl(t.url.clone()));
        }
    }

    // Global HTTPS policy toggle (affects plain-HTTP `.portzero.local` tunnels).
    let https = CheckMenuItem::with_id(
        MenuId(ID_HTTPS.to_string()),
        "Enable HTTPS for HTTP tunnels",
        true,
        snapshot.https.enable_for_port_80,
        None,
    );
    let _ = menu.append(&https);
    // Clicking flips the current value.
    model.insert(
        ID_HTTPS.to_string(),
        Action::ToggleHttps(!snapshot.https.enable_for_port_80),
    );

    // Issues submenu — only when there is something to show.
    if !snapshot.problems.is_empty() {
        let issues = Submenu::new(format!("Issues ({})", snapshot.problems.len()), true);
        let _ = menu.append(&issues);
        for p in &snapshot.problems {
            append_disabled_sub(&issues, &format!("⚠  {}", truncate(&p.summary, 70)));
            if !p.fix.is_empty() {
                append_disabled_sub(&issues, &format!("      ↳ {}", truncate(&p.fix, 80)));
            }
        }
        append_separator_sub(&issues);
        append_action_sub(
            &issues,
            &mut model,
            ID_DASHBOARD_DETAILS,
            "Open dashboard for fixes →",
            Action::OpenUrl(DASHBOARD_URL.to_string()),
        );
    }
    append_separator(&menu);

    // App controls.
    append_action(
        &menu,
        &mut model,
        ID_REFRESH,
        "Refresh Now",
        true,
        Action::Refresh,
    );
    append_action(
        &menu,
        &mut model,
        ID_QUIT,
        "Quit PortZero Tray",
        true,
        Action::Quit,
    );

    (menu, model)
}

// ── small append helpers ────────────────────────────────────────────────────

fn append_disabled(menu: &Menu, text: &str) {
    let _ = menu.append(&MenuItem::new(text, false, None));
}

fn append_disabled_sub(sub: &Submenu, text: &str) {
    let _ = sub.append(&MenuItem::new(text, false, None));
}

fn append_separator(menu: &Menu) {
    let _ = menu.append(&PredefinedMenuItem::separator());
}

fn append_separator_sub(sub: &Submenu) {
    let _ = sub.append(&PredefinedMenuItem::separator());
}

fn append_action(
    menu: &Menu,
    model: &mut MenuModel,
    id: &str,
    text: &str,
    enabled: bool,
    action: Action,
) {
    let item = MenuItem::with_id(MenuId(id.to_string()), text, enabled, None);
    let _ = menu.append(&item);
    model.insert(id.to_string(), action);
}

fn append_action_sub(sub: &Submenu, model: &mut MenuModel, id: &str, text: &str, action: Action) {
    let item = MenuItem::with_id(MenuId(id.to_string()), text, true, None);
    let _ = sub.append(&item);
    model.insert(id.to_string(), action);
}

/// Truncate to `n` chars with an ellipsis, on a char boundary.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_strings_untouched() {
        assert_eq!(truncate("hello", 70), "hello");
    }

    #[test]
    fn truncate_shortens_long_strings_with_ellipsis() {
        let long = "x".repeat(100);
        let t = truncate(&long, 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('…'));
    }
}
