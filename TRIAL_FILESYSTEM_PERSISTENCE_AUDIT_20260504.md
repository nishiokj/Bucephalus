# Trial Filesystem Persistence Audit - 2026-05-04

## Scope

Audited persisted trial/run outputs under `.lab/runs/*/trials`, run-level artifacts under `.lab/runs/*`, and the Rust runner write paths that produce them.

This audit builds on `RESOURCE_USAGE_AUDIT_20260503.md`, but classifies writes by persistence responsibility:

- SQLite: durable truth, operational state, records, indexes, manifests, audit facts.
- DuckDB: analytical access over SQLite plus artifact-derived tables/views.
- CAS/filesystem: large opaque bytes and human-exportable artifacts, always registered by SQLite metadata.
- Scratch filesystem: transient execution protocol between runner, agent, grader, and containers.

## Current Aggregate By Experiment

Snapshot from the local `.lab/runs` directory:

| Experiment | Runs | Total | Trials | Agent Builds | Run Artifacts |
| --- | ---: | ---: | ---: | ---: | ---: |
| `codex_spark_vs_glm5_swebench_real_agent_capture` | 12 | 1675.2M | 1.2M | 1670.8M | 0.2M |
| `exp_local` | 13 | 1711.8M | 1710.6M | 0.0M | 0.0M |
| `codex_spark_vs_glm5_swebench_real_agent_official` | 2 | 280.5M | 0.2M | 279.6M | 0.0M |
| `bench_v0_noop_real_smoke` | 2 | 203.7M | 0.1M | 0.0M | 0.0M |
| `codex_spark_swebench_event_sink_single_trial` | 1 | 140.7M | 0.1M | 139.8M | 0.1M |
| `codex_spark_vs_glm5_swebench` | 2 | 76.4M | 0.0M | 76.0M | 0.0M |
| `exp_2026_02_04_local` | 5 | 5.8M | 5.7M | 0.0M | 0.0M |
| `exp_smoke` | 2 | 5.7M | 5.6M | 0.0M | 0.0M |
| `swebench_mini_harness_eval` | 1 | 4.3M | 0.0M | 4.1M | 0.0M |
| `terminal_bench2_harbor_smoke` | 8 | 1.4M | 0.6M | 0.0M | 0.2M |
| `swebench_lite_astropy_12907_oracle_real_grading` | 3 | 1.1M | 0.2M | 0.0M | 0.0M |
| `swebench_lite_curated_actual_agent_runtime` | 1 | 0.6M | 0.5M | 0.0M | 0.1M |

Two distinct problems show up:

1. Current SWE-bench runs mostly keep tiny trial folders, but duplicate `agent_builds` per run.
2. Older/local experiments retained full `trials/*/workspace` trees, including dependency environments, which made trials the dominant storage plane.

## Current Trial Write Surfaces

### Scratch/Protocol Files

These are necessary during execution because the agent, grader, and runner communicate through mounted directories:

- `trials/<trial>/in/trial_input.json`
- `trials/<trial>/in/grader_input.json`
- `trials/<trial>/out/result.json`
- `trials/<trial>/out/mapped_grader_output.json`
- `trials/<trial>/out/trajectory.jsonl`
- `trials/<trial>/workspace/**`
- `trials/<trial>/tmp/**`
- `trials/<trial>/state/**`

Classification:

- Should exist only in scratch while the trial is running.
- Should not be the source of durable truth after finalization.
- Should only be retained by explicit materialization mode for debugging/export.

Relevant code:

- `rust/crates/lab-runner/src/trial/layout.rs`
- `rust/crates/lab-runner/src/trial/schedule.rs`
- `rust/crates/lab-runner/src/trial/prepare.rs`

### Durable Run/Trial Facts

Currently written as JSON files and/or SQLite rows:

- `trial_state.json`
- `trial_metadata.json`
- `trial_runtime_state.json`
- `state_inventory.json`
- `benchmark_preflight.json`
- `resolved_experiment.json`
- `resolved_schedule.json`
- `resolved_variants.json`
- `manifest.json`
- `attestation.json`
- `staging_manifest.json`
- `run.sqlite`

Classification:

- Should be stored in SQLite as durable truth.
- Small JSON mirrors can exist as export/debug views, but should be generated from SQLite or explicitly tagged as mirrors.
- `trial_state.json`, `trial_metadata.json`, `trial_runtime_state.json`, `state_inventory.json`, and `benchmark_preflight.json` should not be authoritative files.

Current good direction:

- `run.sqlite` already stores `runtime_kv`, run manifests, slot commits, pending completions, trial rows, metrics, events, evidence rows, chain state, benchmark conclusions, attempt objects, lineage, and runtime ops.
- `run_control` and `run_session_state` have already moved into SQLite runtime KV.

Remaining debt:

- Some trial-local state files are still written directly as first-class files.
- Some compatibility/export mirrors still duplicate facts that SQLite already owns.

Relevant code:

- `rust/crates/lab-runner/src/persistence/schema_v2.sql`
- `rust/crates/lab-runner/src/persistence/store.rs`
- `rust/crates/lab-runner/src/experiment/runner.rs`
- `rust/crates/lab-runner/src/trial/state.rs`
- `rust/crates/lab-runner/src/trial/preflight.rs`
- `rust/crates/lab-runner/src/trial/layout.rs`

### Large/Opaque Artifacts

Current examples:

- `agent_builds/*.tar.gz`
- `agent_builds/.agentlab_artifact_cache/**`
- `artifacts/sha256/<digest>/blob`
- `runtime_assets/**`
- dataset packs under `.lab/dataset_packs`
- retained `workspace/**` trees
- stdout/stderr logs and hook event JSONL

Classification:

- Large opaque bytes should live in a content-addressed store.
- SQLite should register each artifact with digest, size, media/kind, producer, run/trial/task ids, role, created time, and retention policy.
- Run-local `artifacts/sha256/<digest>/blob` is good as a shape, but too local. It dedupes only inside one run.
- Agent archives and unpacked agent bundles should be CAS-backed workspace-wide, not copied/unpacked into every run.
- Runtime assets and dataset packs should be CAS-backed or tree-manifest-backed when reused.

Relevant code:

- `rust/crates/lab-core/src/lib.rs` has the current run-local `ArtifactStore`.
- `rust/crates/lab-runner/src/package/staging.rs` stages agent artifacts into package `agent_builds`.
- `rust/crates/lab-runner/src/experiment/runner.rs` copies package subdirs into every run.
- `rust/crates/lab-runner/src/trial/execution.rs` unpacks artifacts into a cache local to the artifact parent.

## What Should Be Written Where

### SQLite

Move or keep these as durable SQLite records:

- run manifest and resolved experiment identity
- resolved schedule and resolved variants
- run control/session state
- trial lifecycle state and status transitions
- trial metadata and runtime state
- benchmark preflight facts
- trial rows, metrics, event rows, evidence rows
- grader conclusions and mapped outcomes
- attempt object registry
- artifact registry and artifact references
- resource usage summaries
- CAS reference counts or live-reference roots

Suggested new/expanded tables:

- `artifact_objects`: digest, size, kind, media_type, storage_uri, created_at, producer.
- `artifact_links`: run_id, trial_id, schedule_idx, role, artifact_digest, metadata_json.
- `resource_usage_rows`: run/trial/package byte summaries by class.
- Optional `state_transition_rows`: append-only trial/run state changes instead of mutable JSON snapshots.

### DuckDB

Use DuckDB for query and reporting:

- attach/read `run.sqlite`
- join SQLite rows with artifact-derived Parquet/JSONL tables
- generate analysis tables and reports
- optionally materialize derived analytical views

DuckDB should not own operational truth. Any DuckDB table that cannot be regenerated should be considered suspect until promoted into SQLite or CAS plus SQLite metadata.

### CAS / Filesystem

Keep these as filesystem/CAS bytes:

- agent runtime archives
- unpacked runtime bundles by digest
- stdout/stderr logs
- hook event raw JSONL if retention is requested
- full trial output envelopes if large
- grader raw output if large
- workspace snapshots, diffs, patches
- runtime assets and dataset pack trees
- exported reports

Target layout:

```text
.lab/cas/blobs/sha256/<hex>/blob
.lab/cas/trees/sha256/<hex>/manifest.json
.lab/cas/unpacked/sha256/<hex>/
.lab/cas/tmp/
```

SQLite should be the only normal way to discover these artifacts. Paths are implementation details behind refs such as `artifact://sha256/<hex>` or `tree://sha256/<hex>`.

### Do Not Persist

Do not retain these by default:

- `trials/*/workspace/**`
- `trials/*/tmp/**`
- `trials/*/state/**`
- copied `in/**` inputs after they are recorded as SQLite/CAS refs
- duplicate `out/**` files after they are recorded as SQLite/CAS refs
- `.pytest_cache`, `.venv`, `.venve`, dependency caches, build caches inside workspaces
- per-run copies of agent archives when a CAS ref is available
- per-run unpacked agent bundle caches

Allow explicit debug/export retention with a clear materialization mode and resource warning.

## Per-Experiment Assessment

### `codex_spark_vs_glm5_swebench_real_agent_capture`

Current dominant write: duplicated `agent_builds` across 12 runs, about 1.67G total.

Policy:

- Agent archive and unpacked bundle should be workspace CAS objects.
- Trial outputs, event rows, and grader results are small and should remain SQLite plus CAS refs.
- Keep filesystem materialization to `outputs_only` or lower by default.

### `codex_spark_swebench_event_sink_single_trial`

Current shape is mostly healthy: `trials` is small, `run.sqlite` is active, and event rows are ingested. The 140M run size is almost entirely agent archive plus unpack cache.

Policy:

- Keep event ingestion into SQLite.
- Persist raw hook JSONL only when `events.persist: true`; otherwise treat it as scratch.
- Move agent artifact and unpack cache to workspace CAS.

### `codex_spark_vs_glm5_swebench_real_agent_official`

Same as the capture experiment: agent build duplication dominates.

Policy:

- Same CAS runtime-artifact plan.
- Runtime assets should be CAS/tree references when unchanged across builds.

### `exp_local` / older local runs

Current dominant write: retained full workspaces, including dependency environments. Three runs alone account for roughly 1.7G of trial storage.

Policy:

- Default materialization should not be `full`.
- Full workspace retention should be explicit, warned, and preferably converted into a CAS tree snapshot rather than a copied directory.
- Dependency directories and caches should be excluded or represented as external environment refs.

### `bench_v0_noop_real_smoke`

One run has about 102M with almost no trial content; likely package/runtime copy behavior rather than useful result data.

Policy:

- Treat benchmark/runtime assets as CAS references.
- Keep trial durable facts in SQLite.
- Do not retain scratch folders by default.

### `terminal_bench2_harbor_smoke`

Small runs. Some raw artifacts exist but do not dominate.

Policy:

- Preserve file artifacts only as CAS objects linked from SQLite.
- Continue avoiding full workspace retention.

### `swebench_lite_astropy_12907_oracle_real_grading` and `swebench_lite_curated_actual_agent_runtime`

Small trial footprint. Current behavior is close to desired.

Policy:

- Keep SQLite as truth for results and grading facts.
- Keep raw grader artifacts/logs in CAS only when useful for audit.

## Recommended Changes

## Concrete Writer Map

This section traces where the runner actually chooses to write files or database rows. The important distinction is whether a callsite writes through a durable sink, a content-addressed artifact store, or directly to the run/trial filesystem.

### Core Write Primitives

| Writer | Location | What It Does | Current Plane | Disposition |
| --- | --- | --- | --- | --- |
| `atomic_write_bytes` | `rust/crates/lab-runner/src/config.rs:18` | writes temp file, fsyncs, renames | plain filesystem | Keep as low-level primitive, but restrict durable callers |
| `atomic_write_json_pretty` | `rust/crates/lab-runner/src/config.rs:41` | validates schema-ish contract, pretty JSON file write | plain filesystem | Too broad; durable state callers should move to SQLite or generated mirrors |
| `ArtifactStore::put_bytes` | `rust/crates/lab-core/src/lib.rs:136` | writes `root/sha256/<hex>/blob`, returns `artifact://sha256/<hex>` | run-local CAS | Promote to workspace CAS |
| `ArtifactStore::put_file` | `rust/crates/lab-core/src/lib.rs:148` | reads whole file, delegates to `put_bytes` | run-local CAS | Promote with `put_file` metadata registration |
| `RunSink` | `rust/crates/lab-runner/src/persistence/journal.rs:20` | structured run row abstraction | SQLite or memory buffer | Keep, but expand to artifact/state rows |
| `SqliteRunJournal` | `rust/crates/lab-runner/src/persistence/journal.rs:67` | `RunSink` implementation backed by SQLite | SQLite | Keep |
| `BufferedRunSink` | `rust/crates/lab-runner/src/persistence/journal.rs:29` | per-worker temporary sink | memory | Keep for parallel workers |
| `append_jsonl` | `rust/crates/lab-runner/src/persistence/journal.rs:225` | maps evidence/chain/conclusion JSONL paths to SQLite when possible | SQLite, except worker payload files | Keep, but rename/comment because it rarely appends files now |
| `append_jsonl_file` | `rust/crates/lab-runner/src/persistence/journal.rs:212` | actual file JSONL append fallback | plain filesystem | Only acceptable for worker payload scratch |
| `copy_path_into_package` | `rust/crates/lab-runner/src/package/compile.rs:41` | recursive copy for package/run materialization | plain filesystem | Split local-ref vs portable package behavior |

`rust/crates/lab-runner/src/persistence/run_sink.rs` previously duplicated much of `persistence/journal.rs` but was not exported by `persistence/mod.rs`; it has been removed.

### SQLite Sinks

`SqliteRunStore::open` creates/bootstraps `run.sqlite` at `rust/crates/lab-runner/src/persistence/store.rs:137`.

Current durable SQLite writes:

| Data | Writer | SQLite Table |
| --- | --- | --- |
| runtime key/value state | `put_runtime_json` at `store.rs:161` | `runtime_kv` |
| run manifest record | `put_run_manifest` at `store.rs:187` | `run_manifests` |
| slot commit intent/commit records | `upsert_slot_commit_record` at `store.rs:204` | `slot_commit_records` |
| pending completions | `replace_pending_trial_completions` at `store.rs:260` | `pending_trial_completions` |
| attempt object refs | `upsert_attempt_object` at `store.rs:327` | `attempt_objects` |
| trial rows | `upsert_trial_row` at `store.rs:573` | `trial_rows` |
| metric rows | `upsert_metric_row` at `store.rs:626` | `metric_rows` |
| event rows | `upsert_event_row` at `store.rs:667` | `event_rows` |
| variant snapshots | `upsert_variant_snapshot_row` at `store.rs:706` | `variant_snapshot_rows` |
| evidence/chain/conclusion generic rows | `upsert_json_row` at `store.rs:747` | `evidence_rows`, `chain_state_rows`, `benchmark_conclusion_rows` |
| artifact refs from evidence | `upsert_attempt_objects_from_evidence_row` at `store.rs:525` | `attempt_objects` |
| lineage from chain state | `upsert_lineage_from_chain_state_row` at `store.rs:434` | `lineage_versions`, `lineage_heads` |

This is the good path. The problem is not that SQLite is absent; it is that several durable facts still bypass it.

### Run Startup Writes

Fresh run setup in `run_experiment_with_behavior`:

| Output | Location | Writer | Current Plane | Disposition |
| --- | --- | --- | --- | --- |
| `run.sqlite/runtime_kv[run_control_v2]` | `experiment/runner.rs:1008` | `write_run_control_v2` | SQLite | Keep |
| `run.sqlite/runtime_kv[run_session_state_v1]` | `experiment/runner.rs:1009` | `write_run_session_state` | SQLite | Keep |
| `tasks/`, `files/`, `agent_builds/`, `runtime_assets/` copied into run | `experiment/runner.rs:1013` | `copy_path_into_package` | plain filesystem copies | Replace with CAS/local refs for local runs |
| `staging_manifest.json` copied into run | `experiment/runner.rs:1031` | `copy_path_into_package` | plain filesystem | SQLite mirror or small generated manifest is fine |
| `resolved_experiment.json` | `experiment/runner.rs:1043` | `atomic_write_json_pretty` | plain filesystem | Store in SQLite as truth; file can be mirror/export |
| `resolved_experiment.digest` | `experiment/runner.rs:1045` | `atomic_write_bytes` | plain filesystem | Store digest in SQLite/runtime KV; optional mirror |
| `manifest.json` | `experiment/runner.rs:1051` | `atomic_write_json_pretty` | plain filesystem | Redundant with `run_manifests`; make mirror |
| `resolved_variants.json` | `experiment/runner.rs:1063`, `config.rs:597` | `atomic_write_json_pretty` | plain filesystem | Store in SQLite/runtime KV or variant table; mirror optional |
| `resolved_schedule.json` | `experiment/runner.rs:1180`, `config.rs:617` | `atomic_write_json_pretty` | plain filesystem | Already schedule progress in SQLite; make this SQLite/mirror |
| `run.sqlite/run_manifests` | `experiment/runner.rs:1160` | `SqliteRunJournal::write_run_manifest` | SQLite | Keep |
| `runtime_kv[schedule_progress_v2]` | `experiment/runner.rs:1193`, `experiment/state.rs:198` | `write_schedule_progress` | SQLite | Keep |

Continue-run setup has the same pattern for `resolved_schedule.json`, `run_control_v2`, and `run_manifests` at `experiment/runner.rs:166`, `170`, and `198`.

### Trial Scratch Setup

Trial directories are split between durable trial dir and scratch dir:

- `TrialPaths::new` creates a scratch root under `.scratch/<trial>_<pid>_<seq>` at `trial/prepare.rs:58`.
- Runtime host paths are under that scratch root via `runner_runtime_host_paths` at `lab-core/src/lib.rs:52`.
- Scratch cleanup happens through `TrialPaths::cleanup_scratch` at `trial/prepare.rs:109` and `Drop` at `trial/prepare.rs:114`.

Scratch writes:

| Output | Location | Writer | Current Plane | Disposition |
| --- | --- | --- | --- | --- |
| scratch `in/`, `workspace/`, `state/`, `out/`, `tmp/` dirs | `trial/prepare.rs:75` | `ensure_dir` | scratch filesystem | Keep |
| optional seeded workspace copy | `trial/prepare.rs:81` | `copy_dir_filtered` | scratch filesystem | Keep only for protocol/debug; do not persist by default |
| scratch `in/trial_input.json` | `trial/prepare.rs:416` | `std::fs::write` | scratch filesystem | Keep as protocol file; durable ref already goes to CAS/SQLite |
| stale scratch output cleanup | `trial/prepare.rs:418` | remove file calls | scratch filesystem | Keep |
| `runtime/prepared_task_environment.json` in durable trial dir | `trial/prepare.rs:259` | `atomic_write_json_pretty` | plain filesystem | Move to SQLite/runtime state or generated mirror |

### Trial Direct Files

These are the durable-ish trial files still written directly:

| Output | Location | Writer | Current Plane | Disposition |
| --- | --- | --- | --- | --- |
| `trials/<trial>/trial_state.json` | `trial/state.rs:458` | `write_trial_state` | plain filesystem | Move to SQLite state transitions/current state; file mirror only |
| `trials/<trial>/trial_runtime_state.json` | `trial/state.rs:289` | `write_trial_attempt_state` | plain filesystem | Move to SQLite runtime ops/current attempt state |
| `trials/<trial>/trial_metadata.json` | `trial/schedule.rs:84` | `write_scheduled_trial_metadata` | plain filesystem | Store in SQLite; mirror optional |
| `trials/<trial>/state_inventory.json` | `trial/layout.rs:102` | `write_state_inventory` | plain filesystem | Store in SQLite, probably attempt metadata/state_inventory table |
| `trials/<trial>/benchmark_preflight.json` | `trial/preflight.rs:12` | `stage_benchmark_trial_preflight` | plain filesystem | Store in SQLite; mirror optional |
| `trials/<trial>/artifacts/benchmark_frozen_agent_input/trial_input.json` | `trial/preflight.rs:44` | `fs::copy` | plain filesystem duplicate | Replace with CAS ref to trial input |

### Trial Artifact and Row Finalization

Finalization in `trial/schedule.rs` is the best current boundary:

| Data | Location | Writer | Current Plane | Disposition |
| --- | --- | --- | --- | --- |
| trial input artifact ref | `trial/schedule.rs:246` | `ArtifactStore::put_bytes` | run-local CAS | Promote to workspace CAS |
| `attempt_objects[trial_input]` | `trial/schedule.rs:248` | `SqliteRunStore::upsert_attempt_object` | SQLite | Keep |
| trial output artifact ref | `trial/schedule.rs:391` | `ArtifactStore::put_bytes` | run-local CAS | Promote to workspace CAS |
| harness stdout/stderr artifact refs | `trial/schedule.rs:395` | `ArtifactStore::put_file` | run-local CAS | Promote to workspace CAS |
| hook events artifact ref | `trial/schedule.rs:408` | `ArtifactStore::put_file` gated by `events.persist` | run-local CAS | Keep policy, promote storage |
| evidence row | `trial/schedule.rs:423` and `476` | `append_jsonl` | SQLite except worker payload | Keep |
| chain state row | `trial/schedule.rs:492` and `507` | `append_jsonl` | SQLite except worker payload | Keep |
| parsed event rows | `trial/schedule.rs:633` | `load_event_rows` then `RunSink` | SQLite/memory | Keep |
| metric rows | `trial/schedule.rs:646` | `RunSink` | SQLite/memory | Keep |
| trial rows | `trial/schedule.rs:668` | `RunSink` | SQLite/memory | Keep |
| variant snapshots | `trial/schedule.rs:658` and `701` | `RunSink` | SQLite/memory | Keep |
| materialized trial layout | `trial/schedule.rs:734` | `materialize_trial_runtime_layout` | plain filesystem | Make lean by default; full only explicit |

### Parallel Worker Sink Flow

Parallel workers do not write rows directly to final SQLite while they are running:

- Worker payload dir: `runtime/worker_payload/<trial_id>` at `experiment/runner.rs:376`.
- Worker evidence/chain files: `experiment/runner.rs:385`.
- Worker sink: `BufferedRunSink` at `experiment/runner.rs:392`.
- Worker artifacts still use `ArtifactStore::new(context.run_dir.join("artifacts"))` at `experiment/runner.rs:393`.
- Main thread loads worker payload JSONL at `experiment/runner.rs:454`.
- Main deterministic committer writes final rows at `experiment/commit.rs:312`.

This is conceptually good: workers buffer facts, deterministic commit writes SQLite in schedule order. But it means `append_jsonl` has a special fallback for `runtime/worker_payload` (`persistence/rows.rs:143`), where it really does append JSONL files rather than ingesting SQLite.

### Deterministic Commit Writes

The deterministic committer is the authoritative row commit point:

| Data | Location | Writer | Current Plane | Disposition |
| --- | --- | --- | --- | --- |
| slot commit intent | `experiment/commit.rs:346` | `append_slot_commit_record` | SQLite | Keep |
| evidence rows | `experiment/commit.rs:366` and `374` | `append_jsonl` | SQLite | Keep |
| chain state rows | `experiment/commit.rs:376` and `384` | `append_jsonl` | SQLite | Keep |
| benchmark conclusion rows | `experiment/commit.rs:386` and `394` | `append_jsonl` | SQLite | Keep |
| trial rows | `experiment/commit.rs:396` and `403` | `RunSink` | SQLite | Keep |
| metric/event/snapshot rows | `experiment/commit.rs:405` through `425` | `RunSink` | SQLite | Keep |
| slot commit completion | `experiment/commit.rs:427` | `append_slot_commit_record` | SQLite | Keep |
| schedule progress | `experiment/commit.rs:466` and `479` | `write_schedule_progress` | SQLite | Keep |
| trial attempt phase reconciliation | `experiment/commit.rs:480` | `reconcile_trial_attempt_as_committed` | plain `trial_runtime_state.json` | Move to SQLite |

### Logs and Grader Protocol

| Output | Location | Writer | Current Plane | Disposition |
| --- | --- | --- | --- | --- |
| `harness_stdout.log`, `harness_stderr.log` | `trial/execution.rs:206` | Docker stream to files | plain filesystem then CAS | Keep as scratch/log files, register CAS refs |
| `grader_stdout.log`, `grader_stderr.log` | `trial/execution.rs:380` and phase records | plain filesystem then indirectly state JSON | Keep logs as CAS; state refs in SQLite |
| `mapper_stdout.log`, `mapper_stderr.log` | `trial/execution.rs:441` | Docker stream to files | plain filesystem | CAS/register if retained |
| `in/grader_input.json` | `trial/grade.rs:562` | `atomic_write_json_pretty` to scratch input host | scratch protocol | Keep as scratch; durable facts should be SQLite/CAS |
| grader aux diff/patch files | `trial/grade.rs:460` | `copy_file_if_exists` | scratch protocol | Keep as scratch; snapshot/diff artifacts should be CAS refs |

### Materialization Policy

`materialize_trial_runtime_layout` at `trial/layout.rs:35` is the central file dump switch:

| Mode | Writes/keeps |
| --- | --- |
| `Full` | copies scratch `in`, `out`, `state`, `workspace`, `tmp`; also copies `trial_input.json`, `harness_manifest.json`, and canonical `result.json` |
| `OutputsOnly` | copies scratch `out`, `harness_manifest.json`, canonical `result.json`; removes durable `workspace`, `state`, `tmp`, `artifacts` dirs |
| `MetadataOnly` | removes `workspace`, `state`, `tmp`, `artifacts`, `out`, `trial_input.json`, `result.json`, `harness_manifest.json`, `trace_manifest.json` |
| `None` | same as `MetadataOnly`, plus removes `state_inventory.json` |

The risky part was default selection:

- `experiment/state.rs:118` defaulted missing materialization to `Full`.
- `experiment/state.rs:129` persisted missing materialization as `Full`.
- `experiment/runner.rs:167` and `experiment/runner.rs:1001` consumed fallback `Full` values.

These now default to `OutputsOnly`. The duplicate inactive `rust/crates/lab-runner/src/run/mod.rs` has also been removed.

This handles the first behavior change for sparse filesystem usage.

### Package and Runtime Asset Copying

Build/package phase:

| Output | Location | Writer | Current Plane | Disposition |
| --- | --- | --- | --- | --- |
| package `agent_builds/` directory | `package/compile.rs:259` | ensure dir | plain filesystem | Replace for local-ref packages |
| package task bundle copies | `package/compile.rs:105` and `131` | `copy_path_into_package` | plain filesystem | CAS/tree refs for local, copy only portable |
| package runtime artifact copies | `package/staging.rs:683` | `stage_source_into_package` | plain filesystem | CAS ref for local, copy only portable |
| package runtime asset copies | `package/staging.rs:161` and `182` | `copy_path_into_package` | plain filesystem | CAS/tree refs for local, copy only portable |
| package manifests/checksums | `package/compile.rs:371`, `401`, `407`, `421` | `atomic_write_json_pretty` | plain filesystem | Keep for portable sealed packages |

Run phase duplicates package content:

- `experiment/runner.rs:1013` copies `tasks`, `files`, `agent_builds`, and `runtime_assets` from package into every run.
- This is responsible for repeated per-run agent archive copies.

Runtime unpack duplicates agent payloads:

- `trial/execution.rs:841` hashes the artifact file.
- `trial/execution.rs:843` chooses cache root as `artifact_path.parent()/.agentlab_artifact_cache`.
- `trial/execution.rs:876` untars into that local cache.
- `trial/execution.rs:902` writes `.agentlab_ready`.

This is why the same agent archive and unpacked bundle repeat across builds/runs. The cache root must become workspace-CAS/unpacked-by-digest, not artifact-parent-local.

### P0: Make lean materialization the default

Current code falls back to `MaterializationMode::Full` in run/session normalization and runner setup.

Change default to `outputs_only` or `metadata_only`; require explicit `--materialize full` for retained workspaces.

Suggested default:

- `outputs_only` for local developer runs.
- `metadata_only` for large benchmark sweeps once artifact refs are complete.

### P0: Promote run-local artifact store to workspace CAS

Replace run-local-only artifact roots with a workspace CAS:

- Store bytes once per digest under `.lab/cas`.
- Store run/trial links in SQLite.
- Keep portable package export as an explicit materialization mode.

### P0: Register all retained artifacts in SQLite

Every retained file should have a corresponding SQLite record. If it is not registered, it is scratch or garbage.

Minimum metadata:

- digest
- byte size
- artifact kind/role
- media type if known
- run id
- trial id / schedule idx when applicable
- producer
- source path or container path
- created_at
- retention policy

### P1: Convert trial-local state JSON files into views/mirrors

Keep CLI/debug compatibility by allowing JSON export, but make SQLite the writer of record.

Candidate mirrors:

- `trial_state.json`
- `trial_metadata.json`
- `trial_runtime_state.json`
- `state_inventory.json`
- `benchmark_preflight.json`

### P1: Add resource usage rows and warnings

At build/run/finalize time, record bytes by class:

- package bytes
- run `agent_builds`
- run CAS links
- trial `out`
- trial retained workspace
- logs/events
- SQLite size
- Docker-owned image refs

Warn before/after `full` materialization if estimated retained bytes are high.

### P1: Keep DuckDB read-only over truth by default

DuckDB should attach SQLite and read artifact-derived tables, not own runtime state. Derived DuckDB outputs should be rebuildable and tagged as analysis artifacts.

## Bottom Line

The filesystem should remain the native communication protocol for agents and graders, but not the persistence model.

The target rule is:

```text
SQLite is durable truth.
DuckDB is analytical access.
CAS/filesystem stores opaque bytes by digest.
Trial directories are scratch plus optional exported views.
Host mounts are transport/export surfaces, not state.
```
