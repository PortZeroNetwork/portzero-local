---
id: a992dee1-7bac-4471-ae39-eca32359ea93
slug: task-66
status: todo
title: Verify CA install works for Playwright browsers on GitHub Actions runners
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
created_at: 2026-07-07T22:54:39.715053Z
updated_at: 2026-07-07T23:07:35.985013Z
preferred-model: sonnet
---

The existing CA generation/installation covers OS trust stores but misses some
browsers (e.g. Brave installed as a snap). Playwright downloads its own browser
builds (not snaps), so runners may actually be the easy case — verify it.

One test workflow on ubuntu-latest: install daemon + CA, run a minimal
Playwright test against an HTTPS local tunnel in Chromium, Firefox, WebKit.
Fix gaps found (e.g. NSS user store via certutil for Chromium/Firefox on
Linux), or document `ignoreHTTPSErrors` as the fallback.

## Acceptance Criteria

- [ ] CI workflow proves (or disproves) trusted TLS for all three Playwright engines on a hosted runner
- [ ] Gaps either fixed or documented with the fallback
- [ ] Known-limitations doc updated (snap-browser caveat recorded)
