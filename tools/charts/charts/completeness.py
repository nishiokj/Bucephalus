"""Data-quality chart: per-variant outcome breakdown.

success / failure are gradeable behavioral trials.
error / timeout are inference failures.
"""

from __future__ import annotations

from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

from brand import (
    SIZE, COLOR, FONT, OUTCOME_COLOR,
    apply_style, title_block, footer, save_pair,
    left_margin_for_labels, vertical_figsize, wrap_labels,
)

NAME = "completeness"
REQUIRES = "always applicable when ≥1 variant has any trial"


def applicable(ctx: dict) -> tuple[bool, str | None]:
    if ctx["n_variants"] < 1:
        return False, "no variants in data"
    if ctx["completeness"].empty:
        return False, "no completeness rows"
    return True, None


def render(ctx: dict, out_dir: Path) -> None:
    apply_style()
    summary = ctx["completeness"]

    y_labels = wrap_labels(summary["label"].tolist(), max_chars=24, max_lines=3)
    fig, ax = plt.subplots(figsize=vertical_figsize(len(summary), min_height=3.6))
    fig.subplots_adjust(
        top=0.84,
        left=left_margin_for_labels(y_labels, minimum=0.20, maximum=0.44),
        right=0.97,
        bottom=0.16,
    )

    y = np.arange(len(summary))
    left = np.zeros(len(summary))
    for cat in ["success", "failure", "error", "timeout"]:
        w = summary[cat].to_numpy()
        for i, wi in enumerate(w):
            if wi == 0:
                continue
            ax.barh(i, wi, left=left[i], height=0.58,
                    color=OUTCOME_COLOR[cat],
                    edgecolor=COLOR["bg"], linewidth=1.6, zorder=3)
            ax.text(left[i] + wi / 2, i, str(int(wi)),
                    ha="center", va="center",
                    fontsize=SIZE["body"], color="white",
                    family=FONT["serif_display"], fontweight="bold")
        left += w

    ax.set_yticks(y)
    ax.set_yticklabels(y_labels,
                       fontsize=SIZE["body"], family=FONT["serif_body"])
    ax.set_xlabel("Trials", labelpad=10, fontsize=SIZE["body"])
    ax.invert_yaxis()
    ax.grid(axis="x")
    ax.tick_params(left=False, bottom=False)

    # inline legend, top-right of axes
    x0 = 0.48
    legend_y = 1.01
    for j, cat in enumerate(["success", "failure", "error", "timeout"]):
        xj = x0 + j * 0.13
        ax.add_patch(plt.Rectangle(
            (xj, legend_y), 0.018, 0.045,
            transform=ax.transAxes,
            color=OUTCOME_COLOR[cat], clip_on=False,
        ))
        ax.text(xj + 0.025, legend_y + 0.022, cat,
                transform=ax.transAxes,
                fontsize=SIZE["caption"], color=COLOR["ink"],
                va="center", family=FONT["serif_body"], style="italic")

    title_block(ax,
                eyebrow=ctx["eyebrow"],
                title="Trial outcome completeness",
                subtitle=ctx["subtitle"])
    footer(fig,
           "success / failure are gradeable behavioral trials.  "
           "error / timeout are inference failures.")
    save_pair(fig, out_dir, NAME)
