# Hosted Cloud CLI

`buc` is the hosted Bucephalus Cloud product CLI. It talks to Cloud APIs only.
It does not run local Core builds, start local runners, or manage Cloud operator
pools.

## Current Boundary

Today, authoring YAML is still compiled by local Core:

```bash
bucephalus build experiment.yaml --out .bucephalus-package
```

Then `buc` takes over the hosted workflow:

```bash
buc build .bucephalus-package
buc doctor <package-digest> --secret-ref NAME=provider://ref
buc run <package-digest> --secret-ref NAME=provider://ref
```

Passing YAML to `buc build` fails before any API call. That is intentional until
the hosted API implements real YAML authoring builds.

## Setup

Log in once and persist the hosted API URL:

```bash
bucephalus login --resource <api-url>
```

You can also pass credentials per command:

```bash
buc --api-url <api-url> --user-token <token> health
```

Environment variables:

| Variable | Meaning |
| --- | --- |
| `BUCEPHALUS_CLOUD_API_URL` | Hosted API base URL. |
| `BUCEPHALUS_CLOUD_USER_TOKEN` | OAuth/API bearer token override. |

## Commands

Use the top-level workflow commands for day-to-day work:

```bash
buc health
buc build <package-dir-or-package.tgz>
buc inspect <package-digest>
buc doctor <package-digest> --secret-ref NAME=provider://ref
buc run <package-digest> --secret-ref NAME=provider://ref
```

Long-form noun commands are equivalent:

```bash
buc packages upload <package-dir-or-package.tgz>
buc packages inspect <package-digest>
buc experiments build <package-dir-or-package.tgz>
buc experiments doctor <package-digest> --secret-ref NAME=provider://ref
buc runs create <package-digest> --secret-ref NAME=provider://ref
buc runs get <run-id>
```

## End-To-End Hosted Run

1. Build and seal the package locally:

   ```bash
   bucephalus build experiment.yaml --out .bucephalus-package
   ```

2. Upload/import it into Cloud:

   ```bash
   buc build .bucephalus-package
   ```

   The command returns a `package_digest`. If Cloud package inspection fails,
   `buc` exits non-zero and prints importer diagnostics.

3. Inspect required secrets:

   ```bash
   buc inspect <package-digest>
   ```

4. Doctor the exact hosted run inputs:

   ```bash
   buc doctor <package-digest> \
     --secret-ref GEMINI_API_KEY=gcp-secret-manager://projects/<project>/secrets/<secret>/versions/latest
   ```

   Doctor checks package acceptance, secret refs, image portability, network
   requirements, architecture/resources, and active runner-pool schedulability.

5. Queue the run:

   ```bash
   buc run <package-digest> \
     --secret-ref GEMINI_API_KEY=gcp-secret-manager://projects/<project>/secrets/<secret>/versions/latest
   ```

6. Fetch status:

   ```bash
   buc runs get <run-id>
   ```

## Secret Refs

Pass refs inline:

```bash
buc doctor <package-digest> --secret-ref GEMINI_API_KEY=gcp-secret-manager://projects/<project>/secrets/gemini/versions/latest
```

Or via YAML/JSON:

```yaml
GEMINI_API_KEY: gcp-secret-manager://projects/<project>/secrets/gemini/versions/latest
```

```bash
buc run <package-digest> --secret-ref-file secrets.yaml
```

## What `buc build` Does

`buc build` currently means:

1. Verify obvious local package shape for directory inputs.
2. Archive the sealed package directory when needed.
3. Create a Cloud upload.
4. Upload package bytes.
5. Complete the upload.
6. Call `POST /v1/experiments/builds`.
7. Fail the CLI command if Cloud package inspection did not accept the package.

The API response includes `build_kind: sealed_package_import` and
`authoring_build.status: unavailable` so clients do not confuse this with future
hosted YAML compilation.

## Operator Boundary

`bucephalus-cloud` is an internal operator utility for service and runner-pool
administration. Product workflows belong in `buc`.
