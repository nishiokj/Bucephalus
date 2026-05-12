# Highest-Leverage Points from User Review

Analysis of `user_review.md` against the actual state of `docs/user/` and the Rust runner code (`rust/crates/lab-runner/src/`).

## Tier 1 — small writes that unblock the entire onboarding (docs only)

1. **A "bring-your-own-agent" walkthrough using Python + Anthropic.** This is the ICP and the only example is a deterministic Node demo baked into `node:20-alpine`. The runtime contract (`agent-runtime-contract.md`) is clear enough that a 30-line example would write itself: `python:3.11-slim` task image, `artifact: ./agent`, an `agent.py` that reads `AGENTLAB_TRIAL_INPUT_PATH`, calls Anthropic, writes `trial_output_v1` to `AGENTLAB_RESULT_PATH`. Without this, the doc set is technically complete and operationally useless to a stranger. Highest leverage by far.

2. **Document `variant_plan` with one A/B example.** `what-you-provide.md:38` shows `variant_plan: []`. The demo (`demos/experiment.yaml`) actually has a `treatment` variant with bindings, so the data is right there to copy. The whole "compare model A vs B" pitch is invisible until this is on the Concepts page. Cheap, very high leverage.

3. **Pick a CLI name.** `quickstart.md` builds `lab-cli` and uses `"$LAB"` everywhere; every other page (`troubleshooting.md`, `env-and-secrets.md`, `inspecting-results.md`) writes `lab`. Either add `alias lab="$LAB"` as step 1.5 of the quickstart, or globally rename the binary. Cosmetic but it's the first wall a reader hits.

4. **Link the schemas.** `task_row_v1`, `trial_output_v1`, `trial_conclusion_v1` are name-dropped on every page and never linked. `schemas/` exists in the repo — point at it. One PR, removes a recurring "what fields exist?" tax.

## Tier 2 — the pitch is broken until these are fixed

5. **Cross-run DuckDB story.** `inspecting-results.md` only shows `lab query <run_id>`. Each run has `.lab/runs/<run_id>/run.sqlite`. There is no `lab query --all` and no documented `ATTACH` pattern. The whole "durable, queryable across runs" pitch in `index.md` evaporates without this. Could be solved by docs (an `ATTACH` cookbook) cheaply, or by a `lab query --runs <ids…>` flag for real leverage. Either way this is a credibility hole, not a polish item.

6. **Grader runtime + secrets.** `env-and-secrets.md` only addresses agent env. The reviewer's question — does an LLM-judge grader get `--env`, does it have network — has no answer in the docs. Worth checking the actual code path; if grader env doesn't propagate today, that's a real API gap, not just a docs gap. LLM-judge graders are common enough that this needs an explicit yes/no.

## Tier 3 — API surface that's lying to users

7. **`provider:` and `materialization.kind:` are decorative.** The Rust enums are `TaskMaterializationKind { TaskImage, BaseImageBundle }` and `GradingStrategy { InTaskImage, Injected, Separate }` (`model.rs:716`, `trial/spec.rs:10`), but only `task_image` / `in_task_image` are surfaced/used (`config.rs:417` only matches `InTaskImage`). And `provider:` only takes `local_jsonl` anywhere in tests. Either land the other variants and document them, or remove the polymorphism. Right now they're polymorphism shaped like a promise.

8. **`describe` vs `preflight`.** Both in the golden path with one-liners. If `preflight` is a strict superset (which is what the descriptions imply), collapse `describe` into `preflight --explain` and cut it from the quickstart.

## Tier 4 — real feature gaps from the wishlist worth taking seriously

The wishlist items I'd actually weight high (vs nice-to-have):

- **Built-in LLM-judge grader** — biggest force-multiplier. Removes the "write Python to grade prompts" tax that blocks the casual user the reviewer represents.
- **Resume/retry** (`lab run --resume`) — critical for any grader that calls a rate-limited API.
- **Token/cost view out of the box** — every LLM eval needs it, and it's a 50-line SQL view over the `model_call_end` events the runner already collects (visible in `demos/experiment.yaml`).
- **Cache the agent step** — `--reuse-agent-results-from <run_id>` makes grader iteration affordable; otherwise every grader change costs a full re-run.

Of those four, the LLM-judge grader and resume probably have the highest workflow impact; cost panel is the easiest given the events are already being collected.

## What I would not prioritize

The wishlist items for `lab tail`, `lab compare`, dataset hashing, and a landing-page diagram are all real but are polish on a flow that's currently blocked by Tier 1. Don't reorder until the bring-your-own-agent walkthrough exists.

## Bottom line

The reviewer is right that the docs "stop one layer short." The cheapest unblock is **Tier 1** (BYO-agent walkthrough + `variant_plan` example + CLI name + schema links) — that's probably a day of writing and removes most of the onboarding friction without touching code. **Tier 2** (cross-run query story + grader env) is where the product's stated value prop either holds up or doesn't, and is worth investigating in code before deciding whether it's a docs fix or a real gap.
