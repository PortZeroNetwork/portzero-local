---
id: 4a80490e-e167-4e7b-b0cd-94d4c0536164
slug: task-21
status: done
title: 'Single canonical install.sh: publish as GitHub Release asset; devenv.tools redirects to it'
milestones:
- milestone-1
created_at: 2026-06-22T18:47:07.453256943Z
updated_at: 2026-06-22T18:47:07.453256943Z
---

## Goal

ONE install script, owned by this (client) repo, with no duplicate to maintain.
`devenv.tools/install.sh` becomes a redirect to it — a redirect is not a second
implementation.

## Model

- Single source: `scripts/install.sh` (this repo). Installs `devenv` +
  `devenv-tunnel` from this repo's GitHub Releases — **latest only**. Pre-release,
  so no backwards-compat baggage: no channels/staging, no version pinning. Only
  env var is `DEVENV_INSTALL_DIR`. Channel plumbing was also removed from
  `bin/devenv.rs` (`devenv update` just re-runs the installer) and
  `DEVENV_TOOLS_NO_UPDATE_CHECK` → `DEVENV_NO_UPDATE_CHECK` in `update.rs`.
  (Shared product/infra vars — `DEVENV_TOOLS_API_URL`, `EDGE_URL`, `WEB_URL`,
  `DASHBOARD_URL`, `BASE_DOMAIN` — left as-is; renaming those is a coordinated
  client/server change.)
- Published as a GitHub Release asset every release (added to `release.yml`
  `files:`), giving a stable URL:
  `https://github.com/LoumTechnologies/devenv-tunnel/releases/latest/download/install.sh`
- `devenv.tools/install.sh` 302-redirects to that URL. `curl -fsSL` follows
  redirects, so `curl -fsSL https://devenv.tools/install.sh | sh` works unchanged.

## Done in this repo

- [x] `scripts/install.sh` rewritten as the canonical installer (GitHub Releases,
      `devenv-tunnel-<target>.tar.gz`, both bins).
- [x] `release.yml` publishes `scripts/install.sh` as a release asset.

## Server-side follow-up (../devenv-tools — NOT this repo)

`devenv.tools/install.sh` is served as a static file by nginx
(`cloud/landing/nginx.conf`) from the Astro landing build (`blog/public/install.sh`).
To switch to the redirect:

- [ ] Add to `cloud/landing/nginx.conf` (exact-match wins over static serving):
      `location = /install.sh { return 302 https://github.com/LoumTechnologies/devenv-tunnel/releases/latest/download/install.sh; }`
- [ ] Delete the now-stale static copies: `cloud/landing/install.sh`,
      `cloud/landing/blog/public/install.sh` (and the generated `blog/dist/install.sh`).
- [ ] Rebuild + redeploy the landing (nginx image / landing deploy pipeline).

## Acceptance Criteria

- [x] `scripts/install.sh` is the only install implementation in this repo,
      pulling from GitHub Releases; `sh -n` clean.
- [x] A release publishes `install.sh` at `releases/latest/download/install.sh`.
- [ ] `devenv.tools/install.sh` redirects there (server repo); old static copies
      deleted. (verify: `curl -fsSLI https://devenv.tools/install.sh` shows 302)