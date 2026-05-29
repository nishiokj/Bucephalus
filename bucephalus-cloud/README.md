# Bucephalus Cloud

Prototype workspace for the hosted registry and analysis plane.

This directory is intentionally separate from Bucephalus Core. Core remains a
local-first runner whose run directory and SQLite database are authoritative for
run-local lifecycle, recovery, leases, active trials, and slot commits.

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
- [deploy/runner-vm/](deploy/runner-vm/README.md) contains the self-hosted VM
  bootstrap script, systemd unit, environment contract, and cloud-init template.
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

The local package includes the foundation of a separate Cloud client CLI. It
talks to the Cloud API and writes exported authoring artifacts to disk. It does
not invoke Core.

```bash
bun run cli -- health
bun run cli -- draft validate --file ../cookbook/agent-eval/experiment.yaml
bun run cli -- draft preview --file ../cookbook/agent-eval/experiment.yaml
bun run cli -- draft export --file ../cookbook/agent-eval/experiment.yaml --out /tmp/bucephalus-cloud-export
```

Upload and inspect a sealed package artifact:

```bash
bun run cli -- import sealed-package /tmp/package.tgz --label smoke
bun run cli -- import inspect <import-id>
bun run cli -- import inspect <import-id> --json
bun run cli -- package get sha256:...
bun run cli -- run create --package-digest sha256:... --backend runner-docker --env OPENAI_BASE_URL=https://api.openai.com
bun run cli -- run get <run-id>
```

Run a VM runner daemon locally:

```bash
RUNNER_POOL_ID=$(
  BUCEPHALUS_CLOUD_WORKER_TOKEN=local-dev-worker-token bun run cli -- runner-pool create \
    --name local-runner-pool \
    --executors runner-docker \
    --resources core_runner,docker_daemon,registry_pull \
  | jq -r .runner_pool_id
)

BUCEPHALUS_WORKER_ID=local-runner-1 \
BUCEPHALUS_RUNNER_POOL_ID=$RUNNER_POOL_ID \
BUCEPHALUS_CLOUD_WORKER_TOKEN=local-dev-worker-token \
BUCEPHALUS_WORKER_EXECUTORS=runner-docker \
BUCEPHALUS_WORKER_RESOURCES=core_runner,docker_daemon,registry_pull \
BUCEPHALUS_CORE_RUNNER_CMD=../target/debug/bucephalus \
bun run worker
```

The worker listens for `cloud_runs_available`, polls as a fallback, claims runs,
heartbeats its attempt lease, downloads the sealed package through the API, and
invokes Core. The production shape is a long-running daemon on a VM, not a
container that happens to mount the host Docker socket.

## Local API

```bash
cd bucephalus-cloud
bun run db:up
bun run db:migrate
BUCEPHALUS_CLOUD_WORKER_TOKEN=local-dev-worker-token PORT=8099 bun run dev
```

Then smoke check:

```bash
curl http://localhost:8099/readyz
```

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
