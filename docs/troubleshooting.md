# Troubleshooting

## `curl: (6) Could not resolve host: <name>.portzero.local`

The overlay is not active. `.portzero.local` names are **only visible once the
overlay is running**, which requires the daemon to be started with **root /
`CAP_NET_ADMIN`**. Start it with `sudo`:

```bash
sudo -E portzero start --foreground
```

If you started the daemon unprivileged it logged `continuing in
cloud/local-only mode` and the overlay (TUN + scoped resolver) is inactive. See
[privileges.md](privileges.md).

## My process was never discovered

`PZ_TUNNEL` must be set **before** the process starts. The daemon
reads the frozen `execve()` environment (`/proc/<pid>/environ` on Linux,
`sysctl KERN_PROCARGS2` on macOS) — a value set after launch (in
`os.environ` / `process.env` / `os.Setenv`) is invisible to it.

- Verify before launching: `echo $PZ_TUNNEL`.
- Use direnv or `portzero-exec` so it is exported before exec.
- Confirm the value is a **full domain** with a recognized suffix
  (`.portzero.local` or `*.<username>.tunnel.portzero.cloud`); nothing is appended implicitly.

## The literal `{branch}` appears in `status`

Template resolution failed. For native processes ensure you are on a real git
branch (not detached HEAD) within the repo. For containers, the daemon resolves
`{branch}`/`{worktree}` from bind mounts / compose labels — if discovery of
those fails the placeholder is left literal. See
[portzero.md](portzero.md).

## Wrong path taken (cloud vs. local)

The **suffix** decides the target, not any flag:

- `.portzero.local` → local overlay
- `*.<username>.tunnel.portzero.cloud` → cloud tunnel

Double-check the suffix in your `PZ_TUNNEL` value.

## TUN creation fails even with sudo (Linux)

Ensure `/dev/net/tun` exists and the `tun` module is loaded
(`modprobe tun`). Inside containers, the TUN device must be made available
(`--device /dev/net/tun --cap-add NET_ADMIN`).

## DNS resolves but the connection hangs / refuses

The name resolved to a `10.254.x.y` VIP but the proxy could not reach the
backend. Check that the backend process or Docker container is still listening
on its ephemeral port and that `portzero status` shows the expected real
address. Restarting it re-registers the same stable VIP.

## Brave shows `ERR_CERT_AUTHORITY_INVALID` for `https://portzero.local`

If this only happens in the Snap package of Brave, use the native Brave package
instead. PortZero installs its local CA into the Linux system trust store and
browser NSS stores, but some Snap Brave/Chromium builds still ignore those local
trust anchors and report the PortZero CA as an unknown issuer. Non-Snap Brave
uses the installed CA correctly. See
[known-limitations.md](known-limitations.md) for the full writeup (and other
browser/engine CA-trust gaps, e.g. Playwright's bundled Firefox).

## Seeing the daemon's own logs

- `~/.portzero/daemon/daemon.log` (background mode).
- Or run `--foreground` to see logs on the console.
- `portzero status` shows the current discovered Local tunnels and routes.

For development workflow, tests, hooks, and contributor instructions see [dev/development.md](dev/development.md).
