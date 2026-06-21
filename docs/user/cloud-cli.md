# Hosted Cloud CLI

`buc` is the hosted Bucephalus Cloud product CLI. It talks to Cloud APIs only.
It does not run local Core builds, start local runners, or manage Cloud operator
pools. Hosted authoring builds run the version-matched Core binary inside the
Cloud API environment.

## Current Boundary

`buc build` accepts either authoring YAML or a sealed package:

```bash
buc build experiment.yaml
buc build experiments/peter/experiment.yaml
buc build .bucephalus-package
```

For YAML, `buc` requires a `bucephalus.project.yaml` or
`bucephalus.project.yml` file above the entrypoint. That manifest is the source
of truth for the uploaded authoring context:
project id, package source, declared entrypoints, include/exclude rules, and the
hosted Cloud target. The API runs bundled Core in an isolated workspace, imports
the produced sealed package, and checks the package against the hosted Cloud
target. For package directories or archives, `buc` uploads/imports the sealed
package directly and runs the same hosted readiness checks.

The YAML authoring context is a Cloud build input, not a raw directory sync.
`buc` excludes local generated and credential material such as `.env`, `.env.*`,
`.npmrc`, `.pypirc`, `.netrc`, `.ssh`, `.aws`, `.docker`, `.config/gcloud`,
`node_modules`, and `target` before upload. The hosted API rejects the same
paths if a context archive is crafted outside the CLI. Use hosted secrets with
`buc secrets put` and pass `bucephalus://NAME` refs to `doctor`/`run`; do not
upload local credential files as build inputs.

Minimal `bucephalus.project.yaml`:

```yaml
schema_version: bucephalus_project_v1
project:
  id: my_evals
package_sources:
  default:
    root: .
    entrypoints:
      - experiment.yaml
      - experiments/peter/experiment.yaml
    include:
      - experiment.yaml
      - cases.jsonl
      - experiments/peter/**
      - shared/**
    exclude:
      - generated/**
targets:
  hosted_cloud: {}
```

For nested experiments, keep the manifest at the shared project root and list
the nested YAML as an entrypoint. Do not use command-line root overrides; the
manifest is the build boundary.

```bash
buc secrets put NAME --from-env NAME
buc doctor <package-digest> --secret-ref NAME=bucephalus://NAME
buc run <package-digest> --secret-ref NAME=bucephalus://NAME
```

## Setup

Log in once and persist the hosted API URL:

```bash
buc login
```

Hosted login opens a browser OAuth flow and listens on a local loopback
callback. No hosted user should need to provide `--api-url`; that option is only
for development, staging, or self-hosted Cloud. The hosted API publishes the CLI
OAuth client, scope, and server-side authorization-code exchange path from
`/v1/auth/config`.

`buc` sends the browser authorization code and PKCE verifier back to the hosted
API, which performs the OAuth token exchange with its own server-side secret and
returns a Bucephalus session token. The CLI never needs the OAuth client secret
and never asks hosted users for issuer, audience, or internal API endpoint
details.

`buc` then reads the shared Cloud profile and cached auth files from
`BUCEPHALUS_HOME`: `cloud.json`, `auth/cloud_user_token`, and
`auth/cloud_user_token.json`. If a self-hosted OAuth cache includes
`auth/cloud_refresh_token`, `buc` can refresh the access token before making
Cloud API calls.

Automation can also pass a token per command:

```bash
buc --user-token <token> health
```

`--api-url` is for development, staging, and self-hosted Cloud overrides. The
installed hosted product default is baked into the release from
`BUCEPHALUS_HOSTED_API_URL`.

Environment variables:

| Variable | Meaning |
| --- | --- |
| `BUCEPHALUS_CLOUD_API_URL` | Hosted API base URL. |
| `BUCEPHALUS_CLOUD_USER_TOKEN` | OAuth/API bearer token override. |
| `BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY` | API-side build provenance policy: `warn` for local/dev, `enforce` for managed production. |

## Commands

Use the top-level workflow commands for day-to-day work:

```bash
buc login
buc auth status
buc health
buc author canonicalize experiment.yaml
buc author resolve experiment.yaml
buc author validate experiment.yaml --validation-level launch_hint
buc build <experiment.yaml-or-package>
buc packages list
buc inspect <package-digest>
buc secrets put NAME --from-env NAME
buc secrets list
buc doctor <package-digest> --secret-ref NAME=bucephalus://NAME
buc run <package-digest> --secret-ref NAME=bucephalus://NAME
buc runs list
buc runs get <run-id>
buc runs get <run-id> Trial
buc runs get <run-id> Trial --wide
buc runs get <run-id> Trial/<trial-name>
buc runs wait <run-id> Trial/<trial-name> --for condition=Ready
buc runs events <run-id>
buc runs audit <run-id>
buc runs top <run-id> --kind Trial
buc runs can-i <run-id> port-forward RunnerInstance/<runner-name>
buc runs logs <run-id> Trial/<trial-name> --stream stdout --metadata-out trial.log.metadata.json
buc runs port-forward <run-id> Trial/<trial-name> --target-port 8080 --local-port 18080 --attach --resource-version <reviewed-version>
buc runs exec <run-id> Trial/<trial-name> --resource-version <reviewed-version> -- python -V
buc runs content <run-id> TrialArtifact/<artifact-name> --out result.json --metadata-out result.metadata.json
buc logout
```

Long-form noun commands are equivalent:

```bash
buc drafts canonicalize <draft.yaml-or-json>
buc drafts resolve <draft.yaml-or-json>
buc drafts validate <draft.yaml-or-json> --validation-level package
buc drafts suggest <draft.yaml-or-json> --target variant
buc drafts diff <left-draft.yaml> <right-draft.yaml>
buc packages list
buc packages upload <package-dir-or-package.tgz>
buc packages inspect <package-digest>
buc secrets list
buc secrets put <name> --from-env <env-var>
buc secrets delete <name>
buc experiments build <experiment.yaml-or-package>
buc experiments doctor <package-digest> --secret-ref NAME=bucephalus://NAME
buc runs list
buc runs create <package-digest> --secret-ref NAME=bucephalus://NAME
buc runs get <run-id>
buc runs get <run-id> Trial
buc runs get <run-id> Trial/<trial-name>
buc runs api-resources <run-id>
buc runs explain <run-id> TrialContainer
buc runs tree <run-id> --kind Trial,TrialContainer
buc runs get <run-id> Trial --field-selector status.phase=running
buc runs describe <run-id> Trial/<trial-name>
buc runs metrics <run-id> --kind Trial
buc runs top <run-id> --kind Trial
buc runs events <run-id>
buc runs audit <run-id>
buc runs logs <run-id> Trial/<trial-name> --stream stdout --metadata-out trial.log.metadata.json
buc runs port-forward <run-id> RunnerInstance/<runner-name> --target-port 8080 --local-port 18080 --attach --resource-version <reviewed-version>
buc runs exec <run-id> RunnerInstance/<runner-name> --resource-version <reviewed-version> -- python -V
buc runs cordon <run-id> RunnerInstance/<runner-name> --reason "maintenance" --resource-version <reviewed-version>
buc runs drain <run-id> RunnerInstance/<runner-name> --reason "maintenance" --resource-version <reviewed-version>
buc runs uncordon <run-id> RunnerInstance/<runner-name> --reason "maintenance complete" --resource-version <reviewed-version>
buc runs complete <run-id> PortForward/<port-forward-name> --reason "done" --resource-version <reviewed-version>
buc runs delete <run-id> PortForward/<port-forward-name> --reason "done" --resource-version <reviewed-version>
```

## End-To-End Hosted Run

1. Validate authoring shape through the hosted authoring API:

   ```bash
   buc author canonicalize experiment.yaml
   buc author resolve experiment.yaml
   buc author validate experiment.yaml
   buc author validate experiment.yaml --validation-level package
   buc author validate experiment.yaml --validation-level launch_hint
   ```

   `authoring` validation catches draft structure and registry reference issues.
   `package` adds checks for packaging inputs such as case sources, variant
   identity, relative build-context paths, and secret mount shape. `launch_hint`
   adds non-fatal hosted-run guidance, such as required `--secret-ref` values,
   network capability hints, and local image rewrite warnings. These commands do
   not upload the authoring context or prove file existence; hosted build is the
   first step that sees the complete upload boundary.

2. Build for hosted Cloud:

   ```bash
   buc build experiment.yaml
   ```

   For nested experiments that reference shared repository files, declare the
   upload boundary in `bucephalus.project.yaml`:

   ```bash
   buc build experiments/peter/experiment.yaml
   ```

   The command returns a `package_digest`. If authoring build, package import,
   or hosted readiness fails, `buc` exits non-zero and prints the failed stage.
   When hosted readiness is `cloud_runnable`, the summary prints concrete
   follow-up commands: hosted secret upload commands when needed, `buc doctor
   <package-digest>`, and `buc run <package-digest>` with the matching
   `--secret-ref NAME=bucephalus://NAME` arguments.

3. Inspect required secrets:

   ```bash
   buc inspect <package-digest>
   ```

4. Upload hosted secrets for any required secret names:

   ```bash
   buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY
   ```

   Other non-leaky value sources are supported:

   ```bash
   buc secrets put GEMINI_API_KEY --value-file ./gemini.key
   printf '%s' "$GEMINI_API_KEY" | buc secrets put GEMINI_API_KEY --stdin
   ```

   The CLI and API never print secret plaintext or backing provider refs. The
   returned ref is `bucephalus://GEMINI_API_KEY`.

5. Doctor the exact hosted run inputs:

   ```bash
   buc doctor <package-digest> \
     --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY
   ```

   Doctor checks package acceptance, secret refs, image portability, network
   requirements, architecture/resources, and active runner-pool schedulability.

6. Queue the run:

   ```bash
   buc run <package-digest> \
     --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY
   ```

7. Inspect the hosted runtime:

   ```bash
   buc runs get <run-id>
   buc runs api-resources <run-id>
   buc runs get <run-id> Trial
   buc runs get <run-id> Trial/<trial-name>
   buc runs wait <run-id> Trial/<trial-name> --for condition=Ready
   buc runs get <run-id> TrialArtifact --field-selector status.content_available=true
   buc runs top <run-id> --kind Trial
   buc runs events <run-id>
   buc runs audit <run-id>
   buc runs can-i <run-id> exec Trial/<trial-name>
   buc runs logs <run-id> Trial/<trial-name> --stream stdout --metadata-out trial.log.metadata.json
   buc runs port-forward <run-id> Trial/<trial-name> --target-port 8080 --no-wait --resource-version <reviewed-version>
   buc runs exec <run-id> Trial/<trial-name> --resource-version <reviewed-version> -- python -V
   buc runs content <run-id> TrialArtifact/<artifact-name> --out result.json --metadata-out result.metadata.json
   ```

## Runtime Resources and Low-Level Operations

Hosted runs expose their execution state as runtime resources, not one-off
compatibility routes. Start with discovery, then work against concrete
`Kind/name` objects:

```bash
buc runs api-resources <run-id>
buc runs explain <run-id> TrialContainer
buc runs tree <run-id> --kind RunnerInstance,RunnerAttempt,Trial,TrialContainer
buc runs get <run-id> RunnerInstance,RunnerAttempt,Trial,TrialContainer --wide
buc runs get <run-id> Trial -o name
buc runs get <run-id> --category runner --wide
buc runs get <run-id> Event --field-selector status.involved=PortForward/<port-forward-name> --wide
buc runs get <run-id> Event --field-selector spec.involved_object.kind=PortForward,spec.involved_object.name=<port-forward-name> --wide
buc runs get <run-id> RunnerInstance/<runner-name>
buc runs status <run-id> RunnerInstance/<runner-name>
buc runs events <run-id> RunnerInstance/<runner-name>
buc runs audit <run-id> RunnerInstance/<runner-name>
buc runs top <run-id> --kind RunnerInstance
```

`buc runs api-resources` prints the generated catalog timestamp, represented
Core run ids, and each kind with count, short names, categories, verbs,
subresources, actions, access verbs, and selector support so operators can pick
the right `--kind` or `--category` before listing resources.
`buc runs get` mirrors the useful part of `kubectl get`: with only a run id it
fetches the Cloud run record, with a kind or selectors it lists runtime
resources, and with `Kind/name` or `KIND NAME` it fetches that raw resource.
Use `--category <name>` to send the same server-owned selector exposed by
`buc runs api-resources`, such as `runner`, `trial`, `access`, or
`access-target`, without memorizing the current concrete kind list. API clients
can use that selector directly with `category=runner` on runtime resource list,
watch, health, metrics, and inspect endpoints.
Add `--wide` on resource lists to render the server-advertised printer columns
instead of the compact summary. Use `--output name` or `-o name` when scripts
need one concrete `Kind/name` ref per line for later `describe`, `logs`,
`can-i`, `delete`, or `wait` commands.
For `Event` resources, `--wide` includes the primary involved runtime object
from `status.involved` plus the event type and row sequence, so access and audit
events can be listed by concrete `Kind/name` without opening raw payload JSON.
The discovery document also advertises concrete Event selectors such as
`spec.involved_object.kind`, `spec.involved_object.name`, `status.involved`,
and `status.involved_uid`; use those advertised paths for tooling instead of
parsing event payloads.
Use `buc runs describe <run-id> Kind/name` or
`buc runs get <run-id> Kind/name --view describe` when you also want related
owner/dependent resources and lifecycle/audit events. Describe output includes
the generated snapshot timestamp and represented Core run ids before related
resources, available operations, and recent related event rows with
`event_resource_version`/cursor metadata. Each advertised operation is printed
with its support decision, verb/subresource/action metadata, whether it requires
a running Cloud run, and command template so CLI users can see the concrete
low-level operation contract before running `can-i` or a mutating command.
Use `buc runs explain <run-id> <kind>` when you need the server-owned contract
for one kind: aliases, selectors, printer columns, paths, actions, and example
commands. The paths section includes low-level subresource endpoint templates
such as `subresource/status`, `subresource/logs`, and `subresource/actions/*`.
Runtime catalog reads are part of the audit stream as
`runtime.api_resources.read`; missing-kind discovery attempts are recorded as
`runtime.api_resources.read.failed` with the requester, selected kind, error
code, HTTP status, and `Run/<run-id>` resource ref.
Field selectors include Kubernetes-style metadata paths such as
`metadata.creationTimestamp`, `metadata.deletionTimestamp`,
`metadata.generation`, and `metadata.resourceVersion` alongside status,
condition, and audit paths.
Use `buc runs tree <run-id>` when you need to see parent/child runtime shape
from `metadata.ownerReferences` before drilling into one object.
`buc runs status <run-id> Kind/name` reads the status subresource and prints the
generated status timestamp, resourceVersion, generation/observedGeneration
freshness, conditions, available actions, deletion timestamp, and audit source
when present; `buc runs wait`
prints that same summary after the predicate is reached. If a wait times out
or the resource reaches a terminal phase first, the error includes the latest
observed phase, reason, waited condition status, resourceVersion, and
generation freshness.

Use `can-i` before a mutating, low-level access, or observability operation
when scripting. It queries the same operation-review subresource the GUI uses
and returns the generated review timestamp, represented Core run ids, and
resource version that can be passed to a later mutation. Human output prints a
reviewed command with the current `--resource-version` materialized when the
operation accepts optimistic concurrency:

```bash
buc runs can-i <run-id> port-forward RunnerInstance/<runner-name>
buc runs can-i <run-id> exec Trial/<trial-name>
buc runs can-i <run-id> top TrialContainer/<container-name>
buc runs can-i <run-id> audit RunnerInstance/<runner-name>
buc runs can-i <run-id> logs/stdout TrialContainer/<container-name>
buc runs can-i <run-id> logs/stderr TrialContainer/<container-name>
buc runs can-i <run-id> content TrialArtifact/<artifact-name>
buc runs can-i <run-id> cordon RunnerInstance/<runner-name>
buc runs can-i <run-id> cancel Exec/<exec-name>
buc runs can-i <run-id> complete PortForward/<port-forward-name>
```

`can-i` exits zero when the operation is currently supported and nonzero when
the server denies it, so scripts can use it as a preflight gate. Reviewed
mutating and low-level access commands include `--resource-version` so the later
request fails safely if the resource changed after review. Operation reviews
are also written to the runtime audit stream as
`runtime.resource.operation.reviewed`; failed review attempts use
`runtime.resource.operation.review.failed` with the selected resource and error
details.

Declared infra resources also carry worker-observed lifecycle evidence.
`ImagePull` resources move past capability-level `Satisfied` to `Pulling`,
`Pulled`, or `Failed` as the runner pre-pulls digest-pinned images before
network policy is applied. These transitions are emitted as
`worker.runtime.image_pull.*` events and keep the image ref tied to the
selected `ImagePull/<name>` resource.
`SecretBinding` resources move past capability-level `Satisfied` to
`Materialized` when the worker reports `worker.runtime.secret_binding.materialized`;
the event names the secret id and resource, never the secret ref or value.
`SidecarRequirement/<sidecar>` moves past capability-level `Satisfied` to
`Checking`, `Available`, or `Failed` from
`worker.runtime.sidecar_requirement.*` events when the worker validates that the
claimed runner actually advertises the required sidecar capability.
`AcceleratorRequirement/<accelerator>` follows the same worker-observed lifecycle
through `worker.runtime.accelerator_requirement.*` events when the worker
validates runner accelerator capability.
`NetworkPerimeter/declared` reports `Applying`, `Applied`, or `Failed` from
`worker.runtime.network_perimeter.*` events after the provider enforcer runs.

Runtime access is resource-scoped. The target can be a `RunnerInstance`,
`RunnerAttempt`, active `Trial`, assigned `ScheduleSlot`, or `TrialContainer`
when the resource advertises `status.access.port_forward` or
`status.access.exec`:

```bash
buc runs port-forward <run-id> Trial/<trial-name> --target-port 8080 --local-port 18080 --attach --resource-version <reviewed-version>
buc runs exec <run-id> Trial/<trial-name> --resource-version <reviewed-version> -- python -V
buc runs logs <run-id> Trial/<trial-name> --stream stdout --tail-lines 200 --metadata-out trial.log.metadata.json
```

`port-forward` creates an audited `PortForward` runtime resource and waits for
the worker to report an active tunnel unless `--no-wait` is set. Add `--attach`
to start a local TCP forward when the active resource reports a supported
provider handle, such as GCE IAP. When the CLI-owned local attach process exits,
the CLI marks the `PortForward` resource `completed` with an audited closeout
reason. If the worker reports a client-reachable endpoint instead, `--attach`
prints the endpoint and leaves the worker-managed `PortForward` active for
explicit cleanup or TTL expiry.
If the tunnel resource ends `failed`, `expired`, or `cancelled`, `buc runs port-forward` exits nonzero after printing
the `PortForward` resource summary. `exec` creates an audited `Exec` resource
and waits for completion unless `--no-wait` is set; when the remote command
exits nonzero, `buc runs exec` exits nonzero after printing the `Exec` resource
summary. Human describe output and the GUI inspector surface closeout commands
for live access resources using the current `metadata.resourceVersion`, so raw
provider attach sessions still have an audited closeout path. Inspect, complete, or cancel
those resources through the same resource API:

```bash
buc runs get <run-id> PortForward,Exec
buc runs describe <run-id> PortForward/<port-forward-name>
buc runs logs <run-id> Exec/<exec-name> --stream stdout --metadata-out exec.log.metadata.json
buc runs complete <run-id> PortForward/<port-forward-name> --reason "local tunnel ended" --resource-version <reviewed-version>
buc runs delete <run-id> Exec/<exec-name> --reason "cancelled by operator" --resource-version <reviewed-version>
buc runs wait <run-id> Exec/<exec-name> --for delete --timeout-seconds 60
```

`describe` output for `PortForward` and `Exec` includes the resolved target
resource and resourceVersion, requester/reason/expiration, runner/attempt/worker
binding, connection mode, raw connection details, and command/output facts. When
a `PortForward` is active with a supported GCE IAP handle, the summary also
prints an `attach_command` line that can be run independently. Worker-managed
active tunnels surface their `client_endpoint` in the same detail output.
`Exec` output facts include stdout/stderr tail byte counts and truncation flags
when the worker had to bound captured command output.
For raw `logs` and `content` downloads, add `--metadata-out FILE` to save the
Cloud response provenance separately from the bytes: Cloud run id, resource
kind/name/resourceVersion, stream, Core run id, trial id, artifact role, object
ref, digest, media type, and byte size. The server also records the read in
runtime audit events as `runtime.resource.logs.read` or
`runtime.resource.content.read` with the requester, resource ref, object ref,
digest, media type, and byte size.
Failed raw reads are also audited as `runtime.resource.logs.read.failed` or
`runtime.resource.content.read.failed` with the selected resource, error code,
HTTP status, and error message.
Normal resource reads are audited too. Server-side `resources`, `watch`,
`describe`, `get`, `status`, `events`, `top`/`metrics`, and metrics-list reads
emit `runtime.resource.list.read`, `runtime.resource.watch.read`,
`runtime.resource.describe.read`, `runtime.resource.get.read`,
`runtime.resource.status.read`, `runtime.resource.events.read`,
`runtime.resource.metrics.read`, or `runtime.resource.metrics.list.read` with
the requester, selected resource or `Run/<run-id>` ref, selector and cursor provenance,
resourceVersion, returned counts, and health/metrics/event summary
metadata. Failure variants keep the same event name plus `.failed` and include
the error code, HTTP status, and message.
The server-discovered `--wide` columns for `PortForward` and `Exec` include the
same target, reviewed target resourceVersion, runner/worker binding, requester,
connection mode, provider tunnel, and expiration fields for scanning access
resources before opening one object. `Exec --wide` columns also expose
stdout/stderr byte totals, retained tail byte counts, and truncation flags so a
resource list can reveal bounded output evidence without dumping the tail text.
Runtime audit summaries for completed Exec resources include the same
stdout/stderr byte totals, retained tail byte counts, and truncation flags
alongside the exit code, so `buc runs audit` can prove what stream evidence was
captured.

`delete` is the Kubernetes-style resource verb; for audited access resources it
is implemented as the same lifecycle transition as `actions/cancel`. The
resource remains inspectable for audit and carries `metadata.deletionTimestamp`
once the delete/cancel is acknowledged, which is also enough for
`wait --for delete` to complete.

Runner VM lifecycle operations use explicit resource verbs:

```bash
buc runs cordon <run-id> RunnerInstance/<runner-name> --reason "maintenance" --resource-version <reviewed-version>
buc runs drain <run-id> RunnerInstance/<runner-name> --reason "maintenance" --resource-version <reviewed-version>
buc runs uncordon <run-id> RunnerInstance/<runner-name> --reason "maintenance complete" --resource-version <reviewed-version>
```

For support handoff, `buc runs inspect <run-id>` fetches a bounded runtime
bundle with API discovery, resource inventory cursors, health summary, metrics
summary, recent event cursors, and log references. Use `--json` to save the full
bundle. Each bundle read emits a `runtime.inspect.bundle.read` audit event with
the requester, `Run/<run-id>` resource ref, applied filters, inventory and event
resource versions, returned counts, and log-reference count. Failed bundle reads emit
`runtime.inspect.bundle.read.failed` with the requester, filters, error code,
HTTP status, and error message when the runtime audit store is reachable.

Every low-level access request, runner lifecycle mutation, and worker-observed
infra lifecycle transition is emitted into the runtime audit stream. Use
`buc runs audit <run-id>` for run-wide operator history, or pass a `Kind/name`
target such as `ImagePull/<image-name>`, `SecretBinding/<secret-id>`,
`SidecarRequirement/<sidecar>`, or `NetworkPerimeter/declared` to scope the
audit to one resource. For
incident review and support handoff, `buc runs events` and `buc runs audit`
also accept repeatable `--event-type` and `--source` filters plus
`--resource-kind`, `--resource-name`, `--trial-id`, `--task-id`, and
`--continue` cursors. Add `--follow` to `buc runs events`, `buc runs audit`,
`buc runs watch`, or `buc runs logs` to keep polling; `--interval-seconds`
controls the poll delay, and `--max-polls` captures a bounded stream for
incident notes or support bundles. Followed logs print only appended bytes, so
sliding `--tail-lines` windows do not duplicate output. Human event output
includes the event `resource_version`, `continue`, and row-sequence cursor
metadata when the API returns it; individual audit lines surface row, event
type, source, actor, resource/access/target refs, runner/worker binding,
reviewed `resource_version_precondition`, reason, and status transition when
present. Human watch output includes the collection `resource_version`, copyable
`known_resource: Kind/name=resourceVersion` cursor entries, and per-event
`rv`/`previous` transitions. `buc runs watch --follow` carries
forward the returned collection and per-resource versions for resource changes
and deletions. Watch follow requests opt into server BOOKMARK events, so an
unchanged collection is still visible as a cursor heartbeat.

## Secret Refs

Hosted secrets are the product path for user-provided credentials. Upload or
rotate a value once:

```bash
buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY
```

List metadata without values:

```bash
buc secrets list
```

Then pass the hosted ref inline:

```bash
buc doctor <package-digest> --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY
```

Or via YAML/JSON:

```yaml
GEMINI_API_KEY: bucephalus://GEMINI_API_KEY
```

```bash
buc run <package-digest> --secret-ref-file secrets.yaml
```

Delete a hosted secret when it should no longer be usable:

```bash
buc secrets delete GEMINI_API_KEY
```

Provider-native refs are still accepted when the Cloud control plane is allowed
to resolve them directly:

```bash
buc doctor <package-digest> --secret-ref GEMINI_API_KEY=gcp-secret-manager://projects/<project>/secrets/gemini/versions/latest
```

## What `buc build` Does

`buc build` currently means:

1. Classify the input as authoring YAML or sealed package.
2. For YAML, find `bucephalus.project.yaml` or `bucephalus.project.yml` by
   walking upward from the entrypoint. The manifest must declare
   `schema_version: bucephalus_project_v1`, `project.id`,
   `targets.hosted_cloud`, and the entrypoint in exactly one package source.
   Each package source must declare non-empty include patterns. The archive root
   is the manifest directory, not the YAML parent. The archive contains only
   declared files plus the manifest and entrypoint, minus
   excluded/generated/credential material such as `.git`, `.env*`, `target`,
   and `node_modules`. The CLI also
   preflights the context before upload with the same default limits as the API:
   10,000 archive entries and 256 MiB expanded bytes. Operators can tune those
   limits with
   `BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_ENTRIES` and
   `BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES`.
3. For sealed package directories, verify obvious package shape and archive the
   package when needed. The hosted API performs the authoritative import check:
   `manifest.json`, `checksums.json`, `package.lock`, `package_checks.json`,
   and `staging_manifest.json` must match the current sealed package contract.
   Cloud import rejects failed package checks, unsealed payload files, broken
   package digests, missing CAS blobs, and runtime staging destinations outside
   the runner contract roots
   `__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support` and
   `/bucephalus/in/runtime`.
4. Create a Cloud upload, upload bytes, and complete the upload.
5. Call `POST /v1/experiments/builds`. The referenced upload is resolved under
   the authenticated Cloud owner; knowing another user's upload id is not a
   build capability.
6. For authoring contexts, the API runs bundled Core in an isolated workspace
   and imports the produced sealed package through the same sealed-package
   contract checks used for direct package uploads.
7. Evaluate the accepted package against the hosted Cloud target and the exact
   runtime options supplied on the command line, such as `--backend`, `--arch`,
   `--isolation`, `--cpu-count`, `--memory-mb`, `--disk-mb`, and
   `--max-parallel-trials`.

   Hosted runtime options are closed over the Cloud API contract. Unknown keys
   and malformed values are rejected instead of ignored, so a typo such as
   `memory_mbb` cannot silently fall back to the default runner size. The
   supported keys are `backend`, `arch`, `cpu_count`,
   `memory_mb`, `disk_mb`, `isolation`, `timeout_ms`,
   `max_parallel_trials`, `network`, `sidecars`, and `accelerators`.
   With `--runtime-option`, scalar keys use `KEY=VALUE`, list keys use
   comma-separated values such as `sidecars=redis,postgres`, and `network`
   uses a JSON object such as
   `network={"default":"allowlist_enforced","egress":["api.openai.com"]}`.
   Hosted Cloud does not accept runtime placement selectors such as `region`,
   `runtime_region`, `placement`, or `zone`; runner placement is controlled by
   runner pools and the Cloud scheduler. The API rejects unsupported placement
   fields with pointers such as `/runtime_options/region`.
   Hosted Cloud does not accept the compatibility aliases `executor` or `cpu`;
   use canonical `backend` and `cpu_count`.
   Hosted `buc` does not accept `--smoke-test` until Cloud has a real hosted
   smoke-test primitive.
8. Fail the CLI command if authoring build/package inspection did not pass
   or if the hosted target checks report `cloud_blocked`.

Hosted Core builds receive an isolated `BUCEPHALUS_HOME`/`HOME`, a stable
non-secret `USER`/`USERNAME` builder identity, build-owned `TMPDIR`/`TMP`/`TEMP`,
and a minimal process environment. Cloud API database URLs, worker tokens, and
service secrets are not forwarded into the authoring build. Hosted Core
stdout/stderr tails are redacted before they leave the API. Hosted authoring
builds are also bounded by `BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS` on the
API service, defaulting to 10 minutes; a timeout returns a failed build with
`authoring_build.code: authoring_build_timed_out` and no imported package.
If Core exits successfully but does not create a usable package output, the API
also returns a failed build without importing anything, using
`authoring_build_missing_package`, `authoring_build_invalid_package`, or
`authoring_build_empty_package`.

The API response includes `build_kind: hosted_authoring_build` for YAML inputs
and `build_kind: sealed_package_import` for package inputs. It also includes
`build_environment` and `cloud_readiness`.

`build_environment` is the provenance/contract for the hosted build/import. It
reports the hosted target, immutable source upload evidence (`input_kind`,
`upload_id`, source archive/package `content_digest`, byte size, authoring
entrypoint, and project manifest evidence when applicable), the runtime option
object checked for readiness,
builder/importer image digest when the deployment provides one, release/git
metadata when available, and the package/readiness schema contract. For YAML
authoring inputs, `builder.kind` is `hosted_authoring_builder` and
`core.executed` is `true` with the bundled Core command/version/path and
timeout used by the API. For sealed package inputs, `builder.kind` is
`sealed_package_importer` and `core.executed` is `false`; Cloud imported an
already-built package and checked hosted readiness without claiming hosted Core
authored it. If this object is absent, the response is not a complete hosted
build result. The CLI also checks that the hosted response is
about the source it just uploaded: `build_environment.source.upload_id`,
`content_digest`, and `byte_size` must be present and match the upload created
by the command and the local archive bytes. For authoring YAML inputs,
`build_environment.source.entrypoint` must also be present and match the
entrypoint sent by the command. `build_environment.source.project_manifest`
records the manifest path, digest, project id, package source, source root, and
entrypoint used for the upload. `build_environment.runtime_options` and
`cloud_readiness.runtime_options` must also match the runtime options requested
by the command. The hosted target must be `hosted_cloud/default`, and the
package contract must match the requested input kind, `sealed_run_package_v2`,
and `hosted_cloud_readiness_v1` with Cloud readiness required. For successful
hosted authoring builds, `package_contract.authoring_compiler` is
`core_universal_v1`, and `authoring_build.source_upload_id` plus
`authoring_build.entrypoint` must also match the upload and entrypoint sent by
the CLI. The same contract reports
`package_contract.authoring_provenance.status=hosted_attested` and
`source=hosted_core`.

For sealed package imports, `package_contract.authoring_compiler` is `null`,
`package_contract.authoring_provenance.status=external_unattested`, and
`source=sealed_package_manifest`. That means Cloud verified the sealed package's
integrity and checked hosted readiness, but `sealed_run_package_v2` does not
attest the package's original local authoring environment, Core version,
platform, or target. `authoring_build.status` must be `unavailable`, because the
Cloud API imported an already sealed package instead of compiling authoring YAML.
When an import object is present, its `import_id`
must match `build_id`, and its `package_digest` must agree with the build-level
package digest.

Cloud also persists package-level provenance on the accepted package artifact.
`buc inspect`, `buc doctor`, `buc run`, and `buc runs get` surface this as
`package_provenance`. Hosted YAML builds keep
`package_provenance.status=hosted_attested`; sealed package imports keep
`package_provenance.status=external_unattested`. Existing rows created before
this contract use `status=unknown_legacy` instead of being silently upgraded.
Because package digests are content identities shared across users and uploads,
Cloud stores provenance on the owner/package association too; one user's sealed
package import cannot overwrite another user's hosted-attested provenance for
the same digest. Worker package downloads also resolve storage metadata through
the run owner's package association, so a later same-digest upload cannot move a
runner onto another owner's storage pointer.
The nested `evidence` object says which policy was applied and whether those
provenance fields are `complete` or `partial`. Local development defaults to
`policy: warn`: partial evidence does not by itself mean the package cannot run,
but it weakens the build's production audit trail and is surfaced as
`build_environment` warnings inside `cloud_readiness.checks`. Managed production
deployments set `BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY=enforce`; under that
policy, partial evidence turns an otherwise runnable package into
`cloud_blocked` with an operator action to complete build-environment evidence.
A production deployment should report complete evidence: immutable builder/API
image digest, release version, release git SHA, and, for hosted authoring
builds, hosted Core version. The managed GCP deployment injects the API image digest into this object from the
immutable Cloud Run image ref used for the service.

The current hosted authoring compiler is `core_universal_v1`: the API runs the
same package compiler shipped with Core, inside the hosted build environment.
The Cloud-specific guarantee comes from the required `hosted_cloud_readiness_v1`
gate after package import. If the product later needs target-specific lowering,
that compiler target must become an explicit API/schema field and be recorded in
`build_environment`; it should not be hidden behind flags or ambient local state.

`cloud_readiness` is the part that says whether the package is actually
runnable in that hosted Cloud target:

- `cloud_runnable`: package imported, image refs/resources/network/isolation map
  to the hosted runtime, and an active runner pool can satisfy it.
- `cloud_blocked`: the package imported, but some hosted runtime contract failed
  such as a local image ref, unsupported backend/arch/isolation/network setting,
  or no active runner capacity.
- `unavailable`: package import failed, so hosted readiness could not run.

Runtime secrets are reported as run-time requirements. A build can be
`cloud_runnable` while still warning that `buc run` must supply matching
`--secret-ref` values.

Plain run environment values are separate from secrets. Use `buc run ... --env
PUBLIC_MODE=smoke` only for non-secret configuration that can appear in run
metadata and CLI/API payloads. Hosted Cloud accepts only uppercase shell-style
env names matching `[A-Z_][A-Z0-9_]*`, rejects names reserved for Cloud
runtime/control-plane state such as `DATABASE_URL`,
`BUCEPHALUS_CLOUD_WORKER_TOKEN`, runner/store/resolver variables, generic
provider credential variables, and rejects any env key that also appears in
`--secret-ref`. Credentials belong in hosted secrets:
`buc secrets put NAME --from-env NAME` followed by
`buc run <digest> --secret-ref NAME=bucephalus://NAME`.

The readiness object also includes `required_actions`. These are the canonical
next steps for clients and UI surfaces:

- `stage: before_run` actions mean the package is build-valid, but the user
  must complete setup before creating a run. Runtime secrets use this state and
  include commands like `buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY`.
- `stage: before_rebuild` actions mean the experiment/package must change and
  be rebuilt, for example replacing a local image ref with a digest-pinned
  registry image.
- `stage: operator` actions mean hosted infrastructure must change, such as
  adding runner capacity for the requested resources.

## Operator Boundary

`bucephalus-cloud` is an internal operator utility for service and runner-pool
administration. Product workflows belong in `buc`.

Runner-pool administration uses `BUCEPHALUS_CLOUD_RUNNER_ADMIN_TOKEN` when it is
configured. Worker daemons use `BUCEPHALUS_CLOUD_WORKER_TOKEN` for registration,
heartbeats, queue claims, package downloads, and attempt updates. If no runner
admin token is configured, the worker token remains the compatibility admin
credential. Once the admin token is configured, worker-token headers no longer
authorize runner-pool administration. The HTTP API accepts the admin credential
as `Authorization: Bearer ...` or `X-Bucephalus-Runner-Admin-Token`; worker
routes accept bearer or `X-Bucephalus-Worker-Token`.

On GCP deploys, `runner_admin_token_secret_version` injects the admin token into
the API service only. The pool controller and runner VMs continue to receive the
worker token but not the runner-admin credential.
