use anyhow::{anyhow, Result};
use chrono::Utc;
use lab_core::{ensure_dir, ArtifactStore};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::config::*;
use crate::experiment::runtime::{resolve_exec_digest, VariantRuntimeProfile};
use crate::model::*;
use crate::persistence::backend::open_attempt_object_store;
use crate::persistence::journal::append_uncommitted_json_row;
use crate::persistence::journal::RunSink;
use crate::persistence::rows::ContractStageRow;
use crate::persistence::rows::TrialRecord;
use crate::trial::artifacts::{
    agent_response_payload_view, artifact_type_from_trial_input_path,
    extract_candidate_artifact_record,
};
use crate::trial::events::{
    build_metric_rows, build_variant_snapshot_rows, extract_declared_metrics,
};
use crate::trial::execution::{
    execution_backend, AdapterRunRequest, EvidenceBlobRef, TrialRuntimeExecutionRequest,
};
use crate::trial::grade::{
    agent_response_execution_outcome, mapped_grader_output_state, task_grading_enabled,
};
use crate::trial::layout::{
    ensure_trial_surface_dirs, materialize_trial_runtime_layout, prune_empty_trial_logs,
    trial_contract_trace_path, trial_metadata_path, trial_summary_path, write_state_inventory,
};
use crate::trial::preflight::stage_benchmark_trial_preflight;
use crate::trial::prepare::{
    prepare_task_environment, prepare_task_environment_with_paths, PreparedTaskEnvironment,
    TrialPaths,
};
use crate::trial::spec::{
    parse_task_boundary_from_packaged_task, TaskBoundaryMaterialization, TaskMaterializationKind,
};
use crate::trial::state::{write_trial_state, TrialStateGuard};

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
    pub(crate) benchmark_config: &'a BenchmarkConfig,
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
    pub(crate) benchmark_grading_enabled: bool,
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
            "benchmark_type_policy": {
                "task_model": request.benchmark_config.policy.task_model.as_str(),
                "scoring_lifecycle": request.benchmark_config.policy.scoring_lifecycle.as_str(),
                "required_evidence_classes": request.benchmark_config.policy.required_evidence_classes.clone()
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
    let task_id = task_boundary
        .task_payload
        .get("id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("task_{}", task_idx));
    if request.benchmark_config.grader.is_some()
        && !task_grading_enabled(&task_boundary.task_payload)
    {
        return Err(anyhow!(
            "benchmark task '{}' sets grading.enabled=false, but benchmark trials require mapped grading output",
            task_id
        ));
    }

    let repl = request.slot.repl_idx;
    let benchmark_grading_enabled = request.benchmark_config.grader.is_some();
    let effective_policy = resolve_effective_task_policy(
        request.policy_config,
        &request.benchmark_config.policy,
        &task_boundary.task_payload,
    );
    let chain_key = format!("{}::{}", variant.id, task_id);
    let chain_step_index = request
        .chain_states
        .get(&chain_key)
        .map(|state| state.step_index + 1)
        .unwrap_or(0);
    let _has_chain_snapshot = request.chain_states.contains_key(&chain_key);

    *request.trial_index += 1;
    let trial_id = format!("trial_{}", *request.trial_index);
    let trial_dir = request.trials_dir.join(&trial_id);
    ensure_dir(&trial_dir)?;
    write_trial_state(&trial_dir, &trial_id, "running", None, None, None)?;
    let trial_guard = TrialStateGuard::new(&trial_dir, &trial_id);

    let prepared = if let Some(trial_paths) = request.precomputed_trial_paths.take() {
        prepare_task_environment_with_paths(
            trial_paths,
            request.run_dir,
            &trial_dir,
            request.run_id,
            &trial_id,
            trial_experiment,
            &variant,
            task_idx,
            repl,
            &task_boundary,
            agent_runtime,
        )?
    } else {
        prepare_task_environment(
            request.run_dir,
            &trial_dir,
            request.run_id,
            &trial_id,
            trial_experiment,
            &variant,
            task_idx,
            repl,
            &task_boundary,
            agent_runtime,
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

    let input_bytes = serde_json::to_vec_pretty(&input)?;
    let trial_input_ref = request.artifact_store.put_bytes(&input_bytes)?;
    let mut bootstrap_store = open_attempt_object_store(request.run_dir)?;
    bootstrap_store.upsert_attempt_object(
        request.run_id,
        &trial_id,
        request.schedule_idx,
        0,
        "trial_input",
        &trial_input_ref,
        None,
    )?;

    let prepared = PreparedScheduledTrial {
        variant,
        variant_runtime,
        task_boundary,
        task_id,
        task_idx,
        repl,
        benchmark_grading_enabled,
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
        configured_network_mode: request.variant_runtime_profiles[request.slot.variant_idx]
            .configured_network_mode
            .clone(),
        effective_network_mode: request.variant_runtime_profiles[request.slot.variant_idx]
            .effective_network_mode
            .clone(),
        invocation_source: request.variant_runtime_profiles[request.slot.variant_idx]
            .invocation_source
            .clone(),
        effective_policy,
    };

    write_scheduled_trial_metadata(request, &prepared)?;
    stage_benchmark_trial_preflight(
        request.benchmark_config,
        &prepared.trial_dir,
        request.run_id,
        &prepared.trial_id,
        request.schedule_idx,
        &prepared.variant.id,
        &prepared.task_boundary.task_payload,
        Some(prepared.task_sandbox_image.as_str()),
        &prepared.io_paths.trial_input_host,
    )?;

    Ok(prepared)
}

pub(crate) fn execute_scheduled_trial_attempt(
    request: &ScheduledTrialRequest<'_>,
    prepared: &PreparedScheduledTrial,
    attempt_no: u32,
) -> Result<crate::trial::execution::TrialRuntimeOutcome> {
    let runtime_env = prepared.prepared_manifest.runtime_env.clone();
    let run_request = AdapterRunRequest {
        package_root: request.run_dir,
        runtime_experiment: &prepared.variant_runtime.experiment,
        runtime: &prepared.variant_runtime.agent_runtime,
        variant_args: &prepared.variant_runtime.variant_args,
        runtime_env: &runtime_env,
        runtime_overrides_env: &prepared.variant_runtime.agent_runtime_env,
        trial_paths: &prepared.trial_paths,
        dynamic_mounts: &prepared.dynamic_mounts,
        secret_file_mounts: &prepared.variant_runtime.secret_file_mounts,
        io_paths: &prepared.io_paths,
        network_mode: prepared.effective_network_mode.as_str(),
        benchmark_grader: request.benchmark_config.grader.as_ref(),
        benchmark_grading_enabled: prepared.benchmark_grading_enabled,
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
        &prepared
            .trial_paths
            .out
            .join(BENCHMARK_GRADE_ERROR_FILENAME),
    ] {
        let _ = fs::remove_file(path);
    }
    let execution_request = TrialRuntimeExecutionRequest {
        trial_dir: &prepared.trial_dir,
        schedule_idx: request.schedule_idx,
        attempt_no,
        adapter: &run_request,
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
        Some(EvidenceBlobRef::RemoteRef {
            uri,
            digest,
            size_bytes,
            media_type,
        }) => {
            if uri.trim().is_empty() {
                return Err(anyhow!("remote evidence ref uri must not be empty"));
            }
            let _metadata = (digest, size_bytes, media_type);
            Ok(Some(uri))
        }
        None => Ok(None),
    }
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
    crate::perf::record_duration(
        request.run_dir,
        request.run_id,
        Some(&prepared.trial_id),
        Some(request.schedule_idx),
        Some(0),
        "trial_finalize_evidence_capture",
        evidence_capture_started_at,
        json!({}),
    )?;

    let trial_duration_ms = trial_started_at.elapsed().as_secs_f64() * 1000.0;
    let mut evidence_record = json!({
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
        "evidence": {
            "trial_input_ref": prepared.trial_input_ref.clone(),
            "trial_output_ref": trial_output_ref.clone(),
            "stdout_ref": stdout_ref.clone(),
            "stderr_ref": stderr_ref.clone(),
            "events_ref": events_ref.clone(),
            "harness_request_ref": prepared.trial_input_ref.clone(),
            "harness_response_ref": trial_output_ref.clone()
        }
    });
    if let Some(evidence) = evidence_record
        .get_mut("evidence")
        .and_then(Value::as_object_mut)
    {
        if stdout_ref.is_none() {
            evidence.remove("stdout_ref");
        }
        if stderr_ref.is_none() {
            evidence.remove("stderr_ref");
        }
        if events_ref.is_none() {
            evidence.remove("events_ref");
        }
    }
    validate_required_evidence_classes(
        &evidence_record,
        &prepared.effective_policy.required_evidence_classes,
    )?;
    let record_buffer_started_at = Instant::now();
    append_uncommitted_json_row(request.evidence_records_path, &evidence_record)?;

    let response_payload = agent_response_payload_view(&trial_output);
    let checkpoint_labels = response_payload
        .get("checkpoints")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    row.get("logical_name")
                        .and_then(Value::as_str)
                        .or_else(|| row.get("path").and_then(Value::as_str))
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
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

    write_state_inventory(
        &prepared.trial_dir,
        &prepared.variant_runtime.experiment,
        &prepared.variant_runtime.agent_runtime,
        &prepared.variant_runtime.secret_file_mounts,
        &prepared.trial_paths,
        &resolve_exec_digest(
            &prepared.variant_runtime.agent_runtime.command_raw,
            request.project_root,
        )?,
        prepared.effective_network_mode.as_str(),
        prepared.invocation_source.as_str(),
        Some(prepared.task_sandbox_image.as_str()),
        prepared.task_sandbox_workdir.as_str(),
    )?;

    let trial_conclusion_outcome = trial_conclusion_row
        .as_ref()
        .and_then(|row| row.pointer("/reported_outcome"))
        .and_then(Value::as_str);
    let mapped_trial_outcome =
        trial_conclusion_outcome.and_then(trial_conclusion_outcome_to_trial_outcome);
    let agent_outcome =
        agent_response_execution_outcome(&status, result_present, result_parse_error.as_deref())
            .to_string();
    let agent_timed_out = agent_outcome == "timeout";
    let mut outcome = agent_outcome.clone();
    if prepared.benchmark_grading_enabled {
        outcome = if agent_timed_out {
            agent_outcome.clone()
        } else if grade_error_reason.is_some() {
            "grading_failed".to_string()
        } else if let Some(mapped_outcome) = mapped_trial_outcome {
            mapped_outcome.to_string()
        } else {
            "missing".to_string()
        };
    }
    let (mut metrics, declared_primary) = if request.metric_definitions.is_empty() {
        (json!({}), None)
    } else {
        extract_declared_metrics(request.metric_definitions, response_payload)
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
    let mapped_primary = trial_conclusion_row.as_ref().and_then(|row| {
        let name = row
            .pointer("/primary_metric/name")
            .and_then(Value::as_str)
            .map(str::to_string)?;
        let value = row
            .pointer("/primary_metric/value")
            .cloned()
            .unwrap_or(json!(null));
        Some((name, value))
    });
    let (primary_metric_name, primary_metric_value) = if prepared.benchmark_grading_enabled {
        if agent_timed_out {
            ("timeout".to_string(), json!(null))
        } else if grade_error_reason.is_some() {
            ("grading_failed".to_string(), json!(null))
        } else if let Some((name, value)) = mapped_primary {
            (name, value)
        } else if let Some(row) = trial_conclusion_row.as_ref() {
            (
                "trial_conclusion_payload".to_string(),
                row.pointer("/payload").cloned().unwrap_or(json!(null)),
            )
        } else {
            ("grading_failed".to_string(), json!(null))
        }
    } else if let Some((name, value)) = declared_primary {
        (name, value)
    } else if let Some(obj) = response_payload.get("objective").and_then(Value::as_object) {
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("primary_metric")
            .to_string();
        let value = obj.get("value").cloned().unwrap_or(json!(null));
        (name, value)
    } else {
        let fallback = if outcome == "success" { 1.0 } else { 0.0 };
        ("success".to_string(), json!(fallback))
    };
    let contract_trace = build_trial_contract_trace(
        request,
        prepared,
        &trial_output,
        &outcome,
        &status,
        &agent_outcome,
        result_present,
        result_parse_error.as_deref(),
        grade_error_reason.as_deref(),
        trial_conclusion_row.as_ref(),
        &primary_metric_name,
        &primary_metric_value,
    )?;
    let contract_stage_rows = build_contract_stage_rows(
        request.run_id,
        &prepared.trial_id,
        request.schedule_idx,
        &prepared.variant.id,
        &prepared.task_id,
        prepared.repl,
        &contract_trace,
    );
    let bindings = variant_bindings_for_summary(&prepared.variant);
    let metric_rows = build_metric_rows(
        request.run_id,
        &prepared.trial_id,
        request.schedule_idx,
        &prepared.variant.id,
        &prepared.task_id,
        prepared.repl,
        &outcome,
        &metrics,
        &primary_metric_name,
        &primary_metric_value,
    );
    let variant_snapshot_rows = build_variant_snapshot_rows(
        request.run_id,
        &prepared.trial_id,
        request.schedule_idx,
        &prepared.variant.id,
        request.baseline_id,
        &prepared.task_id,
        prepared.repl,
        &bindings,
    );
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
        outcome: outcome.clone(),
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
        request.run_dir,
        request.run_id,
        Some(&prepared.trial_id),
        Some(request.schedule_idx),
        Some(0),
        "trial_finalize_record_buffer",
        record_buffer_started_at,
        json!({}),
    )?;

    let classify_guard_started_at = Instant::now();
    let failure_classification = if prepared.benchmark_grading_enabled {
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
        request.run_dir,
        request.run_id,
        Some(&prepared.trial_id),
        Some(request.schedule_idx),
        Some(0),
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
        request.run_dir,
        request.run_id,
        Some(&prepared.trial_id),
        Some(request.schedule_idx),
        Some(0),
        "trial_layout_materialize",
        materialize_started_at,
        json!({ "mode": request.materialize_mode.as_str() }),
    )?;
    prune_empty_trial_logs(&prepared.trial_dir)?;
    write_trial_summary(
        &prepared.trial_dir,
        request.run_id,
        &prepared.trial_id,
        &prepared.variant.id,
        &prepared.task_id,
        prepared.repl,
        &outcome,
        &status,
        &agent_outcome,
        grade_error_reason.as_deref(),
        trial_conclusion_row.as_ref(),
        &primary_metric_name,
        &primary_metric_value,
        &metrics,
        &declared_summary_artifacts(&prepared.variant_runtime.experiment),
    )?;
    atomic_write_json_pretty(
        &trial_contract_trace_path(&prepared.trial_dir),
        &contract_trace,
    )?;
    let scratch_cleanup_started_at = Instant::now();
    prepared.trial_paths.cleanup_scratch()?;
    crate::perf::record_duration(
        request.run_dir,
        request.run_id,
        Some(&prepared.trial_id),
        Some(request.schedule_idx),
        Some(0),
        "trial_scratch_cleanup",
        scratch_cleanup_started_at,
        json!({}),
    )?;

    let slot_status = if prepared.benchmark_grading_enabled {
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

fn write_trial_summary(
    trial_dir: &Path,
    run_id: &str,
    trial_id: &str,
    variant_id: &str,
    task_id: &str,
    repl_idx: usize,
    outcome: &str,
    agent_exit_status: &str,
    agent_outcome: &str,
    grade_error_reason: Option<&str>,
    trial_conclusion_row: Option<&Value>,
    primary_metric_name: &str,
    primary_metric_value: &Value,
    metrics: &Value,
    declared_artifacts: &Value,
) -> Result<()> {
    let grader_outcome = if let Some(reason) = grade_error_reason {
        json!({
            "status": "error",
            "reason": reason,
            "mapped_output": "grader/mapped_output.json"
        })
    } else if let Some(row) = trial_conclusion_row {
        json!({
            "status": row.pointer("/reported_outcome").and_then(Value::as_str).unwrap_or("unknown"),
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
    if let (Some(base), Some(extra)) = (artifacts.as_object_mut(), declared_artifacts.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }

    let summary = json!({
        "schema_version": "trial_summary_v1",
        "ids": {
            "run_id": run_id,
            "trial_id": trial_id,
            "variant_id": variant_id,
            "task_id": task_id,
            "repl_idx": repl_idx
        },
        "outcome": outcome,
        "primary_metric": {
            "name": primary_metric_name,
            "value": primary_metric_value
        },
        "agent": {
            "outcome": agent_outcome,
            "exit_status": agent_exit_status,
            "result": "agent/result.json",
            "events": "agent/events.jsonl",
            "stdout": "agent/stdout.log",
            "stderr": "agent/stderr.log"
        },
        "grader": grader_outcome,
        "artifacts": artifacts,
        "metrics": metrics
    });
    atomic_write_json_pretty(&trial_summary_path(trial_dir), &summary)
}

fn declared_summary_artifacts(experiment: &Value) -> Value {
    let mut artifacts = serde_json::Map::new();
    if let Some(items) = experiment
        .pointer("/benchmark/artifacts")
        .and_then(Value::as_array)
    {
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
    Value::Object(artifacts)
}

fn path_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|metadata| metadata.len())
}

fn primary_metric_is_null(value: &Value) -> bool {
    matches!(value, Value::Null)
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
    outcome: &str,
    agent_exit_status: &str,
    agent_outcome: &str,
    result_present: bool,
    result_parse_error: Option<&str>,
    grade_error_reason: Option<&str>,
    trial_conclusion_row: Option<&Value>,
    primary_metric_name: &str,
    primary_metric_value: &Value,
) -> Result<Value> {
    let captured_patch_path = prepared.trial_paths.out.join("candidate.patch");
    let captured_patch_bytes = path_size(&captured_patch_path);
    let patch_scope = trial_conclusion_row
        .and_then(|row| row.pointer("/payload/candidate_patch_scope"))
        .cloned()
        .unwrap_or(json!(null));
    let scoped_patch_bytes = patch_scope.pointer("/scoped_bytes").and_then(Value::as_u64);
    let artifact_type = artifact_type_from_trial_input_path(&prepared.io_paths.trial_input_host)?;
    let agent_result_artifact =
        extract_candidate_artifact_record(trial_output, result_present, artifact_type.clone());
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

    let agent_status = if agent_exit_status == "0" && result_present && result_parse_error.is_none()
    {
        "ok"
    } else {
        "error"
    };
    let grader_execution_status = if !prepared.benchmark_grading_enabled {
        "not_run"
    } else if grade_error_reason.is_some() {
        "error"
    } else if trial_conclusion_row.is_some() {
        "ok"
    } else {
        "error"
    };
    let grade_mapping_status = if !prepared.benchmark_grading_enabled {
        "not_run"
    } else if grade_error_reason.is_none() && trial_conclusion_row.is_some() {
        "ok"
    } else {
        "error"
    };
    let score_trust = if prepared.benchmark_grading_enabled {
        if grade_mapping_status == "ok" && !primary_metric_is_null(primary_metric_value) {
            "trusted"
        } else {
            "untrusted"
        }
    } else if result_present
        && result_parse_error.is_none()
        && !primary_metric_is_null(primary_metric_value)
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

    let score_source = if prepared.benchmark_grading_enabled {
        if trial_conclusion_row.is_some() {
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
            "metric": primary_metric_name,
            "value": primary_metric_value,
            "source": score_source,
            "outcome": outcome
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
                "exit_status": agent_exit_status,
                "outcome": agent_outcome,
                "result_parse_error": result_parse_error
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
                "strategy": request.benchmark_config.grader.as_ref().map(|grader| grading_strategy_name(&grader.strategy)),
                "stderr": "grader/stderr.log",
                "stdout": "grader/stdout.log",
                "error": grade_error_reason
            },
            "grade_mapping": {
                "status": grade_mapping_status,
                "mapped_output_present": mapped_output_path.exists(),
                "reported_outcome": trial_conclusion_row
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
) -> Vec<ContractStageRow> {
    let recorded_at = Utc::now().to_rfc3339();
    let overall_status = contract_trace
        .pointer("/overall_status")
        .cloned()
        .unwrap_or(json!("unknown"));
    let score_trust = contract_trace
        .pointer("/score_trust")
        .cloned()
        .unwrap_or(json!("untrusted"));
    let score = contract_trace
        .pointer("/score")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let Some(stages) = contract_trace.pointer("/stages").and_then(Value::as_object) else {
        return Vec::new();
    };

    [
        "task_mapping",
        "agent_execution",
        "artifact_extraction",
        "grader_execution",
        "grade_mapping",
    ]
    .into_iter()
    .filter_map(|stage| {
        let mut detail = stages.get(stage)?.clone();
        let status = detail
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        if stage == "grade_mapping" {
            if let Some(obj) = detail.as_object_mut() {
                obj.insert("overall_status".to_string(), overall_status.clone());
                obj.insert("score_trust".to_string(), score_trust.clone());
                obj.insert("score".to_string(), score.clone());
            }
        }
        Some(ContractStageRow {
            run_id: run_id.to_string(),
            trial_id: trial_id.to_string(),
            schedule_idx,
            slot_commit_id: String::new(),
            attempt: 0,
            row_seq: 0,
            variant_id: variant_id.to_string(),
            task_id: task_id.to_string(),
            repl_idx,
            stage: stage.to_string(),
            status,
            recorded_at: recorded_at.clone(),
            detail,
        })
    })
    .enumerate()
    .map(|(row_seq, mut row)| {
        row.row_seq = row_seq;
        row
    })
    .collect()
}
