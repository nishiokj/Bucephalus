# Inspecting Results

After `bucephalus run`, use the run id or run directory to inspect results.

Durable run facts are stored through the configured run store, not in per-run JSONL fact files. Local runs default to the account SQLite database at `<storage>/bucephalus.sqlite`; set `BUCEPHALUS_DB=/absolute/path/to/bucephalus.sqlite` to choose an explicit local database, or `BUCEPHALUS_HOME=/absolute/path/to/dir` to move Bucephalus's default home. Cloud workers select Postgres with `BUCEPHALUS_RUN_STORE=postgres` and `BUCEPHALUS_RUN_STORE_URL`.

## List Runs

```bash
bucephalus runs
bucephalus runs --json
```

`runs` includes a `next_action` column. Active runs point at `views-live`;
interrupted runs point at `recover`.

For an interrupted run:

```bash
bucephalus recover <run_id>
bucephalus continue <run_id>
```

`--run-dir <path>` remains available when you need to operate on a run directory
that is not registered in the configured run store.

## Standard Views

```bash
bucephalus views <run_id>
bucephalus views <run_id> run_progress
bucephalus views <run_id> observability
bucephalus views <run_id> trial_diagnostics
bucephalus views <run_id> variant_summary
bucephalus views <run_id> scoreboard
```

For a run that is still executing, use the live view command against the same
configured run store:

```bash
bucephalus views-live <run_id> run_progress
bucephalus views-live <run_id> observability --once
bucephalus views-live <run_id> events --limit 50
```

For A/B-style experiments:

```bash
bucephalus views <run_id> comparison_summary
bucephalus views <run_id> task_outcomes
bucephalus views <run_id> task_metrics
```

## SQL Query

```bash
bucephalus query <run_id> "SELECT * FROM trials LIMIT 20"
```

Use JSON for scripts:

```bash
bucephalus query <run_id> "SELECT * FROM trials LIMIT 20" --json
```


## Trust and Diagnostics

Use `observability` before trusting a completed run. It summarizes Buc's proof
surface: completed rows, result-file validity, agent exit status, timeouts,
event coverage, tool-event coverage, grader/mapping errors, connector errors,
and empty extracted predictions.

```bash
bucephalus views <run_id> observability
```

Use `trial_diagnostics` when a run "passed" mechanically but the result looks
wrong. It is intentionally not an interpretation view. It gives one row per
trial with raw facts Buc persisted: phase, committed outcome/success, agent exit
code, timeout flag, result parse state, candidate artifact state/source, grader
stages, sandbox workdir, and stdout/stderr paths. Events are not summarized
there; they are printed as raw event payloads.

```bash
bucephalus views <run_id> trial_diagnostics --max-rows 100
```

Use `events` to print the actual event payloads to stdout.

```bash
bucephalus views <run_id> events
```

Interpret absent events as "not observed", not "no tools were used". Buc can
only print semantic model/tool activity when the selected client/protocol emits
events or an explicit event capture is declared. Agent exit status, result file
presence/parse status, grader status, and sandbox paths are Buc-owned and do not
depend on semantic trace support.

Useful raw views include:

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

Runtime events are written from declared `stages.agent.events` JSONL
captures. `bucephalus views <run_id> events` prints the captured payloads.
For SQL, `raw_events.event_json` is the payload Buc ingested, and
`raw_events.buc_event_row_json` is Buc's row wrapper. Malformed JSONL rows are
represented as `trajectory_parse_error` payloads with line numbers, but the raw
malformed line is omitted so inspection views do not become a secret or local
path sink:

```bash
bucephalus query <run_id> "
  SELECT trial_id, row_seq, event_type, event_json
  FROM raw_events
  ORDER BY schedule_idx, row_seq
  LIMIT 50
"
```

Custom event fields can be queried from the payload without adding them to the
fixed event-row schema:

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

The `raw_events` view includes live rows whose trial has not committed yet, so it
is the right place to inspect whether an executing agent is still producing
observable work.

## Cleaning Local Runs

Preview local run cleanup before deleting anything:

```bash
bucephalus clean --runs --dry-run
bucephalus clean --runs --dry-run --json
```

Dry-run reports active, interrupted, and untracked local run-root entries without
requiring override flags.
Actual deletion requires `--force`. Active runs also require
`--include-active`, and interrupted runs require `--include-interrupted`; recover
or continue interrupted runs first when you still need their schedule state.
Untracked entries under the local run root, such as partial or corrupt run
directories, require `--include-untracked` so cleanup never silently removes
files that were not visible in the run inventory preview.

## Support Bundles

Use `publish` when you need a shareable support bundle for a run:

```bash
bucephalus publish --run-dir <run_dir>
bucephalus publish --run-dir <run_dir> --out support.zip --json
```

By default, the bundle is written under `<run_dir>/debug_bundles/` with a
timestamped filename. If you pass `--out`, Bucephalus refuses to overwrite an
existing file.

Support bundles are redacted and curated. They include top-level run metadata
and selected trial JSON/JSONL diagnostics, but skip raw logs, runtime state,
workspace/state/temp/auth directories, unsupported artifact types, and
secret-looking filenames. JSON and JSONL entries redact secret-like keys,
environment fields, local paths, command/argument/URL fields, and
prompt/content/input/output/message-like fields. Inspect the bundle before
sharing it.

## Compare Runs

Use the regression view for the built-in run trend summary:

```bash
bucephalus views <run_id> run_trend
```

For ad hoc comparisons, query the account database through `bucephalus query`:

```bash
bucephalus query <run_id> "
  SELECT variant_id, metric_name, avg(CAST(metric_value AS REAL)) AS mean_value
  FROM metrics_long
  GROUP BY variant_id, metric_name
  ORDER BY metric_name, variant_id
"
```

## Important Files

Run files live under `<run_dir>/`.

Build package files live under `<storage>/builds/<package>/` by default, or under the directory passed with `bucephalus build --out`.

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

The selected run store location is also returned by run commands in JSON output as `run_store_location`.
