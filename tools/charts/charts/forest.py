"""Forest plot: per-variant point estimate + bootstrapped 95% CI.

Editorial direction. Value labels at subhead tier because the dot only shows
approximate position — the percentage is the reader's precise readout.
"""

from __future__ import annotations

from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

from brand import (
    SIZE, COLOR, FONT,
    apply_style, title_block, footer, save_pair,
)

NAME = "forest"
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
        # subtle CI rule
        ax.plot([r["lo"], r["hi"]], [i, i],
                color=COLOR["muted"], linewidth=0.9, zorder=2, alpha=0.9)
        ax.scatter([r["mean"]], [i], s=140, color=c,
                   edgecolors=COLOR["bg"], linewidths=2.0, zorder=5)
        ax.text(_value_x(tick_vals), i, _fmt(r["mean"], tick_fmt),
                va="center", fontsize=SIZE["subhead"], fontweight="bold",
                color=COLOR["ink"], family=FONT["serif_display"])

    ax.set_yticks(y)
    ax.set_yticklabels(summary["label"].tolist(),
                       fontsize=SIZE["body"])
    # small italic n under each variant label
    for i, r in summary.reset_index(drop=True).iterrows():
        ax.annotate(f"n = {r['n_gradeable']}",
                    xy=(0, i), xycoords=("axes fraction", "data"),
                    xytext=(-10, -14), textcoords="offset points",
                    ha="right", va="center",
                    fontsize=SIZE["caption"], style="italic",
                    color=COLOR["muted"], family=FONT["serif_body"],
                    annotation_clip=False)

    _setup_x_axis(ax, tick_fmt, tick_vals)

    ax.invert_yaxis()
    # leave room at the visual bottom so the last variant's `n = N` annotation
    # doesn't collide with the x-axis tick labels
    bottom, top = ax.get_ylim()
    ax.set_ylim(bottom + 0.30, top)
    ax.tick_params(left=False, bottom=False)
    ax.grid(axis="x")

    title_block(ax,
                eyebrow=ctx["eyebrow"],
                title=ctx["title"],
                subtitle=ctx["subtitle"])
    footer(fig,
           "Whiskers are bootstrapped 95% confidence intervals "
           "(5,000 resamples over gradeable trials).")
    save_pair(fig, out_dir, NAME)


def _value_x(tick_vals: list[float] | None) -> float:
    """Where to place value labels on the x-axis (just past the last tick)."""
    if tick_vals:
        return tick_vals[-1] + 0.04 * (tick_vals[-1] - tick_vals[0])
    return 1.04


def _fmt(value: float, fmt: str) -> str:
    return fmt.format(x=value)


def _setup_x_axis(ax, tick_fmt: str, tick_vals: list[float] | None) -> None:
    if tick_vals:
        ax.set_xlim(tick_vals[0], tick_vals[-1] * 1.18)
        ax.set_xticks(tick_vals)
        ax.set_xticklabels([_fmt(t, tick_fmt) for t in tick_vals],
                           fontsize=SIZE["body"])
    else:
        ax.tick_params(axis="x", labelsize=SIZE["body"])
    ax.set_xlabel("", labelpad=10)
