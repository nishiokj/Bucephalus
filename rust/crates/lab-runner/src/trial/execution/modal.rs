use super::*;
use std::io::{BufRead, BufReader, Write};
use std::thread;

pub(crate) const BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES_ENV: &str =
    "BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES";

const DEFAULT_MODAL_MAX_ACTIVE_SANDBOXES: usize = 64;
const MODAL_LAUNCHER_LOG_TAIL_BYTES: u64 = 1024 * 1024;
const MODAL_LAUNCHER_ENV: &str = "BUCEPHALUS_MODAL_LAUNCHER";

#[cfg(windows)]
const MODAL_LAUNCHER_BIN: &str = "bucephalus-modal-launcher.exe";
#[cfg(not(windows))]
const MODAL_LAUNCHER_BIN: &str = "bucephalus-modal-launcher";

fn modal_active_sandbox_limiter() -> &'static ActiveRuntimeLimiter {
    static LIMITER: OnceLock<ActiveRuntimeLimiter> = OnceLock::new();
    LIMITER.get_or_init(ActiveRuntimeLimiter::new)
}

fn planned_modal_active_sandbox_units(request: &AdapterRunRequest<'_>) -> Result<usize> {
    let mut units = 1;
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
        let bucket = std::env::var("BUCEPHALUS_MODAL_S3_BUCKET")
            .or_else(|_| std::env::var("BUCEPHALUS_S3_BUCKET"))
            .map_err(|_| {
                anyhow!(
                    "executor modal requires BUCEPHALUS_MODAL_S3_BUCKET or BUCEPHALUS_S3_BUCKET"
                )
            })?;
        let base_prefix = std::env::var("BUCEPHALUS_MODAL_S3_PREFIX")
            .or_else(|_| std::env::var("BUCEPHALUS_S3_PREFIX"))
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
            endpoint_url: std::env::var("BUCEPHALUS_MODAL_S3_ENDPOINT_URL")
                .or_else(|_| std::env::var("BUCEPHALUS_S3_ENDPOINT_URL"))
                .ok(),
            region: std::env::var("BUCEPHALUS_MODAL_S3_REGION")
                .or_else(|_| std::env::var("AWS_REGION"))
                .ok(),
            modal_secret_name: std::env::var("BUCEPHALUS_MODAL_S3_SECRET").ok(),
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
    launcher: PathBuf,
}

impl ModalExecutionBackend {
    pub(crate) fn from_env() -> Self {
        Self {
            app_name: std::env::var("BUCEPHALUS_MODAL_APP_NAME")
                .unwrap_or_else(|_| "bucephalus-runner".to_string()),
            environment_name: std::env::var("BUCEPHALUS_MODAL_ENVIRONMENT").ok(),
            launcher: modal_launcher_path_from_env(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(app_name: &str, environment_name: Option<&str>) -> Self {
        Self {
            app_name: app_name.to_string(),
            environment_name: environment_name.map(str::to_string),
            launcher: PathBuf::from(MODAL_LAUNCHER_BIN),
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
    run_modal_cleanup(&backend.launcher, request.trial_dir, &worker_ids)
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
    launcher: &Path,
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
    let command = modal_launcher_command(launcher, &modal_dir, "cleanup", &spec_path);
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
    let modal_result = run_modal_launch(&backend.launcher, trial_dir, &launch, lifecycle_context)?;
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
            .grader
            .ok_or_else(|| anyhow!("grading enabled without grader config"))?;
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
    if !request.grading_enabled {
        return Ok(None);
    }
    let grader = request
        .grader
        .ok_or_else(|| anyhow!("grading enabled without grader config"))?;
    let Some(grader_command) = resolve_grader_command(request)? else {
        return Err(anyhow!(
            "grading is mandatory but no grader command resolved for this trial"
        ));
    };
    if matches!(
        grader.strategy,
        GradingStrategy::None | GradingStrategy::Host
    ) {
        return Err(anyhow!(
            "executor modal does not support grading strategy '{}'",
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
                    .grader
                    .ok_or_else(|| anyhow!("grading enabled without grader config"))?,
                &resolve_grader_command(request)?.ok_or_else(|| {
                    anyhow!("grading is mandatory but no grader command resolved")
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
            let dst = Path::new(&remote_name)
                .join(relative)
                .to_string_lossy()
                .to_string();
            let metadata = fs::symlink_metadata(path).with_context(|| {
                format!("stat modal runtime transfer source {}", path.display())
            })?;
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
                format!(
                    "resolve modal runtime transfer source {}",
                    local_path.display()
                )
            })?
        } else {
            local_path.to_path_buf()
        };
        append_modal_archive_file(archive, &source, &remote_name)?;
    }
    Ok(())
}

fn build_modal_runtime_transfer_archive(
    modal_dir: &Path,
    launch: &ModalLaunchSpec,
) -> Result<PathBuf> {
    let archive_path = modal_runtime_transfer_archive_path(modal_dir);
    let file = File::create(&archive_path).with_context(|| {
        format!(
            "create modal runtime transfer archive {}",
            archive_path.display()
        )
    })?;
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

fn default_modal_launcher_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(MODAL_LAUNCHER_BIN)))
        .unwrap_or_else(|| PathBuf::from(MODAL_LAUNCHER_BIN))
}

fn modal_launcher_path_from_env() -> PathBuf {
    std::env::var(MODAL_LAUNCHER_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_modal_launcher_path())
}

fn modal_launcher_command(
    launcher: &Path,
    modal_dir: &Path,
    mode: &str,
    spec_path: &Path,
) -> Command {
    let mut command = Command::new(launcher);
    command.current_dir(modal_dir).arg(mode).arg(spec_path);
    command
}

#[cfg(test)]
pub(crate) fn modal_launcher_command_for_test(
    launcher: &Path,
    modal_dir: &Path,
    mode: &str,
    spec_path: &Path,
) -> Command {
    modal_launcher_command(launcher, modal_dir, mode, spec_path)
}

fn run_modal_launch(
    launcher: &Path,
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
    let mut command = modal_launcher_command(launcher, &modal_dir, "launch", &spec_path);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| {
            format!(
                "spawn modal sandbox launcher command {}; set {} to override the packaged helper path",
                launcher.display(),
                MODAL_LAUNCHER_ENV
            )
        })?;
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
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn modal_launcher_go_source_for_test() -> &'static str {
    include_str!("../../../../../../modal-launcher/main.go")
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
