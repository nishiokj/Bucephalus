# Inspecting Results

After `lab run`, use the run id or run directory to inspect results.

Durable run facts are stored in the account SQLite database, not in per-run JSONL fact files. By default the database is `$HOME/.agentlab/agentlab.sqlite`; set `AGENTLAB_DB=/absolute/path/to/agentlab.sqlite` to choose an explicit database, or `AGENTLAB_HOME=/absolute/path/to/dir` to move AgentLab's default home.

## List Runs

```bash
lab runs
lab runs --json
```

## Standard Views

```bash
lab views <run_id>
lab views <run_id> run_progress
lab views <run_id> variant_summary
lab views <run_id> scoreboard
```

For A/B-style experiments:

```bash
lab views <run_id> comparison_summary
lab views <run_id> task_outcomes
lab views <run_id> task_metrics
```

## SQL Query

```bash
lab query <run_id> "SELECT * FROM trials LIMIT 20"
```

Use JSON for scripts:

```bash
lab query <run_id> "SELECT * FROM trials LIMIT 20" --json
```

Useful raw views include:

| View | Purpose |
| --- | --- |
| `trials` | Committed trial outcomes and primary metrics. |
| `metrics_long` | Declared metric observations plus metric definition metadata. |
| `metric_definitions` | Canonical metric declarations from experiment YAML. |
| `events` | Ingested hook events. |
| `contract_stages` | Runtime contract stage status and details. |
| `variant_snapshots` | Variant binding values per trial. |

## Compare Runs

Use `lab trend` for the built-in cross-run summary:

```bash
lab trend --experiment my_eval --limit 10
```

For ad hoc comparisons, query the account database through `lab query`:

```bash
lab query <run_id> "
  SELECT variant_id, metric_name, avg(try_cast(metric_value AS DOUBLE)) AS mean_value
  FROM metrics_long
  GROUP BY variant_id, metric_name
  ORDER BY metric_name, variant_id
"
```

## Variants

```bash
lab variants <run_id>
lab variants <run_id> control
lab variants <run_id> treatment --against control
```

## Important Files

Run files live under `.lab/runs/<run_id>/`.

| Path | Purpose |
| --- | --- |
| `manifest.json` | Run metadata. |
| `resolved_experiment.json` | Resolved experiment config. |
| `attestation.json` | Provenance summary. |
| `trials/<trial_id>/out/result.json` | Agent result. |
| `trials/<trial_id>/out/mapped_grader_output.json` | Grader conclusion. |
| `trials/<trial_id>/out/<output_mount path>/` | Files written through `trial_runtime.agent.output_mounts`. |
| `trials/<trial_id>/agent_stdout.log` | Agent stdout. |
| `trials/<trial_id>/agent_stderr.log` | Agent stderr. |
| `trials/<trial_id>/grader_stdout.log` | Grader stdout. |
| `trials/<trial_id>/grader_stderr.log` | Grader stderr. |

The account SQLite path is also returned by run commands in JSON output as `account_sqlite_path`.
