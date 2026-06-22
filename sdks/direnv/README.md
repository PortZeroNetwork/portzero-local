# devenv-tunnel — direnv integration

This directory contains the **primary recommended** way to set `DEVENV_TUNNEL`
for local development: a direnv `.envrc` snippet.

## Why DEVENV_TUNNEL must be set before launch

The `devenv-tunnel` daemon reads each process's environment from **outside**
the process:

- Linux: `/proc/<pid>/environ`
- macOS: `sysctl KERN_PROCARGS2`

Both sources are **frozen snapshots** from `execve()` time. Setting
`DEVENV_TUNNEL` inside a running process (e.g. `os.environ["DEVENV_TUNNEL"] =
...` in Python, `process.env.DEVENV_TUNNEL = ...` in Node, `os.Setenv(...)` in
Go) updates only the in-process libc copy — **the daemon never sees it**.
A runtime-set variable silently fails discovery with no error or warning from
the daemon.

**The variable must be present in the environment passed to `execve()`.**

## Recommended approach: direnv

[direnv](https://direnv.net/) hooks into your shell and sources `.envrc`
automatically when you `cd` into a directory. This means `DEVENV_TUNNEL` is
set in the shell **before** you start your server — exactly what is needed.

### Setup

1. [Install direnv](https://direnv.net/docs/installation.html) and hook it
   into your shell (`eval "$(direnv hook bash)"` / `zsh` / `fish`).

2. Add an `.envrc` to your project root (or append to an existing one):

   ```sh
   # Local virtual overlay — resolves branch in the shell before exec:
   export DEVENV_TUNNEL="myapp-$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown).devenv.local"

   # Cloud tunnel variant:
   # export DEVENV_TUNNEL="myapp-$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown).tunnel.devenv.tools"
   ```

   See [`.envrc`](.envrc) in this directory for a copy-paste snippet.

3. Allow the file:

   ```bash
   direnv allow
   ```

4. Start your server normally — `DEVENV_TUNNEL` is already set.

### How template resolution works

The daemon also accepts `{branch}` and `{worktree}` placeholders in
`DEVENV_TUNNEL` and resolves them itself from the host's git context (useful
for Docker containers where the shell never runs). In the direnv case, you
resolve the branch directly in the shell using `$(git ...)` — both approaches
result in a fully-resolved domain by the time the daemon reads it.

## Alternative: devenv-tunnel-exec launcher

When direnv is not available, [`devenv-tunnel-exec`](devenv-tunnel-exec) is a
tiny POSIX shell script that sets `DEVENV_TUNNEL` and then `exec`s your
command:

```bash
# Make it available on PATH (copy or symlink):
cp sdks/direnv/devenv-tunnel-exec ~/.local/bin/devenv-tunnel-exec
chmod +x ~/.local/bin/devenv-tunnel-exec

# Use it:
devenv-tunnel-exec myapp-$(git rev-parse --abbrev-ref HEAD).devenv.local node server.js
devenv-tunnel-exec myapp-main.devenv.local python3 app.py
```

This works for the same reason direnv does: it sets the variable in the shell
and uses `exec` to replace the shell process, so `DEVENV_TUNNEL` appears in
the `execve()` call and is visible in `/proc/<pid>/environ`.

This is the **only** correct programmatic alternative to direnv. SDK helpers
(Node `process.env`, Python `os.environ`, Go `os.Setenv`) cannot substitute
for it.

## docker / docker-compose

For containers, pass the variable at container start time:

```bash
# docker run
docker run -e DEVENV_TUNNEL=myapp-{branch}.devenv.local -p 0:8080 myimage

# docker-compose.yml
services:
  web:
    environment:
      DEVENV_TUNNEL: "myapp-${BRANCH:-main}.devenv.local"
    ports:
      - "0:8080"
```

The daemon resolves `{branch}` from bind mounts or compose labels on the host,
so the literal `{branch}` placeholder works for containers.

## Summary

| Method                 | When to use                                      |
|------------------------|--------------------------------------------------|
| **direnv** (`.envrc`)  | Local dev — recommended; automatic per directory |
| `devenv-tunnel-exec`   | Scripts, CI, when direnv is unavailable          |
| `docker run -e` / compose `environment:` | Containerised services |
| Shell `export`         | Quick one-off: `export DEVENV_TUNNEL=... && node server.js` |
