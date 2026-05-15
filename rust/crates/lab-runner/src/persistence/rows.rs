use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonRowTable {
    Evidence,
    ChainState,
    BenchmarkConclusion,
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

pub(crate) fn infer_run_dir_from_path(path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        let is_project_run_dir = ancestor
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "runs")
            && ancestor
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("run_"));
        let has_run_manifest = ancestor.join("manifest.json").exists()
            || ancestor.join("resolved_experiment.json").exists();
        #[cfg(test)]
        let has_test_account_db = ancestor.join(".agentlab").join("agentlab.sqlite").exists();
        #[cfg(not(test))]
        let has_test_account_db = false;
        if is_project_run_dir || has_run_manifest || has_test_account_db {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

pub(crate) fn json_row_table_from_path(path: &Path) -> Option<JsonRowTable> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.contains("evidence") {
        return Some(JsonRowTable::Evidence);
    }
    if name.contains("task_chain") || name.contains("chain_state") {
        return Some(JsonRowTable::ChainState);
    }
    if name.contains("conclusion") {
        return Some(JsonRowTable::BenchmarkConclusion);
    }
    None
}

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
