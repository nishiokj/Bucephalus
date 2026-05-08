# Patch Spec: Remote Runtime Package Boundary

Status: Draft v2
Date: 2026-05-07

## Goal

Make a sealed build package portable across local Docker and user-owned remote execution without turning AgentLab into a storage provider or multi-cloud SDK.

The runner owns experiment boundaries: immutable package inputs, trial contract IO, execution status, evidence refs, grading records, and analysis semantics. The user owns infra: cloud account, IAM, object storage, worker deployment, app databases, persistent volumes, caches, and private logs.

This patch deliberately avoids a generic cloud storage abstraction in the local runner. Remote mode talks to a user-hosted AgentLab-compatible service. That service owns storage resolution through the identity and permissions the user attached to it.

## Current State

The current tree already has useful pieces:

1. `build_experiment_package(...)` creates `manifest.json`, `resolved_experiment.json`, `checksums.json`, `package.lock`, `tasks/tasks.jsonl`, staged agent builds, and `staging_manifest.json`.
2. Package build rewrites runtime paths into package-relative paths.
3. Large runtime assets can be replaced by `agentlab_cas_pointer_v1` files through `package/cas.rs`.
4. Trial execution already writes evidence through `lab_core::ArtifactStore`, returning `artifact://sha256/<hex>` refs.
5. Local trial execution has a clear contract around `/agentlab/in`, `/agentlab/out`, task image, workdir, grader output, stdout/stderr, and `trial_runtime_state.json`.

The current portability gaps:

1. Large package blobs are not necessarily package-local. `put_file_in_cas(package_dir, ...)` resolves to `.lab/objects/...` when the package is under `.lab/builds`, so a copied package directory can contain pointer files without the actual blob bytes.
2. `resolved_experiment.json` still uses package-relative filesystem paths as runtime artifact identity. That works locally but is not a remote materialization contract.
3. The runtime package has no explicit blob inventory. `checksums.json` proves files in the package, but it does not say which bytes are runtime materialization inputs, where they should appear in the sandbox, or which executor owns resolving them.
4. `trial::execution` calls `DockerRuntime::connect()` directly, so remote execution cannot plug in without duplicating the local scheduling path.
5. SDK types mention `executor: remote`, `remoteEndpoint`, and `remoteTokenEnv`, but the Rust CLI does not expose those flags in production.
6. Existing remote worker docs describe older `WorkerBackend` concepts that are not on the current production path and should not be revived.

## Non-Goals

1. Do not implement S3/GCS/Azure clients in `lab-cli`.
2. Do not require `publish` for local runs.
3. Do not replace user-owned app storage with AgentLab storage.
4. Do not add `local_process` unless a real non-container scientific path exists.
5. Do not support arbitrary remote worker protocols. Define one small AgentLab remote service contract.
6. Do not add a new scheduling engine. Patch the existing schedule path.

## Product Boundary

Local mode:

```text
lab build experiment.yaml --out .lab/builds/e1
lab run .lab/builds/e1
```

The local resolver reads package-local blobs and uses Docker/containerd through the existing local backend. No publish step.

Remote mode:

```text
lab build experiment.yaml --out .lab/builds/e1
lab run .lab/builds/e1 --executor remote --remote-endpoint https://agentlab.company.com
```

The CLI streams/uploads the sealed package to the remote service. The remote service stores package blobs and evidence using its own configured storage and cloud identity. The local CLI does not need to know whether the service uses S3, EFS, GCS, NFS, local disk, or something else.

## Target Runtime Package Contract

This is the Phase 2+ target contract. Phase 1 only moves package blobs under the package directory while keeping `sealed_run_package_v2` and the existing manifest shape.

Target package layout:

```text
manifest.json
resolved_experiment.json
runtime_package.json
checksums.json
package.lock
tasks/tasks.jsonl
blobs/sha256/<hex>/blob
agent_builds/...
runtime_assets/...
files/...
staging_manifest.json
```

`runtime_package.json` is the executor-facing package inventory:

```json
{
  "schema_version": "runtime_package_v1",
  "package_digest": "sha256:...",
  "blobs": {
    "sha256:aaa": {
      "package_path": "blobs/sha256/aaa/blob",
      "size_bytes": 1234,
      "media_type": "application/octet-stream"
    }
  },
  "inputs": {
    "resolved_experiment": {
      "path": "resolved_experiment.json",
      "digest": "sha256:..."
    },
    "tasks": {
      "path": "tasks/tasks.jsonl",
      "digest": "sha256:..."
    }
  },
  "materializations": [
    {
      "id": "agent:baseline",
      "kind": "directory",
      "source": {
        "package_path": "agent_builds/000_agent",
        "digest": "sha256:..."
      },
      "target": "/opt/agent",
      "visibility": "runner_owned",
      "read_only": true
    }
  ],
  "contract_paths": {
    "trial_input": "/agentlab/in/trial_input.json",
    "result": "/agentlab/out/result.json",
    "raw_grader_output": "/agentlab/out/raw_grader_output.json",
    "mapped_grader_output": "/agentlab/out/mapped_grader_output.json",
    "trajectory": "/agentlab/out/trajectory.jsonl"
  }
}
```

Rules:

1. `runtime_package.json` contains no provider URLs and no credentials.
2. All runner-owned package bytes must be reachable from the package directory.
3. Authoring paths are resolved at build time only.
4. Sandbox targets are container paths, not host paths.
5. User-owned opaque mounts are declarations only. The remote service resolves them at run/preflight time.

## Path Resolution

Use scoped path resolution:

| Namespace | Example | Resolved by | Persisted in package |
|---|---|---|---|
| Authoring path | `./agent`, `./grader.py` | build | no raw host path |
| Package blob/path | `agent_builds/000_agent`, `sha256:...` | package resolver | yes |
| Contract path | `/agentlab/out/result.json` | executor | yes |
| Sandbox/user path | `/app/data/app.sqlite` | user image/infra | only as opaque mount target if declared |
| Remote storage URL | S3/GCS/etc. | remote service | no |

The runner validates reserved `/agentlab/*` paths and records evidence refs. It does not resolve user app storage like SQLite databases, caches, private app logs, or external service state.

## Remote Service Contract

Add one small HTTP API. Keep provider-specific storage behind this service.

```text
POST /v1/packages
POST /v1/runs
GET  /v1/runs/{run_id}
GET  /v1/runs/{run_id}/events?from_seq=N
POST /v1/runs/{run_id}/control
GET  /v1/runs/{run_id}/evidence
GET  /v1/evidence/{evidence_id}
```

Minimum semantics:

1. `POST /v1/packages` accepts a sealed package archive or package manifest plus blobs. It returns `remote_package_ref`.
2. `POST /v1/runs` accepts `remote_package_ref`, runtime env metadata, secret binding names, materialization mode, and optional opaque mount bindings.
3. In the first remote implementation, the local runner keeps the existing schedule and submits one trial attempt at a time. The remote service executes submitted trial attempts and runs OCI task images using user-owned infrastructure. A later service-owned scheduler is out of scope until there is a separate design.
4. Workers write required evidence: `result.json`, grader outputs, stdout/stderr, hook events when configured, and declared artifacts.
5. Remote service returns evidence refs with digest, size, media type, role, trial id, and variant/task ids.
6. Control actions are best-effort but must return an explicit receipt.

Authentication is endpoint-level. The local runner supplies a bearer token from `--remote-token-env`, an env var, or future config. Storage credentials are never sent by `lab-cli`; they live on the remote service/worker through IAM, workload identity, mounted credentials, or the user's chosen mechanism.

## Implementation Phasing

The package boundary work should ship before the remote backend. Remote execution is still useful context, but it is too under-specified to be the forcing function for this change.

Phase 1: self-contained package blobs, local-only behavior.

1. Move package CAS blobs under the sealed package directory.
2. Make local CAS pointer materialization resolve package-local blobs after a package is copied.
3. Include package blobs in existing `checksums.json`.
4. Add regression coverage proving a package can be copied without `.lab/objects` and still preflight/materialize.
5. Do not emit `runtime_package.json`, do not bump to `sealed_run_package_v3`, do not add remote CLI flags.

Phase 2: package inventory.

1. Add `runtime_package.json` as a descriptive inventory of inputs, package blobs, runner-owned materializations, and contract paths.
2. Keep it executor-neutral. It should not know Docker, HTTP, S3, IAM, or scheduling.
3. Define directory digest semantics before using directory-level digests. If that is not done, model materialization sources as file-set/checksum references instead of opaque directory digests.
4. Keep default local execution unchanged.

Phase 3: package schema upgrade.

1. Emit `sealed_run_package_v3`.
2. Add `runtime_package_ref` to `manifest.json`.
3. Verify `runtime_package.json` exists, is checksummed, and matches package integrity expectations.
4. Preserve `sealed_run_package_v2` only through an explicit compatibility path.

Phase 4: executor naming and local execution boundary.

1. Make `ExecutorKind` production with `local_docker` and `remote`.
2. Normalize emitted evidence/runtime metadata from `"docker"` to `"local_docker"` where schemas expect it.
3. Add the executor trait only if the local refactor stays small and improves the current call path.
4. Do not implement remote HTTP in this phase.

Phase 5: remote prototype.

1. Add a mock remote service.
2. Submit one trial attempt at a time from the existing local scheduler.
3. Widen `TrialRuntimeOutcome` or add an executor evidence bundle so remote stdout/stderr/hook refs do not require pretending they are local files.
4. Update evidence schemas and persistence to allow remote refs deliberately.

## Phase 1 Detailed Patch Spec

Phase 1 makes packages self-contained without changing the public package schema or remote surface. This is the smallest robust slice and should be safe to ship independently.

### Phase 1 Goals

1. A package built under `.lab/builds/...` contains every runner-owned byte needed for local run/preflight.
2. Copying the package directory to a temp location without `.lab/objects` does not break pointer-backed runtime assets.
3. Existing local Docker behavior remains unchanged for users.
4. No remote backend, no publish step, no new storage provider abstraction.

### Phase 1 Non-Goals

1. Do not add `runtime_package.json`.
2. Do not emit `sealed_run_package_v3`.
3. Do not add `--executor remote`, `--remote-endpoint`, or remote upload.
4. Do not change `ArtifactStore`; run evidence remains under `run_dir/artifacts`.
5. Do not introduce generic object storage traits or S3/GCS/Azure clients.
6. Do not change package-relative authoring path rewrites except where needed for package-local blob resolution.

### Phase 1 Package Layout

Target v2-compatible layout after build:

```text
manifest.json
resolved_experiment.json
checksums.json
package.lock
tasks/tasks.jsonl
blobs/sha256/<hex>/blob
agent_builds/...
runtime_assets/...
files/...
staging_manifest.json
```

`blobs/` is package-owned. It is not a run evidence store and is not a global project cache.

### Phase 1 File-Level Plan

Patch `rust/crates/lab-runner/src/package/cas.rs`:

1. Add `PACKAGE_BLOBS_DIR: &str = "blobs"`.
2. Add `package_blob_path_for_digest(package_dir: &Path, digest: &str) -> Result<PathBuf>` returning `package_dir/blobs/sha256/<hex>/blob`.
3. Add `put_file_in_package_cas(package_dir: &Path, source: &Path) -> Result<(String, PathBuf)>`.
4. Add `resolve_package_cas_pointer_blob(package_dir: &Path, pointer_path: &Path) -> Result<Option<PathBuf>>`.
5. Add `path_contains_cas_pointer` variants or parameters only if needed; the existing function may stay if it only detects pointer presence.
6. Keep `object_root_for_path`, `object_blob_path_for_digest`, and `put_file_in_cas` only if another non-package caller still needs the old global `.lab/objects` behavior. If package build is the only caller, delete or make old helpers private to tests so new package code cannot accidentally use them.

Patch `rust/crates/lab-runner/src/package/compile.rs`:

1. Create `package_dir/blobs` at build start.
2. Replace `put_file_in_cas(package_dir, source)` in runtime asset staging with `put_file_in_package_cas(package_dir, source)`.
3. Keep pointer file contents as:

   ```json
   {
     "schema_version": "agentlab_cas_pointer_v1",
     "kind": "file",
     "digest": "sha256:<hex>",
     "size_bytes": 123
   }
   ```

4. Do not store absolute blob paths in pointer files.
5. Keep the existing checksum walk. Once blobs live under the package directory, `checksums.json` should include `blobs/sha256/<hex>/blob` automatically. Add an assertion/test so this remains true.

Patch `rust/crates/lab-runner/src/trial/prepare.rs`:

1. Stop resolving package pointer blobs from the pointer file path alone.
2. Thread the package root into CAS materialization. Do not assume `project_root` is the package root; expose/pass the loaded package directory (`LoadedExperimentInput.exp_dir` for sealed packages) or an equivalent explicit package root.
3. Replace:

   ```rust
   materialize_cas_backed_path(&spec.source_from_host, &materialized)
   ```

   with a package-root-aware call, for example:

   ```rust
   materialize_package_cas_backed_path(package_dir, &spec.source_from_host, &materialized)
   ```

4. The resolver must read the pointer at `spec.source_from_host`, extract the digest, and resolve the blob as `package_dir/blobs/sha256/<hex>/blob`.
5. Reject missing package-local blobs with a `preflight_failed`-style error during preflight and a clear runtime error during run.

Patch `rust/crates/lab-runner/src/experiment/preflight.rs`:

1. Verify every `agentlab_cas_pointer_v1` found under package-owned runtime materialization sources resolves to `package_dir/blobs/sha256/<hex>/blob`.
2. Verify each resolved blob exists, is a file, and matches the pointer `size_bytes`.
3. Verify the blob path is present in `checksums.json`.
4. Verify the blob file digest equals the pointer digest and the checksum digest.
5. Keep Docker image probing unchanged.

Patch `rust/crates/lab-runner/src/package/sealed.rs`:

1. Keep `sealed_run_package_v2` as the accepted schema for Phase 1.
2. Add integrity validation that catches pointer files whose blobs are missing from the package.
3. Do not require `runtime_package.json` yet.

Patch tests in `rust/crates/lab-runner/src/tests.rs` or a focused package test module:

1. Unit: `package_blob_path_for_digest` accepts only `sha256:<hex>` and returns `blobs/sha256/<hex>/blob`.
2. Unit: package CAS write stores bytes under `package_dir/blobs`, not `.lab/objects`.
3. Regression: build a package with a runtime asset larger than `AGENTLAB_CAS_FILE_THRESHOLD_BYTES`; assert the pointer file exists and the blob is package-local.
4. Regression: copy the package to a temp directory, delete or ignore the original `.lab/objects`, then run package preflight against the copy.
5. Regression: corrupt or delete `blobs/sha256/<hex>/blob`; preflight fails with a specific missing/corrupt package blob message.

### Phase 1 Deletions and Cleanup

Delete or quarantine code that can silently write package blobs outside the package:

1. Remove package-build use of `object_root_for_path(package_dir)`.
2. Remove any package-build call to `put_file_in_cas`.
3. If no non-package production caller remains, delete `lab_root_for_path`, `object_root_for_path`, `object_blob_path_for_digest`, and `put_file_in_cas`.
4. If old helpers remain for compatibility, rename them to make their scope explicit, such as `legacy_lab_object_blob_path_for_digest`, and add a comment that package builds must not use them.
5. Delete tests that assert package CAS blobs land under `.lab/objects`; replace them with package-local assertions.
6. Do not delete `ArtifactStore`; it is run evidence, not package input storage.

### Phase 1 Leak Fixes

Leak: package pointer resolution currently derives the blob root from the pointer path. When a pointer file lives under `.lab/builds/e1/runtime_assets/...`, walking up to `.lab` resolves the blob under `.lab/objects`, outside the package.

Fix:

1. Build writes blobs to `package_dir/blobs/sha256/<hex>/blob`.
2. Pointer files store only digest and size.
3. Runtime/preflight resolution receives `package_dir` explicitly and never derives package blob roots by walking ancestors.
4. `resolve_package_path_under_root` remains the guard for package-relative paths.
5. Any resolved blob path must pass a package-root containment check before it is read.

Leak: `checksums.json` currently proves only files found under the package directory. If blobs are outside the package, integrity verification cannot see them.

Fix:

1. Put blobs under the package before checksum collection runs.
2. Assert checksum files include every package blob referenced by a pointer file.
3. Reject pointer files whose digest is absent from package checksums.

Leak: local trial preparation materializes CAS-backed dynamic mounts into trial temp dirs using pointer-only resolution.

Fix:

1. Pass package root into the materialization function.
2. Resolve pointer blobs from package root.
3. Materialize copied/hard-linked bytes into the trial temp dir exactly as today after resolution.

### Phase 1 Gaps and Load-Bearing Assumptions

1. Package root discovery must be explicit. The implementation should not guess package root from arbitrary runtime paths, and should not treat `project_root` as package root unless that is proven at the call site. If a caller cannot provide package root, that caller is not ready for pointer-backed package assets.
2. Pointer files only support file blobs today. Directory-level CAS is not part of Phase 1.
3. `agent_builds` are copied directly today and are not CAS-backed. That is acceptable for Phase 1.
4. Directory digests are intentionally deferred. Do not add directory digest fields until canonical ordering, symlink handling, permissions, and empty directory behavior are defined.
5. Symlink behavior is load-bearing. Existing copy helpers preserve symlinks; Phase 1 must ensure a symlink cannot make checksum verification or package blob resolution read outside the package root.
6. The large-file threshold env var can make tests nondeterministic. Regression tests should set `AGENTLAB_CAS_FILE_THRESHOLD_BYTES` to a tiny value for the test process.
7. The current package checksum digest excludes `manifest.json`, `checksums.json`, and `package.lock`. Phase 1 should not change that unless a separate integrity migration is planned.
8. Remote semantics are deliberately not validated by Phase 1. A self-contained package is necessary for remote execution, but not sufficient.

## Later Phase Sketches

### Phase 2 Runtime Package Builder

Add `rust/crates/lab-runner/src/package/runtime_package.rs`.

Responsibilities:

1. Build `runtime_package_v1` from `resolved_experiment.json`, `staging_manifest.json`, package checksums, and package blob inventory.
2. Emit materialization entries for agent artifact directories/files and staged runtime assets.
3. Emit contract paths from `lab_core` constants.
4. Reject absolute host paths in runner-owned runtime fields.
5. Validate that every materialization source exists under the package root.

Keep this module boring. It should not know Docker, HTTP, S3, IAM, or worker scheduling.

### Phase 3 Package Schema Upgrade

Patch `rust/crates/lab-runner/src/package/sealed.rs`:

1. Accept `sealed_run_package_v3` with `runtime_package_ref`.
2. Verify `runtime_package.json` exists, is checksummed, and has matching `package_digest`.
3. Preserve v2 loading only behind an explicit compatibility path if needed by tests; do not let v2 be the default build output after Phase 3.

### Phase 4 Execution Boundary

Add `rust/crates/lab-runner/src/trial/executor.rs` only after Phase 1-3 are stable:

```rust
pub(crate) trait TrialRuntimeExecutor {
    fn execute_trial(
        &self,
        request: &PreparedTrialExecutionRequest,
    ) -> anyhow::Result<TrialRuntimeOutcome>;
}
```

`PreparedTrialExecutionRequest` should be an owned request struct assembled in `trial::schedule` from the existing `PreparedScheduledTrial`, `AdapterRunRequest` fields, `TaskSandboxPlan`, and package context. It exists to keep the executor boundary from borrowing scheduler internals.

Do not reintroduce the older `WorkerBackend` abstraction. The current production engine already owns concurrency and exactly-once commit.

Before remote work, resolve these design gaps:

1. `TrialRuntimeOutcome` must be able to carry remote stdout/stderr/hook evidence refs without forcing local artifact downloads.
2. Evidence schemas must intentionally allow remote refs, or the remote service must return local-compatible `artifact://sha256/...` refs backed by downloaded local bytes.
3. Runtime metadata should use `local_docker` and `remote`, not the current ad hoc `"docker"` string.

### Phase 5 Remote Package Upload

Implement remote upload inside `RemoteTrialExecutor` or a small `remote_client` module:

1. Before starting a remote run, call `POST /v1/packages` with the sealed package archive or package manifest plus blobs.
2. Cache returned `remote_package_ref` in run session state.
3. Do not add `lab publish` in the first remote patch. Remote `run` can perform implicit upload. A separate `publish` command can be added later if repeated remote runs need it.

For local mode, no upload and no extra command.

### Later Cleanup Targets

Do not resurrect the old `WorkerBackend` path described in `docs/PATCH_SPEC_PARALLEL_WORKER_HARD_CUTOVER.md`.

Cleanup targets:

1. Mark `docs/PATCH_SPEC_EXECUTOR_STORAGE_BOUNDARY.md` as superseded by this patch where it recommends generic storage adapters in the runner.
2. Mark remote `WorkerBackend` references in `docs/PARALLEL_WORKER_P0_P1_COMPLETION.md` and `docs/P7_REVIEW_FINDINGS.md` as historical notes.
3. Remove SDK `local_process` surface until implemented.
4. Remove or update any tests that assert executor choice is test-only.

## Acceptance Criteria

Phase 1 acceptance:

1. `lab build` still emits `sealed_run_package_v2`.
2. A package directory is self-contained: copying it without `.lab/objects` still passes package integrity checks and local preflight.
3. Large runtime assets are written under `blobs/sha256/<hex>/blob` inside the package.
4. No package-build code path writes package runtime asset blobs to `.lab/objects`.
5. Every package CAS pointer resolves through an explicit package root.
6. `checksums.json` includes every package blob referenced by a pointer.
7. Corrupt or missing package blobs fail preflight before Docker execution.
8. `lab build-run ...` and `lab run ...` keep current local behavior without requiring publish or remote flags.
9. Existing local integration tests pass.

Later phase acceptance:

1. Phase 2: `lab build` emits `runtime_package_v1`.
2. Phase 3: `lab build` emits `sealed_run_package_v3` with `runtime_package_ref`.
3. Phase 4: local executor metadata uses `local_docker` consistently and executor choice is production, not test-only.
4. Phase 5: `lab run ... --executor remote --remote-endpoint ...` uploads the package to a mock remote service, submits one trial attempt at a time, and commits returned trial results through the existing scheduler.

## Test Plan

Phase 1 tests:

1. Unit: package-local blob path derivation rejects malformed digests and returns `blobs/sha256/<hex>/blob`.
2. Unit: package CAS writes bytes to the package root and never to `.lab/objects`.
3. Unit: pointer resolution requires an explicit package root and rejects blob paths outside that root.
4. Unit: package integrity verification reports missing pointer blobs.
5. Unit: package integrity verification reports blob digest mismatch.
6. Regression: package with a large runtime asset preflights after being copied to a temp directory without the original `.lab/objects`.
7. Regression: local materialization of a CAS-backed dynamic mount works from the copied package.
8. Integration: local Docker run parity on a tiny demo package.

Later phase tests:

1. Unit: `runtime_package_v1` rejects absolute host paths and missing materialization sources.
2. Unit: `sealed_run_package_v3` integrity verification includes `runtime_package.json` and package blobs.
3. CLI: `--executor remote` requires `--remote-endpoint`.
4. SDK: remote args emit the expected CLI flags after Rust CLI support exists.
5. Integration: mock remote service accepts package upload and returns evidence sufficient for one committed trial.

## Open Questions

Phase 1 questions:

1. Should package integrity verification scan all package files for CAS pointers, or only scan package-owned runtime materialization roots from `resolved_experiment.json` and `staging_manifest.json`? Scanning all package files is safer but slightly broader.
2. Should the old `.lab/objects` package CAS helpers be deleted immediately if no production caller remains, or retained under `legacy_` names for one release to reduce churn?
3. Should local preflight verify pointer blobs only when pointers exist, or should it also assert that no package references `.lab/objects` anywhere in `resolved_experiment.json`?

Later phase questions:

1. Should implicit remote upload archive the package as a tar stream, or send manifest plus missing blobs by digest?
2. What is the minimal opaque mount binding shape for remote preflight? Suggested start: `id`, `target`, `mode`, `required`, and executor-specific `binding` passed through without interpretation.
3. Should remote refs be first-class evidence URIs in schemas, or should the local runner download remote evidence and re-store it through local `ArtifactStore` for analysis compatibility?
