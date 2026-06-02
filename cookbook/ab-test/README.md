# A/B Test Template

Use this when you want a baseline/treatment comparison over the same cases.

```bash
bucephalus build experiment.yaml --out <package_dir> --json
bucephalus check-package <package_dir> --json
bucephalus preflight <package_dir> --json
bucephalus run <package_dir> --smoke-test --materialize full --json
```

Edit `matrix.variants[].config` to pass different model names, prompts,
temperatures, or feature flags into your agent command.

