//! Shared protocol types for the devenv-tools tunnel.
//!
//! These types define the wire format between the tunnel client (running on the
//! developer's machine) and the edge server (running in the cloud).
//!
//! ## Framing
//!
//! Messages are JSON-encoded and sent as WebSocket text frames. Each frame
//! contains exactly one JSON object with a `"type"` discriminator field.

use serde::{Deserialize, Serialize};

pub mod codec;
pub mod connection;

/// Current protocol version. Included in the `Hello` handshake so the server
/// can reject incompatible clients.
pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Transport protocol for a registered route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteProtocol {
    Http,
    Tcp,
}

/// Strongly-typed error codes returned by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    AuthFailed,
    RateLimited,
    PlanLimitExceeded,
    DomainNotAllowed,
    InternalError,
}

// ---------------------------------------------------------------------------
// Client -> Server messages
// ---------------------------------------------------------------------------

/// Messages sent from the tunnel client to the edge server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Initial handshake: authenticate and declare the client.
    Hello {
        protocol_version: u32,
        auth_token: String,
        client_version: String,
        machine_id: String,
        os: String,
    },
    /// Register a route: make a domain reachable via this tunnel.
    RegisterRoute {
        domain: String,
        local_port: u16,
        protocol: RouteProtocol,
    },
    /// Unregister a route: stop forwarding traffic for this domain.
    UnregisterRoute { domain: String },
    /// Response to an HTTP request forwarded by the edge.
    HttpResponse {
        request_id: u64,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// Keep-alive reply.
    Pong { timestamp: u64 },
}

// ---------------------------------------------------------------------------
// Server -> Client messages
// ---------------------------------------------------------------------------

/// Messages sent from the edge server to the tunnel client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Handshake accepted.
    Welcome {
        session_id: String,
        account_id: String,
        plan: String,
    },
    /// Acknowledgement of a route registration.
    RouteAck {
        domain: String,
        success: bool,
        error: Option<String>,
        url: Option<String>,
    },
    /// An incoming HTTP request to be forwarded to the local service.
    HttpRequest {
        request_id: u64,
        method: String,
        path: String,
        host: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// Keep-alive probe.
    Ping { timestamp: u64 },
    /// Server-side error.
    Error { code: ErrorCode, message: String },
    /// Notification that a route has expired or been revoked.
    RouteExpired { domain: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // ClientMessage roundtrips
    // -----------------------------------------------------------------------

    #[test]
    fn roundtrip_hello() {
        let msg = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: "tok_abc".into(),
            client_version: "0.1.0".into(),
            machine_id: "m-1234".into(),
            os: "linux".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::Hello {
                protocol_version,
                auth_token,
                os,
                ..
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(auth_token, "tok_abc");
                assert_eq!(os, "linux");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_register_route() {
        let msg = ClientMessage::RegisterRoute {
            domain: "myapp.tunnel.devenv.tools".into(),
            local_port: 3000,
            protocol: RouteProtocol::Http,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::RegisterRoute {
                domain,
                local_port,
                protocol,
            } => {
                assert_eq!(domain, "myapp.tunnel.devenv.tools");
                assert_eq!(local_port, 3000);
                assert_eq!(protocol, RouteProtocol::Http);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_unregister_route() {
        let msg = ClientMessage::UnregisterRoute {
            domain: "myapp.tunnel.devenv.tools".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::UnregisterRoute { domain } => {
                assert_eq!(domain, "myapp.tunnel.devenv.tools");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_http_response() {
        let msg = ClientMessage::HttpResponse {
            request_id: 99,
            status: 200,
            headers: vec![("Content-Type".into(), "text/plain".into())],
            body: b"ok".to_vec(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::HttpResponse {
                request_id,
                status,
                body,
                ..
            } => {
                assert_eq!(request_id, 99);
                assert_eq!(status, 200);
                assert_eq!(body, b"ok");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_pong() {
        let msg = ClientMessage::Pong {
            timestamp: 1234567890,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::Pong { timestamp } => assert_eq!(timestamp, 1234567890),
            _ => panic!("wrong variant"),
        }
    }

    // -----------------------------------------------------------------------
    // ServerMessage roundtrips
    // -----------------------------------------------------------------------

    #[test]
    fn roundtrip_welcome() {
        let msg = ServerMessage::Welcome {
            session_id: "sess_1".into(),
            account_id: "acct_1".into(),
            plan: "pro".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::Welcome {
                session_id,
                account_id,
                plan,
            } => {
                assert_eq!(session_id, "sess_1");
                assert_eq!(account_id, "acct_1");
                assert_eq!(plan, "pro");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_route_ack() {
        let msg = ServerMessage::RouteAck {
            domain: "myapp.tunnel.devenv.tools".into(),
            success: true,
            error: None,
            url: Some("https://myapp.tunnel.devenv.tools".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::RouteAck {
                domain,
                success,
                error,
                url,
            } => {
                assert_eq!(domain, "myapp.tunnel.devenv.tools");
                assert!(success);
                assert!(error.is_none());
                assert_eq!(url.unwrap(), "https://myapp.tunnel.devenv.tools");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_http_request() {
        let msg = ServerMessage::HttpRequest {
            request_id: 42,
            method: "GET".into(),
            path: "/api/health".into(),
            host: "api.myapp.tunnel.devenv.tools".into(),
            headers: vec![("Accept".into(), "application/json".into())],
            body: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::HttpRequest { request_id, .. } => assert_eq!(request_id, 42),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_ping() {
        let msg = ServerMessage::Ping { timestamp: 9999999 };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::Ping { timestamp } => assert_eq!(timestamp, 9999999),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_error() {
        let msg = ServerMessage::Error {
            code: ErrorCode::AuthFailed,
            message: "invalid token".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::Error { code, message } => {
                assert_eq!(code, ErrorCode::AuthFailed);
                assert_eq!(message, "invalid token");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_route_expired() {
        let msg = ServerMessage::RouteExpired {
            domain: "myapp.tunnel.devenv.tools".into(),
            reason: "idle timeout".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::RouteExpired { domain, reason } => {
                assert_eq!(domain, "myapp.tunnel.devenv.tools");
                assert_eq!(reason, "idle timeout");
            }
            _ => panic!("wrong variant"),
        }
    }

    // -----------------------------------------------------------------------
    // Supporting types
    // -----------------------------------------------------------------------

    #[test]
    fn route_protocol_roundtrip() {
        for proto in [RouteProtocol::Http, RouteProtocol::Tcp] {
            let json = serde_json::to_string(&proto).unwrap();
            let decoded: RouteProtocol = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, proto);
        }
    }

    #[test]
    fn error_code_roundtrip() {
        let codes = [
            ErrorCode::AuthFailed,
            ErrorCode::RateLimited,
            ErrorCode::PlanLimitExceeded,
            ErrorCode::DomainNotAllowed,
            ErrorCode::InternalError,
        ];
        for code in codes {
            let json = serde_json::to_string(&code).unwrap();
            let decoded: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, code);
        }
    }
}
