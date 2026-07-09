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

All tools take **no arguments** and return a JSON text block.

| Tool               | Returns                                                                 |
|--------------------|-------------------------------------------------------------------------|
| `overview`         | Everything at once: services, tunnels, observed edges, exercised routes, and the observability caveat. |
| `list_services`    | Discovered processes/containers with `PZ_TUNNEL`: domain, port, source (process pid + cwd, or container id/name), health path. |
| `list_tunnels`     | Tunnel domains (local + cloud), resolved URLs, health paths, and cloud review status. |
| `observed_edges`   | `from → to` dependency edges between tunnels (protocol, request count, last seen). |
| `exercised_routes` | HTTP routes hit per tunnel (method, path, count, `X-PZ-Test` attributions) — a smoke-test inventory. |

Example call:

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"exercised_routes","arguments":{}}}
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
