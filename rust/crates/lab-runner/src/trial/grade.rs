use anyhow::{anyhow, Context, Result};
use lab_core::{BUCEPHALUS_CONTRACT_IN_DIR, BUCEPHALUS_CONTRACT_OUT_DIR};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::backend::docker::{ContainerHandle, DockerRuntime, ExecSpec};
use crate::config::trial_conclusion_outcome_to_trial_outcome;
use crate::experiment::runner::agent_artifact_archive_flag;
use crate::model::{GraderConfig, GradingStrategy, InTaskRuntimeGradingConfig};
use crate::trial::env::{resolve_grader_command, ResolvedGradingPhase};
use crate::trial::execution::{exit_status_label, TrialRunRequest};
use crate::trial::execution::{validate_agent_artifact_archive, validate_container_workspace_path};
use crate::trial::state::{GradingSandboxDetails, GradingSandboxPlan, IoMountPlan};
use crate::util::{sanitize_for_fs, shell_quote};

pub(crate) struct HiddenAssetBinding {
    pub(crate) hidden_path: String,
    pub(crate) revealed_path: String,
    pub(crate) stash_container_path: String,
}

const INJECTED_BUNDLE_SOURCE_MOUNT_PATH: &str = "/bucephalus/_materialize/injected_bundle_src";

pub(crate) fn task_grading_enabled(task_payload: &Value) -> bool {
    task_payload
        .pointer("/grading/enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

pub(crate) fn grading_retry_inputs(
    grading_enabled: bool,
    trial_conclusion_row: Option<&Value>,
    grade_error_reason: Option<&str>,
    agent_exit_status: &str,
    result_present: bool,
    result_parse_error: Option<&str>,
) -> (String, String) {
    let agent_outcome =
        agent_response_execution_outcome(agent_exit_status, result_present, result_parse_error);
    if !grading_enabled {
        return (agent_outcome.to_string(), agent_exit_status.to_string());
    }
    if agent_outcome == "timeout" {
        return ("timeout".to_string(), "timeout".to_string());
    }
    if grade_error_reason.is_some() {
        return ("error".to_string(), "0".to_string());
    }
    if let Some(mapped_outcome) = trial_conclusion_row
        .and_then(|row| row.pointer("/reported_outcome"))
        .and_then(Value::as_str)
        .and_then(trial_conclusion_outcome_to_trial_outcome)
    {
        return (mapped_outcome.to_string(), "0".to_string());
    }
    if trial_conclusion_row.is_some() {
        return ("missing".to_string(), "0".to_string());
    }
    ("error".to_string(), "0".to_string())
}

pub(crate) fn agent_response_execution_outcome(
    agent_exit_status: &str,
    result_present: bool,
    result_parse_error: Option<&str>,
) -> &'static str {
    if agent_exit_status == "timeout" {
        "timeout"
    } else if agent_exit_status != "0" {
        "error"
    } else if !result_present {
        "missing"
    } else if result_parse_error.is_some() {
        "error"
    } else {
        "success"
    }
}

pub(crate) fn mapped_grader_output_state(
    trial_conclusion_row: Option<&Value>,
    grade_error_reason: Option<&str>,
) -> Option<&'static str> {
    if trial_conclusion_row.is_some() {
        Some("valid")
    } else if let Some(reason) = grade_error_reason {
        if reason.starts_with("mapped_grader_output_invalid:") {
            Some("present_invalid")
        } else {
            Some("missing")
        }
    } else {
        None
    }
}

fn resolve_in_task_runtime_hidden_asset_pairs(
    grader: &GraderConfig,
) -> Result<Vec<(String, String)>> {
    if !matches!(grader.strategy, GradingStrategy::InTaskRuntime) {
        return Ok(Vec::new());
    }
    let config = required_in_task_runtime_config(grader)?;
    if config.hidden_paths.is_empty() && config.revealed_paths.is_empty() {
        return Ok(Vec::new());
    }
    if config.hidden_paths.is_empty() {
        return Err(anyhow!(
            "in_task_runtime grading revealed_paths requires hidden_paths to be configured"
        ));
    }
    if !config.revealed_paths.is_empty() && config.revealed_paths.len() != config.hidden_paths.len()
    {
        return Err(anyhow!(
            "in_task_runtime hidden_paths and revealed_paths must have matching lengths"
        ));
    }

    let mut bindings = Vec::with_capacity(config.hidden_paths.len());
    for (idx, hidden_path) in config.hidden_paths.iter().enumerate() {
        let revealed_path = config
            .revealed_paths
            .get(idx)
            .cloned()
            .unwrap_or_else(|| hidden_path.clone());
        validate_container_workspace_path(hidden_path).map_err(|err| {
            anyhow!(
                "invalid in_task_runtime hidden_paths[{}] '{}': {}",
                idx,
                hidden_path,
                err
            )
        })?;
        validate_container_workspace_path(&revealed_path).map_err(|err| {
            anyhow!(
                "invalid in_task_runtime revealed_paths[{}] '{}': {}",
                idx,
                revealed_path,
                err
            )
        })?;
        bindings.push((hidden_path.clone(), revealed_path));
    }
    Ok(bindings)
}

fn validate_in_task_runtime_hidden_asset_isolation(grader: &GraderConfig) -> Result<()> {
    resolve_in_task_runtime_hidden_asset_pairs(grader).map(|_| ())
}

fn required_in_task_runtime_config(grader: &GraderConfig) -> Result<&InTaskRuntimeGradingConfig> {
    grader.in_task_runtime.as_ref().ok_or_else(|| {
        anyhow!("in_task_runtime grading requires trial_runtime.grader.in_task_runtime")
    })
}

pub(crate) fn build_hidden_asset_bindings(
    grader: &GraderConfig,
) -> Result<Vec<HiddenAssetBinding>> {
    resolve_in_task_runtime_hidden_asset_pairs(grader)?
        .into_iter()
        .enumerate()
        .map(|(idx, (hidden_path, revealed_path))| {
            Ok(HiddenAssetBinding {
                hidden_path: hidden_path.clone(),
                revealed_path,
                stash_container_path: format!(
                    "/tmp/bucephalus_hidden_stash_{:02}_{}",
                    idx,
                    sanitize_for_fs(&hidden_path)
                ),
            })
        })
        .collect()
}

fn internal_exec_log_paths(trial_dir: &Path, label: &str) -> (PathBuf, PathBuf) {
    let name = sanitize_for_fs(label);
    let log_dir = trial_dir.join("logs").join("runtime");
    (
        log_dir.join(format!("{}_stdout.log", name)),
        log_dir.join(format!("{}_stderr.log", name)),
    )
}

fn run_exec_checked(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    trial_dir: &Path,
    label: &str,
    command: Vec<String>,
    workdir: Option<&str>,
    timeout_ms: u64,
) -> Result<()> {
    let exec = docker.exec(
        handle,
        &ExecSpec {
            command,
            env: BTreeMap::new(),
            workdir: workdir.map(str::to_string),
        },
    )?;
    let (stdout_path, stderr_path) = internal_exec_log_paths(trial_dir, label);
    let stream = docker.stream_exec_output(
        &exec,
        &stdout_path,
        &stderr_path,
        Some(Duration::from_millis(timeout_ms.max(1_000))),
    )?;
    let status = docker
        .wait_exec(&exec)
        .with_context(|| format!("wait for container command '{}'", label))?;
    if stream.timed_out {
        let stdout = read_exec_log(&stdout_path);
        let stderr = read_exec_log(&stderr_path);
        return Err(anyhow!(
            "container command '{}' timed out; stdout:\n{}\nstderr:\n{}\nlogs: {}, {}",
            label,
            stdout,
            stderr,
            stdout_path.display(),
            stderr_path.display()
        ));
    }
    if status.exit_code != Some(0) {
        let stdout = read_exec_log(&stdout_path);
        let stderr = read_exec_log(&stderr_path);
        return Err(anyhow!(
            "container command '{}' failed with exit status {}; stdout:\n{}\nstderr:\n{}\nlogs: {}, {}",
            label,
            exit_status_label(status.exit_code),
            stdout,
            stderr,
            stdout_path.display(),
            stderr_path.display()
        ));
    }
    Ok(())
}

fn read_exec_log(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => format!("<failed to read {}: {}>", path.display(), err),
    }
}

fn container_parent_str<'a>(path: &'a str, field_name: &str) -> Result<&'a str> {
    Path::new(path)
        .parent()
        .and_then(Path::to_str)
        .ok_or_else(|| anyhow!("{} must have a UTF-8 parent path: {}", field_name, path))
}

fn run_shell_checked(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    trial_dir: &Path,
    label: &str,
    script: &str,
    workdir: Option<&str>,
    timeout_ms: u64,
) -> Result<()> {
    run_exec_checked(
        docker,
        handle,
        trial_dir,
        label,
        vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            format!("set -e\n{}", script),
        ],
        workdir,
        timeout_ms,
    )
}

pub(crate) fn stash_hidden_assets(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    trial_dir: &Path,
    bindings: &[HiddenAssetBinding],
    timeout_ms: u64,
) -> Result<()> {
    for (idx, binding) in bindings.iter().enumerate() {
        run_shell_checked(
            docker,
            handle,
            trial_dir,
            &format!("hide_hidden_asset_{}", idx),
            &format!(
                "mkdir -p {stash_parent}\nrm -rf {stash}\nmv {hidden} {stash}",
                stash_parent = shell_quote(container_parent_str(
                    &binding.stash_container_path,
                    "hidden asset stash path",
                )?),
                stash = shell_quote(&binding.stash_container_path),
                hidden = shell_quote(&binding.hidden_path),
            ),
            None,
            timeout_ms,
        )?;
    }
    Ok(())
}

pub(crate) fn reveal_hidden_assets(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    trial_dir: &Path,
    bindings: &[HiddenAssetBinding],
    timeout_ms: u64,
) -> Result<()> {
    for (idx, binding) in bindings.iter().enumerate() {
        let reveal_parent =
            container_parent_str(&binding.revealed_path, "in_task_runtime revealed path")?;
        run_shell_checked(
            docker,
            handle,
            trial_dir,
            &format!("reveal_hidden_asset_{}", idx),
            &format!(
                "mkdir -p {parent}\nrm -rf {revealed}\nmv {stash} {revealed}",
                parent = shell_quote(reveal_parent),
                revealed = shell_quote(&binding.revealed_path),
                stash = shell_quote(&binding.stash_container_path),
            ),
            None,
            timeout_ms,
        )?;
    }
    Ok(())
}

pub(crate) fn materialize_injected_grader_bundle(
    docker: &DockerRuntime,
    handle: &ContainerHandle,
    trial_dir: &Path,
    resolved: &ResolvedGradingPhase,
    timeout_ms: u64,
) -> Result<()> {
    let source = resolved
        .injected_bundle_host_path
        .as_ref()
        .ok_or_else(|| anyhow!("injected grading missing resolved bundle host path"))?;
    let copy_dest = resolved
        .injected_copy_dest
        .as_deref()
        .ok_or_else(|| anyhow!("injected grading missing copy destination"))?;
    validate_container_workspace_path(copy_dest)?;
    let quoted_dest = shell_quote(copy_dest);
    let extract_script = if source.is_dir() {
        format!(
            "cp -R {src}/. {dest}",
            src = shell_quote(INJECTED_BUNDLE_SOURCE_MOUNT_PATH),
            dest = quoted_dest
        )
    } else if let Some(tar_flag) = agent_artifact_archive_flag(source) {
        validate_agent_artifact_archive(source)?;
        format!(
            "tar {tar_flag} {src} -C {dest}",
            tar_flag = tar_flag,
            src = shell_quote(INJECTED_BUNDLE_SOURCE_MOUNT_PATH),
            dest = quoted_dest
        )
    } else {
        format!(
            "cp {src} {dest}/",
            src = shell_quote(INJECTED_BUNDLE_SOURCE_MOUNT_PATH),
            dest = quoted_dest
        )
    };
    run_shell_checked(
        docker,
        handle,
        trial_dir,
        "injected_grader_bundle",
        &format!(
            "mkdir -p {dest}\nfind {dest} -mindepth 1 -maxdepth 1 -exec rm -rf {{}} +\n{extract}",
            dest = quoted_dest,
            extract = extract_script,
        ),
        None,
        timeout_ms,
    )
}

pub(crate) fn validate_grading_contract(request: &TrialRunRequest<'_>) -> Result<()> {
    if !request.grading_enabled {
        return Ok(());
    }
    let grader = request
        .grader
        .ok_or_else(|| anyhow!("grading enabled without grader config"))?;
    validate_in_task_runtime_hidden_asset_isolation(grader)?;
    if resolve_grader_command(request)?.is_none() {
        return Err(anyhow!(
            "grading is mandatory but no grader command resolved for this trial"
        ));
    }
    Ok(())
}

pub(crate) fn build_grading_sandbox_plan(
    grader: &GraderConfig,
    resolved: &ResolvedGradingPhase,
) -> Result<GradingSandboxPlan> {
    let details = match grader.strategy {
        GradingStrategy::None => {
            return Err(anyhow!("grader.strategy=none has no grading sandbox"))
        }
        GradingStrategy::InTaskRuntime => {
            validate_in_task_runtime_hidden_asset_isolation(grader)?;
            let config = required_in_task_runtime_config(grader)?;
            GradingSandboxDetails::InTaskRuntime {
                hidden_paths: config.hidden_paths.clone(),
                revealed_paths: config.revealed_paths.clone(),
            }
        }
        GradingStrategy::Injected => {
            let bundle_host_path = resolved
                .injected_bundle_host_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())
                .or_else(|| grader.injected.as_ref().map(|config| config.bundle.clone()))
                .ok_or_else(|| anyhow!("injected grading missing injected config"))?;
            let copy_dest = resolved
                .injected_copy_dest
                .clone()
                .or_else(|| {
                    grader
                        .injected
                        .as_ref()
                        .map(|config| config.copy_dest.clone())
                })
                .ok_or_else(|| anyhow!("injected grading missing injected copy destination"))?;
            GradingSandboxDetails::Injected {
                bundle_host_path,
                copy_dest,
            }
        }
        GradingStrategy::Separate => GradingSandboxDetails::Separate {
            image: resolved.image.clone(),
            workdir: resolved.workdir.clone(),
        },
        GradingStrategy::Host => GradingSandboxDetails::Host {
            workdir: resolved.workdir.clone(),
        },
    };
    Ok(GradingSandboxPlan {
        strategy: grader.strategy.clone(),
        command: resolved.command.clone(),
        io_mounts: IoMountPlan {
            in_dir: BUCEPHALUS_CONTRACT_IN_DIR.to_string(),
            out_dir: BUCEPHALUS_CONTRACT_OUT_DIR.to_string(),
            telemetry_mounts: Vec::new(),
        },
        details,
    })
}
