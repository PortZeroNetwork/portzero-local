# port-zero — Python helper

Thin, stdlib-only convenience wrapper for Python projects.

## How it works

1. **Set `PORT_ZERO` BEFORE starting your process** to a full domain name
   (suffix decides the route):
   - `myapp-{branch}.devenv.local` → local virtual overlay
   - `myapp-{branch}.tunnel.portzero.cloud` → cloud tunnel

   **This library does NOT and CANNOT set `PORT_ZERO` for daemon discovery.**
   See [Why you must set PORT_ZERO before launch](#why-you-must-set-port_zero-before-launch)
   and [`sdks/direnv/`](../direnv/) for the recommended setup.

2. Bind your server to **port 0** — the OS picks an ephemeral port.
3. The `port-zero` daemon discovers your process, reads `PORT_ZERO`,
   finds the real port, and routes it.

Templates `{branch}` and `{worktree}` are resolved by the **daemon** on the
host — this helper attempts local resolution for logging only (clearly
labelled "informational").

## Why you must set PORT_ZERO before launch

The daemon reads each process's environment from **outside** the process:

- Linux: `/proc/<pid>/environ`
- macOS: `sysctl KERN_PROCARGS2`

Both sources reflect the environment passed to the process at `execve()` time —
they are **frozen snapshots**. Setting `os.environ["PORT_ZERO"]` at runtime
updates only the in-process libc copy and is **never visible to the daemon**.
A runtime-set variable silently fails discovery with no error message.

**Set `PORT_ZERO` before starting your process** using one of:

- **direnv** (recommended): add the export to `.envrc` — see
  [`sdks/direnv/README.md`](../direnv/README.md)
- **shell**: `export PORT_ZERO=myapp-$(git rev-parse --abbrev-ref HEAD).devenv.local`
- **docker**: `docker run -e PORT_ZERO=myapp-{branch}.devenv.local ...`
- **docker-compose**: add to `environment:` in `docker-compose.yml`

## Installation

Copy `port_zero.py` into your project. No pip package yet. No dependencies
beyond the Python stdlib.

## API

### `find_free_port(host, *, service_name, log) → (socket, port)`

Binds a TCP socket to port 0 and returns `(sock, port)`. The socket
**stays open** (safest option — no TOCTOU race). Pass it to your server
framework or close it yourself.

PORT_ZERO must already be set before the process started.

### `get_free_port(host, *, service_name, log) → int`

Convenience wrapper: binds to port 0, records the port, closes the socket,
returns the integer. Use only when your framework accepts a port number and
cannot accept a pre-bound socket.

### `read_tunnel() → str | None`

Read `PORT_ZERO` from the environment (read-only). Returns the value, or
`None` if unset.

## Examples

### stdlib `http.server`

```python
import http.server
import sys
sys.path.insert(0, ".")  # adjust if port_zero.py is elsewhere

from port_zero import find_free_port

# PORT_ZERO must already be set in the environment before running.
sock, port = find_free_port(service_name="http-demo")

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"Hello from port-zero!\n")

server = http.server.HTTPServer(("0.0.0.0", 0), Handler)
# Replace the server's socket with our pre-bound one:
server.socket.close()
server.socket = sock
server.server_address = sock.getsockname()
print(f"Serving on port {port}")
server.serve_forever()
```

See [`examples/http_server.py`](examples/http_server.py) for a runnable version.

Run it as:
```bash
PORT_ZERO=myapp-mybranch.devenv.local python3 examples/http_server.py
```

### Flask

```python
from port_zero import get_free_port
from flask import Flask

app = Flask(__name__)

@app.route("/")
def index():
    return "Hello from port-zero!\n"

if __name__ == "__main__":
    # PORT_ZERO must already be set in the environment before running.
    port = get_free_port(service_name="flask-web")
    app.run(host="0.0.0.0", port=port)
```

### Django (management command / `runserver` equivalent)

```python
# In settings.py or wsgi.py startup:
from port_zero import get_free_port

# PORT_ZERO must already be set in the environment before running.
PORT = get_free_port(service_name="django-web")
# Then pass PORT to whatever starts the WSGI/ASGI server.
```

### Starlette / Uvicorn

```python
import uvicorn
from port_zero import get_free_port
from starlette.applications import Starlette
from starlette.responses import PlainTextResponse
from starlette.routing import Route

def homepage(request):
    return PlainTextResponse("Hello from port-zero!\n")

app = Starlette(routes=[Route("/", homepage)])

if __name__ == "__main__":
    # PORT_ZERO must already be set in the environment before running.
    port = get_free_port(service_name="starlette-web")
    uvicorn.run(app, host="0.0.0.0", port=port)
```
