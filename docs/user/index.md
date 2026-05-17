# AgentLab User Docs

These docs are the product-facing path through the repo. Start here when you want to create, run, grade, and inspect an experiment.

The rest of `docs/` contains architecture notes, patch specs, audits, and design history. Those are useful for development, but they are not the source of truth for first-time users.

## Read In This Order

1. [Quickstart: Run The Benchmark Demo](quickstart.md)
2. [What You Must Provide](what-you-provide.md)
3. [Experiment YAML Reference](experiment-yaml-reference.md)
4. [Bring Your Own Agent](bring-your-own-agent.md)
5. [Agent Runtime Contract](agent-runtime-contract.md)
6. [Task Rows And Benchmarks](task-rows.md)
7. [Metrics](metrics.md)
8. [Package Checks](package-checks.md)
9. [Grader Transport](grader-transport.md)
10. [Grader Runtime](graders-and-mappers.md)
11. [Environment And Secrets](env-and-secrets.md)
12. [Inspecting Results](inspecting-results.md)
13. [Troubleshooting](troubleshooting.md)

## The Mental Model

An AgentLab run has five moving parts:

| Part | Owner | Purpose |
| --- | --- | --- |
| Experiment YAML | You | Declares variants, `trial_runtime`, metrics, and policy. |
| Task rows | You or benchmark author | Declare task payloads and optional `task_row_v2.runtime.container_image`. |
| Agent runtime | You | Runs your agent application against one task. |
| Grader | You or benchmark author | Runs after the agent using declared inputs and declared outputs. |
| Runner | AgentLab | Builds, validates, executes, persists evidence, and exposes results. |

The runner does not know how your agent thinks. It only needs your agent to honor the runtime contract: read the trial input, do the work, and write a valid result.

Custom metrics are also explicit. The runner only persists custom metric observations that are declared in `experiment.yaml`; it does not treat arbitrary agent result fields as analytics schema.

## Current Golden Path

The canonical runnable path is the SWE-bench mini demo under `demos/`.

It uses:

- real benchmark-style task rows derived from SWE-bench issue instances
- a containerized Node agent application
- a real grader command and declared scoring output
- event and artifact collection
- the same `build -> check-package -> preflight -> run -> inspect` flow used by normal experiments

The demo agent is deterministic so documentation can be tested repeatably. Replace that agent runtime with your own app when authoring a real evaluation.
