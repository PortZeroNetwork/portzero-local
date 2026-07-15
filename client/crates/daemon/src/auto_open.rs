//! Auto-open newly-detected HTTP/HTTPS local tunnels in the browser.
//!
//! When the discovery loop finds a `.portzero.local` backend on port 80 or 443,
//! and the `auto_open_http_tunnels` setting is on, we pop the tunnel's URL in the
//! default browser once — the moment a freshly-started app (e.g. an example)
//! becomes reachable. This is what makes "run an example and watch a tab appear"
//! work without the user typing the URL.
//!
//! Tracking is per daemon session and keyed by domain: each tunnel opens at most
//! once while it is present. A tunnel that goes away (app stopped) and comes back
//! (app restarted) opens again, which matches the mental model of "a new tunnel
//! just came up".

use std::collections::HashSet;

use crate::discovery::DiscoveredNetworkService;

/// The scheme+URL a browser should open for a `.portzero.local` service, or
/// `None` when the service is not an HTTP/HTTPS web port.
fn web_url(name: &str, service_port: u16) -> Option<String> {
    match service_port {
        80 => Some(format!("http://{name}.portzero.local")),
        443 => Some(format!("https://{name}.portzero.local")),
        _ => None,
    }
}

/// Remembers which web-tunnel domains have already been opened this session so a
/// steady-state scan does not reopen them every cycle.
#[derive(Default)]
pub struct AutoOpenTracker {
    opened: HashSet<String>,
}

impl AutoOpenTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconcile the currently-detected services against what we have opened.
    ///
    /// For every web tunnel present now that we have not opened yet: if
    /// `enabled`, open it in the browser; either way mark it seen (so toggling
    /// the setting on later does not blast every pre-existing tunnel). Tunnels
    /// that have disappeared are forgotten so they reopen if they return.
    pub fn reconcile(&mut self, enabled: bool, services: &[DiscoveredNetworkService]) {
        let mut present: HashSet<String> = HashSet::new();
        for svc in services {
            let Some(url) = web_url(&svc.name, svc.service_port) else {
                continue;
            };
            present.insert(url.clone());
            if self.opened.contains(&url) {
                continue;
            }
            self.opened.insert(url.clone());
            if enabled {
                tracing::info!(%url, "auto-opening newly detected web tunnel");
                if !crate::browser::open_url(&url) {
                    tracing::debug!(%url, "no browser opener available for auto-open");
                }
            }
        }
        // Forget tunnels that are gone so a later reappearance reopens.
        self.opened.retain(|url| present.contains(url));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_url_only_matches_web_ports() {
        assert_eq!(
            web_url("api", 80).as_deref(),
            Some("http://api.portzero.local")
        );
        assert_eq!(
            web_url("api", 443).as_deref(),
            Some("https://api.portzero.local")
        );
        assert_eq!(web_url("db", 5432), None);
    }
}
