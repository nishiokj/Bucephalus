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

    fig, ax = plt.subplots(figsize=(max(7.5, 1.8 * len(summary) + 2), 4.6))
    fig.subplots_adjust(top=0.74, left=0.10, right=0.96, bottom=0.22)

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

    for i, (m, has) in enumerate(zip(means, has_data)):
        if not has:
            continue
        top = max(m, summary["hi"].iloc[i])
        ax.text(i, top + _ymargin(tick_vals), _fmt(m, tick_fmt),
                ha="center", fontsize=SIZE["body"], fontweight="bold",
                color=COLOR["ink"], family=FONT["serif_display"])

    ax.set_xticks(x)
    ax.set_xticklabels(_two_line_labels(summary["label"].tolist()),
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

def _ymargin(tick_vals: list[float] | None) -> float:
    if tick_vals:
        return 0.03 * (tick_vals[-1] - tick_vals[0])
    return 0.03


def _fmt(value: float, fmt: str) -> str:
    return fmt.format(x=value)


def _two_line_labels(labels: list[str]) -> list[str]:
    """Wrap labels onto two lines only when needed.

    Threshold scales with the longest label in the set — if everything is
    short, nothing wraps; if any label is long, wrap labels that exceed a
    proportional threshold and have a sensible break point.
    """
    if not labels:
        return labels
    longest = max(len(l) for l in labels)
    if longest <= 14:
        return list(labels)  # all short enough — no wrapping
    threshold = max(14, longest // 2 + 4)
    out = []
    for lbl in labels:
        if len(lbl) <= threshold or " " not in lbl:
            out.append(lbl)
            continue
        # Find the space closest to the visual center, biased toward keeping
        # the second line shorter (more elegant when both lines are needed).
        mid = len(lbl) // 2
        before = lbl.rfind(" ", 0, mid + 1)
        after = lbl.find(" ", mid)
        if before == -1 and after == -1:
            out.append(lbl)
            continue
        if before == -1:
            idx = after
        elif after == -1:
            idx = before
        else:
            # prefer the break that yields the smallest difference in line lengths
            idx = before if (mid - before) <= (after - mid) else after
        out.append(lbl[:idx] + "\n" + lbl[idx + 1:])
    return out


def _setup_y_axis(ax, tick_fmt: str, tick_vals: list[float] | None) -> None:
    if tick_vals:
        ax.set_ylim(tick_vals[0], tick_vals[-1] * 1.18)
        ax.set_yticks(tick_vals)
        ax.set_yticklabels([_fmt(t, tick_fmt) for t in tick_vals],
                           fontsize=SIZE["body"])
    else:
        ax.tick_params(axis="y", labelsize=SIZE["body"])
    ax.set_ylabel("", labelpad=10)
