# Metrics

Metrics are declared in `experiment.yaml`. The declaration is the boundary between an agent or grader payload and the durable analytics schema.

Bucephalus does not infer custom metrics by scanning arbitrary top-level fields in the agent response. If a value should become a queryable metric, declare it.

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
| `id` | Canonical metric id stored in Bucephalus. This becomes `metric_name` in `metrics_long`. |
| `label` | Human-readable label for views. |
| `semantic_key` | Optional cross-experiment meaning, such as `runtime.latency`. Use this when two experiments intentionally measure the same concept. |
| `value_type` | Optional type hint, such as `number`, `boolean`, or `string`. |
| `unit` | Optional unit, such as `ms`, `tokens`, `usd`, or `count`. |
| `direction` | Optional optimization direction: commonly `maximize` or `minimize`. |
| `primary` | Marks the declared metric as the primary metric. For grader-backed runs, primary metrics should read from `grader_output`. |
| `required` | If true and the source value is missing, the run fails before the missing value is silently committed. |
| `source.type` | Where the metric value comes from. |
| `source.pointer` | JSON Pointer into the source payload. Required for `agent_response`; optional for declared grader outputs. |

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

`agent_response` metrics are extracted from the agent response JSON written to `BUCEPHALUS_RESULT_PATH`.

```yaml
metrics:
  - id: hidden_cases_passed
    source:
      type: agent_response
      pointer: /metrics/hidden_cases_passed
```

For grader-backed benchmark runs, prefer reading metrics from declared grader outputs:

```yaml
stages:
  grader:
    outputs:
      report:
        capture:
          type: file
          path: /testbed/report.json
          format: json

metrics:
  - id: pass_rate
    source:
      type: grader_output
      output: report
      pointer: /pass_rate
```

The grader writes its native output. The runner captures the declared output, applies the metric source, and builds the internal trial conclusion used by scheduling and persistence.

If you need multiple custom metrics without a grader, write them into the agent response and declare each one with `source.type: agent_response`.

`bucephalus check-package` validates the metric/grader relationship statically. A
no-grader experiment may use `agent_response` or `runtime_output` metrics, but
it fails package checks if any metric uses `source.type: grader_output` while
`stages.grader.strategy: none`.

## Events Are Not Metrics

Declared event streams live under `stages.agent.events`, not top-level
`metrics`. Event capture is for live trace/progress data: JSONL rows are
ingested into SQLite and exposed through the `events` view while a trial runs.

Metrics are scalar observations that become rows in `metrics_long`. If an event
stream contains values you eventually want to analyze as metrics, derive those
values into the agent response or a grader output today, then declare normal
metric sources for them. Event-derived metric declarations are intentionally a
separate future extension.

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
bucephalus query <run_id> "
  SELECT variant_id, metric_name, metric_value, semantic_key, unit
  FROM metrics_long
  ORDER BY variant_id, metric_name
"
```
