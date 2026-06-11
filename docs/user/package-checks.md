# Package Checks

Package checks are static hygiene checks for a sealed build package. They run
after `bucephalus build` and before dynamic preflight.

The split is intentional:

- package checks inspect immutable package contents
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

You can also run the static checks again:

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
- `status`: `pass`, `warn`, `fail`, or `skip`
- `reason`
- `evidence`

Current checks include:

| Check | What it catches |
| --- | --- |
| `provenance.package_digest_present` | The report is tied to a sealed package digest. |
| `variants.unique_ids` | Missing or duplicate variant ids. |
| `design.schedule_matches_comparison` | Historical check id for scheduling contradictions, such as `scheduling.comparison: paired` with one variant, or paired experiments not using paired interleaving. |
| `tasks.unique_valid_rows` | Historical check id for malformed packaged case rows or duplicate case ids. |
| `trial_runtime.schema` | Historical check id for a resolved stage chain that cannot be parsed. |
| `metrics.primary_declared` | Missing or multiple primary metrics. |
| `grader.conditional_integrity` | `from: grader...` metrics in no-grader experiments; grader checks skip cleanly when `grader.strategy: none`. |
| `outputs.result_capture_declared` | Missing agent result capture path. |
| `agent.protocol_supported` | Whether the selected agent protocol is supported by this runner. |
| `events.declaration_present` | Whether agent event streams are declared. |
| `contamination.hidden_path_mount_overlap` | Declared hidden grader paths overlapping agent output mounts. |
| `epistemic_hygiene.qa_engineer` | Placeholder for future dynamic model-assisted QA scans. |

## No-Grader Experiments

Graders are optional. A no-grader experiment can pass package checks when its
metrics come from `from: result...` declarations.

This is valid:

```yaml
stages:
  grader:
    strategy: none

metrics:
  - id: resolved
    primary: true
    from: result.metrics.resolved
```

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

## Epistemic Hygiene

Package checks are not a proof that a benchmark is semantically uncontaminated
or that an agent could not cheat. They are a deterministic first layer for
catching static contradictions and dangerous wiring mistakes before spending
tokens.

Future hygiene layers can build on this report:

- grader positive and negative controls
- deterministic leakage scans over materialized workspaces
- dynamic QA-engineer probes declared with a provider, model, and API key
- provider/runtime health policies and early stops

Treat package checks as the immutable package layer of a larger trust report,
not as a replacement for preflight or live monitoring.
