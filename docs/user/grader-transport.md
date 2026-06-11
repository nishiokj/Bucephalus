# Runtime Transport

This page describes the target contract for connecting stage outputs to downstream stage inputs, and downstream outputs to metrics.

Older grader integrations made graders parse runner envelopes and emit Bucephalus-specific conclusions. The contract is declarative transport: stages consume declared inputs and produce native declared outputs, while the runner owns the Transport Envelope and metric extraction.

The intended contract is declarative transport. The runner owns the plumbing.

## Chain

An evaluation is a chain of stage boundaries:

```text
stage A
  -> named outputs captured by the runner
  -> stage B inputs selected from named outputs or case fields
  -> materialized stage B view
  -> stage B execution
  -> named stage B outputs captured by the runner
  -> metrics read from named outputs
```

For the common agent-to-grader case, stage A is the agent and stage B is the grader.

A downstream stage should not search for candidate artifacts or understand runner envelope internals. If it needs a file or env var, declare how to materialize that view. If it writes a file or workspace diff, declare how to capture it. Metrics then read from declared outputs.

This is not a patch-specific feature. A patch is one possible output kind.

## Stage Outputs

Stage outputs are named values captured by the runner.

```yaml
stages:
  agent:
    outputs:
      candidate_patch:
        capture:
          type: workspace_diff
          format: unified_diff
```

The canonical agent `result` output is added by default and is runner-owned. Do
not declare `stages.agent.outputs.result`; declare extra outputs only when
downstream stages need another named value. Each output has a kind implied or
declared by its capture type. Examples include workspace diffs, plain files,
stdout, stderr, directories, archives, or external responses.

The runner records named outputs in an internal transport envelope. Downstream bindings should reference named outputs and fields, not the envelope's physical layout.

## Stage Inputs

Stage inputs bind upstream values into the downstream stage.

```yaml
stages:
  grader:
    strategy: in_task_runtime
    command:
      - bash
      - /testbed/eval.sh

    inputs:
      patch_file:
        source:
          output: agent.candidate_patch
          field: patch
        materialize:
          as: file
          path: /patch.diff
```

This declaration means:

1. read a field from a named upstream output
2. materialize the selected value into the downstream runtime
3. fail before downstream execution if a required value is missing

The input name is the downstream-facing binding. It does not have to match the upstream output name.
Declared grader inputs default to `required: true`; build writes the boolean
into sealed packages. Set `required: false` only for optional context that the
grader can safely run without.

Current command-grader materializations are:

```yaml
materialize: { as: file, path: /runtime/in/value.json }
materialize: { as: json_file, path: /runtime/in/value.json }
materialize: { as: env, name: CANDIDATE_VALUE }
```

`stdin`, `json_body`, and `multipart_field` are reserved for future runtime
transports and are rejected by the current authoring schema.

## Downstream Outputs

Downstream outputs are native files or responses produced by the downstream runtime and captured by the runner.

```yaml
stages:
  grader:
    outputs:
      pytest_report:
        capture:
          type: file
          path: /testbed/report.json
          format: json
          required: true

      pytest_exit_code:
        capture:
          type: file
          path: /testbed/pytest_exit_code.txt
          format: text
          required: false
```

A grader does not emit Bucephalus's internal trial conclusion when the native
benchmark already writes structured output. Declare that native output and let
metrics read from it.

## Metrics

Metrics can read declared outputs.

```yaml
metrics:
  - id: pass_rate
    from: grader.pytest_report
    transform:
      type: pytest_json_report_pass_rate
      test_ids:
        source:
          task: commit0.test_ids
```

The transform is runner-owned. It converts a known output format into a metric value.

## Example: Patch-Based Test Grader

This example is not the contract. It is one instance of the contract.

A patch-based benchmark may capture a workspace diff from the agent, materialize it as a file for the grader, run the grader command, and capture a test report.

```yaml
stages:
  agent:
    outputs:
      candidate_patch:
        capture:
          type: workspace_diff
          format: unified_diff

  grader:
    strategy: in_task_runtime
    command:
      - bash
      - /testbed/eval.sh
    inputs:
      patch_file:
        source:
          output: agent.candidate_patch
          field: patch
        materialize:
          as: file
          path: /patch.diff
    outputs:
      pytest_report:
        capture:
          type: file
          path: /testbed/report.json
          format: json
          required: true
```

The eval script is the grader runtime command. The patch file is the materialized grader input. The report file is the captured grader output.

For `in_task_runtime`, the agent and grader can run in the same case sandbox. The runner still treats the transition from agent stage to grader stage as a contract boundary: capture outputs, materialize inputs, run the downstream stage, capture outputs.

## Example: JSON Answer Grader

An answer-based benchmark may not have patches at all.

```yaml
stages:
  grader:
    strategy: separate
    command:
      - python3
      - /grader/grade.py
    inputs:
      payload:
        source:
          object:
            prompt:
              case: input.prompt
            reference:
              case: answer
            candidate:
              output: result
              field: final_answer
        materialize:
          as: json_file
          path: /grader/in/payload.json
    outputs:
      grade:
        capture:
          type: file
          path: /grader/out/grade.json
          format: json
          required: true
```

Future non-command grader transports should reuse the same inputs, outputs,
materialization, and capture model instead of exposing runner internals.

## Validation

The runner should validate the chain before execution:

- input sources reference existing outputs or case fields
- selected fields exist for the selected output kind
- required inputs cannot be missing
- materialization paths are allowed for the grader strategy
- grader output paths are valid for the grader strategy
- metrics reference known grader/runtime outputs
- metric transforms support the declared output format

Failures should name the broken link, such as:

```text
grader.inputs.patch_file source field patch missing from agent.candidate_patch
grader.inputs.patch_file failed to materialize /patch.diff
grader.outputs.pytest_report missing at /testbed/report.json
metrics.pass_rate cannot read pytest_report with transform pytest_json_report_pass_rate
```
