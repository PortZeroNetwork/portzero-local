# Contributing to Port Zero Local

Thank you for your interest in contributing to Port Zero Local!

All contributions are made under the terms of our [Contributor License Agreement (CLA)](CLA.md). By submitting a contribution, you agree to the CLA.

## Requirements

- You **must** sign the CLA before your pull request can be merged.
- See [CLA.md](CLA.md) for the full agreement and signing instructions.

## Getting Started

1. Read the development documentation: [docs/dev/README.md](docs/dev/README.md)
2. Review our [development guide](docs/dev/development.md), [SDLC](docs/dev/sdlc.md), and other docs under `docs/dev/`.
3. Follow the project's coding and commit conventions (see `AGENTS.md` and `CLAUDE.md` at the repository root).

## Pull Request Process

1. Fork the repository and create a feature branch from `develop`.
2. Make your changes.
3. Ensure tests and checks pass (`just` is the primary task runner; see docs).
4. Open a Pull Request against the `develop` branch.
5. **Sign the CLA** by commenting on your PR:
   ```
   I have read the CLA Document and I hereby sign the CLA
   ```
6. Address any review feedback.

A GitHub Actions workflow will automatically verify the CLA signature and report a status check named **"CLA Assistant"**. Pull requests without a passing CLA check will not be merged.

> **Maintainers:** To enforce the CLA, enable the "CLA Assistant" status check as a required check in the branch protection rules for `develop` (and `release/*` branches). See `.github/workflows/cla.yml` and the [CLA Assistant action docs](https://github.com/contributor-assistant/github-action).

## Code of Conduct

Be respectful and constructive. We follow standard open-source community norms.

## Questions?

Open an issue or start a discussion in the repository.

---

Port Zero Local is developed by Loum Technologies.