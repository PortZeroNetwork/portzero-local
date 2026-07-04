//! Management API server — binds on localhost:0 and serves the registration API.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use axum::{routing, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use crate::management::handlers;

/// A single port registration entry.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PortRegistration {
    /// The local TCP port this process is listening on. The daemon verifies
    /// the calling PID is actually bound to this port before accepting the
    /// registration.
    #[schema(minimum = 1, example = 8080)]
    pub local_port: u16,
    /// Full domain name to map to this port. Accepted patterns:
    /// `*.portzero.local` for local overlay-only resolution, or
    /// `*.<username>.portzero.cloud` for a cloud-proxied tunnel domain.
    #[schema(examples("api.alice.portzero.cloud", "myservice.portzero.local"))]
    pub domain: String,
}

/// Shared state: PID -> list of registered ports.
pub type RegistrationStore = Arc<RwLock<HashMap<u32, Vec<PortRegistration>>>>;

/// Shared application state passed to all handlers.
#[derive(Clone)]
pub struct AppState {
    /// In-memory registration store (PID → port list).
    pub store: RegistrationStore,
    /// Daemon state directory; handlers read `overlay.json`, `routes.json`,
    /// `cloud_state.json`, and `daemon.pid` from here.
    pub state_dir: std::path::PathBuf,
}

/// The management API server.
pub struct ManagementServer {
    /// The shared registration store.
    pub store: RegistrationStore,
    /// The port the listener was bound to.
    pub bound_port: u16,
    /// Daemon state directory (used by the status UI handler to read daemon state files).
    pub state_dir: std::path::PathBuf,
}

impl ManagementServer {
    /// Bind the listener to a random localhost port and return the server + listener pair.
    ///
    /// The caller is responsible for passing the listener to [`ManagementServer::serve`]
    /// so the bound port is known before the server starts accepting connections.
    pub async fn bind(state_dir: std::path::PathBuf) -> Result<(Self, TcpListener)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let bound_port = listener.local_addr()?.port();
        let store: RegistrationStore = Arc::new(RwLock::new(HashMap::new()));
        let server = ManagementServer {
            store,
            bound_port,
            state_dir,
        };
        Ok((server, listener))
    }

    /// Run the axum server on the provided listener until it returns.
    pub async fn serve(self, listener: TcpListener) {
        let app_state = AppState {
            store: self.store,
            state_dir: self.state_dir,
        };
        let router = build_router(app_state);
        tracing::info!("management API listening on 127.0.0.1:{}", self.bound_port);
        if let Err(e) = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        {
            tracing::warn!("management API server exited: {e}");
        }
    }
}

fn build_router(state: AppState) -> Router {
    Router::new()
        // Management REST API (served at api.portzero.local)
        .route(
            "/v1/register",
            routing::post(handlers::register).delete(handlers::deregister),
        )
        .route("/v1/status", routing::get(handlers::status))
        .route("/v1/daemon/status", routing::get(handlers::daemon_status))
        .route(
            "/v1/config/https",
            routing::put(handlers::update_https_policy),
        )
        // Status UI (served at portzero.local)
        .route("/", routing::get(handlers::status_ui))
        .route("/login", routing::get(handlers::start_login))
        .route(
            "/assets/portzero-mark.jpg",
            routing::get(handlers::portzero_mark_asset),
        )
        .route(
            "/assets/portzero-wordmark.jpg",
            routing::get(handlers::portzero_wordmark_asset),
        )
        .route("/openapi.json", routing::get(handlers::openapi_json))
        .route("/status.json", routing::get(handlers::status_json))
        .with_state(state)
}
