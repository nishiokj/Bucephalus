//! User-facing view specifications.
//!
//! A `ViewSpec` is the editorial contract between raw analysis tables and
//! screens people can actually reason about. SQL/source views remain the
//! durable truth; specs decide scope, naming, aliases, primary fields, and
//! renderer intent.

use anyhow::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ViewScope {
    /// One concrete run directory. This is the only executable scope today.
    Run,
    /// Multiple runs in a comparable lineage. Reserved for run-over-run views.
    RunSet,
    /// Experiment-level history across many runs. Reserved for future trend UI.
    Experiment,
    /// Global inventory / fleet surfaces. Reserved for future operator views.
    Workspace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Overview,
    Results,
    Compare,
    Debug,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Overview => "Overview",
            Category::Results => "Results",
            Category::Compare => "Compare",
            Category::Debug => "Debug",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewRenderer {
    /// Compact metric/status surface. Today this still renders as a table.
    Overview,
    /// Standard columnar table with curated primary columns.
    Table,
    /// Event stream with semantic payload previews.
    Timeline,
    /// A/B or delta-oriented comparison cards.
    Comparison,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowStyle {
    Table,
    Record,
    Event,
    Compare,
}

#[derive(Clone, Copy, Debug)]
pub struct ViewLayout {
    #[allow(dead_code)]
    pub style: RowStyle,
    /// Primary columns shown in the row. Empty means keep the display table
    /// columns after generic metadata elision. Omitted fields remain available
    /// in row detail.
    pub primary: &'static [&'static str],
}

#[derive(Clone, Copy, Debug)]
pub enum ViewQueryPlan {
    Source(&'static str),
    AbComparisonSummary,
    Scoreboard,
}

#[derive(Clone, Copy, Debug)]
pub struct ViewSpec {
    pub name: &'static str,
    #[allow(dead_code)]
    pub title: &'static str,
    #[allow(dead_code)]
    pub question: &'static str,
    pub purpose: &'static str,
    #[allow(dead_code)]
    pub scope: ViewScope,
    pub category: Category,
    pub renderer: ViewRenderer,
    pub plan: ViewQueryPlan,
    pub aliases: &'static [&'static str],
    pub layout: ViewLayout,
}

#[derive(Clone, Debug)]
pub enum ResolvedViewPlan {
    Source(String),
    AbComparisonSummary,
    Scoreboard,
}

#[derive(Clone, Debug)]
pub struct ResolvedView {
    pub name: String,
    pub source: Option<String>,
    pub plan: ResolvedViewPlan,
    pub standardize_ab_terms: bool,
    pub spec: Option<&'static ViewSpec>,
}

const EVENTS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Event,
    primary: &["ts", "trial_id", "event_type"],
};

const RUN_PROGRESS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "run_id",
        "completed_trials",
        "active_trials",
        "variants_seen",
        "tasks_seen",
        "pass_rate",
    ],
};

const VARIANT_SUMMARY_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "variant_id",
        "n_trials",
        "success_rate",
        "primary_metric_mean",
    ],
};

const VARIANT_RANKING_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "variant_id",
        "pass_rate",
        "diff_vs_baseline",
        "mean_primary_metric",
        "n",
    ],
};

const SCOREBOARD_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Table,
    primary: &[],
};

const HEALTH_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[],
};

const COMPARISON_SUMMARY_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "variant_a_rate",
        "variant_b_rate",
        "variant_b_minus_variant_a",
        "cohens_h",
        "magnitude",
    ],
};

const TASK_METRICS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Compare,
    primary: &[
        "task_id",
        "outcome_change",
        "a_outcome",
        "b_outcome",
        "a_result",
        "b_result",
        "d_result",
    ],
};

const TURN_COMPARE_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Compare,
    primary: &[
        "task_id",
        "turn_index",
        "variant_a_status",
        "variant_b_status",
        "delta_tokens_in",
        "delta_tokens_out",
    ],
};

const TRACE_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Compare,
    primary: &[
        "task_id",
        "row_seq",
        "variant_a_event_type",
        "variant_b_event_type",
        "variant_a_tool",
        "variant_b_tool",
        "variant_a_status",
        "variant_b_status",
    ],
};

const PAIRWISE_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "variant_a",
        "variant_b",
        "n_tasks",
        "a_wins",
        "b_wins",
        "ties",
    ],
};

const CONFIGS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &["variant_id", "mean_metric", "pass_rate", "n_trials"],
};

const PARAM_EFFECTS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "parameter_name",
        "parameter_value",
        "mean_metric",
        "std_metric",
        "n",
    ],
};

const PARAM_SENSITIVITY_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "parameter_name",
        "inter_value_variance",
        "value_range",
        "n_values",
    ],
};

const RUN_TREND_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &["run_id", "variant_id", "pass_rate", "n_trials"],
};

const FLAKY_TASKS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "task_id",
        "n_replications",
        "passes",
        "failures",
        "pass_rate",
    ],
};

const FAILURE_CLUSTERS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &["task_group", "total", "failures", "failure_rate"],
};

const fn source_spec(
    name: &'static str,
    title: &'static str,
    question: &'static str,
    purpose: &'static str,
    source: &'static str,
    aliases: &'static [&'static str],
    category: Category,
    renderer: ViewRenderer,
    layout: ViewLayout,
) -> ViewSpec {
    ViewSpec {
        name,
        title,
        question,
        purpose,
        scope: ViewScope::Run,
        category,
        renderer,
        plan: ViewQueryPlan::Source(source),
        aliases,
        layout,
    }
}

const RUN_PROGRESS: ViewSpec = source_spec(
    "run_progress",
    "Run Overview",
    "Is this run alive, healthy, and making progress?",
    "Run completion + pass-rate snapshot.",
    "run_progress",
    &["status", "progress", "overview"],
    Category::Overview,
    ViewRenderer::Overview,
    RUN_PROGRESS_LAYOUT,
);

const HEALTH: ViewSpec = source_spec(
    "health",
    "Health",
    "Can I trust the run contract and scores?",
    "Contract health + score trust.",
    "contract_health",
    &[
        "contract_health",
        "live_health",
        "trust",
        "trial_health",
        "trial_contract_health",
        "score_trust",
    ],
    Category::Overview,
    ViewRenderer::Table,
    HEALTH_LAYOUT,
);

const VARIANT_SUMMARY: ViewSpec = source_spec(
    "variant_summary",
    "Variant Summary",
    "How did each variant perform?",
    "Per-variant pass rate + primary metric.",
    "variant_summary",
    &["variants", "summary_by_variant"],
    Category::Results,
    ViewRenderer::Table,
    VARIANT_SUMMARY_LAYOUT,
);

const SCOREBOARD: ViewSpec = ViewSpec {
    name: "scoreboard",
    title: "Scoreboard",
    question: "What happened on each task?",
    purpose: "Per-task scoreboard grouped by variant.",
    scope: ViewScope::Run,
    category: Category::Results,
    renderer: ViewRenderer::Table,
    plan: ViewQueryPlan::Scoreboard,
    aliases: &[
        "board",
        "scores",
        "tasks",
        "task_variant_matrix",
        "task_matrix",
        "matrix",
        "heatmap",
    ],
    layout: SCOREBOARD_LAYOUT,
};

const COMPARISON_SUMMARY: ViewSpec = ViewSpec {
    name: "comparison_summary",
    title: "A/B Summary",
    question: "Which variant won, by how much, and with what effect size?",
    purpose: "Headline AB stats: rates, delta, McNemar, effect.",
    scope: ViewScope::Run,
    category: Category::Results,
    renderer: ViewRenderer::Comparison,
    plan: ViewQueryPlan::AbComparisonSummary,
    aliases: &[
        "summary",
        "paired_outcomes",
        "paired_diffs",
        "win_loss_tie",
        "effect_size",
        "mcnemar_contingency",
        "task_diffs",
        "variant_summary",
        "variants",
    ],
    layout: COMPARISON_SUMMARY_LAYOUT,
};

const TASK_METRICS: ViewSpec = source_spec(
    "task_metrics",
    "Task Metrics",
    "Which tasks changed between variants?",
    "Per-task A/B outcome + metric deltas.",
    "ab_task_metrics_side_by_side",
    &[
        "task_compare",
        "task_comparison",
        "by_task",
        "task_table",
        "ab_task_metrics_side_by_side",
        "tasks",
        "task_outcomes",
        "outcome_compare",
        "ab_task_outcomes",
        "task_outcome_compare",
        "ab_task_table",
        "scoreboard",
    ],
    Category::Results,
    ViewRenderer::Comparison,
    TASK_METRICS_LAYOUT,
);

const TURN_COMPARE: ViewSpec = source_spec(
    "turn_compare",
    "Turn Compare",
    "Where did the variants diverge turn by turn?",
    "Turn-level A/B (model, status, token deltas).",
    "ab_turn_side_by_side",
    &[
        "turn_diff",
        "turns",
        "turn_side_by_side",
        "trace_turns",
        "ab_turn_side_by_side",
    ],
    Category::Compare,
    ViewRenderer::Comparison,
    TURN_COMPARE_LAYOUT,
);

const TRACE: ViewSpec = source_spec(
    "trace",
    "Trace Compare",
    "What changed at the event level?",
    "Event-level A/B trace diff.",
    "ab_trace_row_side_by_side",
    &[
        "trace_diff",
        "trace_compare",
        "trace_side_by_side",
        "ab_trace_row_side_by_side",
    ],
    Category::Debug,
    ViewRenderer::Comparison,
    TRACE_LAYOUT,
);

const VARIANT_RANKING: ViewSpec = source_spec(
    "variant_ranking",
    "Variant Ranking",
    "Which variant is leading?",
    "Variant leaderboard vs reference.",
    "variant_ranking",
    &["ranking", "leaderboard", "variants", "variant_summary"],
    Category::Results,
    ViewRenderer::Table,
    VARIANT_RANKING_LAYOUT,
);

const PAIRWISE_COMPARE: ViewSpec = source_spec(
    "pairwise_compare",
    "Pairwise Compare",
    "How do variants compare head to head?",
    "Pairwise win/loss/tie counts.",
    "pairwise_comparisons",
    &["pairwise", "pairwise_comparisons"],
    Category::Compare,
    ViewRenderer::Comparison,
    PAIRWISE_LAYOUT,
);

const CONFIG_RANKING: ViewSpec = source_spec(
    "config_ranking",
    "Config Ranking",
    "Which configuration performed best?",
    "Top configurations by metric + pass-rate.",
    "best_config",
    &["best_config", "ranking", "top_configs", "configs", "variant_summary", "variants"],
    Category::Results,
    ViewRenderer::Table,
    CONFIGS_LAYOUT,
);

const PARAMETER_EFFECTS: ViewSpec = source_spec(
    "parameter_effects",
    "Parameter Effects",
    "How did each parameter value affect outcomes?",
    "Average metric per parameter value.",
    "parameter_metric",
    &["parameter_metric", "parameter_impact", "effects"],
    Category::Compare,
    ViewRenderer::Table,
    PARAM_EFFECTS_LAYOUT,
);

const PARAMETER_SENSITIVITY: ViewSpec = source_spec(
    "parameter_sensitivity",
    "Parameter Sensitivity",
    "Which parameters are most sensitive?",
    "Variance + range sensitivity by parameter.",
    "sensitivity",
    &["sensitivity"],
    Category::Compare,
    ViewRenderer::Table,
    PARAM_SENSITIVITY_LAYOUT,
);

const RUN_TREND: ViewSpec = source_spec(
    "run_trend",
    "Run Trend",
    "How is performance moving across comparable runs?",
    "Pass-rate trend per run + variant.",
    "pass_rate_trend",
    &["trend", "pass_rate_trend", "variants", "variant_summary"],
    Category::Results,
    ViewRenderer::Table,
    RUN_TREND_LAYOUT,
);

const FLAKY_TASKS: ViewSpec = source_spec(
    "flaky_tasks",
    "Flaky Tasks",
    "Which tasks are unstable across replications?",
    "Tasks with unstable outcomes across reps.",
    "flaky_tasks",
    &["flaky"],
    Category::Compare,
    ViewRenderer::Table,
    FLAKY_TASKS_LAYOUT,
);

const FAILURE_CLUSTERS: ViewSpec = source_spec(
    "failure_clusters",
    "Failure Clusters",
    "Where are failures concentrated?",
    "Failure concentration by task-group prefix.",
    "failure_clusters",
    &["clusters"],
    Category::Compare,
    ViewRenderer::Table,
    FAILURE_CLUSTERS_LAYOUT,
);

const EVENTS: ViewSpec = source_spec(
    "events",
    "Events",
    "What happened inside the run?",
    "Raw event stream with payload previews.",
    "events",
    &["event_stream", "timeline"],
    Category::Debug,
    ViewRenderer::Timeline,
    EVENTS_LAYOUT,
);

const STANDARD_VIEWS_CORE_ONLY: &[ViewSpec] = &[
    RUN_PROGRESS,
    HEALTH,
    VARIANT_SUMMARY,
    SCOREBOARD,
];

const STANDARD_VIEWS_AB_TEST: &[ViewSpec] = &[
    RUN_PROGRESS,
    HEALTH,
    COMPARISON_SUMMARY,
    TASK_METRICS,
    TURN_COMPARE,
    TRACE,
];

const STANDARD_VIEWS_MULTI_VARIANT: &[ViewSpec] = &[
    RUN_PROGRESS,
    HEALTH,
    VARIANT_RANKING,
    SCOREBOARD,
    PAIRWISE_COMPARE,
];

const STANDARD_VIEWS_PARAMETER_SWEEP: &[ViewSpec] = &[
    RUN_PROGRESS,
    HEALTH,
    CONFIG_RANKING,
    PARAMETER_EFFECTS,
    PARAMETER_SENSITIVITY,
];

const STANDARD_VIEWS_REGRESSION: &[ViewSpec] = &[
    RUN_PROGRESS,
    HEALTH,
    RUN_TREND,
    FLAKY_TASKS,
    FAILURE_CLUSTERS,
];

pub fn standard_views_for_set(view_set: lab_analysis::ViewSet) -> &'static [ViewSpec] {
    match view_set {
        lab_analysis::ViewSet::CoreOnly => STANDARD_VIEWS_CORE_ONLY,
        lab_analysis::ViewSet::AbTest => STANDARD_VIEWS_AB_TEST,
        lab_analysis::ViewSet::MultiVariant => STANDARD_VIEWS_MULTI_VARIANT,
        lab_analysis::ViewSet::ParameterSweep => STANDARD_VIEWS_PARAMETER_SWEEP,
        lab_analysis::ViewSet::Regression => STANDARD_VIEWS_REGRESSION,
    }
}

pub fn standard_view_source_label(spec: &ViewSpec) -> &'static str {
    match spec.plan {
        ViewQueryPlan::Source(source) => source,
        ViewQueryPlan::AbComparisonSummary => "win_loss_tie+effect_size+mcnemar_contingency",
        ViewQueryPlan::Scoreboard => "scoreboard (dynamic)",
    }
}

pub fn normalize_view_key(input: &str) -> String {
    input.trim().replace('-', "_").to_ascii_lowercase()
}

fn find_raw_view_name(raw_view_names: &[String], key: &str) -> Option<String> {
    raw_view_names
        .iter()
        .find(|name| normalize_view_key(name) == key)
        .cloned()
}

pub fn resolved_view_from_spec(
    view_set: lab_analysis::ViewSet,
    spec: &'static ViewSpec,
) -> ResolvedView {
    match spec.plan {
        ViewQueryPlan::Source(source) => ResolvedView {
            name: spec.name.to_string(),
            source: Some(source.to_string()),
            plan: ResolvedViewPlan::Source(source.to_string()),
            standardize_ab_terms: matches!(view_set, lab_analysis::ViewSet::AbTest),
            spec: Some(spec),
        },
        ViewQueryPlan::AbComparisonSummary => ResolvedView {
            name: spec.name.to_string(),
            source: Some(standard_view_source_label(spec).to_string()),
            plan: ResolvedViewPlan::AbComparisonSummary,
            standardize_ab_terms: false,
            spec: Some(spec),
        },
        ViewQueryPlan::Scoreboard => ResolvedView {
            name: spec.name.to_string(),
            source: None,
            plan: ResolvedViewPlan::Scoreboard,
            standardize_ab_terms: false,
            spec: Some(spec),
        },
    }
}

pub fn resolve_requested_view(
    view_set: lab_analysis::ViewSet,
    raw_view_names: &[String],
    requested: &str,
) -> Result<ResolvedView> {
    let normalized = normalize_view_key(requested);
    if normalized.is_empty() {
        anyhow::bail!("view name cannot be empty");
    }

    for spec in standard_views_for_set(view_set) {
        if normalize_view_key(spec.name) == normalized
            || spec
                .aliases
                .iter()
                .any(|alias| normalize_view_key(alias) == normalized)
        {
            return Ok(resolved_view_from_spec(view_set, spec));
        }
    }

    if normalize_view_key(EVENTS.name) == normalized
        || EVENTS
            .aliases
            .iter()
            .any(|alias| normalize_view_key(alias) == normalized)
    {
        return Ok(resolved_view_from_spec(view_set, &EVENTS));
    }

    if let Some(raw_name) = find_raw_view_name(raw_view_names, &normalized) {
        return Ok(ResolvedView {
            name: raw_name.clone(),
            source: Some(raw_name.clone()),
            plan: ResolvedViewPlan::Source(raw_name),
            standardize_ab_terms: false,
            spec: None,
        });
    }

    let available = standard_views_for_set(view_set)
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!(
        "unknown view '{}'. standardized views for {}: {}",
        requested,
        view_set.as_str(),
        available
    );
}

pub fn layout_for_resolved(resolved: &ResolvedView) -> Option<&'static ViewLayout> {
    resolved.spec.map(|spec| &spec.layout)
}

pub fn renderer_for_resolved(resolved: &ResolvedView) -> ViewRenderer {
    resolved
        .spec
        .map(|spec| spec.renderer)
        .unwrap_or(ViewRenderer::Table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_specs_have_explicit_scope() {
        assert!(standard_views_for_set(lab_analysis::ViewSet::AbTest)
            .iter()
            .all(|spec| spec.scope == ViewScope::Run));
    }

    #[test]
    fn resolves_standard_alias_to_spec() {
        let raw = vec!["run_progress".to_string()];
        let resolved =
            resolve_requested_view(lab_analysis::ViewSet::AbTest, &raw, "task-compare").unwrap();
        assert_eq!(resolved.name, "task_metrics");
        assert_eq!(
            resolved.source.as_deref(),
            Some("ab_task_metrics_side_by_side")
        );
        assert!(resolved.spec.is_some());
    }
}
