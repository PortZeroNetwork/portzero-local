# Installer getting-started manifest

This directory holds the checked-in JSON that feeds the local
"Getting Started" section.

- `getting-started.json` is generated from `../portzero-examples/manifest`
  by `just collect` in the examples repo.
- The installer and local UI should read this file directly instead of
  duplicating example metadata.

