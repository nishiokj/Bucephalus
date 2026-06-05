use anyhow::{anyhow, Result};
use chrono::Utc;
use lab_core::{ensure_dir, ArtifactStore};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::config::*;
use crate::experiment::runtime::{resolve_exec_digest, VariantRuntimeProfile};
use crate::model::*;
use crate::persistence::backend::{open_attempt_object_store, AttemptObjectUpsert};
use crate::persistence::journal::append_uncommitted_json_row;
use crate::persistence::journal::RunSink;
use crate::persistence::rows::ContractStageRow;
use crate::persistence::rows::TrialRecord;
use crate::trial::artifacts::{
    agent_response_payload_view, artifact_type_from_trial_input_path,
    extract_candidate_artifact_record,
};
use crate::trial::events::{
    build_metric_rows, build_variant_snapshot_rows, extract_declared_metrics, TrialRowIdentity,
};
use crate::trial::execution::{
    execution_backend, EvidenceBlobRef, TrialRunRequest, TrialRuntimeExecutionRequest,
};
use crate::trial::grade::{
    agent_response_execution_outcome, mapped_grader_output_state, task_grading_enabled,
};
use crate::trial::layout::{
    ensure_trial_surface_dirs, materialize_trial_runtime_layout, prune_empty_trial_logs,
    trial_contract_trace_path, trial_metadata_path, trial_summary_path, write_state_inventory,
    StateInventoryInput,
};
use crate::trial::preflight::stage_trial_preflight;
use crate::trial::prepare::{
    prepare_task_environment, prepare_task_environment_with_paths, PrepareTaskEnvironmentRequest,
    PreparedTaskEnvironment, TrialPaths,
};
use crate::trial::spec::{
    parse_task_boundary_from_packaged_task, TaskBoundaryMaterialization, TaskMaterializationKind,
};
use crate::trial::state::{write_trial_state, TrialStateGuard};
use crate::util::remove_path_if_exists;

pub(crate) struct ScheduledTrialRequest<'a> {
    pub(crate) run_dir: &'a Path,
    pub(crate) run_id: &'a str,
    pub(crate) workload_type: &'a str,
    pub(crate) project_root: &'a Path,
    pub(crate) variants: &'a [Variant],
    pub(crate) tasks: &'a [Value],
    pub(crate) schedule_idx: usize,
    pub(crate) slot: &'a TrialSlot,
    pub(crate) policy_config: &'a PolicyConfig,
    pub(crate) evaluation_config: &'a EvaluationConfig,
    pub(crate) metric_definitions: &'a [MetricDefinition],
    pub(crate) variant_runtime_profiles: &'a [VariantRuntimeProfile],
    pub(crate) executor_kind: ExecutorKind,
    pub(crate) materialize_mode: MaterializationMode,
    pub(crate) precomputed_trial_paths: Option<TrialPaths>,
    pub(crate) trials_dir: &'a Path,
    pub(crate) evidence_records_path: &'a Path,
    pub(crate) task_chain_states_path: &'a Path,
    pub(crate) artifact_store: &'a ArtifactStore,
    pub(crate) trial_index: &'a mut usize,
    pub(crate) chain_states: &'a mut BTreeMap<String, ChainRuntimeState>,
    pub(crate) baseline_id: &'a str,
    pub(crate) run_sink: &'a mut dyn RunSink,
}

pub(crate) struct PreparedScheduledTrial {
    variant: Variant,
    variant_runtime: VariantRuntimeProfile,
    task_boundary: TaskBoundaryMaterialization,
    task_id: String,
    task_idx: usize,
    repl: usize,
    pub(crate) grading_enabled: bool,
    chain_key: String,
    chain_step_index: usize,
    trial_id: String,
    trial_dir: PathBuf,
    trial_guard: TrialStateGuard,
    prepared_manifest: PreparedTaskEnvironmentManifest,
    trial_paths: TrialPaths,
    io_paths: PreparedTrialIo,
    trial_input_ref: String,
    dynamic_mounts: Vec<ResolvedMountReference>,
    task_sandbox_image: String,
    task_sandbox_workdir: String,
    configured_network_mode: String,
    effective_network_mode: String,
    invocation_source: String,
    effective_policy: EffectiveTaskPolicy,
}

fn write_scheduled_trial_metadata(
    request: &ScheduledTrialRequest<'_>,
    prepared: &PreparedScheduledTrial,
) -> Result<()> {
    let variant_digest = variant_digest(&prepared.variant)?;
    let trial_metadata = json!({
        "schema_version": "trial_metadata_v1",
        "variant_digest": variant_digest,
        "ids": {
            "run_id": request.run_id,
            "trial_id": prepared.trial_id.as_str(),
            "variant_id": prepared.variant.id.as_str(),
            "task_id": prepared.task_id.as_str(),
            "repl_idx": prepared.repl,
            "task_index": prepared.task_idx
        },
        "runtime": {
            "integration_level": prepared.variant_runtime.agent_runtime.integration_level.as_str(),
            "network_mode_requested": prepared.configured_network_mode.as_str(),
            "network_mode_effective": prepared.effective_network_mode.as_str(),
            "agent_runtime": {
                "image": prepared.variant_runtime.agent_runtime.image.clone(),
                "workdir": prepared.task_sandbox_workdir.as_str(),
            },
            "task_sandbox": {
                "executor": request.executor_kind.as_str(),
                "image": prepared.task_sandbox_image.as_str(),
                "workdir": prepared.task_sandbox_workdir.as_str()
            }
        },
        "policy_merge": {
            "global_defaults": {
                "state_policy": "isolate_per_trial",
                "task_model": "independent",
                "scoring_lifecycle": "predict_then_score",
                "required_evidence_classes": []
            },
            "experiment_type_policy": {
                "state_policy": match request.policy_config.state {
                    StatePolicy::IsolatePerTrial => "isolate_per_trial",
                    StatePolicy::PersistPerTask => "persist_per_task",
                    StatePolicy::Accumulate => "accumulate",
                }
            },
            "evaluation_policy": {
                "task_model": request.evaluation_config.policy.task_model.as_str(),
                "scoring_lifecycle": request.evaluation_config.policy.scoring_lifecycle.as_str(),
                "required_evidence_classes": request.evaluation_config.policy.required_evidence_classes.clone()
            },
            "task_override": prepared.task_boundary.task_payload.get("policy_override").cloned(),
            "effective": {
                "state_policy": match prepared.effective_policy.state_policy {
                    StatePolicy::IsolatePerTrial => "isolate_per_trial",
                    StatePolicy::PersistPerTask => "persist_per_task",
                    StatePolicy::Accumulate => "accumulate",
                },
                "task_model": prepared.effective_policy.task_model.as_str(),
                "scoring_lifecycle": prepared.effective_policy.scoring_lifecycle.as_str(),
                "required_evidence_classes": prepared.effective_policy.required_evidence_classes.clone(),
                "chain_failure_policy": prepared.effective_policy.chain_failure_policy.as_str(),
            }
        },
        "chain": {
            "chain_id": prepared.chain_key.as_str(),
            "step_index": prepared.chain_step_index
        }
    });
    ensure_trial_surface_dirs(&prepared.trial_dir)?;
    atomic_write_json_pretty(&trial_metadata_path(&prepared.trial_dir), &trial_metadata)
}

pub(crate) fn prepare_scheduled_trial(
    request: &mut ScheduledTrialRequest<'_>,
) -> Result<PreparedScheduledTrial> {
    let variant = request.variants[request.slot.variant_idx].clone();
    let variant_runtime = request.variant_runtime_profiles[request.slot.variant_idx].clone();
    let agent_runtime = &variant_runtime.agent_runtime;
    let trial_experiment = &variant_runtime.experiment;
    let task_idx = request.slot.task_idx;
    let task = &request.tasks[task_idx];
    let task_boundary = parse_task_boundary_from_packaged_task(task)?;
    let task_id = task_boundary.task_id.clone();
    if request.evaluation_config.grader.is_some()
        && !task_grading_enabled(&task_boundary.task_payload)
    {
        return Err(anyhow!(
            "graded task '{}' sets grading.enabled=false, but graded trials require mapped grading output",
            task_id
        ));
    }

    let repl = request.slot.repl_idx;
    let grading_enabled = request.evaluation_config.grader.is_some();
    let effective_policy = resolve_effective_task_policy(
        request.policy_config,
        &request.evaluation_config.policy,
        &task_boundary.task_payload,
    )?;
    let chain_key = format!("{}::{}", variant.id, task_id);
    let chain_step_index = request
        .chain_states
        .get(&chain_key)
        .map(|state| state.step_index + 1)
        .unwrap_or(0);

    *request.trial_index += 1;
    let trial_id = format!("trial_{}", *request.trial_index);
    let trial_dir = request.trials_dir.join(&trial_id);
    ensure_dir(&trial_dir)?;
    write_trial_state(&trial_dir, &trial_id, "running", None, None, None)?;
    let trial_guard = TrialStateGuard::new(&trial_dir, &trial_id);

    let prepared = if let Some(trial_paths) = request.precomputed_trial_paths.take() {
        prepare_task_environment_with_paths(
            trial_paths,
            PrepareTaskEnvironmentRequest {
                run_id: request.run_id,
                trial_experiment,
                variant: &variant,
                task_idx,
                repl,
                task_boundary: &task_boundary,
                agent_runtime,
            },
        )?
    } else {
        prepare_task_environment(
            request.run_dir,
            &trial_dir,
            PrepareTaskEnvironmentRequest {
                run_id: request.run_id,
                trial_experiment,
                variant: &variant,
                task_idx,
                repl,
                task_boundary: &task_boundary,
                agent_runtime,
            },
        )?
    };

    let PreparedTaskEnvironment {
        manifest: prepared_manifest,
        trial_paths,
        io_paths,
        dynamic_mounts,
        trial_input: input,
    } = prepared;
    let task_sandbox_image = prepared_manifest.task_sandbox_image()?.to_string();
    let task_sandbox_workdir = prepared_manifest.task_sandbox_workdir()?.to_string();
    let configured_network_mode = variant_runtime.configured_network_mode.clone();
    let effective_network_mode = variant_runtime.effective_network_mode.clone();
    let invocation_source = variant_runtime.invocation_source.clone();

    let input_bytes = serde_json::to_vec_pretty(&input)?;
    let trial_input_ref = request.artifact_store.put_bytes(&input_bytes)?;
    let mut bootstrap_store = open_attempt_object_store(request.run_dir)?;
    bootstrap_store.upsert_attempt_object(AttemptObjectUpsert {
        run_id: request.run_id,
        trial_id: &trial_id,
        schedule_idx: request.schedule_idx,
        attempt: 0,
        role: "trial_input",
        object_ref: &trial_input_ref,
        metadata: None,
    })?;

    let prepared = PreparedScheduledTrial {
        variant,
        variant_runtime,
        task_boundary,
        task_id,
        task_idx,
        repl,
        grading_enabled,
        chain_key,
        chain_step_index,
        trial_id,
        trial_dir,
        trial_guard,
        prepared_manifest,
        trial_paths,
        io_paths,
        trial_input_ref,
        dynamic_mounts,
        task_sandbox_image,
        task_sandbox_workdir,
        configured_network_mode,
        effective_network_mode,
        invocation_source,
        effective_policy,
    };

    write_scheduled_trial_metadata(request, &prepared)?;
    stage_trial_preflight(
        request.evaluation_config,
        &prepared.trial_dir,
        (
            request.run_id,
            &prepared.trial_id,
            request.schedule_idx,
            &prepared.variant.id,
        ),
        (
            &prepared.task_boundary.task_payload,
            Some(prepared.task_sandbox_image.as_str()),
            &prepared.io_paths.trial_input_host,
        ),
    )?;

    Ok(prepared)
}

fn extract_checkpoint_labels(response_payload: &Value) -> Result<Vec<String>> {
    let Some(checkpoints) = response_payload.get("checkpoints") else {
        return Ok(Vec::new());
    };
    let rows = checkpoints
        .as_array()
        .ok_or_else(|| anyhow!("trial output checkpoints must be an array"))?;
    rows.iter()
        .enumerate()
        .map(|(idx, row)| {
            let object = row
                .as_object()
                .ok_or_else(|| anyhow!("trial output checkpoint {} must be an object", idx))?;
            object
                .get("logical_name")
                .and_then(Value::as_str)
                .or_else(|| object.get("path").and_then(Value::as_str))
                .filter(|label| !label.is_empty())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("trial output checkpoint {} missing label path", idx))
        })
        .collect()
}

pub(crate) fn execute_scheduled_trial_attempt(
    request: &ScheduledTrialRequest<'_>,
    prepared: &PreparedScheduledTrial,
    attempt_no: usize,
) -> Result<crate::trial::execution::TrialRuntimeOutcome> {
    let run_request = TrialRunRequest {
        package_root: request.run_dir,
        runtime_experiment: &prepared.variant_runtime.experiment,
        runtime: &prepared.variant_runtime.agent_runtime,
        variant_args: &prepared.variant_runtime.variant_args,
        runtime_env: &prepared.prepared_manifest.runtime_env,
        runtime_overrides_env: &prepared.variant_runtime.agent_runtime_env,
        trial_paths: &prepared.trial_paths,
        dynamic_mounts: &prepared.dynamic_mounts,
        secret_file_mounts: &prepared.variant_runtime.secret_file_mounts,
        io_paths: &prepared.io_paths,
        network_mode: prepared.effective_network_mode.as_str(),
        grader: request.evaluation_config.grader.as_ref(),
        grading_enabled: prepared.grading_enabled,
        run_id: request.run_id,
        task_image: prepared.task_sandbox_image.as_str(),
        task_workdir: prepared.task_sandbox_workdir.as_str(),
        task_materialization_kind: prepared.task_boundary.materialization.kind.clone(),
        agent_artifact: prepared
            .variant_runtime
            .agent_runtime
            .agent_artifact
            .as_deref(),
        agent_artifact_mount_path: prepared
            .variant_runtime
            .agent_runtime
            .agent_artifact_mount_path
            .as_deref(),
        agent_artifact_read_only: prepared
            .variant_runtime
            .agent_runtime
            .agent_artifact_read_only,
    };

    for path in [
        &prepared.io_paths.result_host,
        &prepared.trial_paths.out.join(MAPPED_GRADER_OUTPUT_FILENAME),
        &prepared.trial_paths.out.join(GRADING_ERROR_FILENAME),
    ] {
        remove_path_if_exists(path)?;
    }
    let execution_request = TrialRuntimeExecutionRequest {
        trial_dir: &prepared.trial_dir,
        schedule_idx: request.schedule_idx,
        attempt_no,
        run_request: &run_request,
        task_id: &prepared.task_id,
        variant_id: &prepared.variant.id,
        repl_idx: prepared.repl,
        task_sandbox_plan: prepared
            .prepared_manifest
            .task_sandbox_plan
            .as_ref()
            .ok_or_else(|| anyhow!("prepared task environment missing task sandbox plan"))?,
    };
    execution_backend(request.executor_kind)?.execute_attempt(execution_request)
}

pub(crate) fn evidence_blob_ref(
    artifact_store: &ArtifactStore,
    blob: Option<EvidenceBlobRef>,
) -> Result<Option<String>> {
    match blob {
        Some(EvidenceBlobRef::LocalPath(path)) => Ok(Some(artifact_store.put_file(&path)?)),
        Some(EvidenceBlobRef::RemoteRef { uri }) => {
            if uri.trim().is_empty() {
                return Err(anyhow!("remote evidence ref uri must not be empty"));
            }
            Ok(Some(uri))
        }
        None => Ok(None),
    }
}

fn insert_evidence_ref(evidence: &mut Map<String, Value>, field: &str, object_ref: &str) {
    evidence.insert(field.to_string(), Value::String(object_ref.to_string()));
}

pub(crate) fn finalize_scheduled_trial(
    request: &mut ScheduledTrialRequest<'_>,
    prepared: &mut PreparedScheduledTrial,
    runtime_outcome: crate::trial::execution::TrialRuntimeOutcome,
    trial_started_at: Instant,
) -> Result<TrialExecutionResult> {
    let evidence_capture_started_at = Instant::now();
    let executor = runtime_outcome.executor;
    let status = runtime_outcome.agent_exit_status;
    let trial_output = runtime_outcome.trial_output;
    let result_present = runtime_outcome.result_present;
    let result_parse_error = runtime_outcome.result_parse_error;
    let stdout = runtime_outcome.stdout;
    let stderr = runtime_outcome.stderr;
    let events = runtime_outcome.events;
    let event_rows = runtime_outcome.event_rows;
    let deferred_trial_conclusion_records = runtime_outcome.deferred_trial_conclusion_records;
    let trial_conclusion_row = runtime_outcome.trial_conclusion_row;
    let grade_error_reason = runtime_outcome.grade_error_reason;

    if !matches!(
        prepared.effective_policy.state_policy,
        StatePolicy::IsolatePerTrial
    ) {
        request.chain_states.insert(
            prepared.chain_key.clone(),
            ChainRuntimeState {
                step_index: prepared.chain_step_index,
            },
        );
    }

    let trial_output_ref = request
        .artifact_store
        .put_bytes(&serde_json::to_vec_pretty(&trial_output)?)?;

    let stdout_ref = evidence_blob_ref(request.artifact_store, stdout)?;
    let stderr_ref = evidence_blob_ref(request.artifact_store, stderr)?;

    let events_ref = evidence_blob_ref(request.artifact_store, events)?;
    let finalize_perf_scope = crate::perf::PerfScope::new(
        request.run_dir,
        request.run_id,
        Some(&prepared.trial_id),
        Some(request.schedule_idx),
        Some(0),
    );
    crate::perf::record_duration(
        finalize_perf_scope,
        "trial_finalize_evidence_capture",
        evidence_capture_started_at,
        json!({}),
    )?;

    let mut evidence_refs = Map::new();
    for (field, object_ref) in [
        ("trial_input_ref", prepared.trial_input_ref.as_str()),
        ("trial_output_ref", trial_output_ref.as_str()),
        ("harness_request_ref", prepared.trial_input_ref.as_str()),
        ("harness_response_ref", trial_output_ref.as_str()),
    ] {
        insert_evidence_ref(&mut evidence_refs, field, object_ref);
    }
    for (field, object_ref) in [
        ("stdout_ref", stdout_ref.as_deref()),
        ("stderr_ref", stderr_ref.as_deref()),
        ("events_ref", events_ref.as_deref()),
    ] {
        if let Some(object_ref) = object_ref {
            insert_evidence_ref(&mut evidence_refs, field, object_ref);
        }
    }

    let trial_duration_ms = trial_started_at.elapsed().as_secs_f64() * 1000.0;
    let evidence_record = json!({
        "schema_version": "evidence_record_v1",
        "ts": Utc::now().to_rfc3339(),
        "ids": {
            "run_id": request.run_id,
            "trial_id": prepared.trial_id.as_str(),
            "variant_id": prepared.variant.id.as_str(),
            "task_id": prepared.task_id.as_str(),
            "repl_idx": prepared.repl
        },
        "policy": {
            "state_policy": match prepared.effective_policy.state_policy {
                StatePolicy::IsolatePerTrial => "isolate_per_trial",
                StatePolicy::PersistPerTask => "persist_per_task",
                StatePolicy::Accumulate => "accumulate",
            },
            "task_model": prepared.effective_policy.task_model.as_str(),
            "chain_id": prepared.chain_key.as_str(),
            "chain_step_index": prepared.chain_step_index
        },
        "runtime": {
            "executor": executor.as_str(),
            "exit_status": status.as_str(),
            "duration_ms": trial_duration_ms
        },
        "evidence": evidence_refs
    });
    validate_required_evidence_classes(
        &evidence_record,
        &prepared.effective_policy.required_evidence_classes,
    )?;
    let record_buffer_started_at = Instant::now();
    append_uncommitted_json_row(request.evidence_records_path, &evidence_record)?;

    let response_payload = agent_response_payload_view(&trial_output)?;
    let checkpoint_labels = extract_checkpoint_labels(response_payload)?;
    let chain_state_record = json!({
        "schema_version": "task_chain_state_v1",
        "ts": Utc::now().to_rfc3339(),
        "run_id": request.run_id,
        "chain_id": prepared.chain_key.as_str(),
        "task_model": prepared.effective_policy.task_model.as_str(),
        "step_index": prepared.chain_step_index,
        "ids": {
            "trial_id": prepared.trial_id.as_str(),
            "variant_id": prepared.variant.id.as_str(),
            "task_id": prepared.task_id.as_str(),
            "repl_idx": prepared.repl
        },
        "checkpoint_labels": checkpoint_labels
    });
    append_uncommitted_json_row(request.task_chain_states_path, &chain_state_record)?;

    write_state_inventory(StateInventoryInput {
        trial_dir: &prepared.trial_dir,
        json_value: &prepared.variant_runtime.experiment,
        agent_runtime: &prepared.variant_runtime.agent_runtime,
        secret_file_mounts: &prepared.variant_runtime.secret_file_mounts,
        exec_digest: &resolve_exec_digest(
            &prepared.variant_runtime.agent_runtime.command_raw,
            request.project_root,
        )?,
        effective_network_mode: prepared.effective_network_mode.as_str(),
        invocation_source: prepared.invocation_source.as_str(),
        task_sandbox_image: Some(prepared.task_sandbox_image.as_str()),
        task_sandbox_workdir: prepared.task_sandbox_workdir.as_str(),
    })?;

    let trial_conclusion_outcome = trial_conclusion_row
        .as_ref()
        .and_then(|row| row.pointer("/reported_outcome"))
        .and_then(Value::as_str);
    let mapped_trial_outcome =
        trial_conclusion_outcome.and_then(trial_conclusion_outcome_to_trial_outcome);
    let agent_outcome =
        agent_response_execution_outcome(&status, result_present, result_parse_error.as_deref());
    let agent_timed_out = agent_outcome == "timeout";
    let outcome = if prepared.grading_enabled {
        if agent_timed_out {
            agent_outcome
        } else if grade_error_reason.is_some() {
            "grading_failed"
        } else {
            mapped_trial_outcome.unwrap_or("missing")
        }
    } else {
        agent_outcome
    };
    let (mut metrics, declared_primary) = if request.metric_definitions.is_empty() {
        (json!({}), None)
    } else {
        extract_declared_metrics(request.metric_definitions, response_payload)?
    };
    if let Some(obj) = metrics.as_object_mut() {
        obj.insert("status_code".to_string(), json!(status.clone()));
        if let Some(mapped_state) =
            mapped_grader_output_state(trial_conclusion_row.as_ref(), grade_error_reason.as_deref())
        {
            obj.insert(
                "mapped_grader_output_state".to_string(),
                json!(mapped_state),
            );
        }
        if let Some(reported_outcome) = trial_conclusion_outcome {
            obj.insert(
                "trial_conclusion_reported_outcome".to_string(),
                json!(reported_outcome),
            );
        }
        if let Some(row) = trial_conclusion_row.as_ref() {
            if let Some(payload) = row.pointer("/payload") {
                obj.insert("trial_conclusion_payload".to_string(), payload.clone());
            }
            if let Some(name) = row.pointer("/grader/name").and_then(Value::as_str) {
                obj.insert("trial_conclusion_grader".to_string(), json!(name));
            }
            if let Some(strategy) = row.pointer("/grader/strategy").and_then(Value::as_str) {
                obj.insert(
                    "trial_conclusion_grader_strategy".to_string(),
                    json!(strategy),
                );
            }
        }
        if let Some(reason) = grade_error_reason.as_ref() {
            obj.insert("grade_error".to_string(), json!(true));
            obj.insert("grade_error_reason".to_string(), json!(reason));
        }
    }
    let mapped_primary = match trial_conclusion_row
        .as_ref()
        .and_then(|row| row.get("primary_metric"))
    {
        Some(primary) => Some((
            primary
                .pointer("/name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow!("trial conclusion primary_metric missing /name"))?
                .to_string(),
            primary
                .pointer("/value")
                .cloned()
                .ok_or_else(|| anyhow!("trial conclusion primary_metric missing /value"))?,
        )),
        None => None,
    };
    let (primary_metric_name, primary_metric_value) = if prepared.grading_enabled {
        if agent_timed_out {
            ("timeout".to_string(), json!(null))
        } else if grade_error_reason.is_some() {
            ("grading_failed".to_string(), json!(null))
        } else if let Some((name, value)) = mapped_primary {
            (name, value)
        } else if let Some(row) = trial_conclusion_row.as_ref() {
            (
                "trial_conclusion_payload".to_string(),
                row.pointer("/payload")
                    .cloned()
                    .ok_or_else(|| anyhow!("trial conclusion row missing /payload"))?,
            )
        } else {
            ("grading_failed".to_string(), json!(null))
        }
    } else if let Some((name, value)) = declared_primary {
        (name, value)
    } else if response_payload.get("objective").is_some() {
        let obj = response_payload
            .get("objective")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("trial output objective must be an object"))?;
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow!("trial output objective missing /name"))?
            .to_string();
        let value = obj
            .get("value")
            .cloned()
            .ok_or_else(|| anyhow!("trial output objective missing /value"))?;
        (name, value)
    } else {
        let success_value = if outcome == "success" { 1.0 } else { 0.0 };
        ("success".to_string(), json!(success_value))
    };
    let outcome_details = TrialOutcomeDetails {
        outcome,
        agent_exit_status: &status,
        agent_outcome,
        result_present,
        result_parse_error: result_parse_error.as_deref(),
        grade_error_reason: grade_error_reason.as_deref(),
        trial_conclusion_row: trial_conclusion_row.as_ref(),
    };
    let primary_metric = (primary_metric_name.as_str(), &primary_metric_value);
    let contract_trace = build_trial_contract_trace(
        request,
        prepared,
        &trial_output,
        outcome_details,
        primary_metric,
    )?;
    let contract_stage_rows = build_contract_stage_rows(
        request.run_id,
        &prepared.trial_id,
        request.schedule_idx,
        &prepared.variant.id,
        &prepared.task_id,
        prepared.repl,
        &contract_trace,
    )?;
    let bindings = variant_bindings_for_summary(&prepared.variant);
    let row_identity = TrialRowIdentity {
        run_id: request.run_id,
        trial_id: &prepared.trial_id,
        schedule_idx: request.schedule_idx,
        variant_id: &prepared.variant.id,
        task_id: &prepared.task_id,
        repl_idx: prepared.repl,
    };
    let metric_rows = build_metric_rows(
        row_identity,
        outcome,
        &metrics,
        &primary_metric_name,
        &primary_metric_value,
    );
    let variant_snapshot_rows =
        build_variant_snapshot_rows(row_identity, request.baseline_id, &bindings);
    request.run_sink.append_trial_record(&TrialRecord {
        run_id: request.run_id.to_string(),
        trial_id: prepared.trial_id.clone(),
        schedule_idx: request.schedule_idx,
        slot_commit_id: String::new(),
        attempt: 0,
        row_seq: 0,
        baseline_id: request.baseline_id.to_string(),
        workload_type: request.workload_type.to_string(),
        variant_id: prepared.variant.id.clone(),
        task_index: prepared.task_idx,
        task_id: prepared.task_id.clone(),
        repl_idx: prepared.repl,
        outcome: outcome.to_string(),
        success: outcome == "success" && grade_error_reason.is_none(),
        status_code: status.clone(),
        integration_level: prepared
            .variant_runtime
            .agent_runtime
            .integration_level
            .clone(),
        network_mode_requested: prepared.configured_network_mode.clone(),
        network_mode_effective: prepared.effective_network_mode.clone(),
        primary_metric_name: primary_metric_name.clone(),
        primary_metric_value: primary_metric_value.clone(),
        metrics: metrics.clone(),
        bindings: bindings.clone(),
        events_total: event_rows.len(),
        has_events: !event_rows.is_empty(),
    })?;
    request.run_sink.append_metric_rows(&metric_rows)?;
    request.run_sink.append_event_rows(&event_rows)?;
    request
        .run_sink
        .append_contract_stage_rows(&contract_stage_rows)?;
    request
        .run_sink
        .append_variant_snapshot(&variant_snapshot_rows)?;
    crate::perf::record_duration(
        finalize_perf_scope,
        "trial_finalize_record_buffer",
        record_buffer_started_at,
        json!({}),
    )?;

    let classify_guard_started_at = Instant::now();
    let failure_classification = if prepared.grading_enabled {
        if grade_error_reason.is_some() {
            prepared
                .trial_guard
                .complete("failed", Some("grade_error"))?;
            Some("grade_error".to_string())
        } else {
            prepared.trial_guard.complete("completed", None)?;
            None
        }
    } else if status != "0" {
        prepared
            .trial_guard
            .complete("failed", Some("agent_exit_nonzero"))?;
        Some("agent_exit_nonzero".to_string())
    } else if !result_present {
        prepared
            .trial_guard
            .complete("failed", Some("result_missing"))?;
        Some("result_missing".to_string())
    } else if result_parse_error.is_some() {
        prepared
            .trial_guard
            .complete("failed", Some("result_parse_error"))?;
        Some("result_parse_error".to_string())
    } else if status == "0" && outcome != "error" {
        prepared.trial_guard.complete("completed", None)?;
        None
    } else {
        prepared
            .trial_guard
            .complete("failed", Some("result_error"))?;
        Some("result_error".to_string())
    };
    crate::perf::record_duration(
        finalize_perf_scope,
        "trial_finalize_classify_guard",
        classify_guard_started_at,
        json!({}),
    )?;

    let materialize_started_at = Instant::now();
    materialize_trial_runtime_layout(
        &prepared.trial_dir,
        &prepared.trial_paths,
        &prepared.variant_runtime.experiment,
        request.materialize_mode,
    )?;
    crate::perf::record_duration(
        finalize_perf_scope,
        "trial_layout_materialize",
        materialize_started_at,
        json!({ "mode": request.materialize_mode.as_str() }),
    )?;
    prune_empty_trial_logs(&prepared.trial_dir)?;
    write_trial_summary(
        request,
        prepared,
        outcome_details,
        primary_metric,
        &metrics,
        &summary_extra_outputs(&prepared.variant_runtime.experiment)?,
    )?;
    atomic_write_json_pretty(
        &trial_contract_trace_path(&prepared.trial_dir),
        &contract_trace,
    )?;
    let scratch_cleanup_started_at = Instant::now();
    prepared.trial_paths.cleanup_scratch()?;
    crate::perf::record_duration(
        finalize_perf_scope,
        "trial_scratch_cleanup",
        scratch_cleanup_started_at,
        json!({}),
    )?;

    let slot_status = if prepared.grading_enabled {
        if agent_timed_out {
            "failed"
        } else if grade_error_reason.is_none() {
            "completed"
        } else {
            "grading_failed"
        }
    } else if status == "0" && outcome != "error" {
        "completed"
    } else {
        "failed"
    };
    let mut result = TrialExecutionResult::minimal(
        prepared.trial_id.clone(),
        slot_status,
        Some(request.slot.variant_idx),
    );
    result.deferred_trial_conclusion_records = deferred_trial_conclusion_records;
    result.failure_classification = failure_classification;
    Ok(result)
}

#[derive(Clone, Copy)]
struct TrialOutcomeDetails<'a> {
    outcome: &'a str,
    agent_exit_status: &'a str,
    agent_outcome: &'a str,
    result_present: bool,
    result_parse_error: Option<&'a str>,
    grade_error_reason: Option<&'a str>,
    trial_conclusion_row: Option<&'a Value>,
}

fn write_trial_summary(
    request: &ScheduledTrialRequest<'_>,
    prepared: &PreparedScheduledTrial,
    status: TrialOutcomeDetails<'_>,
    primary_metric: (&str, &Value),
    metrics: &Value,
    extra_outputs: &Value,
) -> Result<()> {
    let grader_outcome = if let Some(reason) = status.grade_error_reason {
        json!({
            "status": "error",
            "reason": reason,
            "mapped_output": "grader/mapped_output.json"
        })
    } else if let Some(row) = status.trial_conclusion_row {
        let status = row
            .pointer("/reported_outcome")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("trial conclusion row missing /reported_outcome"))?;
        json!({
            "status": status,
            "grader": row.pointer("/grader/name").and_then(Value::as_str),
            "strategy": row.pointer("/grader/strategy").and_then(Value::as_str),
            "mapped_output": "grader/mapped_output.json"
        })
    } else {
        json!({
            "status": "not_run"
        })
    };

    let mut artifacts = json!({
        "candidate_patch": "candidate.patch",
        "metadata": "runner/trial_metadata.json",
        "prepared_task_environment": "runner/prepared_task_environment.json",
        "state_inventory": "runner/state_inventory.json",
        "contract_trace": "runner/contract_trace.json"
    });
    if let (Some(base), Some(extra)) = (artifacts.as_object_mut(), extra_outputs.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }

    let summary = json!({
        "schema_version": "trial_summary_v1",
        "ids": {
            "run_id": request.run_id,
            "trial_id": prepared.trial_id,
            "variant_id": prepared.variant.id,
            "task_id": prepared.task_id,
            "repl_idx": prepared.repl
        },
        "outcome": status.outcome,
        "primary_metric": {
            "name": primary_metric.0,
            "value": primary_metric.1
        },
        "agent": {
            "outcome": status.agent_outcome,
            "exit_status": status.agent_exit_status,
            "result": "agent/result.json",
            "events": "agent/events.jsonl",
            "stdout": "agent/stdout.log",
            "stderr": "agent/stderr.log"
        },
        "grader": grader_outcome,
        "artifacts": artifacts,
        "metrics": metrics
    });
    atomic_write_json_pretty(&trial_summary_path(&prepared.trial_dir), &summary)
}

fn summary_extra_outputs(experiment: &Value) -> Result<Value> {
    let mut artifacts = serde_json::Map::new();
    if let Some(items) = declared_extra_outputs(experiment) {
        for item in items {
            let Some(id) = item
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let Some(path) = item
                .get("summary_path")
                .or_else(|| item.get("source_path"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            artifacts.insert(id.to_string(), json!(path));
        }
    }
    Ok(Value::Object(artifacts))
}

fn path_size(path: &Path) -> Result<Option<u64>> {
    fs::metadata(path)
        .map(|metadata| Some(metadata.len()))
        .or_else(|err| match err.kind() {
            std::io::ErrorKind::NotFound => Ok(None),
            _ => Err(anyhow!("failed to inspect {}: {}", path.display(), err)),
        })
}

fn task_materialization_kind_name(kind: &TaskMaterializationKind) -> &'static str {
    match kind {
        TaskMaterializationKind::TaskImage => "task_image",
        TaskMaterializationKind::BaseImageBundle => "base_image_bundle",
    }
}

fn artifact_type_name(artifact_type: &ArtifactType) -> &'static str {
    match artifact_type {
        ArtifactType::PatchSubmission => "patch_submission",
        ArtifactType::TextResponse => "text_response",
        ArtifactType::StructuredJson => "structured_json",
        ArtifactType::FileRef => "file_ref",
    }
}

fn candidate_artifact_state_name(state: &CandidateArtifactState) -> &'static str {
    match state {
        CandidateArtifactState::Missing => "missing",
        CandidateArtifactState::Invalid => "invalid",
        CandidateArtifactState::Valid => "valid",
    }
}

fn candidate_artifact_source_name(source: &CandidateArtifactSource) -> &'static str {
    match source {
        CandidateArtifactSource::ResultInline => "result.inline",
        CandidateArtifactSource::ResultFileRef => "result.file_ref",
        CandidateArtifactSource::None => "none",
    }
}

fn grading_strategy_name(strategy: &GradingStrategy) -> &'static str {
    match strategy {
        GradingStrategy::None => "none",
        GradingStrategy::InTaskRuntime => "in_task_runtime",
        GradingStrategy::Injected => "injected",
        GradingStrategy::Separate => "separate",
        GradingStrategy::Host => "host",
    }
}

fn build_trial_contract_trace(
    request: &ScheduledTrialRequest<'_>,
    prepared: &PreparedScheduledTrial,
    trial_output: &Value,
    outcome: TrialOutcomeDetails<'_>,
    primary_metric: (&str, &Value),
) -> Result<Value> {
    let captured_patch_path = prepared.trial_paths.out.join("candidate.patch");
    let captured_patch_bytes = path_size(&captured_patch_path)?;
    let patch_scope = outcome
        .trial_conclusion_row
        .and_then(|row| row.pointer("/payload/candidate_patch_scope"));
    let scoped_patch_bytes = patch_scope
        .and_then(|scope| scope.pointer("/scoped_bytes"))
        .and_then(Value::as_u64);
    let artifact_type = artifact_type_from_trial_input_path(&prepared.io_paths.trial_input_host)?;
    let agent_result_artifact = extract_candidate_artifact_record(
        trial_output,
        outcome.result_present,
        artifact_type.clone(),
    );
    let output_extraction_status = match &artifact_type {
        ArtifactType::PatchSubmission => {
            if captured_patch_bytes.is_none() {
                "missing"
            } else if captured_patch_bytes == Some(0) {
                "empty"
            } else if scoped_patch_bytes == Some(0) {
                "empty_scoped"
            } else {
                "available"
            }
        }
        _ => match &agent_result_artifact.state {
            CandidateArtifactState::Missing => "missing",
            CandidateArtifactState::Invalid => "invalid",
            CandidateArtifactState::Valid => "available",
        },
    };

    let agent_status = if outcome.agent_exit_status == "0"
        && outcome.result_present
        && outcome.result_parse_error.is_none()
    {
        "ok"
    } else {
        "error"
    };
    let grader_execution_status = if !prepared.grading_enabled {
        "not_run"
    } else if outcome.grade_error_reason.is_some() {
        "error"
    } else if outcome.trial_conclusion_row.is_some() {
        "ok"
    } else {
        "error"
    };
    let grade_mapping_status = if !prepared.grading_enabled {
        "not_run"
    } else if outcome.grade_error_reason.is_none() && outcome.trial_conclusion_row.is_some() {
        "ok"
    } else {
        "error"
    };
    let score_trust = if prepared.grading_enabled {
        if grade_mapping_status == "ok" && !primary_metric.1.is_null() {
            "trusted"
        } else {
            "untrusted"
        }
    } else if outcome.result_present
        && outcome.result_parse_error.is_none()
        && !primary_metric.1.is_null()
    {
        "trusted"
    } else {
        "untrusted"
    };
    let overall_status = if score_trust != "trusted"
        || grader_execution_status == "error"
        || grade_mapping_status == "error"
    {
        "error"
    } else if agent_status != "ok" {
        "warning"
    } else {
        "ok"
    };

    let score_source = if prepared.grading_enabled {
        if outcome.trial_conclusion_row.is_some() {
            "mapped_grader_output"
        } else {
            "missing"
        }
    } else {
        "agent_response"
    };
    let mapped_output_path = prepared.trial_paths.out.join(MAPPED_GRADER_OUTPUT_FILENAME);

    Ok(json!({
        "schema_version": "trial_contract_trace_v1",
        "ids": {
            "run_id": request.run_id,
            "trial_id": prepared.trial_id,
            "variant_id": prepared.variant.id,
            "task_id": prepared.task_id,
            "repl_idx": prepared.repl,
            "schedule_idx": request.schedule_idx
        },
        "overall_status": overall_status,
        "score_trust": score_trust,
        "score": {
            "metric": primary_metric.0,
            "value": primary_metric.1,
            "source": score_source,
            "outcome": outcome.outcome
        },
        "stages": {
            "task_mapping": {
                "status": "ok",
                "image": prepared.task_sandbox_image,
                "workdir": prepared.task_sandbox_workdir,
                "materialization_kind": task_materialization_kind_name(&prepared.task_boundary.materialization.kind)
            },
            "agent_execution": {
                "status": agent_status,
                "exit_status": outcome.agent_exit_status,
                "outcome": outcome.agent_outcome,
                "result_parse_error": outcome.result_parse_error
            },
            "artifact_extraction": {
                "status": output_extraction_status,
                "artifact_type": artifact_type_name(&artifact_type),
                "agent_result_artifact_state": candidate_artifact_state_name(&agent_result_artifact.state),
                "agent_result_artifact_source": candidate_artifact_source_name(&agent_result_artifact.source),
                "workspace_delta": {
                    "status": output_extraction_status,
                    "patch_path": "candidate.patch",
                    "captured_bytes": captured_patch_bytes,
                    "scoped_bytes": scoped_patch_bytes,
                    "scope": patch_scope,
                    "log_dir": "runner/workspace_patch"
                }
            },
            "grader_execution": {
                "status": grader_execution_status,
                "strategy": request.evaluation_config.grader.as_ref().map(|grader| grading_strategy_name(&grader.strategy)),
                "stderr": "grader/stderr.log",
                "stdout": "grader/stdout.log",
                "error": outcome.grade_error_reason
            },
            "grade_mapping": {
                "status": grade_mapping_status,
                "mapped_output_present": mapped_output_path.exists(),
                "reported_outcome": outcome.trial_conclusion_row
                    .and_then(|row| row.pointer("/reported_outcome"))
                    .and_then(Value::as_str),
                "score_source": score_source
            }
        },
        "artifacts": {
            "candidate_patch": "candidate.patch",
            "mapped_grader_output": "grader/mapped_output.json",
            "trial_summary": "summary.json"
        }
    }))
}

fn build_contract_stage_rows(
    run_id: &str,
    trial_id: &str,
    schedule_idx: usize,
    variant_id: &str,
    task_id: &str,
    repl_idx: usize,
    contract_trace: &Value,
) -> Result<Vec<ContractStageRow>> {
    let recorded_at = Utc::now().to_rfc3339();
    let overall_status = contract_trace
        .pointer("/overall_status")
        .cloned()
        .ok_or_else(|| anyhow!("contract trace missing /overall_status"))?;
    let score_trust = contract_trace
        .pointer("/score_trust")
        .cloned()
        .ok_or_else(|| anyhow!("contract trace missing /score_trust"))?;
    let score = contract_trace
        .pointer("/score")
        .cloned()
        .ok_or_else(|| anyhow!("contract trace missing /score"))?;
    let stages = contract_trace
        .pointer("/stages")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("contract trace missing /stages"))?;

    let rows = [
        "task_mapping",
        "agent_execution",
        "artifact_extraction",
        "grader_execution",
        "grade_mapping",
    ]
    .into_iter()
    .enumerate()
    .map(|(row_seq, stage)| {
        let mut detail = stages
            .get(stage)
            .cloned()
            .ok_or_else(|| anyhow!("contract trace missing /stages/{}", stage))?;
        let status = detail
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("contract trace missing /stages/{}/status", stage))?
            .to_string();
        if stage == "grade_mapping" {
            if let Some(obj) = detail.as_object_mut() {
                obj.insert("overall_status".to_string(), overall_status.clone());
                obj.insert("score_trust".to_string(), score_trust.clone());
                obj.insert("score".to_string(), score.clone());
            }
        }
        Ok(ContractStageRow {
            run_id: run_id.to_string(),
            trial_id: trial_id.to_string(),
            schedule_idx,
            slot_commit_id: String::new(),
            attempt: 0,
            row_seq,
            variant_id: variant_id.to_string(),
            task_id: task_id.to_string(),
            repl_idx,
            stage: stage.to_string(),
            status,
            recorded_at: recorded_at.clone(),
            detail,
        })
    })
    .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}
