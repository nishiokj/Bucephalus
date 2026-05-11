# Generic Runner Boundary Audit

Date: 2026-05-11

Status: read-only boundary audit. No remediation patch is proposed here as code.

This audit inventories benchmark-specific and agent-specific behavior that appears in the current working tree. The goal is to separate acceptable adapter/acquisition code from generic runner contamination.

Important context: the worktree was dirty before this audit. Line references are for the current working tree at audit time, not necessarily `HEAD`.

## Boundary Rule

Generic runner layers must not infer behavior from benchmark names, task ids, image names, agent binary names, provider names, or demo fixture names.

Allowed places for benchmark/app-specific behavior:

1. benchmark acquisition/import scripts
2. named benchmark adapter modules
3. named built-in benchmark templates, if clearly isolated from generic execution
4. tests for those adapters
5. demos and user docs, as examples only

Disallowed places:

1. generic Docker backend
2. generic trial execution
3. generic trial env/command resolution
4. generic persistence, summary, schema, and analysis contracts
5. generic task/materialization/runtime schemas unless the behavior is declared as a general field

## Executive Summary

This is not one isolated hardcode. I found multiple high-impact boundary violations in generic runner layers:

1. SWE-bench image alias resolution in the generic Docker backend.
2. SWE-bench platform inference in generic trial execution.
3. SWE-bench artifact paths persisted in generic trial layout, summary, and schema.
4. Rex command-line mutation in generic agent command resolution.
5. Host grader support wired to a single SWE-bench capability in generic env/staging code.
6. Built-in benchmark authoring logic mixed into `package/authoring.rs`, including benchmark-specific dataset defaults, metrics, grader commands, and sandbox profile selection.

I also found acceptable benchmark-specific code in acquisition/evaluation scripts. Those scripts are not the core problem by themselves; the problem is that generic execution and persistence paths know about SWE-bench/Rex details.

## Findings

### P0: Generic Docker Backend Rewrites SWE-bench Image References

Files:

- `rust/crates/lab-runner/src/backend/docker.rs:248`
- `rust/crates/lab-runner/src/backend/docker.rs:1046`
- `rust/crates/lab-runner/src/backend/docker.rs:1053`
- `rust/crates/lab-runner/src/backend/docker.rs:1061`
- `rust/crates/lab-runner/src/backend/docker.rs:1067`

Observed behavior:

The generic Docker backend tries benchmark-specific image aliases when an image pull fails. It strips `swebench/`, recognizes `sweb.eval.*`, and tries `ghcr.io/epoch-research/...` and `slimshetty/swebench-lite:...`.

Why this violates the boundary:

The Docker backend is a generic transport/runtime layer. It should not know about SWE-bench image naming, third-party registries, or fallback repositories. Any benchmark needing aliases should declare them during acquisition/materialization or package resolution.

Impact:

Other benchmarks cannot get equivalent behavior without copying SWE-bench naming conventions or adding more hardcoded backend branches. This makes Docker behavior benchmark-dependent and non-declarative.

Required remediation:

Remove SWE-bench alias inference from `DockerRuntime`. If aliasing is needed, add a declared image reference list or acquisition-time canonicalization, then make the Docker backend pull exactly the declared refs.

### P0: Generic Trial Execution Infers Platform From SWE-bench Image Names

Files:

- `rust/crates/lab-runner/src/trial/execution.rs:831`
- `rust/crates/lab-runner/src/trial/execution.rs:850`
- `rust/crates/lab-runner/src/trial/execution.rs:1022`

Observed behavior:

`resolve_container_platform(image)` maps image names beginning with `sweb.eval.x86_64.`, `sweb.eval.aarch64.`, or `sweb.eval.arm64.` to Docker platforms.

Why this violates the boundary:

Platform is runtime policy. It must be declared by task rows, runtime config, or grader config. Inferring it from SWE-bench image naming gives SWE-bench privileged behavior and leaves no general way for other benchmarks to specify platform.

Impact:

Any non-SWE-bench benchmark requiring `linux/amd64` or `linux/arm64` has no first-class equivalent. The only implicit workaround is image-name cosplay.

Required remediation:

Add explicit optional platform fields for task sandbox, agent runtime, and separate grader. Remove `resolve_container_platform(image)` entirely. SWE-bench acquisition should emit `platform: linux/amd64` or `linux/arm64` in task rows instead of relying on image names.

### P0: Generic Trial Summary And Schema Require SWE-bench Artifact Knowledge

Files:

- `rust/crates/lab-runner/src/trial/layout.rs:164`
- `rust/crates/lab-runner/src/trial/layout.rs:168`
- `rust/crates/lab-runner/src/trial/schedule.rs:844`
- `schemas/trial_summary_v1.jsonschema:75`
- `schemas/trial_summary_v1.jsonschema:82`

Observed behavior:

Generic trial layout copies `out/official_swebench_eval` into the grader artifact directory. Generic trial summary always includes:

```json
"official_swebench_eval": "grader/official_swebench_eval"
```

The schema requires that key.

Why this violates the boundary:

Trial summary is a generic persistence contract. It must not contain a statically named artifact for one benchmark. Benchmark-specific artifacts should be declared by the grader/benchmark or discovered from produced outputs.

Impact:

Every benchmark appears to have an `official_swebench_eval` artifact slot. This corrupts the result model and makes generic consumers special-case a benchmark they may not use.

Required remediation:

Replace static artifact keys with declared/discovered artifact records. The generic summary may include generic runner artifacts; benchmark artifacts must be emitted by the benchmark adapter or grader output metadata.

### P0: Generic Agent Command Resolution Mutates Rex Commands

Files:

- `rust/crates/lab-runner/src/trial/env.rs:314`
- `rust/crates/lab-runner/src/trial/env.rs:340`
- `rust/crates/lab-runner/src/trial/env.rs:355`

Observed behavior:

Generic runtime command resolution detects `rex`, `rex.js`, `bun rex`, and `bunx rex`, then appends `--input-file` and `--output` flags if they are missing.

Why this violates the boundary:

This is app-specific adapter behavior inside the generic command transport. Generic command resolution should render declared command/env only. If Rex needs a wrapper or adapter, that must be explicit.

Impact:

Rex receives hidden compatibility behavior that no other agent gets. This makes the runner appear to support arbitrary agent CLIs while secretly adapting one CLI.

Required remediation:

Remove command-name detection from generic env resolution. Provide a Rex wrapper script, a Rex adapter module, or explicit command fields in authored YAML.

### P1: Host Grader Capability Is Hardwired To SWE-bench In Generic Env/Staging Code

Files:

- `rust/crates/lab-runner/src/model.rs:724`
- `rust/crates/lab-runner/src/trial/env.rs:104`
- `rust/crates/lab-runner/src/trial/env.rs:105`
- `rust/crates/lab-runner/src/trial/env.rs:121`
- `rust/crates/lab-runner/src/package/staging.rs:384`
- `rust/crates/lab-runner/src/package/staging.rs:388`

Observed behavior:

The runner has a generic `strategy: host`, but host capability validation and path resolution accept only `swebench_official` and `run_official_swebench_eval_from_agentlab.py`.

Why this violates the boundary:

Either `host` is a generic grader strategy, or `swebench_official` is a single built-in benchmark adapter. The current code presents generic host strategy while implementing a one-capability registry in generic env/staging modules.

Impact:

Other host-backed benchmarks cannot use the same mechanism without adding new hardcoded capabilities in generic code.

Required remediation:

Move host capability resolution behind an adapter registry. Generic `host` should either support declared package-owned host commands under a controlled boundary, or the public surface should call this `builtin_host_capability` and dispatch through named adapters.

### P1: Built-in Benchmark Authoring Is Mixed Into Generic Package Authoring

Files:

- `rust/crates/lab-runner/src/package/authoring.rs:706`
- `rust/crates/lab-runner/src/package/authoring.rs:770`
- `rust/crates/lab-runner/src/package/authoring.rs:858`
- `rust/crates/lab-runner/src/package/authoring.rs:887`
- `rust/crates/lab-runner/src/package/authoring.rs:904`
- `rust/crates/lab-runner/src/package/authoring.rs:909`
- `rust/crates/lab-runner/src/package/authoring.rs:1010`

Observed behavior:

The simplified authoring normalizer knows built-in benchmark names, default dataset paths, metric definitions, benchmark policy, grader command, and the `swebench_testbed` sandbox profile.

Why this is risky:

Some built-in template behavior is legitimate, but it is currently colocated with generic package authoring. The risk is that generic behavior and built-in benchmark shortcuts are difficult to separate or reason about.

Impact:

The user-facing authoring path can appear generic while actually routing through two named built-ins: `bench_v0` and `swebench_lite_curated`. New benchmarks may require code changes rather than declarative config.

Required remediation:

Move this into an explicit built-in benchmark registry/module with an interface such as:

```text
BenchmarkAdapter::resolve_authoring_template(input) -> resolved experiment fragment
```

Generic package authoring should call the registry, not contain benchmark-specific match arms.

### P1: `swebench_testbed` Sandbox Profile Adds Generic State Inventory Mounts

Files:

- `rust/crates/lab-runner/src/package/authoring.rs:1010`
- `rust/crates/lab-runner/src/trial/layout.rs:330`

Observed behavior:

The authoring layer sets `policy.task_sandbox.profile` to `swebench_testbed` for SWE-bench. The trial layout layer then checks that string and adds `/testbed` as a writable mount in state inventory.

Why this is risky:

This is at least declarative, but the profile name and behavior are benchmark-specific. If profiles are intended as generic runner capabilities, they need generic names and documented semantics.

Impact:

Another benchmark with a task workdir needing special state inventory behavior has no generic mechanism except adding another hardcoded profile.

Required remediation:

Replace benchmark-named profiles with declarative mount/inventory policy, or move the profile implementation into a benchmark adapter.

### P2: Prebuilt Agent Constants Live In Generic Model

Files:

- `rust/crates/lab-runner/src/model.rs:67`
- `rust/crates/lab-runner/src/model.rs:68`

Observed behavior:

The generic model module defines `prebuilt.codex_cli` and `prebuilt.rex_jesus` constants.

Why this is questionable:

Prebuilt agents may be a real product feature, but they are app-specific identifiers in a generic data model module.

Impact:

This increases ambiguity around whether app-specific support is adapter code or generic runner behavior.

Required remediation:

Move prebuilt agent identifiers into a prebuilt agent registry module, or remove if unused.

## Acceptable Benchmark-Specific Areas

These areas contain many SWE-bench strings, but they are acceptable if treated as adapter/acquisition code rather than generic runner behavior:

- `scripts/acquire_swebench_lite.py`
- `scripts/run_official_swebench_eval_from_agentlab.py`
- `scripts/build_swebench_lite_task_boundary_v3.py`
- `scripts/build-curated-swebench-lite.mjs`
- `scripts/swebench_fetch_official_row.mjs`
- `scripts/agentlab/tests/test_swebench_acquisition_and_official_eval.py`
- `demos/swebench_mini_tasks.jsonl`
- `demos/experiment.yaml`

Condition: generic runner code must not depend on these scripts through hidden path conventions or benchmark-specific artifact names. It may invoke them only through an explicit adapter/capability registry.

## Tests That Currently Assert Violating Behavior

Several tests assert the current hardcoded behavior and must be rewritten when remediation begins:

- `resolve_container_platform_maps_swebench_architecture_tags`
- `p0_i03_swebench_container_commands_request_explicit_platform`
- tests around `official_swebench_eval` in trial layout/summary
- Rex auto-append tests:
  - `resolve_runtime_agent_command_appends_rex_file_io_flags`
  - `resolve_runtime_agent_command_does_not_duplicate_existing_rex_input_flags`
- Docker alias tests:
  - `resolve_remote_image_aliases_maps_swebench_eval_images`
  - `resolve_remote_image_aliases_ignores_non_swebench_images`

These tests are valuable as evidence of the leak. They should not be preserved as generic runner expectations.

## Initial Remediation Plan

Do not patch individual strings first. Fix by boundary:

1. **Runtime Platform Contract**
   - Add explicit platform fields to task rows, agent runtime, and separate grader.
   - Remove image-name platform inference.
   - Update SWE-bench acquisition to emit platform.

2. **Image Resolution Contract**
   - Remove SWE-bench aliasing from Docker backend.
   - Add declared image aliases or canonical refs during acquisition/package build if needed.

3. **Artifact Contract**
   - Replace fixed `official_swebench_eval` summary/schema fields with generic artifact declarations.
   - Let graders declare or emit benchmark-specific artifact records.

4. **Agent Adapter Boundary**
   - Remove Rex detection from generic command resolution.
   - Make Rex compatibility an explicit wrapper/adapter.

5. **Benchmark Adapter Registry**
   - Move `bench_v0` and `swebench_lite_curated` authoring logic out of generic package authoring.
   - Host grader capabilities should resolve through this registry.

6. **Schema And Docs Cleanup**
   - Update schemas to reflect generic contracts.
   - Keep SWE-bench docs only as adapter examples.

## Audit Commands Used

Primary scans:

```bash
rg -n -i "swe.?bench|sweb\\.eval|official_swebench|bench_v0|rex|codex|oracle|glm|kimi|anthropic|openai|mapped_output|raw_output|candidate_patch|task_rows\\.jsonl|task_boundary|platform|resolve_remote_image_aliases|slimshetty" rust/crates scripts docs/user demos .lab/experiment.yaml
rg -n -i "swe.?bench|sweb\\.eval|bench_v0|rex|codex|official_swebench|slimshetty|epoch-research|task_rows\\.jsonl" rust/crates/lab-runner/src --glob '!tests.rs'
rg -n "fn write_trial_summary|official_swebench_eval|resolve_container_platform|resolve_remote_image_aliases|should_append_rex_contract_io|is_rex_headless_command|resolve_builtin_benchmark_dataset_path|builtin_benchmark|prebuilt|task_workdir_support_destination_path" rust/crates/lab-runner/src rust/crates/lab-cli/src scripts
```

Negative scan after the previous naming cleanup:

```bash
rg -n "task_spec|task spec|\\.task_spec" rust/crates/lab-runner/src docs/user demos scripts .lab/experiment.yaml
```

Result at audit time: no remaining `task_spec` hits in those paths.

## Open Questions

1. Should `host` graders be a generic public strategy, or only a built-in capability mechanism?
2. Should built-in benchmark templates remain in the main runner crate, or move to a separate adapter crate/module?
3. Should trial summaries enumerate artifacts from actual files, declared output mounts, grader metadata, or all three?
4. Should image aliasing be a package-build concern, a task-row field, or forbidden entirely?
5. Should Rex support remain in-tree as an adapter, or be treated like any other external agent wrapper?

