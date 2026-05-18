"""Vertical bar chart with subtle error bars and body-tier value labels.

Per the style invariants: bar height conveys the value, so the inline
percentage is at body tier (not subhead).
"""

from __future__ import annotations

from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

from brand import (
    SIZE, COLOR, FONT,
    apply_style, title_block, footer, save_pair,
    bottom_margin_for_labels, horizontal_figsize, wrap_labels,
)

NAME = "bar"
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

    x_labels = wrap_labels(summary["label"].tolist(), max_chars=18, max_lines=3)
    fig, ax = plt.subplots(figsize=horizontal_figsize(len(summary)))
    fig.subplots_adjust(
        top=0.72,
        left=0.10,
        right=0.97,
        bottom=bottom_margin_for_labels(x_labels),
    )

    x = np.arange(len(summary))
    means = summary["mean"].fillna(0).to_numpy()
    has_data = summary["n_gradeable"].gt(0).to_numpy()
    err_lo = np.where(has_data, means - summary["lo"].fillna(0).to_numpy(), 0)
    err_hi = np.where(has_data, summary["hi"].fillna(0).to_numpy() - means, 0)
    colors = [ctx["variant_color"][v] for v in summary["variant_id"]]

    ax.bar(x, means, color=colors, width=0.52,
           edgecolor=COLOR["bg"], linewidth=2, zorder=3)
    ax.errorbar(x, means, yerr=[err_lo, err_hi], fmt="none",
                ecolor=COLOR["muted"], capsize=0, elinewidth=0.9,
                alpha=0.9, zorder=4)

    base = tick_vals[0] if tick_vals else 0.0
    axis_range = (tick_vals[-1] - tick_vals[0]) if tick_vals else 1.0
    for i, (m, has) in enumerate(zip(means, has_data)):
        if not has:
            continue
        if (m - base) >= 0.20 * axis_range:
            ax.text(i, base + (m - base) / 2, _fmt(m, tick_fmt),
                    ha="center", va="center", fontsize=SIZE["body"], fontweight="bold",
                    color=COLOR["bg"], family=FONT["serif_display"])
        else:
            ax.annotate(_fmt(m, tick_fmt), xy=(i, m),
                        xytext=(0, 4), textcoords="offset points",
                        ha="center", va="bottom", fontsize=SIZE["body"], fontweight="bold",
                        color=COLOR["ink"], family=FONT["serif_display"],
                        annotation_clip=False)

    ax.set_xticks(x)
    ax.set_xticklabels(x_labels,
                       fontsize=SIZE["body"], color=COLOR["ink"],
                       family=FONT["serif_body"])
    # italic n underneath each variant label
    for i, n in enumerate(summary["n_gradeable"]):
        ax.annotate(f"n = {n}",
                    xy=(i, 0), xycoords=("data", "axes fraction"),
                    xytext=(0, -42), textcoords="offset points",
                    ha="center", va="top",
                    fontsize=SIZE["caption"], style="italic",
                    color=COLOR["muted"], family=FONT["serif_body"],
                    annotation_clip=False)

    _setup_y_axis(ax, tick_fmt, tick_vals)
    ax.grid(axis="y")
    ax.tick_params(left=False, bottom=False)

    title_block(ax,
                eyebrow=ctx["eyebrow"],
                title=ctx["title"],
                subtitle=ctx["subtitle"])
    footer(fig,
           "Whiskers are bootstrapped 95% confidence intervals "
           "(5,000 resamples over gradeable trials).")
    save_pair(fig, out_dir, NAME)


# --- helpers ---

def _fmt(value: float, fmt: str) -> str:
    return fmt.format(x=value)


def _setup_y_axis(ax, tick_fmt: str, tick_vals: list[float] | None) -> None:
    if tick_vals:
        rng = tick_vals[-1] - tick_vals[0]
        ax.set_ylim(tick_vals[0], tick_vals[-1] + 0.08 * rng)
        ax.set_yticks(tick_vals)
        ax.set_yticklabels([_fmt(t, tick_fmt) for t in tick_vals],
                           fontsize=SIZE["body"])
    else:
        ax.tick_params(axis="y", labelsize=SIZE["body"])
    ax.set_ylabel("", labelpad=10)
