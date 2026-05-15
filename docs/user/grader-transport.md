# Runtime Transport

This page describes the target contract for connecting runtime outputs to downstream runtime inputs, and downstream outputs to metrics.

Older grader integrations made graders parse runner envelopes and emit AgentLab-specific conclusions. The hard-cutover contract is declarative transport: graders consume declared inputs and produce native declared outputs, while the runner owns the plumbing and metric extraction.

The intended contract is declarative transport. The runner owns the plumbing.

## Chain

An evaluation is a chain of runtime boundaries:

```text
runtime A
  -> named outputs captured by the runner
  -> runtime B inputs selected from named outputs or task fields
  -> materialized runtime B view
  -> runtime B execution
  -> named runtime B outputs captured by the runner
  -> metrics read from named outputs
```

For the common agent-to-grader case, runtime A is the agent and runtime B is the grader.

A downstream runtime should not search for candidate artifacts or understand runner envelope internals. If it needs a file, env var, stdin payload, HTTP body, or other input view, declare how to materialize that view. If it writes a file, stdout JSON, HTTP response, or other output, declare how to capture it. Metrics then read from declared outputs.

This is not a patch-specific feature. A patch is one possible output kind.

## Runtime Outputs

Runtime outputs are named values captured by the runner.

```yaml
trial_runtime:
  agent:
    outputs:
      result:
        capture:
          type: file
          path: /agentlab/out/result.json
          format: json

      candidate_patch:
        capture:
          type: workspace_diff
          format: unified_diff
```

Each output has a kind implied or declared by its capture type. Examples include JSON result files, workspace diffs, plain files, stdout, stderr, directories, archives, or external responses.

The runner records named outputs in an internal transport envelope. Downstream bindings should reference named outputs and fields, not the envelope's physical layout.

## Runtime Inputs

Runtime inputs bind upstream values into the downstream runtime.

```yaml
trial_runtime:
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
        required: true
```

This declaration means:

1. read a field from a named upstream output
2. materialize the selected value into the downstream runtime
3. fail before downstream execution if a required value is missing

The input name is the downstream-facing binding. It does not have to match the upstream output name.

Materialization is not only file writing. Valid materializations can include:

```yaml
materialize: { as: file, path: /runtime/in/value.json }
materialize: { as: env, name: CANDIDATE_VALUE }
materialize: { as: stdin }
materialize: { as: json_body }
materialize: { as: multipart_field, name: artifact }
```

## Downstream Outputs

Downstream outputs are native files or responses produced by the downstream runtime and captured by the runner.

```yaml
trial_runtime:
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

A grader does not emit AgentLab's internal trial conclusion when the native benchmark already writes structured output that can be declared and mapped.

## Metrics

Metrics can read declared outputs.

```yaml
metrics:
  - id: pass_rate
    source:
      type: grader_output
      output: pytest_report
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
trial_runtime:
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
        required: true
    outputs:
      pytest_report:
        capture:
          type: file
          path: /testbed/report.json
          format: json
          required: true
```

The eval script is the grader runtime command. The patch file is the materialized grader input. The report file is the captured grader output.

For `in_task_runtime`, the agent and grader can run in the same task container. The runner still treats the transition from agent phase to grader phase as a contract boundary: capture outputs, materialize inputs, run the downstream runtime, capture outputs.

## Example: JSON Answer Grader

An answer-based benchmark may not have patches at all.

```yaml
trial_runtime:
  agent:
    outputs:
      final_answer:
        capture:
          type: result_json
          path: /agentlab/out/result.json
          field: final_answer

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
              task: input.prompt
            reference:
              task: answer
            candidate:
              output: agent.final_answer
              field: value
        materialize:
          as: json_file
          path: /grader/in/payload.json
        required: true
    outputs:
      grade:
        capture:
          type: file
          path: /grader/out/grade.json
          format: json
          required: true
```

## External Graders

The same model works when the grader is not a command.

```yaml
trial_runtime:
  grader:
    strategy: external
    endpoint:
      type: http
      method: POST
      url: https://grader.example.com/evaluate
    inputs:
      request_body:
        source:
          object:
            task_id:
              task: id
            answer:
              output: agent.result
              field: final_answer
        materialize:
          as: json_body
    outputs:
      response:
        capture:
          type: http_response
          format: json
```

`command` is one runtime implementation. Inputs, outputs, materialization, and capture are the stable parts of the contract.

## Validation

The runner should validate the chain before execution:

- input sources reference existing outputs or task fields
- selected fields exist for the selected output kind
- required inputs cannot be missing
- materialization paths are allowed for the grader strategy
- grader output paths are valid for the grader strategy
- metrics reference known grader outputs
- metric transforms support the declared output format

Failures should name the broken link, such as:

```text
grader.inputs.patch_file source field patch missing from agent.candidate_patch
grader.inputs.patch_file failed to materialize /patch.diff
grader.outputs.pytest_report missing at /testbed/report.json
metrics.pass_rate cannot read pytest_report with transform pytest_json_report_pass_rate
```
