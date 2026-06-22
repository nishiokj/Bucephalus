# Product Plan: The Eval Regression Ledger

_Status: direction-setting, 2026-06-20. This is a north-star plan, not a spec. It exists to recenter the work on the product after a long detour into infrastructure._

---

## The product in one sentence

**A trustworthy ledger of eval results that tells you, run after run, whether your model or agent actually got better or worse — and protects you from fooling yourself.**

Think "CI + observability, but for model and agent quality." Not a leaderboard. Not a place to run jobs. The place where eval results _live_, accrue over time, and become decisions you can defend.

---

## First principles (the non-negotiables)

These are the lenses every decision passes through. When in doubt, return here.

1. **The scarce resource in evals is trust, not compute.** The field is drowning in numbers nobody believes — contaminated benchmarks, irreproducible setups, harness drift, gamed scores. Our entire reason to exist is producing a number a skeptic will accept. We compete on trust. We do not compete on orchestration.

2. **The ledger is the product.** A content-addressed, append-only record of measurements with stable identity over time is the asset. Everything else — execution, UI, comparison — is in service of making a _delta in that ledger mean something_.

3. **Protect the user from false positives.** A number that moved is worthless until we can say _why_ and _whether it's real_. Noise, grader drift, environment drift, and contamination are the enemies. Defeating them is the feature.

4. **Compute is a means, never the product.** We own execution only because reproducibility demands it. We ride tried-and-true infrastructure underneath (Docker, Modal, k8s, Slurm as pluggable backends) and never try to out-build them. The litmus test for any infra work: _"Does this make a ledger delta more trustworthy, or does it just make the platform more operable?"_ The first is product; the second is our SRE console.

5. **Speak eval, not infra.** Users think in experiments, variants, cases, runs, trials, scores, baselines, regressions, and attestations. The moment a user has to learn "resources, operations, cordon, port-forward," the abstraction has leaked and we've become a worse Kubernetes.

---

## Where we actually are (the honest version)

- **The foundation exists and it's excellent.** The content-addressed fact ledger was the _first_ thing built and was designed explicitly for cross-run analysis. Every measurement carries stable identity — which task, which agent/model, which grader, which metric, which environment, and when. This is the expensive, get-it-wrong-and-you're-dead part, and it's done right. We built the vault.

- **The product layer on top was never built.** Baselines, regression detection over time, statistical honesty, guardrails, triage, and the experience to consume them — the things that turn the ledger into a product — do not exist yet.

- **We got pulled into a compute control plane.** The recent effort went into a low-level runtime/operations surface. It's needed _internally_, but it became the visible product and starved the layer that delivers the actual value. This plan corrects that.

**Bottom line: we built on rock, then wandered. The path forward is to build the middle — not to start over.**

---

## The end result (north star)

When this is working, a user's experience is:

- They run an eval as casually as they run tests. It's part of the loop, not a project.
- They get back a **verdict, not a number**: "capability held, cost regressed 9% — and here's the proof it's real, not noise."
- They can look at any metric **over weeks or months** and trust the line, because the environment, grader, and tasks underneath it are pinned and identical.
- They can set a **gate** — "block this if refusal-rate regresses more than 2%" — and have it enforced automatically.
- They can hand a result to a skeptic with an **attestation**, and the skeptic can reproduce the exact number.
- They never once think about runners, pods, or resources.

---

## The user journeys (this is the heart of the plan)

### 1. The regression gate — the daily dev loop (primary wedge)

A model/agent developer makes a change and wants to know if it helped before they ship.

> They run the eval against a pinned baseline. Minutes later they get a clear answer: **"3 metrics moved. 1 is a real regression (p < 0.05, 40 repeats); 2 are within noise."** It names the 7 cases that newly fail and links straight to their trajectories. The developer fixes the real one, ignores the noise, and ships with confidence.

The value: they **didn't fool themselves**. No eyeballing two averages and shipping a regression. This is the wedge — sticky, recurring, every loop.

### 2. Longitudinal tracking — the long horizon

A team wants to know if their agent is genuinely improving over a quarter, not just commit-to-commit.

> They open the trend for a capability and see a clean line across 200 runs over 3 months. The system flags one apparent jump as **environment drift** (a base image changed), not a real gain — and excludes it from the comparison. It flags another as a likely **contamination** risk after a model update. The team trusts the curve because everything beneath it is pinned and identical.

The value: a delta over time means a real change in the thing being measured — the single hardest guarantee in longitudinal evals, and the one our foundation was built to make.

### 3. Bring-your-own-task comparison — the decision

Someone needs to choose between models or agents on _their_ tasks, because public benchmarks are saturated and contaminated.

> They point the system at a private task set and a few candidates. They get an honest, apples-to-apples comparison — same tasks, same scoring, same budget, with significance attached and the multi-metric tradeoffs visible (capability up, but cost and latency up too). They walk away with a **reproducible attestation** they can defend to their boss or their auditors.

The value: a trustworthy comparison on tasks that matter, that someone else can reproduce.

### 4. Triage — "what can I actually take away from this run"

A number moved and the user needs to understand it, not just observe it.

> They drill from the aggregate into the cases that moved, see each agent's trajectory, and the system attributes the change: **model vs grader vs environment.** They discover it was the grader that drifted, not the model — and they would have shipped a phantom "improvement" otherwise.

The value: attribution. The answer to the question this whole product is named after.

_(Trust and reproducibility are not a separate journey — they run through all four. Every result is backed by a manifest; any result can be reproduced; nothing in the ledger is mistakable for an unverified internet number.)_

---

## What lifts us above "roll your own on Modal + Postgres"

The opinions _are_ the product. Each is a false-positive defense, and none requires us to be a compute platform:

- **Stable identity over time** — refuse or loudly flag comparisons across incompatible environments. (Foundation done.)
- **Noise vs signal** — repeats, variance, and sequential testing so "is this regression real _yet_" is cheap and honest.
- **Grader integrity** — pin and version graders; attribute deltas to grader vs model; red-team scorers for gameability.
- **Contamination tracking** — flag risk per (model, benchmark, time). Almost nobody does this.
- **Multi-metric guardrails** — track the vector, not a scalar; SLO-style gates make it CI, not a leaderboard.
- **Attestation + one-command reproduce** — turn the provenance we already capture into a credibility instrument.

---

## Boundaries & non-goals

- **The compute control plane stays internal.** Runtime resources, exec, port-forward, fleet operations are real and needed — as low-level building blocks that high-level, eval-native operations compose on top of. They are not public product surface, not headline features, and not exposed as a kubectl clone. We hold this line with the release-boundary tooling we already have.
- **We do not compete on orchestration.** Execution rides agnostic, battle-tested infrastructure.
- **We do not launder untrustworthy numbers.** Comparing against public/internet results is at most a gated, watermarked convenience — never presented next to our trustworthy entries as if equivalent.
- **We do not make users learn infrastructure.** If a workflow forces infra vocabulary on a user, it's a bug in the product, not a feature.

---

## The path from here (high-level, not a spec)

1. **Draw the boundary first.** Before the next release bundles it as committed surface, move the compute/fleet control plane out of the public contract and into internal/SRE space. Cheap now, expensive later.
2. **Reclaim the analysis surface.** The single-run analysis logic exists but its UI was removed and it's scoped to one run. Rebuild the "what can I take away" experience against the cross-run ledger.
3. **Build the longitudinal layer on the ledger we already have**, in order of leverage: first-class baselines → regression detection over time → statistical honesty → guardrails/gates → triage and attribution → the experience that ties them together.
4. **Close the one foundation gap.** Confirm grader identity is pinned per measurement (needed for grader-drift detection); extend if not.

Sequencing favors the wedge: the regression gate (Journey 1) is the first thing that should feel magical, because it's the highest-frequency, highest-trust moment in a user's day.

---

## How we'll know it's working

- A user runs an eval inside their normal dev loop without it being a project.
- The product routinely says "that's noise" or "that's real" — and is right.
- Teams keep months of history and trust the trend lines.
- People cite our attestations because others can reproduce them.
- Nobody outside the team ever needs to know what a "runner instance" is.
