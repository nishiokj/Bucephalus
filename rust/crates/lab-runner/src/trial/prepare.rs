use anyhow::{anyhow, Context, Result};
use lab_core::{
    canonical_json_digest, ensure_dir, runner_runtime_host_paths, RunnerRuntimeHostPaths,
    BUCEPHALUS_CONTRACT_IN_DIR, BUCEPHALUS_CONTRACT_OUT_DIR, BUCEPHALUS_ENV_CASE_ID,
    BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH, BUCEPHALUS_ENV_REPL_IDX, BUCEPHALUS_ENV_RESULT_PATH,
    BUCEPHALUS_ENV_RUN_ID, BUCEPHALUS_ENV_TASK_ID, BUCEPHALUS_ENV_TIMEOUT_MS,
    BUCEPHALUS_ENV_TRAJECTORY_PATH, BUCEPHALUS_ENV_TRIAL_ID, BUCEPHALUS_ENV_TRIAL_INPUT_PATH,
    BUCEPHALUS_ENV_VARIANT_ID,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::config::{atomic_write_json_pretty, effective_sanitization_profile, load_json_file};
use crate::experiment::runtime::AgentRuntimeConfig;
use crate::model::{
    PreparedContractFilePaths, PreparedMountReference, PreparedOutputMountReference,
    PreparedRuntimeImageManifest, PreparedTaskEnvironmentManifest, PreparedTrialIo,
    ResolvedMountReference, Variant, BUCEPHALUS_ENV_CASE_IMAGE, BUCEPHALUS_ENV_TASK_IMAGE,
    DEFAULT_CONTAINER_MAPPED_GRADER_OUTPUT_PATH, DEFAULT_CONTAINER_RESULT_PATH,
    DEFAULT_CONTAINER_TRAJECTORY_PATH, DEFAULT_CONTAINER_TRIAL_INPUT_PATH,
    PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION,
};
use crate::package::authoring::compute_artifact_content_digest;
use crate::package::cas::{
    materialize_package_cas_backed_path, path_contains_cas_pointer,
    resolve_package_cas_pointer_blob,
};
use crate::package::sealed::resolve_package_path_under_root;
use crate::package::staging::strip_task_workdir_support_destination_path;
use crate::persistence::rows::infer_run_dir_from_path;
use crate::trial::env::replace_task_workdir_placeholder;
use crate::trial::spec::TaskBoundaryMaterialization;
use crate::trial::state::{ArtifactMountPlan, IoMountPlan, TaskSandboxPlan};
use crate::util::{
    copy_dir_preserve_contents, copy_file_if_exists, remove_path_if_exists, sanitize_for_fs,
};

#[derive(Debug, Clone)]
pub(crate) struct TrialPaths {
    pub(crate) trial_dir: PathBuf,
    pub(crate) scratch_dir: PathBuf,
    pub(crate) in_dir: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) state: PathBuf,
    pub(crate) out: PathBuf,
    pub(crate) tmp: PathBuf,
    pub(crate) events: PathBuf,
    pub(crate) runtime: RunnerRuntimeHostPaths,
    pub(crate) exp_dir: PathBuf,
}

pub(crate) fn trial_runtime_scratch_dir(trial_dir: &Path) -> PathBuf {
    let root = infer_run_dir_from_path(trial_dir).unwrap_or_else(|| trial_dir.to_path_buf());
    let trial_label = trial_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("trial");
    static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    root.join(".scratch").join(format!(
        "{}_{}_{}",
        sanitize_for_fs(trial_label),
        std::process::id(),
        seq
    ))
}

impl TrialPaths {
    pub(crate) fn new(trial_dir: &Path, exp_dir: &Path) -> Result<Self> {
        let scratch_dir = trial_runtime_scratch_dir(trial_dir);
        let runtime = runner_runtime_host_paths(&scratch_dir);
        Ok(Self {
            trial_dir: trial_dir.to_path_buf(),
            scratch_dir,
            in_dir: runtime.in_dir.clone(),
            workspace: runtime.workspace_dir.clone(),
            state: runtime.state_dir.clone(),
            out: runtime.out_dir.clone(),
            tmp: runtime.tmp_dir.clone(),
            events: runtime.events_dir.clone(),
            runtime,
            exp_dir: exp_dir.to_path_buf(),
        })
    }

    pub(crate) fn prepare(&self, seed_workspace_from_exp_dir: bool) -> Result<()> {
        ensure_dir(&self.in_dir)?;
        ensure_dir(&self.workspace)?;
        ensure_dir(&self.state)?;
        ensure_dir(&self.out)?;
        ensure_dir(&self.tmp)?;
        ensure_dir(&self.events)?;
        if seed_workspace_from_exp_dir {
            crate::util::copy_dir_filtered(
                &self.exp_dir,
                &self.workspace,
                &[
                    ".lab",
                    ".git",
                    "node_modules",
                    ".venv",
                    "__pycache__",
                    ".tox",
                    ".mypy_cache",
                    ".pytest_cache",
                    ".ruff_cache",
                    "target",
                    "rust/target",
                    ".next",
                    ".nuxt",
                    ".turbo",
                    ".nx",
                    "coverage",
                    ".gradle",
                ],
            )?;
        }
        Ok(())
    }

    pub(crate) fn cleanup_scratch(&self) -> Result<()> {
        crate::util::remove_path_if_exists(&self.scratch_dir)
    }
}

impl Drop for TrialPaths {
    fn drop(&mut self) {
        if let Err(err) = crate::util::remove_path_if_exists(&self.scratch_dir) {
            eprintln!(
                "warning: failed to remove trial scratch directory {}: {}",
                self.scratch_dir.display(),
                err
            );
        }
    }
}

pub(crate) fn build_trial_input(
    json_value: &Value,
    run_id: &str,
    trial_id: &str,
    variant: &Variant,
    _task_idx: usize,
    repl: usize,
    task_boundary: &TaskBoundaryMaterialization,
) -> Value {
    let policy_timeout_ms = json_value
        .pointer("/policy/timeout_ms")
        .and_then(Value::as_u64);
    let time_limit_ms = task_boundary
        .time_limit_ms
        .or(policy_timeout_ms)
        .unwrap_or(600_000);
    let requested_network_mode = json_value
        .pointer("/runtime/network/task_sandbox")
        .or_else(|| json_value.pointer("/runtime/network/default"))
        .and_then(Value::as_str)
        .unwrap_or("none");
    let allowed_hosts = json_value
        .pointer("/runtime/network/egress")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let sanitization_profile = effective_sanitization_profile(json_value);
    let integration_level = json_value
        .pointer("/trial_runtime/agent/integration_level")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if json_value
                .pointer("/trial_runtime/agent/events")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
            {
                "cli_events"
            } else {
                "cli_basic"
            }
        });
    let artifact_type = json_value
        .pointer("/agent/artifact_type")
        .or_else(|| json_value.pointer("/trial_runtime/agent/artifact_type"))
        .and_then(Value::as_str)
        .unwrap_or("structured_json");

    let mut input = json!({
        "schema_version": "trial_input_v1",
        "ids": {
            "run_id": run_id,
            "trial_id": trial_id,
            "variant_id": variant.id,
            "case_id": task_boundary.task_id.as_str(),
            "repl_idx": repl
        },
        "case": task_boundary.task_payload.clone(),
        "artifact_type": artifact_type,
        "design": {
            "sanitization_profile": sanitization_profile,
            "integration_level": integration_level
        },
        "runtime": {
            "network_mode": requested_network_mode,
            "allowed_hosts": allowed_hosts,
            "case_image": task_boundary.task_image,
            "workdir": task_boundary.task_workdir,
            "time_limit_ms": time_limit_ms
        }
    });
    if let Some(obj) = input.as_object_mut() {
        obj.remove("ext");
    }
    input
}

pub(crate) fn prepared_task_environment_manifest_path(trial_dir: &Path) -> PathBuf {
    trial_dir
        .join("runner")
        .join("prepared_task_environment.json")
}

pub(crate) fn write_prepared_task_environment_manifest(
    trial_dir: &Path,
    manifest: &PreparedTaskEnvironmentManifest,
) -> Result<()> {
    let manifest_path = prepared_task_environment_manifest_path(trial_dir);
    atomic_write_json_pretty(&manifest_path, &serde_json::to_value(manifest)?)?;
    Ok(())
}

pub(crate) fn load_prepared_task_environment_manifest(
    trial_dir: &Path,
) -> Result<PreparedTaskEnvironmentManifest> {
    let manifest_path = prepared_task_environment_manifest_path(trial_dir);
    if !manifest_path.exists() {
        return Err(anyhow!(
            "prepared_task_environment manifest missing for trial '{}': {}",
            trial_dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown"),
            manifest_path.display()
        ));
    }
    let value = load_json_file(&manifest_path)?;
    let manifest: PreparedTaskEnvironmentManifest =
        serde_json::from_value(value).map_err(|err| {
            anyhow!(
                "invalid prepared_task_environment manifest at {}: {}",
                manifest_path.display(),
                err
            )
        })?;
    manifest.validate()?;
    Ok(manifest)
}

pub(crate) fn resolve_trial_timeout_ms(input: &Value) -> Option<u64> {
    input
        .pointer("/policy/timeout_ms")
        .or_else(|| input.pointer("/runtime/time_limit_ms"))
        .and_then(|v| v.as_u64())
}

pub(crate) fn build_runtime_contract_env(
    run_id: &str,
    input: &Value,
    io: &PreparedTrialIo,
    task_image: Option<&str>,
    timeout_ms: Option<u64>,
) -> std::collections::BTreeMap<String, String> {
    let trial_id = input
        .pointer("/ids/trial_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let variant_id = input
        .pointer("/ids/variant_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let case_id = input
        .pointer("/ids/case_id")
        .or_else(|| input.pointer("/ids/task_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let repl_idx = input
        .pointer("/ids/repl_idx")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut env = std::collections::BTreeMap::new();
    env.insert(
        BUCEPHALUS_ENV_TRIAL_INPUT_PATH.to_string(),
        io.trial_input_path.clone(),
    );
    env.insert(
        BUCEPHALUS_ENV_RESULT_PATH.to_string(),
        io.result_path.clone(),
    );
    env.insert(
        BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH.to_string(),
        io.mapped_grader_output_path.clone(),
    );
    env.insert(
        BUCEPHALUS_ENV_TRAJECTORY_PATH.to_string(),
        io.trajectory_path.clone(),
    );
    env.insert(BUCEPHALUS_ENV_RUN_ID.to_string(), run_id.to_string());
    env.insert(BUCEPHALUS_ENV_TRIAL_ID.to_string(), trial_id.to_string());
    env.insert(
        BUCEPHALUS_ENV_VARIANT_ID.to_string(),
        variant_id.to_string(),
    );
    env.insert(BUCEPHALUS_ENV_CASE_ID.to_string(), case_id.to_string());
    env.insert(BUCEPHALUS_ENV_TASK_ID.to_string(), case_id.to_string());
    if let Some(task_image) = task_image.map(str::trim).filter(|v| !v.is_empty()) {
        env.insert(
            BUCEPHALUS_ENV_CASE_IMAGE.to_string(),
            task_image.to_string(),
        );
        env.insert(
            BUCEPHALUS_ENV_TASK_IMAGE.to_string(),
            task_image.to_string(),
        );
    }
    if let Some(timeout_ms) = timeout_ms {
        env.insert(
            BUCEPHALUS_ENV_TIMEOUT_MS.to_string(),
            timeout_ms.to_string(),
        );
    }
    env.insert(BUCEPHALUS_ENV_REPL_IDX.to_string(), repl_idx.to_string());
    env
}

pub(crate) fn resolve_trial_io_host_path(path: &str, paths: &TrialPaths) -> Result<PathBuf> {
    crate::trial::execution::map_container_path_to_host(path, paths)
}

fn resolve_runtime_event_path(agent_runtime: Option<&AgentRuntimeConfig>) -> String {
    agent_runtime
        .and_then(|runtime| runtime.event_sinks.first())
        .map(|sink| sink.path.clone())
        .or_else(|| {
            agent_runtime
                .and_then(|runtime| runtime.trajectory_path.as_ref())
                .cloned()
        })
        .unwrap_or_else(|| DEFAULT_CONTAINER_TRAJECTORY_PATH.to_string())
}

pub(crate) fn prepare_io_paths(paths: &TrialPaths, input_bytes: &[u8]) -> Result<PreparedTrialIo> {
    prepare_io_paths_for_runtime(paths, input_bytes, None)
}

pub(crate) fn prepare_io_paths_for_runtime(
    paths: &TrialPaths,
    input_bytes: &[u8],
    agent_runtime: Option<&AgentRuntimeConfig>,
) -> Result<PreparedTrialIo> {
    let trial_input_path = DEFAULT_CONTAINER_TRIAL_INPUT_PATH.to_string();
    let result_path = DEFAULT_CONTAINER_RESULT_PATH.to_string();
    let mapped_grader_output_path = DEFAULT_CONTAINER_MAPPED_GRADER_OUTPUT_PATH.to_string();
    let trajectory_path = resolve_runtime_event_path(agent_runtime);
    let trial_input_host = resolve_trial_io_host_path(DEFAULT_CONTAINER_TRIAL_INPUT_PATH, paths)?;
    let result_host = resolve_trial_io_host_path(DEFAULT_CONTAINER_RESULT_PATH, paths)?;
    let mapped_grader_output_host =
        resolve_trial_io_host_path(DEFAULT_CONTAINER_MAPPED_GRADER_OUTPUT_PATH, paths)?;
    // The event stream is runner-owned: the agent appends to a container-local
    // scratch path (never blob-storage backed), so the host side is always the
    // trial events dir, independent of any container path the agent sees.
    let trajectory_host = paths.runtime.trajectory.clone();
    let events_host = trajectory_host.clone();

    for host_path in [
        &trial_input_host,
        &result_host,
        &mapped_grader_output_host,
        &trajectory_host,
    ] {
        if let Some(parent) = host_path.parent() {
            ensure_dir(parent)?;
        }
    }
    for writable_dir in [&paths.out, &paths.events] {
        make_container_writable_dir(writable_dir)?;
    }

    std::fs::write(&trial_input_host, input_bytes)?;

    remove_path_if_exists(&result_host)?;
    remove_path_if_exists(&mapped_grader_output_host)?;
    remove_path_if_exists(&trajectory_host)?;
    Ok(PreparedTrialIo {
        trial_input_host,
        result_host,
        events_host,
        trial_input_path,
        result_path,
        mapped_grader_output_path,
        trajectory_path,
    })
}

#[cfg(unix)]
fn make_container_writable_dir(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o777);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to make container-writable dir {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_container_writable_dir(_path: &Path) -> Result<()> {
    Ok(())
}

pub(crate) struct PreparedTaskEnvironment {
    pub(crate) manifest: PreparedTaskEnvironmentManifest,
    pub(crate) trial_paths: TrialPaths,
    pub(crate) io_paths: PreparedTrialIo,
    pub(crate) dynamic_mounts: Vec<ResolvedMountReference>,
    pub(crate) trial_input: Value,
}

const PREPARED_RUNTIME_IMAGE_MAP_ENV: &str = "BUCEPHALUS_PREPARED_RUNTIME_IMAGE_MAP";
pub(crate) const PREPARED_RUNTIME_IMAGE_MAP_PACKAGE_REL_PATH: &str =
    "runner/prepared_runtime_images.json";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedRuntimeImageMap {
    schema_version: String,
    #[serde(default)]
    #[allow(dead_code)]
    generated_at: Option<String>,
    entries: Vec<PreparedRuntimeImageMapEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedRuntimeImageMapEntry {
    base_image: String,
    agent_artifact_digest: String,
    agent_artifact_mount_path: String,
    runner_contract_version: String,
    #[serde(default)]
    platform: Option<String>,
    prepared_image: String,
}

fn normalize_optional_nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn prepared_runtime_image_platform_matches(
    entry_platform: Option<&str>,
    task_platform: Option<&str>,
) -> bool {
    match (
        normalize_optional_nonempty(entry_platform),
        normalize_optional_nonempty(task_platform),
    ) {
        (Some(entry), Some(task)) => entry == task,
        (None, None) => true,
        _ => false,
    }
}

fn agent_artifact_digest_for_prepared_image_lookup(
    agent_runtime: &AgentRuntimeConfig,
) -> Result<Option<String>> {
    let Some(artifact) = agent_runtime.agent_artifact.as_ref() else {
        return Ok(None);
    };
    if let Some(digest) = agent_runtime
        .agent_artifact_digest
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(digest.to_string()));
    }
    Ok(Some(compute_artifact_content_digest(artifact).with_context(|| {
        format!(
            "failed to digest trial_runtime.agent.mount.source for prepared runtime image lookup: {}",
            artifact.display()
        )
    })?))
}

fn resolve_prepared_runtime_image(
    package_root: &Path,
    task_boundary: &TaskBoundaryMaterialization,
    agent_runtime: &AgentRuntimeConfig,
) -> Result<Option<PreparedRuntimeImageManifest>> {
    let map_path = if let Some(env_path) = std::env::var_os(PREPARED_RUNTIME_IMAGE_MAP_ENV) {
        let env_path = PathBuf::from(env_path);
        if env_path.as_os_str().is_empty() {
            return Ok(None);
        }
        env_path
    } else {
        let package_map = package_root.join(PREPARED_RUNTIME_IMAGE_MAP_PACKAGE_REL_PATH);
        if !package_map.is_file() {
            return Ok(None);
        }
        package_map
    };
    let Some(agent_artifact_digest) =
        agent_artifact_digest_for_prepared_image_lookup(agent_runtime)?
    else {
        return Ok(None);
    };
    let Some(agent_artifact_mount_path) = agent_runtime
        .agent_artifact_mount_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let value = load_json_file(&map_path).with_context(|| {
        format!(
            "failed to load prepared runtime image map from {}",
            map_path.display()
        )
    })?;
    let map: PreparedRuntimeImageMap = serde_json::from_value(value).with_context(|| {
        format!(
            "failed to parse prepared runtime image map from {}",
            map_path.display()
        )
    })?;
    if map.schema_version != "prepared_runtime_image_map_v1" {
        return Err(anyhow!(
            "invalid prepared runtime image map schema_version '{}' in {}",
            map.schema_version,
            map_path.display()
        ));
    }
    let mut matched: Option<&PreparedRuntimeImageMapEntry> = None;
    for entry in &map.entries {
        if entry.base_image.trim() != task_boundary.task_image
            || entry.agent_artifact_digest.trim() != agent_artifact_digest
            || entry.agent_artifact_mount_path.trim() != agent_artifact_mount_path
            || entry.runner_contract_version.trim() != PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION
            || !prepared_runtime_image_platform_matches(
                entry.platform.as_deref(),
                task_boundary.materialization.platform.as_deref(),
            )
        {
            continue;
        }
        if entry.prepared_image.trim().is_empty() {
            return Err(anyhow!(
                "prepared runtime image map entry for base image '{}' has empty prepared_image",
                task_boundary.task_image
            ));
        }
        if let Some(previous) = matched {
            if previous.prepared_image.trim() != entry.prepared_image.trim() {
                return Err(anyhow!(
                    "prepared runtime image map has multiple prepared images for base image '{}' and agent digest '{}'",
                    task_boundary.task_image,
                    agent_artifact_digest
                ));
            }
        }
        matched = Some(entry);
    }
    let Some(entry) = matched else {
        return Ok(None);
    };
    Ok(Some(PreparedRuntimeImageManifest {
        image: entry.prepared_image.trim().to_string(),
        base_image: task_boundary.task_image.clone(),
        agent_artifact_digest,
        agent_artifact_mount_path: agent_artifact_mount_path.to_string(),
        runner_contract_version: PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION.to_string(),
        platform: normalize_optional_nonempty(entry.platform.as_deref()),
        source: Some(format!(
            "{}:{}",
            PREPARED_RUNTIME_IMAGE_MAP_ENV,
            map_path.display()
        )),
    }))
}

fn build_task_sandbox_plan(
    task_boundary: &TaskBoundaryMaterialization,
    agent_runtime: &AgentRuntimeConfig,
    time_limit_ms: u64,
    prepared_runtime_image: Option<&PreparedRuntimeImageManifest>,
) -> Result<TaskSandboxPlan> {
    Ok(TaskSandboxPlan {
        image: prepared_runtime_image
            .map(|prepared| prepared.image.clone())
            .unwrap_or_else(|| task_boundary.task_image.clone()),
        workdir: task_boundary.task_workdir.clone(),
        platform: task_boundary.materialization.platform.clone(),
        materialization: task_boundary.materialization.clone(),
        case_materialization: task_boundary.case_materialization.clone(),
        io_mounts: IoMountPlan {
            in_dir: BUCEPHALUS_CONTRACT_IN_DIR.to_string(),
            out_dir: BUCEPHALUS_CONTRACT_OUT_DIR.to_string(),
            telemetry_mounts: Vec::new(),
        },
        artifact_mount: if prepared_runtime_image.is_some() {
            None
        } else {
            if let Some(artifact) = agent_runtime.agent_artifact.as_ref() {
                let container_artifact_dir = agent_runtime
                    .agent_artifact_mount_path
                    .clone()
                    .ok_or_else(|| anyhow!("agent artifact mount path missing"))?;
                Some(ArtifactMountPlan {
                    host_artifact_path: artifact.to_string_lossy().to_string(),
                    container_artifact_dir,
                    read_only: agent_runtime.agent_artifact_read_only,
                })
            } else {
                None
            }
        },
        network_mode: agent_runtime.network.clone(),
        time_limit_ms,
    })
}

fn trial_input_asset_kind(value: &Value) -> Option<&str> {
    let kind = value
        .get("type")
        .or_else(|| value.get("kind"))
        .and_then(Value::as_str)?;
    if matches!(kind, "file" | "directory") {
        Some(kind)
    } else {
        None
    }
}

fn packaged_case_asset_path(obj: &serde_json::Map<String, Value>) -> Option<String> {
    obj.get("package_path")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            obj.get("uri")
                .and_then(Value::as_str)
                .and_then(|uri| uri.strip_prefix("package://"))
                .map(str::to_string)
        })
}

fn case_asset_projection_root(trial_paths: &TrialPaths) -> PathBuf {
    infer_run_dir_from_path(&trial_paths.trial_dir)
        .unwrap_or_else(|| {
            trial_paths
                .scratch_dir
                .parent()
                .unwrap_or(&trial_paths.scratch_dir)
                .to_path_buf()
        })
        .join(".case_asset_projections")
}

fn project_package_directory_asset_if_needed(
    package_root: &Path,
    package_path: &str,
    source: &Path,
    projection_root: &Path,
) -> Result<PathBuf> {
    if !path_contains_cas_pointer(source)? {
        return Ok(source.to_path_buf());
    }
    static PROJECTION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let projection_key = canonical_json_digest(&json!({
        "package_root": package_root.to_string_lossy(),
        "package_path": package_path,
    }))
    .replace(':', "_");
    let destination = projection_root.join(projection_key);
    if destination.exists() {
        if destination.is_dir() {
            return Ok(destination);
        }
        return Err(anyhow!(
            "case asset projection exists but is not a directory: {}",
            destination.display()
        ));
    }
    let _guard = PROJECTION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow!("case asset projection lock is poisoned"))?;
    if destination.exists() {
        if destination.is_dir() {
            return Ok(destination);
        }
        return Err(anyhow!(
            "case asset projection exists but is not a directory: {}",
            destination.display()
        ));
    }
    ensure_dir(projection_root)?;
    static PROJECTION_TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = projection_root.join(format!(
        ".projection.tmp.{}.{}",
        std::process::id(),
        PROJECTION_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    remove_path_if_exists(&tmp)?;
    materialize_package_cas_backed_path(package_root, source, &tmp)?;
    if let Err(err) = fs::rename(&tmp, &destination) {
        if let Err(cleanup_err) = remove_path_if_exists(&tmp) {
            eprintln!(
                "warning: failed to remove temporary case asset projection {}: {}",
                tmp.display(),
                cleanup_err
            );
        }
        return Err(err).with_context(|| {
            format!(
                "failed to publish case asset projection {}",
                destination.display()
            )
        });
    }
    Ok(destination)
}

fn materialize_trial_case_asset(
    package_root: &Path,
    projection_root: &Path,
    package_path: &str,
    kind: &str,
    projections: &mut BTreeMap<String, (String, PathBuf)>,
) -> Result<(String, PathBuf)> {
    let source = resolve_package_path_under_root(package_root, package_path, "case asset")?;
    let meta = source.metadata().map_err(|err| {
        anyhow!(
            "case asset '{}' is missing from package payload at {}: {}",
            package_path,
            source.display(),
            err
        )
    })?;
    if kind == "file" && !meta.is_file() {
        return Err(anyhow!(
            "case asset '{}' declares type=file but package payload is not a file",
            package_path
        ));
    }
    if kind == "directory" && !meta.is_dir() {
        return Err(anyhow!(
            "case asset '{}' declares type=directory but package payload is not a directory",
            package_path
        ));
    }
    if let Some(existing) = projections.get(package_path) {
        return Ok(existing.clone());
    }
    let name = source
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_for_fs)
        .unwrap_or_else(|| "asset".to_string());
    let runtime_path = format!("/bucephalus/case_assets/{:03}_{}", projections.len(), name);
    let host_path = if kind == "file" {
        resolve_package_cas_pointer_blob(package_root, &source)?.unwrap_or(source)
    } else if kind == "directory" {
        project_package_directory_asset_if_needed(
            package_root,
            package_path,
            &source,
            projection_root,
        )?
    } else {
        source
    };
    let projection = (runtime_path, host_path);
    projections.insert(package_path.to_string(), projection.clone());
    Ok(projection)
}

fn materialize_trial_input_case_assets_value(
    value: &mut Value,
    package_root: &Path,
    projection_root: &Path,
    projections: &mut BTreeMap<String, (String, PathBuf)>,
    context: &str,
) -> Result<()> {
    if let Some(items) = value.as_array_mut() {
        for (idx, item) in items.iter_mut().enumerate() {
            materialize_trial_input_case_assets_value(
                item,
                package_root,
                projection_root,
                projections,
                &format!("{}[{}]", context, idx),
            )?;
        }
        return Ok(());
    }
    let kind = trial_input_asset_kind(value).map(str::to_string);
    let Some(obj) = value.as_object_mut() else {
        return Ok(());
    };
    if let Some(kind) = kind {
        if let Some(package_path) = packaged_case_asset_path(obj) {
            let (container_path, _) = materialize_trial_case_asset(
                package_root,
                projection_root,
                &package_path,
                &kind,
                projections,
            )
            .with_context(|| {
                format!(
                    "failed to project {} package asset '{}'",
                    context, package_path
                )
            })?;
            obj.remove("package_path");
            obj.remove("uri");
            obj.insert("path".to_string(), Value::String(container_path));
        } else if let Some(path) = obj.get("path").and_then(Value::as_str) {
            if !path.starts_with(&format!("{}/", BUCEPHALUS_CONTRACT_IN_DIR))
                && path != BUCEPHALUS_CONTRACT_IN_DIR
            {
                return Err(anyhow!(
                    "{} declares {} asset path '{}' that has not been sealed into the package",
                    context,
                    kind,
                    path
                ));
            }
        }
        return Ok(());
    }
    for (key, nested) in obj.iter_mut() {
        materialize_trial_input_case_assets_value(
            nested,
            package_root,
            projection_root,
            projections,
            &format!("{}.{}", context, key),
        )?;
    }
    Ok(())
}

fn materialize_trial_input_case_assets(
    input: &mut Value,
    package_root: &Path,
    trial_paths: &TrialPaths,
    dynamic_mounts: &mut Vec<ResolvedMountReference>,
) -> Result<()> {
    let mut projections = BTreeMap::new();
    let projection_root = case_asset_projection_root(trial_paths);
    materialize_trial_input_case_assets_value(
        input,
        package_root,
        &projection_root,
        &mut projections,
        "trial_input",
    )?;
    dynamic_mounts.extend(projections.into_values().map(|(mount_path, host_path)| {
        ResolvedMountReference {
            host_path,
            mount_path,
            read_only: true,
        }
    }));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_task_environment_with_paths(
    trial_paths: TrialPaths,
    package_root: &Path,
    trial_dir: &Path,
    run_id: &str,
    trial_id: &str,
    trial_experiment: &Value,
    variant: &Variant,
    task_idx: usize,
    repl: usize,
    task_boundary: &TaskBoundaryMaterialization,
    agent_runtime: &AgentRuntimeConfig,
) -> Result<PreparedTaskEnvironment> {
    let task_interface = trial_experiment
        .pointer("/trial_runtime/task/interface")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("trial_runtime.task.interface is required"))?;
    if !matches!(
        task_interface,
        "input_only" | "readonly_files" | "writable_workspace"
    ) {
        return Err(anyhow::anyhow!(
            "trial_runtime.task.interface '{}' is not supported by this runner",
            task_interface
        ));
    }
    trial_paths.prepare(false)?;
    let mut dynamic_mounts = Vec::with_capacity(agent_runtime.dependency_file_staging.len());
    let staged_mount_root = trial_paths.tmp.join("runtime_mounts");
    let mut task_support_mount_root: Option<PathBuf> = None;
    let mut task_support_read_only = true;
    for (idx, spec) in agent_runtime.dependency_file_staging.iter().enumerate() {
        let host_path = if path_contains_cas_pointer(&spec.source_from_host)? {
            let materialized = staged_mount_root.join(format!("mount_{}", idx));
            materialize_package_cas_backed_path(
                package_root,
                &spec.source_from_host,
                &materialized,
            )?;
            materialized
        } else {
            spec.source_from_host.clone()
        };
        if !host_path.exists() {
            if spec.required {
                return Err(anyhow!(
                    "dependency file staging source missing for {}: {}",
                    spec.destination_path,
                    host_path.display()
                ));
            }
            continue;
        }
        if let Some(rel) = strip_task_workdir_support_destination_path(&spec.destination_path) {
            let support_root = task_support_mount_root
                .get_or_insert_with(|| staged_mount_root.join("task_workdir_bucephalus"));
            let support_dir = support_root.join("support");
            let destination = if rel.is_empty() {
                support_dir
            } else {
                support_dir.join(rel)
            };
            if host_path.is_dir() {
                copy_dir_preserve_contents(&host_path, &destination)?;
            } else {
                copy_file_if_exists(&host_path, &destination)?;
            }
            task_support_read_only &= spec.read_only;
            continue;
        }
        dynamic_mounts.push(ResolvedMountReference {
            host_path,
            mount_path: replace_task_workdir_placeholder(
                &spec.destination_path,
                &task_boundary.task_workdir,
            ),
            read_only: spec.read_only,
        });
    }
    if let Some(support_root) = task_support_mount_root {
        dynamic_mounts.push(ResolvedMountReference {
            host_path: support_root,
            mount_path: replace_task_workdir_placeholder(
                "__BUCEPHALUS_TASK_WORKDIR__/.bucephalus",
                &task_boundary.task_workdir,
            ),
            read_only: task_support_read_only,
        });
    }

    let mut input = build_trial_input(
        trial_experiment,
        run_id,
        trial_id,
        variant,
        task_idx,
        repl,
        task_boundary,
    );
    if matches!(
        task_boundary
            .declaration
            .get("schema_version")
            .and_then(Value::as_str),
        Some("case_v1" | "case_v2" | "task_case_v1")
    ) {
        materialize_trial_input_case_assets(
            &mut input,
            package_root,
            &trial_paths,
            &mut dynamic_mounts,
        )?;
    }
    let input_bytes = serde_json::to_vec_pretty(&input)?;
    let io_paths = prepare_io_paths_for_runtime(&trial_paths, &input_bytes, Some(agent_runtime))?;
    let output_mounts = agent_runtime
        .output_mounts
        .iter()
        .map(|mount| {
            let host_path = trial_paths.out.join(&mount.path);
            ensure_dir(&host_path)?;
            make_container_writable_dir(&host_path)?;
            Ok(PreparedOutputMountReference {
                id: mount.id.clone(),
                kind: mount.kind.clone(),
                host_path: host_path.to_string_lossy().to_string(),
                container_path: mount.container_path(),
                env: mount.env.clone(),
                persist: mount.persist,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let resolved_time_limit_ms = resolve_trial_timeout_ms(&input).unwrap_or(600000);
    let runtime_env = build_runtime_contract_env(
        run_id,
        &input,
        &io_paths,
        Some(task_boundary.task_image.as_str()),
        Some(resolved_time_limit_ms),
    );
    let prepared_runtime_image =
        resolve_prepared_runtime_image(package_root, task_boundary, agent_runtime)?;
    let task_sandbox_plan = build_task_sandbox_plan(
        task_boundary,
        agent_runtime,
        resolved_time_limit_ms,
        prepared_runtime_image.as_ref(),
    )?;
    let manifest = PreparedTaskEnvironmentManifest {
        schema_version: "prepared_task_environment_v1".to_string(),
        declaration: task_boundary.declaration.clone(),
        declaration_digest: canonical_json_digest(&task_boundary.declaration),
        run_id: run_id.to_string(),
        trial_id: trial_id.to_string(),
        variant_id: variant.id.clone(),
        task_id: task_boundary.task_id.clone(),
        task_index: task_idx,
        repl_idx: repl,
        task_image: task_boundary.task_image.clone(),
        workspace_root: trial_paths.workspace.to_string_lossy().to_string(),
        aux_mounts: dynamic_mounts
            .iter()
            .map(|mount| PreparedMountReference {
                host_path: mount.host_path.to_string_lossy().to_string(),
                mount_path: mount.mount_path.clone(),
                read_only: mount.read_only,
            })
            .collect(),
        output_mounts,
        contract_files: PreparedContractFilePaths {
            trial_input: io_paths.trial_input_path.clone(),
            result: io_paths.result_path.clone(),
            mapped_grader_output: io_paths.mapped_grader_output_path.clone(),
            trajectory: io_paths.trajectory_path.clone(),
        },
        runtime_env: runtime_env.clone(),
        prepared_runtime_image,
        task_sandbox_plan: Some(task_sandbox_plan),
    };
    write_prepared_task_environment_manifest(trial_dir, &manifest)?;

    Ok(PreparedTaskEnvironment {
        manifest,
        trial_paths,
        io_paths,
        dynamic_mounts,
        trial_input: input,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_task_environment(
    project_root: &Path,
    trial_dir: &Path,
    run_id: &str,
    trial_id: &str,
    trial_experiment: &Value,
    variant: &Variant,
    task_idx: usize,
    repl: usize,
    task_boundary: &TaskBoundaryMaterialization,
    agent_runtime: &AgentRuntimeConfig,
) -> Result<PreparedTaskEnvironment> {
    let trial_paths = TrialPaths::new(trial_dir, project_root)?;
    prepare_task_environment_with_paths(
        trial_paths,
        project_root,
        trial_dir,
        run_id,
        trial_id,
        trial_experiment,
        variant,
        task_idx,
        repl,
        task_boundary,
        agent_runtime,
    )
}
