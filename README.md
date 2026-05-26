# Bucephalus

A command-line workbench for building, running, recovering, and inspecting agent experiments.

[User Docs](docs/user/index.md) · [Concepts](docs/user/concepts.md) · [YAML Reference](docs/user/experiment-yaml-reference.md) · [Distribution](docs/distribution.md)

**Built with:** Rust · Tokio · Docker · SQLite

---

## What Bucephalus Does

Bucephalus turns an experiment YAML file into a sealed, content-addressed package, then executes the package as a durable run. Each run expands into trials across variants, cases, and repeats. Trials run through declared stages such as case setup, agent execution, grading, and metric extraction.

The runner is built around a few current product boundaries:

- **v1 experiment YAML.** New authoring uses `matrix`, `cases`, `stages`, `ephemerals`, `externals`, `runtime`, and `policy`.
- **Sealed packages.** `lab build` resolves and freezes experiment inputs before execution.
- **Explicit runtime contract.** Agents read `BUCEPHALUS_TRIAL_INPUT_PATH` and write JSON to `BUCEPHALUS_RESULT_PATH` under `/bucephalus`.
- **Durable execution.** Runs persist control, schedule progress, trial state, events, metrics, and committed facts so `pause`, `resume`, `recover`, `continue`, and `kill` can operate truthfully.
- **Backend-aware execution.** Local Docker is the primary runtime backend; Modal is supported for remote sandbox execution with a narrower feature set.

## Workflow

```bash
# 1. Build a sealed package from v1 experiment YAML.
lab build experiment.yaml --out .lab/builds/my-experiment --json

# 2. Run static package hygiene checks.
lab check-package .lab/builds/my-experiment --json

# 3. Check launch readiness: images, env bindings, resources, and grader reachability.
lab preflight .lab/builds/my-experiment --json

# 4. Execute a real end-to-end smoke test for the package digest.
lab run .lab/builds/my-experiment --smoke-test --materialize full --json

# 5. Run the full experiment.
lab run .lab/builds/my-experiment --materialize full --json
```

`lab build-run` combines build and run for cases where you already trust the experiment shape. For new experiments, the staged flow above is easier to debug.

Full runs are smoke-test gated. If a package digest has not passed a smoke test, interactive runs warn before proceeding; non-interactive runs must choose `--smoke-test` or `--run-dangerously`.

## Install

From this checkout:

```bash
cargo install --path .
```

That installs the short CLI command:

```bash
lab --help
```

For local SQL analysis features, build with the optional analysis engine:

```bash
cargo install --path . --features duckdb_engine
```

Container-backed trials require Docker or OrbStack for `local-docker` runs. Modal runs require Modal and S3-compatible sync configuration; see [Runtime Backends](docs/user/runtime-backends.md).

## Commands

Common operator commands:

| Command | Purpose |
| --- | --- |
| `lab build` | Resolve YAML and create a sealed package. |
| `lab check-package` | Run static checks against a sealed package. |
| `lab preflight` | Validate dynamic launch readiness before execution. |
| `lab run` | Execute a sealed package. |
| `lab build-run` | Build from YAML and execute in one command. |
| `lab pause` / `lab resume` | Pause and resume in-flight work when supported by persisted runtime state. |
| `lab recover` / `lab continue` | Reconcile a stale run and continue the schedule. |
| `lab kill` | Stop a running or paused experiment. |
| `lab runs` | List known runs from the account database. |
| `lab views` / `lab query` | Inspect committed run facts and analysis views. |

Run `lab <command> --help` for command-specific flags.

## Runtime Contract

Bucephalus injects `BUCEPHALUS_*` variables into agent and grader processes. The most important ones are:

| Variable | Meaning |
| --- | --- |
| `BUCEPHALUS_TRIAL_INPUT_PATH` | JSON input for the current trial. |
| `BUCEPHALUS_RESULT_PATH` | JSON result path the agent must write. |
| `BUCEPHALUS_RUN_ID` | Current run id. |
| `BUCEPHALUS_TRIAL_ID` | Current trial id. |
| `BUCEPHALUS_TRAJECTORY_PATH` | Optional JSONL event stream path for `cli_events` integrations. |

The in-container filesystem contract uses `/bucephalus/in`, `/bucephalus/out`, `/bucephalus/state`, `/bucephalus/workspace`, and `/bucephalus-events`.

## State And Results

Run artifacts live under `.lab/` by default. Account-level facts are stored in:

```text
$HOME/.bucephalus/bucephalus.sqlite
```

Override this with `BUCEPHALUS_DB=/absolute/path/to/bucephalus.sqlite` or `BUCEPHALUS_HOME=/absolute/path/to/dir`.

Use [Inspecting Results](docs/user/inspecting-results.md), [Metrics](docs/user/metrics.md), and [Package Checks](docs/user/package-checks.md) for the current analysis surface.

## Docs

Start with:

- [Concepts](docs/user/concepts.md): experiment, run, trial, case, stage, ephemeral, and external.
- [Experiment YAML Reference](docs/user/experiment-yaml-reference.md): the canonical v1 authoring shape.
- [What You Provide](docs/user/what-you-provide.md): required inputs for successful experiments.
- [Agent Runtime Contract](docs/user/agent-runtime-contract.md): process env, paths, outputs, and event capture.
- [Graders And Mappers](docs/user/graders-and-mappers.md): declared grader transport and metric extraction.
- [Environment And Secrets](docs/user/env-and-secrets.md): launch-time env and secret binding.
- [Runtime Backends](docs/user/runtime-backends.md): Local Docker, Modal, and active runtime caps.
- [Troubleshooting](docs/user/troubleshooting.md): common build, preflight, run, and analysis failures.

## Repository Map

```text
Cargo.toml                         Publishable Rust crate for the lab CLI
schemas/                           JSON Schemas for package, run, and trial artifacts
rust/crates/lab-cli/                Command parsing and operator-facing UI
rust/crates/lab-runner/             Build, preflight, scheduling, execution, persistence
rust/crates/lab-core/               Shared runtime contract constants and helpers
rust/crates/lab-schemas/            Embedded schema loading
rust/crates/lab-provenance/         Content addressing and attestations
rust/crates/lab-analysis/           Optional SQL analysis views
docs/user/                          Product-facing user documentation
docs/specs/                         Design history and implementation notes
tools/charts/                       Local chart gallery tooling
```

## License

MIT. See [LICENSE](LICENSE).
