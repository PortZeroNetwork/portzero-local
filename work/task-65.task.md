---
id: 2c84fed9-04a6-497c-96c8-659ce2a5dd97
slug: task-65
status: todo
title: Publish a GitHub Action that installs the daemon and opens tunnels in a job
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
created_at: 2026-07-07T22:54:37.736940Z
updated_at: 2026-07-07T23:07:31.829803Z
preferred-model: sonnet
---

A published action (e.g. `portzero/tunnel-action`) so CI integration is one
YAML block: installs the daemon on the runner, brings up tunnels for the job's
compose project / processes, outputs the URL(s) as step outputs, tears down on
job end.

Notes:
- Local tunnels work inside a runner: it is a single machine, which is exactly
  the daemon's scope (TUN needs sudo — available on ubuntu hosted runners;
  container-based jobs are out of scope for v1).
- Cloud tunnels use the OIDC credential exchange (cloud-side backlog task) so
  no long-lived secrets appear in workflows.

## Acceptance Criteria

- [ ] Action published and usable from a public repo
- [ ] Outputs tunnel URL(s); works with `portzero wait` for readiness
- [ ] Example workflow in portzero-examples covering an integration test against a real HTTPS endpoint
- [ ] Documented limitation: container-based jobs unsupported in v1
