//! Domain template engine for DEVENV_TUNNEL support.
//!
//! Resolves templates like `{service}-{project}-{branch}-{user}.tunnel.devenv.tools`
//! into stable DNS-safe domain names for tunnel routes.
//!
//! Tunnel routes live under `tunnel.devenv.tools`. The prefix before
//! `.tunnel.devenv.tools` may contain dots to encode namespaces
//! (e.g. `api.alice.tunnel.devenv.tools`); each dot-separated segment must be
//! a valid DNS label (ASCII alphanumeric + hyphens, no leading/trailing hyphens,
//! ≤ 63 chars).

use std::path::{Path, PathBuf};

/// Tunnel subdomain namespace — all tunnels live under this.
pub const TUNNEL_SUBDOMAIN: &str = "tunnel";

/// Default base domain for devenv.tools services.
pub const DEFAULT_BASE_DOMAIN: &str = "devenv.tools";

/// Default domain template used when none is specified.
///
/// Produces a flat single-label subdomain under `tunnel.devenv.tools`.
/// The base domain can be overridden via `DEVENV_TOOLS_BASE_DOMAIN` for local
/// development.
pub const DEFAULT_TEMPLATE: &str = "{service}-{project}-{branch}-{user}.tunnel.devenv.tools";

/// Build the default domain template using the configured base domain.
///
/// Reads `DEVENV_TOOLS_BASE_DOMAIN` from the environment, falling back to
/// `devenv.tools`.
pub fn default_template() -> String {
    let base = std::env::var("DEVENV_TOOLS_BASE_DOMAIN")
        .unwrap_or_else(|_| DEFAULT_BASE_DOMAIN.to_string());
    format!("{{service}}-{{project}}-{{branch}}-{{user}}.{TUNNEL_SUBDOMAIN}.{base}")
}

/// The tunnel base domain (e.g. `tunnel.devenv.tools`), respecting overrides.
pub fn tunnel_base() -> String {
    let base = std::env::var("DEVENV_TOOLS_BASE_DOMAIN")
        .unwrap_or_else(|_| DEFAULT_BASE_DOMAIN.to_string());
    format!("{TUNNEL_SUBDOMAIN}.{base}")
}

/// Context for resolving DEVENV_TUNNEL templates.
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
    /// Cloud account username (e.g. "alice"). Used for namespace-aware tunnel
    /// domains like `{service}.{username}.tunnel.devenv.tools`.
    /// Falls back to `{uid}` when not available.
    pub username: Option<String>,
}

impl DomainContext {
    /// Build context from the current environment.
    ///
    /// - `service_name`: compose service name or directory name
    /// - `project_dir`: path to the project root (git repo root)
    /// - `account_id`: authenticated account UUID (used for `{uid}`)
    /// - `username`: cloud account username (used for `{username}`)
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
        }
    }

    /// Resolve a domain template, replacing all `{placeholder}` variables.
    ///
    /// All values are DNS-sanitized before substitution.
    ///
    /// Supported variables: `{service}`, `{project}`, `{branch}`, `{worktree}`,
    /// `{user}`, `{machine}`, `{uid}`, `{username}`.
    pub fn resolve(&self, template: &str) -> String {
        let worktree_value = self.worktree.as_deref().unwrap_or(&self.branch);
        let uid_value = self.uid.as_deref().unwrap_or(&self.user);
        let username_value = self.username.as_deref().unwrap_or(uid_value);

        template
            .replace("{service}", &sanitize_for_dns(&self.service))
            .replace("{project}", &sanitize_for_dns(&self.project))
            .replace("{branch}", &sanitize_for_dns(&self.branch))
            .replace("{worktree}", &sanitize_for_dns(worktree_value))
            .replace("{user}", &sanitize_for_dns(&self.user))
            .replace("{machine}", &sanitize_for_dns(&self.machine))
            .replace("{uid}", &sanitize_for_dns(uid_value))
            .replace("{username}", &sanitize_for_dns(username_value))
    }
}

/// Split an optional trailing `:<port>` off a raw `DEVENV_TUNNEL` value.
///
/// The canonical-port feature lets a developer declare the port the overlay
/// should expose a service on by appending `:<port>` to the value, e.g.
/// `db.devenv.local:5432` or `web-{branch}.devenv.local:8080`.
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
///   `{...}`); `web-{branch}.devenv.local:8080` → (`web-{branch}.devenv.local`,
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

/// Validate that `domain` is a legal tunnel subdomain.
///
/// Rules:
/// - Must end with `.tunnel.devenv.tools` (or the configured base domain).
/// - The prefix before `.tunnel.devenv.tools` may contain dots to encode
///   namespaces (e.g. `api.alice.tunnel.devenv.tools`).
/// - Each dot-separated segment must be a valid DNS label: non-empty, ≤ 63
///   characters, ASCII alphanumeric or hyphens, no leading/trailing hyphens.
/// - The bare `tunnel.devenv.tools` hostname is reserved and rejected.
///
/// The expected tunnel base (e.g. `tunnel.devenv.tools`) is derived from
/// `DEVENV_TOOLS_BASE_DOMAIN` the same way as `tunnel_base()`.
pub fn validate_tunnel_domain(domain: &str) -> Result<(), String> {
    let base = tunnel_base();
    let suffix = format!(".{base}");

    if domain == base {
        return Err(format!(
            "'{base}' is a reserved hostname — tunnels must be subdomains of it"
        ));
    }

    let label = domain
        .strip_suffix(&suffix)
        .ok_or_else(|| format!("Tunnel domain must end with '.{base}', got '{domain}'"))?;

    if label.is_empty() {
        return Err(format!(
            "'{base}' is a reserved hostname — tunnels must be subdomains of it"
        ));
    }

    for segment in label.split('.') {
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
            username: Some("alice".to_string()),
        };

        let result = ctx.resolve("{service}-{project}-{branch}-{user}-{uid}.tunnel.devenv.tools");
        assert_eq!(result, "api-myapp-main-alice-abc12345.tunnel.devenv.tools");
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
        };

        let result = ctx.resolve("{service}-{uid}.tunnel.devenv.tools");
        assert_eq!(result, "web-bob.tunnel.devenv.tools");
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
        };

        let result = ctx.resolve("{service}-{worktree}.tunnel.devenv.tools");
        assert_eq!(result, "web-main.tunnel.devenv.tools");
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
        };

        let result = ctx.resolve("{service}-{project}-{branch}-{user}.tunnel.example.com");
        assert_eq!(
            result,
            "my-api-my-app-feat-long-branch-alice-b.tunnel.example.com"
        );
    }

    #[test]
    fn test_resolve_username_template() {
        let ctx = DomainContext {
            service: "api".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "osuser".to_string(),
            machine: "laptop".to_string(),
            uid: Some("abc12345".to_string()),
            username: Some("alice".to_string()),
        };

        let result = ctx.resolve("{service}.{username}.tunnel.devenv.tools");
        assert_eq!(result, "api.alice.tunnel.devenv.tools");
    }

    #[test]
    fn test_resolve_username_falls_back_to_uid() {
        let ctx = DomainContext {
            service: "api".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "osuser".to_string(),
            machine: "laptop".to_string(),
            uid: Some("abc12345".to_string()),
            username: None,
        };

        let result = ctx.resolve("{service}.{username}.tunnel.devenv.tools");
        assert_eq!(result, "api.abc12345.tunnel.devenv.tools");
    }

    #[test]
    fn test_resolve_username_falls_back_to_user_when_no_uid() {
        let ctx = DomainContext {
            service: "api".to_string(),
            project: "proj".to_string(),
            branch: "main".to_string(),
            worktree: None,
            user: "osuser".to_string(),
            machine: "laptop".to_string(),
            uid: None,
            username: None,
        };

        let result = ctx.resolve("{service}.{username}.tunnel.devenv.tools");
        assert_eq!(result, "api.osuser.tunnel.devenv.tools");
    }

    #[test]
    fn test_detect_project_name() {
        let name = detect_project_name(Path::new("/home/user/src/myapp"));
        assert_eq!(name, "myapp");
    }

    // -------------------------------------------------------------------------
    // validate_tunnel_domain tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_tunnel_domain_valid() {
        assert!(validate_tunnel_domain("myapp.tunnel.devenv.tools").is_ok());
        assert!(validate_tunnel_domain("api-myapp-main-alice.tunnel.devenv.tools").is_ok());
        assert!(validate_tunnel_domain("a.tunnel.devenv.tools").is_ok());
        assert!(validate_tunnel_domain("api.alice.tunnel.devenv.tools").is_ok());
        assert!(validate_tunnel_domain("my-api.alice.tunnel.devenv.tools").is_ok());
        assert!(validate_tunnel_domain("svc.team-name.alice.tunnel.devenv.tools").is_ok());
    }

    #[test]
    fn test_validate_tunnel_domain_reserved_bare() {
        let err = validate_tunnel_domain("tunnel.devenv.tools").unwrap_err();
        assert!(
            err.contains("reserved"),
            "Expected reserved error, got: {err}"
        );
    }

    #[test]
    fn test_validate_tunnel_domain_wrong_parent() {
        let err = validate_tunnel_domain("myapp.devenv.tools").unwrap_err();
        assert!(err.contains("tunnel.devenv.tools"), "got: {err}");
    }

    #[test]
    fn test_validate_tunnel_domain_multi_label_valid() {
        assert!(validate_tunnel_domain("api.myapp.tunnel.devenv.tools").is_ok());
    }

    #[test]
    fn test_validate_tunnel_domain_multi_label_invalid_segment() {
        assert!(validate_tunnel_domain("api._bad.tunnel.devenv.tools").is_err());
        assert!(validate_tunnel_domain("api..alice.tunnel.devenv.tools").is_err());
    }

    #[test]
    fn test_validate_tunnel_domain_hyphen_edges() {
        assert!(validate_tunnel_domain("-bad.tunnel.devenv.tools").is_err());
        assert!(validate_tunnel_domain("bad-.tunnel.devenv.tools").is_err());
    }

    #[test]
    fn test_validate_tunnel_domain_invalid_chars() {
        assert!(validate_tunnel_domain("my_app.tunnel.devenv.tools").is_err());
        assert!(validate_tunnel_domain("my app.tunnel.devenv.tools").is_err());
    }

    #[test]
    fn test_validate_tunnel_domain_label_too_long() {
        let long = format!("{}.tunnel.devenv.tools", "a".repeat(64));
        assert!(validate_tunnel_domain(&long).is_err());
    }

    // -------------------------------------------------------------------------
    // split_tunnel_port tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_split_tunnel_port_no_port() {
        assert_eq!(
            split_tunnel_port("db.devenv.local"),
            ("db.devenv.local", None)
        );
    }

    #[test]
    fn test_split_tunnel_port_valid() {
        assert_eq!(
            split_tunnel_port("db.devenv.local:5432"),
            ("db.devenv.local", Some(5432))
        );
        // Lowest and highest valid ports.
        assert_eq!(split_tunnel_port("x.devenv.local:1"), ("x.devenv.local", Some(1)));
        assert_eq!(
            split_tunnel_port("x.devenv.local:65535"),
            ("x.devenv.local", Some(65535))
        );
    }

    #[test]
    fn test_split_tunnel_port_templated_name() {
        // The template is preserved in the domain part; only the port is split.
        assert_eq!(
            split_tunnel_port("web-{branch}.devenv.local:8080"),
            ("web-{branch}.devenv.local", Some(8080))
        );
    }

    #[test]
    fn test_split_tunnel_port_cloud_domain() {
        assert_eq!(
            split_tunnel_port("api.alice.tunnel.devenv.tools:8080"),
            ("api.alice.tunnel.devenv.tools", Some(8080))
        );
    }

    #[test]
    fn test_split_tunnel_port_out_of_range() {
        // 70000 > u16::MAX -> not a valid port; whole value is the domain.
        assert_eq!(
            split_tunnel_port("db.devenv.local:70000"),
            ("db.devenv.local:70000", None)
        );
    }

    #[test]
    fn test_split_tunnel_port_zero_rejected() {
        // Port 0 is not addressable for a canonical VIP listen port.
        assert_eq!(
            split_tunnel_port("db.devenv.local:0"),
            ("db.devenv.local:0", None)
        );
    }

    #[test]
    fn test_split_tunnel_port_non_numeric() {
        assert_eq!(
            split_tunnel_port("db.devenv.local:abc"),
            ("db.devenv.local:abc", None)
        );
    }

    #[test]
    fn test_split_tunnel_port_empty_after_colon() {
        assert_eq!(
            split_tunnel_port("db.devenv.local:"),
            ("db.devenv.local:", None)
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
            split_tunnel_port("a:b.devenv.local:5432"),
            ("a:b.devenv.local", Some(5432))
        );
    }
}
