# Inspecting Results

After a hosted Cloud run starts, inspect it through the runtime resource API.
The Cloud surface is intentionally resource-native: trials, runner instances,
access requests, artifacts, events, metrics, and runtime facts are visible as
resources with status, conditions, owner references, audit fields, and
subresources.

Local run facts are still stored through the configured run store. Local runs
default to the account SQLite database at `<storage>/bucephalus.sqlite`; set
`BUCEPHALUS_DB=/absolute/path/to/bucephalus.sqlite` to choose an explicit local
database, or `BUCEPHALUS_HOME=/absolute/path/to/dir` to move Bucephalus's
default home. Cloud workers select Postgres with
`BUCEPHALUS_RUN_STORE=postgres` and `BUCEPHALUS_RUN_STORE_URL`.

## List Runs

```bash
bucephalus runs
bucephalus runs --json
bucephalus-cloud run get <cloud_run_id>
```

## Cloud Runtime Resources

Start with discovery, then list the resources you care about:

```bash
bucephalus-cloud run api-resources <cloud_run_id> --wide
bucephalus-cloud run api-resources <cloud_run_id> --paths
bucephalus-cloud run explain <cloud_run_id> Trial
bucephalus-cloud run resources <cloud_run_id>
bucephalus-cloud run resources <cloud_run_id> --category runner
bucephalus-cloud run resources <cloud_run_id> --category trial
bucephalus-cloud run resources <cloud_run_id> --category access
bucephalus-cloud run tree <cloud_run_id>
```

`run api-resources --wide` prints selector contracts, including
`LABEL-SELECTORS` and `FIELD-SELECTORS`, before you filter `run resources`,
`run health`, `run tree`, or `run watch`. Use `--category runner`,
`--category trial`, `--category access`, `--category observability`, or another
advertised category when you want an operator group instead of one exact kind.
Use `run explain <cloud_run_id> <kind>` for the server-side contract for one
kind or alias, including selectors, path templates, subresources, access modes,
actions, and example commands.

Resource lists include Kubernetes-style list metadata with
`metadata.resourceVersion`, `metadata.continue`, `metadata.remainingItemCount`,
`metadata.returned`, and `metadata.total`:

```bash
bucephalus-cloud run resources <cloud_run_id> --kind Trial --limit 50
bucephalus-cloud run resources <cloud_run_id> --kind Trial --limit 50 --continue <metadata.continue>
```

## Health And Metrics

Use health and top-style metrics when you need a quick operational picture:

```bash
bucephalus-cloud run health <cloud_run_id>
bucephalus-cloud run health <cloud_run_id> --category runner
bucephalus-cloud run top <cloud_run_id> --category runner --limit 25
bucephalus-cloud run top <cloud_run_id> --category trial --limit 50 --continue <metadata.continue>
bucephalus-cloud run metrics <cloud_run_id> RunnerInstance/<runner_instance_name>
bucephalus-cloud run metrics <cloud_run_id> Trial/<trial_name>
```

`run health` summarizes `conditions[]`, `status.phase`, `status.access`, and
available `actions[]`. `run top` calls
`GET /v1/runs/:run_id/runtime/resources/metrics` with the same selectors as
`run resources`; it returns paginated collection metrics with the same
`metadata.continue`, `remainingItemCount`, `returned`, and `total` shape as
resource lists. Use
`run metrics <kind>/<name>` for the full metric set on one selected resource.

## Trials And Artifacts

Trials and output bytes are first-class runtime resources:

```bash
bucephalus-cloud run resources <cloud_run_id> --kind Trial
bucephalus-cloud run describe <cloud_run_id> Trial/<trial_name>
bucephalus-cloud run wait <cloud_run_id> Trial/<trial_name> --for condition=Ready --timeout-seconds 300
bucephalus-cloud run resources <cloud_run_id> --kind TrialArtifact --field-selector status.content_available=true
bucephalus-cloud run describe <cloud_run_id> TrialArtifact/<artifact_name>
bucephalus-cloud run artifact <cloud_run_id> TrialArtifact/<artifact_name> --out agent-result.json
```

Use `Trial` resources for lifecycle, outcome, metrics, bindings, and runner
access readiness. Use `TrialArtifact` resources for recorded content metadata
and content downloads through the resource content subresource.

## Low-Level Access

Before starting a lower-level Cloud operation, ask the server what the selected
resource supports:

```bash
bucephalus-cloud run describe <cloud_run_id> Trial/<trial_name>
bucephalus-cloud run can-i <cloud_run_id> port-forward Trial/<trial_name>
bucephalus-cloud run can-i <cloud_run_id> exec TrialContainer/<container_name> --json
```

`run describe` includes object-scoped `operations[]` entries with
server-materialized commands and blocked reasons. `run can-i` calls the same
server-owned operation review. Use `run can-i` when you need a focused server-owned operation review; the response includes the resource version, generation, and observed generation used for the decision. Mutating low-level CLI commands preflight the same server-owned operation review before creating or changing `PortForward`, `Exec`, or `RunnerInstance` resources, and GUI clients do not invent support locally. Both send the reviewed `resource_version` back as a precondition, so stale clients fail with Conflict instead of mutating a resource whose state has moved on.

Create and inspect access resources through concrete resource subresources:

```bash
bucephalus-cloud run port-forward <cloud_run_id> Trial/<trial_name> --target-port 8080 --local-port 18080 --reason debug --ttl-seconds 300
bucephalus-cloud run exec <cloud_run_id> TrialContainer/<container_name> --reason debug --ttl-seconds 300 -- python -V
bucephalus-cloud run resources <cloud_run_id> --kind PortForward,Exec
bucephalus-cloud run describe <cloud_run_id> PortForward/<port_forward_name>
bucephalus-cloud run describe <cloud_run_id> Exec/<exec_name>
bucephalus-cloud run logs <cloud_run_id> Exec/<exec_name> --stream stdout
bucephalus-cloud run wait <cloud_run_id> Exec/<exec_id> --for phase=completed --timeout-seconds 120
bucephalus-cloud run delete <cloud_run_id> PortForward/<port_forward_name> --reason cleanup
```

Port-forward and exec are audited resources. Use `--ttl-seconds` to store a
control-plane expiration on the resource; `--timeout-seconds` only bounds how
long the local CLI waits. The resulting `PortForward` and `Exec` resources carry
the selected target in `spec.target_ref`, including target UID and
`metadata.resourceVersion` when known, plus `status.runner_binding` with runner
instance, attempt, and worker IDs. This lets `run get`/`run describe` prove what
was reviewed even before you inspect the event stream.

Use `bucephalus-cloud run wait <cloud_run_id> <kind>/<name>` for Kubernetes-style
lifecycle waits over the selected resource's status subresource. It defaults to
`--for condition=Ready` and also supports `--for condition=Ready=False`,
`--for phase=active`, and `--for phase=completed`.

## Events And Audit

Runtime events are visible both as event streams and as `Event` resources:

```bash
bucephalus-cloud run events <cloud_run_id> --limit 100
bucephalus-cloud run events <cloud_run_id> --limit 100 --continue <metadata.continue>
bucephalus-cloud run events <cloud_run_id> Trial/<trial_name> --follow
bucephalus-cloud run audit <cloud_run_id> --limit 100
bucephalus-cloud run audit <cloud_run_id> RunnerInstance/<runner_instance_name> --follow --limit 50
bucephalus-cloud run watch <cloud_run_id> --resources-only
bucephalus-cloud run watch <cloud_run_id> --events-only --event-type runtime.access.exec.requested
bucephalus-cloud run resources <cloud_run_id> --kind Event
```

Interpret absent events as "not observed", not "no tools were used". Buc can
only expose semantic model/tool activity when the selected client/protocol emits
events or an explicit event capture is declared. Agent exit status, result file
presence/parse status, grader status, and sandbox paths are Buc-owned and do not
depend on semantic trace support.

`run audit` is a focused event view for runtime access and resource lifecycle
families such as `runtime.access.*` and `runtime.resource.*`. Use the broader
`run events` stream when you also need Cloud control-plane lifecycle rows,
worker-reported events, and Core runtime trace rows.
Runtime access audit payloads keep structured evidence in addition to the
normalized `resource_refs`: `access_resource_ref`, `target_ref`,
`resolved_target`, `runner_binding`, and `resource_version_precondition` show
which selected object, runner instance, attempt, worker, and reviewed
`metadata.resourceVersion` produced the transition.

Event lists expose `metadata.resourceVersion`, `metadata.continue`,
`metadata.remainingItemCount`, `metadata.returned`, and numeric
`metadata.next_after_row_seq` for manual debugging/backfill. Prefer
`--continue <metadata.continue>` for manual paging; follow mode keeps using the
server `metadata.resourceVersion` cursor to poll only newly observed events.

Resource describe responses include `event_list` with the same resource-scoped
event metadata as the `/events` subresource. Read events from
`event_list.events` so cursor and resource-version metadata stay attached.

## Inspect Bundles

Use an inspect bundle when you need a portable operator snapshot:

```bash
bucephalus-cloud run inspect <cloud_run_id> --json --out runtime-inspect.json
bucephalus-cloud run inspect <cloud_run_id> --category runner --json --out runtime-inspect-runner.json
```

The bundle includes API discovery, the filtered resource inventory, the
selectors used to build the bundle, resource health, bounded collection metrics,
`event_list` with the bounded recent event stream metadata, and concrete log
subresource references for offline triage or support handoff.

## Compare Runs

For hosted Cloud, compare runs from resource-visible trial and metric rows:

```bash
bucephalus-cloud run resources <cloud_run_id> --kind Trial
bucephalus-cloud run resources <cloud_run_id> --kind MetricObservation
bucephalus-cloud run top <cloud_run_id> --category trial --limit 100
```

For local account-database analysis, query the configured run store directly:

```bash
bucephalus query <run_id> "
  SELECT variant_id, metric_name, avg(CAST(metric_value AS REAL)) AS mean_value
  FROM metrics_long
  GROUP BY variant_id, metric_name
  ORDER BY metric_name, variant_id
"
```

Useful local SQL views include:

| View | Purpose |
| --- | --- |
| `trials` | Committed trial outcomes and primary metrics. |
| `observability_summary` | Run-level proof coverage across results, events, exits, timeouts, grader, and extraction. |
| `trial_diagnostics` | Raw per-trial facts with result state, grader status, sandbox workdir, and log paths. |
| `trial_attempt_latest` | Latest runtime attempt state per trial, including phase and stdout/stderr paths. |
| `raw_events` | Raw event payload JSON exactly as Buc ingested it, plus row context. |
| `metrics_long` | Declared metric observations plus metric definition metadata. |
| `metric_definitions` | Canonical metric declarations from experiment YAML. |
| `events` | Parsed event view for analytic queries over common fields. |
| `contract_stages` | Runtime contract stage status and details. |
| `variant_snapshots` | Variant binding values per trial. |

Raw event payloads can be queried without adding custom fields to a fixed
event-row schema:

```bash
bucephalus query <run_id> "
  SELECT trial_id, row_seq, event_type, event_json
  FROM raw_events
  ORDER BY schedule_idx, row_seq
  LIMIT 50
"
```

```bash
bucephalus query <run_id> "
  SELECT trial_id, row_seq,
         event_json ->> '$.rex.request_id' AS rex_request_id,
         CAST(event_json ->> '$.rex.server_ms' AS REAL) AS rex_server_ms
  FROM raw_events
  ORDER BY schedule_idx, row_seq
  LIMIT 50
"
```

The `raw_events` local SQL view includes live rows whose trial has not committed
yet, so it is the right local query target for checking whether an executing
agent is still producing observable work.

## Important Files

Run files live under `<run_dir>/`.

Build package files live under `<storage>/builds/<package>/` by default, or
under the directory passed with `bucephalus build --out`.

| Package Path | Purpose |
| --- | --- |
| `manifest.json` | Sealed package manifest. |
| `resolved_experiment.json` | Resolved experiment config used by runs. |
| `checksums.json` | Package file digests. |
| `package_checks.json` | Static package hygiene report from `bucephalus build` or `bucephalus check-package`. |

| Path | Purpose |
| --- | --- |
| `manifest.json` | Run metadata. |
| `resolved_experiment.json` | Resolved experiment config. |
| `attestation.json` | Provenance summary. |
| `trials/<trial_id>/out/result.json` | Agent result. |
| `trials/<trial_id>/out/<declared grader output>` | Native grader outputs declared under `stages.grader.outputs`. |
| `trials/<trial_id>/out/<output_mount path>/` | Files written through `stages.agent.output_mounts`. |
| `trials/<trial_id>/agent_stdout.log` | Agent stdout. |
| `trials/<trial_id>/agent_stderr.log` | Agent stderr. |
| `trials/<trial_id>/grader_stdout.log` | Grader stdout. |
| `trials/<trial_id>/grader_stderr.log` | Grader stderr. |
