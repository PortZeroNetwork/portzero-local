//! The developer status dashboard: the self-contained HTML shell served at
//! `portzero.local`, the `/status.json` snapshot it renders from, and the
//! branding assets and OpenAPI document it links to.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use axum::Json;

use super::{read_cloud_state, read_daemon_pid, read_overlay_state};
use crate::management::server::{AppState, PortRegistration};
use crate::route_table::RouteTable;

const PORTZERO_MARK_JPEG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/management/assets/portzero-mark.jpg"
));
const PORTZERO_WORDMARK_JPEG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/management/assets/portzero-wordmark.jpg"
));
const GETTING_STARTED_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../installer/getting-started.json"
));

/// Read and parse `routes.json`.  Returns an empty table on any error.
fn read_route_table(state_dir: &std::path::Path) -> RouteTable {
    RouteTable::load(&state_dir.join("routes.json")).unwrap_or_default()
}

/// Read `cloud_route_status.json` (domain → review status). Empty on any error.
fn read_cloud_route_statuses(
    state_dir: &std::path::Path,
) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(state_dir.join("cloud_route_status.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Read and parse `issues.json` (visibility-layer problems: duplicate overlay
/// names, legacy listeners, Docker port conflicts, invalidly-scoped cloud
/// tunnel domains). Returns an empty state on any error, matching `portzero
/// status`'s CLI behavior for the same file.
pub(super) fn read_issues(state_dir: &std::path::Path) -> crate::notify::IssuesState {
    crate::notify::read_issues(&state_dir.join("issues.json"))
}

fn command_on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            dir.join(format!("{program}.exe")).is_file()
        }
        #[cfg(not(windows))]
        {
            false
        }
    })
}

fn detected_languages() -> serde_json::Value {
    let languages = [
        ("typescript", "TypeScript", &["node", "nodejs", "npm"][..]),
        ("javascript", "JavaScript", &["node", "nodejs"][..]),
        ("python", "Python", &["python3", "python"][..]),
        ("java", "Java", &["java", "javac"][..]),
        ("go", "Go", &["go"][..]),
        ("rust", "Rust", &["cargo", "rustc"][..]),
        ("csharp", "C#", &["dotnet"][..]),
        ("php", "PHP", &["php", "composer"][..]),
        ("ruby", "Ruby", &["ruby", "bundle"][..]),
        (
            "cpp",
            "C/C++",
            &["cc", "gcc", "clang", "g++", "clang++"][..],
        ),
        ("swift", "Swift", &["swift"][..]),
        ("kotlin", "Kotlin", &["kotlin", "kotlinc"][..]),
        ("dart", "Dart", &["dart"][..]),
        ("elixir", "Elixir", &["elixir", "mix"][..]),
        ("scala", "Scala", &["scala", "sbt"][..]),
        ("r", "R", &["R", "Rscript"][..]),
        ("julia", "Julia", &["julia"][..]),
        ("lua", "Lua", &["lua", "luajit"][..]),
        ("perl", "Perl", &["perl"][..]),
        ("zig", "Zig", &["zig"][..]),
        ("haskell", "Haskell", &["ghc", "cabal", "stack"][..]),
        ("shell", "Shell", &["bash", "sh"][..]),
    ];

    let detected: Vec<serde_json::Value> = languages
        .iter()
        .map(|(id, label, tools)| {
            let installed_tools: Vec<&str> = tools
                .iter()
                .copied()
                .filter(|tool| command_on_path(tool))
                .collect();
            serde_json::json!({
                "id": id,
                "label": label,
                "installed": !installed_tools.is_empty(),
                "tools": installed_tools,
            })
        })
        .collect();

    let default_language = detected
        .iter()
        .find(|lang| {
            lang.get("id").and_then(|v| v.as_str()) != Some("shell")
                && lang
                    .get("installed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
        .and_then(|lang| lang.get("id").and_then(|v| v.as_str()))
        .unwrap_or("typescript");

    serde_json::json!({
        "default": default_language,
        "items": detected,
    })
}

fn detected_environment() -> serde_json::Value {
    let (os_id, os_label) = if cfg!(target_os = "macos") {
        ("macos", "macOS")
    } else if cfg!(target_os = "windows") {
        ("windows", "Windows")
    } else if cfg!(target_os = "linux") {
        ("linux", "Linux")
    } else {
        ("unix", std::env::consts::OS)
    };

    serde_json::json!({
        "os": {
            "id": os_id,
            "label": os_label,
        },
        "docker": {
            "installed": command_on_path("docker"),
        },
    })
}

fn getting_started_manifest() -> serde_json::Value {
    serde_json::from_str(GETTING_STARTED_JSON).unwrap_or_else(|_| {
        serde_json::json!({
            "examples": [],
            "languages": [],
        })
    })
}

fn display_substitutions(
    substitutions: &std::collections::BTreeMap<String, String>,
    source: &crate::discovery::ServiceSource,
) -> serde_json::Value {
    let mut values = substitutions.clone();
    if !values.contains_key("folder-name") {
        let folder_name = match source {
            crate::discovery::ServiceSource::Process { cwd } => cwd
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            crate::discovery::ServiceSource::Container { name, .. } => name.clone(),
        };
        values.insert("folder-name".to_string(), folder_name);
    }
    serde_json::json!(values)
}

pub(super) fn local_service_link_url(domain: &str, service_port: u16) -> Option<String> {
    match service_port {
        80 => Some(format!("http://{domain}")),
        443 => Some(format!("https://{domain}")),
        _ => None,
    }
}

pub(super) fn has_dns_token(template: &str, value: &str) -> bool {
    if value.is_empty() {
        return false;
    }

    template.match_indices(value).any(|(start, _)| {
        let end = start + value.len();
        let before_is_boundary = template[..start]
            .chars()
            .next_back()
            .map(|c| !c.is_ascii_alphanumeric())
            .unwrap_or(true);
        let after_is_boundary = template[end..]
            .chars()
            .next()
            .map(|c| !c.is_ascii_alphanumeric())
            .unwrap_or(true);
        before_is_boundary && after_is_boundary
    })
}

pub(super) fn substitution_alerts(
    domain_template: &str,
    substitutions: &std::collections::BTreeMap<String, String>,
) -> Vec<serde_json::Value> {
    const SUGGESTED_KEYS: &[&str] = &[
        "branch",
        "worktree",
        "project",
        "user",
        "machine",
        "uid",
        "local-username",
        "cloud-username",
        "folder-name",
    ];

    let mut alerts = Vec::new();

    if domain_template.contains("{branch}")
        && substitutions.get("branch").map(String::as_str) == Some("unknown")
    {
        alerts.push(serde_json::json!({
            "severity": "info",
            "title": "Could not determine {branch}",
            "detail": "PZ_TUNNEL uses {branch}, but the daemon could not determine the current git branch, so it materialized the value as \"unknown\". Run the service from a git worktree with a checked-out branch, or use a literal tunnel name."
        }));
    }

    alerts.extend(SUGGESTED_KEYS
        .iter()
        .filter_map(|key| {
            let placeholder = format!("{{{key}}}");
            if domain_template.contains(&placeholder) {
                return None;
            }

            let value = substitutions.get(*key)?;
            if !has_dns_token(domain_template, value) {
                return None;
            }

            Some(serde_json::json!({
                "severity": "info",
                "title": format!("Suggest replacing \"{value}\" with \"{placeholder}\""),
                "detail": format!(
                    "PZ_TUNNEL supports variable substitution: write {placeholder} in the value and the daemon materializes it as \"{value}\" for this process. This keeps tunnel names accurate when the branch, worktree, user, or machine changes."
                ),
            }))
        }));

    alerts
}

fn duplicate_route_alert(domain: &str, duplicate_count: usize) -> serde_json::Value {
    serde_json::json!({
        "severity": "warning",
        "title": "Duplicate tunnel domain",
        "detail": format!(
            "{duplicate_count} tunnels materialized to {domain}. Change PZ_TUNNEL so each active tunnel has a unique domain."
        ),
    })
}

fn append_alert(row: &mut serde_json::Value, alert: serde_json::Value) {
    row.get_mut("alerts")
        .and_then(|alerts| alerts.as_array_mut())
        .expect("route rows always include an alerts array")
        .push(alert);
}

pub(super) fn add_duplicate_route_alerts(
    local_services: &mut [serde_json::Value],
    cloud_routes: &mut [serde_json::Value],
) {
    #[derive(Clone, Copy)]
    enum RouteList {
        Local,
        Cloud,
    }

    let mut routes = Vec::new();
    for (index, row) in local_services.iter().enumerate() {
        if let Some(domain) = row.get("domain").and_then(|v| v.as_str()) {
            routes.push((RouteList::Local, index, domain.to_string()));
        }
    }
    for (index, row) in cloud_routes.iter().enumerate() {
        if let Some(domain) = row.get("domain").and_then(|v| v.as_str()) {
            routes.push((RouteList::Cloud, index, domain.to_string()));
        }
    }

    let mut counts = std::collections::HashMap::new();
    for (_, _, domain) in &routes {
        *counts.entry(domain.clone()).or_insert(0usize) += 1;
    }

    for (list, index, domain) in routes {
        let duplicate_count = counts.get(&domain).copied().unwrap_or(0);
        if duplicate_count < 2 {
            continue;
        }

        let alert = duplicate_route_alert(&domain, duplicate_count);
        match list {
            RouteList::Local => append_alert(&mut local_services[index], alert),
            RouteList::Cloud => append_alert(&mut cloud_routes[index], alert),
        }
    }
}

fn management_registrations_json(
    registrations: &std::collections::HashMap<u32, Vec<PortRegistration>>,
) -> Vec<serde_json::Value> {
    registrations
        .iter()
        .flat_map(|(pid, regs)| {
            regs.iter().map(move |r| {
                serde_json::json!({
                    "pid": pid,
                    "local_port": r.local_port,
                    "domain": r.domain,
                    "feature_stability": "unstable",
                })
            })
        })
        .collect()
}

/// GET / — serve the developer status dashboard as a self-contained HTML shell.
/// The page fetches `/status.json` and renders the UI client-side, refreshing every 3 s.
pub async fn status_ui() -> Html<&'static str> {
    Html(STATUS_UI_HTML)
}

/// GET /login — start browser login and redirect this tab to the cloud auth UI.
pub async fn start_login() -> impl IntoResponse {
    match crate::auth::start_browser_login_session().await {
        Ok(session) => {
            tracing::info!(
                callback_port = session.callback_port,
                "started dashboard browser login"
            );
            Redirect::temporary(&session.auth_url).into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to start dashboard browser login");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to start login flow: {error}"),
            )
                .into_response()
        }
    }
}

/// GET /assets/portzero-mark.jpg — favicon and compact brand mark.
pub async fn portzero_mark_asset() -> impl IntoResponse {
    ([("content-type", "image/jpeg")], PORTZERO_MARK_JPEG)
}

/// GET /assets/portzero-wordmark.jpg — full PortZero wordmark.
pub async fn portzero_wordmark_asset() -> impl IntoResponse {
    ([("content-type", "image/jpeg")], PORTZERO_WORDMARK_JPEG)
}

/// GET /openapi.json — read-only OpenAPI document for local process clients.
pub async fn openapi_json() -> impl IntoResponse {
    use utoipa::OpenApi;
    let spec = crate::management::openapi::ManagementApiDoc::openapi()
        .to_pretty_json()
        .unwrap_or_default();
    ([("content-type", "application/json")], spec)
}

const STATUS_UI_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>PortZero Local</title>
<link rel="icon" type="image/jpeg" href="/assets/portzero-mark.jpg">
<link rel="apple-touch-icon" href="/assets/portzero-mark.jpg">
<style>
:root{color-scheme:light dark;--bg:#fdfdfd;--fg:#1f2328;--muted:#6b7280;--soft:#f3f4f6;--line:#e5e7eb;--accent:#2563eb;--accent-soft:#dbeafe;--ok:#16833a;--warn:#a16207;--bad:#c2410c;--code:#111827;--code-fg:#f9fafb}
@media (prefers-color-scheme:dark){:root{--bg:#0f1115;--fg:#e5e7eb;--muted:#9ca3af;--soft:#171a21;--line:#2b303b;--accent:#7dd3fc;--accent-soft:#102a3a;--ok:#86efac;--warn:#facc15;--bad:#fb923c;--code:#05070a;--code-fg:#f3f4f6}}
*,*::before,*::after{box-sizing:border-box}
html{scroll-behavior:smooth}
body{margin:0;background:var(--bg);color:var(--fg);font-family:ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;font-size:16px;line-height:1.65}
a{color:inherit;text-decoration:none}
a:hover{color:var(--accent)}
code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
.shell{width:min(100% - 32px,1120px);margin:0 auto}
.topbar{position:sticky;top:0;z-index:3;background:color-mix(in srgb,var(--bg) 90%,transparent);backdrop-filter:blur(12px);border-bottom:1px solid var(--line)}
.nav{display:flex;align-items:center;gap:18px;min-height:62px}
.brand{display:inline-flex;align-items:center;gap:12px;font-size:18px;font-weight:800}
.brand img{display:block;height:28px;width:auto}
.navlinks{display:flex;gap:14px;margin-left:auto;color:var(--muted);font-size:14px}
.meta{color:var(--muted);font-size:13px;white-space:nowrap}
.hero{padding:58px 0 24px;display:grid;gap:18px}
.hero-brand{max-width:620px;width:100%;height:auto;display:block}
.eyebrow{color:var(--accent);font-size:13px;font-weight:700;margin:0 0 10px}
h1{font-size:clamp(34px,6vw,64px);line-height:1.05;margin:0 0 18px;max-width:760px}
.lede{max-width:680px;color:var(--muted);font-size:19px;margin:0}
.section{padding:30px 0;border-top:1px solid var(--line)}
.section-head{display:flex;align-items:end;gap:16px;justify-content:space-between;margin-bottom:18px}
h2{font-size:28px;line-height:1.2;margin:0}
.section-note{color:var(--muted);font-size:14px;margin:0}
.steps{display:grid;gap:12px}
.step{display:grid;grid-template-columns:40px minmax(0,1fr);gap:14px;padding:18px 0;border-bottom:1px solid var(--line)}
.step:last-child{border-bottom:0}
.num{display:grid;place-items:center;width:32px;height:32px;border-radius:50%;background:var(--accent-soft);color:var(--accent);font-weight:800;font-size:14px}
.step h3,.example h3{font-size:18px;margin:0 0 8px}
.placeholder{color:var(--muted);margin:0}
.code-row{margin-top:12px;background:var(--code);color:var(--code-fg);border-radius:8px;padding:12px 14px;overflow:auto;font-size:14px}
.examples{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:14px}
.example{border-top:2px solid var(--line);padding-top:14px}
.tag{display:inline-flex;align-items:center;border:1px solid var(--line);border-radius:999px;padding:2px 9px;color:var(--muted);font-size:12px;margin-bottom:10px}
.picker{display:flex;align-items:center;gap:10px;flex-wrap:wrap}
.picker select{appearance:none;border:1px solid var(--line);background:var(--bg);color:var(--fg);border-radius:8px;padding:8px 34px 8px 10px;font:inherit;font-size:14px}
.detected{color:var(--muted);font-size:13px}
.status-grid{display:grid;grid-template-columns:minmax(0,1fr);gap:28px}
.status-card{min-width:0}
.status-row{display:flex;align-items:center;gap:8px;color:var(--muted);font-size:14px;margin-bottom:12px}
.dot{width:9px;height:9px;border-radius:50%;background:var(--bad);flex:0 0 auto}
.dot.ok{background:var(--ok)}
.dot.warn{background:var(--warn)}
table{width:100%;border-collapse:collapse;font-size:14px}
th{text-align:left;color:var(--muted);font-weight:600;border-bottom:1px solid var(--line);padding:8px 6px}
td{border-bottom:1px solid var(--line);padding:8px 6px;vertical-align:top;word-break:break-word}
.domain-stack{display:grid;gap:2px}
.domain-stack code{font-size:13px}
.route-alerts{display:grid;gap:6px;margin-top:8px}
.route-alert{border-left:3px solid var(--warn);background:var(--soft);padding:7px 9px;font-size:13px}
.route-alert.info{border-left-color:var(--accent)}
.route-alert.warning{border-left-color:var(--warn)}
.route-alert-title{font-weight:700}
.route-alert-detail{color:var(--muted)}
.substitutions{display:flex;gap:6px;flex-wrap:wrap}
.substitutions code{background:var(--soft);border-radius:999px;padding:2px 7px;color:var(--fg);font-size:12px}
.empty{color:var(--muted);font-size:14px;margin:0}
.empty-action{display:flex;align-items:center;gap:12px;flex-wrap:wrap;color:var(--muted);font-size:14px}
.empty-action p{margin:0}
.button{display:inline-flex;align-items:center;justify-content:center;min-height:36px;border-radius:8px;background:var(--accent);color:var(--bg);font-weight:700;padding:6px 12px}
.button:hover{color:var(--bg)}
.diag{border-left:3px solid var(--line);padding:10px 0 10px 14px;margin-bottom:10px}
.diag-critical,.diag-error{border-color:var(--bad)}
.diag-warning{border-color:var(--warn)}
.diag-info{border-color:var(--accent)}
.diag-title{font-weight:700}
.diag-detail,.diag-fix{color:var(--muted);font-size:14px}
.diag-fix code{background:var(--soft);color:var(--fg);padding:1px 5px;border-radius:4px}
.no-issues{color:var(--ok);font-size:14px;margin:0}
.loading{color:var(--muted);padding:32px 0}
.settings label{display:flex;align-items:center;gap:8px;margin:6px 0;font-size:14px;cursor:pointer}
.settings input[type=checkbox]{accent-color:var(--accent)}
.hint{color:var(--muted);font-size:12px;margin-top:4px}
.endpoint-grid{display:grid;grid-template-columns:minmax(0,.9fr) minmax(0,1.1fr);gap:22px;align-items:start}
.endpoint-list{display:grid;gap:12px}
.endpoint{border-top:1px solid var(--line);padding-top:12px}
.endpoint code{display:block;color:var(--fg);font-size:15px;margin-top:4px;word-break:break-word}
.method{display:inline-flex;align-items:center;border-radius:4px;padding:1px 6px;background:var(--soft);font-weight:800;font-size:12px;color:var(--accent);margin-right:6px}
.api-note{color:var(--muted);margin:0 0 14px}
.spec-block{max-height:520px;overflow:auto;background:var(--code);color:var(--code-fg);border-radius:8px;padding:14px;font-size:12px;line-height:1.5}
.spec-block code{white-space:pre}
@media (prefers-reduced-motion:no-preference){.hero-brand,.brand img{animation:fade-in .45s ease-out both}}
@keyframes fade-in{from{opacity:0;transform:translateY(6px)}to{opacity:1;transform:translateY(0)}}
@media (max-width:760px){.nav{align-items:flex-start;flex-direction:column;gap:6px;padding:12px 0}.navlinks{margin-left:0;flex-wrap:wrap}.hero{padding-top:36px}.examples,.status-grid{grid-template-columns:1fr}.section-head{display:block}.step{grid-template-columns:1fr}.num{margin-bottom:4px}}
@media (max-width:860px){.endpoint-grid{grid-template-columns:1fr}}
</style>
</head>
<body>
<header class="topbar">
  <nav class="shell nav" aria-label="Main">
    <a class="brand" href="/">
      <img src="/assets/portzero-mark.jpg" alt="" aria-hidden="true">
      <span>PortZero</span>
    </a>
    <div class="navlinks">
      <a href="#getting-started">Getting Started</a>
      <a href="#api">API</a>
      <a href="#examples">Examples</a>
      <a href="#tunnels">Tunnels</a>
      <a href="#issues">Issues</a>
    </div>
    <span class="meta" id="hdr-meta"></span>
  </nav>
</header>
<main class="shell">
  <section class="hero" id="getting-started">
    <h1>Getting Started</h1>
    <p class="lede">Use <code>portzero.local</code> for the local dashboard. Local processes should call the management API at <code>http://api.portzero.local</code>.</p>
  </section>

  <section class="section" aria-labelledby="quickstart-title">
    <div class="section-head">
      <h2 id="quickstart-title">Quickstart</h2>
      <p class="section-note">Short steps first. Details can live below.</p>
    </div>
    <div class="steps" id="quickstart-steps">
      <p class="loading">Detecting local setup...</p>
    </div>
  </section>

  <section class="section" id="api" aria-labelledby="api-title">
    <div class="section-head">
      <h2 id="api-title">Local API</h2>
      <p class="section-note">For local processes only. This page does not send API requests.</p>
    </div>
    <div class="endpoint-grid">
      <div>
        <p class="api-note">Endpoint base URL:</p>
        <pre class="code-row"><code>http://api.portzero.local</code></pre>
        <div class="endpoint-list" aria-label="API routes">
          <div class="endpoint">
            <span class="method">POST</span><strong>Register ports</strong>
            <code>/v1/register</code>
          </div>
          <div class="endpoint">
            <span class="method">DELETE</span><strong>Deregister ports</strong>
            <code>/v1/register</code>
          </div>
          <div class="endpoint">
            <span class="method">GET</span><strong>Process registration status</strong>
            <code>/v1/status</code>
          </div>
        </div>
      </div>
      <div>
        <p class="api-note">OpenAPI 3.1 spec. The raw document is also available at <code>/openapi.json</code>.</p>
        <pre class="spec-block" aria-label="OpenAPI specification"><code id="openapi-spec-code">Loading...</code></pre>
      </div>
    </div>
  </section>

  <section class="section" id="examples" aria-labelledby="examples-title">
    <div class="section-head">
      <h2 id="examples-title">Examples</h2>
      <div class="picker">
        <label class="section-note" for="language-picker">Language</label>
        <select id="language-picker"></select>
        <span class="detected" id="language-detected"></span>
      </div>
    </div>
    <div class="examples" id="language-examples"></div>
  </section>

  <section class="section" id="tunnels" aria-labelledby="tunnels-title">
    <div class="section-head">
      <h2 id="tunnels-title">Tunnels</h2>
      <p class="section-note" id="hdr-ts"></p>
    </div>
    <div id="status-root"><p class="loading">Connecting to daemon...</p></div>
  </section>
</main>
<script>
let selectedLanguage=null;
let languagePickerReady=false;
let latestDashboardData=null;
function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;')}
function dot(ok){return '<span class="dot '+(ok?'ok':'')+'"></span>'}
function exampleCommand(ex,os){
  const commands=ex.commands||{};
  return commands[os]||commands.linux||commands.macos||commands.windows||'';
}
function exampleUrl(ex){
  return 'http://'+String(ex.domain||'').replace(/:80$/,'')+'/';
}
function sortedRelevantExamples(d,languageId){
  const examples=((d.getting_started||{}).examples||[]).filter(function(ex){return ex.language===languageId;});
  const dockerInstalled=!!(((d.environment||{}).docker||{}).installed);
  return examples.filter(function(ex){return dockerInstalled||!ex.requires_docker;});
}
function manifestLanguageIds(d){
  const ids={};
  ((d.getting_started||{}).examples||[]).forEach(function(ex){ids[ex.language]=true;});
  return ids;
}
function renderQuickstart(d){
  const root=document.getElementById('quickstart-steps');
  if(!root) return;
  const env=d.environment||{};
  const os=(env.os&&env.os.id)||'linux';
  const osLabel=(env.os&&env.os.label)||os;
  const languageId=selectedLanguage||(d.languages&&d.languages.default)||'rust';
  const examples=sortedRelevantExamples(d,languageId);
  const ex=examples[0]||(((d.getting_started||{}).examples||[]).filter(function(item){return item.language===languageId;})[0]);
  if(!ex){
    root.innerHTML='<p class="empty">No checked-in examples are available for the selected language yet.</p>';
    return;
  }
  const command=exampleCommand(ex,os);
  const url=exampleUrl(ex);
  root.innerHTML=[
    '<article class="step"><span class="num">1</span><div><h3>Clone the examples</h3><p class="placeholder">Use the separate examples repository.</p><pre class="code-row"><code>git clone https://github.com/PortZeroNetwork/portzero-examples.git\ncd portzero-examples</code></pre></div></article>',
    '<article class="step"><span class="num">2</span><div><h3>Run '+esc(ex.title||ex.id)+'</h3><p class="placeholder">Detected '+esc(osLabel)+'. '+(ex.requires_docker?'Docker with Compose is required for this example.':'This example runs as a local process.')+'</p><pre class="code-row"><code>cd '+esc(ex.path)+'\n'+esc(command)+'</code></pre></div></article>',
    '<article class="step"><span class="num">3</span><div><h3>Open the local URL</h3><p class="placeholder">The daemon detects <code>PZ_TUNNEL</code> and routes the local name.</p><pre class="code-row"><code>'+esc(url)+'</code></pre></div></article>'
  ].join('');
}
function renderLanguageExamples(id){
  const picker=document.getElementById('language-picker');
  if(picker&&picker.value!==id) picker.value=id;
  selectedLanguage=id;
  const d=latestDashboardData||{};
  const env=d.environment||{};
  const os=(env.os&&env.os.id)||'linux';
  const all=((d.getting_started||{}).examples||[]).filter(function(ex){return ex.language===id;});
  const examples=sortedRelevantExamples(d,id);
  const dockerInstalled=!!((env.docker||{}).installed);
  const root=document.getElementById('language-examples');
  if(!all.length){
    root.innerHTML='<p class="empty">No checked-in examples are available for this language yet.</p>';
    renderQuickstart(d);
    return;
  }
  let html=examples.map(function(ex){
    return '<article class="example"><span class="tag">'+esc(ex.variant_label||ex.variant)+'</span><h3>'+esc(ex.title||ex.id)+'</h3><p class="placeholder">'+(ex.requires_docker?'Docker Compose':'Local process')+'</p><pre class="code-row"><code>cd '+esc(ex.path)+'\n'+esc(exampleCommand(ex,os))+'</code></pre><p class="placeholder">Open <code>'+esc(exampleUrl(ex))+'</code></p></article>';
  }).join('');
  if(!dockerInstalled&&all.some(function(ex){return ex.requires_docker;})){
    html+='<article class="example"><span class="tag">Docker</span><h3>Docker Compose examples hidden</h3><p class="placeholder">Install and start Docker with Compose to show Docker examples.</p></article>';
  }
  root.innerHTML=html;
  renderQuickstart(d);
}
function renderLanguagePicker(languages){
  if(!languages||!languages.items) return;
  const exampleLanguages=manifestLanguageIds(latestDashboardData||{});
  if(!selectedLanguage){
    const detectedWithExamples=languages.items.find(function(lang){return lang.installed&&exampleLanguages[lang.id];});
    const firstWithExamples=languages.items.find(function(lang){return exampleLanguages[lang.id];});
    selectedLanguage=(detectedWithExamples||firstWithExamples||{}).id||languages.default||'typescript';
  }
  const picker=document.getElementById('language-picker');
  if(!languagePickerReady){
    picker.innerHTML=languages.items.map(function(lang){
      return '<option value="'+esc(lang.id)+'">'+esc(lang.label)+(lang.installed?'':'')+'</option>';
    }).join('');
    picker.addEventListener('change',function(){renderLanguageExamples(picker.value);});
    languagePickerReady=true;
  }
  const current=languages.items.find(function(lang){return lang.id===selectedLanguage;});
  const detected=languages.items.filter(function(lang){return lang.installed;}).map(function(lang){return lang.label;});
  document.getElementById('language-detected').textContent=detected.length?'Detected: '+detected.slice(0,4).join(', '):'Defaulting to Node.js / TypeScript';
  renderLanguageExamples(current?selectedLanguage:(languages.default||'typescript'));
}
function renderSubstitutions(values){
  const entries=Object.entries(values||{});
  if(!entries.length) return '<span class="empty">none detected</span>';
  return '<div class="substitutions">'+entries.map(function(pair){
    return '<code>{'+esc(pair[0])+'}='+esc(pair[1])+'</code>';
  }).join('')+'</div>';
}
function domainCell(row){
  const domain=row.link_url?'<a href="'+esc(row.link_url)+'"><strong>'+esc(row.domain)+'</strong></a>':'<strong>'+esc(row.domain)+'</strong>';
  let alerts='';
  if(row.alerts&&row.alerts.length){
    alerts='<div class="route-alerts">'+row.alerts.map(function(alert){
      const sev=(alert.severity||'warning').toLowerCase();
      return '<div class="route-alert '+esc(sev)+'"><div class="route-alert-title">'+esc(alert.title)+'</div><div class="route-alert-detail">'+esc(alert.detail)+'</div></div>';
    }).join('')+'</div>';
  }
  return '<div class="domain-stack">'+domain+'<code>template: '+esc(row.domain_template||row.domain)+'</code><code>materialized: '+esc(row.domain)+'</code>'+alerts+'</div>';
}

function cloudStatusBadge(status){
  if(status==='pending_review'){
    return '<span title="Awaiting your approval in app.portzero.cloud" style="display:inline-block;padding:1px 8px;border-radius:10px;background:#fdf3e0;color:#a9640a;font-size:12px">🔒 In Review — <a href="https://app.portzero.cloud/reviews" target="_blank" rel="noopener">approve</a></span>';
  }
  if(status==='denied'){
    return '<span style="display:inline-block;padding:1px 8px;border-radius:10px;background:#fde0e0;color:#b02121;font-size:12px">denied</span>';
  }
  return '<span style="display:inline-block;padding:1px 8px;border-radius:10px;background:#e0f5e6;color:#1a7f37;font-size:12px">published</span>';
}

function renderProblem(p){
  const sev=(p.severity||'error').toLowerCase();
  let fixBtn='';
  if(p.needs_login) fixBtn=' <a class="button" href="/login">Log in (still free)</a>';
  let title=esc(p.title);
  // Some issue summaries already spell out "pid N" inline; only append the
  // badge when the title doesn't already mention this pid.
  if(p.pid && p.title.indexOf('pid '+p.pid)===-1) title+=' <span class="diag-pid">(pid '+esc(p.pid)+')</span>';
  let fix='';
  if(p.fix){
    fix='<div class="diag-fix">'+esc(p.fix);
    if(p.fix_command) fix+=' - <code>'+esc(p.fix_command)+'</code>';
    fix+=fixBtn+'</div>';
  } else if(fixBtn){
    fix='<div class="diag-fix">'+fixBtn+'</div>';
  }
  const detail=p.detail?'<div class="diag-detail">'+esc(p.detail)+'</div>':'';
  return '<div class="diag diag-'+sev+'"><div class="diag-title"><span class="sev-'+sev+'">['+sev.toUpperCase()+']</span> '+title+'</div>'
    +detail+fix+'</div>';
}

function render(d){
  latestDashboardData=d;
  document.getElementById('hdr-meta').textContent=d.daemon_pid?'pid '+d.daemon_pid:'local daemon';
  document.getElementById('hdr-ts').textContent='Updated '+new Date().toLocaleTimeString();
  renderLanguagePicker(d.languages);
  renderQuickstart(d);

  let html='';

  html+='<div class="status-grid">';
  html+='<section class="status-card"><h3>Local tunnels</h3>';
  html+='<div class="status-row">'+dot(d.overlay_active)+(d.overlay_active?'active':'inactive')+'</div>';
  if(d.local_services&&d.local_services.length>0){
    html+='<table><thead><tr><th>domain</th><th>substitutions</th><th>real addr</th><th>port</th><th>pid</th></tr></thead><tbody>';
    d.local_services.forEach(function(s){
      html+='<tr><td>'+domainCell(s)+'</td><td>'+renderSubstitutions(s.substitutions)+'</td><td>'+esc(s.real_addr)+'</td><td>'+esc(s.service_port)+'</td><td>'+esc(s.pid)+'</td></tr>';
    });
    html+='</tbody></table>';
  } else {
    html+='<p class="empty">no local services</p>';
  }
  html+='</section>';

  html+='<section class="status-card"><h3>Cloud tunnels</h3>';
  html+='<div class="status-row">'+dot(d.cloud_connected)+(d.cloud_connected?'connected':'disconnected');
  if(d.cloud_plan) html+=' <span style="font-size:13px">['+esc(d.cloud_plan)+']</span>';
  if(d.cloud_error) html+=' <span style="color:var(--bad);font-size:13px">- '+esc(d.cloud_error)+'</span>';
  html+='</div>';
  if(d.auth_authenticated&&d.cloud_can_use_tunnels===false){
    html+='<div style="margin:6px 0;padding:10px 12px;border:1px solid #e6a23c;background:#fef3e6;font-size:13px;">'
      + '<p style="margin:0 0 8px;font-weight:700;">Upgrade to use Cloud tunnels</p>'
      + '<p style="margin:0 0 8px;">Your account is on the free plan and isn\'t a member of any paid group, so Cloud tunnels aren\'t available yet. '
      + '<a href="https://portzero.net/#pricing" target="_blank" rel="noopener">See pricing</a> to upgrade.</p>'
      + '<p style="margin:0 0 8px;">Cloud tunnels are configured just like local tunnels, except <code>PZ_TUNNEL</code> has a <code>.cloud</code> domain name — then they\'re available to any internet-connected device:</p>'
      + '<pre class="code-row" style="margin:0;"><code>PZ_TUNNEL={branch}.mytodoapp.portzero.local:80            # Local tunnel\nPZ_TUNNEL={branch}.mytodoapp.&lt;username&gt;.portzero.cloud:80 # Cloud tunnel</code></pre>'
      + '</div>';
  }
  if(d.cloud_message){
    html+='<div style="margin:6px 0;padding:6px 8px;border:1px solid #e6a23c;background:#fef3e6;font-size:13px;">'
      + esc(d.cloud_message)
      + ' <a href="https://app.portzero.cloud" target="_blank" rel="noopener">Upgrade</a></div>';
  }
  if(d.cloud_routes&&d.cloud_routes.length>0){
    html+='<table><thead><tr><th>domain</th><th>status</th><th>substitutions</th><th>port</th><th>pid</th></tr></thead><tbody>';
    d.cloud_routes.forEach(function(r){
      html+='<tr><td>'+domainCell(r)+'</td><td>'+cloudStatusBadge(r.status)+'</td><td>'+renderSubstitutions(r.substitutions)+'</td><td>'+esc(r.port)+'</td><td>'+esc(r.pid)+'</td></tr>';
    });
    html+='</tbody></table>';
  } else {
    if(!d.auth_authenticated){
      html+='<div class="empty-action"><p>No cloud tunnels because you are not logged in.</p><a class="button" href="/login">Log in</a></div>';
    } else {
      html+='<p class="empty">no cloud tunnels</p>';
    }
  }
  html+='</section>';
  html+='</div>';

  html+='<section class="section"><div class="section-head"><h2>API registrations</h2><span class="section-note">unstable feature</span></div>';
  if(d.management_registrations&&d.management_registrations.length>0){
    html+='<table><thead><tr><th>pid</th><th>local port</th><th>domain</th><th>stability</th></tr></thead><tbody>';
    d.management_registrations.forEach(function(r){
      html+='<tr><td>'+esc(r.pid)+'</td><td>'+esc(r.local_port)+'</td><td>'+esc(r.domain)+'</td><td>'+esc(r.feature_stability||'unstable')+'</td></tr>';
    });
    html+='</tbody></table>';
  } else {
    html+='<p class="empty">none</p>';
  }
  html+='</section>';

  // HTTPS policy editor (config change)
  html+='<section class="section"><div class="section-head"><h2>HTTPS settings</h2><span class="section-note">for *.portzero.local</span></div>';
  html+='<div class="settings" id="https-settings">';
  const hp = (d.https_policy || {});
  html += '<label><input type="checkbox" id="https-enable-80" '+(hp.enable_for_port_80?'checked':'')+'> Enable HTTPS (port 443) for backends on port 80</label>';
  html += '<div class="hint" style="margin:2px 0 8px 20px">We add the CA cert to system trust stores and browser NSS dbs on install for as many places as possible (best-effort). PRs welcome to improve coverage. If it does not fully work, consider Cloud Tunnels (paid) which always use https that works in all browsers.</div>';
  html += '<label><input type="checkbox" id="https-redirect-80" '+(hp.redirect_port_80?'checked':'')+'> Redirect HTTP port 80 → HTTPS</label>';
  html += '<label><input type="checkbox" id="https-passthrough-443" '+(hp.passthrough_port_443?'checked':'')+'> Passthrough TLS on port 443 when backend speaks TLS</label>';
  html+='<p class="hint">Saved to ~/.portzero/config.toml and applied live within several seconds (new connections; no restart).</p>';
  html+='</div></section>';

  html+='<section class="section" id="issues"><div class="section-head"><h2>Issues</h2></div>';
  if(!d.problems||d.problems.length===0){
    const ran=d.diagnostics_checks_run;
    html+='<p class="no-issues">no issues detected'+(ran?' ('+ran+' checks passed)':'')+'</p>';
  } else {
    d.problems.forEach(function(p){html+=renderProblem(p);});
  }
  html+='</section>';

  document.getElementById('status-root').innerHTML=html;

  bindHttpsControls(d);
}

function bindHttpsControls(d){
  const ids = ['https-enable-80','https-redirect-80','https-passthrough-443'];
  const keys = ['enable_for_port_80','redirect_port_80','passthrough_port_443'];
  ids.forEach(function(id, i){
    const el = document.getElementById(id);
    if(!el) return;
    // avoid stacking by replacing listener via clone (simple)
    const fresh = el.cloneNode(true);
    el.parentNode.replaceChild(fresh, el);
    fresh.addEventListener('change', async function(){
      const patch = {};
      patch[keys[i]] = fresh.checked;
      try{
        const res = await fetch('/v1/config/https',{
          method:'PUT',
          headers:{'Content-Type':'application/json'},
          body: JSON.stringify(patch)
        });
        if(!res.ok){
          console.warn('https policy update failed', res.status);
        }
      }catch(e){ console.warn('https update error', e); }
      // refresh data shortly
      setTimeout(load, 200);
    });
  });
}

async function load(){
  try{
    const r=await fetch('/status.json');
    if(r.ok) render(await r.json());
  }catch(e){
    document.getElementById('status-root').innerHTML='<p class="loading">Waiting for daemon...</p>';
  }
}
async function loadSpec(){
  try{
    const r=await fetch('/openapi.json');
    if(r.ok) document.getElementById('openapi-spec-code').textContent=await r.text();
  }catch(e){
    // spec-block keeps its "Loading..." placeholder
  }
}
load();
loadSpec();
setInterval(load,3000);
</script>
</body>
</html>"##;

/// GET /status.json — machine-readable snapshot of all daemon state.
pub async fn status_json(State(state): State<AppState>) -> Json<serde_json::Value> {
    let daemon_pid = read_daemon_pid(&state.state_dir);
    let overlay = read_overlay_state(&state.state_dir);
    let routes = read_route_table(&state.state_dir);
    let (cloud_connected, cloud_error, cloud_plan, cloud_can_use_tunnels, cloud_message) =
        read_cloud_state(&state.state_dir);
    let auth_authenticated = crate::auth::AuthConfig::load().is_authenticated();
    let diagnostics = crate::diagnostics::load_report(&state.state_dir);
    let issues = read_issues(&state.state_dir);

    let registrations_guard = state.store.read().await;
    let management_registrations = management_registrations_json(&registrations_guard);

    let mut local_services: Vec<serde_json::Value> = overlay
        .routes
        .iter()
        .map(|r| {
            let domain_template = if r.domain_template.is_empty() {
                &r.domain
            } else {
                &r.domain_template
            };
            serde_json::json!({
                "domain": r.domain,
                "domain_template": domain_template,
                "substitutions": display_substitutions(&r.substitutions, &r.source),
                "alerts": substitution_alerts(domain_template, &r.substitutions),
                "real_addr": r.real_addr,
                "service_port": r.service_port,
                "health_path": r.health_path,
                "link_url": local_service_link_url(&r.domain, r.service_port),
                "pid": r.pid,
            })
        })
        .collect();

    let cloud_route_statuses = read_cloud_route_statuses(&state.state_dir);
    let mut cloud_routes: Vec<serde_json::Value> = routes
        .routes
        .values()
        .map(|r| {
            let domain_template = if r.domain_template.is_empty() {
                &r.domain
            } else {
                &r.domain_template
            };
            // Review status ("pending_review"/"published"/"denied"); default to
            // "published" for local overlay routes that never carry a status.
            let status = cloud_route_statuses
                .get(&r.domain)
                .map(String::as_str)
                .unwrap_or("published");
            serde_json::json!({
                "domain": r.domain,
                "domain_template": domain_template,
                "substitutions": display_substitutions(&r.substitutions, &r.source),
                "alerts": substitution_alerts(domain_template, &r.substitutions),
                "port": r.port,
                "health_path": r.health_path,
                "pid": r.pid,
                "status": status,
            })
        })
        .collect();

    add_duplicate_route_alerts(&mut local_services, &mut cloud_routes);

    // A single ranked list of problems, combining issues.json (passively
    // detected duplicate names / port conflicts / misconfigured tunnels) and
    // diagnostics.json (the on-demand Binary/Network/DNS/TLS/Auth/System
    // health-check report) — the same list the tray renders from, so
    // "issues" and "diagnostics" don't read as two different things.
    let problems_json: Vec<serde_json::Value> =
        crate::notify::collect_problems(&issues, diagnostics.as_ref())
            .iter()
            .map(|p| serde_json::to_value(p).unwrap_or(serde_json::Value::Null))
            .collect();
    let diagnostics_checks_run = diagnostics.as_ref().map(|r| r.checks_run);

    let https_policy = crate::discovery_loop::DaemonConfig::load().overlay_https;

    Json(serde_json::json!({
        "daemon_pid": daemon_pid,
        "overlay_active": overlay.overlay_active,
        "auth_authenticated": auth_authenticated,
        "local_services": local_services,
        "cloud_connected": cloud_connected,
        "cloud_error": cloud_error,
        "cloud_plan": cloud_plan,
        "cloud_can_use_tunnels": cloud_can_use_tunnels,
        "cloud_message": cloud_message,
        "cloud_routes": cloud_routes,
        "management_registrations": management_registrations,
        "management_registrations_feature_stability": "unstable",
        "problems": problems_json,
        "diagnostics_checks_run": diagnostics_checks_run,
        "languages": detected_languages(),
        "environment": detected_environment(),
        "getting_started": getting_started_manifest(),
        "https_policy": {
            "enable_for_port_80": https_policy.enable_for_port_80,
            "redirect_port_80": https_policy.redirect_port_80,
            "passthrough_port_443": https_policy.passthrough_port_443,
        },
    }))
}
