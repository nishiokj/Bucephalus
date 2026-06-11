# Run Notes

## 2026-06-11

Benchmark source:

- `THUDM/AgentBench`
- Local checkout: `/Users/jevinnishioka/Desktop/AgentBench`
- Commit: `d1e4a10db08c87075c78972e48ecc182be03e2d5`
- Split: `data/lateralthinkingpuzzle/dev.xlsx`

Runtime:

- Nova image base: `nova-service:local`
- Experiment image: `agentbench-ltp-nova:local`
- Experiment package: `agentbench_ltp_nova_20260611_024841_446589`

Validation:

- `bucephalus doctor cookbook/agentbench-ltp-nova/experiment.yaml --env-file .env --json`
  passed preflight with 13/13 checks.
- Smoke run `run_20260611_024843_086698_000001` completed 1/1 trial.
- Full run `run_20260611_025050_990679_000001` completed 5/5 trials.

Full-run score payload summary:

| case | key_recall | matched | total |
| --- | ---: | ---: | ---: |
| `agentbench_ltp_dev_001` | 1.0000 | 4 | 4 |
| `agentbench_ltp_dev_002` | 0.1667 | 1 | 6 |
| `agentbench_ltp_dev_003` | 0.2500 | 1 | 4 |
| `agentbench_ltp_dev_004` | 1.0000 | 4 | 4 |
| `agentbench_ltp_dev_005` | 0.0000 | 0 | 3 |

Aggregate:

- Average key recall: `0.4833333333333333`
- Matched keys: `10`
- Total keys: `21`
- Tokens in: `9679`
- Tokens out: `2932`

Known Bucephalus issue:

- GitHub issue: `https://github.com/nishiokj/Bucephalus/issues/95`
- Required direct metric extraction from declared grader-output fields resolves
  null during local trial execution, despite the mapped grader payload being
  valid. The recipe therefore keeps grader-output metrics non-required and reads
  benchmark scores from `trial_conclusion_payload` until the runner bug is fixed.
