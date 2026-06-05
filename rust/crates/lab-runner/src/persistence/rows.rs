use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use lab_core::sha256_bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonRowTable {
    Evidence,
    ChainState,
    TrialConclusion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunManifestRecord {
    pub schema_version: String,
    pub run_id: String,
    pub created_at: String,
    pub workload_type: String,
    pub baseline_id: String,
    pub variant_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDefinitionRecord {
    pub schema_version: String,
    pub experiment_id: String,
    pub metric_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    pub source_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_pointer: Option<String>,
    pub required: bool,
    pub primary: bool,
    pub definition: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialRecord {
    pub run_id: String,
    pub trial_id: String,
    pub schedule_idx: usize,
    pub slot_commit_id: String,
    pub attempt: usize,
    pub row_seq: usize,
    pub baseline_id: String,
    pub workload_type: String,
    pub variant_id: String,
    pub task_index: usize,
    pub task_id: String,
    pub repl_idx: usize,
    pub outcome: String,
    pub success: bool,
    pub status_code: String,
    pub integration_level: String,
    pub network_mode_requested: String,
    pub network_mode_effective: String,
    pub primary_metric_name: String,
    pub primary_metric_value: Value,
    pub metrics: Value,
    pub bindings: Value,
    pub events_total: usize,
    pub has_events: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricRow {
    pub run_id: String,
    pub trial_id: String,
    pub schedule_idx: usize,
    pub slot_commit_id: String,
    pub attempt: usize,
    pub row_seq: usize,
    pub variant_id: String,
    pub task_id: String,
    pub repl_idx: usize,
    pub outcome: String,
    pub metric_name: String,
    pub metric_value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow {
    pub run_id: String,
    pub trial_id: String,
    pub schedule_idx: usize,
    pub slot_commit_id: String,
    pub attempt: usize,
    pub row_seq: usize,
    pub variant_id: String,
    pub task_id: String,
    pub repl_idx: usize,
    pub seq: usize,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractStageRow {
    pub run_id: String,
    pub trial_id: String,
    pub schedule_idx: usize,
    pub slot_commit_id: String,
    pub attempt: usize,
    pub row_seq: usize,
    pub variant_id: String,
    pub task_id: String,
    pub repl_idx: usize,
    pub stage: String,
    pub status: String,
    pub recorded_at: String,
    pub detail: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantSnapshotRow {
    pub run_id: String,
    pub trial_id: String,
    pub schedule_idx: usize,
    pub slot_commit_id: String,
    pub attempt: usize,
    pub row_seq: usize,
    pub variant_id: String,
    pub baseline_id: String,
    pub task_id: String,
    pub repl_idx: usize,
    pub binding_name: String,
    pub binding_value: Value,
    pub binding_value_text: String,
}

pub(crate) struct EvidenceAttemptObjectRef<'a> {
    pub(crate) role: &'static str,
    pub(crate) object_ref: &'a str,
}

pub(crate) struct EvidenceAttemptObjects<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) trial_id: &'a str,
    pub(crate) schedule_idx: usize,
    pub(crate) attempt: usize,
    pub(crate) refs: Vec<EvidenceAttemptObjectRef<'a>>,
}

pub(crate) fn required_json_str<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string field {}", pointer))
}

pub(crate) fn required_json_usize(value: &Value, pointer: &str) -> Result<usize> {
    let raw = value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("missing integer field {}", pointer))?;
    usize::try_from(raw).map_err(|_| anyhow!("integer field {} overflows usize", pointer))
}

pub(crate) fn optional_json_usize(value: &Value, pointer: &str) -> Result<Option<usize>> {
    let Some(raw) = value.pointer(pointer) else {
        return Ok(None);
    };
    let raw = raw
        .as_u64()
        .ok_or_else(|| anyhow!("integer field {} must be a non-negative integer", pointer))?;
    usize::try_from(raw)
        .map(Some)
        .map_err(|_| anyhow!("integer field {} overflows usize", pointer))
}

pub(crate) fn optional_json_i64(value: &Value, pointer: &str) -> Result<Option<i64>> {
    let Some(raw) = value.pointer(pointer) else {
        return Ok(None);
    };
    let raw = raw
        .as_u64()
        .ok_or_else(|| anyhow!("integer field {} must be a non-negative integer", pointer))?;
    i64::try_from(raw)
        .map(Some)
        .map_err(|_| anyhow!("integer field {} overflows i64", pointer))
}

pub(crate) fn required_json_i64(value: &Value, pointer: &str) -> Result<i64> {
    let raw = value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("missing integer field {}", pointer))?;
    i64::try_from(raw).map_err(|_| anyhow!("integer field {} overflows i64", pointer))
}

const EVIDENCE_ATTEMPT_OBJECT_ROLES: &[(&str, &str)] = &[
    ("trial_input_ref", "trial_input"),
    ("trial_output_ref", "trial_output"),
    ("events_ref", "events"),
    ("stdout_ref", "stdout"),
    ("stderr_ref", "stderr"),
    ("workspace_pre_ref", "workspace_pre"),
    ("workspace_post_ref", "workspace_post"),
    ("diff_incremental_ref", "diff_incremental"),
    ("diff_cumulative_ref", "diff_cumulative"),
    ("patch_incremental_ref", "patch_incremental"),
    ("patch_cumulative_ref", "patch_cumulative"),
    ("workspace_bundle_ref", "workspace_bundle"),
];

pub(crate) fn evidence_attempt_objects(row: &Value) -> Result<Option<EvidenceAttemptObjects<'_>>> {
    let run_id = row
        .pointer("/run_id")
        .or_else(|| row.pointer("/ids/run_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing run_id in evidence row"))?;
    let Some(trial_id) = row.pointer("/ids/trial_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(schedule_idx) = optional_json_usize(row, "/schedule_idx")? else {
        return Ok(None);
    };
    let Some(attempt) = optional_json_usize(row, "/attempt")? else {
        return Ok(None);
    };
    let Some(evidence) = row.pointer("/evidence").and_then(Value::as_object) else {
        return Ok(None);
    };
    let refs = EVIDENCE_ATTEMPT_OBJECT_ROLES
        .iter()
        .filter_map(|(field, role)| {
            evidence
                .get(*field)
                .and_then(Value::as_str)
                .map(|object_ref| EvidenceAttemptObjectRef { role, object_ref })
        })
        .collect();

    Ok(Some(EvidenceAttemptObjects {
        run_id,
        trial_id,
        schedule_idx,
        attempt,
        refs,
    }))
}

pub(crate) struct ChainStateLineage<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) trial_id: &'a str,
    pub(crate) chain_key: &'a str,
    pub(crate) step_index: usize,
    pub(crate) pre_snapshot_ref: Option<&'a str>,
    pub(crate) post_snapshot_ref: Option<&'a str>,
    pub(crate) diff_incremental_ref: Option<&'a str>,
    pub(crate) diff_cumulative_ref: Option<&'a str>,
    pub(crate) patch_incremental_ref: Option<&'a str>,
    pub(crate) patch_cumulative_ref: Option<&'a str>,
    pub(crate) workspace_ref: Option<&'a str>,
    pub(crate) checkpoint_labels: Option<&'a Value>,
}

impl ChainStateLineage<'_> {
    pub(crate) fn version_id(&self) -> String {
        sha256_bytes(
            format!(
                "{}|{}|{}|{}",
                self.run_id, self.chain_key, self.step_index, self.trial_id
            )
            .as_bytes(),
        )
    }

    pub(crate) fn checkpoint_labels_json(&self) -> Result<String> {
        match self.checkpoint_labels {
            Some(labels) => serde_json::to_string(labels).context("serialize checkpoint labels"),
            None => Ok("[]".to_string()),
        }
    }
}

pub(crate) fn chain_state_lineage(row: &Value) -> Result<ChainStateLineage<'_>> {
    let run_id = row
        .pointer("/run_id")
        .or_else(|| row.pointer("/ids/run_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing run_id in chain state row"))?;
    let trial_id = row
        .pointer("/ids/trial_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing /ids/trial_id in chain state row"))?;
    let chain_key = row
        .pointer("/chain_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing /chain_id in chain state row"))?;
    let raw_step_index = row
        .pointer("/step_index")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("missing integer field /step_index"))?;
    let step_index = usize::try_from(raw_step_index)
        .map_err(|_| anyhow!("integer field /step_index overflows usize"))?;
    let str_at = |pointer| row.pointer(pointer).and_then(Value::as_str);

    Ok(ChainStateLineage {
        run_id,
        trial_id,
        chain_key,
        step_index,
        pre_snapshot_ref: str_at("/snapshots/prev_ref"),
        post_snapshot_ref: str_at("/snapshots/post_ref"),
        diff_incremental_ref: str_at("/diffs/incremental_ref"),
        diff_cumulative_ref: str_at("/diffs/cumulative_ref"),
        patch_incremental_ref: str_at("/diffs/patch_incremental_ref"),
        patch_cumulative_ref: str_at("/diffs/patch_cumulative_ref"),
        workspace_ref: str_at("/ext/latest_workspace_ref").or_else(|| str_at("/ext/workspace_ref")),
        checkpoint_labels: row.pointer("/checkpoint_labels"),
    })
}

pub(crate) fn infer_run_dir_from_path(path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        if ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "runtime")
        {
            return ancestor.parent().map(Path::to_path_buf);
        }
        let is_run_child = ancestor
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "runs");
        if is_run_child {
            return Some(ancestor.to_path_buf());
        }
        let has_run_manifest = ancestor.join("manifest.json").exists()
            || ancestor.join("resolved_experiment.json").exists();
        #[cfg(test)]
        let has_test_account_db = ancestor
            .join(".bucephalus")
            .join("bucephalus.sqlite")
            .exists();
        #[cfg(not(test))]
        let has_test_account_db = false;
        if has_run_manifest || has_test_account_db {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

#[cfg(test)]
pub(crate) fn json_row_table_from_path(path: &Path) -> Option<JsonRowTable> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.contains("evidence") {
        return Some(JsonRowTable::Evidence);
    }
    if name.contains("task_chain") || name.contains("chain_state") {
        return Some(JsonRowTable::ChainState);
    }
    if name.contains("conclusion") {
        return Some(JsonRowTable::TrialConclusion);
    }
    None
}

#[cfg(test)]
pub(crate) fn row_has_sqlite_identity_fields(row: &Value) -> bool {
    row.pointer("/run_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        && row
            .pointer("/schedule_idx")
            .and_then(Value::as_u64)
            .is_some()
        && row.pointer("/attempt").and_then(Value::as_u64).is_some()
        && row.pointer("/row_seq").and_then(Value::as_u64).is_some()
        && row
            .pointer("/slot_commit_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
}
