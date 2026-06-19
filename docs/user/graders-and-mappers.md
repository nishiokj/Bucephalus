# Grader Runtime

The grader is one stage in `stages`.

The preferred authoring model is declarative transport:

```text
stages.case
  -> stages.agent inputs
  -> stages.agent outputs
  -> stages.grader inputs
  -> stages.grader outputs
  -> metrics
```

The runner owns extraction and the Transport Envelope between stages. A grader should not search runner envelopes for patches, parse trial layout paths, or translate native reports into Bucephalus metrics.

## Preferred Shape

Declare what the agent produces, what the grader consumes, what the grader emits, and where metrics read from.

```yaml
stages:
  agent:
    outputs:
      candidate:
        capture:
          type: workspace_diff
          format: unified_diff

  grader:
    strategy: in_task_runtime
    command: ["bash", "/testbed/eval.sh"]
    inputs:
      candidate_file:
        source:
          output: agent.candidate
          field: patch
        materialize:
          as: file
          path: /patch.diff
    outputs:
      report:
        capture:
          type: file
          path: /testbed/report.json
          format: json
          required: true

metrics:
  - id: pass_rate
    from: grader.report
    transform:
      type: pytest_json_report_pass_rate
```

This keeps each responsibility in the YAML:

| Declaration | Meaning |
| --- | --- |
| `agent.outputs` | Values captured from the agent phase. |
| `grader.inputs` | Values selected from case fields or upstream outputs and materialized for grading. |
| `grader.command` | The grader runtime command, when the grader strategy is command-based. |
| `grader.outputs` | Native grader outputs captured by the runner. |
| `metrics` | Durable metric extraction from declared outputs. |

See [Grader Transport](grader-transport.md) for the full transport model.

## Grader Strategies

| Strategy | Where it runs | Use when |
| --- | --- | --- |
| `none` | Nowhere | Metrics come from agent output and no grader result is needed. |
| `in_task_runtime` | In the case sandbox runtime | The case image already contains the grader dependencies or the command file can be sealed into the package. |
| `injected` | In the case sandbox after copying a sealed grader bundle | The benchmark grader should be sealed into the package and copied into each case workspace only after the agent exits. |
| `separate` | In a separate grader container | The grader needs a different image from the case sandbox. |
| `host` | On the runner host | The grader is a runner-owned integration such as official SWE-bench evaluation that must invoke host tooling. |

`in_task_runtime` and `injected` require `stages.case.interface: writable_workspace` and `stages.case.workspace.source: container_image`. `separate` uses the run's effective network mode. `host` graders receive launch env such as `--env ANTHROPIC_API_KEY=...`, but their contract paths point to host filesystem paths.

Container grader stages may attach top-level `services` with `stages.grader.services`. Attached services expose env only to the grader stage that declares them. `strategy: host` cannot attach container services.

## Strategy Declarations

Each strategy has a different packaging boundary. Declare the boundary directly instead of relying on arbitrary host paths.
Strategy-specific config blocks are closed:
`in_task_runtime` accepts `hidden_paths` and `revealed_paths`; `injected`
accepts `bundle` and `copy_dest`; `separate` accepts `image` and `workdir`;
`host` accepts `capability`. Use `stages.grader.max_concurrency` when a grader
needs a lower concurrency limit than the run.

### None

Use this when the agent result is the only source of metrics.

```yaml
stages:
  grader:
    strategy: none
```

You can also omit `stages.grader` entirely for this case; the authoring build
defaults an omitted grader to `strategy: none`. Do not declare an empty
`stages.grader: {}`.

Do not declare `from: grader...` metrics when `strategy: none`.

### In Case Runtime

Use this when the case image already has the grader runtime and dependencies.

```yaml
stages:
  grader:
    strategy: in_task_runtime
    command: ["python3", "./grader.py"]
    outputs:
      report:
        capture:
          type: file
          path: /bucephalus/out/grader_report.json
          format: json
          required: true
```

Relative command file paths, such as `./grader.py`, are package-owned files. The runner seals them into the package and mounts them in the case workdir support directory before grading.

For hidden assets already present in the case image, declare what must be hidden during the agent step and revealed during grading:

```yaml
stages:
  grader:
    strategy: in_task_runtime
    in_task_runtime:
      hidden_paths:
        - /workspace/tests
      revealed_paths:
        - /workspace/tests
```

### Injected

Use this when the grader is a sealed bundle that should be copied into the case sandbox for grading.

```yaml
stages:
  grader:
    strategy: injected
    command: ["python3", "/opt/grader/run.py"]
    injected:
      bundle: ./grader_bundle.tar.gz
      copy_dest: /opt/grader
    outputs:
      report:
        capture:
          type: file
          path: /bucephalus/out/grader_report.json
          format: json
          required: true
```

### Separate

Use this when the grader needs a different image from the case sandbox.

```yaml
stages:
  grader:
    strategy: separate
    command: ["python3", "/grader/run.py"]
    separate:
      image: ghcr.io/my-org/my-grader:latest
      workdir: /grader
    outputs:
      report:
        capture:
          type: file
          path: /grader/out/report.json
          format: json
          required: true
```

The separate grader container uses the run's effective network mode.

If the separate grader attaches services, Local Docker places the grader and those services on the same per-trial network so the grader can call each service by service id.

### Host

Host graders are an explicit runner-runtime boundary. They must name a runner-owned capability and reference that capability in the command; package-local files, case-workdir support paths, and arbitrary absolute host script paths are rejected during packaging or preflight.

```yaml
stages:
  grader:
    strategy: host
    host:
      capability: official_grader
    command:
      - official-grader
      - --grader-input
    outputs:
      report:
        capture:
          type: file
          path: /bucephalus/out/swebench_report.json
          format: json
          required: true
```

Do not use `host` for your own local grader script. Use `in_task_runtime`, `injected`, or `separate` so the grader is package-owned and portable.

## Failure Semantics

| Situation | Meaning |
| --- | --- |
| Agent exits 0 but result is missing | Agent contract failure. |
| Agent exits non-zero but writes valid result | Grader still runs; exit code is evidence. |
| Grader exits non-zero but writes declared outputs | Outputs are captured; reported outcome is failure. |
| Grader exits 0 but a required declared output is missing or invalid | Grading failed. |
| Metric reference points at a missing declared output or field | Grading failed before a misleading metric is committed. |

The runner should never fabricate a scientific verdict when declared grader
outputs or required metric references are missing or invalid.
