# Runtime Control Plane Boundary

This is the Path 3 implementation contract for Cloud runner execution.

The goal is that runner VMs behave as API clients, not database clients. The
Cloud API owns durable state: run queue claims, leases, attempts, events,
package access, runner instance state, and runtime snapshots. Runners
may hold worker API credentials and provider-scoped attempt secret access, but
they must not require Postgres credentials.

## Runner API Boundary

Runner daemons should use the Cloud API for:

- registering runner instances
- heartbeating runner instances
- claiming work
- heartbeating attempts
- downloading sealed packages
- appending worker/Core lifecycle events
- completing or failing attempts
- reporting unhealthy or offline runner state

Queue wakeup is currently polling plus API lease expiration. Do not reintroduce
direct Postgres `LISTEN/NOTIFY` from runner VMs. A future wake channel may be
SSE, WebSocket, Pub/Sub, SQS, or another provider queue, but it should still be
mediated by the control plane rather than database credentials on the runner.

## Core Runtime Persistence Boundary

The worker must not pass `DATABASE_URL`, `BUCEPHALUS_WORKER_DATABASE_URL`,
`BUCEPHALUS_RUN_STORE`, `BUCEPHALUS_RUN_STORE_URL`, or
`BUCEPHALUS_RUN_STORE_SCHEMA` to Core. Those variables couple runner execution
to direct database access.

After Core exits, the worker uploads a bounded snapshot of Core's known runtime
JSON mirrors through the Cloud API before attempt cleanup. Snapshot events use
`worker.runtime.snapshot` and include:

- `core_run_id` and `run_dir_name`, where `run_dir_name` is relative and never an
  absolute worker filesystem path
- `runtime_values` for `run_control_v2`, `schedule_progress_v2`, and
  `run_session_state_v1` when those JSON files exist
- bounded `trial_summaries` read from Core trial `summary.json` files, with
  bounded `contract_trace` objects from `runner/contract_trace.json` when
  present
- bounded `trial_events` read from Core trial `agent/events.jsonl` when present
- bounded `evidence_records` read from Core run `evidence/evidence_records.jsonl`
  when present
- `omitted` relative labels for known files or collections that were too large
  or truncated

The API runtime repository reads these snapshots from `cloud.run_events` beside
any older normalized Core runtime tables. It exposes snapshot-backed
`run_control_v2`, `schedule_progress_v2`, trial result rows, metric
observations, contract stage rows, runtime event rows, and attempt-object rows
without giving runner VMs database credentials. The current invariant is that
raw runtime mirrors and user-facing results are durably reported through the
control plane before the attempt workspace is removed.

## Attempt Secret Boundary

Run `secret_refs` are declarations, not local filenames. If a run declares
secrets, the worker must use an attempt-scoped resolver configured by
`BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON`.

The API validates secret ids and refs when the run is created. Invalid
declarations must fail before they become queued work; they should not wait
until a runner has claimed the run.

The resolver command speaks JSON over stdin/stdout:

Input:

```json
{
  "attempt_id": "attempt-id",
  "run_id": "run-id",
  "output_dir": "/attempt/workspace/secrets",
  "secrets": [
    { "id": "OPENAI_API_KEY", "ref": "provider/secret/ref" }
  ]
}
```

Output:

```json
{
  "files": {
    "OPENAI_API_KEY": "OPENAI_API_KEY.secret"
  }
}
```

The resolver owns provider-native secret retrieval. It must write secret files
under `output_dir` and return paths relative to that directory. The worker
rejects absolute paths, traversal outside `output_dir`, missing declared
secrets, and undeclared secret ids.

`bucephalus-cloud-secret-resolver` is the default resolver entrypoint for
provider-backed runners. It currently supports:

- `gcp-secret-manager://projects/<project>/secrets/<secret>/versions/<version>`
  using `gcloud secrets versions access`
- `aws-secrets-manager://<secret-id>` using
  `aws secretsmanager get-secret-value`
- `env:<NAME>` only when `BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV=true`; this is a
  local development adapter, not a production secret boundary

Runner images should include the provider CLI they intend to use, and runner VM
identity should grant only the secrets needed by that pool or attempt class.
Set `BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON` to a command array such as:

```json
["bucephalus-cloud-secret-resolver"]
```

Provider CLI names can be overridden with
`BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD` or
`BUCEPHALUS_SECRET_RESOLVER_AWS_CMD` when images install them in non-default
locations.

The attempt workspace cleanup removes materialized secrets along with package
materialization and run outputs. Logs and worker events must use secret ids and
refs only; they must not contain secret values.

## Runtime Resource Boundary

Run requirements are declarations used by the control plane and provider
adapter. They should describe CPU, memory, disk, architecture, isolation,
container image refs, registry needs, network perimeter, sidecars, accelerators,
and secret refs. A runner may enforce local limits where it can, but missing
provider policy should block provisioning or claiming rather than silently
falling back to ambient VM access.

Cloud run requirements now include:

- `secret_ids`: ids only, never plaintext. A run with any secret id requires a
  runner with the `secret_resolver` resource.
- `network_perimeter`: the accepted Cloud shape is currently default/task/agent
  network mode `none` plus explicit `egress_hosts`. Ambient modes such as
  `agent: full` are rejected for Cloud runs. A run with egress hosts requires a
  runner with the `network_perimeter` resource.
- `sidecars`: declared sidecar ids or services. A sidecar `redis` requires a
  runner/provider resource named `sidecar:redis`.
- `accelerators`: declared accelerator classes. An accelerator `nvidia-l4`
  requires a runner/provider resource named `accelerator:nvidia-l4`.

The important invariant is that these needs are visible before a runner claims
work. If the provider cannot satisfy the policy, the run should remain
unclaimed or fail provisioning explicitly; it should not inherit whatever
network, secrets, or hardware happen to exist on the VM.

Workers advertise `network_perimeter` only when
`BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON` is configured. Before Core starts,
the worker invokes that command with JSON over stdin:

```json
{
  "attempt_id": "attempt-id",
  "run_id": "run-id",
  "runner_instance_id": "runner-instance-id",
  "worker_id": "worker-id",
  "workspace_dir": "/attempt/workspace",
  "run_root_dir": "/attempt/workspace/run-root",
  "network_perimeter": {
    "default": "none",
    "task_sandbox": "none",
    "agent": "none",
    "egress_hosts": ["api.openai.com"]
  },
  "egress_hosts": ["api.openai.com"]
}
```

The command is provider/image-owned: it may configure cloud firewall policy,
iptables/nftables, Docker daemon policy, sidecar proxy rules, or another
enforcement primitive appropriate for that runner class. If the command is
missing or exits non-zero, the attempt fails before Core starts. A boolean
"supports networking" flag is not enough.

## Runner Capacity Boundary

Runner capacity is a control-plane concern until a provider adapter accepts a
provision request. The pool controller reads queued run requirements, current
runner instances, and open provision requests from the Cloud database. It should
only request capacity when no existing claimable runner or open request can
satisfy the run's executor, resources, VM shape, and isolation requirements.

The provision command boundary receives the run requirements and worker startup
environment needed for the eventual runner daemon to register through the Cloud
API. The command must return provider identity metadata, not local SSH state or
handwritten VM assumptions. GCP, AWS, or another provider implementation may
create VMs, managed instance group capacity, batch jobs, or another compute
primitive, but that implementation belongs to the cloud substrate/image path.

The runtime invariant is that capacity decisions are made from declared
requirements and observed runner registrations. If the provider adapter cannot
provision matching capacity, the provision request should fail visibly; the
system should not mutate run requirements or launch a less-capable ambient VM.

## Current Explicit Gaps

- Provider-native secret resolver execution now exists for GCP Secret Manager
  and AWS Secrets Manager, but cloud IAM policy binding and per-pool secret
  grants must still be supplied by the infrastructure path.
- Runtime network perimeter enforcement has a worker command boundary, but the
  provider-specific policy implementations still belong to runner image and
  infrastructure work.
- Provider capacity provisioning after the retired shim cleanup is owned by the
  cloud substrate/deploy path.

These are acceptable blockers. Do not fill them with local VM shims.
