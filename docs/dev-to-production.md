# Dev-to-production flow

Port Zero tunnels are addressable by a stable **name** in local dev, CI, and
(for cloud tunnels) from the internet. These commands let test processes and CI
jobs discover a tunnel's URL and gate on its readiness — the same way locally and
in GitHub Actions.

There is no "service" concept: a tunnel's **domain is its only identity**. Every
command below takes the tunnel domain.

## `portzero url <tunnel-domain>`

Prints the resolved URL for a tunnel — and nothing else — so it is safe to
capture in a script:

```bash
BASE_URL=$(portzero url web.myapp.portzero.local)
# → http://web.myapp.portzero.local
```

URL resolution:

| Tunnel                                   | URL                                   |
|------------------------------------------|---------------------------------------|
| Cloud (`*.tunnel.portzero.cloud`)        | `https://<domain>` (edge terminates TLS) |
| Local overlay, port 80                   | `http://<domain>`                     |
| Local overlay, port 443                  | `https://<domain>`                    |
| Local overlay, other port                | `http://<domain>:<port>`              |

If the tunnel is not currently discovered, the command writes an error to
**stderr** (listing the tunnels it does know about) and exits non-zero, so a
failed lookup never silently yields an empty `BASE_URL`.

## `portzero env` and `portzero env --github`

`portzero env` prints an `export` line for every discovered tunnel, so a shell
can pick them all up at once:

```bash
eval "$(portzero env)"
echo "$PZ_URL_WEB_MYAPP_PORTZERO_LOCAL"    # → http://web.myapp.portzero.local
```

The variable name is `PZ_URL_` followed by the domain upper-cased with every
non-alphanumeric character replaced by `_`.

Inside a GitHub Actions job, `portzero env --github` appends the same
`NAME=URL` pairs to the file named by `$GITHUB_ENV`, exporting them to
subsequent steps:

```yaml
- run: docker compose up -d
- run: portzero env --github        # exports PZ_URL_* to later steps
- run: npx playwright test          # reads process.env.PZ_URL_*
```

`--github` errors clearly if `$GITHUB_ENV` is not set (i.e. you are not in an
Actions job).

> These commands are the script-friendly escape hatch. For Playwright
> specifically, the [`@portzero/playwright`](../client) fixture package talks to
> the daemon directly and is the preferred integration.
