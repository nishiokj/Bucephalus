use super::*;
use crate::util::env_var_with_legacy;
use std::io::{BufRead, BufReader, Write};
use std::thread;

pub(crate) const BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES_ENV: &str =
    "BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES";

const DEFAULT_MODAL_MAX_ACTIVE_SANDBOXES: usize = 64;
const MODAL_LAUNCHER_LOG_TAIL_BYTES: u64 = 1024 * 1024;

fn modal_active_sandbox_limiter() -> &'static ActiveRuntimeLimiter {
    static LIMITER: OnceLock<ActiveRuntimeLimiter> = OnceLock::new();
    LIMITER.get_or_init(ActiveRuntimeLimiter::new)
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

fn acquire_modal_active_sandbox_permit(
    request: &AdapterRunRequest<'_>,
) -> Result<ActiveRuntimePermit> {
    let units = planned_modal_active_sandbox_units(request)?;
    modal_active_sandbox_limiter().acquire(
        units,
        active_runtime_limit(
            BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES_ENV,
            DEFAULT_MODAL_MAX_ACTIVE_SANDBOXES,
        ),
        "Modal sandboxes",
        BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES_ENV,
    )
}

#[cfg(test)]
pub(crate) fn planned_modal_active_sandbox_units_for_test(
    request: &AdapterRunRequest<'_>,
) -> Result<usize> {
    planned_modal_active_sandbox_units(request)
}

#[cfg(test)]
pub(crate) fn acquire_modal_active_sandbox_permit_for_test(
    request: &AdapterRunRequest<'_>,
) -> Result<ActiveRuntimePermit> {
    acquire_modal_active_sandbox_permit(request)
}

fn normalized_modal_remote_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("modal remote path must not be empty"));
    }
    let path = Path::new(trimmed);
    if !path.is_absolute() {
        return Err(anyhow!("modal remote path must be absolute: {}", trimmed));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(anyhow!(
                        "modal remote path contains non-utf8 segment: {}",
                        trimmed
                    ));
                };
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(anyhow!(
                    "modal remote path must not contain '..': {}",
                    trimmed
                ));
            }
            Component::Prefix(_) => {
                return Err(anyhow!(
                    "modal remote path must be a Unix-style absolute path: {}",
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

fn modal_remote_path_contains(parent: &str, child: &str) -> bool {
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
        let target = normalized_modal_remote_path(remote_path)?;
        let local_path = copy
            .get("local_path")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        for (previous_target, previous_local) in &seen {
            if modal_remote_path_contains(&target, previous_target) {
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

impl S3CompatibleRuntimeSync {
    fn from_env(run_id: &str, trial_id: &str, attempt_no: u32) -> Result<Self> {
        let bucket = env_var_with_legacy("BUCEPHALUS_MODAL_S3_BUCKET")
            .or_else(|_| env_var_with_legacy("BUCEPHALUS_S3_BUCKET"))
            .map_err(|_| {
                anyhow!(
                    "executor modal requires BUCEPHALUS_MODAL_S3_BUCKET or BUCEPHALUS_S3_BUCKET"
                )
            })?;
        let base_prefix = env_var_with_legacy("BUCEPHALUS_MODAL_S3_PREFIX")
            .or_else(|_| env_var_with_legacy("BUCEPHALUS_S3_PREFIX"))
            .unwrap_or_else(|_| "bucephalus-runs".to_string());
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
            endpoint_url: env_var_with_legacy("BUCEPHALUS_MODAL_S3_ENDPOINT_URL")
                .or_else(|_| env_var_with_legacy("BUCEPHALUS_S3_ENDPOINT_URL"))
                .ok(),
            region: env_var_with_legacy("BUCEPHALUS_MODAL_S3_REGION")
                .or_else(|_| std::env::var("AWS_REGION"))
                .ok(),
            modal_secret_name: env_var_with_legacy("BUCEPHALUS_MODAL_S3_SECRET").ok(),
            force_path_style: env_flag("BUCEPHALUS_MODAL_S3_FORCE_PATH_STYLE")
                || env_flag("BUCEPHALUS_S3_FORCE_PATH_STYLE"),
        })
    }

    fn uri_for_contract_path(&self, path: &str) -> String {
        let rel = path
            .trim_start_matches("/bucephalus/")
            .trim_start_matches("bucephalus/")
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
    go: String,
}

impl ModalExecutionBackend {
    pub(crate) fn from_env() -> Self {
        Self {
            app_name: env_var_with_legacy("BUCEPHALUS_MODAL_APP_NAME")
                .unwrap_or_else(|_| "bucephalus-runner".to_string()),
            environment_name: env_var_with_legacy("BUCEPHALUS_MODAL_ENVIRONMENT").ok(),
            go: env_var_with_legacy("BUCEPHALUS_MODAL_GO").unwrap_or_else(|_| "go".to_string()),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(app_name: &str, environment_name: Option<&str>) -> Self {
        Self {
            app_name: app_name.to_string(),
            environment_name: environment_name.map(str::to_string),
            go: "go".to_string(),
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
    run_modal_cleanup(&backend.go, request.trial_dir, &worker_ids)
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
    go: &str,
    trial_dir: &Path,
    worker_ids: &[String],
) -> Result<RuntimeCleanupOutcome> {
    let modal_dir = trial_dir.join("modal");
    ensure_dir(&modal_dir)?;
    let spec_path = modal_dir.join("cleanup.json");
    fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({ "sandbox_ids": worker_ids }))?,
    )?;
    let command = modal_go_launcher_command(go, &modal_dir, "cleanup", &spec_path)?;
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
        .find_map(|line| line.strip_prefix("BUCEPHALUS_MODAL_CLEANUP="))
        .ok_or_else(|| {
            anyhow!(
                "modal cleanup launcher did not emit BUCEPHALUS_MODAL_CLEANUP in {}",
                output.stdout_path.display()
            )
        })?;
    let value: Value = serde_json::from_str(&marker)?;
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
    let launcher_dispatched_at = Utc::now().to_rfc3339();
    let launch_started_at = Instant::now();
    let lifecycle_context = ModalLauncherLifecycleContext {
        run_dir: request.package_root.to_path_buf(),
        run_id: request.run_id.to_string(),
        trial_id: trial_id.to_string(),
        schedule_idx,
        attempt: attempt_no as usize,
    };
    let modal_result = run_modal_launch(&backend.go, trial_dir, &launch, lifecycle_context)?;
    let perf_context = PerfSpanContext {
        request,
        trial_id,
        schedule_idx,
        attempt_no,
    };
    let modal_detail = || {
        let mut detail = serde_json::Map::new();
        detail.insert("executor".to_string(), json!("modal"));
        detail.insert(
            "sandbox_id".to_string(),
            json!(modal_result.sandbox_id.as_deref()),
        );
        detail.insert(
            "process_id".to_string(),
            json!(modal_result.process_id.as_deref()),
        );
        detail.insert(
            "agent_command_started_at".to_string(),
            json!(modal_result.agent_command_started_at.as_deref()),
        );
        detail.insert(
            "runtime_transfer_archive_bytes".to_string(),
            json!(modal_result.runtime_transfer_archive_bytes),
        );
        detail.insert("sync".to_string(), json!(sync.kind_label()));
        detail
    };
    record_timestamp_delta(
        &perf_context,
        "modal_runner_dispatch_to_launcher_main",
        "launcher_dispatched_at",
        Some(&launcher_dispatched_at),
        "launcher_main_started_at",
        modal_result.timing("launcher_main_started_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_launcher_main_to_sandbox_create_start",
        "launcher_main_started_at",
        modal_result.timing("launcher_main_started_at"),
        "sandbox_create_started_at",
        modal_result.timing("sandbox_create_started_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_app_lookup",
        "app_lookup_started_at",
        modal_result.timing("app_lookup_started_at"),
        "app_lookup_ended_at",
        modal_result.timing("app_lookup_ended_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_runtime_transfer_archive_build",
        "runtime_transfer_archive_build_started_at",
        modal_result.timing("runtime_transfer_archive_build_started_at"),
        "runtime_transfer_archive_build_ended_at",
        modal_result.timing("runtime_transfer_archive_build_ended_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_launch_mounts_prepare",
        "launch_mounts_prepare_started_at",
        modal_result.timing("launch_mounts_prepare_started_at"),
        "launch_mounts_prepare_ended_at",
        modal_result.timing("launch_mounts_prepare_ended_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_sandbox_create",
        "sandbox_create_started_at",
        modal_result.timing("sandbox_create_started_at"),
        "sandbox_create_ended_at",
        modal_result.timing("sandbox_create_ended_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_sandbox_create_to_exec_submit",
        "sandbox_create_ended_at",
        modal_result.timing("sandbox_create_ended_at"),
        "agent_exec_submit_started_at",
        modal_result.started_at.as_deref(),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_exec_submit_to_container_start",
        "agent_exec_submit_started_at",
        modal_result.started_at.as_deref(),
        "container_started_at",
        modal_result.container_started_at.as_deref(),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_container_start_to_agent_command_start",
        "container_started_at",
        modal_result.container_started_at.as_deref(),
        "agent_command_started_at",
        modal_result.agent_command_started_at.as_deref(),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_exec_submit_to_agent_command_start",
        "agent_exec_submit_started_at",
        modal_result.started_at.as_deref(),
        "agent_command_started_at",
        modal_result.agent_command_started_at.as_deref(),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_agent_exec_runtime",
        "agent_exec_submit_started_at",
        modal_result.started_at.as_deref(),
        "agent_exec_ended_at",
        modal_result.ended_at.as_deref(),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_agent_command_runtime",
        "agent_command_started_at",
        modal_result.agent_command_started_at.as_deref(),
        "agent_exec_ended_at",
        modal_result.ended_at.as_deref(),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_agent_end_to_result_available",
        "agent_exec_ended_at",
        modal_result.ended_at.as_deref(),
        "result_available_at",
        modal_result.timing("result_available_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_result_copy_to_available",
        "result_copy_started_at",
        modal_result.timing("result_copy_started_at"),
        "result_available_at",
        modal_result.timing("result_available_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_launcher_dispatch_to_result_available",
        "launcher_dispatched_at",
        Some(&launcher_dispatched_at),
        "result_available_at",
        modal_result.timing("result_available_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_result_available_to_rust_receive",
        "result_available_at",
        modal_result.timing("result_available_at"),
        "rust_result_marker_received_at",
        modal_result.timing("rust_result_marker_received_at"),
        modal_detail(),
    )?;
    if let Some(container_started_at) = modal_result.container_started_at.as_deref() {
        let dispatch_to_container_start_ms =
            rfc3339_delta_ms(&launcher_dispatched_at, container_started_at)?;
        crate::perf::record(crate::perf::PerfRecord {
            run_dir: request.package_root,
            run_id: request.run_id,
            trial_id: Some(trial_id),
            schedule_idx: Some(schedule_idx),
            attempt: Some(attempt_no as usize),
            sample_kind: "duration",
            stage: "modal_launcher_dispatch_to_container_start",
            duration_ms: Some(dispatch_to_container_start_ms),
            detail: json!({
                "launcher_dispatched_at": launcher_dispatched_at,
                "container_started_at": container_started_at,
                "agent_exec_submit_started_at": modal_result.started_at.as_deref(),
                "agent_command_started_at": modal_result.agent_command_started_at.as_deref(),
                "sandbox_id": modal_result.sandbox_id.as_deref(),
                "process_id": modal_result.process_id.as_deref(),
                "sync": sync.kind_label(),
            }),
        })?;
        crate::perf::record(crate::perf::PerfRecord {
            run_dir: request.package_root,
            run_id: request.run_id,
            trial_id: Some(trial_id),
            schedule_idx: Some(schedule_idx),
            attempt: Some(attempt_no as usize),
            sample_kind: "duration",
            stage: "backend_dispatch_to_container_start",
            duration_ms: Some(dispatch_to_container_start_ms),
            detail: json!({
                "executor": "modal",
                "dispatch_boundary": "runner_launches_modal_launcher",
                "start_boundary": "first_instruction_inside_agent_exec",
                "launcher_dispatched_at": launcher_dispatched_at,
                "container_started_at": container_started_at,
                "agent_exec_submit_started_at": modal_result.started_at.as_deref(),
                "agent_command_started_at": modal_result.agent_command_started_at.as_deref(),
                "sandbox_id": modal_result.sandbox_id.as_deref(),
                "process_id": modal_result.process_id.as_deref(),
                "sync": sync.kind_label(),
            }),
        })?;
    }
    if let Some(agent_command_started_at) = modal_result.agent_command_started_at.as_deref() {
        let dispatch_to_agent_command_start_ms =
            rfc3339_delta_ms(&launcher_dispatched_at, agent_command_started_at)?;
        crate::perf::record(crate::perf::PerfRecord {
            run_dir: request.package_root,
            run_id: request.run_id,
            trial_id: Some(trial_id),
            schedule_idx: Some(schedule_idx),
            attempt: Some(attempt_no as usize),
            sample_kind: "duration",
            stage: "backend_dispatch_to_agent_command_start",
            duration_ms: Some(dispatch_to_agent_command_start_ms),
            detail: json!({
                "executor": "modal",
                "dispatch_boundary": "runner_launches_modal_launcher",
                "start_boundary": "final_agent_command_exec",
                "launcher_dispatched_at": launcher_dispatched_at,
                "container_started_at": modal_result.container_started_at.as_deref(),
                "agent_exec_submit_started_at": modal_result.started_at.as_deref(),
                "agent_command_started_at": agent_command_started_at,
                "sandbox_id": modal_result.sandbox_id.as_deref(),
                "process_id": modal_result.process_id.as_deref(),
                "sync": sync.kind_label(),
            }),
        })?;
    }
    crate::perf::record_duration(
        request.package_root,
        request.run_id,
        Some(trial_id),
        Some(schedule_idx),
        Some(attempt_no as usize),
        "modal_sandbox_run",
        launch_started_at,
        json!({
            "boundary": "launcher_dispatch_to_result_marker_received",
            "note": "post-result durable export and sandbox cleanup are recorded separately",
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
            .agent_command_started_at
            .clone()
            .or_else(|| modal_result.container_started_at.clone())
            .or_else(|| modal_result.started_at.clone())
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
    outcome.stdout = local_blob_if_present(trial_agent_stdout_path(trial_dir));
    outcome.stderr = local_blob_if_present(trial_agent_stderr_path(trial_dir));

    let event_sink = request.runtime.event_sinks.first();
    let retain_raw_events = event_sink.map(|sink| sink.persist).unwrap_or(false);
    let ingest_events = event_sink.map(|sink| sink.ingest).unwrap_or(true);
    if retain_raw_events {
        outcome.events = remote_blob_if_present(
            &request.io_paths.events_host,
            sync.uri_for_contract_path(BUCEPHALUS_EVENTS_DURABLE_PATH),
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

const MODAL_STDOUT_CONTRACT_PATH: &str = "/bucephalus/out/stdout.log";
const MODAL_STDERR_CONTRACT_PATH: &str = "/bucephalus/out/stderr.log";

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

#[derive(Clone)]
struct ModalLauncherLifecycleContext {
    run_dir: PathBuf,
    run_id: String,
    trial_id: String,
    schedule_idx: usize,
    attempt: usize,
}

pub(crate) struct ModalSandboxResult {
    pub(crate) sandbox_id: Option<String>,
    pub(crate) process_id: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) started_at: Option<String>,
    pub(crate) container_started_at: Option<String>,
    pub(crate) agent_command_started_at: Option<String>,
    pub(crate) ended_at: Option<String>,
    pub(crate) runtime_transfer_archive_bytes: Option<u64>,
    pub(crate) timings: BTreeMap<String, String>,
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
    #[allow(dead_code)]
    container_started_at: Option<String>,
    #[allow(dead_code)]
    agent_command_started_at: Option<String>,
    ended_at: Option<String>,
}

impl ModalSandboxResult {
    fn exec_phase(&self, phase: &str) -> Option<&ModalExecPhaseResult> {
        self.execs
            .iter()
            .find(|exec| exec.phase.as_deref() == Some(phase))
    }

    fn timing(&self, key: &str) -> Option<&str> {
        self.timings.get(key).map(String::as_str)
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
            BUCEPHALUS_ENV_AGENT_EXIT_STATUS,
            "__BUCEPHALUS_AGENT_EXIT_STATUS__",
        )),
        false,
    );
    env.insert(
        BUCEPHALUS_ENV_RESULT_PATH.to_string(),
        request.io_paths.result_path.clone(),
    );
    env.insert(
        BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH.to_string(),
        request.io_paths.mapped_grader_output_path.clone(),
    );
    env.insert(
        BUCEPHALUS_ENV_TRAJECTORY_PATH.to_string(),
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
                "remote_path": "/bucephalus/out/grader_stdout.log",
                "local_path": trial_grader_stdout_path(trial_dir),
            },
            "stderr": {
                "remote_path": "/bucephalus/out/grader_stderr.log",
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

fn modal_secret_env_names(request: &AdapterRunRequest<'_>) -> Vec<String> {
    let Some(secrets) = request
        .runtime_experiment
        .pointer("/runtime/secrets")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut names = BTreeSet::new();
    for secret in secrets {
        let Some(name) = secret
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if request.runtime_env.contains_key(name)
            || request.runtime_overrides_env.contains_key(name)
        {
            names.insert(name.to_string());
        }
    }
    names.into_iter().collect()
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
        BUCEPHALUS_ENV_TRIAL_INPUT_PATH.to_string(),
        request.io_paths.trial_input_path.clone(),
    );
    env.insert(
        BUCEPHALUS_ENV_RESULT_PATH.to_string(),
        request.io_paths.result_path.clone(),
    );
    env.insert(
        BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH.to_string(),
        request.io_paths.mapped_grader_output_path.clone(),
    );
    env.insert(
        BUCEPHALUS_ENV_TRAJECTORY_PATH.to_string(),
        request.io_paths.trajectory_path.clone(),
    );
    let secret_env = modal_secret_env_names(request);
    let mut sandbox_env = env.clone();
    for name in &secret_env {
        sandbox_env.remove(name);
    }

    let mut launch_mounts = Vec::new();
    let mut runtime_files = vec![
        json!({
            "local_path": request.trial_paths.in_dir,
            "remote_path": BUCEPHALUS_CONTRACT_IN_DIR,
            "priority": "runtime_transfer",
        }),
        json!({
            "local_path": request.trial_paths.workspace,
            "remote_path": BUCEPHALUS_CONTRACT_WORKSPACE_DIR,
            "priority": "runtime_transfer",
        }),
        json!({
            "local_path": request.trial_paths.state,
            "remote_path": "/bucephalus/state",
            "priority": "runtime_transfer",
        }),
    ];
    for mount in request.dynamic_mounts {
        if mount.read_only
            && (mount.mount_path == "/bucephalus/case_assets"
                || mount.mount_path.starts_with("/bucephalus/case_assets/"))
        {
            launch_mounts.push(json!({
                "local_path": mount.host_path,
                "remote_path": mount.mount_path,
                "source_is_dir": mount.host_path.is_dir(),
                "priority": "launch_required",
            }));
            continue;
        }
        runtime_files.push(json!({
            "local_path": mount.host_path,
            "remote_path": mount.mount_path,
            "priority": "runtime_transfer",
        }));
    }
    for mount in request.secret_file_mounts {
        runtime_files.push(json!({
            "local_path": mount.source_from_host,
            "remote_path": mount.target_path,
            "priority": "runtime_transfer",
        }));
        if let Some(cache) = mount.credential_cache.as_ref() {
            runtime_files.push(json!({
                "local_path": cache.host_dir,
                "remote_path": cache.target_dir,
                "priority": "runtime_transfer",
            }));
        }
    }
    if task_sandbox_plan.artifact_mount.is_some() {
        let bundle = request.agent_artifact.ok_or_else(|| {
            anyhow!("task sandbox plan requires an agent artifact mount but request has no agent artifact")
        })?;
        let mount_path = request.agent_artifact_mount_path.ok_or_else(|| {
            anyhow!("trial_runtime.agent.artifact.mount.path is required when artifact is set")
        })?;
        runtime_files.push(json!({
            "local_path": resolve_agent_artifact_mount_dir(bundle)?,
            "remote_path": mount_path,
            "priority": "runtime_transfer",
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
                runtime_files.push(json!({
                    "local_path": local_path,
                    "remote_path": source,
                    "priority": "runtime_transfer",
                }));
            }
        }
    }
    validate_modal_copy_targets(&runtime_files)?;

    let cpu_count = request
        .runtime_experiment
        .pointer("/policy/task_sandbox/resources/cpu_count")
        .and_then(Value::as_u64);
    let memory_mb = request
        .runtime_experiment
        .pointer("/policy/task_sandbox/resources/memory_mb")
        .and_then(Value::as_u64);
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
            "env": sandbox_env,
            "secret_env": secret_env,
            "block_network": request.network_mode == "none",
            "cpu_count": cpu_count,
            "memory_mb": memory_mb,
            "poll_interval_ms": 1000,
            "sandbox_timeout_seconds": timeout_secs.saturating_add(60),
            "execs": [{
                "phase": "agent",
                "command": command,
                "env": sandbox_env,
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
            "launch_mounts": launch_mounts,
            "runtime_files": runtime_files,
            "result": {
                "remote_path": request.io_paths.result_path,
                "local_path": request.io_paths.result_host,
            },
            "trial_input": {
                "remote_path": request.io_paths.trial_input_path,
                "local_path": request.io_paths.trial_input_host,
            },
            "events": {
                // The agent appends here on plain container disk (never the
                // CloudBucketMount, which rejects appends).
                "scratch_path": request.io_paths.trajectory_path,
                "local_path": request.io_paths.events_host,
                // When the stream is retained, the launcher flushes the
                // completed file to blob storage as a single whole-file write.
                "durable_path": request
                    .runtime
                    .event_sinks
                    .first()
                    .map(|sink| sink.persist)
                    .unwrap_or(false)
                    .then_some(BUCEPHALUS_EVENTS_DURABLE_PATH),
            },
            "transport_envelope": {
                "remote_path": "/bucephalus/out/runtime_transport_envelope.json",
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

fn modal_runtime_transfer_archive_path(modal_dir: &Path) -> PathBuf {
    modal_dir.join("runtime_transfer.tar.gz")
}

fn normalized_modal_archive_name(remote_path: &str) -> Result<String> {
    let normalized = normalized_modal_remote_path(remote_path)?;
    if normalized == "/" {
        return Err(anyhow!("runtime transfer remote_path must not target /"));
    }
    Ok(normalized.trim_start_matches('/').to_string())
}

fn append_modal_archive_dir(
    archive: &mut tar::Builder<flate2::write::GzEncoder<File>>,
    name: &str,
) -> Result<()> {
    let name = name.trim_matches('/');
    if name.is_empty() {
        return Ok(());
    }
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(EntryType::Directory);
    header.set_mode(0o755);
    header.set_uid(0);
    header.set_gid(0);
    header.set_username("")?;
    header.set_groupname("")?;
    header.set_mtime(0);
    header.set_size(0);
    header.set_cksum();
    archive.append_data(&mut header, name, std::io::empty())?;
    Ok(())
}

fn append_modal_archive_parent_dirs(
    archive: &mut tar::Builder<flate2::write::GzEncoder<File>>,
    name: &str,
) -> Result<()> {
    let mut current = PathBuf::new();
    if let Some(parent) = Path::new(name).parent() {
        for component in parent.components() {
            let Component::Normal(part) = component else {
                continue;
            };
            current.push(part);
            append_modal_archive_dir(archive, &current.to_string_lossy())?;
        }
    }
    Ok(())
}

fn append_modal_archive_file(
    archive: &mut tar::Builder<flate2::write::GzEncoder<File>>,
    source: &Path,
    name: &str,
) -> Result<()> {
    append_modal_archive_parent_dirs(archive, name)?;
    let mut file = File::open(source)
        .with_context(|| format!("open modal runtime transfer source {}", source.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("stat modal runtime transfer source {}", source.display()))?;
    let mut header = tar::Header::new_gnu();
    header.set_metadata(&metadata);
    header.set_uid(0);
    header.set_gid(0);
    header.set_username("")?;
    header.set_groupname("")?;
    header.set_mtime(0);
    header.set_cksum();
    archive.append_data(&mut header, name, &mut file)?;
    Ok(())
}

fn append_modal_runtime_path_to_archive(
    archive: &mut tar::Builder<flate2::write::GzEncoder<File>>,
    local_path: &Path,
    remote_path: &str,
) -> Result<()> {
    if !local_path.exists() {
        return Err(anyhow!(
            "modal runtime transfer source does not exist: {}",
            local_path.display()
        ));
    }
    let remote_name = normalized_modal_archive_name(remote_path)?;
    if local_path.is_dir() {
        append_modal_archive_dir(archive, &remote_name)?;
        let root = local_path
            .canonicalize()
            .with_context(|| format!("canonicalize modal copy root {}", local_path.display()))?;
        for entry in walkdir::WalkDir::new(local_path)
            .follow_links(false)
            .sort_by_file_name()
            .min_depth(1)
        {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(local_path).with_context(|| {
                format!(
                    "compute modal runtime transfer relative path for {} under {}",
                    path.display(),
                    local_path.display()
                )
            })?;
            let dst = Path::new(&remote_name).join(relative).to_string_lossy().to_string();
            let metadata = fs::symlink_metadata(path)
                .with_context(|| format!("stat modal runtime transfer source {}", path.display()))?;
            if metadata.file_type().is_symlink() {
                let resolved = path.canonicalize().with_context(|| {
                    format!("resolve modal runtime transfer symlink {}", path.display())
                })?;
                if !resolved.starts_with(&root) {
                    return Err(anyhow!(
                        "refusing to archive symlink outside directory artifact: {}",
                        path.display()
                    ));
                }
                if resolved.is_file() {
                    append_modal_archive_file(archive, &resolved, &dst)?;
                } else if resolved.is_dir() {
                    append_modal_archive_dir(archive, &dst)?;
                }
            } else if metadata.is_dir() {
                append_modal_archive_dir(archive, &dst)?;
            } else if metadata.is_file() {
                append_modal_archive_file(archive, path, &dst)?;
            }
        }
    } else {
        let source = if fs::symlink_metadata(local_path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            local_path.canonicalize().with_context(|| {
                format!("resolve modal runtime transfer source {}", local_path.display())
            })?
        } else {
            local_path.to_path_buf()
        };
        append_modal_archive_file(archive, &source, &remote_name)?;
    }
    Ok(())
}

fn build_modal_runtime_transfer_archive(modal_dir: &Path, launch: &ModalLaunchSpec) -> Result<PathBuf> {
    let archive_path = modal_runtime_transfer_archive_path(modal_dir);
    let file = File::create(&archive_path)
        .with_context(|| format!("create modal runtime transfer archive {}", archive_path.display()))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for directory in [
        "/bucephalus/in",
        "/bucephalus/out",
        "/bucephalus/state",
        "/bucephalus/workspace",
        "/bucephalus/tmp",
        "/bucephalus-events",
    ] {
        append_modal_archive_dir(&mut archive, &normalized_modal_archive_name(directory)?)?;
    }
    for item in launch
        .value
        .get("runtime_files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let local_path = item
            .get("local_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("modal runtime transfer entry missing local_path"))?;
        let remote_path = item
            .get("remote_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("modal runtime transfer entry missing remote_path"))?;
        append_modal_runtime_path_to_archive(&mut archive, Path::new(local_path), remote_path)?;
    }
    archive.finish()?;
    let encoder = archive.into_inner()?;
    encoder.finish()?;
    Ok(archive_path)
}

fn modal_go_launcher_command(
    go: &str,
    modal_dir: &Path,
    mode: &str,
    spec_path: &Path,
) -> Result<Command> {
    fs::write(modal_dir.join("go.mod"), MODAL_LAUNCHER_GO_MOD)?;
    fs::write(modal_dir.join("main.go"), MODAL_LAUNCHER_GO_SOURCE)?;
    let mut command = Command::new(go);
    command
        .current_dir(modal_dir)
        .env("GOTOOLCHAIN", "auto")
        .arg("run")
        .arg(".")
        .arg(mode)
        .arg(spec_path);
    Ok(command)
}

fn run_modal_launch(
    go: &str,
    trial_dir: &Path,
    launch: &ModalLaunchSpec,
    lifecycle_context: ModalLauncherLifecycleContext,
) -> Result<ModalSandboxResult> {
    let modal_dir = trial_dir.join("modal");
    ensure_dir(&modal_dir)?;
    let runtime_transfer_archive = build_modal_runtime_transfer_archive(&modal_dir, launch)?;
    let spec_path = modal_dir.join("launch.json");
    let mut launch_value = launch.value.clone();
    let launch_object = launch_value
        .as_object_mut()
        .ok_or_else(|| anyhow!("modal launch spec must be a JSON object"))?;
    launch_object.insert(
        "runtime_transfer_archive".to_string(),
        json!(runtime_transfer_archive),
    );
    fs::write(&spec_path, serde_json::to_vec_pretty(&launch_value)?)?;
    let stdout_path = modal_dir.join("sandbox_stdout.log");
    let stderr_path = modal_dir.join("sandbox_stderr.log");
    let stdout_log = File::create(&stdout_path)
        .with_context(|| format!("create modal launcher stdout log {}", stdout_path.display()))?;
    let stderr = File::create(&stderr_path)
        .with_context(|| format!("create modal launcher stderr log {}", stderr_path.display()))?;
    let mut command = modal_go_launcher_command(go, &modal_dir, "launch", &spec_path)?;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("spawn modal sandbox launcher command")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("modal sandbox launcher stdout pipe was not available"))?;
    let mut reader = BufReader::new(stdout);
    let mut stdout_log = stdout_log;
    let mut line = String::new();
    let marker = loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .context("read modal sandbox launcher stdout")?;
        if bytes == 0 {
            let status = child.wait().context("wait for modal sandbox launcher")?;
            return Err(anyhow!(
                "modal sandbox launcher exited before emitting BUCEPHALUS_MODAL_RESULT with status {:?}\nstdout:\n{}\nstderr:\n{}",
                status.code(),
                read_file_tail_lossy(&stdout_path, MODAL_LAUNCHER_LOG_TAIL_BYTES)?,
                read_file_tail_lossy(&stderr_path, MODAL_LAUNCHER_LOG_TAIL_BYTES)?
            ));
        }
        stdout_log.write_all(line.as_bytes()).with_context(|| {
            format!("write modal launcher stdout log {}", stdout_path.display())
        })?;
        stdout_log.flush().with_context(|| {
            format!("flush modal launcher stdout log {}", stdout_path.display())
        })?;
        if let Some(marker) = line.strip_prefix("BUCEPHALUS_MODAL_RESULT=") {
            break marker.trim_end().to_string();
        }
    };
    let result_marker_received_at = Instant::now();
    let result_marker_received_at_wall = Utc::now().to_rfc3339();
    let background_context = lifecycle_context.clone();
    thread::spawn(move || {
        let mut lifecycle_marker: Option<String> = None;
        let mut background_line = String::new();
        loop {
            background_line.clear();
            match reader.read_line(&mut background_line) {
                Ok(0) => break,
                Ok(_) => {
                    let _ = stdout_log.write_all(background_line.as_bytes());
                    if let Some(marker) =
                        background_line.strip_prefix("BUCEPHALUS_MODAL_LIFECYCLE=")
                    {
                        lifecycle_marker = Some(marker.trim_end().to_string());
                    }
                }
                Err(_) => break,
            }
        }
        let _ = stdout_log.flush();
        let status = child.wait().ok();
        record_modal_background_lifecycle(
            background_context,
            result_marker_received_at,
            status.and_then(|status| status.code()),
            lifecycle_marker.as_deref(),
        );
    });
    let mut value: Value = serde_json::from_str(&marker)?;
    if let Some(timings) = value.get_mut("timings").and_then(Value::as_object_mut) {
        timings.insert(
            "rust_result_marker_received_at".to_string(),
            json!(result_marker_received_at_wall),
        );
    }
    parse_modal_sandbox_result(&value)
}

fn record_modal_background_lifecycle(
    context: ModalLauncherLifecycleContext,
    result_marker_received_at: Instant,
    launcher_exit_code: Option<i32>,
    lifecycle_marker: Option<&str>,
) {
    let _ = crate::perf::record_duration(
        &context.run_dir,
        &context.run_id,
        Some(&context.trial_id),
        Some(context.schedule_idx),
        Some(context.attempt),
        "modal_result_available_to_launcher_exit",
        result_marker_received_at,
        json!({ "launcher_exit_code": launcher_exit_code }),
    );
    let Some(marker) = lifecycle_marker else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(marker) else {
        return;
    };
    let Some(timings) = value.get("timings").and_then(Value::as_object) else {
        return;
    };
    record_modal_lifecycle_delta(
        &context,
        "modal_durable_events_export",
        timings,
        "durable_events_export_started_at",
        "durable_events_export_ended_at",
    );
    record_modal_lifecycle_delta(
        &context,
        "modal_sandbox_cleanup",
        timings,
        "sandbox_cleanup_started_at",
        "sandbox_cleanup_ended_at",
    );
    record_modal_lifecycle_delta(
        &context,
        "modal_result_available_to_lifecycle_complete",
        timings,
        "result_available_at",
        "launcher_completed_at",
    );
}

fn record_modal_lifecycle_delta(
    context: &ModalLauncherLifecycleContext,
    stage: &'static str,
    timings: &serde_json::Map<String, Value>,
    start_key: &'static str,
    end_key: &'static str,
) {
    let (Some(started_at), Some(ended_at)) = (
        timings.get(start_key).and_then(Value::as_str),
        timings.get(end_key).and_then(Value::as_str),
    ) else {
        return;
    };
    let Ok(duration_ms) = rfc3339_delta_ms(started_at, ended_at) else {
        return;
    };
    let _ = crate::perf::record(crate::perf::PerfRecord {
        run_dir: &context.run_dir,
        run_id: &context.run_id,
        trial_id: Some(&context.trial_id),
        schedule_idx: Some(context.schedule_idx),
        attempt: Some(context.attempt),
        sample_kind: "duration",
        stage,
        duration_ms: Some(duration_ms),
        detail: json!({
            start_key: started_at,
            end_key: ended_at
        }),
    });
}

fn parse_modal_sandbox_result(value: &Value) -> Result<ModalSandboxResult> {
    let exec_values = value.get("execs").and_then(Value::as_array);
    let timings = value
        .get("timings")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
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
                    container_started_at: exec
                        .get("container_started_at")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    agent_command_started_at: exec
                        .get("agent_command_started_at")
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
        container_started_at: agent_exec
            .and_then(|exec| exec.get("container_started_at"))
            .and_then(Value::as_str)
            .map(str::to_string),
        agent_command_started_at: agent_exec
            .and_then(|exec| exec.get("agent_command_started_at"))
            .and_then(Value::as_str)
            .map(str::to_string),
        ended_at: agent_exec
            .and_then(|exec| exec.get("ended_at"))
            .or_else(|| value.get("ended_at"))
            .and_then(Value::as_str)
            .map(str::to_string),
        runtime_transfer_archive_bytes: value
            .get("runtime_transfer_archive_bytes")
            .and_then(Value::as_u64),
        timings,
        execs: exec_results,
    })
}

#[cfg(test)]
pub(crate) fn parse_modal_sandbox_result_for_test(value: &Value) -> Result<ModalSandboxResult> {
    parse_modal_sandbox_result(value)
}

fn env_flag(name: &str) -> bool {
    env_var_with_legacy(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

const MODAL_LAUNCHER_GO_MOD: &str = r#"module bucephalus-modal-launcher

go 1.23

require github.com/modal-labs/modal-client/go v0.7.6
"#;

const MODAL_LAUNCHER_GO_SOURCE: &str = r####"
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	modal "github.com/modal-labs/modal-client/go"
)

const runtimeTransferArchivePath = "/tmp/bucephalus-runtime-transfer.tar.gz"

type execRecord struct {
	Phase                 string  `json:"phase,omitempty"`
	SandboxID             string  `json:"sandbox_id,omitempty"`
	ProcessID             *string `json:"process_id"`
	ExitCode              int     `json:"exit_code"`
	TimedOut              bool    `json:"timed_out"`
	StartedAt             string  `json:"started_at"`
	ContainerStartedAt    *string `json:"container_started_at"`
	AgentCommandStartedAt *string `json:"agent_command_started_at"`
	EndedAt               string  `json:"ended_at"`
}

type launchResult struct {
	SandboxID                   *string           `json:"sandbox_id"`
	Execs                       []execRecord      `json:"execs"`
	ExitCode                    *int              `json:"exit_code"`
	TimedOut                    bool              `json:"timed_out"`
	StartedAt                   string            `json:"started_at"`
	EndedAt                     *string           `json:"ended_at"`
	RuntimeTransferArchiveBytes int64             `json:"runtime_transfer_archive_bytes"`
	Timings                     map[string]string `json:"timings"`
}

type readResult struct {
	data []byte
	err  error
}

func utcNow() string {
	return time.Now().UTC().Format("2006-01-02T15:04:05.000000Z")
}

func timingMark(timings map[string]string, key string) {
	timings[key] = utcNow()
}

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
	os.Exit(1)
}

func loadJSON(path string) (map[string]any, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var value map[string]any
	if err := json.Unmarshal(data, &value); err != nil {
		return nil, err
	}
	return value, nil
}

func jsonObject(value any) map[string]any {
	if value == nil {
		return map[string]any{}
	}
	if object, ok := value.(map[string]any); ok {
		return object
	}
	return map[string]any{}
}

func jsonObjects(value any) []map[string]any {
	items, ok := value.([]any)
	if !ok {
		return nil
	}
	out := make([]map[string]any, 0, len(items))
	for _, item := range items {
		out = append(out, jsonObject(item))
	}
	return out
}

func jsonObjectMap(value any) map[string]map[string]any {
	object, ok := value.(map[string]any)
	if !ok {
		return map[string]map[string]any{}
	}
	out := make(map[string]map[string]any, len(object))
	for key, value := range object {
		out[key] = jsonObject(value)
	}
	return out
}

func stringValue(object map[string]any, key string) string {
	value, _ := object[key].(string)
	return value
}

func optionalString(object map[string]any, key string) (string, bool) {
	value, ok := object[key].(string)
	return value, ok && value != ""
}

func boolValue(object map[string]any, key string) bool {
	value, ok := object[key].(bool)
	return ok && value
}

func intValue(object map[string]any, key string, fallback int) int {
	switch value := object[key].(type) {
	case float64:
		return int(value)
	case int:
		return value
	case json.Number:
		parsed, err := value.Int64()
		if err == nil {
			return int(parsed)
		}
	}
	return fallback
}

func stringList(value any) []string {
	items, ok := value.([]any)
	if !ok {
		return nil
	}
	out := make([]string, 0, len(items))
	for _, item := range items {
		if text, ok := item.(string); ok {
			out = append(out, text)
		}
	}
	return out
}

func stringMap(value any) map[string]string {
	object, ok := value.(map[string]any)
	if !ok {
		return map[string]string{}
	}
	out := make(map[string]string, len(object))
	for key, value := range object {
		if text, ok := value.(string); ok {
			out[key] = text
		} else if value != nil {
			encoded, _ := json.Marshal(value)
			out[key] = string(encoded)
		}
	}
	return out
}

func marker(prefix string, value any) {
	data, err := json.Marshal(value)
	if err != nil {
		fail("marshal %s: %v", prefix, err)
	}
	fmt.Printf("%s=%s\n", prefix, data)
}

func requiredEnv(name string) (string, error) {
	value := os.Getenv(name)
	if value == "" {
		return "", fmt.Errorf("%s is required for Modal S3-compatible sync", name)
	}
	return value, nil
}

func buildSecret(ctx context.Context, mc *modal.Client, sync map[string]any) (*modal.Secret, error) {
	if secretName, ok := optionalString(sync, "modal_secret_name"); ok {
		return mc.Secrets.FromName(ctx, secretName, nil)
	}
	accessKey, err := requiredEnv("AWS_ACCESS_KEY_ID")
	if err != nil {
		return nil, err
	}
	secretKey, err := requiredEnv("AWS_SECRET_ACCESS_KEY")
	if err != nil {
		return nil, err
	}
	data := map[string]string{
		"AWS_ACCESS_KEY_ID":     accessKey,
		"AWS_SECRET_ACCESS_KEY": secretKey,
	}
	if token := os.Getenv("AWS_SESSION_TOKEN"); token != "" {
		data["AWS_SESSION_TOKEN"] = token
	}
	if region, ok := optionalString(sync, "region"); ok {
		data["AWS_REGION"] = region
	} else if region := os.Getenv("AWS_REGION"); region != "" {
		data["AWS_REGION"] = region
	}
	return mc.Secrets.FromMap(ctx, data, nil)
}

func buildBucketMount(ctx context.Context, mc *modal.Client, sync map[string]any, keyPrefix string, readOnly bool) (*modal.CloudBucketMount, error) {
	if keyPrefix != "" && !strings.HasSuffix(keyPrefix, "/") {
		keyPrefix += "/"
	}
	if boolValue(sync, "force_path_style") {
		return nil, errors.New("BUCEPHALUS_MODAL_S3_FORCE_PATH_STYLE is not supported by Modal's Go SDK CloudBucketMount API")
	}
	secret, err := buildSecret(ctx, mc, sync)
	if err != nil {
		return nil, err
	}
	params := &modal.CloudBucketMountParams{Secret: secret, ReadOnly: readOnly}
	if keyPrefix != "" {
		params.KeyPrefix = &keyPrefix
	}
	if endpoint, ok := optionalString(sync, "endpoint_url"); ok {
		params.BucketEndpointURL = &endpoint
	}
	return mc.CloudBucketMounts.New(stringValue(sync, "bucket"), params)
}

func buildAgentSecret(ctx context.Context, mc *modal.Client, spec map[string]any) (*modal.Secret, error) {
	names := stringList(spec["secret_env"])
	if len(names) == 0 {
		return nil, nil
	}
	data := make(map[string]string, len(names))
	for _, name := range names {
		value, err := requiredEnv(name)
		if err != nil {
			return nil, err
		}
		data[name] = value
	}
	return mc.Secrets.FromMap(ctx, data, nil)
}

func appLookup(ctx context.Context, mc *modal.Client, appName string, environmentName string) (*modal.App, error) {
	return mc.Apps.FromName(ctx, appName, &modal.AppFromNameParams{
		Environment:     environmentName,
		CreateIfMissing: true,
	})
}

func runtimeWorkersPath(specPath string) string {
	return filepath.Join(filepath.Dir(specPath), "runtime_workers.json")
}

func writeRuntimeWorker(specPath, role string, sandbox *modal.Sandbox) {
	if sandbox == nil || sandbox.SandboxID == "" {
		return
	}
	path := runtimeWorkersPath(specPath)
	payload := map[string]any{"workers": []any{}}
	if data, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(data, &payload)
	}
	workers, _ := payload["workers"].([]any)
	for _, item := range workers {
		object := jsonObject(item)
		if stringValue(object, "role") == role && stringValue(object, "sandbox_id") == sandbox.SandboxID {
			return
		}
	}
	workers = append(workers, map[string]any{
		"role":        role,
		"sandbox_id":  sandbox.SandboxID,
		"recorded_at": utcNow(),
	})
	payload["workers"] = workers
	data, _ := json.MarshalIndent(payload, "", "  ")
	_ = os.WriteFile(path, data, 0o644)
}

func makeDir(ctx context.Context, fsys *modal.SandboxFilesystem, remotePath string) error {
	return fsys.MakeDirectory(ctx, remotePath, nil)
}

func copyPath(ctx context.Context, fsys *modal.SandboxFilesystem, localPath, remotePath string) error {
	info, err := os.Stat(localPath)
	if err != nil {
		return err
	}
	if !info.IsDir() {
		parent := path.Dir(remotePath)
		if parent != "." && parent != "/" {
			if err := makeDir(ctx, fsys, parent); err != nil {
				return err
			}
		}
		return fsys.CopyFromLocal(ctx, localPath, remotePath, nil)
	}
	if err := makeDir(ctx, fsys, remotePath); err != nil {
		return err
	}
	root, err := filepath.EvalSymlinks(localPath)
	if err != nil {
		return err
	}
	return filepath.WalkDir(localPath, func(current string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if current == localPath {
			return nil
		}
		rel, err := filepath.Rel(localPath, current)
		if err != nil {
			return err
		}
		dst := strings.TrimRight(remotePath, "/") + "/" + filepath.ToSlash(rel)
		if entry.Type()&os.ModeSymlink != 0 {
			resolved, err := filepath.EvalSymlinks(current)
			if err != nil {
				return err
			}
			relToRoot, err := filepath.Rel(root, resolved)
			if err != nil || relToRoot == ".." || strings.HasPrefix(relToRoot, "../") {
				return fmt.Errorf("refusing to copy symlink outside directory artifact: %s", current)
			}
			resolvedInfo, err := os.Stat(resolved)
			if err != nil {
				return err
			}
			if resolvedInfo.IsDir() {
				return makeDir(ctx, fsys, dst)
			}
			return fsys.CopyFromLocal(ctx, resolved, dst, nil)
		}
		if entry.IsDir() {
			return makeDir(ctx, fsys, dst)
		}
		return fsys.CopyFromLocal(ctx, current, dst, nil)
	})
}

func fileExists(ctx context.Context, fsys *modal.SandboxFilesystem, remotePath string) bool {
	_, err := fsys.Stat(ctx, remotePath, nil)
	return err == nil
}

func immutableAssetReady(ctx context.Context, fsys *modal.SandboxFilesystem, item map[string]any) bool {
	remotePath := strings.TrimRight(stringValue(item, "remote_path"), "/")
	if boolValue(item, "source_is_dir") {
		return fileExists(ctx, fsys, remotePath+"/.bucephalus_asset_ready")
	}
	return fileExists(ctx, fsys, remotePath)
}

func terminateSandbox(ctx context.Context, sandbox *modal.Sandbox) {
	if sandbox != nil {
		_, _ = sandbox.Terminate(ctx, nil)
	}
}

func stageLaunchMounts(ctx context.Context, mc *modal.Client, app *modal.App, spec map[string]any, writableAssetMount *modal.CloudBucketMount) error {
	items := jsonObjects(spec["launch_mounts"])
	if len(items) == 0 {
		return nil
	}
	image := mc.Images.FromRegistry(stringValue(spec, "image"), nil)
	stager, err := mc.Sandboxes.Create(ctx, app, image, &modal.SandboxCreateParams{
		Command:           []string{"sleep", "31536000"},
		CloudBucketMounts: map[string]*modal.CloudBucketMount{"/bucephalus/case_assets": writableAssetMount},
		Timeout:           time.Duration(intValue(spec, "sandbox_timeout_seconds", 3600)) * time.Second,
	})
	if err != nil {
		return err
	}
	defer terminateSandbox(context.Background(), stager)
	fsys := stager.Filesystem()
	for _, item := range items {
		if immutableAssetReady(ctx, fsys, item) {
			continue
		}
		if err := copyPath(ctx, fsys, stringValue(item, "local_path"), stringValue(item, "remote_path")); err != nil {
			return err
		}
		if boolValue(item, "source_is_dir") {
			if err := fsys.WriteText(ctx, "ok\n", strings.TrimRight(stringValue(item, "remote_path"), "/")+"/.bucephalus_asset_ready", nil); err != nil {
				return err
			}
		}
	}
	return nil
}

func createSandbox(ctx context.Context, mc *modal.Client, app *modal.App, imageRef string, caseAssetsMount *modal.CloudBucketMount, spec map[string]any, workdir string, runtimeTransferArchive string) (*modal.Sandbox, error) {
	mounts := map[string]*modal.CloudBucketMount{}
	if caseAssetsMount != nil {
		mounts["/bucephalus/case_assets"] = caseAssetsMount
	}
	secrets := []*modal.Secret{}
	agentSecret, err := buildAgentSecret(ctx, mc, spec)
	if err != nil {
		return nil, err
	}
	if agentSecret != nil {
		secrets = append(secrets, agentSecret)
	}
	params := &modal.SandboxCreateParams{
		Command:           []string{"sleep", "31536000"},
		CloudBucketMounts: mounts,
		Env:               stringMap(spec["env"]),
		Secrets:           secrets,
		BlockNetwork:      boolValue(spec, "block_network"),
		Timeout:           time.Duration(intValue(spec, "sandbox_timeout_seconds", 3600)) * time.Second,
	}
	if cpu := intValue(spec, "cpu_count", 0); cpu > 0 {
		params.CPU = float64(cpu)
	}
	if memory := intValue(spec, "memory_mb", 0); memory > 0 {
		params.MemoryMiB = memory
	}
	image := mc.Images.FromRegistry(imageRef, nil)
	sandbox, err := mc.Sandboxes.Create(ctx, app, image, params)
	if err != nil {
		return nil, err
	}
	if runtimeTransferArchive != "" {
		if err := sandbox.Filesystem().CopyFromLocal(ctx, runtimeTransferArchive, runtimeTransferArchivePath, nil); err != nil {
			terminateSandbox(context.Background(), sandbox)
			return nil, err
		}
	}
	return sandbox, nil
}

func bootstrapRuntimeTransferExec(execSpec map[string]any) map[string]any {
	command := stringList(execSpec["command"])
	workdir := stringValue(execSpec, "workdir")
	bootstrapped := cloneObject(execSpec)
	script := "set -e\n" +
		"tar -xzf " + runtimeTransferArchivePath + " -C /\n" +
		"if [ -n \"$1\" ]; then cd \"$1\"; fi\n" +
		"shift\n" +
		"printf 'BUCEPHALUS_AGENT_COMMAND_STARTED_AT=%s\\n' \"$(date -u +%Y-%m-%dT%H:%M:%S.%6NZ)\"\n" +
		"exec \"$@\""
	bootstrapped["command"] = append([]string{"/bin/sh", "-lc", script, "bucephalus-runtime-bootstrap", workdir}, command...)
	delete(bootstrapped, "workdir")
	return bootstrapped
}

func cloneObject(input map[string]any) map[string]any {
	output := make(map[string]any, len(input))
	for key, value := range input {
		output[key] = value
	}
	return output
}

func instrumentContainerStartExec(execSpec map[string]any, markAgentCommandStart bool) map[string]any {
	command := stringList(execSpec["command"])
	workdir := stringValue(execSpec, "workdir")
	instrumented := cloneObject(execSpec)
	script := "printf 'BUCEPHALUS_CONTAINER_STARTED_AT=%s\\n' \"$(date -u +%Y-%m-%dT%H:%M:%S.%6NZ)\"\n" +
		"if [ -n \"$1\" ]; then cd \"$1\"; fi\n" +
		"shift\n"
	if markAgentCommandStart {
		script += "printf 'BUCEPHALUS_AGENT_COMMAND_STARTED_AT=%s\\n' \"$(date -u +%Y-%m-%dT%H:%M:%S.%6NZ)\"\n"
	}
	script += "exec \"$@\""
	instrumented["command"] = append([]string{"/bin/sh", "-lc", script, "bucephalus-container-start", workdir}, command...)
	delete(instrumented, "workdir")
	return instrumented
}

func consumePrefixedLine(text, prefix string) (string, string) {
	rest := strings.TrimPrefix(text, prefix)
	index := strings.IndexByte(rest, '\n')
	if index < 0 {
		return rest, ""
	}
	return rest[:index], rest[index+1:]
}

func splitStartMarkers(stdout string) (*string, *string, string) {
	containerPrefix := "BUCEPHALUS_CONTAINER_STARTED_AT="
	agentPrefix := "BUCEPHALUS_AGENT_COMMAND_STARTED_AT="
	var containerStartedAt *string
	var agentCommandStartedAt *string
	rest := stdout
	for {
		if strings.HasPrefix(rest, containerPrefix) {
			value, tail := consumePrefixedLine(rest, containerPrefix)
			containerStartedAt = &value
			rest = tail
			continue
		}
		if strings.HasPrefix(rest, agentPrefix) {
			value, tail := consumePrefixedLine(rest, agentPrefix)
			agentCommandStartedAt = &value
			rest = tail
			continue
		}
		break
	}
	return containerStartedAt, agentCommandStartedAt, rest
}

func waitAndRead(ctx context.Context, process *modal.ContainerProcess) (int, string, string, error) {
	stdoutCh := make(chan readResult, 1)
	stderrCh := make(chan readResult, 1)
	go func() {
		data, err := io.ReadAll(process.Stdout)
		stdoutCh <- readResult{data: data, err: err}
	}()
	go func() {
		data, err := io.ReadAll(process.Stderr)
		stderrCh <- readResult{data: data, err: err}
	}()
	exitCode, waitErr := process.Wait(ctx, nil)
	stdout := <-stdoutCh
	stderr := <-stderrCh
	if waitErr != nil {
		return exitCode, string(stdout.data), string(stderr.data), waitErr
	}
	if stdout.err != nil {
		return exitCode, string(stdout.data), string(stderr.data), stdout.err
	}
	if stderr.err != nil {
		return exitCode, string(stdout.data), string(stderr.data), stderr.err
	}
	return exitCode, string(stdout.data), string(stderr.data), nil
}

func runProcess(ctx context.Context, sandbox *modal.Sandbox, execSpec map[string]any, result *launchResult, phase string, bootstrapRuntimeTransfer bool) (execRecord, error) {
	if bootstrapRuntimeTransfer {
		execSpec = bootstrapRuntimeTransferExec(execSpec)
	}
	execSpec = instrumentContainerStartExec(execSpec, !bootstrapRuntimeTransfer)
	execStartedAt := utcNow()
	timeout := time.Duration(intValue(execSpec, "timeout_seconds", 300)) * time.Second
	process, err := sandbox.Exec(ctx, stringList(execSpec["command"]), &modal.SandboxExecParams{
		Env:     stringMap(execSpec["env"]),
		Workdir: stringValue(execSpec, "workdir"),
		Timeout: timeout,
	})
	if err != nil {
		return execRecord{}, err
	}
	exitCode, stdout, stderr, err := waitAndRead(ctx, process)
	if err != nil {
		return execRecord{}, err
	}
	containerStartedAt, agentCommandStartedAt, stdout := splitStartMarkers(stdout)
	if output := jsonObject(execSpec["stdout"]); len(output) > 0 {
		localPath := stringValue(output, "local_path")
		if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
			return execRecord{}, err
		}
		if err := os.WriteFile(localPath, []byte(stdout), 0o644); err != nil {
			return execRecord{}, err
		}
		if err := sandbox.Filesystem().WriteText(ctx, stdout, stringValue(output, "remote_path"), nil); err != nil {
			return execRecord{}, err
		}
	}
	if output := jsonObject(execSpec["stderr"]); len(output) > 0 {
		localPath := stringValue(output, "local_path")
		if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
			return execRecord{}, err
		}
		if err := os.WriteFile(localPath, []byte(stderr), 0o644); err != nil {
			return execRecord{}, err
		}
		if err := sandbox.Filesystem().WriteText(ctx, stderr, stringValue(output, "remote_path"), nil); err != nil {
			return execRecord{}, err
		}
	}
	if phase == "" {
		phase = stringValue(execSpec, "phase")
	}
	record := execRecord{
		Phase:                 phase,
		SandboxID:             sandbox.SandboxID,
		ProcessID:             nil,
		ExitCode:              exitCode,
		TimedOut:              false,
		StartedAt:             execStartedAt,
		ContainerStartedAt:    containerStartedAt,
		AgentCommandStartedAt: agentCommandStartedAt,
		EndedAt:               utcNow(),
	}
	result.Execs = append(result.Execs, record)
	return record, nil
}

func runShellChecked(ctx context.Context, sandbox *modal.Sandbox, label, script, workdir string, timeoutSeconds int) error {
	process, err := sandbox.Exec(ctx, []string{"/bin/sh", "-lc", "set -e\n" + script}, &modal.SandboxExecParams{
		Workdir: workdir,
		Timeout: time.Duration(timeoutSeconds) * time.Second,
	})
	if err != nil {
		return err
	}
	exitCode, stdout, stderr, err := waitAndRead(ctx, process)
	if err != nil {
		return err
	}
	if exitCode != 0 {
		return fmt.Errorf("modal sandbox command %q failed with exit %d\nstdout:\n%s\nstderr:\n%s", label, exitCode, stdout, stderr)
	}
	return nil
}

func ensureInlineCaptureSize(label, remotePath string, data []byte, maxInlineCaptureBytes *int) error {
	if maxInlineCaptureBytes != nil && len(data) > *maxInlineCaptureBytes {
		return fmt.Errorf("%s capture at %s is too large to inline: bytes=%d max=%d", label, remotePath, len(data), *maxInlineCaptureBytes)
	}
	return nil
}

func selectField(value any, field any) (any, error) {
	fieldText, ok := field.(string)
	if !ok || strings.TrimSpace(fieldText) == "" {
		return value, nil
	}
	fieldText = strings.TrimSpace(fieldText)
	current := value
	if strings.HasPrefix(fieldText, "/") {
		for _, part := range strings.Split(fieldText, "/")[1:] {
			part = strings.ReplaceAll(strings.ReplaceAll(part, "~1", "/"), "~0", "~")
			if list, ok := current.([]any); ok {
				index, err := strconv.Atoi(part)
				if err != nil {
					return nil, err
				}
				current = list[index]
			} else {
				current = jsonObject(current)[part]
			}
		}
		return current, nil
	}
	for _, part := range strings.Split(fieldText, ".") {
		current = jsonObject(current)[part]
	}
	return current, nil
}

func writeLocalCapture(capture map[string]any, data []byte) (*string, error) {
	localPath := stringValue(capture, "local_path")
	if localPath == "" {
		return nil, nil
	}
	if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
		return nil, err
	}
	if err := os.WriteFile(localPath, data, 0o644); err != nil {
		return nil, err
	}
	return &localPath, nil
}

func readFileValue(ctx context.Context, fsys *modal.SandboxFilesystem, remotePath, format, label string, maxInlineCaptureBytes *int) (any, error) {
	data, err := fsys.ReadBytes(ctx, remotePath, nil)
	if err != nil {
		return nil, err
	}
	switch format {
	case "json":
		if err := ensureInlineCaptureSize(label, remotePath, data, maxInlineCaptureBytes); err != nil {
			return nil, err
		}
		var value any
		if err := json.Unmarshal(data, &value); err != nil {
			return nil, err
		}
		return value, nil
	case "text":
		if err := ensureInlineCaptureSize(label, remotePath, data, maxInlineCaptureBytes); err != nil {
			return nil, err
		}
		return string(data), nil
	case "bytes":
		return map[string]any{"path": remotePath, "bytes": len(data)}, nil
	default:
		return nil, fmt.Errorf("unsupported runtime output format %q", format)
	}
}

func shellQuote(value string) string {
	if value == "" {
		return "''"
	}
	return "'" + strings.ReplaceAll(value, "'", "'\"'\"'") + "'"
}

func captureOutput(ctx context.Context, sandbox *modal.Sandbox, label string, output map[string]any, workdir string, timeoutSeconds int, maxInlineCaptureBytes *int) (map[string]any, error) {
	fsys := sandbox.Filesystem()
	capture := jsonObject(output["capture"])
	captureType := stringValue(capture, "type")
	switch captureType {
	case "file", "result_json":
		remotePath := stringValue(capture, "path")
		required := boolValue(capture, "required") || captureType == "result_json"
		if !fileExists(ctx, fsys, remotePath) {
			if required {
				return nil, fmt.Errorf("declared runtime output %s missing at %s", label, remotePath)
			}
			return map[string]any{"value": nil, "host_path": nil, "container_path": remotePath, "format": capture["format"]}, nil
		}
		data, err := fsys.ReadBytes(ctx, remotePath, nil)
		if err != nil {
			return nil, err
		}
		hostPath, err := writeLocalCapture(capture, data)
		if err != nil {
			return nil, err
		}
		format := stringValue(capture, "format")
		var value any
		if captureType == "result_json" {
			if err := ensureInlineCaptureSize(label, remotePath, data, maxInlineCaptureBytes); err != nil {
				return nil, err
			}
			var resultJSON any
			if err := json.Unmarshal(data, &resultJSON); err != nil {
				return nil, err
			}
			if _, ok := capture["field"]; ok {
				selected, err := selectField(resultJSON, capture["field"])
				if err != nil {
					return nil, err
				}
				value = map[string]any{"value": selected}
			} else {
				value = resultJSON
			}
			format = "json"
		} else {
			value, err = readFileValue(ctx, fsys, remotePath, format, label, maxInlineCaptureBytes)
			if err != nil {
				return nil, err
			}
		}
		return map[string]any{"value": value, "host_path": hostPath, "container_path": remotePath, "format": format}, nil
	case "workspace_diff":
		patchPath := "/bucephalus/out/candidate.patch"
		probe, err := sandbox.Exec(ctx, []string{"git", "-C", workdir, "rev-parse", "--is-inside-work-tree"}, &modal.SandboxExecParams{Timeout: time.Duration(timeoutSeconds) * time.Second})
		if err != nil {
			return nil, err
		}
		exitCode, _, _, err := waitAndRead(ctx, probe)
		if err != nil {
			return nil, err
		}
		patchText := ""
		if exitCode == 0 {
			pathspec := ". ':(exclude).bucephalus' ':(exclude).haiku' ':(exclude).lab' ':(exclude)logs' ':(exclude)out'"
			if err := runShellChecked(ctx, sandbox, "modal_workspace_diff_add", "git -C "+shellQuote(workdir)+" add -N -- "+pathspec, workdir, timeoutSeconds); err != nil {
				return nil, err
			}
			diff, err := sandbox.Exec(ctx, []string{"/bin/sh", "-lc", "git -C " + shellQuote(workdir) + " diff --binary -- " + pathspec}, &modal.SandboxExecParams{Workdir: workdir, Timeout: time.Duration(timeoutSeconds) * time.Second})
			if err != nil {
				return nil, err
			}
			diffExit, stdout, _, err := waitAndRead(ctx, diff)
			if err != nil {
				return nil, err
			}
			if diffExit != 0 {
				return nil, errors.New("failed to capture modal workspace diff")
			}
			patchText = stdout
			if maxInlineCaptureBytes != nil && len([]byte(patchText)) > *maxInlineCaptureBytes {
				return nil, fmt.Errorf("%s workspace_diff is too large to inline: bytes=%d max=%d", label, len([]byte(patchText)), *maxInlineCaptureBytes)
			}
		}
		if err := fsys.WriteText(ctx, patchText, patchPath, nil); err != nil {
			return nil, err
		}
		hostPath, err := writeLocalCapture(capture, []byte(patchText))
		if err != nil {
			return nil, err
		}
		return map[string]any{"value": map[string]any{"patch": patchText, "path": patchPath}, "host_path": hostPath, "container_path": patchPath, "format": "unified_diff"}, nil
	default:
		return nil, fmt.Errorf("%s.capture.type %q is not executable", label, captureType)
	}
}

func captureOutputs(ctx context.Context, sandbox *modal.Sandbox, outputs map[string]map[string]any, prefix, workdir string, timeoutSeconds int, maxInlineCaptureBytes *int) (map[string]any, error) {
	captured := make(map[string]any, len(outputs))
	keys := make([]string, 0, len(outputs))
	for key := range outputs {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, outputID := range keys {
		value, err := captureOutput(ctx, sandbox, prefix+"."+outputID, outputs[outputID], workdir, timeoutSeconds, maxInlineCaptureBytes)
		if err != nil {
			return nil, err
		}
		captured[outputID] = value
	}
	return captured, nil
}

func selectTransportSource(source map[string]any, agentOutputs map[string]any, taskPayload any) (any, error) {
	if output, ok := optionalString(source, "output"); ok {
		outputID := strings.TrimPrefix(output, "agent.")
		outputValue := jsonObject(agentOutputs[outputID])["value"]
		if _, ok := source["field"]; ok {
			return selectField(outputValue, source["field"])
		}
		return outputValue, nil
	}
	if _, ok := source["case"]; ok {
		return selectField(taskPayload, source["case"])
	}
	if _, ok := source["task"]; ok {
		return selectField(taskPayload, source["task"])
	}
	if object := jsonObject(source["object"]); len(object) > 0 {
		out := make(map[string]any, len(object))
		for key, nested := range object {
			value, err := selectTransportSource(jsonObject(nested), agentOutputs, taskPayload)
			if err != nil {
				return nil, err
			}
			out[key] = value
		}
		return out, nil
	}
	return nil, nil
}

func valueToBytes(value any, jsonMode bool) ([]byte, error) {
	if !jsonMode {
		if text, ok := value.(string); ok {
			return []byte(text), nil
		}
	}
	return json.MarshalIndent(value, "", "  ")
}

func materializeGraderInputs(ctx context.Context, sandbox *modal.Sandbox, grader map[string]any, agentOutputs map[string]any, taskPayload any) (map[string]string, error) {
	env := map[string]string{}
	fsys := sandbox.Filesystem()
	for inputID, inputSpec := range jsonObjectMap(grader["inputs"]) {
		value, err := selectTransportSource(jsonObject(inputSpec["source"]), agentOutputs, taskPayload)
		if err != nil {
			return nil, err
		}
		if value == nil {
			if boolValue(inputSpec, "required") {
				return nil, fmt.Errorf("required grader input %q resolved to null", inputID)
			}
			continue
		}
		materialize := jsonObject(inputSpec["materialize"])
		switch stringValue(materialize, "as") {
		case "file", "json_file":
			remotePath := stringValue(materialize, "path")
			data, err := valueToBytes(value, stringValue(materialize, "as") == "json_file")
			if err != nil {
				return nil, err
			}
			parent := path.Dir(remotePath)
			if parent != "." && parent != "/" {
				if err := makeDir(ctx, fsys, parent); err != nil {
					return nil, err
				}
			}
			if err := fsys.WriteBytes(ctx, data, remotePath, nil); err != nil {
				return nil, err
			}
		case "env":
			if text, ok := value.(string); ok {
				env[stringValue(materialize, "name")] = text
			} else {
				encoded, _ := json.Marshal(value)
				env[stringValue(materialize, "name")] = string(encoded)
			}
		default:
			return nil, fmt.Errorf("grader input %q.materialize.as %q is not executable", inputID, stringValue(materialize, "as"))
		}
	}
	return env, nil
}

func writeTransportEnvelope(ctx context.Context, fsys *modal.SandboxFilesystem, spec map[string]any, agentOutputs, graderOutputs map[string]any) error {
	envelope := map[string]any{
		"schema_version": "runtime_transport_envelope_v1",
		"agent":          map[string]any{"outputs": agentOutputs},
		"grader":         map[string]any{"outputs": graderOutputs},
	}
	payload, err := json.MarshalIndent(envelope, "", "  ")
	if err != nil {
		return err
	}
	transport := jsonObject(spec["transport_envelope"])
	if err := fsys.WriteBytes(ctx, payload, stringValue(transport, "remote_path"), nil); err != nil {
		return err
	}
	localPath := stringValue(transport, "local_path")
	if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
		return err
	}
	return os.WriteFile(localPath, payload, 0o644)
}

func prepareModalGrader(ctx context.Context, taskSandbox *modal.Sandbox, grader map[string]any) error {
	timeoutSeconds := intValue(grader, "timeout_seconds", 300)
	for _, binding := range jsonObjects(grader["hidden_assets"]) {
		stashParent := path.Dir(stringValue(binding, "stash_path"))
		script := "mkdir -p " + shellQuote(stashParent) + "\nrm -rf " + shellQuote(stringValue(binding, "stash_path")) + "\nmv " + shellQuote(stringValue(binding, "hidden_path")) + " " + shellQuote(stringValue(binding, "stash_path"))
		if err := runShellChecked(ctx, taskSandbox, "hide_hidden_asset", script, "", timeoutSeconds); err != nil {
			return err
		}
	}
	return nil
}

func revealModalGraderAssets(ctx context.Context, taskSandbox *modal.Sandbox, grader map[string]any) error {
	timeoutSeconds := intValue(grader, "timeout_seconds", 300)
	for _, binding := range jsonObjects(grader["hidden_assets"]) {
		parent := path.Dir(stringValue(binding, "revealed_path"))
		script := "mkdir -p " + shellQuote(parent) + "\nrm -rf " + shellQuote(stringValue(binding, "revealed_path")) + "\nmv " + shellQuote(stringValue(binding, "stash_path")) + " " + shellQuote(stringValue(binding, "revealed_path"))
		if err := runShellChecked(ctx, taskSandbox, "reveal_hidden_asset", script, "", timeoutSeconds); err != nil {
			return err
		}
	}
	injected := jsonObject(grader["injected"])
	if len(injected) == 0 {
		return nil
	}
	src := stringValue(injected, "source_remote_path")
	dest := stringValue(injected, "copy_dest")
	var extract string
	if boolValue(injected, "source_is_dir") {
		extract = "cp -R " + shellQuote(src) + "/. " + shellQuote(dest)
	} else if archiveFlag, ok := optionalString(injected, "archive_flag"); ok {
		extract = "tar " + shellQuote(archiveFlag) + " " + shellQuote(src) + " -C " + shellQuote(dest)
	} else {
		extract = "cp " + shellQuote(src) + " " + shellQuote(dest) + "/"
	}
	return runShellChecked(ctx, taskSandbox, "injected_grader_bundle", "mkdir -p "+shellQuote(dest)+"\nfind "+shellQuote(dest)+" -mindepth 1 -maxdepth 1 -exec rm -rf {} +\n"+extract, "", timeoutSeconds)
}

func copyOptionalToLocal(ctx context.Context, fsys *modal.SandboxFilesystem, remotePath, localPath string) bool {
	if remotePath == "" || localPath == "" {
		return false
	}
	if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
		return false
	}
	return fsys.CopyToLocal(ctx, remotePath, localPath, nil) == nil
}

func exportLocalFileToBucket(ctx context.Context, mc *modal.Client, app *modal.App, spec map[string]any, sync map[string]any, localPath, remotePath string) (bool, error) {
	if _, err := os.Stat(localPath); err != nil {
		if os.IsNotExist(err) {
			return false, nil
		}
		return false, err
	}
	writableMount, err := buildBucketMount(ctx, mc, sync, stringValue(sync, "prefix"), false)
	if err != nil {
		return false, err
	}
	image := mc.Images.FromRegistry(stringValue(spec, "image"), nil)
	stager, err := mc.Sandboxes.Create(ctx, app, image, &modal.SandboxCreateParams{
		Command:           []string{"sleep", "31536000"},
		CloudBucketMounts: map[string]*modal.CloudBucketMount{"/bucephalus": writableMount},
		Timeout:           time.Duration(intValue(spec, "sandbox_timeout_seconds", 3600)) * time.Second,
	})
	if err != nil {
		return false, err
	}
	defer terminateSandbox(context.Background(), stager)
	if err := copyPath(ctx, stager.Filesystem(), localPath, remotePath); err != nil {
		return false, err
	}
	return true, nil
}

func launcherErrorStderrPath(specPath string, spec map[string]any) string {
	execs := jsonObjects(spec["execs"])
	if len(execs) > 0 {
		if stderr := jsonObject(execs[len(execs)-1]["stderr"]); len(stderr) > 0 {
			if localPath := stringValue(stderr, "local_path"); localPath != "" {
				return localPath
			}
		}
	}
	return filepath.Join(filepath.Dir(specPath), "modal_launcher_stderr.log")
}

func appendLauncherError(specPath string, spec map[string]any, err error) {
	stderrPath := launcherErrorStderrPath(specPath, spec)
	_ = os.MkdirAll(filepath.Dir(stderrPath), 0o755)
	file, openErr := os.OpenFile(stderrPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if openErr != nil {
		return
	}
	defer file.Close()
	fmt.Fprintf(file, "\n[bucephalus modal launcher error]\n%v\n", err)
}

func isTimeoutError(err error) bool {
	if err == nil {
		return false
	}
	text := strings.ToLower(err.Error())
	return strings.Contains(text, "timeout") || strings.Contains(text, "timed out") || errors.Is(err, context.DeadlineExceeded)
}

func runLaunch(specPath string) error {
	ctx := context.Background()
	timings := map[string]string{}
	timingMark(timings, "launcher_main_started_at")
	spec, err := loadJSON(specPath)
	if err != nil {
		return err
	}
	var maxInlineCaptureBytes *int
	if raw, ok := spec["max_inline_capture_bytes"]; ok && raw != nil {
		value := intValue(map[string]any{"v": raw}, "v", 0)
		maxInlineCaptureBytes = &value
	}
	runtimeTransferArchive := stringValue(spec, "runtime_transfer_archive")
	archiveInfo, err := os.Stat(runtimeTransferArchive)
	if err != nil {
		return err
	}
	sync := jsonObject(spec["sync"])
	timingMark(timings, "app_lookup_started_at")
	mc, err := modal.NewClient()
	if err != nil {
		return err
	}
	app, err := appLookup(ctx, mc, stringValue(spec, "app_name"), stringValue(spec, "environment_name"))
	if err != nil {
		return err
	}
	timingMark(timings, "app_lookup_ended_at")
	timingMark(timings, "runtime_transfer_archive_build_started_at")
	timingMark(timings, "runtime_transfer_archive_build_ended_at")
	var caseAssetsMount *modal.CloudBucketMount
	timingMark(timings, "launch_mounts_prepare_started_at")
	if len(jsonObjects(spec["launch_mounts"])) > 0 {
		writableAssetMount, err := buildBucketMount(ctx, mc, sync, stringValue(sync, "immutable_case_asset_prefix"), false)
		if err != nil {
			return err
		}
		if err := stageLaunchMounts(ctx, mc, app, spec, writableAssetMount); err != nil {
			return err
		}
		caseAssetsMount, err = buildBucketMount(ctx, mc, sync, stringValue(sync, "immutable_case_asset_prefix"), true)
		if err != nil {
			return err
		}
	}
	timingMark(timings, "launch_mounts_prepare_ended_at")
	var sandbox *modal.Sandbox
	var graderSandbox *modal.Sandbox
	startedAt := utcNow()
	var endedAt *string
	var exitCode *int
	result := &launchResult{
		SandboxID:                   nil,
		Execs:                       []execRecord{},
		ExitCode:                    nil,
		TimedOut:                    false,
		StartedAt:                   startedAt,
		EndedAt:                     nil,
		RuntimeTransferArchiveBytes: archiveInfo.Size(),
		Timings:                     timings,
	}
	var fatalErr error
	runErr := func() error {
		timingMark(timings, "sandbox_create_started_at")
		var err error
		sandbox, err = createSandbox(ctx, mc, app, stringValue(spec, "image"), caseAssetsMount, spec, stringValue(spec, "workdir"), runtimeTransferArchive)
		if err != nil {
			return err
		}
		timingMark(timings, "sandbox_create_ended_at")
		result.SandboxID = &sandbox.SandboxID
		writeRuntimeWorker(specPath, "task", sandbox)
		fsys := sandbox.Filesystem()
		grader := jsonObject(spec["grader"])
		bootstrapRuntimeTransfer := len(grader) == 0
		if !bootstrapRuntimeTransfer {
			if err := runShellChecked(ctx, sandbox, "runtime_transfer_extract", "tar -xzf "+runtimeTransferArchivePath+" -C /", "", intValue(spec, "sandbox_timeout_seconds", 3600)); err != nil {
				return err
			}
			if err := prepareModalGrader(ctx, sandbox, grader); err != nil {
				return err
			}
		}
		for index, execSpec := range jsonObjects(spec["execs"]) {
			record, err := runProcess(ctx, sandbox, execSpec, result, "", bootstrapRuntimeTransfer && index == 0)
			if err != nil {
				return err
			}
			exitCode = &record.ExitCode
		}
		if len(grader) == 0 {
			return nil
		}
		trialInputBytes, err := fsys.ReadBytes(ctx, stringValue(jsonObject(spec["trial_input"]), "remote_path"), nil)
		if err != nil {
			return err
		}
		var taskPayload any
		if err := json.Unmarshal(trialInputBytes, &taskPayload); err != nil {
			return err
		}
		agentOutputs, err := captureOutputs(ctx, sandbox, jsonObjectMap(grader["agent_outputs"]), "agent", stringValue(spec, "workdir"), intValue(grader, "timeout_seconds", 300), maxInlineCaptureBytes)
		if err != nil {
			return err
		}
		if err := revealModalGraderAssets(ctx, sandbox, grader); err != nil {
			return err
		}
		graderSandbox = sandbox
		if stringValue(grader, "sandbox") == "separate" {
			graderSandbox, err = createSandbox(ctx, mc, app, stringValue(grader, "image"), caseAssetsMount, spec, stringValue(grader, "workdir"), runtimeTransferArchive)
			if err != nil {
				return err
			}
			writeRuntimeWorker(specPath, "grading", graderSandbox)
		}
		transportEnv, err := materializeGraderInputs(ctx, graderSandbox, grader, agentOutputs, taskPayload)
		if err != nil {
			return err
		}
		graderEnv := stringMap(grader["env"])
		agentStatus := "signal"
		if result.TimedOut {
			agentStatus = "timeout"
		} else if exitCode != nil {
			agentStatus = strconv.Itoa(*exitCode)
		}
		for key, value := range graderEnv {
			if value == "__BUCEPHALUS_AGENT_EXIT_STATUS__" {
				graderEnv[key] = agentStatus
			}
		}
		for key, value := range transportEnv {
			graderEnv[key] = value
		}
		graderExec := map[string]any{
			"phase":           "grader",
			"command":         stringList(grader["command"]),
			"env":             graderEnv,
			"workdir":         stringValue(grader, "workdir"),
			"timeout_seconds": intValue(grader, "timeout_seconds", 300),
			"stdout":          grader["stdout"],
			"stderr":          grader["stderr"],
		}
		if _, err := runProcess(ctx, graderSandbox, graderExec, result, "grader", false); err != nil {
			return err
		}
		graderOutputs, err := captureOutputs(ctx, graderSandbox, jsonObjectMap(grader["outputs"]), "grader", stringValue(grader, "workdir"), intValue(grader, "timeout_seconds", 300), maxInlineCaptureBytes)
		if err != nil {
			return err
		}
		return writeTransportEnvelope(ctx, graderSandbox.Filesystem(), spec, agentOutputs, graderOutputs)
	}()
	if runErr != nil {
		result.TimedOut = isTimeoutError(runErr)
		appendLauncherError(specPath, spec, runErr)
		if !result.TimedOut {
			fatalErr = runErr
		}
	}
	now := utcNow()
	endedAt = &now
	if sandbox != nil {
		timingMark(timings, "result_copy_started_at")
		fsys := sandbox.Filesystem()
		copyOptionalToLocal(ctx, fsys, stringValue(jsonObject(spec["result"]), "remote_path"), stringValue(jsonObject(spec["result"]), "local_path"))
		copyOptionalToLocal(ctx, fsys, stringValue(jsonObject(spec["events"]), "scratch_path"), stringValue(jsonObject(spec["events"]), "local_path"))
		transportFS := fsys
		if graderSandbox != nil {
			transportFS = graderSandbox.Filesystem()
		}
		copyOptionalToLocal(ctx, transportFS, stringValue(jsonObject(spec["transport_envelope"]), "remote_path"), stringValue(jsonObject(spec["transport_envelope"]), "local_path"))
		if grader := jsonObject(spec["grader"]); len(grader) > 0 {
			copyOptionalToLocal(ctx, transportFS, stringValue(jsonObject(grader["stdout"]), "remote_path"), stringValue(jsonObject(grader["stdout"]), "local_path"))
			copyOptionalToLocal(ctx, transportFS, stringValue(jsonObject(grader["stderr"]), "remote_path"), stringValue(jsonObject(grader["stderr"]), "local_path"))
		}
		timingMark(timings, "result_copy_ended_at")
	}
	result.ExitCode = exitCode
	result.EndedAt = endedAt
	timingMark(timings, "result_available_at")
	marker("BUCEPHALUS_MODAL_RESULT", result)
	if sandbox != nil {
		if durableEventsPath, ok := optionalString(jsonObject(spec["events"]), "durable_path"); ok {
			localEventsPath := stringValue(jsonObject(spec["events"]), "local_path")
			timingMark(timings, "durable_events_export_started_at")
			_, _ = exportLocalFileToBucket(ctx, mc, app, spec, sync, localEventsPath, durableEventsPath)
			timingMark(timings, "durable_events_export_ended_at")
		}
	}
	timingMark(timings, "sandbox_cleanup_started_at")
	if graderSandbox != nil && sandbox != nil && graderSandbox.SandboxID != sandbox.SandboxID {
		terminateSandbox(context.Background(), graderSandbox)
	}
	terminateSandbox(context.Background(), sandbox)
	timingMark(timings, "sandbox_cleanup_ended_at")
	timingMark(timings, "launcher_completed_at")
	marker("BUCEPHALUS_MODAL_LIFECYCLE", map[string]any{"sandbox_id": result.SandboxID, "timings": timings})
	return fatalErr
}

func isNotFound(err error) bool {
	if err == nil {
		return false
	}
	text := strings.ToLower(err.Error())
	return strings.Contains(text, "notfound") || strings.Contains(text, "not found") || strings.Contains(text, "404")
}

func runCleanup(specPath string) error {
	ctx := context.Background()
	spec, err := loadJSON(specPath)
	if err != nil {
		return err
	}
	mc, err := modal.NewClient()
	if err != nil {
		return err
	}
	results := []map[string]any{}
	errorsOut := []map[string]any{}
	cleaned := 0
	for _, sandboxID := range stringList(spec["sandbox_ids"]) {
		sandbox, err := mc.Sandboxes.FromID(ctx, sandboxID, nil)
		if err != nil {
			if isNotFound(err) {
				cleaned++
				results = append(results, map[string]any{"sandbox_id": sandboxID, "status": "not_found"})
			} else {
				errorsOut = append(errorsOut, map[string]any{"sandbox_id": sandboxID, "error": err.Error()})
			}
			continue
		}
		if _, err := sandbox.Terminate(ctx, nil); err != nil {
			if isNotFound(err) {
				cleaned++
				results = append(results, map[string]any{"sandbox_id": sandboxID, "status": "not_found"})
			} else {
				errorsOut = append(errorsOut, map[string]any{"sandbox_id": sandboxID, "error": err.Error()})
			}
			continue
		}
		cleaned++
		results = append(results, map[string]any{"sandbox_id": sandboxID, "status": "terminated"})
	}
	payload := map[string]any{"cleaned": cleaned, "results": results, "errors": errorsOut}
	marker("BUCEPHALUS_MODAL_CLEANUP", payload)
	if len(errorsOut) > 0 {
		return errors.New("modal cleanup failed")
	}
	return nil
}

func main() {
	if len(os.Args) != 3 {
		fail("usage: %s launch|cleanup SPEC.json", os.Args[0])
	}
	var err error
	switch os.Args[1] {
	case "launch":
		err = runLaunch(os.Args[2])
	case "cleanup":
		err = runCleanup(os.Args[2])
	default:
		err = fmt.Errorf("unknown mode %q", os.Args[1])
	}
	if err != nil {
		fail("%v", err)
	}
}
"####;

#[cfg(test)]
pub(crate) fn modal_launcher_go_source_for_test() -> &'static str {
    MODAL_LAUNCHER_GO_SOURCE
}

#[cfg(test)]
pub(crate) fn build_modal_runtime_transfer_archive_for_test(
    modal_dir: &Path,
    value: Value,
) -> Result<PathBuf> {
    build_modal_runtime_transfer_archive(modal_dir, &ModalLaunchSpec { value })
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
