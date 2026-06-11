# AgentBench LTP Nova

This recipe runs Nova against a small slice of the open-source
AgentBench Lateral Thinking Puzzle benchmark.

Benchmark source:

- Upstream repo: `https://github.com/THUDM/AgentBench`
- Local checkout: `/Users/jevinnishioka/Desktop/AgentBench`
- Tested checkout: `d1e4a10`
- Source split: `data/lateralthinkingpuzzle/dev.xlsx`
- AgentBench task config: `configs/tasks/ltp.yaml`

The full AgentBench LTP task is normally served by the published
`longinyu/agentbench-ltp` environment. This Bucephalus recipe keeps the
experiment lightweight by using the same public LTP workbook directly and
scoring final-answer key recall. It does not modify AgentBench or Nova.

## Files

- `experiment.yaml` declares the Bucephalus experiment.
- `Dockerfile.nova-runtime` extends the local `nova-service:local` image with
  Python for grading.
- `agent/nova-config.json` constrains Nova to answer-only behavior.
- `scripts/generate_cases.py` extracts cases from the upstream AgentBench LTP
  workbook.
- `grader/grade_ltp.py` scores Nova's final answer against AgentBench answer
  key bullets.
- `cases.jsonl` is generated from the upstream workbook.

## Run

```bash
docker build -t agentbench-ltp-nova:local \
  -f cookbook/agentbench-ltp-nova/Dockerfile.nova-runtime \
  cookbook/agentbench-ltp-nova

bucephalus dev cookbook/agentbench-ltp-nova/experiment.yaml \
  --env-file .env
```

For a full recipe run after the smoke test:

```bash
bucephalus run cookbook/agentbench-ltp-nova/experiment.yaml \
  --env-file .env
```

Metrics:

- `smoke_ok`: scalar grader-health metric. Grader-output metrics are declared
  but non-required while Bucephalus issue #95 tracks local metric extraction.
- `key_recall`: fraction of AgentBench answer-key bullets covered.
- `matched_keys`: count of key bullets covered.
- `total_keys`: count of answer-key bullets in the case.
- `model_calls`, `tokens_in`, `tokens_out`, `latency_ms`: Nova runtime
  observability copied into the grader report.
