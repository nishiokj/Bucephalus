# Experiment Runner Product Critiques

Perspective: user of Bucephalus/Experiments as both a product surface and a
developer tool, based on the AgentBench and lm-evaluation-harness runs.

## High-Level Take

The runner is powerful, but the trust layer is not yet boring enough. The
execution engine can package, run, grade, and persist real trials, but the user
experience around score provenance, metric extraction, and result interpretation
still has too many places where a valid run can look suspect.

The grader metric issue was especially concerning: the grader file existed, the
mapped payload was valid, the run was trusted, but declared metrics such as
`from: grader.score.exact_match` did not become first-class metric rows. That
kind of mismatch makes it hard to know whether the benchmark, the recipe, or the
runner is wrong.

## Critiques

### 1. Metric Semantics Should Be Boring

The expected user model is simple:

```text
grader writes score.json
metric says from: grader.score.exact_match
views show exact_match
primary metric is exact_match
```

There should be no hidden fallback to `trial_conclusion_payload`, no JSON object
displayed as a metric, and no need to inspect trial internals to confirm what
was scored.

If metric extraction fails, the error should be immediate and specific:

```text
Metric exact_match from grader.score.exact_match could not resolve.
Available grader.score keys: smoke_ok, exact_match, tokens_in, tokens_out.
```

### 2. The Runner Needs Metric Dataflow Explanations

There should be a command like:

```bash
bucephalus explain-metrics <run>
```

Useful output would show the full score path:

```text
exact_match
  declared in experiment.yaml:132
  source: grader.score.exact_match
  grader output file: /bucephalus/out/lm_eval_bbh_date_score.json
  captured: yes
  mapped into trial_conclusion_payload.exact_match: yes
  metric row emitted: yes
  scoreboard column: exact_match_mean
```

That would turn a spooky metric failure into a debuggable graph.

### 3. The Grader Contract Feels Too Indirect

The internal flow has many layers:

```text
grader output file
captured grader output
mapped_grader_output.json
trial_conclusion_payload
metrics_long
trial summary
views
```

Those layers may be justified internally, but the user-facing model should be
simpler. Most recipe authors should not need to know about
`trial_conclusion_v1`.

### 4. Views Need Better Diagnostic Behavior

The run report said trusted scores existed, but the variant metric displayed as
`trial_conclusion_payload | 0.0`. That is actively misleading.

If the runner cannot aggregate a metric, it should say something like:

```text
No primary scalar metric selected.
Available scalar metrics: exact_match, tokens_in, tokens_out, latency_ms.
```

Displaying a payload object as the metric should be avoided.

### 5. Candidate Artifact Status Is Confusing

`latest_agent_output` showed `candidate=invalid` even though the agent's result JSON
was captured and the grader succeeded. If "candidate artifact" means a patch or
file artifact, that should be separate from answer-mode agent result validity.

Suggested columns:

```text
agent_result: valid
candidate_artifact: none
grader_payload: valid
score: trusted
```

Use `invalid` only for actual invalidity, not for absence of an optional
artifact.

### 6. Authoring Syntax Drift Hurts

The installed CLI accepted the recipe shape, while the source-built CLI rejected
`/cases` as legacy and wanted `/matrix/cases`. That creates a bad product
feeling: the recipe is both valid and invalid depending on which binary is used.

Needed:

```bash
bucephalus migrate experiment.yaml
```

or at least an error with a concrete replacement:

```yaml
matrix:
  cases:
    source: file
    path: cases.jsonl
```

Ideally old syntax remains readable with warnings for one release window.

### 7. Doctor Should Catch Semantic Support Gaps

`doctor` passed even though the runtime extractor previously did not support
declared `grader_output` metrics. It should validate that each declared metric
source type is supported by the runtime path that will execute the trial.

Example check:

```text
grader.score.exact_match:
  output id score exists
  extraction backend supports grader_output: yes
  pointer /exact_match will be checked at runtime
```

### 8. Primary Metric Policy Needs To Be Clearer

If no primary metric is declared but scalar metrics exist, the runner should
either require one or choose a deterministic default with an explicit warning:

```text
No primary metric declared. Using first maximize metric: exact_match.
```

Falling back to a payload object is almost never what the user intended.

### 9. Benchmark Provenance Should Be First-Class

Recipes need structured support for benchmark identity:

```yaml
benchmark:
  name: lm-evaluation-harness
  upstream_repo: https://github.com/EleutherAI/lm-evaluation-harness
  upstream_commit: 1dd931087362abba74e0375c8c631295559f48b2
  task: leaderboard_bbh_date_understanding
  dataset: SaylorTwift/bbh
  split: test
```

Right now this lives in README files, run notes, and case metadata. It should be
part of the run's structured provenance.

### 10. Official vs Adapted Scores Should Be Explicit

The AgentBench run and the lm-eval BBH run both showed why this matters. A score
can be useful without being the official benchmark score.

Recipes should be able to declare:

```yaml
benchmark:
  source: lm-evaluation-harness
  task: leaderboard_bbh_date_understanding
  scoring_mode: adapted_generated_answer
  official_metric: false
  deviation: official task uses acc_norm; this run uses exact extracted label
```

Reports should then say "adapted score" or "derived score" rather than implying
leaderboard comparability.

### 11. Post-Run Score Inspection Should Be Simple

There should be a direct command:

```bash
bucephalus scores <run>
```

Expected output:

```text
case                  exact_match  prediction  target
lm_eval_bbh_date_001  1            (B)         (B)
lm_eval_bbh_date_002  1            (A)         (A)
lm_eval_bbh_date_003  0            (C)         (B)
mean                  0.8
```

Users should not need to inspect individual grader JSON files to answer "what
was the score?"

### 12. Errors Should Show Available Paths

For metric resolution, schema validation, captures, and output mapping, errors
should include the observed object shape or available keys.

Example:

```text
Could not resolve grader.score.key_recall.
Captured grader outputs:
  score: keys=[smoke_ok, key_recall, matched_keys, total_keys]
```

This would have exposed the grader metric extraction bug immediately.

## Wish List

1. Make declared grader metrics first-class everywhere: rows, summaries,
   scoreboard, reports.
2. Add `bucephalus explain-metrics <run>`.
3. Add `bucephalus scores <run>`.
4. Rename or split "candidate invalid" so answer-mode output validity is clear.
5. Require an explicit primary metric or auto-select one with a loud warning.
6. Add structured benchmark provenance and adaptation metadata.
7. Add `bucephalus migrate experiment.yaml` for authoring syntax drift.
8. Make `doctor` validate that declared metric source types are actually
   supported by the runtime extraction path.
9. Show exact file/path/key provenance for every score in the run report.
10. Distinguish official, adapted, derived, and smoke scores in the product UI.

## Product Direction

The runner should feel like a score ledger with provenance, not only a trial
executor.

The executor is already capable: it can package, isolate, run, grade, persist,
and inspect real experiments. The next layer of product quality is trust:

- Can I tell exactly where this score came from?
- Can I tell whether it is official or adapted?
- Can I tell whether a missing metric is my fault or the runner's?
- Can I compare runs without spelunking through trial directories?

That trust layer is where the user experience currently feels shaky, and it is
also where the product can become much more differentiated.
