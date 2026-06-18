//! Authentication commands: login, logout, whoami.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::api_client::ApiClient;

/// Stored auth credentials at ~/.devenv/auth.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub email: String,
    pub token: String,
    pub account_id: String,
    #[serde(default)]
    pub username: String,
}

impl AuthConfig {
    /// Path to the auth config file.
    pub fn path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine home directory.\n\n\
                 Set the HOME environment variable and try again."
            )
        })?;
        Ok(home.join(".devenv").join("auth.json"))
    }

    /// Load auth config from disk.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            anyhow::bail!("Not logged in. Run `devenv tunnel login` to authenticate.");
        }

        let content = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "Failed to read auth config at {}.\n\n\
                 The file may be corrupted. Try `devenv tunnel logout` then `devenv tunnel login`.",
                path.display()
            )
        })?;

        let config: AuthConfig = serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse auth config at {}.\n\n\
                 The file may be corrupted. Try `devenv tunnel logout` then `devenv tunnel login`.",
                path.display()
            )
        })?;

        Ok(config)
    }

    /// Save auth config to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create config directory: {}\n\n\
                     Check that you have write permissions to your home directory.",
                    parent.display()
                )
            })?;
        }

        let json = serde_json::to_string_pretty(self).context("Failed to serialize auth config")?;
        std::fs::write(&path, &json)
            .with_context(|| format!("Failed to write auth config to {}", path.display()))?;

        // Restrict permissions on Unix so other users cannot read the token.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&path, perms).ok();
        }

        Ok(())
    }

    /// Remove auth config from disk.
    pub fn remove() -> Result<()> {
        let path = Self::path()?;
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove auth config at {}", path.display()))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

/// Request body for POST /auth/login.
#[derive(Serialize)]
struct LoginRequest {
    email: String,
}

/// Response from POST /auth/login.
#[derive(Deserialize)]
struct LoginResponse {
    message: String,
}

/// Request body for POST /auth/verify.
#[derive(Serialize)]
struct VerifyRequest {
    email: String,
    code: String,
}

/// Response from POST /auth/verify.
#[derive(Deserialize)]
struct VerifyResponse {
    token: String,
    account_id: String,
    email: String,
    #[allow(dead_code)]
    name: String,
    #[serde(default)]
    username: String,
}

/// Callback payload sent by the dashboard to the CLI's local HTTP server.
#[derive(Deserialize)]
struct CallbackPayload {
    code: String,
    email: String,
    token: String,
    account_id: String,
    #[serde(default)]
    username: String,
}

/// Derive the dashboard URL from the API URL.
///
/// - If `DEVENV_TOOLS_DASHBOARD_URL` is set, use it directly.
/// - If `DEVENV_TOOLS_API_URL` looks like `localhost:3001`, use `localhost:3003`.
/// - Otherwise default to `https://app.devenv.tools`.
fn dashboard_url() -> String {
    if let Ok(url) = std::env::var("DEVENV_TOOLS_DASHBOARD_URL") {
        return url;
    }

    let api_url = std::env::var("DEVENV_TOOLS_API_URL")
        .unwrap_or_else(|_| crate::api_client::DEFAULT_API_URL.to_string());

    dashboard_url_from_api_url(&api_url)
}

fn dashboard_url_from_api_url(api_url: &str) -> String {
    if api_url.contains("localhost") || api_url.contains("127.0.0.1") {
        // Replace port with 3003 for the dashboard.
        if let Some(colon_pos) = api_url.rfind(':') {
            let base = &api_url[..colon_pos];
            return format!("{base}:3003");
        }
    }

    "https://app.devenv.tools".to_string()
}

/// Generate a random session code for the browser login flow.
fn generate_session_code() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 32] = rng.gen();
    hex::encode(bytes)
}

/// Run the browser-based login flow (default).
///
/// 1. Bind a temporary local HTTP server on a random port
/// 2. Open the browser to the dashboard's CLI auth page
/// 3. Wait for the dashboard to POST credentials back
/// 4. Save credentials and exit
async fn login_browser() -> Result<()> {
    // Bind to a random available port
    let listener = TcpListener::bind("127.0.0.1:0").await.with_context(|| {
        "Failed to bind a local TCP listener for the login callback.\n\n\
         Ensure that localhost networking is available."
    })?;
    let local_addr = listener.local_addr()?;
    let port = local_addr.port();

    let code = generate_session_code();
    let dash_url = dashboard_url();
    let auth_url = format!("{dash_url}/#/auth/cli?port={port}&code={code}");

    // Try to open the browser
    let browser_opened = open_browser(&auth_url);

    if browser_opened {
        println!("Opening browser for authentication...");
    } else {
        println!("Could not open a browser automatically.");
    }
    println!();
    println!("If the browser did not open, visit this URL to log in:");
    println!();
    println!("  {auth_url}");
    println!();
    println!("Waiting for browser login... (timeout: 2 minutes)");

    // Wait for a single POST /callback request with a 2-minute timeout
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        accept_callback(&listener, &code).await
    })
    .await;

    match result {
        Ok(Ok(config)) => {
            config.save()?;
            println!();
            println!("Logged in as {}.", config.email);
            println!("Credentials saved to ~/.devenv/auth.json");
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            anyhow::bail!(
                "Login timed out after 2 minutes.\n\n\
                 The browser login was not completed in time. Try again with:\n\
                 \n\
                   devenv tunnel login\n\
                 \n\
                 If you are on a headless server without a browser, use:\n\
                 \n\
                   devenv tunnel login --interactive"
            );
        }
    }
}

/// Try to open the given URL in the default browser.
/// Returns true if the command was spawned successfully.
fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let result: Result<std::process::Child, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));

    result.is_ok()
}

/// Accept a single HTTP request on the listener, parse the callback, and
/// return the AuthConfig if the session code matches.
async fn accept_callback(listener: &TcpListener, expected_code: &str) -> Result<AuthConfig> {
    loop {
        let (mut stream, _addr) = listener.accept().await?;

        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse the HTTP request minimally
        let first_line = request.lines().next().unwrap_or("");

        // Handle CORS preflight
        if first_line.starts_with("OPTIONS ") {
            let response = "HTTP/1.1 204 No Content\r\n\
Access-Control-Allow-Origin: *\r\n\
Access-Control-Allow-Methods: POST, OPTIONS\r\n\
Access-Control-Allow-Headers: Content-Type\r\n\
Access-Control-Max-Age: 86400\r\n\
Content-Length: 0\r\n\
\r\n";
            stream.write_all(response.as_bytes()).await.ok();
            stream.flush().await.ok();
            continue;
        }

        if !first_line.starts_with("POST /callback") {
            let response = "HTTP/1.1 404 Not Found\r\n\
Access-Control-Allow-Origin: *\r\n\
Content-Type: text/plain\r\n\
Content-Length: 9\r\n\
\r\n\
Not Found";
            stream.write_all(response.as_bytes()).await.ok();
            stream.flush().await.ok();
            continue;
        }

        // Extract JSON body (everything after the blank line)
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .or_else(|| request.split("\n\n").nth(1))
            .unwrap_or("");

        let payload: CallbackPayload = match serde_json::from_str(body) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("Invalid request body: {e}");
                let response = format!(
                    "HTTP/1.1 400 Bad Request\r\n\
Access-Control-Allow-Origin: *\r\n\
Content-Type: text/plain\r\n\
Content-Length: {}\r\n\
\r\n\
{}",
                    msg.len(),
                    msg
                );
                stream.write_all(response.as_bytes()).await.ok();
                stream.flush().await.ok();
                continue;
            }
        };

        // Verify session code
        if payload.code != expected_code {
            let msg = "Session code mismatch. This request may be from a stale login attempt.";
            let response = format!(
                "HTTP/1.1 403 Forbidden\r\n\
Access-Control-Allow-Origin: *\r\n\
Content-Type: text/plain\r\n\
Content-Length: {}\r\n\
\r\n\
{}",
                msg.len(),
                msg
            );
            stream.write_all(response.as_bytes()).await.ok();
            stream.flush().await.ok();
            continue;
        }

        // Success - send back a success response
        let success_body = r#"{"ok":true}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
Access-Control-Allow-Origin: *\r\n\
Content-Type: application/json\r\n\
Content-Length: {}\r\n\
\r\n\
{}",
            success_body.len(),
            success_body
        );
        stream.write_all(response.as_bytes()).await.ok();
        stream.flush().await.ok();

        return Ok(AuthConfig {
            email: payload.email,
            token: payload.token,
            account_id: payload.account_id,
            username: payload.username,
        });
    }
}

/// Run the interactive (terminal) login flow.
///
/// This is the fallback for headless servers where a browser is not available.
async fn login_interactive(email: Option<String>) -> Result<()> {
    let email = match email {
        Some(e) => e,
        None => {
            print!("Email: ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let trimmed = input.trim().to_string();
            if trimmed.is_empty() {
                anyhow::bail!("Email cannot be empty.");
            }
            trimmed
        }
    };

    println!("Sending verification code to {}...", email);

    let client = ApiClient::new();
    let resp = client
        .post(
            "/auth/login",
            &LoginRequest {
                email: email.clone(),
            },
        )
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to send verification code (HTTP {status}).\n\n\
             Server response: {body}\n\n\
             If you believe this is an error, visit {}/support for help.",
            std::env::var("DEVENV_TOOLS_WEB_URL")
                .unwrap_or_else(|_| "https://devenv.tools".to_string())
        );
    }

    let login_resp: LoginResponse = resp.json().await.with_context(|| {
        "Received an unexpected response from the server.\n\n\
         This may indicate an API version mismatch. Try updating devenv:\n\
         curl -fsSL https://devenv.tools/install.sh | sh"
    })?;

    println!("{}", login_resp.message);
    println!();
    print!("Enter verification code: ");
    io::stdout().flush()?;
    let mut code_input = String::new();
    io::stdin().read_line(&mut code_input)?;
    let code = code_input.trim().to_string();

    if code.is_empty() {
        anyhow::bail!("Verification code cannot be empty.");
    }

    println!("Verifying...");

    let verify_resp = client
        .post(
            "/auth/verify",
            &VerifyRequest {
                email: email.clone(),
                code,
            },
        )
        .await?;

    if !verify_resp.status().is_success() {
        let status = verify_resp.status();
        let body = verify_resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Verification failed (HTTP {status}).\n\n\
             Server response: {body}\n\n\
             The code may have expired. Run `devenv tunnel login --interactive` to try again."
        );
    }

    let verified: VerifyResponse = verify_resp
        .json()
        .await
        .with_context(|| "Received an unexpected response from the server during verification.")?;

    let config = AuthConfig {
        email: verified.email.clone(),
        token: verified.token,
        account_id: verified.account_id,
        username: verified.username,
    };
    config.save()?;

    println!("Logged in as {}.", verified.email);
    println!("Credentials saved to ~/.devenv/auth.json");

    Ok(())
}

/// Run the login flow.
///
/// By default opens a browser for authentication. Use `--interactive` to fall
/// back to terminal-based email+code flow (useful on headless servers).
pub async fn login(interactive: bool, email: Option<String>, _name: Option<String>) -> Result<()> {
    if interactive {
        login_interactive(email).await
    } else {
        login_browser().await
    }
}

// ---------------------------------------------------------------------------
// Logout
// ---------------------------------------------------------------------------

/// Remove stored credentials.
pub fn logout() -> Result<()> {
    AuthConfig::remove()?;
    println!("Logged out. Credentials removed.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Whoami
// ---------------------------------------------------------------------------

/// Response from GET /auth/me.
#[derive(Deserialize)]
struct WhoamiResponse {
    email: String,
    name: Option<String>,
    plan: Option<String>,
    team: Option<String>,
}

/// Show current user info.
pub async fn whoami() -> Result<()> {
    let client = ApiClient::new();
    client.require_auth()?;

    let resp = client.get("/auth/me").await?;

    if !resp.status().is_success() {
        let status = resp.status();
        if status.as_u16() == 401 {
            anyhow::bail!(
                "Authentication expired or invalid.\n\n\
                 Run `devenv tunnel logout` then `devenv tunnel login` to re-authenticate."
            );
        }
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to fetch account info (HTTP {status}).\n\nServer response: {body}");
    }

    let info: WhoamiResponse = resp
        .json()
        .await
        .with_context(|| "Failed to parse account info from server response.")?;

    println!("Email: {}", info.email);
    if let Some(name) = &info.name {
        println!("Name:  {name}");
    }
    if let Some(plan) = &info.plan {
        println!("Plan:  {plan}");
    }
    if let Some(team) = &info.team {
        println!("Team:  {team}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::dashboard_url_from_api_url;

    #[test]
    fn dashboard_url_tracks_the_app_domain_for_the_production_api() {
        assert_eq!(
            dashboard_url_from_api_url("https://app.devenv.tools/api"),
            "https://app.devenv.tools",
        );
    }

    #[test]
    fn dashboard_url_rewrites_local_api_ports_for_dev() {
        assert_eq!(
            dashboard_url_from_api_url("http://127.0.0.1:3001"),
            "http://127.0.0.1:3003",
        );
    }
}
