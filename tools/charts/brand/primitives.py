"""Brand primitives — small composable functions for applying the editorial
style. Chart modules consume these; they never re-implement them locally.
"""

from __future__ import annotations

from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

from .tokens import SIZE, COLOR, FONT, PALETTE_POOL


def apply_style() -> None:
    """Set rcParams to the editorial defaults. Call at the start of every
    chart function."""
    plt.rcParams.update({
        "font.family":         FONT["serif_body"],
        "font.size":           SIZE["body"],
        "axes.facecolor":      COLOR["bg"],
        "figure.facecolor":    COLOR["bg"],
        "savefig.facecolor":   COLOR["bg"],
        "axes.edgecolor":      COLOR["grid"],
        "axes.labelcolor":     COLOR["muted"],
        "axes.titlecolor":     COLOR["ink"],
        "xtick.color":         COLOR["muted"],
        "ytick.color":         COLOR["ink"],
        "axes.spines.top":     False,
        "axes.spines.right":   False,
        "axes.spines.left":    False,
        "axes.linewidth":      0.9,
        "grid.color":          COLOR["grid"],
        "grid.alpha":          0.7,
        "xtick.major.size":    0,
        "ytick.major.size":    0,
        "savefig.dpi":         220,
        "savefig.bbox":        "tight",
        "pdf.fonttype":        42,
        "svg.fonttype":        "none",
    })


def _space_caps(text: str) -> str:
    """Approximate letter-spacing for caps eyebrow by injecting thin spaces.
    matplotlib has no real letter-spacing kwarg."""
    if any(c.islower() for c in text):
        return text  # not all-caps, leave alone
    return "  ".join(text)


def title_block(
    ax,
    *,
    eyebrow: str,
    title: str,
    subtitle: str = "",
) -> None:
    """Top-left aligned title block: eyebrow (caps, accent) → title (display) →
    subtitle (italic muted)."""
    if eyebrow:
        ax.text(
            0.0, 1.28, _space_caps(eyebrow),
            transform=ax.transAxes,
            fontsize=SIZE["caption"],
            color=COLOR["accent"],
            fontweight="bold",
            family=FONT["serif_display"],
        )
    ax.text(
        0.0, 1.16, title,
        transform=ax.transAxes,
        fontsize=SIZE["display"],
        color=COLOR["ink"],
        fontweight="bold",
        family=FONT["serif_display"],
    )
    if subtitle:
        ax.text(
            0.0, 1.06, subtitle,
            transform=ax.transAxes,
            fontsize=SIZE["body"],
            color=COLOR["muted"],
            style="italic",
            family=FONT["serif_display"],
        )


def footer(fig, text: str) -> None:
    """Italic muted note at the bottom-left of the figure."""
    fig.text(
        0.024, -0.05, text,
        fontsize=SIZE["caption"],
        color=COLOR["muted"],
        style="italic",
        family=FONT["serif_body"],
    )


def palette_for(
    n: int,
    *,
    mode: str = "categorical",
    highlight: int | None = None,
) -> list[str]:
    """Return a list of n hex colors.

    mode='categorical': draw from PALETTE_POOL in order (first n slots).
        Falls back to 'highlight' when n exceeds the pool.
    mode='highlight':   one accent color, others muted. The accent goes to
        `highlight` index, defaulting to the last variant. Editorial default
        when there's a single protagonist variant.
    """
    if n <= 0:
        return []
    if mode == "highlight":
        focal = highlight if highlight is not None else (n - 1)
        return [COLOR["accent"] if i == focal else COLOR["muted"] for i in range(n)]
    if mode == "categorical":
        if n <= len(PALETTE_POOL):
            return PALETTE_POOL[:n]
        # too many variants for categorical — fall through
        return palette_for(n, mode="highlight", highlight=highlight)
    raise ValueError(f"Unknown palette mode: {mode!r}")


def save_pair(fig, out_dir: Path, name: str) -> None:
    """Save the figure as both .png and .svg into out_dir."""
    out_dir.mkdir(parents=True, exist_ok=True)
    for ext in ("png", "svg"):
        fig.savefig(out_dir / f"{name}.{ext}")
    plt.close(fig)


def derive_tick_format(metric: dict) -> tuple[str, list[float] | None]:
    """Pick a matplotlib tick format string from the metric definition.

    Returns (format_str, suggested_ticks). suggested_ticks is None when the
    chart should compute them itself.
    """
    unit = (metric.get("unit") or "").lower()
    vtype = (metric.get("value_type") or "").lower()
    semantic = (metric.get("semantic_key") or "").lower()

    # Percentage / probability metrics
    if unit in {"ratio", "rate", "fraction", "probability"} or "rate" in semantic or "success" in semantic:
        return ("{x:.0%}", [0, 0.25, 0.5, 0.75, 1.0])
    if unit == "%":
        return ("{x:.0f}%", None)
    # Time / latency
    if unit in {"ms", "milliseconds", "s", "seconds"}:
        return ("{x:,.0f}", None)
    # Money
    if unit in {"usd", "$"}:
        return ("${x:,.2f}", None)
    # Counts / tokens
    if unit in {"tokens", "count"} or vtype in {"integer", "int"}:
        return ("{x:,.0f}", None)
    # Default
    return ("{x:.2f}", None)
