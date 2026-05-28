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
