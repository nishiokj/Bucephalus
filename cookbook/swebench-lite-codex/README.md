# SWE-bench Lite + Codex CLI

This recipe is a real benchmark slice, not a toy task:

- task: `astropy__astropy-12907` from SWE-bench Lite
- workspace: a SWE-bench task image, extended only to add Node.js and Codex CLI
- agent: Codex CLI running headlessly with `codex exec`
- grader: real patch application plus the official task `test_patch`, followed by
  the task's `FAIL_TO_PASS` and `PASS_TO_PASS` pytest selections

The default subset is one task so a new user can see the full shape without
starting a 300-instance run. Add more rows to `cases.jsonl` and matching task
images when you are ready to scale.

## Prerequisites

- Docker or OrbStack
- an OpenAI credential available to Codex CLI in the container, usually
  `OPENAI_API_KEY`
- network access while building the task image, because the Dockerfile installs
  Node.js and `@openai/codex`
- the base SWE-bench task image:
  `swebench/sweb.eval.x86_64.astropy__astropy-12907:latest`

## Build The Task Image

```bash
bash scripts/build-image.sh
```

This builds:

```text
bucephalus/cookbook-swebench-lite-codex-astropy-12907:local
```

## Build And Check The Package

From the repo root:

```bash
cargo build --bin bucephalus
BUCEPHALUS="$(pwd)/target/debug/bucephalus"

"$BUCEPHALUS" build cookbook/swebench-lite-codex/experiment.yaml \
  --out <package_dir> --json

"$BUCEPHALUS" check-package <package_dir> --json
"$BUCEPHALUS" preflight <package_dir> --env OPENAI_API_KEY="$OPENAI_API_KEY" --json
```

## Run A Smoke Test

```bash
"$BUCEPHALUS" run <package_dir> \
  --smoke-test \
  --env OPENAI_API_KEY="$OPENAI_API_KEY" \
  --materialize full \
  --json
```

The run captures:

- `result`: the Codex wrapper's JSON summary
- `agent.candidate_patch`: the workspace diff after Codex edits `/testbed`
- `grader.report`: the real SWE-bench subset grading report

The primary metric is `resolved`, read from `grader.report`.

## Notes

This is deliberately heavier than the other cookbook recipes. It has real
benchmark semantics, real container setup, provider credentials, a coding agent,
patch capture, and test-based grading. Use the smaller cookbook recipes first if
you only want to learn the YAML shape.
