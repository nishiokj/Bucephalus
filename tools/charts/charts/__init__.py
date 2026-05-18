"""Chart registry.

Each chart module exports:
  NAME       — string used for filenames
  REQUIRES   — human-readable description of data prerequisites
  applicable(ctx) -> (bool, reason_if_not)
  render(ctx, out_dir)

REGISTRY orders the charts roughly by "story": data quality first, then the
headline result, then supporting detail.
"""

from . import completeness, forest, bar, lollipop, per_task

REGISTRY = [
    completeness,
    forest,
    bar,
    lollipop,
    per_task,
]
