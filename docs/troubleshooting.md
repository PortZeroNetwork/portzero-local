# Troubleshooting

## `curl: (6) Could not resolve host: <name>.devenv.local`

The overlay is not active. `.devenv.local` names are **only visible once the
overlay is running**, which requires the daemon to be started with **root /
`CAP_NET_ADMIN`**. Start it with `sudo`:

```bash
sudo -E devenv-tunnel start --foreground
```

If you started the daemon unprivileged it logged `continuing in
cloud/local-only mode` and the overlay (TUN + scoped resolver) is inactive. See
[privileges.md](privileges.md).

## My service was never discovered

`DEVENV_TUNNEL` must be set **before** the service process starts. The daemon
reads the frozen `execve()` environment (`/proc/<pid>/environ` on Linux,
`sysctl KERN_PROCARGS2` on macOS) — a value set after launch (in
`os.environ` / `process.env` / `os.Setenv`) is invisible to it.

- Verify before launching: `echo $DEVENV_TUNNEL`.
- Use direnv or `devenv-tunnel-exec` so it is exported before exec.
- Confirm the value is a **full domain** with a recognized suffix
  (`.devenv.local` or `.tunnel.devenv.tools`); nothing is appended implicitly.

## The literal `{branch}` appears in `status`

Template resolution failed. For native processes ensure you are on a real git
branch (not detached HEAD) within the repo. For containers, the daemon resolves
`{branch}`/`{worktree}` from bind mounts / compose labels — if discovery of
those fails the placeholder is left literal. See
[devenv-tunnel.md](devenv-tunnel.md).

## Wrong path taken (cloud vs. local)

The **suffix** decides the target, not any flag:

- `.devenv.local` → local overlay
- `.tunnel.devenv.tools` → cloud tunnel

Double-check the suffix in your `DEVENV_TUNNEL` value.

## TUN creation fails even with sudo (Linux)

Ensure `/dev/net/tun` exists and the `tun` module is loaded
(`modprobe tun`). Inside containers, the TUN device must be made available
(`--device /dev/net/tun --cap-add NET_ADMIN`).

## DNS resolves but the connection hangs / refuses

The name resolved to a `10.254.x.y` VIP but the proxy could not reach the
backend. Check that the backend service is still listening on its ephemeral port
and that `devenv-tunnel status` shows the expected real address. Restarting the
service re-registers it on the same stable VIP.

## Seeing the daemon's own logs

- `~/.devenv/daemon/daemon.log` (background mode).
- Or run `--foreground` to see logs on the console.
- `devenv-tunnel status` shows the current discovered services and routes.

## Running the test suite

- `just test` — full unprivileged suite; the root-gated real-TUN test skips
  cleanly (prints `skipped: requires root`).
- `just e2e` — runs the real-TUN end-to-end test under `sudo`.
