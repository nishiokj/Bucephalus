use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use flate2::read::GzDecoder;
use lab_core::{
    ensure_dir, sha256_file, BUCEPHALUS_CONTRACT_EVENTS_DIR, BUCEPHALUS_CONTRACT_IN_DIR,
    BUCEPHALUS_CONTRACT_OUT_DIR, BUCEPHALUS_CONTRACT_WORKSPACE_DIR,
    BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH, BUCEPHALUS_ENV_RESULT_PATH,
    BUCEPHALUS_ENV_TRAJECTORY_PATH, BUCEPHALUS_ENV_TRIAL_INPUT_PATH,
    BUCEPHALUS_EVENTS_DURABLE_PATH,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
#[cfg(unix)]
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Condvar, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use tar::{Archive, EntryType};

use crate::config::{load_json_file, normalize_path, parse_string_array_field};
use crate::experiment::runner::{
    agent_artifact_archive_flag, map_contract_path_to_host, ContractPathHostRoots, ContractPathMode,
};
use crate::experiment::runtime::{AgentRuntimeConfig, ResolvedSecretFileMount};
use crate::model::{
    ExecutorKind, GraderConfig, GradingStrategy, PreparedTrialIo, ResolvedMountReference,
    RuntimeOutputConfig, RuntimeTransportSourceConfig, BUCEPHALUS_ENV_AGENT_EXIT_STATUS,
    BUCEPHALUS_MAX_INLINE_CAPTURE_BYTES_ENV, MAPPED_GRADER_OUTPUT_FILENAME,
};
use crate::persistence::backend::open_trial_attempt_store;
use crate::persistence::rows::EventRow;
use crate::persistence::store::is_sqlite_busy_error;
use crate::persistence::writer::RunStoreWriter;
use crate::trial::artifacts::{
    artifact_type_from_trial_input_path, extract_candidate_artifact_record,
    load_agent_response_resilient,
};
use crate::trial::env::{
    build_exec_env, resolve_grader_command, resolve_grading_phase, resolve_runtime_agent_command,
    ResolvedGradingPhase,
};
use crate::trial::events::{
    load_event_rows, spawn_live_event_ingest, LiveEventIngestHandle, LiveEventIngestRequest,
};
use crate::trial::grade::{
    build_grading_sandbox_plan, build_hidden_asset_bindings, materialize_injected_grader_bundle,
    reveal_hidden_assets, stash_hidden_assets, validate_grading_contract,
};
use crate::trial::layout::{
    trial_agent_stderr_path, trial_agent_stdout_path, trial_grader_stderr_path,
    trial_grader_stdout_path, trial_patch_log_dir,
};
use crate::trial::prepare::TrialPaths;
use crate::trial::sidecar::{
    sidecar_env_for_stage, sidecar_plans_for_stage, trial_sidecar_plans, RuntimeSidecarPlan,
};
use crate::trial::spec::TaskMaterializationKind;
use crate::trial::state::{
    load_trial_attempt_container_ids, load_trial_attempt_state, new_trial_attempt_state,
    trial_attempt_state_exists, AgentPhaseRecord, ContainerCleanupRecord, ContractFileState,
    EphemeralNetworkState, EphemeralSandboxState, GradingPhaseRecord, GradingSandboxState,
    TaskSandboxPlan, TaskSandboxState, TrialAttemptState, TrialPhase,
};
use crate::util::{remove_path_if_exists, sanitize_for_fs};
use lab_schemas::compile_schema;

pub(crate) mod local_docker;
pub(crate) mod modal;

static RUN_STORE_WRITER: OnceLock<RwLock<Option<RunStoreWriter>>> = OnceLock::new();

fn run_store_writer_slot() -> &'static RwLock<Option<RunStoreWriter>> {
    RUN_STORE_WRITER.get_or_init(|| RwLock::new(None))
}

pub(crate) struct RunStoreWriterScope {
    previous: Option<RunStoreWriter>,
}

impl RunStoreWriterScope {
    pub(crate) fn install(writer: RunStoreWriter) -> Self {
        let mut slot = run_store_writer_slot()
            .write()
            .expect("run store writer scope lock poisoned");
        let previous = slot.replace(writer);
        Self { previous }
    }
}

impl Drop for RunStoreWriterScope {
    fn drop(&mut self) {
        let mut slot = run_store_writer_slot()
            .write()
            .expect("run store writer scope lock poisoned");
        *slot = self.previous.take();
    }
}

fn current_run_store_writer(run_dir: &Path, run_id: &str) -> Option<RunStoreWriter> {
    run_store_writer_slot()
        .read()
        .expect("run store writer scope lock poisoned")
        .as_ref()
        .filter(|writer| writer.run_id() == run_id && writer.run_dir() == run_dir)
        .cloned()
}

struct ActiveRuntimeLimiter {
    in_use: Mutex<usize>,
    available: Condvar,
}

pub(crate) struct ActiveRuntimePermit {
    limiter: &'static ActiveRuntimeLimiter,
    units: usize,
}

impl ActiveRuntimeLimiter {
    fn new() -> Self {
        Self {
            in_use: Mutex::new(0),
            available: Condvar::new(),
        }
    }

    fn acquire(
        &'static self,
        units: usize,
        limit: usize,
        unit_label: &str,
        env_name: &str,
    ) -> Result<ActiveRuntimePermit> {
        if units > limit {
            return Err(anyhow!(
                "trial requires {} active {} but {} limits this runner to {}",
                units,
                unit_label,
                env_name,
                limit
            ));
        }
        let mut in_use = self
            .in_use
            .lock()
            .expect("active runtime limiter lock poisoned");
        while *in_use + units > limit {
            in_use = self
                .available
                .wait(in_use)
                .expect("active runtime limiter lock poisoned");
        }
        *in_use += units;
        Ok(ActiveRuntimePermit {
            limiter: self,
            units,
        })
    }
}

impl Drop for ActiveRuntimePermit {
    fn drop(&mut self) {
        if self.units == 0 {
            return;
        }
        let mut in_use = self
            .limiter
            .in_use
            .lock()
            .expect("active runtime limiter lock poisoned");
        *in_use = in_use.saturating_sub(self.units);
        self.limiter.available.notify_all();
    }
}

fn active_runtime_limit(env_name: &str, default: usize) -> Result<usize> {
    let Ok(raw) = std::env::var(env_name) else {
        return Ok(default);
    };
    let value = raw
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow!("{} must be a positive integer", env_name))?;
    if value == 0 {
        return Err(anyhow!("{} must be a positive integer", env_name));
    }
    Ok(value)
}

#[derive(Clone)]
pub(crate) struct TrialRunRequest<'a> {
    pub(crate) package_root: &'a Path,
    pub(crate) runtime_experiment: &'a Value,
    pub(crate) runtime: &'a AgentRuntimeConfig,
    pub(crate) variant_args: &'a [String],
    pub(crate) runtime_env: &'a BTreeMap<String, String>,
    pub(crate) runtime_overrides_env: &'a BTreeMap<String, String>,
    pub(crate) trial_paths: &'a TrialPaths,
    pub(crate) dynamic_mounts: &'a [ResolvedMountReference],
    pub(crate) secret_file_mounts: &'a [ResolvedSecretFileMount],
    pub(crate) io_paths: &'a PreparedTrialIo,
    pub(crate) network_mode: &'a str,
    pub(crate) grader: Option<&'a GraderConfig>,
    pub(crate) grading_enabled: bool,
    pub(crate) run_id: &'a str,
    pub(crate) task_image: &'a str,
    pub(crate) task_workdir: &'a str,
    pub(crate) task_materialization_kind: TaskMaterializationKind,
    pub(crate) agent_artifact: Option<&'a Path>,
    pub(crate) agent_artifact_mount_path: Option<&'a str>,
    pub(crate) agent_artifact_read_only: bool,
}

pub(crate) struct TrialRuntimeOutcome {
    pub(crate) executor: ExecutorKind,
    pub(crate) agent_exit_status: String,
    pub(crate) trial_output: Value,
    pub(crate) result_present: bool,
    pub(crate) result_parse_error: Option<String>,
    pub(crate) stdout: Option<EvidenceBlobRef>,
    pub(crate) stderr: Option<EvidenceBlobRef>,
    pub(crate) events: Option<EvidenceBlobRef>,
    pub(crate) event_rows: Vec<EventRow>,
    pub(crate) trial_conclusion_row: Option<Value>,
    pub(crate) deferred_trial_conclusion_records: Vec<Value>,
    pub(crate) grade_error_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum EvidenceBlobRef {
    LocalPath(PathBuf),
    RemoteRef { uri: String },
}

fn extend_with_sidecar_env(
    env: &mut BTreeMap<String, String>,
    request: &TrialRunRequest<'_>,
    stage: &str,
) -> Result<()> {
    for (key, value) in sidecar_env_for_stage(request.runtime_experiment, stage)? {
        if env.insert(key.clone(), value).is_some() {
            return Err(anyhow!(
                "trial_runtime.{}.sidecars expose env '{}' conflicts with runtime env",
                stage,
                key
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn sidecar_env_for_stage_for_test(
    request: &TrialRunRequest<'_>,
    stage: &str,
) -> Result<BTreeMap<String, String>> {
    sidecar_env_for_stage(request.runtime_experiment, stage)
}

pub(crate) trait ExecutionBackend {
    fn executor_kind(&self) -> ExecutorKind;

    fn execute_attempt(
        &self,
        request: TrialRuntimeExecutionRequest<'_>,
    ) -> Result<TrialRuntimeOutcome>;

    fn cleanup_attempt_runtime(
        &self,
        request: TrialRuntimeCleanupRequest<'_>,
    ) -> Result<RuntimeCleanupOutcome>;

    fn cleanup_run_runtime(&self, run_id: &str) -> Result<RuntimeCleanupOutcome>;
}

pub(crate) struct TrialRuntimeExecutionRequest<'a> {
    pub(crate) trial_dir: &'a Path,
    pub(crate) schedule_idx: usize,
    pub(crate) attempt_no: usize,
    pub(crate) run_request: &'a TrialRunRequest<'a>,
    pub(crate) task_id: &'a str,
    pub(crate) variant_id: &'a str,
    pub(crate) repl_idx: usize,
    pub(crate) task_sandbox_plan: &'a TaskSandboxPlan,
}

pub(crate) struct TrialRuntimeCleanupRequest<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) trial_id: &'a str,
    pub(crate) trial_dir: &'a Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeCleanupOutcome {
    pub(crate) cleaned_workers: usize,
}

pub(crate) fn execution_backend(kind: ExecutorKind) -> Result<Box<dyn ExecutionBackend>> {
    match kind {
        ExecutorKind::LocalDocker => Ok(Box::new(local_docker::LocalDockerExecutionBackend::new())),
        ExecutorKind::Modal => Ok(Box::new(modal::ModalExecutionBackend::from_env()?)),
        ExecutorKind::Remote => Err(anyhow!(
            "executor '{}' is declared but no concrete backend is wired",
            kind.as_str()
        )),
    }
}

fn rfc3339_delta_ms(started_at: &str, ended_at: &str) -> Result<f64> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at)
        .with_context(|| format!("parse start timestamp {started_at}"))?;
    let ended = chrono::DateTime::parse_from_rfc3339(ended_at)
        .with_context(|| format!("parse end timestamp {ended_at}"))?;
    let micros = ended
        .signed_duration_since(started)
        .num_microseconds()
        .ok_or_else(|| anyhow!("timestamp delta overflow"))?;
    Ok(micros as f64 / 1000.0)
}

struct PerfSpanContext<'a> {
    request: &'a TrialRunRequest<'a>,
    trial_id: &'a str,
    schedule_idx: usize,
    attempt_no: usize,
}

fn record_timestamp_delta(
    context: &PerfSpanContext<'_>,
    stage: &'static str,
    start_label: &'static str,
    started_at: Option<&str>,
    end_label: &'static str,
    ended_at: Option<&str>,
    mut detail: serde_json::Map<String, Value>,
) -> Result<()> {
    let (Some(started_at), Some(ended_at)) = (started_at, ended_at) else {
        return Ok(());
    };
    let duration_ms = rfc3339_delta_ms(started_at, ended_at)?;
    detail.insert(start_label.to_string(), json!(started_at));
    detail.insert(end_label.to_string(), json!(ended_at));
    crate::perf::record(crate::perf::PerfRecord {
        run_dir: context.request.package_root,
        run_id: context.request.run_id,
        trial_id: Some(context.trial_id),
        schedule_idx: Some(context.schedule_idx),
        attempt: Some(context.attempt_no),
        sample_kind: "duration",
        stage,
        duration_ms: Some(duration_ms),
        detail: Value::Object(detail),
    })
}

fn read_file_tail_lossy(path: &Path, max_bytes: u64) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("open log tail source {}", path.display()))?;
    let len = file
        .metadata()
        .with_context(|| format!("stat log tail source {}", path.display()))?
        .len();
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes))
            .with_context(|| format!("seek log tail source {}", path.display()))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read log tail source {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn max_inline_capture_bytes() -> Result<Option<u64>> {
    match std::env::var(BUCEPHALUS_MAX_INLINE_CAPTURE_BYTES_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = trimmed.parse::<u64>().map_err(|_| {
                anyhow!(
                    "{} must be a positive integer when set (got: {})",
                    BUCEPHALUS_MAX_INLINE_CAPTURE_BYTES_ENV,
                    raw
                )
            })?;
            if parsed == 0 {
                return Err(anyhow!(
                    "{} must be > 0 when set",
                    BUCEPHALUS_MAX_INLINE_CAPTURE_BYTES_ENV
                ));
            }
            Ok(Some(parsed))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(anyhow!(
            "failed reading {}: {}",
            BUCEPHALUS_MAX_INLINE_CAPTURE_BYTES_ENV,
            err
        )),
    }
}

fn enforce_inline_capture_size(path: &Path, label: &str) -> Result<()> {
    let Some(max_bytes) = max_inline_capture_bytes()? else {
        return Ok(());
    };
    let len = fs::metadata(path)
        .with_context(|| format!("stat runtime output capture {}", path.display()))?
        .len();
    if len > max_bytes {
        return Err(anyhow!(
            "{} capture at {} is too large to inline: bytes={} max={} override_env_var={} use format=bytes or BUCEPHALUS_MAX_RUN_BYTES for large artifacts",
            label,
            path.display(),
            len,
            max_bytes,
            BUCEPHALUS_MAX_INLINE_CAPTURE_BYTES_ENV
        ));
    }
    Ok(())
}

struct AgentStageOutcome {
    agent_exit_status: String,
    trial_output: Value,
    result_present: bool,
    result_parse_error: Option<String>,
}

struct GradingStageOutcome {
    trial_conclusion_row: Option<Value>,
    deferred_trial_conclusion_records: Vec<Value>,
    grade_error_reason: Option<String>,
}

fn start_live_event_ingest(
    trial_dir: &Path,
    schedule_idx: usize,
    attempt_no: usize,
    request: &TrialRunRequest<'_>,
    task_id: &str,
    variant_id: &str,
    repl_idx: usize,
) -> Result<Option<LiveEventIngestHandle>> {
    let Some(sink) = request.runtime.event_sinks.first() else {
        return Ok(None);
    };
    if !sink.ingest {
        return Ok(None);
    }
    let trial_id = trial_id_from_dir(trial_dir)?;
    Ok(Some(spawn_live_event_ingest(LiveEventIngestRequest {
        run_dir: request.package_root.to_path_buf(),
        events_path: request.io_paths.events_host.clone(),
        run_id: request.run_id.to_string(),
        trial_id,
        schedule_idx,
        variant_id: variant_id.to_string(),
        task_id: task_id.to_string(),
        repl_idx,
        attempt: attempt_no,
    })))
}

fn stop_live_event_ingest(handle: Option<LiveEventIngestHandle>) -> Result<()> {
    if let Some(handle) = handle {
        handle.stop()?;
    }
    Ok(())
}

struct HostGraderConcurrencyState {
    active: usize,
    max: Option<usize>,
}

struct HostGraderConcurrencyLimiter {
    state: Mutex<HostGraderConcurrencyState>,
    available: Condvar,
}

struct HostGraderConcurrencyPermit {
    limiter: &'static HostGraderConcurrencyLimiter,
}

impl Drop for HostGraderConcurrencyPermit {
    fn drop(&mut self) {
        let mut state = self
            .limiter
            .state
            .lock()
            .expect("host grader concurrency limiter lock poisoned during release");
        state.active = state.active.saturating_sub(1);
        self.limiter.available.notify_one();
    }
}

fn host_grader_concurrency_limiter() -> &'static HostGraderConcurrencyLimiter {
    static LIMITER: OnceLock<HostGraderConcurrencyLimiter> = OnceLock::new();
    LIMITER.get_or_init(|| HostGraderConcurrencyLimiter {
        state: Mutex::new(HostGraderConcurrencyState {
            active: 0,
            max: None,
        }),
        available: Condvar::new(),
    })
}

pub(crate) fn configure_host_grader_max_concurrency(max_concurrency: Option<usize>) {
    let limiter = host_grader_concurrency_limiter();
    let mut state = limiter
        .state
        .lock()
        .expect("host grader concurrency limiter lock poisoned during configuration");
    state.max = max_concurrency.map(|max| max.max(1));
    limiter.available.notify_all();
}

fn acquire_host_grader_concurrency_permit() -> HostGraderConcurrencyPermit {
    let limiter = host_grader_concurrency_limiter();
    let mut state = limiter
        .state
        .lock()
        .expect("host grader concurrency limiter lock poisoned during acquire");
    while state.max.is_some_and(|max| state.active >= max) {
        state = limiter
            .available
            .wait(state)
            .expect("host grader concurrency limiter lock poisoned while waiting");
    }
    state.active += 1;
    HostGraderConcurrencyPermit { limiter }
}

const INJECTED_BUNDLE_SOURCE_MOUNT_PATH: &str = "/bucephalus/_materialize/injected_bundle_src";

fn grading_strategy_name(strategy: &GradingStrategy) -> &'static str {
    match strategy {
        GradingStrategy::None => "none",
        GradingStrategy::InTaskRuntime => "in_task_runtime",
        GradingStrategy::Injected => "injected",
        GradingStrategy::Separate => "separate",
        GradingStrategy::Host => "host",
    }
}

pub(crate) struct GraderRunOutcome {
    pub(crate) exit_code: Option<i32>,
    pub(crate) signal: Option<String>,
    pub(crate) timed_out: bool,
}

#[derive(Clone, Debug)]
struct CapturedTransportOutput {
    value: Value,
    host_path: Option<PathBuf>,
    container_path: Option<String>,
    format: Option<String>,
}

fn parse_agent_outputs(
    request: &TrialRunRequest<'_>,
) -> Result<BTreeMap<String, RuntimeOutputConfig>> {
    let value = request
        .runtime_experiment
        .pointer("/trial_runtime/agent/outputs")
        .cloned()
        .ok_or_else(|| anyhow!("/trial_runtime/agent/outputs is required"))?;
    serde_json::from_value(value)
        .map_err(|err| anyhow!("invalid /trial_runtime/agent/outputs: {}", err))
}

fn read_captured_file_value(host_path: &Path, format: &str) -> Result<Value> {
    match format {
        "json" => {
            enforce_inline_capture_size(host_path, "json runtime output")?;
            Ok(serde_json::from_slice(&fs::read(host_path)?)?)
        }
        "text" => {
            enforce_inline_capture_size(host_path, "text runtime output")?;
            Ok(json!(fs::read_to_string(host_path)?))
        }
        "bytes" => Ok(json!({
            "path": host_path.to_string_lossy(),
            "sha256": sha256_file(host_path)?,
            "bytes": host_path.metadata()?.len()
        })),
        other => Err(anyhow!("unsupported runtime output format '{}'", other)),
    }
}

fn select_transport_field(value: &Value, field: &str) -> Option<Value> {
    let trimmed = field.trim();
    if trimmed.is_empty() {
        return Some(value.clone());
    }
    if trimmed.starts_with('/') {
        return value.pointer(trimmed).cloned();
    }
    let mut current = value;
    for part in trimmed.split('.') {
        if part.is_empty() {
            return None;
        }
        current = current.get(part)?;
    }
    Some(current.clone())
}

fn select_transport_source(
    source: &RuntimeTransportSourceConfig,
    agent_outputs: &BTreeMap<String, CapturedTransportOutput>,
    task_payload: &Value,
) -> Result<Option<Value>> {
    if let Some(output) = source.output.as_deref() {
        let output_id = crate::trial::agent_output_id(output);
        let Some(captured) = agent_outputs.get(output_id) else {
            return Ok(None);
        };
        let value = if let Some(field) = source.field.as_deref() {
            let Some(value) = select_transport_field(&captured.value, field) else {
                return Ok(None);
            };
            value
        } else {
            captured.value.clone()
        };
        return Ok(Some(value));
    }
    if let Some(case_field) = source.case.as_deref().or(source.task.as_deref()) {
        return Ok(select_transport_field(task_payload, case_field));
    }
    if let Some(object) = source.object.as_ref() {
        let mut mapped = serde_json::Map::new();
        for (key, nested) in object {
            let value = select_transport_source(nested, agent_outputs, task_payload)?
                .ok_or_else(|| anyhow!("transport object field '{}' resolved to null", key))?;
            mapped.insert(key.clone(), value);
        }
        return Ok(Some(Value::Object(mapped)));
    }
    Ok(None)
}

fn transport_value_to_bytes(value: &Value, json_mode: bool) -> Result<Vec<u8>> {
    if json_mode {
        return Ok(serde_json::to_vec_pretty(value)?);
    }
    if let Some(text) = value.as_str() {
        Ok(text.as_bytes().to_vec())
    } else {
        Ok(serde_json::to_vec_pretty(value)?)
    }
}

fn write_host_transport_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn metric_source_output_id(metric: &crate::model::MetricDefinition) -> Option<&str> {
    metric
        .definition_json
        .pointer("/source/output")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn metric_source_pointer(metric: &crate::model::MetricDefinition) -> Option<&str> {
    metric
        .definition_json
        .pointer("/source/pointer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn transform_task_source_value(trial_input: &Value, source: &str) -> Option<Value> {
    if source.trim_start().starts_with('/') {
        return select_transport_field(trial_input, source);
    }
    trial_input
        .pointer("/case")
        .and_then(|case| select_transport_field(case, source))
}

fn metric_transform_test_ids(
    metric: &crate::model::MetricDefinition,
    transform: &Value,
    trial_input: &Value,
) -> Result<Vec<String>> {
    let Some(task_source) = transform
        .pointer("/test_ids/source/task")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };
    let Some(value) = transform_task_source_value(trial_input, task_source) else {
        return Err(anyhow!(
            "metric '{}' source.transform.test_ids.source.task resolved to null",
            metric.id
        ));
    };
    parse_string_array_field(
        Some(&value),
        &format!(
            "metric '{}' source.transform.test_ids.source.task",
            metric.id
        ),
    )
}

fn pytest_json_report_pass_rate(
    metric: &crate::model::MetricDefinition,
    report: &Value,
    transform: &Value,
    trial_input: &Value,
) -> Result<Value> {
    let wanted = metric_transform_test_ids(metric, transform, trial_input)?;
    if !wanted.is_empty() {
        let wanted = wanted
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let mut passed = 0usize;
        let mut total = 0usize;
        if let Some(tests) = report.get("tests").and_then(Value::as_array) {
            for test in tests {
                let Some(nodeid) = test.get("nodeid").and_then(Value::as_str) else {
                    continue;
                };
                if wanted.contains(nodeid) {
                    total += 1;
                    if test.get("outcome").and_then(Value::as_str) == Some("passed") {
                        passed += 1;
                    }
                }
            }
        }
        if total > 0 {
            return Ok(json!(passed as f64 / total as f64));
        }
    }

    let summary = report.get("summary").and_then(Value::as_object);
    let passed = summary
        .and_then(|summary| summary.get("passed"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let total = summary
        .and_then(|summary| summary.get("total"))
        .and_then(Value::as_f64)
        .unwrap_or_else(|| {
            report
                .get("tests")
                .and_then(Value::as_array)
                .map(|tests| tests.len() as f64)
                .unwrap_or(0.0)
        });
    if total <= 0.0 {
        return Ok(json!(0.0));
    }
    Ok(json!(passed / total))
}

fn apply_metric_transform(
    metric: &crate::model::MetricDefinition,
    value: &Value,
    trial_input: &Value,
) -> Result<Value> {
    let Some(transform) = metric.definition_json.pointer("/source/transform") else {
        return Ok(value.clone());
    };
    let transform_type = transform
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("metric '{}' source.transform.type is required", metric.id))?;
    match transform_type {
        "identity" => Ok(value.clone()),
        "pytest_json_report_pass_rate" => {
            pytest_json_report_pass_rate(metric, value, transform, trial_input)
        }
        other => Err(anyhow!(
            "metric '{}' source.transform.type '{}' is not supported",
            metric.id,
            other
        )),
    }
}

fn synthesize_grader_trial_conclusion(
    request: &TrialRunRequest<'_>,
    grader: &GraderConfig,
    grader_outputs: &BTreeMap<String, CapturedTransportOutput>,
    grader_run: &GraderRunOutcome,
) -> Result<Value> {
    let metrics = crate::config::parse_metric_definitions(request.runtime_experiment)?;
    let trial_input: Value =
        serde_json::from_slice(&fs::read(&request.io_paths.trial_input_host)?)?;
    let mut payload = serde_json::Map::new();
    let mut primary = None;
    for metric in metrics
        .iter()
        .filter(|metric| metric.source.source_type == "grader_output")
    {
        let output_id = metric_source_output_id(metric)
            .ok_or_else(|| anyhow!("metric '{}' source.output is required", metric.id))?;
        let captured = grader_outputs.get(output_id).ok_or_else(|| {
            anyhow!(
                "metric '{}' references missing grader output '{}'",
                metric.id,
                output_id
            )
        })?;
        let selected = if let Some(pointer) = metric_source_pointer(metric) {
            captured.value.pointer(pointer).cloned()
        } else {
            Some(captured.value.clone())
        };
        let Some(selected) = selected else {
            if metric.required {
                return Err(anyhow!("required metric '{}' resolved to null", metric.id));
            }
            continue;
        };
        let value = apply_metric_transform(metric, &selected, &trial_input)?;
        if value.is_null() && metric.required {
            return Err(anyhow!("required metric '{}' resolved to null", metric.id));
        }
        payload.insert(metric.id.clone(), value.clone());
        if metric.primary && primary.is_none() {
            primary = Some((metric.id.clone(), value));
        }
    }
    let primary_metric = primary.map(|(name, value)| json!({ "name": name, "value": value }));
    let reported_outcome = if grader_run.timed_out {
        "timeout"
    } else if grader_run.exit_code == Some(0) {
        "success"
    } else {
        "failure"
    };
    let strategy = match grader.strategy {
        GradingStrategy::None => "none",
        GradingStrategy::InTaskRuntime => "in_task_runtime",
        GradingStrategy::Injected => "injected",
        GradingStrategy::Separate => "separate",
        GradingStrategy::Host => "host",
    };
    let mut row = serde_json::Map::from_iter([
        ("schema_version".to_string(), json!("trial_conclusion_v1")),
        ("payload".to_string(), Value::Object(payload)),
        ("reported_outcome".to_string(), json!(reported_outcome)),
        (
            "grader".to_string(),
            json!({
                "name": "runtime_transport",
                "strategy": strategy
            }),
        ),
    ]);
    if let Some(primary_metric) = primary_metric {
        row.insert("primary_metric".to_string(), primary_metric);
    }
    Ok(Value::Object(row))
}

fn write_transport_envelope(
    request: &TrialRunRequest<'_>,
    agent_outputs: &BTreeMap<String, CapturedTransportOutput>,
    grader_outputs: &BTreeMap<String, CapturedTransportOutput>,
) -> Result<()> {
    let output_to_json = |output: &CapturedTransportOutput| {
        json!({
            "value": output.value,
            "host_path": output.host_path.as_ref().map(|path| path.to_string_lossy().to_string()),
            "container_path": output.container_path,
            "format": output.format
        })
    };
    let envelope = json!({
        "schema_version": "runtime_transport_envelope_v1",
        "agent": {
            "outputs": agent_outputs
                .iter()
                .map(|(id, output)| (id.clone(), output_to_json(output)))
                .collect::<serde_json::Map<String, Value>>()
        },
        "grader": {
            "outputs": grader_outputs
                .iter()
                .map(|(id, output)| (id.clone(), output_to_json(output)))
                .collect::<serde_json::Map<String, Value>>()
        }
    });
    let path = request
        .trial_paths
        .out
        .join("runtime_transport_envelope.json");
    fs::write(path, serde_json::to_vec_pretty(&envelope)?)?;
    Ok(())
}

fn parse_transport_output(value: &Value) -> Result<CapturedTransportOutput> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("runtime transport output must be an object"))?;
    Ok(CapturedTransportOutput {
        value: object
            .get("value")
            .cloned()
            .ok_or_else(|| anyhow!("runtime transport output missing /value"))?,
        host_path: object
            .get("host_path")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        container_path: object
            .get("container_path")
            .and_then(Value::as_str)
            .map(str::to_string),
        format: object
            .get("format")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn parse_transport_output_map(
    value: Option<&Value>,
) -> Result<BTreeMap<String, CapturedTransportOutput>> {
    let mut outputs = BTreeMap::new();
    let Some(object) = value.and_then(Value::as_object) else {
        return Ok(outputs);
    };
    for (id, output) in object {
        outputs.insert(id.clone(), parse_transport_output(output)?);
    }
    Ok(outputs)
}

fn read_transport_envelope(
    path: &Path,
) -> Result<(
    BTreeMap<String, CapturedTransportOutput>,
    BTreeMap<String, CapturedTransportOutput>,
)> {
    let value: Value = serde_json::from_slice(&fs::read(path).with_context(|| {
        format!(
            "failed to read runtime transport envelope {}",
            path.display()
        )
    })?)?;
    Ok((
        parse_transport_output_map(value.pointer("/agent/outputs"))?,
        parse_transport_output_map(value.pointer("/grader/outputs"))?,
    ))
}

pub(crate) fn run_host_grader(
    request: &TrialRunRequest<'_>,
    resolved: &ResolvedGradingPhase,
    agent_exit_status: &str,
    transport_env: &BTreeMap<String, String>,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<GraderRunOutcome> {
    if resolved.command.is_empty() {
        return Err(anyhow!("host grader command is empty"));
    }
    let _permit = acquire_host_grader_concurrency_permit();
    let mut command = Command::new(&resolved.command[0]);
    command.args(&resolved.command[1..]);
    command.current_dir(&resolved.workdir);
    let mut env = build_exec_env(
        request,
        &resolved.workdir,
        Some((BUCEPHALUS_ENV_AGENT_EXIT_STATUS, agent_exit_status)),
        false,
    );
    env.insert("WORKSPACE".to_string(), request.task_workdir.to_string());
    env.insert(
        BUCEPHALUS_ENV_RESULT_PATH.to_string(),
        request.io_paths.result_host.to_string_lossy().to_string(),
    );
    env.insert(
        BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH.to_string(),
        request
            .trial_paths
            .out
            .join(MAPPED_GRADER_OUTPUT_FILENAME)
            .to_string_lossy()
            .to_string(),
    );
    env.insert(
        "BUCEPHALUS_CONTRACT_IN_HOST".to_string(),
        request.trial_paths.in_dir.to_string_lossy().to_string(),
    );
    env.insert(
        "BUCEPHALUS_CONTRACT_OUT_HOST".to_string(),
        request.trial_paths.out.to_string_lossy().to_string(),
    );
    env.insert(
        "BUCEPHALUS_TASK_WORKDIR".to_string(),
        request.task_workdir.to_string(),
    );
    env.extend(transport_env.clone());
    command.envs(env);
    let output = command.output()?;
    if let Some(parent) = stdout_path.parent() {
        ensure_dir(parent)?;
    }
    if let Some(parent) = stderr_path.parent() {
        ensure_dir(parent)?;
    }
    fs::write(stdout_path, &output.stdout)?;
    fs::write(stderr_path, &output.stderr)?;
    Ok(GraderRunOutcome {
        exit_code: output.status.code(),
        signal: signal_from_status(output.status),
        timed_out: false,
    })
}

#[cfg(unix)]
fn signal_from_status(status: ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|signal| signal.to_string())
}

#[cfg(not(unix))]
fn signal_from_status(_status: ExitStatus) -> Option<String> {
    None
}

pub(crate) fn exit_status_label(exit_code: Option<i32>) -> String {
    exit_code.map_or_else(|| "signal".to_string(), |value| value.to_string())
}

fn trial_id_from_dir(trial_dir: &Path) -> Result<String> {
    trial_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("trial directory has no trial id: {}", trial_dir.display()))
}

pub(crate) fn persist_attempt_state(
    run_dir: &Path,
    run_id: &str,
    trial_dir: &Path,
    state: &TrialAttemptState,
) -> Result<()> {
    let writer = current_run_store_writer(run_dir, run_id);
    persist_attempt_state_with_writer(run_dir, run_id, trial_dir, state, writer.as_ref())
}

pub(crate) fn persist_attempt_state_with_writer(
    run_dir: &Path,
    run_id: &str,
    trial_dir: &Path,
    state: &TrialAttemptState,
    writer: Option<&RunStoreWriter>,
) -> Result<()> {
    let trial_id = trial_id_from_dir(trial_dir)?;
    let file_result = crate::trial::state::write_trial_attempt_state(trial_dir, state)
        .with_context(|| format!("persist trial runtime state file {}", trial_dir.display()));
    let db_result = if let Some(writer) = writer {
        writer.upsert_trial_attempt_state(&trial_id, state)
    } else {
        open_trial_attempt_store(run_dir)
            .and_then(|mut store| store.upsert_trial_attempt_state(run_id, &trial_id, state))
    }
    .with_context(|| {
        format!(
            "persist trial runtime state in runtime store for run {} trial {}",
            run_id, trial_id
        )
    });
    match (file_result, db_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(file_err), Ok(())) => Err(file_err),
        (Ok(()), Err(db_err)) => Err(db_err),
        (Err(file_err), Err(db_err)) => Err(db_err.context(format!(
            "also failed to persist trial runtime state file: {file_err}"
        ))),
    }
}

fn set_attempt_phase(
    run_dir: &Path,
    run_id: &str,
    trial_dir: &Path,
    state: &mut TrialAttemptState,
    phase: TrialPhase,
) -> Result<()> {
    state.phase = phase;
    if state.phase != TrialPhase::Paused {
        state.paused_from_phase = None;
    }
    persist_attempt_state(run_dir, run_id, trial_dir, state)
}

fn reconcile_attempt_as_abandoned(
    run_dir: &Path,
    run_id: &str,
    trial_dir: &Path,
    state: &mut TrialAttemptState,
) -> Result<()> {
    if !matches!(
        state.phase,
        TrialPhase::Committed | TrialPhase::Paused | TrialPhase::Killed
    ) {
        state.phase = TrialPhase::Abandoned;
        state.paused_from_phase = None;
        persist_attempt_state(run_dir, run_id, trial_dir, state)?;
    }
    Ok(())
}

fn finalize_trial_runtime(
    trial_dir: &Path,
    run_dir: &Path,
    run_id: &str,
    attempt_state: &mut TrialAttemptState,
    agent_outcome: AgentStageOutcome,
    grading_outcome: GradingStageOutcome,
) -> Result<TrialRuntimeOutcome> {
    set_attempt_phase(
        run_dir,
        run_id,
        trial_dir,
        attempt_state,
        TrialPhase::CommitPending,
    )?;
    Ok(TrialRuntimeOutcome {
        executor: ExecutorKind::LocalDocker,
        agent_exit_status: agent_outcome.agent_exit_status,
        trial_output: agent_outcome.trial_output,
        result_present: agent_outcome.result_present,
        result_parse_error: agent_outcome.result_parse_error,
        stdout: None,
        stderr: None,
        events: None,
        event_rows: Vec::new(),
        trial_conclusion_row: grading_outcome.trial_conclusion_row,
        deferred_trial_conclusion_records: grading_outcome.deferred_trial_conclusion_records,
        grade_error_reason: grading_outcome.grade_error_reason,
    })
}

fn local_blob_if_present(path: PathBuf) -> Option<EvidenceBlobRef> {
    if path.exists() {
        Some(EvidenceBlobRef::LocalPath(path))
    } else {
        None
    }
}

fn attach_local_runtime_evidence(
    mut outcome: TrialRuntimeOutcome,
    request: &TrialRunRequest<'_>,
    trial_dir: &Path,
    schedule_idx: usize,
    task_id: &str,
    variant_id: &str,
    repl_idx: usize,
) -> Result<TrialRuntimeOutcome> {
    outcome.executor = ExecutorKind::LocalDocker;
    outcome.stdout = local_blob_if_present(trial_agent_stdout_path(trial_dir));
    outcome.stderr = local_blob_if_present(trial_agent_stderr_path(trial_dir));

    let event_sink = request.runtime.event_sinks.first();
    let retain_raw_events = event_sink.map(|sink| sink.persist).unwrap_or(false);
    let ingest_events = event_sink.map(|sink| sink.ingest).unwrap_or(false);
    if retain_raw_events {
        outcome.events = local_blob_if_present(request.io_paths.events_host.clone());
    }
    if ingest_events && request.io_paths.events_host.exists() {
        outcome.event_rows = load_event_rows(
            &request.io_paths.events_host,
            request.run_id,
            &trial_id_from_dir(trial_dir)?,
            schedule_idx,
            variant_id,
            task_id,
            repl_idx,
        )?;
    }
    Ok(outcome)
}

fn remote_blob_if_present(path: &Path, uri: String) -> Option<EvidenceBlobRef> {
    if path.exists() {
        Some(EvidenceBlobRef::RemoteRef { uri })
    } else {
        None
    }
}

fn execute_host_agent_runtime(
    trial_dir: &Path,
    schedule_idx: usize,
    attempt_no: usize,
    request: &TrialRunRequest<'_>,
    task_id: &str,
    variant_id: &str,
    repl_idx: usize,
) -> Result<TrialRuntimeOutcome> {
    let runtime_started_at = Instant::now();
    let mut attempt_state = new_trial_attempt_state(
        (
            trial_dir,
            &request.trial_paths.in_dir,
            &request.trial_paths.out,
        ),
        schedule_idx,
        attempt_no,
        (task_id, variant_id, repl_idx),
    );
    persist_attempt_state(
        request.package_root,
        request.run_id,
        trial_dir,
        &attempt_state,
    )?;
    set_attempt_phase(
        request.package_root,
        request.run_id,
        trial_dir,
        &mut attempt_state,
        TrialPhase::AgentRunning,
    )?;

    let command = resolve_runtime_agent_command(request)?;
    if command.is_empty() {
        return Err(anyhow!("trial_runtime.agent.command must not be empty"));
    }
    let workspace = request.trial_paths.workspace.to_string_lossy().to_string();
    let mut env = build_exec_env(request, &workspace, None, false);
    env.insert(
        BUCEPHALUS_ENV_TRIAL_INPUT_PATH.to_string(),
        request
            .io_paths
            .trial_input_host
            .to_string_lossy()
            .to_string(),
    );
    env.insert(
        BUCEPHALUS_ENV_RESULT_PATH.to_string(),
        request.io_paths.result_host.to_string_lossy().to_string(),
    );
    env.insert(
        BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH.to_string(),
        request
            .trial_paths
            .out
            .join(MAPPED_GRADER_OUTPUT_FILENAME)
            .to_string_lossy()
            .to_string(),
    );
    if !request.runtime.event_sinks.is_empty() {
        env.insert(
            BUCEPHALUS_ENV_TRAJECTORY_PATH.to_string(),
            request
                .trial_paths
                .runtime
                .trajectory
                .to_string_lossy()
                .to_string(),
        );
    }

    let started_at = Utc::now().to_rfc3339();
    let agent_run_started_at = Instant::now();
    let perf_scope = crate::perf::PerfScope::new(
        request.package_root,
        request.run_id,
        trial_dir.file_name().and_then(|name| name.to_str()),
        Some(schedule_idx),
        Some(attempt_no),
    );
    let output = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(&request.trial_paths.workspace)
        .env_clear()
        .envs(&env)
        .output()?;
    crate::perf::record_duration(
        perf_scope,
        "host_agent_command",
        agent_run_started_at,
        json!({ "exit_code": output.status.code() }),
    )?;
    let ended_at = Utc::now().to_rfc3339();
    if let Some(parent) = trial_agent_stdout_path(trial_dir).parent() {
        ensure_dir(parent)?;
    }
    fs::write(trial_agent_stdout_path(trial_dir), &output.stdout)?;
    fs::write(trial_agent_stderr_path(trial_dir), &output.stderr)?;

    let agent_response = load_agent_response_resilient(&request.io_paths.result_host)?;
    let trial_output = agent_response.response;
    let result_present = agent_response.result_present;
    let result_parse_error = agent_response.parse_error;
    let result_state =
        classify_contract_file_state(&request.io_paths.result_host, result_parse_error.as_deref())?;
    attempt_state.agent_phase = Some(AgentPhaseRecord {
        started_at,
        ended_at,
        exit_code: output.status.code(),
        signal: None,
        timed_out: false,
        result_state,
        stdout_path: trial_agent_stdout_path(trial_dir)
            .to_string_lossy()
            .to_string(),
        stderr_path: trial_agent_stderr_path(trial_dir)
            .to_string_lossy()
            .to_string(),
    });
    attempt_state.candidate_artifact = Some(extract_candidate_artifact_record(
        &trial_output,
        result_present,
        artifact_type_from_trial_input_path(&request.io_paths.trial_input_host)?,
    ));
    set_attempt_phase(
        request.package_root,
        request.run_id,
        trial_dir,
        &mut attempt_state,
        TrialPhase::AgentFinished,
    )?;

    let outcome = finalize_trial_runtime(
        trial_dir,
        request.package_root,
        request.run_id,
        &mut attempt_state,
        AgentStageOutcome {
            agent_exit_status: exit_status_label(output.status.code()),
            trial_output,
            result_present,
            result_parse_error,
        },
        GradingStageOutcome {
            trial_conclusion_row: None,
            deferred_trial_conclusion_records: Vec::new(),
            grade_error_reason: None,
        },
    )?;
    crate::perf::record_duration(
        perf_scope,
        "trial_runtime_total",
        runtime_started_at,
        json!({ "agent_site": "host" }),
    )?;
    Ok(outcome)
}

pub(crate) fn classify_contract_file_state(
    path: &Path,
    validation_error: Option<&str>,
) -> Result<ContractFileState> {
    if !path.exists() {
        return Ok(ContractFileState::Missing);
    }
    let state = if fs::metadata(path)
        .with_context(|| format!("stat contract file {}", path.display()))?
        .len()
        == 0
    {
        ContractFileState::Missing
    } else if validation_error.is_some() {
        ContractFileState::PresentInvalid
    } else {
        ContractFileState::Valid
    };
    Ok(state)
}

fn validate_json_schema(schema_name: &str, path: &Path) -> Result<Value> {
    if !path.exists() {
        return Err(anyhow!("{} missing: {}", schema_name, path.display()));
    }
    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let schema = compile_schema(schema_name)?;
    if let Err(errors) = schema.validate(&value) {
        let msgs = errors
            .map(|err| err.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(anyhow!("schema validation failed: {}", msgs));
    }
    Ok(value)
}

pub(crate) fn validate_container_workspace_path(path: &str) -> Result<()> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(anyhow!("mount_path must be absolute"));
    }
    for component in p.components() {
        if matches!(component, Component::ParentDir) {
            return Err(anyhow!("mount_path cannot contain '..'"));
        }
    }
    let allowed_roots = [
        BUCEPHALUS_CONTRACT_WORKSPACE_DIR,
        "/workspace/task",
        "/testbed",
    ];
    if !allowed_roots
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{}/", root)))
    {
        return Err(anyhow!(
            "mount_path must be under {}",
            BUCEPHALUS_CONTRACT_WORKSPACE_DIR
        ));
    }
    Ok(())
}

pub(crate) fn resolve_task_sandbox_image(request: &TrialRunRequest<'_>) -> Result<String> {
    let image = request.task_image.trim();
    if image.is_empty() {
        return Err(anyhow!("task image is required for task sandbox"));
    }
    Ok(image.to_string())
}

pub(crate) fn resolve_container_workspace<'a>(request: &'a TrialRunRequest<'_>) -> Result<&'a str> {
    let workdir = request.task_workdir.trim();
    if workdir.is_empty() {
        return Err(anyhow!("task workdir is required for task sandbox"));
    }
    Ok(workdir)
}

pub(crate) fn agent_artifact_cache_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn cleanup_agent_artifact_staging_dirs(
    cache_root: &Path,
    digest_path_component: &str,
) -> Result<()> {
    let prefix = format!("{}.tmp.", digest_path_component);
    let entries = match fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(anyhow!(
                "failed to inspect agent artifact cache {}: {}",
                cache_root.display(),
                err
            ))
        }
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|err| {
            anyhow!(
                "failed to inspect stale agent artifact staging path {}: {}",
                path.display(),
                err
            )
        })?;
        if metadata.is_dir() {
            fs::remove_dir_all(&path).map_err(|err| {
                anyhow!(
                    "failed to remove stale agent artifact staging directory {}: {}",
                    path.display(),
                    err
                )
            })?;
        } else {
            fs::remove_file(&path).map_err(|err| {
                anyhow!(
                    "failed to remove stale agent artifact staging file {}: {}",
                    path.display(),
                    err
                )
            })?;
        }
    }
    Ok(())
}

fn validate_archive_relative_path(path: &Path, field_name: &str) -> Result<()> {
    let mut saw_normal = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => saw_normal = true,
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(anyhow!(
                    "trial_runtime.agent.artifact archive {} must not contain '..': {}",
                    field_name,
                    path.display()
                ))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!(
                    "trial_runtime.agent.artifact archive {} must be relative: {}",
                    field_name,
                    path.display()
                ))
            }
        }
    }
    if !saw_normal {
        return Err(anyhow!(
            "trial_runtime.agent.artifact archive {} must not be empty",
            field_name
        ));
    }
    Ok(())
}

fn archive_entry_is_root(path: &Path) -> bool {
    path.as_os_str().is_empty()
        || path
            .components()
            .all(|component| matches!(component, Component::CurDir))
}

fn validate_archive_link_target(target: &Path, entry_path: &Path) -> Result<()> {
    if target.is_absolute() {
        return Err(anyhow!(
            "trial_runtime.agent.artifact archive symlink target must be relative: {} (entry: {})",
            target.display(),
            entry_path.display()
        ));
    }
    let entry_parent = entry_path.parent().ok_or_else(|| {
        anyhow!(
            "trial_runtime.agent.artifact archive entry has no parent path: {}",
            entry_path.display()
        )
    })?;
    let resolved = normalize_path(&entry_parent.join(target));
    validate_archive_relative_path(&resolved, "symlink target")
        .map_err(|err| anyhow!("{} (entry: {})", err, entry_path.display()))
}

fn unpack_agent_artifact_archive_reader<R: Read>(
    reader: R,
    destination: &Path,
    artifact_path: &Path,
) -> Result<()> {
    let mut archive = Archive::new(reader);
    for entry in archive.entries().with_context(|| {
        format!(
            "failed to read trial_runtime.agent.artifact archive {}",
            artifact_path.display()
        )
    })? {
        let mut entry = entry?;
        let entry_type = entry.header().entry_type();
        if matches!(
            entry_type,
            EntryType::Link | EntryType::GNULongLink | EntryType::GNULongName
        ) {
            return Err(anyhow!(
                "trial_runtime.agent.artifact archive contains unsupported link entry: {}",
                entry.path()?.display()
            ));
        }
        let entry_path = entry.path()?.into_owned();
        if archive_entry_is_root(&entry_path) {
            continue;
        }
        validate_archive_relative_path(&entry_path, "entry path")?;
        if entry_type == EntryType::Symlink {
            let target = entry.link_name()?.ok_or_else(|| {
                anyhow!(
                    "trial_runtime.agent.artifact archive symlink missing target: {}",
                    entry_path.display()
                )
            })?;
            validate_archive_link_target(target.as_ref(), &entry_path)?;
        }
        entry.unpack_in(destination).with_context(|| {
            format!(
                "failed to unpack trial_runtime.agent.artifact entry {} from {}",
                entry_path.display(),
                artifact_path.display()
            )
        })?;
    }
    Ok(())
}

fn validate_agent_artifact_archive_reader<R: Read>(reader: R, artifact_path: &Path) -> Result<()> {
    let mut archive = Archive::new(reader);
    for entry in archive.entries().with_context(|| {
        format!(
            "failed to read trial_runtime artifact archive {}",
            artifact_path.display()
        )
    })? {
        let entry = entry?;
        let entry_type = entry.header().entry_type();
        if matches!(
            entry_type,
            EntryType::Link | EntryType::GNULongLink | EntryType::GNULongName
        ) {
            return Err(anyhow!(
                "trial_runtime artifact archive contains unsupported link entry: {}",
                entry.path()?.display()
            ));
        }
        let entry_path = entry.path()?.into_owned();
        if archive_entry_is_root(&entry_path) {
            continue;
        }
        validate_archive_relative_path(&entry_path, "entry path")?;
        if entry_type == EntryType::Symlink {
            let target = entry.link_name()?.ok_or_else(|| {
                anyhow!(
                    "trial_runtime artifact archive symlink missing target: {}",
                    entry_path.display()
                )
            })?;
            validate_archive_link_target(target.as_ref(), &entry_path)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_agent_artifact_archive(artifact_path: &Path) -> Result<()> {
    let Some(gzipped) = agent_artifact_archive_gzip_flag(artifact_path) else {
        return Ok(());
    };
    let file = fs::File::open(artifact_path)?;
    if gzipped {
        validate_agent_artifact_archive_reader(GzDecoder::new(file), artifact_path)
    } else {
        validate_agent_artifact_archive_reader(file, artifact_path)
    }
}

fn unpack_agent_artifact_archive(
    artifact_path: &Path,
    staging_dir: &Path,
    gzipped: bool,
) -> Result<()> {
    validate_agent_artifact_archive(artifact_path)?;
    let file = fs::File::open(artifact_path)?;
    if gzipped {
        unpack_agent_artifact_archive_reader(GzDecoder::new(file), staging_dir, artifact_path)
    } else {
        unpack_agent_artifact_archive_reader(file, staging_dir, artifact_path)
    }
}

pub(crate) fn resolve_agent_artifact_mount_dir(artifact: &Path) -> Result<PathBuf> {
    if artifact.is_dir() {
        return fs::canonicalize(artifact).with_context(|| {
            format!(
                "failed to canonicalize trial_runtime.agent.artifact directory {}",
                artifact.display()
            )
        });
    }
    if !artifact.exists() {
        return Err(anyhow!(
            "trial_runtime.agent.artifact not found: {}",
            artifact.display()
        ));
    }
    if !artifact.is_file() {
        return Err(anyhow!(
            "trial_runtime.agent.artifact must be a file or directory: {}",
            artifact.display()
        ));
    }
    let artifact_path = fs::canonicalize(artifact).with_context(|| {
        format!(
            "failed to canonicalize trial_runtime.agent.artifact archive {}",
            artifact.display()
        )
    })?;
    let Some(gzipped) = agent_artifact_archive_gzip_flag(&artifact_path) else {
        return Err(anyhow!(
            "trial_runtime.agent.artifact '{}' must be a directory or .tar/.tar.gz archive",
            artifact_path.display()
        ));
    };

    let digest = sha256_file(&artifact_path)?;
    let digest_path_component = digest.replace(':', "_");
    let cache_root = artifact_path
        .parent()
        .with_context(|| {
            format!(
                "trial_runtime.agent.artifact archive has no parent directory: {}",
                artifact_path.display()
            )
        })?
        .join(".bucephalus_artifact_cache");
    ensure_dir(&cache_root)?;
    let unpacked_dir = cache_root.join(&digest_path_component);
    let ready_marker = unpacked_dir.join(".bucephalus_ready");
    if ready_marker.exists() {
        return Ok(unpacked_dir);
    }

    let _guard = agent_artifact_cache_lock()
        .lock()
        .map_err(|_| anyhow!("agent artifact cache lock poisoned"))?;
    if ready_marker.exists() {
        return Ok(unpacked_dir);
    }
    cleanup_agent_artifact_staging_dirs(&cache_root, &digest_path_component)?;

    if unpacked_dir.exists() {
        fs::remove_dir_all(&unpacked_dir)?;
    }
    let staging_dir = cache_root.join(format!(
        "{}.tmp.{}.{}",
        digest_path_component,
        std::process::id(),
        Utc::now().timestamp_micros()
    ));
    if staging_dir.exists() {
        remove_path_if_exists(&staging_dir).with_context(|| {
            format!(
                "failed to remove stale agent artifact staging directory {}",
                staging_dir.display()
            )
        })?;
    }
    ensure_dir(&staging_dir)?;
    if let Err(err) = unpack_agent_artifact_archive(&artifact_path, &staging_dir, gzipped) {
        remove_path_if_exists(&staging_dir).with_context(|| {
            format!(
                "failed to remove incomplete agent artifact staging directory {}",
                staging_dir.display()
            )
        })?;
        return Err(anyhow!(
            "failed to unpack trial_runtime.agent.artifact {}: {}",
            artifact_path.display(),
            err,
        ));
    }
    if let Err(err) = fs::rename(&staging_dir, &unpacked_dir) {
        remove_path_if_exists(&staging_dir).with_context(|| {
            format!(
                "failed to remove unfinalized agent artifact staging directory {}",
                staging_dir.display()
            )
        })?;
        return Err(anyhow!(
            "failed to finalize unpacked trial_runtime.agent.artifact {} into {}: {}",
            artifact_path.display(),
            unpacked_dir.display(),
            err
        ));
    }
    fs::write(&ready_marker, digest.as_bytes())?;
    Ok(unpacked_dir)
}

fn agent_artifact_archive_gzip_flag(path: &Path) -> Option<bool> {
    let name = path.file_name().and_then(|value| value.to_str())?;
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(true)
    } else if name.ends_with(".tar") {
        Some(false)
    } else {
        None
    }
}

pub(crate) fn map_container_path_to_host(path: &str, paths: &TrialPaths) -> Result<PathBuf> {
    map_contract_path_to_host(
        path,
        &ContractPathHostRoots::from_trial_paths(paths),
        ContractPathMode::ContainerMount,
    )
}

#[cfg(test)]
mod tests {
    use super::apply_metric_transform;
    use crate::config::parse_metric_definitions;
    use serde_json::{json, Value};

    fn metric_with_test_ids_source(source: &str) -> crate::model::MetricDefinition {
        let spec = json!({
            "metrics": [
                {
                    "id": "pass_rate",
                    "source": {
                        "type": "grader_output",
                        "output": "pytest_report",
                        "pointer": "/",
                        "transform": {
                            "type": "pytest_json_report_pass_rate",
                            "test_ids": {
                                "source": { "task": source }
                            }
                        }
                    },
                    "required": true,
                    "primary": true
                }
            ]
        });
        parse_metric_definitions(&spec)
            .expect("metric definition")
            .into_iter()
            .next()
            .expect("metric")
    }

    fn pytest_report() -> Value {
        json!({
            "tests": [
                { "nodeid": "test_passes", "outcome": "passed" },
                { "nodeid": "test_fails", "outcome": "failed" }
            ],
            "summary": { "passed": 1, "total": 2 }
        })
    }

    #[test]
    fn metric_transform_task_source_reads_case_payload() {
        let metric = metric_with_test_ids_source("commit0.test_ids");
        let trial_input = json!({
            "case": {
                "commit0": {
                    "test_ids": ["test_passes"]
                }
            }
        });

        let value =
            apply_metric_transform(&metric, &pytest_report(), &trial_input).expect("transform");

        assert_eq!(value, json!(1.0));
    }

    #[test]
    fn metric_transform_task_source_does_not_fallback_to_trial_input_root() {
        let metric = metric_with_test_ids_source("ids.test_ids");
        let trial_input = json!({
            "case": {},
            "ids": {
                "test_ids": ["test_fails"]
            }
        });

        let err = apply_metric_transform(&metric, &pytest_report(), &trial_input)
            .expect_err("relative source.task should not read trial_input root");

        assert!(
            err.to_string()
                .contains("source.transform.test_ids.source.task resolved to null"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn metric_transform_task_source_absolute_pointer_reads_trial_input_root() {
        let metric = metric_with_test_ids_source("/ids/test_ids");
        let trial_input = json!({
            "case": {},
            "ids": {
                "test_ids": ["test_fails"]
            }
        });

        let value =
            apply_metric_transform(&metric, &pytest_report(), &trial_input).expect("transform");

        assert_eq!(value, json!(0.0));
    }
}
