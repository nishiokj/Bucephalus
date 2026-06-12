use crate::experiment::state::ScheduleSlotRecord;
use crate::model::{TrialSlot, RUNTIME_KEY_RUN_CONTROL, RUNTIME_KEY_SCHEDULE_PROGRESS};
use crate::package::validate::validate_schema_contract_value;
use crate::persistence::rows::{
    chain_state_lineage, evidence_attempt_objects, optional_json_i64, required_json_i64,
    required_json_str, required_json_usize, ContractStageRow, EventRow, JsonRowTable,
    MetricDefinitionRecord, MetricRow, RunManifestRecord, TrialRecord, VariantSnapshotRow,
};
use crate::persistence::store::{
    active_account_id, AttemptObjectUpsert, MetricDefinitionInsert, SlotCommitTransactionInput,
    TrialAttemptContainerUpsert, TrialAttemptRecord,
};
use crate::trial::state::{TrialAttemptState, TrialPhase};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use postgres::{Client, NoTls, Row};
use serde_json::Value;
use std::cell::RefCell;
use std::fs;
use std::path::Path;

const DEFAULT_SCHEMA: &str = "bucephalus_runtime";
pub(crate) const BUCEPHALUS_RUN_STORE_SCHEMA_ENV: &str = "BUCEPHALUS_RUN_STORE_SCHEMA";
const REQUIRED_TABLES: &[&str] = &[
    "account_profile",
    "runs",
    "runtime_kv",
    "run_manifests",
    "metric_definitions",
    "slot_commit_records",
    "pending_trial_completions",
    "schedule_slots",
    "trial_attempts",
    "trial_attempt_containers",
    "trial_rows",
    "metric_rows",
    "event_rows",
    "contract_stage_rows",
    "variant_snapshot_rows",
    "evidence_rows",
    "chain_state_rows",
    "trial_conclusion_rows",
    "attempt_objects",
    "lineage_versions",
    "lineage_heads",
    "runtime_ops",
    "performance_samples",
];

pub(crate) struct PostgresRunStore {
    client: RefCell<Client>,
    account_id: String,
    run_id: String,
    schema: String,
}

impl PostgresRunStore {
    pub(crate) fn open(run_dir: &Path, url: &str) -> Result<Self> {
        if !run_dir.exists() {
            fs::create_dir_all(run_dir).with_context(|| {
                format!(
                    "create run directory for postgres store: {}",
                    run_dir.display()
                )
            })?;
        }
        let schema = postgres_schema_name()?;
        let mut client = Client::connect(url, NoTls).context("connect postgres runtime store")?;
        verify_postgres_schema(&mut client, &schema)?;
        let mut store = Self {
            client: RefCell::new(client),
            account_id: active_account_id()?,
            run_id: run_id_from_dir(run_dir)?,
            schema,
        };
        store.ensure_account_profile()?;
        store.register_run_location(run_dir)?;
        Ok(store)
    }

    fn table(&self, name: &str) -> String {
        format!("{}.{}", quote_ident(&self.schema), quote_ident(name))
    }

    fn ensure_account_profile(&mut self) -> Result<()> {
        let profile = serde_json::json!({
            "schema_version": "account_profile_v1",
            "account_id": self.account_id,
        });
        let sql = format!(
            "INSERT INTO {} (account_id, profile_json, created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $3)
             ON CONFLICT(account_id) DO UPDATE SET
               profile_json=excluded.profile_json,
               updated_at_ms=excluded.updated_at_ms",
            self.table("account_profile")
        );
        self.client
            .borrow_mut()
            .execute(&sql, &[&self.account_id, &json_text(&profile)?, &now_ms()])?;
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
            "runtime_store": {
                "backend": "postgres",
                "schema": self.schema,
            },
        });
        let sql = format!(
            "INSERT INTO {} (
               account_id, run_id, experiment_id, project_root, run_dir, artifact_root,
               status, created_at_ms, updated_at_ms, manifest_json
             ) VALUES ($1, $2, $3, $4, $5, $6, 'registered', $7, $7, $8)
             ON CONFLICT(account_id, run_id) DO UPDATE SET
               experiment_id=excluded.experiment_id,
               project_root=excluded.project_root,
               run_dir=excluded.run_dir,
               artifact_root=excluded.artifact_root,
               updated_at_ms=excluded.updated_at_ms",
            self.table("runs")
        );
        self.client.borrow_mut().execute(
            &sql,
            &[
                &self.account_id,
                &self.run_id,
                &experiment_id,
                &project_root,
                &run_dir_text,
                &artifact_root,
                &now_ms(),
                &json_text(&manifest)?,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn put_runtime_json(&mut self, key: &str, value: &Value) -> Result<()> {
        validate_schema_contract_value(value, format!("runtime_kv key '{}'", key).as_str())?;
        let payload = json_text(value)?;
        let sql = format!(
            "INSERT INTO {} (account_id, run_id, key, value_json, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT(account_id, run_id, key) DO UPDATE SET
               value_json=excluded.value_json,
               updated_at_ms=excluded.updated_at_ms",
            self.table("runtime_kv")
        );
        self.client.borrow_mut().execute(
            &sql,
            &[&self.account_id, &self.run_id, &key, &payload, &now_ms()],
        )?;
        if key == RUNTIME_KEY_RUN_CONTROL {
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("run_control_v2 missing status"))?;
            let sql = format!(
                "UPDATE {} SET status=$1, updated_at_ms=$2 WHERE account_id=$3 AND run_id=$4",
                self.table("runs")
            );
            self.client
                .borrow_mut()
                .execute(&sql, &[&status, &now_ms(), &self.account_id, &self.run_id])?;
        }
        Ok(())
    }

    pub(crate) fn get_runtime_json(&self, key: &str) -> Result<Option<Value>> {
        let sql = format!(
            "SELECT value_json FROM {} WHERE account_id=$1 AND run_id=$2 AND key=$3",
            self.table("runtime_kv")
        );
        let row = self
            .client
            .borrow_mut()
            .query_opt(&sql, &[&self.account_id, &self.run_id, &key])?;
        row.map(|row| parse_json_text(row.get(0))).transpose()
    }

    pub(crate) fn ensure_schedule_slots(
        &mut self,
        run_id: &str,
        schedule: &[TrialSlot],
    ) -> Result<()> {
        let table = self.table("schedule_slots");
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        let now = now_ms();
        let sql = format!(
            "INSERT INTO {} (
               account_id, run_id, schedule_idx, state, slot_json, trial_id,
               attempt, worker_id, owner_id, lease_epoch, lease_expires_at,
               slot_commit_id, slot_status, updated_at_ms
             ) VALUES (
               $1, $2, $3, 'pending', $4, NULL,
               0, NULL, NULL, 0, NULL,
               NULL, NULL, $5
             )
             ON CONFLICT(account_id, run_id, schedule_idx) DO UPDATE SET
               slot_json=excluded.slot_json",
            table
        );
        for (schedule_idx, slot) in schedule.iter().enumerate() {
            tx.execute(
                &sql,
                &[
                    &self.account_id,
                    &run_id,
                    &as_i64(schedule_idx),
                    &json_text(&serde_json::to_value(slot)?)?,
                    &now,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn active_schedule_slots(&self, run_id: &str) -> Result<Vec<ScheduleSlotRecord>> {
        let sql = format!(
            "SELECT schedule_idx, state, slot_json, trial_id, attempt, worker_id,
                    owner_id, lease_epoch, lease_expires_at, slot_commit_id, slot_status
             FROM {}
             WHERE account_id=$1 AND run_id=$2 AND state='active'
             ORDER BY schedule_idx",
            self.table("schedule_slots")
        );
        let rows = self
            .client
            .borrow_mut()
            .query(&sql, &[&self.account_id, &run_id])?;
        rows.into_iter().map(parse_schedule_slot_record).collect()
    }

    pub(crate) fn schedule_slot(
        &self,
        run_id: &str,
        schedule_idx: usize,
    ) -> Result<Option<ScheduleSlotRecord>> {
        let sql = format!(
            "SELECT schedule_idx, state, slot_json, trial_id, attempt, worker_id,
                    owner_id, lease_epoch, lease_expires_at, slot_commit_id, slot_status
             FROM {}
             WHERE account_id=$1 AND run_id=$2 AND schedule_idx=$3",
            self.table("schedule_slots")
        );
        self.client
            .borrow_mut()
            .query_opt(&sql, &[&self.account_id, &run_id, &as_i64(schedule_idx)])?
            .map(parse_schedule_slot_record)
            .transpose()
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
        let table = self.table("schedule_slots");
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        let select_sql = format!(
            "SELECT schedule_idx, state, slot_json, trial_id, attempt, worker_id,
                    owner_id, lease_epoch, lease_expires_at, slot_commit_id, slot_status
             FROM {}
             WHERE account_id=$1 AND run_id=$2 AND schedule_idx=$3
             FOR UPDATE",
            table
        );
        let Some(row) = tx.query_opt(
            &select_sql,
            &[&self.account_id, &run_id, &as_i64(schedule_idx)],
        )?
        else {
            tx.commit()?;
            return Ok(None);
        };
        let existing = parse_schedule_slot_record(row)?;
        if existing.state != "pending" {
            tx.commit()?;
            return Ok(None);
        }
        let next_attempt = existing.attempt.saturating_add(1);
        let next_epoch = existing.lease_epoch.saturating_add(1);
        let update_sql = format!(
            "UPDATE {}
             SET state='active',
                 trial_id=$1,
                 attempt=$2,
                 worker_id=$3,
                 owner_id=$4,
                 lease_epoch=$5,
                 lease_expires_at=$6,
                 slot_commit_id=NULL,
                 slot_status=NULL,
                 updated_at_ms=$7
             WHERE account_id=$8 AND run_id=$9 AND schedule_idx=$10 AND state='pending'",
            table
        );
        let claimed = tx.execute(
            &update_sql,
            &[
                &trial_id,
                &as_i64(next_attempt),
                &worker_id,
                &owner_id,
                &i64::try_from(next_epoch).context("lease epoch must fit i64")?,
                &lease_expires_at,
                &now_ms(),
                &self.account_id,
                &run_id,
                &as_i64(schedule_idx),
            ],
        )?;
        tx.commit()?;
        if claimed != 1 {
            return Ok(None);
        }
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
        let sql = format!(
            "UPDATE {}
             SET state='committed',
                 trial_id=$1,
                 attempt=GREATEST(attempt, $2),
                 worker_id=NULL,
                 owner_id=NULL,
                 lease_expires_at=NULL,
                 slot_commit_id=$3,
                 slot_status=$4,
                 updated_at_ms=$5
             WHERE account_id=$6 AND run_id=$7 AND schedule_idx=$8
               AND state IN ('pending','active','committed','abandoned')",
            self.table("schedule_slots")
        );
        self.client.borrow_mut().execute(
            &sql,
            &[
                &trial_id,
                &as_i64(attempt),
                &slot_commit_id,
                &slot_status,
                &now_ms(),
                &self.account_id,
                &run_id,
                &as_i64(schedule_idx),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn release_schedule_slot_to_pending(
        &mut self,
        run_id: &str,
        schedule_idx: usize,
    ) -> Result<()> {
        let sql = format!(
            "UPDATE {}
             SET state='pending',
                 trial_id=NULL,
                 worker_id=NULL,
                 owner_id=NULL,
                 lease_expires_at=NULL,
                 slot_commit_id=NULL,
                 slot_status=NULL,
                 updated_at_ms=$1
             WHERE account_id=$2 AND run_id=$3 AND schedule_idx=$4
               AND state!='committed'",
            self.table("schedule_slots")
        );
        self.client.borrow_mut().execute(
            &sql,
            &[&now_ms(), &self.account_id, &run_id, &as_i64(schedule_idx)],
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
        for row in input.trial_conclusion_rows {
            validate_schema_contract_value(row, "trial conclusion row")?;
        }

        let account_id = self.account_id.clone();
        let schedule_slots = self.table("schedule_slots");
        let pending = self.table("pending_trial_completions");
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        let now = now_ms();
        if let Some(slot) = input.slot {
            let sql = format!(
                "INSERT INTO {} (
                   account_id, run_id, schedule_idx, state, slot_json, trial_id,
                   attempt, worker_id, owner_id, lease_epoch, lease_expires_at,
                   slot_commit_id, slot_status, updated_at_ms
                 ) VALUES (
                   $1, $2, $3, 'pending', $4, NULL,
                   0, NULL, NULL, 0, NULL,
                   NULL, NULL, $5
                 )
                 ON CONFLICT(account_id, run_id, schedule_idx) DO NOTHING",
                schedule_slots
            );
            tx.execute(
                &sql,
                &[
                    &account_id,
                    &input.run_id,
                    &as_i64(input.schedule_idx),
                    &json_text(&serde_json::to_value(slot)?)?,
                    &now,
                ],
            )?;
        }

        let select_sql = format!(
            "SELECT state, trial_id, slot_commit_id
             FROM {}
             WHERE account_id=$1 AND run_id=$2 AND schedule_idx=$3
             FOR UPDATE",
            schedule_slots
        );
        let Some(row) = tx.query_opt(
            &select_sql,
            &[&account_id, &input.run_id, &as_i64(input.schedule_idx)],
        )?
        else {
            return Err(anyhow!(
                "schedule_slot_missing: schedule_idx {} has no authoritative slot row",
                input.schedule_idx
            ));
        };
        let state: String = row.get(0);
        let active_trial_id: Option<String> = row.get(1);
        let existing_commit_id: Option<String> = row.get(2);
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
            upsert_trial_record_tx(&mut tx, &self.schema, &account_id, row)?;
        }
        for row in input.metric_rows {
            upsert_metric_record_tx(&mut tx, &self.schema, &account_id, row)?;
        }
        for row in input.event_rows {
            upsert_event_record_tx(&mut tx, &self.schema, &account_id, row)?;
        }
        for row in input.contract_stage_rows {
            upsert_contract_stage_record_tx(&mut tx, &self.schema, &account_id, row)?;
        }
        for row in input.variant_snapshot_rows {
            upsert_variant_snapshot_record_tx(&mut tx, &self.schema, &account_id, row)?;
        }
        for row in input.evidence_rows {
            upsert_json_row_tx(
                &mut tx,
                &self.schema,
                &account_id,
                JsonRowTable::Evidence,
                row,
            )?;
        }
        for row in input.chain_state_rows {
            upsert_json_row_tx(
                &mut tx,
                &self.schema,
                &account_id,
                JsonRowTable::ChainState,
                row,
            )?;
        }
        for row in input.trial_conclusion_rows {
            upsert_json_row_tx(
                &mut tx,
                &self.schema,
                &account_id,
                JsonRowTable::TrialConclusion,
                row,
            )?;
        }

        #[cfg(test)]
        if input.fail_after_facts {
            return Err(anyhow!("slot_commit_transaction_failpoint_after_facts"));
        }

        upsert_slot_commit_record_tx(&mut tx, &self.schema, &account_id, input.commit_record)?;
        put_runtime_json_tx(
            &mut tx,
            &self.schema,
            &account_id,
            input.run_id,
            RUNTIME_KEY_SCHEDULE_PROGRESS,
            input.schedule_progress,
        )?;
        let sql = format!(
            "DELETE FROM {} WHERE account_id=$1 AND run_id=$2 AND schedule_idx=$3",
            pending
        );
        tx.execute(
            &sql,
            &[&account_id, &input.run_id, &as_i64(input.schedule_idx)],
        )?;
        let sql = format!(
            "UPDATE {}
             SET state='committed',
                 trial_id=$1,
                 attempt=GREATEST(attempt, $2),
                 worker_id=NULL,
                 owner_id=NULL,
                 lease_expires_at=NULL,
                 slot_commit_id=$3,
                 slot_status=$4,
                 updated_at_ms=$5
             WHERE account_id=$6 AND run_id=$7 AND schedule_idx=$8
               AND state IN ('pending','active','committed')",
            schedule_slots
        );
        let updated = tx.execute(
            &sql,
            &[
                &input.trial_id,
                &as_i64(input.attempt),
                &input.slot_commit_id,
                &input.slot_status,
                &now_ms(),
                &account_id,
                &input.run_id,
                &as_i64(input.schedule_idx),
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
        let state_text = json_text(&state_json)?;
        let trial_attempts = self.table("trial_attempts");
        let containers = self.table("trial_attempt_containers");
        let account_id = self.account_id.clone();
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        let sql = format!(
            "INSERT INTO {} (
               account_id, run_id, trial_id, schedule_idx, attempt, phase, paused_from_phase,
               variant_id, task_id, repl_idx, state_json, updated_at_ms
             ) VALUES (
               $1, $2, $3, $4, $5, $6, $7,
               $8, $9, $10, $11, $12
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
            trial_attempts
        );
        tx.execute(
            &sql,
            &[
                &account_id,
                &run_id,
                &trial_id,
                &as_i64(state.key.schedule_idx),
                &as_i64(state.key.attempt),
                &phase,
                &paused_from_phase,
                &state.slot.variant_id,
                &state.slot.task_id,
                &as_i64(state.slot.repl_idx),
                &state_text,
                &now_ms(),
            ],
        )?;
        if let Some(task) = state.task_sandbox.as_ref() {
            upsert_trial_attempt_container_tx(
                &mut tx,
                &containers,
                &account_id,
                TrialAttemptContainerUpsert {
                    run_id,
                    trial_id,
                    schedule_idx: state.key.schedule_idx,
                    attempt: state.key.attempt,
                    role: "task",
                    container_id: &task.container_id,
                    image: Some(task.image.as_str()),
                    workdir: Some(task.workdir.as_str()),
                },
            )?;
        }
        if let Some(grading) = state.grading_sandbox.as_ref() {
            upsert_trial_attempt_container_tx(
                &mut tx,
                &containers,
                &account_id,
                TrialAttemptContainerUpsert {
                    run_id,
                    trial_id,
                    schedule_idx: state.key.schedule_idx,
                    attempt: state.key.attempt,
                    role: "grading",
                    container_id: &grading.container_id,
                    image: None,
                    workdir: Some(grading.workdir.as_str()),
                },
            )?;
        }
        let update_sql = format!(
            "UPDATE {}
             SET status=$1, updated_at_ms=$2
             WHERE account_id=$3 AND run_id=$4 AND trial_id=$5 AND attempt=$6
               AND role=$7 AND container_id=$8",
            containers
        );
        for cleanup in &state.cleanup.containers {
            tx.execute(
                &update_sql,
                &[
                    &cleanup.status.as_str(),
                    &now_ms(),
                    &account_id,
                    &run_id,
                    &trial_id,
                    &as_i64(state.key.attempt),
                    &cleanup.role.as_str(),
                    &cleanup.container_id.as_str(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn load_latest_trial_attempt(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<TrialAttemptRecord>> {
        let sql = format!(
            "SELECT trial_id, schedule_idx, phase, state_json
             FROM {}
             WHERE account_id=$1 AND run_id=$2 AND trial_id=$3
             ORDER BY attempt DESC
             LIMIT 1",
            self.table("trial_attempts")
        );
        self.client
            .borrow_mut()
            .query_opt(&sql, &[&self.account_id, &run_id, &trial_id])?
            .map(parse_trial_attempt_record)
            .transpose()
    }

    pub(crate) fn trial_attempt_container_ids(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Vec<String>> {
        let sql = format!(
            "SELECT container_id
             FROM {}
             WHERE account_id=$1 AND run_id=$2 AND trial_id=$3
               AND status NOT IN ('removed','killed')
               AND container_id!='host'
             ORDER BY attempt DESC, role",
            self.table("trial_attempt_containers")
        );
        let rows = self
            .client
            .borrow_mut()
            .query(&sql, &[&self.account_id, &run_id, &trial_id])?;
        let mut out = Vec::new();
        for row in rows {
            let id: String = row.get(0);
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
        let sql = format!(
            "SELECT trial_id, schedule_idx, phase, state_json
             FROM {}
             WHERE account_id=$1 AND run_id=$2
             ORDER BY schedule_idx, attempt",
            self.table("trial_attempts")
        );
        let rows = self
            .client
            .borrow_mut()
            .query(&sql, &[&self.account_id, &run_id])?;
        rows.into_iter().map(parse_trial_attempt_record).collect()
    }

    pub(crate) fn put_run_manifest(&mut self, run_id: &str, manifest: &Value) -> Result<()> {
        validate_schema_contract_value(
            manifest,
            format!("run_manifest row for run '{}'", run_id).as_str(),
        )?;
        let sql = format!(
            "INSERT INTO {} (account_id, run_id, manifest_json, updated_at_ms)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT(account_id, run_id) DO UPDATE SET
               manifest_json=excluded.manifest_json,
               updated_at_ms=excluded.updated_at_ms",
            self.table("run_manifests")
        );
        self.client.borrow_mut().execute(
            &sql,
            &[&self.account_id, &run_id, &json_text(manifest)?, &now_ms()],
        )?;
        Ok(())
    }

    pub(crate) fn upsert_metric_definition(
        &mut self,
        row: MetricDefinitionInsert<'_>,
    ) -> Result<()> {
        let sql = format!(
            "INSERT INTO {} (
               account_id, experiment_id, metric_id, semantic_key, label, value_type,
               unit, direction, source_type, source_pointer, required, primary_metric,
               definition_json, updated_at_ms
             ) VALUES (
               $1, $2, $3, $4, $5, $6,
               $7, $8, $9, $10, $11, $12,
               $13, $14
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
            self.table("metric_definitions")
        );
        self.client.borrow_mut().execute(
            &sql,
            &[
                &self.account_id,
                &row.experiment_id,
                &row.metric_id,
                &row.semantic_key,
                &row.label,
                &row.value_type,
                &row.unit,
                &row.direction,
                &row.source_type,
                &row.source_pointer,
                &(if row.required { 1_i64 } else { 0_i64 }),
                &(if row.primary { 1_i64 } else { 0_i64 }),
                &json_text(row.definition_json)?,
                &now_ms(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn upsert_slot_commit_record(&mut self, record: &Value) -> Result<()> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        upsert_slot_commit_record_tx(&mut tx, &self.schema, &self.account_id, record)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn load_slot_commit_records(&self, run_id: &str) -> Result<Vec<Value>> {
        let sql = format!(
            "SELECT record_json
             FROM {}
             WHERE account_id=$1 AND run_id=$2
             ORDER BY schedule_idx, attempt,
               CASE record_type WHEN 'intent' THEN 0 ELSE 1 END",
            self.table("slot_commit_records")
        );
        let rows = self
            .client
            .borrow_mut()
            .query(&sql, &[&self.account_id, &run_id])?;
        rows.into_iter()
            .map(|row| parse_json_text(row.get(0)))
            .collect()
    }

    pub(crate) fn first_run_id_with_slot_commits(&self) -> Result<Option<String>> {
        let sql = format!(
            "SELECT run_id FROM {} WHERE account_id=$1 ORDER BY recorded_at_ms LIMIT 1",
            self.table("slot_commit_records")
        );
        Ok(self
            .client
            .borrow_mut()
            .query_opt(&sql, &[&self.account_id])?
            .map(|row| row.get(0)))
    }

    pub(crate) fn replace_pending_trial_completions(
        &mut self,
        run_id: &str,
        rows: &[Value],
    ) -> Result<()> {
        let table = self.table("pending_trial_completions");
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        let delete_sql = format!("DELETE FROM {} WHERE account_id=$1 AND run_id=$2", table);
        tx.execute(&delete_sql, &[&self.account_id, &run_id])?;
        let insert_sql = format!(
            "INSERT INTO {}
             (account_id, run_id, schedule_idx, trial_result_json, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5)",
            table
        );
        for row in rows {
            validate_schema_contract_value(
                row,
                format!("pending_trial_completions row for run '{}'", run_id).as_str(),
            )?;
            let schedule_idx = required_json_usize(row, "/schedule_idx")?;
            let trial_result = row
                .get("trial_result")
                .ok_or_else(|| anyhow!("pending completion missing /trial_result"))?;
            tx.execute(
                &insert_sql,
                &[
                    &self.account_id,
                    &run_id,
                    &as_i64(schedule_idx),
                    &json_text(trial_result)?,
                    &now_ms(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn load_pending_trial_completions(&self, run_id: &str) -> Result<Vec<Value>> {
        let sql = format!(
            "SELECT schedule_idx, trial_result_json
             FROM {}
             WHERE account_id=$1 AND run_id=$2
             ORDER BY schedule_idx",
            self.table("pending_trial_completions")
        );
        let rows = self
            .client
            .borrow_mut()
            .query(&sql, &[&self.account_id, &run_id])?;
        rows.into_iter()
            .map(|row| {
                let schedule_idx: i64 = row.get(0);
                let trial_result_raw: String = row.get(1);
                Ok(serde_json::json!({
                    "schema_version": "pending_trial_completion_v1",
                    "schedule_idx": schedule_idx,
                    "trial_result": parse_json_text(trial_result_raw)?,
                }))
            })
            .collect()
    }

    pub(crate) fn first_run_id_with_pending_completions(&self) -> Result<Option<String>> {
        let sql = format!(
            "SELECT run_id FROM {} WHERE account_id=$1 ORDER BY updated_at_ms LIMIT 1",
            self.table("pending_trial_completions")
        );
        Ok(self
            .client
            .borrow_mut()
            .query_opt(&sql, &[&self.account_id])?
            .map(|row| row.get(0)))
    }

    pub(crate) fn upsert_attempt_object(&mut self, row: AttemptObjectUpsert<'_>) -> Result<()> {
        let sql = format!(
            "INSERT INTO {} (
               account_id, run_id, trial_id, schedule_idx, attempt, role, object_ref, metadata_json, recorded_at_ms
             ) VALUES (
               $1, $2, $3, $4, $5, $6, $7, $8, $9
             )
             ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, role) DO UPDATE SET
               object_ref=excluded.object_ref,
               metadata_json=excluded.metadata_json,
               recorded_at_ms=excluded.recorded_at_ms",
            self.table("attempt_objects")
        );
        let metadata_json = row.metadata.map(json_text).transpose()?;
        self.client.borrow_mut().execute(
            &sql,
            &[
                &self.account_id,
                &row.run_id,
                &row.trial_id,
                &as_i64(row.schedule_idx),
                &as_i64(row.attempt),
                &row.role,
                &row.object_ref,
                &metadata_json,
                &now_ms(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn latest_attempt_object_ref(
        &self,
        run_id: &str,
        trial_id: &str,
        role: &str,
    ) -> Result<Option<String>> {
        let sql = format!(
            "SELECT object_ref
             FROM {}
             WHERE account_id=$1 AND run_id=$2 AND trial_id=$3 AND role=$4
             ORDER BY attempt DESC, recorded_at_ms DESC
             LIMIT 1",
            self.table("attempt_objects")
        );
        Ok(self
            .client
            .borrow_mut()
            .query_opt(&sql, &[&self.account_id, &run_id, &trial_id, &role])?
            .map(|row| row.get(0)))
    }

    pub(crate) fn latest_lineage_version_id_for_trial(
        &self,
        run_id: &str,
        trial_id: &str,
    ) -> Result<Option<String>> {
        let sql = format!(
            "SELECT version_id
             FROM {}
             WHERE account_id=$1 AND run_id=$2 AND trial_id=$3
             ORDER BY step_index DESC
             LIMIT 1",
            self.table("lineage_versions")
        );
        Ok(self
            .client
            .borrow_mut()
            .query_opt(&sql, &[&self.account_id, &run_id, &trial_id])?
            .map(|row| row.get(0)))
    }

    pub(crate) fn lineage_workspace_ref_by_version(
        &self,
        version_id: &str,
    ) -> Result<Option<String>> {
        let sql = format!(
            "SELECT workspace_ref FROM {} WHERE account_id=$1 AND version_id=$2",
            self.table("lineage_versions")
        );
        Ok(self
            .client
            .borrow_mut()
            .query_opt(&sql, &[&self.account_id, &version_id])?
            .and_then(|row| row.get::<_, Option<String>>(0)))
    }

    pub(crate) fn upsert_runtime_operation(
        &mut self,
        run_id: &str,
        op_kind: &str,
        op_id: &str,
        payload: &Value,
    ) -> Result<()> {
        let sql = format!(
            "INSERT INTO {} (account_id, run_id, op_kind, op_id, payload_json, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT(account_id, run_id, op_kind, op_id) DO UPDATE SET
               payload_json=excluded.payload_json,
               updated_at_ms=excluded.updated_at_ms",
            self.table("runtime_ops")
        );
        self.client.borrow_mut().execute(
            &sql,
            &[
                &self.account_id,
                &run_id,
                &op_kind,
                &op_id,
                &json_text(payload)?,
                &now_ms(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn upsert_performance_sample(&mut self, payload: &Value) -> Result<()> {
        let run_id = required_json_str(payload, "/run_id")?;
        let sample_id = required_json_str(payload, "/sample_id")?;
        let trial_id = extract_str_opt(payload, "/trial_id");
        let schedule_idx = optional_json_i64(payload, "/schedule_idx")?;
        let attempt = optional_json_i64(payload, "/attempt")?;
        let sample_seq = required_json_i64(payload, "/sample_seq")?;
        let sample_kind = required_json_str(payload, "/sample_kind")?;
        let stage = required_json_str(payload, "/stage")?;
        let duration_ms = payload.pointer("/duration_ms").and_then(Value::as_f64);
        let process_rss_kb = payload.pointer("/process_rss_kb").and_then(Value::as_i64);
        let recorded_at_ms = payload
            .pointer("/recorded_at_ms")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("performance sample missing /recorded_at_ms"))?;
        let sql = format!(
            "INSERT INTO {} (
               account_id, run_id, sample_id, trial_id, schedule_idx, attempt,
               sample_seq, sample_kind, stage, duration_ms, process_rss_kb,
               payload_json, recorded_at_ms
             ) VALUES (
               $1, $2, $3, $4, $5, $6,
               $7, $8, $9, $10, $11,
               $12, $13
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
            self.table("performance_samples")
        );
        self.client.borrow_mut().execute(
            &sql,
            &[
                &self.account_id,
                &run_id,
                &sample_id,
                &trial_id,
                &schedule_idx,
                &attempt,
                &sample_seq,
                &sample_kind,
                &stage,
                &duration_ms,
                &process_rss_kb,
                &json_text(payload)?,
                &recorded_at_ms,
            ],
        )?;
        Ok(())
    }
}

impl crate::persistence::journal::RunSink for PostgresRunStore {
    fn write_run_manifest(&mut self, run: &RunManifestRecord) -> Result<()> {
        let payload = serde_json::to_value(run)?;
        validate_schema_contract_value(&payload, "run manifest row")?;
        self.put_run_manifest(&run.run_id, &payload)
    }

    fn write_metric_definitions(&mut self, rows: &[MetricDefinitionRecord]) -> Result<()> {
        for row in rows {
            self.upsert_metric_definition(MetricDefinitionInsert {
                experiment_id: &row.experiment_id,
                metric_id: &row.metric_id,
                semantic_key: row.semantic_key.as_deref(),
                label: row.label.as_deref(),
                value_type: row.value_type.as_deref(),
                unit: row.unit.as_deref(),
                direction: row.direction.as_deref(),
                source_type: &row.source_type,
                source_pointer: row.source_pointer.as_deref(),
                required: row.required,
                primary: row.primary,
                definition_json: &row.definition,
            })?;
        }
        Ok(())
    }

    fn append_trial_record(&mut self, row: &TrialRecord) -> Result<()> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        upsert_trial_record_tx(&mut tx, &self.schema, &self.account_id, row)?;
        tx.commit()?;
        Ok(())
    }

    fn append_metric_rows(&mut self, rows: &[MetricRow]) -> Result<()> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        for row in rows {
            upsert_metric_record_tx(&mut tx, &self.schema, &self.account_id, row)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn append_event_rows(&mut self, rows: &[EventRow]) -> Result<()> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        for row in rows {
            upsert_event_record_tx(&mut tx, &self.schema, &self.account_id, row)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn append_contract_stage_rows(&mut self, rows: &[ContractStageRow]) -> Result<()> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        for row in rows {
            upsert_contract_stage_record_tx(&mut tx, &self.schema, &self.account_id, row)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn append_variant_snapshot(&mut self, rows: &[VariantSnapshotRow]) -> Result<()> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        for row in rows {
            upsert_variant_snapshot_record_tx(&mut tx, &self.schema, &self.account_id, row)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

fn verify_postgres_schema(client: &mut Client, schema: &str) -> Result<()> {
    validate_identifier(schema)?;
    for table_name in REQUIRED_TABLES {
        let qualified = format!("{}.{}", quote_ident(schema), quote_ident(table_name));
        let exists: Option<String> = client
            .query_one("SELECT to_regclass($1)::text", &[&qualified])?
            .get(0);
        if exists.is_none() {
            return Err(anyhow!(
                "postgres runtime schema '{}' is not initialized: missing table '{}'. Run cloud database migrations with admin credentials before starting workers.",
                schema,
                table_name
            ));
        }
    }
    Ok(())
}

fn put_runtime_json_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    run_id: &str,
    key: &str,
    value: &Value,
) -> Result<()> {
    validate_schema_contract_value(value, format!("runtime_kv key '{}'", key).as_str())?;
    let sql = format!(
        "INSERT INTO {} (account_id, run_id, key, value_json, updated_at_ms)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT(account_id, run_id, key) DO UPDATE SET
           value_json=excluded.value_json,
           updated_at_ms=excluded.updated_at_ms",
        table(schema, "runtime_kv")
    );
    tx.execute(
        &sql,
        &[&account_id, &run_id, &key, &json_text(value)?, &now_ms()],
    )?;
    Ok(())
}

fn upsert_trial_record_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    row: &TrialRecord,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {} (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           baseline_id, workload_type, variant_id, task_id, repl_idx, outcome,
           primary_metric_name, primary_metric_value_json, metrics_json, bindings_json,
           events_total, has_events, row_json
         ) VALUES (
           $1, $2, $3, $4, $5, $6, $7,
           $8, $9, $10, $11, $12, $13,
           $14, $15, $16, $17,
           $18, $19, $20
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
        table(schema, "trial_rows")
    );
    tx.execute(
        &sql,
        &[
            &account_id,
            &row.run_id,
            &row.trial_id,
            &as_i64(row.schedule_idx),
            &as_i64(row.attempt),
            &as_i64(row.row_seq),
            &row.slot_commit_id,
            &row.baseline_id,
            &row.workload_type,
            &row.variant_id,
            &row.task_id,
            &as_i64(row.repl_idx),
            &row.outcome,
            &row.primary_metric_name,
            &json_text(&row.primary_metric_value)?,
            &json_text(&row.metrics)?,
            &json_text(&row.bindings)?,
            &as_i64(row.events_total),
            &(if row.has_events { 1_i64 } else { 0_i64 }),
            &json_text(&serde_json::to_value(row)?)?,
        ],
    )?;
    Ok(())
}

fn upsert_metric_record_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    row: &MetricRow,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {} (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           variant_id, task_id, repl_idx, outcome,
           metric_name, metric_value_json, metric_source, row_json
         ) VALUES (
           $1, $2, $3, $4, $5, $6, $7,
           $8, $9, $10, $11,
           $12, $13, $14, $15
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
        table(schema, "metric_rows")
    );
    tx.execute(
        &sql,
        &[
            &account_id,
            &row.run_id,
            &row.trial_id,
            &as_i64(row.schedule_idx),
            &as_i64(row.attempt),
            &as_i64(row.row_seq),
            &row.slot_commit_id,
            &row.variant_id,
            &row.task_id,
            &as_i64(row.repl_idx),
            &row.outcome,
            &row.metric_name,
            &json_text(&row.metric_value)?,
            &row.metric_source,
            &json_text(&serde_json::to_value(row)?)?,
        ],
    )?;
    Ok(())
}

fn upsert_event_record_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    row: &EventRow,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {} (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           variant_id, task_id, repl_idx, seq, event_type, ts, payload_json, row_json
         ) VALUES (
           $1, $2, $3, $4, $5, $6, $7,
           $8, $9, $10, $11, $12, $13, $14, $15
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
        table(schema, "event_rows")
    );
    tx.execute(
        &sql,
        &[
            &account_id,
            &row.run_id,
            &row.trial_id,
            &as_i64(row.schedule_idx),
            &as_i64(row.attempt),
            &as_i64(row.row_seq),
            &row.slot_commit_id,
            &row.variant_id,
            &row.task_id,
            &as_i64(row.repl_idx),
            &as_i64(row.seq),
            &row.event_type,
            &row.ts,
            &json_text(&row.payload)?,
            &json_text(&serde_json::to_value(row)?)?,
        ],
    )?;
    Ok(())
}

fn upsert_contract_stage_record_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    row: &ContractStageRow,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {} (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           variant_id, task_id, repl_idx, stage, status, recorded_at, detail_json, row_json
         ) VALUES (
           $1, $2, $3, $4, $5, $6, $7,
           $8, $9, $10, $11, $12, $13, $14, $15
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
        table(schema, "contract_stage_rows")
    );
    tx.execute(
        &sql,
        &[
            &account_id,
            &row.run_id,
            &row.trial_id,
            &as_i64(row.schedule_idx),
            &as_i64(row.attempt),
            &as_i64(row.row_seq),
            &row.slot_commit_id,
            &row.variant_id,
            &row.task_id,
            &as_i64(row.repl_idx),
            &row.stage,
            &row.status,
            &row.recorded_at,
            &json_text(&row.detail)?,
            &json_text(&serde_json::to_value(row)?)?,
        ],
    )?;
    Ok(())
}

fn upsert_variant_snapshot_record_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    row: &VariantSnapshotRow,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {} (
           account_id, run_id, trial_id, schedule_idx, attempt, row_seq, slot_commit_id,
           variant_id, baseline_id, task_id, repl_idx, binding_name,
           binding_value_json, binding_value_text, row_json
         ) VALUES (
           $1, $2, $3, $4, $5, $6, $7,
           $8, $9, $10, $11, $12,
           $13, $14, $15
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
        table(schema, "variant_snapshot_rows")
    );
    tx.execute(
        &sql,
        &[
            &account_id,
            &row.run_id,
            &row.trial_id,
            &as_i64(row.schedule_idx),
            &as_i64(row.attempt),
            &as_i64(row.row_seq),
            &row.slot_commit_id,
            &row.variant_id,
            &row.baseline_id,
            &row.task_id,
            &as_i64(row.repl_idx),
            &row.binding_name,
            &json_text(&row.binding_value)?,
            &row.binding_value_text,
            &json_text(&serde_json::to_value(row)?)?,
        ],
    )?;
    Ok(())
}

fn upsert_slot_commit_record_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    record: &Value,
) -> Result<()> {
    let run_id = required_json_str(record, "/run_id")?;
    let schedule_idx = required_json_usize(record, "/schedule_idx")?;
    let attempt = required_json_usize(record, "/attempt")?;
    let record_type = required_json_str(record, "/record_type")?;
    let slot_commit_id = required_json_str(record, "/slot_commit_id")?;
    let sql = format!(
        "INSERT INTO {}
         (account_id, run_id, schedule_idx, attempt, record_type, slot_commit_id, record_json, recorded_at_ms)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT(account_id, run_id, schedule_idx, attempt, record_type) DO UPDATE SET
           slot_commit_id=excluded.slot_commit_id,
           record_json=excluded.record_json,
           recorded_at_ms=excluded.recorded_at_ms",
        table(schema, "slot_commit_records")
    );
    tx.execute(
        &sql,
        &[
            &account_id,
            &run_id,
            &as_i64(schedule_idx),
            &as_i64(attempt),
            &record_type,
            &slot_commit_id,
            &json_text(record)?,
            &now_ms(),
        ],
    )?;
    Ok(())
}

fn upsert_json_row_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    table_kind: JsonRowTable,
    row: &Value,
) -> Result<()> {
    let run_id = required_json_str(row, "/run_id")?;
    let schedule_idx = required_json_usize(row, "/schedule_idx")?;
    let attempt = required_json_usize(row, "/attempt")?;
    let row_seq = required_json_usize(row, "/row_seq")?;
    let slot_commit_id = required_json_str(row, "/slot_commit_id")?;
    let table_name = match table_kind {
        JsonRowTable::Evidence => "evidence_rows",
        JsonRowTable::ChainState => "chain_state_rows",
        JsonRowTable::TrialConclusion => "trial_conclusion_rows",
    };
    let sql = format!(
        "INSERT INTO {}
         (account_id, run_id, schedule_idx, attempt, row_seq, slot_commit_id, row_json)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT(account_id, run_id, schedule_idx, attempt, row_seq) DO UPDATE SET
           slot_commit_id=excluded.slot_commit_id,
           row_json=excluded.row_json",
        table(schema, table_name)
    );
    tx.execute(
        &sql,
        &[
            &account_id,
            &run_id,
            &as_i64(schedule_idx),
            &as_i64(attempt),
            &as_i64(row_seq),
            &slot_commit_id,
            &json_text(row)?,
        ],
    )?;
    match table_kind {
        JsonRowTable::Evidence => {
            upsert_attempt_objects_from_evidence_row_tx(tx, schema, account_id, row)?;
        }
        JsonRowTable::ChainState => {
            upsert_lineage_from_chain_state_row_tx(tx, schema, account_id, row)?;
        }
        JsonRowTable::TrialConclusion => {}
    }
    Ok(())
}

fn upsert_trial_attempt_container_tx(
    tx: &mut postgres::Transaction<'_>,
    containers_table: &str,
    account_id: &str,
    row: TrialAttemptContainerUpsert<'_>,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {} (
           account_id, run_id, trial_id, schedule_idx, attempt, role, container_id,
           status, image, workdir, updated_at_ms
         ) VALUES (
           $1, $2, $3, $4, $5, $6, $7,
           'running', $8, $9, $10
         )
         ON CONFLICT(account_id, run_id, trial_id, attempt, role, container_id) DO UPDATE SET
           schedule_idx=excluded.schedule_idx,
           status='running',
           image=excluded.image,
           workdir=excluded.workdir,
           updated_at_ms=excluded.updated_at_ms",
        containers_table
    );
    tx.execute(
        &sql,
        &[
            &account_id,
            &row.run_id,
            &row.trial_id,
            &as_i64(row.schedule_idx),
            &as_i64(row.attempt),
            &row.role,
            &row.container_id,
            &row.image,
            &row.workdir,
            &now_ms(),
        ],
    )?;
    Ok(())
}

fn upsert_attempt_objects_from_evidence_row_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    row: &Value,
) -> Result<()> {
    let Some(objects) = evidence_attempt_objects(row)? else {
        return Ok(());
    };
    for object in objects.refs {
        upsert_attempt_object_tx(
            tx,
            schema,
            account_id,
            AttemptObjectUpsert {
                run_id: objects.run_id,
                trial_id: objects.trial_id,
                schedule_idx: objects.schedule_idx,
                attempt: objects.attempt,
                role: object.role,
                object_ref: object.object_ref,
                metadata: Some(row),
            },
        )?;
    }
    Ok(())
}

fn upsert_attempt_object_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    row: AttemptObjectUpsert<'_>,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {} (
           account_id, run_id, trial_id, schedule_idx, attempt, role, object_ref, metadata_json, recorded_at_ms
         ) VALUES (
           $1, $2, $3, $4, $5, $6, $7, $8, $9
         )
         ON CONFLICT(account_id, run_id, trial_id, schedule_idx, attempt, role) DO UPDATE SET
           object_ref=excluded.object_ref,
           metadata_json=excluded.metadata_json,
           recorded_at_ms=excluded.recorded_at_ms",
        table(schema, "attempt_objects")
    );
    let metadata_json = row.metadata.map(json_text).transpose()?;
    tx.execute(
        &sql,
        &[
            &account_id,
            &row.run_id,
            &row.trial_id,
            &as_i64(row.schedule_idx),
            &as_i64(row.attempt),
            &row.role,
            &row.object_ref,
            &metadata_json,
            &now_ms(),
        ],
    )?;
    Ok(())
}

fn upsert_lineage_from_chain_state_row_tx(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    account_id: &str,
    row: &Value,
) -> Result<()> {
    let lineage = chain_state_lineage(row)?;
    let version_id = lineage.version_id();
    let head_sql = format!(
        "SELECT latest_version_id FROM {}
         WHERE account_id=$1 AND run_id=$2 AND chain_key=$3",
        table(schema, "lineage_heads")
    );
    let parent_version_id: Option<String> = tx
        .query_opt(
            &head_sql,
            &[&account_id, &lineage.run_id, &lineage.chain_key],
        )?
        .map(|row| row.get(0));
    let versions_sql = format!(
        "INSERT INTO {} (
           account_id, version_id, run_id, chain_key, step_index, trial_id, parent_version_id,
           pre_snapshot_ref, post_snapshot_ref,
           diff_incremental_ref, diff_cumulative_ref,
           patch_incremental_ref, patch_cumulative_ref,
           workspace_ref, checkpoint_labels_json
         ) VALUES (
           $1, $2, $3, $4, $5, $6, $7,
           $8, $9, $10, $11, $12, $13, $14, $15
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
        table(schema, "lineage_versions")
    );
    tx.execute(
        &versions_sql,
        &[
            &account_id,
            &version_id,
            &lineage.run_id,
            &lineage.chain_key,
            &as_i64(lineage.step_index),
            &lineage.trial_id,
            &parent_version_id,
            &lineage.pre_snapshot_ref,
            &lineage.post_snapshot_ref,
            &lineage.diff_incremental_ref,
            &lineage.diff_cumulative_ref,
            &lineage.patch_incremental_ref,
            &lineage.patch_cumulative_ref,
            &lineage.workspace_ref,
            &lineage.checkpoint_labels_json()?,
        ],
    )?;
    let heads_sql = format!(
        "INSERT INTO {} (account_id, run_id, chain_key, latest_version_id, step_index, latest_workspace_ref)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT(account_id, run_id, chain_key) DO UPDATE SET
           latest_version_id=excluded.latest_version_id,
           step_index=excluded.step_index,
           latest_workspace_ref=excluded.latest_workspace_ref",
        table(schema, "lineage_heads")
    );
    tx.execute(
        &heads_sql,
        &[
            &account_id,
            &lineage.run_id,
            &lineage.chain_key,
            &version_id,
            &as_i64(lineage.step_index),
            &lineage.workspace_ref,
        ],
    )?;
    Ok(())
}

fn parse_schedule_slot_record(row: Row) -> Result<ScheduleSlotRecord> {
    let slot_json: String = row.get(2);
    let slot: TrialSlot = serde_json::from_str(&slot_json).context("parse schedule slot json")?;
    Ok(ScheduleSlotRecord {
        schedule_idx: postgres_usize(row.get(0), "schedule_slots.schedule_idx")?,
        state: row.get(1),
        slot,
        trial_id: row.get(3),
        attempt: postgres_usize(row.get(4), "schedule_slots.attempt")?,
        worker_id: row.get(5),
        owner_id: row.get(6),
        lease_epoch: postgres_u64(row.get(7), "schedule_slots.lease_epoch")?,
        lease_expires_at: row.get(8),
        slot_commit_id: row.get(9),
        slot_status: row.get(10),
    })
}

fn parse_trial_attempt_record(row: Row) -> Result<TrialAttemptRecord> {
    let phase_text: String = row.get(2);
    let state_json: String = row.get(3);
    Ok(TrialAttemptRecord {
        trial_id: row.get(0),
        schedule_idx: postgres_usize(row.get(1), "trial_attempts.schedule_idx")?,
        phase: trial_phase_from_text(&phase_text)?,
        state: serde_json::from_str(&state_json).context("parse trial attempt state json")?,
    })
}

pub(crate) fn postgres_schema_name() -> Result<String> {
    let raw = match std::env::var(BUCEPHALUS_RUN_STORE_SCHEMA_ENV) {
        Ok(raw) => raw,
        Err(std::env::VarError::NotPresent) => DEFAULT_SCHEMA.to_string(),
        Err(err) => {
            return Err(anyhow!(
                "failed reading {}: {}",
                BUCEPHALUS_RUN_STORE_SCHEMA_ENV,
                err
            ))
        }
    };
    let value = raw.trim();
    let value = if value.is_empty() {
        DEFAULT_SCHEMA
    } else {
        value
    };
    validate_identifier(value)?;
    Ok(value.to_string())
}

pub(crate) fn quote_ident(value: &str) -> String {
    validate_identifier(value).expect("validated postgres identifier");
    format!("\"{}\"", value)
}

fn table(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(name))
}

fn validate_identifier(value: &str) -> Result<()> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(anyhow!("postgres identifier must not be empty"));
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(anyhow!("invalid postgres identifier '{}'", value));
    }
    if !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        return Err(anyhow!("invalid postgres identifier '{}'", value));
    }
    Ok(())
}

fn run_id_from_dir(run_dir: &Path) -> Result<String> {
    run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("unable to infer run_id from {}", run_dir.display()))
}

fn registry_metadata_from_run_dir(run_dir: &Path) -> Result<(Option<String>, Option<String>)> {
    let resolved_path = run_dir.join("resolved_experiment.json");
    let experiment_id = if resolved_path.exists() {
        let raw = fs::read_to_string(&resolved_path)
            .with_context(|| format!("failed to read {}", resolved_path.display()))?;
        let value: Value = serde_json::from_str(&raw)
            .with_context(|| format!("invalid JSON in {}", resolved_path.display()))?;
        Some(crate::config::required_experiment_id(&value)?)
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

fn as_i64(value: usize) -> i64 {
    i64::try_from(value).expect("persistence index must fit i64")
}

fn postgres_usize(value: i64, field: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| anyhow!("{} must fit usize", field))
}

fn postgres_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| anyhow!("{} must be non-negative", field))
}

fn json_text(value: &Value) -> Result<String> {
    serde_json::to_string(value).context("serialize json")
}

fn parse_json_text(raw: String) -> Result<Value> {
    serde_json::from_str(&raw).context("parse json")
}

fn extract_str_opt(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn trial_phase_text(phase: &TrialPhase) -> Result<String> {
    serde_json::to_value(phase)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("trial phase did not serialize to string"))
}

fn trial_phase_from_text(value: &str) -> Result<TrialPhase> {
    serde_json::from_value(Value::String(value.to_string())).context("parse trial phase")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::journal::RunSink;
    use serde_json::json;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct EnvGuard {
        key: &'static str,
        value: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let guard = Self {
                key,
                value: std::env::var(key).ok(),
            };
            std::env::set_var(key, value);
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.value {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn cleanup_runtime_run(store: &mut PostgresRunStore, run_id: &str) -> Result<()> {
        let account_id = store.account_id.clone();
        for table_name in [
            "runtime_kv",
            "run_manifests",
            "slot_commit_records",
            "pending_trial_completions",
            "schedule_slots",
            "trial_attempt_containers",
            "trial_attempts",
            "trial_rows",
            "metric_rows",
            "event_rows",
            "contract_stage_rows",
            "variant_snapshot_rows",
            "evidence_rows",
            "chain_state_rows",
            "trial_conclusion_rows",
            "attempt_objects",
            "lineage_versions",
            "lineage_heads",
            "runtime_ops",
            "performance_samples",
            "runs",
        ] {
            let sql = format!(
                "DELETE FROM {} WHERE account_id=$1 AND run_id=$2",
                table(&store.schema, table_name)
            );
            store
                .client
                .borrow_mut()
                .execute(&sql, &[&account_id, &run_id])?;
        }
        Ok(())
    }

    #[test]
    fn postgres_identifier_validation_rejects_unsafe_schema_names() {
        assert!(validate_identifier("bucephalus_runtime").is_ok());
        assert!(validate_identifier("_runtime1").is_ok());
        assert!(validate_identifier("bad-name").is_err());
        assert!(validate_identifier("1bad").is_err());
        assert!(validate_identifier("runtime;drop").is_err());
    }

    #[test]
    fn postgres_store_round_trip_when_configured() -> Result<()> {
        let Some(url) = std::env::var("BUCEPHALUS_TEST_POSTGRES_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(());
        };
        let _lock = env_lock();
        let schema = DEFAULT_SCHEMA.to_string();
        let _schema_guard = EnvGuard::set(BUCEPHALUS_RUN_STORE_SCHEMA_ENV, &schema);
        let run_dir = std::env::temp_dir().join(format!(
            "bucephalus_pg_roundtrip_{}_{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        let run_id = run_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap()
            .to_string();
        {
            let mut store = PostgresRunStore::open(&run_dir, &url)?;
            store.put_runtime_json(
                RUNTIME_KEY_RUN_CONTROL,
                &json!({
                    "schema_version": "run_control_v2",
                    "run_id": run_id,
                    "status": "running",
                    "active_trials": [],
                    "pause": null,
                    "updated_at": Utc::now().to_rfc3339(),
                }),
            )?;
            let loaded = store.get_runtime_json(RUNTIME_KEY_RUN_CONTROL)?;
            assert_eq!(
                loaded
                    .as_ref()
                    .and_then(|value| value.pointer("/status"))
                    .and_then(Value::as_str),
                Some("running")
            );

            let schedule = vec![TrialSlot {
                variant_idx: 0,
                task_idx: 1,
                repl_idx: 0,
            }];
            store.ensure_schedule_slots(&run_id, &schedule)?;
            let claimed = store
                .claim_schedule_slot(&run_id, 0, "trial_1", "worker_1", "owner_1", None)?
                .unwrap();
            assert_eq!(claimed.state, "active");
            assert_eq!(claimed.attempt, 1);

            store.append_event_rows(&[EventRow {
                run_id: run_id.clone(),
                trial_id: "trial_1".to_string(),
                schedule_idx: 0,
                slot_commit_id: String::new(),
                attempt: 1,
                row_seq: 0,
                variant_id: "variant_1".to_string(),
                task_id: "task_1".to_string(),
                repl_idx: 0,
                seq: 0,
                event_type: "test.event".to_string(),
                ts: None,
                payload: json!({"ok": true}),
            }])?;
            let event_count: i64 = store
                .client
                .borrow_mut()
                .query_one(
                    &format!(
                        "SELECT count(*) FROM {} WHERE run_id=$1",
                        table(&schema, "event_rows")
                    ),
                    &[&run_id],
                )?
                .get(0);
            assert_eq!(event_count, 1);
            cleanup_runtime_run(&mut store, &run_id)?;
        }
        let _ = fs::remove_dir_all(&run_dir);
        Ok(())
    }

    #[test]
    fn postgres_store_open_requires_initialized_schema() -> Result<()> {
        let Some(url) = std::env::var("BUCEPHALUS_TEST_POSTGRES_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(());
        };
        let _lock = env_lock();
        let schema = format!(
            "bucephalus_test_missing_{}_{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        );
        let _schema_guard = EnvGuard::set(BUCEPHALUS_RUN_STORE_SCHEMA_ENV, &schema);
        let run_dir = std::env::temp_dir().join(format!("bucephalus_pg_missing_{}", schema));

        let err = match PostgresRunStore::open(&run_dir, &url) {
            Ok(_) => panic!("runtime open must not create postgres schema"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("postgres runtime schema")
                && err.to_string().contains("is not initialized")
        );

        let created: Option<String> = Client::connect(&url, NoTls)?
            .query_one("SELECT to_regnamespace($1)::text", &[&schema])?
            .get(0);
        assert_eq!(created, None);
        let _ = fs::remove_dir_all(&run_dir);
        Ok(())
    }

    #[test]
    fn postgres_store_works_with_restricted_runtime_role_when_migrated() -> Result<()> {
        let Some(url) = std::env::var("BUCEPHALUS_TEST_POSTGRES_RUNTIME_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(());
        };
        let _lock = env_lock();
        let schema = DEFAULT_SCHEMA.to_string();
        let _schema_guard = EnvGuard::set(BUCEPHALUS_RUN_STORE_SCHEMA_ENV, &schema);
        let run_dir = std::env::temp_dir().join(format!(
            "bucephalus_pg_runtime_role_{}_{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        let run_id = run_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap()
            .to_string();

        let mut store = PostgresRunStore::open(&run_dir, &url)?;
        let can_create: bool = store
            .client
            .borrow_mut()
            .query_one(
                "SELECT has_schema_privilege(current_user, $1, 'CREATE')",
                &[&schema],
            )?
            .get(0);
        assert!(!can_create);
        store.put_runtime_json(
            RUNTIME_KEY_RUN_CONTROL,
            &json!({
                "schema_version": "run_control_v2",
                "run_id": run_id,
                "status": "running",
                "active_trials": [],
                "pause": null,
                "updated_at": Utc::now().to_rfc3339(),
            }),
        )?;
        assert!(store.get_runtime_json(RUNTIME_KEY_RUN_CONTROL)?.is_some());
        let cleanup_sql = format!(
            "DELETE FROM {} WHERE account_id=$1 AND run_id=$2",
            table(&schema, "runtime_kv")
        );
        store
            .client
            .borrow_mut()
            .execute(&cleanup_sql, &[&store.account_id, &run_id])?;
        let cleanup_sql = format!(
            "DELETE FROM {} WHERE account_id=$1 AND run_id=$2",
            table(&schema, "runs")
        );
        store
            .client
            .borrow_mut()
            .execute(&cleanup_sql, &[&store.account_id, &run_id])?;
        let _ = fs::remove_dir_all(&run_dir);
        Ok(())
    }

    #[test]
    fn postgres_slot_commit_transaction_rolls_back_fact_rows() -> Result<()> {
        let Some(url) = std::env::var("BUCEPHALUS_TEST_POSTGRES_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(());
        };
        let _lock = env_lock();
        let schema = DEFAULT_SCHEMA.to_string();
        let _schema_guard = EnvGuard::set(BUCEPHALUS_RUN_STORE_SCHEMA_ENV, &schema);
        let run_dir = std::env::temp_dir().join(format!(
            "bucephalus_pg_rollback_{}_{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        let run_id = run_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap()
            .to_string();

        {
            let mut store = PostgresRunStore::open(&run_dir, &url)?;
            let schedule = vec![TrialSlot {
                variant_idx: 0,
                task_idx: 1,
                repl_idx: 0,
            }];
            store.ensure_schedule_slots(&run_id, &schedule)?;
            store
                .claim_schedule_slot(&run_id, 0, "trial_1", "worker_1", "owner_1", None)?
                .unwrap();

            let slot_commit_id = "slot_atomic_rollback";
            let commit_record = json!({
                "schema_version": "slot_commit_record_v1",
                "record_type": "commit",
                "run_id": run_id,
                "schedule_idx": 0,
                "slot_commit_id": slot_commit_id,
                "trial_id": "trial_1",
                "slot_status": "completed",
                "attempt": 1,
                "recorded_at": Utc::now().to_rfc3339(),
                "written_rows": {
                    "trials": 1,
                    "metrics": 0,
                    "events": 0,
                    "contract_stages": 0,
                    "variant_snapshots": 0,
                    "evidence": 0,
                    "chain_states": 0,
                    "conclusions": 0,
                    "predictions": 0,
                    "scores": 0
                },
                "facts_fsync_completed": true,
                "runtime_fsync_completed": true
            });
            let schedule_progress = json!({
                "schema_version": "schedule_progress_v2",
                "run_id": run_id,
                "total_slots": 1,
                "next_schedule_index": 1,
                "next_trial_index": 1,
                "schedule": schedule,
                "completed_slots": [{
                    "schedule_index": 0,
                    "trial_id": "trial_1",
                    "status": "completed",
                    "slot_commit_id": slot_commit_id,
                    "attempt": 1
                }],
                "pruned_variants": [],
                "consecutive_failures": {},
                "updated_at": Utc::now().to_rfc3339()
            });
            let trial_rows = vec![TrialRecord {
                run_id: run_id.clone(),
                trial_id: "trial_1".to_string(),
                schedule_idx: 0,
                slot_commit_id: slot_commit_id.to_string(),
                attempt: 1,
                row_seq: 0,
                baseline_id: "base".to_string(),
                workload_type: "agent_harness".to_string(),
                variant_id: "base".to_string(),
                task_index: 0,
                task_id: "task_1".to_string(),
                repl_idx: 0,
                outcome: "success".to_string(),
                success: true,
                status_code: "completed".to_string(),
                integration_level: "test".to_string(),
                network_mode_requested: "none".to_string(),
                network_mode_effective: "none".to_string(),
                primary_metric_name: "score".to_string(),
                primary_metric_value: json!(1),
                metrics: json!({"score": 1}),
                bindings: json!({}),
                events_total: 0,
                has_events: false,
            }];

            let err = store
                .commit_schedule_slot_transaction(SlotCommitTransactionInput {
                    run_id: &run_id,
                    schedule_idx: 0,
                    slot: Some(&schedule[0]),
                    trial_id: "trial_1",
                    attempt: 1,
                    slot_commit_id,
                    slot_status: "completed",
                    commit_record: &commit_record,
                    schedule_progress: &schedule_progress,
                    trial_rows: &trial_rows,
                    metric_rows: &[],
                    event_rows: &[],
                    contract_stage_rows: &[],
                    variant_snapshot_rows: &[],
                    evidence_rows: &[],
                    chain_state_rows: &[],
                    trial_conclusion_rows: &[],
                    fail_after_facts: true,
                })
                .expect_err("failpoint should roll back transaction");
            assert!(err
                .to_string()
                .contains("slot_commit_transaction_failpoint_after_facts"));

            let trial_count: i64 = store
                .client
                .borrow_mut()
                .query_one(
                    &format!(
                        "SELECT count(*) FROM {} WHERE run_id=$1",
                        table(&schema, "trial_rows")
                    ),
                    &[&run_id],
                )?
                .get(0);
            assert_eq!(trial_count, 0);
            assert!(store.load_slot_commit_records(&run_id)?.is_empty());
            assert!(store
                .get_runtime_json(RUNTIME_KEY_SCHEDULE_PROGRESS)?
                .is_none());
            let slot = store.schedule_slot(&run_id, 0)?.unwrap();
            assert_eq!(slot.state, "active");
            assert_eq!(slot.trial_id.as_deref(), Some("trial_1"));
            assert_eq!(slot.slot_commit_id, None);

            store.commit_schedule_slot_transaction(SlotCommitTransactionInput {
                run_id: &run_id,
                schedule_idx: 0,
                slot: Some(&schedule[0]),
                trial_id: "trial_1",
                attempt: 1,
                slot_commit_id,
                slot_status: "completed",
                commit_record: &commit_record,
                schedule_progress: &schedule_progress,
                trial_rows: &trial_rows,
                metric_rows: &[],
                event_rows: &[],
                contract_stage_rows: &[],
                variant_snapshot_rows: &[],
                evidence_rows: &[],
                chain_state_rows: &[],
                trial_conclusion_rows: &[],
                fail_after_facts: false,
            })?;

            let trial_count: i64 = store
                .client
                .borrow_mut()
                .query_one(
                    &format!(
                        "SELECT count(*) FROM {} WHERE run_id=$1",
                        table(&schema, "trial_rows")
                    ),
                    &[&run_id],
                )?
                .get(0);
            assert_eq!(trial_count, 1);
            assert_eq!(store.load_slot_commit_records(&run_id)?.len(), 1);
            assert!(store
                .get_runtime_json(RUNTIME_KEY_SCHEDULE_PROGRESS)?
                .is_some());
            let slot = store.schedule_slot(&run_id, 0)?.unwrap();
            assert_eq!(slot.state, "committed");
            assert_eq!(slot.slot_commit_id.as_deref(), Some(slot_commit_id));
            cleanup_runtime_run(&mut store, &run_id)?;
        }

        let _ = fs::remove_dir_all(&run_dir);
        Ok(())
    }
}
