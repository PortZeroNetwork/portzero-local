# Release version numbers

The release workflow (`.github/workflows/release.yml`, `Determine Version` job)
picks the next version by looking at the **tip commit message on `release/*`**
for markers — it does not read a target version number out of the message at
all. See [sdlc.md](sdlc.md#publish-a-release-manually) for the full release
flow.

```bash
if echo "$COMMIT_MSG" | grep -qE "BREAKING CHANGE|\[major\]"; then
  MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0
elif echo "$COMMIT_MSG" | grep -qE "^feat(\([^)]+\))?:|\[minor\]"; then
  MINOR=$((MINOR + 1)); PATCH=0
else
  PATCH=$((PATCH + 1))
fi
```

**Gotcha:** putting `1.0.0` in the commit message (e.g. `Release 1.0.0`) does
nothing — it matches none of the markers above, so it silently falls through
to a patch bump off the latest tag.

To force a specific bump, the tip commit on `release/*` needs one of the
markers, not the target number:

- Major bump (e.g. `0.0.12` → `1.0.0`): include `[major]` or `BREAKING CHANGE`
  in the commit message.
- Minor bump: include `[minor]` or a `feat(...):`-prefixed subject line.
- Anything else: patch bump.

Example commit message to go from `0.0.x` to `1.0.0`:

```
Release 1.0.0 [major]
```

The version is always computed from the latest **stable** `vX.Y.Z` tag that
exists **on origin** (`git tag -l "v[0-9]*.[0-9]*.[0-9]*" | grep -v -- '-' |
sort -V | tail -1` after a full fetch) — stray local-only tags that were never
pushed don't affect it, and prerelease tags (`vX.Y.Z-rc.N`) are filtered out so
they never perturb the next stable bump.

## Prereleases

A `workflow_dispatch` run of the same workflow (Actions → Release → Run
workflow, normally off `develop`) instead produces a prerelease versioned
`X.Y.Z-rc.<run-number>` and tagged/marked `prerelease: true`. Those builds are
the edge testing channel and never touch the stable install paths — see
[prerelease-channel.md](prerelease-channel.md).
