//! Axum request handlers for the management API.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::management::pid_lookup;
use crate::management::port_verify;
use crate::management::server::{PortRegistration, RegistrationStore};

// ─── Request / response types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub ports: Vec<PortRegistration>,
}

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pid: u32,
    registered: usize,
}

#[derive(Debug, Serialize)]
pub struct DeregisterResponse {
    pid: u32,
    deregistered: bool,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pid: u32,
    ports: Vec<PortRegistration>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    error: &'static str,
    detail: String,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Resolve the source port from the connecting address to a PID.
/// Returns an error response tuple on failure.
fn resolve_pid(
    addr: &SocketAddr,
) -> Result<u32, (StatusCode, Json<ErrorResponse>)> {
    let source_port = addr.port();
    match pid_lookup::pid_for_source_port(source_port) {
        Some(pid) => Ok(pid),
        None => {
            tracing::warn!("management: could not identify caller on port {}", source_port);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "caller_unidentifiable",
                    detail: format!(
                        "Could not identify the PID for source port {}",
                        source_port
                    ),
                }),
            ))
        }
    }
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// POST /v1/register — register a list of ports for the calling process.
pub async fn register(
    State(store): State<RegistrationStore>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), (StatusCode, Json<ErrorResponse>)> {
    let pid = resolve_pid(&addr)?;

    for reg in &body.ports {
        if !port_verify::pid_is_listening_on(pid, reg.local_port) {
            tracing::warn!(
                "management: PID {} not listening on port {}",
                pid,
                reg.local_port
            );
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: "port_not_listening",
                    detail: format!(
                        "PID {} is not listening on port {}",
                        pid, reg.local_port
                    ),
                }),
            ));
        }
    }

    let count = body.ports.len();
    store.write().await.insert(pid, body.ports);
    tracing::debug!("management: registered {} port(s) for PID {}", count, pid);

    Ok((StatusCode::OK, Json(RegisterResponse { pid, registered: count })))
}

/// DELETE /v1/register — remove the registration for the calling process.
pub async fn deregister(
    State(store): State<RegistrationStore>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<(StatusCode, Json<DeregisterResponse>), (StatusCode, Json<ErrorResponse>)> {
    let pid = resolve_pid(&addr)?;

    let removed = store.write().await.remove(&pid).is_some();
    if !removed {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "not_registered",
                detail: format!("PID {} has no active registration", pid),
            }),
        ));
    }

    tracing::debug!("management: deregistered PID {}", pid);
    Ok((StatusCode::OK, Json(DeregisterResponse { pid, deregistered: true })))
}

/// GET /v1/status — return the current registration for the calling process.
pub async fn status(
    State(store): State<RegistrationStore>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<(StatusCode, Json<StatusResponse>), (StatusCode, Json<ErrorResponse>)> {
    let pid = resolve_pid(&addr)?;

    let guard = store.read().await;
    match guard.get(&pid) {
        Some(ports) => {
            let ports = ports.clone();
            Ok((StatusCode::OK, Json(StatusResponse { pid, ports })))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "not_registered",
                detail: format!("PID {} has no active registration", pid),
            }),
        )),
    }
}
