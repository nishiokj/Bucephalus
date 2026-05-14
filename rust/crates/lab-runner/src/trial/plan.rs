use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{GradingStrategy, MetricDefinition};
use crate::trial::spec::TaskRow;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskInterface {
    InputOnly,
    ReadonlyFiles,
    WritableWorkspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkspaceSource {
    ContainerImage,
    Files,
    Archive,
    Git,
    Empty,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentSite {
    TaskRuntime,
    AgentContainer,
    Host,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PatchOutputMode {
    None,
    WorkspaceDiff,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeConfig {
    pub(crate) task: TrialRuntimeTaskConfig,
    pub(crate) agent: TrialRuntimeAgentConfig,
    pub(crate) execution: TrialRuntimeExecutionConfig,
    pub(crate) outputs: TrialRuntimeOutputsConfig,
    pub(crate) grader: TrialRuntimeGraderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeTaskConfig {
    pub(crate) interface: TaskInterface,
    #[serde(default)]
    pub(crate) files: Option<TrialRuntimeFilesConfig>,
    #[serde(default)]
    pub(crate) workspace: Option<TrialRuntimeWorkspaceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeFilesConfig {
    pub(crate) source: WorkspaceSource,
    pub(crate) path: String,
    pub(crate) mount_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeWorkspaceConfig {
    pub(crate) source: WorkspaceSource,
    #[serde(default)]
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) repo: Option<String>,
    #[serde(default)]
    pub(crate) rev: Option<String>,
    #[serde(default)]
    pub(crate) image: Option<RuntimeFieldSource>,
    #[serde(default)]
    pub(crate) workdir: Option<RuntimeFieldSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeFieldSource {
    pub(crate) from: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeAgentConfig {
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) image: Option<String>,
    #[serde(default)]
    pub(crate) artifact: Option<String>,
    #[serde(default)]
    pub(crate) artifact_digest: Option<String>,
    #[serde(default)]
    pub(crate) artifact_resolved_path: Option<String>,
    #[serde(default)]
    pub(crate) integration_level: Option<String>,
    #[serde(default)]
    pub(crate) network: Option<String>,
    #[serde(default)]
    pub(crate) env: Value,
    #[serde(default)]
    pub(crate) events: Value,
    #[serde(default)]
    pub(crate) telemetry: Value,
    #[serde(default)]
    pub(crate) output_mounts: Value,
    #[serde(default)]
    pub(crate) secret_files: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeExecutionConfig {
    pub(crate) agent_site: AgentSite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeOutputsConfig {
    pub(crate) result: TrialRuntimeResultOutput,
    pub(crate) patch: TrialRuntimePatchOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeResultOutput {
    pub(crate) path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimePatchOutput {
    pub(crate) mode: PatchOutputMode,
    #[serde(default)]
    pub(crate) path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeGraderConfig {
    pub(crate) strategy: GradingStrategy,
    #[serde(default)]
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) conclusion: Value,
    #[serde(default)]
    pub(crate) injected: Value,
    #[serde(default)]
    pub(crate) separate: Value,
    #[serde(default)]
    pub(crate) host: Value,
}

pub(crate) fn parse_trial_runtime_config(experiment: &Value) -> Result<TrialRuntimeConfig> {
    let value = experiment
        .get("trial_runtime")
        .ok_or_else(|| anyhow!("/trial_runtime is required"))?;
    let config: TrialRuntimeConfig = serde_json::from_value(value.clone())
        .map_err(|err| anyhow!("invalid /trial_runtime: {}", err))?;
    validate_trial_runtime_config(experiment, &config)?;
    Ok(config)
}

pub(crate) fn validate_trial_runtime_config(
    experiment: &Value,
    config: &TrialRuntimeConfig,
) -> Result<()> {
    if config.agent.command.is_empty()
        || config
            .agent
            .command
            .iter()
            .any(|part| part.trim().is_empty())
    {
        return Err(anyhow!(
            "/trial_runtime/agent/command must be a non-empty argv array"
        ));
    }
    match config.execution.agent_site {
        AgentSite::TaskRuntime => {
            let workspace = required_workspace(config)?;
            if config.task.interface != TaskInterface::WritableWorkspace
                || workspace.source != WorkspaceSource::ContainerImage
            {
                return Err(anyhow!(
                    "agent_site=task_runtime requires task.interface=writable_workspace and workspace.source=container_image"
                ));
            }
            if config
                .agent
                .artifact
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(anyhow!(
                    "agent_site=task_runtime requires /trial_runtime/agent/artifact"
                ));
            }
            if config.agent.image.is_some() {
                return Err(anyhow!(
                    "agent_site=task_runtime forbids /trial_runtime/agent/image"
                ));
            }
        }
        AgentSite::AgentContainer => {
            if config
                .agent
                .image
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(anyhow!(
                    "agent_site=agent_container requires /trial_runtime/agent/image"
                ));
            }
        }
        AgentSite::Host => {
            if config.agent.image.is_some() {
                return Err(anyhow!(
                    "agent_site=host forbids /trial_runtime/agent/image"
                ));
            }
        }
    }

    match config.task.interface {
        TaskInterface::InputOnly => {
            if config.task.files.is_some() || config.task.workspace.is_some() {
                return Err(anyhow!(
                    "task.interface=input_only must not declare task files or workspace"
                ));
            }
            if config.execution.agent_site == AgentSite::TaskRuntime {
                return Err(anyhow!(
                    "task.interface=input_only cannot use agent_site=task_runtime"
                ));
            }
        }
        TaskInterface::ReadonlyFiles => {
            let files = config.task.files.as_ref().ok_or_else(|| {
                anyhow!("task.interface=readonly_files requires /trial_runtime/task/files")
            })?;
            if !matches!(
                files.source,
                WorkspaceSource::Files | WorkspaceSource::Archive
            ) {
                return Err(anyhow!(
                    "task.interface=readonly_files supports files.source=files or archive only"
                ));
            }
            if config.outputs.patch.mode != PatchOutputMode::None {
                return Err(anyhow!(
                    "task.interface=readonly_files requires outputs.patch.mode=none"
                ));
            }
        }
        TaskInterface::WritableWorkspace => {
            let workspace = required_workspace(config)?;
            if config.execution.agent_site == AgentSite::TaskRuntime
                && workspace.source != WorkspaceSource::ContainerImage
            {
                return Err(anyhow!(
                    "agent_site=task_runtime requires a task workspace source that provides a process runtime; source does not"
                ));
            }
        }
    }

    if config.outputs.patch.mode == PatchOutputMode::WorkspaceDiff
        && config.task.interface != TaskInterface::WritableWorkspace
    {
        return Err(anyhow!(
            "outputs.patch.mode=workspace_diff requires task.interface=writable_workspace"
        ));
    }
    if config.outputs.patch.mode == PatchOutputMode::File
        && config
            .outputs
            .patch
            .path
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        return Err(anyhow!(
            "outputs.patch.mode=file requires outputs.patch.path"
        ));
    }

    validate_grader_metrics(experiment, config)?;
    Ok(())
}

pub(crate) fn validate_task_row_for_trial_runtime(
    runtime: &TrialRuntimeConfig,
    task_row: &TaskRow,
) -> Result<()> {
    match runtime.task.interface {
        TaskInterface::InputOnly => {
            if task_row.runtime.container_image.is_some() {
                return Err(anyhow!(
                    "task.interface=input_only rejects task_row_v2.runtime.container_image"
                ));
            }
        }
        TaskInterface::ReadonlyFiles => {
            if task_row.runtime.container_image.is_some() {
                return Err(anyhow!(
                    "task.interface=readonly_files rejects task_row_v2.runtime.container_image"
                ));
            }
        }
        TaskInterface::WritableWorkspace => {
            let workspace = required_workspace(runtime)?;
            if workspace.source == WorkspaceSource::ContainerImage {
                let container = task_row.container_image().ok_or_else(|| {
                    anyhow!(
                        "workspace.source=container_image requires task_row_v2.runtime.container_image"
                    )
                })?;
                if container.image.trim().is_empty() || container.workdir.trim().is_empty() {
                    return Err(anyhow!(
                        "workspace.source=container_image requires non-empty task row image and workdir"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn required_workspace(config: &TrialRuntimeConfig) -> Result<&TrialRuntimeWorkspaceConfig> {
    config
        .task
        .workspace
        .as_ref()
        .ok_or_else(|| anyhow!("task.interface=writable_workspace requires task.workspace"))
}

fn validate_grader_metrics(experiment: &Value, runtime: &TrialRuntimeConfig) -> Result<()> {
    if runtime.grader.strategy == GradingStrategy::None && !runtime.grader.command.is_empty() {
        return Err(anyhow!("grader.strategy=none must not declare command"));
    }
    if matches!(
        runtime.grader.strategy,
        GradingStrategy::InTaskRuntime | GradingStrategy::Injected
    ) {
        let workspace = required_workspace(runtime)?;
        if runtime.task.interface != TaskInterface::WritableWorkspace
            || workspace.source != WorkspaceSource::ContainerImage
        {
            return Err(anyhow!(
                "grader strategy requires task.interface=writable_workspace and workspace.source=container_image"
            ));
        }
    }
    let metrics = crate::config::parse_metric_definitions(experiment)?;
    for metric in &metrics {
        if metric.source.source_type == "grader_result"
            && runtime.grader.strategy == GradingStrategy::None
        {
            return Err(anyhow!(
                "metric '{}' uses source.type=grader_result but grader.strategy=none",
                metric.id
            ));
        }
    }
    let _metrics: Vec<MetricDefinition> = metrics;
    Ok(())
}
