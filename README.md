# AgentLab

A command-line workbench for evaluating AI agents as reproducible experiments:
declare your variants, cases, and grading in YAML, and AgentLab seals them into
a content-addressed package, runs every trial in an isolated container, and
records each result to a queryable database.

[Demo](#) · [Walkthrough](#) · [Docs](docs/user/index.md) · [Architecture](docs/specs/ARCHITECTURE.md) · [Concepts](docs/user/concepts.md)

> **Placeholder** — screenshot or GIF of `lab run` and `lab views` here.

**Built with:** Rust · Tokio · Docker · SQLite · DuckDB · ratatui

---

## Why I built this

Agent evaluations are usually a pile of ad-hoc scripts. They are hard to
reproduce, hard to trust, and hard to compare — when a run dies halfway through
you start over, and when the numbers look wrong you can't tell whether the
agent, the harness, or the environment was at fault.

AgentLab makes the experiment itself a durable, content-addressed artifact. It
draws a hard line between what the runner can recreate (cases, sandboxes,
sidecars) and what it cannot (the network, credentials, third-party APIs), so a
result is either reproducible or honestly labelled as not.

## What it does

- **Declarative experiments.** One YAML file describes variants, cases, the
  stage chain, grading, and metrics, under a strict, single-noun-per-level
  model — no ambiguity about what each section owns.
- **Sealed, content-addressed packages.** `lab build` resolves an experiment
  and freezes it into a package identified by a SHA-256 digest. Same digest,
  same experiment — every time.
- **Isolated containerized trials.** Each trial runs in its own Docker (or
  Modal) sandbox with an explicit filesystem and network contract.
- **Durable execution.** `pause`, `resume`, `kill`, `recover`, and `continue`
  operate on persisted state. A crashed run reconciles from disk and resumes at
  the next uncommitted slot, with exactly-once result publication.
- **Replay and fork.** Re-run any package by digest, or fork a trial from a
  checkpoint to explore an alternate path — both are just trials with
  provenance back to a parent.
- **Queryable results.** Trials, metrics, and events land in SQLite. Inspect
  them with standardized `views`, run ad-hoc `SELECT`s with `query`, or compare
  variants with built-in analysis.

## How it works

An experiment flows through four stages, then a fixed build-to-inspect
lifecycle:

```
  Cases  ─────►  Stages  ─────►  Grading  ─────►  Analysis
  what to        how the         per-trial        cross-trial
  evaluate       agent runs      scoring          comparison

  lab build  ─►  lab check-package  ─►  lab preflight  ─►  lab run  ─►  lab views
  seal +         static package        dynamic launch      isolated     query the
  digest         hygiene               readiness           trials       results
```

`build` resolves the YAML and seals a package. `check-package` runs static
hygiene checks; `preflight` checks dynamic launch readiness (images, grader
reachability, env bindings). `run` schedules trials one slot at a time, each in
its own sandbox, grades them, and commits results. `views` and `query` read
them back. A package must pass a real end-to-end smoke test before a full run
is allowed.

See [Concepts](docs/user/concepts.md) for the noun model and
[Architecture](docs/specs/ARCHITECTURE.md) for the runner internals.

> **Placeholder** — architecture diagram here.

## Notable implementation details

- **Content-addressable everything.** `experiment.yaml` → `resolved_experiment.json`
  → digest. Packages, checkpoints, and fork points are all addressed by canonical
  JSON + SHA-256, so "frozen experiment" and "replay" have exact definitions.
- **A durable trial state machine.** Every trial persists its phase
  (`agent_running`, `grader_running`, `commit_pending`, `committed`, …) to
  `trial_runtime_state.json`. Recovery and control decisions are derivable from
  persisted records alone — no in-memory state is load-bearing.
- **Exactly-once commit, separate from retries.** A trial may retry; a schedule
  slot publishes exactly one committed result set. A crash between grading and
  publication reconciles instead of fabricating a duplicate.
- **The reproducibility boundary.** Sidecars and MCP servers are *ephemerals* —
  the runner owns their lifecycle. The network and credentials are *externals* —
  it can only authorize them. That boundary is enforced, not documented away.
- **Backend-agnostic transport.** Docker is driven over the daemon's raw HTTP
  API, isolated behind one module; Modal is a second backend behind the same
  surface. Orchestration code never shells out to `docker`.
- **Hard cutovers, no legacy cruft.** The v1 noun model hard-rejects every
  superseded v0 shape at validation time instead of silently accepting both.

## Trade-offs

- **Optimized for reproducibility and durability.** Sealed packages, persisted
  state, exactly-once commit, and a strict authoring contract. A run survives a
  crash; a result is either reproducible or labelled otherwise.
- **Not optimized for quick REPL iteration.** Authoring is YAML through a
  resolver pipeline — rigorous and reviewable, but more verbose than a fluent
  builder. A programmatic builder API is [designed](docs/specs/DESIGN_PRINCIPLES.md)
  but not shipped.
- **Not yet optimized for cloud scale.** Local Docker is the complete path.
  Modal works but rejects ephemerals until backend-native sidecars exist;
  Kubernetes and OTel export are designed-for but unimplemented.
- **A deliberately strict surface.** The validator rejects old YAML shapes
  outright rather than carrying compatibility shims. Good for a clean product,
  a sharp edge during migration.
- **What I'd do next:** ship the builder API, finish Modal ephemerals, and add
  finer-grained per-trial resource scheduling.

## Run locally

Prerequisites: a Rust toolchain with `cargo`, and Docker or OrbStack running.

```bash
# build the CLI
cargo build --release --bin lab
LAB="$(pwd)/target/release/lab"

# build a sealed package from an experiment
"$LAB" build experiment.yaml --out .lab/builds/demo --json

# static checks, then dynamic launch readiness
"$LAB" check-package .lab/builds/demo --json
"$LAB" preflight .lab/builds/demo --json

# smoke test (one case per variant), then the full run
"$LAB" run .lab/builds/demo --smoke-test --materialize full --json
"$LAB" run .lab/builds/demo --materialize full --json

# inspect results
"$LAB" views <run-id>
"$LAB" query <run-id> "SELECT * FROM trials LIMIT 20"
```

Full walkthrough: [Quickstart](docs/user/quickstart.md). Installation and
release packaging: [Distribution](docs/distribution.md).

## Repo map

```
rust/crates/
  lab-cli/         The `lab` binary — command parsing and the operator surface
  lab-runner/      Build, scheduling, trial execution, persistence, Docker/Modal transport
  lab-core/        Shared domain types and the in-container agent contract
  lab-analysis/    DuckDB-backed views and the SQL query surface
  lab-schemas/     Versioned JSON Schemas, compiled into the binary
  lab-provenance/  Content addressing, digests, and run attestations
schemas/           JSON Schemas for every artifact written to disk
docs/user/         Product docs — YAML reference, concepts, authoring guides
docs/specs/        Architecture notes and design history
```

## Roadmap

- Programmatic builder API alongside YAML authoring
- Backend-native sidecar support for Modal
- Finer-grained per-trial resource scheduling
- Kubernetes compute backend and OTel trace export

## License

MIT — see [LICENSE](LICENSE).
