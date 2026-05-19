use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::model::{
    GradingStrategy, MetricDefinition, RuntimeInputConfig, RuntimeOutputConfig,
    RuntimeTransportSourceConfig, DEFAULT_CONTAINER_RESULT_PATH,
};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeConfig {
    pub(crate) task: TrialRuntimeTaskConfig,
    pub(crate) agent: TrialRuntimeAgentConfig,
    pub(crate) execution: TrialRuntimeExecutionConfig,
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
    pub(crate) mount: Option<TrialRuntimeAgentMountConfig>,
    #[serde(default)]
    pub(crate) sidecars: Vec<String>,
    #[serde(default)]
    pub(crate) integration_level: Option<String>,
    #[serde(default)]
    pub(crate) env: Value,
    #[serde(default)]
    pub(crate) events: Value,
    #[serde(default)]
    pub(crate) telemetry: Value,
    #[serde(default)]
    pub(crate) output_mounts: Value,
    #[serde(default)]
    pub(crate) outputs: BTreeMap<String, RuntimeOutputConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeAgentMountConfig {
    pub(crate) source: String,
    pub(crate) mount: TrialRuntimeAgentMountTargetConfig,
    #[serde(default)]
    pub(crate) digest: Option<String>,
    #[serde(default)]
    pub(crate) resolved_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeAgentMountTargetConfig {
    pub(crate) path: String,
    pub(crate) read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeExecutionConfig {
    pub(crate) agent_site: AgentSite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialRuntimeGraderConfig {
    pub(crate) strategy: GradingStrategy,
    #[serde(default)]
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) injected: Value,
    #[serde(default)]
    pub(crate) separate: Value,
    #[serde(default)]
    pub(crate) host: Value,
    #[serde(default)]
    pub(crate) sidecars: Vec<String>,
    #[serde(default)]
    pub(crate) inputs: BTreeMap<String, RuntimeInputConfig>,
    #[serde(default)]
    pub(crate) outputs: BTreeMap<String, RuntimeOutputConfig>,
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

    validate_runtime_outputs("trial_runtime.agent.outputs", &config.agent.outputs)?;
    validate_agent_mount_config(config.agent.mount.as_ref())?;
    validate_transport_graph(experiment, config)?;
    validate_grader_metrics(experiment, config)?;
    Ok(())
}

fn validate_agent_mount_config(artifact: Option<&TrialRuntimeAgentMountConfig>) -> Result<()> {
    let Some(artifact) = artifact else {
        return Ok(());
    };
    if artifact.source.trim().is_empty() {
        return Err(anyhow!("trial_runtime.agent.mount.source is required"));
    }
    if artifact
        .digest
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(anyhow!(
            "trial_runtime.agent.mount.digest must not be empty"
        ));
    }
    if artifact
        .resolved_path
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(anyhow!(
            "trial_runtime.agent.mount.resolved_path must not be empty"
        ));
    }
    validate_absolute_container_path(&artifact.mount.path, "trial_runtime.agent.mount.mount.path")?;
    for reserved in ["/agentlab/in", "/agentlab/out"] {
        if artifact.mount.path == reserved
            || artifact.mount.path.starts_with(&format!("{}/", reserved))
        {
            return Err(anyhow!(
                "trial_runtime.agent.mount.mount.path targets reserved runner path '{}'",
                reserved
            ));
        }
    }
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
    if runtime.grader.strategy != GradingStrategy::None {
        validate_grader_strategy_config(&runtime.grader)?;
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
        if metric.source.source_type == "grader_output"
            && runtime.grader.strategy == GradingStrategy::None
        {
            return Err(anyhow!(
                "metric '{}' uses source.type={} but grader.strategy=none",
                metric.id,
                metric.source.source_type
            ));
        }
        if metric.source.source_type == "grader_result" {
            return Err(anyhow!(
                "metric '{}' uses removed source.type=grader_result; use source.type=grader_output",
                metric.id
            ));
        }
    }
    let _metrics: Vec<MetricDefinition> = metrics;
    Ok(())
}

fn non_empty_child_string<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
}

fn validate_grader_strategy_config(grader: &TrialRuntimeGraderConfig) -> Result<()> {
    if grader.command.is_empty() || grader.command.iter().any(|part| part.trim().is_empty()) {
        return Err(anyhow!(
            "grader.strategy={} requires /trial_runtime/grader/command to be a non-empty argv array",
            grader_strategy_name(&grader.strategy)
        ));
    }
    match &grader.strategy {
        GradingStrategy::None => Ok(()),
        GradingStrategy::InTaskRuntime => Ok(()),
        GradingStrategy::Injected => {
            if non_empty_child_string(&grader.injected, "/bundle").is_none()
                || non_empty_child_string(&grader.injected, "/copy_dest").is_none()
            {
                return Err(anyhow!(
                    "grader.strategy=injected requires /trial_runtime/grader/injected.bundle and copy_dest"
                ));
            }
            Ok(())
        }
        GradingStrategy::Separate => {
            if non_empty_child_string(&grader.separate, "/image").is_none()
                || non_empty_child_string(&grader.separate, "/workdir").is_none()
            {
                return Err(anyhow!(
                    "grader.strategy=separate requires /trial_runtime/grader/separate.image and workdir"
                ));
            }
            Ok(())
        }
        GradingStrategy::Host => {
            if non_empty_child_string(&grader.host, "/capability").is_none() {
                return Err(anyhow!(
                    "grader.strategy=host requires /trial_runtime/grader/host.capability"
                ));
            }
            Ok(())
        }
    }
}

fn grader_strategy_name(strategy: &GradingStrategy) -> &'static str {
    match strategy {
        GradingStrategy::None => "none",
        GradingStrategy::InTaskRuntime => "in_task_runtime",
        GradingStrategy::Injected => "injected",
        GradingStrategy::Separate => "separate",
        GradingStrategy::Host => "host",
    }
}

fn validate_runtime_outputs(
    context: &str,
    outputs: &BTreeMap<String, RuntimeOutputConfig>,
) -> Result<()> {
    if outputs.is_empty() {
        return Err(anyhow!(
            "{} must declare at least one named output",
            context
        ));
    }
    for (id, output) in outputs {
        validate_transport_id(id, &format!("{}.{}", context, id))?;
        let capture = &output.capture;
        if capture.capture_type.trim().is_empty() {
            return Err(anyhow!("{}.{}.capture.type is required", context, id));
        }
        match capture.capture_type.as_str() {
            "file" => {
                let path = capture
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow!("{}.{}.capture.path is required", context, id))?;
                validate_absolute_container_path(
                    path,
                    &format!("{}.{}.capture.path", context, id),
                )?;
                if !matches!(capture.format.as_deref(), Some("json" | "text" | "bytes")) {
                    return Err(anyhow!(
                        "{}.{}.capture.format must be json, text, or bytes",
                        context,
                        id
                    ));
                }
            }
            "result_json" => {
                let path = capture
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow!("{}.{}.capture.path is required", context, id))?;
                validate_absolute_container_path(
                    path,
                    &format!("{}.{}.capture.path", context, id),
                )?;
            }
            "workspace_diff" => {
                if capture.format.as_deref() != Some("unified_diff") {
                    return Err(anyhow!(
                        "{}.{}.capture.format must be unified_diff for workspace_diff",
                        context,
                        id
                    ));
                }
            }
            other => {
                return Err(anyhow!(
                    "{}.{}.capture.type '{}' is not supported",
                    context,
                    id,
                    other
                ))
            }
        }
    }
    Ok(())
}

fn validate_transport_graph(experiment: &Value, config: &TrialRuntimeConfig) -> Result<()> {
    if !config.agent.outputs.contains_key("result") {
        return Err(anyhow!(
            "trial_runtime.agent.outputs.result is required as the canonical agent result output"
        ));
    }
    let result = &config.agent.outputs["result"].capture;
    if result.capture_type != "file"
        || result.path.as_deref() != Some(DEFAULT_CONTAINER_RESULT_PATH)
        || result.format.as_deref() != Some("json")
    {
        return Err(anyhow!(
            "trial_runtime.agent.outputs.result must capture the canonical JSON result file at {}",
            DEFAULT_CONTAINER_RESULT_PATH
        ));
    }
    let agent_output_ids = config
        .agent
        .outputs
        .keys()
        .map(|id| format!("agent.{}", id))
        .collect::<BTreeSet<_>>();
    for (id, input) in &config.grader.inputs {
        validate_transport_id(id, &format!("trial_runtime.grader.inputs.{}", id))?;
        validate_source(
            &input.source,
            &agent_output_ids,
            &format!("trial_runtime.grader.inputs.{}.source", id),
        )?;
        validate_materialize(
            &input.materialize.as_kind,
            input.materialize.path.as_deref(),
            input.materialize.name.as_deref(),
            &format!("trial_runtime.grader.inputs.{}.materialize", id),
        )?;
    }
    if config.grader.strategy != GradingStrategy::None {
        validate_runtime_outputs("trial_runtime.grader.outputs", &config.grader.outputs)?;
    } else if !config.grader.inputs.is_empty() || !config.grader.outputs.is_empty() {
        return Err(anyhow!(
            "grader.strategy=none must not declare grader inputs or outputs"
        ));
    }
    let grader_output_ids = config
        .grader
        .outputs
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    for metric in crate::config::parse_metric_definitions(experiment)? {
        let Some(output) = metric
            .definition_json
            .pointer("/source/output")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        match metric.source.source_type.as_str() {
            "grader_output" => {
                if !grader_output_ids.contains(output) {
                    return Err(anyhow!(
                        "metrics.{} references unknown grader output '{}'",
                        metric.id,
                        output
                    ));
                }
            }
            "runtime_output" => {
                let output_id = output.strip_prefix("agent.").unwrap_or(output);
                if !config.agent.outputs.contains_key(output_id) {
                    return Err(anyhow!(
                        "metrics.{} references unknown runtime output '{}'",
                        metric.id,
                        output
                    ));
                }
                if output_id != "result" {
                    return Err(anyhow!(
                        "metrics.{} references runtime output '{}', but only agent.result is currently persisted into metric extraction without a grader",
                        metric.id,
                        output
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_source(
    source: &RuntimeTransportSourceConfig,
    agent_output_ids: &BTreeSet<String>,
    context: &str,
) -> Result<()> {
    let mut variants = 0;
    if let Some(output) = source.output.as_deref() {
        variants += 1;
        if !agent_output_ids.contains(output) {
            return Err(anyhow!(
                "{} references unknown output '{}'",
                context,
                output
            ));
        }
    }
    if source.task.is_some() {
        variants += 1;
    }
    if let Some(object) = source.object.as_ref() {
        variants += 1;
        for (key, nested) in object {
            validate_transport_id(key, &format!("{}.object.{}", context, key))?;
            validate_source(
                nested,
                agent_output_ids,
                &format!("{}.object.{}", context, key),
            )?;
        }
    }
    if variants != 1 {
        return Err(anyhow!(
            "{} must declare exactly one of output, task, or object",
            context
        ));
    }
    Ok(())
}

fn validate_materialize(
    as_kind: &str,
    path: Option<&str>,
    name: Option<&str>,
    context: &str,
) -> Result<()> {
    match as_kind {
        "file" | "json_file" => {
            let path = path
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("{}.path is required", context))?;
            validate_absolute_container_path(path, &format!("{}.path", context))?;
        }
        "env" => {
            let name = name
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("{}.name is required", context))?;
            if !name
                .chars()
                .all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
            {
                return Err(anyhow!(
                    "{}.name must be an uppercase env var name",
                    context
                ));
            }
        }
        "stdin" => {
            return Err(anyhow!(
                "{}.as=stdin is reserved for command stdin transport and is not executable yet",
                context
            ))
        }
        "json_body" | "multipart_field" => {
            return Err(anyhow!(
                "{}.as={} is reserved for non-command grader runtimes and is not executable yet",
                context,
                as_kind
            ))
        }
        other => return Err(anyhow!("{}.as '{}' is not supported", context, other)),
    }
    Ok(())
}

fn validate_transport_id(id: &str, context: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch == '_' || ch == '-' || ch.is_ascii_alphanumeric())
    {
        return Err(anyhow!("{} must be a non-empty transport id", context));
    }
    Ok(())
}

fn validate_absolute_container_path(path: &str, context: &str) -> Result<()> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(anyhow!("{} must be an absolute runtime path", context));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(anyhow!("{} must not contain '..'", context));
    }
    Ok(())
}
