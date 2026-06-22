# `DEVENV_TUNNEL` semantics

`DEVENV_TUNNEL` is the single environment variable that opts a service into the
devenv-tunnel system. Set it, bind to port 0, and the daemon does the rest.

## The full-domain rule

The value **must be a full domain name, including its suffix**. Nothing is ever
appended implicitly. The daemon takes the value verbatim.

```
DEVENV_TUNNEL=hello.devenv.local            # valid (local overlay)
DEVENV_TUNNEL=web.tunnel.devenv.tools       # valid (cloud tunnel)
DEVENV_TUNNEL=hello                          # NOT valid — no suffix
```

## The suffix decides the target

The daemon routes a service based purely on the **suffix** of the full domain:

| Suffix                                            | Target                  |
|---------------------------------------------------|-------------------------|
| `.devenv.local`                                   | Local virtual overlay   |
| `.tunnel.devenv.tools` (incl. `foo.user.tunnel.devenv.tools`) | Cloud tunnel |

- `.devenv.local` → the service is given a virtual IP from `10.254.0.0/16`,
  served by scoped DNS, and proxied through the TUN + user-space stack on this
  machine. See [architecture.md](architecture.md).
- `.tunnel.devenv.tools` → the service is exposed via the cloud edge (requires
  login). Namespaced forms like `foo.username.tunnel.devenv.tools` are also
  cloud routes.

You choose the path simply by which suffix you put in the value.

## Must be set BEFORE launch

The daemon reads each process's environment from **outside** the process:

- Linux: `/proc/<pid>/environ`
- macOS: `sysctl KERN_PROCARGS2`

Both are **frozen snapshots taken at `execve()` time**. Setting the variable
*inside* a running process (Python `os.environ[...]`, Node `process.env[...]`,
Go `os.Setenv(...)`) updates only the in-process libc copy — **the daemon never
sees it**, and discovery silently fails with no error.

Correct ways to set it before launch:

```bash
# direnv (recommended): export in .envrc, then `direnv allow`
export DEVENV_TUNNEL=hello.devenv.local

# shell one-off
export DEVENV_TUNNEL=hello.devenv.local && python3 server.py

# the exec launcher from the SDKs
sdks/direnv/devenv-tunnel-exec hello.devenv.local python3 server.py

# docker (passed at container start)
docker run -e DEVENV_TUNNEL=hello.devenv.local -p 0:8080 myimage
```

See [`../sdks/direnv/README.md`](../sdks/direnv/README.md) for details.

## `{branch}` and `{worktree}` templates

The value may contain two placeholders that the daemon resolves from the host's
git context:

| Placeholder  | Resolves to                                         |
|--------------|-----------------------------------------------------|
| `{branch}`   | the current git branch (e.g. `feature-x`)           |
| `{worktree}` | the basename of the git worktree / repo root        |

```
DEVENV_TUNNEL=web-{branch}.devenv.local
DEVENV_TUNNEL=api-{worktree}.tunnel.devenv.tools
```

The daemon resolves these itself from the host (for native processes from the
process context; for containers by inspecting bind mounts and compose labels),
so the literal `{branch}` works even inside a container where no shell ran. When
using direnv you may also resolve the branch directly in the shell with
`$(git rev-parse --abbrev-ref HEAD)` — both approaches yield a fully-resolved
domain by the time the daemon reads it.

The SDK helpers may resolve these locally too, but **for display/logging only** —
the daemon resolves independently.

## Bind to port 0

Always bind your listener to port 0 so the OS assigns an ephemeral port. The
daemon discovers the real port; you never hardcode it. The VIP exposes a stable
*service* port (e.g. `:5432`, `:80`) regardless of the random backend port.
