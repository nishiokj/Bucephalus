# Concepts

Bucephalus authoring uses one noun per level:

| Noun | Meaning |
| --- | --- |
| Experiment | A YAML recipe. |
| Run | One execution of an experiment. |
| Trial | One variant x case x repeat. |
| Case | A declared problem, unwrapped and materialized into a workspace for a trial. |
| Workspace | The subject the machinery acts on. It is not a stage, service, or external. |
| Stage | A link in the runner-wired chain, such as agent or grader. Each stage declares only its own input and output. |
| Transport Envelope | The runner-owned shape that carries one stage's output into the next stage's input. |
| Service | A per-trial resource the runner starts and tears down, but which is not a link in the stage chain. MCP servers, memory systems, and helper daemons belong here. |
| External | Anything outside runner jurisdiction: network egress, credentials, third-party APIs, and other boundary crossings. |

The classification test is:

1. Is it a link the runner wires in the chain? If yes, it is a Stage.
2. If not, does the runner own its lifecycle? If yes, it is a Service. If no, it is an External.

The reproducibility boundary is the line between services and externals. The runner can recreate stages, workspaces, transport envelopes, and services. It can only authorize access to externals; it cannot make external services reproducible.

Canonical YAML sections:

| Section | Responsibility |
| --- | --- |
| `matrix` | Variants, cases, and repeats. |
| `scheduling` | Concurrency, ordering, and comparison. |
| `stages` | Stage chain and declared I/O. |
| `services` | Per-trial resources attached to stages. |
| `externals` | Declared boundary crossings. |
| `runtime` | Backend declarations plus external perimeter configuration. |
| `policy` | Timeouts, sanitization, validity, and runner policy. |

`ephemerals` remains accepted as a compatibility alias for `services`, but new
authoring should use `services`.

Removed v0 nouns are hard rejected: `baseline`, `variant_plan`, `dataset`, `design`, `bindings`, `runtime_overrides`, agent `artifact`, scattered network fields, and `secret_files`.
