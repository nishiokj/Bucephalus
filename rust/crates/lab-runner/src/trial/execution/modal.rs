use super::*;
use crate::util::env_var_with_legacy;

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
    python: String,
}

impl ModalExecutionBackend {
    pub(crate) fn from_env() -> Self {
        Self {
            app_name: env_var_with_legacy("BUCEPHALUS_MODAL_APP_NAME")
                .unwrap_or_else(|_| "bucephalus-runner".to_string()),
            environment_name: env_var_with_legacy("BUCEPHALUS_MODAL_ENVIRONMENT").ok(),
            python: env_var_with_legacy("BUCEPHALUS_MODAL_PYTHON")
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
    let script_path = modal_dir.join("bucephalus_modal_cleanup.py");
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
        .find_map(|line| line.strip_prefix("BUCEPHALUS_MODAL_CLEANUP="))
        .ok_or_else(|| {
            anyhow!(
                "modal cleanup launcher did not emit BUCEPHALUS_MODAL_CLEANUP in {}",
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
    let modal_result = run_modal_launch(&backend.python, trial_dir, &launch)?;
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
        detail.insert("sync".to_string(), json!(sync.kind_label()));
        detail
    };
    record_timestamp_delta(
        &perf_context,
        "modal_runner_dispatch_to_python_main",
        "launcher_dispatched_at",
        Some(&launcher_dispatched_at),
        "python_main_started_at",
        modal_result.timing("python_main_started_at"),
        modal_detail(),
    )?;
    record_timestamp_delta(
        &perf_context,
        "modal_python_main_to_sandbox_create_start",
        "python_main_started_at",
        modal_result.timing("python_main_started_at"),
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
            .container_started_at
            .clone()
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

pub(crate) struct ModalSandboxResult {
    pub(crate) sandbox_id: Option<String>,
    pub(crate) process_id: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) started_at: Option<String>,
    pub(crate) container_started_at: Option<String>,
    pub(crate) ended_at: Option<String>,
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
    if let Some(bundle) = request.agent_artifact {
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

fn run_modal_launch(
    python: &str,
    trial_dir: &Path,
    launch: &ModalLaunchSpec,
) -> Result<ModalSandboxResult> {
    let modal_dir = trial_dir.join("modal");
    ensure_dir(&modal_dir)?;
    let script_path = modal_dir.join("bucephalus_modal_sandbox.py");
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
        .find_map(|line| line.strip_prefix("BUCEPHALUS_MODAL_RESULT="))
        .ok_or_else(|| {
            anyhow!(
                "modal sandbox launcher did not emit BUCEPHALUS_MODAL_RESULT in {}",
                output.stdout_path.display()
            )
        })?;
    let value: Value = serde_json::from_str(marker)?;
    parse_modal_sandbox_result(&value)
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
        ended_at: agent_exec
            .and_then(|exec| exec.get("ended_at"))
            .or_else(|| value.get("ended_at"))
            .and_then(Value::as_str)
            .map(str::to_string),
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

const MODAL_SANDBOX_SCRIPT: &str = r#"
import json
import os
import pathlib
import shlex
import sys
import tarfile
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
    if key_prefix and not key_prefix.endswith("/"):
        key_prefix = key_prefix + "/"
    return modal.CloudBucketMount(
        bucket_name=sync["bucket"],
        bucket_endpoint_url=sync.get("endpoint_url"),
        key_prefix=key_prefix,
        secret=build_secret(sync),
        read_only=read_only,
        force_path_style=bool(sync.get("force_path_style", False)),
    )


def build_agent_secret(spec):
    names = spec.get("secret_env") or []
    if not names:
        return None
    data = {name: required_env(name) for name in names}
    return modal.Secret.from_dict(data)


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


def normalized_archive_name(remote_path):
    path = pathlib.PurePosixPath(remote_path)
    if not path.is_absolute():
        raise RuntimeError(f"runtime transfer remote_path must be absolute: {remote_path}")
    parts = []
    for part in path.parts:
        if part in ("", "/"):
            continue
        if part in (".", ".."):
            raise RuntimeError(f"runtime transfer remote_path must not contain {part!r}: {remote_path}")
        parts.append(part)
    if not parts:
        raise RuntimeError("runtime transfer remote_path must not target /")
    return "/".join(parts)


def add_directory_entry(tar, arcname):
    arcname = arcname.strip("/")
    if not arcname:
        return
    info = tarfile.TarInfo(arcname)
    info.type = tarfile.DIRTYPE
    info.mode = 0o755
    info.mtime = 0
    tar.addfile(info)


def add_parent_dirs(tar, arcname):
    parent = pathlib.PurePosixPath(arcname).parent
    if str(parent) in ("", "."):
        return
    current = pathlib.PurePosixPath("")
    for part in parent.parts:
        current = current / part
        add_directory_entry(tar, current.as_posix())


def normalize_tar_info(info):
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = 0
    return info


def add_file_entry(tar, source, arcname):
    add_parent_dirs(tar, arcname)
    tar.add(str(source), arcname=arcname, recursive=False, filter=normalize_tar_info)


def add_runtime_path_to_archive(tar, local_path, remote_path):
    local = pathlib.Path(local_path)
    if not local.exists():
        raise FileNotFoundError(str(local))
    remote_name = normalized_archive_name(remote_path)
    if local.is_dir():
        add_directory_entry(tar, remote_name)
        root = local.resolve()
        for path in local.rglob("*"):
            rel = path.relative_to(local).as_posix()
            dst = f"{remote_name.rstrip('/')}/{rel}"
            if path.is_symlink():
                try:
                    resolved = path.resolve()
                    resolved.relative_to(root)
                except ValueError:
                    raise RuntimeError(f"refusing to archive symlink outside directory artifact: {path}")
                if resolved.is_file():
                    add_file_entry(tar, resolved, dst)
                    continue
            if path.is_dir():
                add_directory_entry(tar, dst)
            else:
                add_file_entry(tar, path, dst)
    else:
        source = local.resolve() if local.is_symlink() else local
        add_file_entry(tar, source, remote_name)


def build_runtime_transfer_archive(spec):
    archive_path = pathlib.Path(sys.argv[1]).parent / "runtime_transfer.tar.gz"
    base_dirs = [
        "/bucephalus/in",
        "/bucephalus/out",
        "/bucephalus/state",
        "/bucephalus/workspace",
        "/bucephalus/tmp",
        "/bucephalus-events",
    ]
    with tarfile.open(archive_path, "w:gz") as tar:
        for directory in base_dirs:
            add_directory_entry(tar, normalized_archive_name(directory))
        for item in spec.get("runtime_files", []):
            add_runtime_path_to_archive(tar, item["local_path"], item["remote_path"])
    return archive_path


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


def bootstrap_runtime_transfer_exec(exec_spec):
    command = exec_spec["command"]
    workdir = exec_spec.get("workdir") or ""
    bootstrapped = dict(exec_spec)
    bootstrapped["command"] = [
        "/bin/sh",
        "-lc",
        "set -e\n"
        "tar -xzf /tmp/bucephalus-runtime-transfer.tar.gz -C /\n"
        "if [ -n \"$1\" ]; then cd \"$1\"; fi\n"
        "shift\n"
        "exec \"$@\"",
        "bucephalus-runtime-bootstrap",
        workdir,
        *command,
    ]
    # The requested workdir may be created by the archive extraction, so the
    # bootstrap shell must start from the image default and cd after extracting.
    bootstrapped["workdir"] = None
    return bootstrapped


def instrument_container_start_exec(exec_spec):
    command = exec_spec["command"]
    workdir = exec_spec.get("workdir") or ""
    instrumented = dict(exec_spec)
    instrumented["command"] = [
        "/bin/sh",
        "-lc",
        "printf 'BUCEPHALUS_CONTAINER_STARTED_AT=%s\\n' \"$(date -u +%Y-%m-%dT%H:%M:%S.%6NZ)\"\n"
        "if [ -n \"$1\" ]; then cd \"$1\"; fi\n"
        "shift\n"
        "exec \"$@\"",
        "bucephalus-container-start",
        workdir,
        *command,
    ]
    instrumented["workdir"] = None
    return instrumented


def split_container_start_marker(stdout):
    prefix = "BUCEPHALUS_CONTAINER_STARTED_AT="
    if not stdout.startswith(prefix):
        return None, stdout
    first_line, sep, rest = stdout.partition("\n")
    if not sep:
        return first_line[len(prefix):], ""
    return first_line[len(prefix):], rest


def timing_mark(timings, key):
    timings[key] = utc_now()


def run_process(sandbox, exec_spec, result, phase=None, bootstrap_runtime_transfer=False):
    if bootstrap_runtime_transfer:
        exec_spec = bootstrap_runtime_transfer_exec(exec_spec)
    exec_spec = instrument_container_start_exec(exec_spec)
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
    container_started_at, stdout = split_container_start_marker(stdout)
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
        "container_started_at": container_started_at,
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
        return file_exists(fs, remote_path + "/.bucephalus_asset_ready")
    return file_exists(fs, remote_path)


def stage_launch_mounts(app, spec, writable_asset_mount):
    items = spec.get("launch_mounts") or []
    if not items:
        return
    stager = None
    try:
        stager = modal.Sandbox.create(
            "sleep",
            "31536000",
            app=app,
            image=modal.Image.from_registry(spec["image"]),
            volumes={"/bucephalus/case_assets": writable_asset_mount},
            timeout=int(spec.get("sandbox_timeout_seconds", 3600)),
        )
        fs = stager.filesystem
        for item in items:
            if immutable_asset_ready(fs, item):
                continue
            copy_path(fs, item["local_path"], item["remote_path"])
            if item.get("source_is_dir"):
                fs.write_text("ok\n", item["remote_path"].rstrip("/") + "/.bucephalus_asset_ready")
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
        patch_path = "/bucephalus/out/candidate.patch"
        probe = sandbox.exec("git", "-C", workdir, "rev-parse", "--is-inside-work-tree", text=True)
        _ = probe.stdout.read()
        _ = probe.stderr.read()
        if wait_process(probe) != 0:
            patch_text = ""
        else:
            pathspec = ". ':(exclude).bucephalus' ':(exclude).haiku' ':(exclude).lab' ':(exclude)logs' ':(exclude)out'"
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


def create_sandbox(app, image_ref, case_assets_mount, spec, workdir, runtime_transfer_archive=None):
    volumes = {}
    if case_assets_mount is not None:
        volumes["/bucephalus/case_assets"] = case_assets_mount
    image = modal.Image.from_registry(image_ref)
    if runtime_transfer_archive is not None:
        image = image.add_local_file(
            str(runtime_transfer_archive),
            "/tmp/bucephalus-runtime-transfer.tar.gz",
        )
    create_kwargs = {}
    if spec.get("cpu_count") is not None:
        create_kwargs["cpu"] = float(spec["cpu_count"])
    if spec.get("memory_mb") is not None:
        create_kwargs["memory"] = int(spec["memory_mb"])
    secrets = []
    agent_secret = build_agent_secret(spec)
    if agent_secret is not None:
        secrets.append(agent_secret)
    return modal.Sandbox.create(
        "sleep",
        "31536000",
        app=app,
        image=image,
        volumes=volumes,
        env=spec.get("env", {}),
        secrets=secrets,
        workdir=workdir,
        block_network=bool(spec.get("block_network", False)),
        timeout=int(spec.get("sandbox_timeout_seconds", 3600)),
        **create_kwargs,
    )


def export_local_file_to_bucket(app, spec, sync, local_path, remote_path):
    local = pathlib.Path(local_path)
    if not local.exists():
        return False
    stager = None
    try:
        writable_mount = build_bucket_mount(sync, sync["prefix"], read_only=False)
        stager = modal.Sandbox.create(
            "sleep",
            "31536000",
            app=app,
            image=modal.Image.from_registry(spec["image"]),
            volumes={"/bucephalus": writable_mount},
            timeout=int(spec.get("sandbox_timeout_seconds", 3600)),
        )
        copy_path(stager.filesystem, str(local), remote_path)
        return True
    finally:
        if stager is not None:
            try:
                stager.terminate()
            finally:
                stager.detach()


def main():
    timings = {}
    timing_mark(timings, "python_main_started_at")
    spec = json.loads(pathlib.Path(sys.argv[1]).read_text())
    max_inline_capture_bytes = spec.get("max_inline_capture_bytes")
    if max_inline_capture_bytes is not None:
        max_inline_capture_bytes = int(max_inline_capture_bytes)
    sync = spec["sync"]
    timing_mark(timings, "app_lookup_started_at")
    app = app_lookup(spec["app_name"], spec.get("environment_name"))
    timing_mark(timings, "app_lookup_ended_at")
    timing_mark(timings, "runtime_transfer_archive_build_started_at")
    runtime_transfer_archive = build_runtime_transfer_archive(spec)
    timing_mark(timings, "runtime_transfer_archive_build_ended_at")
    case_assets_mount = None
    launch_mounts = spec.get("launch_mounts") or []
    timing_mark(timings, "launch_mounts_prepare_started_at")
    if launch_mounts:
        writable_asset_mount = build_bucket_mount(
            sync,
            sync["immutable_case_asset_prefix"],
            read_only=False,
        )
        stage_launch_mounts(app, spec, writable_asset_mount)
        case_assets_mount = build_bucket_mount(
            sync,
            sync["immutable_case_asset_prefix"],
            read_only=True,
        )
    timing_mark(timings, "launch_mounts_prepare_ended_at")
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
        "timings": timings,
    }
    try:
        timing_mark(timings, "sandbox_create_started_at")
        sandbox = create_sandbox(
            app,
            spec["image"],
            case_assets_mount,
            spec,
            spec.get("workdir"),
            runtime_transfer_archive=runtime_transfer_archive,
        )
        timing_mark(timings, "sandbox_create_ended_at")
        result["sandbox_id"] = getattr(sandbox, "object_id", None)
        write_runtime_worker("task", sandbox)
        fs = sandbox.filesystem
        bootstrap_runtime_transfer = not bool(spec.get("grader"))
        if not bootstrap_runtime_transfer:
            run_shell_checked(
                sandbox,
                "runtime_transfer_extract",
                "tar -xzf /tmp/bucephalus-runtime-transfer.tar.gz -C /",
                timeout_seconds=int(spec.get("sandbox_timeout_seconds", 3600)),
            )
            prepare_modal_grader(sandbox, spec["grader"])
        for index, exec_spec in enumerate(spec.get("execs", [])):
            record = run_process(
                sandbox,
                exec_spec,
                result,
                bootstrap_runtime_transfer=bootstrap_runtime_transfer and index == 0,
            )
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
                grader_sandbox = create_sandbox(
                    app,
                    grader["image"],
                    case_assets_mount,
                    spec,
                    grader.get("workdir"),
                    runtime_transfer_archive=runtime_transfer_archive,
                )
                write_runtime_worker("grading", grader_sandbox)
            transport_env = materialize_grader_inputs(grader_sandbox, grader, agent_outputs, task_payload)
            grader_env = dict(grader.get("env", {}))
            agent_status = "timeout" if timed_out else str(exit_code) if exit_code is not None else "signal"
            for key, value in list(grader_env.items()):
                if value == "__BUCEPHALUS_AGENT_EXIT_STATUS__":
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
            handle.write("\n[bucephalus modal launcher error]\n")
            handle.write("".join(traceback.format_exception(exc)))
        if not timed_out:
            raise
    finally:
        ended_at = utc_now()
        if sandbox is not None:
            fs = sandbox.filesystem
            copy_optional_to_local(fs, spec["result"]["remote_path"], spec["result"]["local_path"])
            copy_optional_to_local(fs, spec["events"]["scratch_path"], spec["events"]["local_path"])
            durable_events_path = spec["events"].get("durable_path")
            if durable_events_path:
                local_events_path = pathlib.Path(spec["events"]["local_path"])
                try:
                    export_local_file_to_bucket(app, spec, sync, str(local_events_path), durable_events_path)
                except Exception:
                    pass
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
        print("BUCEPHALUS_MODAL_RESULT=" + json.dumps(result, sort_keys=True))


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
    print("BUCEPHALUS_MODAL_CLEANUP=" + json.dumps(payload, sort_keys=True))
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
