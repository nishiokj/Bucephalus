use anyhow::{anyhow, Result};
use lab_core::BUCEPHALUS_CONTRACT_WORKSPACE_DIR;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

use crate::model::{
    WorkspaceAuxMountSpec, WorkspaceBaseKind, WorkspaceBaseSpec, WorkspaceMode,
    WorkspaceOverlaySpec, WorkspaceSpec,
};

fn empty_object_value() -> Value {
    json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskMaterializationKind {
    TaskImage,
    BaseImageBundle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskMaterializationSpec {
    pub(crate) kind: TaskMaterializationKind,
    #[serde(default)]
    pub(crate) task_bundle_ref: Option<String>,
    #[serde(default)]
    pub(crate) platform: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskRowContainerImage {
    pub(crate) image: String,
    pub(crate) workdir: String,
    #[serde(default)]
    pub(crate) platform: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskRowRuntime {
    #[serde(default)]
    pub(crate) container_image: Option<TaskRowContainerImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskRowV2 {
    pub(crate) schema_version: String,
    pub(crate) id: String,
    pub(crate) task: Value,
    #[serde(default)]
    pub(crate) runtime: TaskRowRuntime,
    #[serde(default)]
    pub(crate) time_limit_ms: Option<u64>,
}

pub(crate) type TaskRow = TaskRowV2;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskCaseLimits {
    #[serde(default)]
    pub(crate) timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskCaseV1 {
    pub(crate) schema_version: String,
    pub(crate) id: String,
    #[serde(default = "empty_object_value")]
    pub(crate) inputs: Value,
    #[serde(default = "empty_object_value")]
    pub(crate) resources: Value,
    #[serde(default = "empty_object_value")]
    pub(crate) metadata: Value,
    #[serde(default)]
    pub(crate) limits: TaskCaseLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaseWorkspaceSource {
    ContainerImage,
    Empty,
    DatasetPack,
    GitCheckout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaseMaterializationStage {
    Case,
    Agent,
    Grader,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaseMaterializationOperation {
    Command,
    Copy,
    Mount,
    ExtractArchive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CaseMaterializationMountPlan {
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CaseMaterializationStepPlan {
    pub(crate) id: String,
    pub(crate) stage: CaseMaterializationStage,
    pub(crate) operation: CaseMaterializationOperation,
    #[serde(default)]
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) resource: Option<String>,
    #[serde(default = "empty_object_value")]
    pub(crate) source: Value,
    #[serde(default)]
    pub(crate) mount: Option<CaseMaterializationMountPlan>,
    #[serde(default)]
    pub(crate) workdir: Option<String>,
    #[serde(default)]
    pub(crate) network: Option<String>,
    #[serde(default)]
    pub(crate) timeout_ms: Option<u64>,
    #[serde(default)]
    pub(crate) hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct TaskCaseV2Resources {
    #[serde(default)]
    pub(crate) environment: Option<TaskCaseV2Environment>,
    #[serde(default)]
    pub(crate) workspace: Option<TaskCaseV2Workspace>,
    #[serde(default)]
    pub(crate) assets: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskCaseV2Environment {
    #[serde(default)]
    pub(crate) image: Option<String>,
    #[serde(default)]
    pub(crate) workdir: Option<String>,
    #[serde(default)]
    pub(crate) platform: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskCaseV2Workspace {
    pub(crate) source: CaseWorkspaceSource,
    #[serde(default)]
    pub(crate) mode: Option<WorkspaceMode>,
    #[serde(default)]
    pub(crate) image: Option<String>,
    #[serde(default)]
    pub(crate) workdir: Option<String>,
    #[serde(default)]
    pub(crate) platform: Option<String>,
    #[serde(default)]
    pub(crate) dataset_pack_ref: Option<String>,
    #[serde(default)]
    pub(crate) repo: Option<String>,
    #[serde(default)]
    pub(crate) commit: Option<String>,
    #[serde(default)]
    pub(crate) overlays: Vec<WorkspaceOverlaySpec>,
    #[serde(default)]
    pub(crate) aux_mounts: Vec<WorkspaceAuxMountSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskCaseV2 {
    pub(crate) schema_version: String,
    pub(crate) id: String,
    #[serde(default = "empty_object_value")]
    pub(crate) inputs: Value,
    #[serde(default)]
    pub(crate) resources: TaskCaseV2Resources,
    #[serde(default)]
    pub(crate) materialization: Vec<CaseMaterializationStepPlan>,
    #[serde(default = "empty_object_value")]
    pub(crate) metadata: Value,
    #[serde(default)]
    pub(crate) limits: TaskCaseLimits,
}

impl TaskRowV2 {
    pub(crate) fn task_id(&self, task_idx: usize) -> String {
        let trimmed = self.id.trim();
        if trimmed.is_empty() {
            format!("task_{}", task_idx)
        } else {
            trimmed.to_string()
        }
    }

    pub(crate) fn container_image(&self) -> Option<&TaskRowContainerImage> {
        self.runtime.container_image.as_ref()
    }
}

impl TaskCaseV1 {
    pub(crate) fn case_id(&self, task_idx: usize) -> String {
        let trimmed = self.id.trim();
        if trimmed.is_empty() {
            format!("case_{}", task_idx)
        } else {
            trimmed.to_string()
        }
    }
}

impl TaskCaseV2 {
    pub(crate) fn case_id(&self, task_idx: usize) -> String {
        let trimmed = self.id.trim();
        if trimmed.is_empty() {
            format!("case_{}", task_idx)
        } else {
            trimmed.to_string()
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskBoundaryMaterialization {
    pub(crate) declaration: Value,
    pub(crate) task_payload: Value,
    pub(crate) workspace: WorkspaceSpec,
    pub(crate) dependencies: Value,
    pub(crate) materialization: TaskMaterializationSpec,
    pub(crate) case_materialization: Vec<CaseMaterializationStepPlan>,
    pub(crate) task_id: String,
    pub(crate) task_image: String,
    pub(crate) task_workdir: String,
    pub(crate) time_limit_ms: Option<u64>,
}

pub(crate) fn parse_task_row(task: &Value) -> Result<TaskRow> {
    let obj = task
        .as_object()
        .ok_or_else(|| anyhow!("task row must be an object"))?;
    match obj.get("schema_version").and_then(Value::as_str) {
        Some("task_row_v2") => {}
        Some("task_row_v1") => {
            return Err(anyhow!(
                "task row schema_version 'task_row_v1' is not supported; use task_row_v2"
            ))
        }
        Some(other) => {
            return Err(anyhow!(
                "task row schema_version '{}' is not supported; expected 'task_row_v2'",
                other
            ))
        }
        None => {
            return Err(anyhow!(
                "task row missing schema_version; expected 'task_row_v2'"
            ))
        }
    }
    let task_row: TaskRow =
        serde_json::from_value(task.clone()).map_err(|err| anyhow!("invalid task row: {}", err))?;
    validate_task_row(&task_row)?;
    Ok(task_row)
}

pub(crate) fn parse_task_case(task: &Value) -> Result<TaskCaseV1> {
    let obj = task
        .as_object()
        .ok_or_else(|| anyhow!("case row must be an object"))?;
    match obj.get("schema_version").and_then(Value::as_str) {
        Some("case_v1") | Some("task_case_v1") => {}
        Some(other) => {
            return Err(anyhow!(
                "case row schema_version '{}' is not supported; expected 'case_v1'",
                other
            ))
        }
        None => {
            return Err(anyhow!(
                "case row missing schema_version; expected 'case_v1'"
            ))
        }
    }
    let task_case: TaskCaseV1 =
        serde_json::from_value(task.clone()).map_err(|err| anyhow!("invalid case row: {}", err))?;
    validate_task_case(&task_case)?;
    Ok(task_case)
}

pub(crate) fn parse_task_case_v2(task: &Value) -> Result<TaskCaseV2> {
    let obj = task
        .as_object()
        .ok_or_else(|| anyhow!("case row must be an object"))?;
    match obj.get("schema_version").and_then(Value::as_str) {
        Some("case_v2") => {}
        Some(other) => {
            return Err(anyhow!(
                "case row schema_version '{}' is not supported; expected 'case_v2'",
                other
            ))
        }
        None => {
            return Err(anyhow!(
                "case row missing schema_version; expected 'case_v2'"
            ))
        }
    }
    let task_case: TaskCaseV2 =
        serde_json::from_value(task.clone()).map_err(|err| anyhow!("invalid case row: {}", err))?;
    validate_task_case_v2(&task_case)?;
    Ok(task_case)
}

pub(crate) fn materialize_task_row(task_row: TaskRow) -> TaskBoundaryMaterialization {
    let container = task_row.container_image();
    let materialization = TaskMaterializationSpec {
        kind: TaskMaterializationKind::TaskImage,
        task_bundle_ref: None,
        platform: container.and_then(|value| value.platform.clone()),
    };
    TaskBoundaryMaterialization {
        declaration: serde_json::to_value(&task_row).unwrap_or_else(|_| json!({})),
        task_payload: task_row.task.clone(),
        workspace: WorkspaceSpec {
            mode: WorkspaceMode::Scratch,
            base: WorkspaceBaseSpec {
                kind: WorkspaceBaseKind::Empty,
                dataset_pack_ref: None,
                repo: None,
                commit: None,
            },
            overlays: Vec::new(),
            aux_mounts: Vec::new(),
        },
        dependencies: json!({}),
        materialization,
        case_materialization: Vec::new(),
        task_id: task_row.task_id(0),
        task_image: container
            .map(|value| value.image.clone())
            .unwrap_or_default(),
        task_workdir: container
            .map(|value| value.workdir.clone())
            .unwrap_or_default(),
        time_limit_ms: task_row.time_limit_ms,
    }
}

fn case_container_image(case: &TaskCaseV1) -> Result<Option<TaskRowContainerImage>> {
    let Some(workspace) = case.resources.pointer("/workspace") else {
        return Ok(None);
    };
    if workspace
        .get("type")
        .or_else(|| workspace.get("kind"))
        .and_then(Value::as_str)
        != Some("container_image")
    {
        return Ok(None);
    }
    let image = workspace
        .get("image")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("case resources.workspace.image is required for container_image"))?
        .to_string();
    let workdir = workspace
        .get("workdir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("case resources.workspace.workdir is required for container_image"))?
        .to_string();
    let platform = workspace
        .get("platform")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    Ok(Some(TaskRowContainerImage {
        image,
        workdir,
        platform,
    }))
}

pub(crate) fn materialize_task_case(task_case: TaskCaseV1) -> Result<TaskBoundaryMaterialization> {
    let container = case_container_image(&task_case)?;
    let task_id = task_case.case_id(0);
    let materialization = TaskMaterializationSpec {
        kind: TaskMaterializationKind::TaskImage,
        task_bundle_ref: None,
        platform: container.as_ref().and_then(|value| value.platform.clone()),
    };
    let task_payload = json!({
        "id": task_case.id.clone(),
        "inputs": task_case.inputs.clone(),
        "metadata": task_case.metadata.clone(),
    });
    Ok(TaskBoundaryMaterialization {
        declaration: serde_json::to_value(&task_case).unwrap_or_else(|_| json!({})),
        task_payload,
        workspace: WorkspaceSpec {
            mode: WorkspaceMode::Scratch,
            base: WorkspaceBaseSpec {
                kind: WorkspaceBaseKind::Empty,
                dataset_pack_ref: None,
                repo: None,
                commit: None,
            },
            overlays: Vec::new(),
            aux_mounts: Vec::new(),
        },
        dependencies: json!({}),
        materialization,
        case_materialization: Vec::new(),
        task_id,
        task_image: container
            .as_ref()
            .map(|value| value.image.clone())
            .unwrap_or_default(),
        task_workdir: container
            .as_ref()
            .map(|value| value.workdir.clone())
            .unwrap_or_default(),
        time_limit_ms: task_case.limits.timeout_ms,
    })
}

fn non_empty_string(value: Option<&String>, context: &str) -> Result<Option<String>> {
    match value.map(|raw| raw.trim()).filter(|raw| !raw.is_empty()) {
        Some(trimmed) => Ok(Some(trimmed.to_string())),
        None if value.is_some() => Err(anyhow!("{} must be non-empty when provided", context)),
        None => Ok(None),
    }
}

fn require_string(value: Option<String>, context: &str) -> Result<String> {
    value.ok_or_else(|| anyhow!("{} is required", context))
}

fn workspace_mode_or_default(workspace: Option<&TaskCaseV2Workspace>) -> WorkspaceMode {
    workspace
        .and_then(|workspace| workspace.mode.clone())
        .unwrap_or(WorkspaceMode::Scratch)
}

fn lower_case_v2_workspace(workspace: Option<&TaskCaseV2Workspace>) -> Result<WorkspaceSpec> {
    let Some(workspace) = workspace else {
        return Ok(WorkspaceSpec {
            mode: WorkspaceMode::Scratch,
            base: WorkspaceBaseSpec {
                kind: WorkspaceBaseKind::Empty,
                dataset_pack_ref: None,
                repo: None,
                commit: None,
            },
            overlays: Vec::new(),
            aux_mounts: Vec::new(),
        });
    };
    let base = match workspace.source {
        CaseWorkspaceSource::ContainerImage | CaseWorkspaceSource::Empty => WorkspaceBaseSpec {
            kind: WorkspaceBaseKind::Empty,
            dataset_pack_ref: None,
            repo: None,
            commit: None,
        },
        CaseWorkspaceSource::DatasetPack => WorkspaceBaseSpec {
            kind: WorkspaceBaseKind::DatasetPack,
            dataset_pack_ref: Some(require_string(
                non_empty_string(
                    workspace.dataset_pack_ref.as_ref(),
                    "case resources.workspace.dataset_pack_ref",
                )?,
                "case resources.workspace.dataset_pack_ref",
            )?),
            repo: None,
            commit: None,
        },
        CaseWorkspaceSource::GitCheckout => WorkspaceBaseSpec {
            kind: WorkspaceBaseKind::GitCheckout,
            dataset_pack_ref: None,
            repo: Some(require_string(
                non_empty_string(workspace.repo.as_ref(), "case resources.workspace.repo")?,
                "case resources.workspace.repo",
            )?),
            commit: Some(require_string(
                non_empty_string(workspace.commit.as_ref(), "case resources.workspace.commit")?,
                "case resources.workspace.commit",
            )?),
        },
    };
    Ok(WorkspaceSpec {
        mode: workspace_mode_or_default(Some(workspace)),
        base,
        overlays: workspace.overlays.clone(),
        aux_mounts: workspace.aux_mounts.clone(),
    })
}

fn case_v2_container_image(case: &TaskCaseV2) -> Result<Option<TaskRowContainerImage>> {
    let Some(workspace) = case.resources.workspace.as_ref() else {
        return Ok(None);
    };
    if workspace.source != CaseWorkspaceSource::ContainerImage {
        if case.resources.environment.is_some() {
            return Err(anyhow!(
                "case_v2 resources.environment is currently executable only with resources.workspace.source=container_image"
            ));
        }
        return Ok(None);
    }
    let env = case.resources.environment.as_ref();
    let image = require_string(
        non_empty_string(
            workspace
                .image
                .as_ref()
                .or_else(|| env.and_then(|env| env.image.as_ref())),
            "case resources.workspace.image",
        )?,
        "case resources.workspace.image",
    )?;
    let workdir = require_string(
        non_empty_string(
            workspace
                .workdir
                .as_ref()
                .or_else(|| env.and_then(|env| env.workdir.as_ref())),
            "case resources.workspace.workdir",
        )?,
        "case resources.workspace.workdir",
    )?;
    let platform = non_empty_string(
        workspace
            .platform
            .as_ref()
            .or_else(|| env.and_then(|env| env.platform.as_ref())),
        "case resources.workspace.platform",
    )?;
    Ok(Some(TaskRowContainerImage {
        image,
        workdir,
        platform,
    }))
}

pub(crate) fn materialize_task_case_v2(
    task_case: TaskCaseV2,
) -> Result<TaskBoundaryMaterialization> {
    let container = case_v2_container_image(&task_case)?;
    let task_id = task_case.case_id(0);
    let materialization = TaskMaterializationSpec {
        kind: TaskMaterializationKind::TaskImage,
        task_bundle_ref: None,
        platform: container.as_ref().and_then(|value| value.platform.clone()),
    };
    let task_payload = json!({
        "id": task_case.id.clone(),
        "inputs": task_case.inputs.clone(),
        "metadata": task_case.metadata.clone(),
    });
    Ok(TaskBoundaryMaterialization {
        declaration: serde_json::to_value(&task_case).unwrap_or_else(|_| json!({})),
        task_payload,
        workspace: lower_case_v2_workspace(task_case.resources.workspace.as_ref())?,
        dependencies: json!({}),
        materialization,
        case_materialization: task_case.materialization.clone(),
        task_id,
        task_image: container
            .as_ref()
            .map(|value| value.image.clone())
            .unwrap_or_default(),
        task_workdir: container
            .as_ref()
            .map(|value| value.workdir.clone())
            .unwrap_or_default(),
        time_limit_ms: task_case.limits.timeout_ms,
    })
}

pub(crate) fn materialize_packaged_task_boundary(
    task: &Value,
) -> Result<TaskBoundaryMaterialization> {
    match task.get("schema_version").and_then(Value::as_str) {
        Some("task_row_v2") => Ok(materialize_task_row(parse_task_row(task)?)),
        Some("case_v1") | Some("task_case_v1") => materialize_task_case(parse_task_case(task)?),
        Some("case_v2") => materialize_task_case_v2(parse_task_case_v2(task)?),
        Some("task_row_v1") => Err(anyhow!(
            "packaged task schema_version 'task_row_v1' is not supported at runtime; expected 'case_v2' or 'case_v1'"
        )),
        Some(other) => Err(anyhow!(
            "packaged task schema_version '{}' is not supported at runtime; expected 'case_v2' or 'case_v1'",
            other
        )),
        None => Err(anyhow!(
            "packaged task missing schema_version; expected 'case_v2' or 'case_v1'"
        )),
    }
}

pub(crate) fn parse_task_boundary_from_packaged_task(
    task: &Value,
) -> Result<TaskBoundaryMaterialization> {
    materialize_packaged_task_boundary(task)
}

pub(crate) fn validate_task_row(task_row: &TaskRow) -> Result<()> {
    if task_row.id.trim().is_empty() {
        return Err(anyhow!("task row field 'id' must be a non-empty string"));
    }
    if !task_row.task.is_object() {
        return Err(anyhow!("task row field 'task' must be an object"));
    }
    if task_row.time_limit_ms == Some(0) {
        return Err(anyhow!(
            "task row field 'time_limit_ms' must be > 0 when provided"
        ));
    }
    if let Some(container) = task_row.runtime.container_image.as_ref() {
        if container.image.trim().is_empty() {
            return Err(anyhow!(
                "task row runtime.container_image.image must be a non-empty string when provided"
            ));
        }
        if container.workdir.trim().is_empty() {
            return Err(anyhow!(
                "task row runtime.container_image.workdir must be a non-empty string when provided"
            ));
        }
        validate_task_container_workdir(container.workdir.trim())?;
        if container
            .platform
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(anyhow!(
                "task row runtime.container_image.platform must be non-empty when provided"
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_task_case(task_case: &TaskCaseV1) -> Result<()> {
    if task_case.id.trim().is_empty() {
        return Err(anyhow!("case field 'id' must be a non-empty string"));
    }
    if !task_case.inputs.is_object() {
        return Err(anyhow!("case field 'inputs' must be an object"));
    }
    if !task_case.resources.is_object() {
        return Err(anyhow!("case field 'resources' must be an object"));
    }
    if !task_case.metadata.is_object() {
        return Err(anyhow!("case field 'metadata' must be an object"));
    }
    if task_case.limits.timeout_ms == Some(0) {
        return Err(anyhow!("case limits.timeout_ms must be > 0 when provided"));
    }
    if let Some(container) = case_container_image(task_case)? {
        validate_task_container_workdir(container.workdir.trim()).map_err(|err| {
            anyhow!(
                "{} (case resources.workspace.workdir)",
                err.to_string()
                    .replace("task row runtime.container_image.workdir", "workdir")
            )
        })?;
    }
    Ok(())
}

pub(crate) fn validate_task_case_v2(task_case: &TaskCaseV2) -> Result<()> {
    if task_case.id.trim().is_empty() {
        return Err(anyhow!("case field 'id' must be a non-empty string"));
    }
    if !task_case.inputs.is_object() {
        return Err(anyhow!("case field 'inputs' must be an object"));
    }
    if !task_case.metadata.is_object() {
        return Err(anyhow!("case field 'metadata' must be an object"));
    }
    if !task_case.resources.assets.is_null() && !task_case.resources.assets.is_object() {
        return Err(anyhow!(
            "case resources.assets must be an object when provided"
        ));
    }
    if task_case.limits.timeout_ms == Some(0) {
        return Err(anyhow!("case limits.timeout_ms must be > 0 when provided"));
    }
    if let Some(workspace) = task_case.resources.workspace.as_ref() {
        for (value, context) in [
            (&workspace.image, "case resources.workspace.image"),
            (&workspace.workdir, "case resources.workspace.workdir"),
            (&workspace.platform, "case resources.workspace.platform"),
            (
                &workspace.dataset_pack_ref,
                "case resources.workspace.dataset_pack_ref",
            ),
            (&workspace.repo, "case resources.workspace.repo"),
            (&workspace.commit, "case resources.workspace.commit"),
        ] {
            non_empty_string(value.as_ref(), context)?;
        }
        if let Some(workdir) = workspace.workdir.as_deref() {
            validate_task_container_workdir(workdir.trim()).map_err(|err| {
                anyhow!(
                    "{} (case resources.workspace.workdir)",
                    err.to_string()
                        .replace("task row runtime.container_image.workdir", "workdir")
                )
            })?;
        }
        match workspace.source {
            CaseWorkspaceSource::ContainerImage => {
                let container = case_v2_container_image(task_case)?;
                if let Some(container) = container {
                    validate_task_container_workdir(container.workdir.trim()).map_err(|err| {
                        anyhow!(
                            "{} (case resources.workspace.workdir)",
                            err.to_string()
                                .replace("task row runtime.container_image.workdir", "workdir")
                        )
                    })?;
                }
            }
            CaseWorkspaceSource::DatasetPack => {
                require_string(
                    non_empty_string(
                        workspace.dataset_pack_ref.as_ref(),
                        "case resources.workspace.dataset_pack_ref",
                    )?,
                    "case resources.workspace.dataset_pack_ref",
                )?;
            }
            CaseWorkspaceSource::GitCheckout => {
                require_string(
                    non_empty_string(workspace.repo.as_ref(), "case resources.workspace.repo")?,
                    "case resources.workspace.repo",
                )?;
                require_string(
                    non_empty_string(workspace.commit.as_ref(), "case resources.workspace.commit")?,
                    "case resources.workspace.commit",
                )?;
            }
            CaseWorkspaceSource::Empty => {}
        }
    }
    validate_case_materialization_steps(task_case)?;
    Ok(())
}

fn validate_case_materialization_steps(task_case: &TaskCaseV2) -> Result<()> {
    if task_case.materialization.is_empty() {
        return Ok(());
    }
    let workspace = task_case
        .resources
        .workspace
        .as_ref()
        .ok_or_else(|| anyhow!("case_v2 materialization requires resources.workspace"))?;
    if workspace.source != CaseWorkspaceSource::ContainerImage {
        return Err(anyhow!(
            "case_v2 materialization is currently executable only with resources.workspace.source=container_image"
        ));
    }
    for (idx, step) in task_case.materialization.iter().enumerate() {
        let context = format!("case materialization[{}]", idx);
        if step.id.trim().is_empty() {
            return Err(anyhow!("{}.id must be a non-empty string", context));
        }
        if step.stage != CaseMaterializationStage::Case {
            return Err(anyhow!(
                "{}.stage={} is not executable yet; only stage=case is currently supported",
                context,
                case_materialization_stage_name(&step.stage)
            ));
        }
        if step.operation != CaseMaterializationOperation::Command {
            return Err(anyhow!(
                "{}.operation={} is not executable yet; only operation=command is currently supported",
                context,
                case_materialization_operation_name(&step.operation)
            ));
        }
        if step.command.is_empty() || step.command.iter().any(|part| part.trim().is_empty()) {
            return Err(anyhow!(
                "{}.command must be a non-empty argv array",
                context
            ));
        }
        if step.hidden {
            return Err(anyhow!(
                "{}.hidden=true is not supported for pre-agent command materialization",
                context
            ));
        }
        if let Some(network) = step.network.as_deref() {
            if network != "none" {
                return Err(anyhow!(
                    "{}.network={} is not executable yet; only network=none is currently supported",
                    context,
                    network
                ));
            }
        }
        if step.timeout_ms == Some(0) {
            return Err(anyhow!("{}.timeout_ms must be > 0 when provided", context));
        }
        if let Some(workdir) = step.workdir.as_deref() {
            validate_task_container_workdir(workdir.trim()).map_err(|err| {
                anyhow!(
                    "{} ({})",
                    err.to_string()
                        .replace("task row runtime.container_image.workdir", "workdir"),
                    format!("{}.workdir", context)
                )
            })?;
        }
    }
    Ok(())
}

fn case_materialization_stage_name(stage: &CaseMaterializationStage) -> &'static str {
    match stage {
        CaseMaterializationStage::Case => "case",
        CaseMaterializationStage::Agent => "agent",
        CaseMaterializationStage::Grader => "grader",
    }
}

fn case_materialization_operation_name(operation: &CaseMaterializationOperation) -> &'static str {
    match operation {
        CaseMaterializationOperation::Command => "command",
        CaseMaterializationOperation::Copy => "copy",
        CaseMaterializationOperation::Mount => "mount",
        CaseMaterializationOperation::ExtractArchive => "extract_archive",
    }
}

fn validate_task_container_workdir(path: &str) -> Result<()> {
    let workdir = Path::new(path);
    if !workdir.is_absolute() {
        return Err(anyhow!(
            "task row runtime.container_image.workdir must be an absolute path"
        ));
    }
    if workdir
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(anyhow!(
            "task row runtime.container_image.workdir must not contain '..'"
        ));
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
            "task row runtime.container_image.workdir must be under {}, /workspace/task, or /testbed",
            BUCEPHALUS_CONTRACT_WORKSPACE_DIR
        ));
    }
    Ok(())
}

pub(crate) fn validate_task_boundary_workspace_materialization(
    task_boundary: &TaskBoundaryMaterialization,
) -> Result<()> {
    if !task_boundary.dependencies.is_object() {
        return Err(anyhow!(
            "task '{}' dependencies must be a JSON object",
            task_boundary.task_id
        ));
    }
    if task_boundary.workspace.mode != WorkspaceMode::Patch {
        return Ok(());
    }
    if task_boundary.workspace.base.kind != WorkspaceBaseKind::Empty {
        return Ok(());
    }
    Err(anyhow!(
        "task '{}' uses workspace.mode='patch' but workspace.base.kind='empty'; patch tasks require a real base (dataset_pack or git_checkout)",
        task_boundary.task_id
    ))
}
