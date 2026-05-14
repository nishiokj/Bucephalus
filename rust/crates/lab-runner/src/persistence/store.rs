use crate::experiment::state::PendingTrialCompletionRecord;
use crate::model::{TrialExecutionResult, RUNTIME_KEY_RUN_CONTROL};
use crate::package::validate::validate_schema_contract_value;
use crate::persistence::rows::JsonRowTable;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use lab_core::sha256_bytes;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const SCHEMA_SQL: &str = include_str!("schema_v2.sql");
pub const ACCOUNT_SQLITE_FILE: &str = "agentlab.sqlite";
pub const AGENTLAB_DB_ENV: &str = "AGENTLAB_DB";
#[cfg_attr(test, allow(dead_code))]
pub const AGENTLAB_HOME_ENV: &str = "AGENTLAB_HOME";
pub const AGENTLAB_ACCOUNT_ID_ENV: &str = "AGENTLAB_ACCOUNT_ID";

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
    pub hook_events_total: usize,
    pub has_hook_events: bool,
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

pub fn account_sqlite_path_for_run(_run_dir: &Path) -> Result<PathBuf> {
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

pub fn active_account_id() -> String {
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

    let project_root = run_dir
        .parent()
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("runs"))
        .and_then(Path::parent)
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some(".lab"))
        .and_then(Path::parent)
        .map(|path| path.display().to_string());

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
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;",
        )
        .context("configure sqlite pragmas")?;
        conn.execute_batch(SCHEMA_SQL)
            .context("bootstrap sqlite schema")?;
        let mut store = Self {
            conn,
            account_id: active_account_id(),
            run_id: run_id_from_dir(run_dir),
            db_path,
        };
        store.ensure_account_profile()?;
        store.register_run_location(run_dir)?;
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
        let tx = self.conn.transaction()?;
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
            "hook_events_ref",
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
               hook_events_total, has_hook_events, row_json
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
               hook_events_total=excluded.hook_events_total,
               has_hook_events=excluded.has_hook_events,
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
                as_i64(row.hook_events_total),
                if row.has_hook_events { 1_i64 } else { 0_i64 },
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
    pub fn latest_runtime_operation(&self, run_id: &str, op_kind: &str) -> Result<Option<Value>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT payload_json
                 FROM runtime_ops
                 WHERE account_id=?1 AND run_id=?2 AND op_kind=?3
                 ORDER BY updated_at_ms DESC
                 LIMIT 1",
                params![self.account_id, run_id, op_kind],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(parse_json_text).transpose()
    }

    #[cfg(test)]
    pub fn row_count(&self, table: &str) -> Result<i64> {
        let sql = format!("SELECT count(*) FROM {}", table);
        let count = self.conn.query_row(&sql, [], |row| row.get(0))?;
        Ok(count)
    }
}

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
