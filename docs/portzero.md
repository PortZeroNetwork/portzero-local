# `PZ_TUNNEL` semantics

`PZ_TUNNEL` is the single environment variable that opts a process or Docker
container into the Port Zero system. Set it, bind to port 0, and the daemon
does the rest.

## The full-domain rule

The value **must be a full domain name, including its suffix**. Nothing is ever
appended implicitly. The daemon takes the value verbatim.

```
PZ_TUNNEL=hello.portzero.local                   # valid (local overlay)
PZ_TUNNEL=web.alice.tunnel.portzero.cloud        # valid (cloud tunnel)
PZ_TUNNEL=hello                          # NOT valid — no suffix
```

## The suffix decides the target

The daemon routes based purely on the **suffix** of the full domain:

| Suffix                                            | Target                  |
|---------------------------------------------------|-------------------------|
| `.portzero.local` | Local tunnel |
| `*.<cloud-username>.tunnel.portzero.cloud` (incl. `foo.user.tunnel.portzero.cloud`) | Cloud tunnel |

- `.portzero.local` → the process or container is given a virtual IP from `10.254.0.0/16`,
  served by scoped DNS, and proxied through the TUN + user-space stack on this
  machine. See [architecture.md](architecture.md).
- `*.<cloud-username>.tunnel.portzero.cloud` → the process or container is exposed via the
  Cloud tunnel (requires login). Namespaced forms like
  `foo.team.username.tunnel.portzero.cloud` are also Cloud tunnels.

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
export PZ_TUNNEL=hello.portzero.local

# shell one-off
export PZ_TUNNEL=hello.portzero.local && python3 server.py

# docker (passed at container start)
docker run -e PZ_TUNNEL=hello.portzero.local -p 0:8080 myimage
```

## `{branch}` and `{worktree}` templates

The value may contain two placeholders that the daemon resolves from the host's
git context:

| Placeholder  | Resolves to                                         |
|--------------|-----------------------------------------------------|
| `{branch}`   | the current git branch (e.g. `feature-x`)           |
| `{worktree}` | the basename of the git worktree / repo root        |

```
PZ_TUNNEL=web-{branch}.portzero.local
PZ_TUNNEL=api-{worktree}.{cloud-username}.tunnel.portzero.cloud
```

The daemon resolves these itself from the host (for native processes from the
process context; for Docker containers by inspecting bind mounts and compose
labels), so the literal `{branch}` works even inside a container where no shell
ran. When using direnv you may also resolve the branch directly in the shell with
`$(git rev-parse --abbrev-ref HEAD)` — both approaches yield a fully-resolved
domain by the time the daemon reads it.

## `{local-username}` vs `{cloud-username}`

There are two separate username placeholders. They are **never** interchangeable,
and the daemon enforces that:

| Placeholder         | Resolves to                                    | Changes when you `portzero login`? |
|----------------------|------------------------------------------------|-------------------------------------|
| `{local-username}`  | your OS login username                          | never                               |
| `{cloud-username}`  | your Port Zero Cloud account username           | becomes available once you log in   |

This split exists so a `.local` tunnel's name is stable regardless of login
state. A single unified `{username}` placeholder would either have to change
value the moment you ran `portzero login` (breaking a `.local` tunnel someone
was already using), or never reflect your cloud identity at all — neither is
acceptable, so the two are kept orthogonal instead:

- `{local-username}` may be used freely in a `.local` tunnel template. **It
  must never be used in a cloud tunnel template** (`*.tunnel.portzero.cloud`)
  — doing so would tie the cloud tunnel's name to this machine's OS username,
  which breaks the guarantee that `.local` and `.cloud` tunnel names stay
  independent. The daemon rejects this with an error diagnostic.
- `{cloud-username}` may be used in a `.local` tunnel template, but needs an
  authenticated account to resolve. If you use `{cloud-username}` in a
  `.local` template while logged out, the daemon raises a diagnostic with a
  **"Log in (still free)"** fix button in the [portzero.local dashboard](http://portzero.local#issues) —
  clicking it starts the same browser login flow as running `portzero login`
  yourself, without leaving the browser. **Local tunnels are always free**,
  regardless of login state; logging in here only resolves the
  `{cloud-username}` value, it does not change billing for this tunnel.
- `{cloud-username}` is required (implicitly) in cloud tunnel templates,
  since cloud tunnel domains must be scoped under
  `*.<cloud-username>.tunnel.portzero.cloud` — see the suffix table above.

```
PZ_TUNNEL=db-{local-username}.portzero.local                       # ok: local-only scoping, never changes
PZ_TUNNEL=web-{branch}.{cloud-username}.tunnel.portzero.cloud       # ok: cloud tunnel, scoped by cloud username
PZ_TUNNEL=web-{branch}.{local-username}.tunnel.portzero.cloud       # ERROR: {local-username} not allowed in cloud tunnels
PZ_TUNNEL=db-{cloud-username}.portzero.local                        # ok if logged in; diagnostic + "Log in" fix if not
```

## Bind to port 0

Always bind your listener to port 0 so the OS assigns an ephemeral port. The
daemon discovers the real port; you never hardcode it. The VIP exposes a stable
tunnel port (e.g. `:5432`, `:80`) regardless of the random backend port.

## Companion environment variables

These optional variables are read from the same process/container environment as
`PZ_TUNNEL`, and like `PZ_TUNNEL` they must be set **before launch**.

| Variable              | Purpose                                                                 |
|-----------------------|-------------------------------------------------------------------------|
| `PZ_TUNNEL_HTTP_PORT` | Choose which HTTP port to forward: an explicit port, or `CHOOSE_LOWEST` (default) / `CHOOSE_HIGHEST`. |
| `PZ_TUNNEL_PORTS`     | Extra raw port mappings for non-HTTP forwarding: `local:tunnel[;local:tunnel...]` (e.g. `9222:9222`). |
| `PZ_HEALTH_PATH`      | HTTP path that signals the endpoint is ready (e.g. `/health`).          |

### `PZ_HEALTH_PATH`

`PZ_HEALTH_PATH` declares the HTTP path that returns `2xx` once the tunneled
endpoint is ready to serve traffic. It is **entirely optional** — omitting it
changes nothing.

```bash
export PZ_TUNNEL=web-{branch}.portzero.local
export PZ_HEALTH_PATH=/health          # bare paths are rooted automatically → /health
```

- The value is normalized to a rooted path: `health`, `/health`, and ` health `
  all become `/health`.
- It is stored on the discovered route/tunnel record and shown in the `HEALTH`
  column of `portzero status` (and in `portzero inspect`) for any tunnel that
  declares it. Tunnels without it are unaffected and the column is hidden when
  no tunnel declares one.
- [`portzero wait <domain> --healthy`](portzero.md) polls this path until it
  returns `2xx`, which is how CI and Playwright `webServer` blocks gate on real
  readiness rather than mere port-up.
- The path is a **portable fact**: it survives graduation to a production PaaS
  (which will have its own health-check configuration) even though the `PZ_*`
  variable itself does not. AI coding-agent skills carry the value into the
  production config at graduation time.
