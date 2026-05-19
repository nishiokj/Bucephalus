"""Brand tokens. Pure constants — never compute, never branch.

If you find yourself wanting to add a fifth font size or a sixth color
role, the design system is wrong, not the constants.
"""

# -----------------------------------------------------------------------------
# Type scale — 4 tiers, hard cap.
# -----------------------------------------------------------------------------
SIZE = {
    "display":  13,   # title only
    "subhead":  11,   # value labels when label is the only precise readout
    "body":      8.5, # axis labels, ticks, embedded labels, value
                      # labels when chart geometry already conveys the value
    "caption":   7.5, # eyebrow, metadata, footer
}

# -----------------------------------------------------------------------------
# Color tokens — 5 roles.
# -----------------------------------------------------------------------------
COLOR = {
    "ink":    "#1B1B1F",   # high-contrast type (title, value, categorical ticks)
    "muted":  "#807A6F",   # secondary type + chart elements (axis labels, CIs, grid in spirit)
    "accent": "#A6342B",   # eyebrow ONLY — the brand mark
    "bg":     "#F6F1E6",   # warm cream ground
    "grid":   "#E3DBC9",   # grid + spines + mark strokes
}

# Outcome colors for completeness-style charts. Distinct from the variant
# palette because outcomes are semantic, not categorical.
OUTCOME_COLOR = {
    "success": "#5E8B5E",
    "failure": "#C97A4F",
    "error":   "#A09689",
    "timeout": "#2E3038",
}

# -----------------------------------------------------------------------------
# Variant palette pool — 8 jewel tones, drawn in order for N variants.
# Variant marks ONLY. Never used for type.
# -----------------------------------------------------------------------------
PALETTE_POOL = [
    "#465C7E",   # deep blue
    "#A6342B",   # editorial red (focal slot)
    "#7A6B8E",   # muted violet
    "#9C7A2F",   # muted gold
    "#3E7060",   # deep teal
    "#8B4A52",   # maroon
    "#5E6E47",   # olive
    "#7F5B2E",   # ochre
]

# -----------------------------------------------------------------------------
# Font stacks — display serif first, fallbacks to widely available faces.
# -----------------------------------------------------------------------------
FONT = {
    "serif_display": ["Charter", "Iowan Old Style", "Hoefler Text", "Georgia", "DejaVu Serif"],
    "serif_body":    ["Charter", "Georgia", "DejaVu Serif"],
    "mono":          ["Menlo", "Monaco", "DejaVu Sans Mono"],
}
