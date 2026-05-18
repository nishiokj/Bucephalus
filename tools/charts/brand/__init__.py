"""AgentLab editorial brand system — tokens + primitives.

This module is experiment-agnostic. Nothing in here references a specific
experiment, variant, or metric. The chart modules consume these tokens.
"""

from .tokens import SIZE, COLOR, OUTCOME_COLOR, PALETTE_POOL, FONT  # noqa: F401
from .primitives import (  # noqa: F401
    apply_style,
    title_block,
    footer,
    palette_for,
    save_pair,
    derive_tick_format,
)
