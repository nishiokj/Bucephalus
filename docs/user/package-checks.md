# Package Checks

Package checks are static hygiene checks for immutable sealed package contents.
They run after schema and resolved-contract validation, after `bucephalus build`,
and before dynamic preflight.

The split is intentional:

- schema and resolved-contract validation reject contradictory wiring
- package checks inspect immutable package contents and write the package
  evidence report
- preflight inspects dynamic launch resources such as Docker, images, env vars,
  secrets, writable paths, and runtime smoke commands
- runtime health is continuous and should monitor live failures such as provider
  quota walls, invalid JSON spikes, missing outputs, disk pressure, and crash
  loops

If the package digest does not change, package-check results remain meaningful.
If the machine, secrets, provider access, Docker daemon, or available disk
changes, run preflight again.

## Run Checks

`bucephalus build` writes a package report automatically:

```bash
bucephalus build experiment.yaml --out <package_dir> --json
```

The JSON response includes:

```json
{
  "package_checks_path": "<package_dir>/package_checks.json"
}
```

You can also validate the sealed package and rewrite the static evidence report:

```bash
bucephalus check-package <package_dir>
bucephalus check-package <package_dir> --json
```

`check-package` does not start Docker, access providers, load secrets, or run
trial commands.

## Current Checks

The report is written as `package_checks.json` with `schema_version:
package_checks_v1`. Each check has:

- `id`
- `scope`
- `status`: `pass`, `warn`, or `fail`
- `reason`
- `evidence`

Current checks include:

| Check | What it catches |
| --- | --- |
| `provenance.package_digest_present` | The report is tied to a sealed package digest. |
| `tasks.unique_valid_rows` | Historical check id for malformed packaged case rows or duplicate case ids. |
| `images.task_refs_digest_pinned` | Mutable task image tags that should be pinned to a digest for reproducibility. |

## No-Grader Experiments

Graders are optional. A no-grader experiment validates when its metrics come
from `from: result...` declarations.

This is valid:

```yaml
metrics:
  - id: resolved
    from: result.metrics.resolved
```

A single authored metric defaults to primary in the sealed package. If you
declare multiple metrics, mark exactly one with `primary: true`; build
validation rejects ambiguous primary metric declarations before package checks
are written.

`stages.grader.strategy: none` may be explicit, but omitting `stages.grader`
is the preferred no-grader default. An explicit empty `stages.grader: {}` is
rejected.

This is not valid:

```yaml
stages:
  grader:
    strategy: none

metrics:
  - id: score
    primary: true
    from: grader.report.score
```

This relationship, metric `from:` references to declared grader outputs or the
supported canonical runtime result output, and hidden grader path/output mount
overlap are enforced by schema validation.

## Epistemic Hygiene

Package checks are not a proof that a benchmark is semantically uncontaminated
or that an agent could not cheat. They are a deterministic package-evidence
layer after schema and resolved-contract validation, before spending tokens.

Future hygiene layers can build on this report:

- grader positive and negative controls
- deterministic leakage scans over materialized workspaces
- dynamic QA-engineer probes declared with a provider, model, and API key
- provider/runtime health policies and early stops

Treat package checks as the immutable package layer of a larger trust report,
not as a replacement for preflight or live monitoring.
