//! Domain management commands: list, add, verify, remove.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::api_client::ApiClient;

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Domain {
    id: String,
    pattern: String,
    cname_target: String,
    verified: bool,
}

#[derive(Debug, Deserialize)]
struct DomainsListResponse {
    domains: Vec<Domain>,
}

#[derive(Serialize)]
struct AddDomainRequest {
    pattern: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AddDomainResponse {
    id: String,
    pattern: String,
    cname_target: String,
}

#[derive(Debug, Deserialize)]
struct VerifyResponse {
    verified: bool,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// List all domains for the authenticated account.
pub async fn list() -> Result<()> {
    let client = ApiClient::new();
    client.require_auth()?;

    let resp = client.get("/domains").await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to list domains (HTTP {status}).\n\nServer response: {body}");
    }

    let data: DomainsListResponse = resp
        .json()
        .await
        .context("Failed to parse domain list from server response.")?;

    if data.domains.is_empty() {
        println!("No custom domains configured.");
        println!("\nAdd one with: devenv tunnel domains add '*.dev.example.com'");
        return Ok(());
    }

    // Column widths.
    let mut max_pattern = "DOMAIN".len();
    let mut max_cname = "CNAME TARGET".len();

    for d in &data.domains {
        max_pattern = max_pattern.max(d.pattern.len());
        max_cname = max_cname.max(d.cname_target.len());
    }

    println!(
        "{:<pw$}  {:<cw$}  STATUS",
        "DOMAIN",
        "CNAME TARGET",
        pw = max_pattern,
        cw = max_cname,
    );

    for d in &data.domains {
        let status = if d.verified { "verified" } else { "pending" };
        println!(
            "{:<pw$}  {:<cw$}  {status}",
            d.pattern,
            d.cname_target,
            pw = max_pattern,
            cw = max_cname,
        );
    }

    Ok(())
}

/// Add a custom domain.
pub async fn add(domain: &str) -> Result<()> {
    let client = ApiClient::new();
    client.require_auth()?;

    let resp = client
        .post(
            "/domains",
            &AddDomainRequest {
                pattern: domain.to_string(),
            },
        )
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to add domain \"{domain}\" (HTTP {status}).\n\n\
             Server response: {body}\n\n\
             Make sure the domain is valid and you have permission to add it."
        );
    }

    let data: AddDomainResponse = resp
        .json()
        .await
        .context("Failed to parse add-domain response from server.")?;

    println!("Domain added: {}", data.pattern);
    println!();
    println!("Add this DNS record to verify ownership:");
    println!();
    println!("  {}  CNAME  {}", data.pattern, data.cname_target);
    println!();
    println!(
        "Then verify: devenv tunnel domains verify \"{}\"",
        data.pattern
    );

    Ok(())
}

/// Verify DNS for a domain.
pub async fn verify(domain: &str) -> Result<()> {
    let client = ApiClient::new();
    client.require_auth()?;

    // URL-encode the domain for the path.
    let encoded = urlencoded(domain);
    let resp = client.get(&format!("/domains/{encoded}/verify")).await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to verify domain \"{domain}\" (HTTP {status}).\n\n\
             Server response: {body}\n\n\
             Make sure you have added the domain first with `devenv tunnel domains add`."
        );
    }

    let data: VerifyResponse = resp
        .json()
        .await
        .context("Failed to parse verification response from server.")?;

    if data.verified {
        println!("Domain \"{domain}\" is verified.");
    } else {
        let hint = data
            .error
            .unwrap_or_else(|| "CNAME record not found or not yet propagated.".to_string());
        println!("Domain \"{domain}\" is NOT verified.");
        println!();
        println!("Reason: {hint}");
        println!();
        println!("DNS changes can take up to 48 hours to propagate.");
        println!("Run `devenv tunnel domains verify \"{domain}\"` again later.");
    }

    Ok(())
}

/// Remove a custom domain.
pub async fn remove(domain: &str) -> Result<()> {
    let client = ApiClient::new();
    client.require_auth()?;

    let encoded = urlencoded(domain);
    let resp = client.delete(&format!("/domains/{encoded}")).await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to remove domain \"{domain}\" (HTTP {status}).\n\n\
             Server response: {body}"
        );
    }

    println!("Domain \"{domain}\" removed.");
    println!("You can safely remove the CNAME record from your DNS provider.");

    Ok(())
}

/// Simple percent-encoding for domain patterns used in URL paths.
///
/// Encodes `*` and other special characters that would be problematic in URLs.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        match c {
            '*' => out.push_str("%2A"),
            ' ' => out.push_str("%20"),
            '/' => out.push_str("%2F"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            _ => out.push(c),
        }
    }
    out
}
