# MCP server & `portzero inspect` — runtime truth

The daemon knows what is *actually* running: which processes and Docker
containers advertised `PZ_TUNNEL`, their ports, the tunnel domains and health
paths, who-talks-to-whom between tunnels, and which HTTP routes were exercised.
Port Zero exposes this **observed runtime truth** two ways:

- **`portzero inspect`** — for humans: readable text, no JSON.
- **`portzero mcp`** — for AI coding agents: a Model Context Protocol server
  (JSON-RPC 2.0 over stdio).

Both read the same daemon state, fresh on each call.

## `portzero inspect`

```
$ portzero inspect
portzero inspect — observed runtime truth
Daemon: running

TUNNELS
  api.alice.tunnel.portzero.cloud
      url:    https://api.alice.tunnel.portzero.cloud
      health: /health
      source: container myapp-api-1 (abc123def456)
  web.portzero.local
      url:    http://web.portzero.local
      health: /healthz
      source: process pid 456 (/work/web)

OBSERVED EDGES (who-talks-to-whom)
  web.alice.tunnel.portzero.cloud -> api.alice.tunnel.portzero.cloud  [http, 12 req]

EXERCISED ROUTES (smoke-test inventory)
  api.alice.tunnel.portzero.cloud
      GET    /health  x30
      POST   /users   x3  (tests: signup flow)
```

`inspect` is intentionally text-only. For a machine-readable view, use the MCP
server.

## The MCP server (`portzero mcp`)

`portzero mcp` speaks the **MCP stdio transport**: newline-delimited JSON-RPC 2.0
on stdin/stdout. An agent launches it as a subprocess.

### Connecting

Any MCP-capable client can register it as a stdio server. For example, in a
Claude Code / Claude Desktop `mcpServers` config:

```json
{
  "mcpServers": {
    "portzero": {
      "command": "portzero",
      "args": ["mcp"]
    }
  }
}
```

The server implements `initialize`, `tools/list`, `tools/call`, and `ping`. It
advertises the `tools` capability and protocol revision `2024-11-05`.

### Tools

Every tool returns a JSON text block. The daemon-state tools take **no
arguments**; the two cloud-backed feedback tools take typed arguments (see
below). Arguments are passed the standard MCP way, as the `arguments` object
inside `tools/call` params.

| Tool               | Arguments | Returns                                                                 |
|--------------------|-----------|-------------------------------------------------------------------------|
| `overview`         | none      | Everything at once: services, tunnels, observed edges, exercised routes, and the observability caveat. |
| `list_services`    | none      | Discovered processes/containers with `PZ_TUNNEL`: domain, port, source (process pid + cwd, or container id/name), health path. |
| `list_tunnels`     | none      | Tunnel domains (local + cloud), resolved URLs, health paths, and cloud review status. |
| `observed_edges`   | none      | `from → to` dependency edges between tunnels (protocol, request count, last seen). |
| `exercised_routes` | none      | HTTP routes hit per tunnel (method, path, count, `X-PZ-Test` attributions) — a smoke-test inventory. |
| `list_feedback`    | `status?` | Feedback threads from portzero.cloud: reviewer comments pinned on your tunneled app. Requires `portzero login`. |
| `propose_fix`      | `thread_id`, `fix_commit`, `fix_summary` | Marks a feedback thread as fixed by a commit (`fix_proposed`). Requires `portzero login`. |

Example call (no-argument tool — `arguments` may be `{}` or omitted):

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"exercised_routes","arguments":{}}}
```

### Feedback tools (`list_feedback`, `propose_fix`)

These two tools call the portzero.cloud API and need stored credentials from
`portzero login`; without them the call returns an `isError: true` result with
the "Not logged in" message. See [Review records](review-records.md) for the
full workflow they belong to.

**`list_feedback`** — arguments: `{ "status"?: "open" | "fix_proposed" |
"resolved" }` (default `"open"`). Returns, for each thread: its `ref`
(`PZ-<n>`), `id`, `status`, `domain`, `route`, `guest_name`, `created_at`, the
comment bodies, and `fix_commit`/`fix_summary` when a fix has been proposed.
These are reviewer comments pinned on your tunneled app. To mark one fixed,
either call `propose_fix`, or include `Fixes PZ-<n>` in the commit message of
the fixing commit and upload a review record with `portzero review` — the
cloud advances the thread automatically.

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_feedback","arguments":{"status":"open"}}}
```

**`propose_fix`** — arguments (all required): `{ "thread_id": string,
"fix_commit": string, "fix_summary": string }`. `thread_id` is the `id` field
from `list_feedback` (not the `PZ-<n>` ref). The thread moves to
`fix_proposed` and then awaits human confirmation: the commenter or a team
member resolves it. The `Fixes PZ-<n>` commit-message convention (see above)
is the hands-off alternative.

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"propose_fix","arguments":{"thread_id":"<id>","fix_commit":"abc1234","fix_summary":"Fix checkout button contrast"}}}
```

### How edges and routes are recorded

The userspace proxy already carries every request addressed to a tunnel name, so
it records — with no extra instrumentation:

- **Edges**: when a request to tunnel `B` carries a `Referer`/`Origin` whose host
  is another tunnel `A`, that is an `A → B` dependency edge.
- **Exercised routes**: the `(method, path)` pairs hit on each tunnel, plus any
  `X-PZ-Test` header (the `@portzero/playwright` fixture stamps this so routes
  can be attributed per test).

Observations persist to `~/.portzero/daemon/observations.json` and accumulate
while the daemon runs.

## Extraction skill for AI agents

`portzero skill install` drops a PaaS-agnostic **extraction skill** into your
project (default `.claude/skills/`, or `--print` / `--dir` for other agent
tools). It teaches an AI coding agent to consume the MCP tools above (plus your
compose file) and extract everything needed to configure production hosting on
**any** platform — which containers are public vs internal, ports, health paths,
service dependencies, and a smoke-test route list. The skill extracts and
interprets only; it contains no platform-specific templates.

## Observability caveat

**Only traffic addressed via tunnel names is observed.** Container-to-container
traffic over compose-internal DNS — e.g. one compose service reaching another as
`http://db:5432` on the shared compose network — never passes through the daemon
and therefore does **not** appear in observed edges or exercised routes. Treat
the edge/route data as a lower bound on real dependencies: everything shown is
real, but internal service-to-service calls that never used a tunnel name are
invisible. When extracting a production topology, combine this with the compose
file (which shows the internal wiring).
