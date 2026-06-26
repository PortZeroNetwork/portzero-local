//! Management API server — binds on localhost:0 and serves the registration API.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use axum::{Router, routing};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use crate::management::handlers;

/// A single port registration entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortRegistration {
    pub local_port: u16,
    pub domain: String,
}

/// Shared state: PID -> list of registered ports.
pub type RegistrationStore = Arc<RwLock<HashMap<u32, Vec<PortRegistration>>>>;

/// The management API server.
pub struct ManagementServer {
    /// The shared registration store.
    pub store: RegistrationStore,
    /// The port the listener was bound to.
    pub bound_port: u16,
}

impl ManagementServer {
    /// Bind the listener to a random localhost port and return the server + listener pair.
    ///
    /// The caller is responsible for passing the listener to [`ManagementServer::serve`]
    /// so the bound port is known before the server starts accepting connections.
    pub async fn bind() -> Result<(Self, TcpListener)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let bound_port = listener.local_addr()?.port();
        let store: RegistrationStore = Arc::new(RwLock::new(HashMap::new()));
        let server = ManagementServer { store, bound_port };
        Ok((server, listener))
    }

    /// Run the axum server on the provided listener until it returns.
    pub async fn serve(self, listener: TcpListener) {
        let router = build_router(self.store);
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

fn build_router(store: RegistrationStore) -> Router {
    Router::new()
        .route("/v1/register", routing::post(handlers::register))
        .route("/v1/register", routing::delete(handlers::deregister))
        .route("/v1/status", routing::get(handlers::status))
        .with_state(store)
}
