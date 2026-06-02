use anyhow::Result;
use lab_runner as lab_analysis;
use serde_json::Value;
use std::collections::BTreeMap;

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
    Overview,
    Table,
    Scoreboard,
    Timeline,
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
    pub purpose: &'static str,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldRole {
    Identity,
    Metric,
    Status,
    Timestamp,
    Metadata,
    Payload,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedColumn {
    pub source: String,
    pub label: String,
    pub role: FieldRole,
}

#[derive(Clone, Debug)]
pub struct PresentedTable {
    pub table: lab_analysis::QueryTable,
    pub legend: Vec<(String, String)>,
    #[allow(dead_code)]
    pub columns: Vec<PresentedColumn>,
    #[allow(dead_code)]
    pub hidden_columns: Vec<String>,
}

const EVENTS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Event,
    primary: &["ts", "trial_id", "event_type"],
};

const RUN_PROGRESS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "completed_trials",
        "active_trials",
        "total_trials",
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
    primary: &[
        "variant_id",
        "task_id",
        "n_trials",
        "success_rate",
        "primary_metric_mean",
        "lifecycle",
        "started_at",
    ],
};

const HEALTH_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "completed_trials",
        "trusted_scores",
        "untrusted_scores",
        "warning_trials",
        "error_trials",
        "empty_predictions",
        "grader_or_mapping_errors",
        "connector_errors",
    ],
};

const COMPARISON_SUMMARY_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Record,
    primary: &[
        "variant_a",
        "variant_b",
        "variant_a_id",
        "variant_b_id",
        "variant_a_n",
        "variant_b_n",
        "variant_a_rate",
        "variant_b_rate",
        "variant_b_minus_variant_a",
        "mcnemar_chi2",
        "cohens_h",
        "magnitude",
    ],
};

const TASK_METRICS_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Compare,
    primary: &[
        "task_id",
        "repl_idx",
        "outcome_change",
        "a_outcome",
        "b_outcome",
        "a_result",
        "b_result",
        "d_result",
        "a_resolved",
        "b_resolved",
        "d_resolved",
        "a_tokens_in",
        "b_tokens_in",
        "d_tokens_in",
        "a_tokens_out",
        "b_tokens_out",
        "d_tokens_out",
    ],
};

const TURN_COMPARE_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Compare,
    primary: &[
        "task_id",
        "repl_idx",
        "turn_index",
        "a_status",
        "b_status",
        "a_model",
        "b_model",
        "a_tokens_in",
        "b_tokens_in",
        "d_tokens_in",
        "a_tokens_out",
        "b_tokens_out",
        "d_tokens_out",
        "variant_a_status",
        "variant_b_status",
        "variant_a_model",
        "variant_b_model",
        "delta_tokens_in",
        "delta_tokens_out",
    ],
};

const TRACE_LAYOUT: ViewLayout = ViewLayout {
    style: RowStyle::Compare,
    primary: &[
        "task_id",
        "repl_idx",
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
        "a_win_rate",
        "b_win_rate",
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
    purpose: &'static str,
    source: &'static str,
    aliases: &'static [&'static str],
    category: Category,
    renderer: ViewRenderer,
    layout: ViewLayout,
) -> ViewSpec {
    ViewSpec {
        name,
        purpose,
        category,
        renderer,
        plan: ViewQueryPlan::Source(source),
        aliases,
        layout,
    }
}

const RUN_PROGRESS: ViewSpec = source_spec(
    "run_progress",
    "Run completion + pass-rate snapshot.",
    "run_progress",
    &["status", "progress", "overview"],
    Category::Overview,
    ViewRenderer::Overview,
    RUN_PROGRESS_LAYOUT,
);

const HEALTH: ViewSpec = source_spec(
    "health",
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
    ViewRenderer::Overview,
    HEALTH_LAYOUT,
);

const VARIANT_SUMMARY: ViewSpec = source_spec(
    "variant_summary",
    "Per-variant pass rate + primary metric.",
    "variant_summary",
    &["variants", "summary_by_variant"],
    Category::Results,
    ViewRenderer::Table,
    VARIANT_SUMMARY_LAYOUT,
);

const SCOREBOARD: ViewSpec = ViewSpec {
    name: "scoreboard",
    purpose: "Per-task scoreboard grouped by variant.",
    category: Category::Results,
    renderer: ViewRenderer::Scoreboard,
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
    purpose: "Headline AB stats: rates, delta, McNemar, effect.",
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
    "Variant leaderboard vs reference.",
    "variant_ranking",
    &["ranking", "leaderboard", "variants", "variant_summary"],
    Category::Results,
    ViewRenderer::Table,
    VARIANT_RANKING_LAYOUT,
);

const PAIRWISE_COMPARE: ViewSpec = source_spec(
    "pairwise_compare",
    "Pairwise win/loss/tie counts.",
    "pairwise_comparisons",
    &["pairwise", "pairwise_comparisons"],
    Category::Compare,
    ViewRenderer::Comparison,
    PAIRWISE_LAYOUT,
);

const CONFIG_RANKING: ViewSpec = source_spec(
    "config_ranking",
    "Top configurations by metric + pass-rate.",
    "best_config",
    &[
        "best_config",
        "ranking",
        "top_configs",
        "configs",
        "variant_summary",
        "variants",
    ],
    Category::Results,
    ViewRenderer::Table,
    CONFIGS_LAYOUT,
);

const PARAMETER_EFFECTS: ViewSpec = source_spec(
    "parameter_effects",
    "Average metric per parameter value.",
    "parameter_metric",
    &["parameter_metric", "parameter_impact", "effects"],
    Category::Compare,
    ViewRenderer::Table,
    PARAM_EFFECTS_LAYOUT,
);

const PARAMETER_SENSITIVITY: ViewSpec = source_spec(
    "parameter_sensitivity",
    "Variance + range sensitivity by parameter.",
    "sensitivity",
    &["sensitivity"],
    Category::Compare,
    ViewRenderer::Table,
    PARAM_SENSITIVITY_LAYOUT,
);

const RUN_TREND: ViewSpec = source_spec(
    "run_trend",
    "Pass-rate trend per run + variant.",
    "pass_rate_trend",
    &["trend", "pass_rate_trend", "variants", "variant_summary"],
    Category::Results,
    ViewRenderer::Table,
    RUN_TREND_LAYOUT,
);

const FLAKY_TASKS: ViewSpec = source_spec(
    "flaky_tasks",
    "Tasks with unstable outcomes across reps.",
    "flaky_tasks",
    &["flaky"],
    Category::Compare,
    ViewRenderer::Table,
    FLAKY_TASKS_LAYOUT,
);

const FAILURE_CLUSTERS: ViewSpec = source_spec(
    "failure_clusters",
    "Failure concentration by task-group prefix.",
    "failure_clusters",
    &["clusters"],
    Category::Compare,
    ViewRenderer::Table,
    FAILURE_CLUSTERS_LAYOUT,
);

const EVENTS: ViewSpec = source_spec(
    "events",
    "Raw event stream with payload previews.",
    "events",
    &["event_stream", "timeline"],
    Category::Debug,
    ViewRenderer::Timeline,
    EVENTS_LAYOUT,
);

const STANDARD_VIEWS_CORE_ONLY: &[ViewSpec] = &[RUN_PROGRESS, HEALTH, VARIANT_SUMMARY, SCOREBOARD];

const STANDARD_VIEWS_AB_TEST: &[ViewSpec] = &[
    RUN_PROGRESS,
    HEALTH,
    SCOREBOARD,
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

pub fn renderer_for_resolved(resolved: &ResolvedView) -> ViewRenderer {
    resolved
        .spec
        .map(|spec| spec.renderer)
        .unwrap_or(ViewRenderer::Table)
}

pub fn present_table(spec: Option<&ViewSpec>, table: &lab_analysis::QueryTable) -> PresentedTable {
    let selected_indices = select_display_indices(spec, table);
    let (aliases, legend) = build_aliases(table, &selected_indices);
    let mut presented_columns = Vec::with_capacity(selected_indices.len());
    let columns = selected_indices
        .iter()
        .map(|&idx| {
            let source = table.columns[idx].clone();
            let role = classify_field(&source);
            let label = display_label_for_column(&source);
            presented_columns.push(PresentedColumn {
                source,
                label: label.clone(),
                role,
            });
            label
        })
        .collect::<Vec<_>>();

    let rows = table
        .rows
        .iter()
        .map(|row| {
            selected_indices
                .iter()
                .map(|&idx| {
                    let source = &table.columns[idx];
                    let value = row.get(idx).cloned().unwrap_or(Value::Null);
                    present_cell_value(source, &value, aliases.get(&idx))
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let hidden_columns = table
        .columns
        .iter()
        .enumerate()
        .filter_map(|(idx, column)| (!selected_indices.contains(&idx)).then(|| column.clone()))
        .collect();

    PresentedTable {
        table: lab_analysis::QueryTable { columns, rows },
        legend,
        columns: presented_columns,
        hidden_columns,
    }
}

fn build_aliases(
    table: &lab_analysis::QueryTable,
    selected_indices: &[usize],
) -> (
    BTreeMap<usize, BTreeMap<String, String>>,
    Vec<(String, String)>,
) {
    let mut aliases_by_column = BTreeMap::new();
    let mut legend = Vec::new();
    let mut values_by_prefix: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut columns_by_prefix: BTreeMap<&'static str, Vec<usize>> = BTreeMap::new();

    for &idx in selected_indices {
        let Some(prefix) = alias_prefix_for_column(&table.columns[idx]) else {
            continue;
        };
        columns_by_prefix.entry(prefix).or_default().push(idx);
        let values = values_by_prefix.entry(prefix).or_default();
        for row in &table.rows {
            let value = row.get(idx).map(cell_to_string).unwrap_or_default();
            if value.trim().is_empty() || value == "null" || values.contains(&value) {
                continue;
            }
            values.push(value);
        }
    }

    for (prefix, values) in values_by_prefix {
        if values.len() <= 1 {
            continue;
        }

        let mut map = BTreeMap::new();
        for (alias_idx, value) in values.into_iter().enumerate() {
            let alias = format!("{prefix}{}", alias_idx + 1);
            legend.push((alias.clone(), value.clone()));
            map.insert(value, alias);
        }
        for idx in columns_by_prefix.get(prefix).into_iter().flatten() {
            aliases_by_column.insert(*idx, map.clone());
        }
    }

    (aliases_by_column, legend)
}

fn alias_prefix_for_column(column: &str) -> Option<&'static str> {
    if matches!(
        column,
        "variant_id"
            | "variant_a"
            | "variant_b"
            | "variant_a_id"
            | "variant_b_id"
            | "baseline_id"
            | "treatment_id"
            | "a_variant_id"
            | "b_variant_id"
    ) {
        Some("V")
    } else {
        None
    }
}

fn select_display_indices(spec: Option<&ViewSpec>, table: &lab_analysis::QueryTable) -> Vec<usize> {
    let mut indices = Vec::new();
    if let Some(spec) = spec {
        for name in spec.layout.primary {
            if let Some(idx) = table.columns.iter().position(|column| column == name) {
                if !indices.contains(&idx) && !is_payload_column(name) {
                    indices.push(idx);
                }
            }
        }
    }

    if indices.is_empty() {
        for (idx, column) in table.columns.iter().enumerate() {
            if !is_metadata_column(column) && !is_payload_column(column) {
                indices.push(idx);
            }
        }
    }

    indices
}

fn classify_field(column: &str) -> FieldRole {
    if is_payload_column(column) {
        FieldRole::Payload
    } else if is_metadata_column(column) {
        FieldRole::Metadata
    } else if column.ends_with("_id")
        || matches!(
            column,
            "run_id"
                | "trial_id"
                | "task_id"
                | "variant_id"
                | "variant_a"
                | "variant_b"
                | "baseline_id"
                | "treatment_id"
        )
    {
        FieldRole::Identity
    } else if column.contains("status") || column.contains("outcome") || column == "lifecycle" {
        FieldRole::Status
    } else if column.ends_with("_at") || column == "ts" || column == "timestamp" {
        FieldRole::Timestamp
    } else if column.contains("rate")
        || column.contains("metric")
        || column.contains("tokens")
        || column.ends_with("_trials")
        || column.ends_with("_scores")
        || column.ends_with("_seen")
        || matches!(
            column,
            "trusted_scores"
                | "untrusted_scores"
                | "unknown_score_trust"
                | "warning_trials"
                | "error_trials"
                | "empty_predictions"
                | "grader_or_mapping_errors"
                | "connector_errors"
                | "a_wins"
                | "b_wins"
                | "ties"
                | "n_tasks"
                | "n_values"
                | "passes"
                | "failures"
                | "total"
        )
        || column == "n"
    {
        FieldRole::Metric
    } else {
        FieldRole::Text
    }
}

fn is_payload_column(column: &str) -> bool {
    matches!(column, "payload" | "payload_json" | "row_json")
        || column.ends_with("_json")
        || column.contains("payload")
}

fn is_metadata_column(column: &str) -> bool {
    matches!(
        column,
        "run_id"
            | "slot_commit_id"
            | "schedule_idx"
            | "attempt"
            | "row_seq"
            | "worker_id"
            | "call_id"
            | "variant_a_call_id"
            | "variant_b_call_id"
            | "variant_a_trial_id"
            | "variant_b_trial_id"
            | "baseline_trial_id"
            | "treatment_trial_id"
            | "a_trial_id"
            | "b_trial_id"
    )
}

fn display_label_for_column(column: &str) -> String {
    let mapped = match column {
        "variant_id" => "variant",
        "task_id" => "task",
        "trial_id" => "trial",
        "experiment_id" => "experiment",
        "baseline_id" => "baseline",
        "primary_metric_mean" => "metric",
        "mean_primary_metric" => "metric",
        "mean_metric" => "metric",
        "std_metric" => "std",
        "primary_metric_value" => "metric_val",
        "primary_metric_name" => "metric_name",
        "variant_a_rate" => "A pass%",
        "variant_b_rate" => "B pass%",
        "variant_b_minus_variant_a" => "B-A",
        "diff_vs_baseline" => "vs_ref",
        "cohens_h" => "h",
        "mcnemar_chi2" => "chi2",
        "magnitude" => "effect",
        "success_rate" | "pass_rate" => "pass%",
        "failure_rate" => "fail%",
        "n_trials" | "trial_count" => "trials",
        "n_tasks" => "tasks",
        "n_replications" => "reps",
        "n_values" => "values",
        "variant_count" => "variants",
        "task_count" => "tasks",
        "active_trials" => "active",
        "completed_trials" => "done",
        "total_trials" => "total",
        "variants_seen" => "variants",
        "tasks_seen" => "tasks",
        "event_type" => "event",
        "outcome_change" => "change",
        "turn_number" | "turn_index" => "turn",
        "tool_name" => "tool",
        "status_code" => "status",
        "error_message" => "error",
        "metric_name" => "metric",
        "metric_value" => "value",
        "started_at" => "started",
        "completed_at" => "completed",
        "updated_at" => "updated",
        "duration_seconds" => "dur_s",
        "win_rate" => "win%",
        "loss_rate" => "loss%",
        "tie_rate" => "tie%",
        "effect_size" => "effect",
        "mcnemar_p" => "p_val",
        "outcome" => "outcome",
        "trusted_scores" => "trusted",
        "untrusted_scores" => "untrusted",
        "unknown_score_trust" => "unknown",
        "warning_trials" => "warnings",
        "error_trials" => "errors",
        "empty_predictions" => "empty",
        "grader_or_mapping_errors" => "grader_err",
        "connector_errors" => "connector_err",
        "parameter_name" => "parameter",
        "parameter_value" => "value",
        "inter_value_variance" => "variance",
        "value_range" => "range",
        "task_group" => "group",
        _ => "",
    };
    if !mapped.is_empty() {
        return mapped.to_string();
    }

    if let Some(rest) = column.strip_prefix("variant_a_") {
        return format!("a_{}", display_label_for_column(rest));
    }
    if let Some(rest) = column.strip_prefix("variant_b_") {
        return format!("b_{}", display_label_for_column(rest));
    }
    if let Some(rest) = column.strip_prefix("delta_") {
        return format!("d_{}", display_label_for_column(rest));
    }
    if let Some(rest) = column.strip_suffix("_count") {
        return format!("{rest}s");
    }

    column.to_string()
}

fn present_cell_value(
    column: &str,
    value: &Value,
    aliases: Option<&BTreeMap<String, String>>,
) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    if let Some(alias) = aliases.and_then(|map| map.get(&cell_to_string(value))) {
        return Value::String(alias.clone());
    }
    match classify_field(column) {
        FieldRole::Identity => Value::String(compact_identifier_value(&cell_to_string(value))),
        FieldRole::Timestamp => Value::String(compact_timestamp_value(&cell_to_string(value))),
        FieldRole::Metric if column.ends_with("rate") || column.ends_with("_pct") => {
            Value::String(format_rate_value(value))
        }
        FieldRole::Payload
        | FieldRole::Metadata
        | FieldRole::Metric
        | FieldRole::Status
        | FieldRole::Text => value.clone(),
    }
}

fn cell_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => v.clone(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn compact_identifier_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let (tag, rest) = if let Some(rest) = trimmed.strip_prefix("trial_") {
        ("tr", rest)
    } else if let Some(rest) = trimmed.strip_prefix("task_") {
        ("tk", rest)
    } else if let Some(rest) = trimmed.strip_prefix("run_") {
        ("rn", rest)
    } else if let Some(rest) = trimmed.strip_prefix("variant_") {
        ("v", rest)
    } else {
        ("", trimmed)
    };
    let tail_len = 8;
    let count = rest.chars().count();
    if count <= tail_len {
        if tag.is_empty() {
            return trimmed.to_string();
        }
        return format!("{tag}:{rest}");
    }
    let tail: String = rest.chars().skip(count - tail_len).collect();
    if tag.is_empty() {
        format!("...{tail}")
    } else {
        format!("{tag}:...{tail}")
    }
}

fn compact_timestamp_value(value: &str) -> String {
    if value.len() >= 19 && value.as_bytes().get(10) == Some(&b'T') {
        value[11..19].to_string()
    } else {
        value.to_string()
    }
}

fn format_rate_value(value: &Value) -> String {
    let Some(rate) = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<f64>().ok()))
    else {
        return cell_to_string(value);
    };
    if (0.0..=1.0).contains(&rate) {
        format!("{:.0}%", rate * 100.0)
    } else {
        format!("{rate:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn presentation_projects_run_progress_to_curated_columns() {
        let raw = lab_analysis::QueryTable {
            columns: vec![
                "run_id".to_string(),
                "completed_trials".to_string(),
                "variants_seen".to_string(),
                "tasks_seen".to_string(),
                "pass_rate".to_string(),
                "slot_commit_id".to_string(),
                "payload_json".to_string(),
            ],
            rows: vec![vec![
                Value::String("run_abcdefghijklmnopqrstuvwxyz".to_string()),
                Value::from(42),
                Value::from(2),
                Value::from(21),
                Value::from(0.5),
                Value::String("slot_1".to_string()),
                Value::String("{\"nope\":true}".to_string()),
            ]],
        };

        let presented = present_table(Some(&RUN_PROGRESS), &raw);
        assert_eq!(
            presented.table.columns,
            vec!["done", "variants", "tasks", "pass%"]
        );
        assert_eq!(presented.table.rows[0][3], Value::String("50%".to_string()));
        assert!(presented.hidden_columns.contains(&"run_id".to_string()));
        assert!(presented
            .hidden_columns
            .contains(&"slot_commit_id".to_string()));
        assert!(presented
            .hidden_columns
            .contains(&"payload_json".to_string()));
    }

    #[test]
    fn presentation_fallback_hides_metadata_and_payloads() {
        let raw = lab_analysis::QueryTable {
            columns: vec![
                "task_id".to_string(),
                "row_seq".to_string(),
                "payload_json".to_string(),
                "outcome".to_string(),
            ],
            rows: vec![vec![
                Value::String("task_123456789abcdef".to_string()),
                Value::from(9),
                Value::String("{\"debug\":true}".to_string()),
                Value::String("success".to_string()),
            ]],
        };

        let presented = present_table(None, &raw);
        assert_eq!(presented.table.columns, vec!["task", "outcome"]);
        assert_eq!(
            presented.table.rows[0][0],
            Value::String("tk:...89abcdef".to_string())
        );
        assert_eq!(
            presented.hidden_columns,
            vec!["row_seq".to_string(), "payload_json".to_string()]
        );
    }

    #[test]
    fn presentation_aliases_variants_with_legend() {
        let raw = lab_analysis::QueryTable {
            columns: vec![
                "variant_id".to_string(),
                "task_id".to_string(),
                "n_trials".to_string(),
                "success_rate".to_string(),
            ],
            rows: vec![
                vec![
                    Value::String("codex_spark_really_long_variant_name".to_string()),
                    Value::String("django__django-12345".to_string()),
                    Value::from(1),
                    Value::from(1.0),
                ],
                vec![
                    Value::String("glm_5_really_long_variant_name".to_string()),
                    Value::String("django__django-67890".to_string()),
                    Value::from(1),
                    Value::from(0.0),
                ],
            ],
        };

        let presented = present_table(Some(&SCOREBOARD), &raw);
        assert_eq!(presented.table.rows[0][0], Value::String("V1".to_string()));
        assert_eq!(presented.table.rows[1][0], Value::String("V2".to_string()));
        assert_eq!(
            presented.legend,
            vec![
                (
                    "V1".to_string(),
                    "codex_spark_really_long_variant_name".to_string()
                ),
                (
                    "V2".to_string(),
                    "glm_5_really_long_variant_name".to_string()
                )
            ]
        );
    }

    #[test]
    fn presentation_aliases_pairwise_variants_in_one_namespace() {
        let raw = lab_analysis::QueryTable {
            columns: vec![
                "variant_a".to_string(),
                "variant_b".to_string(),
                "n_tasks".to_string(),
                "a_wins".to_string(),
                "b_wins".to_string(),
                "ties".to_string(),
            ],
            rows: vec![
                vec![
                    Value::String("alpha".to_string()),
                    Value::String("beta".to_string()),
                    Value::from(10),
                    Value::from(3),
                    Value::from(4),
                    Value::from(3),
                ],
                vec![
                    Value::String("alpha".to_string()),
                    Value::String("gamma".to_string()),
                    Value::from(10),
                    Value::from(5),
                    Value::from(2),
                    Value::from(3),
                ],
                vec![
                    Value::String("beta".to_string()),
                    Value::String("gamma".to_string()),
                    Value::from(10),
                    Value::from(1),
                    Value::from(6),
                    Value::from(3),
                ],
            ],
        };

        let presented = present_table(Some(&PAIRWISE_COMPARE), &raw);
        assert_eq!(presented.table.rows[0][0], Value::String("V1".to_string()));
        assert_eq!(presented.table.rows[0][1], Value::String("V2".to_string()));
        assert_eq!(presented.table.rows[1][0], Value::String("V1".to_string()));
        assert_eq!(presented.table.rows[1][1], Value::String("V3".to_string()));
        assert_eq!(presented.table.rows[2][0], Value::String("V2".to_string()));
        assert_eq!(presented.table.rows[2][1], Value::String("V3".to_string()));
        assert_eq!(
            presented.legend,
            vec![
                ("V1".to_string(), "alpha".to_string()),
                ("V2".to_string(), "beta".to_string()),
                ("V3".to_string(), "gamma".to_string())
            ]
        );
    }
}
