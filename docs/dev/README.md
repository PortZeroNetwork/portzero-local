# Contributing to portzero-local

These documents are for people working on the portzero-local codebase, CI, releases, and infrastructure.

Audience: project contributors and maintainers.

## Documents

- [Development](development.md) — test suite, local verification with `just`, git hooks with lefthook, and Ticketry.
- [Complexity budgets](complexity-budgets.md) — the `just complexity` file-size check, its threshold and rationale, and what's deferred.
- [Software delivery lifecycle](sdlc.md) — the tag-addressed release model: the `staging` integration branch, the gated `vX.Y.Z` release tag, and release validation.
- [Release version numbers](release-version-numbers.md) — how the release picks the next version from the bump you choose at release time.
- [Prerelease (edge) channel](prerelease-channel.md) — cutting unsigned installers off `staging` for testing without stable users detecting or receiving them.
- [Windows signing runbook](windows-signing.md) — Azure Artifact Signing setup, release signing order, and Defender false-positive follow-up.

See also the files at the repository root:

- [CONTRIBUTING.md](../CONTRIBUTING.md) — how to contribute.
- `AGENTS.md` — ticketry workflow and agent instructions.
- `CLAUDE.md` — project-specific development rules (terraform, logs as XML, just, etc.).
- `justfile` at repo root — primary task runner.
