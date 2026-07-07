---
id: 0b3d581b-4412-4327-8c5d-781f5c2dca12
slug: doc-2
status: todo
title: Document the review-apps pattern (orchestrator-neutral)
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
ticket_type: doc
created_at: 2026-07-07T22:55:31.771396Z
updated_at: 2026-07-07T23:07:46.069229Z
preferred-model: sonnet
---

Documentation/blog post: per-PR preview environments using only tunnel naming —
no portzero-provided orchestrator, users bring their own (compose, scripts,
anything). Pattern: each PR's deployment sets `PZ_TUNNEL={branch}--myapp...`
(or a nested name under a wildcard custom domain); random published ports plus
discovery mean coexisting PRs on one host with zero port juggling; the URL is
knowable before deploy so a bot can comment it on the PR; teardown is the
user's concern.

Explicitly out of scope for the product: any deploy agent. Document that
boundary.

## Acceptance Criteria

- [ ] Doc in portzero-local/docs (linked from Docs tab) and/or a blog post on portzero.net
- [ ] End-to-end example with plain docker compose + a GitHub Actions workflow
