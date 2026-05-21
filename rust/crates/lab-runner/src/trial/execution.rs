use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use flate2::read::GzDecoder;
use lab_core::{
    canonical_json_digest, ensure_dir, sha256_file, AGENTLAB_CONTRACT_IN_DIR,
    AGENTLAB_CONTRACT_OUT_DIR, AGENTLAB_CONTRACT_WORKSPACE_DIR,
    AGENTLAB_ENV_MAPPED_GRADER_OUTPUT_PATH, AGENTLAB_ENV_RESULT_PATH, AGENTLAB_ENV_TRAJECTORY_PATH,
    AGENTLAB_ENV_TRIAL_INPUT_PATH,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Condvar, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use tar::{Archive, EntryType};

use crate::backend::docker::{
    ContainerHandle, ContainerMount, ContainerSpec, DockerRuntime, ExecSpec, NetworkHandle,
};
use crate::config::{load_json_file, normalize_path};
use crate::experiment::runner::{
    agent_artifact_archive_flag, map_contract_path_to_host, ContractPathHostRoots, ContractPathMode,
};
use crate::experiment::runtime::{AgentRuntimeConfig, ResolvedSecretFileMount};
use crate::model::{
    BenchmarkGraderConfig, ExecutorKind, GradingStrategy, PreparedTrialIo, ResolvedMountReference,
    RuntimeOutputConfig, RuntimeTransportSourceConfig, AGENTLAB_ENV_AGENT_EXIT_STATUS,
    AGENTLAB_MAX_INLINE_CAPTURE_BYTES_ENV, MAPPED_GRADER_OUTPUT_FILENAME,
};
use crate::persistence::rows::EventRow;
use crate::persistence::store::{is_sqlite_busy_error, SqliteRunStore};
use crate::persistence::writer::RunStoreWriter;
use crate::trial::artifacts::{
    artifact_type_from_trial_input_path, extract_candidate_artifact_record,
    load_agent_response_resilient,
};
use crate::trial::env::{
    build_exec_env, resolve_benchmark_grader_command, resolve_grading_phase,
    resolve_runtime_agent_command, ResolvedGradingPhase,
};
use crate::trial::events::{
    load_event_rows, spawn_live_event_ingest, LiveEventIngestHandle, LiveEventIngestRequest,
};
use crate::trial::grade::{
    build_grading_sandbox_plan, build_hidden_asset_bindings, materialize_injected_grader_bundle,
    reveal_hidden_assets, stash_hidden_assets, validate_benchmark_grading_contract,
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
use crate::util::sanitize_for_fs;
use lab_schemas::compile_schema;

static RUN_STORE_WRITER: OnceLock<RwLock<Option<RunStoreWriter>>> = OnceLock::new();
pub(crate) const AGENTLAB_DOCKER_MAX_ACTIVE_CONTAINERS_ENV: &str =
    "AGENTLAB_DOCKER_MAX_ACTIVE_CONTAINERS";
pub(crate) const AGENTLAB_MODAL_MAX_ACTIVE_SANDBOXES_ENV: &str =
    "AGENTLAB_MODAL_MAX_ACTIVE_SANDBOXES";
const DEFAULT_DOCKER_MAX_ACTIVE_CONTAINERS: usize = 24;
const DEFAULT_MODAL_MAX_ACTIVE_SANDBOXES: usize = 64;
const MODAL_LAUNCHER_LOG_TAIL_BYTES: u64 = 1024 * 1024;

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

fn docker_active_container_limiter() -> &'static ActiveRuntimeLimiter {
    static LIMITER: OnceLock<ActiveRuntimeLimiter> = OnceLock::new();
    LIMITER.get_or_init(ActiveRuntimeLimiter::new)
}

fn modal_active_sandbox_limiter() -> &'static ActiveRuntimeLimiter {
    static LIMITER: OnceLock<ActiveRuntimeLimiter> = OnceLock::new();
    LIMITER.get_or_init(ActiveRuntimeLimiter::new)
}

fn active_runtime_limit(env_name: &str, default: usize) -> usize {
    std::env::var(env_name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn planned_docker_active_container_units(request: &AdapterRunRequest<'_>) -> Result<usize> {
    let host_agent_without_grading = request
        .runtime_experiment
        .pointer("/trial_runtime/execution/agent_site")
        .and_then(Value::as_str)
        == Some("host")
        && !request.benchmark_grading_enabled;
    if host_agent_without_grading {
        return Ok(0);
    }

    let mut units = 1 + trial_sidecar_plans(request.runtime_experiment)?.len();
    if request.benchmark_grading_enabled
        && request
            .benchmark_grader
            .map(|grader| matches!(grader.strategy, GradingStrategy::Separate))
            .unwrap_or(false)
    {
        units += 1;
    }
    Ok(units)
}

fn planned_modal_active_sandbox_units(request: &AdapterRunRequest<'_>) -> Result<usize> {
    let mut units = 1;
    if request.benchmark_grading_enabled
        && request
            .benchmark_grader
            .map(|grader| matches!(grader.strategy, GradingStrategy::Separate))
            .unwrap_or(false)
    {
        units += 1;
    }
    Ok(units)
}

#[cfg(test)]
fn acquire_docker_active_container_permit(
    request: &AdapterRunRequest<'_>,
) -> Result<ActiveRuntimePermit> {
    let units = planned_docker_active_container_units(request)?;
    acquire_docker_active_container_units_permit(units)
}

fn acquire_docker_active_container_units_permit(units: usize) -> Result<ActiveRuntimePermit> {
    docker_active_container_limiter().acquire(
        units,
        active_runtime_limit(
            AGENTLAB_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
            DEFAULT_DOCKER_MAX_ACTIVE_CONTAINERS,
        ),
        "Docker containers",
        AGENTLAB_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
    )
}

fn enforce_observed_docker_active_container_cap(
    docker: &DockerRuntime,
    planned_units: usize,
) -> Result<()> {
    if planned_units == 0 {
        return Ok(());
    }
    let limit = active_runtime_limit(
        AGENTLAB_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
        DEFAULT_DOCKER_MAX_ACTIVE_CONTAINERS,
    );
    let active = docker
        .list_running_containers_by_labels(&["agentlab.run_id".to_string()])
        .context("listing active AgentLab Docker containers")?
        .len();
    if active + planned_units > limit {
        return Err(anyhow!(
            "Docker currently has {} active AgentLab containers and this trial requires {} more, but {} limits this runner to {}",
            active,
            planned_units,
            AGENTLAB_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
            limit
        ));
    }
    Ok(())
}

fn acquire_modal_active_sandbox_permit(
    request: &AdapterRunRequest<'_>,
) -> Result<ActiveRuntimePermit> {
    let units = planned_modal_active_sandbox_units(request)?;
    modal_active_sandbox_limiter().acquire(
        units,
        active_runtime_limit(
            AGENTLAB_MODAL_MAX_ACTIVE_SANDBOXES_ENV,
            DEFAULT_MODAL_MAX_ACTIVE_SANDBOXES,
        ),
        "Modal sandboxes",
        AGENTLAB_MODAL_MAX_ACTIVE_SANDBOXES_ENV,
    )
}

#[cfg(test)]
pub(crate) fn planned_docker_active_container_units_for_test(
    request: &AdapterRunRequest<'_>,
) -> Result<usize> {
    planned_docker_active_container_units(request)
}

#[cfg(test)]
pub(crate) fn planned_modal_active_sandbox_units_for_test(
    request: &AdapterRunRequest<'_>,
) -> Result<usize> {
    planned_modal_active_sandbox_units(request)
}

#[cfg(test)]
pub(crate) fn acquire_docker_active_container_permit_for_test(
    request: &AdapterRunRequest<'_>,
) -> Result<ActiveRuntimePermit> {
    acquire_docker_active_container_permit(request)
}

#[cfg(test)]
pub(crate) fn acquire_modal_active_sandbox_permit_for_test(
    request: &AdapterRunRequest<'_>,
) -> Result<ActiveRuntimePermit> {
    acquire_modal_active_sandbox_permit(request)
}

#[derive(Clone)]
pub(crate) struct AdapterRunRequest<'a> {
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
    pub(crate) benchmark_grader: Option<&'a BenchmarkGraderConfig>,
    pub(crate) benchmark_grading_enabled: bool,
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
    #[allow(dead_code)]
    RemoteRef {
        uri: String,
        digest: Option<String>,
        size_bytes: Option<u64>,
        media_type: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeSyncKind {
    LocalBindMount,
    S3Compatible,
}

pub(crate) trait RuntimeSync {
    fn kind(&self) -> RuntimeSyncKind;
}

pub(crate) trait LocalContainerRuntimeSync: RuntimeSync {
    fn container_mounts(
        &self,
        request: &AdapterRunRequest<'_>,
        include_agent_artifact: bool,
        extra_mounts: &[ResolvedMountReference],
    ) -> Result<Vec<ContainerMount>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct LocalBindMountRuntimeSync;

impl RuntimeSync for LocalBindMountRuntimeSync {
    fn kind(&self) -> RuntimeSyncKind {
        RuntimeSyncKind::LocalBindMount
    }
}

impl LocalContainerRuntimeSync for LocalBindMountRuntimeSync {
    fn container_mounts(
        &self,
        request: &AdapterRunRequest<'_>,
        include_agent_artifact: bool,
        extra_mounts: &[ResolvedMountReference],
    ) -> Result<Vec<ContainerMount>> {
        let mut mounts = vec![
            ContainerMount {
                host_path: request.trial_paths.in_dir.clone(),
                container_path: AGENTLAB_CONTRACT_IN_DIR.to_string(),
                read_only: true,
            },
            ContainerMount {
                host_path: request.trial_paths.out.clone(),
                container_path: AGENTLAB_CONTRACT_OUT_DIR.to_string(),
                read_only: false,
            },
        ];
        mounts.extend(request.dynamic_mounts.iter().map(|mount| ContainerMount {
            host_path: mount.host_path.clone(),
            container_path: mount.mount_path.clone(),
            read_only: mount.read_only,
        }));
        mounts.extend(extra_mounts.iter().map(|mount| ContainerMount {
            host_path: mount.host_path.clone(),
            container_path: mount.mount_path.clone(),
            read_only: mount.read_only,
        }));
        mounts.extend(
            request
                .secret_file_mounts
                .iter()
                .map(|mount| ContainerMount {
                    host_path: mount.source_from_host.clone(),
                    container_path: mount.target_path.clone(),
                    read_only: true,
                }),
        );
        mounts.extend(
            request
                .secret_file_mounts
                .iter()
                .filter_map(|mount| mount.credential_cache.as_ref())
                .map(|cache| ContainerMount {
                    host_path: cache.host_dir.clone(),
                    container_path: cache.target_dir.clone(),
                    read_only: false,
                }),
        );
        if include_agent_artifact {
            if let Some(bundle) = request.agent_artifact {
                let mount_path = request.agent_artifact_mount_path.ok_or_else(|| {
                    anyhow!(
                        "trial_runtime.agent.artifact.mount.path is required when artifact is set"
                    )
                })?;
                let bundle_root = resolve_agent_artifact_mount_dir(bundle)?;
                mounts.push(ContainerMount {
                    host_path: bundle_root,
                    container_path: mount_path.to_string(),
                    read_only: request.agent_artifact_read_only,
                });
            }
        }
        validate_container_mount_targets(&mounts)?;
        Ok(mounts)
    }
}

fn extend_with_sidecar_env(
    env: &mut BTreeMap<String, String>,
    request: &AdapterRunRequest<'_>,
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

fn trial_ephemeral_network_name(
    request: &AdapterRunRequest<'_>,
    schedule_idx: usize,
    attempt_no: u32,
) -> String {
    canonical_json_digest(&json!({
        "run_id": request.run_id,
        "trial_dir": request.trial_paths.trial_dir.to_string_lossy(),
        "schedule_idx": schedule_idx,
        "attempt": attempt_no,
        "pid": std::process::id(),
    }))
    .replace(':', "_")
    .chars()
    .fold("agentlab_ephemeral_".to_string(), |mut acc, ch| {
        if ch == '_' || ch == '-' || ch.is_ascii_alphanumeric() {
            acc.push(ch);
        }
        acc
    })
}

#[derive(Debug)]
struct TrialEphemeralNetwork {
    name: String,
    internal: bool,
}

fn create_trial_ephemeral_network(
    docker: &DockerRuntime,
    request: &AdapterRunRequest<'_>,
    schedule_idx: usize,
    attempt_no: u32,
) -> Result<Option<TrialEphemeralNetwork>> {
    if trial_sidecar_plans(request.runtime_experiment)?.is_empty() {
        return Ok(None);
    }
    let name = trial_ephemeral_network_name(request, schedule_idx, attempt_no);
    let mut labels = BTreeMap::new();
    labels.insert("agentlab.run_id".to_string(), request.run_id.to_string());
    labels.insert("agentlab.role".to_string(), "ephemeral_network".to_string());
    if let Some(run_dir_digest) =
        run_dir_scope_digest_from_trial_dir(&request.trial_paths.trial_dir)
    {
        labels.insert("agentlab.run_dir_digest".to_string(), run_dir_digest);
    }
    if let Some(trial_id) = request
        .trial_paths
        .trial_dir
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
    {
        labels.insert("agentlab.trial_id".to_string(), trial_id.to_string());
    }
    let internal = request.network_mode == "none";
    docker.create_network(&name, internal, labels)?;
    Ok(Some(TrialEphemeralNetwork { name, internal }))
}

fn remove_trial_ephemeral_network(
    docker: &DockerRuntime,
    network: Option<&TrialEphemeralNetwork>,
) -> Option<String> {
    let network = network?;
    match docker.remove_network(&network.name) {
        Ok(()) => None,
        Err(err) if err.to_string().contains("not found") || err.to_string().contains("404") => {
            None
        }
        Err(err) => Some(err.to_string()),
    }
}

fn start_trial_ephemerals(
    docker: &DockerRuntime,
    request: &AdapterRunRequest<'_>,
    network_name: &str,
    attempt_state: &mut TrialAttemptState,
    stage: &str,
    already_started: &BTreeSet<String>,
) -> Result<Vec<(RuntimeSidecarPlan, ContainerHandle)>> {
    let plans = sidecar_plans_for_stage(request.runtime_experiment, stage)?;
    let mut started = Vec::new();
    for plan in plans {
        if already_started.contains(&plan.id) {
            continue;
        }
        let start_one = (|| -> Result<(RuntimeSidecarPlan, ContainerHandle)> {
            if plan.lifecycle != "per-trial" {
                return Err(anyhow!(
                    "sidecar '{}' lifecycle '{}' is not supported",
                    plan.id,
                    plan.lifecycle
                ));
            }
            docker.ensure_image_with_platform(&plan.image, None)?;
            let mut spec = ContainerSpec::image_default(plan.image.clone());
            if !plan.command.is_empty() {
                spec.command = plan.command.clone();
            }
            spec.env = plan.env.clone();
            spec.workdir = plan.workdir.clone();
            spec.network_mode = Some(network_name.to_string());
            spec.network_aliases = vec![plan.id.clone()];
            spec.labels = trial_container_labels(request, &format!("sidecar:{}", plan.id));
            let handle = docker.create_and_start_container_checked(
                &spec,
                &format!("sidecar '{}' container", plan.id),
            )?;
            attempt_state.ephemerals.push(EphemeralSandboxState {
                id: plan.id.clone(),
                container_id: handle.container_id.clone(),
                image: plan.image.clone(),
                lifecycle: plan.lifecycle.clone(),
            });
            Ok((plan, handle))
        })();
        match start_one {
            Ok(started_one) => started.push(started_one),
            Err(err) => {
                let mut cleanup_errors = Vec::new();
                for (started_plan, handle) in started.iter().rev() {
                    if let Err(cleanup_err) = docker.remove_container_with_retry(
                        handle,
                        true,
                        &format!("sidecar '{}' rollback cleanup", started_plan.id),
                    ) {
                        cleanup_errors.push(cleanup_err.to_string());
                    }
                }
                if cleanup_errors.is_empty() {
                    return Err(err);
                }
                return Err(err.context(format!(
                    "sidecar rollback cleanup also failed: {}",
                    cleanup_errors.join("; ")
                )));
            }
        }
    }
    Ok(started)
}

#[cfg(test)]
pub(crate) fn sidecar_env_for_stage_for_test(
    request: &AdapterRunRequest<'_>,
    stage: &str,
) -> Result<BTreeMap<String, String>> {
    sidecar_env_for_stage(request.runtime_experiment, stage)
}

fn normalized_container_mount_target(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("container mount target must not be empty"));
    }
    let path = Path::new(trimmed);
    if !path.is_absolute() {
        return Err(anyhow!(
            "container mount target must be absolute: {}",
            trimmed
        ));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(anyhow!(
                        "container mount target contains non-utf8 segment: {}",
                        trimmed
                    ));
                };
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(anyhow!(
                    "container mount target must not contain '..': {}",
                    trimmed
                ));
            }
            Component::Prefix(_) => {
                return Err(anyhow!(
                    "container mount target must be a Unix-style absolute path: {}",
                    trimmed
                ));
            }
        }
    }
    if parts.is_empty() {
        return Ok("/".to_string());
    }
    Ok(format!("/{}", parts.join("/")))
}

fn validate_container_mount_targets(mounts: &[ContainerMount]) -> Result<()> {
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    for mount in mounts {
        let target = normalized_container_mount_target(&mount.container_path)?;
        if let Some(previous_host) = seen.insert(target.clone(), mount.host_path.clone()) {
            return Err(anyhow!(
                "container mount target '{}' is declared more than once ({} and {})",
                target,
                previous_host.display(),
                mount.host_path.display()
            ));
        }
        for (previous_target, previous_host) in &seen {
            if previous_target == &target {
                continue;
            }
            if container_mount_target_contains(previous_target, &target)
                || container_mount_target_contains(&target, previous_target)
            {
                return Err(anyhow!(
                    "container mount target '{}' overlaps with '{}' ({} and {})",
                    target,
                    previous_target,
                    mount.host_path.display(),
                    previous_host.display()
                ));
            }
        }
    }
    Ok(())
}

fn container_mount_target_contains(parent: &str, child: &str) -> bool {
    if parent == "/" {
        return child != "/";
    }
    child.starts_with(&format!("{}/", parent.trim_end_matches('/')))
}

fn validate_modal_copy_targets(copies: &[Value]) -> Result<()> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for copy in copies {
        let remote_path = copy
            .get("remote_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("modal copy entry missing remote_path"))?;
        let target = normalized_container_mount_target(remote_path)?;
        let local_path = copy
            .get("local_path")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        for (previous_target, previous_local) in &seen {
            if container_mount_target_contains(&target, previous_target) {
                return Err(anyhow!(
                    "modal copy remote_path '{}' overlaps with '{}' ({} and {})",
                    target,
                    previous_target,
                    local_path,
                    previous_local
                ));
            }
        }
        if let Some(previous_local) = seen.insert(target.clone(), local_path.to_string()) {
            return Err(anyhow!(
                "modal copy remote_path '{}' is declared more than once ({} and {})",
                target,
                previous_local,
                local_path
            ));
        }
    }
    Ok(())
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
    pub(crate) attempt_no: u32,
    pub(crate) adapter: &'a AdapterRunRequest<'a>,
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

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct LocalDockerExecutionBackend<S = LocalBindMountRuntimeSync> {
    runtime_sync: S,
}

impl LocalDockerExecutionBackend<LocalBindMountRuntimeSync> {
    pub(crate) fn new() -> Self {
        Self {
            runtime_sync: LocalBindMountRuntimeSync,
        }
    }
}

impl<S> LocalDockerExecutionBackend<S>
where
    S: LocalContainerRuntimeSync,
{
    #[cfg(test)]
    pub(crate) fn with_runtime_sync(runtime_sync: S) -> Self {
        Self { runtime_sync }
    }
}

impl<S> ExecutionBackend for LocalDockerExecutionBackend<S>
where
    S: LocalContainerRuntimeSync,
{
    fn executor_kind(&self) -> ExecutorKind {
        ExecutorKind::LocalDocker
    }

    fn execute_attempt(
        &self,
        request: TrialRuntimeExecutionRequest<'_>,
    ) -> Result<TrialRuntimeOutcome> {
        let mut outcome = execute_local_docker_trial_runtime(request, &self.runtime_sync)?;
        outcome.executor = self.executor_kind();
        Ok(outcome)
    }

    fn cleanup_attempt_runtime(
        &self,
        request: TrialRuntimeCleanupRequest<'_>,
    ) -> Result<RuntimeCleanupOutcome> {
        cleanup_local_docker_attempt_runtime(request)
    }

    fn cleanup_run_runtime(&self, run_id: &str) -> Result<RuntimeCleanupOutcome> {
        cleanup_local_docker_run_runtime(run_id)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct S3CompatibleRuntimeSync {
    bucket: String,
    base_prefix: String,
    prefix: String,
    endpoint_url: Option<String>,
    region: Option<String>,
    modal_secret_name: Option<String>,
    force_path_style: bool,
}

impl RuntimeSync for S3CompatibleRuntimeSync {
    fn kind(&self) -> RuntimeSyncKind {
        RuntimeSyncKind::S3Compatible
    }
}

impl S3CompatibleRuntimeSync {
    fn from_env(run_id: &str, trial_id: &str, attempt_no: u32) -> Result<Self> {
        let bucket = std::env::var("AGENTLAB_MODAL_S3_BUCKET")
            .or_else(|_| std::env::var("AGENTLAB_S3_BUCKET"))
            .map_err(|_| {
                anyhow!("executor modal requires AGENTLAB_MODAL_S3_BUCKET or AGENTLAB_S3_BUCKET")
            })?;
        let base_prefix = std::env::var("AGENTLAB_MODAL_S3_PREFIX")
            .or_else(|_| std::env::var("AGENTLAB_S3_PREFIX"))
            .unwrap_or_else(|_| "agentlab-runs".to_string());
        let prefix = format!(
            "{}/{}/{}/attempt_{}",
            base_prefix.trim_matches('/'),
            run_id,
            trial_id,
            attempt_no
        );
        Ok(Self {
            bucket,
            base_prefix: base_prefix.trim_matches('/').to_string(),
            prefix,
            endpoint_url: std::env::var("AGENTLAB_MODAL_S3_ENDPOINT_URL")
                .or_else(|_| std::env::var("AGENTLAB_S3_ENDPOINT_URL"))
                .ok(),
            region: std::env::var("AGENTLAB_MODAL_S3_REGION")
                .or_else(|_| std::env::var("AWS_REGION"))
                .ok(),
            modal_secret_name: std::env::var("AGENTLAB_MODAL_S3_SECRET").ok(),
            force_path_style: env_flag("AGENTLAB_MODAL_S3_FORCE_PATH_STYLE")
                || env_flag("AGENTLAB_S3_FORCE_PATH_STYLE"),
        })
    }

    fn uri_for_contract_path(&self, path: &str) -> String {
        let rel = path
            .trim_start_matches("/agentlab/")
            .trim_start_matches("agentlab/")
            .trim_start_matches('/');
        format!(
            "s3://{}/{}/{}",
            self.bucket,
            self.prefix.trim_matches('/'),
            rel
        )
    }

    fn immutable_case_asset_prefix(&self, package_root: &Path) -> String {
        let package_digest = load_json_file(&package_root.join("package.lock"))
            .ok()
            .and_then(|lock| {
                lock.pointer("/package_digest")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| {
                canonical_json_digest(&json!({
                    "package_root": package_root.to_string_lossy()
                }))
            });
        format!(
            "{}/packages/{}/case_assets",
            self.base_prefix.trim_matches('/'),
            sanitize_for_fs(&package_digest)
        )
    }

    #[cfg(test)]
    pub(crate) fn from_env_for_test(run_id: &str, trial_id: &str, attempt_no: u32) -> Result<Self> {
        Self::from_env(run_id, trial_id, attempt_no)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        bucket: &str,
        prefix: &str,
        endpoint_url: Option<&str>,
        region: Option<&str>,
        modal_secret_name: Option<&str>,
        force_path_style: bool,
    ) -> Self {
        Self {
            bucket: bucket.to_string(),
            base_prefix: prefix
                .split('/')
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or(prefix)
                .to_string(),
            prefix: prefix.to_string(),
            endpoint_url: endpoint_url.map(str::to_string),
            region: region.map(str::to_string),
            modal_secret_name: modal_secret_name.map(str::to_string),
            force_path_style,
        }
    }

    #[cfg(test)]
    pub(crate) fn uri_for_contract_path_for_test(&self, path: &str) -> String {
        self.uri_for_contract_path(path)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ModalExecutionBackend {
    app_name: String,
    environment_name: Option<String>,
    python: String,
}

impl ModalExecutionBackend {
    pub(crate) fn from_env() -> Self {
        Self {
            app_name: std::env::var("AGENTLAB_MODAL_APP_NAME")
                .unwrap_or_else(|_| "agentlab-runner".to_string()),
            environment_name: std::env::var("AGENTLAB_MODAL_ENVIRONMENT").ok(),
            python: std::env::var("AGENTLAB_MODAL_PYTHON")
                .unwrap_or_else(|_| "python3".to_string()),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(app_name: &str, environment_name: Option<&str>) -> Self {
        Self {
            app_name: app_name.to_string(),
            environment_name: environment_name.map(str::to_string),
            python: "python3".to_string(),
        }
    }
}

impl ExecutionBackend for ModalExecutionBackend {
    fn executor_kind(&self) -> ExecutorKind {
        ExecutorKind::Modal
    }

    fn execute_attempt(
        &self,
        request: TrialRuntimeExecutionRequest<'_>,
    ) -> Result<TrialRuntimeOutcome> {
        execute_modal_trial_runtime(request, self)
    }

    fn cleanup_attempt_runtime(
        &self,
        request: TrialRuntimeCleanupRequest<'_>,
    ) -> Result<RuntimeCleanupOutcome> {
        cleanup_modal_attempt_runtime(request, self)
    }

    fn cleanup_run_runtime(&self, _run_id: &str) -> Result<RuntimeCleanupOutcome> {
        Ok(RuntimeCleanupOutcome { cleaned_workers: 0 })
    }
}

fn cleanup_run_dir_from_trial_dir(trial_dir: &Path) -> Option<PathBuf> {
    trial_dir.parent()?.parent().map(Path::to_path_buf)
}

fn docker_runtime_file_trial_container_handles(trial_dir: &Path) -> Result<Vec<ContainerHandle>> {
    Ok(load_trial_attempt_container_ids(trial_dir)?
        .into_iter()
        .map(|container_id| ContainerHandle { container_id })
        .collect())
}

fn docker_runtime_file_trial_network_handles(trial_dir: &Path) -> Result<Vec<NetworkHandle>> {
    if !trial_attempt_state_exists(trial_dir) {
        return Ok(Vec::new());
    }
    Ok(load_trial_attempt_state(trial_dir)?
        .state
        .ephemeral_networks
        .into_iter()
        .map(|network| NetworkHandle {
            network_id: network.name,
        })
        .collect())
}

fn docker_runtime_db_trial_container_handles(
    run_id: &str,
    trial_id: &str,
    trial_dir: &Path,
) -> Result<Vec<ContainerHandle>> {
    let run_dir = cleanup_run_dir_from_trial_dir(trial_dir)
        .ok_or_else(|| anyhow!("trial directory has no run parent: {}", trial_dir.display()))?;
    Ok(SqliteRunStore::open(&run_dir)?
        .trial_attempt_container_ids(run_id, trial_id)?
        .into_iter()
        .map(|container_id| ContainerHandle { container_id })
        .collect())
}

fn docker_db_trial_attempt_exists(run_id: &str, trial_id: &str, trial_dir: &Path) -> Result<bool> {
    let Some(run_dir) = cleanup_run_dir_from_trial_dir(trial_dir) else {
        return Ok(false);
    };
    Ok(SqliteRunStore::open(&run_dir)?
        .load_latest_trial_attempt(run_id, trial_id)?
        .is_some())
}

fn docker_db_trial_attempt_exists_if_available(
    run_id: &str,
    trial_id: &str,
    trial_dir: &Path,
) -> Result<Option<bool>> {
    match docker_db_trial_attempt_exists(run_id, trial_id, trial_dir) {
        Ok(exists) => Ok(Some(exists)),
        Err(err) if is_sqlite_busy_error(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

fn run_dir_scope_digest(run_dir: &Path) -> String {
    canonical_json_digest(&json!({
        "run_dir": run_dir.to_string_lossy(),
    }))
}

fn run_dir_scope_digest_from_trial_dir(trial_dir: &Path) -> Option<String> {
    cleanup_run_dir_from_trial_dir(trial_dir).map(|run_dir| run_dir_scope_digest(&run_dir))
}

fn docker_agentlab_runtime_labels(
    run_id: &str,
    trial_id: Option<&str>,
    run_dir_digest: Option<&str>,
) -> Vec<String> {
    let mut labels = vec![format!("agentlab.run_id={}", run_id)];
    if let Some(trial_id) = trial_id {
        labels.push(format!("agentlab.trial_id={}", trial_id));
    }
    if let Some(run_dir_digest) = run_dir_digest {
        labels.push(format!("agentlab.run_dir_digest={}", run_dir_digest));
    }
    labels
}

fn docker_labeled_trial_container_handles(
    run_id: &str,
    trial_id: &str,
    run_dir_digest: Option<&str>,
) -> Result<Vec<ContainerHandle>> {
    DockerRuntime::connect()?.list_containers_by_labels(&docker_agentlab_runtime_labels(
        run_id,
        Some(trial_id),
        run_dir_digest,
    ))
}

fn docker_labeled_run_container_handles(run_id: &str) -> Result<Vec<ContainerHandle>> {
    DockerRuntime::connect()?
        .list_containers_by_labels(&docker_agentlab_runtime_labels(run_id, None, None))
}

fn docker_labeled_trial_network_handles(
    run_id: &str,
    trial_id: &str,
    run_dir_digest: Option<&str>,
) -> Result<Vec<NetworkHandle>> {
    DockerRuntime::connect()?.list_networks_by_labels(&docker_agentlab_runtime_labels(
        run_id,
        Some(trial_id),
        run_dir_digest,
    ))
}

fn docker_labeled_run_network_handles(run_id: &str) -> Result<Vec<NetworkHandle>> {
    DockerRuntime::connect()?
        .list_networks_by_labels(&docker_agentlab_runtime_labels(run_id, None, None))
}

fn dedupe_docker_container_handles(handles: Vec<ContainerHandle>) -> Vec<ContainerHandle> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for handle in handles {
        if seen.insert(handle.container_id.clone()) {
            deduped.push(handle);
        }
    }
    deduped
}

fn remove_docker_container_handles_required(handles: &[ContainerHandle]) -> Result<()> {
    if handles.is_empty() {
        return Ok(());
    }
    let docker = DockerRuntime::connect()?;
    for handle in handles {
        if let Err(err) = docker.kill_container(handle) {
            if !err.to_string().contains("not found") {
                return Err(err);
            }
        }
        if let Err(err) = docker.remove_container_with_retry(handle, true, "kill trial cleanup") {
            if !err.to_string().contains("not found") {
                return Err(err);
            }
        }
    }
    Ok(())
}

fn dedupe_docker_network_handles(handles: Vec<NetworkHandle>) -> Vec<NetworkHandle> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for handle in handles {
        if seen.insert(handle.network_id.clone()) {
            deduped.push(handle);
        }
    }
    deduped
}

fn remove_docker_network_handles_required(handles: &[NetworkHandle]) -> Result<()> {
    if handles.is_empty() {
        return Ok(());
    }
    let docker = DockerRuntime::connect()?;
    for handle in handles {
        if let Err(err) = docker.remove_network(&handle.network_id) {
            if !err.to_string().contains("not found") && !err.to_string().contains("404") {
                return Err(err);
            }
        }
    }
    Ok(())
}

fn cleanup_local_docker_attempt_runtime(
    request: TrialRuntimeCleanupRequest<'_>,
) -> Result<RuntimeCleanupOutcome> {
    let mut handles = docker_runtime_file_trial_container_handles(request.trial_dir)?;
    let mut network_handles = docker_runtime_file_trial_network_handles(request.trial_dir)?;
    let scoped_digest = run_dir_scope_digest_from_trial_dir(request.trial_dir);
    let db_container_lookup_error = if handles.is_empty() {
        match docker_runtime_db_trial_container_handles(
            request.run_id,
            request.trial_id,
            request.trial_dir,
        ) {
            Ok(db_handles) => {
                handles.extend(db_handles);
                None
            }
            Err(err) if is_sqlite_busy_error(&err) => Some(err),
            Err(err) => return Err(err),
        }
    } else {
        None
    };
    handles.extend(docker_labeled_trial_container_handles(
        request.run_id,
        request.trial_id,
        scoped_digest.as_deref(),
    )?);
    network_handles.extend(docker_labeled_trial_network_handles(
        request.run_id,
        request.trial_id,
        scoped_digest.as_deref(),
    )?);
    let active_state_exists = trial_attempt_state_exists(request.trial_dir)
        || docker_db_trial_attempt_exists_if_available(
            request.run_id,
            request.trial_id,
            request.trial_dir,
        )?
        .unwrap_or(false);
    if handles.is_empty() && network_handles.is_empty() && !active_state_exists {
        handles.extend(docker_labeled_trial_container_handles(
            request.run_id,
            request.trial_id,
            None,
        )?);
        network_handles.extend(docker_labeled_trial_network_handles(
            request.run_id,
            request.trial_id,
            None,
        )?);
    }
    let handles = dedupe_docker_container_handles(handles);
    let network_handles = dedupe_docker_network_handles(network_handles);
    if handles.is_empty() && network_handles.is_empty() {
        if active_state_exists {
            return Err(anyhow!(
                "cleanup_missing_runtime_container: active runtime state exists for {} but no persisted or labeled container ids were found",
                request.trial_id
            ));
        }
        if let Some(err) = db_container_lookup_error {
            return Err(err.context(format!(
                "cleanup_runtime_container_lookup_locked: unable to inspect persisted runtime containers for {}",
                request.trial_id
            )));
        }
        return Ok(RuntimeCleanupOutcome { cleaned_workers: 0 });
    }
    let count = handles.len();
    remove_docker_container_handles_required(&handles)?;
    remove_docker_network_handles_required(&network_handles)?;
    Ok(RuntimeCleanupOutcome {
        cleaned_workers: count,
    })
}

fn cleanup_local_docker_run_runtime(run_id: &str) -> Result<RuntimeCleanupOutcome> {
    let handles = dedupe_docker_container_handles(docker_labeled_run_container_handles(run_id)?);
    let network_handles =
        dedupe_docker_network_handles(docker_labeled_run_network_handles(run_id)?);
    let count = handles.len();
    remove_docker_container_handles_required(&handles)?;
    remove_docker_network_handles_required(&network_handles)?;
    Ok(RuntimeCleanupOutcome {
        cleaned_workers: count,
    })
}

fn cleanup_modal_attempt_runtime(
    request: TrialRuntimeCleanupRequest<'_>,
    backend: &ModalExecutionBackend,
) -> Result<RuntimeCleanupOutcome> {
    let worker_ids = load_modal_runtime_worker_ids(request.trial_dir)?;
    if worker_ids.is_empty() {
        if trial_attempt_state_exists(request.trial_dir) {
            return Err(anyhow!(
                "modal_cleanup_missing_runtime_worker: active runtime state exists for {} but no modal sandbox id was recorded",
                request.trial_id
            ));
        }
        return Ok(RuntimeCleanupOutcome { cleaned_workers: 0 });
    }
    run_modal_cleanup(&backend.python, request.trial_dir, &worker_ids)
}

fn modal_runtime_workers_path(trial_dir: &Path) -> PathBuf {
    trial_dir.join("modal").join("runtime_workers.json")
}

fn load_modal_runtime_worker_ids(trial_dir: &Path) -> Result<Vec<String>> {
    let mut ids = load_trial_attempt_container_ids(trial_dir)?;
    let path = modal_runtime_workers_path(trial_dir);
    if path.exists() {
        let value: Value = match fs::read(&path)
            .with_context(|| format!("read modal runtime workers {}", path.display()))
            .and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse modal runtime workers {}", path.display()))
            }) {
            Ok(value) => value,
            Err(err) if !ids.is_empty() => {
                let mut seen = BTreeSet::new();
                ids.retain(|id| !id.trim().is_empty() && seen.insert(id.clone()));
                return Ok(ids);
            }
            Err(err) => return Err(err),
        };
        if let Some(workers) = value.get("workers").and_then(Value::as_array) {
            for worker in workers {
                if let Some(id) = worker.get("sandbox_id").and_then(Value::as_str) {
                    ids.push(id.to_string());
                }
            }
        }
        if let Some(sandbox_ids) = value.get("sandbox_ids").and_then(Value::as_array) {
            for id in sandbox_ids {
                if let Some(id) = id.as_str() {
                    ids.push(id.to_string());
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    ids.retain(|id| !id.trim().is_empty() && seen.insert(id.clone()));
    Ok(ids)
}

#[cfg(test)]
pub(crate) fn load_modal_runtime_worker_ids_for_test(trial_dir: &Path) -> Result<Vec<String>> {
    load_modal_runtime_worker_ids(trial_dir)
}

fn run_modal_cleanup(
    python: &str,
    trial_dir: &Path,
    worker_ids: &[String],
) -> Result<RuntimeCleanupOutcome> {
    let modal_dir = trial_dir.join("modal");
    ensure_dir(&modal_dir)?;
    let script_path = modal_dir.join("agentlab_modal_cleanup.py");
    let spec_path = modal_dir.join("cleanup.json");
    fs::write(&script_path, MODAL_CLEANUP_SCRIPT)?;
    fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({ "sandbox_ids": worker_ids }))?,
    )?;
    let mut command = Command::new(python);
    command.arg(&script_path).arg(&spec_path);
    let output = run_modal_launcher_command(command, &modal_dir, "cleanup")?;
    if !output.status.success() {
        return Err(anyhow!(
            "modal_cleanup_failed: modal cleanup launcher failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            output.stdout_tail,
            output.stderr_tail
        ));
    }
    let marker = output
        .stdout_tail
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("AGENTLAB_MODAL_CLEANUP="))
        .ok_or_else(|| {
            anyhow!(
                "modal cleanup launcher did not emit AGENTLAB_MODAL_CLEANUP in {}",
                output.stdout_path.display()
            )
        })?;
    let value: Value = serde_json::from_str(marker)?;
    let cleaned = value
        .get("cleaned")
        .and_then(Value::as_u64)
        .unwrap_or(worker_ids.len() as u64) as usize;
    Ok(RuntimeCleanupOutcome {
        cleaned_workers: cleaned,
    })
}

struct ModalLauncherOutput {
    status: ExitStatus,
    stdout_tail: String,
    stderr_tail: String,
    stdout_path: PathBuf,
}

fn run_modal_launcher_command(
    mut command: Command,
    modal_dir: &Path,
    log_stem: &str,
) -> Result<ModalLauncherOutput> {
    ensure_dir(modal_dir)?;
    let stdout_path = modal_dir.join(format!("{log_stem}_stdout.log"));
    let stderr_path = modal_dir.join(format!("{log_stem}_stderr.log"));
    let stdout = File::create(&stdout_path)
        .with_context(|| format!("create modal launcher stdout log {}", stdout_path.display()))?;
    let stderr = File::create(&stderr_path)
        .with_context(|| format!("create modal launcher stderr log {}", stderr_path.display()))?;
    let status = command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .context("run modal launcher command")?;
    Ok(ModalLauncherOutput {
        status,
        stdout_tail: read_file_tail_lossy(&stdout_path, MODAL_LAUNCHER_LOG_TAIL_BYTES)?,
        stderr_tail: read_file_tail_lossy(&stderr_path, MODAL_LAUNCHER_LOG_TAIL_BYTES)?,
        stdout_path,
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
    match std::env::var(AGENTLAB_MAX_INLINE_CAPTURE_BYTES_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = trimmed.parse::<u64>().map_err(|_| {
                anyhow!(
                    "{} must be a positive integer when set (got: {})",
                    AGENTLAB_MAX_INLINE_CAPTURE_BYTES_ENV,
                    raw
                )
            })?;
            if parsed == 0 {
                return Err(anyhow!(
                    "{} must be > 0 when set",
                    AGENTLAB_MAX_INLINE_CAPTURE_BYTES_ENV
                ));
            }
            Ok(Some(parsed))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(anyhow!(
            "failed reading {}: {}",
            AGENTLAB_MAX_INLINE_CAPTURE_BYTES_ENV,
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
            "{} capture at {} is too large to inline: bytes={} max={} override_env_var={} use format=bytes or AGENTLAB_MAX_RUN_BYTES for large artifacts",
            label,
            path.display(),
            len,
            max_bytes,
            AGENTLAB_MAX_INLINE_CAPTURE_BYTES_ENV
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
    attempt_no: u32,
    request: &AdapterRunRequest<'_>,
    task_id: &str,
    variant_id: &str,
    repl_idx: usize,
) -> Option<LiveEventIngestHandle> {
    let sink = request.runtime.event_sinks.first()?;
    if !sink.ingest {
        return None;
    }
    Some(spawn_live_event_ingest(LiveEventIngestRequest {
        run_dir: request.package_root.to_path_buf(),
        events_path: request.io_paths.events_host.clone(),
        run_id: request.run_id.to_string(),
        trial_id: trial_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("trial")
            .to_string(),
        schedule_idx,
        variant_id: variant_id.to_string(),
        task_id: task_id.to_string(),
        repl_idx,
        attempt: attempt_no as usize,
    }))
}

fn stop_live_event_ingest(handle: Option<LiveEventIngestHandle>) -> Result<()> {
    if let Some(handle) = handle {
        let _rows_ingested = handle.stop()?;
    }
    Ok(())
}

struct HostGraderConcurrencyState {
    active: usize,
    max: usize,
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
        let mut state = self.limiter.state.lock().unwrap();
        state.active = state.active.saturating_sub(1);
        self.limiter.available.notify_one();
    }
}

fn host_grader_concurrency_limiter() -> &'static HostGraderConcurrencyLimiter {
    static LIMITER: OnceLock<HostGraderConcurrencyLimiter> = OnceLock::new();
    LIMITER.get_or_init(|| HostGraderConcurrencyLimiter {
        state: Mutex::new(HostGraderConcurrencyState {
            active: 0,
            max: usize::MAX,
        }),
        available: Condvar::new(),
    })
}

pub(crate) fn configure_host_grader_max_concurrency(max_concurrency: Option<usize>) {
    let limiter = host_grader_concurrency_limiter();
    let mut state = limiter.state.lock().unwrap();
    state.max = max_concurrency.unwrap_or(usize::MAX).max(1);
    limiter.available.notify_all();
}

fn acquire_host_grader_concurrency_permit() -> HostGraderConcurrencyPermit {
    let limiter = host_grader_concurrency_limiter();
    let mut state = limiter.state.lock().unwrap();
    while state.active >= state.max {
        state = limiter.available.wait(state).unwrap();
    }
    state.active += 1;
    HostGraderConcurrencyPermit { limiter }
}

const INJECTED_BUNDLE_SOURCE_MOUNT_PATH: &str = "/agentlab/_materialize/injected_bundle_src";

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

fn capture_candidate_workspace_patch(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    timeout_ms: u64,
) -> Result<Option<PathBuf>> {
    let patch_log_dir = trial_patch_log_dir(trial_dir);
    ensure_dir(&patch_log_dir)?;
    let probe = docker.exec(
        handle,
        &ExecSpec {
            command: vec![
                "git".to_string(),
                "-C".to_string(),
                request.task_workdir.to_string(),
                "rev-parse".to_string(),
                "--is-inside-work-tree".to_string(),
            ],
            env: BTreeMap::new(),
            workdir: Some(request.task_workdir.to_string()),
        },
    )?;
    let probe_stream = docker.stream_exec_output(
        &probe,
        &patch_log_dir.join("probe_stdout.log"),
        &patch_log_dir.join("probe_stderr.log"),
        Some(Duration::from_millis(timeout_ms.max(1_000))),
    )?;
    let probe_status = docker
        .wait_exec(&probe)
        .unwrap_or(crate::backend::docker::ExecStatus { exit_code: None });
    if probe_stream.timed_out || probe_status.exit_code != Some(0) {
        return Ok(None);
    }

    let pathspec = vec![
        ".".to_string(),
        ":(exclude).agentlab".to_string(),
        ":(exclude).haiku".to_string(),
        ":(exclude).lab".to_string(),
        ":(exclude)logs".to_string(),
        ":(exclude)out".to_string(),
    ];
    let mut add_command = vec![
        "git".to_string(),
        "-C".to_string(),
        request.task_workdir.to_string(),
        "add".to_string(),
        "-N".to_string(),
        "--".to_string(),
    ];
    add_command.extend(pathspec.clone());
    let add_exec = docker.exec(
        handle,
        &ExecSpec {
            command: add_command,
            env: BTreeMap::new(),
            workdir: Some(request.task_workdir.to_string()),
        },
    )?;
    let add_stream = docker.stream_exec_output(
        &add_exec,
        &patch_log_dir.join("add_stdout.log"),
        &patch_log_dir.join("add_stderr.log"),
        Some(Duration::from_millis(timeout_ms.max(1_000))),
    )?;
    let add_status = docker
        .wait_exec(&add_exec)
        .unwrap_or(crate::backend::docker::ExecStatus { exit_code: None });
    if add_stream.timed_out || add_status.exit_code != Some(0) {
        return Err(anyhow!(
            "failed to prepare candidate workspace patch; see {} and {}",
            patch_log_dir.join("add_stdout.log").display(),
            patch_log_dir.join("add_stderr.log").display()
        ));
    }

    let mut diff_command = vec![
        "git".to_string(),
        "-C".to_string(),
        request.task_workdir.to_string(),
        "diff".to_string(),
        "--binary".to_string(),
        "--".to_string(),
    ];
    diff_command.extend(pathspec);
    let patch_path = request.trial_paths.out.join("candidate.patch");
    let diff_exec = docker.exec(
        handle,
        &ExecSpec {
            command: diff_command,
            env: BTreeMap::new(),
            workdir: Some(request.task_workdir.to_string()),
        },
    )?;
    let diff_stream = docker.stream_exec_output(
        &diff_exec,
        &patch_path,
        &patch_log_dir.join("diff_stderr.log"),
        Some(Duration::from_millis(timeout_ms.max(1_000))),
    )?;
    let diff_status = docker
        .wait_exec(&diff_exec)
        .unwrap_or(crate::backend::docker::ExecStatus { exit_code: None });
    if diff_stream.timed_out || diff_status.exit_code != Some(0) {
        return Err(anyhow!(
            "failed to capture candidate workspace patch; see {}",
            patch_log_dir.join("diff_stderr.log").display()
        ));
    }
    Ok(Some(patch_path))
}

fn parse_agent_outputs(
    request: &AdapterRunRequest<'_>,
) -> Result<BTreeMap<String, RuntimeOutputConfig>> {
    let value = request
        .runtime_experiment
        .pointer("/trial_runtime/agent/outputs")
        .cloned()
        .ok_or_else(|| anyhow!("/trial_runtime/agent/outputs is required"))?;
    serde_json::from_value(value)
        .map_err(|err| anyhow!("invalid /trial_runtime/agent/outputs: {}", err))
}

fn container_file_exists(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    trial_dir: &Path,
    label: &str,
    container_path: &str,
    timeout_ms: u64,
) -> Result<bool> {
    let log_dir = trial_dir.join("logs").join("transport");
    ensure_dir(&log_dir)?;
    let exec = docker.exec(
        handle,
        &ExecSpec {
            command: vec![
                "sh".to_string(),
                "-lc".to_string(),
                "test -f \"$1\"".to_string(),
                "sh".to_string(),
                container_path.to_string(),
            ],
            env: BTreeMap::new(),
            workdir: None,
        },
    )?;
    let stream = docker.stream_exec_output(
        &exec,
        &log_dir.join(format!("{}_exists_stdout.log", sanitize_for_fs(label))),
        &log_dir.join(format!("{}_exists_stderr.log", sanitize_for_fs(label))),
        Some(Duration::from_millis(timeout_ms.max(1_000))),
    )?;
    let status = docker
        .wait_exec(&exec)
        .unwrap_or(crate::backend::docker::ExecStatus { exit_code: None });
    if stream.timed_out {
        return Err(anyhow!(
            "timed out checking declared runtime output file {}",
            container_path
        ));
    }
    Ok(status.exit_code == Some(0))
}

fn copy_container_file_to_host(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    trial_dir: &Path,
    label: &str,
    container_path: &str,
    host_path: &Path,
    timeout_ms: u64,
) -> Result<()> {
    if let Some(parent) = host_path.parent() {
        ensure_dir(parent)?;
    }
    let log_dir = trial_dir.join("logs").join("transport");
    ensure_dir(&log_dir)?;
    let exec = docker.exec(
        handle,
        &ExecSpec {
            command: vec![
                "sh".to_string(),
                "-lc".to_string(),
                "cat \"$1\"".to_string(),
                "sh".to_string(),
                container_path.to_string(),
            ],
            env: BTreeMap::new(),
            workdir: None,
        },
    )?;
    let stream = docker.stream_exec_output(
        &exec,
        host_path,
        &log_dir.join(format!("{}_copy_stderr.log", sanitize_for_fs(label))),
        Some(Duration::from_millis(timeout_ms.max(1_000))),
    )?;
    let status = docker
        .wait_exec(&exec)
        .unwrap_or(crate::backend::docker::ExecStatus { exit_code: None });
    if stream.timed_out || status.exit_code != Some(0) {
        return Err(anyhow!(
            "failed to capture declared runtime output file {}",
            container_path
        ));
    }
    Ok(())
}

fn captured_file_host_path(
    docker: Option<&DockerRuntime>,
    handle: Option<&ContainerHandle>,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    label: &str,
    container_path: &str,
    required: bool,
    timeout_ms: u64,
) -> Result<Option<PathBuf>> {
    if let Ok(host_path) = map_container_path_to_host(container_path, request.trial_paths) {
        if host_path.exists() {
            return Ok(Some(host_path));
        }
        if required {
            return Err(anyhow!(
                "declared runtime output {} missing at {}",
                label,
                container_path
            ));
        }
        return Ok(None);
    }

    let host_path = if let (Some(docker), Some(handle)) = (docker, handle) {
        if !container_file_exists(docker, handle, trial_dir, label, container_path, timeout_ms)? {
            if required {
                return Err(anyhow!(
                    "declared runtime output {} missing at {}",
                    label,
                    container_path
                ));
            }
            return Ok(None);
        }
        let extension = Path::new(container_path)
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("out");
        let staged = request
            .trial_paths
            .out
            .join("transport")
            .join("captured")
            .join(format!("{}.{}", sanitize_for_fs(label), extension));
        copy_container_file_to_host(
            docker,
            handle,
            trial_dir,
            label,
            container_path,
            &staged,
            timeout_ms,
        )?;
        staged
    } else {
        let path = PathBuf::from(container_path);
        if !path.exists() {
            if required {
                return Err(anyhow!(
                    "declared host runtime output {} missing at {}",
                    label,
                    container_path
                ));
            }
            return Ok(None);
        }
        path
    };
    Ok(Some(host_path))
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
            "bytes": host_path.metadata().map(|meta| meta.len()).unwrap_or(0)
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

fn capture_runtime_output(
    docker: Option<&DockerRuntime>,
    handle: Option<&ContainerHandle>,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    label: &str,
    output: &RuntimeOutputConfig,
    timeout_ms: u64,
) -> Result<CapturedTransportOutput> {
    let capture = &output.capture;
    match capture.capture_type.as_str() {
        "file" => {
            let container_path = capture
                .path
                .as_deref()
                .ok_or_else(|| anyhow!("{}.capture.path is required", label))?;
            let format = capture
                .format
                .as_deref()
                .ok_or_else(|| anyhow!("{}.capture.format is required", label))?;
            let Some(host_path) = captured_file_host_path(
                docker,
                handle,
                request,
                trial_dir,
                label,
                container_path,
                capture.required,
                timeout_ms,
            )?
            else {
                return Ok(CapturedTransportOutput {
                    value: Value::Null,
                    host_path: None,
                    container_path: Some(container_path.to_string()),
                    format: Some(format.to_string()),
                });
            };
            Ok(CapturedTransportOutput {
                value: read_captured_file_value(&host_path, format)?,
                host_path: Some(host_path),
                container_path: Some(container_path.to_string()),
                format: Some(format.to_string()),
            })
        }
        "result_json" => {
            let container_path = capture
                .path
                .as_deref()
                .ok_or_else(|| anyhow!("{}.capture.path is required", label))?;
            let host_path = captured_file_host_path(
                docker,
                handle,
                request,
                trial_dir,
                label,
                container_path,
                true,
                timeout_ms,
            )?
            .ok_or_else(|| {
                anyhow!(
                    "declared runtime result output missing at {}",
                    container_path
                )
            })?;
            let result_json = read_captured_file_value(&host_path, "json")?;
            let value = if let Some(field) = capture.field.as_deref() {
                json!({
                    "value": select_transport_field(&result_json, field).unwrap_or(Value::Null)
                })
            } else {
                result_json
            };
            Ok(CapturedTransportOutput {
                value,
                host_path: Some(host_path),
                container_path: Some(container_path.to_string()),
                format: Some("json".to_string()),
            })
        }
        "workspace_diff" => {
            let patch_path = capture_candidate_workspace_patch(
                docker.ok_or_else(|| {
                    anyhow!("workspace_diff capture requires a container runtime")
                })?,
                handle.ok_or_else(|| {
                    anyhow!("workspace_diff capture requires a container runtime")
                })?,
                request,
                trial_dir,
                timeout_ms,
            )?;
            if let Some(path) = patch_path.as_ref() {
                enforce_inline_capture_size(path, "workspace_diff runtime output")?;
            }
            let patch_text = patch_path
                .as_ref()
                .map(fs::read_to_string)
                .transpose()?
                .unwrap_or_default();
            Ok(CapturedTransportOutput {
                value: json!({
                    "patch": patch_text,
                    "path": "/agentlab/out/candidate.patch"
                }),
                host_path: patch_path,
                container_path: Some("/agentlab/out/candidate.patch".to_string()),
                format: Some("unified_diff".to_string()),
            })
        }
        other => Err(anyhow!(
            "{}.capture.type '{}' is not executable",
            label,
            other
        )),
    }
}

fn capture_agent_transport_outputs(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    timeout_ms: u64,
) -> Result<BTreeMap<String, CapturedTransportOutput>> {
    let outputs = parse_agent_outputs(request)?;
    outputs
        .iter()
        .map(|(id, output)| {
            let captured = capture_runtime_output(
                Some(docker),
                Some(handle),
                request,
                trial_dir,
                &format!("agent.{}", id),
                output,
                timeout_ms,
            )?;
            Ok((id.clone(), captured))
        })
        .collect()
}

fn select_transport_source(
    source: &RuntimeTransportSourceConfig,
    agent_outputs: &BTreeMap<String, CapturedTransportOutput>,
    task_payload: &Value,
) -> Option<Value> {
    if let Some(output) = source.output.as_deref() {
        let output_id = output.strip_prefix("agent.").unwrap_or(output);
        let captured = agent_outputs.get(output_id)?;
        let value = if let Some(field) = source.field.as_deref() {
            select_transport_field(&captured.value, field)?
        } else {
            captured.value.clone()
        };
        return Some(value);
    }
    if let Some(case_field) = source.case.as_deref().or(source.task.as_deref()) {
        return select_transport_field(task_payload, case_field);
    }
    if let Some(object) = source.object.as_ref() {
        let mut mapped = serde_json::Map::new();
        for (key, nested) in object {
            mapped.insert(
                key.clone(),
                select_transport_source(nested, agent_outputs, task_payload).unwrap_or(Value::Null),
            );
        }
        return Some(Value::Object(mapped));
    }
    None
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

fn materialize_container_file(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    input_id: &str,
    target_container_path: &str,
    bytes: &[u8],
    timeout_ms: u64,
) -> Result<()> {
    if let Ok(host_path) = map_container_path_to_host(target_container_path, request.trial_paths) {
        return write_host_transport_file(&host_path, bytes);
    }

    let staged_host_path = request
        .trial_paths
        .out
        .join("transport")
        .join("grader_inputs")
        .join(sanitize_for_fs(input_id));
    write_host_transport_file(&staged_host_path, bytes)?;
    let staged_container_path = format!(
        "/agentlab/out/transport/grader_inputs/{}",
        sanitize_for_fs(input_id)
    );
    let log_dir = trial_dir.join("logs").join("transport");
    ensure_dir(&log_dir)?;
    let exec = docker.exec(
        handle,
        &ExecSpec {
            command: vec![
                "sh".to_string(),
                "-lc".to_string(),
                "mkdir -p \"$(dirname \"$2\")\" && cp \"$1\" \"$2\"".to_string(),
                "sh".to_string(),
                staged_container_path,
                target_container_path.to_string(),
            ],
            env: BTreeMap::new(),
            workdir: None,
        },
    )?;
    let stream = docker.stream_exec_output(
        &exec,
        &log_dir.join(format!(
            "{}_materialize_stdout.log",
            sanitize_for_fs(input_id)
        )),
        &log_dir.join(format!(
            "{}_materialize_stderr.log",
            sanitize_for_fs(input_id)
        )),
        Some(Duration::from_millis(timeout_ms.max(1_000))),
    )?;
    let status = docker
        .wait_exec(&exec)
        .unwrap_or(crate::backend::docker::ExecStatus { exit_code: None });
    if stream.timed_out || status.exit_code != Some(0) {
        return Err(anyhow!(
            "failed to materialize grader input '{}' at {}",
            input_id,
            target_container_path
        ));
    }
    Ok(())
}

fn materialize_grader_inputs(
    docker: Option<&DockerRuntime>,
    handle: Option<&ContainerHandle>,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    grader: &BenchmarkGraderConfig,
    agent_outputs: &BTreeMap<String, CapturedTransportOutput>,
    task_payload: &Value,
    timeout_ms: u64,
) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for (id, input) in &grader.inputs {
        let value = select_transport_source(&input.source, agent_outputs, task_payload);
        let Some(value) = value else {
            if input.required {
                return Err(anyhow!("required grader input '{}' resolved to null", id));
            }
            continue;
        };
        if value.is_null() && input.required {
            return Err(anyhow!("required grader input '{}' resolved to null", id));
        }
        match input.materialize.as_kind.as_str() {
            "file" | "json_file" => {
                let target =
                    input.materialize.path.as_deref().ok_or_else(|| {
                        anyhow!("grader input '{}'.materialize.path is required", id)
                    })?;
                let bytes =
                    transport_value_to_bytes(&value, input.materialize.as_kind == "json_file")?;
                if let (Some(docker), Some(handle)) = (docker, handle) {
                    materialize_container_file(
                        docker, handle, request, trial_dir, id, target, &bytes, timeout_ms,
                    )?;
                } else {
                    let host_path = map_container_path_to_host(target, request.trial_paths)
                        .unwrap_or_else(|_| PathBuf::from(target));
                    write_host_transport_file(&host_path, &bytes)?;
                }
            }
            "env" => {
                let name =
                    input.materialize.name.as_deref().ok_or_else(|| {
                        anyhow!("grader input '{}'.materialize.name is required", id)
                    })?;
                let text = value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string());
                env.insert(name.to_string(), text);
            }
            other => {
                return Err(anyhow!(
                    "grader input '{}'.materialize.as '{}' is not executable",
                    id,
                    other
                ))
            }
        }
    }
    Ok(env)
}

fn capture_grader_transport_outputs(
    docker: Option<&DockerRuntime>,
    handle: Option<&ContainerHandle>,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    grader: &BenchmarkGraderConfig,
    timeout_ms: u64,
) -> Result<BTreeMap<String, CapturedTransportOutput>> {
    grader
        .outputs
        .iter()
        .map(|(id, output)| {
            let captured = capture_runtime_output(
                docker,
                handle,
                request,
                trial_dir,
                &format!("grader.{}", id),
                output,
                timeout_ms,
            )?;
            Ok((id.clone(), captured))
        })
        .collect()
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
        .pointer("/task")
        .and_then(|task| select_transport_field(task, source))
        .or_else(|| select_transport_field(trial_input, source))
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
    let Some(items) = value.as_array() else {
        return Err(anyhow!(
            "metric '{}' source.transform.test_ids.source.task must resolve to an array",
            metric.id
        ));
    };
    Ok(items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect())
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
    request: &AdapterRunRequest<'_>,
    grader: &BenchmarkGraderConfig,
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
            captured
                .value
                .pointer(pointer)
                .cloned()
                .unwrap_or(Value::Null)
        } else {
            captured.value.clone()
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
    let mut row = json!({
        "schema_version": "trial_conclusion_v1",
        "payload": Value::Object(payload),
        "reported_outcome": reported_outcome,
        "grader": {
            "name": "runtime_transport",
            "strategy": strategy
        }
    });
    if let Some(primary_metric) = primary_metric {
        row.as_object_mut()
            .expect("trial conclusion row object")
            .insert("primary_metric".to_string(), primary_metric);
    }
    Ok(row)
}

fn write_transport_envelope(
    request: &AdapterRunRequest<'_>,
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
    Ok(CapturedTransportOutput {
        value: value.get("value").cloned().unwrap_or(Value::Null),
        host_path: value
            .get("host_path")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        container_path: value
            .get("container_path")
            .and_then(Value::as_str)
            .map(str::to_string),
        format: value
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

fn run_container_grader(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    request: &AdapterRunRequest<'_>,
    resolved: &ResolvedGradingPhase,
    agent_exit_status: &str,
    transport_env: &BTreeMap<String, String>,
    trial_dir: &Path,
    timeout_ms: u64,
) -> Result<GraderRunOutcome> {
    let mut env = build_exec_env(
        request,
        &resolved.workdir,
        Some((AGENTLAB_ENV_AGENT_EXIT_STATUS, agent_exit_status)),
        false,
    );
    env.extend(transport_env.clone());
    extend_with_sidecar_env(&mut env, request, "grader")?;
    let grader_exec = docker.exec(
        handle,
        &ExecSpec {
            command: resolved.command.clone(),
            env,
            workdir: Some(resolved.workdir.clone()),
        },
    )?;
    let grader_stream = docker.stream_exec_output(
        &grader_exec,
        &trial_grader_stdout_path(trial_dir),
        &trial_grader_stderr_path(trial_dir),
        Some(Duration::from_millis(timeout_ms)),
    )?;
    let grader_status = docker
        .wait_exec(&grader_exec)
        .unwrap_or(crate::backend::docker::ExecStatus { exit_code: None });
    Ok(GraderRunOutcome {
        exit_code: grader_status.exit_code,
        signal: if grader_stream.timed_out {
            Some("KILL".to_string())
        } else {
            None
        },
        timed_out: grader_stream.timed_out,
    })
}

pub(crate) fn run_host_grader(
    request: &AdapterRunRequest<'_>,
    resolved: &ResolvedGradingPhase,
    agent_exit_status: &str,
    transport_env: &BTreeMap<String, String>,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<GraderRunOutcome> {
    if resolved.command.is_empty() {
        return Err(anyhow!("host benchmark grader command is empty"));
    }
    let _permit = acquire_host_grader_concurrency_permit();
    let mut command = Command::new(&resolved.command[0]);
    command.args(&resolved.command[1..]);
    command.current_dir(&resolved.workdir);
    let mut env = build_exec_env(
        request,
        &resolved.workdir,
        Some((AGENTLAB_ENV_AGENT_EXIT_STATUS, agent_exit_status)),
        false,
    );
    env.insert("WORKSPACE".to_string(), request.task_workdir.to_string());
    env.insert(
        AGENTLAB_ENV_RESULT_PATH.to_string(),
        request.io_paths.result_host.to_string_lossy().to_string(),
    );
    env.insert(
        AGENTLAB_ENV_MAPPED_GRADER_OUTPUT_PATH.to_string(),
        request
            .trial_paths
            .out
            .join(MAPPED_GRADER_OUTPUT_FILENAME)
            .to_string_lossy()
            .to_string(),
    );
    env.insert(
        "AGENTLAB_CONTRACT_IN_HOST".to_string(),
        request.trial_paths.in_dir.to_string_lossy().to_string(),
    );
    env.insert(
        "AGENTLAB_CONTRACT_OUT_HOST".to_string(),
        request.trial_paths.out.to_string_lossy().to_string(),
    );
    env.insert(
        "AGENTLAB_TASK_WORKDIR".to_string(),
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

fn signal_from_status(status: ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        return status.signal().map(|signal| signal.to_string());
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
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
        SqliteRunStore::open(run_dir)
            .and_then(|mut store| store.upsert_trial_attempt_state(run_id, &trial_id, state))
    }
    .with_context(|| {
        format!(
            "persist trial runtime state in sqlite for run {} trial {}",
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
) {
    if !matches!(
        state.phase,
        TrialPhase::Committed | TrialPhase::Paused | TrialPhase::Killed
    ) {
        state.phase = TrialPhase::Abandoned;
        state.paused_from_phase = None;
        let _ = persist_attempt_state(run_dir, run_id, trial_dir, state);
    }
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
    request: &AdapterRunRequest<'_>,
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
    let ingest_events = event_sink.map(|sink| sink.ingest).unwrap_or(true);
    if retain_raw_events {
        outcome.events = local_blob_if_present(request.io_paths.events_host.clone());
    }
    if ingest_events && request.io_paths.events_host.exists() {
        outcome.event_rows = load_event_rows(
            &request.io_paths.events_host,
            request.run_id,
            trial_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("trial"),
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
        Some(EvidenceBlobRef::RemoteRef {
            uri,
            digest: None,
            size_bytes: path.metadata().ok().map(|meta| meta.len()),
            media_type: None,
        })
    } else {
        None
    }
}

fn execute_host_agent_runtime(
    trial_dir: &Path,
    schedule_idx: usize,
    attempt_no: u32,
    request: &AdapterRunRequest<'_>,
    task_id: &str,
    variant_id: &str,
    repl_idx: usize,
) -> Result<TrialRuntimeOutcome> {
    let runtime_started_at = Instant::now();
    let mut attempt_state = new_trial_attempt_state(
        trial_dir,
        schedule_idx,
        attempt_no,
        task_id,
        variant_id,
        repl_idx,
        &request.trial_paths.in_dir,
        &request.trial_paths.out,
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
        AGENTLAB_ENV_TRIAL_INPUT_PATH.to_string(),
        request
            .io_paths
            .trial_input_host
            .to_string_lossy()
            .to_string(),
    );
    env.insert(
        AGENTLAB_ENV_RESULT_PATH.to_string(),
        request.io_paths.result_host.to_string_lossy().to_string(),
    );
    env.insert(
        AGENTLAB_ENV_MAPPED_GRADER_OUTPUT_PATH.to_string(),
        request
            .trial_paths
            .out
            .join(MAPPED_GRADER_OUTPUT_FILENAME)
            .to_string_lossy()
            .to_string(),
    );
    env.insert(
        AGENTLAB_ENV_TRAJECTORY_PATH.to_string(),
        request
            .trial_paths
            .runtime
            .trajectory
            .to_string_lossy()
            .to_string(),
    );

    let started_at = Utc::now().to_rfc3339();
    let agent_run_started_at = Instant::now();
    let output = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(&request.trial_paths.workspace)
        .envs(&env)
        .output()?;
    crate::perf::record_duration(
        request.package_root,
        request.run_id,
        trial_dir.file_name().and_then(|name| name.to_str()),
        Some(schedule_idx),
        Some(attempt_no as usize),
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
        classify_contract_file_state(&request.io_paths.result_host, result_parse_error.as_deref());
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
            agent_exit_status: output
                .status
                .code()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "signal".to_string()),
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
        request.package_root,
        request.run_id,
        trial_dir.file_name().and_then(|name| name.to_str()),
        Some(schedule_idx),
        Some(attempt_no as usize),
        "trial_runtime_total",
        runtime_started_at,
        json!({ "agent_site": "host" }),
    )?;
    Ok(outcome)
}

fn execute_modal_trial_runtime(
    execution_request: TrialRuntimeExecutionRequest<'_>,
    backend: &ModalExecutionBackend,
) -> Result<TrialRuntimeOutcome> {
    let TrialRuntimeExecutionRequest {
        trial_dir,
        schedule_idx,
        attempt_no,
        adapter: request,
        task_id,
        variant_id,
        repl_idx,
        task_sandbox_plan,
    } = execution_request;
    validate_modal_execution_request(request, task_sandbox_plan)?;
    let _active_sandbox_permit = acquire_modal_active_sandbox_permit(request)?;
    let trial_id = trial_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("trial");
    let sync = S3CompatibleRuntimeSync::from_env(request.run_id, trial_id, attempt_no)?;
    debug_assert_eq!(sync.kind(), RuntimeSyncKind::S3Compatible);
    let command = resolve_runtime_agent_command(request)?;
    if command.is_empty() {
        return Err(anyhow!("trial_runtime.agent.command must not be empty"));
    }

    let mut attempt_state = new_trial_attempt_state(
        trial_dir,
        schedule_idx,
        attempt_no,
        task_id,
        variant_id,
        repl_idx,
        &request.trial_paths.in_dir,
        &request.trial_paths.out,
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
        TrialPhase::AgentMaterializing,
    )?;
    let modal_grading = build_modal_grading_launch_spec(request, trial_dir, task_sandbox_plan)?;
    let launch = build_modal_launch_spec(
        backend,
        &sync,
        request,
        trial_dir,
        task_sandbox_plan,
        command,
        modal_grading.as_ref(),
    )?;
    set_attempt_phase(
        request.package_root,
        request.run_id,
        trial_dir,
        &mut attempt_state,
        TrialPhase::AgentRunning,
    )?;
    let launch_started_at = Instant::now();
    let modal_result = run_modal_launch(&backend.python, trial_dir, &launch)?;
    crate::perf::record_duration(
        request.package_root,
        request.run_id,
        Some(trial_id),
        Some(schedule_idx),
        Some(attempt_no as usize),
        "modal_sandbox_run",
        launch_started_at,
        json!({
            "sandbox_id": modal_result.sandbox_id.as_deref(),
            "process_id": modal_result.process_id.as_deref(),
            "exit_code": modal_result.exit_code,
            "timed_out": modal_result.timed_out,
            "sync": sync.kind_label(),
        }),
    )?;

    let task_sandbox = TaskSandboxState {
        container_id: modal_result
            .sandbox_id
            .clone()
            .unwrap_or_else(|| "modal_sandbox".to_string()),
        image: task_sandbox_plan.image.clone(),
        workdir: task_sandbox_plan.workdir.clone(),
        platform: task_sandbox_plan.platform.clone(),
        materialization: task_sandbox_plan.materialization.clone(),
    };
    attempt_state.task_sandbox = Some(task_sandbox);
    record_modal_sandbox_cleanup(&mut attempt_state, "task");

    let agent_response = load_agent_response_resilient(&request.io_paths.result_host)?;
    let trial_output = agent_response.response;
    let result_present = agent_response.result_present;
    let result_parse_error = agent_response.parse_error;
    let result_state =
        classify_contract_file_state(&request.io_paths.result_host, result_parse_error.as_deref());
    let candidate_artifact = extract_candidate_artifact_record(
        &trial_output,
        result_present,
        artifact_type_from_trial_input_path(&request.io_paths.trial_input_host)?,
    );
    let exit_code = modal_result.exit_code;
    let agent_exit_status = if modal_result.timed_out {
        "timeout".to_string()
    } else {
        exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "signal".to_string())
    };
    attempt_state.agent_phase = Some(AgentPhaseRecord {
        started_at: modal_result
            .started_at
            .clone()
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        ended_at: modal_result
            .ended_at
            .clone()
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        exit_code,
        signal: if modal_result.timed_out {
            Some("KILL".to_string())
        } else {
            None
        },
        timed_out: modal_result.timed_out,
        result_state,
        stdout_path: trial_agent_stdout_path(trial_dir)
            .to_string_lossy()
            .to_string(),
        stderr_path: trial_agent_stderr_path(trial_dir)
            .to_string_lossy()
            .to_string(),
    });
    attempt_state.candidate_artifact = Some(candidate_artifact);
    set_attempt_phase(
        request.package_root,
        request.run_id,
        trial_dir,
        &mut attempt_state,
        TrialPhase::AgentFinished,
    )?;

    let mut grading_outcome = GradingStageOutcome {
        trial_conclusion_row: None,
        deferred_trial_conclusion_records: Vec::new(),
        grade_error_reason: None,
    };
    if let Some(modal_grading) = modal_grading.as_ref() {
        let grader_exec = modal_result
            .exec_phase("grader")
            .ok_or_else(|| anyhow!("modal sandbox launcher did not report grader exec result"))?;
        let grading_sandbox = GradingSandboxState {
            container_id: grader_exec
                .sandbox_id
                .clone()
                .or_else(|| modal_result.sandbox_id.clone())
                .unwrap_or_else(|| "modal_sandbox".to_string()),
            strategy: modal_grading.strategy.clone(),
            workdir: modal_grading.workdir.clone(),
        };
        attempt_state.grading_sandbox = Some(grading_sandbox);
        record_modal_sandbox_cleanup(&mut attempt_state, "grading");
        set_attempt_phase(
            request.package_root,
            request.run_id,
            trial_dir,
            &mut attempt_state,
            TrialPhase::GraderRunning,
        )?;
        let grader_run = GraderRunOutcome {
            exit_code: grader_exec.exit_code,
            signal: if grader_exec.timed_out {
                Some("KILL".to_string())
            } else {
                None
            },
            timed_out: grader_exec.timed_out,
        };
        attempt_state.grading_phase = Some(GradingPhaseRecord {
            started_at: grader_exec
                .started_at
                .clone()
                .unwrap_or_else(|| Utc::now().to_rfc3339()),
            ended_at: grader_exec
                .ended_at
                .clone()
                .unwrap_or_else(|| Utc::now().to_rfc3339()),
            exit_code: grader_run.exit_code,
            signal: grader_run.signal.clone(),
            timed_out: grader_run.timed_out,
            output_state: ContractFileState::Valid,
            stdout_path: trial_grader_stdout_path(trial_dir)
                .to_string_lossy()
                .to_string(),
            stderr_path: trial_grader_stderr_path(trial_dir)
                .to_string_lossy()
                .to_string(),
        });
        persist_attempt_state(
            request.package_root,
            request.run_id,
            trial_dir,
            &attempt_state,
        )?;
        let (agent_transport_outputs, grader_transport_outputs) = read_transport_envelope(
            &request
                .trial_paths
                .out
                .join("runtime_transport_envelope.json"),
        )?;
        write_transport_envelope(request, &agent_transport_outputs, &grader_transport_outputs)?;
        let grader = request
            .benchmark_grader
            .ok_or_else(|| anyhow!("benchmark grading enabled without grader config"))?;
        let synthesized = synthesize_grader_trial_conclusion(
            request,
            grader,
            &grader_transport_outputs,
            &grader_run,
        )?;
        let mapped_output_path = request.trial_paths.out.join(MAPPED_GRADER_OUTPUT_FILENAME);
        fs::write(
            &mapped_output_path,
            serde_json::to_vec_pretty(&synthesized)?,
        )?;
        match validate_json_schema("trial_conclusion_v1.jsonschema", &mapped_output_path) {
            Ok(row) => {
                grading_outcome
                    .deferred_trial_conclusion_records
                    .push(row.clone());
                grading_outcome.trial_conclusion_row = Some(row);
            }
            Err(err) => {
                grading_outcome.grade_error_reason =
                    Some(format!("mapped_grader_output_invalid: {}", err));
            }
        }
        set_attempt_phase(
            request.package_root,
            request.run_id,
            trial_dir,
            &mut attempt_state,
            TrialPhase::GraderMapping,
        )?;
    }

    let mut outcome = finalize_trial_runtime(
        trial_dir,
        request.package_root,
        request.run_id,
        &mut attempt_state,
        AgentStageOutcome {
            agent_exit_status,
            trial_output,
            result_present,
            result_parse_error,
        },
        grading_outcome,
    )?;
    outcome.executor = ExecutorKind::Modal;
    outcome.stdout = remote_blob_if_present(
        &trial_agent_stdout_path(trial_dir),
        sync.uri_for_contract_path(MODAL_STDOUT_CONTRACT_PATH),
    );
    outcome.stderr = remote_blob_if_present(
        &trial_agent_stderr_path(trial_dir),
        sync.uri_for_contract_path(MODAL_STDERR_CONTRACT_PATH),
    );

    let event_sink = request.runtime.event_sinks.first();
    let retain_raw_events = event_sink.map(|sink| sink.persist).unwrap_or(false);
    let ingest_events = event_sink.map(|sink| sink.ingest).unwrap_or(true);
    if retain_raw_events {
        outcome.events = remote_blob_if_present(
            &request.io_paths.events_host,
            sync.uri_for_contract_path(&request.io_paths.trajectory_path),
        );
    }
    if ingest_events && request.io_paths.events_host.exists() {
        outcome.event_rows = load_event_rows(
            &request.io_paths.events_host,
            request.run_id,
            trial_id,
            schedule_idx,
            variant_id,
            task_id,
            repl_idx,
        )?;
    }
    Ok(outcome)
}

const MODAL_STDOUT_CONTRACT_PATH: &str = "/agentlab/out/stdout.log";
const MODAL_STDERR_CONTRACT_PATH: &str = "/agentlab/out/stderr.log";

impl S3CompatibleRuntimeSync {
    fn kind_label(&self) -> &'static str {
        "s3_compatible"
    }
}

fn validate_modal_execution_request(
    request: &AdapterRunRequest<'_>,
    task_sandbox_plan: &TaskSandboxPlan,
) -> Result<()> {
    if !trial_sidecar_plans(request.runtime_experiment)?.is_empty() {
        return Err(anyhow!(
            "executor modal does not yet support trial_runtime sidecars"
        ));
    }
    if request
        .runtime_experiment
        .pointer("/trial_runtime/execution/agent_site")
        .and_then(Value::as_str)
        == Some("host")
    {
        return Err(anyhow!(
            "executor modal does not support trial_runtime.execution.agent_site=host"
        ));
    }
    if !matches!(
        request.task_materialization_kind,
        TaskMaterializationKind::TaskImage
    ) {
        return Err(anyhow!(
            "executor modal currently supports only task-image materialization"
        ));
    }
    if !task_sandbox_plan.case_materialization.is_empty() {
        return Err(anyhow!(
            "executor modal does not yet support case materialization steps"
        ));
    }
    Ok(())
}

pub(crate) fn record_modal_sandbox_cleanup(attempt_state: &mut TrialAttemptState, role: &str) {
    let container_id = match role {
        "task" => attempt_state
            .task_sandbox
            .as_ref()
            .map(|sandbox| sandbox.container_id.clone()),
        "grading" => attempt_state
            .grading_sandbox
            .as_ref()
            .map(|sandbox| sandbox.container_id.clone()),
        _ => None,
    };
    let Some(container_id) = container_id else {
        return;
    };
    if attempt_state.cleanup.containers.iter().any(|record| {
        record.role == role
            && record.container_id == container_id
            && matches!(record.status.as_str(), "removed" | "killed")
    }) {
        return;
    }
    attempt_state
        .cleanup
        .containers
        .push(ContainerCleanupRecord {
            container_id,
            role: role.to_string(),
            status: "removed".to_string(),
            error: None,
        });
}

struct ModalLaunchSpec {
    value: Value,
}

pub(crate) struct ModalSandboxResult {
    pub(crate) sandbox_id: Option<String>,
    pub(crate) process_id: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) started_at: Option<String>,
    pub(crate) ended_at: Option<String>,
    execs: Vec<ModalExecPhaseResult>,
}

#[derive(Debug, Clone)]
struct ModalExecPhaseResult {
    phase: Option<String>,
    sandbox_id: Option<String>,
    #[allow(dead_code)]
    process_id: Option<String>,
    exit_code: Option<i32>,
    timed_out: bool,
    started_at: Option<String>,
    ended_at: Option<String>,
}

impl ModalSandboxResult {
    fn exec_phase(&self, phase: &str) -> Option<&ModalExecPhaseResult> {
        self.execs
            .iter()
            .find(|exec| exec.phase.as_deref() == Some(phase))
    }
}

struct ModalGradingLaunchSpec {
    value: Value,
    strategy: GradingStrategy,
    workdir: String,
}

fn modal_local_path_for_capture(
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    output: &RuntimeOutputConfig,
) -> Option<PathBuf> {
    if output.capture.capture_type == "workspace_diff" {
        return Some(request.trial_paths.out.join("candidate.patch"));
    }
    let path = output.capture.path.as_deref()?;
    map_container_path_to_host(path, request.trial_paths)
        .ok()
        .or_else(|| {
            let extension = Path::new(path)
                .extension()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .unwrap_or("out");
            Some(
                trial_dir
                    .join("out")
                    .join("transport")
                    .join("captured")
                    .join(format!("{}.{}", sanitize_for_fs(path), extension)),
            )
        })
}

fn modal_output_config_value(
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    output: &RuntimeOutputConfig,
) -> Result<Value> {
    let mut value = serde_json::to_value(output)?;
    if let Some(local_path) = modal_local_path_for_capture(request, trial_dir, output) {
        if let Some(capture) = value.get_mut("capture").and_then(Value::as_object_mut) {
            capture.insert(
                "local_path".to_string(),
                json!(local_path.to_string_lossy().to_string()),
            );
        }
    }
    Ok(value)
}

fn modal_output_map_value(
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    outputs: &BTreeMap<String, RuntimeOutputConfig>,
) -> Result<Value> {
    let mut object = serde_json::Map::new();
    for (id, output) in outputs {
        object.insert(
            id.clone(),
            modal_output_config_value(request, trial_dir, output)?,
        );
    }
    Ok(Value::Object(object))
}

fn build_modal_grading_launch_spec(
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    task_sandbox_plan: &TaskSandboxPlan,
) -> Result<Option<ModalGradingLaunchSpec>> {
    if !request.benchmark_grading_enabled {
        return Ok(None);
    }
    let grader = request
        .benchmark_grader
        .ok_or_else(|| anyhow!("benchmark grading enabled without grader config"))?;
    let Some(grader_command) = resolve_benchmark_grader_command(request)? else {
        return Err(anyhow!(
            "benchmark grading is mandatory but no grader command resolved for this trial"
        ));
    };
    if matches!(
        grader.strategy,
        GradingStrategy::None | GradingStrategy::Host
    ) {
        return Err(anyhow!(
            "executor modal does not support benchmark grading strategy '{}'",
            grading_strategy_name(&grader.strategy)
        ));
    }
    let resolved = resolve_grading_phase(request, grader, &grader_command)?;
    let grading_plan = build_grading_sandbox_plan(grader, &resolved)?;
    let agent_outputs = parse_agent_outputs(request)?;
    let hidden_assets = build_hidden_asset_bindings(grader)?
        .iter()
        .map(|binding| {
            json!({
                "hidden_path": binding.hidden_path,
                "revealed_path": binding.revealed_path,
                "stash_path": binding.stash_container_path,
            })
        })
        .collect::<Vec<_>>();
    let injected = if matches!(grader.strategy, GradingStrategy::Injected) {
        let source = resolved
            .injected_bundle_host_path
            .as_ref()
            .ok_or_else(|| anyhow!("injected grading missing resolved bundle host path"))?;
        if agent_artifact_archive_flag(source).is_some() {
            validate_agent_artifact_archive(source)?;
        }
        Some(json!({
            "source_remote_path": INJECTED_BUNDLE_SOURCE_MOUNT_PATH,
            "copy_dest": resolved.injected_copy_dest.as_deref().ok_or_else(|| {
                anyhow!("injected grading missing copy destination")
            })?,
            "source_is_dir": source.is_dir(),
            "archive_flag": agent_artifact_archive_flag(source),
        }))
    } else {
        None
    };
    let mut env = build_exec_env(
        request,
        &resolved.workdir,
        Some((
            AGENTLAB_ENV_AGENT_EXIT_STATUS,
            "__AGENTLAB_AGENT_EXIT_STATUS__",
        )),
        false,
    );
    env.insert(
        AGENTLAB_ENV_RESULT_PATH.to_string(),
        request.io_paths.result_path.clone(),
    );
    env.insert(
        AGENTLAB_ENV_MAPPED_GRADER_OUTPUT_PATH.to_string(),
        request.io_paths.mapped_grader_output_path.clone(),
    );
    env.insert(
        AGENTLAB_ENV_TRAJECTORY_PATH.to_string(),
        request.io_paths.trajectory_path.clone(),
    );
    let timeout_secs = ((task_sandbox_plan.time_limit_ms + 999) / 1000)
        .max(1)
        .saturating_add(30);
    Ok(Some(ModalGradingLaunchSpec {
        strategy: grader.strategy.clone(),
        workdir: resolved.workdir.clone(),
        value: json!({
            "strategy": grading_strategy_name(&grader.strategy),
            "image": resolved.image,
            "workdir": resolved.workdir,
            "command": resolved.command,
            "env": env,
            "timeout_seconds": timeout_secs,
            "stdout": {
                "remote_path": "/agentlab/out/grader_stdout.log",
                "local_path": trial_grader_stdout_path(trial_dir),
            },
            "stderr": {
                "remote_path": "/agentlab/out/grader_stderr.log",
                "local_path": trial_grader_stderr_path(trial_dir),
            },
            "agent_outputs": modal_output_map_value(request, trial_dir, &agent_outputs)?,
            "inputs": grader.inputs,
            "outputs": modal_output_map_value(request, trial_dir, &grader.outputs)?,
            "hidden_assets": hidden_assets,
            "injected": injected,
            "sandbox": if matches!(grader.strategy, GradingStrategy::Separate) {
                "separate"
            } else {
                "task"
            },
            "plan": serde_json::to_value(&grading_plan)?,
        }),
    }))
}

fn build_modal_launch_spec(
    backend: &ModalExecutionBackend,
    sync: &S3CompatibleRuntimeSync,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    task_sandbox_plan: &TaskSandboxPlan,
    command: Vec<String>,
    grading: Option<&ModalGradingLaunchSpec>,
) -> Result<ModalLaunchSpec> {
    let mut env = build_exec_env(request, request.task_workdir, None, true);
    env.insert(
        AGENTLAB_ENV_TRIAL_INPUT_PATH.to_string(),
        request.io_paths.trial_input_path.clone(),
    );
    env.insert(
        AGENTLAB_ENV_RESULT_PATH.to_string(),
        request.io_paths.result_path.clone(),
    );
    env.insert(
        AGENTLAB_ENV_MAPPED_GRADER_OUTPUT_PATH.to_string(),
        request.io_paths.mapped_grader_output_path.clone(),
    );
    env.insert(
        AGENTLAB_ENV_TRAJECTORY_PATH.to_string(),
        request.io_paths.trajectory_path.clone(),
    );

    let mut immutable_assets = Vec::new();
    let mut copies = vec![
        json!({
            "local_path": request.trial_paths.in_dir,
            "remote_path": AGENTLAB_CONTRACT_IN_DIR,
        }),
        json!({
            "local_path": request.trial_paths.workspace,
            "remote_path": AGENTLAB_CONTRACT_WORKSPACE_DIR,
        }),
        json!({
            "local_path": request.trial_paths.state,
            "remote_path": "/agentlab/state",
        }),
    ];
    for mount in request.dynamic_mounts {
        if mount.read_only
            && (mount.mount_path == "/agentlab/case_assets"
                || mount.mount_path.starts_with("/agentlab/case_assets/"))
        {
            immutable_assets.push(json!({
                "local_path": mount.host_path,
                "remote_path": mount.mount_path,
                "source_is_dir": mount.host_path.is_dir(),
            }));
            continue;
        }
        copies.push(json!({
            "local_path": mount.host_path,
            "remote_path": mount.mount_path,
        }));
    }
    for mount in request.secret_file_mounts {
        copies.push(json!({
            "local_path": mount.source_from_host,
            "remote_path": mount.target_path,
        }));
        if let Some(cache) = mount.credential_cache.as_ref() {
            copies.push(json!({
                "local_path": cache.host_dir,
                "remote_path": cache.target_dir,
            }));
        }
    }
    if let Some(bundle) = request.agent_artifact {
        let mount_path = request.agent_artifact_mount_path.ok_or_else(|| {
            anyhow!("trial_runtime.agent.artifact.mount.path is required when artifact is set")
        })?;
        copies.push(json!({
            "local_path": resolve_agent_artifact_mount_dir(bundle)?,
            "remote_path": mount_path,
        }));
    }
    if let Some(grading) = grading {
        if let Some(source) = grading
            .value
            .pointer("/injected/source_remote_path")
            .and_then(Value::as_str)
        {
            let resolved = resolve_grading_phase(
                request,
                request
                    .benchmark_grader
                    .ok_or_else(|| anyhow!("benchmark grading enabled without grader config"))?,
                &resolve_benchmark_grader_command(request)?.ok_or_else(|| {
                    anyhow!("benchmark grading is mandatory but no grader command resolved")
                })?,
            )?;
            if let Some(local_path) = resolved.injected_bundle_host_path.as_ref() {
                copies.push(json!({
                    "local_path": local_path,
                    "remote_path": source,
                }));
            }
        }
    }
    validate_modal_copy_targets(&copies)?;

    let timeout_secs = ((task_sandbox_plan.time_limit_ms + 999) / 1000)
        .max(1)
        .saturating_add(30);
    Ok(ModalLaunchSpec {
        value: json!({
            "app_name": backend.app_name,
            "environment_name": backend.environment_name,
            "max_inline_capture_bytes": max_inline_capture_bytes()?,
            "image": task_sandbox_plan.image,
            "platform": task_sandbox_plan.platform,
            "workdir": request.task_workdir,
            "env": env,
            "block_network": request.network_mode == "none",
            "poll_interval_ms": 1000,
            "sandbox_timeout_seconds": timeout_secs.saturating_add(60),
            "execs": [{
                "phase": "agent",
                "command": command,
                "env": env,
                "workdir": request.task_workdir,
                "timeout_seconds": timeout_secs,
                "stdout": {
                    "remote_path": MODAL_STDOUT_CONTRACT_PATH,
                    "local_path": trial_agent_stdout_path(trial_dir),
                },
                "stderr": {
                    "remote_path": MODAL_STDERR_CONTRACT_PATH,
                    "local_path": trial_agent_stderr_path(trial_dir),
                },
            }],
            "sync": {
                "type": sync.kind_label(),
                "bucket": sync.bucket,
                "prefix": sync.prefix,
                "immutable_case_asset_prefix": sync.immutable_case_asset_prefix(request.package_root),
                "endpoint_url": sync.endpoint_url,
                "region": sync.region,
                "modal_secret_name": sync.modal_secret_name,
                "force_path_style": sync.force_path_style,
            },
            "immutable_assets": immutable_assets,
            "copies": copies,
            "result": {
                "remote_path": request.io_paths.result_path,
                "local_path": request.io_paths.result_host,
            },
            "trial_input": {
                "remote_path": request.io_paths.trial_input_path,
                "local_path": request.io_paths.trial_input_host,
            },
            "events": {
                "remote_path": request.io_paths.trajectory_path,
                "local_path": request.io_paths.events_host,
            },
            "transport_envelope": {
                "remote_path": "/agentlab/out/runtime_transport_envelope.json",
                "local_path": request.trial_paths.out.join("runtime_transport_envelope.json"),
            },
            "grader": grading.map(|value| value.value.clone()),
        }),
    })
}

#[cfg(test)]
pub(crate) fn modal_launch_spec_for_test(
    backend: &ModalExecutionBackend,
    sync: &S3CompatibleRuntimeSync,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    task_sandbox_plan: &TaskSandboxPlan,
    command: Vec<String>,
) -> Result<Value> {
    Ok(build_modal_launch_spec(
        backend,
        sync,
        request,
        trial_dir,
        task_sandbox_plan,
        command,
        None,
    )?
    .value)
}

#[cfg(test)]
pub(crate) fn modal_launch_spec_with_grading_for_test(
    backend: &ModalExecutionBackend,
    sync: &S3CompatibleRuntimeSync,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    task_sandbox_plan: &TaskSandboxPlan,
    command: Vec<String>,
) -> Result<Value> {
    let grading = build_modal_grading_launch_spec(request, trial_dir, task_sandbox_plan)?;
    Ok(build_modal_launch_spec(
        backend,
        sync,
        request,
        trial_dir,
        task_sandbox_plan,
        command,
        grading.as_ref(),
    )?
    .value)
}

fn run_modal_launch(
    python: &str,
    trial_dir: &Path,
    launch: &ModalLaunchSpec,
) -> Result<ModalSandboxResult> {
    let modal_dir = trial_dir.join("modal");
    ensure_dir(&modal_dir)?;
    let script_path = modal_dir.join("agentlab_modal_sandbox.py");
    let spec_path = modal_dir.join("launch.json");
    fs::write(&script_path, MODAL_SANDBOX_SCRIPT)?;
    fs::write(&spec_path, serde_json::to_vec_pretty(&launch.value)?)?;
    let mut command = Command::new(python);
    command.arg(&script_path).arg(&spec_path);
    let output = run_modal_launcher_command(command, &modal_dir, "sandbox")?;
    if !output.status.success() {
        return Err(anyhow!(
            "modal sandbox launcher failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            output.stdout_tail,
            output.stderr_tail
        ));
    }
    let marker = output
        .stdout_tail
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("AGENTLAB_MODAL_RESULT="))
        .ok_or_else(|| {
            anyhow!(
                "modal sandbox launcher did not emit AGENTLAB_MODAL_RESULT in {}",
                output.stdout_path.display()
            )
        })?;
    let value: Value = serde_json::from_str(marker)?;
    parse_modal_sandbox_result(&value)
}

fn parse_modal_sandbox_result(value: &Value) -> Result<ModalSandboxResult> {
    let exec_values = value.get("execs").and_then(Value::as_array);
    let exec_results = value
        .get("execs")
        .and_then(Value::as_array)
        .map(|execs| {
            execs
                .iter()
                .map(|exec| ModalExecPhaseResult {
                    phase: exec
                        .get("phase")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    sandbox_id: exec
                        .get("sandbox_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    process_id: exec
                        .get("process_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    exit_code: exec
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok()),
                    timed_out: exec
                        .get("timed_out")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    started_at: exec
                        .get("started_at")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    ended_at: exec
                        .get("ended_at")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let agent_exec = match exec_values {
        Some(execs) if execs.is_empty() => None,
        Some(execs) => Some(
            execs
                .iter()
                .find(|exec| exec.get("phase").and_then(Value::as_str) == Some("agent"))
                .ok_or_else(|| {
                    anyhow!("modal sandbox launcher did not report agent exec result")
                })?,
        ),
        None => None,
    };
    Ok(ModalSandboxResult {
        sandbox_id: value
            .get("sandbox_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        process_id: agent_exec
            .and_then(|exec| exec.get("process_id"))
            .and_then(Value::as_str)
            .map(str::to_string),
        exit_code: agent_exec
            .and_then(|exec| exec.get("exit_code"))
            .or_else(|| value.get("exit_code"))
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        timed_out: agent_exec
            .and_then(|exec| exec.get("timed_out"))
            .or_else(|| value.get("timed_out"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        started_at: agent_exec
            .and_then(|exec| exec.get("started_at"))
            .or_else(|| value.get("started_at"))
            .and_then(Value::as_str)
            .map(str::to_string),
        ended_at: agent_exec
            .and_then(|exec| exec.get("ended_at"))
            .or_else(|| value.get("ended_at"))
            .and_then(Value::as_str)
            .map(str::to_string),
        execs: exec_results,
    })
}

#[cfg(test)]
pub(crate) fn parse_modal_sandbox_result_for_test(value: &Value) -> Result<ModalSandboxResult> {
    parse_modal_sandbox_result(value)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

const MODAL_SANDBOX_SCRIPT: &str = r#"
import json
import os
import pathlib
import shlex
import sys
import traceback
from datetime import datetime, timezone

import modal


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def runtime_workers_path():
    return pathlib.Path(sys.argv[1]).parent / "runtime_workers.json"


def write_runtime_worker(role, sandbox):
    sandbox_id = getattr(sandbox, "object_id", None)
    if not sandbox_id:
        return
    path = runtime_workers_path()
    try:
        payload = json.loads(path.read_text()) if path.exists() else {"workers": []}
    except Exception:
        payload = {"workers": []}
    workers = payload.setdefault("workers", [])
    if not any(item.get("role") == role and item.get("sandbox_id") == sandbox_id for item in workers):
        workers.append({"role": role, "sandbox_id": sandbox_id, "recorded_at": utc_now()})
    path.write_text(json.dumps(payload, indent=2, sort_keys=True))


def required_env(name):
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"{name} is required for Modal S3-compatible sync")
    return value


def build_secret(sync):
    secret_name = sync.get("modal_secret_name")
    if secret_name:
        return modal.Secret.from_name(secret_name)
    data = {
        "AWS_ACCESS_KEY_ID": required_env("AWS_ACCESS_KEY_ID"),
        "AWS_SECRET_ACCESS_KEY": required_env("AWS_SECRET_ACCESS_KEY"),
    }
    if os.environ.get("AWS_SESSION_TOKEN"):
        data["AWS_SESSION_TOKEN"] = os.environ["AWS_SESSION_TOKEN"]
    if sync.get("region"):
        data["AWS_REGION"] = sync["region"]
    elif os.environ.get("AWS_REGION"):
        data["AWS_REGION"] = os.environ["AWS_REGION"]
    return modal.Secret.from_dict(data)


def build_bucket_mount(sync, key_prefix, read_only):
    return modal.CloudBucketMount(
        bucket_name=sync["bucket"],
        bucket_endpoint_url=sync.get("endpoint_url"),
        key_prefix=key_prefix,
        secret=build_secret(sync),
        read_only=read_only,
        force_path_style=bool(sync.get("force_path_style", False)),
    )


def app_lookup(app_name, environment_name):
    if environment_name:
        return modal.App.lookup(app_name, create_if_missing=True, environment_name=environment_name)
    return modal.App.lookup(app_name, create_if_missing=True)


def make_dir(fs, remote_path):
    try:
        fs.make_directory(remote_path, create_parents=True)
    except TypeError:
        fs.make_directory(remote_path)


def copy_path(fs, local_path, remote_path):
    local = pathlib.Path(local_path)
    if not local.exists():
        raise FileNotFoundError(str(local))
    if local.is_dir():
        make_dir(fs, remote_path)
        root = local.resolve()
        for path in local.rglob("*"):
            rel = path.relative_to(local).as_posix()
            dst = remote_path.rstrip("/") + "/" + rel
            if path.is_symlink():
                try:
                    path.resolve().relative_to(root)
                except ValueError:
                    raise RuntimeError(f"refusing to copy symlink outside directory artifact: {path}")
            if path.is_dir():
                make_dir(fs, dst)
            else:
                fs.copy_from_local(str(path), dst)
    else:
        parent = str(pathlib.PurePosixPath(remote_path).parent)
        if parent and parent != ".":
            make_dir(fs, parent)
        fs.copy_from_local(str(local), remote_path)


def copy_optional_to_local(fs, remote_path, local_path):
    try:
        pathlib.Path(local_path).parent.mkdir(parents=True, exist_ok=True)
        fs.copy_to_local(remote_path, local_path)
        return True
    except Exception:
        return False


def wait_process(process):
    try:
        exit_code = process.wait()
    except TypeError:
        process.wait()
        exit_code = process.returncode
    if exit_code is None:
        exit_code = process.returncode
    return exit_code


def run_process(sandbox, exec_spec, result, phase=None):
    exec_started_at = utc_now()
    process = sandbox.exec(
        *exec_spec["command"],
        env=exec_spec.get("env", {}),
        workdir=exec_spec.get("workdir"),
        timeout=int(exec_spec.get("timeout_seconds", 300)),
        text=True,
    )
    process_id = getattr(process, "object_id", None)
    stdout = process.stdout.read() or ""
    stderr = process.stderr.read() or ""
    exit_code = wait_process(process)
    if exec_spec.get("stdout"):
        pathlib.Path(exec_spec["stdout"]["local_path"]).parent.mkdir(parents=True, exist_ok=True)
        pathlib.Path(exec_spec["stdout"]["local_path"]).write_text(stdout)
        sandbox.filesystem.write_text(stdout, exec_spec["stdout"]["remote_path"])
    if exec_spec.get("stderr"):
        pathlib.Path(exec_spec["stderr"]["local_path"]).parent.mkdir(parents=True, exist_ok=True)
        pathlib.Path(exec_spec["stderr"]["local_path"]).write_text(stderr)
        sandbox.filesystem.write_text(stderr, exec_spec["stderr"]["remote_path"])
    record = {
        "phase": phase or exec_spec.get("phase"),
        "sandbox_id": getattr(sandbox, "object_id", None),
        "process_id": process_id,
        "exit_code": exit_code,
        "timed_out": False,
        "started_at": exec_started_at,
        "ended_at": utc_now(),
    }
    result["execs"].append(record)
    return record


def run_shell_checked(sandbox, label, script, workdir=None, timeout_seconds=300):
    spec = {
        "phase": label,
        "command": ["/bin/sh", "-lc", "set -e\n" + script],
        "env": {},
        "workdir": workdir,
        "timeout_seconds": timeout_seconds,
    }
    process = sandbox.exec(
        *spec["command"],
        env={},
        workdir=workdir,
        timeout=timeout_seconds,
        text=True,
    )
    stdout = process.stdout.read() or ""
    stderr = process.stderr.read() or ""
    exit_code = wait_process(process)
    if exit_code != 0:
        raise RuntimeError(
            f"modal sandbox command {label!r} failed with exit {exit_code}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        )


def file_exists(fs, path):
    try:
        fs.read_bytes(path)
        return True
    except Exception:
        return False


def immutable_asset_ready(fs, item):
    remote_path = item["remote_path"].rstrip("/")
    if item.get("source_is_dir"):
        return file_exists(fs, remote_path + "/.agentlab_asset_ready")
    return file_exists(fs, remote_path)


def stage_immutable_assets(app, spec, sync, writable_asset_mount):
    items = spec.get("immutable_assets") or []
    if not items:
        return
    stager = None
    try:
        stager = modal.Sandbox.create(
            app=app,
            image=modal.Image.from_registry(spec["image"]),
            volumes={"/agentlab/case_assets": writable_asset_mount},
            timeout=int(spec.get("sandbox_timeout_seconds", 3600)),
        )
        fs = stager.filesystem
        for item in items:
            if immutable_asset_ready(fs, item):
                continue
            copy_path(fs, item["local_path"], item["remote_path"])
            if item.get("source_is_dir"):
                fs.write_text("ok\n", item["remote_path"].rstrip("/") + "/.agentlab_asset_ready")
    finally:
        if stager is not None:
            try:
                stager.terminate()
            finally:
                stager.detach()


def ensure_inline_capture_size(label, path, data, max_inline_capture_bytes):
    if max_inline_capture_bytes is None:
        return
    if len(data) > max_inline_capture_bytes:
        raise RuntimeError(
            f"{label} capture at {path} is too large to inline: "
            f"bytes={len(data)} max={max_inline_capture_bytes}"
        )


def read_file_value(fs, path, fmt, label, max_inline_capture_bytes):
    data = fs.read_bytes(path)
    if fmt == "json":
        ensure_inline_capture_size(label, path, data, max_inline_capture_bytes)
        return json.loads(data.decode("utf-8"))
    if fmt == "text":
        ensure_inline_capture_size(label, path, data, max_inline_capture_bytes)
        return data.decode("utf-8")
    if fmt == "bytes":
        return {"path": path, "bytes": len(data)}
    raise RuntimeError(f"unsupported runtime output format {fmt!r}")


def select_field(value, field):
    if field is None or str(field).strip() == "":
        return value
    field = str(field).strip()
    current = value
    if field.startswith("/"):
        for part in field.split("/")[1:]:
            part = part.replace("~1", "/").replace("~0", "~")
            if isinstance(current, list):
                current = current[int(part)]
            else:
                current = current[part]
        return current
    for part in field.split("."):
        current = current[part]
    return current


def write_local_capture(capture, data):
    local_path = capture.get("local_path")
    if not local_path:
        return None
    local = pathlib.Path(local_path)
    local.parent.mkdir(parents=True, exist_ok=True)
    local.write_bytes(data)
    return str(local)


def capture_output(sandbox, label, output, workdir, timeout_seconds, max_inline_capture_bytes):
    fs = sandbox.filesystem
    capture = output["capture"]
    capture_type = capture["type"]
    if capture_type in ("file", "result_json"):
        path = capture["path"]
        required = bool(capture.get("required", capture_type == "result_json"))
        if not file_exists(fs, path):
            if required:
                raise RuntimeError(f"declared runtime output {label} missing at {path}")
            return {
                "value": None,
                "host_path": None,
                "container_path": path,
                "format": capture.get("format"),
            }
        data = fs.read_bytes(path)
        host_path = write_local_capture(capture, data)
        fmt = capture.get("format", "json" if capture_type == "result_json" else None)
        if capture_type == "result_json":
            ensure_inline_capture_size(label, path, data, max_inline_capture_bytes)
            result_json = json.loads(data.decode("utf-8"))
            value = {"value": select_field(result_json, capture["field"])} if capture.get("field") else result_json
            fmt = "json"
        else:
            value = read_file_value(fs, path, fmt, label, max_inline_capture_bytes)
        return {
            "value": value,
            "host_path": host_path,
            "container_path": path,
            "format": fmt,
        }
    if capture_type == "workspace_diff":
        patch_path = "/agentlab/out/candidate.patch"
        probe = sandbox.exec("git", "-C", workdir, "rev-parse", "--is-inside-work-tree", text=True)
        _ = probe.stdout.read()
        _ = probe.stderr.read()
        if wait_process(probe) != 0:
            patch_text = ""
        else:
            pathspec = ". ':(exclude).agentlab' ':(exclude).haiku' ':(exclude).lab' ':(exclude)logs' ':(exclude)out'"
            run_shell_checked(
                sandbox,
                "modal_workspace_diff_add",
                f"git -C {shlex.quote(workdir)} add -N -- {pathspec}",
                workdir=workdir,
                timeout_seconds=timeout_seconds,
            )
            diff = sandbox.exec(
                "/bin/sh",
                "-lc",
                f"git -C {shlex.quote(workdir)} diff --binary -- {pathspec}",
                workdir=workdir,
                timeout=timeout_seconds,
                text=True,
            )
            patch_text = diff.stdout.read() or ""
            _ = diff.stderr.read()
            if wait_process(diff) != 0:
                raise RuntimeError("failed to capture modal workspace diff")
            if max_inline_capture_bytes is not None and len(patch_text.encode("utf-8")) > max_inline_capture_bytes:
                raise RuntimeError(
                    f"{label} workspace_diff is too large to inline: "
                    f"bytes={len(patch_text.encode('utf-8'))} max={max_inline_capture_bytes}"
                )
        fs.write_text(patch_text, patch_path)
        host_path = write_local_capture(capture, patch_text.encode("utf-8"))
        return {
            "value": {"patch": patch_text, "path": patch_path},
            "host_path": host_path,
            "container_path": patch_path,
            "format": "unified_diff",
        }
    raise RuntimeError(f"{label}.capture.type {capture_type!r} is not executable")


def capture_outputs(sandbox, outputs, prefix, workdir, timeout_seconds, max_inline_capture_bytes):
    captured = {}
    for output_id, output in outputs.items():
        captured[output_id] = capture_output(
            sandbox,
            f"{prefix}.{output_id}",
            output,
            workdir,
            timeout_seconds,
            max_inline_capture_bytes,
        )
    return captured


def select_transport_source(source, agent_outputs, task_payload):
    if source.get("output"):
        output_id = source["output"].removeprefix("agent.")
        value = agent_outputs[output_id]["value"]
        return select_field(value, source.get("field")) if source.get("field") else value
    if source.get("case") or source.get("task"):
        return select_field(task_payload, source.get("case") or source["task"])
    if source.get("object"):
        return {
            key: select_transport_source(nested, agent_outputs, task_payload)
            for key, nested in source["object"].items()
        }
    return None


def value_to_bytes(value, json_mode):
    if json_mode:
        return json.dumps(value, indent=2, sort_keys=True).encode("utf-8")
    if isinstance(value, str):
        return value.encode("utf-8")
    return json.dumps(value, indent=2, sort_keys=True).encode("utf-8")


def materialize_grader_inputs(sandbox, grader, agent_outputs, task_payload):
    env = {}
    fs = sandbox.filesystem
    for input_id, input_spec in grader.get("inputs", {}).items():
        value = select_transport_source(input_spec["source"], agent_outputs, task_payload)
        if value is None:
            if input_spec.get("required"):
                raise RuntimeError(f"required grader input {input_id!r} resolved to null")
            continue
        materialize = input_spec["materialize"]
        kind = materialize["as"]
        if kind in ("file", "json_file"):
            path = materialize["path"]
            data = value_to_bytes(value, kind == "json_file")
            parent = str(pathlib.PurePosixPath(path).parent)
            if parent and parent != ".":
                make_dir(fs, parent)
            fs.write_bytes(data, path)
        elif kind == "env":
            env[materialize["name"]] = value if isinstance(value, str) else json.dumps(value)
        else:
            raise RuntimeError(f"grader input {input_id!r}.materialize.as {kind!r} is not executable")
    return env


def write_transport_envelope(fs, spec, agent_outputs, grader_outputs):
    envelope = {
        "schema_version": "runtime_transport_envelope_v1",
        "agent": {"outputs": agent_outputs},
        "grader": {"outputs": grader_outputs},
    }
    payload = json.dumps(envelope, indent=2, sort_keys=True)
    fs.write_text(payload, spec["transport_envelope"]["remote_path"])
    pathlib.Path(spec["transport_envelope"]["local_path"]).parent.mkdir(parents=True, exist_ok=True)
    pathlib.Path(spec["transport_envelope"]["local_path"]).write_text(payload)


def prepare_modal_grader(task_sandbox, grader):
    timeout_seconds = int(grader.get("timeout_seconds", 300))
    for binding in grader.get("hidden_assets", []):
        stash_parent = str(pathlib.PurePosixPath(binding["stash_path"]).parent)
        run_shell_checked(
            task_sandbox,
            "hide_hidden_asset",
            "mkdir -p {parent}\nrm -rf {stash}\nmv {hidden} {stash}".format(
                parent=shlex.quote(stash_parent or "/tmp"),
                stash=shlex.quote(binding["stash_path"]),
                hidden=shlex.quote(binding["hidden_path"]),
            ),
            timeout_seconds=timeout_seconds,
        )


def reveal_modal_grader_assets(task_sandbox, grader):
    timeout_seconds = int(grader.get("timeout_seconds", 300))
    for binding in grader.get("hidden_assets", []):
        parent = str(pathlib.PurePosixPath(binding["revealed_path"]).parent)
        run_shell_checked(
            task_sandbox,
            "reveal_hidden_asset",
            "mkdir -p {parent}\nrm -rf {revealed}\nmv {stash} {revealed}".format(
                parent=shlex.quote(parent or "/"),
                revealed=shlex.quote(binding["revealed_path"]),
                stash=shlex.quote(binding["stash_path"]),
            ),
            timeout_seconds=timeout_seconds,
        )
    injected = grader.get("injected")
    if injected:
        src = injected["source_remote_path"]
        dest = injected["copy_dest"]
        if injected.get("source_is_dir"):
            extract = f"cp -R {shlex.quote(src)}/. {shlex.quote(dest)}"
        elif injected.get("archive_flag"):
            extract = f"tar {shlex.quote(injected['archive_flag'])} {shlex.quote(src)} -C {shlex.quote(dest)}"
        else:
            extract = f"cp {shlex.quote(src)} {shlex.quote(dest)}/"
        run_shell_checked(
            task_sandbox,
            "injected_grader_bundle",
            f"mkdir -p {shlex.quote(dest)}\nfind {shlex.quote(dest)} -mindepth 1 -maxdepth 1 -exec rm -rf {{}} +\n{extract}",
            timeout_seconds=timeout_seconds,
        )


def create_sandbox(app, image_ref, sync, bucket_mount, case_assets_mount, spec, workdir):
    if case_assets_mount is None:
        volumes = {"/agentlab": bucket_mount}
    else:
        prefix = sync["prefix"].rstrip("/")
        volumes = {
            "/agentlab/in": build_bucket_mount(sync, prefix + "/in", read_only=False),
            "/agentlab/out": build_bucket_mount(sync, prefix + "/out", read_only=False),
            "/agentlab/state": build_bucket_mount(sync, prefix + "/state", read_only=False),
            "/agentlab/workspace": build_bucket_mount(sync, prefix + "/workspace", read_only=False),
            "/agentlab/tmp": build_bucket_mount(sync, prefix + "/tmp", read_only=False),
        }
    if case_assets_mount is not None:
        volumes["/agentlab/case_assets"] = case_assets_mount
    return modal.Sandbox.create(
        app=app,
        image=modal.Image.from_registry(image_ref),
        volumes=volumes,
        env=spec.get("env", {}),
        workdir=workdir,
        block_network=bool(spec.get("block_network", False)),
        timeout=int(spec.get("sandbox_timeout_seconds", 3600)),
    )


def main():
    spec = json.loads(pathlib.Path(sys.argv[1]).read_text())
    max_inline_capture_bytes = spec.get("max_inline_capture_bytes")
    if max_inline_capture_bytes is not None:
        max_inline_capture_bytes = int(max_inline_capture_bytes)
    sync = spec["sync"]
    app = app_lookup(spec["app_name"], spec.get("environment_name"))
    bucket_mount = build_bucket_mount(sync, sync["prefix"], read_only=False)
    case_assets_mount = None
    immutable_assets = spec.get("immutable_assets") or []
    if immutable_assets:
        writable_asset_mount = build_bucket_mount(
            sync,
            sync["immutable_case_asset_prefix"],
            read_only=False,
        )
        stage_immutable_assets(app, spec, sync, writable_asset_mount)
        case_assets_mount = build_bucket_mount(
            sync,
            sync["immutable_case_asset_prefix"],
            read_only=True,
        )
    sandbox = None
    grader_sandbox = None
    started_at = utc_now()
    ended_at = None
    exit_code = None
    timed_out = False
    result = {
        "sandbox_id": None,
        "execs": [],
        "exit_code": None,
        "timed_out": False,
        "started_at": started_at,
        "ended_at": None,
    }
    try:
        sandbox = create_sandbox(app, spec["image"], sync, bucket_mount, case_assets_mount, spec, spec.get("workdir"))
        result["sandbox_id"] = getattr(sandbox, "object_id", None)
        write_runtime_worker("task", sandbox)
        fs = sandbox.filesystem
        for path in ["/agentlab/in", "/agentlab/out", "/agentlab/state", "/agentlab/workspace", "/agentlab/tmp"]:
            make_dir(fs, path)
        for item in spec.get("copies", []):
            copy_path(fs, item["local_path"], item["remote_path"])
        if spec.get("grader"):
            prepare_modal_grader(sandbox, spec["grader"])
        for exec_spec in spec.get("execs", []):
            record = run_process(sandbox, exec_spec, result)
            exit_code = record["exit_code"]
        grader = spec.get("grader")
        if grader:
            task_payload = json.loads(fs.read_text(spec["trial_input"]["remote_path"]))
            agent_outputs = capture_outputs(
                sandbox,
                grader.get("agent_outputs", {}),
                "agent",
                spec.get("workdir"),
                int(grader.get("timeout_seconds", 300)),
                max_inline_capture_bytes,
            )
            reveal_modal_grader_assets(sandbox, grader)
            grader_sandbox = sandbox
            if grader.get("sandbox") == "separate":
                grader_sandbox = create_sandbox(app, grader["image"], sync, bucket_mount, case_assets_mount, spec, grader.get("workdir"))
                write_runtime_worker("grading", grader_sandbox)
            transport_env = materialize_grader_inputs(grader_sandbox, grader, agent_outputs, task_payload)
            grader_env = dict(grader.get("env", {}))
            agent_status = "timeout" if timed_out else str(exit_code) if exit_code is not None else "signal"
            for key, value in list(grader_env.items()):
                if value == "__AGENTLAB_AGENT_EXIT_STATUS__":
                    grader_env[key] = agent_status
            grader_env.update(transport_env)
            grader_exec = {
                "phase": "grader",
                "command": grader["command"],
                "env": grader_env,
                "workdir": grader.get("workdir"),
                "timeout_seconds": grader.get("timeout_seconds", 300),
                "stdout": grader["stdout"],
                "stderr": grader["stderr"],
            }
            run_process(grader_sandbox, grader_exec, result, phase="grader")
            grader_outputs = capture_outputs(
                grader_sandbox,
                grader.get("outputs", {}),
                "grader",
                grader.get("workdir"),
                int(grader.get("timeout_seconds", 300)),
                max_inline_capture_bytes,
            )
            write_transport_envelope(grader_sandbox.filesystem, spec, agent_outputs, grader_outputs)
    except Exception as exc:
        timed_out = "timeout" in type(exc).__name__.lower() or "timed out" in str(exc).lower()
        exec_specs = spec.get("execs") or [{}]
        stderr_path = exec_specs[-1].get("stderr", spec.get("stderr", {})).get("local_path")
        if stderr_path is None:
            stderr_path = pathlib.Path(sys.argv[1]).parent / "modal_launcher_stderr.log"
        pathlib.Path(stderr_path).parent.mkdir(parents=True, exist_ok=True)
        with pathlib.Path(stderr_path).open("a") as handle:
            handle.write("\n[agentlab modal launcher error]\n")
            handle.write("".join(traceback.format_exception(exc)))
        if not timed_out:
            raise
    finally:
        ended_at = utc_now()
        if sandbox is not None:
            fs = sandbox.filesystem
            copy_optional_to_local(fs, spec["result"]["remote_path"], spec["result"]["local_path"])
            copy_optional_to_local(fs, spec["events"]["remote_path"], spec["events"]["local_path"])
            transport_fs = grader_sandbox.filesystem if grader_sandbox is not None else fs
            copy_optional_to_local(transport_fs, spec["transport_envelope"]["remote_path"], spec["transport_envelope"]["local_path"])
            if spec.get("grader"):
                copy_optional_to_local(transport_fs, spec["grader"]["stdout"]["remote_path"], spec["grader"]["stdout"]["local_path"])
                copy_optional_to_local(transport_fs, spec["grader"]["stderr"]["remote_path"], spec["grader"]["stderr"]["local_path"])
        if grader_sandbox is not None and grader_sandbox is not sandbox:
            try:
                grader_sandbox.terminate()
            finally:
                grader_sandbox.detach()
        if sandbox is not None:
            try:
                sandbox.terminate()
            finally:
                sandbox.detach()
        result["exit_code"] = exit_code
        result["timed_out"] = timed_out
        result["ended_at"] = ended_at
        print("AGENTLAB_MODAL_RESULT=" + json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
"#;

const MODAL_CLEANUP_SCRIPT: &str = r#"
import json
import pathlib
import sys
import traceback

import modal


def is_not_found(exc):
    text = (type(exc).__name__ + " " + str(exc)).lower()
    return "notfound" in text or "not found" in text or "404" in text


def main():
    spec = json.loads(pathlib.Path(sys.argv[1]).read_text())
    results = []
    errors = []
    cleaned = 0
    for sandbox_id in spec.get("sandbox_ids", []):
        sandbox = None
        try:
            sandbox = modal.Sandbox.from_id(sandbox_id)
            sandbox.terminate()
            cleaned += 1
            results.append({"sandbox_id": sandbox_id, "status": "terminated"})
        except Exception as exc:
            if is_not_found(exc):
                cleaned += 1
                results.append({"sandbox_id": sandbox_id, "status": "not_found"})
            else:
                errors.append({
                    "sandbox_id": sandbox_id,
                    "error": "".join(traceback.format_exception(exc)),
                })
        finally:
            if sandbox is not None:
                try:
                    sandbox.detach()
                except Exception:
                    pass
    payload = {"cleaned": cleaned, "results": results, "errors": errors}
    print("AGENTLAB_MODAL_CLEANUP=" + json.dumps(payload, sort_keys=True))
    if errors:
        sys.exit(1)


if __name__ == "__main__":
    main()
"#;

#[cfg(test)]
pub(crate) fn modal_sandbox_script_for_test() -> &'static str {
    MODAL_SANDBOX_SCRIPT
}

#[cfg(test)]
pub(crate) fn modal_cleanup_script_for_test() -> &'static str {
    MODAL_CLEANUP_SCRIPT
}

#[cfg(test)]
pub(crate) fn modal_launcher_log_tail_bytes_for_test() -> u64 {
    MODAL_LAUNCHER_LOG_TAIL_BYTES
}

#[cfg(test)]
pub(crate) fn read_modal_launcher_log_tail_for_test(path: &Path) -> Result<String> {
    read_file_tail_lossy(path, MODAL_LAUNCHER_LOG_TAIL_BYTES)
}

#[cfg(test)]
pub(crate) fn read_captured_file_value_for_test(path: &Path, format: &str) -> Result<Value> {
    read_captured_file_value(path, format)
}

#[cfg(test)]
pub(crate) fn run_modal_launcher_command_for_test(
    command: Command,
    modal_dir: &Path,
    log_stem: &str,
) -> Result<(ExitStatus, String, String, PathBuf)> {
    let output = run_modal_launcher_command(command, modal_dir, log_stem)?;
    Ok((
        output.status,
        output.stdout_tail,
        output.stderr_tail,
        output.stdout_path,
    ))
}

fn execute_local_docker_trial_runtime<S>(
    execution_request: TrialRuntimeExecutionRequest<'_>,
    runtime_sync: &S,
) -> Result<TrialRuntimeOutcome>
where
    S: LocalContainerRuntimeSync,
{
    debug_assert_eq!(runtime_sync.kind(), RuntimeSyncKind::LocalBindMount);
    let TrialRuntimeExecutionRequest {
        trial_dir,
        schedule_idx,
        attempt_no,
        adapter: request,
        task_id,
        variant_id,
        repl_idx,
        task_sandbox_plan,
    } = execution_request;
    validate_benchmark_grading_contract(request)?;
    let trial_runtime_started_at = Instant::now();
    if request
        .runtime_experiment
        .pointer("/trial_runtime/execution/agent_site")
        .and_then(Value::as_str)
        == Some("host")
        && !request.benchmark_grading_enabled
    {
        let outcome = execute_host_agent_runtime(
            trial_dir,
            schedule_idx,
            attempt_no,
            request,
            task_id,
            variant_id,
            repl_idx,
        )?;
        return attach_local_runtime_evidence(
            outcome,
            request,
            trial_dir,
            schedule_idx,
            task_id,
            variant_id,
            repl_idx,
        );
    }
    let planned_container_units = planned_docker_active_container_units(request)?;
    let _active_container_permit =
        acquire_docker_active_container_units_permit(planned_container_units)?;
    let docker = DockerRuntime::connect()?;
    enforce_observed_docker_active_container_cap(&docker, planned_container_units)?;
    let ensure_image_started_at = Instant::now();
    docker.ensure_image_with_platform(
        &task_sandbox_plan.image,
        task_sandbox_plan.platform.as_deref(),
    )?;
    crate::perf::record_duration(
        request.package_root,
        request.run_id,
        trial_dir.file_name().and_then(|name| name.to_str()),
        Some(schedule_idx),
        Some(attempt_no as usize),
        "docker_ensure_task_image",
        ensure_image_started_at,
        json!({
            "image": task_sandbox_plan.image.as_str(),
            "platform": task_sandbox_plan.platform.as_deref()
        }),
    )?;
    let hidden_asset_bindings = request
        .benchmark_grader
        .map(build_hidden_asset_bindings)
        .transpose()?
        .unwrap_or_default();
    let injected_grading_phase = if request.benchmark_grading_enabled {
        if let Some(grader) = request.benchmark_grader {
            if matches!(grader.strategy, GradingStrategy::Injected) {
                resolve_benchmark_grader_command(request)?
                    .as_ref()
                    .map(|command| resolve_grading_phase(request, grader, command))
                    .transpose()?
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let mut attempt_state = new_trial_attempt_state(
        trial_dir,
        schedule_idx,
        attempt_no,
        task_id,
        variant_id,
        repl_idx,
        &request.trial_paths.in_dir,
        &request.trial_paths.out,
    );
    persist_attempt_state(
        request.package_root,
        request.run_id,
        trial_dir,
        &attempt_state,
    )?;

    let mut task_container: Option<ContainerHandle> = None;
    let mut grading_container: Option<ContainerHandle> = None;
    let mut ephemeral_containers: Vec<(RuntimeSidecarPlan, ContainerHandle)> = Vec::new();
    let ephemeral_network =
        create_trial_ephemeral_network(&docker, request, schedule_idx, attempt_no)?;
    if let Some(network) = ephemeral_network.as_ref() {
        attempt_state
            .ephemeral_networks
            .push(EphemeralNetworkState {
                name: network.name.clone(),
                internal: network.internal,
            });
        persist_attempt_state(
            request.package_root,
            request.run_id,
            trial_dir,
            &attempt_state,
        )?;
    }

    let execution = (|| -> Result<TrialRuntimeOutcome> {
        set_attempt_phase(
            request.package_root,
            request.run_id,
            trial_dir,
            &mut attempt_state,
            TrialPhase::AgentMaterializing,
        )?;

        let task_materialize_started_at = Instant::now();
        let task_handle = materialize_task_sandbox(
            &docker,
            runtime_sync,
            request,
            task_sandbox_plan,
            injected_grading_phase.as_ref(),
            ephemeral_network
                .as_ref()
                .map(|network| network.name.as_str()),
        )?;
        crate::perf::record_duration(
            request.package_root,
            request.run_id,
            trial_dir.file_name().and_then(|name| name.to_str()),
            Some(schedule_idx),
            Some(attempt_no as usize),
            "task_container_start",
            task_materialize_started_at,
            json!({
                "container_id": task_handle.container_id.as_str(),
                "image": task_sandbox_plan.image.as_str(),
                "platform": task_sandbox_plan.platform.as_deref()
            }),
        )?;
        crate::perf::record_container_stats(
            request.package_root,
            request.run_id,
            trial_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("trial"),
            schedule_idx,
            attempt_no as usize,
            "task_container_started_stats",
            &task_handle.container_id,
            "task",
        )?;
        if !hidden_asset_bindings.is_empty() {
            stash_hidden_assets(
                &docker,
                &task_handle,
                trial_dir,
                &hidden_asset_bindings,
                task_sandbox_plan.time_limit_ms,
            )?;
        }
        let task_sandbox = TaskSandboxState {
            container_id: task_handle.container_id.clone(),
            image: task_sandbox_plan.image.clone(),
            workdir: task_sandbox_plan.workdir.clone(),
            platform: task_sandbox_plan.platform.clone(),
            materialization: task_sandbox_plan.materialization.clone(),
        };
        attempt_state.task_sandbox = Some(task_sandbox.clone());
        persist_attempt_state(
            request.package_root,
            request.run_id,
            trial_dir,
            &attempt_state,
        )?;
        task_container = Some(task_handle.clone());
        run_case_materialization_steps(
            &docker,
            &task_handle,
            request,
            trial_dir,
            task_sandbox_plan,
        )?;
        if let Some(network) = ephemeral_network.as_ref() {
            let already_started = ephemeral_containers
                .iter()
                .map(|(plan, _)| plan.id.clone())
                .collect::<BTreeSet<_>>();
            ephemeral_containers.extend(start_trial_ephemerals(
                &docker,
                request,
                &network.name,
                &mut attempt_state,
                "agent",
                &already_started,
            )?);
            persist_attempt_state(
                request.package_root,
                request.run_id,
                trial_dir,
                &attempt_state,
            )?;
        }

        set_attempt_phase(
            request.package_root,
            request.run_id,
            trial_dir,
            &mut attempt_state,
            TrialPhase::AgentRunning,
        )?;

        let agent_started_at = Utc::now().to_rfc3339();
        let agent_exec_create_started_at = Instant::now();
        let mut agent_env = build_exec_env(request, request.task_workdir, None, true);
        extend_with_sidecar_env(&mut agent_env, request, "agent")?;
        let agent_exec = docker.exec(
            &task_handle,
            &ExecSpec {
                command: resolve_runtime_agent_command(request)?,
                env: agent_env,
                workdir: Some(request.task_workdir.to_string()),
            },
        )?;
        crate::perf::record_duration(
            request.package_root,
            request.run_id,
            trial_dir.file_name().and_then(|name| name.to_str()),
            Some(schedule_idx),
            Some(attempt_no as usize),
            "agent_exec_create",
            agent_exec_create_started_at,
            json!({ "container_id": task_handle.container_id.as_str() }),
        )?;
        let live_event_ingest = start_live_event_ingest(
            trial_dir,
            schedule_idx,
            attempt_no,
            request,
            task_id,
            variant_id,
            repl_idx,
        );
        let agent_run_started_at = Instant::now();
        let agent_stream_result = docker.stream_exec_output(
            &agent_exec,
            &trial_agent_stdout_path(trial_dir),
            &trial_agent_stderr_path(trial_dir),
            Some(Duration::from_millis(task_sandbox_plan.time_limit_ms)),
        );
        let agent_status = docker
            .wait_exec(&agent_exec)
            .unwrap_or(crate::backend::docker::ExecStatus { exit_code: None });
        let live_event_ingest_result = stop_live_event_ingest(live_event_ingest);
        let agent_stream = agent_stream_result?;
        live_event_ingest_result?;
        crate::perf::record_duration(
            request.package_root,
            request.run_id,
            trial_dir.file_name().and_then(|name| name.to_str()),
            Some(schedule_idx),
            Some(attempt_no as usize),
            "agent_exec_stream_wait",
            agent_run_started_at,
            json!({
                "container_id": task_handle.container_id.as_str(),
                "exit_code": agent_status.exit_code,
                "timed_out": agent_stream.timed_out
            }),
        )?;
        let agent_ended_at = Utc::now().to_rfc3339();

        let agent_output_parse_started_at = Instant::now();
        let agent_response = load_agent_response_resilient(&request.io_paths.result_host)?;
        let trial_output = agent_response.response;
        let result_present = agent_response.result_present;
        let result_parse_error = agent_response.parse_error;
        let result_state = classify_contract_file_state(
            &request.io_paths.result_host,
            result_parse_error.as_deref(),
        );

        let candidate_artifact = extract_candidate_artifact_record(
            &trial_output,
            result_present,
            artifact_type_from_trial_input_path(&request.io_paths.trial_input_host)?,
        );
        let agent_phase = AgentPhaseRecord {
            started_at: agent_started_at.clone(),
            ended_at: agent_ended_at.clone(),
            exit_code: agent_status.exit_code,
            signal: if agent_stream.timed_out {
                Some("KILL".to_string())
            } else {
                None
            },
            timed_out: agent_stream.timed_out,
            result_state,
            stdout_path: trial_agent_stdout_path(trial_dir)
                .to_string_lossy()
                .to_string(),
            stderr_path: trial_agent_stderr_path(trial_dir)
                .to_string_lossy()
                .to_string(),
        };
        attempt_state.agent_phase = Some(agent_phase);
        attempt_state.candidate_artifact = Some(candidate_artifact);
        set_attempt_phase(
            request.package_root,
            request.run_id,
            trial_dir,
            &mut attempt_state,
            TrialPhase::AgentFinished,
        )?;
        crate::perf::record_duration(
            request.package_root,
            request.run_id,
            trial_dir.file_name().and_then(|name| name.to_str()),
            Some(schedule_idx),
            Some(attempt_no as usize),
            "agent_output_parse",
            agent_output_parse_started_at,
            json!({
                "result_present": result_present,
                "result_parse_error": result_parse_error.as_deref()
            }),
        )?;

        let agent_exit_status = if agent_stream.timed_out {
            "timeout".to_string()
        } else {
            agent_status
                .exit_code
                .map(|value| value.to_string())
                .unwrap_or_else(|| "signal".to_string())
        };
        let mut trial_conclusion_row = None;
        let mut deferred_trial_conclusion_records = Vec::new();
        let mut grade_error_reason = None;
        let agent_outcome = AgentStageOutcome {
            agent_exit_status: agent_exit_status.clone(),
            trial_output: trial_output.clone(),
            result_present,
            result_parse_error: result_parse_error.clone(),
        };
        if agent_stream.timed_out {
            return finalize_trial_runtime(
                trial_dir,
                request.package_root,
                request.run_id,
                &mut attempt_state,
                agent_outcome,
                GradingStageOutcome {
                    trial_conclusion_row,
                    deferred_trial_conclusion_records,
                    grade_error_reason: Some("agent_timeout: benchmark grader skipped".to_string()),
                },
            );
        }

        if request.benchmark_grading_enabled {
            let task_payload: Value =
                serde_json::from_slice(&fs::read(&request.io_paths.trial_input_host)?)?;
            let agent_transport_started_at = Instant::now();
            let agent_transport_outputs = capture_agent_transport_outputs(
                &docker,
                &task_handle,
                request,
                trial_dir,
                task_sandbox_plan.time_limit_ms,
            )?;
            crate::perf::record_duration(
                request.package_root,
                request.run_id,
                trial_dir.file_name().and_then(|name| name.to_str()),
                Some(schedule_idx),
                Some(attempt_no as usize),
                "agent_transport_capture",
                agent_transport_started_at,
                json!({ "outputs": agent_transport_outputs.len() }),
            )?;

            let Some(grader_command) = resolve_benchmark_grader_command(request)? else {
                return finalize_trial_runtime(
                    trial_dir,
                    request.package_root,
                    request.run_id,
                    &mut attempt_state,
                    agent_outcome,
                    GradingStageOutcome {
                        trial_conclusion_row,
                        deferred_trial_conclusion_records,
                        grade_error_reason: Some(
                            "mapped_grader_output_missing: benchmark grader command not resolved"
                                .to_string(),
                        ),
                    },
                );
            };
            let grader = request
                .benchmark_grader
                .ok_or_else(|| anyhow!("benchmark grading enabled without grader config"))?;
            let grading_phase_resolved = resolve_grading_phase(request, grader, &grader_command)?;
            let grading_plan = build_grading_sandbox_plan(grader, &grading_phase_resolved)?;

            set_attempt_phase(
                request.package_root,
                request.run_id,
                trial_dir,
                &mut attempt_state,
                TrialPhase::GraderMaterializing,
            )?;
            if let Some(network) = ephemeral_network.as_ref() {
                let already_started = ephemeral_containers
                    .iter()
                    .map(|(plan, _)| plan.id.clone())
                    .collect::<BTreeSet<_>>();
                ephemeral_containers.extend(start_trial_ephemerals(
                    &docker,
                    request,
                    &network.name,
                    &mut attempt_state,
                    "grader",
                    &already_started,
                )?);
                persist_attempt_state(
                    request.package_root,
                    request.run_id,
                    trial_dir,
                    &attempt_state,
                )?;
            }
            let grading_handle = match grader.strategy {
                GradingStrategy::None => {
                    return Err(anyhow!("grader.strategy=none reached grading execution"))
                }
                GradingStrategy::InTaskRuntime => {
                    if !hidden_asset_bindings.is_empty() {
                        reveal_hidden_assets(
                            &docker,
                            &task_handle,
                            trial_dir,
                            &hidden_asset_bindings,
                            task_sandbox_plan.time_limit_ms,
                        )?;
                    }
                    task_handle.clone()
                }
                GradingStrategy::Injected => {
                    materialize_injected_grader_bundle(
                        &docker,
                        &task_handle,
                        trial_dir,
                        &grading_phase_resolved,
                        task_sandbox_plan.time_limit_ms,
                    )?;
                    task_handle.clone()
                }
                GradingStrategy::Separate => {
                    let grading_materialize_started_at = Instant::now();
                    let handle = materialize_grading_sandbox(
                        &docker,
                        runtime_sync,
                        request,
                        &grading_phase_resolved,
                        ephemeral_network
                            .as_ref()
                            .map(|network| network.name.as_str()),
                    )?;
                    crate::perf::record_duration(
                        request.package_root,
                        request.run_id,
                        trial_dir.file_name().and_then(|name| name.to_str()),
                        Some(schedule_idx),
                        Some(attempt_no as usize),
                        "grading_container_start",
                        grading_materialize_started_at,
                        json!({
                            "container_id": handle.container_id.as_str(),
                            "image": grading_phase_resolved.image.as_str()
                        }),
                    )?;
                    crate::perf::record_container_stats(
                        request.package_root,
                        request.run_id,
                        trial_dir
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("trial"),
                        schedule_idx,
                        attempt_no as usize,
                        "grading_container_started_stats",
                        &handle.container_id,
                        "grading",
                    )?;
                    grading_container = Some(handle.clone());
                    handle
                }
                GradingStrategy::Host => ContainerHandle {
                    container_id: "host".to_string(),
                },
            };
            let grading_sandbox = GradingSandboxState {
                container_id: grading_handle.container_id.clone(),
                strategy: grader.strategy.clone(),
                workdir: grading_phase_resolved.workdir.clone(),
            };
            attempt_state.grading_sandbox = Some(grading_sandbox.clone());
            persist_attempt_state(
                request.package_root,
                request.run_id,
                trial_dir,
                &attempt_state,
            )?;

            let grader_input_started_at = Instant::now();
            let transport_env = if matches!(grader.strategy, GradingStrategy::Host) {
                materialize_grader_inputs(
                    None,
                    None,
                    request,
                    trial_dir,
                    grader,
                    &agent_transport_outputs,
                    &task_payload,
                    task_sandbox_plan.time_limit_ms,
                )?
            } else {
                materialize_grader_inputs(
                    Some(&docker),
                    Some(&grading_handle),
                    request,
                    trial_dir,
                    grader,
                    &agent_transport_outputs,
                    &task_payload,
                    task_sandbox_plan.time_limit_ms,
                )?
            };
            crate::perf::record_duration(
                request.package_root,
                request.run_id,
                trial_dir.file_name().and_then(|name| name.to_str()),
                Some(schedule_idx),
                Some(attempt_no as usize),
                "grader_input_materialization",
                grader_input_started_at,
                json!({ "strategy": grading_strategy_name(&grader.strategy) }),
            )?;

            set_attempt_phase(
                request.package_root,
                request.run_id,
                trial_dir,
                &mut attempt_state,
                TrialPhase::GraderRunning,
            )?;
            let grader_started_at = Utc::now().to_rfc3339();
            let grader_run_started_at = Instant::now();
            let grader_run = if matches!(grader.strategy, GradingStrategy::Host) {
                run_host_grader(
                    request,
                    &grading_phase_resolved,
                    &agent_exit_status,
                    &transport_env,
                    &trial_grader_stdout_path(trial_dir),
                    &trial_grader_stderr_path(trial_dir),
                )?
            } else {
                run_container_grader(
                    &docker,
                    &grading_handle,
                    request,
                    &grading_phase_resolved,
                    &agent_exit_status,
                    &transport_env,
                    trial_dir,
                    task_sandbox_plan.time_limit_ms,
                )?
            };
            crate::perf::record_duration(
                request.package_root,
                request.run_id,
                trial_dir.file_name().and_then(|name| name.to_str()),
                Some(schedule_idx),
                Some(attempt_no as usize),
                "grader_run",
                grader_run_started_at,
                json!({
                    "strategy": grading_strategy_name(&grader.strategy),
                    "exit_code": grader_run.exit_code,
                    "timed_out": grader_run.timed_out
                }),
            )?;
            let grader_ended_at = Utc::now().to_rfc3339();

            let output_state = ContractFileState::Valid;
            attempt_state.grading_phase = Some(GradingPhaseRecord {
                started_at: grader_started_at,
                ended_at: grader_ended_at,
                exit_code: grader_run.exit_code,
                signal: grader_run.signal.clone(),
                timed_out: grader_run.timed_out,
                output_state,
                stdout_path: trial_grader_stdout_path(trial_dir)
                    .to_string_lossy()
                    .to_string(),
                stderr_path: trial_grader_stderr_path(trial_dir)
                    .to_string_lossy()
                    .to_string(),
            });
            persist_attempt_state(
                request.package_root,
                request.run_id,
                trial_dir,
                &attempt_state,
            )?;

            let grader_output_started_at = Instant::now();
            let grader_transport_outputs = if matches!(grader.strategy, GradingStrategy::Host) {
                capture_grader_transport_outputs(
                    None,
                    None,
                    request,
                    trial_dir,
                    grader,
                    task_sandbox_plan.time_limit_ms,
                )?
            } else {
                capture_grader_transport_outputs(
                    Some(&docker),
                    Some(&grading_handle),
                    request,
                    trial_dir,
                    grader,
                    task_sandbox_plan.time_limit_ms,
                )?
            };
            crate::perf::record_duration(
                request.package_root,
                request.run_id,
                trial_dir.file_name().and_then(|name| name.to_str()),
                Some(schedule_idx),
                Some(attempt_no as usize),
                "grader_output_capture",
                grader_output_started_at,
                json!({ "outputs": grader_transport_outputs.len() }),
            )?;
            write_transport_envelope(request, &agent_transport_outputs, &grader_transport_outputs)?;
            let synthesized = synthesize_grader_trial_conclusion(
                request,
                grader,
                &grader_transport_outputs,
                &grader_run,
            )?;
            let mapped_output_path = request.trial_paths.out.join(MAPPED_GRADER_OUTPUT_FILENAME);
            fs::write(
                &mapped_output_path,
                serde_json::to_vec_pretty(&synthesized)?,
            )?;

            match validate_json_schema("trial_conclusion_v1.jsonschema", &mapped_output_path) {
                Ok(row) => {
                    deferred_trial_conclusion_records.push(row.clone());
                    trial_conclusion_row = Some(row);
                }
                Err(err) => {
                    grade_error_reason = Some(format!("mapped_grader_output_invalid: {}", err));
                }
            }

            let _ = grading_plan;
        }

        finalize_trial_runtime(
            trial_dir,
            request.package_root,
            request.run_id,
            &mut attempt_state,
            agent_outcome,
            GradingStageOutcome {
                trial_conclusion_row,
                deferred_trial_conclusion_records,
                grade_error_reason,
            },
        )
    })();

    let mut cleanup_errors = Vec::new();
    let grading_cleanup_started_at = Instant::now();
    if let Some(error) = cleanup_trial_container(
        &docker,
        request.package_root,
        request.run_id,
        trial_dir,
        &mut attempt_state,
        "grading",
        grading_container.as_ref(),
    ) {
        cleanup_errors.push(error);
    }
    if grading_container.is_some() {
        crate::perf::record_duration(
            request.package_root,
            request.run_id,
            trial_dir.file_name().and_then(|name| name.to_str()),
            Some(schedule_idx),
            Some(attempt_no as usize),
            "grading_container_cleanup",
            grading_cleanup_started_at,
            json!({}),
        )?;
    }
    let ephemeral_cleanup_started_at = Instant::now();
    for (plan, handle) in ephemeral_containers.iter().rev() {
        if let Some(error) = cleanup_trial_container(
            &docker,
            request.package_root,
            request.run_id,
            trial_dir,
            &mut attempt_state,
            &format!("sidecar:{}", plan.id),
            Some(handle),
        ) {
            cleanup_errors.push(error);
        }
    }
    if !ephemeral_containers.is_empty() {
        crate::perf::record_duration(
            request.package_root,
            request.run_id,
            trial_dir.file_name().and_then(|name| name.to_str()),
            Some(schedule_idx),
            Some(attempt_no as usize),
            "sidecar_container_cleanup",
            ephemeral_cleanup_started_at,
            json!({ "sidecars": ephemeral_containers.len() }),
        )?;
    }
    let task_cleanup_started_at = Instant::now();
    if let Some(error) = cleanup_trial_container(
        &docker,
        request.package_root,
        request.run_id,
        trial_dir,
        &mut attempt_state,
        "task",
        task_container.as_ref(),
    ) {
        cleanup_errors.push(error);
    }
    if task_container.is_some() {
        crate::perf::record_duration(
            request.package_root,
            request.run_id,
            trial_dir.file_name().and_then(|name| name.to_str()),
            Some(schedule_idx),
            Some(attempt_no as usize),
            "task_container_cleanup",
            task_cleanup_started_at,
            json!({}),
        )?;
    }
    if let Some(error) = remove_trial_ephemeral_network(&docker, ephemeral_network.as_ref()) {
        cleanup_errors.push(format!("ephemeral network cleanup failed: {}", error));
    }

    if execution.is_err() {
        reconcile_attempt_as_abandoned(
            request.package_root,
            request.run_id,
            trial_dir,
            &mut attempt_state,
        );
    }
    match (execution, cleanup_errors.is_empty()) {
        (Ok(outcome), true) => {
            crate::perf::record_duration(
                request.package_root,
                request.run_id,
                trial_dir.file_name().and_then(|name| name.to_str()),
                Some(schedule_idx),
                Some(attempt_no as usize),
                "trial_runtime_total",
                trial_runtime_started_at,
                json!({}),
            )?;
            attach_local_runtime_evidence(
                outcome,
                request,
                trial_dir,
                schedule_idx,
                task_id,
                variant_id,
                repl_idx,
            )
        }
        (Ok(_), false) => Err(anyhow!(
            "container cleanup failed: {}",
            cleanup_errors.join("; ")
        )),
        (Err(err), true) => Err(err),
        (Err(err), false) => Err(err.context(format!(
            "container cleanup also failed: {}",
            cleanup_errors.join("; ")
        ))),
    }
}

fn materialize_task_sandbox<S>(
    docker: &DockerRuntime,
    runtime_sync: &S,
    request: &AdapterRunRequest<'_>,
    plan: &TaskSandboxPlan,
    injected_phase: Option<&ResolvedGradingPhase>,
    ephemeral_network: Option<&str>,
) -> Result<ContainerHandle>
where
    S: LocalContainerRuntimeSync,
{
    let mut extra_mounts = Vec::new();
    if let Some(bundle_host_path) =
        injected_phase.and_then(|phase| phase.injected_bundle_host_path.as_ref())
    {
        extra_mounts.push(ResolvedMountReference {
            host_path: bundle_host_path.clone(),
            mount_path: INJECTED_BUNDLE_SOURCE_MOUNT_PATH.to_string(),
            read_only: true,
        });
    }
    let mut spec = build_container_spec(
        runtime_sync,
        request,
        &plan.image,
        &plan.workdir,
        plan.network_mode.as_str(),
        true,
        &extra_mounts,
    )?;
    spec.platform = plan.platform.clone();
    if let Some(network) = ephemeral_network {
        spec.network_mode = Some(network.to_string());
    }
    spec.labels = trial_container_labels(request, "task");
    docker.create_and_start_container_checked(&spec, "task container")
}

fn run_case_materialization_steps(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    request: &AdapterRunRequest<'_>,
    trial_dir: &Path,
    plan: &TaskSandboxPlan,
) -> Result<()> {
    if plan.case_materialization.is_empty() {
        return Ok(());
    }
    let log_dir = trial_dir.join("runner").join("case_materialization");
    ensure_dir(&log_dir)?;
    for (idx, step) in plan.case_materialization.iter().enumerate() {
        let step_id = step.id.trim();
        let label = format!("{:03}_{}", idx, sanitize_for_fs(step_id));
        let workdir = step
            .workdir
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(plan.workdir.as_str());
        let mut env = build_exec_env(request, workdir, None, true);
        env.insert(
            "AGENTLAB_CASE_MATERIALIZATION_ID".to_string(),
            step_id.to_string(),
        );
        let exec = docker.exec(
            handle,
            &ExecSpec {
                command: step.command.clone(),
                env,
                workdir: Some(workdir.to_string()),
            },
        )?;
        let timeout_ms = step.timeout_ms.unwrap_or(plan.time_limit_ms).max(1);
        let stream = docker.stream_exec_output(
            &exec,
            &log_dir.join(format!("{}_stdout.log", label)),
            &log_dir.join(format!("{}_stderr.log", label)),
            Some(Duration::from_millis(timeout_ms)),
        )?;
        let status = docker
            .wait_exec(&exec)
            .unwrap_or(crate::backend::docker::ExecStatus { exit_code: None });
        if stream.timed_out {
            return Err(anyhow!(
                "case materialization step '{}' timed out after {} ms",
                step_id,
                timeout_ms
            ));
        }
        if status.exit_code != Some(0) {
            return Err(anyhow!(
                "case materialization step '{}' failed with exit code {:?}",
                step_id,
                status.exit_code
            ));
        }
    }
    Ok(())
}

fn materialize_grading_sandbox<S>(
    docker: &DockerRuntime,
    runtime_sync: &S,
    request: &AdapterRunRequest<'_>,
    resolved: &ResolvedGradingPhase,
    ephemeral_network: Option<&str>,
) -> Result<ContainerHandle>
where
    S: LocalContainerRuntimeSync,
{
    let mut spec = build_container_spec(
        runtime_sync,
        request,
        &resolved.image,
        &resolved.workdir,
        request.network_mode,
        false,
        &resolved.extra_mounts,
    )?;
    if let Some(network) = ephemeral_network {
        spec.network_mode = Some(network.to_string());
    }
    spec.labels = trial_container_labels(request, "grading");
    docker.create_and_start_container_checked(&spec, "grading container")
}

fn cleanup_trial_container(
    docker: &DockerRuntime,
    run_dir: &Path,
    run_id: &str,
    trial_dir: &Path,
    attempt_state: &mut TrialAttemptState,
    role: &str,
    handle: Option<&ContainerHandle>,
) -> Option<String> {
    let handle = handle?;
    let result =
        docker.remove_container_with_retry(handle, true, &format!("{} container cleanup", role));
    let error = result.as_ref().err().map(|err| err.to_string());
    attempt_state
        .cleanup
        .containers
        .push(ContainerCleanupRecord {
            container_id: handle.container_id.clone(),
            role: role.to_string(),
            status: if error.is_some() {
                "failed".to_string()
            } else {
                "removed".to_string()
            },
            error: error.clone(),
        });
    if let Err(err) = persist_attempt_state(run_dir, run_id, trial_dir, attempt_state) {
        return Some(match error {
            Some(cleanup_error) => format!(
                "{}; failed to persist cleanup state for {} container: {}",
                cleanup_error, role, err
            ),
            None => format!(
                "failed to persist cleanup state for {} container {}: {}",
                role, handle.container_id, err
            ),
        });
    }
    error
}

fn trial_container_labels(request: &AdapterRunRequest<'_>, role: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("agentlab.run_id".to_string(), request.run_id.to_string());
    labels.insert("agentlab.role".to_string(), role.to_string());
    if let Some(run_dir_digest) =
        run_dir_scope_digest_from_trial_dir(&request.trial_paths.trial_dir)
    {
        labels.insert("agentlab.run_dir_digest".to_string(), run_dir_digest);
    }
    labels.insert(
        "agentlab.task_materialization_kind".to_string(),
        task_materialization_kind_label(&request.task_materialization_kind).to_string(),
    );
    if let Some(trial_id) = request
        .trial_paths
        .trial_dir
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
    {
        labels.insert("agentlab.trial_id".to_string(), trial_id.to_string());
    }
    labels
}

fn task_materialization_kind_label(kind: &TaskMaterializationKind) -> &'static str {
    match kind {
        TaskMaterializationKind::TaskImage => "task_image",
        TaskMaterializationKind::BaseImageBundle => "base_image_bundle",
    }
}

pub(crate) fn build_container_spec<S>(
    runtime_sync: &S,
    request: &AdapterRunRequest<'_>,
    image: &str,
    workdir: &str,
    network_mode: &str,
    include_agent_artifact: bool,
    extra_mounts: &[ResolvedMountReference],
) -> Result<ContainerSpec>
where
    S: LocalContainerRuntimeSync,
{
    let mounts = runtime_sync.container_mounts(request, include_agent_artifact, extra_mounts)?;
    let mut tmpfs = BTreeMap::new();
    tmpfs.insert("/tmp".to_string(), "rw".to_string());
    if include_agent_artifact && request.agent_artifact.is_some() {
        tmpfs.insert("/opt/bench".to_string(), "rw".to_string());
    }

    let cpu_count = request
        .runtime_experiment
        .pointer("/policy/task_sandbox/resources/cpu_count")
        .and_then(Value::as_u64);
    let memory_mb = request
        .runtime_experiment
        .pointer("/policy/task_sandbox/resources/memory_mb")
        .and_then(Value::as_u64);

    let mut spec = ContainerSpec::idle(image.to_string());
    spec.workdir = Some(workdir.to_string());
    spec.mounts = mounts;
    spec.tmpfs = tmpfs;
    spec.network_mode = docker_network_mode(network_mode);
    spec.security_opt = if request
        .runtime_experiment
        .pointer("/policy/task_sandbox/hardening/no_new_privileges")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        vec!["no-new-privileges".to_string()]
    } else {
        Vec::new()
    };
    spec.cap_drop = if request
        .runtime_experiment
        .pointer("/policy/task_sandbox/hardening/drop_all_caps")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        vec!["ALL".to_string()]
    } else {
        Vec::new()
    };
    spec.cpu_count = cpu_count;
    spec.memory_mb = memory_mb;
    Ok(spec)
}

pub(crate) fn docker_network_mode(network_mode: &str) -> Option<String> {
    match network_mode {
        "none" => Some("none".to_string()),
        "full" | "allowlist_enforced" | "llm_egress" => None,
        _ => None,
    }
}

fn classify_contract_file_state(path: &Path, validation_error: Option<&str>) -> ContractFileState {
    if !path.exists() || path.metadata().map(|meta| meta.len()).unwrap_or(0) == 0 {
        ContractFileState::Missing
    } else if validation_error.is_some() {
        ContractFileState::PresentInvalid
    } else {
        ContractFileState::Valid
    }
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
        AGENTLAB_CONTRACT_WORKSPACE_DIR,
        "/workspace/task",
        "/testbed",
    ];
    if !allowed_roots
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{}/", root)))
    {
        return Err(anyhow!(
            "mount_path must be under {}",
            AGENTLAB_CONTRACT_WORKSPACE_DIR
        ));
    }
    Ok(())
}

pub(crate) fn resolve_task_sandbox_image(request: &AdapterRunRequest<'_>) -> Result<String> {
    let image = request.task_image.trim();
    if image.is_empty() {
        return Err(anyhow!("task image is required for task sandbox"));
    }
    Ok(image.to_string())
}

pub(crate) fn resolve_container_workspace<'a>(
    request: &'a AdapterRunRequest<'_>,
) -> Result<&'a str> {
    let workdir = request.task_workdir.trim();
    if workdir.is_empty() {
        return Err(anyhow!("task workdir is required for task sandbox"));
    }
    Ok(workdir)
}

pub(crate) fn resolve_container_image_digest(image: &str) -> Option<String> {
    let runtime = DockerRuntime::connect().ok()?;
    let metadata = runtime.ensure_image(image).ok()?;
    metadata
        .repo_digests
        .first()
        .and_then(|value| value.rsplit_once('@').map(|(_, digest)| digest.to_string()))
        .or(metadata.image_id)
}

pub(crate) fn agent_artifact_cache_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) fn repair_agent_artifact_layout(unpacked_dir: &Path) -> Result<()> {
    let packages_root = unpacked_dir.join("packages");
    let nested_packages_root = packages_root.join("packages");
    if !packages_root.is_dir() || !nested_packages_root.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&nested_packages_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let shim_path = packages_root.join(&name);
        if shim_path.exists() {
            continue;
        }
        let nested_rel = Path::new("packages").join(&name);
        let nested_abs = packages_root.join(&nested_rel);
        if !nested_abs.exists() {
            continue;
        }
        symlink(&nested_rel, &shim_path).map_err(|err| {
            anyhow!(
                "failed to create artifact layout shim {} -> {}: {}",
                shim_path.display(),
                nested_rel.display(),
                err
            )
        })?;
    }
    Ok(())
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
    let resolved = normalize_path(
        &entry_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(target),
    );
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
    let artifact_name = artifact_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let gzipped = if artifact_name.ends_with(".tar.gz") || artifact_name.ends_with(".tgz") {
        true
    } else if artifact_name.ends_with(".tar") {
        false
    } else {
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
        return Ok(fs::canonicalize(artifact).unwrap_or_else(|_| artifact.to_path_buf()));
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
    let artifact_path = fs::canonicalize(artifact).unwrap_or_else(|_| artifact.to_path_buf());
    let artifact_name = artifact_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let gzipped = if artifact_name.ends_with(".tar.gz") || artifact_name.ends_with(".tgz") {
        true
    } else if artifact_name.ends_with(".tar") {
        false
    } else {
        return Err(anyhow!(
            "trial_runtime.agent.artifact '{}' must be a directory or .tar/.tar.gz archive",
            artifact_path.display()
        ));
    };

    let digest = sha256_file(&artifact_path)?;
    let digest_path_component = digest.replace(':', "_");
    let cache_root = artifact_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".agentlab_artifact_cache");
    ensure_dir(&cache_root)?;
    let unpacked_dir = cache_root.join(&digest_path_component);
    let ready_marker = unpacked_dir.join(".agentlab_ready");
    if ready_marker.exists() {
        repair_agent_artifact_layout(&unpacked_dir)?;
        return Ok(unpacked_dir);
    }

    let _guard = agent_artifact_cache_lock()
        .lock()
        .map_err(|_| anyhow!("agent artifact cache lock poisoned"))?;
    if ready_marker.exists() {
        repair_agent_artifact_layout(&unpacked_dir)?;
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
        let _ = fs::remove_dir_all(&staging_dir);
    }
    ensure_dir(&staging_dir)?;
    if let Err(err) = unpack_agent_artifact_archive(&artifact_path, &staging_dir, gzipped) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(anyhow!(
            "failed to unpack trial_runtime.agent.artifact {}: {}",
            artifact_path.display(),
            err,
        ));
    }
    if let Err(err) = fs::rename(&staging_dir, &unpacked_dir) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(anyhow!(
            "failed to finalize unpacked trial_runtime.agent.artifact {} into {}: {}",
            artifact_path.display(),
            unpacked_dir.display(),
            err
        ));
    }
    repair_agent_artifact_layout(&unpacked_dir)?;
    fs::write(&ready_marker, digest.as_bytes())?;
    Ok(unpacked_dir)
}

pub(crate) fn map_container_path_to_host(path: &str, paths: &TrialPaths) -> Result<PathBuf> {
    map_contract_path_to_host(
        path,
        &ContractPathHostRoots::from_trial_paths(paths),
        ContractPathMode::ContainerMount,
    )
}
