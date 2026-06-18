//! Team management commands: list, invite, members.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::api_client::ApiClient;

/// Resolve the base website URL from the environment or fall back to the default.
fn base_web_url() -> String {
    std::env::var("DEVENV_TOOLS_WEB_URL").unwrap_or_else(|_| "https://devenv.tools".to_string())
}

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Team {
    id: String,
    name: String,
    role: String,
}

#[derive(Debug, Deserialize)]
struct TeamsListResponse {
    teams: Vec<Team>,
}

#[derive(Serialize)]
struct InviteRequest {
    email: String,
}

#[derive(Debug, Deserialize)]
struct Member {
    email: String,
    name: Option<String>,
    role: String,
    environments: Vec<Environment>,
}

#[derive(Debug, Deserialize)]
struct Environment {
    domain: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct MembersResponse {
    members: Vec<Member>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// List teams the authenticated user belongs to.
pub async fn list() -> Result<()> {
    let client = ApiClient::new();
    client.require_auth()?;

    let resp = client.get("/teams").await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to list teams (HTTP {status}).\n\nServer response: {body}");
    }

    let data: TeamsListResponse = resp
        .json()
        .await
        .context("Failed to parse team list from server response.")?;

    if data.teams.is_empty() {
        println!("You are not a member of any teams.");
        println!(
            "\nCreate a team at {}/teams or ask a team admin to invite you.",
            base_web_url()
        );
        return Ok(());
    }

    // Column widths.
    let mut max_name = "TEAM".len();
    let mut max_role = "ROLE".len();

    for t in &data.teams {
        max_name = max_name.max(t.name.len());
        max_role = max_role.max(t.role.len());
    }

    println!(
        "{:<nw$}  {:<rw$}  ID",
        "TEAM",
        "ROLE",
        nw = max_name,
        rw = max_role,
    );

    for t in &data.teams {
        println!(
            "{:<nw$}  {:<rw$}  {}",
            t.name,
            t.role,
            t.id,
            nw = max_name,
            rw = max_role,
        );
    }

    Ok(())
}

/// Invite a user to the current team.
pub async fn invite(email: &str) -> Result<()> {
    let client = ApiClient::new();
    let auth = client.require_auth()?;

    // Determine the team. For now, use the first team the user belongs to.
    // A future improvement would accept a --team flag.
    let teams_resp = client.get("/teams").await?;
    if !teams_resp.status().is_success() {
        let status = teams_resp.status();
        let body = teams_resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to list teams (HTTP {status}).\n\nServer response: {body}");
    }

    let teams: TeamsListResponse = teams_resp
        .json()
        .await
        .context("Failed to parse team list from server response.")?;

    let team = teams.teams.first().ok_or_else(|| {
        anyhow::anyhow!(
            "You are not a member of any teams.\n\n\
             Create a team at {}/teams before inviting members.",
            base_web_url()
        )
    })?;

    let _auth = auth; // ensure auth stays borrowed for the scope
    let resp = client
        .post(
            &format!("/teams/{}/invite", team.id),
            &InviteRequest {
                email: email.to_string(),
            },
        )
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to invite {email} to team \"{}\" (HTTP {status}).\n\n\
             Server response: {body}",
            team.name,
        );
    }

    println!("Invitation sent to {email} for team \"{}\".", team.name);

    Ok(())
}

/// List members of the current team and their active environments.
pub async fn members() -> Result<()> {
    let client = ApiClient::new();
    client.require_auth()?;

    // Determine the team.
    let teams_resp = client.get("/teams").await?;
    if !teams_resp.status().is_success() {
        let status = teams_resp.status();
        let body = teams_resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to list teams (HTTP {status}).\n\nServer response: {body}");
    }

    let teams: TeamsListResponse = teams_resp
        .json()
        .await
        .context("Failed to parse team list from server response.")?;

    let team = teams.teams.first().ok_or_else(|| {
        anyhow::anyhow!(
            "You are not a member of any teams.\n\n\
             Create a team at {}/teams before listing members.",
            base_web_url()
        )
    })?;

    let resp = client.get(&format!("/teams/{}/members", team.id)).await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to list members of team \"{}\" (HTTP {status}).\n\n\
             Server response: {body}",
            team.name,
        );
    }

    let data: MembersResponse = resp
        .json()
        .await
        .context("Failed to parse members list from server response.")?;

    println!("Team: {}", team.name);
    println!();

    if data.members.is_empty() {
        println!("No members found.");
        return Ok(());
    }

    // Column widths.
    let mut max_email = "EMAIL".len();
    let mut max_name = "NAME".len();
    let mut max_role = "ROLE".len();

    for m in &data.members {
        max_email = max_email.max(m.email.len());
        max_name = max_name.max(m.name.as_deref().unwrap_or("-").len());
        max_role = max_role.max(m.role.len());
    }

    println!(
        "{:<ew$}  {:<nw$}  {:<rw$}  ENVIRONMENTS",
        "EMAIL",
        "NAME",
        "ROLE",
        ew = max_email,
        nw = max_name,
        rw = max_role,
    );

    for m in &data.members {
        let envs = if m.environments.is_empty() {
            "-".to_string()
        } else {
            m.environments
                .iter()
                .map(|e| format!("{} ({})", e.domain, e.status))
                .collect::<Vec<_>>()
                .join(", ")
        };

        println!(
            "{:<ew$}  {:<nw$}  {:<rw$}  {envs}",
            m.email,
            m.name.as_deref().unwrap_or("-"),
            m.role,
            ew = max_email,
            nw = max_name,
            rw = max_role,
        );
    }

    Ok(())
}
