"""Gallery renderer.

Usage:
    python gallery.py <experiment_id> [<experiment_id> ...]   # named
    python gallery.py --sweep                                  # all from DB
    python gallery.py --sweep --force                          # rerender all

For each experiment, renders every applicable chart in the editorial style
into ./gallery/<experiment_id>/<chart_name>.{png,svg}. After processing all
experiments, regenerates ./gallery/index.html — a browsable editorial gallery.

The idea: render everything cheap, pick winners by eye. No heuristic.
"""

from __future__ import annotations

import argparse
import datetime as dt
import html
import sqlite3
import sys
import time
from pathlib import Path

import charts
from data import DB, ExperimentConfig, load_render_context


OUT_ROOT = Path(__file__).parent / "gallery"


def _color(s: str, code: str) -> str:
    if not sys.stdout.isatty():
        return s
    return f"\033[{code}m{s}\033[0m"


def discover_experiments(latest_first: bool = True) -> list[tuple[str, int]]:
    """Find all experiment_ids with ≥1 completed run.
    Returns [(experiment_id, latest_run_at_ms), ...]."""
    con = sqlite3.connect(DB)
    try:
        cur = con.execute(
            """
            SELECT experiment_id, MAX(created_at_ms) AS latest
            FROM runs
            WHERE status = 'completed' AND experiment_id IS NOT NULL
            GROUP BY experiment_id
            ORDER BY latest DESC
            """
        )
        rows = cur.fetchall()
    finally:
        con.close()
    return rows if latest_first else list(reversed(rows))


def is_stale(out_dir: Path, latest_run_at_ms: int) -> bool:
    """True if the gallery directory has no charts, or any chart is older than
    the most recent run."""
    if not out_dir.exists():
        return True
    pngs = list(out_dir.glob("*.png"))
    if not pngs:
        return True
    oldest_chart_ms = min(p.stat().st_mtime for p in pngs) * 1000
    return latest_run_at_ms > oldest_chart_ms


def render_experiment(
    experiment_id: str,
    *,
    config: ExperimentConfig | None = None,
    out_root: Path = OUT_ROOT,
    latest_run_at_ms: int | None = None,
) -> dict:
    """Render every applicable chart for one experiment. Returns a summary dict."""
    print(_color(f"\n▮ {experiment_id}", "1"))
    t0 = time.perf_counter()

    try:
        ctx = load_render_context(experiment_id, config=config)
    except Exception as e:
        print(f"  {_color('load failed:', '31')} {e}")
        return {"experiment_id": experiment_id, "error": str(e),
                "rendered": [], "skipped": [],
                "title": experiment_id, "subtitle": "",
                "latest_run_at_ms": latest_run_at_ms or 0}

    print(f"  loaded {len(ctx['trials'])} trials  ·  "
          f"{ctx['n_variants']} variants  ·  "
          f"{ctx['n_tasks']} tasks  ·  "
          f"{ctx['n_gradeable']} gradeable")

    out_dir = out_root / experiment_id
    rendered: list[str] = []
    skipped: list[tuple[str, str]] = []

    for mod in charts.REGISTRY:
        name = mod.NAME
        ok, reason = mod.applicable(ctx)
        if not ok:
            skipped.append((name, reason or "not applicable"))
            print(f"  - {name}: {_color('skipped', '90')} ({reason})")
            continue
        try:
            mod.render(ctx, out_dir)
        except Exception as e:
            skipped.append((name, f"render error: {e}"))
            print(f"  - {name}: {_color('error', '31')} {e}")
            continue
        rendered.append(name)
        print(f"  + {name}: {_color('rendered', '32')}")

    elapsed = time.perf_counter() - t0
    print(f"  wrote {len(rendered)} charts to {out_dir}/  ({elapsed:.1f}s)")
    return {
        "experiment_id": experiment_id,
        "rendered": rendered,
        "skipped": skipped,
        "out_dir": str(out_dir),
        "elapsed_s": elapsed,
        "title": ctx["title"],
        "subtitle": ctx["subtitle"],
        "eyebrow": ctx["eyebrow"],
        "n_trials": len(ctx["trials"]),
        "n_variants": ctx["n_variants"],
        "n_tasks": ctx["n_tasks"],
        "n_gradeable": ctx["n_gradeable"],
        "latest_run_at_ms": latest_run_at_ms or 0,
    }


def render_index(summaries: list[dict], out_root: Path = OUT_ROOT) -> Path:
    """Generate gallery/index.html — a browsable editorial gallery of every
    rendered experiment, with chart thumbnails."""
    # sort by latest run, newest first
    summaries = sorted(summaries,
                       key=lambda s: s.get("latest_run_at_ms", 0),
                       reverse=True)

    now = dt.datetime.now().strftime("%Y-%m-%d %H:%M")
    title = "Experiment Gallery"
    eyebrow = "AGENTLAB · GALLERY"

    sections = []
    for s in summaries:
        exp_id = s["experiment_id"]
        section_charts = []
        for name in s.get("rendered", []):
            section_charts.append(f"""
              <a class="chart" href="{html.escape(exp_id)}/{name}.png">
                <div class="thumb"><img loading="lazy" src="{html.escape(exp_id)}/{name}.png" alt="{name}"></div>
                <div class="chart-name">{name}</div>
              </a>
            """)

        if s.get("error"):
            body = f'<div class="error">load failed: {html.escape(s["error"])}</div>'
        elif not section_charts:
            body = '<div class="empty">No charts applicable — see completeness if generated.</div>'
        else:
            body = f'<div class="charts">{"".join(section_charts)}</div>'

        latest_str = ""
        if s.get("latest_run_at_ms"):
            latest_str = dt.datetime.fromtimestamp(s["latest_run_at_ms"] / 1000).strftime("%Y-%m-%d")

        scope_parts = []
        if "n_variants" in s:
            v, t = s["n_variants"], s["n_tasks"]
            scope_parts.append(f"{v} variant{'s' if v != 1 else ''} × {t} task{'s' if t != 1 else ''}")
        if "n_trials" in s:
            scope_parts.append(f"{s['n_trials']} trials")
            scope_parts.append(f"{s['n_gradeable']} gradeable")
        if latest_str:
            scope_parts.append(f"latest run {latest_str}")
        scope = "  ·  ".join(scope_parts)

        sections.append(f"""
          <section class="experiment">
            <div class="row-head">
              <div class="title-block">
                <h2>{html.escape(s.get("title") or exp_id)}</h2>
                <div class="subtitle">{html.escape(s.get("subtitle") or "")}</div>
                <div class="meta">{html.escape(exp_id)}  ·  {scope}</div>
              </div>
            </div>
            {body}
          </section>
        """)

    html_doc = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{html.escape(title)}</title>
<style>
  :root {{
    --bg: #F6F1E6;
    --ink: #1B1B1F;
    --muted: #807A6F;
    --accent: #A6342B;
    --grid: #E3DBC9;
  }}
  * {{ box-sizing: border-box; }}
  html, body {{ background: var(--bg); color: var(--ink); margin: 0; padding: 0; }}
  body {{
    font-family: Charter, "Iowan Old Style", "Hoefler Text", Georgia, serif;
    line-height: 1.5;
    font-size: 16px;
  }}
  .page {{ max-width: 1200px; margin: 0 auto; padding: 64px 48px 96px; }}
  header.page-head {{ margin-bottom: 56px; border-bottom: 1px solid var(--grid); padding-bottom: 32px; }}
  .eyebrow {{
    font-size: 11px; font-weight: bold; letter-spacing: 0.22em;
    color: var(--accent); text-transform: uppercase;
    margin-bottom: 14px;
  }}
  h1 {{ font-size: 44px; font-weight: bold; margin: 0 0 8px; letter-spacing: -0.01em; }}
  .page-meta {{ color: var(--muted); font-style: italic; font-size: 14px; }}
  section.experiment {{
    padding: 32px 0;
    border-bottom: 1px solid var(--grid);
  }}
  section.experiment:last-child {{ border-bottom: 0; }}
  .row-head {{ margin-bottom: 24px; }}
  h2 {{ font-size: 24px; font-weight: bold; margin: 0 0 4px; }}
  .subtitle {{ color: var(--muted); font-style: italic; font-size: 14px; margin-bottom: 6px; }}
  .meta {{ color: var(--muted); font-size: 12px; font-family: Menlo, Monaco, monospace; }}
  .charts {{
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 24px;
    margin-top: 8px;
  }}
  .chart {{
    text-decoration: none; color: inherit;
    display: block;
    transition: transform 0.12s ease;
  }}
  .chart:hover {{ transform: translateY(-2px); }}
  .thumb {{
    background: #fff;
    border: 1px solid var(--grid);
    padding: 8px;
    aspect-ratio: 16 / 9;
    display: flex; align-items: center; justify-content: center;
    overflow: hidden;
  }}
  .thumb img {{ max-width: 100%; max-height: 100%; object-fit: contain; }}
  .chart-name {{
    margin-top: 8px;
    font-size: 12px;
    font-family: Menlo, Monaco, monospace;
    color: var(--muted);
    text-transform: lowercase;
  }}
  .empty, .error {{
    color: var(--muted);
    font-style: italic;
    padding: 16px 0;
  }}
  .error {{ color: var(--accent); }}
  footer.page-foot {{
    margin-top: 64px;
    padding-top: 24px;
    border-top: 1px solid var(--grid);
    color: var(--muted);
    font-size: 12px;
    font-style: italic;
  }}
</style>
</head>
<body>
<div class="page">
  <header class="page-head">
    <div class="eyebrow">{html.escape(eyebrow)}</div>
    <h1>{html.escape(title)}</h1>
    <div class="page-meta">{len(summaries)} experiment{'s' if len(summaries) != 1 else ''}  ·  updated {now}</div>
  </header>
  <main>
    {"".join(sections)}
  </main>
  <footer class="page-foot">
    Generated by gallery.py.  Render everything cheap; pick winners by eye.
  </footer>
</div>
</body>
</html>
"""
    out_path = out_root / "index.html"
    out_root.mkdir(parents=True, exist_ok=True)
    out_path.write_text(html_doc, encoding="utf-8")
    return out_path


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("experiment_id", nargs="*",
                    help="One or more experiment_ids to render (omit with --sweep)")
    ap.add_argument("--sweep", action="store_true",
                    help="Render every experiment with completed runs in the DB")
    ap.add_argument("--force", action="store_true",
                    help="Re-render even when charts are already up to date")
    ap.add_argument("--palette-mode", choices=["categorical", "highlight"],
                    default="categorical")
    ap.add_argument("--highlight",
                    help="When --palette-mode=highlight, the focal variant_id")
    ap.add_argument("--no-index", action="store_true",
                    help="Skip regenerating gallery/index.html")
    args = ap.parse_args()

    config = ExperimentConfig(
        palette_mode=args.palette_mode,
        highlight_variant=args.highlight,
    )

    # Resolve which experiments to consider
    if args.sweep:
        targets = discover_experiments()
        print(_color(f"sweep: found {len(targets)} experiment(s) with completed runs", "1"))
    else:
        if not args.experiment_id:
            ap.error("provide experiment_id(s) or --sweep")
        # Look up latest run timestamps for freshness checks
        con = sqlite3.connect(DB)
        try:
            placeholders = ",".join("?" for _ in args.experiment_id)
            cur = con.execute(
                f"SELECT experiment_id, COALESCE(MAX(created_at_ms), 0) "
                f"FROM runs WHERE experiment_id IN ({placeholders}) "
                f"GROUP BY experiment_id",
                args.experiment_id,
            )
            latest_map = dict(cur.fetchall())
        finally:
            con.close()
        targets = [(eid, latest_map.get(eid, 0)) for eid in args.experiment_id]

    summaries: list[dict] = []
    skipped_fresh: list[str] = []
    for exp_id, latest_at_ms in targets:
        out_dir = OUT_ROOT / exp_id
        if not args.force and not is_stale(out_dir, latest_at_ms):
            print(_color(f"\n▮ {exp_id}", "1"))
            print(f"  {_color('up to date', '90')} — skipping (use --force to rerender)")
            skipped_fresh.append(exp_id)
            # still include in index — load minimal info
            try:
                ctx = load_render_context(exp_id, config=config)
                summaries.append({
                    "experiment_id": exp_id,
                    "rendered": [p.stem for p in sorted(out_dir.glob("*.png"))],
                    "skipped": [],
                    "title": ctx["title"],
                    "subtitle": ctx["subtitle"],
                    "eyebrow": ctx["eyebrow"],
                    "n_trials": len(ctx["trials"]),
                    "n_variants": ctx["n_variants"],
                    "n_tasks": ctx["n_tasks"],
                    "n_gradeable": ctx["n_gradeable"],
                    "latest_run_at_ms": latest_at_ms,
                })
            except Exception as e:
                summaries.append({
                    "experiment_id": exp_id, "error": str(e),
                    "rendered": [], "skipped": [],
                    "title": exp_id, "subtitle": "",
                    "latest_run_at_ms": latest_at_ms,
                })
            continue
        summaries.append(render_experiment(
            exp_id, config=config,
            latest_run_at_ms=latest_at_ms,
        ))

    print()
    print(_color("─" * 60, "90"))
    total_rendered = sum(len(s.get("rendered", [])) for s in summaries
                         if s["experiment_id"] not in skipped_fresh)
    total_skipped = sum(len(s.get("skipped", [])) for s in summaries
                        if s["experiment_id"] not in skipped_fresh)
    print(f"{total_rendered} charts rendered  ·  {total_skipped} skipped  ·  "
          f"{len(skipped_fresh)} experiment(s) up to date  ·  "
          f"{len(summaries)} total")

    if not args.no_index:
        index_path = render_index(summaries)
        print(f"index: {index_path}")


if __name__ == "__main__":
    main()
