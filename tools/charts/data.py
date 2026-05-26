"""Per-experiment data loading + render context.

`load_render_context(experiment_id, overrides)` produces a dict with everything
chart modules need. Auto-derives titles, axis labels, palettes from the data
and metric_definitions; falls back to overrides where the user wants control.
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import numpy as np
import pandas as pd

from brand import palette_for, derive_tick_format

DB = Path.home() / ".bucephalus" / "bucephalus.sqlite"


def open_db() -> sqlite3.Connection:
    """Open the Bucephalus account DB read-only.

    Chart rendering should never mutate the account database; using SQLite's
    read-only URI also avoids creating WAL/SHM files during quick gallery views.
    """
    db = (
        os.environ.get("BUCEPHALUS_DB")
        or os.environ.get("AGENTLAB_DB")
        or str(DB)
    )
    db_path = Path(db).expanduser()
    if not db_path.exists():
        legacy = Path.home() / ".agentlab" / "agentlab.sqlite"
        if legacy.exists():
            db_path = legacy
    return sqlite3.connect(f"file:{db_path.as_posix()}?mode=ro&immutable=1", uri=True)


# -----------------------------------------------------------------------------
# Per-experiment config overrides — everything optional.
# -----------------------------------------------------------------------------
@dataclass
class ExperimentConfig:
    title: str | None = None
    eyebrow: str | None = None
    subtitle: str | None = None
    y_label: str | None = None
    variant_label_overrides: dict[str, str] = field(default_factory=dict)
    task_label_overrides: dict[str, str] = field(default_factory=dict)
    palette_mode: str = "categorical"   # or "highlight"
    highlight_variant: str | None = None
    metric_id: str | None = None


# -----------------------------------------------------------------------------
# Label humanization — pluggable, sensible defaults.
# -----------------------------------------------------------------------------
def humanize_id(raw: str) -> str:
    """Generic id humanizer. Strips common prefixes, normalizes separators,
    title-cases. Override per-experiment via config when this isn't enough."""
    stripped = re.sub(r"^[a-z]+-[a-z]+-seed-\d+-", "", raw)
    stripped = re.sub(r"^(generator|agent|variant)_", "", stripped)
    return stripped.replace("-", " ").replace("_", " ").title()


# -----------------------------------------------------------------------------
# Auto-derivation helpers.
# -----------------------------------------------------------------------------
def _parse_metric_value(raw: str | None) -> float:
    if not raw or raw == "null":
        return 0.0
    try:
        v = json.loads(raw)
    except (TypeError, ValueError):
        return 0.0
    if isinstance(v, (int, float)):
        return float(v)
    if isinstance(v, bool):
        return 1.0 if v else 0.0
    if isinstance(v, dict):
        for k in ("value", "score", "resolved"):
            if k in v and isinstance(v[k], (int, float)):
                return float(v[k])
    return 0.0


def _default_primary_metric() -> dict:
    return {"metric_id": "success", "label": "Success",
            "value_type": "number", "unit": "ratio",
            "direction": "maximize"}


def _load_metric(con, experiment_id: str | None, metric_id: str | None = None) -> dict:
    if not experiment_id:
        return _default_primary_metric()
    if metric_id:
        rows = pd.read_sql_query(
            """
            SELECT metric_id, label, semantic_key, value_type, unit, direction,
                   source_type, source_pointer, definition_json
            FROM metric_definitions
            WHERE experiment_id = ? AND metric_id = ?
            LIMIT 1
            """,
            con, params=(experiment_id, metric_id),
        )
        if rows.empty:
            raise ValueError(f"metric {metric_id!r} is not declared for {experiment_id!r}")
        return rows.iloc[0].to_dict()
    rows = pd.read_sql_query(
        """
        SELECT metric_id, label, semantic_key, value_type, unit, direction,
               source_type, source_pointer, definition_json
        FROM metric_definitions
        WHERE experiment_id = ? AND primary_metric = 1
        LIMIT 1
        """,
        con, params=(experiment_id,),
    )
    if rows.empty:
        # Fallback to any defined metric
        rows = pd.read_sql_query(
            "SELECT metric_id, label, semantic_key, value_type, unit, direction "
            "FROM metric_definitions WHERE experiment_id = ? LIMIT 1",
            con, params=(experiment_id,),
        )
    if rows.empty:
        return _default_primary_metric()
    return rows.iloc[0].to_dict()


def _load_metric_values(
    con,
    *,
    experiment_id: str | None,
    run_id: str | None,
    metric_id: str,
) -> pd.DataFrame:
    if run_id:
        sql = """
            SELECT m.run_id, m.trial_id, m.schedule_idx, m.attempt,
                   m.metric_value_json, m.metric_source, m.row_seq
            FROM metric_rows m
            JOIN runs r ON r.run_id = m.run_id
            WHERE m.run_id = ? AND r.status = 'completed' AND m.metric_name = ?
        """
        params = (run_id, metric_id)
    else:
        sql = """
            SELECT m.run_id, m.trial_id, m.schedule_idx, m.attempt,
                   m.metric_value_json, m.metric_source, m.row_seq
            FROM metric_rows m
            JOIN runs r ON r.run_id = m.run_id
            WHERE r.experiment_id = ? AND r.status = 'completed' AND m.metric_name = ?
        """
        params = (experiment_id, metric_id)
    values = pd.read_sql_query(sql, con, params=params)
    if values.empty:
        return values
    values["_primary"] = values["metric_source"].eq("primary")
    values = values.sort_values(["_primary", "row_seq"], ascending=[False, False])
    values = values.drop_duplicates(
        ["run_id", "trial_id", "schedule_idx", "attempt"],
        keep="first",
    )
    return values.drop(columns=["_primary", "metric_source", "row_seq"])


def _derive_model_label(trials: pd.DataFrame) -> str:
    """Most common 'model' (+ optional reasoning) in the bindings, as a label."""
    bindings_seen: list[dict] = []
    for raw in trials["bindings_json"].dropna().head(50):
        try:
            b = json.loads(raw)
            if isinstance(b, dict):
                bindings_seen.append(b)
        except (TypeError, ValueError):
            pass
    if not bindings_seen:
        return ""
    models = [b.get("model") for b in bindings_seen if b.get("model")]
    if not models:
        return ""
    model = Counter(models).most_common(1)[0][0]
    reasoning = Counter(
        b.get("reasoning") for b in bindings_seen if b.get("reasoning")
    ).most_common(1)
    if reasoning:
        return f"{model} · {reasoning[0][0]}"
    return str(model)


def _compose_subtitle(trials: pd.DataFrame, n_variants: int, n_tasks: int) -> str:
    """Compose an editorial subtitle: model + experimental scope."""
    model = _derive_model_label(trials)
    # Pluralize sensibly
    scope = (
        f"{n_variants} variant{'s' if n_variants != 1 else ''}"
        f" × {n_tasks} task{'s' if n_tasks != 1 else ''}"
    )
    return f"{model}  ·  {scope}" if model else scope


# -----------------------------------------------------------------------------
# Stats helpers.
# -----------------------------------------------------------------------------
def bootstrap_ci(vals: np.ndarray, n: int = 5000, ci: float = 0.95,
                 rng: np.random.Generator | None = None) -> tuple[float, float, float]:
    if rng is None:
        rng = np.random.default_rng(7)
    if len(vals) == 0:
        return (float("nan"), float("nan"), float("nan"))
    idx = rng.integers(0, len(vals), size=(n, len(vals)))
    means = vals[idx].mean(axis=1)
    a = (1 - ci) / 2
    return float(vals.mean()), float(np.quantile(means, a)), float(np.quantile(means, 1 - a))


# -----------------------------------------------------------------------------
# The main entry point.
# -----------------------------------------------------------------------------
def load_render_context(
    experiment_id: str | None = None,
    *,
    run_id: str | None = None,
    config: ExperimentConfig | None = None,
) -> dict[str, Any]:
    """Load everything a chart module needs to render this experiment or run."""
    if bool(experiment_id) == bool(run_id):
        raise ValueError("provide exactly one of experiment_id or run_id")

    config = config or ExperimentConfig()
    con = open_db()
    try:
        if run_id:
            trials_sql = """
                SELECT t.run_id, t.trial_id, t.schedule_idx, t.attempt,
                       t.variant_id, t.task_id,
                       t.outcome, t.primary_metric_value_json, t.bindings_json,
                       r.experiment_id
                FROM trial_rows t JOIN runs r ON r.run_id = t.run_id
                WHERE t.run_id = ? AND r.status = 'completed'
            """
            trials_params = (run_id,)
        else:
            trials_sql = """
                SELECT t.run_id, t.trial_id, t.schedule_idx, t.attempt,
                       t.variant_id, t.task_id,
                       t.outcome, t.primary_metric_value_json, t.bindings_json,
                       r.experiment_id
                FROM trial_rows t JOIN runs r ON r.run_id = t.run_id
                WHERE r.experiment_id = ? AND r.status = 'completed'
            """
            trials_params = (experiment_id,)

        trials = pd.read_sql_query(
            trials_sql,
            con, params=trials_params,
        )

        if trials.empty:
            scope = f"run {run_id!r}" if run_id else f"experiment {experiment_id!r}"
            raise ValueError(f"No completed-run trials for {scope}")

        resolved_experiment_id = (
            experiment_id
            or trials["experiment_id"].dropna().astype(str).head(1).squeeze()
        )
        if not isinstance(resolved_experiment_id, str):
            resolved_experiment_id = None

        primary_metric = _load_metric(con, resolved_experiment_id, config.metric_id)
        selected_metric_id = primary_metric.get("metric_id") or "success"
        if config.metric_id:
            metric_values = _load_metric_values(
                con,
                experiment_id=experiment_id,
                run_id=run_id,
                metric_id=config.metric_id,
            )
            if metric_values.empty:
                raise ValueError(f"No observations for metric {config.metric_id!r}")
            trials = trials.merge(
                metric_values,
                how="left",
                on=["run_id", "trial_id", "schedule_idx", "attempt"],
            )
            trials["selected_metric_value_json"] = trials["metric_value_json"]
        else:
            trials["selected_metric_value_json"] = trials["primary_metric_value_json"]
        # Pull workload type from any run manifest for the eyebrow
        if run_id:
            workload_sql = """
                SELECT DISTINCT json_extract(manifest_json, '$.workload_type') AS wl
                FROM runs WHERE run_id = ? LIMIT 1
            """
            workload_params = (run_id,)
        else:
            workload_sql = """
                SELECT DISTINCT json_extract(manifest_json, '$.workload_type') AS wl
                FROM runs WHERE experiment_id = ? LIMIT 1
            """
            workload_params = (experiment_id,)
        workload = pd.read_sql_query(
            workload_sql,
            con, params=workload_params,
        )
        workload_type = (workload.iloc[0]["wl"] if not workload.empty
                         and workload.iloc[0]["wl"] else "behavioral experiment")
    finally:
        con.close()

    trials["metric_value"] = trials["selected_metric_value_json"].apply(_parse_metric_value)
    if selected_metric_id == "success":
        fallback = trials["metric_value"].eq(0.0) & trials["outcome"].eq("success")
        trials.loc[fallback, "metric_value"] = 1.0
    trials["success"] = trials["metric_value"]
    trials["gradeable"] = trials["outcome"].isin(["success", "failure"])

    variant_order = (
        trials.groupby("variant_id").size()
        .sort_values(ascending=False).index.tolist()
    )
    variant_labels = {
        v: config.variant_label_overrides.get(v, humanize_id(v))
        for v in variant_order
    }
    task_labels = {
        t: config.task_label_overrides.get(t, humanize_id(t))
        for t in trials["task_id"].unique()
    }

    # Per-variant summary with bootstrapped CIs over gradeable trials only.
    summary_rows = []
    for v in variant_order:
        sub = trials.loc[(trials["variant_id"] == v) & trials["gradeable"]]
        vals = sub["success"].to_numpy()
        mean, lo, hi = bootstrap_ci(vals)
        all_sub = trials.loc[trials["variant_id"] == v]
        summary_rows.append({
            "variant_id":  v,
            "label":       variant_labels[v],
            "mean":        mean,
            "lo":          lo,
            "hi":          hi,
            "n_gradeable": len(vals),
            "n_total":     len(all_sub),
            "k":           int(vals.sum()) if len(vals) else 0,
        })
    summary = pd.DataFrame(summary_rows)

    # Per-variant outcome breakdown for completeness charts.
    comp_rows = []
    for v in variant_order:
        sub = trials.loc[trials["variant_id"] == v]
        c = sub["outcome"].value_counts().to_dict()
        comp_rows.append({
            "variant_id": v,
            "label":      variant_labels[v],
            "success":    c.get("success", 0),
            "failure":    c.get("failure", 0),
            "error":      c.get("error", 0),
            "timeout":    c.get("timeout", 0),
            "n":          len(sub),
        })
    completeness = pd.DataFrame(comp_rows)

    # Smart unit inference: if no unit declared and all observed means fall
    # in [0, 1], treat as a ratio for tick formatting. Lets ratio-shaped
    # metrics render as percentages without requiring `unit: ratio` in YAML.
    primary_metric = dict(primary_metric)  # don't mutate the row
    if not primary_metric.get("unit"):
        valid_means = summary["mean"].dropna()
        if len(valid_means) and valid_means.between(0, 1).all():
            primary_metric["unit"] = "ratio"

    # Auto-derived strings, override-overridable.
    metric_label = primary_metric.get("label") or humanize_id(
        str(primary_metric.get("metric_id") or selected_metric_id)
    )
    title = config.title or metric_label
    eyebrow = config.eyebrow or f"BUCEPHALUS · {workload_type.upper()}"
    subtitle = (config.subtitle
                if config.subtitle is not None
                else _compose_subtitle(trials, len(variant_order), trials["task_id"].nunique()))
    y_label = config.y_label or (
        f"{metric_label}"
        + (f" ({primary_metric.get('unit')})" if primary_metric.get("unit") not in (None, "", "ratio") else "")
    )

    # Palette
    highlight_idx = None
    if config.highlight_variant and config.highlight_variant in variant_order:
        highlight_idx = variant_order.index(config.highlight_variant)
    palette = palette_for(
        len(variant_order),
        mode=config.palette_mode,
        highlight=highlight_idx,
    )
    variant_color = dict(zip(variant_order, palette))

    tick_fmt, tick_vals = derive_tick_format(primary_metric)

    return {
        "experiment_id":   resolved_experiment_id,
        "run_id":          run_id,
        "workload_type":   workload_type,
        "trials":          trials,
        "summary":         summary,
        "completeness":    completeness,
        "primary_metric":  primary_metric,
        "variant_order":   variant_order,
        "variant_labels":  variant_labels,
        "task_labels":     task_labels,
        "variant_color":   variant_color,
        "palette":         palette,
        "title":           title,
        "eyebrow":         eyebrow,
        "subtitle":        subtitle,
        "y_label":         y_label,
        "tick_format":     tick_fmt,
        "tick_values":     tick_vals,
        "n_variants":      len(variant_order),
        "n_tasks":         trials["task_id"].nunique(),
        "n_gradeable":     int(trials["gradeable"].sum()),
    }
