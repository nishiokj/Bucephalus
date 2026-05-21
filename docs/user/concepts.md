# Concepts

AgentLab authoring uses one noun per level:

| Noun | Meaning |
| --- | --- |
| Experiment | A YAML recipe. |
| Run | One execution of an experiment. |
| Trial | One variant x case x repeat. |
| Case | A declared problem, unwrapped and materialized into a workspace for a trial. |
| Workspace | The subject the machinery acts on. It is not a stage, ephemeral, or external. |
| Stage | A link in the runner-wired chain, such as agent or grader. Each stage declares only its own input and output. |
| Transport Envelope | The runner-owned shape that carries one stage's output into the next stage's input. |
| Ephemeral | A per-trial resource the runner starts and tears down, but which is not a link in the stage chain. Sidecars, MCP servers, and memory systems belong here. |
| External | Anything outside runner jurisdiction: network egress, credentials, third-party APIs, and other boundary crossings. |

The classification test is:

1. Is it a link the runner wires in the chain? If yes, it is a Stage.
2. If not, does the runner own its lifecycle? If yes, it is an Ephemeral. If no, it is an External.

The reproducibility boundary is the line between ephemerals and externals. The runner can recreate stages, workspaces, transport envelopes, and ephemerals. It can only authorize access to externals; it cannot make those services reproducible.

Canonical YAML sections:

| Section | Responsibility |
| --- | --- |
| `matrix` | Variants, cases, repeats, and seeds. |
| `cases` | Local JSONL case source. |
| `scheduling` | Concurrency, ordering, and comparison. |
| `stages` | Stage chain and declared I/O. |
| `ephemerals` | Per-trial resources attached to stages. |
| `externals` | Declared boundary crossings. |
| `runtime` | Backend declarations plus external perimeter configuration. |
| `policy` | Timeouts, sanitization, validity, and runner policy. |

Removed v0 nouns are hard rejected: `baseline`, `variant_plan`, `dataset`, `design`, `bindings`, `runtime_overrides`, agent `artifact`, scattered network fields, and `secret_files`.
