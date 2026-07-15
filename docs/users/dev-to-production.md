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
> specifically, the [`@portzero/playwright`](../../client) fixture package talks to
> the daemon directly and is the preferred integration.

## `portzero wait <tunnel-domain> [--healthy] [--timeout <secs>]`

Blocks until the tunnel is **up**, then exits `0`. This is the readiness gate for
CD smoke tests and Playwright `webServer` blocks — the same command works for
Local and Cloud tunnels.

```bash
docker compose up -d
portzero wait web.myapp.portzero.local            # up = discovered & routable
portzero wait web.myapp.portzero.local --healthy  # also polls the health path
portzero wait web.myapp.portzero.local --timeout 120
```

- **`--healthy`** additionally polls the endpoint's health path until it returns
  `2xx`. If the endpoint declares [`PZ_HEALTH_PATH`](portzero.md#pz_health_path),
  that path is polled automatically (even without `--healthy`); otherwise
  `--healthy` defaults to `/`.
- **`--timeout`** caps the wait (default **60s**). On timeout the command exits
  non-zero with a message explaining what it was still waiting for.
- **Paused vs dead.** A *paused* tunnel (an edge-only pause — a cloud-side
  feature) is reported **distinctly**: `wait` fails fast with a "paused" message
  rather than blocking, because waiting cannot help. A *dead* tunnel (one that
  never comes up) fails on timeout. The two never look the same.

### Playwright `webServer` example

Start the stack and gate the test run on the tunnel being healthy before
Playwright opens the first page:

```ts
// playwright.config.ts
import { defineConfig } from '@playwright/test';

export default defineConfig({
  webServer: {
    command:
      'docker compose up -d && portzero wait web.myapp.portzero.local --healthy',
    url: 'http://web.myapp.portzero.local',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
  use: {
    baseURL: 'http://web.myapp.portzero.local',
  },
});
```

The identical `webServer` block runs in local dev and in a GitHub Actions job —
that is the point. In CI you can also export the URL first with
`portzero env --github` and read `process.env.PZ_URL_*` for `baseURL`.
