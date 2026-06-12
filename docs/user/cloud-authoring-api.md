# Hosted Cloud Authoring API

Hosted draft authoring APIs are Cloud API primitives for tools that edit,
inspect, and compare experiment drafts before `buc build experiment.yaml`.
They do not replace local YAML authoring; the same primitives are exposed by
the hosted CLI as `buc author ...` and `buc drafts ...`.

All endpoints accept JSON and require the same Cloud auth as the hosted CLI.

## Draft Shape

Requests wrap the draft experiment object:

```json
{
  "draft": {
    "experiment": { "id": "demo", "name": "Demo" },
    "runtime": { "compute": { "backend": "local-docker" } },
    "matrix": {
      "variants": [{ "id": "baseline" }],
      "cases": { "count": 2 }
    },
    "stages": {},
    "policy": {}
  }
}
```

## Endpoints

- `POST /v1/drafts/canonicalize`: returns a stable canonical draft digest and
  digest bindings for known inline entities.
- `POST /v1/drafts/resolve`: resolves registry aliases/digests and annotates
  draft entities with content digests where possible.
- `POST /v1/drafts/validate`: returns authoring/package/launch-hint issues.
- `POST /v1/drafts/preview-schedule`: estimates variant/case/repeat expansion.
- `POST /v1/drafts/export`: emits YAML or resolved JSON.
- `POST /v1/drafts/suggest`: returns registry-backed suggestions for a target
  such as `variant`, `agent_app`, `grader`, `metric`, or `runtime_profile`,
  plus relevant validation warnings.
- `POST /v1/drafts/diff`: compares two inline drafts, or an inline draft and a
  registered object ref, and returns JSON-pointer changes with rough
  significance labels.

## Build Boundary

Draft APIs help author or inspect an experiment. The runnable Cloud package
boundary is still `buc build experiment.yaml`, which uploads an explicit
authoring context, runs hosted Core, imports the sealed package, and reports
Cloud readiness. Import is part of that boundary: the API rejects sealed
packages whose package checks failed, whose metadata/digests do not match, or
whose runtime staging manifest would mount files outside the Cloud runner
contract roots.

For authoring-context uploads, the source upload must be a completed `.tgz`,
`.tar.gz`, or `.tar` archive. Before Core runs, the server re-verifies the
materialized archive bytes against the completed upload digest and byte size,
then enforces the same safety boundary as the hosted CLI: paths must be
relative POSIX paths, entrypoints must exist inside the archive, symlinks and
unsafe entry types are rejected, and local-only secret/build/dependency
directories such as `.git`, `.env*`, `target`, `node_modules`, `.bucephalus`,
and `.bucephalus-package` are blocked.

## CLI

```bash
buc author canonicalize experiment.yaml
buc author resolve experiment.yaml
buc author validate experiment.yaml
buc author preview experiment.yaml
buc author suggest experiment.yaml --target variant --q baseline
buc author diff before.yaml after.yaml
buc author export experiment.yaml --format yaml
```

Use `--json` on any command to receive the raw API response.
