# Runtime Backends

Backend declarations live in `runtime`:

```yaml
runtime:
  compute:
    backend: local-docker
    config: { max_parallel: 4, trial_timeout_ms: 600000 }
  storage:
    backend: local-fs
    config: { root: .lab/runs/ }
  registry:
    default: ghcr.io/acme
  traces:
    backend: local-stdout
```

This patch lands the interface. Implemented backends are local only:

- `compute.backend: local-docker`
- `storage.backend: local-fs`
- `traces.backend: local-stdout`

Modal, S3, Kubernetes, private registry auth, and OTel exporters can be added behind this surface without changing experiment YAML.
