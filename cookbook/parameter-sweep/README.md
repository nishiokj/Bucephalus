# Parameter Sweep Template

Use this when you want several variants that differ only by configuration.

```bash
bucephalus build experiment.yaml --out <package_dir> --json
bucephalus check-package <package_dir> --json
bucephalus preflight <package_dir> --json
bucephalus run <package_dir> --smoke-test --materialize full --json
```

Add or remove entries under `matrix.variants` to sweep your own parameters.

