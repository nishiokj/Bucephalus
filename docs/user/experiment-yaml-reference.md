# Experiment YAML Reference

This is the canonical authoring reference for v1 `experiment.yaml`. Authoring uses `matrix.cases`, `stages`, `ephemerals`, and `externals`; packaging normalizes those names to the current internal runtime shape. Mapping keys must be unique at every level; duplicate YAML keys are rejected instead of silently taking the last value.

The editor-facing schema is
[`schemas/experiment_authoring_v1.jsonschema`](../../schemas/experiment_authoring_v1.jsonschema).
Use it for authoring files; sealed packages use the separate resolved
experiment schema.

Validate an authoring file directly with:

```bash
bucephalus schema-validate
```

Run static package checks without preflight or smoke execution with:

```bash
bucephalus lint
```

## Minimal Shape

```yaml
experiment:
  id: smoke_eval
  mode: answer

matrix:
  cases:
    path: cases.jsonl

stages:
  case:
    workspace:
      source: container_image
      image: { from: case_row }
      workdir: { from: case_row }
  agent:
    image: ghcr.io/acme/agent:latest
    command: ["agent", "run", "--model", "gpt-5.5"]
    outputs:
      patch:
        capture: { type: workspace_diff, format: unified_diff }

metrics: []
```

Only this v1 shape is accepted by package authoring and validation.

Do not declare a top-level `version`; `experiment.id` identifies the
experiment, and the schema file/version is selected by the CLI command.

`experiment.mode` is authoring-only evaluation intent. Supported values are
`answer` and `patch`; package build removes the field after writing explicit
outputs, metrics, and grading contracts.

Optional `experiment.description`, `experiment.owner`, and `experiment.tags`
must be non-empty when present; tags must be unique.

`stages.agent.command` is how Bucephalus invokes the agent today. The runner
launches that argv, injects runner-owned input/output paths, and can ingest
command-agent event streams. Do not declare `stages.agent.protocol`; command
invocation is implied by `stages.agent.command`.

The canonical agent `result` output is added by default. Use
`stages.agent.outputs` only for additional captures such as patches, stdout
logs, or workspace diffs.

Declared output captures are closed by type:

| Capture type | Required fields | Optional fields |
| --- | --- | --- |
| `file` | `path`, `format` (`json`, `text`, or `bytes`) | `required` |
| `result_json` | `path` | `field`, `required` |
| `workspace_diff` | `format: unified_diff` | none |

For authoring YAML, omitted `required` on `file` and `result_json` captures
defaults to `true`; build writes the boolean into sealed packages. Set it to
`false` only for diagnostic files that may legitimately be absent.

`workspace_diff` paths are runner-owned, and `field` selectors are only valid
on `result_json` captures.

`traces.source` is separate from invocation. Omit `traces` for runner lifecycle
events only. Set `source: protocol` when you want Buc to use the command-agent
trace channel. That creates a runner-owned JSONL event path and injects
`BUCEPHALUS_TRAJECTORY_PATH`. Your agent must append JSONL there; Buc does not
guess or scrape arbitrary trace files.
Command arguments that need the path use the declared event sink placeholder
`__BUCEPHALUS_EVENT_PATH_<id>__`; the generic
`__BUCEPHALUS_TRAJECTORY_PATH__` command placeholder is rejected.
Top-level `traces` is authoring-only; the build lowers it into
`trial_runtime.agent.events` and the sealed package does not keep `/traces`.

`traces.retain` accepts `never`, `on_failure`, or `always`. In the current
command-agent implementation, `always` retains the raw JSONL file; `never` and
`on_failure` both ingest events without retaining the raw file yet.

Explicit `stages.agent.events` entries are closed declarations for the same
runner-owned event path. The current command runtime supports one JSONL sink
with `id`; do not declare `path`. `format` and `mode` default to `jsonl`,
`ingest` defaults to `true`, and `retain_raw` defaults to `false` in authoring
YAML. Build writes all four fields explicitly into sealed packages. Use
`traces.source: protocol` when you want the default command-agent event sink
without writing the lowered event declaration yourself.

Do not declare `stages.agent.telemetry`; telemetry wiring is an internal
resolved-package field. In authoring YAML, use `traces.source: protocol` or an
explicit `stages.agent.events` sink.

`stages.agent.output_mounts` entries declare directories under
`/bucephalus/out`. Each item requires `id` and a relative `path`; optional
`env` injects the container path. `kind` defaults to `directory` and `persist`
defaults to `true` in authoring YAML, and build writes both fields explicitly
into sealed packages. Set `persist: false` only for scratch directories that do
not belong in trial outputs. Output mount `id`, `path`, and `env` values must
each be unique within the agent stage.

`evaluation.policy` is optional and closed. It controls task-level evaluation
semantics with `task_model`, `scoring_lifecycle`, `chain_failure_policy`, and
`required_evidence_classes`. Grader execution still lives under
`stages.grader`; do not put grader runtime config under `evaluation`.

`runtime.compute.backend` selects where trials execute. Supported values are `local-docker` and `modal`; CLI `--executor` can override this for a run without changing the package.

`stages.case.interface` owns which case resource block is valid. In authoring
YAML, omit it when the resource block is unambiguous:

| Interface | Resource block |
| --- | --- |
| `input_only` | no `files` or `workspace` |
| `readonly_files` | `files` with `source` of `files` or `archive`, plus `path` and `mount_path` |
| `writable_workspace` | `workspace` with `source` and optional `path`, `repo`, `rev`, `image`, or `workdir` |

Do not declare both `files` and `workspace`. If you declare `interface`
explicitly, it must match the selected resource block because the interface
selects exactly one case materialization mode.

`extra_outputs` is an optional declared artifact list for files or directories
that are produced under the trial output surface and should appear in trial
summaries. Each item requires `id`, `source_path`, and `summary_path`; both
paths are relative and may not escape with `..`.

```yaml
extra_outputs:
  - id: host_eval
    source_path: host_eval
    summary_path: grader/host_eval
```

Per-variant `overrides` are closed stage patches. Use the same public stage
nouns as the top-level `stages` block: `case`, `agent`, `execution`, and
`grader`. The build step lowers `overrides.case` into the sealed runtime patch.
Do not put `policy`, `runtime`, `task`, `trial_runtime`, or other resolved
package internals under variant overrides.

## Defaults

Authoring YAML can omit local runtime plumbing. The build step writes these
explicit defaults into the sealed package:

| Field | Default |
| --- | --- |
| `experiment.name` | `experiment.id` |
| `runtime.compute.backend` | `local-docker` |
| `runtime.network.default` | exclusive authoring shorthand for setting both `task_sandbox` and `agent` to the same value; accepts `none`, `full`, or `allowlist_enforced`; lowered away |
| `runtime.network.task_sandbox` | `none` |
| `runtime.network.agent` | `none`; also accepts agent-only `llm_egress` |
| omitted `matrix.variants` | one `baseline` variant |
| `matrix.variants[].baseline` | single declared variant defaults to `true` when omitted; multiple variants must mark exactly one `baseline: true`; all others are sealed as `false` |
| `matrix.variants[].config` | `{}` |
| empty `stages.case` | `interface: input_only` |
| `stages.case.files` | `interface: readonly_files` |
| `stages.case.workspace` | `interface: writable_workspace` |
| name-only `runtime.secrets[]` | `from: env` |
| mounted `runtime.secrets[]` | `from: file`; `credential_cache` must be paired with `mount.target` |
| `matrix.cases.source` | `file` when `matrix.cases.path` is present |
| `matrix.repeats` | `1` |
| `metrics[].required` | `true` |
| `metrics[].primary` | `false`, except one declared metric defaults to `true` |
| `scheduling.max_concurrency` | `1` |
| `scheduling.random_seed` | `1` |
| `scheduling.comparison: paired` | exclusive authoring shorthand lowered to `policy.policies.scheduling: paired_interleaved` |
| `policy.timeout_ms` | `600000` |
| `policy.sanitization_profile` | `standard_runtime` |
| `policy.task_sandbox.hardening` | `no_new_privileges: true`, `drop_all_caps: true` |
| `policy.policies.scheduling` | `variant_sequential`, or `paired_interleaved` when lowered from `scheduling.comparison: paired` |
| `policy.policies.state` | `isolate_per_trial` |
| `policy.policies.retry` | `max_attempts: 1`, `retry_on: []` |
| `policy.policies.pruning` | `max_consecutive_failures: 0` meaning no pruning limit |
| `policy.policies.concurrency` | `require_chain_lease: true`; omit `max_in_flight_per_variant` for no per-variant cap |
| `evaluation.policy` | `task_model: independent`, `scoring_lifecycle: predict_then_score`, `chain_failure_policy: continue_with_flag`, `required_evidence_classes: []` |
| `trial_runtime.agent.integration_level` | sealed default `cli_basic`, or `cli_events` when an agent event sink is declared |
| omitted `stages.grader` | `strategy: none` |
| active `stages.grader.inputs` | `{}` |

`policy.task_sandbox` is a closed sandbox policy object. It accepts optional
`resources.cpu_count`, optional `resources.memory_mb`, and hardening booleans
`hardening.no_new_privileges` and `hardening.drop_all_caps`; both hardening
booleans default to `true` and are written into sealed packages. Network mode
belongs under `runtime.network.task_sandbox`, not `policy.task_sandbox`.

`policy.validity` is also closed. The supported booleans are
`fail_on_state_leak` and `fail_on_profile_invariant_violation`.

`stages.execution.agent_site` is also inferred when the runtime boundary is
unambiguous: `stages.agent.image` implies `agent_container`, a writable
container-image workspace without an agent image implies `task_runtime`, and
`stages.case.interface: input_only` without an agent image implies `host`.
Declare it explicitly when you need another supported boundary or when those
inference rules do not apply, such as read-only file cases without an agent
image.

Resolved packages must carry the explicit `experiment.name`,
`runtime.compute.backend`, `runtime.network.task_sandbox`,
`runtime.network.agent`, `matrix.variants[].baseline`,
`matrix.variants[].config`, `scheduling.max_concurrency`, `scheduling.random_seed`, and
metric `primary`/`required`, file-like `capture.required`, event
`ingest`/`retain_raw`, and output-mount `persist` values written by the
authoring build step. Declared grader inputs must also carry explicit
`required` booleans; authoring YAML defaults them to `true` and uses `false`
only for optional grader context. Resolved packages must also carry the
normalized `policy.sanitization_profile`,
`policy.policies`, `evaluation.policy`, and `trial_runtime.agent.integration_level`
values. Active graders must carry an explicit `inputs` map, even when empty,
and at least one declared `outputs` entry; direct resolved configs cannot rely
on runner-side fallbacks for those fields.

If `stages.grader` declares any command, outputs, inputs, ephemerals, or
strategy-specific config, it must also declare `strategy`; only an omitted
grader defaults to `none`. An explicit empty `stages.grader: {}` is rejected.

Use `scheduling.comparison: paired` only when you want the shorthand for
paired-interleaved scheduling. Do not combine it with
`policy.policies.scheduling`; write the concrete policy directly for any other
run ordering.

These defaults do not grant external access. Declare `runtime.network.agent`,
`runtime.network.task_sandbox`, `runtime.secrets`, or `externals` when an
experiment needs credentials or egress.

## Ephemerals

`ephemerals` is optional. Each top-level ephemeral defines a per-trial resource whose lifecycle the runner owns, but which is not a link in the stage chain. A stage attaches an ephemeral by listing its id under `stages.agent.ephemerals` or `stages.grader.ephemerals`.

```yaml
ephemerals:
  service-id:
    image: ghcr.io/acme/service:latest
    lifecycle: per-trial
    command: ["service", "--port", "8080"] # optional
    workdir: /srv/service                  # optional
    env: { LOG_LEVEL: info }               # optional, for the service
    expose: { SERVICE_URL: "http://service-id:8080" } # optional, for attached stages

stages:
  agent:
    ephemerals: [service-id]
```

Ids use portable DNS-label syntax because the id is also the runtime hostname alias: lowercase letters, numbers, and `-`, starting and ending with a letter or number. Only `lifecycle: per-trial` is supported. Local Docker supports ephemerals; Modal currently rejects them.
Each stage-level `ephemerals` list must be duplicate-free, and attached
ephemerals must expose unique env names within that stage.

## Legacy Files

Authoring files should not use resolved package internals such as `matrix.tasks`, `trial_runtime`, or `sidecars`. Use `matrix.cases`, `stages`, `ephemerals`, and `externals`; the build step lowers them into the sealed package contract.
Inline `knobs` are not part of `experiment.yaml`; use an external
`knob_manifest_v1` file with `experiment_overrides_v1` when applying
`--overrides`. The default manifest location is `.lab/knobs/manifest.json`;
an overrides file may set `manifest_path`, but it must be a project-relative
path with no `..` traversal, absolute host path, blank value, or surrounding
whitespace. Override files must contain at least one value. Override value keys
are knob ids; they must not be blank or padded with whitespace.

Every knob id and `json_pointer` target in the manifest must be unique. Knob
manifests must declare at least one knob. Knob ids must not be blank or padded
with whitespace. Knob pointers must target a concrete experiment field: root
pointers, empty path segments, and malformed JSON Pointer escapes are rejected.
Numeric pointer segments must be canonical and must not use leading zeros.
Applying an override requires the target field to already exist and to have the
same type declared by the knob.

Knob manifests are validated as contracts, not hints. Allowed options must be
unique, match the knob type, and respect any numeric `minimum`, `maximum`, and
`step`. Integer knobs require integer numeric controls. `autotune` declarations
are not accepted until the build/package pipeline implements them; materialize
chosen values in `experiment_overrides_v1` instead.
