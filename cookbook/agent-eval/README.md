# Agent Eval Template

The smallest no-key recipe: one variant, a few container-backed cases, and
metrics read from the agent response JSON.

```bash
bucephalus build experiment.yaml --out .lab/builds/agent-eval --json
bucephalus check-package .lab/builds/agent-eval --json
bucephalus preflight .lab/builds/agent-eval --json
bucephalus run .lab/builds/agent-eval --smoke-test --materialize full --json
```

Replace `agent/run.js` with your agent app, or replace `stages.agent.image`
and `stages.agent.command` with your own container entrypoint.
