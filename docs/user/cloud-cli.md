# Hosted Cloud CLI

`buc` is the hosted Bucephalus Cloud product CLI. It talks to Cloud APIs only.
It does not run local Core builds, start local runners, or manage Cloud operator
pools. Hosted authoring builds run the version-matched Core binary inside the
Cloud API environment.

## Current Boundary

`buc build` accepts either authoring YAML or a sealed package:

```bash
buc build experiment.yaml
buc build experiments/peter/experiment.yaml
buc build .bucephalus-package
```

For YAML, `buc` requires a `bucephalus.project.yaml` or
`bucephalus.project.yml` file above the entrypoint. That manifest is the source
of truth for the uploaded authoring context:
project id, package source, declared entrypoints, include/exclude rules, and the
hosted Cloud target. The API runs bundled Core in an isolated workspace, imports
the produced sealed package, and checks the package against the hosted Cloud
target. For package directories or archives, `buc` uploads/imports the sealed
package directly and runs the same hosted readiness checks.

The YAML authoring context is a Cloud build input, not a raw directory sync.
`buc` excludes local generated and credential material such as `.env`, `.env.*`,
`.npmrc`, `.pypirc`, `.netrc`, `.ssh`, `.aws`, `.docker`, `.config/gcloud`,
`node_modules`, and `target` before upload. The hosted API rejects the same
paths if a context archive is crafted outside the CLI. Use hosted secrets with
`buc secrets put` and pass `bucephalus://NAME` refs to `doctor`/`run`; do not
upload local credential files as build inputs.

Minimal `bucephalus.project.yaml`:

```yaml
schema_version: bucephalus_project_v1
project:
  id: my_evals
package_sources:
  default:
    root: .
    entrypoints:
      - experiment.yaml
      - experiments/peter/experiment.yaml
    include:
      - experiment.yaml
      - cases.jsonl
      - experiments/peter/**
      - shared/**
    exclude:
      - generated/**
targets:
  hosted_cloud: {}
```

For nested experiments, keep the manifest at the shared project root and list
the nested YAML as an entrypoint. Do not use command-line root overrides; the
manifest is the build boundary.

```bash
buc secrets put NAME --from-env NAME
buc doctor <package-digest> --secret-ref NAME=bucephalus://NAME
buc run <package-digest> --secret-ref NAME=bucephalus://NAME
```

## Setup

Log in once and persist the hosted API URL:

```bash
buc login
```

Hosted login opens a browser OAuth flow and listens on a local loopback
callback. No hosted user should need to provide `--api-url`; that option is only
for development, staging, or self-hosted Cloud. The hosted API publishes the CLI
OAuth client, scope, and server-side authorization-code exchange path from
`/v1/auth/config`.

`buc` sends the browser authorization code and PKCE verifier back to the hosted
API, which performs the OAuth token exchange with its own server-side secret and
returns a Bucephalus session token. The CLI never needs the OAuth client secret
and never asks hosted users for issuer, audience, or internal API endpoint
details.

`buc` then reads the shared Cloud profile and cached auth files from
`BUCEPHALUS_HOME`: `cloud.json`, `auth/cloud_user_token`, and
`auth/cloud_user_token.json`. If a self-hosted OAuth cache includes
`auth/cloud_refresh_token`, `buc` can refresh the access token before making
Cloud API calls.

Automation can also pass a token per command:

```bash
buc --user-token <token> health
```

`--api-url` is for development, staging, and self-hosted Cloud overrides. The
installed hosted product default is baked into the release from
`BUCEPHALUS_HOSTED_API_URL`.

Environment variables:

| Variable | Meaning |
| --- | --- |
| `BUCEPHALUS_CLOUD_API_URL` | Hosted API base URL. |
| `BUCEPHALUS_CLOUD_USER_TOKEN` | OAuth/API bearer token override. |
| `BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY` | API-side build provenance policy: `warn` for local/dev, `enforce` for managed production. |

## Commands

Use the top-level workflow commands for day-to-day work:

```bash
buc login
buc auth status
buc health
buc author canonicalize experiment.yaml
buc author resolve experiment.yaml
buc author validate experiment.yaml --validation-level launch_hint
buc build <experiment.yaml-or-package>
buc packages list
buc inspect <package-digest>
buc secrets put NAME --from-env NAME
buc secrets list
buc doctor <package-digest> --secret-ref NAME=bucephalus://NAME
buc run <package-digest> --secret-ref NAME=bucephalus://NAME
buc runs list
buc runs events <run-id>
buc runs results <run-id>
buc runs value <run-id> <key>
buc logout
```

Long-form noun commands are equivalent:

```bash
buc drafts canonicalize <draft.yaml-or-json>
buc drafts resolve <draft.yaml-or-json>
buc drafts validate <draft.yaml-or-json> --validation-level package
buc drafts suggest <draft.yaml-or-json> --target variant
buc drafts diff <left-draft.yaml> <right-draft.yaml>
buc packages list
buc packages upload <package-dir-or-package.tgz>
buc packages inspect <package-digest>
buc secrets list
buc secrets put <name> --from-env <env-var>
buc secrets delete <name>
buc experiments build <experiment.yaml-or-package>
buc experiments doctor <package-digest> --secret-ref NAME=bucephalus://NAME
buc runs list
buc runs create <package-digest> --secret-ref NAME=bucephalus://NAME
buc runs get <run-id>
buc runs runtime <run-id>
buc runs events <run-id>
buc runs results <run-id>
buc runs value <run-id> <key>
```

## End-To-End Hosted Run

1. Validate authoring shape through the hosted authoring API:

   ```bash
   buc author canonicalize experiment.yaml
   buc author resolve experiment.yaml
   buc author validate experiment.yaml
   buc author validate experiment.yaml --validation-level package
   buc author validate experiment.yaml --validation-level launch_hint
   ```

   `authoring` validation catches draft structure and registry reference issues.
   `package` adds checks for packaging inputs such as case sources, variant
   identity, relative build-context paths, and secret mount shape. `launch_hint`
   adds non-fatal hosted-run guidance, such as required `--secret-ref` values,
   network capability hints, and local image rewrite warnings. These commands do
   not upload the authoring context or prove file existence; hosted build is the
   first step that sees the complete upload boundary.

2. Build for hosted Cloud:

   ```bash
   buc build experiment.yaml
   ```

   For nested experiments that reference shared repository files, declare the
   upload boundary in `bucephalus.project.yaml`:

   ```bash
   buc build experiments/peter/experiment.yaml
   ```

   The command returns a `package_digest`. If authoring build, package import,
   or hosted readiness fails, `buc` exits non-zero and prints the failed stage.
   When hosted readiness is `cloud_runnable`, the summary prints concrete
   follow-up commands: hosted secret upload commands when needed, `buc doctor
   <package-digest>`, and `buc run <package-digest>` with the matching
   `--secret-ref NAME=bucephalus://NAME` arguments.

3. Inspect required secrets:

   ```bash
   buc inspect <package-digest>
   ```

4. Upload hosted secrets for any required secret names:

   ```bash
   buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY
   ```

   Other non-leaky value sources are supported:

   ```bash
   buc secrets put GEMINI_API_KEY --value-file ./gemini.key
   printf '%s' "$GEMINI_API_KEY" | buc secrets put GEMINI_API_KEY --stdin
   ```

   The CLI and API never print secret plaintext or backing provider refs. The
   returned ref is `bucephalus://GEMINI_API_KEY`.

5. Doctor the exact hosted run inputs:

   ```bash
   buc doctor <package-digest> \
     --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY
   ```

   Doctor checks package acceptance, secret refs, image portability, network
   requirements, architecture/resources, and active runner-pool schedulability.

6. Queue the run:

   ```bash
   buc run <package-digest> \
     --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY
   ```

7. Fetch status:

   ```bash
   buc runs get <run-id>
   buc runs events <run-id>
   buc runs results <run-id>
   buc runs value <run-id> <key>
   ```

## Secret Refs

Hosted secrets are the product path for user-provided credentials. Upload or
rotate a value once:

```bash
buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY
```

List metadata without values:

```bash
buc secrets list
```

Then pass the hosted ref inline:

```bash
buc doctor <package-digest> --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY
```

Or via YAML/JSON:

```yaml
GEMINI_API_KEY: bucephalus://GEMINI_API_KEY
```

```bash
buc run <package-digest> --secret-ref-file secrets.yaml
```

Delete a hosted secret when it should no longer be usable:

```bash
buc secrets delete GEMINI_API_KEY
```

Provider-native refs are still accepted when the Cloud control plane is allowed
to resolve them directly:

```bash
buc doctor <package-digest> --secret-ref GEMINI_API_KEY=gcp-secret-manager://projects/<project>/secrets/gemini/versions/latest
```

## What `buc build` Does

`buc build` currently means:

1. Classify the input as authoring YAML or sealed package.
2. For YAML, find `bucephalus.project.yaml` or `bucephalus.project.yml` by
   walking upward from the entrypoint. The manifest must declare
   `schema_version: bucephalus_project_v1`, `project.id`,
   `targets.hosted_cloud`, and the entrypoint in exactly one package source.
   Each package source must declare non-empty include patterns. The archive root
   is the manifest directory, not the YAML parent. The archive contains only
   declared files plus the manifest and entrypoint, minus
   excluded/generated/credential material such as `.git`, `.env*`, `target`,
   and `node_modules`. The CLI also
   preflights the context before upload with the same default limits as the API:
   10,000 archive entries and 256 MiB expanded bytes. Operators can tune those
   limits with
   `BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_ENTRIES` and
   `BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES`.
3. For sealed package directories, verify obvious package shape and archive the
   package when needed. The hosted API performs the authoritative import check:
   `manifest.json`, `checksums.json`, `package.lock`, `package_checks.json`,
   and `staging_manifest.json` must match the current sealed package contract.
   Cloud import rejects failed package checks, unsealed payload files, broken
   package digests, missing CAS blobs, and runtime staging destinations outside
   the runner contract roots
   `__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support` and
   `/bucephalus/in/runtime`.
4. Create a Cloud upload, upload bytes, and complete the upload.
5. Call `POST /v1/experiments/builds`. The referenced upload is resolved under
   the authenticated Cloud owner; knowing another user's upload id is not a
   build capability.
6. For authoring contexts, the API runs bundled Core in an isolated workspace
   and imports the produced sealed package through the same sealed-package
   contract checks used for direct package uploads.
7. Evaluate the accepted package against the hosted Cloud target and the exact
   runtime options supplied on the command line, such as `--backend`, `--arch`,
   `--isolation`, `--cpu-count`, `--memory-mb`, `--disk-mb`, and
   `--max-parallel-trials`.

   Hosted runtime options are closed over the Cloud API contract. Unknown keys
   and malformed values are rejected instead of ignored, so a typo such as
   `memory_mbb` cannot silently fall back to the default runner size. The
   supported keys are `backend`, `executor`, `arch`, `cpu_count`, `cpu`,
   `memory_mb`, `disk_mb`, `isolation`, `timeout_ms`,
   `max_parallel_trials`, `network`, `sidecars`, and `accelerators`.
   With `--runtime-option`, scalar keys use `KEY=VALUE`, list keys use
   comma-separated values such as `sidecars=redis,postgres`, and `network`
   uses a JSON object such as
   `network={"default":"allowlist_enforced","egress":["api.openai.com"]}`.
   Do not provide both aliases for the same meaning: use `backend` rather than
   `executor`, and `cpu_count` rather than `cpu`, when both could apply.
   Hosted `buc` does not accept `--smoke-test` until Cloud has a real hosted
   smoke-test primitive.
8. Fail the CLI command if authoring build/package inspection did not pass
   or if the hosted target checks report `cloud_blocked`.

Hosted Core builds receive an isolated `BUCEPHALUS_HOME`/`HOME`, a stable
non-secret `USER`/`USERNAME` builder identity, build-owned `TMPDIR`/`TMP`/`TEMP`,
and a minimal process environment. Cloud API database URLs, worker tokens, and
service secrets are not forwarded into the authoring build. Hosted Core
stdout/stderr tails are redacted before they leave the API. Hosted authoring
builds are also bounded by `BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS` on the
API service, defaulting to 10 minutes; a timeout returns a failed build with
`authoring_build.code: authoring_build_timed_out` and no imported package.
If Core exits successfully but does not create a usable package output, the API
also returns a failed build without importing anything, using
`authoring_build_missing_package`, `authoring_build_invalid_package`, or
`authoring_build_empty_package`.

The API response includes `build_kind: hosted_authoring_build` for YAML inputs
and `build_kind: sealed_package_import` for package inputs. It also includes
`build_environment` and `cloud_readiness`.

`build_environment` is the provenance/contract for the hosted build/import. It
reports the hosted target, immutable source upload evidence (`input_kind`,
`upload_id`, source archive/package `content_digest`, byte size, authoring
entrypoint, and project manifest evidence when applicable), the runtime option
object checked for readiness,
builder/importer image digest when the deployment provides one, release/git
metadata when available, and the package/readiness schema contract. For YAML
authoring inputs, `builder.kind` is `hosted_authoring_builder` and
`core.executed` is `true` with the bundled Core command/version/path and
timeout used by the API. For sealed package inputs, `builder.kind` is
`sealed_package_importer` and `core.executed` is `false`; Cloud imported an
already-built package and checked hosted readiness without claiming hosted Core
authored it. If this object is absent, the response is not a complete hosted
build result. The CLI also checks that the hosted response is
about the source it just uploaded: `build_environment.source.upload_id`,
`content_digest`, and `byte_size` must be present and match the upload created
by the command and the local archive bytes. For authoring YAML inputs,
`build_environment.source.entrypoint` must also be present and match the
entrypoint sent by the command. `build_environment.source.project_manifest`
records the manifest path, digest, project id, package source, source root, and
entrypoint used for the upload. `build_environment.runtime_options` and
`cloud_readiness.runtime_options` must also match the runtime options requested
by the command. The hosted target must be `hosted_cloud/default`, and the
package contract must match the requested input kind, `sealed_run_package_v2`,
and `hosted_cloud_readiness_v1` with Cloud readiness required. For successful
hosted authoring builds, `package_contract.authoring_compiler` is
`core_universal_v1`, and `authoring_build.source_upload_id` plus
`authoring_build.entrypoint` must also match the upload and entrypoint sent by
the CLI. The same contract reports
`package_contract.authoring_provenance.status=hosted_attested` and
`source=hosted_core`.

For sealed package imports, `package_contract.authoring_compiler` is `null`,
`package_contract.authoring_provenance.status=external_unattested`, and
`source=sealed_package_manifest`. That means Cloud verified the sealed package's
integrity and checked hosted readiness, but `sealed_run_package_v2` does not
attest the package's original local authoring environment, Core version,
platform, or target. `authoring_build.status` must be `unavailable`, because the
Cloud API imported an already sealed package instead of compiling authoring YAML.
When an import object is present, its `import_id`
must match `build_id`, and its `package_digest` must agree with the build-level
package digest.

Cloud also persists package-level provenance on the accepted package artifact.
`buc inspect`, `buc doctor`, `buc run`, and `buc runs get` surface this as
`package_provenance`. Hosted YAML builds keep
`package_provenance.status=hosted_attested`; sealed package imports keep
`package_provenance.status=external_unattested`. Existing rows created before
this contract use `status=unknown_legacy` instead of being silently upgraded.
Because package digests are content identities shared across users and uploads,
Cloud stores provenance on the owner/package association too; one user's sealed
package import cannot overwrite another user's hosted-attested provenance for
the same digest. Worker package downloads also resolve storage metadata through
the run owner's package association, so a later same-digest upload cannot move a
runner onto another owner's storage pointer.
The nested `evidence` object says which policy was applied and whether those
provenance fields are `complete` or `partial`. Local development defaults to
`policy: warn`: partial evidence does not by itself mean the package cannot run,
but it weakens the build's production audit trail and is surfaced as
`build_environment` warnings inside `cloud_readiness.checks`. Managed production
deployments set `BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY=enforce`; under that
policy, partial evidence turns an otherwise runnable package into
`cloud_blocked` with an operator action to complete build-environment evidence.
A production deployment should report complete evidence: immutable builder/API
image digest, release version, release git SHA, and, for hosted authoring
builds, hosted Core version. The managed GCP deployment injects the API image digest into this object from the
immutable Cloud Run image ref used for the service.

The current hosted authoring compiler is `core_universal_v1`: the API runs the
same package compiler shipped with Core, inside the hosted build environment.
The Cloud-specific guarantee comes from the required `hosted_cloud_readiness_v1`
gate after package import. If the product later needs target-specific lowering,
that compiler target must become an explicit API/schema field and be recorded in
`build_environment`; it should not be hidden behind flags or ambient local state.

`cloud_readiness` is the part that says whether the package is actually
runnable in that hosted Cloud target:

- `cloud_runnable`: package imported, image refs/resources/network/isolation map
  to the hosted runtime, and an active runner pool can satisfy it.
- `cloud_blocked`: the package imported, but some hosted runtime contract failed
  such as a local image ref, unsupported backend/arch/isolation/network setting,
  or no active runner capacity.
- `unavailable`: package import failed, so hosted readiness could not run.

Runtime secrets are reported as run-time requirements. A build can be
`cloud_runnable` while still warning that `buc run` must supply matching
`--secret-ref` values.

Plain run environment values are separate from secrets. Use `buc run ... --env
PUBLIC_MODE=smoke` only for non-secret configuration that can appear in run
metadata and CLI/API payloads. Hosted Cloud accepts only uppercase shell-style
env names matching `[A-Z_][A-Z0-9_]*`, rejects names reserved for Cloud
runtime/control-plane state such as `DATABASE_URL`,
`BUCEPHALUS_CLOUD_WORKER_TOKEN`, runner/store/resolver variables, generic
provider credential variables, and rejects any env key that also appears in
`--secret-ref`. Credentials belong in hosted secrets:
`buc secrets put NAME --from-env NAME` followed by
`buc run <digest> --secret-ref NAME=bucephalus://NAME`.

The readiness object also includes `required_actions`. These are the canonical
next steps for clients and UI surfaces:

- `stage: before_run` actions mean the package is build-valid, but the user
  must complete setup before creating a run. Runtime secrets use this state and
  include commands like `buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY`.
- `stage: before_rebuild` actions mean the experiment/package must change and
  be rebuilt, for example replacing a local image ref with a digest-pinned
  registry image.
- `stage: operator` actions mean hosted infrastructure must change, such as
  adding runner capacity for the requested resources.

## Operator Boundary

`bucephalus-cloud` is an internal operator utility for service and runner-pool
administration. Product workflows belong in `buc`.

Runner-pool administration uses `BUCEPHALUS_CLOUD_RUNNER_ADMIN_TOKEN` when it is
configured. Worker daemons use `BUCEPHALUS_CLOUD_WORKER_TOKEN` for registration,
heartbeats, queue claims, package downloads, and attempt updates. If no runner
admin token is configured, the worker token remains the compatibility admin
credential. Once the admin token is configured, worker-token headers no longer
authorize runner-pool administration. The HTTP API accepts the admin credential
as `Authorization: Bearer ...` or `X-Bucephalus-Runner-Admin-Token`; worker
routes accept bearer or `X-Bucephalus-Worker-Token`.

On GCP deploys, `runner_admin_token_secret_version` injects the admin token into
the API service only. The pool controller and runner VMs continue to receive the
worker token but not the runner-admin credential.
