# lm-evaluation-harness BBH Date Understanding Nova

This recipe runs Nova against a small slice of the open-source
`lm-evaluation-harness` Open LLM Leaderboard BBH `date_understanding` task.

Benchmark source:

- Upstream repo: `https://github.com/EleutherAI/lm-evaluation-harness`
- Local checkout: `/Users/jevinnishioka/Desktop/lm-evaluation-harness`
- Tested checkout: `1dd931087362abba74e0375c8c631295559f48b2`
- Task config: `lm_eval/tasks/leaderboard/bbh_mc/date_understanding.yaml`
- Template config: `lm_eval/tasks/leaderboard/bbh_mc/_fewshot_template_yaml`
- Dataset: `SaylorTwift/bbh`, `date_understanding`, `test`

The official leaderboard task is `output_type: multiple_choice` and reports
`acc_norm` over answer choices. Nova is a generative CLI agent, so this recipe
preserves the task prompt and target label but grades the extracted generated
answer with exact match. That makes the result a faithful static-task diagnostic,
not a canonical lm-eval leaderboard submission.

## Files

- `experiment.yaml` declares the Bucephalus experiment.
- `Dockerfile.nova-runtime` extends the local `nova-service:local` image with
  Python for grading.
- `agent/nova-config.json` constrains Nova to answer-only behavior with no
  external tools.
- `scripts/generate_cases.py` generates cases from the upstream Hugging Face
  BBH dataset using the lm-eval leaderboard few-shot examples.
- `grader/grade_bbh_date.py` extracts a choice label from Nova's response and
  compares it to the benchmark target.
- `cases.jsonl` is generated from the upstream dataset.

## Run

```bash
docker build -t lm-eval-bbh-date-nova:local \
  -f cookbook/lm-eval-bbh-date-nova/Dockerfile.nova-runtime \
  cookbook/lm-eval-bbh-date-nova

bucephalus dev cookbook/lm-eval-bbh-date-nova/experiment.yaml \
  --env-file .env

bucephalus run cookbook/lm-eval-bbh-date-nova/experiment.yaml \
  --env-file .env
```
