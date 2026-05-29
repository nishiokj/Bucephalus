# Cloud Database

The Cloud database is a semantic registry and committed-fact store, not the
runtime control database for local runs.

SQLite in Core remains authoritative while an experiment is executing. Postgres
receives committed facts after Core has created a durable slot commit. This
keeps local execution ownable and lets the hosted plane provide stronger
cross-experiment analysis without becoming mandatory infrastructure.

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

Cloud run requests live in `cloud.runs`. Workers claim runs through Postgres as
a durable queue, using row locks rather than in-memory process state.

The queue contract is:

1. `POST /v1/runs` inserts a durable run row and sends `pg_notify` on
   `cloud_runs_available`.
2. Workers claim runs with `FOR UPDATE SKIP LOCKED`.
3. A claim creates a `cloud.run_attempts` row with a lease.
4. Workers heartbeat to extend the lease.
5. A timer-driven sweeper expires stale attempts and requeues their runs.
6. Workers append `cloud.run_events` for audit and UI feedback.

Postgres `LISTEN/NOTIFY` is only a wake-up signal. The run table remains the
source of truth, and a polling/sweeper loop is still required for missed
notifications and heartbeat expiry.

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
bun run db:migrate
```
