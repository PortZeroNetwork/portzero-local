# Cloud Tunnel Interop Tests

The release workflow verifies the built client against the real
`portzero-cloud` staging environment. These tests are consumers of staging;
they do not provision or mutate cloud infrastructure.

The default test account username is `tunnel-e2e`, so the public tunnel checks
use domains like `client-e2e-macos.tunnel-e2e.tunnel.devenvtools.top`. This
exercises the normal namespace rule: cloud tunnel domains must end with
`USERNAME.tunnel.<staging-domain>`.

## Required `staging` Environment Inputs

Configure these in the `portzero-local` GitHub Environment named `staging`:

- Variable `STAGING_DOMAIN`: staging base domain, for example
  `devenvtools.top`
- Secret `TEST_LOGIN_SEED_TOKEN`: bearer token for the staging-only
  `/auth/test-seed-login` endpoint

## Forbidden Inputs

Do not add these to `portzero-local` for interop tests:

- DigitalOcean tokens
- Cloudflare tokens
- Terraform backend credentials
- deploy-agent tokens
- GitHub tokens that can create, update, or delete GitHub Environments

If the tests need staging to be changed, make that change in `portzero-cloud`.
`portzero-local` should fail fast when staging is unavailable rather than
trying to stand staging up itself.
