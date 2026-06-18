//! Codec for encoding and decoding tunnel protocol messages over WebSocket
//! text frames.
//!
//! Each WebSocket text frame contains exactly one JSON-encoded message.

use crate::{ClientMessage, ServerMessage};

/// Error type for codec operations.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("failed to serialize message: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("failed to deserialize message: {source}\n\nReceived text: {text}")]
    Deserialize {
        source: serde_json::Error,
        text: String,
    },
}

pub type Result<T> = std::result::Result<T, CodecError>;

/// Encode a [`ClientMessage`] into a JSON string suitable for a WebSocket text
/// frame.
pub fn encode(msg: &ClientMessage) -> Result<String> {
    Ok(serde_json::to_string(msg)?)
}

/// Decode a WebSocket text frame into a [`ServerMessage`].
pub fn decode_server(text: &str) -> Result<ServerMessage> {
    serde_json::from_str(text).map_err(|e| CodecError::Deserialize {
        source: e,
        text: text.to_owned(),
    })
}

/// Encode a [`ServerMessage`] into a JSON string suitable for a WebSocket text
/// frame.
pub fn encode_server(msg: &ServerMessage) -> Result<String> {
    Ok(serde_json::to_string(msg)?)
}

/// Decode a WebSocket text frame into a [`ClientMessage`].
pub fn decode_client(text: &str) -> Result<ClientMessage> {
    serde_json::from_str(text).map_err(|e| CodecError::Deserialize {
        source: e,
        text: text.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, RouteProtocol, PROTOCOL_VERSION};

    #[test]
    fn encode_decode_client_hello() {
        let msg = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: "tok_test".into(),
            client_version: "0.1.0".into(),
            machine_id: "m-test".into(),
            os: "linux".into(),
        };
        let text = encode(&msg).unwrap();
        let decoded = decode_client(&text).unwrap();
        match decoded {
            ClientMessage::Hello { auth_token, .. } => assert_eq!(auth_token, "tok_test"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn encode_decode_server_welcome() {
        let msg = ServerMessage::Welcome {
            session_id: "sess_1".into(),
            account_id: "acct_1".into(),
            plan: "free".into(),
        };
        let text = encode_server(&msg).unwrap();
        let decoded = decode_server(&text).unwrap();
        match decoded {
            ServerMessage::Welcome { session_id, .. } => assert_eq!(session_id, "sess_1"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn encode_decode_register_route() {
        let msg = ClientMessage::RegisterRoute {
            domain: "test.devenv.tools".into(),
            local_port: 8080,
            protocol: RouteProtocol::Tcp,
        };
        let text = encode(&msg).unwrap();
        let decoded = decode_client(&text).unwrap();
        match decoded {
            ClientMessage::RegisterRoute { protocol, .. } => {
                assert_eq!(protocol, RouteProtocol::Tcp);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn encode_decode_error() {
        let msg = ServerMessage::Error {
            code: ErrorCode::RateLimited,
            message: "slow down".into(),
        };
        let text = encode_server(&msg).unwrap();
        let decoded = decode_server(&text).unwrap();
        match decoded {
            ServerMessage::Error { code, .. } => assert_eq!(code, ErrorCode::RateLimited),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn decode_invalid_json_gives_helpful_error() {
        let result = decode_server("not json at all");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("not json at all"),
            "error should include the received text"
        );
    }

    #[test]
    fn decode_wrong_type_tag() {
        let result = decode_server(r#"{"type":"Bogus"}"#);
        assert!(result.is_err());
    }
}
