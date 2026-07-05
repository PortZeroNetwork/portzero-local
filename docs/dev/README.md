# Contributing to portzero-local

These documents are for people working on the portzero-local codebase, CI, releases, and infrastructure.

Audience: project contributors and maintainers.

> **Important:** All external contributions require signing the [Contributor License Agreement (CLA)](../CLA.md) (also linked as [CLA.md](../CLA.md) from the repo root). See the root [CONTRIBUTING.md](../CONTRIBUTING.md) for details and signing instructions.
>
> A CLA check runs on every PR via `.github/workflows/cla.yml`. Make the "CLA Assistant" check **required** via GitHub branch protection rules on `develop` and release branches.

## Documents

- [Development](development.md) — test suite, local verification with `just`, git hooks with lefthook, and Ticketry.
- [Software delivery lifecycle](sdlc.md) — branch flow (`develop` / `release/current`), release validation, and manual GitHub release publishing.
- [Release version numbers](release-version-numbers.md) — how the release workflow picks the next version, and how to force a specific bump (e.g. `1.0.0`).
- [Windows signing runbook](windows-signing.md) — Azure Artifact Signing setup, release signing order, and Defender false-positive follow-up.

See also the files at the repository root:

- [CONTRIBUTING.md](../CONTRIBUTING.md) — how to contribute, including the CLA requirement.
- [CLA.md](../CLA.md) — the Contributor License Agreement that all contributors must sign.
- `AGENTS.md` — ticketry workflow and agent instructions.
- `CLAUDE.md` — project-specific development rules (terraform, logs as XML, just, etc.).
- `justfile` at repo root — primary task runner.

## Maintainer notes: CLA enforcement

- The CLA requirement is implemented via:
  - `CLA.md` — the legal text
  - `CONTRIBUTING.md` and `.github/PULL_REQUEST_TEMPLATE.md`
  - `.github/workflows/cla.yml` using `contributor-assistant/github-action`
- To fully require it:
  1. In GitHub → Settings → Branches, protect `develop` (and `release/*` if desired).
  2. Add "CLA Assistant" (or the job name from the workflow) as a **required** status check.
  3. Optionally also configure https://cla-assistant.io/ (point it at a Gist containing the contents of `CLA.md` or the raw `CLA.md` URL) for a nicer "Sign" button UX in PRs. The GitHub Action and cla-assistant.io can coexist.
- The action creates/uses a `cla-signatures` branch to store signature records (you can rename or configure via the workflow).
- Update `allowlist` in `cla.yml` as team members change.
