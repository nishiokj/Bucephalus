# AgentLab Resource Usage Audit - 2026-05-03

## Scope

Audited host filesystem usage under `/Users/jevinnishioka/Desktop/Experiments/.lab`, the latest one-trial SWE-bench run, and the runner paths that copy, materialize, and persist data.

Primary run inspected:

`/Users/jevinnishioka/Desktop/Experiments/.lab/runs/run_20260503_001003_982559_000001`

## Disk Snapshot

Top-level `.lab` usage:

```text
12G  .lab
4.5G .lab/dataset_packs
4.1G .lab/runs
2.4G .lab/builds
331M .lab/swebench-harness-py312-venv
152M .lab/agents
62M  .lab/upstream
45M  .lab/git_checkouts
39M  .lab/debug
8.9M .lab/swebench-harness-venv
```

Docker storage is separate from `.lab`. SWE-bench task images are in Docker's image store, not copied into `.lab` by the current SWE-bench task-image flow. Local Docker still has multiple large benchmark images, including several SWE-bench images in the 2-3GB range and older `bench-v0-workspace-task*` images around 1.03GB each.

## Latest SWE-bench Run Layout

Run size:

```text
141M run_20260503_001003_982559_000001
140M agent_builds
548K run.sqlite
128K trials
76K  artifacts
60K  runtime_assets
4K   tasks
```

The current one-trial SWE-bench evidence is small. The large part is `agent_builds`.

Largest files in that run:

```text
99M  agent_builds/.agentlab_artifact_cache/.../bin/bun
39M  agent_builds/000_rex-desktop-agent-20260502-official.linux-x64.tar.gz
2.8M agent_builds/.agentlab_artifact_cache/.../bin/rex
60K  trials/trial_1/out/trajectory.jsonl
548K run.sqlite
```

SQLite content for this one trial:

```text
event_rows: 146
metric_rows: 14
trial_rows: 1
variant_snapshot_rows: 3
```

SQLite page usage was dominated by `event_rows` at roughly 238KB, so event ingestion is not the current disk problem for this small run.

## Findings

### 1. Agent artifacts are duplicated per build and per run

The same Rex tarball digest appears in `.lab/agents`, multiple `.lab/builds/*/agent_builds`, and multiple `.lab/runs/*/agent_builds`.

The unpacked payload is duplicated too. `bin/bun` alone is 99M and appears under repeated per-build/per-run `.agentlab_artifact_cache` directories.

Relevant code:

- Build packaging stages `runtime.agent_runtime.artifact` into `agent_builds`: `rust/crates/lab-runner/src/package/staging.rs:683`
- Run startup copies package subdirs including `agent_builds` into every run: `rust/crates/lab-runner/src/experiment/runner.rs:1013`
- Artifact unpack cache is local to the artifact file's parent: `rust/crates/lab-runner/src/trial/execution.rs:841`

Impact:

For Rex, each build/run pays about 39M for the archive plus about 101M for unpacked cache. Reusing one agent across many runs can cost GBs of repeated host data without adding benchmark value.

### 2. Build packages are full copies, not manifests over shared content

`build_experiment_package` writes a self-contained sealed package. It copies tasks, runtime assets, benchmark files, and agent artifacts into `.lab/builds/<name>`.

Relevant code:

- `rust/crates/lab-runner/src/package/compile.rs:260` creates package directories.
- `rust/crates/lab-runner/src/package/compile.rs:289` rewrites runtime paths into package-local copies.
- Package checksums are computed over copied package files.

This is useful for portable export, but expensive for local iteration.

### 3. `dataset_packs` contains 40 near-identical full repo packs

`.lab/dataset_packs/sha256` contains 40 directories, each about 114M, totaling 4.5G. File inspection shows repeated repository content across task-specific pack directories.

Relevant code:

- `base_image_bundle` task rows copy each task bundle into the package: `rust/crates/lab-runner/src/package/compile.rs:105`
- The source bundle is copied directly: `rust/crates/lab-runner/src/package/compile.rs:129`

This is the main historical pattern that can explode benchmark storage: per-task full workspace bundles rather than shared base plus task delta.

### 4. Older runs retained full workspaces and dependencies

Older run example:

```text
550M run_20260210_052404
275M trials/trial_1
275M trials/trial_2
```

Large repeated files were under `trials/*/workspace/.venve`, including `pyarrow` dylibs and duplicated Jupyter assets.

Relevant code:

- `MaterializationMode::Full` copies `workspace`, `tmp`, `state`, `in`, and `out`: `rust/crates/lab-runner/src/trial/layout.rs:41`
- `OutputsOnly` removes `workspace`, `state`, `tmp`, and `artifacts`: `rust/crates/lab-runner/src/trial/layout.rs:57`
- `MetadataOnly` and `None` remove more: `rust/crates/lab-runner/src/trial/layout.rs:78`

The latest real SWE-bench run did not retain the workspace; `trials` was only 128K. Current behavior is leaner, but `Full` materialization still needs budget guardrails.

### 5. Artifact store dedupes only within a run

The run artifact store writes `run_dir/artifacts/sha256/<digest>/blob` and avoids rewriting an existing blob.

Relevant code:

- `rust/crates/lab-core/src/lib.rs:136`

This is good intra-run behavior, but does not dedupe across runs or builds.

## Current Persistence Behavior

For each trial, the runner persists:

- `trial_output` into the run artifact store.
- stdout/stderr logs into the run artifact store if present.
- hook events into the run artifact store if `events.persist: true`.
- structured event rows into SQLite if `events.ingest: true`.
- trial rows, metric rows, variant snapshots, evidence rows, and benchmark conclusion rows into SQLite.
- materialized trial files according to `MaterializationMode`.

Relevant code:

- Artifact refs: `rust/crates/lab-runner/src/trial/schedule.rs:391`
- Hook event artifact: `rust/crates/lab-runner/src/trial/schedule.rs:408`
- Event row ingestion: `rust/crates/lab-runner/src/trial/schedule.rs:633`
- Materialization: `rust/crates/lab-runner/src/trial/schedule.rs:734`

## Recommended Fixes

### P0: Stop copying/unpacking agent artifacts into every run

Introduce a workspace-level content-addressed runtime artifact store:

```text
.lab/cas/artifacts/sha256/<digest>/blob
.lab/cas/artifact_mounts/sha256_<digest>/
```

Build packages should store a manifest reference plus digest for local runs. Portable export can still materialize full copies when explicitly requested.

Expected effect for Rex:

- Current local iteration: about 140M per build/run.
- Desired local iteration: one 39M archive and one 101M unpacked cache per unique artifact digest across the workspace.

### P0: Split local packages from portable packages

Separate "local executable package" from "portable sealed package".

Proposed modes:

- `local_ref`: records digest and source/CAS ref; no heavy copy.
- `portable`: copies all required bytes and can move machines.

This keeps reproducibility without making every local iteration pay export costs.

### P1: Store dataset packs as base plus delta

For `base_image_bundle` and similar local bundle modes:

- Hash files individually into CAS.
- Store a manifest/tree object per task.
- Reuse shared files across tasks.
- Materialize tar/zstd exports only for portable packages.

The current 40 x 114M pack pattern should collapse substantially because most task packs share the same repo content.

### P1: Add first-class resource usage reporting

At build and run completion, write `resource_usage.json` and ingest summary metrics:

- package bytes by class: `agent_artifacts`, `runtime_assets`, `tasks`, `files`
- run bytes by class: `agent_builds`, `trials`, `artifacts`, `sqlite`, `runtime_assets`
- trial bytes by class: `out`, `workspace`, `tmp`, `state`, logs, hook events
- Docker image refs/digests used, with bytes clearly marked as Docker-owned storage
- warning thresholds for repeated artifact copies and retained workspaces

This makes resource usage part of experiment value, alongside grade and latency.

### P1: Keep lean materialization as the default

The latest SWE-bench run behavior is sane: output evidence and SQLite are small, workspace is not retained. Keep `outputs_only` or equivalent as the normal path. Require explicit opt-in for `full` materialization and surface a warning before large runs.

### P2: Add cleanup/GC commands

Add first-class cleanup operations:

- prune old runs by age or keep-last-N
- prune unreferenced build packages
- prune CAS entries with no live references
- report Docker images/containers separately, and delete them only by explicit user confirmation

## Open Questions

- Should `build` default to local references unless `--portable` is passed, or should portable sealed packages remain the default with a new `build-local` command?
- Should event JSONL default to `persist: true`, or should the default become `ingest: true, persist: false` once event rows contain enough detail?
- Should task images remain external immutable Docker refs only, or should there ever be a portable image export mode?
