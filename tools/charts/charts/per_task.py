"""Per-task acceptance grid. Dot size = N trials in cell, color = success rate.

Needs ≥2 tasks to be informative — otherwise it's just one row of dots.
"""

from __future__ import annotations

from pathlib import Path

import matplotlib.pyplot as plt
from matplotlib.colors import LinearSegmentedColormap, Normalize

from brand import (
    SIZE, COLOR, FONT,
    apply_style, title_block, footer, save_pair,
)

NAME = "per_task"
REQUIRES = "≥2 tasks, ≥1 gradeable trial somewhere"


def applicable(ctx: dict) -> tuple[bool, str | None]:
    if ctx["n_tasks"] < 2:
        return False, f"need ≥2 tasks (got {ctx['n_tasks']})"
    if ctx["n_gradeable"] < 1:
        return False, "no gradeable trials"
    if ctx["n_variants"] < 1:
        return False, "no variants"
    return True, None


def render(ctx: dict, out_dir: Path) -> None:
    apply_style()
    trials = ctx["trials"]
    gradeable = trials.loc[trials["gradeable"]].copy()

    # Map ids to human labels via the context
    gradeable["task_label"] = gradeable["task_id"].map(ctx["task_labels"])
    gradeable["variant_label"] = gradeable["variant_id"].map(ctx["variant_labels"])

    variant_order = [ctx["variant_labels"][v] for v in ctx["variant_order"]]
    task_order = sorted(gradeable["task_label"].dropna().unique().tolist())

    pivot_rate = (
        gradeable.groupby(["task_label", "variant_label"], observed=True)["success"]
        .mean().unstack()
        .reindex(index=task_order, columns=variant_order)
    )
    pivot_n = (
        gradeable.groupby(["task_label", "variant_label"], observed=True)["success"]
        .count().unstack()
        .reindex(index=task_order, columns=variant_order)
        .fillna(0).astype(int)
    )

    cmap = LinearSegmentedColormap.from_list(
        "editorial",
        ["#A6342B", "#D4904C", "#E4C36A", "#9CB48E", "#5E8B5E"],
    )
    norm = Normalize(0, 1)

    n_rows, n_cols = pivot_rate.shape
    fig, ax = plt.subplots(
        figsize=(max(7.5, 1.7 * n_cols + 2.5),
                 max(3.6, 0.65 * n_rows + 2.0)),
    )
    fig.subplots_adjust(top=0.76, left=0.22, right=0.84, bottom=0.16)

    for yi, task in enumerate(pivot_rate.index):
        for xi, var in enumerate(pivot_rate.columns):
            n = pivot_n.loc[task, var]
            if n == 0:
                ax.text(xi, yi, "—", ha="center", va="center",
                        fontsize=14, color=COLOR["muted"],
                        family=FONT["serif_display"])
                continue
            rate = pivot_rate.loc[task, var]
            c = cmap(norm(rate))
            size = 320 + n * 90
            ax.scatter([xi], [yi], s=size, color=c,
                       edgecolors=COLOR["bg"], linewidths=2.2, zorder=5)
            ax.text(xi, yi, f"{int(round(rate * n))}/{n}",
                    ha="center", va="center", fontsize=SIZE["caption"] + 1,
                    color="white" if 0.25 < rate < 0.80 else COLOR["ink"],
                    family=FONT["serif_display"], fontweight="bold")

    ax.set_xticks(range(n_cols))
    ax.set_xticklabels(_two_line(variant_order),
                       fontsize=SIZE["body"], family=FONT["serif_body"])
    ax.set_yticks(range(n_rows))
    ax.set_yticklabels(task_order, fontsize=SIZE["body"],
                       family=FONT["serif_body"], style="italic",
                       color=COLOR["ink"])
    ax.set_xlim(-0.6, n_cols - 0.4)
    ax.set_ylim(-0.6, n_rows - 0.4)
    ax.invert_yaxis()
    ax.grid(True, alpha=0.5)
    ax.set_axisbelow(True)
    ax.tick_params(left=False, bottom=False)

    # adaptive size legend: pick three reference N values from the actual data
    actual_ns = pivot_n.to_numpy().flatten()
    actual_ns = sorted({int(n) for n in actual_ns if n > 0})
    if not actual_ns:
        legend_ns: list[int] = []
    elif len(actual_ns) == 1:
        legend_ns = actual_ns
    elif len(actual_ns) == 2:
        legend_ns = actual_ns
    else:
        legend_ns = [actual_ns[0], actual_ns[len(actual_ns) // 2], actual_ns[-1]]

    if legend_ns:
        ax.text(1.08, 0.95, "Trials", transform=ax.transAxes,
                fontsize=SIZE["caption"] + 1, color=COLOR["accent"],
                fontweight="bold", family=FONT["serif_display"], style="italic")
        for j, n_ex in enumerate(legend_ns):
            size = 320 + n_ex * 90
            x_lg, y_lg = 1.08, 0.82 - j * 0.13
            ax.scatter([x_lg], [y_lg], s=size, color="#BFBAB1",
                       edgecolors=COLOR["bg"], linewidths=2.2,
                       transform=ax.transAxes, clip_on=False)
            ax.text(x_lg + 0.06, y_lg, _spell_n(n_ex),
                    transform=ax.transAxes, va="center",
                    fontsize=SIZE["body"], color=COLOR["ink"],
                    family=FONT["serif_body"], style="italic")

    title_block(ax,
                eyebrow=ctx["eyebrow"],
                title="Per-task results",
                subtitle=ctx["subtitle"])
    footer(fig,
           "Color encodes the per-cell acceptance rate.  "
           "Marker size encodes trials per cell.  "
           "Em-dash marks cells with no gradeable trials.")
    save_pair(fig, out_dir, NAME)


_WORD_NUMBERS = {
    1: "one", 2: "two", 3: "three", 4: "four", 5: "five",
    6: "six", 7: "seven", 8: "eight", 9: "nine", 10: "ten",
}


def _spell_n(n: int) -> str:
    """Numerals through ten get spelled out (editorial style); rest stay as digits."""
    return _WORD_NUMBERS.get(n, str(n))


def _two_line(labels: list[str]) -> list[str]:
    """Wrap labels onto two lines only when the longest justifies it."""
    if not labels:
        return labels
    longest = max(len(l) for l in labels)
    if longest <= 14:
        return list(labels)
    threshold = max(14, longest // 2 + 4)
    out = []
    for lbl in labels:
        if len(lbl) <= threshold or " " not in lbl:
            out.append(lbl)
            continue
        mid = len(lbl) // 2
        before = lbl.rfind(" ", 0, mid + 1)
        after = lbl.find(" ", mid)
        if before == -1 and after == -1:
            out.append(lbl)
            continue
        idx = (before if before != -1
               else after) if (before == -1 or after == -1) else (
            before if (mid - before) <= (after - mid) else after
        )
        out.append(lbl[:idx] + "\n" + lbl[idx + 1:])
    return out
