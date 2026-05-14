use anyhow::{anyhow, Result};
use lab_core::{AGENTLAB_CONTRACT_RUNTIME_AUX_DIR, AGENTLAB_TASK_WORKDIR_PLACEHOLDER};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{canonicalize_best_effort, normalize_path};
use crate::experiment::preflight::is_runner_staged_script_path;
use crate::experiment::runtime::TASK_WORKDIR_TEMPLATE_PLACEHOLDER;
use crate::model::{
    BenchmarkGraderConfig, GraderConclusionMode, GradingStrategy, ResolvedMountReference,
    AGENT_ARTIFACT_PATH_ENV_VALUE, HOST_GRADER_CAPABILITIES_DIR, HOST_GRADER_CAPABILITY_PREFIX,
    MAPPED_GRADER_OUTPUT_FILENAME, RAW_GRADER_OUTPUT_FILENAME,
};
use crate::package::staging::matches_contract_runtime_root;
use crate::trial::execution::AdapterRunRequest;
use crate::trial::execution::{
    map_container_path_to_host, resolve_container_workspace, resolve_task_sandbox_image,
};

pub(crate) struct ResolvedGradingPhase {
    pub(crate) image: String,
    pub(crate) workdir: String,
    pub(crate) command: Vec<String>,
    pub(crate) extra_mounts: Vec<ResolvedMountReference>,
    pub(crate) injected_bundle_host_path: Option<PathBuf>,
    pub(crate) injected_copy_dest: Option<String>,
}

fn resolve_grading_bundle_host_path(
    request: &AdapterRunRequest<'_>,
    raw_bundle: &str,
) -> Result<PathBuf> {
    let rendered = replace_task_workdir_placeholder(raw_bundle, request.task_workdir);
    if rendered.starts_with("/agentlab/") || rendered.starts_with(AGENTLAB_TASK_WORKDIR_PLACEHOLDER)
    {
        return map_container_path_to_host(&rendered, request.trial_paths);
    }
    Ok(PathBuf::from(rendered))
}

pub(crate) fn resolve_grading_phase(
    request: &AdapterRunRequest<'_>,
    grader: &BenchmarkGraderConfig,
    base_command: &[String],
) -> Result<ResolvedGradingPhase> {
    let task_image = resolve_task_sandbox_image(request)?;
    let task_workdir = resolve_container_workspace(request)?;
    match grader.strategy {
        GradingStrategy::None => Err(anyhow!("grader.strategy=none has no grading phase")),
        GradingStrategy::InTaskRuntime => Ok(ResolvedGradingPhase {
            image: task_image,
            workdir: task_workdir.to_string(),
            command: base_command.to_vec(),
            extra_mounts: Vec::new(),
            injected_bundle_host_path: None,
            injected_copy_dest: None,
        }),
        GradingStrategy::Separate => {
            let separate = grader.separate.as_ref().ok_or_else(|| {
                anyhow!("trial_runtime.grader.separate is required when strategy='separate'")
            })?;
            Ok(ResolvedGradingPhase {
                image: separate.image.clone(),
                workdir: separate.workdir.clone(),
                command: base_command.to_vec(),
                extra_mounts: Vec::new(),
                injected_bundle_host_path: None,
                injected_copy_dest: None,
            })
        }
        GradingStrategy::Host => Ok(ResolvedGradingPhase {
            image: "host".to_string(),
            workdir: request.trial_paths.exp_dir.to_string_lossy().to_string(),
            command: base_command.to_vec(),
            extra_mounts: Vec::new(),
            injected_bundle_host_path: None,
            injected_copy_dest: None,
        }),
        GradingStrategy::Injected => {
            let injected = grader.injected.as_ref().ok_or_else(|| {
                anyhow!("trial_runtime.grader.injected is required when strategy='injected'")
            })?;
            let bundle_host_path = resolve_grading_bundle_host_path(request, &injected.bundle)?;
            if !bundle_host_path.exists() {
                return Err(anyhow!(
                    "benchmark grader bundle not found for injected strategy: {}",
                    bundle_host_path.display()
                ));
            }
            Ok(ResolvedGradingPhase {
                image: task_image,
                workdir: task_workdir.to_string(),
                command: base_command.to_vec(),
                extra_mounts: Vec::new(),
                injected_bundle_host_path: Some(bundle_host_path),
                injected_copy_dest: Some(injected.copy_dest.clone()),
            })
        }
    }
}

fn validate_package_relative_path(raw: &str, context: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{} must not be empty", context));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(anyhow!("{} must be relative", context));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(value) => normalized.push(value),
            std::path::Component::ParentDir => {
                return Err(anyhow!("{} must not contain '..'", context))
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(anyhow!("{} must be relative", context))
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(anyhow!("{} cannot resolve to empty", context));
    }
    Ok(normalized)
}

pub(crate) fn host_grader_capability_package_path(
    package_root: &Path,
    capability: &str,
    relative_path: &str,
) -> Result<PathBuf> {
    let capability = validate_package_relative_path(capability, "host grader capability id")?;
    if capability.components().count() != 1 {
        return Err(anyhow!(
            "host grader capability id must be a single path segment"
        ));
    }
    let relative_path =
        validate_package_relative_path(relative_path, "host grader capability path")?;
    let capability_root = normalize_path(
        &package_root
            .join(HOST_GRADER_CAPABILITIES_DIR)
            .join(&capability),
    );
    let resolved = normalize_path(&capability_root.join(&relative_path));
    let root_cmp = canonicalize_best_effort(&capability_root);
    let resolved_cmp = canonicalize_best_effort(&resolved);
    if !resolved_cmp.starts_with(&root_cmp) {
        return Err(anyhow!(
            "host grader capability path resolves outside package capability root: {}",
            resolved.display()
        ));
    }
    if !resolved.is_file() {
        return Err(anyhow!(
            "host grader capability file not found in package: {}",
            resolved.display()
        ));
    }
    Ok(resolved)
}

pub(crate) fn resolve_host_grader_command(
    grader: &BenchmarkGraderConfig,
    package_root: &Path,
) -> Result<Vec<String>> {
    let host = grader.host.as_ref().ok_or_else(|| {
        anyhow!("trial_runtime.grader.host.capability is required when strategy='host'")
    })?;
    if host.capability.trim().is_empty() {
        return Err(anyhow!(
            "trial_runtime.grader.host.capability must not be empty when strategy='host'"
        ));
    }
    let mut saw_capability_path = false;
    let mut resolved = Vec::with_capacity(grader.command.len());
    for (idx, token) in grader.command.iter().enumerate() {
        let trimmed = token.trim();
        if trimmed == HOST_GRADER_CAPABILITY_PREFIX
            || trimmed.starts_with(&format!("{}/", HOST_GRADER_CAPABILITY_PREFIX))
        {
            let rel = trimmed
                .strip_prefix(HOST_GRADER_CAPABILITY_PREFIX)
                .unwrap_or_default()
                .trim_start_matches('/');
            let mut parts = rel.splitn(2, '/');
            let capability = parts.next().unwrap_or_default();
            let relative_path = parts.next().unwrap_or_default();
            if capability != host.capability {
                return Err(anyhow!(
                    "trial_runtime.grader.command[{}] uses host grader capability '{}' but trial_runtime.grader.host.capability is '{}'",
                    idx,
                    capability,
                    host.capability
                ));
            }
            let path =
                host_grader_capability_package_path(package_root, capability, relative_path)?;
            saw_capability_path = true;
            resolved.push(path.to_string_lossy().to_string());
            continue;
        }
        if idx > 0 {
            if trimmed.starts_with(AGENTLAB_TASK_WORKDIR_PLACEHOLDER)
                || trimmed.starts_with("/agentlab/")
                || Path::new(trimmed).is_absolute()
            {
                return Err(anyhow!(
                    "trial_runtime.grader.command[{}] crosses runtime boundaries: host grader commands cannot reference task-workdir assets, /agentlab paths, or arbitrary absolute host paths",
                    idx
                ));
            }
        }
        resolved.push(token.clone());
    }
    if !saw_capability_path {
        return Err(anyhow!(
            "strategy='host' requires trial_runtime.grader.command to reference a package host grader capability under {}/<capability>/...",
            HOST_GRADER_CAPABILITY_PREFIX
        ));
    }
    Ok(resolved)
}

fn matches_grader_strategy_runtime_root(
    grader: &BenchmarkGraderConfig,
    script_path: &str,
    task_workdir: &str,
) -> bool {
    match grader.strategy {
        GradingStrategy::None => false,
        GradingStrategy::InTaskRuntime => matches_contract_runtime_root(script_path, task_workdir),
        GradingStrategy::Injected => {
            matches_contract_runtime_root(script_path, task_workdir)
                || grader.injected.as_ref().is_some_and(|config| {
                    matches_contract_runtime_root(script_path, &config.copy_dest)
                })
        }
        GradingStrategy::Separate => grader
            .separate
            .as_ref()
            .is_some_and(|config| matches_contract_runtime_root(script_path, &config.workdir)),
        GradingStrategy::Host => false,
    }
}

pub(crate) fn resolve_benchmark_grader_command(
    request: &AdapterRunRequest<'_>,
) -> Result<Option<Vec<String>>> {
    if !request.benchmark_grading_enabled {
        return Ok(None);
    }
    let Some(grader) = request.benchmark_grader else {
        return Ok(None);
    };
    if grader.command.is_empty() {
        return Ok(None);
    }
    if matches!(grader.strategy, GradingStrategy::Host) {
        return Ok(Some(resolve_host_grader_command(
            grader,
            request.package_root,
        )?));
    }
    let workspace = resolve_container_workspace(request)?;
    let rendered = grader
        .command
        .iter()
        .map(|token| replace_task_workdir_placeholder(token, workspace))
        .collect::<Vec<_>>();
    if let Some(script_path) = rendered.get(1).map(|value| value.trim()) {
        if Path::new(script_path).is_absolute()
            && !is_runner_staged_script_path(script_path)
            && !matches_grader_strategy_runtime_root(grader, script_path, workspace)
        {
            return Err(anyhow!(
                "forbidden benchmark grader script path '{}': script must be under the selected grader runtime boundary",
                script_path
            ));
        }
    }
    Ok(Some(rendered))
}

pub(crate) fn benchmark_grader_uses_mapper(grader: Option<&BenchmarkGraderConfig>) -> bool {
    grader.is_some_and(|grader| matches!(grader.conclusion.mode, GraderConclusionMode::Mapper))
}

pub(crate) fn benchmark_grader_expected_output_filename(
    grader: Option<&BenchmarkGraderConfig>,
) -> &'static str {
    if benchmark_grader_uses_mapper(grader) {
        RAW_GRADER_OUTPUT_FILENAME
    } else {
        MAPPED_GRADER_OUTPUT_FILENAME
    }
}

pub(crate) fn resolve_benchmark_conclusion_mapper_command(
    request: &AdapterRunRequest<'_>,
    grader: &BenchmarkGraderConfig,
) -> Result<Option<Vec<String>>> {
    if !matches!(grader.conclusion.mode, GraderConclusionMode::Mapper) {
        return Ok(None);
    }
    let mapper = grader
        .conclusion
        .mapper
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "trial_runtime.grader.conclusion.mapper is required when trial_runtime.grader.conclusion.mode='mapper'"
            )
        })?;
    let workspace = resolve_container_workspace(request)?;
    let rendered = replace_task_workdir_placeholder(mapper, workspace);
    if Path::new(&rendered).is_absolute()
        && !is_runner_staged_script_path(&rendered)
        && !matches_contract_runtime_root(&rendered, workspace)
    {
        return Err(anyhow!(
            "forbidden benchmark conclusion mapper path '{}': mapper must be under {} or the task workdir",
            rendered,
            AGENTLAB_CONTRACT_RUNTIME_AUX_DIR
        ));
    }
    Ok(Some(vec![rendered]))
}

pub(crate) fn resolve_runtime_agent_command(
    request: &AdapterRunRequest<'_>,
) -> Result<Vec<String>> {
    if request.runtime.command_raw.is_empty() {
        return Err(anyhow!("resolved trial_runtime.agent.command is empty"));
    }
    let mut command = request
        .runtime
        .command_raw
        .iter()
        .map(|token| {
            replace_event_path_placeholders(
                &replace_task_workdir_placeholder(token, request.task_workdir),
                request,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    command.extend(
        request
            .variant_args
            .iter()
            .map(|token| {
                replace_event_path_placeholders(
                    &replace_task_workdir_placeholder(token, request.task_workdir),
                    request,
                )
            })
            .collect::<Result<Vec<_>>>()?,
    );
    Ok(command)
}

fn replace_event_path_placeholders(raw: &str, request: &AdapterRunRequest<'_>) -> Result<String> {
    let mut rendered = raw.replace(
        "__AGENTLAB_TRAJECTORY_PATH__",
        request.io_paths.trajectory_path.as_str(),
    );
    for sink in &request.runtime.event_sinks {
        let placeholder = format!("__AGENTLAB_EVENT_PATH_{}__", sink.id);
        rendered = rendered.replace(&placeholder, sink.path.as_str());
    }
    if rendered.contains("__AGENTLAB_EVENT_PATH_") {
        return Err(anyhow!(
            "trial_runtime.agent.command references an unknown __AGENTLAB_EVENT_PATH_<id>__ placeholder"
        ));
    }
    Ok(rendered)
}

pub(crate) fn replace_task_workdir_placeholder(raw: &str, task_workdir: &str) -> String {
    raw.replace(TASK_WORKDIR_TEMPLATE_PLACEHOLDER, task_workdir)
}

pub(crate) fn build_exec_env(
    request: &AdapterRunRequest<'_>,
    workspace: &str,
    extra_env: Option<(&str, &str)>,
    include_agent_path: bool,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (key, value) in request.runtime_overrides_env {
        env.insert(
            key.clone(),
            replace_task_workdir_placeholder(value, workspace),
        );
    }
    for (key, value) in request.runtime_env {
        env.insert(
            key.clone(),
            replace_task_workdir_placeholder(value, workspace),
        );
    }
    if include_agent_path && request.agent_artifact.is_some() && !env.contains_key("PATH") {
        if let Some((_, value)) = AGENT_ARTIFACT_PATH_ENV_VALUE.split_once('=') {
            env.insert("PATH".to_string(), value.to_string());
        }
    }
    env.insert("WORKSPACE".to_string(), workspace.to_string());
    if let Some((key, value)) = extra_env {
        env.insert(key.to_string(), value.to_string());
    }
    env
}
