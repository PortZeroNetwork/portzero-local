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

### macOS: a process run under a system interpreter is never discovered

On macOS 15.7+ the kernel **hides the environment of SIP-protected system
binaries** from every other process — including the daemon, and including root.
This is not specific to how the daemon reads env: neither `sysctl
KERN_PROCARGS2` nor `ps -E` can see it. So if you tag a process whose executable
is a **system-shipped interpreter**, its `PZ_TUNNEL` is invisible and the
process is silently not discovered. Affected executables include:

- `/usr/bin/python3` (the Xcode/Command-Line-Tools python)
- system `/usr/bin/perl`, `/usr/bin/ruby`
- `/bin/sh`, `/bin/bash` (the system copies)

Run the tagged process under a **non-system runtime** instead — anything not
under `/usr/bin` or `/bin`/`/sbin` is fine:

- Homebrew (`/opt/homebrew/bin/...`, `/usr/local/bin/...`), `nvm`, `pyenv`,
  `rbenv`, `asdf`, or a user-compiled binary.
- Quick check: `PZ_TUNNEL` is readable iff
  `ps -p <pid> -wwwE -o command= | tr ' ' '\n' | grep PZ_TUNNEL` prints it.
  If that shows nothing, the process is under a SIP binary — switch runtimes.

Linux (`/proc/<pid>/environ`) and Windows (`ReadProcessMemory`) have no such
restriction, so system interpreters are discovered there normally.

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

## `.portzero.local` names don't resolve in a cloud sandbox or container

The overlay (TUN device + embedded DNS server) can be up and healthy while
`*.portzero.local` still doesn't resolve via `getaddrinfo`, if the OS-level
scoped resolver never got wired up. On Linux, that wiring needs either
systemd-resolved or a `dnsmasq` fallback (see `net/resolver_config.rs`);
container/cloud sandboxes — including Claude Code web sessions, see
[FAQ.md](FAQ.md#how-do-i-set-up-port-zero-inside-a-claude-code-claudeaicode-cloudbrowser-session)
— frequently have neither: no systemd as PID 1 at all, and no `dnsmasq`
installed. This step logs a warning and does nothing further; it's
best-effort and non-fatal by design, and doesn't affect the overlay itself.

**Fix**: add the dashboard/tunnel hosts entries yourself (`portzero setup`
suggests the exact line; `portzero doctor` re-checks it), or skip name
resolution and use `portzero url` / `portzero env` / `portzero wait` to get
the concrete tunnel URL for scripts — see
[`tunnel-action`](../tunnel-action/README.md), which uses the same pattern
because CI runners have the identical no-systemd shape.

**Before editing `/etc/hosts` yourself**, know that a plain permission check
isn't enough to tell whether an edit will actually work: Linux's immutable
file attribute (`chattr +i`) rejects writes with a bare `EPERM` that looks
like a permissions problem; some distros (e.g. NixOS) symlink `/etc/hosts` to
a generated, declaratively-rebuilt path where a direct edit is pointless; and
inside an actual container, `/etc/hosts` is typically its own bind mount that
the runtime overwrites on every restart, so an edit "succeeds" but doesn't
survive one. `portzero setup`/`portzero doctor` check for all three
(`portzero_daemon::hosts::check_hosts_write_safety`) before writing or
recommending a write, rather than assuming from indirect signals — notably
**not** `/.dockerenv` or `systemd-detect-virt`, which can both be misleading:
a sandbox can report itself as `docker` (or omit `/.dockerenv`) without
actually bind-mounting `/etc/hosts` the way a real container does. The
precise check is whether `/proc/mounts` shows a mount at that exact path.

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
