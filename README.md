# port zero

Open-source tunnel client for portzero.cloud.

This repository contains the local tunnel workspace:

- `client/crates/cli` for the `devenv` and `port-zero` binaries
- `client/crates/daemon` for service discovery and cloud connectivity
- `client/crates/domain` for tunnel domain resolution
- `client/crates/tunnel-client` for the local route table helper
- `proto` for the WebSocket message protocol shared with the edge

Repository: https://github.com/LoumTechnologies/port-zero

## Documentation & examples

- [`docs/`](docs/) — architecture overview, `PORT_ZERO` semantics, privilege
  requirements, and troubleshooting.
- [`examples/local-overlay/`](examples/local-overlay/) — a runnable end-to-end
  `.devenv.local` virtual overlay example.
- [`sdks/`](sdks/) — thin language helpers (direnv, Node, Python, Go).

## License

PolyForm Shield. See [LICENSE](LICENSE).
