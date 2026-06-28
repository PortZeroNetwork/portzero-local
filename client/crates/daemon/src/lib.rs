pub mod auth;
pub mod autostart;
pub mod cloud;
pub mod diagnostics;
pub mod discovery;
pub mod discovery_loop;
pub mod docker_events;
pub mod forwarder;
pub mod legacy_monitor;
pub mod management;
pub mod net;
pub mod notify;
pub mod protocol_detect;
pub mod route_table;
pub mod tls;

/// Install rustls' process-wide crypto provider.
///
/// Some client-side TLS users, including websocket and HTTP clients, ask rustls
/// for the process default provider. The daemon's server TLS path builds with an
/// explicit provider, but cloud connections need this initialized before first
/// use.
pub fn install_default_crypto_provider() {
    static INIT: std::sync::Once = std::sync::Once::new();

    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
