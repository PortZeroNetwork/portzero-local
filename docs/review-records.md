# Review records — tie feedback to code

Reviewers can pin comments directly on your tunneled app while browsing it
(the feedback widget on `*.tunnel.portzero.cloud` domains). Each comment
thread gets a short ref like `PZ-42`. A **review record** ties those threads
to the code that changed: it captures your branch — commit list, unified diff,
base and head SHAs — plus the tunnel domain hosting the live app, and uploads
it to portzero.cloud.

The dashboard then shows the diff, the commits, and every feedback thread on
that domain side by side, at `https://app.portzero.cloud/#/review-records`.

## `portzero review`

Run it from your branch:

```sh
portzero review
```

It requires `portzero login`, then:

1. Resolves the base ref: `--base <ref>` if given, else origin's default
   branch, else `main`.
2. Collects the commits and the diff from `merge-base HEAD <base>` to `HEAD`.
   Diffs over 5 MB are rejected — upload a smaller branch or pass a nearer
   `--base`.
3. Picks the cloud tunnel domain hosting the live app: `--domain <domain>` if
   given, else a discovered cloud tunnel matching the project or branch name,
   else the only discovered cloud tunnel.
4. Uploads the record and prints its dashboard URL. `--open` opens it in a
   browser.

Flags: `--base <ref>`, `--domain <domain>`, `--project <name>`, `--open`.

Re-running on the same branch updates the open record in place — one open
record per branch.

## The `Fixes PZ-<n>` convention

Mention a thread ref in a commit message to mark it fixed:

```sh
git commit -m "Fix checkout button contrast (Fixes PZ-42)"
portzero review
```

When the review record uploads, the cloud scans commit messages for
`Fixes PZ-<n>` (also `Closes` / `Resolves`) and advances each referenced open
thread to **fix_proposed**, recording the commit SHA and subject line. The CLI
prints each advanced thread:

```
PZ-42: fix proposed (abc1234)
```

## Humans resolve, not machines

A proposed fix is a claim, not a resolution. A `fix_proposed` thread awaits
human confirmation:

- The **commenter** sees a "Marked fixed" banner on their pin in the live app
  and clicks *Confirm fixed* — or *Still broken*, which reopens the thread.
- A **team member** can resolve or reopen it from the dashboard.

Threads therefore move `open → fix_proposed → resolved`, with provenance
recorded at each step.

## For AI coding agents

The [MCP server](mcp.md) exposes the same workflow: `list_feedback` lists the
threads (refs, routes, comment bodies), and `propose_fix` marks one fixed by a
specific commit. Or skip the tool call entirely: commit with `Fixes PZ-<n>`
and run `portzero review`.
