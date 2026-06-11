# Runtime Backends

Backend declarations live in `runtime`. The compute backend is the canonical trial executor for the package:

```yaml
runtime:
  compute:
    backend: local-docker
  registry:
    image_rewrites:
      - match_prefix: registry.example.invalid/project.
        replace_prefix: ghcr.io/acme/project.
        platform: linux/amd64
```

Implemented backends:

- `compute.backend: local-docker`
- `compute.backend: modal`

Storage and trace sinks are runner-owned today. Do not declare
`runtime.storage` or `runtime.traces`; those no-op backend declarations are
rejected so packages only carry fields that affect execution.

In authoring YAML, `runtime.registry` is a closed package-build object with one
supported field: `image_rewrites`. Each rewrite replaces case-row image prefixes
and can set a platform before the package is sealed. The sealed package carries
the rewritten case images directly and rejects `runtime.registry`. Declare
credentials through `runtime.secrets`, not registry auth blobs.

`runtime.compute.backend` selects the trial executor unless the CLI supplies an explicit executor override. Use `local-docker` for local container execution and `modal` for Modal sandbox execution.

Backend `config` objects are currently closed and empty for implemented backends. Experiment concurrency belongs to `scheduling.max_concurrency`; trial timeouts belong to `policy.timeout_ms`.

The CLI `--executor` flag is an operator override for an existing package. It uses CLI enum spelling, such as `--executor local_docker` or `--executor modal`, while YAML uses backend spelling, such as `local-docker` or `modal`.

Modal support is not identical to Local Docker: ephemerals are rejected until backend-native service attachment exists.

The Modal backend launches sandboxes through a packaged `bucephalus-modal-launcher` helper built from Modal's official Go SDK. Release archives install it next to `bucephalus`; override the helper path with `BUCEPHALUS_MODAL_LAUNCHER` for development or custom packaging. Modal authentication uses the SDK's standard environment, for example `MODAL_TOKEN_ID` and `MODAL_TOKEN_SECRET`.

For private GCP Artifact Registry task images, configure the worker with either
`BUCEPHALUS_MODAL_GCP_ARTIFACT_REGISTRY_SECRET` pointing at a Modal Secret that
contains `SERVICE_ACCOUNT_JSON`, or
`BUCEPHALUS_MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_B64` containing the
base64-encoded service-account JSON.
For Modal runtime sync through a GCS CloudBucketMount, set
`BUCEPHALUS_MODAL_S3_ENDPOINT_URL=https://storage.googleapis.com` and provide the
same credential through `BUCEPHALUS_MODAL_GCP_SERVICE_ACCOUNT_JSON_B64` or a
Modal Secret named by `BUCEPHALUS_MODAL_GCS_SECRET`.

## Active Runtime Caps

The runner enforces a simple active-resource cap before launching a trial:

- Local Docker defaults to `24` active Bucephalus-owned containers on the Docker daemon. A trial counts its case sandbox, any ephemerals, and a separate grader sandbox when `stages.grader.strategy: separate` is used. Override with `BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS`.
- Modal defaults to `64` active sandboxes per runner process. A trial counts its case sandbox and a separate grader sandbox when one is needed. Override with `BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES`.

These caps are intentionally coarse safety rails. They prevent a high-concurrency run from silently multiplying containers or Modal sandboxes faster than the runner can clean them up. More granular CPU, memory, and backend quota scheduling can be layered on later without changing experiment YAML.

Kubernetes, private registry auth, and OTel exporters can be added behind this surface without changing experiment YAML.
