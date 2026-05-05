# AgentLab User Docs

These docs are the product-facing path through the repo. Start here when you want to create, run, grade, and inspect an experiment.

The rest of `docs/` contains architecture notes, patch specs, audits, and design history. Those are useful for development, but they are not the source of truth for first-time users.

## Read In This Order

1. [Quickstart: Run The Benchmark Demo](quickstart.md)
2. [What You Must Provide](what-you-provide.md)
3. [Agent Runtime Contract](agent-runtime-contract.md)
4. [Task Rows And Benchmarks](task-rows.md)
5. [Graders And Mappers](graders-and-mappers.md)
6. [Environment And Secrets](env-and-secrets.md)
7. [Inspecting Results](inspecting-results.md)
8. [Troubleshooting](troubleshooting.md)

## The Mental Model

An AgentLab run has five moving parts:

| Part | Owner | Purpose |
| --- | --- | --- |
| Experiment YAML | You | Declares variants, runtime, benchmark, metrics, and policy. |
| Task rows | You or benchmark author | Declare the tasks and task sandbox image/workdir. |
| Agent runtime | You | Runs your agent application against one task. |
| Grader | You or benchmark author | Converts the agent result into a scored conclusion. |
| Runner | AgentLab | Builds, validates, executes, persists evidence, and exposes results. |

The runner does not know how your agent thinks. It only needs your agent to honor the runtime contract: read the trial input, do the work, and write a valid result.

## Current Golden Path

The canonical runnable path is the SWE-bench mini demo under `demos/`.

It uses:

- real benchmark-style task rows derived from SWE-bench issue instances
- a containerized Node agent application
- a real grader command that emits `trial_conclusion_v1`
- event and artifact collection
- the same `build -> describe -> preflight -> run -> inspect` flow used by normal experiments

The demo agent is deterministic so documentation can be tested repeatably. Replace that agent runtime with your own app when authoring a real evaluation.

