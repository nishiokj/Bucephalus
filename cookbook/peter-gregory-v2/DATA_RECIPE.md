# Peter Gregory — Data Recipe

**Version:** 0.1 · **Updated:** 2026-06-03 · **Status:** living inventory

The design invariants a Peter Gregory case must satisfy to be a *valid* latent-exposure
test — i.e. one whose answer can only be reached by reasoning about the company, not by
exploiting a surface artifact. Each was learned by finding a leak and closing it.

## Governing principle

> **NO-FREE-SIGNAL.** Across every channel the agent receives — state content, state
> sparsity, event text, event register, output schema, names, metadata — the *only* thing
> that may correlate with the answer is semantic relevance to the company. Any surface
> feature that predicts the answer (length, specificity, presence/absence, a label, a name)
> is a leak. Every invariant below is an instance of this principle in one channel.

**Gate legend:** ✅ automated check exists · ⏳ scriptable, not yet enforced · ✋ manual / design review.

## S — State channel

| Invariant | Rule | Gate |
|---|---|---|
| **SPARSITY-PARITY** | The state holds enough plausible distractor entities that an entity's mere *presence* never identifies the target. | ✋ |
| **BASELINE-COVERAGE** | Absent the event, every committed obligation is coverable from on-hand + qualified available supply that is **deliverable within the commitment window** — i.e. sufficient on *tons* **and** *grade/cert* **and** *timing* (harvest window / lead time), not just tons on paper. No standalone-arguable concrete exposure in static state. | ✅ window-aware coverage audit (per product/tender/program) |
| **UNASSUMING-STATE** | State carries facts (quantities, lead times, capacities, alternate-source rows), never verdicts (`constraint_status`, `do_not_prioritize`, `tight_but_irrelevant`, `substitutes:[]`-as-conclusion, editorial notes). | ⏳ grep for verdict fields |
| **NEUTRAL-API** | The data tool returns reachable facts, not adjudications: no `constrained_inventory` answer, no `reason: actionable_*`, no hardcoded event→answer map, no always-true coverage signals (`finished<requested`). | ⏳ assert no verdict keys in trace output |
| **FRAGILITY-IS-A-COEFFICIENT** | Single-source / no-substitute / concentration / long-lead are severity parameters, not alerts. Structure ≠ exposure. | ✋ |
| **NO-BAKED-OPPORTUNITY** | Never pre-state the actionable option (accelerated window+bonus, reservable-lane+shortfall); the event introduces the reason to act. | ✋ |
| **DISTRACTOR-NON-ACTIONABILITY** | No non-target entity carries the same actionable signal as the target; decoys are considered-and-dismissed, never valid second answers. | ✅ per-commodity actionability audit (procurement) |

## E — Event channel

| Invariant | Rule | Gate |
|---|---|---|
| **REGISTER-PARITY** | Signal, near-miss, and filler events share one distribution of length / specificity / entity-density / number-density / tone; a stylometric or length classifier sits at chance. | ✅ `blind_reader.py` (signal #1 in 0/4 positives) + length-rank check |
| **BLIND-READER-LITMUS** | A reader given the feed but *not* the company profile cannot pick the load-bearing event. | ✅ `scripts/blind_reader.py` (context-free reader, top-3 ranking) |
| **SPECIFIC-BUT-IRRELEVANT-FILLER** | Distractors have real texture (commodities/regions/firms/dates) pointing to the *wrong* place — not throwaway noise. | ✅ no-REACH scan (distractor never names a real input/customer) |
| **NOISE-DAY-PARITY** | The no-alert/noise case contains events as long/serious/specific as any positive's signal, so "a serious event exists ⇒ alert" doesn't hold. | ✅ noise feed carries ≥4 serious distractors; litmus picks are non-exposures |
| **GENRE-REALISM** | Parity is achieved *within* the feed's genre (a social feed stays social via variance + collisions, not uniform Bloomberg register). | ✋ |

## C — Composition (event × state)

| Invariant | Rule | Gate |
|---|---|---|
| **REACH ∧ LEVER ∧ FLIP** | Alert iff the event's most-downstream effect reaches a committed obligation/opportunity, a lever with slack exists, and the event flips that lever's payoff. All three required. | ✋ design review |
| **EVENT-LOAD-BEARING** | For positive cases, state-only ⇒ no-action; the event must be necessary. If the action is derivable from state alone, the event isn't doing work. | ✅ state-only run alert-rate on `expect_no_alert` cases |
| **ACTIONABILITY-BOUNDARY** | Actionable = a binding constraint breached *within the decision window*; standing strategic fragility is out of scope by design. | ✋ |
| **NO-LEVER-BAN** | An event landing on a capped line with no lever (no reallocation/substitute/expedite) is not a test — flag/exempt it, don't fake it. | ✋ (tag `leak_invariant: exempt`) |
| **REASONED-NOT-COMPUTED** | Composition is judgment over numbers (which move, roughly how much, reaching what), not a closed-form function of a noisy event. | ✋ design principle |

## D — Schema / instruction delivery

| Invariant | Rule | Gate |
|---|---|---|
| **INLINED-CONTRACT** | The output schema is inlined verbatim into the prompt; a file-path reference is a dead pointer (the agent can't read files). | ✅ grep no path ref |
| **CHANNEL-MATCHED-SCHEMA** | Per-arm schema matches available channels (no `causal_event_id` without an event; no state-grounded fields without state) so no required field forces fabrication. | ✋ structural |
| **CANONICAL-SCHEMA** | One schema file is the source; generators inline it; `embedded == canonical` is checked. | ✅ embedded==canonical |

## N — Identity / metadata

| Invariant | Rule | Gate |
|---|---|---|
| **NO-NAMING-TELLS** | Prompts/names/titles/tool-names must not telegraph the answer (e.g. "Peter Gregory" beside a cicada tweet). | ✅ grep: 0 "Peter Gregory" + 0 `pg_<tool>` in any prompt; tools renamed `cd_*`; smoke confirms they resolve |
| **ACCURATE-NAMES** | Scenario/case names describe their actual content (e.g. not `sesame_canonical` for a castor scenario). | ✋ |
| **NO-METADATA-BACKCHANNEL** | Arms drop fields that re-leak a removed channel (state-only drops `event_stream`/`feed_type`). | ✅ assert removed fields absent |

## P — Population / coverage

| Invariant | Rule | Gate |
|---|---|---|
| **POSITIVE-DIVERSITY** | Genuine event-load-bearing positives spread across exposure kinds (supply/demand/arbitrage/capacity), shock types (crop/logistics/regulatory/energy/labor), and product lines — no thin set, no thematic monoculture. | ✋ taxonomy review |
| **NO-ALERT-IS-FIRST-CLASS** | Zero-exposure is a valid correct answer; no-alert/near-miss traps require a minimum of considered-and-dismissed paths (empty = skim = soft fail). | ⏳ oracle `no_alert_paths_considered_minimum` |
| **TWO-AXIS-LABELS** | Every case carries `full_arm_verdict` (with event) and `leak_invariant` (state-only); never conflate them. | ✅ metadata presence check |

## X — Process / cross-arm

| Invariant | Rule | Gate |
|---|---|---|
| **CANONICAL-SYNC** | A shared corpus stays identical across all arms serving it; data fixes are canonical. | ✅ `diff -rq` full vs state-only |
| **DETERMINISTIC-DERIVATION** | Derived arms have one source of truth + idempotent generators. | ✅ regen → no diff |
| **ORACLE-CONSISTENCY** | Oracles are updated to match the data after every edit (verdict, revenue, coverage). | ✋ |

## Status snapshot (2026-06-03)

- **Holds, verified (8 of 9 cases — all except the no-lever case):** BASELINE-COVERAGE (now window-aware), DISTRACTOR-NON-ACTIONABILITY, UNASSUMING-STATE, NEUTRAL-API, NO-BAKED-OPPORTUNITY — confirmed live (state-only leak rate **0/16**). INLINED-CONTRACT, CANONICAL-SCHEMA, CHANNEL-MATCHED-SCHEMA, NO-METADATA-BACKCHANNEL, TWO-AXIS-LABELS, CANONICAL-SYNC, DETERMINISTIC-DERIVATION; NO-NAMING-TELLS (all prompts scrubbed of "Peter Gregory"; `pg_*` tools renamed `cd_*`, agent-visible surfaces clean; project name/dirs/experiment-id retain "Peter Gregory" but are not agent-visible), ACCURATE-NAMES (castor rename).
- **Event channel — holds, litmus passed:** REGISTER-PARITY / NOISE-DAY-PARITY / SPECIFIC-BUT-IRRELEVANT-FILLER / BLIND-READER-LITMUS. Feeds rewritten so the signal is no longer the length outlier (rank 3–4 of N) and a context-free reader ranks the true signal **#1 in 0/4 positives** (off top-3 in 2/4); near-miss keys are still salient (by design); noise-day picks are all non-exposures. No rewritten distractor reaches a real input/customer. Reader = `claude -p` (cross-model to the gpt-5.5 benchmark agent); re-run `scripts/blind_reader.py` to re-verify after any feed edit.
- **EVENT-LOAD-BEARING — verified live (state-only, 8/9 cases clean):** across the latest run the 8 `expect_no_alert` cases — castor, brand, gpu, and the five no-alert traps — leak in **0/16 trials**. Getting brand and gpu there took two real fixes (not exemptions):
  - **brand** leaked on decoy tenders that were unfillable on *tons*, then (after a tons-only fix) on *delivery timing* — a careful agent flagged "harvest_window Q4 can't fill a Q3 order." Covering every decoy order on tons **and** grade **and** in-window delivery closed it (was flaky 0,0 / 2,3 / 2,0 → 0,0). The event-activated reservable Indonesia hedge stays the with-event signal.
  - **gpu** leaked only because `finished_units ≪ requested_units` makes every program look short; giving each program a build pipeline (`finished + in-progress ≥ requested`, each with an input allocation) closed it (1,1 → 0,0). **It never needed an event-aware API** — the with-event signal is the same directional judgment as the others ("cable disruption → customer pulls capacity forward → long HBM lead → reallocate the in-pipeline memory").
- **Exempt — one case, genuine:** `customer_of_customer` (demand_pull, 1,1) — the event raises demand on a supply-capped line with **no available lever** (no reallocation/substitute/expedite), so there is no action to take. NO-LEVER-BAN; not a data weakness. Either give it a real lever or leave it flagged.
- **Known violations, open:** WITH-EVENT side unverified — the full arm (events present) has not been run, so "positives actually alert with the event, traps stay silent" is confirmed at data level only. POSITIVE-DIVERSITY — 3 clean event-load-bearing positives now (castor supply, brand arbitrage, gpu capacity) — better, but castor + brand are both crop shocks; want more non-agricultural variety. brand's clean result is one run after the window-fix (its history was flaky) — worth a second confirming run. Intermittent runtime flakes under disk pressure (docker 409 kill, occasional empty agent response).

## Changelog

- **v0.1.6 (2026-06-03)** — gpu fixed and promoted to a clean positive; brand's flaky leak closed; coverage invariant sharpened. gpu: gave each accelerator program a build pipeline (`finished + in-progress ≥ requested`, each with an input allocation) → 1,1 → 0,0 — corrected the earlier "needs event-aware API" claim (it doesn't). brand: the residual flake was a *delivery-window* gap (covering lanes had Q4 harvest / vegetative crop stage, can't fill a Q3 order) — fixed by making covering lanes deliverable-in-window → 0,0. **BASELINE-COVERAGE** refined to window-aware (tons × grade × timing). Latest state-only run: **8/9 cases clean, 0/16 leak**; only `customer_of_customer` exempt (genuine no-lever). `leak_invariant` exempt set is now just {demand_pull}.
- **v0.1.5 (2026-06-03)** — Brand fixed and promoted back to a clean positive. Covered every brand decoy order (bumped/qualified lanes to spot-available; added an organic-sesame spot lane) so no order is unfillable in static state; the only reservable lane stays the event-activated Indonesia sesame hedge. Second full state-only run: brand 2,3 → **0,0**; expect_no_alert leak rate **0/14** across 7 cases. `leak_invariant` rule refined — exempt = {demand_pull, capacity_reallocation}; procurement_arbitrage is clean. gpu + customer_of_customer remain exempt (no-lever / needs event-aware API).
- **v0.1.4 (2026-06-03)** — Full state-only run (9×2) as the live EVENT-LOAD-BEARING gate. The 6 `expect_no_alert` cases leaked 0/12. `brand_exposure_tweet` and `gpu_capacity_reallocation` alerted state-only (on uncovered decoy tenders / `finished≪requested` decoy program, not the intended target) → reclassified `exempt`; `leak_invariant` is now exposure-class-driven (the three opportunity kinds are exempt). Net: leak closed for supply-disruption + no-alert; opportunity class remains state-recoverable and exempt.
- **v0.1.3 (2026-06-03)** — NO-NAMING-TELLS closed: scrubbed spelled-out "Peter Gregory" from every prompt in all three arms; renamed the six MCP tools `pg_* → cd_*` (the `pg_001…` *case-id* prefix left intact); neutralized the `mcp.json` server key (`company_data`) and the FastMCP name; folded the missing `pg_009` into the full arm's `CaseId`. Nova + data-api images rebuilt; state-only smoke confirms the `cd_*` tools resolve (45–49 tool calls) and pg_001 state-only returns 0 alerts. Project name / dirs / experiment id keep "Peter Gregory" (not agent-visible).
- **v0.1.2 (2026-06-03)** — BLIND-READER-LITMUS run via `scripts/blind_reader.py` (context-free reader, shuffled+relabeled feeds). Surfaced a residual leak in pg_002 (Mississippi barge was the lone vivid logistics shock → still picked #1); added two company-irrelevant logistics/commodity shocks (cocoa port suspension, PNW port lockout) so it's one of several. Final: signal #1 in 0/4 positives. E-group gates marked passed.
- **v0.1.1 (2026-06-03)** — Event-feed rewrite pass for REGISTER-PARITY: signals taken off rank-1 in all 9 feeds (now rank 3–4) by enriching distractors into specific-but-irrelevant peers and giving the noise day equally-serious events; signal/near-miss text and oracle IDs preserved; no new REACH. See `scripts/rewrite_feeds.py` (one-shot).
- **v0.1 (2026-06-03)** — Initial inventory: 28 invariants in 8 groups, distilled from the state-leak hardening, the event-register finding, the schema-inlining fix, the castor rename, and the Peter Gregory scrub.
