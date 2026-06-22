# devenv-tunnel SDKs

Language helpers for the devenv-tunnel ecosystem. Each SDK is a thin
convenience wrapper around the two-step universal mechanism.

## The universal mechanism (no real SDK required)

Any language, any framework, any container — the integration is always
the same two steps:

1. **Set `DEVENV_TUNNEL` BEFORE starting your process** to a full domain
   name (including suffix). Templates `{branch}` and `{worktree}` are
   resolved by the daemon at the host level, so they work in containers too.

   ```
   # Local virtual overlay
   DEVENV_TUNNEL=myapp-{branch}.devenv.local

   # Cloud tunnel
   DEVENV_TUNNEL=myapp-{branch}.tunnel.devenv.tools
   ```

   The suffix decides the target — nothing is appended implicitly.

2. **Bind to port 0** so the OS picks an ephemeral port. The
   long-running discovery daemon (`devenv-tunnel start`) detects your
   process or container, reads `DEVENV_TUNNEL`, finds the real port,
   and routes it.

That's it. You don't install an agent, SDK library, or sidecar inside
the container. The daemon runs once on the host.

## Why DEVENV_TUNNEL must be set before launch

The daemon reads each process's environment from **outside** the process:

- Linux: `/proc/<pid>/environ`
- macOS: `sysctl KERN_PROCARGS2`

Both sources are **frozen snapshots** from `execve()` time. Setting
`DEVENV_TUNNEL` inside a running process (Python `os.environ[...]`, Node
`process.env[...]`, Go `os.Setenv(...)`) updates only the in-process libc
copy — **the daemon never sees it**. A runtime-set variable silently fails
discovery with no error message.

The language helpers in this directory **do NOT and CANNOT set
`DEVENV_TUNNEL` for daemon discovery**. They read the variable from the
environment (set before launch) and log its value so you can confirm
the correct domain is configured.

**Recommended setup:** use [direnv](direnv/) — it exports `DEVENV_TUNNEL`
automatically when you `cd` into your project directory, before any server
process starts.

## What the helpers do

Each language helper is a thin convenience wrapper that:

- Binds to **port 0** and returns/logs the chosen ephemeral port.
- Reads `DEVENV_TUNNEL` from the environment (read-only) and logs its value,
  or emits a WARNING with setup instructions when it is unset.
- Attempts local `{branch}`/`{worktree}` resolution for **display/logging only**,
  clearly labelled as informational (the daemon resolves independently from
  its own cwd/git context).

## Per-language helpers

| Language / Tool | Directory               | Notes                                                      |
|-----------------|-------------------------|------------------------------------------------------------|
| direnv          | [`direnv/`](direnv/)    | **Start here** — exports `DEVENV_TUNNEL` before any process starts |
| Node.js / TS    | [`node/`](node/)        | Works with plain `http`, Express, Fastify…                 |
| Python          | [`python/`](python/)    | stdlib-only; Flask/Starlette snippets in README            |
| Go              | [`go/`](go/)            | Single-file package, no extra dependencies                 |

## Quick start (direnv)

```sh
# 1. Install direnv and hook it into your shell
#    https://direnv.net/docs/installation.html

# 2. Add to your project's .envrc:
echo 'export DEVENV_TUNNEL="myapp-$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown).devenv.local"' >> .envrc
direnv allow

# 3. Start your server — DEVENV_TUNNEL is already set
node server.js   # or python3 app.py, go run ., etc.
```

See [`direnv/README.md`](direnv/README.md) for full details including the
`devenv-tunnel-exec` launcher (for when direnv is unavailable) and docker
integration.

## Deferred / follow-ups

- IDE integrations (VS Code extension, JetBrains plugin) — see follow-up ticket.
- Additional language SDKs (Ruby, Java/JVM, Rust, PHP, .NET) — see follow-up ticket.
- Framework-specific deep integrations (Django, Rails, Spring, etc.).

## Running the daemon

```bash
# Install (requires Rust toolchain)
cargo install --path client/crates/cli

# Start (once, leave running)
devenv-tunnel start

# Check discovered tunnels
devenv-tunnel status

# Stop
devenv-tunnel stop
```
