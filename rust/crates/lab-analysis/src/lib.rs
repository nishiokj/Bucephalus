use anyhow::{anyhow, Context, Result};
use include_dir::{include_dir, Dir};
use lab_core::sha256_bytes;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub mod stats;
pub mod verdict;

static VIEW_BUNDLES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/views");

const BUCEPHALUS_DB_ENV: &str = "BUCEPHALUS_DB";
const BUCEPHALUS_HOME_ENV: &str = "BUCEPHALUS_HOME";
const BUCEPHALUS_ACCOUNT_ID_ENV: &str = "BUCEPHALUS_ACCOUNT_ID";

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
pub(crate) struct RunAnalysisContext {
    pub(crate) account_id: String,
    pub(crate) run_id: String,
    pub(crate) db_path: PathBuf,
    pub(crate) comparison_policy: String,
    pub(crate) scheduling_policy: String,
    pub(crate) view_set: ViewSet,
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

fn active_account_id() -> String {
    if let Ok(value) = std::env::var(BUCEPHALUS_ACCOUNT_ID_ENV) {
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

fn account_sqlite_path_for_run(_run_dir: &Path) -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os(BUCEPHALUS_DB_ENV) {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err(anyhow!("{} must be an absolute path", BUCEPHALUS_DB_ENV));
        }
        return Ok(path);
    }

    if let Some(raw) = std::env::var_os(BUCEPHALUS_HOME_ENV) {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err(anyhow!("{} must be an absolute path", BUCEPHALUS_HOME_ENV));
        }
        return Ok(path.join("bucephalus.sqlite"));
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; set {}", BUCEPHALUS_HOME_ENV))?;
        return Ok(home
            .join("Library")
            .join("Application Support")
            .join("Bucephalus")
            .join("bucephalus.sqlite"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            return Ok(appdata.join("Bucephalus").join("bucephalus.sqlite"));
        }
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .ok_or_else(|| {
                anyhow!(
                    "APPDATA and USERPROFILE are not set; set {}",
                    BUCEPHALUS_HOME_ENV
                )
            })?;
        return Ok(home
            .join("AppData")
            .join("Roaming")
            .join("Bucephalus")
            .join("bucephalus.sqlite"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
            return Ok(data_home.join("bucephalus").join("bucephalus.sqlite"));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; set {}", BUCEPHALUS_HOME_ENV))?;
        return Ok(home
            .join(".local")
            .join("share")
            .join("bucephalus")
            .join("bucephalus.sqlite"));
    }

    #[allow(unreachable_code)]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; set {}", BUCEPHALUS_HOME_ENV))?;
        Ok(home.join("Bucephalus").join("bucephalus.sqlite"))
    }
}

pub fn list_views(run_dir: &Path) -> Result<Vec<String>> {
    let context = load_run_context(run_dir)?;
    let conn = open_account_db(&context.db_path)?;
    register_views(&conn, &context)?;
    let list_sql = "SELECT name
                    FROM sqlite_temp_master
                    WHERE type = 'view'
                      AND name NOT LIKE 'sqlite_%'
                    ORDER BY name";
    let table = execute_select_query(&conn, list_sql)?;
    let mut out = Vec::new();
    for row in table.rows {
        if let Some(name) = row.first().and_then(Value::as_str) {
            out.push(name.to_string());
        }
    }
    Ok(out)
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

pub fn query_run(run_dir: &Path, sql: &str) -> Result<QueryTable> {
    let normalized = validate_read_only_sql(sql)?;
    let context = load_run_context(run_dir)?;
    let conn = open_account_db(&context.db_path)?;
    register_views(&conn, &context)?;
    execute_select_query(&conn, &normalized)
}

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
    let conn = open_account_db(&db_path)?;
    register_trend_views(&conn, &account_id)?;

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

pub(crate) fn load_run_context(run_dir: &Path) -> Result<RunAnalysisContext> {
    let canonical = run_dir
        .canonicalize()
        .unwrap_or_else(|_| run_dir.to_path_buf());
    let db_path = account_sqlite_path_for_run(&canonical)?;
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
        account_id: active_account_id(),
        run_id: canonical
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("run")
            .to_string(),
        db_path,
        comparison_policy: design.comparison,
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

pub(crate) fn open_account_db(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("failed to open account database {}", db_path.display()))?;
    register_math_functions(&conn)?;
    Ok(conn)
}

/// The analysis views use `sqrt`, `asin`, and `power` (Cohen's h, McNemar chi2,
/// sample stddev). The bundled SQLite is not compiled with math functions, so we
/// register them as deterministic scalar UDFs. NULL in -> NULL out.
fn register_math_functions(conn: &Connection) -> Result<()> {
    use rusqlite::functions::FunctionFlags;
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("sqrt", 1, flags, |ctx| {
        Ok(ctx.get::<Option<f64>>(0)?.map(f64::sqrt))
    })?;
    conn.create_scalar_function("asin", 1, flags, |ctx| {
        Ok(ctx.get::<Option<f64>>(0)?.map(f64::asin))
    })?;
    conn.create_scalar_function("power", 2, flags, |ctx| {
        let base = ctx.get::<Option<f64>>(0)?;
        let exp = ctx.get::<Option<f64>>(1)?;
        Ok(base.zip(exp).map(|(b, e)| b.powf(e)))
    })?;
    Ok(())
}

/// Normalize the analysis view SQL (embedded views + bundle files) for the
/// SQLite dialect the rusqlite engine runs. The run data already lives in
/// SQLite, so this is the only place the view syntax differs:
/// - `CREATE OR REPLACE VIEW` -> `CREATE TEMP VIEW`: views are per-connection and
///   the account DB is opened read-only, so there is never a name clash to replace.
/// - `account_db.<table>` -> `main.<table>`: tables are native to the directly
///   opened file DB (schema `main`); there is no ATTACH and no scanner extension.
///   The explicit `main.` qualifier is required because some views share a name
///   with their source table (e.g. `metric_definitions`), and an unqualified
///   reference would resolve to the temp view itself (a circular reference).
/// - `json_extract_string(x, p)` -> `json_extract(x, p)`: SQLite's `json_extract`
///   returns the unquoted scalar for a single path.
/// - `try_cast(x AS T)` -> `CAST(x AS T)`: SQLite type affinity accepts BIGINT /
///   DOUBLE / VARCHAR. Every `try_cast(...) IS [NOT] NULL` site extracts a JSON
///   field, where a missing key is already SQL NULL and `CAST(NULL ...)` stays NULL.
fn normalize_view_sql_for_sqlite(sql: &str) -> String {
    sql.replace("CREATE OR REPLACE VIEW", "CREATE TEMP VIEW")
        .replace("account_db.", "main.")
        .replace("json_extract_string(", "json_extract(")
        .replace("try_cast(", "CAST(")
        // Every current first(x) site groups on a column that is constant within
        // the group, so min(x) returns the same value on SQLite.
        .replace("first(", "min(")
}

pub(crate) fn register_views(conn: &Connection, context: &RunAnalysisContext) -> Result<()> {
    let account_id = sql_literal(&context.account_id);
    let run_id = sql_literal(&context.run_id);
    let mut sql = build_fact_views_sql(&account_id, Some(run_id.as_str()));
    sql.push_str(&build_metadata_view_sql(context));
    if let Some(bundle) = load_view_bundle_sql(context.view_set)? {
        sql.push('\n');
        sql.push_str(&bundle);
    }
    conn.execute_batch(&normalize_view_sql_for_sqlite(&sql))
        .context("failed to register analysis views")
}

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
    try_cast(json_extract(row_json, '$.success') AS BOOLEAN) AS success,
    json_extract_string(row_json, '$.status_code') AS status_code,
    json_extract_string(row_json, '$.primary_metric_name') AS primary_metric_name,
    json_extract_string(row_json, '$.primary_metric_value') AS primary_metric_value,
    json_extract(row_json, '$.bindings') AS bindings
FROM filtered;

CREATE OR REPLACE VIEW ab_variant_roles AS
WITH baseline AS (
    SELECT min(baseline_id) AS baseline_id FROM trials
),
treatments AS (
    SELECT DISTINCT t.variant_id
    FROM trials t, baseline b
    WHERE t.variant_id <> b.baseline_id
    ORDER BY t.variant_id
)
SELECT
    b.baseline_id,
    (SELECT variant_id FROM treatments LIMIT 1) AS treatment_id,
    (SELECT count(*) FROM treatments) AS treatment_variant_count,
    b.baseline_id AS variant_a_id,
    (SELECT variant_id FROM treatments LIMIT 1) AS variant_b_id
FROM baseline b;

CREATE OR REPLACE VIEW paired_outcomes AS
SELECT
    b.run_id,
    b.task_id,
    b.repl_idx,
    b.trial_id AS baseline_trial_id,
    t.trial_id AS treatment_trial_id,
    b.outcome AS baseline_outcome,
    t.outcome AS treatment_outcome,
    try_cast(b.primary_metric_value AS DOUBLE) AS baseline_metric,
    try_cast(t.primary_metric_value AS DOUBLE) AS treatment_metric,
    try_cast(t.primary_metric_value AS DOUBLE) - try_cast(b.primary_metric_value AS DOUBLE) AS metric_delta,
    CASE
        WHEN b.outcome = t.outcome THEN 'same'
        WHEN b.outcome = 'success' AND t.outcome <> 'success' THEN 'regression'
        WHEN b.outcome <> 'success' AND t.outcome = 'success' THEN 'improvement'
        ELSE 'changed'
    END AS delta_type
FROM trials b
JOIN ab_variant_roles roles ON b.variant_id = roles.baseline_id
JOIN trials t
    ON t.task_id = b.task_id
   AND t.repl_idx = b.repl_idx
   AND t.variant_id = roles.treatment_id;

CREATE OR REPLACE VIEW trial_grader_identity AS
WITH raw AS (
    SELECT row_json
    FROM account_db.trial_conclusion_rows
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
    try_cast(json_extract(row_json, '$.schedule_idx') AS BIGINT) AS schedule_idx,
    try_cast(json_extract(row_json, '$.attempt') AS BIGINT) AS attempt,
    try_cast(json_extract(row_json, '$.row_seq') AS BIGINT) AS row_seq,
    json_extract_string(row_json, '$.grader.name') AS grader_name,
    json_extract_string(row_json, '$.grader.strategy') AS grader_strategy,
    json_extract_string(row_json, '$.grader.digest') AS grader_digest,
    json_extract_string(row_json, '$.grader.version') AS grader_version
FROM filtered;

CREATE OR REPLACE VIEW flaky_tasks AS
SELECT
    task_id,
    count(*) AS n_replications,
    sum(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) AS passes,
    sum(CASE WHEN outcome <> 'success' THEN 1 ELSE 0 END) AS failures,
    round(avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS pass_rate
FROM trials
GROUP BY task_id
HAVING count(DISTINCT outcome) > 1;

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
    try_cast(json_extract(f.row_json, '$.repl_idx') AS BIGINT) AS repl_idx,
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
    SELECT row_json, payload_json
    FROM account_db.event_rows
    WHERE {filter}
),
filtered AS (
    SELECT row_json, payload_json
    FROM raw
    WHERE json_extract_string(row_json, '$.slot_commit_id') = ''
    OR (
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
    ) AS usage_tokens_out,
    json(payload_json) AS payload_json
FROM filtered;

CREATE OR REPLACE VIEW raw_events AS
WITH raw AS (
    SELECT row_json, payload_json
    FROM account_db.event_rows
    WHERE {filter}
),
filtered AS (
    SELECT row_json, payload_json
    FROM raw
    WHERE json_extract_string(row_json, '$.slot_commit_id') = ''
    OR (
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
),
aged AS (
    SELECT
        row_json,
        payload_json,
        CAST(strftime('%s', 'now') AS BIGINT)
            - CAST(strftime('%s', json_extract_string(row_json, '$.ts')) AS BIGINT) AS age_seconds
    FROM filtered
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
    json_extract_string(row_json, '$.event_type') AS event_type,
    CASE
        WHEN age_seconds IS NULL THEN NULL
        WHEN age_seconds < 0 THEN 'just now'
        WHEN age_seconds < 60 THEN age_seconds || 's ago'
        WHEN age_seconds < 3600 THEN
            (age_seconds / 60)
            || (CASE WHEN age_seconds < 120 THEN ' min ago' ELSE ' mins ago' END)
        WHEN age_seconds < 86400 THEN
            (age_seconds / 3600)
            || (CASE WHEN age_seconds < 7200 THEN ' hr ago' ELSE ' hrs ago' END)
        ELSE 'over 1d'
    END AS event_age,
    payload_json AS event_json,
    row_json AS buc_event_row_json
FROM aged
ORDER BY schedule_idx, row_seq, trial_id;

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

CREATE OR REPLACE VIEW token_usage_by_variant AS
SELECT
    run_id,
    variant_id,
    count(DISTINCT trial_id) AS trials_with_events,
    sum(CASE WHEN usage_tokens_in IS NOT NULL OR usage_tokens_out IS NOT NULL THEN 1 ELSE 0 END) AS model_events,
    round(coalesce(sum(usage_tokens_in), 0), 0) AS tokens_in,
    round(coalesce(sum(usage_tokens_out), 0), 0) AS tokens_out,
    round(coalesce(sum(usage_tokens_in), 0) + coalesce(sum(usage_tokens_out), 0), 0) AS total_tokens
FROM events
GROUP BY run_id, variant_id
ORDER BY run_id, variant_id;

CREATE OR REPLACE VIEW tool_usage_by_variant AS
SELECT
    run_id,
    variant_id,
    tool_name,
    count(*) AS calls,
    count(DISTINCT trial_id) AS trials
FROM events
WHERE tool_name IS NOT NULL
  AND tool_name <> ''
GROUP BY run_id, variant_id, tool_name
ORDER BY run_id, variant_id, calls DESC, tool_name;

CREATE OR REPLACE VIEW run_errors AS
SELECT
    run_id,
    variant_id,
    event_type,
    coalesce(outcome_status, '') AS outcome_status,
    count(*) AS count
FROM events
WHERE lower(coalesce(outcome_status, '')) IN ('error', 'failed', 'failure')
   OR lower(event_type) LIKE '%error%'
   OR lower(event_type) LIKE '%fail%'
GROUP BY run_id, variant_id, event_type, coalesce(outcome_status, '')
ORDER BY run_id, variant_id, count DESC, event_type;

CREATE OR REPLACE VIEW variant_summary AS
SELECT
    variant_id,
    CAST(count(*) AS BIGINT) AS n_trials,
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
    sum(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) AS successful_trials,
    sum(CASE WHEN outcome <> 'success' OR outcome IS NULL THEN 1 ELSE 0 END) AS failed_trials,
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

CREATE OR REPLACE VIEW trial_attempt_latest AS
WITH ranked AS (
    SELECT
        run_id,
        trial_id,
        schedule_idx,
        attempt,
        phase,
        paused_from_phase,
        variant_id,
        task_id,
        repl_idx,
        state_json,
        updated_at_ms,
        row_number() OVER (
            PARTITION BY trial_id
            ORDER BY attempt DESC, updated_at_ms DESC
        ) AS rn
    FROM account_db.trial_attempts
    WHERE {filter}
)
SELECT
    run_id,
    trial_id,
    schedule_idx,
    attempt,
    phase,
    paused_from_phase,
    variant_id,
    task_id,
    repl_idx,
    updated_at_ms,
    try_cast(json_extract(state_json, '$.agent_phase.exit_code') AS BIGINT) AS agent_exit_code,
    try_cast(json_extract(state_json, '$.agent_phase.timed_out') AS BOOLEAN) AS agent_timed_out,
    json_extract_string(state_json, '$.agent_phase.result_state') AS agent_result_state,
    json_extract_string(state_json, '$.agent_phase.stdout_path') AS agent_stdout_path,
    json_extract_string(state_json, '$.agent_phase.stderr_path') AS agent_stderr_path,
    json_extract_string(state_json, '$.candidate_artifact.state') AS candidate_artifact_state,
    json_extract_string(state_json, '$.candidate_artifact.source') AS candidate_artifact_source,
    json_extract_string(state_json, '$.candidate_artifact.artifact_type') AS candidate_artifact_type,
    json_extract_string(state_json, '$.task_sandbox.image') AS task_image,
    json_extract_string(state_json, '$.task_sandbox.workdir') AS task_workdir,
    try_cast(json_extract(state_json, '$.grading_phase.exit_code') AS BIGINT) AS grading_exit_code,
    try_cast(json_extract(state_json, '$.grading_phase.timed_out') AS BOOLEAN) AS grading_timed_out,
    json_extract_string(state_json, '$.grading_phase.output_state') AS grading_output_state,
    json_extract_string(state_json, '$.grading_phase.stdout_path') AS grader_stdout_path,
    json_extract_string(state_json, '$.grading_phase.stderr_path') AS grader_stderr_path,
    json_extract_string(state_json, '$.grading_sandbox.strategy') AS grading_strategy,
    json_extract_string(state_json, '$.grading_sandbox.workdir') AS grading_workdir,
    json_array_length(json_extract(state_json, '$.cleanup.containers')) AS cleanup_container_count,
    state_json
FROM ranked
WHERE rn = 1;

CREATE OR REPLACE VIEW trial_event_rollup AS
SELECT
    run_id,
    trial_id,
    count(*) AS event_count,
    sum(CASE WHEN tool_name IS NOT NULL AND tool_name <> '' THEN 1 ELSE 0 END) AS tool_event_count,
    sum(CASE
        WHEN lower(coalesce(outcome_status, '')) IN ('error', 'failed', 'failure')
          OR lower(event_type) LIKE '%error%'
          OR lower(event_type) LIKE '%fail%'
        THEN 1 ELSE 0
    END) AS error_event_count,
    max(ts) AS last_event_ts
FROM events
GROUP BY run_id, trial_id;

CREATE OR REPLACE VIEW trial_diagnostics AS
WITH ids AS (
    SELECT run_id, trial_id FROM trials
    UNION
    SELECT run_id, trial_id FROM trial_attempt_latest
)
SELECT
    ids.run_id,
    ids.trial_id,
    coalesce(t.schedule_idx, a.schedule_idx) AS schedule_idx,
    coalesce(t.variant_id, a.variant_id) AS variant_id,
    coalesce(t.task_id, a.task_id) AS task_id,
    coalesce(t.repl_idx, a.repl_idx) AS repl_idx,
    a.phase,
    t.outcome AS trial_outcome,
    t.success AS trial_success,
    a.agent_exit_code,
    coalesce(a.agent_timed_out, false) AS agent_timed_out,
    a.agent_result_state,
    a.candidate_artifact_state,
    a.candidate_artifact_source,
    coalesce(e.event_count, 0) AS event_count,
    coalesce(e.tool_event_count, 0) AS tool_event_count,
    coalesce(e.error_event_count, 0) AS error_event_count,
    e.last_event_ts,
    h.score_trust,
    h.overall_status,
    h.task_mapping,
    h.agent_execution,
    h.artifact_extraction,
    h.grader_input_mapping,
    h.grader_execution,
    h.grade_mapping,
    a.task_image,
    a.task_workdir,
    a.grading_strategy,
    a.grading_exit_code,
    coalesce(a.grading_timed_out, false) AS grading_timed_out,
    a.agent_stdout_path,
    a.agent_stderr_path,
    a.grader_stdout_path,
    a.grader_stderr_path,
    a.updated_at_ms
FROM ids
LEFT JOIN trials t
    ON t.run_id = ids.run_id
   AND t.trial_id = ids.trial_id
LEFT JOIN trial_attempt_latest a
    ON a.run_id = ids.run_id
   AND a.trial_id = ids.trial_id
LEFT JOIN trial_event_rollup e
    ON e.run_id = ids.run_id
   AND e.trial_id = ids.trial_id
LEFT JOIN trial_contract_health h
    ON h.run_id = ids.run_id
   AND h.trial_id = ids.trial_id
ORDER BY
    coalesce(t.schedule_idx, a.schedule_idx),
    ids.trial_id;

CREATE OR REPLACE VIEW observability_summary AS
SELECT
    run_id,
    count(*) AS trials_seen,
    sum(CASE WHEN trial_outcome IS NOT NULL THEN 1 ELSE 0 END) AS completed_trials,
    sum(CASE WHEN trial_success THEN 1 ELSE 0 END) AS successful_trials,
    sum(CASE WHEN trial_outcome IS NOT NULL AND NOT coalesce(trial_success, false) THEN 1 ELSE 0 END) AS failed_trials,
    round(avg(CASE WHEN trial_success THEN 1.0 ELSE 0.0 END), 4) AS pass_rate,
    sum(CASE WHEN phase IN (
        'pending',
        'agent_materializing',
        'agent_running',
        'agent_finished',
        'grader_materializing',
        'grader_running',
        'grader_mapping',
        'commit_pending',
        'paused'
    ) THEN 1 ELSE 0 END) AS active_trials,
    sum(CASE WHEN phase = 'abandoned' THEN 1 ELSE 0 END) AS abandoned_trials,
    sum(CASE WHEN trial_outcome IS NULL THEN 1 ELSE 0 END) AS missing_trial_rows,
    sum(CASE WHEN agent_timed_out THEN 1 ELSE 0 END) AS agent_timeouts,
    sum(CASE WHEN coalesce(agent_exit_code, 0) <> 0 THEN 1 ELSE 0 END) AS nonzero_agent_exits,
    sum(CASE WHEN agent_result_state = 'missing' THEN 1 ELSE 0 END) AS missing_results,
    sum(CASE WHEN agent_result_state = 'present_invalid' THEN 1 ELSE 0 END) AS invalid_results,
    sum(CASE WHEN candidate_artifact_state = 'missing' THEN 1 ELSE 0 END) AS missing_candidates,
    sum(CASE WHEN candidate_artifact_state = 'invalid' THEN 1 ELSE 0 END) AS invalid_candidates,
    sum(CASE WHEN event_count > 0 THEN 1 ELSE 0 END) AS trials_with_events,
    sum(CASE WHEN tool_event_count > 0 THEN 1 ELSE 0 END) AS trials_with_tool_events,
    sum(CASE WHEN error_event_count > 0 THEN 1 ELSE 0 END) AS trials_with_error_events,
    sum(CASE WHEN grader_execution = 'error' OR grade_mapping = 'error' THEN 1 ELSE 0 END) AS grader_or_mapping_errors,
    sum(CASE WHEN task_mapping = 'error' OR grader_input_mapping = 'error' THEN 1 ELSE 0 END) AS connector_errors,
    sum(CASE WHEN artifact_extraction IN ('empty', 'empty_scoped') THEN 1 ELSE 0 END) AS empty_predictions,
    CASE
        WHEN sum(CASE WHEN trial_outcome IS NOT NULL THEN 1 ELSE 0 END) > 0
         AND sum(CASE WHEN trial_success THEN 1 ELSE 0 END) = 0
        THEN 'systemic_failure_suspected'
        WHEN sum(CASE
            WHEN phase IN (
                'pending',
                'agent_materializing',
                'agent_running',
                'agent_finished',
                'grader_materializing',
                'grader_running',
                'grader_mapping',
                'commit_pending',
                'paused',
                'abandoned'
            )
              OR coalesce(agent_timed_out, false)
              OR coalesce(agent_exit_code, 0) <> 0
              OR agent_result_state IN ('missing', 'present_invalid')
              OR candidate_artifact_state IN ('missing', 'invalid')
              OR trial_outcome IS NULL
              OR grader_execution = 'error'
              OR grade_mapping = 'error'
              OR task_mapping = 'error'
              OR grader_input_mapping = 'error'
              OR artifact_extraction IN ('empty', 'empty_scoped')
              OR trial_outcome <> 'success'
            THEN 1 ELSE 0 END) > 0
        THEN 'needs_investigation'
        ELSE 'no_observed_runtime_gaps'
    END AS diagnostic_verdict
FROM trial_diagnostics
GROUP BY run_id
ORDER BY run_id;
"
    )
}

fn register_trend_views(conn: &Connection, account_id: &str) -> Result<()> {
    let account_literal = sql_literal(account_id);
    let sql = format!(
        "CREATE OR REPLACE VIEW all_trials AS
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
        account_literal, account_literal
    );
    conn.execute_batch(&normalize_view_sql_for_sqlite(&sql))
        .context("failed to register trend views")
}

fn value_ref_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => Value::String(hex::encode(bytes)),
    }
}

fn execute_select_query(conn: &Connection, sql: &str) -> Result<QueryTable> {
    let normalized = normalize_sql(sql)?;
    let mut stmt = conn
        .prepare(&normalized)
        .with_context(|| format!("failed to prepare query: {}", normalized))?;
    let columns: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let column_count = columns.len();
    let mut rows = stmt
        .query([])
        .with_context(|| format!("failed to execute query: {}", normalized))?;

    let mut out_rows: Vec<Vec<Value>> = Vec::new();
    while let Some(row) = rows.next()? {
        let mut out = Vec::with_capacity(column_count);
        for idx in 0..column_count {
            out.push(value_ref_to_json(row.get_ref(idx)?));
        }
        out_rows.push(out);
    }

    Ok(QueryTable {
        columns,
        rows: out_rows,
    })
}

fn validate_read_only_sql(sql: &str) -> Result<String> {
    let normalized = normalize_sql(sql)?;
    let lower = normalized.to_ascii_lowercase();
    let starters = ["select", "with", "show", "describe", "pragma", "explain"];
    if !starters.iter().any(|prefix| lower.starts_with(prefix)) {
        return Err(anyhow!(
            "bucephalus query only supports read-only SQL starting with SELECT/WITH/SHOW/DESCRIBE/PRAGMA/EXPLAIN"
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
                "bucephalus query only supports read-only SQL (found forbidden keyword '{}')",
                token
            ));
        }
    }

    Ok(normalized)
}

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

pub(crate) fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
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
