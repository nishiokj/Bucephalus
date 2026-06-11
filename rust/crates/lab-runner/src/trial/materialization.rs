use anyhow::{anyhow, Context, Result};
use lab_core::ensure_dir;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::trial::spec::{
    CaseMaterializationOperation, CaseMaterializationStage, CaseMaterializationStepPlan,
};
use crate::util::{
    copy_dir_preserve_contents, copy_file_if_exists, remove_path_if_exists, sanitize_for_fs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaseMaterializationPhase {
    AgentVisible,
    GraderVisible,
}

impl CaseMaterializationPhase {
    fn includes(self, step: &CaseMaterializationStepPlan) -> bool {
        match self {
            Self::AgentVisible => {
                !step.hidden
                    && matches!(
                        step.stage,
                        CaseMaterializationStage::Case | CaseMaterializationStage::Agent
                    )
            }
            Self::GraderVisible => step.hidden || step.stage == CaseMaterializationStage::Grader,
        }
    }
}

pub(crate) fn selected_case_materialization_steps<'a>(
    steps: &'a [CaseMaterializationStepPlan],
    phase: CaseMaterializationPhase,
) -> Vec<&'a CaseMaterializationStepPlan> {
    steps.iter().filter(|step| phase.includes(step)).collect()
}

pub(crate) struct HostCaseMaterializationRequest<'a> {
    pub(crate) manifest_dir: &'a Path,
    pub(crate) workspace_dir: &'a Path,
    pub(crate) log_dir: &'a Path,
    pub(crate) phase: CaseMaterializationPhase,
    pub(crate) default_timeout_ms: u64,
    pub(crate) env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct HostCaseMaterializationExecutor;

impl HostCaseMaterializationExecutor {
    pub(crate) fn execute(
        &self,
        request: HostCaseMaterializationRequest<'_>,
        steps: &[CaseMaterializationStepPlan],
    ) -> Result<()> {
        let selected = selected_case_materialization_steps(steps, request.phase);
        if selected.is_empty() {
            return Ok(());
        }
        ensure_dir(request.log_dir)?;
        for (idx, step) in selected.into_iter().enumerate() {
            self.execute_step(&request, idx, step)?;
        }
        Ok(())
    }

    fn execute_step(
        &self,
        request: &HostCaseMaterializationRequest<'_>,
        idx: usize,
        step: &CaseMaterializationStepPlan,
    ) -> Result<()> {
        validate_common_step(step)?;
        match step.operation {
            CaseMaterializationOperation::Command => self.execute_command_step(request, idx, step),
            CaseMaterializationOperation::Copy => self.execute_copy_step(request, step),
            CaseMaterializationOperation::ExtractArchive => {
                self.execute_extract_archive_step(request, step)
            }
            CaseMaterializationOperation::Mount => self.execute_mount_step(request, step),
        }
    }

    fn execute_command_step(
        &self,
        request: &HostCaseMaterializationRequest<'_>,
        idx: usize,
        step: &CaseMaterializationStepPlan,
    ) -> Result<()> {
        if step.command.is_empty() || step.command.iter().any(|part| part.trim().is_empty()) {
            return Err(anyhow!(
                "case materialization step '{}' command must be a non-empty argv array",
                step.id
            ));
        }
        let label = step_log_label(idx, step);
        let stdout = File::create(request.log_dir.join(format!("{}_stdout.log", label)))?;
        let stderr = File::create(request.log_dir.join(format!("{}_stderr.log", label)))?;
        let cwd = step
            .workdir
            .as_deref()
            .map(|workdir| map_workspace_path(request.workspace_dir, workdir))
            .transpose()?
            .unwrap_or_else(|| request.workspace_dir.to_path_buf());
        ensure_dir(&cwd)?;

        let argv = step
            .command
            .iter()
            .map(|part| map_command_arg(request.workspace_dir, part))
            .collect::<Vec<_>>();
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).current_dir(&cwd);
        command.envs(request.env.iter());
        command.env("BUCEPHALUS_CASE_MATERIALIZATION_ID", step.id.trim());
        command.env("BUCEPHALUS_WORKSPACE_DIR", request.workspace_dir);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to spawn host case materialization step '{}'",
                step.id
            )
        })?;
        let timeout_ms = step.timeout_ms.unwrap_or(request.default_timeout_ms).max(1);
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    return Err(anyhow!(
                        "case materialization step '{}' failed with exit code {:?}",
                        step.id,
                        status.code()
                    ));
                }
                return Ok(());
            }
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!(
                    "case materialization step '{}' timed out after {} ms",
                    step.id,
                    timeout_ms
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn execute_copy_step(
        &self,
        request: &HostCaseMaterializationRequest<'_>,
        step: &CaseMaterializationStepPlan,
    ) -> Result<()> {
        let source = resolve_step_source(request.manifest_dir, step)?;
        let target = step_target_path(request.workspace_dir, step)?;
        copy_source_to_target(&source, &target)
            .with_context(|| format!("copy materialization step '{}'", step.id))
    }

    fn execute_extract_archive_step(
        &self,
        request: &HostCaseMaterializationRequest<'_>,
        step: &CaseMaterializationStepPlan,
    ) -> Result<()> {
        let source = resolve_step_source(request.manifest_dir, step)?;
        let target = step
            .mount
            .as_ref()
            .map(|mount| map_workspace_path(request.workspace_dir, &mount.path))
            .transpose()?
            .unwrap_or_else(|| request.workspace_dir.to_path_buf());
        ensure_dir(&target)?;
        unpack_archive(&source, &target)
            .with_context(|| format!("extract_archive materialization step '{}'", step.id))
    }

    fn execute_mount_step(
        &self,
        request: &HostCaseMaterializationRequest<'_>,
        step: &CaseMaterializationStepPlan,
    ) -> Result<()> {
        let source = resolve_step_source(request.manifest_dir, step)?;
        let target = step_target_path(request.workspace_dir, step)?;
        if step.mount.as_ref().is_some_and(|mount| mount.read_only) {
            host_project_read_only_source(&source, &target)
        } else {
            copy_source_to_target(&source, &target)
        }
        .with_context(|| format!("mount materialization step '{}'", step.id))
    }
}

fn validate_common_step(step: &CaseMaterializationStepPlan) -> Result<()> {
    if step.id.trim().is_empty() {
        return Err(anyhow!("case materialization step id must not be empty"));
    }
    if let Some(network) = step.network.as_deref() {
        match network {
            "none" | "full" | "allowlist_enforced" => {}
            other => {
                return Err(anyhow!(
                    "case materialization step '{}' has unsupported network '{}'",
                    step.id,
                    other
                ))
            }
        }
    }
    Ok(())
}

fn resolve_step_source(manifest_dir: &Path, step: &CaseMaterializationStepPlan) -> Result<PathBuf> {
    let raw = step
        .source
        .pointer("/path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "case materialization step '{}' requires source.path",
                step.id
            )
        })?;
    let path = PathBuf::from(raw);
    Ok(if path.is_absolute() {
        path
    } else {
        manifest_dir.join(path)
    })
}

fn step_target_path(workspace_dir: &Path, step: &CaseMaterializationStepPlan) -> Result<PathBuf> {
    if let Some(mount) = step.mount.as_ref() {
        return map_workspace_path(workspace_dir, &mount.path);
    }
    Err(anyhow!(
        "case materialization step '{}' requires mount.path",
        step.id
    ))
}

fn copy_source_to_target(source: &Path, target: &Path) -> Result<()> {
    if source.is_dir() {
        copy_dir_preserve_contents(source, target)
    } else if source.is_file() {
        copy_file_if_exists(source, target)
    } else {
        Err(anyhow!(
            "materialization source must be a file or directory: {}",
            source.display()
        ))
    }
}

fn host_project_read_only_source(source: &Path, target: &Path) -> Result<()> {
    remove_path_if_exists(target)?;
    if let Some(parent) = target.parent() {
        ensure_dir(parent)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        copy_source_to_target(source, target)
    }
}

fn unpack_archive(source: &Path, target: &Path) -> Result<()> {
    let mut file = File::open(source).with_context(|| {
        format!(
            "failed to open materialization archive {}",
            source.display()
        )
    })?;
    let mut magic = [0_u8; 2];
    let read = file.read(&mut magic)?;
    drop(file);
    let is_gz = read == 2 && magic == [0x1f, 0x8b]
        || source
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext == "gz" || ext == "tgz");
    let file = File::open(source).with_context(|| {
        format!(
            "failed to open materialization archive {}",
            source.display()
        )
    })?;
    if is_gz {
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(target)?;
    } else {
        let mut archive = tar::Archive::new(file);
        archive.unpack(target)?;
    }
    Ok(())
}

fn map_command_arg(workspace_dir: &Path, arg: &str) -> String {
    if Path::new(arg.trim()).is_absolute() {
        map_workspace_path(workspace_dir, arg)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|_| arg.to_string())
    } else {
        arg.to_string()
    }
}

pub(crate) fn map_workspace_path(workspace_dir: &Path, raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("workspace path must not be empty"));
    }
    let path = Path::new(trimmed);
    let mapped = if path.is_absolute() {
        map_absolute_workspace_path(workspace_dir, trimmed)?
    } else {
        workspace_dir.join(path)
    };
    ensure_path_under_workspace(workspace_dir, &mapped)
}

fn map_absolute_workspace_path(workspace_dir: &Path, raw: &str) -> Result<PathBuf> {
    for root in [
        "/bucephalus/workspace",
        "/workspace/task",
        "/workspace",
        "/testbed",
    ] {
        if raw == root {
            return Ok(workspace_dir.to_path_buf());
        }
        if let Some(rest) = raw.strip_prefix(&format!("{}/", root)) {
            return Ok(workspace_dir.join(rest));
        }
    }
    Err(anyhow!(
        "host case materialization path '{}' is outside the declared workspace roots",
        raw
    ))
}

fn ensure_path_under_workspace(workspace_dir: &Path, path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(anyhow!("workspace path escapes workspace"));
                }
            }
        }
    }
    if normalized == workspace_dir || normalized.starts_with(workspace_dir) {
        Ok(normalized)
    } else {
        Err(anyhow!(
            "workspace path {} escapes workspace {}",
            path.display(),
            workspace_dir.display()
        ))
    }
}

fn step_log_label(idx: usize, step: &CaseMaterializationStepPlan) -> String {
    format!("{:03}_{}", idx, sanitize_for_fs(step.id.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trial::spec::{CaseMaterializationMountPlan, CaseMaterializationStepPlan};
    use serde_json::json;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{}_{}_{}",
            name,
            std::process::id(),
            chrono::Utc::now().timestamp_micros()
        ));
        ensure_dir(&path).expect("temp root");
        path
    }

    #[test]
    fn phase_selection_withholds_hidden_until_grader_phase() {
        let steps = vec![
            step("case", CaseMaterializationStage::Case, false),
            step("agent", CaseMaterializationStage::Agent, false),
            step("hidden", CaseMaterializationStage::Agent, true),
            step("grader", CaseMaterializationStage::Grader, false),
        ];
        let visible =
            selected_case_materialization_steps(&steps, CaseMaterializationPhase::AgentVisible)
                .into_iter()
                .map(|step| step.id.as_str())
                .collect::<Vec<_>>();
        assert_eq!(visible, vec!["case", "agent"]);
        let grader =
            selected_case_materialization_steps(&steps, CaseMaterializationPhase::GraderVisible)
                .into_iter()
                .map(|step| step.id.as_str())
                .collect::<Vec<_>>();
        assert_eq!(grader, vec!["hidden", "grader"]);
    }

    #[test]
    fn host_executor_runs_copy_and_command_steps() {
        let root = temp_root("bucephalus_host_case_materialization");
        let manifest_dir = root.join("manifest");
        let workspace = root.join("workspace");
        let logs = root.join("logs");
        ensure_dir(&manifest_dir).expect("manifest dir");
        ensure_dir(&workspace).expect("workspace dir");
        fs::write(manifest_dir.join("input.txt"), "hello").expect("source");
        let steps = vec![
            CaseMaterializationStepPlan {
                id: "copy".to_string(),
                stage: CaseMaterializationStage::Case,
                operation: CaseMaterializationOperation::Copy,
                command: Vec::new(),
                source: json!({"path": "input.txt"}),
                mount: Some(CaseMaterializationMountPlan {
                    path: "/workspace/task/copied.txt".to_string(),
                    read_only: false,
                }),
                workdir: None,
                network: Some("none".to_string()),
                timeout_ms: None,
                hidden: false,
            },
            CaseMaterializationStepPlan {
                id: "command".to_string(),
                stage: CaseMaterializationStage::Agent,
                operation: CaseMaterializationOperation::Command,
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "cat copied.txt > generated.txt".to_string(),
                ],
                source: json!({}),
                mount: None,
                workdir: Some("/workspace/task".to_string()),
                network: Some("none".to_string()),
                timeout_ms: Some(5_000),
                hidden: false,
            },
        ];
        HostCaseMaterializationExecutor
            .execute(
                HostCaseMaterializationRequest {
                    manifest_dir: &manifest_dir,
                    workspace_dir: &workspace,
                    log_dir: &logs,
                    phase: CaseMaterializationPhase::AgentVisible,
                    default_timeout_ms: 5_000,
                    env: BTreeMap::new(),
                },
                &steps,
            )
            .expect("host materialization");
        assert_eq!(
            fs::read_to_string(workspace.join("generated.txt")).expect("generated"),
            "hello"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn host_executor_rejects_case_materialization_source_aliases() {
        let root = temp_root("bucephalus_host_case_materialization_aliases");
        let manifest_dir = root.join("manifest");
        let workspace = root.join("workspace");
        let logs = root.join("logs");
        ensure_dir(&manifest_dir).expect("manifest dir");
        ensure_dir(&workspace).expect("workspace dir");
        fs::write(manifest_dir.join("input.txt"), "hello").expect("source");

        for (label, source, expected) in [
            ("ref", json!({"ref": "input.txt"}), "requires source.path"),
            ("uri", json!({"uri": "input.txt"}), "requires source.path"),
            (
                "destination",
                json!({"path": "input.txt", "destination": "/workspace/task/copied.txt"}),
                "requires mount.path",
            ),
            (
                "dest",
                json!({"path": "input.txt", "dest": "/workspace/task/copied.txt"}),
                "requires mount.path",
            ),
            (
                "target",
                json!({"path": "input.txt", "target": "/workspace/task/copied.txt"}),
                "requires mount.path",
            ),
        ] {
            let step = CaseMaterializationStepPlan {
                id: label.to_string(),
                stage: CaseMaterializationStage::Case,
                operation: CaseMaterializationOperation::Copy,
                command: Vec::new(),
                source,
                mount: None,
                workdir: None,
                network: Some("none".to_string()),
                timeout_ms: None,
                hidden: false,
            };
            let err = HostCaseMaterializationExecutor
                .execute(
                    HostCaseMaterializationRequest {
                        manifest_dir: &manifest_dir,
                        workspace_dir: &workspace,
                        log_dir: &logs,
                        phase: CaseMaterializationPhase::AgentVisible,
                        default_timeout_ms: 5_000,
                        env: BTreeMap::new(),
                    },
                    &[step],
                )
                .expect_err("source aliases should be rejected");
            assert!(
                err.to_string().contains(expected),
                "{label}: expected {expected}, got {err}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    fn step(
        id: &str,
        stage: CaseMaterializationStage,
        hidden: bool,
    ) -> CaseMaterializationStepPlan {
        CaseMaterializationStepPlan {
            id: id.to_string(),
            stage,
            operation: CaseMaterializationOperation::Command,
            command: vec!["true".to_string()],
            source: json!({}),
            mount: None,
            workdir: None,
            network: Some("none".to_string()),
            timeout_ms: None,
            hidden,
        }
    }
}
