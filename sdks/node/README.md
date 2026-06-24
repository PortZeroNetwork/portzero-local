# port-zero — Node.js helper

Thin convenience wrapper for Node.js / TypeScript projects.

## How it works

1. **Set `PORT_ZERO` BEFORE starting your process** to a full domain name
   (suffix decides the route):
   - `myapp-{branch}.portzero.local` → local virtual overlay
   - `myapp-{branch}.tunnel.portzero.cloud` → cloud tunnel

   **This library does NOT and CANNOT set `PORT_ZERO` for daemon discovery.**
   See [Why you must set PORT_ZERO before launch](#why-you-must-set-port_zero-before-launch)
   and [`sdks/direnv/`](../direnv/) for the recommended setup.

2. Bind your server to **port 0** — the OS assigns an ephemeral port.
3. The `port-zero` daemon discovers your process, reads `PORT_ZERO`,
   finds the real port, and routes it.

The templates `{branch}` and `{worktree}` are resolved by the **daemon**
on the host side. This helper attempts a local resolution for logging
purposes only (clearly labelled "informational").

## Why you must set PORT_ZERO before launch

The daemon reads each process's environment from **outside** the process:

- Linux: `/proc/<pid>/environ`
- macOS: `sysctl KERN_PROCARGS2`

Both sources reflect the environment that was passed to the process at
`execve()` time — they are **frozen snapshots**. Setting
`process.env.PORT_ZERO` at runtime updates only the in-process libc copy
and is **never visible to the daemon**. A runtime-set variable silently fails
discovery with no error message.

**Set `PORT_ZERO` before starting your process** using one of:

- **direnv** (recommended): add the export to `.envrc` — see
  [`sdks/direnv/README.md`](../direnv/README.md)
- **shell**: `export PORT_ZERO=myapp-$(git rev-parse --abbrev-ref HEAD).portzero.local`
- **docker**: `docker run -e PORT_ZERO=myapp-{branch}.portzero.local ...`
- **docker-compose**: add to `environment:` in `docker-compose.yml`

## Installation

Copy `index.js` (and optionally `index.d.ts`) into your project, or
reference it from the `sdks/node/` directory. No npm publish yet.

## API

### `listenWithTunnel(server, options?)`

Calls `server.listen(0, host, callback)` and resolves with the assigned port.
Logs the `PORT_ZERO` value (or a warning with setup instructions if unset).

```js
const http = require("http");
const { listenWithTunnel } = require("./index");

const server = http.createServer((req, res) => {
  res.end("Hello from port-zero!\n");
});

// PORT_ZERO must already be set in the environment before this runs.
listenWithTunnel(server, {
  serviceName: "web",
}).then((port) => {
  console.log(`Server running on port ${port}`);
});
```

Works with Express, Fastify, and any framework whose app/server object
has a `listen(port, host, callback)` method.

### `reservePort(options?)`

Reserves an ephemeral port without creating an HTTP server. Returns
`{ port, close() }`. Useful when you need the port number before
constructing the server.

```js
const { reservePort } = require("./index");

// PORT_ZERO must already be set in the environment before this runs.
reservePort().then(({ port, close }) => {
  console.log(`Reserved port ${port}`);
  // ... start your server on this port, then call close() on the reserved socket
});
```

## Options

| Option        | Type     | Default      | Description                          |
|---------------|----------|--------------|--------------------------------------|
| `host`        | `string` | `"0.0.0.0"` | Network interface to bind on         |
| `serviceName` | `string` | `"app"`      | Label shown in log output            |

## Example

See [`examples/express-app.js`](examples/express-app.js) for a working
plain-`http` example (no extra dependencies). Run it as:

```bash
PORT_ZERO=myapp-mybranch.portzero.local node examples/express-app.js
```

## Framework snippets

### Express

```js
const express = require("express");
const { listenWithTunnel } = require("./index");

const app = express();
app.get("/", (req, res) => res.send("Hello!"));

// Set PORT_ZERO before running: export PORT_ZERO=myapp-{branch}.portzero.local
listenWithTunnel(app, {
  serviceName: "express-web",
}).then((port) => console.log(`Express on port ${port}`));
```

### Fastify

```js
const fastify = require("fastify")();
const { listenWithTunnel } = require("./index");

fastify.get("/", async () => "Hello!");

// Set PORT_ZERO before running: export PORT_ZERO=myapp-{branch}.portzero.local
// Fastify exposes server.listen — wrap the underlying server:
listenWithTunnel(fastify.server, {
  serviceName: "fastify-web",
}).then((port) => console.log(`Fastify on port ${port}`));
// Note: call fastify.ready() first if you need plugins initialised.
```
