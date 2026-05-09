# Inspecting Results

After `lab run`, use the run id or run directory to inspect results.

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
| `run.sqlite` | Queryable run database. |
| `trials/<trial_id>/out/result.json` | Agent result. |
| `trials/<trial_id>/out/mapped_grader_output.json` | Grader conclusion. |
| `trials/<trial_id>/out/<output_mount path>/` | Files written through `runtime.agent_runtime.output_mounts`. |
| `trials/<trial_id>/agent_stdout.log` | Agent stdout. |
| `trials/<trial_id>/agent_stderr.log` | Agent stderr. |
| `trials/<trial_id>/grader_stdout.log` | Grader stdout. |
| `trials/<trial_id>/grader_stderr.log` | Grader stderr. |
