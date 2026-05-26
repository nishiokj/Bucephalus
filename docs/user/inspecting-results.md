# Inspecting Results

After `bucephalus run`, use the run id or run directory to inspect results.

Durable run facts are stored in the account SQLite database, not in per-run JSONL fact files. By default the database is `$HOME/.bucephalus/bucephalus.sqlite`; set `BUCEPHALUS_DB=/absolute/path/to/bucephalus.sqlite` to choose an explicit database, or `BUCEPHALUS_HOME=/absolute/path/to/dir` to move Bucephalus's default home. Legacy `AGENTLAB_*` host env vars are still accepted as fallbacks during migration.

## List Runs

```bash
bucephalus runs
bucephalus runs --json
```

## Standard Views

```bash
bucephalus views <run_id>
bucephalus views <run_id> run_progress
bucephalus views <run_id> variant_summary
bucephalus views <run_id> scoreboard
```

For a run that is still executing, use the live view command against the same
account SQLite database:

```bash
bucephalus views-live <run_id> run_progress
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

Useful raw views include:

| View | Purpose |
| --- | --- |
| `trials` | Committed trial outcomes and primary metrics. |
| `metrics_long` | Declared metric observations plus metric definition metadata. |
| `metric_definitions` | Canonical metric declarations from experiment YAML. |
| `events` | Ingested runtime events. |
| `contract_stages` | Runtime contract stage status and details. |
| `variant_snapshots` | Variant binding values per trial. |

Runtime events are written from declared `stages.agent.events` JSONL
captures. The runner stores the original JSON line as payload and exposes common
columns when present. The full event payload is available as `payload_json`:

```bash
bucephalus query <run_id> "
  SELECT trial_id, row_seq, event_type, ts, tool_name, outcome_status,
         usage_tokens_in, usage_tokens_out, payload_json
  FROM events
  ORDER BY schedule_idx, row_seq
  LIMIT 50
"
```

Custom event fields can be queried from the payload without adding them to the
fixed event-row schema:

```bash
bucephalus query <run_id> "
  SELECT trial_id, row_seq,
         json_extract_string(payload_json, '$.rex.request_id') AS rex_request_id,
         try_cast(json_extract(payload_json, '$.rex.server_ms') AS DOUBLE) AS rex_server_ms
  FROM events
  ORDER BY schedule_idx, row_seq
  LIMIT 50
"
```

The `events` view includes live rows whose trial has not committed yet, so it is
the right place to inspect whether an executing agent is still producing
observable work.

## Compare Runs

Use the regression view for the built-in run trend summary:

```bash
bucephalus views <run_id> run_trend
```

For ad hoc comparisons, query the account database through `bucephalus query`:

```bash
bucephalus query <run_id> "
  SELECT variant_id, metric_name, avg(try_cast(metric_value AS DOUBLE)) AS mean_value
  FROM metrics_long
  GROUP BY variant_id, metric_name
  ORDER BY metric_name, variant_id
"
```

## Important Files

Run files live under `.lab/runs/<run_id>/`.

Build package files live under `.lab/builds/<package>/`.

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

The account SQLite path is also returned by run commands in JSON output as `account_sqlite_path`.
