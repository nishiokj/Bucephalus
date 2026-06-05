use anyhow::{anyhow, Result};
use chrono::Utc;
use lab_core::{ensure_dir, sha256_file};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use crate::config::atomic_write_json_pretty;
use crate::model::EvaluationConfig;
use crate::trial::grade::task_grading_enabled;
use crate::trial::layout::{trial_preflight_path, trial_runner_dir};

pub(crate) fn stage_trial_preflight(
    evaluation_config: &EvaluationConfig,
    trial_dir: &Path,
    trial_ref: (&str, &str, usize, &str),
    task_input: (&Value, Option<&str>, &Path),
) -> Result<()> {
    if evaluation_config.grader.is_none() {
        return Ok(());
    }
    let (run_id, trial_id, schedule_idx, variant_id) = trial_ref;
    let (task_payload, environment_image, trial_input_path) = task_input;

    let task_id = task_payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("trial preflight: task payload missing non-empty id"))?;
    let environment_image = environment_image
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    let grading_enabled = task_grading_enabled(task_payload);
    if !grading_enabled {
        return Err(anyhow!(
            "trial preflight: grading.enabled=false is not supported; every graded task must emit mapped_grader_output.json"
        ));
    }

    let frozen_dir = trial_dir.join("artifacts").join("frozen_agent_input");
    ensure_dir(&frozen_dir)?;
    let frozen_input_path = frozen_dir.join("trial_input.json");
    fs::copy(trial_input_path, &frozen_input_path)?;
    let frozen_input_digest = sha256_file(&frozen_input_path)?;

    let preflight = json!({
        "schema_version": "trial_preflight_v1",
        "run_id": run_id,
        "trial_id": trial_id,
        "schedule_idx": schedule_idx,
        "variant_id": variant_id,
        "task_id": task_id,
        "environment_image": environment_image,
        "grading": {
            "enabled": grading_enabled,
        },
        "frozen_agent_artifacts": {
            "trial_input_path": frozen_input_path,
            "trial_input_digest": frozen_input_digest,
        },
        "checked_at": Utc::now().to_rfc3339(),
    });
    ensure_dir(&trial_runner_dir(trial_dir))?;
    atomic_write_json_pretty(&trial_preflight_path(trial_dir), &preflight)
}
