# Contributing to portzero-local

These documents are for people working on the portzero-local codebase, CI, releases, and infrastructure.

Audience: project contributors and maintainers.

## Documents

- [Development](development.md) — test suite, local verification with `just`, git hooks with lefthook, and Ticketry.
- [Complexity budgets](complexity-budgets.md) — the `just complexity` file-size check, its threshold and rationale, and what's deferred.
- [Software delivery lifecycle](sdlc.md) — branch flow (`develop` / `release/current`), release validation, and manual GitHub release publishing.
- [Release version numbers](release-version-numbers.md) — how the release workflow picks the next version, and how to force a specific bump (e.g. `1.0.0`).
- [Prerelease (edge) channel](prerelease-channel.md) — cutting signed installers off `develop` for testing without stable users detecting or receiving them.
- [Windows signing runbook](windows-signing.md) — Azure Artifact Signing setup, release signing order, and Defender false-positive follow-up.

See also the files at the repository root:

- [CONTRIBUTING.md](../CONTRIBUTING.md) — how to contribute.
- `AGENTS.md` — ticketry workflow and agent instructions.
- `CLAUDE.md` — project-specific development rules (terraform, logs as XML, just, etc.).
- `justfile` at repo root — primary task runner.
