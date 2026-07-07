---
id: 84791bc2-98c0-44da-a316-4033901b74c4
slug: task-67
status: todo
title: Playwright fixture package for portzero tunnels
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
depends_on:
- fc757857-e263-4eec-a737-58fa44079c55
- e77844ff-b30b-46c5-a91d-99cb31c2306c
created_at: 2026-07-07T22:55:27.893514Z
updated_at: 2026-07-07T23:07:39.695119Z
preferred-model: sonnet
---

An npm package (e.g. `@portzero/playwright`) that makes tunnel-backed apps
first-class in Playwright:

- Resolves `baseURL` by asking the daemon for the tunnel's URL
- Waits for tunnel readiness / health path before tests start
- Supplies `httpCredentials` for access-controlled tunnels (cloud-side feature)
- Optionally stamps requests with an `X-PZ-Test: <title>` header so the daemon's
  observed-routes data can attribute exercised routes per test

Same fixture works in local dev and GitHub Actions — that is the point.

## Acceptance Criteria

- [ ] Package published; works against Local and Cloud tunnels
- [ ] Example in portzero-examples (local dev + Actions workflow)
- [ ] Degrades clearly when the daemon is not running
