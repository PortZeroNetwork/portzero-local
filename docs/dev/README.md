# Contributing to portzero-local

These documents are for people working on the portzero-local codebase, CI, releases, and infrastructure.

Audience: project contributors and maintainers.

## Documents

- [Development](development.md) — test suite, local verification with `just`, git hooks with lefthook, and Ticketry.
- [Software delivery lifecycle](sdlc.md) — branch flow (`develop` / `release/current`), release validation, and manual GitHub release publishing.
- [Windows signing runbook](windows-signing.md) — Azure Artifact Signing setup, release signing order, and Defender false-positive follow-up.

See also the files at the repository root:

- `AGENTS.md` — ticketry workflow and agent instructions.
- `CLAUDE.md` — project-specific development rules (terraform, logs as XML, just, etc.).
- `justfile` at repo root — primary task runner.
