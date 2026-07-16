//! Auto-open newly-detected HTTP/HTTPS local tunnels in the browser.
//!
//! When the discovery loop finds a `.portzero.local` backend on port 80 or 443,
//! and the `auto_open_http_tunnels` setting is on, we pop the tunnel's URL in the
//! default browser once — the moment a freshly-started app (e.g. an example)
//! becomes reachable. This is what makes "run an example and watch a tab appear"
//! work without the user typing the URL.
//!
//! Tracking is per daemon session and keyed by domain, but the *identity* of a
//! tunnel is the owning process: we remember the PID we opened each domain for.
//! A domain that is still served by the same PID opens at most once, no matter
//! how the scan flickers. A domain reclaimed by a *different* PID is a genuine
//! restart (the old app stopped, a new one took the name) and opens again. The
//! PID check is what stops a transient scan gap — a single empty or slow scan —
//! from masquerading as a teardown-and-restart and reopening the browser.

use std::collections::{HashMap, HashSet};

use crate::discovery::DiscoveredNetworkService;

/// Number of consecutive scans an absent domain is remembered before we drop it
/// to reclaim memory.
///
/// This is only a cleanup fallback: reopen decisions are driven by the PID (a
/// live process keeps its PID, a restarted one gets a new one), so dropping a
/// long-absent entry is safe — if that exact PID somehow reappears we would
/// merely open once more, and in practice a process gone this long is dead. The
/// window (roughly this many × `scan_interval_secs`, ~30s at the 2s default) is
/// deliberately generous so it never fires during an ordinary scan blip.
const MISSES_BEFORE_FORGET: u32 = 15;

/// What we remember about a web tunnel we have already handled this session.
struct OpenedTunnel {
    /// PID of the process we opened this domain for. A different PID on the same
    /// domain means a new process took it over — a genuine restart — so we open
    /// the browser again.
    pid: u32,
    /// Consecutive scans the domain has been absent (0 while present). Used only
    /// to age the entry out of memory once it has been gone a long time.
    misses: u32,
}

/// The scheme+URL a browser should open for a `.portzero.local` service, or
/// `None` when the service is not an HTTP/HTTPS web port.
fn web_url(name: &str, service_port: u16) -> Option<String> {
    match service_port {
        80 => Some(format!("http://{name}.portzero.local")),
        443 => Some(format!("https://{name}.portzero.local")),
        _ => None,
    }
}

/// Remembers which web tunnels (domain + owning PID) have already been opened
/// this session so a steady-state scan does not reopen them every cycle.
#[derive(Default)]
pub struct AutoOpenTracker {
    opened: HashMap<String, OpenedTunnel>,
}

impl AutoOpenTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconcile the currently-detected services against what we have opened.
    ///
    /// A web tunnel is opened (when `enabled`) the first time we see its domain,
    /// or when the domain reappears under a *different* PID (a real restart).
    /// Either way it is recorded as seen — so toggling the setting on later does
    /// not blast every pre-existing tunnel. A domain still served by the same PID
    /// only has its absence counter reset. Domains absent this scan are aged out
    /// and dropped after `MISSES_BEFORE_FORGET` consecutive misses.
    pub fn reconcile(&mut self, enabled: bool, services: &[DiscoveredNetworkService]) {
        let mut present: HashSet<String> = HashSet::new();
        for svc in services {
            let Some(url) = web_url(&svc.name, svc.service_port) else {
                continue;
            };
            present.insert(url.clone());

            // New domain, or the same domain now owned by a different PID (a
            // restart), is treated as freshly appeared. The same PID reappearing
            // — even after the domain flickered out of a scan — is the same
            // process and must not reopen.
            let is_new = self.opened.get(&url).map(|e| e.pid) != Some(svc.pid);
            // Overwriting resets the absence counter and records the current PID.
            self.opened.insert(
                url.clone(),
                OpenedTunnel {
                    pid: svc.pid,
                    misses: 0,
                },
            );
            if is_new && enabled {
                tracing::info!(%url, pid = svc.pid, "auto-opening newly detected web tunnel");
                if !crate::browser::open_url(&url) {
                    tracing::debug!(%url, "no browser opener available for auto-open");
                }
            }
        }
        // Age out domains absent this scan; drop one only after it has been gone
        // long enough to reclaim memory. Reopen correctness comes from the PID
        // check above, not from this window.
        self.opened.retain(|url, entry| {
            if present.contains(url) {
                return true;
            }
            entry.misses += 1;
            entry.misses < MISSES_BEFORE_FORGET
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::discovery::ServiceSource;

    fn svc(name: &str, service_port: u16, pid: u32) -> DiscoveredNetworkService {
        DiscoveredNetworkService {
            name: name.to_owned(),
            real_addr: "127.0.0.1:0".parse().unwrap(),
            service_port,
            backend_protocol: None,
            pid,
            source: ServiceSource::Process { cwd: None },
            domain_template: format!("{name}.portzero.local"),
            substitutions: BTreeMap::new(),
            health_path: None,
        }
    }

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

    #[test]
    fn a_present_tunnel_is_only_recorded_once() {
        let mut tracker = AutoOpenTracker::new();
        let services = [svc("api", 80, 100)];
        tracker.reconcile(false, &services);
        tracker.reconcile(false, &services);
        assert_eq!(
            tracker.opened.get("http://api.portzero.local").map(|e| e.pid),
            Some(100)
        );
    }

    #[test]
    fn a_transient_missing_scan_does_not_reopen_the_same_process() {
        let mut tracker = AutoOpenTracker::new();
        let services = [svc("api", 80, 100)];
        tracker.reconcile(false, &services);

        // Several empty scans — e.g. overlay-refresh timeouts — must not forget
        // the tunnel within the grace window...
        for _ in 0..(MISSES_BEFORE_FORGET - 1) {
            tracker.reconcile(false, &[]);
            assert!(tracker.opened.contains_key("http://api.portzero.local"));
        }
        // ...and when the *same* PID returns, the counter resets and it is still
        // considered already-opened (no reopen).
        tracker.reconcile(false, &services);
        assert_eq!(
            tracker.opened.get("http://api.portzero.local").map(|e| e.misses),
            Some(0)
        );
    }

    #[test]
    fn same_domain_new_pid_is_treated_as_a_restart() {
        let mut tracker = AutoOpenTracker::new();
        tracker.reconcile(false, &[svc("api", 80, 100)]);

        // A different PID on the same domain is a genuine restart: the entry now
        // tracks the new PID (and would have reopened the browser).
        tracker.reconcile(false, &[svc("api", 80, 200)]);
        assert_eq!(
            tracker.opened.get("http://api.portzero.local").map(|e| e.pid),
            Some(200)
        );
    }

    #[test]
    fn a_domain_gone_past_the_grace_window_is_forgotten() {
        let mut tracker = AutoOpenTracker::new();
        tracker.reconcile(false, &[svc("api", 80, 100)]);
        for _ in 0..MISSES_BEFORE_FORGET {
            tracker.reconcile(false, &[]);
        }
        assert!(!tracker.opened.contains_key("http://api.portzero.local"));
    }
}
