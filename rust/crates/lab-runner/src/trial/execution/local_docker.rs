use super::*;

use crate::backend::docker::{
    ContainerHandle, ContainerMount, ContainerSpec, DockerRuntime, ExecHandle, ExecSpec,
    ExecStatus, NetworkHandle,
};
use crate::persistence::backend::open_trial_attempt_store;
use crate::trial::materialization::{
    selected_case_materialization_steps, CaseMaterializationPhase,
};
use crate::trial::spec::CaseMaterializationOperation;
use lab_core::canonical_json_digest;

pub(crate) const BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS_ENV: &str =
    "BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS";

const DEFAULT_DOCKER_MAX_ACTIVE_CONTAINERS: usize = 24;

fn docker_active_container_limiter() -> &'static ActiveRuntimeLimiter {
    static LIMITER: OnceLock<ActiveRuntimeLimiter> = OnceLock::new();
    LIMITER.get_or_init(ActiveRuntimeLimiter::new)
}

fn wait_exec_status(
    docker: &DockerRuntime,
    exec: &ExecHandle,
    operation: impl FnOnce() -> String,
) -> Result<ExecStatus> {
    docker.wait_exec(exec).with_context(operation)
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

pub(crate) trait LocalContainerRuntimeSync {
    fn container_mounts(
        &self,
        request: &TrialRunRequest<'_>,
        include_agent_artifact: bool,
        extra_mounts: &[ResolvedMountReference],
    ) -> Result<Vec<ContainerMount>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct LocalBindMountRuntimeSync;

impl LocalContainerRuntimeSync for LocalBindMountRuntimeSync {
    fn container_mounts(
        &self,
        request: &TrialRunRequest<'_>,
        include_agent_artifact: bool,
        extra_mounts: &[ResolvedMountReference],
    ) -> Result<Vec<ContainerMount>> {
        let mut mounts = vec![
            ContainerMount {
                host_path: request.trial_paths.in_dir.clone(),
                container_path: BUCEPHALUS_CONTRACT_IN_DIR.to_string(),
                read_only: true,
            },
            ContainerMount {
                host_path: request.trial_paths.out.clone(),
                container_path: BUCEPHALUS_CONTRACT_OUT_DIR.to_string(),
                read_only: false,
            },
            ContainerMount {
                host_path: request.trial_paths.events.clone(),
                container_path: BUCEPHALUS_CONTRACT_EVENTS_DIR.to_string(),
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

fn trial_ephemeral_network_name(
    request: &TrialRunRequest<'_>,
    schedule_idx: usize,
    attempt_no: usize,
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
    .fold("bucephalus_ephemeral_".to_string(), |mut acc, ch| {
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
    request: &TrialRunRequest<'_>,
    schedule_idx: usize,
    attempt_no: usize,
) -> Result<Option<TrialEphemeralNetwork>> {
    if !trial_requires_ephemeral_network(request)? {
        return Ok(None);
    }
    let name = trial_ephemeral_network_name(request, schedule_idx, attempt_no);
    let mut labels = BTreeMap::new();
    labels.insert("bucephalus.run_id".to_string(), request.run_id.to_string());
    labels.insert(
        "bucephalus.role".to_string(),
        "ephemeral_network".to_string(),
    );
    if let Some(run_dir_digest) =
        run_dir_scope_digest_from_trial_dir(&request.trial_paths.trial_dir)
    {
        labels.insert("bucephalus.run_dir_digest".to_string(), run_dir_digest);
    }
    if let Some(trial_id) = request
        .trial_paths
        .trial_dir
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
    {
        labels.insert("bucephalus.trial_id".to_string(), trial_id.to_string());
    }
    let internal = request.network_mode == "none";
    docker.create_network(&name, internal, labels)?;
    Ok(Some(TrialEphemeralNetwork { name, internal }))
}

fn trial_requires_ephemeral_network(request: &TrialRunRequest<'_>) -> Result<bool> {
    Ok(!trial_sidecar_plans(request.runtime_experiment)?.is_empty()
        || request.network_mode == "allowlist_enforced")
}

pub(crate) fn trial_requires_ephemeral_network_for_test(
    request: &TrialRunRequest<'_>,
) -> Result<bool> {
    trial_requires_ephemeral_network(request)
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
    request: &TrialRunRequest<'_>,
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

fn planned_docker_active_container_units(request: &TrialRunRequest<'_>) -> Result<usize> {
    let host_agent_without_grading = request
        .runtime_experiment
        .pointer("/trial_runtime/execution/agent_site")
        .and_then(Value::as_str)
        == Some("host")
        && !request.grading_enabled;
    if host_agent_without_grading {
        return Ok(0);
    }

    let mut units = 1 + trial_sidecar_plans(request.runtime_experiment)?.len();
    if request.grading_enabled
        && request
            .grader
            .map(|grader| matches!(grader.strategy, GradingStrategy::Separate))
            .unwrap_or(false)
    {
        units += 1;
    }
    Ok(units)
}

#[cfg(test)]
fn acquire_docker_active_container_permit(
    request: &TrialRunRequest<'_>,
) -> Result<ActiveRuntimePermit> {
    let units = planned_docker_active_container_units(request)?;
    acquire_docker_active_container_units_permit(units)
}

fn acquire_docker_active_container_units_permit(units: usize) -> Result<ActiveRuntimePermit> {
    docker_active_container_limiter().acquire(
        units,
        active_runtime_limit(
            BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
            DEFAULT_DOCKER_MAX_ACTIVE_CONTAINERS,
        )?,
        "Docker containers",
        BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
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
        BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
        DEFAULT_DOCKER_MAX_ACTIVE_CONTAINERS,
    )?;
    let active = docker
        .list_running_containers_by_labels(&["bucephalus.run_id".to_string()])
        .context("listing active Bucephalus Docker containers")?
        .len();
    if active + planned_units > limit {
        return Err(anyhow!(
            "Docker currently has {} active Bucephalus containers and this trial requires {} more, but {} limits this runner to {}",
            active,
            planned_units,
            BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
            limit
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn planned_docker_active_container_units_for_test(
    request: &TrialRunRequest<'_>,
) -> Result<usize> {
    planned_docker_active_container_units(request)
}

#[cfg(test)]
pub(crate) fn acquire_docker_active_container_permit_for_test(
    request: &TrialRunRequest<'_>,
) -> Result<ActiveRuntimePermit> {
    acquire_docker_active_container_permit(request)
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
    Ok(open_trial_attempt_store(&run_dir)?
        .trial_attempt_container_ids(run_id, trial_id)?
        .into_iter()
        .map(|container_id| ContainerHandle { container_id })
        .collect())
}

fn docker_db_trial_attempt_exists(run_id: &str, trial_id: &str, trial_dir: &Path) -> Result<bool> {
    let Some(run_dir) = cleanup_run_dir_from_trial_dir(trial_dir) else {
        return Ok(false);
    };
    Ok(open_trial_attempt_store(&run_dir)?
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

fn docker_bucephalus_runtime_labels(
    run_id: &str,
    trial_id: Option<&str>,
    run_dir_digest: Option<&str>,
) -> Vec<String> {
    let mut labels = vec![format!("bucephalus.run_id={}", run_id)];
    if let Some(trial_id) = trial_id {
        labels.push(format!("bucephalus.trial_id={}", trial_id));
    }
    if let Some(run_dir_digest) = run_dir_digest {
        labels.push(format!("bucephalus.run_dir_digest={}", run_dir_digest));
    }
    labels
}

fn docker_labeled_trial_container_handles(
    run_id: &str,
    trial_id: &str,
    run_dir_digest: Option<&str>,
) -> Result<Vec<ContainerHandle>> {
    DockerRuntime::connect()?.list_containers_by_labels(&docker_bucephalus_runtime_labels(
        run_id,
        Some(trial_id),
        run_dir_digest,
    ))
}

fn docker_labeled_run_container_handles(run_id: &str) -> Result<Vec<ContainerHandle>> {
    DockerRuntime::connect()?
        .list_containers_by_labels(&docker_bucephalus_runtime_labels(run_id, None, None))
}

fn docker_labeled_trial_network_handles(
    run_id: &str,
    trial_id: &str,
    run_dir_digest: Option<&str>,
) -> Result<Vec<NetworkHandle>> {
    DockerRuntime::connect()?.list_networks_by_labels(&docker_bucephalus_runtime_labels(
        run_id,
        Some(trial_id),
        run_dir_digest,
    ))
}

fn docker_labeled_run_network_handles(run_id: &str) -> Result<Vec<NetworkHandle>> {
    DockerRuntime::connect()?
        .list_networks_by_labels(&docker_bucephalus_runtime_labels(run_id, None, None))
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

fn capture_candidate_workspace_patch(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    request: &TrialRunRequest<'_>,
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
    let probe_status = wait_exec_status(docker, &probe, || {
        format!(
            "wait for candidate patch git worktree probe in {}",
            request.task_workdir
        )
    })?;
    if probe_stream.timed_out || probe_status.exit_code != Some(0) {
        return Ok(None);
    }

    let pathspec = vec![
        ".".to_string(),
        ":(exclude).bucephalus".to_string(),
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
    let add_status = wait_exec_status(docker, &add_exec, || {
        format!(
            "wait for candidate patch git add in {}",
            request.task_workdir
        )
    })?;
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
    let diff_status = wait_exec_status(docker, &diff_exec, || {
        format!(
            "wait for candidate patch git diff in {}",
            request.task_workdir
        )
    })?;
    if diff_stream.timed_out || diff_status.exit_code != Some(0) {
        return Err(anyhow!(
            "failed to capture candidate workspace patch; see {}",
            patch_log_dir.join("diff_stderr.log").display()
        ));
    }
    Ok(Some(patch_path))
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
    let status = wait_exec_status(docker, &exec, || {
        format!("wait while checking container file {}", container_path)
    })?;
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
    let status = wait_exec_status(docker, &exec, || {
        format!("wait while copying container file {}", container_path)
    })?;
    if stream.timed_out || status.exit_code != Some(0) {
        return Err(anyhow!(
            "failed to capture declared runtime output file {}",
            container_path
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct RuntimeTransportContext<'a, 'run> {
    docker: Option<&'a DockerRuntime>,
    handle: Option<&'a ContainerHandle>,
    request: &'a TrialRunRequest<'run>,
    trial_dir: &'a Path,
    timeout_ms: u64,
}

#[derive(Clone, Copy)]
struct ContainerTransportContext<'a, 'run> {
    docker: &'a DockerRuntime,
    handle: &'a ContainerHandle,
    request: &'a TrialRunRequest<'run>,
    trial_dir: &'a Path,
    timeout_ms: u64,
}

impl<'a, 'run> RuntimeTransportContext<'a, 'run> {
    fn container(self) -> Option<ContainerTransportContext<'a, 'run>> {
        Some(ContainerTransportContext {
            docker: self.docker?,
            handle: self.handle?,
            request: self.request,
            trial_dir: self.trial_dir,
            timeout_ms: self.timeout_ms,
        })
    }
}

impl<'a, 'run> ContainerTransportContext<'a, 'run> {
    fn runtime(self) -> RuntimeTransportContext<'a, 'run> {
        RuntimeTransportContext {
            docker: Some(self.docker),
            handle: Some(self.handle),
            request: self.request,
            trial_dir: self.trial_dir,
            timeout_ms: self.timeout_ms,
        }
    }
}

fn captured_file_host_path(
    ctx: RuntimeTransportContext<'_, '_>,
    label: &str,
    container_path: &str,
    required: bool,
) -> Result<Option<PathBuf>> {
    if let Ok(host_path) = map_container_path_to_host(container_path, ctx.request.trial_paths) {
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

    let host_path = if let Some(container) = ctx.container() {
        if !container_file_exists(
            container.docker,
            container.handle,
            container.trial_dir,
            label,
            container_path,
            container.timeout_ms,
        )? {
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
        let staged = ctx
            .request
            .trial_paths
            .out
            .join("transport")
            .join("captured")
            .join(format!("{}.{}", sanitize_for_fs(label), extension));
        copy_container_file_to_host(
            container.docker,
            container.handle,
            container.trial_dir,
            label,
            container_path,
            &staged,
            container.timeout_ms,
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

fn capture_runtime_output(
    ctx: RuntimeTransportContext<'_, '_>,
    label: &str,
    output: &RuntimeOutputConfig,
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
            let Some(host_path) =
                captured_file_host_path(ctx, label, container_path, capture.required)?
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
            let host_path =
                captured_file_host_path(ctx, label, container_path, true)?.ok_or_else(|| {
                    anyhow!(
                        "declared runtime result output missing at {}",
                        container_path
                    )
                })?;
            let result_json = read_captured_file_value(&host_path, "json")?;
            let value = if let Some(field) = capture.field.as_deref() {
                let selected = select_transport_field(&result_json, field).ok_or_else(|| {
                    anyhow!("{}.capture.field '{}' resolved to null", label, field)
                })?;
                json!({
                    "value": selected
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
            let container = ctx
                .container()
                .ok_or_else(|| anyhow!("workspace_diff capture requires a container runtime"))?;
            let patch_path = capture_candidate_workspace_patch(
                container.docker,
                container.handle,
                container.request,
                container.trial_dir,
                container.timeout_ms,
            )?;
            if let Some(path) = patch_path.as_ref() {
                enforce_inline_capture_size(path, "workspace_diff runtime output")?;
            }
            let value = match patch_path.as_ref() {
                Some(path) => json!({
                    "patch": fs::read_to_string(path)?,
                    "path": "/bucephalus/out/candidate.patch"
                }),
                None => Value::Null,
            };
            Ok(CapturedTransportOutput {
                value,
                host_path: patch_path,
                container_path: Some("/bucephalus/out/candidate.patch".to_string()),
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
    ctx: ContainerTransportContext<'_, '_>,
) -> Result<BTreeMap<String, CapturedTransportOutput>> {
    let outputs = parse_agent_outputs(ctx.request)?;
    outputs
        .iter()
        .map(|(id, output)| {
            let captured = capture_runtime_output(ctx.runtime(), &format!("agent.{}", id), output)?;
            Ok((id.clone(), captured))
        })
        .collect()
}

fn materialize_container_file(
    ctx: ContainerTransportContext<'_, '_>,
    input_id: &str,
    target_container_path: &str,
    bytes: &[u8],
) -> Result<()> {
    if let Ok(host_path) =
        map_container_path_to_host(target_container_path, ctx.request.trial_paths)
    {
        return write_host_transport_file(&host_path, bytes);
    }

    let staged_host_path = ctx
        .request
        .trial_paths
        .out
        .join("transport")
        .join("grader_inputs")
        .join(sanitize_for_fs(input_id));
    write_host_transport_file(&staged_host_path, bytes)?;
    let staged_container_path = format!(
        "/bucephalus/out/transport/grader_inputs/{}",
        sanitize_for_fs(input_id)
    );
    let log_dir = ctx.trial_dir.join("logs").join("transport");
    ensure_dir(&log_dir)?;
    let exec = ctx.docker.exec(
        ctx.handle,
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
    let stream = ctx.docker.stream_exec_output(
        &exec,
        &log_dir.join(format!(
            "{}_materialize_stdout.log",
            sanitize_for_fs(input_id)
        )),
        &log_dir.join(format!(
            "{}_materialize_stderr.log",
            sanitize_for_fs(input_id)
        )),
        Some(Duration::from_millis(ctx.timeout_ms.max(1_000))),
    )?;
    let status = wait_exec_status(ctx.docker, &exec, || {
        format!("wait while materializing grader input '{}'", input_id)
    })?;
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
    ctx: RuntimeTransportContext<'_, '_>,
    grader: &GraderConfig,
    agent_outputs: &BTreeMap<String, CapturedTransportOutput>,
    task_payload: &Value,
) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for (id, input) in &grader.inputs {
        let value = select_transport_source(&input.source, agent_outputs, task_payload)?;
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
                if let Some(container) = ctx.container() {
                    materialize_container_file(container, id, target, &bytes)?;
                } else {
                    let host_path = map_container_path_to_host(target, ctx.request.trial_paths)
                        .with_context(|| {
                            format!(
                                "grader input '{}'.materialize.path must target a trial contract path: {}",
                                id, target
                            )
                        })?;
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
                    .map_or_else(|| value.to_string(), str::to_string);
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

#[cfg(test)]
pub(crate) fn materialize_grader_inputs_for_test(
    request: &TrialRunRequest<'_>,
    trial_dir: &Path,
    grader: &GraderConfig,
    task_payload: &Value,
) -> Result<BTreeMap<String, String>> {
    materialize_grader_inputs(
        RuntimeTransportContext {
            docker: None,
            handle: None,
            request,
            trial_dir,
            timeout_ms: 1,
        },
        grader,
        &BTreeMap::new(),
        task_payload,
    )
}

fn capture_grader_transport_outputs(
    ctx: RuntimeTransportContext<'_, '_>,
    grader: &GraderConfig,
) -> Result<BTreeMap<String, CapturedTransportOutput>> {
    grader
        .outputs
        .iter()
        .map(|(id, output)| {
            let captured = capture_runtime_output(ctx, &format!("grader.{}", id), output)?;
            Ok((id.clone(), captured))
        })
        .collect()
}

fn run_container_grader(
    ctx: ContainerTransportContext<'_, '_>,
    resolved: &ResolvedGradingPhase,
    agent_exit_status: &str,
    transport_env: &BTreeMap<String, String>,
) -> Result<GraderRunOutcome> {
    let mut env = build_exec_env(
        ctx.request,
        &resolved.workdir,
        Some((BUCEPHALUS_ENV_AGENT_EXIT_STATUS, agent_exit_status)),
        false,
    );
    env.extend(transport_env.clone());
    extend_with_sidecar_env(&mut env, ctx.request, "grader")?;
    let grader_exec = ctx.docker.exec(
        ctx.handle,
        &ExecSpec {
            command: resolved.command.clone(),
            env,
            workdir: Some(resolved.workdir.clone()),
        },
    )?;
    let grader_stream = ctx.docker.stream_exec_output(
        &grader_exec,
        &trial_grader_stdout_path(ctx.trial_dir),
        &trial_grader_stderr_path(ctx.trial_dir),
        Some(Duration::from_millis(ctx.timeout_ms)),
    )?;
    let grader_status = wait_exec_status(ctx.docker, &grader_exec, || {
        "wait for grader command".to_string()
    })?;
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

fn execute_local_docker_trial_runtime<S>(
    execution_request: TrialRuntimeExecutionRequest<'_>,
    runtime_sync: &S,
) -> Result<TrialRuntimeOutcome>
where
    S: LocalContainerRuntimeSync,
{
    let TrialRuntimeExecutionRequest {
        trial_dir,
        schedule_idx,
        attempt_no,
        run_request: request,
        task_id,
        variant_id,
        repl_idx,
        task_sandbox_plan,
    } = execution_request;
    validate_grading_contract(request)?;
    let trial_runtime_started_at = Instant::now();
    if request
        .runtime_experiment
        .pointer("/trial_runtime/execution/agent_site")
        .and_then(Value::as_str)
        == Some("host")
        && !request.grading_enabled
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
    let perf_scope = crate::perf::PerfScope::new(
        request.package_root,
        request.run_id,
        trial_dir.file_name().and_then(|name| name.to_str()),
        Some(schedule_idx),
        Some(attempt_no),
    );
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
        perf_scope,
        "docker_ensure_task_image",
        ensure_image_started_at,
        json!({
            "image": task_sandbox_plan.image.as_str(),
            "platform": task_sandbox_plan.platform.as_deref()
        }),
    )?;
    let hidden_asset_bindings = request
        .grader
        .map(build_hidden_asset_bindings)
        .transpose()?
        .unwrap_or_default();
    let injected_grading_phase = if request.grading_enabled {
        if let Some(grader) = request.grader {
            if matches!(grader.strategy, GradingStrategy::Injected) {
                resolve_grader_command(request)?
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
            perf_scope,
            "task_container_start",
            task_materialize_started_at,
            json!({
                "container_id": task_handle.container_id.as_str(),
                "image": task_sandbox_plan.image.as_str(),
                "platform": task_sandbox_plan.platform.as_deref()
            }),
        )?;
        crate::perf::record_duration(
            perf_scope,
            "backend_dispatch_to_container_start",
            task_materialize_started_at,
            json!({
                "executor": "local-docker",
                "dispatch_boundary": "runner_enters_task_container_materialization",
                "start_boundary": "docker_reports_task_container_running",
                "container_id": task_handle.container_id.as_str(),
                "image": task_sandbox_plan.image.as_str(),
                "platform": task_sandbox_plan.platform.as_deref()
            }),
        )?;
        crate::perf::record_container_stats(
            perf_scope,
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
            perf_scope,
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
        )?;
        let agent_run_started_at = Instant::now();
        let agent_stream_result = docker.stream_exec_output(
            &agent_exec,
            &trial_agent_stdout_path(trial_dir),
            &trial_agent_stderr_path(trial_dir),
            Some(Duration::from_millis(task_sandbox_plan.time_limit_ms)),
        );
        let agent_status = wait_exec_status(&docker, &agent_exec, || {
            "wait for agent command".to_string()
        })?;
        let live_event_ingest_result = stop_live_event_ingest(live_event_ingest);
        let agent_stream = agent_stream_result?;
        live_event_ingest_result?;
        crate::perf::record_duration(
            perf_scope,
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
        )?;

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
            perf_scope,
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
            exit_status_label(agent_status.exit_code)
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
                    grade_error_reason: Some("agent_timeout: grader skipped".to_string()),
                },
            );
        }

        if request.grading_enabled {
            let task_payload: Value =
                serde_json::from_slice(&fs::read(&request.io_paths.trial_input_host)?)?;
            let agent_transport_started_at = Instant::now();
            let agent_transport_outputs =
                capture_agent_transport_outputs(ContainerTransportContext {
                    docker: &docker,
                    handle: &task_handle,
                    request,
                    trial_dir,
                    timeout_ms: task_sandbox_plan.time_limit_ms,
                })?;
            crate::perf::record_duration(
                perf_scope,
                "agent_transport_capture",
                agent_transport_started_at,
                json!({ "outputs": agent_transport_outputs.len() }),
            )?;

            let Some(grader_command) = resolve_grader_command(request)? else {
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
                            "mapped_grader_output_missing: grader command not resolved".to_string(),
                        ),
                    },
                );
            };
            let grader = request
                .grader
                .ok_or_else(|| anyhow!("grading enabled without grader config"))?;
            let grading_phase_resolved = resolve_grading_phase(request, grader, &grader_command)?;
            build_grading_sandbox_plan(grader, &grading_phase_resolved)?;

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
                        perf_scope,
                        "grading_container_start",
                        grading_materialize_started_at,
                        json!({
                            "container_id": handle.container_id.as_str(),
                            "image": grading_phase_resolved.image.as_str()
                        }),
                    )?;
                    crate::perf::record_container_stats(
                        perf_scope,
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
            let grader_transport = if matches!(grader.strategy, GradingStrategy::Host) {
                RuntimeTransportContext {
                    docker: None,
                    handle: None,
                    request,
                    trial_dir,
                    timeout_ms: task_sandbox_plan.time_limit_ms,
                }
            } else {
                ContainerTransportContext {
                    docker: &docker,
                    handle: &grading_handle,
                    request,
                    trial_dir,
                    timeout_ms: task_sandbox_plan.time_limit_ms,
                }
                .runtime()
            };
            let transport_env = materialize_grader_inputs(
                grader_transport,
                grader,
                &agent_transport_outputs,
                &task_payload,
            )?;
            crate::perf::record_duration(
                perf_scope,
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
                    ContainerTransportContext {
                        docker: &docker,
                        handle: &grading_handle,
                        request,
                        trial_dir,
                        timeout_ms: task_sandbox_plan.time_limit_ms,
                    },
                    &grading_phase_resolved,
                    &agent_exit_status,
                    &transport_env,
                )?
            };
            crate::perf::record_duration(
                perf_scope,
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
            let grader_transport = if matches!(grader.strategy, GradingStrategy::Host) {
                RuntimeTransportContext {
                    docker: None,
                    handle: None,
                    request,
                    trial_dir,
                    timeout_ms: task_sandbox_plan.time_limit_ms,
                }
            } else {
                ContainerTransportContext {
                    docker: &docker,
                    handle: &grading_handle,
                    request,
                    trial_dir,
                    timeout_ms: task_sandbox_plan.time_limit_ms,
                }
                .runtime()
            };
            let grader_transport_outputs =
                capture_grader_transport_outputs(grader_transport, grader)?;
            crate::perf::record_duration(
                perf_scope,
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

    let mut post_runtime_errors = Vec::new();
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
        post_runtime_errors.push(error);
    }
    if grading_container.is_some() {
        crate::perf::record_duration(
            perf_scope,
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
            post_runtime_errors.push(error);
        }
    }
    if !ephemeral_containers.is_empty() {
        crate::perf::record_duration(
            perf_scope,
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
        post_runtime_errors.push(error);
    }
    if task_container.is_some() {
        crate::perf::record_duration(
            perf_scope,
            "task_container_cleanup",
            task_cleanup_started_at,
            json!({}),
        )?;
    }
    if let Some(error) = remove_trial_ephemeral_network(&docker, ephemeral_network.as_ref()) {
        post_runtime_errors.push(format!("ephemeral network cleanup failed: {}", error));
    }

    if execution.is_err() {
        if let Err(error) = reconcile_attempt_as_abandoned(
            request.package_root,
            request.run_id,
            trial_dir,
            &mut attempt_state,
        ) {
            post_runtime_errors.push(format!("abandoned state persistence failed: {}", error));
        }
    }
    match (execution, post_runtime_errors.is_empty()) {
        (Ok(outcome), true) => {
            crate::perf::record_duration(
                perf_scope,
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
            "post-runtime cleanup failed: {}",
            post_runtime_errors.join("; ")
        )),
        (Err(err), true) => Err(err),
        (Err(err), false) => Err(err.context(format!(
            "post-runtime cleanup also failed: {}",
            post_runtime_errors.join("; ")
        ))),
    }
}

fn materialize_task_sandbox<S>(
    docker: &DockerRuntime,
    runtime_sync: &S,
    request: &TrialRunRequest<'_>,
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
        plan.artifact_mount.is_some(),
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
    request: &TrialRunRequest<'_>,
    trial_dir: &Path,
    plan: &TaskSandboxPlan,
) -> Result<()> {
    if plan.case_materialization.is_empty() {
        return Ok(());
    }
    let selected = selected_case_materialization_steps(
        &plan.case_materialization,
        CaseMaterializationPhase::AgentVisible,
    );
    if selected.is_empty() {
        return Ok(());
    }
    let log_dir = trial_dir.join("runner").join("case_materialization");
    ensure_dir(&log_dir)?;
    for (idx, step) in selected.into_iter().enumerate() {
        let step_id = step.id.trim();
        if step.operation != CaseMaterializationOperation::Command {
            return Err(anyhow!(
                "local Docker case materialization step '{}' uses operation={:?}; only command is supported inside the task container",
                step_id,
                step.operation
            ));
        }
        let label = format!("{:03}_{}", idx, sanitize_for_fs(step_id));
        let workdir = step
            .workdir
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(plan.workdir.as_str());
        let mut env = build_exec_env(request, workdir, None, true);
        env.insert(
            "BUCEPHALUS_CASE_MATERIALIZATION_ID".to_string(),
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
        let status = wait_exec_status(docker, &exec, || {
            format!("wait for case materialization step '{}'", step_id)
        })?;
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
    request: &TrialRunRequest<'_>,
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

fn trial_container_labels(request: &TrialRunRequest<'_>, role: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("bucephalus.run_id".to_string(), request.run_id.to_string());
    labels.insert("bucephalus.role".to_string(), role.to_string());
    if let Some(run_dir_digest) =
        run_dir_scope_digest_from_trial_dir(&request.trial_paths.trial_dir)
    {
        labels.insert("bucephalus.run_dir_digest".to_string(), run_dir_digest);
    }
    labels.insert(
        "bucephalus.task_materialization_kind".to_string(),
        task_materialization_kind_label(&request.task_materialization_kind).to_string(),
    );
    if let Some(trial_id) = request
        .trial_paths
        .trial_dir
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
    {
        labels.insert("bucephalus.trial_id".to_string(), trial_id.to_string());
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
    request: &TrialRunRequest<'_>,
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
