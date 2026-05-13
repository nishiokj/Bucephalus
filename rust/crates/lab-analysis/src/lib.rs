use anyhow::{anyhow, Context, Result};
#[cfg(feature = "duckdb_engine")]
use duckdb::Connection;
#[cfg(feature = "duckdb_engine")]
use include_dir::{include_dir, Dir};
#[cfg(feature = "duckdb_engine")]
use lab_core::sha256_bytes;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
#[cfg(feature = "duckdb_engine")]
use std::path::PathBuf;

#[cfg(feature = "duckdb_engine")]
static VIEW_BUNDLES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/views");

#[cfg(feature = "duckdb_engine")]
const ACCOUNT_SQLITE_FILE: &str = "agentlab.sqlite";
#[cfg(feature = "duckdb_engine")]
const AGENTLAB_DB_ENV: &str = "AGENTLAB_DB";
#[cfg(all(feature = "duckdb_engine", not(test)))]
const AGENTLAB_HOME_ENV: &str = "AGENTLAB_HOME";
#[cfg(feature = "duckdb_engine")]
const AGENTLAB_ACCOUNT_ID_ENV: &str = "AGENTLAB_ACCOUNT_ID";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewSet {
    CoreOnly,
    AbTest,
    MultiVariant,
    ParameterSweep,
    Regression,
}

impl ViewSet {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CoreOnly => "core_only",
            Self::AbTest => "ab_test",
            Self::MultiVariant => "multi_variant",
            Self::ParameterSweep => "parameter_sweep",
            Self::Regression => "regression",
        }
    }

    pub fn headline_view(self) -> Option<&'static str> {
        match self {
            Self::AbTest => Some("win_loss_tie"),
            Self::MultiVariant => Some("variant_ranking"),
            Self::ParameterSweep => Some("best_config"),
            Self::Regression => Some("pass_rate_trend"),
            Self::CoreOnly => None,
        }
    }

    #[cfg(feature = "duckdb_engine")]
    fn bundle_file(self) -> Option<&'static str> {
        match self {
            Self::CoreOnly => None,
            Self::AbTest => Some("ab_test.sql"),
            Self::MultiVariant => Some("multi_variant.sql"),
            Self::ParameterSweep => Some("parameter_sweep.sql"),
            Self::Regression => Some("regression.sql"),
        }
    }
}

#[derive(Debug, Clone)]
struct ExperimentDesign {
    comparison: String,
    scheduling: String,
    variant_count: usize,
}

#[derive(Debug, Clone)]
struct RunAnalysisContext {
    #[cfg(feature = "duckdb_engine")]
    account_id: String,
    #[cfg(feature = "duckdb_engine")]
    run_id: String,
    #[cfg(feature = "duckdb_engine")]
    db_path: PathBuf,
    #[cfg(feature = "duckdb_engine")]
    comparison_policy: String,
    #[cfg(feature = "duckdb_engine")]
    scheduling_policy: String,
    view_set: ViewSet,
}

#[derive(Debug, Clone)]
pub struct QueryTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

pub fn run_view_set(run_dir: &Path) -> Result<ViewSet> {
    let context = load_run_context(run_dir)?;
    Ok(context.view_set)
}

#[cfg(not(feature = "duckdb_engine"))]
fn duckdb_disabled_error(op: &str) -> anyhow::Error {
    anyhow!(
        "DuckDB support is disabled in this binary; '{}' is unavailable.\n\
         Rebuild with:\n\
         cargo build --manifest-path rust/Cargo.toml -p lab-cli --release --features lab-analysis/duckdb_engine\n\
         Then run the rebuilt binary directly (for example: rust/target/release/lab-cli ...).",
        op
    )
}

#[cfg(feature = "duckdb_engine")]
fn active_account_id() -> String {
    if let Ok(value) = std::env::var(AGENTLAB_ACCOUNT_ID_ENV) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "local".to_string());
    let home = std::env::var("HOME").unwrap_or_default();
    let digest = sha256_bytes(format!("{user}|{home}").as_bytes());
    let hex = digest.strip_prefix("sha256:").unwrap_or(&digest);
    format!("local-{}", &hex[..16])
}

#[cfg(feature = "duckdb_engine")]
fn account_sqlite_path_for_run(_run_dir: &Path) -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os(AGENTLAB_DB_ENV) {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err(anyhow!("{} must be an absolute path", AGENTLAB_DB_ENV));
        }
        return Ok(path);
    }

    #[cfg(test)]
    {
        return Ok(_run_dir.join(".agentlab").join(ACCOUNT_SQLITE_FILE));
    }

    #[cfg(not(test))]
    {
        let home = if let Some(raw) = std::env::var_os(AGENTLAB_HOME_ENV) {
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(anyhow!("{} must be an absolute path", AGENTLAB_HOME_ENV));
            }
            path
        } else {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("HOME is not set; set {}", AGENTLAB_HOME_ENV))?;
            home.join(".agentlab")
        };
        Ok(home.join(ACCOUNT_SQLITE_FILE))
    }
}

#[cfg(feature = "duckdb_engine")]
pub fn list_views(run_dir: &Path) -> Result<Vec<String>> {
    let context = load_run_context(run_dir)?;
    let list_sql = "SELECT view_name AS table_name
                    FROM duckdb_views()
                    WHERE schema_name = 'main'
                      AND view_name NOT LIKE 'duckdb_%'
                      AND view_name NOT LIKE 'sqlite_%'
                      AND view_name NOT LIKE 'pragma_%'
                    ORDER BY view_name";
    let table = query_run_with_duckdb(&context, list_sql)?;
    let mut out = Vec::new();
    for row in table.rows {
        if let Some(name) = row.first().and_then(Value::as_str) {
            out.push(name.to_string());
        }
    }
    Ok(out)
}

#[cfg(not(feature = "duckdb_engine"))]
pub fn list_views(_run_dir: &Path) -> Result<Vec<String>> {
    Err(duckdb_disabled_error("views"))
}

pub fn query_view(run_dir: &Path, view_name: &str, limit: usize) -> Result<QueryTable> {
    if !is_safe_identifier(view_name) {
        return Err(anyhow!(
            "invalid view name '{}': only [A-Za-z0-9_] is allowed",
            view_name
        ));
    }
    let safe_limit = if limit == 0 { 100 } else { limit };
    let sql = format!(
        "SELECT * FROM {} LIMIT {}",
        quote_identifier(view_name),
        safe_limit
    );
    query_run(run_dir, &sql)
}

#[cfg(feature = "duckdb_engine")]
pub fn query_run(run_dir: &Path, sql: &str) -> Result<QueryTable> {
    let normalized = validate_read_only_sql(sql)?;
    let context = load_run_context(run_dir)?;
    query_run_with_duckdb(&context, &normalized)
}

#[cfg(not(feature = "duckdb_engine"))]
pub fn query_run(_run_dir: &Path, _sql: &str) -> Result<QueryTable> {
    Err(duckdb_disabled_error("query"))
}

#[cfg(feature = "duckdb_engine")]
pub fn query_trend(
    project_root: &Path,
    experiment_id: &str,
    task_id: Option<&str>,
    variant_id: Option<&str>,
) -> Result<QueryTable> {
    let experiment_id = experiment_id.trim();
    if experiment_id.is_empty() {
        return Err(anyhow!("experiment_id cannot be empty"));
    }

    let db_path = account_sqlite_path_for_run(project_root)?;
    if !db_path.exists() {
        return Err(anyhow!(
            "account sqlite database not found: {}",
            db_path.display()
        ));
    }
    let account_id = active_account_id();
    let conn = Connection::open_in_memory().context("failed to open in-memory DuckDB")?;
    load_account_trend_views(&conn, &db_path, &account_id)?;

    let mut conditions = vec![format!("r.experiment_id = {}", sql_literal(experiment_id))];
    if let Some(task) = task_id {
        if !task.trim().is_empty() {
            conditions.push(format!("t.task_id = {}", sql_literal(task.trim())));
        }
    }
    if let Some(variant) = variant_id {
        if !variant.trim().is_empty() {
            conditions.push(format!("t.variant_id = {}", sql_literal(variant.trim())));
        }
    }

    let sql = format!(
        "SELECT
            t.run_id,
            t.variant_id,
            round(avg(CASE WHEN t.outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS pass_rate,
            count(*) AS n_trials
         FROM all_trials t
         JOIN all_runs r USING (run_id)
         WHERE {}
         GROUP BY t.run_id, t.variant_id
         ORDER BY t.run_id, t.variant_id",
        conditions.join(" AND ")
    );
    execute_select_query(&conn, &sql)
}

#[cfg(not(feature = "duckdb_engine"))]
pub fn query_trend(
    _project_root: &Path,
    _experiment_id: &str,
    _task_id: Option<&str>,
    _variant_id: Option<&str>,
) -> Result<QueryTable> {
    Err(duckdb_disabled_error("trend"))
}

fn load_run_context(run_dir: &Path) -> Result<RunAnalysisContext> {
    let canonical = run_dir
        .canonicalize()
        .map_err(|_| anyhow!("run directory not found: {}", run_dir.display()))?;
    #[cfg(feature = "duckdb_engine")]
    let db_path = account_sqlite_path_for_run(&canonical)?;
    #[cfg(feature = "duckdb_engine")]
    if !db_path.exists() {
        return Err(anyhow!(
            "account sqlite database not found for run {}: {}",
            canonical.display(),
            db_path.display()
        ));
    }
    let resolved = read_resolved_experiment(&canonical)?;
    let design = resolved
        .as_ref()
        .map(parse_experiment_design)
        .unwrap_or_else(default_experiment_design);
    let view_set = view_set_for_design(&design);
    Ok(RunAnalysisContext {
        #[cfg(feature = "duckdb_engine")]
        account_id: active_account_id(),
        #[cfg(feature = "duckdb_engine")]
        run_id: canonical
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("run")
            .to_string(),
        #[cfg(feature = "duckdb_engine")]
        db_path,
        #[cfg(feature = "duckdb_engine")]
        comparison_policy: design.comparison,
        #[cfg(feature = "duckdb_engine")]
        scheduling_policy: design.scheduling,
        view_set,
    })
}

fn read_resolved_experiment(run_dir: &Path) -> Result<Option<Value>> {
    let path = run_dir.join("resolved_experiment.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let value = serde_json::from_str::<Value>(&raw)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    Ok(Some(value))
}

fn default_experiment_design() -> ExperimentDesign {
    ExperimentDesign {
        comparison: "paired".to_string(),
        scheduling: "variant_sequential".to_string(),
        variant_count: 1,
    }
}

fn parse_experiment_design(resolved: &Value) -> ExperimentDesign {
    let comparison = resolved
        .pointer("/design/policies/comparison")
        .and_then(Value::as_str)
        .or_else(|| {
            resolved
                .pointer("/design/comparison")
                .and_then(Value::as_str)
        })
        .unwrap_or("paired")
        .trim()
        .to_ascii_lowercase();
    let scheduling = resolved
        .pointer("/design/policies/scheduling")
        .and_then(Value::as_str)
        .unwrap_or("variant_sequential")
        .trim()
        .to_ascii_lowercase();

    let mut variants = BTreeSet::new();
    if let Some(base) = resolved
        .pointer("/baseline/variant_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        variants.insert(base.to_string());
    }
    if let Some(plan) = resolved.pointer("/variant_plan").and_then(Value::as_array) {
        for item in plan {
            if let Some(id) = item
                .get("variant_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                variants.insert(id.to_string());
            }
        }
    }
    ExperimentDesign {
        comparison,
        scheduling,
        variant_count: variants.len().max(1),
    }
}

fn view_set_for_design(design: &ExperimentDesign) -> ViewSet {
    if design.comparison == "none" {
        return ViewSet::Regression;
    }
    if design.scheduling == "paired_interleaved" && design.comparison == "paired" {
        return if design.variant_count <= 2 {
            ViewSet::AbTest
        } else {
            ViewSet::MultiVariant
        };
    }
    if design.scheduling == "variant_sequential" && design.comparison == "unpaired" {
        return ViewSet::ParameterSweep;
    }

    match design.comparison.as_str() {
        "paired" => {
            if design.variant_count <= 2 {
                ViewSet::AbTest
            } else {
                ViewSet::MultiVariant
            }
        }
        "unpaired" => ViewSet::ParameterSweep,
        _ => ViewSet::CoreOnly,
    }
}

#[cfg(feature = "duckdb_engine")]
fn load_view_bundle_sql(view_set: ViewSet) -> Result<Option<String>> {
    let Some(file_name) = view_set.bundle_file() else {
        return Ok(None);
    };
    let file = VIEW_BUNDLES
        .get_file(file_name)
        .ok_or_else(|| anyhow!("missing embedded view bundle: {}", file_name))?;
    let content = file
        .contents_utf8()
        .ok_or_else(|| anyhow!("view bundle is not valid UTF-8: {}", file_name))?;
    Ok(Some(content.to_string()))
}

#[cfg(feature = "duckdb_engine")]
fn build_load_sql(context: &RunAnalysisContext, bundle_sql: Option<&str>) -> String {
    let mut sql = String::from("LOAD json;\nLOAD sqlite_scanner;\n");
    sql.push_str(&format!(
        "ATTACH {} AS account_db (TYPE sqlite);\n",
        sql_literal_path(&context.db_path)
    ));
    let account_id = sql_literal(&context.account_id);
    let run_id = sql_literal(&context.run_id);
    sql.push_str(&build_fact_views_sql(&account_id, Some(run_id.as_str())));
    sql.push_str(&build_metadata_view_sql(context));
    sql.push('\n');
    if let Some(bundle) = bundle_sql {
        sql.push_str(bundle);
        if !bundle.ends_with('\n') {
            sql.push('\n');
        }
    }
    sql
}

#[cfg(feature = "duckdb_engine")]
fn build_metadata_view_sql(context: &RunAnalysisContext) -> String {
    format!(
        "CREATE OR REPLACE VIEW analysis_metadata AS
SELECT
    {} AS run_id,
    {} AS view_set,
    {} AS comparison_policy,
    {} AS scheduling_policy;
",
        sql_literal(&context.run_id),
        sql_literal(context.view_set.as_str()),
        sql_literal(&context.comparison_policy),
        sql_literal(&context.scheduling_policy),
    )
}

#[cfg(feature = "duckdb_engine")]
fn build_fact_views_sql(account_id: &str, run_id: Option<&str>) -> String {
    let filter = match run_id {
        Some(run_id) => format!("account_id = {account_id} AND run_id = {run_id}"),
        None => format!("account_id = {account_id}"),
    };
    format!(
        "CREATE OR REPLACE VIEW slot_commit_journal_commits AS
SELECT
    schedule_idx,
    slot_commit_id
FROM account_db.slot_commit_records
WHERE {filter}
  AND record_type = 'commit';

CREATE OR REPLACE VIEW schedule_progress_runtime AS
WITH progress AS (
    SELECT value_json
    FROM account_db.runtime_kv
    WHERE {filter}
      AND key = 'schedule_progress_v2'
)
SELECT
    coalesce(try_cast(json_extract(value_json, '$.next_schedule_index') AS BIGINT), 9223372036854775807) AS next_schedule_index
FROM progress
UNION ALL
SELECT 9223372036854775807 AS next_schedule_index
WHERE NOT EXISTS (SELECT 1 FROM progress);

CREATE OR REPLACE VIEW committed_slot_publications AS
SELECT
    c.schedule_idx,
    c.slot_commit_id
FROM slot_commit_journal_commits c
CROSS JOIN schedule_progress_runtime p
WHERE c.schedule_idx < p.next_schedule_index;

CREATE OR REPLACE VIEW committed_slot_guard AS
SELECT count(*) AS committed_count
FROM committed_slot_publications;

CREATE OR REPLACE VIEW trials AS
WITH raw AS (
    SELECT row_json
    FROM account_db.trial_rows
    WHERE {filter}
),
filtered AS (
    SELECT row_json
    FROM raw
    WHERE (
        (
            json_extract_string(row_json, '$.slot_commit_id') IS NULL
            OR try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) IS NULL
        )
        AND (SELECT committed_count FROM committed_slot_guard) = 0
    )
    OR EXISTS (
        SELECT 1
        FROM committed_slot_publications c
        WHERE c.slot_commit_id = json_extract_string(row_json, '$.slot_commit_id')
          AND c.schedule_idx = try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT)
    )
)
SELECT
    json_extract_string(row_json, '$.run_id') AS run_id,
    json_extract_string(row_json, '$.trial_id') AS trial_id,
    try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) AS schedule_idx,
    json_extract_string(row_json, '$.slot_commit_id') AS slot_commit_id,
    try_cast(json_extract(row_json, '$.attempt') AS BIGINT) AS attempt,
    try_cast(json_extract(row_json, '$.row_seq') AS BIGINT) AS row_seq,
    json_extract_string(row_json, '$.variant_id') AS variant_id,
    json_extract_string(row_json, '$.baseline_id') AS baseline_id,
    json_extract_string(row_json, '$.task_id') AS task_id,
    try_cast(json_extract(row_json, '$.repl_idx') AS BIGINT) AS repl_idx,
    json_extract_string(row_json, '$.outcome') AS outcome,
    json_extract_string(row_json, '$.primary_metric_name') AS primary_metric_name,
    json_extract_string(row_json, '$.primary_metric_value') AS primary_metric_value,
    json_extract(row_json, '$.bindings') AS bindings
FROM filtered;

CREATE OR REPLACE VIEW metrics_long AS
WITH raw AS (
    SELECT account_id, run_id, metric_name, row_json
    FROM account_db.metric_rows
    WHERE {filter}
),
filtered AS (
    SELECT account_id, run_id, metric_name, row_json
    FROM raw
    WHERE (
        (
            json_extract_string(row_json, '$.slot_commit_id') IS NULL
            OR try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) IS NULL
        )
        AND (SELECT committed_count FROM committed_slot_guard) = 0
    )
    OR EXISTS (
        SELECT 1
        FROM committed_slot_publications c
        WHERE c.slot_commit_id = json_extract_string(row_json, '$.slot_commit_id')
          AND c.schedule_idx = try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT)
    )
)
SELECT
    json_extract_string(f.row_json, '$.run_id') AS run_id,
    json_extract_string(f.row_json, '$.trial_id') AS trial_id,
    try_cast(json_extract(f.row_json, '$.schedule_idx') AS BIGINT) AS schedule_idx,
    json_extract_string(f.row_json, '$.slot_commit_id') AS slot_commit_id,
    try_cast(json_extract(f.row_json, '$.attempt') AS BIGINT) AS attempt,
    try_cast(json_extract(f.row_json, '$.row_seq') AS BIGINT) AS row_seq,
    json_extract_string(f.row_json, '$.variant_id') AS variant_id,
    json_extract_string(f.row_json, '$.task_id') AS task_id,
    json_extract_string(f.row_json, '$.metric_name') AS metric_name,
    json_extract_string(f.row_json, '$.metric_value') AS metric_value,
    d.semantic_key,
    d.label AS metric_label,
    d.value_type,
    d.unit,
    d.direction
FROM filtered f
LEFT JOIN account_db.runs r
  ON r.account_id = f.account_id
 AND r.run_id = f.run_id
LEFT JOIN account_db.metric_definitions d
  ON d.account_id = f.account_id
 AND d.experiment_id = r.experiment_id
 AND d.metric_id = f.metric_name;

CREATE OR REPLACE VIEW metric_definitions AS
SELECT
    experiment_id,
    metric_id,
    semantic_key,
    label,
    value_type,
    unit,
    direction,
    source_type,
    source_pointer,
    required,
    primary_metric,
    definition_json
FROM account_db.metric_definitions
WHERE account_id = {account_id};

CREATE OR REPLACE VIEW events AS
WITH raw AS (
    SELECT row_json
    FROM account_db.event_rows
    WHERE {filter}
),
filtered AS (
    SELECT row_json
    FROM raw
    WHERE (
        (
            json_extract_string(row_json, '$.slot_commit_id') IS NULL
            OR try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) IS NULL
        )
        AND (SELECT committed_count FROM committed_slot_guard) = 0
    )
    OR EXISTS (
        SELECT 1
        FROM committed_slot_publications c
        WHERE c.slot_commit_id = json_extract_string(row_json, '$.slot_commit_id')
          AND c.schedule_idx = try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT)
    )
)
SELECT
    json_extract_string(row_json, '$.run_id') AS run_id,
    json_extract_string(row_json, '$.trial_id') AS trial_id,
    try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) AS schedule_idx,
    json_extract_string(row_json, '$.slot_commit_id') AS slot_commit_id,
    try_cast(json_extract(row_json, '$.attempt') AS BIGINT) AS attempt,
    try_cast(json_extract(row_json, '$.row_seq') AS BIGINT) AS row_seq,
    json_extract_string(row_json, '$.variant_id') AS variant_id,
    json_extract_string(row_json, '$.task_id') AS task_id,
    try_cast(json_extract(row_json, '$.repl_idx') AS BIGINT) AS repl_idx,
    try_cast(json_extract(row_json, '$.seq') AS BIGINT) AS seq,
    json_extract_string(row_json, '$.event_type') AS event_type,
    json_extract_string(row_json, '$.ts') AS ts,
    try_cast(
        coalesce(
            json_extract(row_json, '$.step_index'),
            json_extract(row_json, '$.payload.step_index')
        ) AS BIGINT
    ) AS step_index,
    try_cast(
        coalesce(
            json_extract(row_json, '$.turn_index'),
            json_extract(row_json, '$.payload.turn_index')
        ) AS BIGINT
    ) AS turn_index,
    coalesce(
        json_extract_string(row_json, '$.call_id'),
        json_extract_string(row_json, '$.payload.call_id')
    ) AS call_id,
    coalesce(
        json_extract_string(row_json, '$.model.identity'),
        json_extract_string(row_json, '$.payload.model.identity')
    ) AS model_identity,
    coalesce(
        json_extract_string(row_json, '$.tool.name'),
        json_extract_string(row_json, '$.payload.tool.name')
    ) AS tool_name,
    coalesce(
        json_extract_string(row_json, '$.outcome.status'),
        json_extract_string(row_json, '$.payload.outcome.status')
    ) AS outcome_status,
    try_cast(
        coalesce(
            json_extract(row_json, '$.usage.tokens_in'),
            json_extract(row_json, '$.payload.usage.tokens_in')
        ) AS DOUBLE
    ) AS usage_tokens_in,
    try_cast(
        coalesce(
            json_extract(row_json, '$.usage.tokens_out'),
            json_extract(row_json, '$.payload.usage.tokens_out')
        ) AS DOUBLE
    ) AS usage_tokens_out
FROM filtered;

CREATE OR REPLACE VIEW variant_snapshots AS
WITH raw AS (
    SELECT row_json
    FROM account_db.variant_snapshot_rows
    WHERE {filter}
),
filtered AS (
    SELECT row_json
    FROM raw
    WHERE (
        (
            json_extract_string(row_json, '$.slot_commit_id') IS NULL
            OR try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) IS NULL
        )
        AND (SELECT committed_count FROM committed_slot_guard) = 0
    )
    OR EXISTS (
        SELECT 1
        FROM committed_slot_publications c
        WHERE c.slot_commit_id = json_extract_string(row_json, '$.slot_commit_id')
          AND c.schedule_idx = try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT)
    )
)
SELECT
    json_extract_string(row_json, '$.run_id') AS run_id,
    json_extract_string(row_json, '$.trial_id') AS trial_id,
    try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) AS schedule_idx,
    json_extract_string(row_json, '$.slot_commit_id') AS slot_commit_id,
    try_cast(json_extract(row_json, '$.attempt') AS BIGINT) AS attempt,
    try_cast(json_extract(row_json, '$.row_seq') AS BIGINT) AS row_seq,
    json_extract_string(row_json, '$.variant_id') AS variant_id,
    json_extract_string(row_json, '$.task_id') AS task_id,
    try_cast(json_extract(row_json, '$.repl_idx') AS BIGINT) AS repl_idx,
    json_extract_string(row_json, '$.binding_name') AS binding_name,
    json_extract(row_json, '$.binding_value') AS binding_value,
    json_extract_string(row_json, '$.binding_value_text') AS binding_value_text
FROM filtered;

CREATE OR REPLACE VIEW bindings_long AS
SELECT
    run_id,
    trial_id,
    variant_id,
    task_id,
    repl_idx,
    binding_name,
    binding_value,
    binding_value_text
FROM variant_snapshots;

CREATE OR REPLACE VIEW event_counts_by_trial AS
SELECT
    run_id,
    trial_id,
    variant_id,
    event_type,
    count(*) AS count
FROM events
GROUP BY run_id, trial_id, variant_id, event_type
ORDER BY
    run_id,
    variant_id,
    try_cast(regexp_extract(trial_id, '([0-9]+)$', 1) AS BIGINT) NULLS LAST,
    trial_id,
    event_type;

CREATE OR REPLACE VIEW event_counts_by_variant AS
SELECT
    run_id,
    variant_id,
    event_type,
    count(*) AS count
FROM events
GROUP BY run_id, variant_id, event_type
ORDER BY run_id, variant_id, event_type;

CREATE OR REPLACE VIEW variant_summary AS
SELECT
    variant_id,
    count(*)::BIGINT AS n_trials,
    round(avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS success_rate,
    first(primary_metric_name) AS primary_metric_name,
    round(avg(try_cast(primary_metric_value AS DOUBLE)), 4) AS primary_metric_mean
FROM trials
GROUP BY variant_id;

CREATE OR REPLACE VIEW task_variant_matrix AS
SELECT
    task_id,
    variant_id,
    round(avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS pass_rate,
    count(*) AS n_trials
FROM trials
GROUP BY task_id, variant_id
ORDER BY task_id, variant_id;

CREATE OR REPLACE VIEW run_progress AS
SELECT
    run_id,
    count(*) AS completed_trials,
    count(DISTINCT variant_id) AS variants_seen,
    count(DISTINCT task_id) AS tasks_seen,
    round(avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS pass_rate
FROM trials
GROUP BY run_id
ORDER BY run_id;

CREATE OR REPLACE VIEW contract_stages AS
WITH raw AS (
    SELECT row_json
    FROM account_db.contract_stage_rows
    WHERE {filter}
),
filtered AS (
    SELECT row_json
    FROM raw
    WHERE (
        (
            json_extract_string(row_json, '$.slot_commit_id') IS NULL
            OR try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) IS NULL
        )
        AND (SELECT committed_count FROM committed_slot_guard) = 0
    )
    OR EXISTS (
        SELECT 1
        FROM committed_slot_publications c
        WHERE c.slot_commit_id = json_extract_string(row_json, '$.slot_commit_id')
          AND c.schedule_idx = try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT)
    )
)
SELECT
    json_extract_string(row_json, '$.run_id') AS run_id,
    json_extract_string(row_json, '$.trial_id') AS trial_id,
    try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) AS schedule_idx,
    json_extract_string(row_json, '$.slot_commit_id') AS slot_commit_id,
    try_cast(json_extract(row_json, '$.attempt') AS BIGINT) AS attempt,
    try_cast(json_extract(row_json, '$.row_seq') AS BIGINT) AS row_seq,
    json_extract_string(row_json, '$.variant_id') AS variant_id,
    json_extract_string(row_json, '$.task_id') AS task_id,
    try_cast(json_extract(row_json, '$.repl_idx') AS BIGINT) AS repl_idx,
    json_extract_string(row_json, '$.stage') AS stage,
    json_extract_string(row_json, '$.status') AS status,
    json_extract_string(row_json, '$.recorded_at') AS recorded_at,
    json_extract(row_json, '$.detail') AS detail
FROM filtered;

CREATE OR REPLACE VIEW trial_contract_health AS
SELECT
    t.run_id,
    t.trial_id,
    t.schedule_idx,
    t.variant_id,
    t.task_id,
    t.repl_idx,
    t.outcome,
    try_cast(t.primary_metric_value AS DOUBLE) AS score,
    max(CASE WHEN cs.stage = 'grade_mapping' THEN json_extract_string(cs.detail, '$.overall_status') END) AS overall_status,
    max(CASE WHEN cs.stage = 'grade_mapping' THEN json_extract_string(cs.detail, '$.score_trust') END) AS score_trust,
    max(CASE WHEN cs.stage = 'task_mapping' THEN cs.status END) AS task_mapping,
    max(CASE WHEN cs.stage = 'agent_execution' THEN cs.status END) AS agent_execution,
    max(CASE WHEN cs.stage = 'artifact_extraction' THEN cs.status END) AS artifact_extraction,
    max(CASE WHEN cs.stage = 'grader_input_mapping' THEN cs.status END) AS grader_input_mapping,
    max(CASE WHEN cs.stage = 'grader_execution' THEN cs.status END) AS grader_execution,
    max(CASE WHEN cs.stage = 'grade_mapping' THEN cs.status END) AS grade_mapping,
    max(CASE WHEN cs.stage = 'grade_mapping' THEN json_extract_string(cs.detail, '$.score.official_status') END) AS official_status,
    max(CASE WHEN cs.stage = 'grade_mapping' THEN json_extract_string(cs.detail, '$.score.source') END) AS score_source,
    try_cast(max(CASE WHEN cs.stage = 'artifact_extraction' THEN json_extract(cs.detail, '$.workspace_delta.captured_bytes') END) AS DOUBLE) AS patch_captured_bytes,
    try_cast(max(CASE WHEN cs.stage = 'artifact_extraction' THEN json_extract(cs.detail, '$.workspace_delta.scoped_bytes') END) AS DOUBLE) AS patch_scoped_bytes
FROM trials t
LEFT JOIN contract_stages cs
    ON cs.run_id = t.run_id
   AND cs.trial_id = t.trial_id
GROUP BY
    t.run_id,
    t.trial_id,
    t.schedule_idx,
    t.variant_id,
    t.task_id,
    t.repl_idx,
    t.outcome,
    t.primary_metric_value
ORDER BY
    t.run_id,
    t.schedule_idx,
    t.trial_id;

CREATE OR REPLACE VIEW contract_health AS
SELECT
    run_id,
    count(*) AS completed_trials,
    sum(CASE WHEN score_trust = 'trusted' THEN 1 ELSE 0 END) AS trusted_scores,
    sum(CASE WHEN score_trust = 'untrusted' THEN 1 ELSE 0 END) AS untrusted_scores,
    sum(CASE WHEN score_trust IS NULL THEN 1 ELSE 0 END) AS unknown_score_trust,
    sum(CASE WHEN overall_status = 'warning' THEN 1 ELSE 0 END) AS warning_trials,
    sum(CASE WHEN overall_status = 'error' THEN 1 ELSE 0 END) AS error_trials,
    sum(CASE WHEN artifact_extraction IN ('empty', 'empty_scoped') THEN 1 ELSE 0 END) AS empty_predictions,
    sum(CASE WHEN grader_execution = 'error' OR grade_mapping = 'error' THEN 1 ELSE 0 END) AS grader_or_mapping_errors,
    sum(CASE WHEN task_mapping = 'error' OR grader_input_mapping = 'error' THEN 1 ELSE 0 END) AS connector_errors
FROM trial_contract_health
GROUP BY run_id
ORDER BY run_id;
"
    )
}

#[cfg(feature = "duckdb_engine")]
fn query_run_with_duckdb(context: &RunAnalysisContext, sql: &str) -> Result<QueryTable> {
    let conn = Connection::open_in_memory().context("failed to open in-memory DuckDB")?;
    let bundle_sql = load_view_bundle_sql(context.view_set)?;
    let load_sql = build_load_sql(context, bundle_sql.as_deref());
    conn.execute_batch(&load_sql).with_context(|| {
        format!(
            "failed to attach account SQLite database {}",
            context.db_path.display()
        )
    })?;
    execute_select_query(&conn, sql)
}

#[cfg(feature = "duckdb_engine")]
fn load_account_trend_views(conn: &Connection, db_path: &Path, account_id: &str) -> Result<()> {
    let account_literal = sql_literal(account_id);
    let sql = format!(
        "LOAD json;
LOAD sqlite_scanner;
ATTACH {} AS account_db (TYPE sqlite);

CREATE OR REPLACE VIEW all_trials AS
SELECT
    run_id,
    variant_id,
    task_id,
    outcome,
    json_extract_string(row_json, '$.primary_metric_value') AS primary_metric_value
FROM account_db.trial_rows
WHERE account_id = {};

CREATE OR REPLACE VIEW all_runs AS
SELECT
    run_id,
    coalesce(experiment_id, '') AS experiment_id
FROM account_db.runs
WHERE account_id = {};

CREATE OR REPLACE VIEW pass_rate_trend AS
SELECT
    t.run_id,
    t.variant_id,
    round(avg(CASE WHEN t.outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS pass_rate,
    count(*) AS n_trials
FROM all_trials t
GROUP BY t.run_id, t.variant_id
ORDER BY t.run_id, t.variant_id;

CREATE OR REPLACE VIEW task_pass_rate_trend AS
SELECT
    t.run_id,
    t.variant_id,
    t.task_id,
    round(avg(CASE WHEN t.outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS pass_rate,
    count(*) AS n_trials
FROM all_trials t
GROUP BY t.run_id, t.variant_id, t.task_id
ORDER BY t.run_id, t.variant_id, t.task_id;",
        sql_literal_path(db_path),
        account_literal,
        account_literal
    );
    conn.execute_batch(&sql).with_context(|| {
        format!(
            "failed to attach account SQLite database {}",
            db_path.display()
        )
    })
}

#[cfg(feature = "duckdb_engine")]
fn execute_select_query(conn: &Connection, sql: &str) -> Result<QueryTable> {
    let normalized = normalize_sql(sql)?;
    let describe_sql = format!("DESCRIBE SELECT * FROM ({}) AS __q", normalized);
    let mut columns: Vec<String> = Vec::new();
    if let Ok(mut describe_stmt) = conn.prepare(&describe_sql) {
        if let Ok(mut describe_rows) = describe_stmt.query([]) {
            while let Ok(Some(row)) = describe_rows.next() {
                if let Ok(name) = row.get::<_, String>(0) {
                    columns.push(name);
                }
            }
        }
    }

    let row_json_sql = format!(
        "SELECT to_json(__q) AS row_json FROM ({}) AS __q",
        normalized
    );
    let mut stmt = conn
        .prepare(&row_json_sql)
        .with_context(|| format!("failed to prepare query: {}", normalized))?;
    let mut rows = stmt
        .query([])
        .with_context(|| format!("failed to execute query: {}", normalized))?;

    let mut seen_columns: BTreeSet<String> = columns.iter().cloned().collect();
    let mut parsed_rows: Vec<Value> = Vec::new();

    while let Some(row) = rows.next()? {
        let raw: Option<String> = row.get(0)?;
        let parsed = match raw {
            Some(text) => serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text)),
            None => Value::Null,
        };
        if let Some(obj) = parsed.as_object() {
            for key in obj.keys() {
                if seen_columns.insert(key.clone()) {
                    columns.push(key.clone());
                }
            }
        }
        parsed_rows.push(parsed);
    }

    let mut out_rows: Vec<Vec<Value>> = Vec::new();
    for parsed in parsed_rows {
        if let Some(obj) = parsed.as_object() {
            let mut out = Vec::with_capacity(columns.len());
            for column in &columns {
                out.push(obj.get(column).cloned().unwrap_or(Value::Null));
            }
            out_rows.push(out);
        } else if columns.is_empty() {
            out_rows.push(vec![parsed]);
        } else {
            out_rows.push(vec![parsed; columns.len()]);
        }
    }

    Ok(QueryTable {
        columns,
        rows: out_rows,
    })
}

#[cfg(any(feature = "duckdb_engine", test))]
fn validate_read_only_sql(sql: &str) -> Result<String> {
    let normalized = normalize_sql(sql)?;
    let lower = normalized.to_ascii_lowercase();
    let starters = ["select", "with", "show", "describe", "pragma", "explain"];
    if !starters.iter().any(|prefix| lower.starts_with(prefix)) {
        return Err(anyhow!(
            "lab query only supports read-only SQL starting with SELECT/WITH/SHOW/DESCRIBE/PRAGMA/EXPLAIN"
        ));
    }

    let forbidden = [
        "insert", "update", "delete", "drop", "alter", "create", "attach", "detach", "copy",
        "vacuum", "install", "load",
    ];
    for token in lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| !token.is_empty())
    {
        if forbidden.contains(&token) {
            return Err(anyhow!(
                "lab query only supports read-only SQL (found forbidden keyword '{}')",
                token
            ));
        }
    }

    Ok(normalized)
}

#[cfg(any(feature = "duckdb_engine", test))]
fn normalize_sql(sql: &str) -> Result<String> {
    let mut normalized = sql.trim();
    while normalized.ends_with(';') {
        normalized = normalized[..normalized.len() - 1].trim_end();
    }
    if normalized.is_empty() {
        return Err(anyhow!("query cannot be empty"));
    }
    if normalized.contains(';') {
        return Err(anyhow!(
            "multiple SQL statements are not supported in a single query"
        ));
    }
    Ok(normalized.to_string())
}

fn is_safe_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(feature = "duckdb_engine")]
fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(feature = "duckdb_engine")]
fn sql_literal_path(path: &Path) -> String {
    sql_literal(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_ab_test_for_two_variant_paired_interleaved() {
        let design = ExperimentDesign {
            comparison: "paired".to_string(),
            scheduling: "paired_interleaved".to_string(),
            variant_count: 2,
        };
        assert_eq!(view_set_for_design(&design), ViewSet::AbTest);
    }

    #[test]
    fn picks_multi_variant_for_three_variant_paired_interleaved() {
        let design = ExperimentDesign {
            comparison: "paired".to_string(),
            scheduling: "paired_interleaved".to_string(),
            variant_count: 3,
        };
        assert_eq!(view_set_for_design(&design), ViewSet::MultiVariant);
    }

    #[test]
    fn picks_parameter_sweep_for_unpaired_variant_sequential() {
        let design = ExperimentDesign {
            comparison: "unpaired".to_string(),
            scheduling: "variant_sequential".to_string(),
            variant_count: 5,
        };
        assert_eq!(view_set_for_design(&design), ViewSet::ParameterSweep);
    }

    #[test]
    fn picks_regression_when_comparison_is_none() {
        let design = ExperimentDesign {
            comparison: "none".to_string(),
            scheduling: "variant_sequential".to_string(),
            variant_count: 1,
        };
        assert_eq!(view_set_for_design(&design), ViewSet::Regression);
    }

    #[test]
    fn parse_design_uses_policy_fields_and_variant_plan() {
        let resolved = json!({
            "design": {
                "comparison": "paired",
                "policies": {
                    "comparison": "unpaired",
                    "scheduling": "paired_interleaved"
                }
            },
            "baseline": { "variant_id": "base" },
            "variant_plan": [
                { "variant_id": "v1" },
                { "variant_id": "v2" }
            ]
        });
        let parsed = parse_experiment_design(&resolved);
        assert_eq!(parsed.comparison, "unpaired");
        assert_eq!(parsed.scheduling, "paired_interleaved");
        assert_eq!(parsed.variant_count, 3);
    }

    #[test]
    fn query_validation_rejects_writes() {
        let err = validate_read_only_sql("SELECT * FROM trials; DROP TABLE trials")
            .expect_err("should reject multi statement");
        assert!(err.to_string().contains("multiple SQL statements"));

        let err = validate_read_only_sql("DELETE FROM trials").expect_err("should reject delete");
        assert!(err.to_string().contains("read-only"));
    }
}
