# Parameter Sweep Template

Use this when you want several variants that differ only by configuration.

```bash
bucephalus build experiment.yaml --out .lab/builds/parameter-sweep --json
bucephalus check-package .lab/builds/parameter-sweep --json
bucephalus preflight .lab/builds/parameter-sweep --json
bucephalus run .lab/builds/parameter-sweep --smoke-test --materialize full --json
```

Add or remove entries under `matrix.variants` to sweep your own parameters.

