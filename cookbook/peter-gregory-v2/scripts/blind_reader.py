#!/usr/bin/env python3
"""BLIND-READER-LITMUS (Data Recipe E-group acceptance test).

Hand each event feed to a fresh model with NO company profile, NO oracle, events
shuffled and relabeled to neutral numbers. Ask it to rank the 3 items most likely to be
a material latent exposure. If the true signal is the runaway #1 pick, the event channel
still leaks salience; if it's buried among several plausible peers, register parity holds.

Interpretation:
  POSITIVE cases  -> signal should NOT be the dominant pick (want rank>1, ideally off top-3).
  NEAR-MISS cases -> the key event is *designed* to look load-bearing; a blind pick of it is
                     expected (company grounding is what dismisses it). Reported, not scored.
  NOISE case      -> picks should be diffuse / NONE.

Reader: `claude -p` run from /tmp (no project context). Chance of a #1 hit ~ 1/N.
"""
from __future__ import annotations
import json, re, random, subprocess, pathlib, sys

random.seed(7)
ROOT = pathlib.Path(__file__).resolve().parent.parent
SIGNAL = {"pg_001":"H03","pg_002":"E03","pg_003":"R04","pg_004":"T05","pg_005":None,
          "pg_006":"H05","pg_007":"E04","pg_008":"R06","pg_009":"I02"}
KIND = {"pg_001":"POSITIVE","pg_002":"POSITIVE","pg_004":"POSITIVE","pg_009":"POSITIVE",
        "pg_003":"near-miss","pg_006":"near-miss","pg_007":"near-miss","pg_008":"near-miss","pg_005":"noise"}

PROMPT = """You are a supply-chain and business-risk analyst. Below are {n} items from a daily external monitoring feed, in no particular order. Without assuming any specific company, identify the items most likely to represent a MATERIAL LATENT BUSINESS EXPOSURE — a supply, demand, regulatory, logistics, or capacity risk that could materially hurt some company watching this feed. Most items are likely noise.

Items:
{items}

Respond with ONLY one line, most-likely first: TOP3=<n>,<n>,<n>
If truly nothing stands out above the rest, respond: TOP3=NONE"""


def parse_events(feed: str):
    evs = []
    for chunk in re.split(r"\n## ", feed):
        m = re.match(r"(\w+)", chunk)
        if not m:
            continue
        body = chunk.split("\n", 1)[1].strip() if "\n" in chunk else ""
        if body:
            evs.append((m.group(1), body))
    return evs


def ask(prompt: str) -> str:
    out = subprocess.run(["claude", "-p", prompt], cwd="/tmp",
                         capture_output=True, text=True, timeout=180)
    return out.stdout.strip()


def main():
    rows = [json.loads(l) for l in (ROOT / "cases.jsonl").read_text().splitlines() if l.strip()]
    print(f"{'case':8} {'kind':9} {'N':>2}  signal  reader_top3(orig)        signal_rank")
    pos_top1 = pos_top3 = pos_total = 0
    for r in rows:
        cid = r["id"]; evs = parse_events(r["inputs"]["event_stream"])
        order = list(range(len(evs))); random.shuffle(order)
        # neutral label i+1 -> original event id
        label2orig = {i + 1: evs[order[i]][0] for i in range(len(evs))}
        items = "\n".join(f"{i+1}. {evs[order[i]][1]}" for i in range(len(evs)))
        resp = ask(PROMPT.format(n=len(evs), items=items))
        m = re.search(r"TOP3\s*=\s*([0-9,\s]+|NONE)", resp, re.I)
        picks = []
        if m and "NONE" not in m.group(1).upper():
            picks = [int(x) for x in re.findall(r"\d+", m.group(1))][:3]
        top3_orig = [label2orig.get(p, "?") for p in picks]
        sig = SIGNAL[cid]
        rank = (top3_orig.index(sig) + 1) if sig in top3_orig else (">3" if picks else "NONE")
        print(f"{cid:8} {KIND[cid]:9} {len(evs):>2}  {str(sig):6}  {str(top3_orig):24} {rank}")
        if KIND[cid] == "POSITIVE":
            pos_total += 1
            if top3_orig[:1] == [sig]: pos_top1 += 1
            if sig in top3_orig: pos_top3 += 1
    print(f"\nPOSITIVES (the cases that matter): signal ranked #1 in {pos_top1}/{pos_total}, "
          f"in top-3 in {pos_top3}/{pos_total}  (chance #1 ~ 1/N ≈ 9%)")
    print("Lower = better register parity (signal not blind-identifiable without the company).")


if __name__ == "__main__":
    main()
