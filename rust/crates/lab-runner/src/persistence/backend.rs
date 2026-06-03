use crate::experiment::state::{
    PendingTrialCompletionRecord, ScheduleSlotRecord, SlotCommitRecord,
};
use crate::model::{TrialExecutionResult, TrialSlot};
use crate::persistence::journal::{RunSink, SqliteRunJournal};
use crate::persistence::postgres::{
    postgres_schema_name, quote_ident, PostgresRunStore, BUCEPHALUS_RUN_STORE_URL_ENV,
};
use crate::persistence::rows::EventRow;
use crate::persistence::store::account_sqlite_path_for_run;
use crate::persistence::store::TrialAttemptRecord;
use crate::persistence::store::{SlotCommitTransactionInput, SqliteRunStore};
use crate::trial::state::TrialAttemptState;
use anyhow::Result;
use anyhow::{anyhow, Context};
use postgres::{Client, NoTls};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) const BUCEPHALUS_RUN_STORE_ENV: &str = "BUCEPHALUS_RUN_STORE";

pub(crate) trait RuntimeKvStore {
    fn put_runtime_json(&mut self, key: &str, value: &Value) -> Result<()>;
    fn get_runtime_json(&self, key: &str) -> Result<Option<Value>>;
}

pub(crate) trait TrialAttemptStateStore {
    fn upsert_trial_attempt_state(
        &mut self,
        run_id: &str,
        trial_id: &str,
        state: &TrialAttemptState,
    ) -> Result<()>;
}

pub(crate) trait RuntimeStateStore: RuntimeKvStore + TrialAttemptStateStore {}

impl<T> RuntimeStateStore for T where T: RuntimeKvStore + TrialAttemptStateStore {}

pub(crate) trait TrialAttemptStore: TrialAttemptStateStore {
    fn load_latest_trial_attempt(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<TrialAttemptRecord>>;

    fn trial_attempt_container_ids(&self, run_id: &str, trial_id: &str) -> Result<Vec<String>>;

    fn trial_attempts_for_recovery(&self, run_id: &str) -> Result<Vec<TrialAttemptRecord>>;
}

pub(crate) trait EventRowStore {
    fn append_event_rows(&mut self, rows: &[EventRow]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}

pub(crate) trait ScheduleSlotStore {
    fn ensure_schedule_slots(&mut self, run_id: &str, schedule: &[TrialSlot]) -> Result<()>;

    fn active_schedule_slots(&self, run_id: &str) -> Result<Vec<ScheduleSlotRecord>>;

    fn claim_schedule_slot(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
        trial_id: &str,
        worker_id: &str,
        owner_id: &str,
        lease_expires_at: Option<&str>,
    ) -> Result<Option<ScheduleSlotRecord>>;

    fn mark_schedule_slot_committed(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
        trial_id: &str,
        attempt: usize,
        slot_commit_id: &str,
        slot_status: &str,
    ) -> Result<()>;

    fn release_schedule_slot_to_pending(&mut self, run_id: &str, schedule_idx: usize)
        -> Result<()>;

    fn commit_schedule_slot_transaction(
        &mut self,
        input: SlotCommitTransactionInput<'_>,
    ) -> Result<()>;
}

pub(crate) trait ScheduleSlotReadStore {
    fn schedule_slot(
        &self,
        run_id: &str,
        schedule_idx: usize,
    ) -> Result<Option<ScheduleSlotRecord>>;
}

pub(crate) trait SlotCommitRecordStore {
    fn upsert_slot_commit_record(&mut self, record: &Value) -> Result<()>;
    fn load_slot_commit_records(&self, run_id: &str) -> Result<Vec<Value>>;
    fn first_run_id_with_slot_commits(&self) -> Result<Option<String>>;
}

pub(crate) trait PendingCompletionStore {
    fn replace_pending_trial_completions(&mut self, run_id: &str, rows: &[Value]) -> Result<()>;
    fn load_pending_trial_completions(&self, run_id: &str) -> Result<Vec<Value>>;
    fn first_run_id_with_pending_completions(&self) -> Result<Option<String>>;
}

pub(crate) trait AttemptObjectStore {
    fn upsert_attempt_object(
        &mut self,
        run_id: &str,
        trial_id: &str,
        schedule_idx: usize,
        attempt: usize,
        role: &str,
        object_ref: &str,
        metadata: Option<&Value>,
    ) -> Result<()>;

    fn latest_attempt_object_ref(
        &self,
        run_id: &str,
        trial_id: &str,
        role: &str,
    ) -> Result<Option<String>>;
}

pub(crate) trait LineageStore {
    fn latest_lineage_version_id_for_trial(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<String>>;

    fn lineage_workspace_ref_by_version(&self, version_id: &str) -> Result<Option<String>>;
}

pub(crate) trait RuntimeOperationStore {
    fn upsert_runtime_operation(
        &mut self,
        run_id: &str,
        op_kind: &str,
        op_id: &str,
        payload: &Value,
    ) -> Result<()>;
}

pub(crate) trait PerformanceSampleStore {
    fn upsert_performance_sample(&mut self, payload: &Value) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct RunStoreInventoryEntry {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub experiment_id: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Default)]
pub struct RunStoreMetrics {
    pub variants: usize,
    pub pass_rate: Option<f64>,
}

impl RuntimeKvStore for SqliteRunStore {
    fn put_runtime_json(&mut self, key: &str, value: &Value) -> Result<()> {
        SqliteRunStore::put_runtime_json(self, key, value)
    }

    fn get_runtime_json(&self, key: &str) -> Result<Option<Value>> {
        SqliteRunStore::get_runtime_json(self, key)
    }
}

impl TrialAttemptStateStore for SqliteRunStore {
    fn upsert_trial_attempt_state(
        &mut self,
        run_id: &str,
        trial_id: &str,
        state: &TrialAttemptState,
    ) -> Result<()> {
        SqliteRunStore::upsert_trial_attempt_state(self, run_id, trial_id, state)
    }
}

impl TrialAttemptStore for SqliteRunStore {
    fn load_latest_trial_attempt(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<TrialAttemptRecord>> {
        SqliteRunStore::load_latest_trial_attempt(self, run_id, trial_id)
    }

    fn trial_attempt_container_ids(&self, run_id: &str, trial_id: &str) -> Result<Vec<String>> {
        SqliteRunStore::trial_attempt_container_ids(self, run_id, trial_id)
    }

    fn trial_attempts_for_recovery(&self, run_id: &str) -> Result<Vec<TrialAttemptRecord>> {
        SqliteRunStore::trial_attempts_for_recovery(self, run_id)
    }
}

impl EventRowStore for SqliteRunJournal {
    fn append_event_rows(&mut self, rows: &[EventRow]) -> Result<()> {
        RunSink::append_event_rows(self, rows)
    }

    fn flush(&mut self) -> Result<()> {
        RunSink::flush(self)
    }
}

impl ScheduleSlotStore for SqliteRunStore {
    fn ensure_schedule_slots(&mut self, run_id: &str, schedule: &[TrialSlot]) -> Result<()> {
        SqliteRunStore::ensure_schedule_slots(self, run_id, schedule)
    }

    fn active_schedule_slots(&self, run_id: &str) -> Result<Vec<ScheduleSlotRecord>> {
        SqliteRunStore::active_schedule_slots(self, run_id)
    }

    fn claim_schedule_slot(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
        trial_id: &str,
        worker_id: &str,
        owner_id: &str,
        lease_expires_at: Option<&str>,
    ) -> Result<Option<ScheduleSlotRecord>> {
        SqliteRunStore::claim_schedule_slot(
            self,
            run_id,
            schedule_idx,
            trial_id,
            worker_id,
            owner_id,
            lease_expires_at,
        )
    }

    fn mark_schedule_slot_committed(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
        trial_id: &str,
        attempt: usize,
        slot_commit_id: &str,
        slot_status: &str,
    ) -> Result<()> {
        SqliteRunStore::mark_schedule_slot_committed(
            self,
            run_id,
            schedule_idx,
            trial_id,
            attempt,
            slot_commit_id,
            slot_status,
        )
    }

    fn release_schedule_slot_to_pending(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
    ) -> Result<()> {
        SqliteRunStore::release_schedule_slot_to_pending(self, run_id, schedule_idx)
    }

    fn commit_schedule_slot_transaction(
        &mut self,
        input: SlotCommitTransactionInput<'_>,
    ) -> Result<()> {
        SqliteRunStore::commit_schedule_slot_transaction(self, input)
    }
}

impl ScheduleSlotReadStore for SqliteRunStore {
    fn schedule_slot(
        &self,
        run_id: &str,
        schedule_idx: usize,
    ) -> Result<Option<ScheduleSlotRecord>> {
        SqliteRunStore::schedule_slot(self, run_id, schedule_idx)
    }
}

impl SlotCommitRecordStore for SqliteRunStore {
    fn upsert_slot_commit_record(&mut self, record: &Value) -> Result<()> {
        SqliteRunStore::upsert_slot_commit_record(self, record)
    }

    fn load_slot_commit_records(&self, run_id: &str) -> Result<Vec<Value>> {
        SqliteRunStore::load_slot_commit_records(self, run_id)
    }

    fn first_run_id_with_slot_commits(&self) -> Result<Option<String>> {
        SqliteRunStore::first_run_id_with_slot_commits(self)
    }
}

impl PendingCompletionStore for SqliteRunStore {
    fn replace_pending_trial_completions(&mut self, run_id: &str, rows: &[Value]) -> Result<()> {
        SqliteRunStore::replace_pending_trial_completions(self, run_id, rows)
    }

    fn load_pending_trial_completions(&self, run_id: &str) -> Result<Vec<Value>> {
        SqliteRunStore::load_pending_trial_completions(self, run_id)
    }

    fn first_run_id_with_pending_completions(&self) -> Result<Option<String>> {
        SqliteRunStore::first_run_id_with_pending_completions(self)
    }
}

impl AttemptObjectStore for SqliteRunStore {
    fn upsert_attempt_object(
        &mut self,
        run_id: &str,
        trial_id: &str,
        schedule_idx: usize,
        attempt: usize,
        role: &str,
        object_ref: &str,
        metadata: Option<&Value>,
    ) -> Result<()> {
        SqliteRunStore::upsert_attempt_object(
            self,
            run_id,
            trial_id,
            schedule_idx,
            attempt,
            role,
            object_ref,
            metadata,
        )
    }

    fn latest_attempt_object_ref(
        &self,
        run_id: &str,
        trial_id: &str,
        role: &str,
    ) -> Result<Option<String>> {
        SqliteRunStore::latest_attempt_object_ref(self, run_id, trial_id, role)
    }
}

impl LineageStore for SqliteRunStore {
    fn latest_lineage_version_id_for_trial(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<String>> {
        SqliteRunStore::latest_lineage_version_id_for_trial(self, run_id, trial_id)
    }

    fn lineage_workspace_ref_by_version(&self, version_id: &str) -> Result<Option<String>> {
        SqliteRunStore::lineage_workspace_ref_by_version(self, version_id)
    }
}

impl RuntimeOperationStore for SqliteRunStore {
    fn upsert_runtime_operation(
        &mut self,
        run_id: &str,
        op_kind: &str,
        op_id: &str,
        payload: &Value,
    ) -> Result<()> {
        SqliteRunStore::upsert_runtime_operation(self, run_id, op_kind, op_id, payload)
    }
}

impl PerformanceSampleStore for SqliteRunStore {
    fn upsert_performance_sample(&mut self, payload: &Value) -> Result<()> {
        SqliteRunStore::upsert_performance_sample(self, payload)
    }
}

impl RuntimeKvStore for PostgresRunStore {
    fn put_runtime_json(&mut self, key: &str, value: &Value) -> Result<()> {
        PostgresRunStore::put_runtime_json(self, key, value)
    }

    fn get_runtime_json(&self, key: &str) -> Result<Option<Value>> {
        PostgresRunStore::get_runtime_json(self, key)
    }
}

impl TrialAttemptStateStore for PostgresRunStore {
    fn upsert_trial_attempt_state(
        &mut self,
        run_id: &str,
        trial_id: &str,
        state: &TrialAttemptState,
    ) -> Result<()> {
        PostgresRunStore::upsert_trial_attempt_state(self, run_id, trial_id, state)
    }
}

impl TrialAttemptStore for PostgresRunStore {
    fn load_latest_trial_attempt(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<TrialAttemptRecord>> {
        PostgresRunStore::load_latest_trial_attempt(self, run_id, trial_id)
    }

    fn trial_attempt_container_ids(&self, run_id: &str, trial_id: &str) -> Result<Vec<String>> {
        PostgresRunStore::trial_attempt_container_ids(self, run_id, trial_id)
    }

    fn trial_attempts_for_recovery(&self, run_id: &str) -> Result<Vec<TrialAttemptRecord>> {
        PostgresRunStore::trial_attempts_for_recovery(self, run_id)
    }
}

impl EventRowStore for PostgresRunStore {
    fn append_event_rows(&mut self, rows: &[EventRow]) -> Result<()> {
        RunSink::append_event_rows(self, rows)
    }

    fn flush(&mut self) -> Result<()> {
        RunSink::flush(self)
    }
}

impl ScheduleSlotStore for PostgresRunStore {
    fn ensure_schedule_slots(&mut self, run_id: &str, schedule: &[TrialSlot]) -> Result<()> {
        PostgresRunStore::ensure_schedule_slots(self, run_id, schedule)
    }

    fn active_schedule_slots(&self, run_id: &str) -> Result<Vec<ScheduleSlotRecord>> {
        PostgresRunStore::active_schedule_slots(self, run_id)
    }

    fn claim_schedule_slot(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
        trial_id: &str,
        worker_id: &str,
        owner_id: &str,
        lease_expires_at: Option<&str>,
    ) -> Result<Option<ScheduleSlotRecord>> {
        PostgresRunStore::claim_schedule_slot(
            self,
            run_id,
            schedule_idx,
            trial_id,
            worker_id,
            owner_id,
            lease_expires_at,
        )
    }

    fn mark_schedule_slot_committed(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
        trial_id: &str,
        attempt: usize,
        slot_commit_id: &str,
        slot_status: &str,
    ) -> Result<()> {
        PostgresRunStore::mark_schedule_slot_committed(
            self,
            run_id,
            schedule_idx,
            trial_id,
            attempt,
            slot_commit_id,
            slot_status,
        )
    }

    fn release_schedule_slot_to_pending(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
    ) -> Result<()> {
        PostgresRunStore::release_schedule_slot_to_pending(self, run_id, schedule_idx)
    }

    fn commit_schedule_slot_transaction(
        &mut self,
        input: SlotCommitTransactionInput<'_>,
    ) -> Result<()> {
        PostgresRunStore::commit_schedule_slot_transaction(self, input)
    }
}

impl ScheduleSlotReadStore for PostgresRunStore {
    fn schedule_slot(
        &self,
        run_id: &str,
        schedule_idx: usize,
    ) -> Result<Option<ScheduleSlotRecord>> {
        PostgresRunStore::schedule_slot(self, run_id, schedule_idx)
    }
}

impl SlotCommitRecordStore for PostgresRunStore {
    fn upsert_slot_commit_record(&mut self, record: &Value) -> Result<()> {
        PostgresRunStore::upsert_slot_commit_record(self, record)
    }

    fn load_slot_commit_records(&self, run_id: &str) -> Result<Vec<Value>> {
        PostgresRunStore::load_slot_commit_records(self, run_id)
    }

    fn first_run_id_with_slot_commits(&self) -> Result<Option<String>> {
        PostgresRunStore::first_run_id_with_slot_commits(self)
    }
}

impl PendingCompletionStore for PostgresRunStore {
    fn replace_pending_trial_completions(&mut self, run_id: &str, rows: &[Value]) -> Result<()> {
        PostgresRunStore::replace_pending_trial_completions(self, run_id, rows)
    }

    fn load_pending_trial_completions(&self, run_id: &str) -> Result<Vec<Value>> {
        PostgresRunStore::load_pending_trial_completions(self, run_id)
    }

    fn first_run_id_with_pending_completions(&self) -> Result<Option<String>> {
        PostgresRunStore::first_run_id_with_pending_completions(self)
    }
}

impl AttemptObjectStore for PostgresRunStore {
    fn upsert_attempt_object(
        &mut self,
        run_id: &str,
        trial_id: &str,
        schedule_idx: usize,
        attempt: usize,
        role: &str,
        object_ref: &str,
        metadata: Option<&Value>,
    ) -> Result<()> {
        PostgresRunStore::upsert_attempt_object(
            self,
            run_id,
            trial_id,
            schedule_idx,
            attempt,
            role,
            object_ref,
            metadata,
        )
    }

    fn latest_attempt_object_ref(
        &self,
        run_id: &str,
        trial_id: &str,
        role: &str,
    ) -> Result<Option<String>> {
        PostgresRunStore::latest_attempt_object_ref(self, run_id, trial_id, role)
    }
}

impl LineageStore for PostgresRunStore {
    fn latest_lineage_version_id_for_trial(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<String>> {
        PostgresRunStore::latest_lineage_version_id_for_trial(self, run_id, trial_id)
    }

    fn lineage_workspace_ref_by_version(&self, version_id: &str) -> Result<Option<String>> {
        PostgresRunStore::lineage_workspace_ref_by_version(self, version_id)
    }
}

impl RuntimeOperationStore for PostgresRunStore {
    fn upsert_runtime_operation(
        &mut self,
        run_id: &str,
        op_kind: &str,
        op_id: &str,
        payload: &Value,
    ) -> Result<()> {
        PostgresRunStore::upsert_runtime_operation(self, run_id, op_kind, op_id, payload)
    }
}

impl PerformanceSampleStore for PostgresRunStore {
    fn upsert_performance_sample(&mut self, payload: &Value) -> Result<()> {
        PostgresRunStore::upsert_performance_sample(self, payload)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunStoreBackend {
    Sqlite,
    Postgres,
}

fn selected_backend() -> Result<RunStoreBackend> {
    if let Ok(value) = std::env::var(BUCEPHALUS_RUN_STORE_ENV) {
        return match value.trim().to_ascii_lowercase().as_str() {
            "" | "sqlite" => Ok(RunStoreBackend::Sqlite),
            "postgres" | "postgresql" => Ok(RunStoreBackend::Postgres),
            other => Err(anyhow!(
                "unsupported {} '{}'",
                BUCEPHALUS_RUN_STORE_ENV,
                other
            )),
        };
    }
    if std::env::var(BUCEPHALUS_RUN_STORE_URL_ENV)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(RunStoreBackend::Postgres);
    }
    if std::env::var("BUCEPHALUS_CLOUD_API_URL").is_ok()
        && std::env::var("DATABASE_URL")
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(RunStoreBackend::Postgres);
    }
    Ok(RunStoreBackend::Sqlite)
}

fn postgres_url() -> Result<String> {
    std::env::var(BUCEPHALUS_RUN_STORE_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .filter(|value| !value.trim().is_empty())
        .context(
            "postgres run store selected but no BUCEPHALUS_RUN_STORE_URL or DATABASE_URL is set",
        )
}

fn open_postgres_store(run_dir: &Path) -> Result<PostgresRunStore> {
    PostgresRunStore::open(run_dir, &postgres_url()?)
}

fn run_id_from_dir(run_dir: &Path) -> Option<String> {
    run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
}

fn postgres_client() -> Result<Client> {
    Client::connect(&postgres_url()?, NoTls).context("connect postgres runtime store")
}

fn postgres_table(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(name))
}

pub(crate) fn load_runtime_json(run_dir: &Path, key: &str) -> Result<Option<Value>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => {
            let db_path = account_sqlite_path_for_run(run_dir)?;
            if !db_path.exists() {
                return Ok(None);
            }
            let Some(run_id) = run_id_from_dir(run_dir) else {
                return Ok(None);
            };
            let account_id = crate::persistence::store::active_account_id();
            let conn = Connection::open(db_path)?;
            let raw: Option<String> = conn
                .query_row(
                    "SELECT value_json FROM runtime_kv
                     WHERE account_id=?1 AND run_id=?2 AND key=?3",
                    params![account_id, run_id, key],
                    |row| row.get(0),
                )
                .optional()?;
            raw.map(|payload| serde_json::from_str::<Value>(&payload).context("parse runtime json"))
                .transpose()
        }
        RunStoreBackend::Postgres => {
            let Some(run_id) = run_id_from_dir(run_dir) else {
                return Ok(None);
            };
            let schema = postgres_schema_name()?;
            let account_id = crate::persistence::store::active_account_id();
            let mut client = postgres_client()?;
            let sql = format!(
                "SELECT value_json FROM {}
                 WHERE account_id=$1 AND run_id=$2 AND key=$3",
                postgres_table(&schema, "runtime_kv")
            );
            let raw: Option<String> = client
                .query_opt(&sql, &[&account_id, &run_id, &key])?
                .map(|row| row.get(0));
            raw.map(|payload| serde_json::from_str::<Value>(&payload).context("parse runtime json"))
                .transpose()
        }
    }
}

pub(crate) fn resolve_run_dir(run_id: &str, anchor: &Path) -> Result<Option<PathBuf>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => {
            let db_path = account_sqlite_path_for_run(anchor)?;
            if !db_path.exists() {
                return Ok(None);
            }
            let account_id = crate::persistence::store::active_account_id();
            let conn = Connection::open(db_path)?;
            let run_dir: Option<String> = conn
                .query_row(
                    "SELECT run_dir FROM runs WHERE account_id=?1 AND run_id=?2",
                    params![account_id, run_id],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(run_dir.map(PathBuf::from))
        }
        RunStoreBackend::Postgres => {
            let schema = postgres_schema_name()?;
            let account_id = crate::persistence::store::active_account_id();
            let mut client = postgres_client()?;
            let sql = format!(
                "SELECT run_dir FROM {} WHERE account_id=$1 AND run_id=$2",
                postgres_table(&schema, "runs")
            );
            let run_dir: Option<String> = client
                .query_opt(&sql, &[&account_id, &run_id])?
                .map(|row| row.get(0));
            Ok(run_dir.map(PathBuf::from))
        }
    }
}

pub(crate) fn list_run_inventory(anchor: &Path) -> Result<Vec<RunStoreInventoryEntry>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => {
            let db_path = account_sqlite_path_for_run(anchor)?;
            if !db_path.exists() {
                return Ok(Vec::new());
            }
            let account_id = crate::persistence::store::active_account_id();
            let conn = Connection::open(db_path)?;
            let mut stmt = conn.prepare(
                "SELECT run_id, run_dir, experiment_id, updated_at_ms FROM runs
                 WHERE account_id=?1
                 ORDER BY updated_at_ms DESC",
            )?;
            let mut rows = stmt.query(params![account_id])?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next()? {
                entries.push(RunStoreInventoryEntry {
                    run_id: row.get(0)?,
                    run_dir: PathBuf::from(row.get::<_, String>(1)?),
                    experiment_id: row.get(2)?,
                    updated_at_ms: row.get(3)?,
                });
            }
            Ok(entries)
        }
        RunStoreBackend::Postgres => {
            let schema = postgres_schema_name()?;
            let account_id = crate::persistence::store::active_account_id();
            let mut client = postgres_client()?;
            let sql = format!(
                "SELECT run_id, run_dir, experiment_id, updated_at_ms FROM {}
                 WHERE account_id=$1
                 ORDER BY updated_at_ms DESC",
                postgres_table(&schema, "runs")
            );
            let rows = client.query(&sql, &[&account_id])?;
            Ok(rows
                .into_iter()
                .map(|row| RunStoreInventoryEntry {
                    run_id: row.get(0),
                    run_dir: PathBuf::from(row.get::<_, String>(1)),
                    experiment_id: row.get(2),
                    updated_at_ms: row.get(3),
                })
                .collect())
        }
    }
}

pub(crate) fn run_metrics(run_dir: &Path) -> Result<RunStoreMetrics> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => {
            let db_path = account_sqlite_path_for_run(run_dir)?;
            if !db_path.exists() {
                return Ok(RunStoreMetrics::default());
            }
            let Some(run_id) = run_id_from_dir(run_dir) else {
                return Ok(RunStoreMetrics::default());
            };
            let account_id = crate::persistence::store::active_account_id();
            let conn = Connection::open(db_path)?;
            let variants = conn
                .query_row(
                    "SELECT count(DISTINCT variant_id)
                     FROM trial_rows
                     WHERE account_id=?1 AND run_id=?2",
                    params![account_id, run_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0_i64) as usize;
            let baseline_id: Option<String> = conn
                .query_row(
                    "SELECT baseline_id FROM trial_rows
                     WHERE account_id=?1 AND run_id=?2
                     LIMIT 1",
                    params![account_id, run_id],
                    |row| row.get(0),
                )
                .optional()
                .unwrap_or(None);
            let pass_rate = match baseline_id {
                Some(baseline) => conn
                    .query_row(
                        "SELECT avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END)
                         FROM trial_rows
                         WHERE account_id=?1 AND run_id=?2 AND variant_id = ?3",
                        params![account_id, run_id, baseline],
                        |row| row.get::<_, Option<f64>>(0),
                    )
                    .unwrap_or(None),
                None => None,
            };
            Ok(RunStoreMetrics {
                variants,
                pass_rate,
            })
        }
        RunStoreBackend::Postgres => {
            let Some(run_id) = run_id_from_dir(run_dir) else {
                return Ok(RunStoreMetrics::default());
            };
            let schema = postgres_schema_name()?;
            let account_id = crate::persistence::store::active_account_id();
            let mut client = postgres_client()?;
            let trial_rows = postgres_table(&schema, "trial_rows");
            let variants: i64 = client
                .query_one(
                    &format!(
                        "SELECT count(DISTINCT variant_id)
                         FROM {trial_rows}
                         WHERE account_id=$1 AND run_id=$2"
                    ),
                    &[&account_id, &run_id],
                )?
                .get(0);
            let baseline_id: Option<String> = client
                .query_opt(
                    &format!(
                        "SELECT baseline_id FROM {trial_rows}
                         WHERE account_id=$1 AND run_id=$2
                         LIMIT 1"
                    ),
                    &[&account_id, &run_id],
                )?
                .map(|row| row.get(0));
            let pass_rate = match baseline_id {
                Some(baseline) => client
                    .query_one(
                        &format!(
                            "SELECT avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END)
                             FROM {trial_rows}
                             WHERE account_id=$1 AND run_id=$2 AND variant_id=$3"
                        ),
                        &[&account_id, &run_id, &baseline],
                    )?
                    .get(0),
                None => None,
            };
            Ok(RunStoreMetrics {
                variants: variants as usize,
                pass_rate,
            })
        }
    }
}

pub(crate) fn open_runtime_state_store(
    run_dir: &Path,
) -> Result<Box<dyn RuntimeStateStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_event_row_store(run_dir: &Path) -> Result<Box<dyn EventRowStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunJournal::new(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_schedule_slot_store(
    run_dir: &Path,
) -> Result<Box<dyn ScheduleSlotStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_schedule_slot_read_store(
    run_dir: &Path,
) -> Result<Box<dyn ScheduleSlotReadStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_trial_attempt_store(
    run_dir: &Path,
) -> Result<Box<dyn TrialAttemptStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_slot_commit_record_store(
    run_dir: &Path,
) -> Result<Box<dyn SlotCommitRecordStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_pending_completion_store(
    run_dir: &Path,
) -> Result<Box<dyn PendingCompletionStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_attempt_object_store(
    run_dir: &Path,
) -> Result<Box<dyn AttemptObjectStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_lineage_store(run_dir: &Path) -> Result<Box<dyn LineageStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_runtime_operation_store(
    run_dir: &Path,
) -> Result<Box<dyn RuntimeOperationStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_performance_sample_store(
    run_dir: &Path,
) -> Result<Box<dyn PerformanceSampleStore + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunStore::open(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn open_run_sink(run_dir: &Path) -> Result<Box<dyn RunSink + Send>> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(Box::new(SqliteRunJournal::new(run_dir)?)),
        RunStoreBackend::Postgres => Ok(Box::new(open_postgres_store(run_dir)?)),
    }
}

pub(crate) fn run_store_location(run_dir: &Path) -> Result<String> {
    match selected_backend()? {
        RunStoreBackend::Sqlite => Ok(account_sqlite_path_for_run(run_dir)?.display().to_string()),
        RunStoreBackend::Postgres => Ok("postgres:<configured>".to_string()),
    }
}

pub(crate) fn append_slot_commit_record(run_dir: &Path, record: &SlotCommitRecord) -> Result<()> {
    let record_json = serde_json::to_value(record)?;
    crate::package::validate::validate_schema_contract_value(&record_json, "slot commit record")?;
    open_slot_commit_record_store(run_dir)?.upsert_slot_commit_record(&record_json)
}

pub(crate) fn load_slot_commit_records(run_dir: &Path) -> Result<Vec<SlotCommitRecord>> {
    let store = open_slot_commit_record_store(run_dir)?;
    let run_id = open_runtime_state_store(run_dir)?
        .get_runtime_json(crate::model::RUNTIME_KEY_RUN_CONTROL)?
        .and_then(|value| {
            value
                .pointer("/run_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| store.first_run_id_with_slot_commits().ok().flatten())
        .unwrap_or_default();
    if run_id.is_empty() {
        return Ok(Vec::new());
    }
    let values = store.load_slot_commit_records(&run_id)?;
    let mut rows = Vec::with_capacity(values.len());
    for value in values {
        rows.push(serde_json::from_value(value)?);
    }
    Ok(rows)
}

pub(crate) fn load_pending_trial_completion_records(
    run_dir: &Path,
) -> Result<BTreeMap<usize, TrialExecutionResult>> {
    let store = open_pending_completion_store(run_dir)?;
    let run_id = open_runtime_state_store(run_dir)?
        .get_runtime_json(crate::model::RUNTIME_KEY_RUN_CONTROL)?
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

pub(crate) fn persist_pending_trial_completions(
    run_dir: &Path,
    records: &[PendingTrialCompletionRecord],
) -> Result<()> {
    let run_id = open_runtime_state_store(run_dir)?
        .get_runtime_json(crate::model::RUNTIME_KEY_RUN_CONTROL)?
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
        crate::package::validate::validate_schema_contract_value(
            value,
            "pending trial completion",
        )?;
    }
    open_pending_completion_store(run_dir)?.replace_pending_trial_completions(&run_id, &values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn clear(keys: &[&'static str]) -> Self {
            let saved = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for key in keys {
                std::env::remove_var(key);
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }

    const KEYS: &[&str] = &[
        BUCEPHALUS_RUN_STORE_ENV,
        BUCEPHALUS_RUN_STORE_URL_ENV,
        "BUCEPHALUS_CLOUD_API_URL",
        "DATABASE_URL",
    ];

    #[test]
    fn run_store_backend_defaults_to_sqlite() {
        let _lock = env_lock();
        let _guard = EnvGuard::clear(KEYS);
        assert_eq!(selected_backend().unwrap(), RunStoreBackend::Sqlite);
    }

    #[test]
    fn run_store_backend_accepts_explicit_postgres() {
        let _lock = env_lock();
        let _guard = EnvGuard::clear(KEYS);
        std::env::set_var(BUCEPHALUS_RUN_STORE_ENV, "postgres");
        assert_eq!(selected_backend().unwrap(), RunStoreBackend::Postgres);
    }

    #[test]
    fn run_store_backend_uses_explicit_store_url() {
        let _lock = env_lock();
        let _guard = EnvGuard::clear(KEYS);
        std::env::set_var(BUCEPHALUS_RUN_STORE_URL_ENV, "postgres://example/db");
        assert_eq!(selected_backend().unwrap(), RunStoreBackend::Postgres);
    }

    #[test]
    fn run_store_backend_uses_database_url_only_in_cloud_context() {
        let _lock = env_lock();
        let _guard = EnvGuard::clear(KEYS);
        std::env::set_var("DATABASE_URL", "postgres://example/db");
        assert_eq!(selected_backend().unwrap(), RunStoreBackend::Sqlite);
        std::env::set_var("BUCEPHALUS_CLOUD_API_URL", "https://cloud.example");
        assert_eq!(selected_backend().unwrap(), RunStoreBackend::Postgres);
    }
}
