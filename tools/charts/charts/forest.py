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
    left_margin_for_labels, vertical_figsize, wrap_labels,
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

    y_labels = wrap_labels(summary["label"].tolist(), max_chars=24, max_lines=3)
    fig, ax = plt.subplots(figsize=vertical_figsize(len(summary)))
    fig.subplots_adjust(
        top=0.84,
        left=left_margin_for_labels(y_labels, minimum=0.22, maximum=0.46),
        right=0.93,
        bottom=0.16,
    )

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
        ax.annotate(f"{_fmt(r['mean'], tick_fmt)}  n={r['n_gradeable']}",
                    xy=(r["mean"], i), xytext=(8, 0),
                    textcoords="offset points",
                    va="center", ha="left",
                    fontsize=SIZE["body"], fontweight="bold",
                    color=COLOR["ink"], family=FONT["serif_display"],
                    annotation_clip=False)

    ax.set_yticks(y)
    ax.set_yticklabels(y_labels,
                       fontsize=SIZE["body"])

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


def _fmt(value: float, fmt: str) -> str:
    return fmt.format(x=value)


def _setup_x_axis(ax, tick_fmt: str, tick_vals: list[float] | None) -> None:
    if tick_vals:
        rng = tick_vals[-1] - tick_vals[0]
        ax.set_xlim(tick_vals[0] - 0.03 * rng, tick_vals[-1] + 0.08 * rng)
        ax.set_xticks(tick_vals)
        ax.set_xticklabels([_fmt(t, tick_fmt) for t in tick_vals],
                           fontsize=SIZE["body"])
    else:
        ax.tick_params(axis="x", labelsize=SIZE["body"])
    ax.set_xlabel("", labelpad=10)
