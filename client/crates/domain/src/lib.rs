//! Domain template engine for PZ_TUNNEL support.
//!
//! Resolves templates like `{service}-{project}-{branch}.{cloud-username}.tunnel.portzero.cloud`
//! into stable DNS-safe domain names for tunnel routes.
//!
//! There are two distinct username placeholders, and they are never
//! interchangeable:
//!
//! - `{local-username}` — the OS login username. Always available, and never
//!   changes based on whether you are logged in to Port Zero Cloud. Using it
//!   in a `.local` tunnel template lets you scope a tunnel name per-machine
//!   without that name shifting the moment you run `portzero login`.
//! - `{cloud-username}` — the Port Zero Cloud account username. Only
//!   available when logged in. Cloud tunnel routes live under
//!   `*.<cloud-username>.tunnel.portzero.cloud`.
//!
//! `{local-username}` must never be used in a cloud tunnel template — cloud
//! tunnels are already scoped by `{cloud-username}`, and allowing the OS
//! username in as well would break the orthogonality between `.local` and
//! `.cloud` tunnel names. `{cloud-username}` may be used in a `.local`
//! template, but requires being logged in to resolve.
//!
//! The prefix before `.<cloud-username>.tunnel.portzero.cloud` may contain
//! dots to encode namespaces (e.g. `api.team.alice.tunnel.portzero.cloud`);
//! each dot-separated segment must be a valid DNS label (ASCII alphanumeric +
//! hyphens, no leading/trailing hyphens, ≤ 63 chars).

use std::path::{Path, PathBuf};

/// Default base domain for portzero.cloud tunnels. The portzero-cloud edge only
/// accepts tunnel routes ending in this suffix.
pub const DEFAULT_BASE_DOMAIN: &str = "tunnel.portzero.cloud";

/// Default domain template used when none is specified.
///
/// Produces a cloud-username-scoped hostname under
/// `*.<cloud-username>.tunnel.portzero.cloud`. The base domain can be
/// overridden via `PZ_TUNNEL_BASE_DOMAIN` for local development.
pub const DEFAULT_TEMPLATE: &str =
    "{service}-{project}-{branch}.{cloud-username}.tunnel.portzero.cloud";

/// Build the default domain template using the configured base domain.
///
/// Reads `PZ_TUNNEL_BASE_DOMAIN` from the environment, falling back to
/// `tunnel.portzero.cloud`.
pub fn default_template() -> String {
    let base =
        std::env::var("PZ_TUNNEL_BASE_DOMAIN").unwrap_or_else(|_| DEFAULT_BASE_DOMAIN.to_string());
    format!("{{service}}-{{project}}-{{branch}}.{{cloud-username}}.{base}")
}

/// Context for resolving PZ_TUNNEL templates.
#[derive(Debug, Clone)]
pub struct DomainContext {
    pub service: String,
    pub project: String,
    pub branch: String,
    pub worktree: Option<String>,
    pub user: String,
    pub machine: String,
    /// Short account identifier (first 8 chars of account UUID).
    /// Falls back to the OS username when not logged in.
    pub uid: Option<String>,
    /// Cloud account username (e.g. "alice"). Used for `{cloud-username}` in
    /// namespace-aware tunnel domains like
    /// `{service}.{cloud-username}.tunnel.portzero.cloud`.
    ///
    /// `None` when not logged in. Unlike the old unified `{username}`
    /// placeholder, this deliberately has no fallback — resolving
    /// `{cloud-username}` without an account would silently change a tunnel's
    /// identity depending on login state, which is exactly what the
    /// `{local-username}`/`{cloud-username}` split exists to prevent.
    pub username: Option<String>,
    /// Pull-request number for `{pr}`, from the CI environment (GitHub Actions
    /// `GITHUB_REF=refs/pull/<n>/merge`, or an explicit `PZ_PR_NUMBER`). `None`
    /// outside a pull request — resolving `{pr}` then fails discovery for that
    /// tunnel with a clear diagnostic rather than producing a garbled name.
    pub pr: Option<String>,
    /// CI run id for `{run-id}`, from GitHub Actions `GITHUB_RUN_ID`. `None`
    /// outside Actions.
    pub run_id: Option<String>,
}

impl DomainContext {
    /// Build context from the current environment.
    ///
    /// - `service_name`: compose service name or directory name
    /// - `project_dir`: path to the project root (git repo root)
    /// - `account_id`: authenticated account UUID (used for `{uid}`)
    /// - `username`: cloud account username (used for `{cloud-username}`)
    ///
    /// Branch and worktree detection failures are silently treated as
    /// "unknown" so the daemon keeps running even outside a git repo.
    pub fn from_environment(
        service_name: &str,
        project_dir: &Path,
        account_id: Option<&str>,
        username: Option<&str>,
    ) -> Self {
        let project = detect_project_name(project_dir);
        let branch = detect_branch(project_dir).unwrap_or_else(|_| "unknown".to_string());
        let worktree = detect_worktree_name(project_dir);
        let user = detect_user();
        let machine = detect_machine();
        let uid = account_id.map(|id| {
            id.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect::<String>()
                .to_ascii_lowercase()
        });

        Self {
            service: service_name.to_string(),
            project,
            branch,
            worktree,
            user,
            machine,
            uid,
            username: username.map(|u| u.to_string()),
            pr: detect_pr(),
            run_id: detect_run_id(),
        }
    }

    /// Build context from an optional project directory.
    ///
    /// When no project directory is available, git-derived values stay
    /// `"unknown"`/`None` instead of consulting the caller's current directory.
    pub fn from_optional_environment(
        service_name: &str,
        project_dir: Option<&Path>,
        account_id: Option<&str>,
        username: Option<&str>,
    ) -> Self {
        match project_dir {
            Some(project_dir) => {
                Self::from_environment(service_name, project_dir, account_id, username)
            }
            None => {
                let user = detect_user();
                let machine = detect_machine();
                let uid = account_id.map(|id| {
                    id.chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .take(8)
                        .collect::<String>()
                        .to_ascii_lowercase()
                });

                Self {
                    service: service_name.to_string(),
                    project: "unknown".to_string(),
                    branch: "unknown".to_string(),
                    worktree: None,
                    user,
                    machine,
                    uid,
                    username: username.map(|u| u.to_string()),
                    pr: detect_pr(),
                    run_id: detect_run_id(),
                }
            }
        }
    }

    /// Resolve a domain template, replacing all `{placeholder}` variables.
    ///
    /// All values are DNS-sanitized before substitution.
    ///
    /// Supported variables: `{service}`, `{project}`, `{branch}`, `{worktree}`,
    /// `{user}`, `{machine}`, `{uid}`, `{local-username}`, `{cloud-username}`.
    ///
    /// `{local-username}` always resolves to the OS username and never
    /// changes based on login state. `{cloud-username}` resolves to the
    /// logged-in cloud account username, or an empty string when not logged
    /// in — callers should validate placeholder usage with
    /// [`validate_username_placeholders`] before resolving so that misuse
    /// (or a missing login) surfaces as a diagnostic instead of a silently
    /// broken domain.
    pub fn resolve(&self, template: &str) -> String {
        let worktree_value = self.worktree.as_deref().unwrap_or(&self.branch);
        let uid_value = self.uid.as_deref().unwrap_or(&self.user);
        let cloud_username_value = self.username.as_deref().unwrap_or("");

        let mut resolved = template
            .replace("{service}", &sanitize_for_dns(&self.service))
            .replace("{project}", &sanitize_for_dns(&self.project))
            .replace("{branch}", &sanitize_for_dns(&self.branch))
            .replace("{worktree}", &sanitize_for_dns(worktree_value))
            .replace("{user}", &sanitize_for_dns(&self.user))
            .replace("{machine}", &sanitize_for_dns(&self.machine))
            .replace("{uid}", &sanitize_for_dns(uid_value))
            .replace("{local-username}", &sanitize_for_dns(&self.user))
            .replace("{cloud-username}", &sanitize_for_dns(cloud_username_value));

        // `{pr}` and `{run-id}` come from the CI environment and are only
        // available inside GitHub Actions (a pull request / a run). When absent
        // we deliberately leave the literal `{pr}` / `{run-id}` in place so that
        // [`unresolved_tokens`] can flag it and discovery fails cleanly with a
        // clear diagnostic — never a garbled name like `web-.example.com`.
        if let Some(pr) = self.pr.as_deref() {
            resolved = resolved.replace("{pr}", &sanitize_for_dns(pr));
        }
        if let Some(run_id) = self.run_id.as_deref() {
            resolved = resolved.replace("{run-id}", &sanitize_for_dns(run_id));
        }

        resolved
    }
}

/// Return every `{...}` token still present in an already-resolved name.
///
/// After [`DomainContext::resolve`] has run, a remaining `{token}` means the
/// template referenced something the environment could not supply (e.g. `{pr}`
/// outside a pull request) or an unknown placeholder. Discovery uses this to
/// fail the tunnel with a clear diagnostic rather than registering a name that
/// still contains braces.
pub fn unresolved_tokens(resolved: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = resolved;
    while let Some(start) = rest.find('{') {
        let after = &rest[start..];
        if let Some(end) = after.find('}') {
            out.push(after[..=end].to_string());
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    out
}

/// Validate that a single DNS label does not contain an ambiguous internal
/// `--`.
///
/// `--` is the reserved hierarchy separator inside a single-label tunnel name
/// (e.g. `{branch}--myapp`). For that to stay unambiguous, each `--`-delimited
/// segment must itself be a clean sub-label: non-empty and not starting or
/// ending with a hyphen. This rejects `a--` / `--a` / `a----b` (empty segment)
/// and `a---b` (a `---` run), while accepting `feat--myapp` and
/// `my-api--web`.
pub fn validate_no_internal_double_hyphen(label: &str) -> Result<(), String> {
    if !label.contains("--") {
        return Ok(());
    }
    for segment in label.split("--") {
        if segment.is_empty() || segment.starts_with('-') || segment.ends_with('-') {
            return Err(format!(
                "label '{label}' has an ambiguous internal '--': '--' separates hierarchy \
                 segments, so each segment must be a non-empty label without a leading or \
                 trailing hyphen (got an empty or hyphen-edged segment)"
            ));
        }
    }
    Ok(())
}

/// Validate the resolved (post-template) domain for token / `--` problems.
///
/// Returns `Err` with a single clear diagnostic when the name still contains an
/// unresolved `{token}` or any dot-separated label carries an ambiguous internal
/// `--`. Suitable for both `.local` overlay names and cloud tunnel domains.
pub fn validate_resolved_name(resolved: &str) -> Result<(), String> {
    let unresolved = unresolved_tokens(resolved);
    if !unresolved.is_empty() {
        return Err(format!(
            "'{resolved}' still contains unresolved template token(s) {}: this value is only \
             available in the right context (e.g. {{pr}} needs a pull request and {{run-id}} \
             needs a GitHub Actions run). Discovery was skipped rather than register a garbled name.",
            unresolved.join(", ")
        ));
    }
    for label in resolved.split('.') {
        validate_no_internal_double_hyphen(label)?;
    }
    Ok(())
}

/// Validate that `{local-username}`/`{cloud-username}` are used correctly for
/// the kind of tunnel `template` targets.
///
/// - `{local-username}` is reserved for `.local` tunnels. Using it in a cloud
///   tunnel template would tie the cloud tunnel's name to this machine's OS
///   username, breaking the orthogonality between `.local` and `.cloud`
///   tunnel names — this is always an error, regardless of login state.
/// - `{cloud-username}` may be used in a `.local` template, but needs an
///   authenticated account to resolve. Note that logging in is only required
///   to resolve the *value*: the `.local` tunnel itself remains free either
///   way — all local tunnels are free, always.
///
/// `is_local` should reflect the template's suffix (`.portzero.local` vs.
/// `*.tunnel.portzero.cloud`), which callers can determine before resolving
/// since the suffix is literal text, never templated.
pub fn validate_username_placeholders(
    template: &str,
    is_local: bool,
    logged_in: bool,
) -> Result<(), String> {
    if !is_local && template.contains("{local-username}") {
        return Err(format!(
            "'{template}' uses {{local-username}} in a cloud tunnel domain — \
             {{local-username}} is reserved for .local tunnels, so that a .local tunnel's \
             name never changes when you run `portzero login`. Use {{cloud-username}} instead."
        ));
    }

    if is_local && template.contains("{cloud-username}") && !logged_in {
        return Err(format!(
            "'{template}' uses {{cloud-username}}, which requires being logged in to resolve. \
             Run `portzero login` to fix this. Note: this is a .local tunnel, and local tunnels \
             are always free — logging in is only needed here to resolve {{cloud-username}}, not \
             because this tunnel will be billed."
        ));
    }

    Ok(())
}

/// Split an optional trailing `:<port>` off a raw `PZ_TUNNEL` value.
///
/// The canonical-port feature lets a developer declare the port the overlay
/// should expose a service on by appending `:<port>` to the value, e.g.
/// `db.portzero.local:5432` or `web-{branch}.portzero.local:8080`.
///
/// This helper is PURE: it only splits the value; it does NOT resolve
/// templates or validate the domain. The returned domain part is what the
/// caller should template-resolve and suffix-classify.
///
/// Rules:
/// - The port is the segment after the LAST `:`. It must parse as an integer
///   in `1..=65535`.
/// - If there is no `:`, or the trailing segment is not a valid port (empty,
///   non-numeric, zero, or out of range), the WHOLE value is treated as the
///   domain and `None` is returned. This is a graceful fallback — discovery
///   must never break on a malformed port.
/// - Templates are preserved in the domain part (the helper does not touch
///   `{...}`); `web-{branch}.portzero.local:8080` → (`web-{branch}.portzero.local`,
///   `Some(8080)`).
pub fn split_tunnel_port(value: &str) -> (&str, Option<u16>) {
    match value.rsplit_once(':') {
        Some((domain, port_str)) => match port_str.parse::<u16>() {
            Ok(port) if port >= 1 => (domain, Some(port)),
            _ => (value, None),
        },
        None => (value, None),
    }
}

/// Validate that `domain` is a legal cloud tunnel subdomain.
///
/// Rules:
/// - Must end with `.tunnel.portzero.cloud` (or the configured base domain).
/// - Must include a cloud username scope, so at least two labels must appear
///   before the base domain (e.g. `api.alice.tunnel.portzero.cloud`).
/// - The prefix before `.<cloud-username>.tunnel.portzero.cloud` may contain dots to
///   encode namespaces (e.g. `api.team.alice.tunnel.portzero.cloud`).
/// - Each dot-separated segment must be a valid DNS label: non-empty, ≤ 63
///   characters, ASCII alphanumeric or hyphens, no leading/trailing hyphens.
/// - Bare base domains like `tunnel.portzero.cloud` or `alice.tunnel.portzero.cloud`
///   are rejected.
///
/// The expected base domain (e.g. `tunnel.portzero.cloud`) is derived from
/// `PZ_TUNNEL_BASE_DOMAIN`.
pub fn validate_tunnel_domain(domain: &str) -> Result<(), String> {
    let base =
        std::env::var("PZ_TUNNEL_BASE_DOMAIN").unwrap_or_else(|_| DEFAULT_BASE_DOMAIN.to_string());
    let suffix = format!(".{base}");

    if domain == base {
        return Err(format!(
            "'{base}' is a reserved hostname — cloud tunnels must be subdomains of it"
        ));
    }

    let scoped = domain.strip_suffix(&suffix).ok_or_else(|| {
        format!(
            "'{domain}' is not a valid cloud tunnel domain: it must end with '.{base}' and be \
             scoped to your cloud username, e.g. 'myservice.<cloud-username>.{base}'. \
             Run `portzero whoami` to find your cloud username."
        )
    })?;

    if scoped.is_empty() {
        return Err(format!(
            "'{base}' is a reserved hostname — cloud tunnels must be subdomains of it"
        ));
    }

    let mut segments: Vec<&str> = scoped.split('.').collect();
    if segments.len() < 2 {
        return Err(format!(
            "'{domain}' is missing the cloud username scope — cloud tunnel domains must be under \
             '*.<cloud-username>.{base}' (e.g. '{scoped}.<cloud-username>.{base}'). \
             Run `portzero whoami` to find your cloud username, or `portzero login` if you are \
             not logged in yet."
        ));
    }

    for segment in segments.drain(..) {
        validate_dns_label(segment)?;
    }

    Ok(())
}

/// Validate a single DNS label (the part between dots).
pub fn validate_dns_label(label: &str) -> Result<(), String> {
    if label.is_empty() {
        return Err("DNS label cannot be empty".to_string());
    }
    if label.len() > 63 {
        return Err(format!(
            "DNS label '{label}' is too long ({} chars, max 63)",
            label.len()
        ));
    }
    if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!(
            "DNS label '{label}' contains invalid characters (only letters, digits, and hyphens allowed)"
        ));
    }
    if label.starts_with('-') || label.ends_with('-') {
        return Err(format!(
            "DNS label '{label}' must not start or end with a hyphen"
        ));
    }
    Ok(())
}

/// Sanitize a string for use as a DNS label.
///
/// - Replaces non-alphanumeric characters with hyphens
/// - Lowercases everything
/// - Collapses consecutive hyphens
/// - Trims leading/trailing hyphens
/// - Truncates to 63 characters (DNS label limit)
pub fn sanitize_for_dns(s: &str) -> String {
    let lowered = s.to_lowercase();

    let replaced: String = lowered
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let mut collapsed = String::with_capacity(replaced.len());
    let mut prev_hyphen = false;
    for c in replaced.chars() {
        if c == '-' {
            if !prev_hyphen {
                collapsed.push(c);
            }
            prev_hyphen = true;
        } else {
            collapsed.push(c);
            prev_hyphen = false;
        }
    }

    let trimmed = collapsed.trim_matches('-');

    let mut result: String = trimmed.chars().take(63).collect();
    while result.ends_with('-') {
        result.pop();
    }

    result
}

// ---------------------------------------------------------------------------
// Variable detection helpers
// ---------------------------------------------------------------------------

/// Walk up from `start` to find the nearest directory containing `.git`.
pub fn find_git_root(start: &Path) -> Option<(&Path, std::path::PathBuf)> {
    let mut dir = start;
    loop {
        let candidate = dir.join(".git");
        if candidate.exists() {
            return Some((dir, candidate));
        }
        dir = dir.parent()?;
    }
}

/// Given a list of host filesystem paths (e.g. from Docker bind mounts or compose
/// working directories), return the first one that is (or is inside) a git repository.
/// This is useful for resolving templates like `{branch}` or `{worktree}` when
/// discovering Docker containers from the host.
pub fn find_git_project_dir(candidates: &[&Path]) -> Option<PathBuf> {
    for c in candidates {
        if let Some((root, _)) = find_git_root(c) {
            return Some(root.to_path_buf());
        }
    }
    None
}

/// Detect the project name from the project directory.
fn detect_project_name(project_dir: &Path) -> String {
    let git_root = find_git_root(project_dir);

    if let Some((root, git_path)) = &git_root {
        if git_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(git_path) {
                if let Some(gitdir) = content.strip_prefix("gitdir:") {
                    let gitdir = gitdir.trim();
                    let gitdir_path = Path::new(gitdir);
                    if let Some(main_git_dir) = gitdir_path
                        .parent()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.parent())
                    {
                        if let Some(name) = main_git_dir.file_name() {
                            return name.to_string_lossy().to_string();
                        }
                    }
                }
            }
        }

        if let Some(name) = root.file_name() {
            return name.to_string_lossy().to_string();
        }
    }

    project_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Detect the current git branch by reading `.git/HEAD` directly.
fn detect_branch(project_dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let (_root, git_path) = find_git_root(project_dir).ok_or_else(|| {
        format!(
            "No .git found in '{}' or any parent directory. \
             Is this inside a git repository?",
            project_dir.display()
        )
    })?;

    let head_path = if git_path.is_file() {
        let content = std::fs::read_to_string(&git_path)?;
        let gitdir = content
            .strip_prefix("gitdir:")
            .ok_or("Invalid .git file: missing 'gitdir:' prefix")?
            .trim()
            .to_string();
        Path::new(&gitdir).join("HEAD")
    } else {
        git_path.join("HEAD")
    };

    let head_content = std::fs::read_to_string(&head_path).map_err(|e| {
        format!(
            "Could not read git HEAD at {}: {}. \
             Is '{}' inside a git repository?",
            head_path.display(),
            e,
            project_dir.display()
        )
    })?;

    if let Some(ref_path) = head_content.strip_prefix("ref: ") {
        let ref_path = ref_path.trim();
        let branch = ref_path.strip_prefix("refs/heads/").unwrap_or(ref_path);
        Ok(branch.to_string())
    } else {
        Ok(head_content.trim().chars().take(12).collect())
    }
}

/// Detect the git worktree name, if we are in a worktree.
fn detect_worktree_name(project_dir: &Path) -> Option<String> {
    let (_root, git_path) = find_git_root(project_dir)?;
    if !git_path.is_file() {
        return None;
    }

    let content = std::fs::read_to_string(&git_path).ok()?;
    let gitdir = content.strip_prefix("gitdir:")?.trim().to_string();
    let gitdir_path = Path::new(&gitdir);

    gitdir_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
}

/// Detect the current OS username.
fn detect_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Detect the pull-request number for `{pr}` from the CI environment.
///
/// GitHub Actions sets `GITHUB_REF=refs/pull/<n>/merge` (or `/head`) on
/// `pull_request` events. As an escape hatch for other CI systems, an explicit
/// `PZ_PR_NUMBER` is also honoured. Returns `None` outside a pull request.
fn detect_pr() -> Option<String> {
    if let Ok(git_ref) = std::env::var("GITHUB_REF") {
        if let Some(rest) = git_ref.strip_prefix("refs/pull/") {
            if let Some(num) = rest.split('/').next() {
                if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                    return Some(num.to_string());
                }
            }
        }
    }
    if let Ok(explicit) = std::env::var("PZ_PR_NUMBER") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Detect the GitHub Actions run id for `{run-id}` from `GITHUB_RUN_ID`.
fn detect_run_id() -> Option<String> {
    std::env::var("GITHUB_RUN_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Detect the machine hostname.
fn detect_machine() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_basic() {
        assert_eq!(sanitize_for_dns("feat/login"), "feat-login");
        assert_eq!(sanitize_for_dns("my_branch.v2"), "my-branch-v2");
        assert_eq!(sanitize_for_dns("Hello World!"), "hello-world");
    }

    #[test]
    fn test_sanitize_collapses_hyphens() {
        assert_eq!(sanitize_for_dns("a//b__c..d"), "a-b-c-d");
    }

    #[test]
    fn test_sanitize_trims_hyphens() {
        assert_eq!(sanitize_for_dns("--foo--"), "foo");
        assert_eq!(sanitize_for_dns("/branch/"), "branch");
    }

    #[test]
    fn test_sanitize_truncates_to_63() {
        let long = "a".repeat(100);
        let result = sanitize_for_dns(&long);
        assert_eq!(result.len(), 63);
    }

    #[test]
    fn test_sanitize_truncation_no_trailing_hyphen() {
        let input = format!("{}/{}", "a".repeat(62), "bbb");
        let result = sanitize_for_dns(&input);
        assert!(!result.ends_with('-'));
        assert!(result.len() <= 63);
    }

    #[test]
    fn test_sanitize_empty() {
        assert_eq!(sanitize_for_dns(""), "");
        assert_eq!(sanitize_for_dns("---"), "");
    }

    #[test]
    fn test_resolve_all_variables() {
        let ctx = DomainContext {
            service: "api".to_string(),
            project: "myapp".to_string(),
            branch: "main".to_string(),
            worktree: Some("wt-feature".to_string()),
            user: "alice".to_string(),
            machine: "dev-box".to_string(),
            uid: Some("abc12345".to_string()),
            username: Some("alice-cloud".to_string()),
            pr: None,
            run_id: None,
        };

        let result = ctx.resolve(
            "{service}-{project}-{branch}-{user}-{uid}.{cloud-username}.tunnel.portzero.cloud",
        );
        assert_eq!(
            result,
            "api-myapp-main-alice-abc12345.alice-cloud.tunnel.portzero.cloud"
        );
    }

    #[test]
    fn test_resolve_uid_falls_back_to_user() {
        let ctx = DomainContext {
            service: "web".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "bob".to_string(),
            machine: "laptop".to_string(),
            uid: None,
            username: None,
            pr: None,
            run_id: None,
        };

        let result = ctx.resolve("{service}-{uid}.{local-username}.tunnel.portzero.cloud");
        assert_eq!(result, "web-bob.bob.tunnel.portzero.cloud");
    }

    #[test]
    fn test_resolve_worktree_falls_back_to_branch() {
        let ctx = DomainContext {
            service: "web".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "bob".to_string(),
            machine: "laptop".to_string(),
            uid: None,
            username: None,
            pr: None,
            run_id: None,
        };

        let result = ctx.resolve("{service}-{worktree}.{local-username}.tunnel.portzero.cloud");
        assert_eq!(result, "web-main.bob.tunnel.portzero.cloud");
    }

    #[test]
    fn test_resolve_sanitizes_values() {
        let ctx = DomainContext {
            service: "my_api".to_string(),
            project: "My App".to_string(),
            branch: "feat/long-branch".to_string(),
            worktree: None,
            user: "Alice.B".to_string(),
            machine: "Dev Box!".to_string(),
            uid: None,
            username: None,
            pr: None,
            run_id: None,
        };

        let result =
            ctx.resolve("{service}-{project}-{branch}-{user}.{local-username}.example.com");
        assert_eq!(
            result,
            "my-api-my-app-feat-long-branch-alice-b.alice-b.example.com"
        );
    }

    #[test]
    fn test_resolve_local_username_ignores_login_state() {
        let ctx = DomainContext {
            service: "api".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "osuser".to_string(),
            machine: "laptop".to_string(),
            uid: Some("abc12345".to_string()),
            username: Some("alice".to_string()),
            pr: None,
            run_id: None,
        };

        // {local-username} always resolves to the OS user, regardless of
        // whether a cloud account is logged in.
        let result = ctx.resolve("{service}.{local-username}.portzero.local");
        assert_eq!(result, "api.osuser.portzero.local");
    }

    #[test]
    fn test_resolve_cloud_username_template() {
        let ctx = DomainContext {
            service: "api".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "osuser".to_string(),
            machine: "laptop".to_string(),
            uid: Some("abc12345".to_string()),
            username: Some("alice".to_string()),
            pr: None,
            run_id: None,
        };

        let result = ctx.resolve("{service}.{cloud-username}.tunnel.portzero.cloud");
        assert_eq!(result, "api.alice.tunnel.portzero.cloud");
    }

    #[test]
    fn test_resolve_cloud_username_empty_when_not_logged_in() {
        let ctx = DomainContext {
            service: "api".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "osuser".to_string(),
            machine: "laptop".to_string(),
            uid: None,
            username: None,
            pr: None,
            run_id: None,
        };

        // No silent fallback to {uid}/{user} — an unresolved {cloud-username}
        // must surface as a diagnostic, not a mystery domain.
        let result = ctx.resolve("{service}.{cloud-username}.tunnel.portzero.cloud");
        assert_eq!(result, "api..tunnel.portzero.cloud");
    }

    // -------------------------------------------------------------------------
    // validate_username_placeholders tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_username_placeholders_local_username_in_cloud_template_errors() {
        let err = validate_username_placeholders(
            "web.{local-username}.tunnel.portzero.cloud",
            false,
            true,
        )
        .unwrap_err();
        assert!(err.contains("{local-username}"), "got: {err}");
        assert!(err.contains("cloud tunnel"), "got: {err}");
    }

    #[test]
    fn test_validate_username_placeholders_local_username_in_local_template_ok() {
        assert!(
            validate_username_placeholders("web.{local-username}.portzero.local", true, false)
                .is_ok()
        );
    }

    #[test]
    fn test_validate_username_placeholders_cloud_username_in_local_template_requires_login() {
        let err =
            validate_username_placeholders("web.{cloud-username}.portzero.local", true, false)
                .unwrap_err();
        assert!(err.contains("logged in"), "got: {err}");
        assert!(
            err.contains("free"),
            "diagnostic must clarify local tunnels stay free, got: {err}"
        );
    }

    #[test]
    fn test_validate_username_placeholders_cloud_username_in_local_template_ok_when_logged_in() {
        assert!(
            validate_username_placeholders("web.{cloud-username}.portzero.local", true, true)
                .is_ok()
        );
    }

    #[test]
    fn test_validate_username_placeholders_cloud_username_in_cloud_template_ok() {
        // Cloud templates already require login structurally (an unresolved
        // {cloud-username} fails validate_tunnel_domain), so this helper
        // doesn't need to special-case it.
        assert!(validate_username_placeholders(
            "web.{cloud-username}.tunnel.portzero.cloud",
            false,
            false
        )
        .is_ok());
    }

    fn ci_ctx(pr: Option<&str>, run_id: Option<&str>) -> DomainContext {
        DomainContext {
            service: "web".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "alice".to_string(),
            machine: "runner".to_string(),
            uid: None,
            username: Some("alice".to_string()),
            pr: pr.map(str::to_string),
            run_id: run_id.map(str::to_string),
        }
    }

    #[test]
    fn test_resolve_pr_and_run_id_tokens() {
        let ctx = ci_ctx(Some("42"), Some("123456"));
        assert_eq!(
            ctx.resolve("web-pr-{pr}.alice.tunnel.portzero.cloud"),
            "web-pr-42.alice.tunnel.portzero.cloud"
        );
        assert_eq!(
            ctx.resolve("web-{run-id}.alice.tunnel.portzero.cloud"),
            "web-123456.alice.tunnel.portzero.cloud"
        );
    }

    #[test]
    fn test_resolve_user_token_still_works() {
        // {user} resolves to the (local) username — unchanged existing behavior.
        let ctx = ci_ctx(None, None);
        assert_eq!(
            ctx.resolve("web-{user}.portzero.local"),
            "web-alice.portzero.local"
        );
    }

    #[test]
    fn test_unresolved_pr_left_literal_then_flagged() {
        // Outside a PR, {pr} stays literal so validate_resolved_name can flag it
        // rather than producing "web-.example.com".
        let ctx = ci_ctx(None, None);
        let resolved = ctx.resolve("web-{pr}.alice.tunnel.portzero.cloud");
        assert_eq!(resolved, "web-{pr}.alice.tunnel.portzero.cloud");
        assert_eq!(unresolved_tokens(&resolved), vec!["{pr}".to_string()]);
        let err = validate_resolved_name(&resolved).unwrap_err();
        assert!(err.contains("{pr}"), "got: {err}");
    }

    #[test]
    fn test_unresolved_run_id_flagged() {
        let ctx = ci_ctx(Some("7"), None);
        let resolved = ctx.resolve("web-{run-id}.alice.tunnel.portzero.cloud");
        assert!(validate_resolved_name(&resolved).is_err());
    }

    #[test]
    fn test_validate_resolved_name_ok_for_clean_names() {
        assert!(validate_resolved_name("web-42.alice.tunnel.portzero.cloud").is_ok());
        assert!(validate_resolved_name("web-main.portzero.local").is_ok());
        // A single, deliberate `--` hierarchy separator is allowed.
        assert!(validate_resolved_name("feat--myapp.alice.tunnel.portzero.cloud").is_ok());
    }

    #[test]
    fn test_validate_no_internal_double_hyphen() {
        assert!(validate_no_internal_double_hyphen("feat--myapp").is_ok());
        assert!(validate_no_internal_double_hyphen("my-api--web").is_ok());
        assert!(validate_no_internal_double_hyphen("plain").is_ok());
        // Ambiguous forms are rejected.
        assert!(validate_no_internal_double_hyphen("feat--").is_err());
        assert!(validate_no_internal_double_hyphen("--myapp").is_err());
        assert!(validate_no_internal_double_hyphen("a----b").is_err());
        assert!(validate_no_internal_double_hyphen("a---b").is_err());
    }

    #[test]
    fn test_validate_resolved_name_rejects_internal_double_hyphen_label() {
        let err = validate_resolved_name("bad--.alice.tunnel.portzero.cloud").unwrap_err();
        assert!(err.contains("--"), "got: {err}");
    }

    #[test]
    fn test_detect_project_name() {
        let name = detect_project_name(Path::new("/home/user/src/myapp"));
        assert_eq!(name, "myapp");
    }

    #[test]
    fn test_optional_context_without_project_dir_stays_unknown() {
        let ctx = DomainContext::from_optional_environment("svc", None, None, None);
        assert_eq!(ctx.project, "unknown");
        assert_eq!(ctx.branch, "unknown");
        assert_eq!(ctx.worktree, None);
    }

    // -------------------------------------------------------------------------
    // validate_tunnel_domain tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_tunnel_domain_valid() {
        assert!(validate_tunnel_domain("myapp.alice.tunnel.portzero.cloud").is_ok());
        assert!(validate_tunnel_domain("api-myapp-main.alice.tunnel.portzero.cloud").is_ok());
        assert!(validate_tunnel_domain("a.b.tunnel.portzero.cloud").is_ok());
        assert!(validate_tunnel_domain("api.alice.tunnel.portzero.cloud").is_ok());
        assert!(validate_tunnel_domain("my-api.alice.tunnel.portzero.cloud").is_ok());
        assert!(validate_tunnel_domain("svc.team-name.alice.tunnel.portzero.cloud").is_ok());
    }

    #[test]
    fn test_validate_tunnel_domain_reserved_bare() {
        let err = validate_tunnel_domain("tunnel.portzero.cloud").unwrap_err();
        assert!(
            err.contains("reserved"),
            "Expected reserved error, got: {err}"
        );
    }

    #[test]
    fn test_validate_tunnel_domain_missing_username_scope() {
        let err = validate_tunnel_domain("myapp.tunnel.portzero.cloud").unwrap_err();
        assert!(
            err.contains("*.<cloud-username>.tunnel.portzero.cloud"),
            "got: {err}"
        );
        assert!(err.contains("portzero whoami"), "got: {err}");
    }

    #[test]
    fn test_validate_tunnel_domain_missing_tunnel_prefix() {
        // e.g. a user drops the `tunnel.` segment entirely: myservice.portzero.cloud
        let err = validate_tunnel_domain("myservice.portzero.cloud").unwrap_err();
        assert!(
            err.contains("tunnel.portzero.cloud"),
            "should mention the expected base domain, got: {err}"
        );
        assert!(
            err.contains("<cloud-username>"),
            "should mention the cloud username scope requirement, got: {err}"
        );
        assert!(err.contains("portzero whoami"), "got: {err}");
    }

    #[test]
    fn test_validate_tunnel_domain_multi_label_valid() {
        assert!(validate_tunnel_domain("api.myapp.alice.tunnel.portzero.cloud").is_ok());
    }

    #[test]
    fn test_validate_tunnel_domain_multi_label_invalid_segment() {
        assert!(validate_tunnel_domain("api._bad.alice.tunnel.portzero.cloud").is_err());
        assert!(validate_tunnel_domain("api..alice.tunnel.portzero.cloud").is_err());
    }

    #[test]
    fn test_validate_tunnel_domain_hyphen_edges() {
        assert!(validate_tunnel_domain("-bad.alice.tunnel.portzero.cloud").is_err());
        assert!(validate_tunnel_domain("bad-.alice.tunnel.portzero.cloud").is_err());
    }

    #[test]
    fn test_validate_tunnel_domain_invalid_chars() {
        assert!(validate_tunnel_domain("my_app.alice.tunnel.portzero.cloud").is_err());
        assert!(validate_tunnel_domain("my app.alice.tunnel.portzero.cloud").is_err());
    }

    #[test]
    fn test_validate_tunnel_domain_label_too_long() {
        let long = format!("{}.alice.tunnel.portzero.cloud", "a".repeat(64));
        assert!(validate_tunnel_domain(&long).is_err());
    }

    // -------------------------------------------------------------------------
    // split_tunnel_port tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_split_tunnel_port_no_port() {
        assert_eq!(
            split_tunnel_port("db.portzero.local"),
            ("db.portzero.local", None)
        );
    }

    #[test]
    fn test_split_tunnel_port_valid() {
        assert_eq!(
            split_tunnel_port("db.portzero.local:5432"),
            ("db.portzero.local", Some(5432))
        );
        // Lowest and highest valid ports.
        assert_eq!(
            split_tunnel_port("x.portzero.local:1"),
            ("x.portzero.local", Some(1))
        );
        assert_eq!(
            split_tunnel_port("x.portzero.local:65535"),
            ("x.portzero.local", Some(65535))
        );
    }

    #[test]
    fn test_split_tunnel_port_templated_name() {
        // The template is preserved in the domain part; only the port is split.
        assert_eq!(
            split_tunnel_port("web-{branch}.portzero.local:8080"),
            ("web-{branch}.portzero.local", Some(8080))
        );
    }

    #[test]
    fn test_split_tunnel_port_cloud_domain() {
        assert_eq!(
            split_tunnel_port("api.alice.tunnel.portzero.cloud:8080"),
            ("api.alice.tunnel.portzero.cloud", Some(8080))
        );
    }

    #[test]
    fn test_split_tunnel_port_out_of_range() {
        // 70000 > u16::MAX -> not a valid port; whole value is the domain.
        assert_eq!(
            split_tunnel_port("db.portzero.local:70000"),
            ("db.portzero.local:70000", None)
        );
    }

    #[test]
    fn test_split_tunnel_port_zero_rejected() {
        // Port 0 is not addressable for a canonical VIP listen port.
        assert_eq!(
            split_tunnel_port("db.portzero.local:0"),
            ("db.portzero.local:0", None)
        );
    }

    #[test]
    fn test_split_tunnel_port_non_numeric() {
        assert_eq!(
            split_tunnel_port("db.portzero.local:abc"),
            ("db.portzero.local:abc", None)
        );
    }

    #[test]
    fn test_split_tunnel_port_empty_after_colon() {
        assert_eq!(
            split_tunnel_port("db.portzero.local:"),
            ("db.portzero.local:", None)
        );
    }

    #[test]
    fn test_split_tunnel_port_empty_value() {
        assert_eq!(split_tunnel_port(""), ("", None));
    }

    #[test]
    fn test_split_tunnel_port_uses_last_colon() {
        // Only the final segment is considered the port.
        assert_eq!(
            split_tunnel_port("a:b.portzero.local:5432"),
            ("a:b.portzero.local", Some(5432))
        );
    }

    // -------------------------------------------------------------------------
    // validate_dns_label edge cases
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_dns_label_exactly_63_chars_accepted() {
        let label = "a".repeat(63);
        assert!(validate_dns_label(&label).is_ok());
    }

    #[test]
    fn test_validate_dns_label_64_chars_rejected() {
        let label = "a".repeat(64);
        let err = validate_dns_label(&label).unwrap_err();
        assert!(err.contains("too long"), "got: {err}");
    }

    #[test]
    fn test_validate_dns_label_empty_rejected() {
        let err = validate_dns_label("").unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    // -------------------------------------------------------------------------
    // unresolved / unknown placeholder detection
    // -------------------------------------------------------------------------

    #[test]
    fn test_unknown_placeholder_left_literal_and_flagged() {
        // {bogus} is not a recognized placeholder, so resolve() leaves it
        // untouched and unresolved_tokens/validate_resolved_name must flag it
        // rather than silently registering a garbled name.
        let ctx = DomainContext {
            service: "web".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "alice".to_string(),
            machine: "laptop".to_string(),
            uid: None,
            username: None,
            pr: None,
            run_id: None,
        };
        let resolved = ctx.resolve("web-{bogus}.alice.tunnel.portzero.cloud");
        assert_eq!(resolved, "web-{bogus}.alice.tunnel.portzero.cloud");
        assert_eq!(unresolved_tokens(&resolved), vec!["{bogus}".to_string()]);
        let err = validate_resolved_name(&resolved).unwrap_err();
        assert!(err.contains("{bogus}"), "got: {err}");
    }

    #[test]
    fn test_unresolved_tokens_multiple() {
        let resolved = "web-{a}-{b}.example.com";
        assert_eq!(
            unresolved_tokens(resolved),
            vec!["{a}".to_string(), "{b}".to_string()]
        );
    }

    #[test]
    fn test_unresolved_tokens_unterminated_brace_ignored() {
        // A stray unmatched '{' with no closing '}' should not be reported as
        // a token (nothing to substitute against) and must not panic/loop.
        assert_eq!(unresolved_tokens("web-{oops.example.com").len(), 0);
    }

    #[test]
    fn test_unresolved_tokens_none() {
        assert!(unresolved_tokens("web-42.alice.tunnel.portzero.cloud").is_empty());
    }

    // -------------------------------------------------------------------------
    // default_template / PZ_TUNNEL_BASE_DOMAIN override
    // -------------------------------------------------------------------------

    #[test]
    fn test_default_template_uses_default_base_domain() {
        // Ensure no leaked override from another test running in-process.
        // SAFETY: test-only env mutation, serialized via `env_lock`.
        let _guard = env_lock();
        std::env::remove_var("PZ_TUNNEL_BASE_DOMAIN");
        assert_eq!(
            default_template(),
            "{service}-{project}-{branch}.{cloud-username}.tunnel.portzero.cloud"
        );
    }

    #[test]
    fn test_default_template_honors_base_domain_override() {
        let _guard = env_lock();
        std::env::set_var("PZ_TUNNEL_BASE_DOMAIN", "tunnel.example.dev");
        let result = default_template();
        std::env::remove_var("PZ_TUNNEL_BASE_DOMAIN");
        assert_eq!(
            result,
            "{service}-{project}-{branch}.{cloud-username}.tunnel.example.dev"
        );
    }

    #[test]
    fn test_validate_tunnel_domain_honors_base_domain_override() {
        let _guard = env_lock();
        std::env::set_var("PZ_TUNNEL_BASE_DOMAIN", "tunnel.example.dev");
        let result = validate_tunnel_domain("myapp.alice.tunnel.example.dev");
        let rejected = validate_tunnel_domain("myapp.alice.tunnel.portzero.cloud");
        std::env::remove_var("PZ_TUNNEL_BASE_DOMAIN");
        assert!(result.is_ok(), "got: {result:?}");
        assert!(
            rejected.is_err(),
            "default suffix should not validate once overridden"
        );
    }

    /// Serializes tests that mutate process-wide env vars (`std::env::set_var`
    /// is process-global, so parallel tests touching `PZ_TUNNEL_BASE_DOMAIN`
    /// or `USER`/`USERNAME`/`GITHUB_REF`/etc. would otherwise race).
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -------------------------------------------------------------------------
    // detect_user / detect_pr / detect_run_id
    // -------------------------------------------------------------------------

    #[test]
    fn test_detect_user_reads_user_env_var() {
        let _guard = env_lock();
        let prev = std::env::var("USER").ok();
        std::env::set_var("USER", "testuser123");
        let result = detect_user();
        match prev {
            Some(v) => std::env::set_var("USER", v),
            None => std::env::remove_var("USER"),
        }
        assert_eq!(result, "testuser123");
    }

    #[test]
    fn test_detect_user_falls_back_to_username_var() {
        let _guard = env_lock();
        let prev_user = std::env::var("USER").ok();
        let prev_username = std::env::var("USERNAME").ok();
        std::env::remove_var("USER");
        std::env::set_var("USERNAME", "winuser");
        let result = detect_user();
        match prev_user {
            Some(v) => std::env::set_var("USER", v),
            None => std::env::remove_var("USER"),
        }
        match prev_username {
            Some(v) => std::env::set_var("USERNAME", v),
            None => std::env::remove_var("USERNAME"),
        }
        assert_eq!(result, "winuser");
    }

    #[test]
    fn test_detect_pr_from_github_ref() {
        let _guard = env_lock();
        let prev = std::env::var("GITHUB_REF").ok();
        std::env::set_var("GITHUB_REF", "refs/pull/42/merge");
        let result = detect_pr();
        match prev {
            Some(v) => std::env::set_var("GITHUB_REF", v),
            None => std::env::remove_var("GITHUB_REF"),
        }
        assert_eq!(result, Some("42".to_string()));
    }

    #[test]
    fn test_detect_pr_from_explicit_override() {
        let _guard = env_lock();
        let prev_ref = std::env::var("GITHUB_REF").ok();
        let prev_pr = std::env::var("PZ_PR_NUMBER").ok();
        std::env::remove_var("GITHUB_REF");
        std::env::set_var("PZ_PR_NUMBER", "99");
        let result = detect_pr();
        match prev_ref {
            Some(v) => std::env::set_var("GITHUB_REF", v),
            None => std::env::remove_var("GITHUB_REF"),
        }
        match prev_pr {
            Some(v) => std::env::set_var("PZ_PR_NUMBER", v),
            None => std::env::remove_var("PZ_PR_NUMBER"),
        }
        assert_eq!(result, Some("99".to_string()));
    }

    #[test]
    fn test_detect_pr_none_outside_ci() {
        let _guard = env_lock();
        let prev_ref = std::env::var("GITHUB_REF").ok();
        let prev_pr = std::env::var("PZ_PR_NUMBER").ok();
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("PZ_PR_NUMBER");
        let result = detect_pr();
        match prev_ref {
            Some(v) => std::env::set_var("GITHUB_REF", v),
            None => std::env::remove_var("GITHUB_REF"),
        }
        match prev_pr {
            Some(v) => std::env::set_var("PZ_PR_NUMBER", v),
            None => std::env::remove_var("PZ_PR_NUMBER"),
        }
        assert_eq!(result, None);
    }

    #[test]
    fn test_detect_pr_non_numeric_ref_ignored() {
        let _guard = env_lock();
        let prev = std::env::var("GITHUB_REF").ok();
        std::env::set_var("GITHUB_REF", "refs/heads/main");
        let result = detect_pr();
        match prev {
            Some(v) => std::env::set_var("GITHUB_REF", v),
            None => std::env::remove_var("GITHUB_REF"),
        }
        assert_eq!(result, None);
    }

    #[test]
    fn test_detect_run_id_from_env() {
        let _guard = env_lock();
        let prev = std::env::var("GITHUB_RUN_ID").ok();
        std::env::set_var("GITHUB_RUN_ID", "123456789");
        let result = detect_run_id();
        match prev {
            Some(v) => std::env::set_var("GITHUB_RUN_ID", v),
            None => std::env::remove_var("GITHUB_RUN_ID"),
        }
        assert_eq!(result, Some("123456789".to_string()));
    }

    #[test]
    fn test_detect_run_id_none_when_absent() {
        let _guard = env_lock();
        let prev = std::env::var("GITHUB_RUN_ID").ok();
        std::env::remove_var("GITHUB_RUN_ID");
        let result = detect_run_id();
        if let Some(v) = prev {
            std::env::set_var("GITHUB_RUN_ID", v);
        }
        assert_eq!(result, None);
    }

    // -------------------------------------------------------------------------
    // find_git_root / detect_branch / detect_worktree_name (filesystem-based)
    // -------------------------------------------------------------------------

    /// Build a throwaway directory under the OS temp dir; caller is
    /// responsible for cleanup via `std::fs::remove_dir_all`.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pz-domain-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn test_find_git_root_finds_dot_git_dir() {
        let root = temp_dir("find-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_git_root(&nested);
        assert!(found.is_some());
        assert_eq!(found.unwrap().0, root.as_path());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_find_git_root_none_when_absent() {
        let root = temp_dir("find-root-none");
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(find_git_root(&nested).is_none());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_detect_branch_reads_head_ref() {
        let root = temp_dir("branch-ref");
        let git_dir = root.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/foo\n").unwrap();

        let branch = detect_branch(&root).unwrap();
        assert_eq!(branch, "feature/foo");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_detect_branch_detached_head_uses_short_sha() {
        let root = temp_dir("branch-detached");
        let git_dir = root.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(
            git_dir.join("HEAD"),
            "abcdef1234567890abcdef1234567890abcdef12\n",
        )
        .unwrap();

        let branch = detect_branch(&root).unwrap();
        assert_eq!(branch, "abcdef123456");
        assert_eq!(branch.len(), 12);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_detect_branch_no_git_repo_errors() {
        let root = temp_dir("branch-no-repo");
        let err = detect_branch(&root).unwrap_err();
        assert!(err.to_string().contains("No .git found"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_detect_worktree_name_from_gitdir_file() {
        let root = temp_dir("worktree-name");
        // Simulate a worktree: <root>/.git is a *file* pointing at
        // <main>/.git/worktrees/<name>.
        let main_git = root.join("main-repo/.git/worktrees/my-feature-wt");
        std::fs::create_dir_all(&main_git).unwrap();
        std::fs::write(
            root.join(".git"),
            format!("gitdir: {}\n", main_git.display()),
        )
        .unwrap();

        let name = detect_worktree_name(&root);
        assert_eq!(name, Some("my-feature-wt".to_string()));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_detect_worktree_name_none_for_normal_repo() {
        let root = temp_dir("worktree-none");
        // A normal (non-worktree) repo has .git as a directory, not a file.
        std::fs::create_dir_all(root.join(".git")).unwrap();

        assert_eq!(detect_worktree_name(&root), None);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_detect_project_name_worktree_uses_main_repo_name() {
        let root = temp_dir("project-name-wt");
        let main_git = root.join("myproject/.git/worktrees/some-wt");
        std::fs::create_dir_all(&main_git).unwrap();
        let wt_dir = root.join("wt-checkout");
        std::fs::create_dir_all(&wt_dir).unwrap();
        std::fs::write(
            wt_dir.join(".git"),
            format!("gitdir: {}\n", main_git.display()),
        )
        .unwrap();

        let name = detect_project_name(&wt_dir);
        assert_eq!(name, "myproject");

        std::fs::remove_dir_all(&root).ok();
    }
}
