//! Cloud connector: manages the WebSocket tunnel to the edge server.
//!
//! When authenticated, the daemon connects to `wss://edge.portzero.cloud/tunnel`
//! and registers discovered routes so they become reachable from the internet.
//! Incoming HTTP requests are forwarded to local services.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use portzero_proto::{ClientMessage, ServerMessage};
use portzero_tunnel_client::domain_router::DomainRouter;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::forwarder;

/// Default edge server WebSocket URL.
const DEFAULT_EDGE_URL: &str = "wss://edge.portzero.cloud/tunnel";

/// Resolve the edge server URL from the environment or fall back to the default.
fn resolve_edge_url() -> String {
    std::env::var("PZ_TUNNEL_EDGE_URL").unwrap_or_else(|_| DEFAULT_EDGE_URL.to_string())
}

/// Cloud connector state.
pub struct CloudConnector {
    /// Outbound message sender.
    tx: Option<mpsc::Sender<ClientMessage>>,
    /// Auth token for the edge server.
    auth_token: String,
    /// Machine ID (derived from hostname + random suffix).
    machine_id: String,
    /// Edge server URL.
    edge_url: String,
    /// Session ID from the server Welcome message.
    session_id: Option<String>,
    /// Plan reported by the edge on Welcome (e.g. "free", "pro").
    /// Used to drive upsell prompts for non-paying users attempting cloud tunnels.
    plan: Arc<Mutex<Option<String>>>,
    /// Latest user-facing status message from the edge (e.g. plan limit explanation).
    /// This is surfaced in `portzero status` and the web dashboard when present.
    status_message: Arc<Mutex<Option<String>>>,
    /// True while the inbound WebSocket reader task is alive.
    /// The outbound writer task only fails on write, so it can stay alive after
    /// the connection drops if nothing is being sent — causing is_connected() to
    /// return true on a dead connection. This flag is set by the reader task.
    inbound_alive: Arc<AtomicBool>,
    /// Set by the inbound reader when the server rejects our token (AuthFailed).
    /// Signals the discovery loop to stop retrying and wait for a new token.
    auth_failed: Arc<AtomicBool>,
}

impl CloudConnector {
    /// Create a new cloud connector.
    ///
    /// Reads the edge server URL from `PZ_TUNNEL_EDGE_URL` (useful for
    /// local development) or falls back to the default production URL.
    pub fn new(auth_token: String) -> Self {
        let machine_id = generate_machine_id();
        Self {
            tx: None,
            auth_token,
            machine_id,
            edge_url: resolve_edge_url(),
            session_id: None,
            plan: Arc::new(Mutex::new(None)),
            status_message: Arc::new(Mutex::new(None)),
            inbound_alive: Arc::new(AtomicBool::new(false)),
            auth_failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a cloud connector with a custom edge URL (useful for testing).
    #[cfg(test)]
    pub fn with_url(auth_token: String, edge_url: String) -> Self {
        let machine_id = generate_machine_id();
        Self {
            tx: None,
            auth_token,
            machine_id,
            edge_url,
            session_id: None,
            plan: Arc::new(Mutex::new(None)),
            status_message: Arc::new(Mutex::new(None)),
            inbound_alive: Arc::new(AtomicBool::new(false)),
            auth_failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Connect to the edge server via WebSocket.
    ///
    /// Spawns a background task that reads from the WebSocket and forwards
    /// incoming requests to local services via the domain router.
    pub async fn connect(&mut self, domain_router: DomainRouter) -> Result<()> {
        tracing::info!("Connecting to edge server at {}", self.edge_url);

        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.edge_url)
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to edge server at {}.\n\n\
                     Check your internet connection and that the edge server is running.\n\
                     The daemon will continue in local-only mode.",
                    self.edge_url
                )
            })?;

        let (mut ws_sink, mut ws_stream_rx) = ws_stream.split();

        // Channel for outbound messages
        let (tx, mut rx) = mpsc::channel::<ClientMessage>(64);
        self.tx = Some(tx.clone());
        self.inbound_alive.store(true, Ordering::Relaxed);

        // Send Hello
        let hello = ClientMessage::Hello {
            protocol_version: portzero_proto::PROTOCOL_VERSION,
            auth_token: self.auth_token.clone(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            machine_id: self.machine_id.clone(),
            os: std::env::consts::OS.to_string(),
        };
        let hello_json = serde_json::to_string(&hello)?;
        ws_sink.send(Message::Text(hello_json.into())).await?;

        // Spawn outbound writer task
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::error!("Failed to serialize outbound message: {}", e);
                        continue;
                    }
                };
                if let Err(e) = ws_sink.send(Message::Text(json.into())).await {
                    tracing::error!("Failed to send message to edge: {}", e);
                    break;
                }
            }
            tracing::debug!("Outbound writer task exiting");
        });

        // Spawn inbound reader task
        let tx_for_reader = tx.clone();
        let inbound_alive = Arc::clone(&self.inbound_alive);
        let auth_failed = Arc::clone(&self.auth_failed);
        let plan_for_reader = Arc::clone(&self.plan);
        let status_msg_for_reader = Arc::clone(&self.status_message);
        tokio::spawn(async move {
            while let Some(msg_result) = ws_stream_rx.next().await {
                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!("WebSocket read error: {}", e);
                        break;
                    }
                };

                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => {
                        tracing::info!("Edge server closed connection");
                        break;
                    }
                    Message::Ping(_) | Message::Pong(_) => continue,
                    _ => continue,
                };

                let server_msg: ServerMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("Failed to parse server message: {}", e);
                        continue;
                    }
                };

                // Capture plan (from Welcome) and friendly upsell / plan messages for the client UI.
                // These drive "you need a paid plan" prompts when users try *.portzero.cloud on free.
                match &server_msg {
                    ServerMessage::Welcome { plan, .. } => {
                        if let Ok(mut g) = plan_for_reader.lock() {
                            *g = Some(plan.clone());
                        }
                        tracing::info!("Edge session established (plan: {})", plan);
                    }
                    ServerMessage::Error { code, message } => {
                        if *code == portzero_proto::ErrorCode::PlanLimitExceeded {
                            let friendly = "Your plan does not include cloud tunnels (portzero.cloud). \
                                Upgrade at https://app.portzero.cloud to use *.portzero.cloud domains.".to_string();
                            if let Ok(mut g) = status_msg_for_reader.lock() {
                                *g = Some(friendly);
                            }
                            tracing::warn!("Edge reported plan limit: {}", message);
                        } else if message.to_lowercase().contains("plan")
                            || message.to_lowercase().contains("limit")
                            || message.to_lowercase().contains("upgrade")
                            || message.to_lowercase().contains("paid")
                            || message.to_lowercase().contains("subscription")
                        {
                            if let Ok(mut g) = status_msg_for_reader.lock() {
                                *g = Some(message.clone());
                            }
                        }
                        if message
                            .starts_with("Route accepted by edge, but dashboard persistence failed")
                        {
                            tracing::warn!("{}", message);
                        } else if *code != portzero_proto::ErrorCode::PlanLimitExceeded {
                            tracing::error!("Edge server error ({:?}): {}", code, message);
                        }
                        if *code == portzero_proto::ErrorCode::AuthFailed {
                            auth_failed.store(true, Ordering::Relaxed);
                            inbound_alive.store(false, Ordering::Relaxed);
                            break;
                        }
                        continue;
                    }
                    ServerMessage::RouteAck {
                        success: false,
                        error: Some(err),
                        domain,
                        ..
                    } => {
                        let low = err.to_lowercase();
                        if low.contains("plan")
                            || low.contains("limit")
                            || low.contains("upgrade")
                            || low.contains("paid")
                            || low.contains("subscription")
                        {
                            let friendly = format!(
                                "Cloud route {} rejected: {}. Upgrade at https://app.portzero.cloud for portzero.cloud tunnels.",
                                domain, err
                            );
                            if let Ok(mut g) = status_msg_for_reader.lock() {
                                *g = Some(friendly);
                            }
                        }
                        // General warn happens in handle_incoming
                    }
                    _ => {}
                }

                match handle_incoming(server_msg, &domain_router).await {
                    Ok(Some(response)) => {
                        if let Err(e) = tx_for_reader.send(response).await {
                            tracing::error!("Failed to queue response: {}", e);
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::error!("Error handling incoming message: {}", e);
                    }
                }
            }
            inbound_alive.store(false, Ordering::Relaxed);
            tracing::info!("Inbound reader task exiting");
        });

        tracing::info!("Connected to edge server");
        Ok(())
    }

    /// Register a discovered route with the cloud edge.
    pub async fn register_route(&self, domain: &str, local_port: u16) -> Result<()> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not connected to edge server"))?;

        let msg = ClientMessage::RegisterRoute {
            domain: domain.to_string(),
            local_port,
            protocol: portzero_proto::RouteProtocol::Http,
        };

        tx.send(msg)
            .await
            .context("Failed to send RegisterRoute to edge")?;

        tracing::info!("Registered route with cloud: {} -> :{}", domain, local_port);
        Ok(())
    }

    /// Unregister a route from the cloud edge.
    pub async fn unregister_route(&self, domain: &str) -> Result<()> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not connected to edge server"))?;

        let msg = ClientMessage::UnregisterRoute {
            domain: domain.to_string(),
        };

        tx.send(msg)
            .await
            .context("Failed to send UnregisterRoute to edge")?;

        tracing::info!("Unregistered route from cloud: {}", domain);
        Ok(())
    }

    /// Check if the tunnel connection is fully alive (outbound channel open AND
    /// inbound reader running). Checking only the outbound tx is insufficient:
    /// the writer task idles on recv() and won't notice a dead socket until it
    /// tries to write, so a dropped inbound reader goes undetected indefinitely.
    pub fn is_connected(&self) -> bool {
        self.inbound_alive.load(Ordering::Relaxed)
            && self.tx.as_ref().map(|tx| !tx.is_closed()).unwrap_or(false)
    }

    /// Whether the server rejected our token with an AuthFailed error.
    /// When true the discovery loop drops this connector and waits for a new token.
    pub fn is_auth_failed(&self) -> bool {
        self.auth_failed.load(Ordering::Relaxed)
    }

    /// Plan reported on successful Welcome (e.g. "free", "pro").
    /// None until the first Welcome is received.
    pub fn plan(&self) -> Option<String> {
        self.plan.lock().ok().and_then(|g| g.clone())
    }

    /// Latest user-facing status message (plan limits, quota, etc.).
    /// Set by the inbound reader on relevant ServerMessages.
    pub fn status_message(&self) -> Option<String> {
        self.status_message.lock().ok().and_then(|g| g.clone())
    }

    /// Reconnect to the edge server.
    pub async fn reconnect(&mut self, domain_router: DomainRouter) -> Result<()> {
        tracing::info!("Reconnecting to edge server...");
        self.tx = None;
        self.session_id = None;
        if let Ok(mut g) = self.plan.lock() {
            *g = None;
        }
        if let Ok(mut g) = self.status_message.lock() {
            *g = None;
        }
        self.inbound_alive.store(false, Ordering::Relaxed);
        self.auth_failed.store(false, Ordering::Relaxed);
        self.connect(domain_router).await
    }
}

/// Handle an incoming server message.
///
/// Returns an optional response to send back through the tunnel.
pub async fn handle_incoming(
    msg: ServerMessage,
    domain_router: &DomainRouter,
) -> Result<Option<ClientMessage>> {
    match msg {
        ServerMessage::Welcome {
            session_id, plan, ..
        } => {
            // Plan is captured in the reader loop before calling handle_incoming for upsell logic.
            tracing::debug!(
                "Welcome received for session {} (plan {})",
                session_id,
                plan
            );
            Ok(None)
        }
        ServerMessage::RouteAck {
            domain,
            success,
            error,
            url,
        } => {
            if success {
                let public_url = url.as_deref().unwrap_or(&domain);
                tracing::info!("Route accepted by edge: {} → {}", domain, public_url);
            } else {
                tracing::warn!(
                    "Route rejected by edge: {} - {}",
                    domain,
                    error.as_deref().unwrap_or("unknown error")
                );
            }
            Ok(None)
        }
        ServerMessage::RouteExpired { domain, reason } => {
            tracing::warn!("Route expired: {} ({})", domain, reason);
            Ok(None)
        }
        ServerMessage::HttpRequest {
            request_id,
            method,
            path,
            host,
            headers,
            body,
        } => {
            let local_port = domain_router.resolve(&host).ok_or_else(|| {
                anyhow::anyhow!(
                    "No local service for host '{}' — route may have been removed",
                    host
                )
            })?;

            let response = forwarder::forward_request(
                request_id, &method, &path, &host, &headers, &body, local_port,
            )
            .await?;

            Ok(Some(response))
        }
        ServerMessage::Ping { timestamp } => Ok(Some(ClientMessage::Pong { timestamp })),
        ServerMessage::Error { code, message } => {
            tracing::error!("Edge server error ({:?}): {}", code, message);
            Ok(None)
        }
    }
}

/// Generate a machine ID from hostname + random suffix.
fn generate_machine_id() -> String {
    let host = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());
    let suffix = &uuid::Uuid::new_v4().to_string()[..8];
    format!("{}-{}", host, suffix)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_machine_id() {
        let id = generate_machine_id();
        assert!(!id.is_empty());
        assert!(id.contains('-'));
    }

    #[test]
    fn test_cloud_connector_new() {
        let conn = CloudConnector::new("tok_test".to_string());
        assert!(!conn.is_connected());
        assert!(!conn.is_auth_failed());
        assert!(conn.session_id.is_none());
        assert_eq!(conn.auth_token, "tok_test");
        assert!(conn.plan().is_none());
        assert!(conn.status_message().is_none());
    }

    #[test]
    fn test_is_auth_failed_flag() {
        let conn = CloudConnector::new("tok_test".to_string());
        assert!(!conn.is_auth_failed());
        conn.auth_failed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(conn.is_auth_failed());
    }

    #[tokio::test]
    async fn test_handle_incoming_welcome() {
        let router = DomainRouter::new();
        let msg = ServerMessage::Welcome {
            session_id: "sess_123".to_string(),
            account_id: "acct_1".to_string(),
            plan: "free".to_string(),
        };
        let result = handle_incoming(msg, &router).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_handle_incoming_ping() {
        let router = DomainRouter::new();
        let msg = ServerMessage::Ping { timestamp: 42 };
        let result = handle_incoming(msg, &router).await.unwrap();
        match result {
            Some(ClientMessage::Pong { timestamp }) => assert_eq!(timestamp, 42),
            other => panic!("Expected Pong, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_handle_incoming_route_ack_success() {
        let router = DomainRouter::new();
        let msg = ServerMessage::RouteAck {
            domain: "api-test.alice.portzero.cloud".to_string(),
            success: true,
            error: None,
            url: Some("https://api-test.alice.portzero.cloud".to_string()),
        };
        let result = handle_incoming(msg, &router).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_handle_incoming_route_ack_failure() {
        let router = DomainRouter::new();
        let msg = ServerMessage::RouteAck {
            domain: "api-test.alice.portzero.cloud".to_string(),
            success: false,
            error: Some("domain not authorized".to_string()),
            url: None,
        };
        let result = handle_incoming(msg, &router).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_handle_incoming_error() {
        let router = DomainRouter::new();
        let msg = ServerMessage::Error {
            code: portzero_proto::ErrorCode::InternalError,
            message: "service unavailable".to_string(),
        };
        let result = handle_incoming(msg, &router).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_handle_incoming_http_request_no_route() {
        let router = DomainRouter::new();
        let msg = ServerMessage::HttpRequest {
            request_id: 1,
            method: "GET".to_string(),
            path: "/".to_string(),
            host: "unknown-svc.alice.portzero.cloud".to_string(),
            headers: vec![],
            body: vec![],
        };
        let result = handle_incoming(msg, &router).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_register_route_not_connected() {
        let conn = CloudConnector::new("tok_test".to_string());
        let result = conn
            .register_route("api-test.alice.portzero.cloud", 8080)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Not connected"));
    }

    #[tokio::test]
    async fn test_unregister_route_not_connected() {
        let conn = CloudConnector::new("tok_test".to_string());
        let result = conn.unregister_route("api-test.alice.portzero.cloud").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Not connected"));
    }
}
