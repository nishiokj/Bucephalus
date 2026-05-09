# First-time user feedback on AgentLab

I read through `index.md` → `troubleshooting.md` in the order you suggested. Below is what I'd be doing, what blocked me, and what I'd want next. I have an existing agent (Python, calls Anthropic API, runs in a container) and want to benchmark it on (a) a custom JSONL of prompts I have lying around, and (b) something SWE-bench-shaped later.

## Checklist — what I think I have to do to land my first run

1. Build `lab-cli` from source. (Already a friction point — see below.)
2. Bring my agent up as a container image and push it somewhere `image:` can pull from, or figure out if there's a local-build path.
3. Decide what goes in `artifact: ./agent` vs. baked into the image. The docs are fuzzy on the split.
4. Write a `tasks.jsonl` with `task_row_v1` rows. For my prompts I'd probably want `image: python:3.11-slim` and a `materialization.kind: task_image`.
5. Write a grader that reads `AGENTLAB_GRADER_INPUT_PATH` and writes `trial_conclusion_v1`. For my freeform prompts this is going to be an LLM-judge — and I have no idea where that grader runs (in the task image? does it have network? can it call Anthropic?).
6. Write `experiment.yaml`. Copy the minimal shape, fill in my command/image/grader.
7. `build` → `describe` → `preflight` → `run` → `views`/`query`.
8. Re-run with one variable changed (model, temperature) and learn how `variant_plan` works — currently a black box because the example is `[]`.
9. Eventually figure out how to query *across* runs in DuckDB, since that was the pitch.

## Unclear from the docs

**CLI name.** `quickstart.md` calls it `lab-cli`, every other page says `lab`. Is `lab` a shortened alias I'm supposed to set up, or the actual binary name? Either alias it in step 1 of the quickstart or pick one.

**Bring-your-own-agent walkthrough is missing.** Every page assumes I have a containerized agent already. The demo uses a Node app baked into `node:20-alpine`. I don't see anywhere that explains:
- Do I need to `docker build` and `docker push` myself before `image:` will resolve, or does the runner build for me?
- What's the contract between `artifact:` and `image:`? Is `artifact` overlaid on top of the image at `/opt/agent`? If yes, why does the demo also bake the harness into the image?
- Can I point `artifact:` at a tarball? A git ref? Docs say "repo dir, tarball, or packaged runtime files" but show no example beyond `./agent`.

**`variant_plan` is the most important field and is undocumented.** The whole reason I'd use this is to compare model A vs. model B. The example is `variant_plan: []`. There is no schema, no example with two variants, no link. `lab views <run_id> comparison_summary` is mentioned but the variant_plan that produces a comparison is invisible.

**Cross-run analysis is the pitch but absent from the docs.** `inspecting-results.md` only shows queries scoped to one `<run_id>`. Where does the durable, cross-run DuckDB layer live? Is there a `lab query --all` or do I attach `.lab/runs/*/run.sqlite` myself? If the value prop is "many runs over time," I should land on a page about that within five minutes of reading.

**Grader runtime is ambiguous.** `strategy: in_task_image` is the only strategy shown. What else exists? My LLM-judge grader needs network and an API key — does that propagate from `--env`, or are grader env vars separate? `env-and-secrets.md` only talks about agent runtime env.

**`integration_level: cli_basic` vs `cli_events`.** The contract page mentions both but doesn't tell me when I'd care. If `cli_events` is what gives me token counts and step counts (implied at the bottom of `agent-runtime-contract.md`), say that up front — that's table-stakes for anyone evaluating an LLM agent.

**`materialize: full` vs `outputs_only`.** Mentioned in troubleshooting under "storage growth" but not actually defined. What's in `full` that isn't in `outputs_only`? A table would fix it.

**Replications, concurrency, retries.** `replications: 1` and `max_concurrency: 1` appear with no explanation. What happens on a flaky trial? Is there an automatic retry, or do I re-run? Can I retry only failed trials of a previous run?

**Dataset providers.** `provider: local_jsonl` implies others exist. Which? HF datasets? S3? If `local_jsonl` is the only one, drop the field.

**`materialization.kind: task_image`.** Same — implies other kinds. What about a git-repo materialization for SWE-bench-style tasks? The demo says "a real coding-agent benchmark would instead provide a task image with repository state, tests, and grading logic" — so I have to bake everything into a task image per instance? That's a lot of images. Is there guidance?

**Schemas.** `task_row_v1`, `trial_output_v1`, `trial_conclusion_v1` are referenced constantly but never linked. I want a JSON schema or at least a complete field list, especially for `trial_output_v1.objective` vs `metrics` (when do I use which?).

**`describe` vs `preflight`.** Both are in the quickstart with one-line descriptions. I'd cut `describe` from the golden path or explain why it's separate from `preflight` (which seems to do everything `describe` does plus more).

## Wishlist

1. **A "bring your own agent" tutorial.** Take a 30-line Python script that calls Anthropic, walk it through to a passing trial. The SWE-bench demo is too much machinery for first contact.
2. **Cross-run DuckDB cookbook.** "Compare resolved-rate across the last 10 runs of variant `claude-4-7` vs `gpt-5.3`." If I can't write this query in five minutes, the storage layer isn't paying for itself.
3. **Variant plan examples.** At minimum a sweep over model + temperature, and an A/B with one binding changed.
4. **Resume / retry.** `lab run --resume <run_id>` to retry only failed trials. Critical when an LLM grader rate-limits halfway through.
5. **Cache the agent step.** If I'm iterating on the grader (very common), I shouldn't have to re-pay the agent's API cost. Some kind of `--reuse-agent-results-from <run_id>`.
6. **Live tail.** `lab tail <run_id>` to watch trials as they execute. Right now I'd be `tail -f`-ing files in `.lab/runs/...`.
7. **Cost / token panel out of the box.** Standard view that sums `usage.input_tokens` / `usage.output_tokens` / dollar estimate per variant. Every LLM eval needs this; nobody wants to write the SQL.
8. **Built-in LLM-judge grader.** A library grader where I supply a rubric prompt and a model id, no Python required.
9. **Dataset hashing.** `manifest.json` should include a hash of the task rows so I can prove two runs scored the same dataset.
10. **Versioned diff between two runs.** `lab compare <run_a> <run_b>` showing per-task deltas, not just per-variant aggregates.
11. **Make `lab` actually be the binary name** (or document the alias step).
12. **A landing-page diagram.** The "five moving parts" table is good, but a one-image data-flow (YAML + tasks.jsonl → build → trial container → result.json → grader → conclusion → DuckDB) would save a re-read of the contract page.

The shape of this thing is exactly what I want — sealed builds, separation of agent/grader, durable storage, SQL over runs. The docs just stop one layer short of letting a stranger land their first run with their own code.
