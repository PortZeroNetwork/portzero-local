//! Local request forwarding: proxies HTTP requests from the edge to local services.
//!
//! When the edge server sends an HttpRequest through the tunnel, this module
//! builds a corresponding request to the local service (on localhost) and
//! collects the response to send back.

use anyhow::{Context, Result};
use devenv_tunnel_proto::ClientMessage;
use tracing::{debug, warn};

/// Forward an HTTP request from the edge to a local service.
///
/// Builds a request to `http://127.0.0.1:{local_port}{path}`, sets the
/// forwarded headers, sends it, and returns a `ClientMessage::HttpResponse`.
///
/// On connection failure, returns a 502 Bad Gateway response rather than
/// an error, so the tunnel can relay a meaningful status to the end user.
pub async fn forward_request(
    request_id: u64,
    method: &str,
    path: &str,
    host: &str,
    headers: &[(String, String)],
    body: &[u8],
    local_port: u16,
) -> Result<ClientMessage> {
    let url = format!("http://127.0.0.1:{}{}", local_port, path);
    debug!(request_id, %method, %url, %host, "Forwarding request to local service");

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .context("Failed to build HTTP client")?;

    let req_method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);

    let mut builder = client.request(req_method, &url);

    // Forward headers from the tunnel request, replacing Host with the original
    for (key, value) in headers {
        if key.eq_ignore_ascii_case("host") {
            // Preserve original host header so the local service sees it
            builder = builder.header("X-Forwarded-Host", value.as_str());
        } else {
            builder = builder.header(key.as_str(), value.as_str());
        }
    }

    // Add forwarding metadata
    builder = builder.header("X-Forwarded-For", "tunnel");
    builder = builder.header("X-Forwarded-Proto", "https");

    if !body.is_empty() {
        builder = builder.body(body.to_vec());
    }

    let response = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(request_id, "Local service error: {e}");
            return Ok(ClientMessage::HttpResponse {
                request_id,
                status: 502,
                headers: vec![("Content-Type".into(), "text/plain".into())],
                body: format!(
                    "Bad Gateway: could not reach local service on port {}.\n\
                     Error: {}\n\n\
                     Is the service running?",
                    local_port, e
                )
                .into_bytes(),
            });
        }
    };

    let status = response.status().as_u16();
    let resp_headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    let resp_body = response.bytes().await.unwrap_or_default().to_vec();

    debug!(
        request_id,
        status,
        body_len = resp_body.len(),
        "Local service responded"
    );

    Ok(ClientMessage::HttpResponse {
        request_id,
        status,
        headers: resp_headers,
        body: resp_body,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_forward_to_unreachable_port() {
        // Forwarding to a port with nothing listening should return 502
        let result = forward_request(
            1,
            "GET",
            "/health",
            "api.test.devenv.tools",
            &[],
            &[],
            19999, // unlikely to be in use
        )
        .await
        .unwrap();

        match result {
            ClientMessage::HttpResponse {
                request_id,
                status,
                body,
                ..
            } => {
                assert_eq!(request_id, 1);
                assert_eq!(status, 502);
                let body_str = String::from_utf8_lossy(&body);
                assert!(body_str.contains("Bad Gateway"));
                assert!(body_str.contains("19999"));
            }
            other => panic!("Expected HttpResponse, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_forward_preserves_request_id() {
        let result = forward_request(
            42,
            "POST",
            "/api/data",
            "api.test.devenv.tools",
            &[("Content-Type".to_string(), "application/json".to_string())],
            b"{}",
            19998,
        )
        .await
        .unwrap();

        match result {
            ClientMessage::HttpResponse { request_id, .. } => {
                assert_eq!(request_id, 42);
            }
            other => panic!("Expected HttpResponse, got {:?}", other),
        }
    }
}
