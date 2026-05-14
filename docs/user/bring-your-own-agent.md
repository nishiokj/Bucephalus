# Bring Your Own Agent

Your agent is just an application that follows the runtime contract. AgentLab invokes `runtime.agent_runtime.command`, passes trial data through environment variables, and expects any valid JSON response at `AGENTLAB_RESULT_PATH`.

## Minimal Python Agent

```python
import json
import os
from pathlib import Path


trial = json.loads(Path(os.environ["AGENTLAB_TRIAL_INPUT_PATH"]).read_text())
task = trial["task"]

result = {
    "answer": {
        "task": task,
        "message": "agent completed the task"
    },
    "checkpoints": []
}

Path(os.environ["AGENTLAB_RESULT_PATH"]).write_text(json.dumps(result))
```

## Provider-Backed Agent

```python
import json
import os
from pathlib import Path

from anthropic import Anthropic


trial = json.loads(Path(os.environ["AGENTLAB_TRIAL_INPUT_PATH"]).read_text())
prompt = trial["task"]["input"]["prompt"]

client = Anthropic(api_key=os.environ["ANTHROPIC_API_KEY"])
message = client.messages.create(
    model=os.environ["MODEL"],
    max_tokens=1024,
    messages=[{"role": "user", "content": prompt}],
)

result = {
    "answer": message.content[0].text,
    "checkpoints": []
}

Path(os.environ["AGENTLAB_RESULT_PATH"]).write_text(json.dumps(result))
```

Wire it through YAML:

```yaml
runtime:
  agent_runtime:
    artifact: ./agent
    image: ghcr.io/my-org/my-agent-runtime:latest
    command: ["python", "-m", "agent.run"]
    env:
      ANTHROPIC_API_KEY: "$ANTHROPIC_API_KEY"
      MODEL: "$model"
    network: full
```

Run with:

```bash
lab run .lab/builds/my-package --env ANTHROPIC_API_KEY=...
```

## Inputs And Outputs

| Path or env | Direction | Contract |
| --- | --- | --- |
| `AGENTLAB_TRIAL_INPUT_PATH` | Runner to agent | `schemas/trial_input_v1.jsonschema` |
| `AGENTLAB_RESULT_PATH` | Agent to runner | Any valid JSON response |
| `AGENTLAB_TRAJECTORY_PATH` | Agent to runner, optional | event JSONL when declared |
| `runtime.agent_runtime.output_mounts` | Agent to runner, optional | extra persisted files |

The runner does not remap your app's custom input or output flags. Put the command line shape your app needs directly in `runtime.agent_runtime.command`, and read/write the contract env paths inside your app or wrapper.

Agent response metrics are also not remapped implicitly. If your agent writes `"metrics": {"speed": 123}`, the runner will only persist it as a custom metric when `experiment.yaml` declares a metric source pointing at `/metrics/speed`. See [Metrics](metrics.md).
