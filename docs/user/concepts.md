# Concepts

AgentLab v1 uses one noun per level:

| Noun | Meaning |
| --- | --- |
| Experiment | A YAML recipe. |
| Run | One execution of an experiment. |
| Trial | One variant × task × repeat. |
| Apparatus | What the runner provisions for a trial: workspace, procedure, and sidecars. |
| Stage | One runtime invocation inside the apparatus: task setup, agent, or grader. |
| Sidecar | A per-trial service container attached to one or more stages. |
| External perimeter | What the apparatus may reach outside the runner: secrets and network egress. |

The reproducibility boundary is the line between apparatus and external perimeter. The runner can recreate the apparatus. It can only authorize access to external services; it cannot make those services reproducible.

Canonical YAML sections:

| Section | Responsibility |
| --- | --- |
| `matrix` | Variants, tasks, repeats, and seeds. |
| `scheduling` | Concurrency, ordering, and comparison. |
| `trial_runtime` | Apparatus stages and their declared I/O. |
| `sidecars` | Per-trial services used by stages. |
| `runtime` | Backend declarations plus external perimeter. |
| `policy` | Timeouts, sanitization, validity, and runner policy. |

Removed v0 nouns are hard rejected: `baseline`, `variant_plan`, `dataset`, `design`, `bindings`, `runtime_overrides`, agent `artifact`, scattered network fields, and `secret_files`.
