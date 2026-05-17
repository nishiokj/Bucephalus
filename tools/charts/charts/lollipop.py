"""Lollipop chart: stem + head + value. No CI overlay — the chart is about
clarity of the point estimate. CI lives on the forest plot.
"""

from __future__ import annotations

from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

from brand import (
    SIZE, COLOR, FONT,
    apply_style, title_block, footer, save_pair,
)

NAME = "lollipop"
REQUIRES = "≥2 variants, ≥1 gradeable trial somewhere"


def applicable(ctx: dict) -> tuple[bool, str | None]:
    if ctx["n_variants"] < 2:
        return False, f"need ≥2 variants (got {ctx['n_variants']})"
    if ctx["n_gradeable"] < 1:
        return False, "no gradeable trials"
    return True, None


def render(ctx: dict, out_dir: Path) -> None:
    apply_style()
    summary = ctx["summary"]
    tick_fmt = ctx["tick_format"]
    tick_vals = ctx["tick_values"]

    fig, ax = plt.subplots(figsize=(8.8, max(3.6, 0.6 * len(summary) + 2.0)))
    fig.subplots_adjust(top=0.76, left=0.22, right=0.93, bottom=0.18)

    y = np.arange(len(summary))
    for i, r in summary.reset_index(drop=True).iterrows():
        if r["n_gradeable"] == 0:
            ax.text(0.02, i, "no gradeable trials",
                    va="center", fontsize=SIZE["caption"],
                    color=COLOR["muted"], style="italic",
                    family=FONT["serif_body"])
            continue
        c = ctx["variant_color"][r["variant_id"]]
        # stem
        base = tick_vals[0] if tick_vals else 0
        ax.plot([base, r["mean"]], [i, i],
                color=c, linewidth=2.0, alpha=0.85, zorder=3)
        # head
        ax.scatter([r["mean"]], [i], s=140, color=c,
                   edgecolors=COLOR["bg"], linewidths=2.0, zorder=5)
        # value
        vx = (tick_vals[-1] + 0.04 * (tick_vals[-1] - tick_vals[0])) if tick_vals else 1.04
        ax.text(vx, i, tick_fmt.format(x=r["mean"]),
                va="center", fontsize=SIZE["subhead"], fontweight="bold",
                color=COLOR["ink"], family=FONT["serif_display"])

    ax.set_yticks(y)
    ax.set_yticklabels(summary["label"].tolist(), fontsize=SIZE["body"])
    for i, r in summary.reset_index(drop=True).iterrows():
        ax.annotate(f"n = {r['n_gradeable']}",
                    xy=(0, i), xycoords=("axes fraction", "data"),
                    xytext=(-10, -14), textcoords="offset points",
                    ha="right", va="center",
                    fontsize=SIZE["caption"], style="italic",
                    color=COLOR["muted"], family=FONT["serif_body"],
                    annotation_clip=False)

    if tick_vals:
        ax.set_xlim(tick_vals[0], tick_vals[-1] * 1.18)
        ax.set_xticks(tick_vals)
        ax.set_xticklabels([tick_fmt.format(x=t) for t in tick_vals],
                           fontsize=SIZE["body"])
    ax.invert_yaxis()
    # leave room at the visual bottom for the last variant's `n = N` annotation
    bottom, top = ax.get_ylim()
    ax.set_ylim(bottom + 0.30, top)
    ax.tick_params(left=False, bottom=False)
    ax.grid(axis="x")

    title_block(ax,
                eyebrow=ctx["eyebrow"],
                title=ctx["title"],
                subtitle=ctx["subtitle"])
    footer(fig,
           "Point estimates over gradeable trials.  "
           "See forest plot for 95% confidence intervals.")
    save_pair(fig, out_dir, NAME)
