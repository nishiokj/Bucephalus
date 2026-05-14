# Metrics

Metrics are declared in `experiment.yaml`. The declaration is the boundary between an agent or grader payload and the durable analytics schema.

AgentLab does not infer custom metrics by scanning arbitrary top-level fields in the agent response. If a value should become a queryable metric, declare it.

## Metric Declaration

```yaml
metrics:
  - id: latency
    label: Latency
    semantic_key: runtime.latency
    value_type: number
    unit: ms
    direction: minimize
    primary: true
    required: true
    source:
      type: agent_response
      pointer: /metrics/speed
```

| Field | Meaning |
| --- | --- |
| `id` | Canonical metric id stored in AgentLab. This becomes `metric_name` in `metrics_long`. |
| `label` | Human-readable label for views. |
| `semantic_key` | Optional cross-experiment meaning, such as `runtime.latency`. Use this when two experiments intentionally measure the same concept. |
| `value_type` | Optional type hint, such as `number`, `boolean`, or `string`. |
| `unit` | Optional unit, such as `ms`, `tokens`, `usd`, or `count`. |
| `direction` | Optional optimization direction: commonly `maximize` or `minimize`. |
| `primary` | Marks the declared metric as the primary metric for non-grading runs. |
| `required` | If true and the source value is missing, AgentLab records the metric as `null`. |
| `source.type` | Where the metric value comes from. |
| `source.pointer` | JSON Pointer into the source payload. Required for `agent_response`. |

## Canonical IDs

The metric id is the stored name. The source pointer is only an extraction path.

For example, this declaration:

```yaml
metrics:
  - id: latency
    source:
      type: agent_response
      pointer: /metrics/speed
```

with this agent response:

```json
{
  "metrics": {
    "speed": 123.0
  }
}
```

persists a metric named `latency`, not `speed`.

## Source Types

`agent_response` metrics are extracted from the agent response JSON written to `AGENTLAB_RESULT_PATH`.

```yaml
metrics:
  - id: hidden_cases_passed
    source:
      type: agent_response
      pointer: /metrics/hidden_cases_passed
```

For grader-backed benchmark runs, the durable benchmark verdict comes from `trial_conclusion_v1`. The grader should write `primary_metric.name` using the same canonical id you declared:

```yaml
metrics:
  - id: resolved
    source:
      type: grader_result
      pointer: /primary_metric/value
    direction: maximize
    primary: true
```

```json
{
  "schema_version": "trial_conclusion_v1",
  "reported_outcome": "success",
  "primary_metric": {
    "name": "resolved",
    "value": 1.0
  }
}
```

For `grader_result`, the declaration records the metric metadata and the grader conclusion supplies the committed primary metric. Additional grader payload fields are not exploded into custom metric rows today.

If you need multiple custom metrics without a grader, write them into the agent response and declare each one with `source.type: agent_response`.

## Strict Shape

Metric sources must use the object form:

```yaml
source:
  type: agent_response
  pointer: /metrics/resolved
```

These legacy forms are rejected:

```yaml
source: output
json_pointer: /metrics/resolved
```

```yaml
source: runner
```

## Persistence

Metric declarations are stored in the account SQLite database in `metric_definitions`. Metric observations are stored in `metric_rows` and exposed through the `metrics_long` analysis view.

`metrics_long` includes both the observation and declaration metadata:

- `metric_name`
- `metric_value`
- `semantic_key`
- `metric_label`
- `value_type`
- `unit`
- `direction`

Example:

```bash
lab query <run_id> "
  SELECT variant_id, metric_name, metric_value, semantic_key, unit
  FROM metrics_long
  ORDER BY variant_id, metric_name
"
```
