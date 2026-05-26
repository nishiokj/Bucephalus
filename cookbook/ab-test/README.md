# A/B Test Template

Use this when you want a baseline/treatment comparison over the same cases.

```bash
bucephalus build experiment.yaml --out .lab/builds/ab-test --json
bucephalus check-package .lab/builds/ab-test --json
bucephalus preflight .lab/builds/ab-test --json
bucephalus run .lab/builds/ab-test --smoke-test --materialize full --json
```

Edit `matrix.variants[].config` to pass different model names, prompts,
temperatures, or feature flags into your agent command.

