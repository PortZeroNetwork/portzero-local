# Review apps (per-PR preview environments)

A "review app" is a full copy of your stack, deployed per pull request, reachable
at a URL a reviewer can click before the PR merges. Port Zero makes this pattern
almost free to build yourself — but **Port Zero does not provide a review-app
orchestrator**. There is no PortZero-hosted concept of a "PR environment," no
button that deploys one, and no PortZero-managed lifecycle. What Port Zero
provides is narrower and more composable: **a tunnel name is the only identity
a running process or container needs**, ports are never hand-assigned, and the
URL for a given name is knowable before anything is deployed. That's enough to
build review apps on top of whatever you already use to deploy — plain
`docker compose`, a shell script, Kubernetes, Nomad, anything.

This page documents the pattern and gives a complete, copy-pasteable example
using `docker compose` and GitHub Actions. Swap in your own deploy mechanism —
none of this depends on GitHub Actions or Compose specifically.

## The core idea

Normally, running N versions of an app on one host means juggling N sets of
ports (or standing up N reverse-proxy vhost entries, or N subdomains in some
external DNS you control). Port Zero removes that juggling:

1. Bind your app's listener to port **0** so the OS always finds a free port —
   no port collisions between PR-1's containers and PR-2's containers on the
   same host, ever.
2. Set `PZ_TUNNEL` to a name that encodes which PR this is, e.g.
   `PZ_TUNNEL=pr-142--myapp.portzero.local:80`.
3. The Port Zero daemon (already running on the host, once) discovers the
   process/container, and `http://pr-142--myapp.portzero.local/` works
   immediately — no manual proxy config, no port bookkeeping, no restart of
   anything else already running on the host.

Two properties fall out of this for free:

- **Coexistence.** Because names (not ports) are the identity, and ports are
  always OS-assigned, PR-140's stack and PR-142's stack can run on the same
  host at the same time with zero coordination between them.
- **The URL is knowable before deploy.** `pr-142--myapp.portzero.local` is
  computable from the PR number alone, before the deploy even starts. A CI job
  can post that URL as a PR comment in the same step that kicks off the
  deploy — it doesn't need to wait for the deploy to finish and scrape a
  dynamically-assigned address out of some tool's output.

## Naming convention

Give every PR's tunnel a name derived from the PR number (or branch), and keep
the app name as a fixed suffix so all review apps for one project group
together in `portzero status` / `portzero inspect`:

```
PZ_TUNNEL=pr-142--myapp.portzero.local:80
PZ_TUNNEL=pr-142--myapp-api.portzero.local:80        # a second service in the same PR's stack
```

Following the naming convention from `PZ_TUNNEL` templating: hierarchy is
expressed with `--` **inside** a single label, not with dots — dots/nesting are
reserved for wildcard custom domains (a Cloud tunnel feature). `_` is not usable
(invalid in hostnames, banned in certificate SANs), and segments must not
contain an internal `--` themselves, or the name becomes ambiguous.

You can compute the `pr-142--` prefix yourself from CI environment variables
(shown in the example below — this works today, with no PortZero-side
dependency), or use the built-in template placeholders once available:

| Placeholder  | Resolves to                                    |
|--------------|-------------------------------------------------|
| `{branch}`   | the current git branch                           |
| `{pr}`       | the pull request number, from Actions env        |
| `{run-id}`   | the GitHub Actions run id (`GITHUB_RUN_ID`)      |

`{branch}` is available today (see [`portzero.md`](portzero.md)). `{pr}` and
`{run-id}` are tracked in `work/task-64.task.md` as part of this milestone; the
example below shows the manual (`${{ github.event.pull_request.number }}`)
form so it works regardless of when that lands, with a note on the shorthand.

For a wildcard **cloud** custom domain (e.g. `review.mycompany.com`), the same
`pr-142--myapp` label works as the leftmost segment under that domain — ask
your Cloud tunnel administrator whether a wildcard custom domain is configured
for your team; that's a Cloud-side feature, out of scope for this local-only
example.

## End-to-end example

This example deploys a review app to a **long-lived host** that already has
the Port Zero daemon running (a self-hosted GitHub Actions runner, a shared dev
box, whatever you already use). That's a deliberate difference from CI-only
ephemeral tunnels (see [`tunnel-action`](../../tunnel-action/README.md)): a review
app needs to keep running *after* the CI job that deployed it finishes, so it
must live somewhere persistent, not on a throwaway hosted runner.

### 1. The app: `docker-compose.yml`

```yaml
# docker-compose.yml
services:
  web:
    build: .
    ports:
      - "0:8080"          # OS-assigned host port — never hardcoded
    environment:
      # PZ_TUNNEL is passed through from the shell that runs `docker compose up`,
      # so one compose file works for every PR without editing it per-PR.
      PZ_TUNNEL: "${PZ_TUNNEL}"
```

The application itself needs zero PortZero-specific code — it just listens on
`8080` inside the container. `PZ_TUNNEL` only needs to be visible to Docker at
container-start time (before `execve()`), which `docker compose up` satisfies
by forwarding your shell's exported variable.

### 2. The workflow: `.github/workflows/review-app.yml`

```yaml
name: Review App

on:
  pull_request:
    types: [opened, synchronize, reopened, closed]

# One review-app deploy per PR at a time; a fast-follow push cancels the
# in-flight deploy for the same PR rather than racing it.
concurrency:
  group: review-app-pr-${{ github.event.pull_request.number }}
  cancel-in-progress: true

jobs:
  deploy:
    if: github.event.action != 'closed'
    runs-on: [self-hosted, review-host]   # a persistent host, not ubuntu-latest
    steps:
      - uses: actions/checkout@v5

      - name: Deploy this PR's stack
        env:
          PR: ${{ github.event.pull_request.number }}
        run: |
          export PZ_TUNNEL="pr-${PR}--myapp.portzero.local:80"
          docker compose -p "pr-${PR}" up -d --build

      - name: Wait for readiness
        env:
          PR: ${{ github.event.pull_request.number }}
        run: portzero wait "pr-${PR}--myapp.portzero.local" --healthy --timeout 120

      - name: Comment the preview URL on the PR
        uses: actions/github-script@v7
        with:
          script: |
            const pr = context.payload.pull_request.number;
            const url = `http://pr-${pr}--myapp.portzero.local/`;
            await github.rest.issues.createComment({
              ...context.repo,
              issue_number: pr,
              body: `Preview deployed: ${url}`,
            });

  teardown:
    if: github.event.action == 'closed'
    runs-on: [self-hosted, review-host]
    steps:
      - name: Tear down this PR's stack
        env:
          PR: ${{ github.event.pull_request.number }}
        run: docker compose -p "pr-${PR}" down -v --remove-orphans
```

Notice what is absent: no PortZero API call to "register a review app," no
PortZero-side concept of "this PR's environment." The workflow directly drives
`docker compose` (or could just as easily be `kubectl apply`, `nomad run`, a
bare `ssh` + shell script — anything). Port Zero's only job is discovering the
container and making `pr-142--myapp.portzero.local` resolve and proxy once it's
up.

### 3. Coexistence in practice

Suppose PR 140 and PR 142 are both open. Their compose projects are named
`pr-140` and `pr-142` (`docker compose -p`), so Compose itself keeps their
containers, networks, and volumes separate. Each container's host port is
OS-assigned (`0:8080`), so there is no chance of a `bind: address already in
use` collision between them. Port Zero discovers both by their distinct
`PZ_TUNNEL` names and serves:

```
http://pr-140--myapp.portzero.local/
http://pr-142--myapp.portzero.local/
```

No step in the workflow above had to know or avoid the other PR's port. That's
the entire trick.

## What's explicitly out of scope

**Port Zero does not ship a deploy agent.** It has no opinion on:

- *when* to deploy (that's your `on:` trigger)
- *where* to deploy (that's your `runs-on:` / target host)
- *how* to deploy (that's your Compose file / Helm chart / shell script)
- *when* to tear down (that's your `closed` handler, a cron job, a TTL sweep —
  your choice)

If you need something to watch for new PRs and *decide* to deploy them,
reconcile drift, or manage a fleet of review hosts, that's a separate tool you
bring yourself (or build) — it is not, and will not become, part of Port Zero.
Port Zero's contract stops at: give a process or container a name, and that
name resolves and proxies. Anything above that layer is deliberately your
code, not ours.

## Teardown is the user's concern

The example above tears down on the `pull_request: closed` event, which covers
the common case. Two other patterns worth knowing about:

- **TTL sweep**: a scheduled workflow (`on: schedule`) that lists PRs older
  than N days still holding a running `pr-*` compose project and tears them
  down, in case a `closed` event was ever missed (a host reboot mid-event,
  etc.).
- **Manual**: `portzero status` on the review host lists every discovered
  tunnel; anything with no matching open PR is safe to
  `docker compose -p <name> down -v` by hand.

Port Zero does not track PR state and will not clean anything up on its own —
a stopped container's tunnel simply stops resolving once discovery no longer
sees it running.

## See also

- [`PZ_TUNNEL` semantics](portzero.md) — the full-domain rule and template
  placeholders.
- [`tunnel-action`](../../tunnel-action/README.md) — the GitHub Action for the
  *different* case of ephemeral, single-job tunnels on hosted runners (e.g. a
  test suite that needs a real HTTPS URL for the duration of one CI job, not a
  long-lived preview environment).
- [Examples](examples.md) — minimal per-language `PZ_TUNNEL` examples.
