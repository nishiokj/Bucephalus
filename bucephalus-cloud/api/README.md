# Bucephalus Cloud API

OpenAPI contracts for the hosted API layer.

These APIs are primitives for future GUI workflows. They are intentionally
organized by product boundary instead of screen:

- [registry.yaml](openapi/registry.yaml): content-addressed entities, aliases,
  fuzzy search, review, canonicalization, and resolution.
- [drafts.yaml](openapi/drafts.yaml): interactive authoring helpers such as
  draft validation, suggestions, schedule preview, and semantic diffs.
- [analysis.yaml](openapi/analysis.yaml): committed run ingestion, cross-run
  comparison, metric observations, artifacts, and reports.
- [observability.yaml](openapi/observability.yaml): live provisional run mirrors,
  event streams, active slot snapshots, and staleness indicators.
- [imports.yaml](openapi/imports.yaml): explicit uploads, sealed package artifact
  intake, and package diagnostics.
- [runs.yaml](openapi/runs.yaml): package artifact lookup, Cloud run records,
  runner pools, runner instances, and the durable run queue.

Tier-1 latch dispatch also uses `POST /v1/latch/resolve` to resolve a registered
`benchmark` object into a `latch_manifest_v1` plus optional host-fetchable
materials. Completed host attempts upload a result archive through `/v1/uploads`
and then register the domain record with `POST /v1/latch/submissions`.

The APIs preserve the Core/Cloud split:

- Core owns active execution truth and local recovery.
- Cloud owns registry memory, read models, committed-fact analysis, and live UI
  mirrors.
- A live observation is provisional until a Core slot commit is ingested.

## Design Rules

1. Display names and YAML ids are handles, not identity.
2. Stable identity is a canonical JSON digest: `sha256:<64 lowercase hex chars>`.
3. Registry writes are idempotent by digest.
4. Ingestion writes are idempotent by committed slot payload digest.
5. Analysis reads only committed facts unless an endpoint explicitly says it is
   live/provisional.
6. Draft APIs may recommend and resolve registered objects, but authoring must
   still work with inline YAML and no hosted dependency.
7. Suggestions are never resolutions. Resolutions are never mutations. Mutations
   are explicit actions.
8. Cloud authoring can only reference local-looking paths after an explicit
   import has turned them into Cloud resources.
9. Build targets an environment. Upload is a materialization mechanism for a
   target, not a registry-registration workflow.

That final rule is the authoring boundary. A fuzzy match can say "this looks like
an existing variant." A resolver can say "this alias currently points at this
digest." Only an explicit mutation can register a new object, create an alias, or
move an alias. Uploading a sealed package is not such a mutation. The API should
be a careful librarian, not a hidden editor.

## Spec Layout

Shared schemas and parameters live in [common.yaml](openapi/common.yaml). The
family specs reference those shared components through relative `$ref`s.
