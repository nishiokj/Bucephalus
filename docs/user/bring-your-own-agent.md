# Bring Your Own Agent

Your agent is just an application that follows the runtime contract. Bucephalus invokes `stages.agent.command`, passes trial data through environment variables, and expects any valid JSON response at `BUCEPHALUS_RESULT_PATH`.

## Minimal Agent

```js
#!/usr/bin/env node
const fs = require("fs");

const trial = JSON.parse(fs.readFileSync(process.env.BUCEPHALUS_TRIAL_INPUT_PATH, "utf8"));
const result = {
  answer: {
    case: trial.case,
    message: "agent completed the case"
  },
  checkpoints: []
};

fs.writeFileSync(process.env.BUCEPHALUS_RESULT_PATH, JSON.stringify(result));
```

## Provider-Backed Agent

Your provider-backed agent follows the same contract: read `BUCEPHALUS_TRIAL_INPUT_PATH`, call the provider using the credentials you explicitly pass with `--env` or `--env-file`, then write JSON to `BUCEPHALUS_RESULT_PATH`.

Wire it through YAML:

```yaml
runtime:
  network:
    agent: full

stages:
  agent:
    mount:
      source: ./agent
      mount:
        path: /opt/agent
        read_only: true
    image: ghcr.io/my-org/my-agent-runtime:latest
    command: ["agent", "run", "--model", "$model"]
    env:
      ANTHROPIC_API_KEY: "$ANTHROPIC_API_KEY"
  execution:
    agent_site: agent_container
```

Run with:

```bash
lab run .lab/builds/my-package --env ANTHROPIC_API_KEY=...
```

## Inputs And Outputs

| Path or env | Direction | Contract |
| --- | --- | --- |
| `BUCEPHALUS_TRIAL_INPUT_PATH` | Runner to agent | `schemas/trial_input_v1.jsonschema` |
| `BUCEPHALUS_RESULT_PATH` | Agent to runner | Any valid JSON response |
| `BUCEPHALUS_TRAJECTORY_PATH` | Agent to runner, optional | first declared event JSONL path when `integration_level: cli_events` |
| `stages.agent.events` | Agent to runner, optional | declared JSONL event captures ingested into SQLite while the trial runs |
| `stages.agent.output_mounts` | Agent to runner, optional | extra persisted files |

The runner does not remap your app's custom input or output flags. Put the command line shape your app needs directly in `stages.agent.command`, and read/write the contract env paths inside your app or wrapper.

Agent response metrics are also not remapped implicitly. If your agent writes `"metrics": {"speed": 123}`, the runner will only persist it as a custom metric when `experiment.yaml` declares a metric source pointing at `/metrics/speed`. See [Metrics](metrics.md).
