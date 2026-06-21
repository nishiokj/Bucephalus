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
buc runs list
buc runs get <cloud_run_id>
```

## Cloud Runtime Resources

Start with discovery, then list the resources you care about:

```bash
buc runs api-resources <cloud_run_id>
buc runs api-resources <cloud_run_id> Trial --json
buc runs explain <cloud_run_id> TrialContainer
buc runs tree <cloud_run_id> --kind Trial,TrialContainer
buc runs resources <cloud_run_id>
buc runs resources <cloud_run_id> --category runner --wide
buc runs resources <cloud_run_id> --kind RunnerInstance
buc runs resources <cloud_run_id> --kind RunnerInstance --wide
buc runs resources <cloud_run_id> --kind Trial
buc runs resources <cloud_run_id> --kind Trial -o name
buc runs resources <cloud_run_id> --kind PortForward
buc runs describe <cloud_run_id> Trial/<trial_name>
```

`buc runs api-resources <run-id> [kind]` prints the server-owned contract for
runtime kinds. Use `buc runs explain <run-id> <kind>` for the human view of one
kind's aliases, categories, verbs, subresources, selectors, actions, access,
printer columns, path templates, and example commands. Use that contract before
filtering resources with `--kind`, `--label-selector`, or `--field-selector`;
`--category` selects categories advertised by that server-owned discovery
contract, such as `runner`, `trial`, `access`, or `access-target`. The CLI
forwards that selector to the hosted runtime API as `category=runner` on
resource list, watch, health, metrics, and inspect requests.
Add `--wide` to `buc runs resources` or resource-list forms of
`buc runs get` when you want server-discovered printer columns grouped by
runtime kind. Add `--output name` or `-o name` when you need raw `Kind/name`
refs for shell pipelines.
Catalog discovery reads are audited as `runtime.api_resources.read`; failed
kind lookups use `runtime.api_resources.read.failed` and keep the requested
kind, error code, HTTP status, requester, and `Run/<run-id>` resource ref.

Use `buc runs tree <run-id>` when ownership and lifecycle shape matter more
than table scanning. It prints the current resource inventory as an
owner-reference tree and keeps filtered-out parents visible as owner refs on
root rows.

Resource lists include Kubernetes-style list metadata with
`metadata.resourceVersion`, `metadata.continue`, `metadata.remainingItemCount`,
`metadata.returned`, and `metadata.total`:

```bash
buc runs resources <cloud_run_id> --kind Trial --limit 50
buc runs resources <cloud_run_id> --category trial --limit 50
buc runs resources <cloud_run_id> --kind Trial --limit 50 --continue <metadata.continue>
```

## Health And Metrics

Use health and metrics when you need a quick operational picture:

```bash
buc runs health <cloud_run_id>
buc runs health <cloud_run_id> --kind RunnerInstance
buc runs top <cloud_run_id> --kind RunnerInstance --limit 25
buc runs top <cloud_run_id> --kind Trial --limit 50 --continue <metadata.continue>
buc runs metrics <cloud_run_id> --kind RunnerInstance --limit 25
buc runs metrics <cloud_run_id> --kind Trial --limit 50 --continue <metadata.continue>
buc runs metrics <cloud_run_id> RunnerInstance/<runner_instance_name>
buc runs metrics <cloud_run_id> Trial/<trial_name>
```

`run health` summarizes `conditions[]`, `status.phase`, `status.access`, and
available `actions[]`. `run top` is the operator-friendly alias for collection
runtime metrics, and `run metrics` can return the same collection shape or the
metrics subresource for one selected runtime object.

## Trials And Artifacts

Trials and output bytes are first-class runtime resources:

```bash
buc runs resources <cloud_run_id> --kind Trial
buc runs describe <cloud_run_id> Trial/<trial_name>
buc runs wait <cloud_run_id> Trial/<trial_name> --for condition=Ready --timeout-seconds 300
buc runs resources <cloud_run_id> --kind TrialArtifact --field-selector status.content_available=true
buc runs describe <cloud_run_id> TrialArtifact/<artifact_name>
buc runs content <cloud_run_id> TrialArtifact/<artifact_name> --out agent-result.json --metadata-out agent-result.metadata.json
```

Use `Trial` resources for lifecycle, outcome, metrics, bindings, and runner
access readiness. Use `TrialArtifact` resources for recorded content metadata
and content downloads through the resource content subresource. Add
`--metadata-out FILE` on raw content or log downloads to save the Cloud response
provenance separately from the bytes, including run/resource identity, Core run
id, trial id, object ref, digest, media type, and byte size. These reads are
also visible in runtime audit events as `runtime.resource.logs.read` and
`runtime.resource.content.read` with requester and object metadata. Failed raw
reads are visible as `runtime.resource.logs.read.failed` and
`runtime.resource.content.read.failed` with the selected resource, error code,
HTTP status, and error message.

Declared infra resources are inspectable too. `ImagePull/<image_name>` records
the runner pre-pull lifecycle as `Pulling`, `Pulled`, or `Failed` through
`worker.runtime.image_pull.*` audit events. `SecretBinding/<secret_id>` never
contains secret refs or values, but its status advances to `Materialized` when
the worker emits `worker.runtime.secret_binding.materialized`. A declared
sidecar appears as `SidecarRequirement/<sidecar>` and advances to `Checking`,
`Available`, or `Failed` through `worker.runtime.sidecar_requirement.*` events
when the worker validates runner capability. A declared accelerator appears as
`AcceleratorRequirement/<accelerator>` and advances through
`worker.runtime.accelerator_requirement.*` events after worker validation. A
declared network allowlist appears as `NetworkPerimeter/declared`; provider enforcement is audited as
`worker.runtime.network_perimeter.applying`,
`worker.runtime.network_perimeter.applied`, or
`worker.runtime.network_perimeter.failed`.

## Low-Level Access

Before starting a lower-level Cloud operation, ask the server what the selected
resource supports:

```bash
buc runs describe <cloud_run_id> Trial/<trial_name>
buc runs can-i <cloud_run_id> port-forward Trial/<trial_name>
buc runs can-i <cloud_run_id> exec TrialContainer/<container_name> --json
```

`run describe` includes object-scoped `operations[]` entries with
server-materialized commands and blocked reasons. `run can-i` calls the same
server-owned operation review. Use `run can-i` when you need a focused
server-owned operation review; the response includes the resource version,
generation, and observed generation used for the decision. Mutating low-level
CLI commands require that reviewed `--resource-version`, and GUI clients require
the same reviewed version from the operation review before creating or changing
`PortForward`, `Exec`, or `RunnerInstance` resources. Operation reviews are
audited as `runtime.resource.operation.reviewed`, and failed review attempts are
audited as `runtime.resource.operation.review.failed`. Stale clients fail with
Conflict instead of mutating a resource whose state has moved on.

Create and inspect access resources through concrete resource subresources:

```bash
buc runs port-forward <cloud_run_id> Trial/<trial_name> --target-port 8080 --local-port 18080 --attach --reason debug --ttl-seconds 300 --resource-version <reviewed-version>
buc runs exec <cloud_run_id> TrialContainer/<container_name> --reason debug --ttl-seconds 300 --resource-version <reviewed-version> -- python -V
buc runs resources <cloud_run_id> --kind PortForward
buc runs resources <cloud_run_id> --kind Exec
buc runs describe <cloud_run_id> PortForward/<port_forward_name>
buc runs describe <cloud_run_id> Exec/<exec_name>
buc runs logs <cloud_run_id> Exec/<exec_name> --stream stdout --metadata-out exec.log.metadata.json
buc runs wait <cloud_run_id> Exec/<exec_id> --for phase=completed --timeout-seconds 120
buc runs complete <cloud_run_id> PortForward/<port_forward_name> --reason cleanup --resource-version <reviewed-version>
buc runs wait <cloud_run_id> PortForward/<port_forward_name> --for phase=completed --timeout-seconds 60
```

Port-forward and exec are audited resources. Use `--ttl-seconds` to store a
control-plane expiration on the resource; `--wait-seconds` only bounds how long
the local CLI waits for an access resource to become active/completed. The
`--attach` port-forward mode holds a local GCE IAP TCP tunnel and marks the
`PortForward` resource completed with an audited closeout reason when the local
attach process exits cleanly. When a worker owns the active tunnel and reports a
client-reachable endpoint instead, `--attach` prints that endpoint and leaves
the `PortForward` active for explicit cleanup or TTL expiry. The
resulting `PortForward` and `Exec` resources carry
the selected target in `spec.target_ref`, including target UID and
`metadata.resourceVersion` when known, plus `status.runner_binding` with runner
instance, attempt, and worker IDs. Human `runs describe` output also surfaces
requester/reason/expiration, connection mode, raw connection details, remote exec
command/output facts, an `attach_command` for active GCE IAP `PortForward`
resources, and `client_endpoint` for worker-managed active tunnels. Live access
resources also include a
`cleanup_command` with the current `metadata.resourceVersion`, giving raw attach
sessions the same audited closeout path as `buc runs port-forward --attach`.
Exec output facts include stdout/stderr tail byte counts and truncation flags,
so operators can tell when status carries bounded stream evidence rather than a
complete stream.
`buc runs resources <cloud_run_id> --kind Exec --wide` exposes the same
stdout/stderr byte totals, retained tail byte counts, and truncation flags in
the server-advertised printer columns without dumping the tail text into the
table.
`buc runs audit <cloud_run_id> Exec/<exec_name>` summarizes those same stream
evidence facts next to the exit code on completed Exec lifecycle events.
This lets `runs describe` prove what was reviewed even before you inspect the
event stream.
Every runtime resource also exposes Kubernetes-style
`metadata.creationTimestamp` when the source row has creation-time evidence.
Audited access resources also carry `metadata.deletionTimestamp` once
resource-level delete/cancel is acknowledged; the older
`metadata.created_at` compatibility field is retained.

Use `buc runs wait <cloud_run_id> <kind>/<name>` for Kubernetes-style
lifecycle waits over the selected resource's status subresource. It defaults to
`--for condition=Ready` and also supports `--for condition=Ready=False`,
`--for phase=active`, `--for phase=completed`, and `--for delete` for cleanup
flows. For audited access resources, delete completion means the status
subresource reports `deletionTimestamp`, mirroring the resource's
`metadata.deletionTimestamp`; a later 404 is also treated as success. Human
status and successful wait output include resourceVersion, generation
freshness, conditions, available actions, deletion timestamp, and audit source
so operators can copy preconditions directly into follow-up commands.

## Events And Audit

Runtime events are visible both as event streams and as `Event` resources:

```bash
buc runs events <cloud_run_id> --limit 100
buc runs events <cloud_run_id> --limit 100 --continue <metadata.continue>
buc runs events <cloud_run_id> Trial/<trial_name> --limit 50
buc runs audit <cloud_run_id> --limit 100
buc runs audit <cloud_run_id> RunnerInstance/<runner_instance_name> --limit 50
buc runs watch <cloud_run_id> --kind Trial --resource-version <metadata.resourceVersion>
buc runs watch <cloud_run_id> --known-resource Trial/<trial_name>=<metadata.resourceVersion>
buc runs watch <cloud_run_id> --kind Trial --follow --interval-seconds 2 --max-polls 30
buc runs resources <cloud_run_id> --kind Event --wide
buc runs resources <cloud_run_id> --kind Event --field-selector status.involved=PortForward/<port_forward_name> --wide
buc runs resources <cloud_run_id> --kind Event --field-selector spec.involved_object.kind=PortForward,spec.involved_object.name=<port_forward_name> --wide
```

Interpret absent events as "not observed", not "no tools were used". Buc can
only expose semantic model/tool activity when the selected client/protocol emits
events or an explicit event capture is declared. Agent exit status, result file
presence/parse status, grader status, and sandbox paths are Buc-owned and do not
depend on semantic trace support. Use `buc runs audit` for the focused runtime
access and resource-lifecycle audit families, `buc runs events` when you need
the broader stream, and `buc runs watch` when you need a resource-version
cursor over resource changes. Add `--follow` to `buc runs watch` to keep
polling with the returned collection and per-resource cursors, including
deletions when the server returns `resource_versions`; follow polls opt into
watch BOOKMARK events so an unchanged collection is still an explicit cursor
heartbeat.
Runtime access audit payloads keep structured evidence in addition to the
normalized `resource_refs`: `access_resource_ref`, `target_ref`,
`resolved_target`, `runner_binding`, and `resource_version_precondition` show
which selected object, runner instance, attempt, worker, and reviewed
`metadata.resourceVersion` produced the transition.
`Event` resources project the primary involved object into
`spec.involved_object` and `status.involved`; `--wide` renders that as an
`Involved` column so operators can list or field-select events by the concrete
`Kind/name` object without opening raw payload JSON.
Runtime API discovery advertises those concrete Event selector paths, including
`spec.involved_object.kind`, `spec.involved_object.name`, `status.involved`,
and `status.involved_uid`, so automation should use the selector contract rather
than parsing raw event payloads.

Event lists expose `metadata.resourceVersion`, `metadata.continue`,
`metadata.remainingItemCount`, `metadata.returned`, and numeric
`metadata.next_after_row_seq` for manual debugging/backfill. Prefer
`--continue <metadata.continue>` for manual paging.

Resource describe responses include `event_list` with the same resource-scoped
event metadata as the `/events` subresource. Read events from
`event_list.events` so cursor and resource-version metadata stay attached.

## Inspect Bundles

Use an inspect bundle when you need a portable operator snapshot:

```bash
buc runs inspect <cloud_run_id> --json
buc runs inspect <cloud_run_id> --kind RunnerInstance --json
```

The bundle includes API discovery, the filtered resource inventory, the
selectors used to build the bundle, resource health, bounded collection metrics,
`event_list` with the bounded recent event stream metadata, and concrete log
subresource references for offline triage or support handoff.

## Compare Runs

For hosted Cloud, compare runs from resource-visible trial and metric rows:

```bash
buc runs resources <cloud_run_id> --kind Trial
buc runs resources <cloud_run_id> --kind MetricObservation
buc runs top <cloud_run_id> --kind Trial --limit 100
buc runs metrics <cloud_run_id> --kind Trial --limit 100
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
