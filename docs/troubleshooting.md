# Troubleshooting

## `curl: (6) Could not resolve host: <name>.portzero.local`

The overlay is not active. `.portzero.local` names are **only visible once the
overlay is running**, which requires the daemon to be started with **root /
`CAP_NET_ADMIN`**. Start it with `sudo`:

```bash
sudo -E port-zero start --foreground
```

If you started the daemon unprivileged it logged `continuing in
cloud/local-only mode` and the overlay (TUN + scoped resolver) is inactive. See
[privileges.md](privileges.md).

## My service was never discovered

`PZ_TUNNEL` must be set **before** the service process starts. The daemon
reads the frozen `execve()` environment (`/proc/<pid>/environ` on Linux,
`sysctl KERN_PROCARGS2` on macOS) — a value set after launch (in
`os.environ` / `process.env` / `os.Setenv`) is invisible to it.

- Verify before launching: `echo $PZ_TUNNEL`.
- Use direnv or `portzero-exec` so it is exported before exec.
- Confirm the value is a **full domain** with a recognized suffix
  (`.portzero.local` or `*.<username>.portzero.cloud`); nothing is appended implicitly.

## The literal `{branch}` appears in `status`

Template resolution failed. For native processes ensure you are on a real git
branch (not detached HEAD) within the repo. For containers, the daemon resolves
`{branch}`/`{worktree}` from bind mounts / compose labels — if discovery of
those fails the placeholder is left literal. See
[portzero.md](portzero.md).

## Wrong path taken (cloud vs. local)

The **suffix** decides the target, not any flag:

- `.portzero.local` → local overlay
- `*.<username>.portzero.cloud` → cloud tunnel

Double-check the suffix in your `PZ_TUNNEL` value.

## TUN creation fails even with sudo (Linux)

Ensure `/dev/net/tun` exists and the `tun` module is loaded
(`modprobe tun`). Inside containers, the TUN device must be made available
(`--device /dev/net/tun --cap-add NET_ADMIN`).

## DNS resolves but the connection hangs / refuses

The name resolved to a `10.254.x.y` VIP but the proxy could not reach the
backend. Check that the backend service is still listening on its ephemeral port
and that `port-zero status` shows the expected real address. Restarting the
service re-registers it on the same stable VIP.

## Brave shows `ERR_CERT_AUTHORITY_INVALID` for `https://portzero.local`

If this only happens in the Snap package of Brave, use the native Brave package
instead. PortZero installs its local CA into the Linux system trust store and
browser NSS stores, but some Snap Brave/Chromium builds still ignore those local
trust anchors and report the PortZero CA as an unknown issuer. Non-Snap Brave
uses the installed CA correctly.

## Seeing the daemon's own logs

- `~/.portzero/daemon/daemon.log` (background mode).
- Or run `--foreground` to see logs on the console.
- `port-zero status` shows the current discovered services and routes.

## Running the test suite

- `just test` — full unprivileged suite; the root-gated real-TUN test skips
  cleanly (prints `skipped: requires root`).
- `just e2e` — runs the real-TUN end-to-end test under `sudo`.

## Local checks to save GitHub Actions minutes

GitHub Actions is not free. The main CI job runs:

- `cargo fmt -- --check`
- `cargo clippy --workspace -- -D warnings`
- `cargo test --workspace`

(plus privileged E2E on a separate job).

Run the equivalent locally before you push:

```bash
just verify          # fmt + clippy (-D) + test
# or individually
just fmt-check
just clippy
just test
```

These are the same commands the CI uses on Linux (and the Rust parts of the other OS jobs).

## Git hooks (automatic checks on commit & push)

We use **lefthook** (a fast, lightweight, cross-platform git hooks framework)
so the checks run automatically:

- `pre-commit`: `just fmt-check`
- `pre-push`  : `just fmt-check` + `just clippy` + `just test`

### One-time setup on a machine (Linux, macOS, or Windows)

```bash
just install-hooks
```

This will:

1. Tell you how to install `lefthook` if it is missing (brew / winget / scoop / etc.).
2. Run `lefthook install`.

You only need to do this once per clone / per machine. After that the hooks
stay active.

### Also set up Ticketry hooks

```bash
ticketry init
```

This installs the non-blocking background hooks that keep the ticket index
up to date after commits, checkouts, etc. It is idempotent.

### Privileged / real TUN tests and hooks

**The root-gated test is deliberately not run by the hooks.**

- `just e2e` (and the `real_tun_overlay` test) requires `sudo` / root.
- Running `sudo` from inside a git hook is unreliable:
  - Hooks often have no controlling tty → password prompt fails or hangs.
  - Behavior is different on Windows (no sudo).
  - It would be surprising and annoying on every push.
- The unprivileged `just test` (which the hooks run) is already comprehensive.
  The privileged test **skips cleanly and passes** when you are not root.
- The real E2E is still run by CI in the separate "E2E (privileged real TUN)" job.

Run `just e2e` manually when you are working on TUN/overlay networking changes
and have root available.

### Bypassing (last resort)

```bash
git commit --no-verify
git push --no-verify
```

Use sparingly. The point of the hooks is to stop broken clippy or failing
tests from ever reaching CI.

### Manual hook run

```bash
lefthook run pre-push
lefthook run pre-commit
```

## Other just recipes for development

```bash
just --list
just check        # fast cargo check
just clippy-all   # broader clippy (all targets + features)
just verify       # the full local pre-push equivalent
```
