# Bucephalus Cloud

Prototype workspace for the hosted registry and analysis plane.

This directory is intentionally separate from Bucephalus Core. Core remains a
local-first runner by default, while allocated cloud workers act as Cloud API
clients: they claim attempts, download sealed packages, run Core, report events,
and upload bounded runtime snapshots without requiring direct Postgres
credentials on the runner VM.

Cloud is the optional product layer that remembers committed experiment facts
across runs:

- content-addressed registries for variants, cases, metrics, agents, graders,
  datasets, runtime profiles, and sealed packages
- idempotent ingestion of committed slot facts emitted by Core
- cross-run comparison keyed by stable content digests instead of run-local
  display names
- team dashboards, lineage, provenance, and authoring ergonomics

The core rule is:

> The runner must produce globally meaningful identities without requiring a
> global database.

Cloud may lag a local run, but it must never claim a result that Core did not
durably commit. Ingestion is therefore based on immutable slot commit records and
payload digests.

## Repository Boundary

This folder is not part of the shipped CLI crate. The root `Cargo.toml` uses an
explicit package `include` list, and this directory is not on it.

Nothing under `rust/crates/*` should import code from this directory. When this
plane has enough shape to stand alone, it should move into its own repository.

## Current Contents

- [api/](api/README.md) defines the hosted API primitive contracts for registry,
  draft authoring, package intake, runs, analysis, and live observability.
- [db/migrations/0001_registry_and_fact_store.sql](db/migrations/0001_registry_and_fact_store.sql)
  defines the first Postgres schema for the registry and committed-fact store.
- [docs/canonicalization.md](docs/canonicalization.md) defines the mechanical
  canonicalization vs. semantic normalization boundary.
- [docs/project-boundary.md](docs/project-boundary.md) defines the Cloud/Core
  project split and the rule that Core CLI invocation is a compatibility/testing
  concern unless Cloud later grows explicit managed runners.
- [docs/build-targets.md](docs/build-targets.md) defines the target contract:
  build targets an environment, and upload is materialization for that target.
- [docs/runner-vm-architecture.md](docs/runner-vm-architecture.md) defines the
  Cloud execution boundary: long-running runner VM daemons claim compatible
  runs and invoke Core.
- [docs/runtime-control-plane.md](docs/runtime-control-plane.md) defines the
  Path 3 runtime boundary: runners are API clients, user secrets are
  attempt-scoped, and direct runner database access is retired.
- [deploy/](deploy/README.md) records that the old local-VM, SSH, systemd,
  startup-script, and handwritten provider deployment surface is retired. The
  replacement goal-state lives in
  [../docs/specs/CLOUD_DEPLOYMENT_GOAL_STATE.md](../docs/specs/CLOUD_DEPLOYMENT_GOAL_STATE.md).
- [docs/golden-paths.md](docs/golden-paths.md) defines the primary Cloud user
  journeys: author experiments, register resources, build for a target, and run.
- [src/primitives/](src/primitives) contains the first executable primitives for
  canonicalization, digesting, normalization hints, and explicit reference
  resolution.
- [docker-compose.yml](docker-compose.yml) starts a local Postgres instance for
  schema iteration.

## Local Schema Iteration

```bash
cd bucephalus-cloud
docker compose up -d postgres
docker compose exec postgres psql -U bucephalus -d bucephalus_cloud \
  -f /migrations/0001_registry_and_fact_store.sql
```

## Primitive Library

```bash
cd bucephalus-cloud
bun install
bun test
bun run typecheck
```

## Cloud CLI

The Cloud client CLI talks to an explicit Bucephalus Cloud API. `deploy`
additionally invokes Core locally to build a sealed package before uploading the
package artifact.

```bash
export BUCEPHALUS_CLOUD_API_URL=https://<first-party-cloud-api>
bucephalus-cloud health
bucephalus-cloud draft validate --file ../cookbook/agent-eval/experiment.yaml
bucephalus-cloud draft preview --file ../cookbook/agent-eval/experiment.yaml
bucephalus-cloud draft export --file ../cookbook/agent-eval/experiment.yaml --out /tmp/bucephalus-cloud-export
bucephalus-cloud deploy ../cookbook/agent-eval/experiment.yaml --label smoke
```

User-facing Cloud APIs always require bearer auth. Set
`BUCEPHALUS_CLOUD_USER_TOKEN` or pass `--user-token` for registry, draft,
import, package, and run commands. Unauthenticated ownerless Cloud runs are not
supported. Runner pool and worker management commands intentionally use
`BUCEPHALUS_CLOUD_WORKER_TOKEN` or `--worker-token` instead.

Upload and inspect an already-built sealed package artifact:

```bash
bucephalus-cloud import sealed-package /tmp/package.tgz --label smoke
bucephalus-cloud import inspect <import-id>
bucephalus-cloud import inspect <import-id> --json
bucephalus-cloud package get sha256:...
bucephalus-cloud run create --package-digest sha256:... --backend runner-docker --env OPENAI_BASE_URL=https://api.openai.com
bucephalus-cloud run get <run-id>
```

## Cloud Worker Runtime

Cloud workers and pool controllers are deployment/runtime components. They must
be configured with the same explicit first-party Cloud API URL as the CLI:

```bash
BUCEPHALUS_CLOUD_API_URL=https://<first-party-cloud-api> \
BUCEPHALUS_WORKER_ID=worker-1 \
BUCEPHALUS_RUNNER_POOL_ID=<runner-pool-id> \
BUCEPHALUS_CLOUD_WORKER_TOKEN=<worker-token> \
BUCEPHALUS_WORKER_EXECUTORS=runner-docker \
BUCEPHALUS_WORKER_RESOURCES=core_runner,docker_daemon,registry_pull \
BUCEPHALUS_CORE_RUNNER_CMD=bucephalus \
bun run worker
```

The worker polls the Cloud API, claims runs, heartbeats its attempt lease,
downloads the sealed package through the API, and invokes Core. The production
shape is a long-running daemon on cloud-managed runner capacity, not a container
that happens to mount the host Docker socket.

Runs with `secret_refs` require an attempt-scoped resolver. Provider runner
images can use the bundled resolver entrypoint:

```bash
BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON='["bucephalus-cloud-secret-resolver"]'
```

The resolver supports GCP Secret Manager and AWS Secrets Manager refs via the
provider CLI installed in the runner image.

Users normally never write provider refs: the hosted secret store
(`PUT/GET/DELETE /v1/secrets/<name>`, write-only) accepts a value once and run
submissions reference it as `bucephalus://<name>`. The API translates hosted
refs to backing provider refs at run creation, so workers and the resolver see
only provider refs. Configure the backing store with
`BUCEPHALUS_CLOUD_SECRETS_BACKEND=gcp` and
`BUCEPHALUS_CLOUD_SECRETS_GCP_PROJECT=<project>` (the API service account needs
`secretmanager.admin` on that project, runner service accounts need
`secretmanager.secretAccessor`). The default `filesystem` backend is for local
development and pairs with `BUCEPHALUS_SECRET_RESOLVER_ALLOW_FILE=true` on the
resolver side.

Runs with declared network egress require an explicit network policy enforcer:

```bash
BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON='["/opt/bucephalus/apply-network-policy"]'
```

Workers only advertise `network_perimeter` when that command is configured.
The command is responsible for applying the provider/image-specific firewall,
proxy, or container-network policy before Core starts.

For real deployments, configure the API as an OAuth resource server with
`BUCEPHALUS_CLOUD_OAUTH_ISSUER`, `BUCEPHALUS_CLOUD_OAUTH_AUDIENCE`, and
`BUCEPHALUS_CLOUD_OAUTH_JWKS_URL`. For Google user auth, the audience is the
user OAuth client ID, and the JWKS URL is
`https://www.googleapis.com/oauth2/v3/certs`. Setting
`BUCEPHALUS_CLOUD_AUTH_REQUIRED=false` is rejected at startup.

The identity-provider JWT is only the sign-in credential. Clients exchange it
once via `POST /v1/auth/sessions` for a Bucephalus session token (opaque
`buc_` bearer, hashed at rest, sliding 30-day expiry, revocation-checked on
every request), and `POST /v1/auth/api-keys` mints non-expiring tokens for
scripts and CI. Both authenticate every `/v1` route interchangeably with
OAuth JWTs; see `api/openapi/auth.yaml`.

## Web UI

The web console lives in the separate `bucephalus-frontend` repository. That
repo owns the Vite build, Cloudflare Worker shell, and frontend CI/CD. This
backend keeps only the API and worker runtime.

The dev API currently implements the first registry vertical slice:

- `POST /v1/registry/canonicalize`
- `POST /v1/registry/review`
- `POST /v1/registry/objects`
- `GET /v1/registry/objects/:digest`
- `GET /v1/registry/search`
- `POST /v1/registry/resolve`
- `POST /v1/registry/aliases`

And the first authoring hot-path slice:

- `POST /v1/drafts/canonicalize`
- `POST /v1/drafts/resolve`
- `POST /v1/drafts/validate`
- `POST /v1/drafts/preview-schedule`
- `POST /v1/drafts/export`

And the first explicit package artifact intake slice:

- `POST /v1/uploads`
- `PUT /v1/uploads/:upload_id/content`
- `POST /v1/uploads/:upload_id/complete`
- `POST /v1/imports/sealed-package`
- `GET /v1/imports/:import_id`

And the first Cloud run-record slice:

- `GET /v1/packages/:package_digest`
- `GET /v1/packages/:package_digest/content`
- `POST /v1/runs`
- `GET /v1/runs/:run_id`
- `GET /v1/runs/:run_id/runtime`
- `GET /v1/runs/:run_id/runtime/events`
- `GET /v1/runs/:run_id/runtime/kv/:key`

And the first runner pool slice:

- `POST /v1/runner-pools`
- `GET /v1/runner-pools`
- `GET /v1/runner-pools/:runner_pool_id`
- `POST /v1/runner-pools/:runner_pool_id/drain`
- `POST /v1/runner-instances/register`
- `POST /v1/runner-instances/:runner_instance_id/heartbeat`
- `POST /v1/runner-instances/:runner_instance_id/drain`

And the first durable worker queue slice:

- `POST /v1/worker/runs/claim`
- `POST /v1/worker/run-attempts/:attempt_id/heartbeat`
- `POST /v1/worker/run-attempts/:attempt_id/events`
- `POST /v1/worker/run-attempts/:attempt_id/complete`
- `POST /v1/worker/run-attempts/:attempt_id/fail`
- `POST /v1/worker/runs/expire-leases`
