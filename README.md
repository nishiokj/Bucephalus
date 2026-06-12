# Bucephalus

A command-line workbench for building, running, recovering, and analyzing agent experiments. 

Experiments are controlled executions where we aim to extract measurable outcomes for the purpose of understanding and fueling decisions. Experiments can be but not limited to:
- AB testing two different models in the same harness against the same benchmark
- Sandboxing agents with your product + documentation and measuring the frequency of successful onboarding / bootstrapping
- Regression testing your agent system on a new model release

[User Docs](docs/user/index.md) · [Cookbook](cookbook/README.md) · [Concepts](docs/user/concepts.md) · [YAML Reference](docs/user/experiment-yaml-reference.md) · [Hosted Cloud CLI](docs/user/cloud-cli.md) · [Distribution](docs/distribution.md)

**Built with:** Rust · Tokio · Docker · SQLite

---

## Install

Install the latest release:

```bash
curl -fsSL https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh | sh
```

The installer downloads the right prebuilt archive for macOS or Linux, verifies
its SHA-256 checksum and archive shape, and installs `bucephalus` plus its
runtime helpers to `$HOME/.local/bin`.

```bash
bucephalus --help
```

### Run your own agent on your own machine (optional)

The lowest-friction way to use Bucephalus: point an existing agent at a benchmark
and run it right on your machine — no Docker, no YAML to author. Run setup once
after install:

```bash
bucephalus setup
```

That installs a small managed local runtime and registers Bucephalus as a tool
with any MCP clients it detects, so an agent can launch itself against a
benchmark and stream results back. (This is the "Tier 1 — Local" row in the
[Tiers](#tiers) table below.) To do both from the install one-liner:

```bash
curl -fsSL https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh | env BUCEPHALUS_SETUP=1 sh
```

Check or remove that local setup with:

```bash
bucephalus setup status
bucephalus setup uninstall
bucephalus setup uninstall --client cursor-project --project <project-dir>
```

Update an installed release with:

```bash
bucephalus update
```

`update` installs into the same directory as the running `bucephalus` binary and
refreshes daemon/MCP setup by default. Use `bucephalus update --setup=false` to
replace only the binaries.

The installer adds `$HOME/.local/bin` to your `PATH` (via your shell profile) if
it isn't already there — restart your shell afterward. To install elsewhere or
skip the profile edit:

```bash
# install to a different directory
curl -fsSL https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh | env BUCEPHALUS_INSTALL_DIR=/usr/local/bin sh

# don't touch my shell profile
curl -fsSL https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh | env BUCEPHALUS_NO_MODIFY_PATH=1 sh
```

Container-backed trials require Docker or OrbStack for `local-docker` runs. Modal
runs require Modal and S3-compatible sync configuration; see
[Runtime Backends](docs/user/runtime-backends.md).

## Start Here

Three commands take you from nothing to a finished run. No repo to clone, no
API key required:

```bash
bucephalus init my-eval --client cli --command 'python3 agent.py --input {{input}} --output {{output}}'
bucephalus dev my-eval
bucephalus run my-eval/experiment.yaml
```

1. **`init`** scaffolds `my-eval/` with a starter experiment, built from the way
   a client invokes your agent.
2. **`dev`** is the local happy path: it finds `experiment.yaml`, builds a sealed
   package, runs static checks and dynamic preflight, and executes a smoke test.
3. **`run`** builds automatically and, for new packages, smoke-tests before
   launching the full experiment.

Browse [the cookbook](cookbook/README.md) for copy-ready starter experiments.

For scripts or deeper inspection, the package-level workflow remains available:

```bash
bucephalus build experiment.yaml --json
bucephalus check-package <package_dir> --json
bucephalus preflight <package_dir> --json
bucephalus run <package_dir> --smoke-test --materialize full --json
bucephalus run <package_dir> --materialize full --json
```

For hosted Cloud runs, use `buc` against the hosted API:

```bash
bucephalus login --resource <api-url>
buc build experiment.yaml
buc secrets put NAME --from-env NAME
buc doctor <package-digest> --secret-ref NAME=bucephalus://NAME
buc run <package-digest> --secret-ref NAME=bucephalus://NAME
```

`buc build experiment.yaml` uploads the YAML directory as an authoring context,
runs the bundled Core builder in Cloud, imports the produced sealed package, and
reports hosted Cloud readiness. Existing sealed packages can still be uploaded
with `buc build <package-dir>`. For nested experiments that reference shared
repo files, use `buc build path/to/experiment.yaml --context-root .` so the
Cloud build receives the intended authoring tree. Local generated and credential
material such as `.env`, `.npmrc`, `.ssh`, `.aws`, `node_modules`, and `target`
is excluded from YAML context uploads and rejected by the hosted API. Hosted
secrets are write-only:
`buc secrets put` stores the value in Cloud and returns a stable
`bucephalus://NAME` ref for doctor/run. See
[Hosted Cloud CLI](docs/user/cloud-cli.md).

## What Bucephalus Does

Bucephalus is a command-line workbench for repeatable agent experiments. It:

- seals experiment inputs so runs can be inspected and recovered,
- executes agents and graders locally or on configured backends,
- captures outputs, metrics, traces, and runtime state,
- supports pausing, resuming, recovering, and comparing runs.

Experiments are declared with a few resource categories:

| Term | Meaning |
| --- | --- |
| Stage | A process Bucephalus runs and wires into the trial, such as an agent or grader. |
| Ephemeral | A temporary owned dependency, such as a sidecar or mocked service. |
| External | A dependency Bucephalus does not own, such as a persistent database or third-party API. |

See the [Cookbook](cookbook/README.md) for copy-ready recipes and the
[Experiment YAML Reference](docs/user/experiment-yaml-reference.md) for the full
schema.

## Commands

Common operator commands:

| Command | Purpose |
| --- | --- |
| `bucephalus init` | Generate an experiment, cases, and adapter from an agent client workflow. |
| `bucephalus login` / `bucephalus logout` | Cache or remove Cloud OAuth credentials for first-party Cloud flows. |
| `bucephalus dev` | Build, check, preflight, and smoke-test a YAML experiment. |
| `bucephalus doctor` | Diagnose build/check/preflight readiness without launching a full run. |
| `bucephalus run` | Run YAML directly or execute a sealed package. |
| `bucephalus build` | Resolve YAML and create a sealed package. |
| `bucephalus check-package` | Run static checks against a sealed package. |
| `bucephalus preflight` | Validate dynamic launch readiness before execution. |
| `bucephalus build-run` | Advanced build-and-execute command for scripts. |
| `bucephalus pause` / `bucephalus resume` | Pause and resume in-flight work when supported by persisted runtime state. |
| `bucephalus recover` / `bucephalus continue` | Reconcile a stale run and continue the schedule. |
| `bucephalus kill` | Stop a running or paused experiment. |
| `bucephalus runs` | List known runs from the account database. |
| `bucephalus views` / `bucephalus query` | Inspect committed run facts and analysis views, served directly from the account SQLite database. |
| `bucephalus publish` | Create a local redacted support bundle for a run; review before sharing. |

Run `bucephalus <command> --help` for command-specific flags.

## Runtime Contract

Bucephalus injects `BUCEPHALUS_*` variables into agent and grader processes. The most important ones are:

| Variable | Meaning |
| --- | --- |
| `BUCEPHALUS_TRIAL_INPUT_PATH` | JSON input for the current trial. |
| `BUCEPHALUS_RESULT_PATH` | JSON result path the agent must write. |
| `BUCEPHALUS_RUN_ID` | Current run id. |
| `BUCEPHALUS_TRIAL_ID` | Current trial id. |
| `BUCEPHALUS_TRAJECTORY_PATH` | Optional JSONL event stream path when `traces.source: protocol` or explicit events enable command-agent traces. |

The in-container filesystem contract uses `/bucephalus/in`, `/bucephalus/out`, `/bucephalus/state`, `/bucephalus/workspace`, and `/bucephalus-events`.

## State And Results

Local artifacts and account facts live under the Bucephalus app storage directory by default:

```text
macOS:   ~/Library/Application Support/Bucephalus/
Linux:   $XDG_DATA_HOME/bucephalus or ~/.local/share/bucephalus/
Windows: %APPDATA%\Bucephalus\
```

The account SQLite database is `<storage>/bucephalus.sqlite`, runs are under
`<storage>/runs/`, packages are under `<storage>/builds/`, and bare agent
artifact names resolve from `<storage>/agents/`.

Override storage with `BUCEPHALUS_HOME=/absolute/path/to/dir`, or override only
the database with `BUCEPHALUS_DB=/absolute/path/to/bucephalus.sqlite`.

Use [Inspecting Results](docs/user/inspecting-results.md), [Metrics](docs/user/metrics.md), and [Package Checks](docs/user/package-checks.md) for the current analysis surface.

## Docs

Start with:

- [Cookbook](cookbook/README.md): copy-ready YAML starter templates.
- [Concepts](docs/user/concepts.md): experiment, run, trial, case, stage, ephemeral, and external.
- [Experiment YAML Reference](docs/user/experiment-yaml-reference.md): the canonical v1 authoring shape.
- [What You Provide](docs/user/what-you-provide.md): required inputs for successful experiments.
- [Agent Runtime Contract](docs/user/agent-runtime-contract.md): process env, paths, outputs, and event capture.
- [Graders And Mappers](docs/user/graders-and-mappers.md): declared grader transport and metric extraction.
- [Environment And Secrets](docs/user/env-and-secrets.md): launch-time env and secret binding.
- [Runtime Backends](docs/user/runtime-backends.md): Local Docker, Modal, and active runtime caps.
- [Troubleshooting](docs/user/troubleshooting.md): common build, preflight, run, and analysis failures.

## Tiers

Bucephalus runs the same experiment at three levels of investment. The ladder is
a single trade-off: **how much of the environment you hand us.** Hand us nothing
and run on your own machine; hand us a build recipe and we run it; hand us a fully
authored experiment and we provision and isolate everything. More setup buys you
stronger isolation, higher concurrency, and full recoverability.

| | Tier 1 — Local | Tier 2 — Managed | Tier 3 — Authored |
| --- | --- | --- | --- |
| **In a sentence** | Run your own agent on your own machine. | Hand us a build recipe; we build and run it on our compute. | A fully declared experiment you author end to end. |
| **You provide** | A command that launches your agent, plus a supported benchmark. | A setup recipe (Dockerfile / setup script) and a packageable agent. | A full experiment YAML: images, stages, strategies, mounts, secrets. |
| **Bucephalus creates** | A scratch workspace per case, guardrails, and result capture. | The built environment image, isolation, and scheduling. | Provisioned pools/VMs and full run orchestration. |
| **Dependencies** | Just the binary (`bucephalus setup`). No Docker. | A build recipe and managed compute. | Docker/OrbStack or Modal, plus YAML authoring. |
| **Isolation** | Low. macOS: scratch dir + soft guardrails — *not* a security boundary. Linux: OS-level namespaces via bubblewrap. | Containerized on managed compute. | Strongest — dedicated Linux VMs per trial. |
| **Concurrency** | Bounded by your local machine. | Scales on managed compute. | High; pooled and scheduled. |
| **Recoverability** | Per case; each result ships as it finishes. | Managed by the service. | Full pause / resume / recover from persisted state. |
| **Best for** | Quickly trying an agent against a benchmark. | Benchmarks that need a built environment but not full authoring. | Reproducible, isolated, large-scale evals (e.g. SWE-bench). |

Each shipped result is stamped with the isolation level it actually ran under
(`guarded`, `contained`, or `isolated`), so a local run is never mistaken for an
isolated one.

> **Tier 1 eligibility.** Local runs fit benchmarks whose inputs drop in as files
> and whose grading does not depend on a built environment. Benchmarks that need a
> setup script (Tier 2) or ship a prebuilt image as the environment (Tier 3, e.g.
> SWE-bench) cannot run locally — Bucephalus checks this up front and tells you
> which tier to graduate to.

## Repository Map

```text
Cargo.toml                         Cargo workspace root
schemas/                           JSON Schemas for package, run, and trial artifacts
rust/crates/lab-cli/                Command parsing and operator-facing UI
rust/crates/lab-runner/             Build, preflight, scheduling, execution, persistence
rust/crates/lab-core/               Shared runtime contract constants and helpers
rust/crates/lab-schemas/            Embedded schema loading
rust/crates/lab-provenance/         Content addressing and attestations
rust/crates/lab-analysis/           Optional SQL analysis views
cookbook/                           Copy-ready starter experiment workspaces
docs/user/                          Product-facing user documentation
docs/specs/                         Design history and implementation notes
tools/charts/                       Local chart gallery tooling
```

## License

MIT. See [LICENSE](LICENSE).
