# Run Notes

## 2026-06-11

Benchmark source:

- `EleutherAI/lm-evaluation-harness`
- Local checkout: `/Users/jevinnishioka/Desktop/lm-evaluation-harness`
- Commit: `1dd931087362abba74e0375c8c631295559f48b2`
- Task: `leaderboard_bbh_date_understanding`
- Task config: `lm_eval/tasks/leaderboard/bbh_mc/date_understanding.yaml`
- Template config: `lm_eval/tasks/leaderboard/bbh_mc/_fewshot_template_yaml`
- Dataset: `SaylorTwift/bbh`, config `date_understanding`, split `test`

Runtime:

- Nova image base: `nova-service:local`
- Experiment image: `lm-eval-bbh-date-nova:local`
- Experiment image id: `sha256:d235e3f6c4c1d4fc9170f4e0385cd3e5dae3fd03b0504883da143854ce97305c`
- Final full-run package: `lm_eval_bbh_date_nova_20260611_035353_839445`
- Final package digest: `sha256:cbf5b87035bc43f40250b0efd3213dbcdecabd96ccea7bd53c6338dda912cf1c`

Validation:

- `bucephalus doctor cookbook/lm-eval-bbh-date-nova/experiment.yaml --env-file .env --json`
  passed 13/13 preflight checks.
- Corrected smoke run `run_20260611_035327_342852_000001` completed 1/1
  trial with exact_match `1`.
- Full run `run_20260611_035354_499260_000001` completed 5/5 trials.
- Health view reported 5/5 trusted scores, 0 untrusted, 0 warnings, 0 errors.

Full-run score payload summary:

| case | prediction | target | exact_match | tokens_in | tokens_out |
| --- | --- | --- | ---: | ---: | ---: |
| `lm_eval_bbh_date_001` | `(B)` | `(B)` | 1 | 2415 | 113 |
| `lm_eval_bbh_date_002` | `(A)` | `(A)` | 1 | 2463 | 152 |
| `lm_eval_bbh_date_003` | `(C)` | `(B)` | 0 | 2447 | 139 |
| `lm_eval_bbh_date_004` | `(E)` | `(E)` | 1 | 2453 | 165 |
| `lm_eval_bbh_date_005` | `(B)` | `(B)` | 1 | 2441 | 143 |

Aggregate:

- Exact-match accuracy: `4/5 = 0.8`
- Answer extraction: `5/5`
- Tokens in: `12219`
- Tokens out: `712`
- Total tokens: `12931`
- Sum latency: `41352 ms`

Topology and product notes:

- This benchmark topology is much cleaner than AgentBench for Nova: static
  prompt in, generated answer out, deterministic grader out. No persistent task
  server, worker pool, or environment loop is required.
- The official leaderboard config is still not exactly this score. lm-eval's
  `leaderboard_bbh_date_understanding` is `output_type: multiple_choice` and
  reports `acc_norm` over answer choices. Nova is evaluated here as a
  generative CLI agent, so the recipe preserves the prompt/target but grades an
  extracted generated label. Treat this as an answer-extraction diagnostic, not
  an official Open LLM Leaderboard submission.
- Case `lm_eval_bbh_date_003` is interesting: the prompt says Jane and John
  married on Jan 2, 1958 and it is their 5-year anniversary today; ordinary
  reading makes today Jan 2, 1963 and one week later Jan 9, 1963, which Nova
  selected as `(C)`. The dataset target is `(B) 01/09/1961`. This appears to be
  a benchmark-data oddity in the selected BBH row, not a runner failure.
- Bucephalus still stores the trusted scalar grader values under
  `trial_conclusion_payload`; the run report's variant metric displays
  `trial_conclusion_payload` as a single payload-like primary metric rather than
  aggregating the declared scalar metrics. This is consistent with existing
  Bucephalus issue #95.
- `bucephalus views ... latest_agent_output` marks the candidate artifact as
  `invalid` even though the Nova result JSON is captured and all grader payloads
  are valid/trusted. That is an observability/product mismatch, not a scoring
  failure.

Corrected grader note:

- An earlier smoke run `run_20260611_035229_274427_000001` exposed a bug in the
  recipe's answer extractor: it matched the bare `A` in `A: (B)` before the
  parenthesized answer. The grader was fixed to prefer parenthesized labels and
  choose the final label if multiple labels are present, then the smoke and full
  runs above were rerun with the corrected grader.
