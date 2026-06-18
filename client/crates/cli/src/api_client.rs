//! HTTP client for the devenv.tools cloud API.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Serialize;

use crate::auth::AuthConfig;

/// Default base URL for the devenv.tools API.
pub(crate) const DEFAULT_API_URL: &str = "https://app.devenv.tools/api";

/// HTTP client for the devenv.tools API with automatic auth injection.
pub struct ApiClient {
    base_url: String,
    auth: Option<AuthConfig>,
    client: Client,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_api_url_points_at_the_dashboard_api() {
        assert_eq!(DEFAULT_API_URL, "https://app.devenv.tools/api");
    }
}

impl ApiClient {
    /// Create a new API client.
    ///
    /// Loads auth config from disk if available, and reads the API URL from
    /// the `DEVENV_TOOLS_API_URL` environment variable (useful for local
    /// development) or falls back to the default.
    pub fn new() -> Self {
        let base_url =
            std::env::var("DEVENV_TOOLS_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string());
        let auth = AuthConfig::load().ok();

        Self {
            base_url,
            auth,
            client: Client::new(),
        }
    }

    /// Require that auth is loaded, returning a helpful error if not.
    pub fn require_auth(&self) -> Result<&AuthConfig> {
        self.auth.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Not logged in. Run `devenv tunnel login` to authenticate.")
        })
    }

    /// Send a GET request to the given API path (e.g. "/auth/me").
    pub async fn get(&self, path: &str) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);

        if let Some(auth) = &self.auth {
            req = req.header("Authorization", format!("Bearer {}", auth.token));
        }

        let resp = req.send().await.with_context(|| {
            format!(
                "Failed to reach the devenv.tools API at {url}\n\n\
                 Check your internet connection, or if you are using a custom API URL,\n\
                 verify that DEVENV_TOOLS_API_URL is correct."
            )
        })?;

        Ok(resp)
    }

    /// Send a POST request with a JSON body.
    pub async fn post<T: Serialize>(&self, path: &str, body: &T) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.post(&url).json(body);

        if let Some(auth) = &self.auth {
            req = req.header("Authorization", format!("Bearer {}", auth.token));
        }

        let resp = req.send().await.with_context(|| {
            format!(
                "Failed to reach the devenv.tools API at {url}\n\n\
                 Check your internet connection, or if you are using a custom API URL,\n\
                 verify that DEVENV_TOOLS_API_URL is correct."
            )
        })?;

        Ok(resp)
    }

    /// Send a DELETE request.
    pub async fn delete(&self, path: &str) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.delete(&url);

        if let Some(auth) = &self.auth {
            req = req.header("Authorization", format!("Bearer {}", auth.token));
        }

        let resp = req.send().await.with_context(|| {
            format!(
                "Failed to reach the devenv.tools API at {url}\n\n\
                 Check your internet connection, or if you are using a custom API URL,\n\
                 verify that DEVENV_TOOLS_API_URL is correct."
            )
        })?;

        Ok(resp)
    }
}
