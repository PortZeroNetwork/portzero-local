# Windows signing runbook

This document tracks the Windows signing setup for public Port Zero releases.

## Current state

- Windows Defender is deleting the unsigned Windows release artifacts on Windows
  11.
- The old MSI installed into `C:\Program Files (x86)\Port Zero` because the WiX
  package was not marked as x64.
- The installer has been updated locally to install into 64-bit Program Files,
  add the install folder to machine `PATH`, and package `portzero.exe` plus
  `wintun.dll`.
- Windows autostart has been updated locally to run:

  ```text
  portzero.exe start --foreground
  ```

  instead of the nonexistent `daemon` subcommand.

- Azure Artifact Signing is the chosen signing path.
- Loum Technologies has completed Microsoft identity validation.
- The release workflow signs `portzero.exe` before creating the Windows ZIP,
  then signs the MSI before generating the winget manifest and uploading
  artifacts.

## Desired release flow

The Windows release pipeline should do this in order:

1. Build the Windows ZIP payload containing `portzero.exe` and `wintun.dll`.
2. Sign `portzero.exe`.
3. Verify the `portzero.exe` Authenticode signature.
4. Build the MSI from the signed payload.
5. Sign the MSI.
6. Verify the MSI Authenticode signature.
7. Upload the signed ZIP and signed MSI as release artifacts.
8. Submit the first signed artifacts to Microsoft as false positives if Defender
   still flags them.

The EXE must be signed before MSI packaging so the installed binary is signed.
The MSI must also be signed so Windows trusts the installer container.

## Azure Artifact Signing resources

Fill these in after verification completes:

```text
Azure subscription:
Resource group:
Artifact Signing account:
Region:
Endpoint:
Identity validation name: Loum Technologies, LLC
Identity validation ID: b09e34f4-c92a-410e-a085-753a19b81ee9
Certificate profile name:
Certificate profile type: Public Trust
GitHub Actions principal / managed identity:
```

Use a **Public** identity validation and a **Public Trust** certificate profile
for public releases. Private trust is only for internal enterprise policy
signing and will not solve public Windows install/download trust.

## Required Azure roles

For identity validation in the Azure Portal, the interactive user needs:

```text
Artifact Signing Identity Verifier
Reader
```

For signing from GitHub Actions, the workflow identity needs:

```text
Artifact Signing Certificate Profile Signer
```

Assign the signing role at the certificate profile scope if possible:

```text
/subscriptions/<subscription-id>/resourceGroups/<resource-group>/providers/Microsoft.CodeSigning/codeSigningAccounts/<account>/certificateProfiles/<profile>
```

Azure role assignments may take several minutes to propagate.

## GitHub Actions values

Prefer GitHub OIDC / Azure federated credentials over long-lived client secrets.
Store these as GitHub Actions variables:

```text
AZURE_CLIENT_ID
AZURE_TENANT_ID
AZURE_SUBSCRIPTION_ID
ARTIFACT_SIGNING_ENDPOINT
ARTIFACT_SIGNING_ACCOUNT
ARTIFACT_SIGNING_PROFILE
```

The endpoint is region-specific, for example:

```text
https://eus.codesigning.azure.net
```

The Azure app registration used by GitHub Actions needs a federated credential
for this repository and the release branch/ref used to publish releases. The
release workflow grants `id-token: write`, authenticates with `azure/login`,
and signs with `azure/artifact-signing-action`.

## Signing command shape

Artifact Signing integrates with `signtool.exe` through the Azure Code Signing
dlib. The final workflow should generate a metadata file like:

```json
{
  "Endpoint": "<ARTIFACT_SIGNING_ENDPOINT>",
  "CodeSigningAccountName": "<ARTIFACT_SIGNING_ACCOUNT>",
  "CertificateProfileName": "<ARTIFACT_SIGNING_PROFILE>"
}
```

Then sign files with:

```powershell
signtool sign `
  /v `
  /fd SHA256 `
  /tr "http://timestamp.acs.microsoft.com" `
  /td SHA256 `
  /dlib "<path-to-Azure.CodeSigning.Dlib.dll>" `
  /dmdf ".\metadata.json" `
  "<file-to-sign>"
```

Verify with:

```powershell
signtool verify /pa /v "<signed-file>"
Get-AuthenticodeSignature "<signed-file>"
```

Expected verification result:

```text
Status: Valid
```

## Microsoft Defender submission

If Defender still detects the first signed build, submit the exact signed files
to Microsoft as a software developer false positive:

```text
https://www.microsoft.com/en-us/wdsi/filesubmission
```

Submit:

- signed `portzero.exe`
- signed MSI
- signed ZIP payload, if the ZIP is also flagged

Suggested context:

```text
Port Zero is a local development networking CLI. It creates a local overlay and
uses wintun.dll for Windows TUN support. These are official signed release
artifacts built from https://github.com/PortZeroNetwork/portzero-local.
```

## Local diagnostic commands

Check Defender detections:

```powershell
Get-MpThreatDetection |
  Sort-Object InitialDetectionTime -Descending |
  Select-Object -First 10 InitialDetectionTime,ThreatName,Resources
```

Check signatures:

```powershell
Get-AuthenticodeSignature '.\portzero.exe'
Get-AuthenticodeSignature '.\portzero-*.msi'
```

Check installed files:

```powershell
Get-ChildItem 'C:\Program Files\Port Zero','C:\Program Files (x86)\Port Zero' -ErrorAction SilentlyContinue
```

















I want to build a set of minimal working examples for using PortZero with the following web frameworks and languages:

| **Language**        | **Framework**     | **Ecosystem Presence / Use Case**               | **Primary Integration Strategy** | **Target Interface / Hook**                    |
| ------------------- | ----------------- | ----------------------------------------------- | -------------------------------- | ---------------------------------------------- |
| **Go**              | **Gin**           | Most popular Go API framework; Martini-like API | Custom Handler Middleware        | `func(c *gin.Context)`                         |
| **Go**              | **Echo**          | High-performance, minimalist REST APIs          | Idiomatic HTTP Middleware        | `func(next echo.HandlerFunc) echo.HandlerFunc` |
| **Go**              | **Chi**           | Ultra-lightweight, 100% `net/http` compatible   | Standard HTTP Middleware         | `func(http.Handler) http.Handler`              |
| **Rust**            | **Axum**          | Modern default (Tokio ecosystem); declarative   | Tower Service / Layer            | `axum::middleware::from_fn`                    |
| **Rust**            | **Actix Web**     | Maximum throughput; actor-based routing         | Actix Transform Middleware       | `actix_web::dev::Transform` traits             |
| **TypeScript / JS** | **Express.js**    | Defacto standard for legacy & simple Node APIs  | Node Request Mutation            | `func(req, res, next)`                         |
| **TypeScript / JS** | **Next.js**       | King of full-stack React; edge-ready            | Edge/Server Middleware           | `middleware.ts` / Fetch interception           |
| **Python**          | **FastAPI**       | Modern default for REST APIs and AI wrappers    | ASGI Middleware                  | `BaseHTTPMiddleware` / Starlette Lifecycle     |
| **Python**          | **Django**        | Monolithic rapid prototyping / enterprise data  | Django Middleware Class          | `__call__(self, request)` pipeline             |
| **C#**              | **ASP.NET Core**  | Massive enterprise presence; cloud-native speed | Pipeline Execution Chain         | `app.Use(async (context, next) => { ... })`    |
| **Java**            | **Spring Boot**   | Corporate standard for massive scale backends   | Servlet Filter / Interceptor     | `OncePerRequestFilter` / `HandlerInterceptor`  |
| **Ruby**            | **Ruby on Rails** | The startup MVP classic; heavy convention       | Rack Middleware                  | `call(env)` method in the Rack stack           |
| **PHP**             | **Laravel**       | Modern PHP champion; huge SaaS presence         | HTTP Kernel Middleware           | `handle($request, Closure $next)`              |

For each one, I want a .sh script for how to use portzero with that language/framework on linux, a separate .sh script for how to use portzero with that language/framework on macos, and a .ps1 script for how to use portzero with that language/framework on windows. There won't be separate examples for each platform though. We will assume all these languages are cross-platform.

I also want two levels of each example: integration with portzero.local via PZ_TUNNEL environment variable (simple, assumes only one port) and integration with portzero.local via our sdk that is specific to that language and possibly specific to that framework. So yes--we'll need to include a package for each language at the very least, possible for each framework in some cases. These packages will all connect to portzero's local rest api to tell portzero about the web server.

I want two additional levels of each example: integration with portzero.cloud via PZ_TUNNEL environment variable and integration with portzero.cloud via our sdk that is specific to that language. Creating a portzero.cloud tunnel requires talking to the same endpoint on portzero.local incidentally. We will need to make sure all of this works.

Every combination of these will have its own example, and we will have github actions that verify that these examples all work. I want to verify that these examples all work using `just` task runner to avoid burning through my github actions minutes too quickly. This task runner should know which operating system I'm on; the just task will assume portzero is already installed. So yes there will be portzero.cloud tunnels created as part of this process. We will need to supply credentials to the Github actions so that this works.

These scripts will have comments in them and we will put them in a standard location, and there will be a `just` script which, when run, collects all these examples into a format that can be presented in the portzero.local Getting Started section. The Getting Started section there should feed off the json file that this `just` script builds and puts in the installer. This json file will be checked into portzero-local so that the Getting Started content can be right.

All of this will ensure that we do not accidentally ship a version of portzero that breaks any of our documentation.

The sdks will all go in the portzero-sdk github repo. These examples will all go in the portzero-examples github repo. I have created both.
