# Patch Spec: User Review Product Boundary Hard Cutover

Status: Draft (Hard Cut Required)
Date: 2026-05-09
Owner: `docs/user`, `lab-cli`, `lab-runner`
Priority: P0 Onboarding / Contract Truthfulness

## 1. Intent

Make the product-facing path truthful and executable for a new user who wants to benchmark their own agent.

This patch is not a feature expansion pass. It is a hard cutover that removes misleading surfaces, stale migration language, hidden shape mapping, and docs that stop one layer short of a real experiment.

After this patch:

1. The user docs show a real bring-your-own-agent flow.
2. The canonical CLI command name is `lab`.
3. The runner transports benchmark task payloads through the trial envelope without semantic rewriting.
4. The public experiment surface does not advertise unsupported polymorphism.
5. Grader env/network behavior is explicit and covered.
6. The quickstart uses one pre-run validation path.

## 2. Problem

`user_review_analysis.md` identifies a pattern: the repo has most of the machinery, but the product surface is either under-documented or shaped like a promise the runner does not actually keep.

Current issues to cut over:

1. `docs/user/` has a deterministic demo but no bring-your-own-agent walkthrough.
2. `variant_plan` is central to model A/B evaluation but barely explained.
3. Docs use `lab` while the built binary is `lab-cli`.
4. Schemas are named repeatedly but not linked.
5. Result inspection is framed around one run and does not explain cross-run analysis.
6. Grader env/network behavior is ambiguous, especially for LLM-judge graders.
7. `dataset.provider` is polymorphic-looking while only `local_jsonl` exists.
8. Task/grader strategy variants exist in code but are not product-documented as supported contracts.
9. `describe` and `preflight` both appear in the golden path even though preflight is the validation gate.
10. Runtime trial input previously contained runner-side task alias mapping; that violates the envelope/pass-through boundary.

## 3. Non-Negotiable Invariants

1. No runner-side semantic task mapping survives in trial input construction.
2. The `/task` field in `trial_input_v1` is an opaque benchmark payload owned by the benchmark/agent adapter.
3. If a field has only one supported value, docs and validation must say that directly.
4. If a strategy is supported by production code, docs must show its contract and required fields.
5. If a strategy is not supported, it must be rejected, not ignored or silently defaulted.
6. User-facing errors must describe the current contract. They must not explain migrations, milestones, or historic cutovers.
7. The golden path must have one pre-run validation command.
8. No compatibility aliases should be added. No deprecated field should be accepted in order to be “helpful.”

## 4. Scope

This patch covers the highest-leverage review items that can be fixed surgically:

1. docs onboarding
2. CLI naming consistency
3. schema links
4. variant examples
5. cross-run result inspection docs using current storage
6. grader env/network docs and validation
7. public API truthfulness for dataset provider and materialization/grading strategies
8. runner task-envelope pass-through
9. removal of stale validation language and obsolete test-only IO remapping surfaces

This patch does not implement the larger wishlist items:

1. built-in LLM-judge grader
2. agent-result cache/reuse
3. cost dashboard
4. new retry/resume behavior beyond existing run continuation/recovery

Those need separate product specs. Folding them into this patch would make the cutover non-surgical.

## 5. Required Public Contract

### 5.1 Agent trial input

The agent receives exactly one runner-owned envelope:

```json
{
  "schema_version": "trial_input_v1",
  "ids": {},
  "task": {},
  "artifact_type": "structured_json",
  "design": {},
  "runtime": {}
}
```

`task` is pass-through. The runner may validate that it is an object, but it must not rewrite benchmark-native fields such as:

1. `/prompt`
2. `/input/prompt`
3. `/swebench/input/prompt`

Any agent-specific shape mapping belongs in the agent artifact or a benchmark/agent adapter, not in `lab-runner`.

### 5.2 Dataset provider

`dataset.provider` has one supported value:

```yaml
provider: local_jsonl
```

The patch must make this a literal contract, not a pretend plugin surface:

1. reject any value other than `local_jsonl`
2. document `provider` as a fixed literal
3. remove wording that implies alternate providers exist

Do not add new providers in this patch.

### 5.3 Task materialization

The runner currently has two task materialization kinds:

1. `task_image`
2. `base_image_bundle`

The patch must choose one of these two hard-cut options:

1. document and test both as supported user-facing contracts, or
2. reject `base_image_bundle` in public task rows and keep it runner-private until fully documented

Minimum preferred cut: document both only if the existing production path is complete across build, preflight, run, fork, and replay. Otherwise reject `base_image_bundle` at public build input with a direct error.

### 5.4 Grader strategies

The runner currently parses:

1. `in_task_image`
2. `injected`
3. `separate`
4. `host`

The patch must make these truthful:

1. `in_task_image`: grader command runs in the task container after agent execution.
2. `injected`: runner copies a grader bundle into the task container, then runs the command there.
3. `separate`: runner starts a separate grading container using `benchmark.grader.separate.image` and `benchmark.grader.separate.workdir`.
4. `host`: runner executes the grader command on the host with host contract paths. This is the official SWE-bench integration strategy and must be documented and tested rather than left implicit.

For every documented strategy, add:

1. a minimal YAML example
2. required fields
3. network behavior
4. env behavior
5. one focused production or integration test that proves the strategy resolves and runs or preflights

If any strategy lacks complete production behavior, reject it until it is complete.

## 6. Required Code Changes

### 6.1 Trial input pass-through

Files:

1. `rust/crates/lab-runner/src/trial/prepare.rs`
2. `rust/crates/lab-runner/src/tests.rs`

Required changes:

1. Delete `normalize_task_prompt_aliases`.
2. Remove all call sites.
3. Build `trial_input_v1.task` from `task_boundary.task_payload.clone()` directly.
4. Replace alias-normalization tests with a pass-through invariant test.
5. Add a regression with duplicated prompt-looking fields proving the runner preserves all of them unchanged.

Acceptance:

```bash
cargo test -p lab-runner build_trial_input_preserves_task_payload_without_alias_mapping --quiet
rg -n "normalize_task_prompt_aliases|swebench/input/prompt" rust/crates/lab-runner/src
```

The `rg` command must return no production code matches for semantic task alias mapping.

### 6.2 Remove test-only IO remapping surface

Files:

1. `rust/crates/lab-runner/src/experiment/runtime.rs`
2. `rust/crates/lab-runner/src/trial/env.rs`
3. `rust/crates/lab-runner/src/tests.rs`

Current obsolete surface:

1. `AgentRuntimeIoConfig`
2. `AgentRuntimeConfig.io`
3. `#[cfg(test)]` appending of `input_arg` / `output_arg`
4. tests that set `runtime.io`

Required changes:

1. Delete `AgentRuntimeIoConfig`.
2. Delete `AgentRuntimeConfig.io`.
3. Delete the `#[cfg(test)]` fallback in `resolve_runtime_agent_command`.
4. Update fixtures to construct the current runtime shape only.
5. Keep the production Rex convenience only if it is deliberately product-supported. If kept, document it as a Rex-specific compatibility shim and add an explicit test showing generic agents are not remapped.

Preferred hard cut:

1. Remove generic IO remapping entirely.
2. Require every agent command to consume `AGENTLAB_TRIAL_INPUT_PATH` / `AGENTLAB_RESULT_PATH` or explicitly pass those env values in its own command.
3. Do not append input/output flags based on agent type.

Acceptance:

```bash
rg -n "AgentRuntimeIoConfig|input_arg|output_arg|runtime\\.agent_runtime/io|runtime\\.io" rust/crates/lab-runner/src
```

Only documentation of rejected fields may remain, and only if the error is part of the current public contract.

### 6.3 Current-contract validation language

Files:

1. `rust/crates/lab-runner/src/package/validate.rs`
2. `rust/crates/lab-runner/src/experiment/runtime.rs`
3. `rust/crates/lab-runner/src/package/authoring.rs`
4. `rust/crates/lab-runner/src/trial/preflight.rs`
5. `rust/crates/lab-runner/src/tests.rs`

Required changes:

1. Replace user-facing phrases like `was removed`, `legacy`, `hard cut`, and milestone references with current-contract language.
2. Keep rejection behavior for unsupported fields.
3. Rename tests and fixtures that use `legacy` only to mean “unsupported old shape.”
4. Do not keep fallback parsing paths whose only purpose is backwards compatibility.

Allowed exception:

1. Low-level recovery names such as `legacy_slot_commit_id` may remain only if they are internal durable-state migration helpers and not part of user-facing configuration or docs. If they remain, add a comment that they are persistence compatibility, not accepted authoring surface.

Acceptance:

```bash
rg -n "was removed|hard cut|Milestone|legacy" rust/crates/lab-runner/src docs/user
```

Every remaining match must be either:

1. internal persistence compatibility with an explanatory comment, or
2. a test that asserts old persisted state can still be read, not a public authoring fallback.

### 6.4 Dataset provider literal validation

Files:

1. `rust/crates/lab-runner/src/package/compile.rs`
2. `rust/crates/lab-runner/src/config.rs`
3. `schemas/resolved_experiment.jsonschema`
4. `schemas/task_row_v1.jsonschema` if needed
5. `docs/user/task-rows.md`
6. `docs/user/what-you-provide.md`

Required changes:

1. Validate `/dataset/provider == "local_jsonl"` during build/package loading.
2. Fail on missing provider only if the schema requires it. Otherwise choose one rule and document it:
   1. required fixed literal, or
   2. optional field defaulting to `local_jsonl`
3. Remove any docs implying alternate providers.
4. Add a rejection test for `provider: something_else`.

Acceptance:

```bash
cargo test -p lab-runner dataset_provider --quiet
rg -n "provider:" docs/user demos .lab/experiment.yaml
```

All examples must either use `local_jsonl` or omit the field under the documented default.

### 6.5 Grader env and network contract

Files:

1. `rust/crates/lab-runner/src/trial/env.rs`
2. `rust/crates/lab-runner/src/trial/execution.rs`
3. `rust/crates/lab-runner/src/experiment/preflight.rs`
4. `docs/user/env-and-secrets.md`
5. `docs/user/graders-and-mappers.md`

Required changes:

1. Document that grader commands receive the same resolved runtime env as the agent command unless explicitly changed by a new first-class field.
2. Do not introduce silent secret filtering in this patch.
3. Document that `in_task_image` and `injected` use the task sandbox network.
4. Document that `separate` currently uses the same effective network mode passed to the trial runtime unless a first-class `benchmark.grader.separate.network` is added.
5. Preferred hard cut: add `benchmark.grader.separate.network` only if it is required to make LLM-judge graders honest. If added, validate it with the same allowed values as runtime network and document it.
6. Add a test proving grader env contains an `--env` value.
7. Add a test proving grader network mode for `separate`.

Acceptance:

```bash
cargo test -p lab-runner grader_env --quiet
cargo test -p lab-runner separate_grader_network --quiet
```

### 6.6 CLI command name

Files:

1. `rust/crates/lab-cli/Cargo.toml`
2. `scripts/lab-fresh.sh`
3. `scripts/verify-docs-golden-path.sh`
4. `docs/user/*.md`
5. any smoke scripts under `scripts/` that invoke the binary directly

Required changes:

1. Build a binary named `lab`.
2. Keep the Cargo package name `lab-cli` only if needed for workspace stability.
3. Remove docs that instruct users to run `lab-cli`.
4. Update scripts to prefer `lab`, then the local built binary `rust/target/release/lab`.
5. Do not add an alias instruction as the primary path.

Acceptance:

```bash
cargo build --manifest-path rust/Cargo.toml --bin lab --release
rust/target/release/lab --help
rg -n "lab-cli|target/release/lab-cli" docs/user scripts/verify-docs-golden-path.sh scripts/lab-fresh.sh
```

The final `rg` may mention the Cargo package directory only when watching `rust/crates/lab-cli`; it must not describe a user-facing command.

### 6.7 Preflight replaces describe in the golden path

Files:

1. `docs/user/quickstart.md`
2. `scripts/verify-docs-golden-path.sh`
3. `rust/crates/lab-cli/src/main.rs` only if adding `preflight --explain`

Required changes:

1. Remove `describe` from the quickstart golden path.
2. Make `preflight` the single pre-run validation step.
3. If users still need package introspection, document `describe` outside the golden path as optional inspection, or add `preflight --explain`.
4. Do not keep both as mandatory one-liners.

Acceptance:

```bash
rg -n "describe" docs/user/quickstart.md scripts/verify-docs-golden-path.sh
```

No required quickstart step may depend on `describe`.

## 7. Required Docs Changes

### 7.1 Bring-your-own-agent walkthrough

Add:

1. `docs/user/bring-your-own-agent.md`

The walkthrough must include:

1. a tiny Python agent artifact
2. `python:3.11-slim` runtime image
3. one provider-backed call using an env var such as `ANTHROPIC_API_KEY`
4. reading `AGENTLAB_TRIAL_INPUT_PATH`
5. writing the runner result contract to `AGENTLAB_RESULT_PATH`
6. a minimal `experiment.yaml`
7. commands: `lab build`, `lab preflight`, `lab run`, `lab views`

Use a clear note that the agent is responsible for mapping `trial_input_v1.task` into its native request shape.

### 7.2 Variant plan example

Files:

1. `docs/user/index.md`
2. `docs/user/what-you-provide.md`

Add one concrete model A/B example:

```yaml
baseline:
  variant_id: codex
  bindings:
    model: gpt-5.3-codex

variant_plan:
  - variant_id: glm
    bindings:
      model: glm-5
```

Explain that `$model` placeholders in `runtime.agent_runtime.command` and `runtime.agent_runtime.env` are resolved per variant.

### 7.3 Schema links

Files:

1. `docs/user/index.md`
2. `docs/user/task-rows.md`
3. `docs/user/agent-runtime-contract.md`
4. `docs/user/graders-and-mappers.md`
5. `docs/user/what-you-provide.md`

Link at least:

1. `schemas/task_row_v1.jsonschema`
2. `schemas/trial_input_v1.jsonschema`
3. agent-authored result schema, now removed in favor of arbitrary JSON responses
4. `schemas/grader_input_v1.jsonschema`
5. `schemas/trial_conclusion_v1.jsonschema`

### 7.4 Cross-run analysis

Files:

1. `docs/user/inspecting-results.md`

Minimum patch:

1. document `.lab/runs/<run_id>/run.sqlite`
2. show DuckDB or SQLite `ATTACH` examples across two run DBs
3. document current `lab trend` behavior if it already satisfies cross-run comparison

Do not claim `lab query --all` exists unless implementing it in this patch.

## 8. Obsolete Code To Delete Or Quarantine

Delete:

1. `normalize_task_prompt_aliases`
2. `AgentRuntimeIoConfig`
3. `AgentRuntimeConfig.io`
4. `resolve_runtime_agent_command` test-only IO append block
5. docs that tell users to use `lab-cli`
6. required quickstart `describe` step

Quarantine or rename:

1. internal persistence functions named `legacy_*` only if they cannot be removed
2. tests whose only purpose is old public authoring compatibility

Reject, do not translate:

1. `runtime.agent`
2. `runtime.sandbox`
3. `runtime.policy`
4. `runtime.agent_runtime.io`
5. `runtime.agent_runtime.launch`
6. `runtime.agent_runtime.env_from_host`
7. `runtime.agent_runtime.binding_args`
8. `runtime.dependencies.*`
9. `benchmark.*.support_files`

## 9. Tests

Required targeted tests:

1. trial input task pass-through
2. unsupported `dataset.provider`
3. unsupported runtime IO remapping field
4. grader env receives `--env`
5. separate grader network behavior
6. docs golden path script uses `lab`, `build`, `preflight`, `run`, not mandatory `describe`
7. schema links exist and point to files

Suggested commands:

```bash
cargo test -p lab-runner build_trial_input_preserves_task_payload_without_alias_mapping --quiet
cargo test -p lab-runner dataset_provider --quiet
cargo test -p lab-runner grader_env --quiet
cargo test -p lab-runner separate_grader_network --quiet
scripts/verify-docs-golden-path.sh
rg -n "normalize_task_prompt_aliases|AgentRuntimeIoConfig|input_arg|output_arg|was removed|hard cut|Milestone" rust/crates/lab-runner/src docs/user
```

## 10. Completion Criteria

The patch is complete only when:

1. A new user can follow `docs/user/` with the `lab` command only.
2. The docs include a real BYO-agent example.
3. The docs include a model A/B variant example.
4. Every named schema in user docs links to a real file.
5. `trial_input_v1.task` is pass-through.
6. Unsupported public fields are rejected, not translated.
7. The quickstart has one validation gate: `preflight`.
8. Grader env/network behavior is documented and tested.
9. No user-facing hard-cutover or legacy wording remains.
10. No broad wishlist feature is snuck into this patch.
