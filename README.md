# Bucephalus

A command-line workbench for building, running, recovering, and analyzing agent experiments. 

Experiments are controlled executions where we aim to extract measurable outcomes for the purpose of understanding and fueling decisions. Experiments can be but not limited to:
- AB testing two different models in the same harness against the same benchmark
- Sandboxing agents with your product + documentation and measuring the frequency of successful onboarding / bootstrapping
- Regression testing your agent system on a new model release

[User Docs](docs/user/index.md) · [Cookbook](cookbook/README.md) · [Concepts](docs/user/concepts.md) · [YAML Reference](docs/user/experiment-yaml-reference.md) · [Distribution](docs/distribution.md)

**Built with:** Rust · Tokio · Docker · SQLite

---

## What Bucephalus Does

Bucephalus provides an interface for designing, running and observing experiments. You author experiments by declaring your Stages*, Ephemerals* and Externals* in the YAML as well as policy, metrics, and backend targets for storage and compute. 

## *Stages, Ephemerals and Externals* 
The intuition for these primitives lies in the categorization of resources as you declare them in the YAML. It follows a simple flow: Is the lifecycle of this resource NOT owned within the experiment itself? It's an external. Think a persistent database or third party API, we do not materialize these nor take them down. Is Bucephalus responsible for transporting data into and out of this? It's a stage. Think your agent application, or a grader script. The agent app needs to be passed a case in order to handle it, and the grader script needs the result of the agent application in order to run. If for some reason you agent application called a grader script running on a 3rd party server, the grader would not be a stage. Finally, if the lifecycle is owned by Bucephalus, but the transport is NOT, it is an Ephemeral. Think an MCP server, sidecar or mocked temporary database.

A real benchmark recipe looks like this. The complete runnable workspace is
[cookbook/swebench-lite-codex](cookbook/swebench-lite-codex/README.md).

```yaml
experiment:
  id: cookbook_swebench_lite_codex
  name: Cookbook SWE-bench Lite Codex
  description: Real SWE-bench Lite task with Codex CLI headless execution and real pytest grading.
  tags: [cookbook, swebench-lite, codex, real-grader]

runtime:
  compute: { backend: local-docker }
  storage: { backend: local-fs, config: { root: .lab/runs/ } }
  traces: { backend: local-stdout }
  secrets:
    - { name: OPENAI_API_KEY, from: env }
  network:
    task_sandbox: full
    agent: full

externals:
  apis: [api.openai.com]
  credentials: [OPENAI_API_KEY]

matrix:
  variants:
    - id: codex_cli
      baseline: true
      config:
        model: gpt-5-codex
  cases:
    source: file
    path: cases.jsonl
  repeats: 1
  seeds: [1]

scheduling:
  max_concurrency: 1
  shuffle_tasks: false
  random_seed: 1
  comparison: none

ephemerals: {}

stages:
  case:
    interface: writable_workspace
    workspace:
      source: container_image
      image: { from: case_row }
      workdir: { from: case_row }
  agent:
    mount:
      source: .
      mount:
        path: /opt/agent
        read_only: true
    command: ["python3", "/opt/agent/agent/run_codex.py", "--model", "$model"]
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
      CODEX_TIMEOUT_SECONDS: "900"
    integration_level: cli_basic
    outputs:
      result:
        capture:
          type: file
          path: /bucephalus/out/result.json
          format: json
          required: true
      candidate_patch:
        capture:
          type: workspace_diff
          format: unified_diff
  execution:
    agent_site: task_runtime
  grader:
    strategy: in_task_runtime
    command:
      - python3
      - /opt/agent/grader/swebench_subset_grader.py
      - --instance-id
      - astropy__astropy-12907
      - --metadata-dir
      - /opt/agent/official_metadata
      - --patch-file
      - /tmp/candidate.patch
      - --repo
      - __BUCEPHALUS_TASK_WORKDIR__
      - --report-output
      - /bucephalus/out/swebench_report.json
    inputs:
      patch_file:
        source:
          output: agent.candidate_patch
          field: patch
        materialize:
          as: file
          path: /tmp/candidate.patch
        required: true
    outputs:
      report:
        capture:
          type: file
          path: /bucephalus/out/swebench_report.json
          format: json
          required: true

metrics:
  - id: resolved
    source:
      type: grader_output
      output: report
      pointer: /resolved
    direction: maximize
    primary: true
    required: true
  - id: fail_to_pass_passed
    source:
      type: grader_output
      output: report
      pointer: /fail_to_pass_passed
    direction: maximize
  - id: pass_to_pass_passed
    source:
      type: grader_output
      output: report
      pointer: /pass_to_pass_passed
    direction: maximize

policy:
  timeout_ms: 1200000
  sanitization_profile: perf_benchmark
  task_sandbox:
    resources:
      cpu_count: 4
      memory_mb: 8192
```

## Workflow

```bash
# 1. Build a sealed package from v1 experiment YAML.
bucephalus build experiment.yaml --out .lab/builds/my-experiment --json

# 2. Run static package hygiene checks.
bucephalus check-package .lab/builds/my-experiment --json

# 3. Check launch readiness: images, env bindings, resources, and grader reachability.
bucephalus preflight .lab/builds/my-experiment --json

# 4. Execute a real end-to-end smoke test for the package digest.
bucephalus run .lab/builds/my-experiment --smoke-test --materialize full --json

# 5. Run the full experiment.
bucephalus run .lab/builds/my-experiment --materialize full --json
```

`bucephalus build-run` combines build and run for cases where you already trust the experiment shape. For new experiments, the staged flow above is easier to debug.

Full runs are smoke-test gated. If a package digest has not passed a smoke test, interactive runs warn before proceeding; non-interactive runs must choose `--smoke-test` or `--run-dangerously`.

## Install

From this checkout:

```bash
cargo install --path .
```

That installs the CLI command:

```bash
bucephalus --help
```

For local SQL analysis commands (`views`, `views-live`, and `query`), build with the optional analysis engine:

```bash
cargo install --path . --features duckdb_engine
```

Container-backed trials require Docker or OrbStack for `local-docker` runs. Modal runs require Modal and S3-compatible sync configuration; see [Runtime Backends](docs/user/runtime-backends.md).

## Commands

Common operator commands:

| Command | Purpose |
| --- | --- |
| `bucephalus build` | Resolve YAML and create a sealed package. |
| `bucephalus check-package` | Run static checks against a sealed package. |
| `bucephalus preflight` | Validate dynamic launch readiness before execution. |
| `bucephalus run` | Execute a sealed package. |
| `bucephalus build-run` | Build from YAML and execute in one command. |
| `bucephalus pause` / `bucephalus resume` | Pause and resume in-flight work when supported by persisted runtime state. |
| `bucephalus recover` / `bucephalus continue` | Reconcile a stale run and continue the schedule. |
| `bucephalus kill` | Stop a running or paused experiment. |
| `bucephalus runs` | List known runs from the account database. |
| `bucephalus views` / `bucephalus query` | Inspect committed run facts and analysis views when built with `duckdb_engine`. |

Run `bucephalus <command> --help` for command-specific flags.

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

Run artifacts live under `.lab/` by default; `.lab` is the artifact directory,
not the CLI command. Account-level facts are stored in:

```text
$HOME/.bucephalus/bucephalus.sqlite
```

Override this with `BUCEPHALUS_DB=/absolute/path/to/bucephalus.sqlite` or `BUCEPHALUS_HOME=/absolute/path/to/dir`.

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

## Repository Map

```text
Cargo.toml                         Publishable Rust crate for the Bucephalus CLI
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
