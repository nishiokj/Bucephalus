#[cfg(test)]
use crate::experiment::state::PendingTrialCompletionRecord;
use crate::experiment::state::ScheduleSlotRecord;
use crate::local_storage;
#[cfg(test)]
use crate::model::TrialExecutionResult;
use crate::model::{TrialSlot, RUNTIME_KEY_RUN_CONTROL, RUNTIME_KEY_SCHEDULE_PROGRESS};
use crate::package::sealed::verify_sealed_package_integrity;
use crate::package::validate::validate_schema_contract_value;
use crate::persistence::rows::{
    ContractStageRow, EventRow, JsonRowTable, MetricRow, TrialRecord, VariantSnapshotRow,
};
use crate::trial::state::{TrialAttemptState, TrialPhase};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use lab_core::sha256_bytes;
use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior,
};
use serde_json::Value;
#[cfg(test)]
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCHEMA_SQL: &str = include_str!("schema_v2.sql");
const MIGRATION_EXPERIMENT_BUNDLES: &str = "20260516_experiment_bundles";
const MIGRATION_TRIAL_ROWS_EVENT_COLUMNS: &str = "20260516_trial_rows_event_columns";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);
const SQLITE_BUSY_RETRY_ATTEMPTS: usize = 80;
pub const BUCEPHALUS_ACCOUNT_ID_ENV: &str = "BUCEPHALUS_ACCOUNT_ID";

#[derive(Debug)]
pub struct TrialRowInsert<'a> {
    pub run_id: &'a str,
    pub trial_id: &'a str,
    pub schedule_idx: usize,
    pub attempt: usize,
    pub row_seq: usize,
    pub slot_commit_id: &'a str,
    pub baseline_id: &'a str,
    pub workload_type: &'a str,
    pub variant_id: &'a str,
    pub task_id: &'a str,
    pub repl_idx: usize,
    pub outcome: &'a str,
    pub primary_metric_name: &'a str,
    pub primary_metric_value: &'a Value,
    pub metrics: &'a Value,
    pub bindings: &'a Value,
    pub events_total: usize,
    pub has_events: bool,
    pub row_json: &'a Value,
}

#[derive(Debug)]
pub struct MetricRowInsert<'a> {
    pub run_id: &'a str,
    pub trial_id: &'a str,
    pub schedule_idx: usize,
    pub attempt: usize,
    pub row_seq: usize,
    pub slot_commit_id: &'a str,
    pub variant_id: &'a str,
    pub task_id: &'a str,
    pub repl_idx: usize,
    pub outcome: &'a str,
    pub metric_name: &'a str,
    pub metric_value: &'a Value,
    pub metric_source: Option<&'a str>,
    pub row_json: &'a Value,
}

#[derive(Debug)]
pub struct MetricDefinitionInsert<'a> {
    pub experiment_id: &'a str,
    pub metric_id: &'a str,
    pub semantic_key: Option<&'a str>,
    pub label: Option<&'a str>,
    pub value_type: Option<&'a str>,
    pub unit: Option<&'a str>,
    pub direction: Option<&'a str>,
    pub source_type: &'a str,
    pub source_pointer: Option<&'a str>,
    pub required: bool,
    pub primary: bool,
    pub definition_json: &'a Value,
}

#[derive(Debug, Clone)]
pub struct ExperimentBundleValidation {
    pub package_digest: String,
    pub experiment_id: Option<String>,
    pub package_dir: PathBuf,
    pub smoke_tested: bool,
    pub smoke_run_id: Option<String>,
    pub smoke_tested_at_ms: Option<i64>,
}

#[derive(Debug)]
pub struct EventRowInsert<'a> {
    pub run_id: &'a str,
    pub trial_id: &'a str,
    pub schedule_idx: usize,
    pub attempt: usize,
    pub row_seq: usize,
    pub slot_commit_id: &'a str,
    pub variant_id: &'a str,
    pub task_id: &'a str,
    pub repl_idx: usize,
    pub seq: usize,
    pub event_type: &'a str,
    pub ts: Option<&'a str>,
    pub payload: &'a Value,
    pub row_json: &'a Value,
}

#[derive(Debug)]
pub struct ContractStageRowInsert<'a> {
    pub run_id: &'a str,
    pub trial_id: &'a str,
    pub schedule_idx: usize,
    pub attempt: usize,
    pub row_seq: usize,
    pub slot_commit_id: &'a str,
    pub variant_id: &'a str,
    pub task_id: &'a str,
    pub repl_idx: usize,
    pub stage: &'a str,
    pub status: &'a str,
    pub recorded_at: &'a str,
    pub detail: &'a Value,
    pub row_json: &'a Value,
}

#[derive(Debug)]
pub struct VariantSnapshotRowInsert<'a> {
    pub run_id: &'a str,
    pub trial_id: &'a str,
    pub schedule_idx: usize,
    pub attempt: usize,
    pub row_seq: usize,
    pub slot_commit_id: &'a str,
    pub variant_id: &'a str,
    pub baseline_id: &'a str,
    pub task_id: &'a str,
    pub repl_idx: usize,
    pub binding_name: &'a str,
    pub binding_value: &'a Value,
    pub binding_value_text: &'a str,
    pub row_json: &'a Value,
}

pub(crate) struct SlotCommitTransactionInput<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) schedule_idx: usize,
    pub(crate) slot: Option<&'a TrialSlot>,
    pub(crate) trial_id: &'a str,
    pub(crate) attempt: usize,
    pub(crate) slot_commit_id: &'a str,
    pub(crate) slot_status: &'a str,
    pub(crate) commit_record: &'a Value,
    pub(crate) schedule_progress: &'a Value,
    pub(crate) trial_rows: &'a [TrialRecord],
    pub(crate) metric_rows: &'a [MetricRow],
    pub(crate) event_rows: &'a [EventRow],
    pub(crate) contract_stage_rows: &'a [ContractStageRow],
    pub(crate) variant_snapshot_rows: &'a [VariantSnapshotRow],
    pub(crate) evidence_rows: &'a [Value],
    pub(crate) chain_state_rows: &'a [Value],
    pub(crate) benchmark_conclusion_rows: &'a [Value],
    #[cfg(test)]
    pub(crate) fail_after_facts: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct TrialAttemptRecord {
    pub(crate) run_id: String,
    pub(crate) trial_id: String,
    pub(crate) schedule_idx: usize,
    pub(crate) attempt: usize,
    pub(crate) phase: TrialPhase,
    pub(crate) paused_from_phase: Option<TrialPhase>,
    pub(crate) state: TrialAttemptState,
}

pub fn account_sqlite_path_for_run(_run_dir: &Path) -> Result<PathBuf> {
    local_storage::account_sqlite_path()
}

pub fn active_account_id() -> String {
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

fn run_id_from_dir(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run")
        .to_string()
}

fn registry_metadata_from_run_dir(run_dir: &Path) -> Result<(Option<String>, Option<String>)> {
    let resolved_path = run_dir.join("resolved_experiment.json");
    let experiment_id = if resolved_path.exists() {
        let raw = fs::read_to_string(&resolved_path)
            .with_context(|| format!("failed to read {}", resolved_path.display()))?;
        let value: Value = serde_json::from_str(&raw)
            .with_context(|| format!("invalid JSON in {}", resolved_path.display()))?;
        value
            .pointer("/experiment/id")
            .or_else(|| value.pointer("/id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    } else {
        None
    };

    let manifest_path = run_dir.join("manifest.json");
    let project_root = if manifest_path.exists() {
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let value: Value = serde_json::from_str(&raw)
            .with_context(|| format!("invalid JSON in {}", manifest_path.display()))?;
        value
            .pointer("/project_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    } else {
        None
    };

    Ok((experiment_id, project_root))
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn json_text(value: &Value) -> Result<String> {
    serde_json::to_string(value).context("serialize json")
}

fn parse_json_text(raw: String) -> Result<Value> {
    serde_json::from_str(&raw).context("parse json")
}

fn as_i64(v: usize) -> i64 {
    v as i64
}

fn bootstrap_sqlite_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)
        .context("bootstrap sqlite schema")?;
    apply_schema_migrations(conn)
}

fn retry_sqlite_busy<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    let mut attempt = 0usize;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(err) if attempt < SQLITE_BUSY_RETRY_ATTEMPTS && is_sqlite_busy_error(&err) => {
                attempt += 1;
                let delay_ms = 10 * attempt.min(10) as u64;
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            Err(err) => return Err(err),
        }
    }
}

pub(crate) fn is_sqlite_busy_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .and_then(sqlite_busy_error_code)
            .is_some()
    })
}

fn sqlite_busy_error_code(err: &rusqlite::Error) -> Option<ErrorCode> {
    match err {
        rusqlite::Error::SqliteFailure(error, _) => match error.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => Some(error.code),
            _ => None,
        },
        _ => None,
    }
}

fn apply_schema_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
           migration_id TEXT PRIMARY KEY,
           applied_at_ms INTEGER NOT NULL
         ) STRICT;",
    )
    .context("bootstrap schema migrations table")?;

    let applied = conn
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE migration_id=?1",
            params![MIGRATION_EXPERIMENT_BUNDLES],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !applied {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS experiment_bundles (
              account_id TEXT NOT NULL,
              package_digest TEXT NOT NULL,
              experiment_id TEXT,
              package_dir TEXT NOT NULL,
              smoke_tested INTEGER NOT NULL CHECK(smoke_tested IN (0,1)),
              smoke_run_id TEXT,
              smoke_tested_at_ms INTEGER,
              validation_json TEXT NOT NULL CHECK(json_valid(validation_json)),
              created_at_ms INTEGER NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (account_id, package_digest)
            ) STRICT;
            CREATE INDEX IF NOT EXISTS idx_experiment_bundles_experiment
              ON experiment_bundles (account_id, experiment_id, updated_at_ms DESC);",
        )
        .context("apply experiment bundle validation migration")?;
        mark_migration_applied(conn, MIGRATION_EXPERIMENT_BUNDLES)?;
    }

    let applied = conn
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE migration_id=?1",
            params![MIGRATION_TRIAL_ROWS_EVENT_COLUMNS],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !applied {
        migrate_trial_rows_event_columns(conn)?;
        mark_migration_applied(conn, MIGRATION_TRIAL_ROWS_EVENT_COLUMNS)?;
    }
    Ok(())
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(columns)
}

fn has_column(columns: &[String], column: &str) -> bool {
    columns.iter().any(|value| value == column)
}

fn select_expr_for_column(columns: &[String], current: &str, legacy: Option<&str>) -> String {
    if has_column(columns, current) {
        current.to_string()
    } else if let Some(legacy) = legacy.filter(|legacy| has_column(columns, legacy)) {
        legacy.to_string()
    } else {
        match current {
            "attempt" => "1".to_string(),
            "row_seq" | "schedule_idx" | "repl_idx" | "events_total" | "has_events" => {
                "0".to_string()
            }
            "primary_metric_value_json" | "metrics_json" | "bindings_json" | "row_json" => {
                "'{}'".to_string()
            }
            _ => "''".to_string(),
        }
    }
}

fn migrate_trial_rows_event_columns(conn: &Connection) -> Result<()> {
    let columns = table_columns(conn, "trial_rows")?;
    let needs_rebuild = has_column(&columns, "hook_events_total")
        || has_column(&columns, "has_hook_events")
        || !has_column(&columns, "events_total")
        || !has_column(&columns, "has_events");
    if !needs_rebuild {
        return Ok(());
    }

    let trial_row_columns = [
        ("account_id", None),
        ("run_id", None),
        ("trial_id", None),
        ("schedule_idx", None),
        ("attempt", None),
        ("row_seq", None),
        ("slot_commit_id", None),
        ("baseline_id", None),
        ("workload_type", None),
        ("variant_id", None),
        ("task_id", None),
        ("repl_idx", None),
        ("outcome", None),
        ("primary_metric_name", None),
        ("primary_metric_value_json", None),
        ("metrics_json", None),
        ("bindings_json", None),
        ("events_total", Some("hook_events_total")),
        ("has_events", Some("has_hook_events")),
        ("row_json", None),
    ];
    let insert_columns = trial_row_columns
        .iter()
        .map(|(column, _)| *column)
        .collect::<Vec<_>>()
        .join(", ");
    let select_columns = trial_row_columns
        .iter()
        .map(|(column, legacy)| select_expr_for_column(&columns, column, *legacy))
        .collect::<Vec<_>>()
        .join(", ");

    let sql = format!(
        "DROP TABLE IF EXISTS trial_rows_migrated;
         CREATE TABLE trial_rows_migrated (
           account_id TEXT NOT NULL,
           run_id TEXT NOT NULL,
           trial_id TEXT NOT NULL,
           schedule_idx INTEGER NOT NULL,
           attempt INTEGER NOT NULL,
           row_seq INTEGER NOT NULL,
           slot_commit_id TEXT NOT NULL,
           baseline_id TEXT NOT NULL,
           workload_type TEXT NOT NULL,
           variant_id TEXT NOT NULL,
           task_id TEXT NOT NULL,
           repl_idx INTEGER NOT NULL,
           outcome TEXT NOT NULL,
           primary_metric_name TEXT NOT NULL,
           primary_metric_value_json TEXT NOT NULL CHECK(json_valid(primary_metric_value_json)),
           metrics_json TEXT NOT NULL CHECK(json_valid(metrics_json)),
           bindings_json TEXT NOT NULL CHECK(json_valid(bindings_json)),
           events_total INTEGER NOT NULL,
           has_events INTEGER NOT NULL CHECK(has_events IN (0,1)),
           row_json TEXT NOT NULL CHECK(json_valid(row_json)),
           PRIMARY KEY (account_id, run_id, trial_id, schedule_idx, attempt, row_seq)
         ) STRICT;
         INSERT OR IGNORE INTO trial_rows_migrated ({insert_columns})
         SELECT {select_columns} FROM trial_rows;
         DROP TABLE trial_rows;
         ALTER TABLE trial_rows_migrated RENAME TO trial_rows;
         CREATE INDEX IF NOT EXISTS idx_trial_rows_variant ON trial_rows (account_id, run_id, variant_id);
         CREATE INDEX IF NOT EXISTS idx_trial_rows_task ON trial_rows (account_id, run_id, task_id);"
    );
    conn.execute_batch(&sql)
        .context("migrate trial_rows event summary columns")?;
    Ok(())
}

fn mark_migration_applied(conn: &Connection, migration_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (migration_id, applied_at_ms) VALUES (?1, ?2)",
        params![migration_id, now_ms()],
    )?;
    Ok(())
}

fn open_account_connection(anchor: &Path) -> Result<Connection> {
    let db_path = account_sqlite_path_for_run(anchor)?;
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "create account sqlite parent directory: {}",
                parent.display()
            )
        })?;
    }
    let conn = Connection::open(&db_path)
        .with_context(|| format!("open sqlite database {}", db_path.display()))?;
    retry_sqlite_busy(|| configure_sqlite_connection(&conn))?;
    retry_sqlite_busy(|| bootstrap_sqlite_schema(&conn))?;
    Ok(conn)
}

fn configure_sqlite_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)
        .context("configure sqlite busy timeout")?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA foreign_keys=ON;
         PRAGMA temp_store=MEMORY;",
    )
    .context("configure sqlite pragmas")?;
    Ok(())
}

fn sealed_package_manifest_path(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if canonical.is_dir() {
        let manifest = canonical.join("manifest.json");
        if !manifest.is_file() {
            return Err(anyhow!(
                "run_input_invalid_kind: expected sealed package dir or manifest"
            ));
        }
        Ok((manifest, canonical))
    } else if canonical
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "manifest.json")
    {
        let package_dir = canonical
            .parent()
            .ok_or_else(|| anyhow!("manifest has no parent directory"))?
            .to_path_buf();
        Ok((canonical, package_dir))
    } else {
        Err(anyhow!(
            "run_input_invalid_kind: expected sealed package dir or manifest"
        ))
    }
}

fn load_experiment_bundle_identity(
    path: &Path,
) -> Result<(PathBuf, String, Option<String>, Value)> {
    let (manifest_path, package_dir) = sealed_package_manifest_path(path)?;
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Value = serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON in {}", manifest_path.display()))?;
    let resolved_experiment = verify_sealed_package_integrity(&package_dir, &manifest)?;
    let package_digest = manifest
        .pointer("/package_digest")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("sealed package manifest missing package_digest"))?
        .to_string();
    let experiment_id = resolved_experiment
        .pointer("/experiment/id")
        .or_else(|| resolved_experiment.pointer("/id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Ok((
        package_dir,
        package_digest,
        experiment_id,
        resolved_experiment,
    ))
}

fn row_to_experiment_bundle_validation(
    package_digest: String,
    experiment_id: Option<String>,
    package_dir: String,
    smoke_tested: i64,
    smoke_run_id: Option<String>,
    smoke_tested_at_ms: Option<i64>,
) -> ExperimentBundleValidation {
    ExperimentBundleValidation {
        package_digest,
        experiment_id,
        package_dir: PathBuf::from(package_dir),
        smoke_tested: smoke_tested != 0,
        smoke_run_id,
        smoke_tested_at_ms,
    }
}

pub fn register_experiment_bundle(package: &Path) -> Result<ExperimentBundleValidation> {
    let (package_dir, package_digest, experiment_id, resolved_experiment) =
        load_experiment_bundle_identity(package)?;
    let account_id = active_account_id();
    let conn = open_account_connection(&package_dir)?;
    let now = now_ms();
    let validation = serde_json::json!({
        "schema_version": "experiment_bundle_validation_v1",
        "package_digest": package_digest,
        "experiment_id": experiment_id,
        "package_dir": package_dir.display().to_string(),
        "resolved_experiment_digest": lab_core::canonical_json_digest(&resolved_experiment),
    });
    conn.execute(
        "INSERT INTO experiment_bundles (
           account_id, package_digest, experiment_id, package_dir, smoke_tested,
           smoke_run_id, smoke_tested_at_ms, validation_json, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, 0, NULL, NULL, ?5, ?6, ?6)
         ON CONFLICT(account_id, package_digest) DO UPDATE SET
           experiment_id=excluded.experiment_id,
           package_dir=excluded.package_dir,
           validation_json=excluded.validation_json,
           updated_at_ms=excluded.updated_at_ms",
        params![
            account_id,
            package_digest,
            experiment_id,
            package_dir.display().to_string(),
            json_text(&validation)?,
            now,
        ],
    )?;
    experiment_bundle_validation(package)
}

pub fn experiment_bundle_validation(package: &Path) -> Result<ExperimentBundleValidation> {
    let (package_dir, package_digest, _, _) = load_experiment_bundle_identity(package)?;
    let account_id = active_account_id();
    let conn = open_account_connection(&package_dir)?;
    let row = conn
        .query_row(
            "SELECT package_digest, experiment_id, package_dir, smoke_tested, smoke_run_id, smoke_tested_at_ms
             FROM experiment_bundles
             WHERE account_id=?1 AND package_digest=?2",
            params![account_id, package_digest],
            |row| {
                Ok(row_to_experiment_bundle_validation(
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    match row {
        Some(row) => Ok(row),
        None => register_experiment_bundle(package),
    }
}

pub fn mark_experiment_bundle_smoke_tested(
    package: &Path,
    smoke_run_id: &str,
) -> Result<ExperimentBundleValidation> {
    let (package_dir, package_digest, experiment_id, resolved_experiment) =
        load_experiment_bundle_identity(package)?;
    let account_id = active_account_id();
    let conn = open_account_connection(&package_dir)?;
    let now = now_ms();
    let validation = serde_json::json!({
        "schema_version": "experiment_bundle_validation_v1",
        "package_digest": package_digest,
        "experiment_id": experiment_id,
        "package_dir": package_dir.display().to_string(),
        "resolved_experiment_digest": lab_core::canonical_json_digest(&resolved_experiment),
        "smoke_run_id": smoke_run_id,
        "smoke_tested_at_ms": now,
    });
    conn.execute(
        "INSERT INTO experiment_bundles (
           account_id, package_digest, experiment_id, package_dir, smoke_tested,
           smoke_run_id, smoke_tested_at_ms, validation_json, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?6, ?6)
         ON CONFLICT(account_id, package_digest) DO UPDATE SET
           experiment_id=excluded.experiment_id,
           package_dir=excluded.package_dir,
           smoke_tested=1,
           smoke_run_id=excluded.smoke_run_id,
           smoke_tested_at_ms=excluded.smoke_tested_at_ms,
           validation_json=excluded.validation_json,
           updated_at_ms=excluded.updated_at_ms",
        params![
            account_id,
            package_digest,
            experiment_id,
            package_dir.display().to_string(),
            smoke_run_id,
            now,
            json_text(&validation)?,
        ],
    )?;
    experiment_bundle_validation(package)
}

fn extract_str<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string field '{}'", pointer))
}

fn extract_usize(value: &Value, pointer: &str) -> Result<usize> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .ok_or_else(|| anyhow!("missing integer field '{}'", pointer))
}

fn extract_str_opt<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

fn trial_phase_text(phase: &TrialPhase) -> Result<String> {
    serde_json::to_value(phase)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("serialize trial phase"))
}

fn put_runtime_json_tx(
    tx: &Transaction<'_>,
    account_id: &str,
    run_id: &str,
    key: &str,
    value: &Value,
) -> Result<()> {
    validate_schema_contract_value(value, format!("runtime_kv key '{}'", key).as_str())?;
    tx.execute(
        "INSERT INTO runtime_kv (account_id, run_id, key, value_json, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(account_id, run_id, key) DO UPDATE SET
           value_json=excluded.value_json,
           updated_at_ms=excluded.updated_at_ms",
        params![account_id, run_id, key, json_text(value)?, now_ms()],
    )?;
    Ok(())
}

fn upsert_slot_commit_record_tx(
    tx: &Transaction<'_>,
    account_id: &str,
    record: &Value,
) -> Result<()> {
    validate_schema_contract_value(record, "slot commit record")?;
    let run_id = extract_str(record, "/run_id")?;
    let schedule_idx = extract_usize(record, "/schedule_idx")?;
    let attempt = extract_usize(record, "/attempt")?;
    let record_type = extract_str(record, "/record_type")?;
    let slot_commit_id = extract_str(record, "/slot_commit_id")?;
    tx.execute(
        "INSERT INTO slot_commit_records
         (account_id, run_id, schedule_idx, attempt, record_type, slot_commit_id, record_json, recorded_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(account_id, run_id, schedule_idx, attempt, record_type) DO UPDATE SET
           slot_commit_id=excluded.slot_commit_id,
           record_json=excluded.record_json,
           recorded_at_ms=excluded.recorded_at_ms",
        params![
            account_id,
            run_id,
            as_i64(schedule_idx),
            as_i64(attempt),
            record_type,
            slot_commit_id,
            json_text(record)?,
            now_ms()
        ],
    )?;
    Ok(())
}

fn upsert_trial_record_tx(tx: &Transaction<'_>, account_id: &str, row: &TrialRecord) -> Result<()> {
    let row_json = serde_json::to_value(row)?;
    tx.execute(
        "INSERT INTO trial_rows (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           baseline_id, workload_type, variant_id, task_id, repl_idx, outcome,
           primary_metric_name, primary_metric_value_json, metrics_json, bindings_json,
           events_total, has_events, row_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7,
           ?8, ?9, ?10, ?11, ?12, ?13,
           ?14, ?15, ?16, ?17,
           ?18, ?19, ?20
         )
         ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
           slot_commit_id=excluded.slot_commit_id,
           baseline_id=excluded.baseline_id,
           workload_type=excluded.workload_type,
           variant_id=excluded.variant_id,
           task_id=excluded.task_id,
           repl_idx=excluded.repl_idx,
           outcome=excluded.outcome,
           primary_metric_name=excluded.primary_metric_name,
           primary_metric_value_json=excluded.primary_metric_value_json,
           metrics_json=excluded.metrics_json,
           bindings_json=excluded.bindings_json,
           events_total=excluded.events_total,
           has_events=excluded.has_events,
           row_json=excluded.row_json",
        params![
            account_id,
            row.run_id,
            row.trial_id,
            as_i64(row.schedule_idx),
            as_i64(row.attempt),
            as_i64(row.row_seq),
            row.slot_commit_id,
            row.baseline_id,
            row.workload_type,
            row.variant_id,
            row.task_id,
            as_i64(row.repl_idx),
            row.outcome,
            row.primary_metric_name,
            json_text(&row.primary_metric_value)?,
            json_text(&row.metrics)?,
            json_text(&row.bindings)?,
            as_i64(row.events_total),
            if row.has_events { 1_i64 } else { 0_i64 },
            json_text(&row_json)?,
        ],
    )?;
    Ok(())
}

fn upsert_metric_record_tx(tx: &Transaction<'_>, account_id: &str, row: &MetricRow) -> Result<()> {
    let row_json = serde_json::to_value(row)?;
    tx.execute(
        "INSERT INTO metric_rows (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           variant_id, task_id, repl_idx, outcome,
           metric_name, metric_value_json, metric_source, row_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7,
           ?8, ?9, ?10, ?11,
           ?12, ?13, ?14, ?15
         )
         ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
           slot_commit_id=excluded.slot_commit_id,
           variant_id=excluded.variant_id,
           task_id=excluded.task_id,
           repl_idx=excluded.repl_idx,
           outcome=excluded.outcome,
           metric_name=excluded.metric_name,
           metric_value_json=excluded.metric_value_json,
           metric_source=excluded.metric_source,
           row_json=excluded.row_json",
        params![
            account_id,
            row.run_id,
            row.trial_id,
            as_i64(row.schedule_idx),
            as_i64(row.attempt),
            as_i64(row.row_seq),
            row.slot_commit_id,
            row.variant_id,
            row.task_id,
            as_i64(row.repl_idx),
            row.outcome,
            row.metric_name,
            json_text(&row.metric_value)?,
            row.metric_source,
            json_text(&row_json)?,
        ],
    )?;
    Ok(())
}

fn upsert_event_record_tx(tx: &Transaction<'_>, account_id: &str, row: &EventRow) -> Result<()> {
    let row_json = serde_json::to_value(row)?;
    tx.execute(
        "INSERT INTO event_rows (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           variant_id, task_id, repl_idx, seq, event_type, ts, payload_json, row_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7,
           ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
         )
         ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
           slot_commit_id=excluded.slot_commit_id,
           variant_id=excluded.variant_id,
           task_id=excluded.task_id,
           repl_idx=excluded.repl_idx,
           seq=excluded.seq,
           event_type=excluded.event_type,
           ts=excluded.ts,
           payload_json=excluded.payload_json,
           row_json=excluded.row_json",
        params![
            account_id,
            row.run_id,
            row.trial_id,
            as_i64(row.schedule_idx),
            as_i64(row.attempt),
            as_i64(row.row_seq),
            row.slot_commit_id,
            row.variant_id,
            row.task_id,
            as_i64(row.repl_idx),
            as_i64(row.seq),
            row.event_type,
            row.ts,
            json_text(&row.payload)?,
            json_text(&row_json)?,
        ],
    )?;
    Ok(())
}

fn upsert_contract_stage_record_tx(
    tx: &Transaction<'_>,
    account_id: &str,
    row: &ContractStageRow,
) -> Result<()> {
    let row_json = serde_json::to_value(row)?;
    tx.execute(
        "INSERT INTO contract_stage_rows (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           variant_id, task_id, repl_idx, stage, status, recorded_at, detail_json, row_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7,
           ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
         )
         ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
           slot_commit_id=excluded.slot_commit_id,
           variant_id=excluded.variant_id,
           task_id=excluded.task_id,
           repl_idx=excluded.repl_idx,
           stage=excluded.stage,
           status=excluded.status,
           recorded_at=excluded.recorded_at,
           detail_json=excluded.detail_json,
           row_json=excluded.row_json",
        params![
            account_id,
            row.run_id,
            row.trial_id,
            as_i64(row.schedule_idx),
            as_i64(row.attempt),
            as_i64(row.row_seq),
            row.slot_commit_id,
            row.variant_id,
            row.task_id,
            as_i64(row.repl_idx),
            row.stage,
            row.status,
            row.recorded_at,
            json_text(&row.detail)?,
            json_text(&row_json)?,
        ],
    )?;
    Ok(())
}

fn upsert_variant_snapshot_record_tx(
    tx: &Transaction<'_>,
    account_id: &str,
    row: &VariantSnapshotRow,
) -> Result<()> {
    let row_json = serde_json::to_value(row)?;
    tx.execute(
        "INSERT INTO variant_snapshot_rows (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           variant_id, baseline_id, task_id, repl_idx, binding_name,
           binding_value_json, binding_value_text, row_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7,
           ?8, ?9, ?10, ?11, ?12,
           ?13, ?14, ?15
         )
         ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
           slot_commit_id=excluded.slot_commit_id,
           variant_id=excluded.variant_id,
           baseline_id=excluded.baseline_id,
           task_id=excluded.task_id,
           repl_idx=excluded.repl_idx,
           binding_name=excluded.binding_name,
           binding_value_json=excluded.binding_value_json,
           binding_value_text=excluded.binding_value_text,
           row_json=excluded.row_json",
        params![
            account_id,
            row.run_id,
            row.trial_id,
            as_i64(row.schedule_idx),
            as_i64(row.attempt),
            as_i64(row.row_seq),
            row.slot_commit_id,
            row.variant_id,
            row.baseline_id,
            row.task_id,
            as_i64(row.repl_idx),
            row.binding_name,
            json_text(&row.binding_value)?,
            row.binding_value_text,
            json_text(&row_json)?,
        ],
    )?;
    Ok(())
}

fn upsert_attempt_object_tx(
    tx: &Transaction<'_>,
    account_id: &str,
    run_id: &str,
    trial_id: &str,
    schedule_idx: usize,
    attempt: usize,
    role: &str,
    object_ref: &str,
    metadata: Option<&Value>,
) -> Result<()> {
    let metadata_json = metadata.map(json_text).transpose()?;
    tx.execute(
        "INSERT INTO attempt_objects (
           account_id, run_id, trial_id, schedule_idx, attempt, role, object_ref, metadata_json, recorded_at_ms
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
         )
         ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, role) DO UPDATE SET
           object_ref=excluded.object_ref,
           metadata_json=excluded.metadata_json,
           recorded_at_ms=excluded.recorded_at_ms",
        params![
            account_id,
            run_id,
            trial_id,
            as_i64(schedule_idx),
            as_i64(attempt),
            role,
            object_ref,
            metadata_json,
            now_ms()
        ],
    )?;
    Ok(())
}

fn upsert_attempt_objects_from_evidence_row_tx(
    tx: &Transaction<'_>,
    account_id: &str,
    row: &Value,
) -> Result<()> {
    let run_id = extract_str_opt(row, "/run_id")
        .or_else(|| extract_str_opt(row, "/ids/run_id"))
        .ok_or_else(|| anyhow!("missing run_id in evidence row"))?;
    let Some(trial_id) = extract_str_opt(row, "/ids/trial_id") else {
        return Ok(());
    };
    let Some(schedule_idx) = extract_usize(row, "/schedule_idx").ok() else {
        return Ok(());
    };
    let Some(attempt) = extract_usize(row, "/attempt").ok() else {
        return Ok(());
    };
    let Some(evidence) = row.pointer("/evidence").and_then(Value::as_object) else {
        return Ok(());
    };

    for role in [
        "trial_input_ref",
        "trial_output_ref",
        "events_ref",
        "stdout_ref",
        "stderr_ref",
        "workspace_pre_ref",
        "workspace_post_ref",
        "diff_incremental_ref",
        "diff_cumulative_ref",
        "patch_incremental_ref",
        "patch_cumulative_ref",
        "workspace_bundle_ref",
    ] {
        let Some(object_ref) = evidence.get(role).and_then(Value::as_str) else {
            continue;
        };
        upsert_attempt_object_tx(
            tx,
            account_id,
            run_id,
            trial_id,
            schedule_idx,
            attempt,
            role.trim_end_matches("_ref"),
            object_ref,
            Some(row),
        )?;
    }
    Ok(())
}

fn upsert_lineage_from_chain_state_row_tx(
    tx: &Transaction<'_>,
    account_id: &str,
    row: &Value,
) -> Result<()> {
    let run_id = extract_str_opt(row, "/run_id")
        .or_else(|| extract_str_opt(row, "/ids/run_id"))
        .ok_or_else(|| anyhow!("missing run_id in chain state row"))?;
    let trial_id = extract_str_opt(row, "/ids/trial_id")
        .ok_or_else(|| anyhow!("missing /ids/trial_id in chain state row"))?;
    let chain_key = extract_str_opt(row, "/chain_id")
        .ok_or_else(|| anyhow!("missing /chain_id in chain state row"))?;
    let step_index = extract_usize(row, "/step_index")?;
    let pre_snapshot_ref = extract_str_opt(row, "/snapshots/prev_ref");
    let post_snapshot_ref = extract_str_opt(row, "/snapshots/post_ref");
    let diff_incremental_ref = extract_str_opt(row, "/diffs/incremental_ref");
    let diff_cumulative_ref = extract_str_opt(row, "/diffs/cumulative_ref");
    let patch_incremental_ref = extract_str_opt(row, "/diffs/patch_incremental_ref");
    let patch_cumulative_ref = extract_str_opt(row, "/diffs/patch_cumulative_ref");
    let workspace_ref = extract_str_opt(row, "/ext/latest_workspace_ref")
        .or_else(|| extract_str_opt(row, "/ext/workspace_ref"));
    let token = format!("{run_id}|{chain_key}|{step_index}|{trial_id}");
    let version_id = sha256_bytes(token.as_bytes());
    let parent_version_id: Option<String> = tx
        .query_row(
            "SELECT latest_version_id
             FROM lineage_heads
             WHERE account_id=?1 AND run_id=?2 AND chain_key=?3",
            params![account_id, run_id, chain_key],
            |db_row| db_row.get(0),
        )
        .optional()?;
    let checkpoint_labels = row
        .pointer("/checkpoint_labels")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));

    tx.execute(
        "INSERT INTO lineage_versions (
           account_id, version_id, run_id, chain_key, step_index, trial_id, parent_version_id,
           pre_snapshot_ref, post_snapshot_ref,
           diff_incremental_ref, diff_cumulative_ref,
           patch_incremental_ref, patch_cumulative_ref,
           workspace_ref, checkpoint_labels_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7,
           ?8, ?9,
           ?10, ?11,
           ?12, ?13,
           ?14, ?15
         )
         ON CONFLICT(version_id) DO UPDATE SET
           parent_version_id=excluded.parent_version_id,
           pre_snapshot_ref=excluded.pre_snapshot_ref,
           post_snapshot_ref=excluded.post_snapshot_ref,
           diff_incremental_ref=excluded.diff_incremental_ref,
           diff_cumulative_ref=excluded.diff_cumulative_ref,
           patch_incremental_ref=excluded.patch_incremental_ref,
           patch_cumulative_ref=excluded.patch_cumulative_ref,
           workspace_ref=excluded.workspace_ref,
           checkpoint_labels_json=excluded.checkpoint_labels_json",
        params![
            account_id,
            version_id,
            run_id,
            chain_key,
            as_i64(step_index),
            trial_id,
            parent_version_id,
            pre_snapshot_ref,
            post_snapshot_ref,
            diff_incremental_ref,
            diff_cumulative_ref,
            patch_incremental_ref,
            patch_cumulative_ref,
            workspace_ref,
            json_text(&checkpoint_labels)?
        ],
    )?;
    tx.execute(
        "INSERT INTO lineage_heads (account_id, run_id, chain_key, latest_version_id, step_index, latest_workspace_ref)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(account_id, run_id, chain_key) DO UPDATE SET
           latest_version_id=excluded.latest_version_id,
           step_index=excluded.step_index,
           latest_workspace_ref=excluded.latest_workspace_ref",
        params![
            account_id,
            run_id,
            chain_key,
            version_id,
            as_i64(step_index),
            workspace_ref
        ],
    )?;
    Ok(())
}

fn upsert_json_row_tx(
    tx: &Transaction<'_>,
    account_id: &str,
    table: JsonRowTable,
    row: &Value,
) -> Result<()> {
    validate_schema_contract_value(row, "durable row")?;
    let run_id = extract_str(row, "/run_id")?;
    let schedule_idx = extract_usize(row, "/schedule_idx")?;
    let attempt = extract_usize(row, "/attempt")?;
    let row_seq = extract_usize(row, "/row_seq")?;
    let slot_commit_id = extract_str(row, "/slot_commit_id")?;
    let (table_name, sql) = match table {
        JsonRowTable::Evidence => (
            "evidence_rows",
            "INSERT INTO evidence_rows
             (account_id, run_id, schedule_idx, attempt, row_seq, slot_commit_id, row_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(account_id, run_id, schedule_idx, attempt, row_seq) DO UPDATE SET
               slot_commit_id=excluded.slot_commit_id,
               row_json=excluded.row_json",
        ),
        JsonRowTable::ChainState => (
            "chain_state_rows",
            "INSERT INTO chain_state_rows
             (account_id, run_id, schedule_idx, attempt, row_seq, slot_commit_id, row_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(account_id, run_id, schedule_idx, attempt, row_seq) DO UPDATE SET
               slot_commit_id=excluded.slot_commit_id,
               row_json=excluded.row_json",
        ),
        JsonRowTable::BenchmarkConclusion => (
            "benchmark_conclusion_rows",
            "INSERT INTO benchmark_conclusion_rows
             (account_id, run_id, schedule_idx, attempt, row_seq, slot_commit_id, row_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(account_id, run_id, schedule_idx, attempt, row_seq) DO UPDATE SET
               slot_commit_id=excluded.slot_commit_id,
               row_json=excluded.row_json",
        ),
    };
    tx.execute(
        sql,
        params![
            account_id,
            run_id,
            as_i64(schedule_idx),
            as_i64(attempt),
            as_i64(row_seq),
            slot_commit_id,
            json_text(row)?
        ],
    )
    .with_context(|| format!("upsert row in {}", table_name))?;
    match table {
        JsonRowTable::Evidence => upsert_attempt_objects_from_evidence_row_tx(tx, account_id, row)?,
        JsonRowTable::ChainState => upsert_lineage_from_chain_state_row_tx(tx, account_id, row)?,
        JsonRowTable::BenchmarkConclusion => {}
    }
    Ok(())
}

fn upsert_trial_attempt_container_tx(
    tx: &Transaction<'_>,
    account_id: &str,
    run_id: &str,
    trial_id: &str,
    state: &TrialAttemptState,
    role: &str,
    container_id: &str,
    image: Option<&str>,
    workdir: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO trial_attempt_containers (
           account_id, run_id, trial_id, schedule_idx, attempt, role,
           container_id, status, image, workdir, updated_at_ms
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6,
           ?7, 'active', ?8, ?9, ?10
         )
         ON CONFLICT(account_id, run_id, trial_id, attempt, role, container_id) DO UPDATE SET
           status=CASE
             WHEN trial_attempt_containers.status IN ('removed','killed') THEN trial_attempt_containers.status
             ELSE excluded.status
           END,
           image=excluded.image,
           workdir=excluded.workdir,
           updated_at_ms=excluded.updated_at_ms",
        params![
            account_id,
            run_id,
            trial_id,
            as_i64(state.key.schedule_idx as usize),
            as_i64(state.key.attempt as usize),
            role,
            container_id,
            image,
            workdir,
            now_ms()
        ],
    )?;
    Ok(())
}

fn parse_trial_attempt_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrialAttemptRecord> {
    let schedule_idx: i64 = row.get(2)?;
    let attempt: i64 = row.get(3)?;
    let phase_raw: String = row.get(4)?;
    let paused_raw: Option<String> = row.get(5)?;
    let state_raw: String = row.get(6)?;
    let state: TrialAttemptState = serde_json::from_str(&state_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(err))
    })?;
    let phase: TrialPhase = serde_json::from_value(Value::String(phase_raw)).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(err))
    })?;
    let paused_from_phase = paused_raw
        .map(|raw| serde_json::from_value(Value::String(raw)))
        .transpose()
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(err))
        })?;
    Ok(TrialAttemptRecord {
        run_id: row.get(0)?,
        trial_id: row.get(1)?,
        schedule_idx: schedule_idx as usize,
        attempt: attempt as usize,
        phase,
        paused_from_phase,
        state,
    })
}

pub struct SqliteRunStore {
    conn: Connection,
    account_id: String,
    run_id: String,
    db_path: PathBuf,
}

impl SqliteRunStore {
    pub fn open(run_dir: &Path) -> Result<Self> {
        if !run_dir.exists() {
            std::fs::create_dir_all(run_dir).with_context(|| {
                format!(
                    "create run directory for sqlite store: {}",
                    run_dir.display()
                )
            })?;
        }
        let db_path = account_sqlite_path_for_run(run_dir)?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "create account sqlite parent directory: {}",
                    parent.display()
                )
            })?;
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open sqlite database {}", db_path.display()))?;
        retry_sqlite_busy(|| configure_sqlite_connection(&conn))?;
        retry_sqlite_busy(|| bootstrap_sqlite_schema(&conn))?;
        let mut store = Self {
            conn,
            account_id: active_account_id(),
            run_id: run_id_from_dir(run_dir),
            db_path,
        };
        retry_sqlite_busy(|| store.ensure_account_profile())?;
        retry_sqlite_busy(|| store.register_run_location(run_dir))?;
        Ok(store)
    }

    fn ensure_account_profile(&mut self) -> Result<()> {
        let profile = serde_json::json!({
            "schema_version": "account_profile_v1",
            "account_id": self.account_id,
        });
        self.conn.execute(
            "INSERT INTO account_profile (account_id, profile_json, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(account_id) DO UPDATE SET
               profile_json=excluded.profile_json,
               updated_at_ms=excluded.updated_at_ms",
            params![self.account_id, json_text(&profile)?, now_ms()],
        )?;
        Ok(())
    }

    fn register_run_location(&mut self, run_dir: &Path) -> Result<()> {
        let (experiment_id, project_root) = registry_metadata_from_run_dir(run_dir)?;
        let run_dir_text = run_dir.display().to_string();
        let artifact_root = run_dir.join("artifacts").display().to_string();
        let manifest = serde_json::json!({
            "schema_version": "run_registry_v1",
            "account_id": self.account_id,
            "run_id": self.run_id,
            "experiment_id": experiment_id.clone(),
            "project_root": project_root.clone(),
            "run_dir": run_dir_text,
            "account_db_path": self.db_path.display().to_string(),
        });
        self.conn.execute(
            "INSERT INTO runs (
               account_id, run_id, experiment_id, project_root, run_dir, artifact_root,
               status, created_at_ms, updated_at_ms, manifest_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'registered', ?7, ?7, ?8)
             ON CONFLICT(account_id, run_id) DO UPDATE SET
               experiment_id=excluded.experiment_id,
               project_root=excluded.project_root,
               run_dir=excluded.run_dir,
               artifact_root=excluded.artifact_root,
               updated_at_ms=excluded.updated_at_ms",
            params![
                self.account_id,
                self.run_id,
                experiment_id,
                project_root,
                run_dir_text,
                artifact_root,
                now_ms(),
                json_text(&manifest)?
            ],
        )?;
        Ok(())
    }

    pub fn put_runtime_json(&mut self, key: &str, value: &Value) -> Result<()> {
        validate_schema_contract_value(value, format!("runtime_kv key '{}'", key).as_str())?;
        let payload = json_text(value)?;
        self.conn.execute(
            "INSERT INTO runtime_kv (account_id, run_id, key, value_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(account_id, run_id, key) DO UPDATE SET
               value_json=excluded.value_json,
               updated_at_ms=excluded.updated_at_ms",
            params![self.account_id, self.run_id, key, payload, now_ms()],
        )?;
        if key == RUNTIME_KEY_RUN_CONTROL {
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            self.conn.execute(
                "UPDATE runs
                 SET status=?1, updated_at_ms=?2
                 WHERE account_id=?3 AND run_id=?4",
                params![status, now_ms(), self.account_id, self.run_id],
            )?;
        }
        Ok(())
    }

    pub fn get_runtime_json(&self, key: &str) -> Result<Option<Value>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM runtime_kv
                 WHERE account_id=?1 AND run_id=?2 AND key=?3",
                params![self.account_id, self.run_id, key],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(parse_json_text).transpose()
    }

    pub(crate) fn ensure_schedule_slots(
        &mut self,
        run_id: &str,
        schedule: &[TrialSlot],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        for (schedule_idx, slot) in schedule.iter().enumerate() {
            tx.execute(
                "INSERT INTO schedule_slots (
                   account_id, run_id, schedule_idx, state, slot_json, trial_id,
                   attempt, worker_id, owner_id, lease_epoch, lease_expires_at,
                   slot_commit_id, slot_status, updated_at_ms
                 ) VALUES (
                   ?1, ?2, ?3, 'pending', ?4, NULL,
                   0, NULL, NULL, 0, NULL,
                   NULL, NULL, ?5
                 )
                 ON CONFLICT(account_id, run_id, schedule_idx) DO UPDATE SET
                   slot_json=excluded.slot_json",
                params![
                    self.account_id,
                    run_id,
                    as_i64(schedule_idx),
                    json_text(&serde_json::to_value(slot)?)?,
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn active_schedule_slots(&self, run_id: &str) -> Result<Vec<ScheduleSlotRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT schedule_idx, state, slot_json, trial_id, attempt, worker_id,
                    owner_id, lease_epoch, lease_expires_at, slot_commit_id, slot_status
             FROM schedule_slots
             WHERE account_id=?1 AND run_id=?2 AND state='active'
             ORDER BY schedule_idx",
        )?;
        let rows = stmt.query_map(params![self.account_id, run_id], |row| {
            let slot_json: String = row.get(2)?;
            let slot: TrialSlot = serde_json::from_str(&slot_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?;
            let schedule_idx: i64 = row.get(0)?;
            let attempt: i64 = row.get(4)?;
            let lease_epoch: i64 = row.get(7)?;
            Ok(ScheduleSlotRecord {
                schedule_idx: schedule_idx as usize,
                state: row.get(1)?,
                slot,
                trial_id: row.get(3)?,
                attempt: attempt as usize,
                worker_id: row.get(5)?,
                owner_id: row.get(6)?,
                lease_epoch: lease_epoch as u64,
                lease_expires_at: row.get(8)?,
                slot_commit_id: row.get(9)?,
                slot_status: row.get(10)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn schedule_slot(
        &self,
        run_id: &str,
        schedule_idx: usize,
    ) -> Result<Option<ScheduleSlotRecord>> {
        self.load_schedule_slot_by_sql(
            "SELECT schedule_idx, state, slot_json, trial_id, attempt, worker_id,
                    owner_id, lease_epoch, lease_expires_at, slot_commit_id, slot_status
             FROM schedule_slots
             WHERE account_id=?1 AND run_id=?2 AND schedule_idx=?3",
            params![self.account_id, run_id, as_i64(schedule_idx)],
        )
    }

    fn load_schedule_slot_by_sql<P>(
        &self,
        sql: &str,
        params: P,
    ) -> Result<Option<ScheduleSlotRecord>>
    where
        P: rusqlite::Params,
    {
        self.conn
            .query_row(sql, params, |row| {
                let slot_json: String = row.get(2)?;
                let slot: TrialSlot = serde_json::from_str(&slot_json).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(err),
                    )
                })?;
                let schedule_idx: i64 = row.get(0)?;
                let attempt: i64 = row.get(4)?;
                let lease_epoch: i64 = row.get(7)?;
                Ok(ScheduleSlotRecord {
                    schedule_idx: schedule_idx as usize,
                    state: row.get(1)?,
                    slot,
                    trial_id: row.get(3)?,
                    attempt: attempt as usize,
                    worker_id: row.get(5)?,
                    owner_id: row.get(6)?,
                    lease_epoch: lease_epoch as u64,
                    lease_expires_at: row.get(8)?,
                    slot_commit_id: row.get(9)?,
                    slot_status: row.get(10)?,
                })
            })
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn claim_schedule_slot(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
        trial_id: &str,
        worker_id: &str,
        owner_id: &str,
        lease_expires_at: Option<&str>,
    ) -> Result<Option<ScheduleSlotRecord>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT schedule_idx, state, slot_json, trial_id, attempt, worker_id,
                        owner_id, lease_epoch, lease_expires_at, slot_commit_id, slot_status
                 FROM schedule_slots
                 WHERE account_id=?1 AND run_id=?2 AND schedule_idx=?3",
                params![self.account_id, run_id, as_i64(schedule_idx)],
                |row| {
                    let slot_json: String = row.get(2)?;
                    let slot: TrialSlot = serde_json::from_str(&slot_json).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(err),
                        )
                    })?;
                    let schedule_idx_raw: i64 = row.get(0)?;
                    let attempt: i64 = row.get(4)?;
                    let lease_epoch: i64 = row.get(7)?;
                    Ok(ScheduleSlotRecord {
                        schedule_idx: schedule_idx_raw as usize,
                        state: row.get(1)?,
                        slot,
                        trial_id: row.get(3)?,
                        attempt: attempt as usize,
                        worker_id: row.get(5)?,
                        owner_id: row.get(6)?,
                        lease_epoch: lease_epoch as u64,
                        lease_expires_at: row.get(8)?,
                        slot_commit_id: row.get(9)?,
                        slot_status: row.get(10)?,
                    })
                },
            )
            .optional()?;
        let Some(existing) = existing else {
            tx.commit()?;
            return Ok(None);
        };
        if existing.state != "pending" {
            tx.commit()?;
            return Ok(None);
        }
        let next_attempt = existing.attempt.saturating_add(1);
        let next_epoch = existing.lease_epoch.saturating_add(1);
        let claimed = tx.execute(
            "UPDATE schedule_slots
             SET state='active',
                 trial_id=?1,
                 attempt=?2,
                 worker_id=?3,
                 owner_id=?4,
                 lease_epoch=?5,
                 lease_expires_at=?6,
                 slot_commit_id=NULL,
                 slot_status=NULL,
                 updated_at_ms=?7
             WHERE account_id=?8 AND run_id=?9 AND schedule_idx=?10 AND state='pending'",
            params![
                trial_id,
                as_i64(next_attempt),
                worker_id,
                owner_id,
                next_epoch as i64,
                lease_expires_at,
                now_ms(),
                self.account_id,
                run_id,
                as_i64(schedule_idx)
            ],
        )?;
        if claimed != 1 {
            tx.commit()?;
            return Ok(None);
        }
        tx.commit()?;
        Ok(Some(ScheduleSlotRecord {
            state: "active".to_string(),
            trial_id: Some(trial_id.to_string()),
            attempt: next_attempt,
            worker_id: Some(worker_id.to_string()),
            owner_id: Some(owner_id.to_string()),
            lease_epoch: next_epoch,
            lease_expires_at: lease_expires_at.map(str::to_string),
            slot_commit_id: None,
            slot_status: None,
            ..existing
        }))
    }

    pub(crate) fn mark_schedule_slot_committed(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
        trial_id: &str,
        attempt: usize,
        slot_commit_id: &str,
        slot_status: &str,
    ) -> Result<()> {
        if let Some(existing) = self.schedule_slot(run_id, schedule_idx)? {
            if existing.state == "active" && existing.trial_id.as_deref() != Some(trial_id) {
                return Err(anyhow!(
                    "schedule_slot_owner_mismatch: schedule_idx {} is active for {:?}, refusing commit from {}",
                    schedule_idx,
                    existing.trial_id,
                    trial_id
                ));
            }
        }
        self.conn.execute(
            "UPDATE schedule_slots
             SET state='committed',
                 trial_id=?1,
                 attempt=MAX(attempt, ?2),
                 worker_id=NULL,
                 owner_id=NULL,
                 lease_expires_at=NULL,
                 slot_commit_id=?3,
                 slot_status=?4,
                 updated_at_ms=?5
             WHERE account_id=?6 AND run_id=?7 AND schedule_idx=?8
               AND state IN ('pending','active','committed','abandoned')",
            params![
                trial_id,
                as_i64(attempt),
                slot_commit_id,
                slot_status,
                now_ms(),
                self.account_id,
                run_id,
                as_i64(schedule_idx)
            ],
        )?;
        Ok(())
    }

    pub(crate) fn release_schedule_slot_to_pending(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE schedule_slots
             SET state='pending',
                 trial_id=NULL,
                 worker_id=NULL,
                 owner_id=NULL,
                 lease_expires_at=NULL,
                 slot_commit_id=NULL,
                 slot_status=NULL,
                 updated_at_ms=?1
             WHERE account_id=?2 AND run_id=?3 AND schedule_idx=?4
               AND state!='committed'",
            params![now_ms(), self.account_id, run_id, as_i64(schedule_idx)],
        )?;
        Ok(())
    }

    pub(crate) fn commit_schedule_slot_transaction(
        &mut self,
        input: SlotCommitTransactionInput<'_>,
    ) -> Result<()> {
        validate_schema_contract_value(input.commit_record, "slot commit record")?;
        validate_schema_contract_value(input.schedule_progress, RUNTIME_KEY_SCHEDULE_PROGRESS)?;
        for row in input.evidence_rows {
            validate_schema_contract_value(row, "evidence row")?;
        }
        for row in input.chain_state_rows {
            validate_schema_contract_value(row, "chain state row")?;
        }
        for row in input.benchmark_conclusion_rows {
            validate_schema_contract_value(row, "benchmark conclusion row")?;
        }

        let account_id = self.account_id.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        if let Some(slot) = input.slot {
            tx.execute(
                "INSERT INTO schedule_slots (
                   account_id, run_id, schedule_idx, state, slot_json, trial_id,
                   attempt, worker_id, owner_id, lease_epoch, lease_expires_at,
                   slot_commit_id, slot_status, updated_at_ms
                 ) VALUES (
                   ?1, ?2, ?3, 'pending', ?4, NULL,
                   0, NULL, NULL, 0, NULL,
                   NULL, NULL, ?5
                 )
                 ON CONFLICT(account_id, run_id, schedule_idx) DO NOTHING",
                params![
                    account_id,
                    input.run_id,
                    as_i64(input.schedule_idx),
                    json_text(&serde_json::to_value(slot)?)?,
                    now
                ],
            )?;
        }

        let existing: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT state, trial_id, slot_commit_id
                 FROM schedule_slots
                 WHERE account_id=?1 AND run_id=?2 AND schedule_idx=?3",
                params![account_id, input.run_id, as_i64(input.schedule_idx)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((state, active_trial_id, existing_commit_id)) = existing else {
            return Err(anyhow!(
                "schedule_slot_missing: schedule_idx {} has no authoritative slot row",
                input.schedule_idx
            ));
        };
        if state == "active" && active_trial_id.as_deref() != Some(input.trial_id) {
            return Err(anyhow!(
                "schedule_slot_owner_mismatch: schedule_idx {} is active for {:?}, refusing commit from {}",
                input.schedule_idx,
                active_trial_id,
                input.trial_id
            ));
        }
        if state == "committed" && existing_commit_id.as_deref() != Some(input.slot_commit_id) {
            return Err(anyhow!(
                "schedule_slot_already_committed: schedule_idx {} is committed as {:?}, refusing commit {}",
                input.schedule_idx,
                existing_commit_id,
                input.slot_commit_id
            ));
        }

        for row in input.trial_rows {
            upsert_trial_record_tx(&tx, &account_id, row)?;
        }
        for row in input.metric_rows {
            upsert_metric_record_tx(&tx, &account_id, row)?;
        }
        for row in input.event_rows {
            upsert_event_record_tx(&tx, &account_id, row)?;
        }
        for row in input.contract_stage_rows {
            upsert_contract_stage_record_tx(&tx, &account_id, row)?;
        }
        for row in input.variant_snapshot_rows {
            upsert_variant_snapshot_record_tx(&tx, &account_id, row)?;
        }
        for row in input.evidence_rows {
            upsert_json_row_tx(&tx, &account_id, JsonRowTable::Evidence, row)?;
        }
        for row in input.chain_state_rows {
            upsert_json_row_tx(&tx, &account_id, JsonRowTable::ChainState, row)?;
        }
        for row in input.benchmark_conclusion_rows {
            upsert_json_row_tx(&tx, &account_id, JsonRowTable::BenchmarkConclusion, row)?;
        }

        #[cfg(test)]
        if input.fail_after_facts {
            return Err(anyhow!("slot_commit_transaction_failpoint_after_facts"));
        }

        upsert_slot_commit_record_tx(&tx, &account_id, input.commit_record)?;
        put_runtime_json_tx(
            &tx,
            &account_id,
            input.run_id,
            RUNTIME_KEY_SCHEDULE_PROGRESS,
            input.schedule_progress,
        )?;
        tx.execute(
            "DELETE FROM pending_trial_completions
             WHERE account_id=?1 AND run_id=?2 AND schedule_idx=?3",
            params![account_id, input.run_id, as_i64(input.schedule_idx)],
        )?;
        let updated = tx.execute(
            "UPDATE schedule_slots
             SET state='committed',
                 trial_id=?1,
                 attempt=MAX(attempt, ?2),
                 worker_id=NULL,
                 owner_id=NULL,
                 lease_expires_at=NULL,
                 slot_commit_id=?3,
                 slot_status=?4,
                 updated_at_ms=?5
             WHERE account_id=?6 AND run_id=?7 AND schedule_idx=?8
               AND state IN ('pending','active','committed')",
            params![
                input.trial_id,
                as_i64(input.attempt),
                input.slot_commit_id,
                input.slot_status,
                now_ms(),
                account_id,
                input.run_id,
                as_i64(input.schedule_idx)
            ],
        )?;
        if updated != 1 {
            return Err(anyhow!(
                "schedule_slot_commit_failed: schedule_idx {} was not committed",
                input.schedule_idx
            ));
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn upsert_trial_attempt_state(
        &mut self,
        run_id: &str,
        trial_id: &str,
        state: &TrialAttemptState,
    ) -> Result<()> {
        let state_json = serde_json::to_value(state)?;
        let phase = trial_phase_text(&state.phase)?;
        let paused_from_phase = state
            .paused_from_phase
            .as_ref()
            .map(trial_phase_text)
            .transpose()?;
        let state_json_text = json_text(&state_json)?;
        let account_id = self.account_id.clone();
        retry_sqlite_busy(|| {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO trial_attempts (
                   account_id, run_id, trial_id, schedule_idx, attempt, phase, paused_from_phase,
                   variant_id, task_id, repl_idx, state_json, updated_at_ms
                 ) VALUES (
                   ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                   ?8, ?9, ?10, ?11, ?12
                 )
                 ON CONFLICT(account_id, run_id, trial_id, attempt) DO UPDATE SET
                   schedule_idx=excluded.schedule_idx,
                   phase=excluded.phase,
                   paused_from_phase=excluded.paused_from_phase,
                   variant_id=excluded.variant_id,
                   task_id=excluded.task_id,
                   repl_idx=excluded.repl_idx,
                   state_json=excluded.state_json,
                   updated_at_ms=excluded.updated_at_ms",
                params![
                    account_id.as_str(),
                    run_id,
                    trial_id,
                    as_i64(state.key.schedule_idx as usize),
                    as_i64(state.key.attempt as usize),
                    phase.as_str(),
                    paused_from_phase.as_deref(),
                    state.slot.variant_id.as_str(),
                    state.slot.task_id.as_str(),
                    as_i64(state.slot.repl_idx as usize),
                    state_json_text.as_str(),
                    now_ms()
                ],
            )?;

            if let Some(task) = state.task_sandbox.as_ref() {
                upsert_trial_attempt_container_tx(
                    &tx,
                    &account_id,
                    run_id,
                    trial_id,
                    state,
                    "task",
                    &task.container_id,
                    Some(task.image.as_str()),
                    Some(task.workdir.as_str()),
                )?;
            }
            if let Some(grading) = state.grading_sandbox.as_ref() {
                upsert_trial_attempt_container_tx(
                    &tx,
                    &account_id,
                    run_id,
                    trial_id,
                    state,
                    "grading",
                    &grading.container_id,
                    None,
                    Some(grading.workdir.as_str()),
                )?;
            }
            for cleanup in &state.cleanup.containers {
                tx.execute(
                    "UPDATE trial_attempt_containers
                     SET status=?1, updated_at_ms=?2
                     WHERE account_id=?3 AND run_id=?4 AND trial_id=?5 AND attempt=?6
                       AND role=?7 AND container_id=?8",
                    params![
                        cleanup.status.as_str(),
                        now_ms(),
                        account_id.as_str(),
                        run_id,
                        trial_id,
                        as_i64(state.key.attempt as usize),
                        cleanup.role.as_str(),
                        cleanup.container_id.as_str()
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn load_latest_trial_attempt(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<TrialAttemptRecord>> {
        self.conn
            .query_row(
                "SELECT run_id, trial_id, schedule_idx, attempt, phase, paused_from_phase, state_json
                 FROM trial_attempts
                 WHERE account_id=?1 AND run_id=?2 AND trial_id=?3
                 ORDER BY attempt DESC
                 LIMIT 1",
                params![self.account_id, run_id, trial_id],
                |row| parse_trial_attempt_record(row),
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn trial_attempt_container_ids(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT container_id
             FROM trial_attempt_containers
             WHERE account_id=?1 AND run_id=?2 AND trial_id=?3
               AND status NOT IN ('removed','killed')
               AND container_id!='host'
             ORDER BY attempt DESC, role",
        )?;
        let rows = stmt.query_map(params![self.account_id, run_id, trial_id], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            let id = row?;
            if !out.iter().any(|existing| existing == &id) {
                out.push(id);
            }
        }
        Ok(out)
    }

    pub(crate) fn trial_attempts_for_recovery(
        &self,
        run_id: &str,
    ) -> Result<Vec<TrialAttemptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, trial_id, schedule_idx, attempt, phase, paused_from_phase, state_json
             FROM trial_attempts
             WHERE account_id=?1 AND run_id=?2
             ORDER BY schedule_idx, attempt",
        )?;
        let rows = stmt.query_map(params![self.account_id, run_id], parse_trial_attempt_record)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn put_run_manifest(&mut self, run_id: &str, manifest: &Value) -> Result<()> {
        validate_schema_contract_value(
            manifest,
            format!("run_manifest row for run '{}'", run_id).as_str(),
        )?;
        let payload = json_text(manifest)?;
        self.conn.execute(
            "INSERT INTO run_manifests (account_id, run_id, manifest_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, run_id) DO UPDATE SET
               manifest_json=excluded.manifest_json,
               updated_at_ms=excluded.updated_at_ms",
            params![self.account_id, run_id, payload, now_ms()],
        )?;
        Ok(())
    }

    pub fn upsert_metric_definition(&mut self, row: MetricDefinitionInsert<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO metric_definitions (
               account_id, experiment_id, metric_id, semantic_key, label, value_type,
               unit, direction, source_type, source_pointer, required, primary_metric,
               definition_json, updated_at_ms
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6,
               ?7, ?8, ?9, ?10, ?11, ?12,
               ?13, ?14
             )
             ON CONFLICT(account_id, experiment_id, metric_id) DO UPDATE SET
               semantic_key=excluded.semantic_key,
               label=excluded.label,
               value_type=excluded.value_type,
               unit=excluded.unit,
               direction=excluded.direction,
               source_type=excluded.source_type,
               source_pointer=excluded.source_pointer,
               required=excluded.required,
               primary_metric=excluded.primary_metric,
               definition_json=excluded.definition_json,
               updated_at_ms=excluded.updated_at_ms",
            params![
                self.account_id,
                row.experiment_id,
                row.metric_id,
                row.semantic_key,
                row.label,
                row.value_type,
                row.unit,
                row.direction,
                row.source_type,
                row.source_pointer,
                if row.required { 1_i64 } else { 0_i64 },
                if row.primary { 1_i64 } else { 0_i64 },
                json_text(row.definition_json)?,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    pub fn upsert_slot_commit_record(&mut self, record: &Value) -> Result<()> {
        let run_id = extract_str(record, "/run_id")?;
        let schedule_idx = extract_usize(record, "/schedule_idx")?;
        let attempt = extract_usize(record, "/attempt")?;
        let record_type = extract_str(record, "/record_type")?;
        let slot_commit_id = extract_str(record, "/slot_commit_id")?;
        let payload = json_text(record)?;
        self.conn.execute(
            "INSERT INTO slot_commit_records
             (account_id, run_id, schedule_idx, attempt, record_type, slot_commit_id, record_json, recorded_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(account_id, run_id, schedule_idx, attempt, record_type) DO UPDATE SET
               slot_commit_id=excluded.slot_commit_id,
               record_json=excluded.record_json,
               recorded_at_ms=excluded.recorded_at_ms",
            params![
                self.account_id,
                run_id,
                as_i64(schedule_idx),
                as_i64(attempt),
                record_type,
                slot_commit_id,
                payload,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn load_slot_commit_records(&self, run_id: &str) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT record_json
             FROM slot_commit_records
             WHERE account_id=?1 AND run_id=?2
             ORDER BY schedule_idx, attempt,
               CASE record_type WHEN 'intent' THEN 0 ELSE 1 END",
        )?;
        let mut rows = stmt.query(params![self.account_id, run_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            out.push(parse_json_text(raw)?);
        }
        Ok(out)
    }

    pub fn first_run_id_with_slot_commits(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT run_id FROM slot_commit_records
                 WHERE account_id=?1
                 ORDER BY recorded_at_ms LIMIT 1",
                params![self.account_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn replace_pending_trial_completions(
        &mut self,
        run_id: &str,
        rows: &[Value],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM pending_trial_completions WHERE account_id=?1 AND run_id=?2",
            params![self.account_id, run_id],
        )?;
        for row in rows {
            validate_schema_contract_value(
                row,
                format!("pending_trial_completions row for run '{}'", run_id).as_str(),
            )?;
            let schedule_idx = extract_usize(row, "/schedule_idx")?;
            let trial_result = row
                .get("trial_result")
                .ok_or_else(|| anyhow!("pending completion missing /trial_result"))?;
            tx.execute(
                "INSERT INTO pending_trial_completions
                 (account_id, run_id, schedule_idx, trial_result_json, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    self.account_id,
                    run_id,
                    as_i64(schedule_idx),
                    json_text(trial_result)?,
                    now_ms()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_pending_trial_completions(&self, run_id: &str) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT schedule_idx, trial_result_json
             FROM pending_trial_completions
             WHERE account_id=?1 AND run_id=?2
             ORDER BY schedule_idx",
        )?;
        let mut rows = stmt.query(params![self.account_id, run_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let schedule_idx: i64 = row.get(0)?;
            let trial_result_raw: String = row.get(1)?;
            out.push(serde_json::json!({
                "schema_version": "pending_trial_completion_v1",
                "schedule_idx": schedule_idx,
                "trial_result": parse_json_text(trial_result_raw)?,
            }));
        }
        Ok(out)
    }

    pub fn first_run_id_with_pending_completions(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT run_id FROM pending_trial_completions
                 WHERE account_id=?1
                 ORDER BY updated_at_ms LIMIT 1",
                params![self.account_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_attempt_object(
        &mut self,
        run_id: &str,
        trial_id: &str,
        schedule_idx: usize,
        attempt: usize,
        role: &str,
        object_ref: &str,
        metadata: Option<&Value>,
    ) -> Result<()> {
        let metadata_json = metadata.map(json_text).transpose()?;
        self.conn.execute(
            "INSERT INTO attempt_objects (
               account_id, run_id, trial_id, schedule_idx, attempt, role, object_ref, metadata_json, recorded_at_ms
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
             )
             ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, role) DO UPDATE SET
               object_ref=excluded.object_ref,
               metadata_json=excluded.metadata_json,
               recorded_at_ms=excluded.recorded_at_ms",
            params![
                self.account_id,
                run_id,
                trial_id,
                as_i64(schedule_idx),
                as_i64(attempt),
                role,
                object_ref,
                metadata_json,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn latest_attempt_object_ref(
        &self,
        run_id: &str,
        trial_id: &str,
        role: &str,
    ) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT object_ref
                 FROM attempt_objects
                 WHERE account_id=?1 AND run_id=?2 AND trial_id=?3 AND role=?4
                 ORDER BY attempt DESC, recorded_at_ms DESC
                 LIMIT 1",
                params![self.account_id, run_id, trial_id, role],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn latest_lineage_version_id_for_trial(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT version_id
                 FROM lineage_versions
                 WHERE account_id=?1 AND run_id=?2 AND trial_id=?3
                 ORDER BY step_index DESC
                 LIMIT 1",
                params![self.account_id, run_id, trial_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn lineage_workspace_ref_by_version(&self, version_id: &str) -> Result<Option<String>> {
        let workspace_ref = self
            .conn
            .query_row(
                "SELECT workspace_ref
                 FROM lineage_versions
                 WHERE account_id=?1 AND version_id=?2",
                params![self.account_id, version_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(anyhow::Error::from)?;
        Ok(workspace_ref.flatten())
    }

    pub fn upsert_runtime_operation(
        &mut self,
        run_id: &str,
        op_kind: &str,
        op_id: &str,
        payload: &Value,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runtime_ops (account_id, run_id, op_kind, op_id, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id, run_id, op_kind, op_id) DO UPDATE SET
               payload_json=excluded.payload_json,
               updated_at_ms=excluded.updated_at_ms",
            params![
                self.account_id,
                run_id,
                op_kind,
                op_id,
                json_text(payload)?,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn upsert_performance_sample(&mut self, payload: &Value) -> Result<()> {
        let run_id = extract_str(payload, "/run_id")?;
        let sample_id = extract_str(payload, "/sample_id")?;
        let trial_id = extract_str_opt(payload, "/trial_id");
        let schedule_idx = payload
            .pointer("/schedule_idx")
            .and_then(Value::as_u64)
            .map(|value| value as i64);
        let attempt = payload
            .pointer("/attempt")
            .and_then(Value::as_u64)
            .map(|value| value as i64);
        let sample_seq = payload
            .pointer("/sample_seq")
            .and_then(Value::as_u64)
            .unwrap_or(0) as i64;
        let sample_kind = extract_str(payload, "/sample_kind")?;
        let stage = extract_str(payload, "/stage")?;
        let duration_ms = payload.pointer("/duration_ms").and_then(Value::as_f64);
        let process_rss_kb = payload.pointer("/process_rss_kb").and_then(Value::as_i64);
        let recorded_at_ms = payload
            .pointer("/recorded_at_ms")
            .and_then(Value::as_i64)
            .unwrap_or_else(now_ms);
        self.conn.execute(
            "INSERT INTO performance_samples (
               account_id, run_id, sample_id, trial_id, schedule_idx, attempt,
               sample_seq, sample_kind, stage, duration_ms, process_rss_kb,
               payload_json, recorded_at_ms
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6,
               ?7, ?8, ?9, ?10, ?11,
               ?12, ?13
             )
             ON CONFLICT(account_id, run_id, sample_id) DO UPDATE SET
               trial_id=excluded.trial_id,
               schedule_idx=excluded.schedule_idx,
               attempt=excluded.attempt,
               sample_seq=excluded.sample_seq,
               sample_kind=excluded.sample_kind,
               stage=excluded.stage,
               duration_ms=excluded.duration_ms,
               process_rss_kb=excluded.process_rss_kb,
               payload_json=excluded.payload_json,
               recorded_at_ms=excluded.recorded_at_ms",
            params![
                self.account_id,
                run_id,
                sample_id,
                trial_id,
                schedule_idx,
                attempt,
                sample_seq,
                sample_kind,
                stage,
                duration_ms,
                process_rss_kb,
                json_text(payload)?,
                recorded_at_ms,
            ],
        )?;
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn upsert_lineage_from_chain_state_row(&mut self, row: &Value) -> Result<()> {
        let run_id = extract_str_opt(row, "/run_id")
            .or_else(|| extract_str_opt(row, "/ids/run_id"))
            .ok_or_else(|| anyhow!("missing run_id in chain state row"))?;
        let trial_id = extract_str_opt(row, "/ids/trial_id")
            .ok_or_else(|| anyhow!("missing /ids/trial_id in chain state row"))?;
        let chain_key = extract_str_opt(row, "/chain_id")
            .ok_or_else(|| anyhow!("missing /chain_id in chain state row"))?;
        let step_index = extract_usize(row, "/step_index")?;
        let pre_snapshot_ref = extract_str_opt(row, "/snapshots/prev_ref");
        let post_snapshot_ref = extract_str_opt(row, "/snapshots/post_ref");
        let diff_incremental_ref = extract_str_opt(row, "/diffs/incremental_ref");
        let diff_cumulative_ref = extract_str_opt(row, "/diffs/cumulative_ref");
        let patch_incremental_ref = extract_str_opt(row, "/diffs/patch_incremental_ref");
        let patch_cumulative_ref = extract_str_opt(row, "/diffs/patch_cumulative_ref");
        let workspace_ref = extract_str_opt(row, "/ext/latest_workspace_ref")
            .or_else(|| extract_str_opt(row, "/ext/workspace_ref"));

        let token = format!("{run_id}|{chain_key}|{step_index}|{trial_id}");
        let version_id = sha256_bytes(token.as_bytes());

        let parent_version_id: Option<String> = self
            .conn
            .query_row(
                "SELECT latest_version_id
                 FROM lineage_heads
                 WHERE account_id=?1 AND run_id=?2 AND chain_key=?3",
                params![self.account_id, run_id, chain_key],
                |db_row| db_row.get(0),
            )
            .optional()?;

        let checkpoint_labels = row
            .pointer("/checkpoint_labels")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));

        self.conn.execute(
            "INSERT INTO lineage_versions (
               account_id, version_id, run_id, chain_key, step_index, trial_id, parent_version_id,
               pre_snapshot_ref, post_snapshot_ref,
               diff_incremental_ref, diff_cumulative_ref,
               patch_incremental_ref, patch_cumulative_ref,
               workspace_ref, checkpoint_labels_json
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7,
               ?8, ?9,
               ?10, ?11,
               ?12, ?13,
               ?14, ?15
             )
             ON CONFLICT(version_id) DO UPDATE SET
               parent_version_id=excluded.parent_version_id,
               pre_snapshot_ref=excluded.pre_snapshot_ref,
               post_snapshot_ref=excluded.post_snapshot_ref,
               diff_incremental_ref=excluded.diff_incremental_ref,
               diff_cumulative_ref=excluded.diff_cumulative_ref,
               patch_incremental_ref=excluded.patch_incremental_ref,
               patch_cumulative_ref=excluded.patch_cumulative_ref,
               workspace_ref=excluded.workspace_ref,
               checkpoint_labels_json=excluded.checkpoint_labels_json",
            params![
                self.account_id,
                version_id,
                run_id,
                chain_key,
                as_i64(step_index),
                trial_id,
                parent_version_id,
                pre_snapshot_ref,
                post_snapshot_ref,
                diff_incremental_ref,
                diff_cumulative_ref,
                patch_incremental_ref,
                patch_cumulative_ref,
                workspace_ref,
                json_text(&checkpoint_labels)?
            ],
        )?;

        self.conn.execute(
            "INSERT INTO lineage_heads (account_id, run_id, chain_key, latest_version_id, step_index, latest_workspace_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id, run_id, chain_key) DO UPDATE SET
               latest_version_id=excluded.latest_version_id,
               step_index=excluded.step_index,
               latest_workspace_ref=excluded.latest_workspace_ref",
            params![
                self.account_id,
                run_id,
                chain_key,
                version_id,
                as_i64(step_index),
                workspace_ref
            ],
        )?;
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn upsert_attempt_objects_from_evidence_row(&mut self, row: &Value) -> Result<()> {
        let run_id = extract_str_opt(row, "/run_id")
            .or_else(|| extract_str_opt(row, "/ids/run_id"))
            .ok_or_else(|| anyhow!("missing run_id in evidence row"))?;
        let Some(trial_id) = extract_str_opt(row, "/ids/trial_id") else {
            return Ok(());
        };
        let Some(schedule_idx) = extract_usize(row, "/schedule_idx").ok() else {
            return Ok(());
        };
        let Some(attempt) = extract_usize(row, "/attempt").ok() else {
            return Ok(());
        };
        let Some(evidence) = row.pointer("/evidence").and_then(Value::as_object) else {
            return Ok(());
        };

        for role in [
            "trial_input_ref",
            "trial_output_ref",
            "events_ref",
            "stdout_ref",
            "stderr_ref",
            "workspace_pre_ref",
            "workspace_post_ref",
            "diff_incremental_ref",
            "diff_cumulative_ref",
            "patch_incremental_ref",
            "patch_cumulative_ref",
            "workspace_bundle_ref",
        ] {
            let Some(object_ref) = evidence.get(role).and_then(Value::as_str) else {
                continue;
            };
            let normalized_role = role.trim_end_matches("_ref");
            self.upsert_attempt_object(
                run_id,
                trial_id,
                schedule_idx,
                attempt,
                normalized_role,
                object_ref,
                Some(row),
            )?;
        }
        Ok(())
    }

    pub fn upsert_trial_row(&mut self, row: TrialRowInsert<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO trial_rows (
               account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
               baseline_id, workload_type, variant_id, task_id, repl_idx, outcome,
               primary_metric_name, primary_metric_value_json, metrics_json, bindings_json,
               events_total, has_events, row_json
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7,
               ?8, ?9, ?10, ?11, ?12, ?13,
               ?14, ?15, ?16, ?17,
               ?18, ?19, ?20
             )
             ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
               slot_commit_id=excluded.slot_commit_id,
               baseline_id=excluded.baseline_id,
               workload_type=excluded.workload_type,
               variant_id=excluded.variant_id,
               task_id=excluded.task_id,
               repl_idx=excluded.repl_idx,
               outcome=excluded.outcome,
               primary_metric_name=excluded.primary_metric_name,
               primary_metric_value_json=excluded.primary_metric_value_json,
               metrics_json=excluded.metrics_json,
               bindings_json=excluded.bindings_json,
               events_total=excluded.events_total,
               has_events=excluded.has_events,
               row_json=excluded.row_json",
            params![
                self.account_id,
                row.run_id,
                row.trial_id,
                as_i64(row.schedule_idx),
                as_i64(row.attempt),
                as_i64(row.row_seq),
                row.slot_commit_id,
                row.baseline_id,
                row.workload_type,
                row.variant_id,
                row.task_id,
                as_i64(row.repl_idx),
                row.outcome,
                row.primary_metric_name,
                json_text(row.primary_metric_value)?,
                json_text(row.metrics)?,
                json_text(row.bindings)?,
                as_i64(row.events_total),
                if row.has_events { 1_i64 } else { 0_i64 },
                json_text(row.row_json)?,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_metric_row(&mut self, row: MetricRowInsert<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO metric_rows (
               account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
               variant_id, task_id, repl_idx, outcome,
               metric_name, metric_value_json, metric_source, row_json
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7,
               ?8, ?9, ?10, ?11,
               ?12, ?13, ?14, ?15
             )
             ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
               slot_commit_id=excluded.slot_commit_id,
               variant_id=excluded.variant_id,
               task_id=excluded.task_id,
               repl_idx=excluded.repl_idx,
               outcome=excluded.outcome,
               metric_name=excluded.metric_name,
               metric_value_json=excluded.metric_value_json,
               metric_source=excluded.metric_source,
               row_json=excluded.row_json",
            params![
                self.account_id,
                row.run_id,
                row.trial_id,
                as_i64(row.schedule_idx),
                as_i64(row.attempt),
                as_i64(row.row_seq),
                row.slot_commit_id,
                row.variant_id,
                row.task_id,
                as_i64(row.repl_idx),
                row.outcome,
                row.metric_name,
                json_text(row.metric_value)?,
                row.metric_source,
                json_text(row.row_json)?,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_event_row(&mut self, row: EventRowInsert<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO event_rows (
               account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
               variant_id, task_id, repl_idx, seq, event_type, ts, payload_json, row_json
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7,
               ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
             )
             ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
               slot_commit_id=excluded.slot_commit_id,
               variant_id=excluded.variant_id,
               task_id=excluded.task_id,
               repl_idx=excluded.repl_idx,
               seq=excluded.seq,
               event_type=excluded.event_type,
               ts=excluded.ts,
               payload_json=excluded.payload_json,
               row_json=excluded.row_json",
            params![
                self.account_id,
                row.run_id,
                row.trial_id,
                as_i64(row.schedule_idx),
                as_i64(row.attempt),
                as_i64(row.row_seq),
                row.slot_commit_id,
                row.variant_id,
                row.task_id,
                as_i64(row.repl_idx),
                as_i64(row.seq),
                row.event_type,
                row.ts,
                json_text(row.payload)?,
                json_text(row.row_json)?,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_contract_stage_row(&mut self, row: ContractStageRowInsert<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO contract_stage_rows (
               account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
               variant_id, task_id, repl_idx, stage, status, recorded_at, detail_json, row_json
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7,
               ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
             )
             ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
               slot_commit_id=excluded.slot_commit_id,
               variant_id=excluded.variant_id,
               task_id=excluded.task_id,
               repl_idx=excluded.repl_idx,
               stage=excluded.stage,
               status=excluded.status,
               recorded_at=excluded.recorded_at,
               detail_json=excluded.detail_json,
               row_json=excluded.row_json",
            params![
                self.account_id,
                row.run_id,
                row.trial_id,
                as_i64(row.schedule_idx),
                as_i64(row.attempt),
                as_i64(row.row_seq),
                row.slot_commit_id,
                row.variant_id,
                row.task_id,
                as_i64(row.repl_idx),
                row.stage,
                row.status,
                row.recorded_at,
                json_text(row.detail)?,
                json_text(row.row_json)?,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_variant_snapshot_row(&mut self, row: VariantSnapshotRowInsert<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO variant_snapshot_rows (
               account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
               variant_id, baseline_id, task_id, repl_idx, binding_name,
               binding_value_json, binding_value_text, row_json
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7,
               ?8, ?9, ?10, ?11, ?12,
               ?13, ?14, ?15
             )
             ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, row_seq) DO UPDATE SET
               slot_commit_id=excluded.slot_commit_id,
               variant_id=excluded.variant_id,
               baseline_id=excluded.baseline_id,
               task_id=excluded.task_id,
               repl_idx=excluded.repl_idx,
               binding_name=excluded.binding_name,
               binding_value_json=excluded.binding_value_json,
               binding_value_text=excluded.binding_value_text,
               row_json=excluded.row_json",
            params![
                self.account_id,
                row.run_id,
                row.trial_id,
                as_i64(row.schedule_idx),
                as_i64(row.attempt),
                as_i64(row.row_seq),
                row.slot_commit_id,
                row.variant_id,
                row.baseline_id,
                row.task_id,
                as_i64(row.repl_idx),
                row.binding_name,
                json_text(row.binding_value)?,
                row.binding_value_text,
                json_text(row.row_json)?,
            ],
        )?;
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn upsert_json_row(&mut self, table: JsonRowTable, row: &Value) -> Result<()> {
        let run_id = extract_str(row, "/run_id")?;
        let schedule_idx = extract_usize(row, "/schedule_idx")?;
        let attempt = extract_usize(row, "/attempt")?;
        let row_seq = extract_usize(row, "/row_seq")?;
        let slot_commit_id = extract_str(row, "/slot_commit_id")?;
        let payload = json_text(row)?;
        let (table_name, sql) = match table {
            JsonRowTable::Evidence => (
                "evidence_rows",
                "INSERT INTO evidence_rows
                 (account_id, run_id, schedule_idx, attempt, row_seq, slot_commit_id, row_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(account_id, run_id, schedule_idx, attempt, row_seq) DO UPDATE SET
                   slot_commit_id=excluded.slot_commit_id,
                   row_json=excluded.row_json",
            ),
            JsonRowTable::ChainState => (
                "chain_state_rows",
                "INSERT INTO chain_state_rows
                 (account_id, run_id, schedule_idx, attempt, row_seq, slot_commit_id, row_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(account_id, run_id, schedule_idx, attempt, row_seq) DO UPDATE SET
                   slot_commit_id=excluded.slot_commit_id,
                   row_json=excluded.row_json",
            ),
            JsonRowTable::BenchmarkConclusion => (
                "benchmark_conclusion_rows",
                "INSERT INTO benchmark_conclusion_rows
                 (account_id, run_id, schedule_idx, attempt, row_seq, slot_commit_id, row_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(account_id, run_id, schedule_idx, attempt, row_seq) DO UPDATE SET
                   slot_commit_id=excluded.slot_commit_id,
                   row_json=excluded.row_json",
            ),
        };
        self.conn
            .execute(
                sql,
                params![
                    self.account_id,
                    run_id,
                    as_i64(schedule_idx),
                    as_i64(attempt),
                    as_i64(row_seq),
                    slot_commit_id,
                    payload
                ],
            )
            .with_context(|| format!("upsert row in {}", table_name))?;
        match table {
            JsonRowTable::Evidence => {
                self.upsert_attempt_objects_from_evidence_row(row)?;
            }
            JsonRowTable::ChainState => {
                self.upsert_lineage_from_chain_state_row(row)?;
            }
            JsonRowTable::BenchmarkConclusion => {}
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn has_lineage_for_trial(&self, run_id: &str, trial_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*)
             FROM lineage_versions
             WHERE account_id=?1 AND run_id=?2 AND trial_id=?3",
            params![self.account_id, run_id, trial_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    #[cfg(test)]
    pub fn row_count(&self, table: &str) -> Result<i64> {
        let sql = format!("SELECT count(*) FROM {}", table);
        let count = self.conn.query_row(&sql, [], |row| row.get(0))?;
        Ok(count)
    }
}

#[cfg(test)]
pub(crate) fn load_pending_trial_completion_records(
    run_dir: &Path,
) -> Result<BTreeMap<usize, TrialExecutionResult>> {
    let store = SqliteRunStore::open(run_dir)?;
    let run_id = store
        .get_runtime_json(RUNTIME_KEY_RUN_CONTROL)?
        .and_then(|value| {
            value
                .pointer("/run_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| store.first_run_id_with_pending_completions().ok().flatten())
        .unwrap_or_default();
    if run_id.is_empty() {
        return Ok(BTreeMap::new());
    }
    let records = store.load_pending_trial_completions(&run_id)?;
    let mut by_schedule = BTreeMap::new();
    for record_value in records {
        let record: PendingTrialCompletionRecord = serde_json::from_value(record_value)?;
        if record.schema_version != "pending_trial_completion_v1" {
            continue;
        }
        by_schedule.insert(record.schedule_idx, record.trial_result);
    }
    Ok(by_schedule)
}

#[cfg(test)]
pub(crate) fn persist_pending_trial_completions(
    run_dir: &Path,
    records: &[PendingTrialCompletionRecord],
) -> Result<()> {
    let run_id = SqliteRunStore::open(run_dir)?
        .get_runtime_json(RUNTIME_KEY_RUN_CONTROL)?
        .and_then(|value| {
            value
                .pointer("/run_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            records
                .iter()
                .find_map(|row| row.trial_result.deferred_trial_records.first())
                .map(|row| row.run_id.clone())
        })
        .unwrap_or_default();
    if run_id.is_empty() {
        return Ok(());
    }
    let values = records
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for value in &values {
        validate_schema_contract_value(value, "pending trial completion")?;
    }
    let mut store = SqliteRunStore::open(run_dir)?;
    store.replace_pending_trial_completions(&run_id, &values)
}
