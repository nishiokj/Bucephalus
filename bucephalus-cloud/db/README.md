# Cloud Database

The Cloud database is the durable queue, semantic registry, committed-fact
store, and API-owned runtime reporting store for cloud runs. Core still
supports SQLite for local runs, but allocated cloud workers must not receive
Postgres credentials or write directly to the runtime schema. Workers report
bounded runtime snapshots through the Cloud API instead.

Runtime schema creation is an admin/developer operation. Run `bun run
db:migrate` before starting the API. Runner VM credentials must not own schema
DDL or runtime-table access.

SQL migrations are merge-gated through an integration test, not through a
shared developer database. `bun run test:migrations` creates a scratch database
on the configured Postgres server, applies the full migration set from empty,
checks the migration ledger and required schema objects, writes a small
representative Cloud row, and applies the migrations a second time to prove
idempotency. The GitHub `Cloud/Core gates` check must remain required for merges
to `main`.

The migration test defaults to local/CI Postgres via `DATABASE_URL`. To rehearse
against an intentional non-local staging server, set
`BUCEPHALUS_MIGRATION_TEST_DATABASE_URL` and
`BUCEPHALUS_ALLOW_REMOTE_MIGRATION_TESTS=true`; never point this at production,
because it creates and drops scratch databases.

## Identity Model

Every stateful, reusable experiment entity should have a canonical JSON shape
and a stable digest:

```text
sha256:<64 lowercase hex chars>
```

Display names, YAML ids, labels, and aliases are handles. They are not identity.

The first migration stores canonical entities in `content_objects`, then exposes
typed registry tables for the domain nouns that matter most:

- `agent_apps`
- `variants`
- `cases`
- `metrics`
- `graders`
- `datasets`
- `runtime_profiles`
- `experiment_packages`

Runs and slot commits reference those digests. Cross-run comparison should join
on digests first, then use aliases only for presentation.

## Package Artifact Intake

Uploaded sealed packages are package artifacts. They may be stored with manifest
metadata, resolved experiment JSON, package digest, and diagnostics so Cloud can
launch, audit, or associate them with runs.

Package intake does not register reusable entities merely because they appear in
the package. Registry registration remains explicit and scoped per entity.

Early migrations include proposal/action tables from the first import sketch.
The current product boundary does not use those tables for sealed package
intake.

## Cloud Run Queue

Cloud run requests live in `cloud.runs`. The API uses Postgres as the durable
queue, using row locks rather than in-memory process state. Workers claim runs
through the Cloud API and never by connecting to Postgres.

The queue contract is:

1. `POST /v1/runs` inserts a durable run row with explicit runtime
   requirements.
2. Workers register runner instances and claim runs through the Cloud API.
3. The API selects claimable runs with `FOR UPDATE SKIP LOCKED`, matching the
   run requirements against runner capabilities.
4. A claim creates a `cloud.run_attempts` row with a lease.
5. Workers heartbeat through the Cloud API to extend the lease.
6. A timer-driven sweeper expires stale attempts and requeues their runs.
7. Workers append `cloud.run_events` through the Cloud API for audit, UI
   feedback, and bounded runtime snapshots.

Queue wakeup is currently polling plus API lease expiration. Do not add
runner-side Postgres `LISTEN/NOTIFY`; a future wake channel should still be
mediated by the control plane.

Workers do not read API-local artifact paths from Postgres. They materialize
packages through the package content API so local smoke tests preserve the
production boundary between API storage and worker compute.

## Sync Boundary

Ingestion should be append/replay friendly:

1. Core commits a slot locally.
2. Core or an uploader sends the committed slot payload to Cloud.
3. Cloud upserts immutable registry objects by digest.
4. Cloud inserts committed facts keyed by `slot_commit_id`.
5. Replays are no-ops when the existing digest matches.
6. A key collision with a different digest is corruption, not an update.

## Local Postgres

The local compose file uses `pgvector/pgvector:pg16`, which is Postgres 16 with
extra extensions available. The schema currently only requires `pgcrypto`; the
image choice keeps local development working on machines that already have the
pgvector image cached.

```bash
bun run db:up
bun run test:migrations
bun run db:migrate
```

The runtime store migration creates `bucephalus_runtime` by default for legacy
runtime-table reads by the API. If a cloud environment uses a different
`BUCEPHALUS_RUN_STORE_SCHEMA`, apply an equivalent admin migration for that
schema before pointing the API at it. Do not point runner workers at it.
