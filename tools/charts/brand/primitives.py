"""Brand primitives — small composable functions for applying the editorial
style. Chart modules consume these; they never re-implement them locally.
"""

from __future__ import annotations

from pathlib import Path
import textwrap

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
    return text


def title_block(
    ax,
    *,
    eyebrow: str,
    title: str,
    subtitle: str = "",
) -> None:
    """Top-left aligned title block: eyebrow (small caps) → title."""
    title = wrap_label(title, max_chars=76, max_lines=2)
    title_lines = max(1, title.count("\n") + 1)
    if eyebrow:
        ax.text(
            0.0, 1.13 + 0.04 * (title_lines - 1), _space_caps(eyebrow),
            transform=ax.transAxes,
            fontsize=SIZE["caption"],
            color=COLOR["accent"],
            fontweight="bold",
            family=FONT["serif_display"],
        )
    ax.text(
        0.0, 1.04, title,
        transform=ax.transAxes,
        fontsize=SIZE["display"],
        color=COLOR["ink"],
        fontweight="bold",
        family=FONT["serif_display"],
    )


def footer(fig, text: str) -> None:
    """Italic muted note at the bottom-left of the figure."""
    fig.text(
        0.024, 0.018, wrap_label(text, max_chars=112, max_lines=2),
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
    fig.canvas.draw()
    for ext in ("png", "svg"):
        fig.savefig(out_dir / f"{name}.{ext}")
    plt.close(fig)


def wrap_label(text: str, *, max_chars: int = 18, max_lines: int = 3) -> str:
    """Word-wrap chart labels with a hard cap on line count.

    Matplotlib will happily let long tick labels collide with neighboring
    marks. Centralizing wrapping keeps every chart using the same safety rail.
    """
    raw = str(text or "").strip()
    if len(raw) <= max_chars:
        return raw
    if " " not in raw and "_" in raw:
        raw = raw.replace("_", " ")
    wrapped = textwrap.wrap(
        raw,
        width=max_chars,
        break_long_words=True,
        break_on_hyphens=True,
    )
    if not wrapped:
        return raw
    if len(wrapped) <= max_lines:
        return "\n".join(wrapped)
    kept = wrapped[:max_lines]
    overflow = " ".join(wrapped[max_lines - 1:])
    kept[-1] = textwrap.shorten(overflow, width=max_chars, placeholder="...")
    return "\n".join(kept)


def wrap_labels(
    labels: list[str],
    *,
    max_chars: int = 18,
    max_lines: int = 3,
) -> list[str]:
    return [
        wrap_label(label, max_chars=max_chars, max_lines=max_lines)
        for label in labels
    ]


def max_label_line_length(labels: list[str]) -> int:
    longest = 0
    for label in labels:
        for line in str(label).splitlines() or [""]:
            longest = max(longest, len(line))
    return longest


def max_label_lines(labels: list[str]) -> int:
    return max((str(label).count("\n") + 1 for label in labels), default=1)


def left_margin_for_labels(
    labels: list[str],
    *,
    minimum: float = 0.18,
    maximum: float = 0.38,
) -> float:
    return min(maximum, max(minimum, 0.09 + max_label_line_length(labels) * 0.011))


def bottom_margin_for_labels(
    labels: list[str],
    *,
    minimum: float = 0.18,
    maximum: float = 0.36,
) -> float:
    lines = max_label_lines(labels)
    longest = max_label_line_length(labels)
    return min(maximum, max(minimum, 0.10 + lines * 0.055 + longest * 0.002))


def horizontal_figsize(n_items: int, *, min_width: float = 7.8) -> tuple[float, float]:
    return (max(min_width, 1.45 * max(1, n_items) + 2.8), 4.5)


def vertical_figsize(n_rows: int, *, min_height: float = 3.8) -> tuple[float, float]:
    return (9.0, max(min_height, 0.62 * max(1, n_rows) + 2.2))


def grid_figsize(n_rows: int, n_cols: int) -> tuple[float, float]:
    return (
        max(8.2, 2.20 * max(1, n_cols) + 3.2),
        max(4.0, 0.72 * max(1, n_rows) + 2.6),
    )


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
