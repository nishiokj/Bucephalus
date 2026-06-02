# Agent Eval Template

The smallest no-key recipe: one variant, a few container-backed cases, and
metrics read from the agent response JSON.

```bash
bucephalus build experiment.yaml --out <package_dir> --json
bucephalus check-package <package_dir> --json
bucephalus preflight <package_dir> --json
bucephalus run <package_dir> --smoke-test --materialize full --json
```

Replace `agent/run.js` with your agent app, or replace `stages.agent.image`
and `stages.agent.command` with your own container entrypoint.
