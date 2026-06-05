use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use lab_core::{
    canonical_json_digest, ensure_dir, ArtifactStore, BUCEPHALUS_CONTRACT_IN_DIR,
    BUCEPHALUS_CONTRACT_OUT_DIR, BUCEPHALUS_CONTRACT_STATE_DIR,
    BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER,
};
use lab_provenance::{default_attestation, write_attestation};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::*;
use crate::experiment::commit::*;
use crate::experiment::control::*;
use crate::experiment::lease::{
    acquire_run_operation_lease, adopt_engine_lease_for_recovery,
    start_engine_lease_heartbeat_with_writer, RunOperationType,
};
use crate::experiment::preflight::*;
use crate::experiment::runtime::*;
use crate::experiment::state::*;
use crate::model::*;
use crate::package::sealed::*;
use crate::package::validate::*;
use crate::persistence::backend::{
    load_pending_trial_completion_records, load_slot_commit_records, open_attempt_object_store,
    open_lineage_store, open_run_sink, open_runtime_operation_store, open_schedule_slot_read_store,
    open_schedule_slot_store, open_trial_attempt_store, persist_pending_trial_completions,
    run_store_location,
};
use crate::persistence::journal::*;
use crate::persistence::rows::*;
use crate::persistence::writer::RunStoreWriterGuard;
use crate::trial::execution::local_docker::LocalDockerExecutionBackend;
use crate::trial::execution::{
    configure_host_grader_max_concurrency, ExecutionBackend, TrialRunRequest,
    TrialRuntimeExecutionRequest,
};
use crate::trial::grade::{agent_response_execution_outcome, grading_retry_inputs};
use crate::trial::prepare::{
    build_runtime_contract_env, load_prepared_task_environment_manifest, prepare_io_paths,
    prepare_task_environment, resolve_trial_timeout_ms, PreparedTaskEnvironment, TrialPaths,
};
use crate::trial::schedule::*;
use crate::trial::spec::{
    materialize_packaged_task_boundary, validate_task_boundary_workspace_materialization,
};
use crate::trial::state::{write_trial_state, TrialPhase, TrialStateGuard};
use crate::util::{output_error_detail, remove_path_if_exists};
use crate::INTERRUPTED;

pub fn continue_run_with_options(
    run_dir: &Path,
    options: RunExecutionOptions,
) -> Result<RunResult> {
    let _op_lease = acquire_run_operation_lease(run_dir, RunOperationType::Continue)?;
    let run_dir = run_dir
        .canonicalize()
        .with_context(|| format!("resolve run directory '{}'", run_dir.display()))?;

    let control = load_run_control(&run_dir)?;
    let run_status = run_control_status(&control);
    let recovered_active_trials = run_control_active_trials(&control);
    match run_status {
        "failed" | "paused" | "interrupted" => {}
        "completed" => return Err(anyhow!("run already completed — nothing to continue")),
        "running" => {
            return Err(anyhow!(
                "run is currently active — cannot continue a running experiment; run `lab recover --run-dir {}` first",
                run_dir.display()
            ));
        }
        other => return Err(anyhow!("unexpected run status: {}", other)),
    }

    let run_id = require_run_control_run_id(&control)?;
    let (_run_store_writer_guard, run_store_writer) =
        RunStoreWriterGuard::start(&run_dir, &run_id)?;
    let _run_store_writer_scope =
        crate::trial::execution::RunStoreWriterScope::install(run_store_writer.clone());
    let _engine_lease_guard =
        start_engine_lease_heartbeat_with_writer(&run_dir, &run_id, Some(run_store_writer))?;
    let run_session = load_run_session_state(&run_dir)?;
    if run_session.run_id != run_id {
        return Err(anyhow!(
            "run session state mismatch: run_control has {}, run_session_state has {}",
            run_id,
            run_session.run_id
        ));
    }
    let behavior = run_session.behavior;
    let persisted_execution = run_session.execution;
    let execution = normalize_execution_options(&RunExecutionOptions {
        executor: persisted_execution.executor,
        materialize: persisted_execution.materialize,
        run_root: None,
        runtime_env: options.runtime_env,
        runtime_env_files: options.runtime_env_files,
        secret_files: options.secret_files,
        stdout_progress: options.stdout_progress,
    });
    ensure_supported_executor(&execution)?;

    let progress = load_schedule_progress(&run_dir)?;
    let completed_schedule_count = progress
        .completed_slots
        .iter()
        .map(|slot| slot.schedule_index)
        .collect::<HashSet<_>>()
        .len();
    if completed_schedule_count >= progress.total_slots {
        return Err(anyhow!(
            "all {} schedule slots were already processed — nothing to continue",
            progress.total_slots
        ));
    }

    let resolved_path = run_dir.join("resolved_experiment.json");
    let json_value: Value = serde_json::from_slice(&fs::read(&resolved_path)?)?;
    let policy_config = parse_policies(&json_value);
    let max_concurrency = experiment_max_concurrency(&json_value);
    let project_root = run_session.project_root.canonicalize().with_context(|| {
        format!(
            "resolve project root '{}'",
            run_session.project_root.display()
        )
    })?;

    let workload_type = experiment_workload_type(&json_value)?;

    if !matches!(policy_config.state, StatePolicy::IsolatePerTrial) {
        return Err(anyhow!(
            "continue_run only supports IsolatePerTrial state policy; \
             this run uses {:?} — chain state recovery is not yet implemented",
            policy_config.state
        ));
    }

    let (variants, baseline_id) = load_run_variants(&run_dir, &json_value)?;
    write_resolved_variants(&run_dir, &json_value, &baseline_id, &variants)?;
    let exp_dir = resolved_path
        .parent()
        .ok_or_else(|| anyhow!("resolved_experiment.json has no parent directory"))?
        .to_path_buf();
    let dataset_path = resolve_dataset_path_in_package(&json_value, &exp_dir)?;
    let tasks = load_tasks(&dataset_path, &json_value)?;
    let replications = json_value
        .pointer("/matrix/repeats")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("missing /matrix/repeats"))? as usize;
    let random_seed = experiment_random_seed(&json_value);

    let reconstructed_schedule = build_trial_schedule(
        variants.len(),
        tasks.len(),
        replications,
        policy_config.scheduling,
        random_seed,
    );

    if reconstructed_schedule != progress.schedule {
        return Err(anyhow!(
            "schedule mismatch — the experiment configuration has changed since this run was \
             created; cannot safely continue (reconstructed {} slots vs stored {})",
            reconstructed_schedule.len(),
            progress.schedule.len()
        ));
    }

    let schedule = reconstructed_schedule;
    write_resolved_schedule(&run_dir, &schedule)?;
    open_schedule_slot_store(&run_dir)?.ensure_schedule_slots(&run_id, &schedule)?;
    let materialize_mode = execution
        .materialize
        .unwrap_or(MaterializationMode::OutputsOnly);

    write_run_control(&run_dir, &run_id, "running", &[], None)?;
    let mut run_guard = RunControlGuard::new(&run_dir, &run_id);

    let mut variant_runtime_profiles = Vec::with_capacity(variants.len());
    for variant in &variants {
        let profile =
            resolve_variant_runtime_profile(&json_value, variant, &exp_dir, &behavior, &execution)?;
        ensure_required_runtime_env_present(&profile.agent_runtime, &profile.agent_runtime_env)?;
        variant_runtime_profiles.push(profile);
    }
    let run_integration_level = variant_runtime_profiles
        .first()
        .map(|profile| profile.agent_runtime.integration_level.clone())
        .unwrap_or_else(|| "cli_basic".to_string());
    let isolation_grade = resolve_run_isolation_grade(&variant_runtime_profiles, &behavior);

    let evaluation_config = parse_evaluation_config(&json_value)?;
    let metric_definitions = parse_metric_definitions(&json_value)?;

    let mut consecutive_failures: BTreeMap<usize, usize> = progress.consecutive_failures.clone();
    let mut pruned_variants: HashSet<usize> = progress.pruned_variants.iter().copied().collect();

    let trials_dir = run_dir.join("trials");
    ensure_dir(&trials_dir)?;
    let evidence_dir = run_dir.join("runtime").join("durable_rows");
    let evidence_records_path = evidence_dir.join("evidence_records.row.json");
    let task_chain_states_path = evidence_dir.join("task_chain_states.row.json");
    let mut run_sink = open_run_sink(&run_dir)?;
    run_sink.write_run_manifest(&RunManifestRecord {
        schema_version: "run_manifest_v1".to_string(),
        run_id: run_id.clone(),
        created_at: Utc::now().to_rfc3339(),
        workload_type: workload_type.clone(),
        baseline_id: baseline_id.clone(),
        variant_ids: variants.iter().map(|variant| variant.id.clone()).collect(),
    })?;
    run_sink.write_metric_definitions(&metric_definition_records(
        &json_value,
        &metric_definitions,
    )?)?;

    let mut schedule_progress = progress.clone();
    let recovered_max_trial_index = recovered_active_trials
        .iter()
        .filter_map(|active| trial_index_from_trial_id(&active.trial_id))
        .max()
        .unwrap_or(0);
    let mut trial_index: usize = schedule_progress
        .next_trial_index
        .max(recovered_max_trial_index);

    let schedule_outcome = execute_schedule_engine(
        &run_dir,
        &run_id,
        &workload_type,
        &project_root,
        &dataset_path,
        &variants,
        &tasks,
        &schedule,
        &policy_config,
        &evaluation_config,
        &metric_definitions,
        &variant_runtime_profiles,
        resolved_executor_kind(&execution),
        &behavior,
        materialize_mode,
        &policy_config.task_boundary,
        &trials_dir,
        &evidence_dir,
        &evidence_records_path,
        &task_chain_states_path,
        &mut schedule_progress,
        &mut trial_index,
        &mut consecutive_failures,
        &mut pruned_variants,
        &recovered_active_trials,
        &baseline_id,
        &mut *run_sink,
        max_concurrency,
        execution.stdout_progress,
    )?;
    run_sink.flush()?;
    if schedule_outcome != ScheduleEngineOutcome::Completed {
        match schedule_outcome {
            ScheduleEngineOutcome::Interrupted => {
                run_guard.complete("interrupted")?;
            }
            _ => {
                run_guard.disarm();
            }
        }
        return Ok(RunResult {
            run_dir: run_dir.to_path_buf(),
            run_id,
            account_db_path: run_store_location(&run_dir)?.into(),
        });
    }

    let (_project_root, _evaluation_config, _evidence_records_path, _task_chain_states_path) = (
        project_root,
        evaluation_config,
        evidence_records_path,
        task_chain_states_path,
    );

    let resolved_digest = canonical_json_digest(&json_value);
    if isolation_grade != "hermetic" {
        run_guard.complete("invalid_isolation")?;
        return Err(anyhow!(
            "scientific run completed without hermetic isolation (got {})",
            isolation_grade
        ));
    }
    let grades = json!({
        "schema_version": "grades_v1",
        "integration_level": run_integration_level,
        "replay_grade": "best_effort",
        "isolation_grade": isolation_grade,
        "comparability_grade": "unknown",
        "provenance_grade": "recorded",
        "privacy_grade": "unknown"
    });

    let att = default_attestation(
        &resolved_digest,
        None,
        grades.clone(),
        vec![],
        json!({"name": "unknown"}),
        "events",
    );
    write_attestation(&run_dir, att)?;
    run_guard.complete("completed")?;

    Ok(RunResult {
        run_dir: run_dir.to_path_buf(),
        run_id,
        account_db_path: run_store_location(&run_dir)?.into(),
    })
}

pub(crate) fn trial_index_from_trial_id(trial_id: &str) -> Option<usize> {
    trial_id
        .strip_prefix("trial_")
        .and_then(|suffix| suffix.parse::<usize>().ok())
        .filter(|idx| *idx > 0)
}

fn resolved_experiment_id(json_value: &Value) -> Result<String> {
    json_value
        .pointer("/experiment/id")
        .or_else(|| json_value.pointer("/id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("missing /experiment/id for metric definition registry"))
}

fn metric_definition_records(
    json_value: &Value,
    definitions: &[MetricDefinition],
) -> Result<Vec<MetricDefinitionRecord>> {
    if definitions.is_empty() {
        return Ok(Vec::new());
    }
    let experiment_id = resolved_experiment_id(json_value)?;
    Ok(definitions
        .iter()
        .map(|definition| MetricDefinitionRecord {
            schema_version: "metric_definition_v1".to_string(),
            experiment_id: experiment_id.clone(),
            metric_id: definition.id.clone(),
            semantic_key: definition.semantic_key.clone(),
            label: definition.label.clone(),
            value_type: definition.value_type.clone(),
            unit: definition.unit.clone(),
            direction: definition.direction.clone(),
            source_type: definition.source.source_type.clone(),
            source_pointer: definition.source.pointer.clone(),
            required: definition.required,
            primary: definition.primary,
            definition: definition.definition_json.clone(),
        })
        .collect())
}

#[derive(Clone)]
pub(crate) struct ParallelWorkerExecutionContext {
    run_dir: PathBuf,
    run_id: String,
    workload_type: String,
    project_root: PathBuf,
    variants: Vec<Variant>,
    tasks: Vec<Value>,
    policy_config: PolicyConfig,
    evaluation_config: EvaluationConfig,
    metric_definitions: Vec<MetricDefinition>,
    variant_runtime_profiles: Vec<VariantRuntimeProfile>,
    executor_kind: ExecutorKind,
    materialize_mode: MaterializationMode,
    trials_dir: PathBuf,
    baseline_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct InFlightDispatch {
    schedule_idx: usize,
    trial_id: String,
    variant_idx: usize,
    variant_id: String,
    worker_id: String,
    started_at: String,
}

pub(crate) struct LocalTrialLaunch {
    pub(crate) schedule_idx: usize,
    pub(crate) trial_id: String,
    slot: TrialSlot,
    trial_paths: TrialPaths,
    dispatched_at: Instant,
}

#[derive(Debug)]
pub(crate) struct LocalTrialCompletion {
    worker_id: String,
    trial_id: String,
    schedule_idx: usize,
    completed_at: Instant,
    result: std::result::Result<TrialExecutionResult, String>,
}

#[derive(Debug)]
enum LocalWorkerEvent {
    Claimed {
        active: RunControlActiveTrial,
        timing: WorkerClaimTiming,
    },
    Completed(LocalTrialCompletion),
    SkippedPruned {
        worker_id: String,
        schedule_idx: usize,
    },
    Exited {
        worker_id: String,
    },
    Fatal {
        worker_id: String,
        detail: String,
    },
}

#[derive(Debug)]
struct WorkerClaimTiming {
    claim_wait_ms: f64,
    claim_intent_persist_ms: f64,
    completion_to_claim_ms: Option<f64>,
    previous_completion: Option<(Instant, String, usize)>,
}

pub(crate) fn trial_claim_intent_path(trial_dir: &Path) -> PathBuf {
    trial_dir.join("runner").join("claim_intent.json")
}

fn write_trial_claim_intent(
    run_dir: &Path,
    run_id: &str,
    launch: &LocalTrialLaunch,
    active: &RunControlActiveTrial,
) -> Result<()> {
    let trial_dir = run_dir.join("trials").join(&launch.trial_id);
    atomic_write_json_pretty(
        &trial_claim_intent_path(&trial_dir),
        &json!({
            "schema_version": "trial_claim_intent_v1",
            "run_id": run_id,
            "trial_id": launch.trial_id,
            "schedule_idx": launch.schedule_idx,
            "worker_id": active.worker_id,
            "variant_id": active.variant_id,
            "started_at": active.started_at,
            "created_at": Utc::now().to_rfc3339(),
        }),
    )
}

fn load_trial_claim_intents(run_dir: &Path) -> Result<Vec<RunControlActiveTrial>> {
    let trials_dir = run_dir.join("trials");
    if !trials_dir.exists() {
        return Ok(Vec::new());
    }
    let mut intents = Vec::new();
    for entry in fs::read_dir(&trials_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = trial_claim_intent_path(&entry.path());
        if !path.exists() {
            continue;
        }
        let value = load_json_file(&path)?;
        if value.pointer("/schema_version").and_then(Value::as_str) != Some("trial_claim_intent_v1")
        {
            continue;
        }
        let Some(trial_id) = value
            .pointer("/trial_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let schedule_idx = value
            .pointer("/schedule_idx")
            .and_then(Value::as_u64)
            .map(|idx| idx as usize);
        let worker_id = value
            .pointer("/worker_id")
            .and_then(Value::as_str)
            .unwrap_or(RUN_CONTROL_UNKNOWN_WORKER_ID)
            .to_string();
        let variant_id = value
            .pointer("/variant_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let started_at = value
            .pointer("/started_at")
            .and_then(Value::as_str)
            .map(str::to_string);
        intents.push(RunControlActiveTrial {
            trial_id,
            worker_id,
            schedule_idx,
            variant_id,
            started_at,
            #[cfg(test)]
            control: None,
        });
    }
    intents.sort_by_key(|intent| intent.schedule_idx.unwrap_or(usize::MAX));
    Ok(intents)
}

struct SchedulerPerfSample {
    trial_id: Option<String>,
    schedule_idx: Option<usize>,
    attempt: Option<usize>,
    stage: &'static str,
    duration_ms: f64,
    detail: Value,
}

#[derive(Default)]
struct SchedulerPerfBuffer {
    samples: Vec<SchedulerPerfSample>,
}

impl SchedulerPerfBuffer {
    fn record_value(
        &mut self,
        trial_id: Option<&str>,
        schedule_idx: Option<usize>,
        attempt: Option<usize>,
        stage: &'static str,
        duration_ms: f64,
        detail: Value,
    ) {
        self.samples.push(SchedulerPerfSample {
            trial_id: trial_id.map(str::to_string),
            schedule_idx,
            attempt,
            stage,
            duration_ms,
            detail,
        });
    }

    fn record_duration(
        &mut self,
        trial_id: Option<&str>,
        schedule_idx: Option<usize>,
        attempt: Option<usize>,
        stage: &'static str,
        started: Instant,
        detail: Value,
    ) {
        self.samples.push(SchedulerPerfSample {
            trial_id: trial_id.map(str::to_string),
            schedule_idx,
            attempt,
            stage,
            duration_ms: started.elapsed().as_secs_f64() * 1000.0,
            detail,
        });
    }

    fn flush(&mut self, run_dir: &Path, run_id: &str) -> Result<()> {
        for sample in self.samples.drain(..) {
            crate::perf::record(crate::perf::PerfRecord {
                run_dir,
                run_id,
                trial_id: sample.trial_id.as_deref(),
                schedule_idx: sample.schedule_idx,
                attempt: sample.attempt,
                sample_kind: "duration",
                stage: sample.stage,
                duration_ms: Some(sample.duration_ms),
                detail: sample.detail,
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrokerSlotState {
    Pending,
    Active,
    CompletedPendingCommit,
    Committed,
    BlockedActive,
}

struct SlotBrokerState {
    slot_states: Vec<BrokerSlotState>,
    in_flight: HashMap<String, InFlightDispatch>,
    in_flight_by_variant: BTreeMap<usize, usize>,
    pruned_variants: HashSet<usize>,
    trial_index: usize,
    accepting: bool,
}

#[derive(Clone)]
pub(crate) struct SlotBroker {
    project_root: PathBuf,
    trials_dir: PathBuf,
    variants: Vec<Variant>,
    schedule: Vec<TrialSlot>,
    max_in_flight_per_variant: Option<usize>,
    inner: Arc<(Mutex<SlotBrokerState>, Condvar)>,
}

pub(crate) enum PulledWork {
    Trial {
        launch: LocalTrialLaunch,
        active: RunControlActiveTrial,
    },
    SkippedPruned {
        schedule_idx: usize,
    },
}

impl SlotBroker {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        run_dir: &Path,
        run_id: &str,
        project_root: &Path,
        trials_dir: &Path,
        variants: &[Variant],
        schedule: &[TrialSlot],
        committed_schedules: &HashSet<usize>,
        pending_completion_schedules: &HashSet<usize>,
        initial_pruned_variants: &HashSet<usize>,
        trial_index: usize,
    ) -> Result<Self> {
        let store = open_schedule_slot_read_store(run_dir)?;
        let mut slot_states = Vec::with_capacity(schedule.len());
        for schedule_idx in 0..schedule.len() {
            if committed_schedules.contains(&schedule_idx) {
                slot_states.push(BrokerSlotState::Committed);
                continue;
            }
            if pending_completion_schedules.contains(&schedule_idx) {
                slot_states.push(BrokerSlotState::CompletedPendingCommit);
                continue;
            }
            let state = store
                .schedule_slot(run_id, schedule_idx)?
                .map(|slot| slot.state)
                .unwrap_or_else(|| "pending".to_string());
            slot_states.push(match state.as_str() {
                "pending" => BrokerSlotState::Pending,
                "committed" => BrokerSlotState::Committed,
                "active" => BrokerSlotState::BlockedActive,
                _ => BrokerSlotState::Pending,
            });
        }
        Ok(Self {
            project_root: project_root.to_path_buf(),
            trials_dir: trials_dir.to_path_buf(),
            variants: variants.to_vec(),
            schedule: schedule.to_vec(),
            max_in_flight_per_variant: None,
            inner: Arc::new((
                Mutex::new(SlotBrokerState {
                    slot_states,
                    in_flight: HashMap::new(),
                    in_flight_by_variant: BTreeMap::new(),
                    pruned_variants: initial_pruned_variants.clone(),
                    trial_index,
                    accepting: true,
                }),
                Condvar::new(),
            )),
        })
    }

    fn with_variant_limit(mut self, limit: Option<usize>) -> Self {
        self.max_in_flight_per_variant = limit;
        self
    }

    fn rollback_claim(
        &self,
        schedule_idx: usize,
        trial_id: &str,
        variant_idx: usize,
        next_state: BrokerSlotState,
    ) {
        let (lock, cv) = &*self.inner;
        let mut state = lock.lock().expect("slot broker mutex poisoned");
        state.in_flight.remove(trial_id);
        if let Some(count) = state.in_flight_by_variant.get_mut(&variant_idx) {
            if *count > 0 {
                *count -= 1;
            }
            if *count == 0 {
                state.in_flight_by_variant.remove(&variant_idx);
            }
        }
        if let Some(slot_state) = state.slot_states.get_mut(schedule_idx) {
            *slot_state = next_state;
        }
        cv.notify_all();
    }

    pub(crate) fn claim_next(&self, worker_id: &str) -> Result<Option<PulledWork>> {
        let (lock, cv) = &*self.inner;
        let mut state = lock.lock().expect("slot broker mutex poisoned");
        loop {
            if !state.accepting {
                return Ok(None);
            }
            let mut blocked_by_capacity = false;
            for schedule_idx in 0..self.schedule.len() {
                if state.slot_states[schedule_idx] != BrokerSlotState::Pending {
                    continue;
                }
                let slot = &self.schedule[schedule_idx];
                if state.pruned_variants.contains(&slot.variant_idx) {
                    state.slot_states[schedule_idx] = BrokerSlotState::CompletedPendingCommit;
                    return Ok(Some(PulledWork::SkippedPruned { schedule_idx }));
                }
                if let Some(limit) = self.max_in_flight_per_variant {
                    let variant_in_flight = state
                        .in_flight_by_variant
                        .get(&slot.variant_idx)
                        .copied()
                        .unwrap_or(0);
                    if variant_in_flight >= limit {
                        blocked_by_capacity = true;
                        continue;
                    }
                }

                let next_trial_index = state.trial_index.saturating_add(1);
                let trial_id = format!("trial_{}", next_trial_index);
                let variant = &self.variants[slot.variant_idx];
                state.trial_index = next_trial_index;
                state.slot_states[schedule_idx] = BrokerSlotState::Active;
                let started_at = Utc::now().to_rfc3339();
                let dispatch = InFlightDispatch {
                    schedule_idx,
                    trial_id: trial_id.clone(),
                    variant_idx: slot.variant_idx,
                    variant_id: variant.id.clone(),
                    worker_id: worker_id.to_string(),
                    started_at: started_at.clone(),
                };
                state.in_flight.insert(trial_id.clone(), dispatch.clone());
                *state
                    .in_flight_by_variant
                    .entry(slot.variant_idx)
                    .or_default() += 1;
                let slot = slot.clone();
                let variant_id = variant.id.clone();
                let active = RunControlActiveTrial {
                    trial_id: trial_id.clone(),
                    worker_id: worker_id.to_string(),
                    schedule_idx: Some(schedule_idx),
                    variant_id: Some(variant_id),
                    started_at: Some(started_at),
                    #[cfg(test)]
                    control: None,
                };
                drop(state);

                let trial_paths = match (|| -> Result<TrialPaths> {
                    let trial_dir = self.trials_dir.join(&trial_id);
                    ensure_dir(&trial_dir)?;
                    let trial_paths = TrialPaths::new(&trial_dir, &self.project_root)?;
                    trial_paths.prepare(false)?;
                    Ok(trial_paths)
                })() {
                    Ok(trial_paths) => trial_paths,
                    Err(err) => {
                        self.rollback_claim(
                            schedule_idx,
                            &trial_id,
                            slot.variant_idx,
                            BrokerSlotState::Pending,
                        );
                        return Err(err);
                    }
                };
                let launch = LocalTrialLaunch {
                    schedule_idx,
                    trial_id,
                    slot,
                    trial_paths,
                    dispatched_at: Instant::now(),
                };
                return Ok(Some(PulledWork::Trial { launch, active }));
            }
            if !blocked_by_capacity || state.in_flight.is_empty() {
                return Ok(None);
            }
            state = cv
                .wait(state)
                .expect("slot broker mutex poisoned while waiting");
        }
    }

    pub(crate) fn complete_owned(
        &self,
        worker_id: &str,
        trial_id: &str,
        schedule_idx: usize,
    ) -> Result<()> {
        let (lock, cv) = &*self.inner;
        let mut state = lock.lock().expect("slot broker mutex poisoned");
        let dispatch = state.in_flight.get(trial_id).ok_or_else(|| {
            anyhow!(
                "slot broker ownership fault: completion for unknown active trial {}",
                trial_id
            )
        })?;
        if dispatch.worker_id != worker_id || dispatch.schedule_idx != schedule_idx {
            return Err(anyhow!(
                "slot broker ownership fault: worker {} completed trial {} schedule_idx {}, but active owner is worker {} schedule_idx {}",
                worker_id,
                trial_id,
                schedule_idx,
                dispatch.worker_id,
                dispatch.schedule_idx
            ));
        }
        let dispatch = state
            .in_flight
            .remove(trial_id)
            .expect("validated in-flight trial disappeared");
        if let Some(count) = state.in_flight_by_variant.get_mut(&dispatch.variant_idx) {
            if *count > 0 {
                *count -= 1;
            }
            if *count == 0 {
                state.in_flight_by_variant.remove(&dispatch.variant_idx);
            }
        }
        if let Some(slot_state) = state.slot_states.get_mut(schedule_idx) {
            *slot_state = BrokerSlotState::CompletedPendingCommit;
        }
        cv.notify_all();
        Ok(())
    }

    fn release_owned_to_pending(
        &self,
        worker_id: &str,
        trial_id: &str,
        schedule_idx: usize,
    ) -> Result<()> {
        let (lock, cv) = &*self.inner;
        let mut state = lock.lock().expect("slot broker mutex poisoned");
        let dispatch = state.in_flight.get(trial_id).ok_or_else(|| {
            anyhow!(
                "slot broker ownership fault: release for unknown active trial {}",
                trial_id
            )
        })?;
        if dispatch.worker_id != worker_id || dispatch.schedule_idx != schedule_idx {
            return Err(anyhow!(
                "slot broker ownership fault: worker {} released trial {} schedule_idx {}, but active owner is worker {} schedule_idx {}",
                worker_id,
                trial_id,
                schedule_idx,
                dispatch.worker_id,
                dispatch.schedule_idx
            ));
        }
        let dispatch = state
            .in_flight
            .remove(trial_id)
            .expect("validated in-flight trial disappeared");
        if let Some(count) = state.in_flight_by_variant.get_mut(&dispatch.variant_idx) {
            if *count > 0 {
                *count -= 1;
            }
            if *count == 0 {
                state.in_flight_by_variant.remove(&dispatch.variant_idx);
            }
        }
        if let Some(slot_state) = state.slot_states.get_mut(schedule_idx) {
            *slot_state = BrokerSlotState::Pending;
        }
        cv.notify_all();
        Ok(())
    }

    fn mark_committed(&self, schedule_idx: usize) {
        let (lock, cv) = &*self.inner;
        let mut state = lock.lock().expect("slot broker mutex poisoned");
        if let Some(slot_state) = state.slot_states.get_mut(schedule_idx) {
            *slot_state = BrokerSlotState::Committed;
        }
        cv.notify_all();
    }

    fn stop_accepting(&self) {
        let (lock, cv) = &*self.inner;
        let mut state = lock.lock().expect("slot broker mutex poisoned");
        state.accepting = false;
        cv.notify_all();
    }

    fn update_pruned_variants(&self, pruned_variants: &HashSet<usize>) {
        let (lock, cv) = &*self.inner;
        let mut state = lock.lock().expect("slot broker mutex poisoned");
        state.pruned_variants = pruned_variants.clone();
        cv.notify_all();
    }

    pub(crate) fn active_trials(&self) -> Vec<RunControlActiveTrial> {
        let (lock, _) = &*self.inner;
        in_flight_active_trials(&lock.lock().expect("slot broker mutex poisoned").in_flight)
    }

    fn in_flight_map(&self) -> HashMap<String, InFlightDispatch> {
        let (lock, _) = &*self.inner;
        lock.lock()
            .expect("slot broker mutex poisoned")
            .in_flight
            .clone()
    }

    fn has_unfinished_slots(&self) -> bool {
        let (lock, _) = &*self.inner;
        let state = lock.lock().expect("slot broker mutex poisoned");
        state
            .slot_states
            .iter()
            .any(|slot| matches!(slot, BrokerSlotState::Pending | BrokerSlotState::Active))
    }

    fn trial_index(&self) -> usize {
        let (lock, _) = &*self.inner;
        lock.lock().expect("slot broker mutex poisoned").trial_index
    }
}

pub(crate) fn in_flight_active_trials(
    in_flight: &HashMap<String, InFlightDispatch>,
) -> Vec<RunControlActiveTrial> {
    let mut active: Vec<RunControlActiveTrial> = in_flight
        .values()
        .map(|item| RunControlActiveTrial {
            trial_id: item.trial_id.clone(),
            worker_id: item.worker_id.clone(),
            schedule_idx: Some(item.schedule_idx),
            variant_id: Some(item.variant_id.clone()),
            started_at: Some(item.started_at.clone()),
            #[cfg(test)]
            control: None,
        })
        .collect();
    active.sort_by_key(|entry| entry.schedule_idx.unwrap_or(usize::MAX));
    active
}

pub(crate) fn cleanup_in_flight_trial_containers(
    run_dir: &Path,
    run_id: &str,
    trials_dir: &Path,
    in_flight: &HashMap<String, InFlightDispatch>,
) -> Result<Vec<String>> {
    let mut cleaned = Vec::new();
    let mut errors = Vec::new();
    for dispatch in in_flight.values() {
        let trial_dir = trials_dir.join(&dispatch.trial_id);
        match cleanup_trial_runtime_required(run_dir, run_id, &dispatch.trial_id, &trial_dir) {
            Ok(true) => {
                emit_run_log(
                    run_id,
                    format!(
                        "cleaned runtime worker(s) for in-flight {} during scheduler shutdown",
                        dispatch.trial_id
                    ),
                );
                cleaned.push(dispatch.trial_id.clone());
            }
            Ok(false) => {}
            Err(err) => errors.push(format!("{}: {}", dispatch.trial_id, err)),
        }
    }
    if errors.is_empty() {
        Ok(cleaned)
    } else {
        Err(anyhow!(
            "failed to clean in-flight runtime worker(s): {}",
            errors.join("; ")
        ))
    }
}

pub(crate) fn execute_local_trial(
    context: &ParallelWorkerExecutionContext,
    launch: LocalTrialLaunch,
) -> Result<TrialExecutionResult> {
    let worker_started_at = Instant::now();
    crate::perf::record_duration(
        &context.run_dir,
        &context.run_id,
        Some(&launch.trial_id),
        Some(launch.schedule_idx),
        Some(0),
        "dispatch_to_worker_thread_start",
        launch.dispatched_at,
        json!({
            "variant_idx": launch.slot.variant_idx,
            "task_idx": launch.slot.task_idx,
            "repl_idx": launch.slot.repl_idx
        }),
    )?;
    let payload_dir = context
        .run_dir
        .join("runtime")
        .join("worker_payload")
        .join(&launch.trial_id);
    if payload_dir.exists() {
        fs::remove_dir_all(&payload_dir)?;
    }
    ensure_dir(&payload_dir)?;
    let payload_evidence = payload_dir.join("evidence_records.jsonl");
    let payload_chain = payload_dir.join("task_chain_states.jsonl");

    let mut local_trial_index = trial_index_from_trial_id(&launch.trial_id)
        .unwrap_or(launch.schedule_idx + 1)
        .saturating_sub(1);
    let mut local_chain_states: BTreeMap<String, ChainRuntimeState> = BTreeMap::new();
    let mut buffered_sink = BufferedRunSink::default();
    let artifact_store = ArtifactStore::new(context.run_dir.join("artifacts"));
    let execution = (|| -> Result<TrialExecutionResult> {
        let mut request = ScheduledTrialRequest {
            run_dir: &context.run_dir,
            run_id: &context.run_id,
            workload_type: &context.workload_type,
            project_root: &context.project_root,
            variants: &context.variants,
            tasks: &context.tasks,
            schedule_idx: launch.schedule_idx,
            slot: &launch.slot,
            policy_config: &context.policy_config,
            evaluation_config: &context.evaluation_config,
            metric_definitions: &context.metric_definitions,
            variant_runtime_profiles: &context.variant_runtime_profiles,
            executor_kind: context.executor_kind,
            materialize_mode: context.materialize_mode,
            precomputed_trial_paths: Some(launch.trial_paths),
            trials_dir: &context.trials_dir,
            evidence_records_path: &payload_evidence,
            task_chain_states_path: &payload_chain,
            artifact_store: &artifact_store,
            trial_index: &mut local_trial_index,
            chain_states: &mut local_chain_states,
            baseline_id: &context.baseline_id,
            run_sink: &mut buffered_sink,
        };
        let prepare_started_at = Instant::now();
        let mut prepared = prepare_scheduled_trial(&mut request)?;
        crate::perf::record_duration(
            &context.run_dir,
            &context.run_id,
            Some(&launch.trial_id),
            Some(launch.schedule_idx),
            Some(0),
            "trial_prepare",
            prepare_started_at,
            json!({
                "worker_start_to_prepare_start_ms": worker_started_at.elapsed().as_secs_f64() * 1000.0
            }),
        )?;
        let trial_started_at = Instant::now();
        let mut runtime_outcome = None;
        for attempt in 0..context.policy_config.retry_max_attempts {
            let attempt_started_at = Instant::now();
            let outcome =
                execute_scheduled_trial_attempt(&request, &prepared, (attempt + 1) as u32)?;
            crate::perf::record_duration(
                &context.run_dir,
                &context.run_id,
                Some(&launch.trial_id),
                Some(launch.schedule_idx),
                Some((attempt + 1) as usize),
                "trial_attempt_runtime",
                attempt_started_at,
                json!({ "retry_max_attempts": context.policy_config.retry_max_attempts }),
            )?;
            let (retry_outcome, retry_exit_status) = grading_retry_inputs(
                prepared.grading_enabled,
                outcome.trial_conclusion_row.as_ref(),
                outcome.grade_error_reason.as_deref(),
                &outcome.agent_exit_status,
                outcome.result_present,
                outcome.result_parse_error.as_deref(),
            );
            let is_last_attempt = attempt + 1 >= context.policy_config.retry_max_attempts;
            let should_retry = !is_last_attempt
                && should_retry_outcome(
                    &retry_outcome,
                    &retry_exit_status,
                    &context.policy_config.retry_on,
                );
            runtime_outcome = Some(outcome);
            if !should_retry {
                break;
            }
        }
        let runtime_outcome_available_at = Instant::now();
        let finalize_started_at = Instant::now();
        let mut trial_result = finalize_scheduled_trial(
            &mut request,
            &mut prepared,
            runtime_outcome.ok_or_else(|| anyhow!("trial runtime produced no attempt outcome"))?,
            trial_started_at,
        )?;
        crate::perf::record_duration(
            &context.run_dir,
            &context.run_id,
            Some(&trial_result.trial_id),
            Some(launch.schedule_idx),
            Some(0),
            "trial_finalize_and_persist",
            finalize_started_at,
            json!({}),
        )?;
        crate::perf::record_duration(
            &context.run_dir,
            &context.run_id,
            Some(&trial_result.trial_id),
            Some(launch.schedule_idx),
            Some(0),
            "trial_runtime_outcome_to_worker_completion",
            runtime_outcome_available_at,
            json!({
                "boundary": "runtime_outcome_available_until_worker_can_send_completion"
            }),
        )?;
        crate::perf::record_duration(
            &context.run_dir,
            &context.run_id,
            Some(&trial_result.trial_id),
            Some(launch.schedule_idx),
            Some(0),
            "trial_total_worker",
            worker_started_at,
            json!({}),
        )?;
        trial_result.variant_idx = Some(launch.slot.variant_idx);
        trial_result.deferred_trial_records = buffered_sink.trial_records;
        trial_result.deferred_metric_rows = buffered_sink.metric_rows;
        trial_result.deferred_event_rows = buffered_sink.event_rows;
        trial_result.deferred_contract_stage_rows = buffered_sink.contract_stage_rows;
        trial_result.deferred_variant_snapshot_rows = buffered_sink.variant_snapshot_rows;
        trial_result.deferred_evidence_records = load_jsonl_value_rows(&payload_evidence)?;
        trial_result.deferred_chain_state_records = load_jsonl_value_rows(&payload_chain)?;
        Ok(trial_result)
    })();

    if let Err(cleanup_err) = remove_path_if_exists(&payload_dir) {
        if execution.is_ok() {
            return Err(cleanup_err).with_context(|| {
                format!(
                    "failed to remove worker payload directory {}",
                    payload_dir.display()
                )
            });
        }
        eprintln!(
            "failed to remove worker payload directory {} after trial failure: {}",
            payload_dir.display(),
            cleanup_err
        );
    }
    execution
}

fn send_worker_event(
    event_tx: &mpsc::Sender<LocalWorkerEvent>,
    event: LocalWorkerEvent,
    worker_id: &str,
) -> bool {
    if let Err(err) = event_tx.send(event) {
        eprintln!(
            "warning: local worker {} could not report scheduler event: {}",
            worker_id, err
        );
        return false;
    }
    true
}

fn spawn_pull_worker(
    context: Arc<ParallelWorkerExecutionContext>,
    broker: SlotBroker,
    worker_id: String,
    event_tx: mpsc::Sender<LocalWorkerEvent>,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name(worker_id.clone())
        .spawn(move || {
            let mut last_completion: Option<(Instant, String, usize)> = None;
            loop {
                let claim_started_at = Instant::now();
                let work = match broker.claim_next(&worker_id) {
                    Ok(Some(work)) => work,
                    Ok(None) => break,
                    Err(err) => {
                        send_worker_event(
                            &event_tx,
                            LocalWorkerEvent::Fatal {
                                worker_id: worker_id.clone(),
                                detail: err.to_string(),
                            },
                            &worker_id,
                        );
                        break;
                    }
                };
                let claim_returned_at = Instant::now();
                match work {
                    PulledWork::SkippedPruned { schedule_idx } => {
                        if !send_worker_event(
                            &event_tx,
                            LocalWorkerEvent::SkippedPruned {
                                worker_id: worker_id.clone(),
                                schedule_idx,
                            },
                            &worker_id,
                        ) {
                            break;
                        }
                    }
                    PulledWork::Trial { launch, active } => {
                        let trial_id = launch.trial_id.clone();
                        let schedule_idx = launch.schedule_idx;
                        let claim_intent_started_at = Instant::now();
                        if let Err(err) = write_trial_claim_intent(
                            &context.run_dir,
                            &context.run_id,
                            &launch,
                            &active,
                        ) {
                            let detail = match broker.release_owned_to_pending(
                                &worker_id,
                                &trial_id,
                                schedule_idx,
                            ) {
                                Ok(()) => format!(
                                    "failed to persist claim intent for trial {} schedule_idx {} before launch: {}",
                                    trial_id, schedule_idx, err
                                ),
                                Err(release_err) => format!(
                                    "failed to persist claim intent for trial {} schedule_idx {} before launch: {}; release to pending also failed: {}",
                                    trial_id, schedule_idx, err, release_err
                                ),
                            };
                            send_worker_event(
                                &event_tx,
                                LocalWorkerEvent::Fatal {
                                    worker_id: worker_id.clone(),
                                    detail,
                                },
                                &worker_id,
                            );
                            break;
                        }
                        let claim_intent_persist_ms =
                            claim_intent_started_at.elapsed().as_secs_f64() * 1000.0;
                        let previous_completion = last_completion.take();
                        let completion_to_claim_ms =
                            previous_completion.as_ref().map(|(completed_at, _, _)| {
                                claim_returned_at.duration_since(*completed_at).as_secs_f64()
                                    * 1000.0
                            });
                        let timing = WorkerClaimTiming {
                            claim_wait_ms: claim_returned_at
                                .duration_since(claim_started_at)
                                .as_secs_f64()
                                * 1000.0,
                            claim_intent_persist_ms,
                            completion_to_claim_ms,
                            previous_completion,
                        };
                        if !send_worker_event(
                            &event_tx,
                            LocalWorkerEvent::Claimed {
                                active: active.clone(),
                                timing,
                            },
                            &worker_id,
                        ) {
                            if let Err(err) =
                                broker.release_owned_to_pending(&worker_id, &trial_id, schedule_idx)
                            {
                                eprintln!(
                                    "warning: local worker {} could not release {} after scheduler event channel closed: {}",
                                    worker_id, trial_id, err
                                );
                            }
                            break;
                        }
                        let result =
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                execute_local_trial(context.as_ref(), launch)
                            })) {
                                Ok(Ok(result)) => Ok(result),
                                Ok(Err(err)) => Err(err.to_string()),
                                Err(_) => Err("local trial execution panicked".to_string()),
                            };
                        let ownership_result =
                            broker.complete_owned(&worker_id, &trial_id, schedule_idx);
                        let result = match (result, ownership_result) {
                            (Ok(result), Ok(())) => Ok(result),
                            (Err(err), Ok(())) => Err(err),
                            (_, Err(err)) => Err(err.to_string()),
                        };
                        let completed_at = Instant::now();
                        if !send_worker_event(
                            &event_tx,
                            LocalWorkerEvent::Completed(LocalTrialCompletion {
                                worker_id: worker_id.clone(),
                                trial_id: trial_id.clone(),
                                schedule_idx,
                                completed_at,
                                result,
                            }),
                            &worker_id,
                        ) {
                            break;
                        }
                        last_completion = Some((completed_at, trial_id, schedule_idx));
                    }
                }
            }
            send_worker_event(
                &event_tx,
                LocalWorkerEvent::Exited {
                    worker_id: worker_id.clone(),
                },
                &worker_id,
            );
        })
        .map_err(|err| anyhow!("failed to spawn pull worker thread: {}", err))
}

pub(crate) fn load_external_schedule_outcome_request(
    run_dir: &Path,
) -> Result<Option<ScheduleEngineOutcome>> {
    let run_control = load_run_control(run_dir)?;
    let status = run_control_status(&run_control);
    Ok(match status {
        "paused" => Some(ScheduleEngineOutcome::Paused),
        "killed" => Some(ScheduleEngineOutcome::Killed),
        _ => None,
    })
}

pub(crate) fn schedule_engine_status(
    requested_outcome: Option<ScheduleEngineOutcome>,
) -> &'static str {
    match requested_outcome {
        Some(ScheduleEngineOutcome::Paused) => "paused",
        Some(ScheduleEngineOutcome::Killed) => "killed",
        Some(ScheduleEngineOutcome::Interrupted) => "interrupted",
        _ => "running",
    }
}

fn completed_schedule_count(progress: &ScheduleProgress) -> usize {
    progress
        .completed_slots
        .iter()
        .map(|slot| slot.schedule_index)
        .collect::<HashSet<_>>()
        .len()
}
pub(crate) fn format_progress_bar(completed: usize, total: usize, width: usize) -> String {
    let filled = if total == 0 {
        width
    } else {
        completed.saturating_mul(width) / total
    }
    .min(width);
    let mut bar = String::with_capacity(width + 10);
    bar.push('[');
    for idx in 0..width {
        bar.push(if idx < filled { '#' } else { '-' });
    }
    bar.push(']');
    let percent = if total == 0 {
        100.0
    } else {
        (completed as f64 / total as f64) * 100.0
    };
    bar.push(' ');
    bar.push_str(&format!("{:.1}%", percent));
    bar
}

#[derive(Debug, Clone, Copy)]
struct StdoutRunProgress {
    enabled: bool,
    total_slots: usize,
    workers: usize,
    started_at: Instant,
}

impl StdoutRunProgress {
    fn new(enabled: bool, total_slots: usize, workers: usize) -> Self {
        Self {
            enabled,
            total_slots,
            workers,
            started_at: Instant::now(),
        }
    }

    fn emit(
        &self,
        run_id: &str,
        stage: &str,
        progress: &ScheduleProgress,
        active_trials: &[RunControlActiveTrial],
        pending_commits: usize,
        schedule: &[TrialSlot],
        last_event: &str,
        last_agent_output: Option<&str>,
        agent_output_capture: &str,
    ) {
        if !self.enabled {
            return;
        }
        let completed = completed_schedule_count(progress);
        let active = active_trials.len();
        let accounted = completed
            .saturating_add(active)
            .saturating_add(pending_commits);
        let remaining = self.total_slots.saturating_sub(accounted);
        let elapsed = format_duration(self.started_at.elapsed());
        let throughput = format_throughput(completed, self.started_at.elapsed());
        let eta = format_eta(completed, self.total_slots, self.started_at.elapsed());
        let completed_value = format!("{}/{}", completed, self.total_slots);
        let active_value = format!("{}/{}", active, self.workers);
        let pending_value = pending_commits.to_string();
        let remaining_value = remaining.to_string();
        let progress_value = format_progress_bar(completed, self.total_slots, 24);
        let active_detail = format_active_trials(active_trials, schedule);
        let monitor = format!("bucephalus views-live {} run_progress", run_id);
        let trust = format!("bucephalus views-live {} health", run_id);
        let agent_output_view = format!("bucephalus views-live {} latest_agent_output", run_id);
        let mut rows = vec![
            ("Run", run_id),
            ("Stage", stage),
            ("Completed trials", completed_value.as_str()),
            ("Progress bar", progress_value.as_str()),
            ("Active workers", active_value.as_str()),
            ("Active trials", active_detail.as_str()),
            ("Pending commits", pending_value.as_str()),
            ("Remaining slots", remaining_value.as_str()),
            ("Elapsed", elapsed.as_str()),
            ("Throughput", throughput.as_str()),
            ("ETA", eta.as_str()),
            ("Last event", last_event),
        ];
        if let Some(last_agent_output) = last_agent_output.filter(|value| !value.trim().is_empty())
        {
            rows.push(("Last agent output", last_agent_output));
        }
        if !agent_output_capture.trim().is_empty() {
            rows.push(("Agent result capture", agent_output_capture));
        }
        rows.push(("Monitor", monitor.as_str()));
        rows.push(("Agent output view", agent_output_view.as_str()));
        rows.push(("Trust", trust.as_str()));
        print_ascii_table("Bucephalus run progress", &rows);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LatestAgentOutputProgress {
    pub(crate) output_preview: Option<String>,
    pub(crate) capture_status: String,
}

pub(crate) fn latest_agent_output_progress_for_trial(
    run_dir: &Path,
    trial_id: &str,
) -> LatestAgentOutputProgress {
    let path = run_dir
        .join("trials")
        .join(trial_id)
        .join("agent")
        .join("result.json");
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => {
            return LatestAgentOutputProgress {
                output_preview: None,
                capture_status: "missing agent result file".to_string(),
            };
        }
    };
    let result: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => {
            return LatestAgentOutputProgress {
                output_preview: None,
                capture_status: "invalid agent result JSON".to_string(),
            };
        }
    };
    let preview = preview_agent_output_value(&result);
    let output_preview = if preview.is_empty() {
        Some("<empty agent result>".to_string())
    } else {
        Some(preview)
    };
    LatestAgentOutputProgress {
        output_preview,
        capture_status: "captured".to_string(),
    }
}

fn preview_agent_output_value(value: &Value) -> String {
    let rendered = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    rendered
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m{secs:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

fn format_throughput(completed: usize, elapsed: Duration) -> String {
    let elapsed_secs = elapsed.as_secs_f64();
    if completed == 0 || elapsed_secs <= 0.0 {
        return "warming up".to_string();
    }
    format!("{:.2} trials/min", completed as f64 * 60.0 / elapsed_secs)
}

fn format_eta(completed: usize, total: usize, elapsed: Duration) -> String {
    if completed == 0 || completed >= total {
        return "unknown".to_string();
    }
    let elapsed_secs = elapsed.as_secs_f64();
    if elapsed_secs <= 0.0 {
        return "unknown".to_string();
    }
    let remaining = total.saturating_sub(completed);
    let seconds = elapsed_secs * remaining as f64 / completed as f64;
    format_duration(Duration::from_secs(seconds.round().max(0.0) as u64))
}

fn format_active_trials(active_trials: &[RunControlActiveTrial], schedule: &[TrialSlot]) -> String {
    if active_trials.is_empty() {
        return "none".to_string();
    }
    let mut active = active_trials.to_vec();
    active.sort_by_key(|entry| entry.schedule_idx.unwrap_or(usize::MAX));
    let now = Utc::now();
    let mut items = active
        .iter()
        .take(4)
        .map(|trial| format_active_trial(trial, schedule, now))
        .collect::<Vec<_>>();
    if active.len() > items.len() {
        items.push(format!("+{} more", active.len() - items.len()));
    }
    items.join("; ")
}

fn format_active_trial(
    trial: &RunControlActiveTrial,
    schedule: &[TrialSlot],
    now: DateTime<Utc>,
) -> String {
    let slot = trial
        .schedule_idx
        .and_then(|idx| schedule.get(idx).map(|slot| (idx, slot)));
    let age = trial
        .started_at
        .as_deref()
        .and_then(|started| DateTime::parse_from_rfc3339(started).ok())
        .map(|started| now.signed_duration_since(started.with_timezone(&Utc)))
        .and_then(|duration| duration.to_std().ok())
        .map(format_duration)
        .unwrap_or_else(|| "?".to_string());
    match slot {
        Some((schedule_idx, slot)) => format!(
            "{} slot={} variant={} task={} repl={} worker={} age={}",
            trial.trial_id,
            schedule_idx,
            trial.variant_id.as_deref().unwrap_or("unknown"),
            slot.task_idx,
            slot.repl_idx,
            trial.worker_id,
            age
        ),
        None => format!(
            "{} slot=? variant={} worker={} age={}",
            trial.trial_id,
            trial.variant_id.as_deref().unwrap_or("unknown"),
            trial.worker_id,
            age
        ),
    }
}

pub(crate) fn render_ascii_table(title: &str, rows: &[(&str, &str)]) -> String {
    let key_width = rows
        .iter()
        .map(|(key, _)| key.len())
        .chain(std::iter::once(title.len()))
        .max()
        .unwrap_or(0);
    let value_width = rows.iter().map(|(_, value)| value.len()).max().unwrap_or(0);
    let table_width = key_width + value_width + 7;
    let top_border = format!("+{}+", "-".repeat(table_width));
    let row_border = format!(
        "+{}+{}+",
        "-".repeat(key_width + 2),
        "-".repeat(value_width + 2)
    );
    let mut out = String::new();
    out.push('\n');
    out.push_str(&top_border);
    out.push('\n');
    out.push_str(&format!("| {:<width$} |", title, width = table_width - 2));
    out.push('\n');
    out.push_str(&row_border);
    out.push('\n');
    for (key, value) in rows {
        out.push_str(&format!(
            "| {:<key_width$} | {:<value_width$} |",
            key,
            value,
            key_width = key_width,
            value_width = value_width
        ));
        out.push('\n');
    }
    out.push_str(&row_border);
    out.push('\n');
    out
}

fn print_ascii_table(title: &str, rows: &[(&str, &str)]) {
    print!("{}", render_ascii_table(title, rows));
    if let Err(err) = std::io::stdout().flush() {
        eprintln!("warning: failed to flush progress table: {}", err);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_schedule_engine_local_pull(
    run_dir: &Path,
    run_id: &str,
    workload_type: &str,
    project_root: &Path,
    _dataset_path: &Path,
    variants: &[Variant],
    tasks: &[Value],
    schedule: &[TrialSlot],
    policy_config: &PolicyConfig,
    evaluation_config: &EvaluationConfig,
    metric_definitions: &[MetricDefinition],
    variant_runtime_profiles: &[VariantRuntimeProfile],
    executor_kind: ExecutorKind,
    _behavior: &RunBehavior,
    materialize_mode: MaterializationMode,
    _task_boundary_policy: &TaskBoundaryPolicy,
    trials_dir: &Path,
    _evidence_dir: &Path,
    evidence_records_path: &Path,
    task_chain_states_path: &Path,
    schedule_progress: &mut ScheduleProgress,
    trial_index: &mut usize,
    consecutive_failures: &mut BTreeMap<usize, usize>,
    pruned_variants: &mut HashSet<usize>,
    recovered_active_trials: &[RunControlActiveTrial],
    baseline_id: &str,
    run_sink: &mut dyn RunSink,
    max_concurrency: usize,
    stdout_progress: bool,
) -> Result<ScheduleEngineOutcome> {
    configure_host_grader_max_concurrency(
        evaluation_config
            .grader
            .as_ref()
            .and_then(|grader| grader.max_concurrency),
    );
    let grading_conclusions_path = run_dir
        .join("runtime")
        .join("durable_rows")
        .join("trial_conclusions.row.json");

    let requested_dispatch_capacity = max_concurrency.max(1);
    let configured_ceiling = parse_local_worker_capacity_ceiling_from_env()?;
    let (dispatch_capacity, capacity_warning) =
        resolve_local_worker_max_in_flight(requested_dispatch_capacity, configured_ceiling);
    if let Some(warning) = capacity_warning {
        eprintln!("{}", warning);
    }
    let progress_reporter =
        StdoutRunProgress::new(stdout_progress, schedule.len(), dispatch_capacity);

    let execution_context = Arc::new(ParallelWorkerExecutionContext {
        run_dir: run_dir.to_path_buf(),
        run_id: run_id.to_string(),
        workload_type: workload_type.to_string(),
        project_root: project_root.to_path_buf(),
        variants: variants.to_vec(),
        tasks: tasks.to_vec(),
        policy_config: policy_config.clone(),
        evaluation_config: evaluation_config.clone(),
        metric_definitions: metric_definitions.to_vec(),
        variant_runtime_profiles: variant_runtime_profiles.to_vec(),
        executor_kind,
        materialize_mode,
        trials_dir: trials_dir.to_path_buf(),
        baseline_id: baseline_id.to_string(),
    });

    let min_free_bytes = resolve_min_free_bytes()?;
    let max_run_bytes = parse_max_run_bytes_from_env()?;
    let disk_check_interval = Duration::from_secs(RUNTIME_DISK_HEADROOM_CHECK_INTERVAL_SECONDS);
    let run_size_check_interval = Duration::from_secs(RUNTIME_RUN_SIZE_CHECK_INTERVAL_SECONDS);
    let mut last_disk_check = Instant::now() - disk_check_interval;
    let mut last_run_size_check = Instant::now() - run_size_check_interval;

    let journal_records = load_slot_commit_records(run_dir)?;
    let mut committer = DeterministicCommitter::from_progress(schedule_progress, &journal_records);
    let mut slot_store = open_schedule_slot_store(run_dir)?;
    slot_store.ensure_schedule_slots(run_id, schedule)?;
    let persisted_pending = load_pending_trial_completion_records(run_dir)?;
    for (schedule_idx, result) in &persisted_pending {
        if *schedule_idx >= schedule.len() || committer.is_committed_schedule(*schedule_idx) {
            continue;
        }
        committer.enqueue_trial(*schedule_idx, result.clone())?;
    }
    if !recovered_active_trials.is_empty() {
        let mut variant_idx_by_id: HashMap<String, usize> = HashMap::new();
        for (idx, variant) in variants.iter().enumerate() {
            variant_idx_by_id.insert(variant.id.clone(), idx);
        }
        for recovered in recovered_active_trials {
            let Some(schedule_idx) = recovered.schedule_idx else {
                continue;
            };
            if schedule_idx >= schedule.len() || committer.is_committed_schedule(schedule_idx) {
                continue;
            }
            if persisted_pending.contains_key(&schedule_idx) {
                continue;
            }
            let variant_idx = recovered
                .variant_id
                .as_ref()
                .and_then(|id| variant_idx_by_id.get(id).copied());
            let result = TrialExecutionResult::worker_lost(
                recovered.trial_id.clone(),
                variant_idx,
                Some("worker_lost".to_string()),
            );
            let recovered_trial_dir = run_dir.join("trials").join(&recovered.trial_id);
            cleanup_trial_runtime_required(
                run_dir,
                run_id,
                &recovered.trial_id,
                &recovered_trial_dir,
            )
            .with_context(|| {
                format!(
                    "failed to clean recovered active trial {} before marking worker_lost",
                    recovered.trial_id
                )
            })?;
            crate::trial::state::reconcile_trial_attempt_as_abandoned(&recovered_trial_dir)
                .with_context(|| {
                    format!(
                        "failed to mark recovered active trial {} runtime state abandoned",
                        recovered.trial_id
                    )
                })?;
            committer.enqueue_trial(schedule_idx, result)?;
        }
    }
    persist_pending_trial_completions(run_dir, &committer.pending_trial_completion_records())?;

    let mut scheduler_perf = SchedulerPerfBuffer::default();
    let schedule_engine_started_at = Instant::now();
    let initial_drain_started_at = Instant::now();
    let initial_committed = committer.drain_ready(
        run_dir,
        policy_config,
        evidence_records_path,
        task_chain_states_path,
        &grading_conclusions_path,
        schedule_progress,
        *trial_index,
        pruned_variants,
        consecutive_failures,
        run_sink,
    )?;
    scheduler_perf.record_duration(
        None,
        None,
        None,
        "scheduler_initial_drain_ready",
        initial_drain_started_at,
        json!({ "committed": initial_committed }),
    );
    persist_pending_trial_completions(run_dir, &committer.pending_trial_completion_records())?;

    let pending_completion_schedules = committer
        .pending_by_schedule
        .keys()
        .copied()
        .collect::<HashSet<_>>();
    let broker = SlotBroker::new(
        run_dir,
        run_id,
        project_root,
        trials_dir,
        variants,
        schedule,
        &committer.committed_schedules,
        &pending_completion_schedules,
        pruned_variants,
        *trial_index,
    )?
    .with_variant_limit(policy_config.concurrency.max_in_flight_per_variant);
    write_run_control(run_dir, run_id, "running", &broker.active_trials(), None)?;

    let (event_tx, event_rx) = mpsc::channel::<LocalWorkerEvent>();
    let mut worker_handles = Vec::with_capacity(dispatch_capacity);
    for worker_idx in 0..dispatch_capacity {
        let worker_id = format!("bucephalus-worker-{}", worker_idx + 1);
        worker_handles.push(spawn_pull_worker(
            execution_context.clone(),
            broker.clone(),
            worker_id,
            event_tx.clone(),
        )?);
    }
    drop(event_tx);

    let mut requested_outcome: Option<ScheduleEngineOutcome> = None;
    let mut exited_workers = 0_usize;
    let mut first_trial_dispatched = false;

    macro_rules! drain_ready_and_persist {
        ($drain_stage:expr, $persist_stage:expr) => {{
            let committed_before = committer.committed_schedules.clone();
            let drain_started_at = Instant::now();
            let committed = committer.drain_ready(
                run_dir,
                policy_config,
                evidence_records_path,
                task_chain_states_path,
                &grading_conclusions_path,
                schedule_progress,
                broker.trial_index(),
                pruned_variants,
                consecutive_failures,
                run_sink,
            )?;
            let newly_committed = committer
                .committed_schedules
                .difference(&committed_before)
                .copied()
                .collect::<Vec<_>>();
            for schedule_idx in &newly_committed {
                broker.mark_committed(*schedule_idx);
            }
            broker.update_pruned_variants(pruned_variants);
            *trial_index = broker.trial_index();
            schedule_progress.next_trial_index = *trial_index;
            scheduler_perf.record_duration(
                None,
                None,
                None,
                $drain_stage,
                drain_started_at,
                json!({
                    "committed": committed,
                    "newly_committed": newly_committed
                }),
            );
            let persist_started_at = Instant::now();
            let pending_records = committer.pending_trial_completion_records();
            persist_pending_trial_completions(run_dir, &pending_records)?;
            scheduler_perf.record_duration(
                None,
                None,
                None,
                $persist_stage,
                persist_started_at,
                json!({ "pending_record_count": pending_records.len() }),
            );
            Ok::<usize, anyhow::Error>(committed)
        }};
    }

    let engine_result = (|| -> Result<ScheduleEngineOutcome> {
        loop {
            let loop_started_at = Instant::now();
            if INTERRUPTED.load(Ordering::SeqCst) {
                emit_run_log(
                    run_id,
                    "received interrupt signal, shutting down gracefully",
                );
                requested_outcome = Some(ScheduleEngineOutcome::Interrupted);
                broker.stop_accepting();
                write_run_control(
                    run_dir,
                    run_id,
                    "interrupted",
                    &broker.active_trials(),
                    None,
                )?;
                return Ok(ScheduleEngineOutcome::Interrupted);
            }
            let external_outcome_check_started_at = Instant::now();
            if let Some(external_outcome) = load_external_schedule_outcome_request(run_dir)? {
                requested_outcome = Some(external_outcome);
                broker.stop_accepting();
            }
            scheduler_perf.record_duration(
                None,
                None,
                None,
                "scheduler_loop_external_outcome_check",
                external_outcome_check_started_at,
                json!({}),
            );

            if last_disk_check.elapsed() >= disk_check_interval {
                let disk_check_started_at = Instant::now();
                enforce_runtime_disk_headroom(run_dir, min_free_bytes)?;
                last_disk_check = Instant::now();
                scheduler_perf.record_duration(
                    None,
                    None,
                    None,
                    "scheduler_loop_disk_headroom_check",
                    disk_check_started_at,
                    json!({}),
                );
            }
            if let Some(max_bytes) = max_run_bytes {
                if last_run_size_check.elapsed() >= run_size_check_interval {
                    let run_size_check_started_at = Instant::now();
                    enforce_runtime_run_size_budget(run_dir, max_bytes)?;
                    last_run_size_check = Instant::now();
                    scheduler_perf.record_duration(
                        None,
                        None,
                        None,
                        "scheduler_loop_run_size_check",
                        run_size_check_started_at,
                        json!({ "max_run_bytes": max_bytes }),
                    );
                }
            }

            if let Some(outcome) = requested_outcome {
                if broker.in_flight_map().is_empty() && !committer.has_pending() {
                    return Ok(outcome);
                }
            }
            if requested_outcome.is_none()
                && exited_workers >= worker_handles.len()
                && !committer.has_pending()
            {
                if broker.has_unfinished_slots() {
                    return Err(anyhow!(
                        "pull scheduler protocol fault: all workers exited before all schedule slots finished"
                    ));
                }
                break;
            }

            let event = match event_rx.recv_timeout(Duration::from_millis(5)) {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if exited_workers < worker_handles.len() {
                        return Err(anyhow!("pull scheduler event channel disconnected"));
                    }
                    None
                }
            };
            scheduler_perf.record_duration(
                None,
                None,
                None,
                "scheduler_loop_top_to_event_poll",
                loop_started_at,
                json!({
                    "in_flight": broker.in_flight_map().len(),
                    "exited_workers": exited_workers,
                    "dispatch_capacity": dispatch_capacity
                }),
            );

            let Some(event) = event else {
                continue;
            };
            match event {
                LocalWorkerEvent::Claimed { active, timing } => {
                    let Some(schedule_idx) = active.schedule_idx else {
                        return Err(anyhow!(
                            "pull scheduler protocol fault: claimed trial {} has no schedule_idx",
                            active.trial_id
                        ));
                    };
                    let slot = schedule.get(schedule_idx).ok_or_else(|| {
                        anyhow!(
                            "pull scheduler protocol fault: claimed schedule_idx {} is out of range",
                            schedule_idx
                        )
                    })?;
                    scheduler_perf.record_value(
                        Some(&active.trial_id),
                        Some(schedule_idx),
                        Some(0),
                        "worker_claim_next_wait",
                        timing.claim_wait_ms,
                        json!({
                            "worker_id": active.worker_id.as_str()
                        }),
                    );
                    scheduler_perf.record_value(
                        Some(&active.trial_id),
                        Some(schedule_idx),
                        Some(0),
                        "worker_claim_intent_persist",
                        timing.claim_intent_persist_ms,
                        json!({
                            "worker_id": active.worker_id.as_str(),
                            "boundary": "durable_claim_before_external_execution"
                        }),
                    );
                    if let Some((completed_at, completed_trial_id, completed_schedule_idx)) =
                        timing.previous_completion
                    {
                        scheduler_perf.record_value(
                            Some(&active.trial_id),
                            Some(schedule_idx),
                            Some(0),
                            "worker_completion_to_next_claim",
                            timing
                                .completion_to_claim_ms
                                .unwrap_or_else(|| completed_at.elapsed().as_secs_f64() * 1000.0),
                            json!({
                                "worker_id": active.worker_id.as_str(),
                                "completed_trial_id": completed_trial_id,
                                "completed_schedule_idx": completed_schedule_idx,
                                "claim_wait_ms": timing.claim_wait_ms
                            }),
                        );
                    }
                    let active_persist_started_at = Instant::now();
                    slot_store.claim_schedule_slot(
                        run_id,
                        schedule_idx,
                        &active.trial_id,
                        &active.worker_id,
                        "slot-broker",
                        None,
                    )?;
                    scheduler_perf.record_duration(
                        Some(&active.trial_id),
                        Some(schedule_idx),
                        Some(0),
                        "coordinator_active_slot_persist",
                        active_persist_started_at,
                        json!({
                            "worker_id": active.worker_id.as_str(),
                            "hot_path": false
                        }),
                    );
                    if !first_trial_dispatched {
                        first_trial_dispatched = true;
                        crate::perf::record_cli_latency(
                            run_dir,
                            run_id,
                            "cli_to_first_trial_dispatch",
                            json!({
                                "trial_id": active.trial_id,
                                "schedule_idx": schedule_idx,
                                "variant_id": active.variant_id,
                                "task_idx": slot.task_idx,
                                "repl_idx": slot.repl_idx,
                                "dispatch_model": "pull_worker"
                            }),
                        )?;
                        crate::perf::record_duration(
                            run_dir,
                            run_id,
                            Some(&active.trial_id),
                            Some(schedule_idx),
                            Some(0),
                            "schedule_start_to_first_trial_dispatch",
                            schedule_engine_started_at,
                            json!({
                                "dispatch_capacity": dispatch_capacity,
                                "configured_ceiling": configured_ceiling,
                                "dispatch_model": "pull_worker"
                            }),
                        )?;
                    }
                    write_run_control(
                        run_dir,
                        run_id,
                        schedule_engine_status(requested_outcome),
                        &broker.active_trials(),
                        None,
                    )?;
                    let last_event = format!(
                        "claimed {} slot {} variant {}",
                        active.trial_id,
                        schedule_idx,
                        active.variant_id.as_deref().unwrap_or("unknown")
                    );
                    progress_reporter.emit(
                        run_id,
                        schedule_engine_status(requested_outcome),
                        schedule_progress,
                        &broker.active_trials(),
                        committer.pending_by_schedule.len(),
                        schedule,
                        last_event.as_str(),
                        None,
                        "",
                    );
                }
                LocalWorkerEvent::SkippedPruned {
                    worker_id,
                    schedule_idx,
                } => {
                    let enqueue_started_at = Instant::now();
                    committer.enqueue_skipped(schedule_idx)?;
                    scheduler_perf.record_duration(
                        None,
                        Some(schedule_idx),
                        Some(0),
                        "scheduler_skipped_pruned_enqueue",
                        enqueue_started_at,
                        json!({ "worker_id": worker_id }),
                    );
                    drain_ready_and_persist!(
                        "scheduler_post_skip_drain_ready",
                        "scheduler_post_skip_persist_pending"
                    )?;
                    write_run_control(
                        run_dir,
                        run_id,
                        schedule_engine_status(requested_outcome),
                        &broker.active_trials(),
                        None,
                    )?;
                    let last_event = format!("skipped pruned slot {}", schedule_idx);
                    progress_reporter.emit(
                        run_id,
                        schedule_engine_status(requested_outcome),
                        schedule_progress,
                        &broker.active_trials(),
                        committer.pending_by_schedule.len(),
                        schedule,
                        last_event.as_str(),
                        None,
                        "",
                    );
                }
                LocalWorkerEvent::Completed(completion) => {
                    let scheduler_received_completion_at = Instant::now();
                    let slot = schedule.get(completion.schedule_idx).ok_or_else(|| {
                        anyhow!(
                            "pull scheduler protocol fault: completion schedule_idx {} is out of range",
                            completion.schedule_idx
                        )
                    })?;
                    let variant = variants.get(slot.variant_idx).ok_or_else(|| {
                        anyhow!(
                            "pull scheduler protocol fault: completion variant_idx {} is out of range",
                            slot.variant_idx
                        )
                    })?;
                    scheduler_perf.record_duration(
                        Some(&completion.trial_id),
                        Some(completion.schedule_idx),
                        Some(0),
                        "worker_completion_to_coordinator_receive",
                        completion.completed_at,
                        json!({
                            "worker_id": completion.worker_id.as_str(),
                            "variant_id": variant.id.as_str()
                        }),
                    );
                    let mut trial_result = match completion.result {
                        Ok(result) => result,
                        Err(detail) => {
                            return Err(anyhow!(
                                "local trial execution failed (trial_id={}, schedule_idx={}): {}",
                                completion.trial_id,
                                completion.schedule_idx,
                                detail
                            ));
                        }
                    };
                    if trial_result.trial_id != completion.trial_id {
                        return Err(anyhow!(
                            "pull scheduler protocol fault: completion trial_id mismatch: expected {}, got {}",
                            completion.trial_id,
                            trial_result.trial_id
                        ));
                    }
                    if trial_result.variant_idx.is_none() {
                        trial_result.variant_idx = Some(slot.variant_idx);
                    }
                    let completed_status = trial_result.slot_status.clone();
                    let enqueue_started_at = Instant::now();
                    committer.enqueue_trial(completion.schedule_idx, trial_result)?;
                    scheduler_perf.record_duration(
                        Some(&completion.trial_id),
                        Some(completion.schedule_idx),
                        Some(0),
                        "scheduler_completion_enqueue",
                        enqueue_started_at,
                        json!({
                            "variant_id": variant.id.as_str(),
                            "pending_completion_count": committer.pending_by_schedule.len()
                        }),
                    );
                    scheduler_perf.record_duration(
                        Some(&completion.trial_id),
                        Some(completion.schedule_idx),
                        Some(0),
                        "scheduler_completion_receive_to_enqueue",
                        scheduler_received_completion_at,
                        json!({
                            "variant_id": variant.id.as_str(),
                            "pending_completion_count": committer.pending_by_schedule.len()
                        }),
                    );
                    let pre_commit_persist_started_at = Instant::now();
                    let pending_records = committer.pending_trial_completion_records();
                    persist_pending_trial_completions(run_dir, &pending_records)?;
                    scheduler_perf.record_duration(
                        Some(&completion.trial_id),
                        Some(completion.schedule_idx),
                        Some(0),
                        "scheduler_completion_persist_pending_before_commit",
                        pre_commit_persist_started_at,
                        json!({
                            "pending_record_count": pending_records.len(),
                            "boundary": "completion_result_durable_before_slot_commit"
                        }),
                    );
                    drain_ready_and_persist!(
                        "scheduler_post_completion_drain_ready",
                        "scheduler_post_completion_persist_pending"
                    )?;
                    write_run_control(
                        run_dir,
                        run_id,
                        schedule_engine_status(requested_outcome),
                        &broker.active_trials(),
                        None,
                    )?;
                    let last_event = format!(
                        "completed {} slot {} status {}",
                        completion.trial_id, completion.schedule_idx, completed_status
                    );
                    let agent_output =
                        latest_agent_output_progress_for_trial(run_dir, &completion.trial_id);
                    progress_reporter.emit(
                        run_id,
                        schedule_engine_status(requested_outcome),
                        schedule_progress,
                        &broker.active_trials(),
                        committer.pending_by_schedule.len(),
                        schedule,
                        last_event.as_str(),
                        agent_output.output_preview.as_deref(),
                        agent_output.capture_status.as_str(),
                    );
                }
                LocalWorkerEvent::Exited { worker_id } => {
                    exited_workers = exited_workers.saturating_add(1);
                    scheduler_perf.record_duration(
                        None,
                        None,
                        None,
                        "worker_exit_observed",
                        loop_started_at,
                        json!({
                            "worker_id": worker_id,
                            "exited_workers": exited_workers,
                            "dispatch_capacity": dispatch_capacity
                        }),
                    );
                }
                LocalWorkerEvent::Fatal { worker_id, detail } => {
                    broker.stop_accepting();
                    return Err(anyhow!(
                        "pull worker {} failed before completing schedule: {}",
                        worker_id,
                        detail
                    ));
                }
            }
        }

        drain_ready_and_persist!(
            "scheduler_final_drain_ready",
            "scheduler_final_persist_pending"
        )?;
        write_run_control(
            run_dir,
            run_id,
            schedule_engine_status(requested_outcome),
            &broker.active_trials(),
            None,
        )?;
        let outcome = requested_outcome.unwrap_or(ScheduleEngineOutcome::Completed);
        Ok(outcome)
    })();

    broker.stop_accepting();
    let active_at_shutdown = broker.in_flight_map();
    let cleanup_result = if active_at_shutdown.is_empty() {
        Ok(Vec::new())
    } else {
        let mut cleanup_result =
            cleanup_in_flight_trial_containers(run_dir, run_id, trials_dir, &active_at_shutdown);
        for dispatch in active_at_shutdown.values() {
            if let Err(err) =
                slot_store.release_schedule_slot_to_pending(run_id, dispatch.schedule_idx)
            {
                if cleanup_result.is_ok() {
                    cleanup_result = Err(err.context(format!(
                        "failed to release active schedule_idx {} during scheduler shutdown",
                        dispatch.schedule_idx
                    )));
                } else {
                    eprintln!(
                        "warning: failed to release active schedule_idx {} during scheduler shutdown: {}",
                        dispatch.schedule_idx, err
                    );
                }
            }
        }
        if let Err(err) = write_run_control(
            run_dir,
            run_id,
            schedule_engine_status(requested_outcome),
            &[],
            None,
        ) {
            if cleanup_result.is_ok() {
                cleanup_result =
                    Err(err.context("failed to clear active trials during scheduler shutdown"));
            } else {
                eprintln!(
                    "warning: failed to clear active trials during scheduler shutdown: {}",
                    err
                );
            }
        }
        cleanup_result
    };
    for handle in worker_handles {
        if handle.join().is_err() {
            eprintln!("warning: local worker thread panicked during scheduler shutdown");
        }
    }
    if let Err(err) = scheduler_perf.flush(run_dir, run_id) {
        eprintln!("warning: scheduler perf flush failed: {}", err);
    }
    if let Err(cleanup_err) = cleanup_result {
        return match engine_result {
            Ok(outcome) => Err(cleanup_err.context(format!(
                "scheduler exited with {:?} but in-flight cleanup failed",
                outcome
            ))),
            Err(err) => Err(err.context(format!("in-flight cleanup also failed: {}", cleanup_err))),
        };
    }

    engine_result
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_schedule_engine(
    run_dir: &Path,
    run_id: &str,
    workload_type: &str,
    project_root: &Path,
    dataset_path: &Path,
    variants: &[Variant],
    tasks: &[Value],
    schedule: &[TrialSlot],
    policy_config: &PolicyConfig,
    evaluation_config: &EvaluationConfig,
    metric_definitions: &[MetricDefinition],
    variant_runtime_profiles: &[VariantRuntimeProfile],
    executor_kind: ExecutorKind,
    behavior: &RunBehavior,
    materialize_mode: MaterializationMode,
    task_boundary_policy: &TaskBoundaryPolicy,
    trials_dir: &Path,
    evidence_dir: &Path,
    evidence_records_path: &Path,
    task_chain_states_path: &Path,
    schedule_progress: &mut ScheduleProgress,
    trial_index: &mut usize,
    consecutive_failures: &mut BTreeMap<usize, usize>,
    pruned_variants: &mut HashSet<usize>,
    recovered_active_trials: &[RunControlActiveTrial],
    baseline_id: &str,
    run_sink: &mut dyn RunSink,
    max_concurrency: usize,
    stdout_progress: bool,
) -> Result<ScheduleEngineOutcome> {
    if !matches!(policy_config.state, StatePolicy::IsolatePerTrial) {
        return Err(anyhow!(
            "local async docker path supports only isolate_per_trial state policy; got {:?}",
            policy_config.state
        ));
    }
    execute_schedule_engine_local_pull(
        run_dir,
        run_id,
        workload_type,
        project_root,
        dataset_path,
        variants,
        tasks,
        schedule,
        policy_config,
        evaluation_config,
        metric_definitions,
        variant_runtime_profiles,
        executor_kind,
        behavior,
        materialize_mode,
        task_boundary_policy,
        trials_dir,
        evidence_dir,
        evidence_records_path,
        task_chain_states_path,
        schedule_progress,
        trial_index,
        consecutive_failures,
        pruned_variants,
        recovered_active_trials,
        baseline_id,
        run_sink,
        max_concurrency,
        stdout_progress,
    )
}

pub(crate) fn run_experiment_with_behavior(
    path: &Path,
    behavior: RunBehavior,
    execution: RunExecutionOptions,
) -> Result<RunResult> {
    let run_invocation_started = Instant::now();
    let LoadedExperimentInput {
        json_value,
        exp_dir,
        project_root,
    } = load_sealed_package_for_run(path)?;
    validate_required_fields(&json_value)?;
    let workload_type = experiment_workload_type(&json_value)?;

    let execution = normalize_execution_options_for_experiment(&json_value, &execution)?;
    ensure_supported_executor(&execution)?;
    let materialize_mode = execution
        .materialize
        .unwrap_or(MaterializationMode::OutputsOnly);

    let default_run_root;
    let run_root = if let Some(run_root) = execution.run_root.as_deref() {
        run_root
    } else {
        default_run_root = crate::local_storage::default_run_root()?;
        default_run_root.as_path()
    };
    let (run_id, run_dir) = create_unique_run_dir(run_root)?;
    emit_run_log(
        &run_id,
        format!("created run directory {}", run_dir.display()),
    );
    crate::perf::record_cli_latency(
        &run_dir,
        &run_id,
        "cli_to_run_dir_created",
        json!({ "package": path.display().to_string() }),
    )?;
    crate::perf::record_duration(
        &run_dir,
        &run_id,
        None,
        None,
        None,
        "runner_invocation_to_run_dir_created",
        run_invocation_started,
        json!({ "package": path.display().to_string() }),
    )?;
    write_run_control(&run_dir, &run_id, "running", &[], None)?;
    write_run_session_state_with_project_root(
        &run_dir,
        &run_id,
        &project_root,
        &behavior,
        &execution,
    )?;
    let (_run_store_writer_guard, run_store_writer) =
        RunStoreWriterGuard::start(&run_dir, &run_id)?;
    let _run_store_writer_scope =
        crate::trial::execution::RunStoreWriterScope::install(run_store_writer.clone());
    let _engine_lease_guard =
        start_engine_lease_heartbeat_with_writer(&run_dir, &run_id, Some(run_store_writer))?;
    let mut run_guard = RunControlGuard::new(&run_dir, &run_id);

    copy_verified_package_payload_for_run(&exp_dir, &run_dir).with_context(|| {
        format!(
            "failed to copy verified sealed package payload from {} into run directory {}",
            exp_dir.display(),
            run_dir.display()
        )
    })?;

    let resolved_path = run_dir.join("resolved_experiment.json");
    atomic_write_json_pretty(&resolved_path, &json_value)?;
    let resolved_digest = canonical_json_digest(&json_value);
    atomic_write_bytes(
        &run_dir.join("resolved_experiment.digest"),
        resolved_digest.as_bytes(),
    )?;

    let manifest = json!({
        "schema_version": "manifest_v1",
        "run_id": run_id,
        "runner_version": "rust-0.3.0",
        "created_at": Utc::now().to_rfc3339(),
        "project_root": project_root.display().to_string(),
        "run_mode": if behavior.smoke_test { "smoke_test" } else { "full" },
    });
    atomic_write_json_pretty(&run_dir.join("manifest.json"), &manifest)?;

    let dataset_path = resolve_dataset_path_in_package(&json_value, &run_dir)?;
    let tasks = load_tasks(&dataset_path, &json_value)?;

    let (variants, baseline_id) = resolve_variant_plan(&json_value)?;
    write_resolved_variants(&run_dir, &json_value, &baseline_id, &variants)?;
    let replications = json_value
        .pointer("/matrix/repeats")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("missing /matrix/repeats"))? as usize;
    emit_run_log(
        &run_id,
        format!(
            "resolved experiment: tasks={} variants={} replications={} total_trials={}",
            tasks.len(),
            variants.len(),
            replications,
            tasks.len() * variants.len() * replications
        ),
    );
    let trials_dir = run_dir.join("trials");
    ensure_dir(&trials_dir)?;

    let evidence_dir = run_dir.join("runtime").join("durable_rows");
    let evidence_records_path = evidence_dir.join("evidence_records.row.json");
    let task_chain_states_path = evidence_dir.join("task_chain_states.row.json");
    let evaluation_config = parse_evaluation_config(&json_value)?;
    let metric_definitions = parse_metric_definitions(&json_value)?;
    let mut variant_runtime_profiles = Vec::with_capacity(variants.len());
    for variant in &variants {
        let profile =
            resolve_variant_runtime_profile(&json_value, variant, &run_dir, &behavior, &execution)?;
        ensure_required_runtime_env_present(&profile.agent_runtime, &profile.agent_runtime_env)?;
        variant_runtime_profiles.push(profile);
    }
    let run_integration_level = variant_runtime_profiles
        .first()
        .map(|profile| profile.agent_runtime.integration_level.clone())
        .unwrap_or_else(|| "cli_basic".to_string());
    let isolation_grade = resolve_run_isolation_grade(&variant_runtime_profiles, &behavior);

    {
        let executor_kind = resolved_executor_kind(&execution);
        emit_run_log(
            &run_id,
            if executor_kind == ExecutorKind::Modal {
                "starting preflight checks (Modal executor; local Docker probes are skipped)"
            } else {
                "starting preflight checks (Docker probes can take a while for per-task images)"
            },
        );
        let preflight_started = Instant::now();
        let checks = collect_preflight_checks_for_executor(
            &json_value,
            &run_dir,
            &run_dir,
            &project_root,
            &tasks,
            &evaluation_config,
            &variants,
            &variant_runtime_profiles,
            executor_kind,
        );

        let preflight = PreflightReport {
            passed: checks
                .iter()
                .all(|c| c.passed || matches!(c.severity, PreflightSeverity::Warning)),
            checks,
        };

        let mut passed_count = 0usize;
        let mut warning_count = 0usize;
        let mut failed_count = 0usize;
        for check in &preflight.checks {
            let status = if check.passed {
                passed_count += 1;
                "PASS"
            } else {
                match check.severity {
                    PreflightSeverity::Error => {
                        failed_count += 1;
                        "FAIL"
                    }
                    PreflightSeverity::Warning => {
                        warning_count += 1;
                        "WARN"
                    }
                }
            };
            emit_preflight_log(format!("[{}] {}: {}", status, check.name, check.message));
        }
        emit_run_log(
            &run_id,
            format!(
                "preflight finished in {:.1}s (passed={}, warnings={}, failed={})",
                preflight_started.elapsed().as_secs_f32(),
                passed_count,
                warning_count,
                failed_count
            ),
        );
        crate::perf::record_duration(
            &run_dir,
            &run_id,
            None,
            None,
            None,
            "preflight_checks",
            preflight_started,
            json!({
                "passed": preflight.passed,
                "passed_count": passed_count,
                "warning_count": warning_count,
                "failed_count": failed_count
            }),
        )?;

        if !preflight.passed {
            run_guard.complete("preflight_failed")?;
            return Err(anyhow!("preflight failed:\n{}", preflight));
        }
    }

    let mut run_sink = open_run_sink(&run_dir)?;
    run_sink.write_run_manifest(&RunManifestRecord {
        schema_version: "run_manifest_v1".to_string(),
        run_id: run_id.clone(),
        created_at: Utc::now().to_rfc3339(),
        workload_type: workload_type.clone(),
        baseline_id: baseline_id.clone(),
        variant_ids: variants.iter().map(|variant| variant.id.clone()).collect(),
    })?;
    run_sink.write_metric_definitions(&metric_definition_records(
        &json_value,
        &metric_definitions,
    )?)?;

    let policy_config = parse_policies(&json_value);
    let max_concurrency = experiment_max_concurrency(&json_value);
    let random_seed = experiment_random_seed(&json_value);
    let schedule = if behavior.smoke_test {
        if tasks.is_empty() {
            return Err(anyhow!("smoke test requires at least one task"));
        }
        (0..variants.len())
            .map(|variant_idx| TrialSlot {
                variant_idx,
                task_idx: 0,
                repl_idx: 0,
            })
            .collect::<Vec<_>>()
    } else {
        build_trial_schedule(
            variants.len(),
            tasks.len(),
            replications,
            policy_config.scheduling,
            random_seed,
        )
    };
    write_resolved_schedule(&run_dir, &schedule)?;
    open_schedule_slot_store(&run_dir)?.ensure_schedule_slots(&run_id, &schedule)?;
    emit_run_log(
        &run_id,
        format!(
            "starting {}schedule execution: slots={} max_concurrency={}",
            if behavior.smoke_test {
                "smoke-test "
            } else {
                ""
            },
            schedule.len(),
            max_concurrency.max(1)
        ),
    );
    crate::perf::record_cli_latency(
        &run_dir,
        &run_id,
        "cli_to_schedule_start",
        json!({
            "slots": schedule.len(),
            "max_concurrency": max_concurrency.max(1)
        }),
    )?;

    let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
    let mut pruned_variants: HashSet<usize> = HashSet::new();

    let mut schedule_progress = new_schedule_progress(&run_id, &schedule);
    write_schedule_progress(&run_dir, &schedule_progress)?;

    let mut trial_index: usize = 0;
    let schedule_outcome = execute_schedule_engine(
        &run_dir,
        &run_id,
        &workload_type,
        &project_root,
        &dataset_path,
        &variants,
        &tasks,
        &schedule,
        &policy_config,
        &evaluation_config,
        &metric_definitions,
        &variant_runtime_profiles,
        resolved_executor_kind(&execution),
        &behavior,
        materialize_mode,
        &policy_config.task_boundary,
        &trials_dir,
        &evidence_dir,
        &evidence_records_path,
        &task_chain_states_path,
        &mut schedule_progress,
        &mut trial_index,
        &mut consecutive_failures,
        &mut pruned_variants,
        &[],
        &baseline_id,
        &mut *run_sink,
        max_concurrency,
        execution.stdout_progress,
    )?;
    run_sink.flush()?;
    if schedule_outcome != ScheduleEngineOutcome::Completed {
        emit_run_log(
            &run_id,
            format!("schedule execution halted with {:?}", schedule_outcome),
        );
        match schedule_outcome {
            ScheduleEngineOutcome::Interrupted => {
                run_guard.complete("interrupted")?;
            }
            _ => {
                run_guard.disarm();
            }
        }
        return Ok(RunResult {
            account_db_path: run_store_location(&run_dir)?.into(),
            run_dir,
            run_id,
        });
    }
    let (_project_root, _evaluation_config, _evidence_records_path, _task_chain_states_path) = (
        project_root,
        evaluation_config,
        evidence_records_path,
        task_chain_states_path,
    );

    if isolation_grade != "hermetic" {
        run_guard.complete("invalid_isolation")?;
        return Err(anyhow!(
            "scientific run completed without hermetic isolation (got {})",
            isolation_grade
        ));
    }

    let grades = json!({
        "schema_version": "grades_v1",
        "integration_level": run_integration_level,
        "replay_grade": "best_effort",
        "isolation_grade": isolation_grade,
        "comparability_grade": "unknown",
        "provenance_grade": "recorded",
        "privacy_grade": "unknown"
    });

    let att = default_attestation(
        &resolved_digest,
        None,
        grades.clone(),
        vec![],
        json!({"name": "unknown"}),
        "events",
    );
    write_attestation(&run_dir, att)?;
    run_guard.complete("completed")?;
    emit_run_log(&run_id, "run completed");

    Ok(RunResult {
        account_db_path: run_store_location(&run_dir)?.into(),
        run_dir,
        run_id,
    })
}

pub fn experiment_summary(path: &Path) -> Result<ExperimentSummary> {
    experiment_summary_with_options(path, &RunExecutionOptions::default())
}

pub fn experiment_summary_with_options(
    path: &Path,
    execution: &RunExecutionOptions,
) -> Result<ExperimentSummary> {
    let LoadedExperimentInput {
        json_value,
        exp_dir,
        project_root: _,
    } = load_sealed_package_for_run(path)?;
    validate_required_fields(&json_value)?;
    let execution = normalize_execution_options_for_experiment(&json_value, execution)?;

    let dataset_path = resolve_dataset_path_in_package(&json_value, &exp_dir)?;
    let task_count = count_tasks(&dataset_path, &json_value)?;
    let (variants, _) = resolve_variant_plan(&json_value)?;
    let replications = json_value
        .pointer("/matrix/repeats")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("missing /matrix/repeats"))? as usize;
    let variant_count = variants.len();
    let total_trials = task_count * replications * variant_count;

    let baseline_variant = variants
        .first()
        .ok_or_else(|| anyhow!("no variants available in experiment"))?;
    let runtime_profile = resolve_variant_runtime_profile(
        &json_value,
        baseline_variant,
        &exp_dir,
        &RunBehavior::default(),
        &execution,
    )?;
    let preflight_runtime_profiles = vec![runtime_profile.clone()];
    let VariantRuntimeProfile {
        agent_runtime: runtime_agent,
        configured_network_mode: network_mode,
        ..
    } = runtime_profile;
    let image = Some(runtime_agent.image.clone());

    let exp_id = json_value
        .pointer("/experiment/id")
        .and_then(|v| v.as_str())
        .unwrap_or("exp")
        .to_string();
    let workload_type = experiment_workload_type(&json_value)?;

    let policy_config = parse_policies(&json_value);
    let comparison = json_value
        .pointer("/scheduling/comparison")
        .and_then(|v| v.as_str())
        .unwrap_or("paired")
        .to_string();

    let evaluation_config = parse_evaluation_config(&json_value)?;
    let tasks_for_preflight = load_tasks(&dataset_path, &json_value).unwrap_or_default();
    let mut preflight_warnings = Vec::new();
    for check in check_dataset_task_ids(
        &tasks_for_preflight,
        &evaluation_config,
        &preflight_runtime_profiles,
    ) {
        if matches!(check.severity, PreflightSeverity::Warning) || !check.passed {
            preflight_warnings.push(format!("[{}] {}", check.name, check.message));
        }
    }
    {
        let grader_check = check_grader_reachable(
            &evaluation_config,
            &resolve_variant_runtime_profile(
                &json_value,
                baseline_variant,
                &exp_dir,
                &RunBehavior::default(),
                &execution,
            )?,
            baseline_variant,
            &tasks_for_preflight,
            &exp_dir,
        );
        if matches!(grader_check.severity, PreflightSeverity::Warning)
            && !grader_check.message.contains("no grader")
        {
            preflight_warnings.push(format!("[{}] {}", grader_check.name, grader_check.message));
        }
    }

    Ok(ExperimentSummary {
        exp_id,
        workload_type,
        dataset_path,
        task_count,
        replications,
        variant_count,
        total_trials,
        agent_runtime_command: runtime_agent.command_raw,
        image,
        network_mode,
        trajectory_path: runtime_agent.trajectory_path,
        causal_extraction: runtime_agent.causal_extraction,
        scheduling: match policy_config.scheduling {
            SchedulingPolicy::PairedInterleaved => "paired_interleaved".to_string(),
            SchedulingPolicy::VariantSequential => "variant_sequential".to_string(),
            SchedulingPolicy::Randomized => "randomized".to_string(),
        },
        state_policy: match policy_config.state {
            StatePolicy::IsolatePerTrial => "isolate_per_trial".to_string(),
            StatePolicy::PersistPerTask => "persist_per_task".to_string(),
            StatePolicy::Accumulate => "accumulate".to_string(),
        },
        comparison,
        retry_max_attempts: policy_config.retry_max_attempts,
        preflight_warnings,
    })
}

pub(crate) fn recover_reconciled_status(previous: &str) -> Result<&'static str> {
    match previous {
        "running" | "paused" | "interrupted" | "failed" => Ok("interrupted"),
        "completed" => Err(anyhow!("run already completed — nothing to recover")),
        "killed" => Err(anyhow!("run was killed — nothing to recover")),
        "preflight_failed" => Err(anyhow!(
            "run failed preflight before schedule execution — nothing to recover"
        )),
        "" => Err(anyhow!("missing run status — cannot recover")),
        other => Err(anyhow!("run status '{}' is not recoverable", other)),
    }
}

fn reconcile_runtime_trials_for_recovery(
    run_id: &str,
    run_dir: &Path,
    committed_by_schedule: &BTreeMap<usize, SlotCommitRecord>,
) -> Result<(usize, HashSet<String>)> {
    let mut released = 0usize;
    let mut runtime_state_trial_ids = HashSet::new();
    let mut store = open_trial_attempt_store(run_dir)?;
    for mut attempt in store.trial_attempts_for_recovery(run_id)? {
        runtime_state_trial_ids.insert(attempt.trial_id.clone());
        if committed_by_schedule
            .get(&attempt.schedule_idx)
            .is_some_and(|committed| committed.trial_id == attempt.trial_id)
        {
            attempt.state.phase = TrialPhase::Committed;
            attempt.state.paused_from_phase = None;
            store.upsert_trial_attempt_state(run_id, &attempt.trial_id, &attempt.state)?;
            let trial_dir = run_dir.join("trials").join(&attempt.trial_id);
            crate::trial::state::write_trial_attempt_state(&trial_dir, &attempt.state)
                .with_context(|| {
                    format!(
                        "failed to mirror committed runtime state for recovered trial {}",
                        attempt.trial_id
                    )
                })?;
            continue;
        }
        if !crate::trial::state::trial_phase_requires_recovery_release(&attempt.phase) {
            continue;
        }
        let trial_dir = run_dir.join("trials").join(&attempt.trial_id);
        cleanup_trial_runtime_required(run_dir, run_id, &attempt.trial_id, &trial_dir)
            .with_context(|| {
                format!(
                    "failed to clean runtime workers for recovered active trial {}",
                    attempt.trial_id
                )
            })?;
        write_trial_state(
            &trial_dir,
            &attempt.trial_id,
            "failed",
            None,
            None,
            Some("worker_lost_recovered"),
        )
        .with_context(|| {
            format!(
                "failed to mark recovered trial {} as worker_lost",
                attempt.trial_id
            )
        })?;
        attempt.state.phase = TrialPhase::Abandoned;
        attempt.state.paused_from_phase = None;
        store.upsert_trial_attempt_state(run_id, &attempt.trial_id, &attempt.state)?;
        crate::trial::state::write_trial_attempt_state(&trial_dir, &attempt.state).with_context(
            || {
                format!(
                    "failed to mirror abandoned runtime state for recovered trial {}",
                    attempt.trial_id
                )
            },
        )?;
        open_schedule_slot_store(run_dir)?
            .release_schedule_slot_to_pending(run_id, attempt.schedule_idx)?;
        released += 1;
    }

    Ok((released, runtime_state_trial_ids))
}

pub fn recover_run(run_dir: &Path, force: bool) -> Result<RecoverResult> {
    let _op_lease = acquire_run_operation_lease(run_dir, RunOperationType::Recover)?;
    let run_dir = run_dir
        .canonicalize()
        .map_err(|_| anyhow!("run_dir not found: {}", run_dir.display()))?;

    let control = load_run_control(&run_dir)?;
    let previous_status = run_control_status(&control).to_string();
    let recovered_status = recover_reconciled_status(&previous_status)?.to_string();
    let run_id = require_run_control_run_id(&control)?;
    let run_session = load_run_session_state(&run_dir)?;
    if run_session.run_id != run_id {
        return Err(anyhow!(
            "run session state mismatch: run_control has {}, run_session_state has {}",
            run_id,
            run_session.run_id
        ));
    }

    let mut progress = load_schedule_progress(&run_dir)?;
    let journal_records = load_slot_commit_records(&run_dir)?;
    adopt_engine_lease_for_recovery(&run_dir, &run_id, force)?;
    let committed_by_schedule = commit_record_by_schedule(&journal_records);
    let mut slot_store = open_schedule_slot_store(&run_dir)?;
    slot_store.ensure_schedule_slots(&run_id, &progress.schedule)?;
    for (schedule_idx, record) in &committed_by_schedule {
        slot_store.mark_schedule_slot_committed(
            &run_id,
            *schedule_idx,
            &record.trial_id,
            record.attempt,
            &record.slot_commit_id,
            &record.slot_status,
        )?;
    }
    let mut active_trials = run_control_active_trials(&control);
    let pending_completion_schedules = load_pending_trial_completion_records(&run_dir)?
        .keys()
        .copied()
        .collect::<HashSet<_>>();
    let mut active_trial_ids = active_trials
        .iter()
        .map(|trial| trial.trial_id.clone())
        .collect::<HashSet<_>>();
    for intent in load_trial_claim_intents(&run_dir)? {
        let Some(schedule_idx) = intent.schedule_idx else {
            continue;
        };
        if committed_by_schedule.contains_key(&schedule_idx)
            || pending_completion_schedules.contains(&schedule_idx)
            || active_trial_ids.contains(&intent.trial_id)
        {
            continue;
        }
        active_trial_ids.insert(intent.trial_id.clone());
        active_trials.push(intent);
    }

    let progress_by_schedule = progress
        .completed_slots
        .iter()
        .map(|slot| (slot.schedule_index, slot))
        .collect::<BTreeMap<_, _>>();
    let mut divergence_idx: Option<usize> = None;
    for slot in &progress.completed_slots {
        let matches_journal = committed_by_schedule
            .get(&slot.schedule_index)
            .map(|committed| {
                slot.slot_commit_id == committed.slot_commit_id
                    && slot.trial_id == committed.trial_id
                    && slot.status == committed.slot_status
            })
            .unwrap_or(false);
        if !matches_journal {
            divergence_idx = Some(
                divergence_idx
                    .map(|idx| idx.min(slot.schedule_index))
                    .unwrap_or(slot.schedule_index),
            );
        }
    }
    let mut reconciled_completed_slots = Vec::new();
    for (schedule_idx, committed) in &committed_by_schedule {
        if progress_by_schedule.get(schedule_idx).map(|slot| {
            slot.slot_commit_id == committed.slot_commit_id
                && slot.trial_id == committed.trial_id
                && slot.status == committed.slot_status
        }) != Some(true)
        {
            divergence_idx = Some(
                divergence_idx
                    .map(|idx| idx.min(*schedule_idx))
                    .unwrap_or(*schedule_idx),
            );
        }
        reconciled_completed_slots.push(SlotCompletion {
            schedule_index: *schedule_idx,
            trial_id: committed.trial_id.clone(),
            status: committed.slot_status.clone(),
            slot_commit_id: committed.slot_commit_id.clone(),
            attempt: committed.attempt.max(1),
        });
    }
    progress.completed_slots = reconciled_completed_slots;
    progress
        .completed_slots
        .sort_by_key(|slot| slot.schedule_index);
    if divergence_idx.is_some() {
        progress.pruned_variants.clear();
        progress.consecutive_failures.clear();
    }
    let committed_slots_verified = progress.completed_slots.len();
    let committed_schedules = progress
        .completed_slots
        .iter()
        .map(|slot| slot.schedule_index)
        .collect::<HashSet<_>>();
    progress.next_schedule_index = (0..progress.total_slots)
        .find(|schedule_idx| !committed_schedules.contains(schedule_idx))
        .unwrap_or(progress.total_slots);
    progress.schema_version = "schedule_progress_v2".to_string();
    progress.updated_at = Utc::now().to_rfc3339();
    let rewound_to = divergence_idx.unwrap_or(progress.next_schedule_index);

    let (runtime_trials_released, runtime_state_trial_ids) =
        reconcile_runtime_trials_for_recovery(&run_id, &run_dir, &committed_by_schedule)?;
    let mut active_trials_released = runtime_trials_released;
    for active in active_trials {
        if runtime_state_trial_ids.contains(&active.trial_id) {
            continue;
        }
        let Some(schedule_idx) = active.schedule_idx else {
            continue;
        };
        if schedule_idx < progress.next_schedule_index
            && committed_by_schedule.contains_key(&schedule_idx)
        {
            continue;
        }
        let trial_dir = run_dir.join("trials").join(&active.trial_id);
        if trial_dir.exists() {
            write_trial_state(
                &trial_dir,
                &active.trial_id,
                "failed",
                None,
                None,
                Some("worker_lost_recovered"),
            )
            .with_context(|| {
                format!(
                    "failed to mark recovered run-control trial {} as worker_lost",
                    active.trial_id
                )
            })?;
            crate::trial::state::reconcile_trial_attempt_as_abandoned(&trial_dir).with_context(
                || {
                    format!(
                        "failed to mark recovered run-control trial {} runtime state abandoned",
                        active.trial_id
                    )
                },
            )?;
        }
        slot_store.release_schedule_slot_to_pending(&run_id, schedule_idx)?;
        active_trials_released += 1;
    }
    for active_slot in slot_store.active_schedule_slots(&run_id)? {
        if committed_by_schedule.contains_key(&active_slot.schedule_idx) {
            continue;
        }

        if let Some(trial_id) = active_slot.trial_id.as_deref() {
            if runtime_state_trial_ids.contains(trial_id) {
                continue;
            }
            let trial_dir = run_dir.join("trials").join(trial_id);
            if trial_dir.exists() {
                cleanup_trial_runtime_required(&run_dir, &run_id, trial_id, &trial_dir)
                    .with_context(|| {
                        format!(
                            "failed to clean runtime workers for recovered active schedule slot {} trial {}",
                            active_slot.schedule_idx, trial_id
                        )
                    })?;
                write_trial_state(
                    &trial_dir,
                    trial_id,
                    "failed",
                    None,
                    None,
                    Some("worker_lost_recovered"),
                )
                .with_context(|| {
                    format!(
                        "failed to mark recovered active slot {} trial {} as worker_lost",
                        active_slot.schedule_idx, trial_id
                    )
                })?;
                crate::trial::state::reconcile_trial_attempt_as_abandoned(&trial_dir)
                    .with_context(|| {
                        format!(
                            "failed to mark recovered active slot {} trial {} runtime state abandoned",
                            active_slot.schedule_idx, trial_id
                        )
                    })?;
            }
        }

        slot_store.release_schedule_slot_to_pending(&run_id, active_slot.schedule_idx)?;
        active_trials_released += 1;
    }
    let label_drift_containers_removed = if runtime_trials_released > 0 {
        cleanup_run_runtime_required(&run_dir, &run_id).with_context(|| {
            format!(
                "failed to sweep labeled runtime workers for recovered run {}",
                run_id
            )
        })?
    } else {
        0
    };

    write_schedule_progress(&run_dir, &progress)?;
    write_run_control(&run_dir, &run_id, &recovered_status, &[], None)?;
    let notes = vec![
        format!("engine lease adopted for run {}", run_id),
        format!("committed slots verified {}", committed_slots_verified),
        "active trials reconciled and released".to_string(),
    ];
    let report = json!({
        "schema_version": "recovery_report_v1",
        "run_id": run_id.clone(),
        "previous_status": previous_status.clone(),
        "recovered_status": recovered_status.clone(),
        "rewound_to_schedule_idx": rewound_to,
        "active_trials_released": active_trials_released,
        "label_drift_containers_removed": label_drift_containers_removed,
        "committed_slots_verified": committed_slots_verified,
        "notes": notes,
        "recovered_at": Utc::now().to_rfc3339(),
    });
    let recovery_report_path = run_dir.join("runtime").join("recovery_report.json");
    atomic_write_json_pretty(&recovery_report_path, &report)?;

    Ok(RecoverResult {
        run_id,
        previous_status: previous_status.clone(),
        recovered_status,
        rewound_to_schedule_idx: rewound_to,
        active_trials_released,
        label_drift_containers_removed,
        committed_slots_verified,
        notes: report
            .pointer("/notes")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    })
}

pub fn replay_trial(run_dir: &Path, trial_id: &str, strict: bool) -> Result<ReplayResult> {
    let _op_lease = acquire_run_operation_lease(run_dir, RunOperationType::Replay)?;
    let run_dir = run_dir
        .canonicalize()
        .map_err(|_| anyhow!("run_dir not found: {}", run_dir.display()))?;
    let run_id = run_dir
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("run")
        .to_string();
    let resolved_path = run_dir.join("resolved_experiment.json");
    if !resolved_path.exists() {
        return Err(anyhow!(
            "missing resolved_experiment.json in {}",
            run_dir.display()
        ));
    }
    let json_value: Value = serde_json::from_slice(&fs::read(&resolved_path)?)?;
    let parent_trial_dir = run_dir.join("trials").join(trial_id);
    let prepared_manifest = load_prepared_task_environment_manifest(&parent_trial_dir)?;
    let (variants, _) = load_run_variants(&run_dir, &json_value)?;
    let variant_id = prepared_manifest.variant_id.as_str();
    let variant = find_variant_by_id(&variants, variant_id)?;
    let runtime_profile = resolve_variant_runtime_profile(
        &json_value,
        variant,
        &run_dir,
        &RunBehavior::default(),
        &RunExecutionOptions::default(),
    )?;
    let variant_args = runtime_profile.variant_args.clone();
    let agent_runtime = runtime_profile.agent_runtime;
    let agent_runtime_env = runtime_profile.agent_runtime_env;
    let effective_network_mode = runtime_profile.effective_network_mode;
    let runtime_experiment = runtime_profile.experiment;

    if strict && agent_runtime.integration_level != "control_full" {
        return Err(anyhow!(
            "strict replay requires integration_level control_full (found: {})",
            agent_runtime.integration_level
        ));
    }

    let replay_id = format!("replay_{}", Utc::now().format("%Y%m%d_%H%M%S"));
    let replay_dir = run_dir.join("replays").join(&replay_id);
    ensure_dir(&replay_dir)?;

    let replay_trial_id = format!("{}_{}", trial_id, replay_id);
    let task_boundary = materialize_packaged_task_boundary(&prepared_manifest.declaration)?;
    validate_task_boundary_workspace_materialization(&task_boundary)?;

    let replay_trial_dir = replay_dir.join("trial_1");
    ensure_dir(&replay_trial_dir)?;
    write_trial_state(
        &replay_trial_dir,
        &replay_trial_id,
        "running",
        None,
        None,
        None,
    )?;
    let mut trial_guard = TrialStateGuard::new(&replay_trial_dir, &replay_trial_id);

    let lineage_workspace_ref = {
        let store = open_lineage_store(&run_dir)?;
        if let Some(version_id) = store.latest_lineage_version_id_for_trial(&run_id, trial_id)? {
            store.lineage_workspace_ref_by_version(&version_id)?
        } else {
            None
        }
    };
    let prepared = prepare_task_environment(
        &run_dir,
        &replay_trial_dir,
        &run_id,
        &replay_trial_id,
        &runtime_experiment,
        variant,
        prepared_manifest.task_index,
        prepared_manifest.repl_idx,
        &task_boundary,
        &agent_runtime,
    )?;
    let PreparedTaskEnvironment {
        manifest: replay_prepared_manifest,
        trial_paths,
        io_paths: _,
        dynamic_mounts,
        trial_input: mut input,
    } = prepared;

    set_json_pointer_value(
        &mut input,
        "/runtime/control_plane/path",
        json!(DEFAULT_CONTAINER_CONTROL_PATH),
    )?;
    set_json_pointer_value(&mut input, "/runtime/control_plane/mode", json!("file"))?;
    let input_bytes = serde_json::to_vec_pretty(&input)?;
    let replay_task_sandbox_image = replay_prepared_manifest.task_sandbox_image()?.to_string();
    let replay_task_sandbox_workdir = replay_prepared_manifest.task_sandbox_workdir()?.to_string();

    let io_paths = prepare_io_paths(&trial_paths, &input_bytes)?;
    let runtime_env = build_runtime_contract_env(
        &run_id,
        &input,
        &io_paths,
        Some(replay_task_sandbox_image.as_str()),
        resolve_trial_timeout_ms(&input),
    );
    let run_request = TrialRunRequest {
        package_root: &run_dir,
        runtime_experiment: &runtime_experiment,
        runtime: &agent_runtime,
        variant_args: &variant_args,
        runtime_env: &runtime_env,
        runtime_overrides_env: &agent_runtime_env,
        trial_paths: &trial_paths,
        dynamic_mounts: &dynamic_mounts,
        secret_file_mounts: &runtime_profile.secret_file_mounts,
        io_paths: &io_paths,
        network_mode: effective_network_mode.as_str(),
        grader: None,
        grading_enabled: false,
        run_id: &run_id,
        task_image: replay_task_sandbox_image.as_str(),
        task_workdir: replay_task_sandbox_workdir.as_str(),
        task_materialization_kind: task_boundary.materialization.kind.clone(),
        agent_artifact: agent_runtime.agent_artifact.as_deref(),
        agent_artifact_mount_path: agent_runtime.agent_artifact_mount_path.as_deref(),
        agent_artifact_read_only: agent_runtime.agent_artifact_read_only,
    };
    let executor = LocalDockerExecutionBackend::new();
    let runtime_outcome = executor.execute_attempt(TrialRuntimeExecutionRequest {
        trial_dir: &replay_trial_dir,
        schedule_idx: 0,
        attempt_no: 1,
        run_request: &run_request,
        task_id: &replay_prepared_manifest.task_id,
        variant_id: &variant.id,
        repl_idx: replay_prepared_manifest.repl_idx,
        task_sandbox_plan: replay_prepared_manifest
            .task_sandbox_plan
            .as_ref()
            .ok_or_else(|| anyhow!("prepared replay task missing task sandbox plan"))?,
    })?;
    let status = runtime_outcome.agent_exit_status;
    let trial_output = runtime_outcome.trial_output;
    let result_present = runtime_outcome.result_present;
    let result_parse_error = runtime_outcome.result_parse_error;

    let outcome =
        agent_response_execution_outcome(&status, result_present, result_parse_error.as_deref());
    if status == "0" && outcome == "success" {
        trial_guard.complete("completed", None)?;
    } else if status != "0" {
        trial_guard.complete("failed", Some("harness_exit_nonzero"))?;
    } else if !result_present {
        trial_guard.complete("failed", Some("trial_output_missing"))?;
    } else if result_parse_error.is_some() {
        trial_guard.complete("failed", Some("trial_output_parse_error"))?;
    } else {
        trial_guard.complete("failed", Some("trial_output_error"))?;
    }

    let replay_grade = replay_grade_for_integration(&agent_runtime.integration_level).to_string();
    let artifact_store = ArtifactStore::new(run_dir.join("artifacts"));
    let trial_input_ref = artifact_store.put_bytes(&input_bytes)?;
    let trial_output_ref = artifact_store.put_bytes(&serde_json::to_vec_pretty(&trial_output)?)?;
    let manifest = json!({
        "schema_version": "replay_manifest_v1",
        "operation": "replay",
        "replay_id": replay_id.clone(),
        "parent_trial_id": trial_id,
        "strict": strict,
        "integration_level": agent_runtime.integration_level.clone(),
        "replay_grade": replay_grade.clone(),
        "trial_id": replay_trial_id.clone(),
        "refs": {
            "trial_input_ref": trial_input_ref,
            "trial_output_ref": trial_output_ref,
            "lineage_workspace_ref": lineage_workspace_ref,
        },
        "created_at": Utc::now().to_rfc3339(),
    });
    validate_schema_contract_value(&manifest, "replay manifest metadata")?;
    let mut store = open_attempt_object_store(&run_dir)?;
    store.upsert_attempt_object(
        &run_id,
        &replay_trial_id,
        0,
        1,
        "trial_input",
        &trial_input_ref,
        Some(&manifest),
    )?;
    store.upsert_attempt_object(
        &run_id,
        &replay_trial_id,
        0,
        1,
        "trial_output",
        &trial_output_ref,
        Some(&manifest),
    )?;
    open_runtime_operation_store(&run_dir)?
        .upsert_runtime_operation(&run_id, "replay", &replay_id, &manifest)?;
    crate::trial::state::reconcile_trial_attempt_as_committed(&replay_trial_dir)?;
    trial_paths.cleanup_scratch()?;

    Ok(ReplayResult {
        replay_dir,
        replay_id,
        parent_trial_id: trial_id.to_string(),
        strict,
        replay_grade,
        harness_status: status,
    })
}

pub(crate) fn replay_grade_for_integration(level: &str) -> &'static str {
    match level {
        "control_full" => "strict",
        "control_checkpoint" => "checkpointed",
        "cli_events" | "otel" => "best_effort",
        _ => "best_effort",
    }
}

pub fn fork_trial(
    run_dir: &Path,
    from_trial: &str,
    selector: &str,
    set_bindings: &BTreeMap<String, Value>,
    strict: bool,
) -> Result<ForkResult> {
    let _op_lease = acquire_run_operation_lease(run_dir, RunOperationType::Fork)?;
    fork_trial_inner(run_dir, from_trial, selector, set_bindings, strict)
}

pub(crate) fn fork_trial_inner(
    run_dir: &Path,
    from_trial: &str,
    selector: &str,
    set_bindings: &BTreeMap<String, Value>,
    strict: bool,
) -> Result<ForkResult> {
    let run_dir = run_dir
        .canonicalize()
        .map_err(|_| anyhow!("run_dir not found: {}", run_dir.display()))?;
    let resolved_path = run_dir.join("resolved_experiment.json");
    if !resolved_path.exists() {
        return Err(anyhow!(
            "missing resolved_experiment.json in {}",
            run_dir.display()
        ));
    }
    let json_value: Value = serde_json::from_slice(&fs::read(&resolved_path)?)?;
    let parsed_selector = parse_fork_selector(selector)?;

    let run_id = run_dir
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("run")
        .to_string();

    let parent_trial_dir = run_dir.join("trials").join(from_trial);
    let prepared_manifest = load_prepared_task_environment_manifest(&parent_trial_dir)?;
    let parent_output = load_trial_output_payload(&run_dir, &run_id, from_trial).ok();
    let (variants, _) = load_run_variants(&run_dir, &json_value)?;
    let variant_id = prepared_manifest.variant_id.as_str();
    let mut variant = find_variant_by_id(&variants, variant_id)?.clone();
    apply_variant_binding_overrides(&mut variant, set_bindings)?;
    let runtime_profile = resolve_variant_runtime_profile(
        &json_value,
        &variant,
        &run_dir,
        &RunBehavior::default(),
        &RunExecutionOptions::default(),
    )?;
    let variant_args = runtime_profile.variant_args.clone();
    let agent_runtime = runtime_profile.agent_runtime;
    let agent_runtime_env = runtime_profile.agent_runtime_env;
    let effective_network_mode = runtime_profile.effective_network_mode;
    let runtime_experiment = runtime_profile.experiment;

    if strict && agent_runtime.integration_level != "control_full" {
        return Err(anyhow!(
            "strict fork requires integration_level control_full (found: {})",
            agent_runtime.integration_level
        ));
    }
    let source_checkpoint = resolve_selector_checkpoint(
        &parsed_selector,
        parent_output.as_ref(),
        &run_dir.join("trials").join(from_trial),
        strict,
    )?;
    if strict && source_checkpoint.is_none() {
        return Err(anyhow!(
            "strict_source_unavailable: selector {} did not resolve to a committed checkpoint",
            selector
        ));
    }

    let fork_id = format!("fork_{}", Utc::now().format("%Y%m%d_%H%M%S"));
    let fork_dir = run_dir.join("forks").join(&fork_id);
    ensure_dir(&fork_dir)?;
    let fork_trial_id = format!("{}_{}", from_trial, fork_id);
    let task_boundary = materialize_packaged_task_boundary(&prepared_manifest.declaration)?;
    validate_task_boundary_workspace_materialization(&task_boundary)?;

    let fork_trial_dir = fork_dir.join("trial_1");
    ensure_dir(&fork_trial_dir)?;
    write_trial_state(
        &fork_trial_dir,
        &fork_trial_id,
        "running",
        None,
        source_checkpoint.as_deref(),
        None,
    )?;
    let mut trial_guard = TrialStateGuard::new(&fork_trial_dir, &fork_trial_id);

    let _checkpoint_workspace_ref = if let Some(ref checkpoint_token) = source_checkpoint {
        resolve_workspace_ref_from_checkpoint_token(&run_dir, checkpoint_token)?
    } else {
        None
    };
    let prepared = prepare_task_environment(
        &run_dir,
        &fork_trial_dir,
        &run_id,
        &fork_trial_id,
        &runtime_experiment,
        &variant,
        prepared_manifest.task_index,
        prepared_manifest.repl_idx,
        &task_boundary,
        &agent_runtime,
    )?;
    let PreparedTaskEnvironment {
        manifest: fork_prepared_manifest,
        trial_paths,
        io_paths: _,
        dynamic_mounts,
        trial_input: mut input,
    } = prepared;
    set_json_pointer_value(
        &mut input,
        "/ext/fork",
        json!({
            "parent_run_id": run_id,
            "parent_trial_id": from_trial,
            "selector": selector,
            "source_checkpoint": source_checkpoint.clone(),
            "strict": strict
        }),
    )?;
    set_json_pointer_value(
        &mut input,
        "/runtime/control_plane/path",
        json!(DEFAULT_CONTAINER_CONTROL_PATH),
    )?;
    set_json_pointer_value(&mut input, "/runtime/control_plane/mode", json!("file"))?;
    let input_bytes = serde_json::to_vec_pretty(&input)?;
    let fork_task_sandbox_image = fork_prepared_manifest.task_sandbox_image()?.to_string();
    let fork_task_sandbox_workdir = fork_prepared_manifest.task_sandbox_workdir()?.to_string();

    let io_paths = prepare_io_paths(&trial_paths, &input_bytes)?;
    let runtime_env = build_runtime_contract_env(
        &run_id,
        &input,
        &io_paths,
        Some(fork_task_sandbox_image.as_str()),
        resolve_trial_timeout_ms(&input),
    );
    let run_request = TrialRunRequest {
        package_root: &run_dir,
        runtime_experiment: &runtime_experiment,
        runtime: &agent_runtime,
        variant_args: &variant_args,
        runtime_env: &runtime_env,
        runtime_overrides_env: &agent_runtime_env,
        trial_paths: &trial_paths,
        dynamic_mounts: &dynamic_mounts,
        secret_file_mounts: &runtime_profile.secret_file_mounts,
        io_paths: &io_paths,
        network_mode: effective_network_mode.as_str(),
        grader: None,
        grading_enabled: false,
        run_id: &run_id,
        task_image: fork_task_sandbox_image.as_str(),
        task_workdir: fork_task_sandbox_workdir.as_str(),
        task_materialization_kind: task_boundary.materialization.kind.clone(),
        agent_artifact: agent_runtime.agent_artifact.as_deref(),
        agent_artifact_mount_path: agent_runtime.agent_artifact_mount_path.as_deref(),
        agent_artifact_read_only: agent_runtime.agent_artifact_read_only,
    };
    let executor = LocalDockerExecutionBackend::new();
    let runtime_outcome = executor.execute_attempt(TrialRuntimeExecutionRequest {
        trial_dir: &fork_trial_dir,
        schedule_idx: 0,
        attempt_no: 1,
        run_request: &run_request,
        task_id: &fork_prepared_manifest.task_id,
        variant_id: &variant.id,
        repl_idx: fork_prepared_manifest.repl_idx,
        task_sandbox_plan: fork_prepared_manifest
            .task_sandbox_plan
            .as_ref()
            .ok_or_else(|| anyhow!("prepared fork task missing task sandbox plan"))?,
    })?;
    let status = runtime_outcome.agent_exit_status;
    let trial_output = runtime_outcome.trial_output;
    let result_present = runtime_outcome.result_present;
    let result_parse_error = runtime_outcome.result_parse_error;
    let outcome =
        agent_response_execution_outcome(&status, result_present, result_parse_error.as_deref());
    if status == "0" && outcome == "success" {
        trial_guard.complete("completed", None)?;
    } else if status != "0" {
        trial_guard.complete("failed", Some("harness_exit_nonzero"))?;
    } else if !result_present {
        trial_guard.complete("failed", Some("trial_output_missing"))?;
    } else if result_parse_error.is_some() {
        trial_guard.complete("failed", Some("trial_output_parse_error"))?;
    } else {
        trial_guard.complete("failed", Some("trial_output_error"))?;
    }

    let replay_grade = replay_grade_for_integration(&agent_runtime.integration_level).to_string();
    let artifact_store = ArtifactStore::new(run_dir.join("artifacts"));
    let trial_input_ref = artifact_store.put_bytes(&input_bytes)?;
    let trial_output_ref = artifact_store.put_bytes(&serde_json::to_vec_pretty(&trial_output)?)?;
    let manifest = json!({
        "schema_version": "fork_manifest_v1",
        "operation": "fork",
        "fork_id": fork_id.clone(),
        "parent_trial_id": from_trial,
        "selector": selector,
        "source_checkpoint": source_checkpoint.clone(),
        "strict": strict,
        "integration_level": agent_runtime.integration_level.clone(),
        "replay_grade": replay_grade.clone(),
        "trial_id": fork_trial_id.clone(),
        "refs": {
            "trial_input_ref": trial_input_ref,
            "trial_output_ref": trial_output_ref,
        },
        "created_at": Utc::now().to_rfc3339(),
    });
    validate_schema_contract_value(&manifest, "fork manifest metadata")?;
    let mut store = open_attempt_object_store(&run_dir)?;
    store.upsert_attempt_object(
        &run_id,
        &fork_trial_id,
        0,
        1,
        "trial_input",
        &trial_input_ref,
        Some(&manifest),
    )?;
    store.upsert_attempt_object(
        &run_id,
        &fork_trial_id,
        0,
        1,
        "trial_output",
        &trial_output_ref,
        Some(&manifest),
    )?;
    open_runtime_operation_store(&run_dir)?
        .upsert_runtime_operation(&run_id, "fork", &fork_id, &manifest)?;
    crate::trial::state::reconcile_trial_attempt_as_committed(&fork_trial_dir)?;
    trial_paths.cleanup_scratch()?;

    Ok(ForkResult {
        fork_dir,
        fork_id,
        parent_trial_id: from_trial.to_string(),
        selector: selector.to_string(),
        strict,
        replay_grade,
        harness_status: status,
        source_checkpoint,
    })
}

pub(crate) fn load_trial_payload_from_attempt_objects(
    run_dir: &Path,
    run_id: &str,
    trial_id: &str,
    role: &str,
) -> Result<Option<Value>> {
    let store = open_attempt_object_store(run_dir)?;
    let Some(object_ref) = store.latest_attempt_object_ref(run_id, trial_id, role)? else {
        return Ok(None);
    };
    let artifact_store = ArtifactStore::new(run_dir.join("artifacts"));
    let payload = artifact_store.read_ref(&object_ref)?;
    Ok(Some(serde_json::from_slice(&payload)?))
}

pub(crate) fn load_trial_output_payload(
    run_dir: &Path,
    run_id: &str,
    trial_id: &str,
) -> Result<Value> {
    if let Some(value) =
        load_trial_payload_from_attempt_objects(run_dir, run_id, trial_id, "trial_output")?
    {
        return Ok(value);
    }
    Err(anyhow!(
        "trial output payload not found in runtime store for trial '{}'",
        trial_id
    ))
}

pub(crate) fn resolve_workspace_ref_from_checkpoint_token(
    run_dir: &Path,
    token: &str,
) -> Result<Option<String>> {
    let Some(version_id) = token.strip_prefix("lineage:") else {
        return Ok(None);
    };
    let store = open_lineage_store(run_dir)?;
    store.lineage_workspace_ref_by_version(version_id)
}

pub(crate) fn resolve_resume_selector(
    run_dir: &Path,
    run_id: &str,
    trial_id: &str,
    preferred_label: Option<&str>,
) -> Result<String> {
    let output = load_trial_output_payload(run_dir, run_id, trial_id)?;
    let checkpoints = output
        .get("checkpoints")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if checkpoints.is_empty() {
        return Err(anyhow!(
            "resume_no_checkpoint: paused trial has no declared checkpoints"
        ));
    }

    if let Some(label) = preferred_label {
        let found = checkpoints.iter().any(|cp| {
            cp.get("logical_name").and_then(|v| v.as_str()) == Some(label)
                || cp.get("path").and_then(|v| v.as_str()) == Some(label)
        });
        if !found {
            return Err(anyhow!(
                "resume_checkpoint_not_found: label '{}' was not found in trial checkpoints",
                label
            ));
        }
        return Ok(format!("checkpoint:{}", label));
    }

    let mut best_with_step: Option<(u64, Value)> = None;
    for cp in checkpoints.iter() {
        if let Some(step) = cp.get("step").and_then(|v| v.as_u64()) {
            match best_with_step {
                Some((cur, _)) if step <= cur => {}
                _ => best_with_step = Some((step, cp.clone())),
            }
        }
    }
    let chosen = if let Some((_, cp)) = best_with_step {
        cp
    } else {
        checkpoints
            .last()
            .cloned()
            .ok_or_else(|| anyhow!("resume_no_checkpoint"))?
    };
    if let Some(name) = chosen.get("logical_name").and_then(|v| v.as_str()) {
        return Ok(format!("checkpoint:{}", name));
    }
    if let Some(path) = chosen.get("path").and_then(|v| v.as_str()) {
        return Ok(format!("checkpoint:{}", path));
    }
    Err(anyhow!("resume_no_checkpoint_token"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContractPathRoot {
    In,
    Out,
    State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContractPathMode {
    ContainerMount,
    RuntimeEvents,
}

#[derive(Debug, Clone)]
pub(crate) struct ContractPathHostRoots {
    pub(crate) in_dir: PathBuf,
    pub(crate) out_dir: PathBuf,
    pub(crate) state_dir: PathBuf,
    pub(crate) workspace_dir: PathBuf,
}

impl ContractPathHostRoots {
    pub(crate) fn from_trial_paths(paths: &TrialPaths) -> Self {
        Self {
            in_dir: paths.in_dir.clone(),
            out_dir: paths.out.clone(),
            state_dir: paths.state.clone(),
            workspace_dir: paths.workspace.clone(),
        }
    }

    pub(crate) fn from_trial_dir(trial_dir: &Path) -> Self {
        Self {
            in_dir: trial_dir.join("in"),
            out_dir: trial_dir.join("out"),
            state_dir: trial_dir.join("state"),
            workspace_dir: trial_dir.join("workspace"),
        }
    }

    fn base_for(&self, root: ContractPathRoot) -> &Path {
        match root {
            ContractPathRoot::In => self.in_dir.as_path(),
            ContractPathRoot::Out => self.out_dir.as_path(),
            ContractPathRoot::State => self.state_dir.as_path(),
        }
    }
}

pub(crate) fn strip_contract_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if path == prefix {
        return Some("");
    }
    let rest = path.strip_prefix(prefix)?;
    if rest.starts_with('/') {
        Some(rest)
    } else {
        None
    }
}

pub(crate) fn resolve_contract_path_components(path: &str) -> Option<(ContractPathRoot, &str)> {
    if let Some(rest) = strip_contract_prefix(path, BUCEPHALUS_CONTRACT_IN_DIR) {
        return Some((ContractPathRoot::In, rest));
    }
    if let Some(rest) = strip_contract_prefix(path, BUCEPHALUS_CONTRACT_OUT_DIR) {
        return Some((ContractPathRoot::Out, rest));
    }
    if let Some(rest) = strip_contract_prefix(path, BUCEPHALUS_CONTRACT_STATE_DIR) {
        return Some((ContractPathRoot::State, rest));
    }
    None
}

pub(crate) fn strip_task_workdir_placeholder_prefix(path: &str) -> Option<&str> {
    if path == BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER {
        return Some("");
    }
    let rest = path.strip_prefix(BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER)?;
    if rest.starts_with('/') {
        Some(rest)
    } else {
        None
    }
}

pub(crate) fn mode_allows_root(mode: ContractPathMode, root: ContractPathRoot) -> bool {
    match mode {
        ContractPathMode::ContainerMount => {
            matches!(root, ContractPathRoot::In | ContractPathRoot::Out)
        }
        ContractPathMode::RuntimeEvents => {
            matches!(
                root,
                ContractPathRoot::In | ContractPathRoot::Out | ContractPathRoot::State
            )
        }
    }
}

pub(crate) fn map_contract_path_to_host(
    path: &str,
    roots: &ContractPathHostRoots,
    mode: ContractPathMode,
) -> Result<PathBuf> {
    let raw = match mode {
        ContractPathMode::ContainerMount => path.trim(),
        ContractPathMode::RuntimeEvents => path,
    };
    if raw.is_empty() {
        return Err(match mode {
            ContractPathMode::ContainerMount => anyhow!("container path is empty"),
            ContractPathMode::RuntimeEvents => anyhow!(
                "runtime event path must be absolute when resolving trial events: {}",
                raw
            ),
        });
    }
    if matches!(mode, ContractPathMode::ContainerMount) {
        if let Some(rest) = strip_task_workdir_placeholder_prefix(raw) {
            return Ok(roots.workspace_dir.join(rest.trim_start_matches('/')));
        }
    }
    if !raw.starts_with('/') {
        return Err(match mode {
            ContractPathMode::ContainerMount => anyhow!("container path must be absolute: {}", raw),
            ContractPathMode::RuntimeEvents => anyhow!(
                "runtime event path must be absolute when resolving trial events: {}",
                raw
            ),
        });
    }

    let Some((root, rest)) = resolve_contract_path_components(raw) else {
        return Err(match mode {
            ContractPathMode::ContainerMount => {
                anyhow!("unsupported container mount path: {}", raw)
            }
            ContractPathMode::RuntimeEvents => {
                anyhow!("unsupported runtime event path for trial: {}", raw)
            }
        });
    };

    if !mode_allows_root(mode, root) {
        return Err(match mode {
            ContractPathMode::ContainerMount => {
                anyhow!("unsupported container mount path: {}", raw)
            }
            ContractPathMode::RuntimeEvents => {
                anyhow!("unsupported runtime event path for trial: {}", raw)
            }
        });
    }

    Ok(roots.base_for(root).join(rest.trim_start_matches('/')))
}

pub(crate) fn resolve_event_path_for_trial(events_path: &str, trial_dir: &Path) -> Result<PathBuf> {
    map_contract_path_to_host(
        events_path,
        &ContractPathHostRoots::from_trial_dir(trial_dir),
        ContractPathMode::RuntimeEvents,
    )
}

pub fn run_experiment(path: &Path) -> Result<RunResult> {
    run_experiment_with_behavior(path, RunBehavior::default(), RunExecutionOptions::default())
}

pub fn run_experiment_with_options(path: &Path, options: RunExecutionOptions) -> Result<RunResult> {
    run_experiment_with_behavior(path, RunBehavior::default(), options)
}

pub fn run_smoke_test_with_options(path: &Path, options: RunExecutionOptions) -> Result<RunResult> {
    let behavior = RunBehavior {
        smoke_test: true,
        ..RunBehavior::default()
    };
    let result = run_experiment_with_behavior(path, behavior, options)?;
    ensure_smoke_test_completed(result)
}

pub fn run_smoke_test_strict_with_options(
    path: &Path,
    options: RunExecutionOptions,
) -> Result<RunResult> {
    let behavior = RunBehavior {
        network_mode_override: None,
        require_network_none: true,
        smoke_test: true,
    };
    let result = run_experiment_with_behavior(path, behavior, options)?;
    ensure_smoke_test_completed(result)
}

pub(crate) fn ensure_smoke_test_completed(result: RunResult) -> Result<RunResult> {
    let control = load_run_control(&result.run_dir)?;
    let status = run_control_status(&control);
    if status != "completed" {
        return Err(anyhow!(
            "smoke test did not complete successfully (status={}, run_id={}, run_dir={})",
            status,
            result.run_id,
            result.run_dir.display()
        ));
    }
    let progress = load_schedule_progress(&result.run_dir)?;
    if progress.completed_slots.len() != progress.total_slots {
        return Err(anyhow!(
            "smoke test completed scheduler run but did not commit every slot (run_id={}, run_dir={}, completed={}, total={})",
            result.run_id,
            result.run_dir.display(),
            progress.completed_slots.len(),
            progress.total_slots
        ));
    }
    let failed_slots = progress
        .completed_slots
        .iter()
        .filter(|slot| slot.status != "completed")
        .map(|slot| {
            format!(
                "schedule_idx={} trial_id={} status={}",
                slot.schedule_index, slot.trial_id, slot.status
            )
        })
        .collect::<Vec<_>>();
    if !failed_slots.is_empty() {
        return Err(anyhow!(
            "smoke test completed scheduler run but trial slots failed (run_id={}, run_dir={}, failures={})",
            result.run_id,
            result.run_dir.display(),
            failed_slots.join("; ")
        ));
    }
    Ok(result)
}

pub fn run_experiment_strict(path: &Path) -> Result<RunResult> {
    run_experiment_strict_with_options(path, RunExecutionOptions::default())
}

pub fn run_experiment_strict_with_options(
    path: &Path,
    options: RunExecutionOptions,
) -> Result<RunResult> {
    let behavior = RunBehavior {
        network_mode_override: None,
        require_network_none: true,
        smoke_test: false,
    };
    run_experiment_with_behavior(path, behavior, options)
}

pub fn continue_run(run_dir: &Path) -> Result<RunResult> {
    continue_run_with_options(run_dir, RunExecutionOptions::default())
}

#[cfg(test)]
pub(crate) fn read_control_seq(control_path: &Path) -> Result<u64> {
    if !control_path.exists() {
        return Ok(0);
    }
    let value = load_json_file(control_path)?;
    Ok(value.pointer("/seq").and_then(|v| v.as_u64()).unwrap_or(0))
}

#[cfg(test)]
pub(crate) fn control_ack_received(
    events_path: &Path,
    action: &str,
    control_version: &str,
) -> Result<bool> {
    if !events_path.exists() {
        return Ok(false);
    }
    let data = fs::read_to_string(events_path)?;
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if parsed.get("event_type").and_then(|v| v.as_str()) != Some("control_ack") {
            continue;
        }
        if parsed
            .get("action_observed")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            != action
        {
            continue;
        }
        if parsed
            .get("control_version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            == control_version
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn parse_fork_selector(selector: &str) -> Result<ForkSelector> {
    let (kind, value) = selector
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid selector '{}': expected kind:value", selector))?;
    match kind {
        "checkpoint" => {
            if value.trim().is_empty() {
                return Err(anyhow!(
                    "invalid selector '{}': checkpoint name empty",
                    selector
                ));
            }
            Ok(ForkSelector::Checkpoint(value.to_string()))
        }
        "step" => Ok(ForkSelector::Step(value.parse::<u64>().map_err(|_| {
            anyhow!("invalid selector '{}': step must be integer", selector)
        })?)),
        "event_seq" => Ok(ForkSelector::EventSeq(value.parse::<u64>().map_err(
            |_| anyhow!("invalid selector '{}': event_seq must be integer", selector),
        )?)),
        _ => Err(anyhow!(
            "invalid selector kind '{}': expected checkpoint|step|event_seq",
            kind
        )),
    }
}

pub(crate) fn resolve_selector_checkpoint(
    selector: &ForkSelector,
    trial_output: Option<&Value>,
    trial_dir: &Path,
    strict: bool,
) -> Result<Option<String>> {
    let checkpoints = trial_output
        .and_then(|v| v.get("checkpoints"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let selected = match selector {
        ForkSelector::Checkpoint(name) => checkpoints.into_iter().find(|cp| {
            cp.get("logical_name").and_then(|v| v.as_str()) == Some(name.as_str())
                || cp.get("path").and_then(|v| v.as_str()) == Some(name.as_str())
        }),
        ForkSelector::Step(step) => checkpoints
            .into_iter()
            .filter_map(|cp| {
                let cp_step = cp.get("step").and_then(|v| v.as_u64());
                cp_step.map(|s| (s, cp))
            })
            .filter(|(s, _)| *s <= *step)
            .max_by_key(|(s, _)| *s)
            .map(|(_, cp)| cp),
        ForkSelector::EventSeq(seq) => checkpoints
            .into_iter()
            .filter_map(|cp| {
                let cp_step = cp.get("step").and_then(|v| v.as_u64());
                cp_step.map(|s| (s, cp))
            })
            .filter(|(s, _)| *s <= *seq)
            .max_by_key(|(s, _)| *s)
            .map(|(_, cp)| cp),
    };

    let Some(cp) = selected else {
        if strict {
            return Err(anyhow!(
                "strict_source_unavailable: selector checkpoint not found"
            ));
        }
        return Ok(None);
    };

    if let Some(run_dir) = infer_run_dir_from_path(trial_dir) {
        let run_id = run_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("run")
            .to_string();
        let trial_id = trial_dir
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow!("unable to infer trial_id from {}", trial_dir.display()))?;
        let store = open_lineage_store(&run_dir)?;
        if let Some(version_id) = store.latest_lineage_version_id_for_trial(&run_id, trial_id)? {
            return Ok(Some(format!("lineage:{}", version_id)));
        }
        if strict {
            return Err(anyhow!(
                "strict_source_unavailable: selector resolved but lineage version is unavailable"
            ));
        }
        return Ok(None);
    }

    let raw_path = cp
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("invalid checkpoint entry: missing path"))?;
    let resolved = resolve_event_path_for_trial(raw_path, trial_dir)?;
    if strict && !resolved.exists() {
        return Err(anyhow!(
            "strict_source_unavailable: checkpoint path not found {}",
            resolved.display()
        ));
    }
    if resolved.exists() {
        return Ok(Some(resolved.to_string_lossy().to_string()));
    }

    if strict {
        return Err(anyhow!(
            "strict_source_unavailable: checkpoint path not found {}",
            trial_dir.display()
        ));
    }
    Ok(None)
}

pub(crate) fn apply_variant_binding_overrides(
    variant: &mut Variant,
    set_bindings: &BTreeMap<String, Value>,
) -> Result<()> {
    if set_bindings.is_empty() {
        return Ok(());
    }
    if !variant.bindings.is_object() {
        variant.bindings = json!({});
    }
    for (key, value) in set_bindings {
        let pointer = format!("/{}", key.split('.').collect::<Vec<_>>().join("/"));
        set_json_pointer_value(&mut variant.bindings, &pointer, value.clone())?;
    }
    Ok(())
}
pub(crate) fn tokenize_command_string(raw: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            continue;
        }
        if in_double {
            if ch == '"' {
                in_double = false;
            } else if ch == '\\' {
                escaped = true;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '\'' => in_single = true,
            '"' => in_double = true,
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if escaped || in_single || in_double {
        return Err(anyhow!("agent.command has unclosed quote/escape"));
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if tokens.is_empty() {
        return Err(anyhow!("agent.command must not be empty"));
    }
    Ok(tokens)
}

pub(crate) fn agent_artifact_archive_flag(path: &Path) -> Option<&'static str> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some("-xzf")
    } else if name.ends_with(".tar") {
        Some("-xf")
    } else {
        None
    }
}

pub(crate) fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| {
                let lower = value.to_ascii_lowercase();
                lower == "exe" || lower == "bat" || lower == "cmd"
            })
            .unwrap_or(false)
    }
}

pub(crate) fn read_file_head(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut buf = vec![0u8; max_bytes];
    let read = file.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

pub(crate) fn normalize_shell_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(|ch: char| {
        ch == '"'
            || ch == '\''
            || ch == '`'
            || ch == ';'
            || ch == ','
            || ch == '('
            || ch == ')'
            || ch == '['
            || ch == ']'
            || ch == '{'
            || ch == '}'
    });
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    Some(trimmed.to_string())
}

pub(crate) fn token_looks_like_script_source_path(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    AGENT_ARTIFACT_SCRIPT_SOURCE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(ext))
}

pub(crate) fn validate_agent_artifact_entrypoint_script(
    entrypoint_path: &Path,
    artifact_mount_path: &str,
    context: &str,
) -> Result<()> {
    let head = read_file_head(entrypoint_path, AGENT_ARTIFACT_ENTRYPOINT_HEAD_BYTES)?;
    if !head.starts_with(b"#!") {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&head);
    let Some(_) = text.lines().next() else {
        return Ok(());
    };
    for (line_idx, line) in text.lines().take(8).enumerate() {
        let trimmed_line = line.trim_start();
        if line_idx > 0
            && !(trimmed_line.starts_with("exec ")
                || trimmed_line == "exec"
                || trimmed_line.starts_with("exec\t"))
        {
            continue;
        }
        for raw in line.split_whitespace() {
            let Some(token) = normalize_shell_token(raw) else {
                continue;
            };
            if token.starts_with("#!") {
                let shebang_target = token.trim_start_matches("#!");
                if shebang_target == "/usr/bin/env" {
                    continue;
                }
                if shebang_target.starts_with('/')
                    && !path_is_under_runtime_root(shebang_target, artifact_mount_path)
                {
                    return Err(anyhow!(
                        "{} entrypoint delegates to image-resident path '{}'; only artifact mount paths under '{}' are allowed",
                        context,
                        shebang_target,
                        artifact_mount_path
                    ));
                }
                continue;
            }
            if !token.starts_with('/') {
                continue;
            }
            if path_is_under_runtime_root(&token, artifact_mount_path) {
                if token_looks_like_script_source_path(&token) {
                    return Err(anyhow!(
                        "{} entrypoint delegates to readable script path '{}'; bundle a binary entrypoint instead",
                        context,
                        token
                    ));
                }
                continue;
            }
            return Err(anyhow!(
                "{} entrypoint delegates to image-resident path '{}'; only artifact mount paths under '{}' are allowed",
                context,
                token,
                artifact_mount_path
            ));
        }
    }
    Ok(())
}

fn path_is_under_runtime_root(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[derive(Debug, Clone)]
pub(crate) struct CommandArtifactTarget {
    token_index: usize,
    raw_token: String,
    resolved_path: PathBuf,
}

pub(crate) fn resolve_artifact_path_from_command_token(
    root: &Path,
    artifact_mount_path: &str,
    token_index: usize,
    token: &str,
    context: &str,
) -> Result<Option<CommandArtifactTarget>> {
    if token.is_empty() {
        return Ok(None);
    }
    let Some(rest) = token.strip_prefix(artifact_mount_path) else {
        return Ok(None);
    };
    let relative = rest.trim_start_matches('/');
    if relative.is_empty() {
        return Ok(None);
    }
    let resolved = normalize_path(&root.join(relative));
    let root_cmp = canonicalize_best_effort(root);
    let resolved_cmp = canonicalize_best_effort(&resolved);
    if !resolved_cmp.starts_with(&root_cmp) {
        return Err(anyhow!(
            "{} runtime.command[{}] escapes artifact root: '{}'",
            context,
            token_index,
            token
        ));
    }
    if !resolved.exists() {
        return Err(anyhow!(
            "{} runtime.command[{}] references artifact path '{}' but it does not exist in {}",
            context,
            token_index,
            token,
            root.display()
        ));
    }
    Ok(Some(CommandArtifactTarget {
        token_index,
        raw_token: token.to_string(),
        resolved_path: resolved,
    }))
}

pub(crate) fn resolve_command_artifact_targets(
    root: &Path,
    artifact_mount_path: &str,
    command: &[String],
    context: &str,
) -> Result<Vec<CommandArtifactTarget>> {
    if command.is_empty() {
        return Err(anyhow!("{} runtime.command must not be empty", context));
    }

    let mut targets = Vec::new();
    let mut first_bin_candidate: Option<(String, PathBuf)> = None;

    let first = command[0].trim();
    if let Some(target) =
        resolve_artifact_path_from_command_token(root, artifact_mount_path, 0, first, context)?
    {
        targets.push(target);
    } else if !first.contains('/') {
        let candidate = normalize_path(&root.join("bin").join(first));
        first_bin_candidate = Some((first.to_string(), candidate.clone()));
        if candidate.exists() {
            targets.push(CommandArtifactTarget {
                token_index: 0,
                raw_token: first.to_string(),
                resolved_path: candidate,
            });
        }
    }

    for (idx, token) in command.iter().enumerate().skip(1) {
        if let Some(target) = resolve_artifact_path_from_command_token(
            root,
            artifact_mount_path,
            idx,
            token.trim(),
            context,
        )? {
            targets.push(target);
        }
    }

    if targets.is_empty() {
        if let Some((token, candidate)) = first_bin_candidate {
            return Err(anyhow!(
                "{} runtime.command[0] '{}' did not resolve to artifact executable {} and no explicit artifact mount path '{}' was referenced",
                context,
                token,
                candidate.display(),
                artifact_mount_path
            ));
        }
        return Err(anyhow!(
            "{} runtime.command does not reference the mounted artifact; point it at '{}' or a binary under '{}/bin'",
            context,
            artifact_mount_path,
            artifact_mount_path.trim_end_matches('/')
        ));
    }

    Ok(targets)
}

pub(crate) fn validate_agent_artifact_root(
    root: &Path,
    artifact_mount_path: &str,
    command: &[String],
    context: &str,
) -> Result<()> {
    if !root.is_dir() {
        return Err(anyhow!(
            "{} artifact root must be a directory: {}",
            context,
            root.display()
        ));
    }
    let targets = resolve_command_artifact_targets(root, artifact_mount_path, command, context)?;
    if let Some(primary) = targets.iter().find(|target| target.token_index == 0) {
        if !is_executable_file(&primary.resolved_path) {
            return Err(anyhow!(
                "{} runtime.command[0] '{}' is not executable inside artifact: {}",
                context,
                primary.raw_token,
                primary.resolved_path.display()
            ));
        }
        validate_agent_artifact_entrypoint_script(
            &primary.resolved_path,
            artifact_mount_path,
            context,
        )?;
    }
    Ok(())
}

pub(crate) fn validate_agent_artifact_path(
    path: &Path,
    artifact_mount_path: &str,
    command: &[String],
    context: &str,
) -> Result<()> {
    if path.is_dir() {
        return validate_agent_artifact_root(path, artifact_mount_path, command, context);
    }
    if !path.is_file() {
        return Err(anyhow!(
            "{} artifact path is not a file or directory: {}",
            context,
            path.display()
        ));
    }
    let Some(tar_flag) = agent_artifact_archive_flag(path) else {
        return Err(anyhow!(
            "{} artifact archive must use .tar/.tar.gz/.tgz: {}",
            context,
            path.display()
        ));
    };
    let staging_dir = env::temp_dir().join(format!(
        "bucephalus_artifact_validate_{}_{}",
        std::process::id(),
        Utc::now().timestamp_micros()
    ));
    ensure_dir(&staging_dir)?;
    let artifact_arg = path.to_string_lossy().to_string();
    let staging_arg = staging_dir.to_string_lossy().to_string();
    let unpack_out = Command::new("tar")
        .args([tar_flag, artifact_arg.as_str(), "-C", staging_arg.as_str()])
        .output()?;
    if !unpack_out.status.success() {
        remove_path_if_exists(&staging_dir).with_context(|| {
            format!(
                "failed to remove invalid artifact staging directory {}",
                staging_dir.display()
            )
        })?;
        return Err(anyhow!(
            "{} failed to unpack artifact archive {}: {}",
            context,
            path.display(),
            output_error_detail(&unpack_out)
        ));
    }
    let validation =
        validate_agent_artifact_root(&staging_dir, artifact_mount_path, command, context);
    remove_path_if_exists(&staging_dir).with_context(|| {
        format!(
            "failed to remove artifact validation staging directory {}",
            staging_dir.display()
        )
    })?;
    validation
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeArtifactValidationSpec {
    pointer: String,
    artifact_path: String,
    artifact_mount_path: String,
    command: Vec<String>,
}

pub(crate) fn parse_optional_command_field_named(
    value: Option<&Value>,
    field: &str,
) -> Result<Option<Vec<String>>> {
    match value {
        Some(Value::String(raw)) => Ok(Some(tokenize_command_string(raw)?)),
        Some(Value::Array(_)) => {
            let parts = parse_string_array_field(value, field)?;
            if parts.is_empty() {
                return Err(anyhow!("{} must not be empty", field));
            }
            Ok(Some(parts))
        }
        Some(_) => Err(anyhow!("{} must be a string or string[]", field)),
        None => Ok(None),
    }
}

pub(crate) fn command_for_artifact_validation(
    agent: Option<&Value>,
    field_prefix: &str,
    inherited_command: Option<&Vec<String>>,
) -> Result<Option<Vec<String>>> {
    let local = parse_optional_command_field_named(
        agent.and_then(|value| value.get("command")),
        &format!("{}/command", field_prefix),
    )?;
    if local.is_some() {
        return Ok(local);
    }
    Ok(inherited_command.cloned())
}

pub(crate) fn collect_runtime_artifact_validation_specs(
    experiment: &Value,
) -> Result<Vec<RuntimeArtifactValidationSpec>> {
    let root_agent = experiment.pointer("/trial_runtime/agent");
    let root_command = command_for_artifact_validation(root_agent, "/trial_runtime/agent", None)?;
    let mut specs = Vec::new();

    let mut push_spec = |pointer: String,
                         agent: Option<&Value>,
                         inherited_command: Option<&Vec<String>>|
     -> Result<()> {
        let Some(artifact) = agent.and_then(|value| value.get("mount")) else {
            return Ok(());
        };
        let artifact = artifact
            .as_object()
            .ok_or_else(|| anyhow!("{} must be an object with source and mount", pointer))?;
        let path = artifact
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{}.source is required", pointer))?;
        let artifact_mount_path = artifact
            .get("mount")
            .and_then(Value::as_object)
            .and_then(|mount| mount.get("path"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{}.mount.path is required", pointer))?
            .to_string();
        let command = command_for_artifact_validation(
            agent,
            pointer.trim_end_matches("/mount"),
            inherited_command,
        )?
        .ok_or_else(|| anyhow!("{} requires a command to validate artifact usage", pointer))?;
        specs.push(RuntimeArtifactValidationSpec {
            pointer,
            artifact_path: path.to_string(),
            artifact_mount_path,
            command,
        });
        Ok(())
    };

    push_spec("/trial_runtime/agent/mount".to_string(), root_agent, None)?;

    if let Some(variants) = experiment
        .pointer("/matrix/variants")
        .and_then(Value::as_array)
    {
        for (idx, variant) in variants.iter().enumerate() {
            push_spec(
                format!("/matrix/variants/{}/overrides/agent/mount", idx),
                variant.pointer("/overrides/agent"),
                root_command.as_ref(),
            )?;
        }
    }

    Ok(specs)
}

pub(crate) fn validate_packaged_runtime_artifacts(
    package_dir: &Path,
    experiment: &Value,
) -> Result<()> {
    let mut seen_specs = HashSet::new();
    for spec in collect_runtime_artifact_validation_specs(experiment)? {
        let trimmed = spec.artifact_path.trim();
        if trimmed.is_empty() {
            continue;
        }
        let dedupe_key = format!("{}\u{0}{}", trimmed, spec.command.join("\u{1}"));
        if !seen_specs.insert(dedupe_key) {
            continue;
        }
        let artifact_path =
            resolve_package_path_under_root(package_dir, trimmed, spec.pointer.as_str())?;
        let context = format!("runtime artifact {} ({})", trimmed, spec.pointer);
        validate_agent_artifact_path(
            &artifact_path,
            &spec.artifact_mount_path,
            &spec.command,
            context.as_str(),
        )?;
    }
    Ok(())
}

pub(crate) fn configured_network_mode(json_value: &Value) -> Result<String> {
    json_value
        .pointer("/runtime/network/task_sandbox")
        .or_else(|| json_value.pointer("/runtime/network/default"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .ok_or_else(|| anyhow!("missing /runtime/network/task_sandbox"))
}

pub(crate) fn emit_slot_commit_progress(
    run_id: &str,
    completed_slots: usize,
    total_slots: usize,
    schedule_idx: usize,
    trial_id: &str,
    slot_status: &str,
) {
    let pct = if total_slots == 0 {
        100.0
    } else {
        (completed_slots as f64 / total_slots as f64) * 100.0
    };
    emit_run_log(
        run_id,
        format!(
            "progress {}/{} ({:.1}%) slot={} trial={} status={}",
            completed_slots, total_slots, pct, schedule_idx, trial_id, slot_status
        ),
    );
}

pub(crate) fn parse_local_worker_capacity_ceiling_from_env() -> Result<Option<usize>> {
    match std::env::var(BUCEPHALUS_LOCAL_WORKER_MAX_IN_FLIGHT_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = trimmed.parse::<usize>().map_err(|_| {
                anyhow!(
                    "{} must be a positive integer when set (got: {})",
                    BUCEPHALUS_LOCAL_WORKER_MAX_IN_FLIGHT_ENV,
                    raw
                )
            })?;
            if parsed == 0 {
                return Err(anyhow!(
                    "{} must be > 0 when set",
                    BUCEPHALUS_LOCAL_WORKER_MAX_IN_FLIGHT_ENV
                ));
            }
            Ok(Some(parsed))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(anyhow!(
            "failed reading {}: {}",
            BUCEPHALUS_LOCAL_WORKER_MAX_IN_FLIGHT_ENV,
            err
        )),
    }
}

pub(crate) fn parse_max_run_bytes_from_env() -> Result<Option<u64>> {
    match std::env::var(BUCEPHALUS_MAX_RUN_BYTES_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = trimmed.parse::<u64>().map_err(|_| {
                anyhow!(
                    "{} must be a positive integer when set (got: {})",
                    BUCEPHALUS_MAX_RUN_BYTES_ENV,
                    raw
                )
            })?;
            if parsed == 0 {
                return Err(anyhow!(
                    "{} must be > 0 when set",
                    BUCEPHALUS_MAX_RUN_BYTES_ENV
                ));
            }
            Ok(Some(parsed))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(anyhow!(
            "failed reading {}: {}",
            BUCEPHALUS_MAX_RUN_BYTES_ENV,
            err
        )),
    }
}

pub(crate) fn resolve_local_worker_max_in_flight(
    requested_max_in_flight: usize,
    configured_ceiling: Option<usize>,
) -> (usize, Option<String>) {
    let effective_max_in_flight = configured_ceiling
        .map(|ceiling| requested_max_in_flight.min(ceiling))
        .unwrap_or(requested_max_in_flight)
        .max(1);
    if effective_max_in_flight < requested_max_in_flight {
        let warning = format!(
            "local worker backend capacity ceiling applied: requested_max_in_flight={} effective_max_in_flight={} env_var={}",
            requested_max_in_flight,
            effective_max_in_flight,
            BUCEPHALUS_LOCAL_WORKER_MAX_IN_FLIGHT_ENV
        );
        return (effective_max_in_flight, Some(warning));
    }
    (effective_max_in_flight, None)
}

pub(crate) fn create_unique_run_dir(run_root: &Path) -> Result<(String, PathBuf)> {
    let runs_dir = run_root;
    ensure_dir(&runs_dir)?;
    static RUN_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

    for _ in 0..RUN_DIR_CREATE_MAX_ATTEMPTS {
        let now = Utc::now();
        let seq = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let run_id = format!(
            "run_{}_{:06}_{:06}",
            now.format("%Y%m%d_%H%M%S"),
            now.timestamp_subsec_micros(),
            seq % 1_000_000
        );
        let run_dir = runs_dir.join(&run_id);
        match fs::create_dir(&run_dir) {
            Ok(_) => return Ok((run_id, run_dir)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(anyhow!(
                    "failed to create run directory {}: {}",
                    run_dir.display(),
                    err
                ));
            }
        }
    }

    Err(anyhow!(
        "failed to allocate a unique run directory under {} after {} attempts",
        runs_dir.display(),
        RUN_DIR_CREATE_MAX_ATTEMPTS
    ))
}
