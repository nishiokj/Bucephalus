# Package Checks

Package checks are static hygiene checks for a sealed build package. They run
after `lab build` and before dynamic preflight.

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

`lab build` writes a package report automatically:

```bash
lab build experiment.yaml --out .lab/builds/my-package --json
```

The JSON response includes:

```json
{
  "package_checks_path": ".lab/builds/my-package/package_checks.json"
}
```

You can also run the static checks again:

```bash
lab check-package .lab/builds/my-package
lab check-package .lab/builds/my-package --json
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
| `tasks.unique_valid_rows` | Malformed packaged task rows or duplicate task ids. |
| `trial_runtime.schema` | Resolved trial runtime cannot be parsed. |
| `metrics.primary_declared` | Missing or multiple primary metrics. |
| `grader.conditional_integrity` | `grader_output` metrics in no-grader experiments; grader checks skip cleanly when `grader.strategy: none`. |
| `outputs.result_capture_declared` | Missing agent result capture path. |
| `events.declaration_present` | Whether agent event streams are declared. |
| `contamination.hidden_path_mount_overlap` | Declared hidden grader paths overlapping agent output mounts. |
| `epistemic_hygiene.qa_engineer` | Placeholder for future dynamic model-assisted QA scans. |

## No-Grader Experiments

Graders are optional. A no-grader experiment can pass package checks when its
metrics come from `agent_response` or `runtime_output`.

This is valid:

```yaml
trial_runtime:
  grader:
    strategy: none

metrics:
  - id: resolved
    primary: true
    source:
      type: agent_response
      pointer: /metrics/resolved
```

This is not valid:

```yaml
trial_runtime:
  grader:
    strategy: none

metrics:
  - id: score
    primary: true
    source:
      type: grader_output
      output: report
      pointer: /score
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
