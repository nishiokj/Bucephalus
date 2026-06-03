# Runtime Backends

Backend declarations live in `runtime`. The compute backend is the canonical trial executor for the package:

```yaml
runtime:
  compute:
    backend: local-docker
    config: { max_parallel: 4, trial_timeout_ms: 600000 }
  storage:
    backend: local-fs
    config: {}
  registry:
    default: ghcr.io/acme
  traces:
    backend: local-stdout
```

Implemented backends:

- `compute.backend: local-docker`
- `compute.backend: modal`
- `storage.backend: local-fs`
- `traces.backend: local-stdout`

`runtime.compute.backend` selects the trial executor unless the CLI supplies an explicit executor override. Use `local-docker` for local container execution and `modal` for Modal sandbox execution.

The CLI `--executor` flag is an operator override for an existing package. It uses CLI enum spelling, such as `--executor local_docker` or `--executor modal`, while YAML uses backend spelling, such as `local-docker` or `modal`.

Modal support is not identical to Local Docker: ephemerals are rejected until backend-native service attachment exists.

## Active Runtime Caps

The runner enforces a simple active-resource cap before launching a trial:

- Local Docker defaults to `24` active Bucephalus-owned containers on the Docker daemon. A trial counts its case sandbox, any ephemerals, and a separate grader sandbox when `stages.grader.strategy: separate` is used. Override with `BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS`.
- Modal defaults to `64` active sandboxes per runner process. A trial counts its case sandbox and a separate grader sandbox when one is needed. Override with `BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES`.

These caps are intentionally coarse safety rails. They prevent a high-concurrency run from silently multiplying containers or Modal sandboxes faster than the runner can clean them up. More granular CPU, memory, and backend quota scheduling can be layered on later without changing experiment YAML.

Kubernetes, private registry auth, and OTel exporters can be added behind this surface without changing experiment YAML.
