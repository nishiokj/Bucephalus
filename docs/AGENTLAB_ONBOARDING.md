# AgentLab Onboarding

This repo is an experiment runner around an explicit `trial_runtime` contract.

## Starter File

- `experiment.yaml`

## Mental Model

For each task/variant/replication, runner:

1. materializes trial input and any declared task workspace,
2. executes the declared agent command,
3. ingests declared event streams while the trial runs,
4. reads the agent response JSON, and
5. commits durable run facts, metrics, events, and evidence to SQLite.

Your runtime program should:

1. read trial input from `AGENTLAB_TRIAL_INPUT_PATH`,
2. run autonomously,
3. optionally write JSONL events to a declared `trial_runtime.agent.events` path,
4. write any valid JSON response to `AGENTLAB_RESULT_PATH`.

## Runtime Contract

Use the current contract in `experiment.yaml`:

- `trial_runtime.task` declares the task interface and workspace
- `trial_runtime.agent.command` declares the process argv
- `trial_runtime.agent.outputs.result` captures `/agentlab/out/result.json`
- `trial_runtime.agent.events` optionally declares JSONL event captures
- `trial_runtime.execution.agent_site` selects where the agent runs
- `trial_runtime.grader` declares grading, or `strategy: none`
- `metrics` declares queryable scalar observations

Runner env vars include:

- `AGENTLAB_TRIAL_INPUT_PATH`
- `AGENTLAB_RESULT_PATH`
- `AGENTLAB_TRAJECTORY_PATH`
- `AGENTLAB_RUN_ID`
- `AGENTLAB_TRIAL_ID`
- `AGENTLAB_VARIANT_ID`
- `AGENTLAB_TASK_ID`
- `AGENTLAB_TIMEOUT_MS`

## Try It

```bash
# from repository root
cargo build --manifest-path rust/Cargo.toml -p lab-cli --release
rust/target/release/lab-cli preflight experiment.yaml
rust/target/release/lab-cli build-run experiment.yaml --json
```

After launch, inspect progress and live events with:

```bash
rust/target/release/lab-cli views-live <run_id> run_progress
rust/target/release/lab-cli views-live <run_id> events --limit 50
```
