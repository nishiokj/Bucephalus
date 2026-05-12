use anyhow::Result;
use lab_core::{ensure_dir, AGENTLAB_CONTRACT_IN_DIR, AGENTLAB_CONTRACT_OUT_DIR};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::atomic_write_json_pretty;
use crate::experiment::runner::{
    map_contract_path_to_host, ContractPathHostRoots, ContractPathMode,
};
use crate::experiment::runtime::{
    AgentRuntimeConfig, ResolvedSecretFileMount, DEFAULT_TASK_WORKDIR_FALLBACK,
};
use crate::model::{
    MaterializationMode, BENCHMARK_GRADE_ERROR_FILENAME, MAPPED_GRADER_OUTPUT_FILENAME,
    RAW_GRADER_OUTPUT_FILENAME,
};
use crate::trial::execution::resolve_container_image_digest;
use crate::trial::prepare::TrialPaths;
use crate::util::{copy_dir_preserve_contents, copy_file_if_exists, remove_path_if_exists};

pub(crate) fn trial_agent_dir(trial_dir: &Path) -> PathBuf {
    trial_dir.join("agent")
}

pub(crate) fn trial_grader_dir(trial_dir: &Path) -> PathBuf {
    trial_dir.join("grader")
}

pub(crate) fn trial_runner_dir(trial_dir: &Path) -> PathBuf {
    trial_dir.join("runner")
}

pub(crate) fn trial_task_dir(trial_dir: &Path) -> PathBuf {
    trial_dir.join("task")
}

pub(crate) fn trial_agent_stdout_path(trial_dir: &Path) -> PathBuf {
    trial_agent_dir(trial_dir).join("stdout.log")
}

pub(crate) fn trial_agent_stderr_path(trial_dir: &Path) -> PathBuf {
    trial_agent_dir(trial_dir).join("stderr.log")
}

pub(crate) fn trial_grader_stdout_path(trial_dir: &Path) -> PathBuf {
    trial_grader_dir(trial_dir).join("stdout.log")
}

pub(crate) fn trial_grader_stderr_path(trial_dir: &Path) -> PathBuf {
    trial_grader_dir(trial_dir).join("stderr.log")
}

pub(crate) fn trial_mapper_stdout_path(trial_dir: &Path) -> PathBuf {
    trial_grader_dir(trial_dir).join("mapper_stdout.log")
}

pub(crate) fn trial_mapper_stderr_path(trial_dir: &Path) -> PathBuf {
    trial_grader_dir(trial_dir).join("mapper_stderr.log")
}

pub(crate) fn trial_patch_log_dir(trial_dir: &Path) -> PathBuf {
    trial_runner_dir(trial_dir).join("workspace_patch")
}

pub(crate) fn trial_benchmark_preflight_path(trial_dir: &Path) -> PathBuf {
    trial_runner_dir(trial_dir).join("benchmark_preflight.json")
}

pub(crate) fn trial_metadata_path(trial_dir: &Path) -> PathBuf {
    trial_runner_dir(trial_dir).join("trial_metadata.json")
}

pub(crate) fn trial_state_inventory_path(trial_dir: &Path) -> PathBuf {
    trial_runner_dir(trial_dir).join("state_inventory.json")
}

pub(crate) fn trial_candidate_patch_path(trial_dir: &Path) -> PathBuf {
    trial_dir.join("candidate.patch")
}

pub(crate) fn trial_summary_path(trial_dir: &Path) -> PathBuf {
    trial_dir.join("summary.json")
}

pub(crate) fn ensure_trial_surface_dirs(trial_dir: &Path) -> Result<()> {
    ensure_dir(&trial_agent_dir(trial_dir))?;
    ensure_dir(&trial_grader_dir(trial_dir))?;
    ensure_dir(&trial_runner_dir(trial_dir))?;
    ensure_dir(&trial_task_dir(trial_dir))?;
    Ok(())
}

pub(crate) fn prune_empty_trial_logs(trial_dir: &Path) -> Result<()> {
    for dir in [
        trial_agent_dir(trial_dir),
        trial_grader_dir(trial_dir),
        trial_runner_dir(trial_dir),
    ] {
        if !dir.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            let path = entry.path();
            if !entry.file_type().is_file()
                || path.extension().and_then(|ext| ext.to_str()) != Some("log")
            {
                continue;
            }
            if fs::metadata(path).map(|meta| meta.len()).unwrap_or(1) == 0 {
                let _ = fs::remove_file(path);
            }
        }
    }
    Ok(())
}

pub(crate) fn materialize_trial_result(trial_dir: &Path, output_path: &Path) -> Result<PathBuf> {
    ensure_dir(&trial_agent_dir(trial_dir))?;
    let canonical_output = trial_agent_dir(trial_dir).join("result.json");
    if output_path != canonical_output {
        if canonical_output.exists() {
            let _ = fs::remove_file(&canonical_output);
        }
        if output_path.exists() {
            if let Some(parent) = canonical_output.parent() {
                ensure_dir(parent)?;
            }
            fs::copy(output_path, &canonical_output)?;
        }
    }
    Ok(canonical_output)
}

fn materialize_trial_outputs_surface(trial_dir: &Path, paths: &TrialPaths) -> Result<()> {
    ensure_trial_surface_dirs(trial_dir)?;
    copy_file_if_exists(
        &paths.runtime.result,
        &trial_agent_dir(trial_dir).join("result.json"),
    )?;
    copy_file_if_exists(
        &paths.out.join("candidate.patch"),
        &trial_candidate_patch_path(trial_dir),
    )?;
    copy_file_if_exists(
        &paths.out.join("rex-events.jsonl"),
        &trial_agent_dir(trial_dir).join("events.jsonl"),
    )?;
    copy_file_if_exists(
        &paths.out.join(RAW_GRADER_OUTPUT_FILENAME),
        &trial_grader_dir(trial_dir).join("raw_output.json"),
    )?;
    copy_file_if_exists(
        &paths.out.join(MAPPED_GRADER_OUTPUT_FILENAME),
        &trial_grader_dir(trial_dir).join("mapped_output.json"),
    )?;
    copy_file_if_exists(
        &paths.out.join(BENCHMARK_GRADE_ERROR_FILENAME),
        &trial_grader_dir(trial_dir).join("error.txt"),
    )?;
    let official_eval = paths.out.join("official_swebench_eval");
    if official_eval.exists() {
        copy_dir_preserve_contents(
            &official_eval,
            &trial_grader_dir(trial_dir).join("official_swebench_eval"),
        )?;
    }
    Ok(())
}

pub(crate) fn materialize_trial_runtime_layout(
    trial_dir: &Path,
    paths: &TrialPaths,
    mode: MaterializationMode,
) -> Result<()> {
    match mode {
        MaterializationMode::Full => {
            copy_dir_preserve_contents(&paths.in_dir, &trial_dir.join("in"))?;
            copy_dir_preserve_contents(&paths.out, &trial_dir.join("out"))?;
            copy_dir_preserve_contents(&paths.state, &trial_dir.join("state"))?;
            copy_dir_preserve_contents(&paths.workspace, &trial_dir.join("workspace"))?;
            copy_dir_preserve_contents(&paths.tmp, &trial_dir.join("tmp"))?;
            copy_file_if_exists(
                &paths.runtime.trial_input,
                &trial_dir.join("trial_input.json"),
            )?;
            copy_file_if_exists(
                &paths.out.join("harness_manifest.json"),
                &trial_dir.join("harness_manifest.json"),
            )?;
            let _ = materialize_trial_result(trial_dir, &paths.runtime.result)?;
        }
        MaterializationMode::OutputsOnly => {
            materialize_trial_outputs_surface(trial_dir, paths)?;
            copy_file_if_exists(
                &paths.out.join("harness_manifest.json"),
                &trial_runner_dir(trial_dir).join("harness_manifest.json"),
            )?;
        }
        MaterializationMode::MetadataOnly | MaterializationMode::None => {}
    }
    apply_materialization_policy(trial_dir, mode)
}

fn apply_materialization_policy(trial_dir: &Path, mode: MaterializationMode) -> Result<()> {
    match mode {
        MaterializationMode::Full => return Ok(()),
        MaterializationMode::OutputsOnly => {
            for dir_name in ["workspace", "state", "tmp", "artifacts", "out", "runtime"] {
                remove_path_if_exists(&trial_dir.join(dir_name))?;
            }
            remove_path_if_exists(&trial_dir.join("result.json"))?;
            remove_path_if_exists(&trial_dir.join("harness_manifest.json"))?;
            remove_path_if_exists(&trial_dir.join("trial_runtime_state.json"))?;
            remove_path_if_exists(&trial_dir.join("trial_state.json"))?;
        }
        MaterializationMode::MetadataOnly | MaterializationMode::None => {
            for dir_name in ["workspace", "state", "tmp", "artifacts", "out", "runtime"] {
                remove_path_if_exists(&trial_dir.join(dir_name))?;
            }
            remove_path_if_exists(&trial_dir.join("trial_input.json"))?;
            remove_path_if_exists(&trial_dir.join("result.json"))?;
            remove_path_if_exists(&trial_dir.join("harness_manifest.json"))?;
            remove_path_if_exists(&trial_dir.join("trace_manifest.json"))?;
            remove_path_if_exists(&trial_dir.join("trial_runtime_state.json"))?;
            remove_path_if_exists(&trial_dir.join("trial_state.json"))?;
            remove_path_if_exists(&trial_agent_dir(trial_dir))?;
            remove_path_if_exists(&trial_grader_dir(trial_dir))?;
            remove_path_if_exists(&trial_runner_dir(trial_dir))?;
            remove_path_if_exists(&trial_task_dir(trial_dir))?;
            remove_path_if_exists(&trial_candidate_patch_path(trial_dir))?;
            remove_path_if_exists(&trial_summary_path(trial_dir))?;
            if matches!(mode, MaterializationMode::None) {
                remove_path_if_exists(&trial_state_inventory_path(trial_dir))?;
            }
        }
    }
    Ok(())
}

pub(crate) fn resolve_agent_runtime_manifest_path(paths: &TrialPaths) -> Result<PathBuf> {
    map_contract_path_to_host(
        &format!("{}/harness_manifest.json", AGENTLAB_CONTRACT_OUT_DIR),
        &ContractPathHostRoots::from_trial_paths(paths),
        ContractPathMode::ContainerMount,
    )
}

pub(crate) fn write_state_inventory(
    trial_dir: &Path,
    json_value: &Value,
    agent_runtime: &AgentRuntimeConfig,
    secret_file_mounts: &[ResolvedSecretFileMount],
    _paths: &TrialPaths,
    exec_digest: &str,
    effective_network_mode: &str,
    invocation_source: &str,
    task_sandbox_image: Option<&str>,
    task_sandbox_workdir: Option<&str>,
) -> Result<()> {
    let sanitization_profile = json_value
        .pointer("/design/sanitization_profile")
        .and_then(|v| v.as_str())
        .unwrap_or("hermetic_functional");
    let integration_level = agent_runtime.integration_level.as_str();
    let mode_requested = json_value
        .pointer("/policy/task_sandbox/network")
        .and_then(|v| v.as_str())
        .unwrap_or("none");
    let mode_effective = effective_network_mode;
    let enforcement_effective = if mode_requested == "none" {
        "docker_none"
    } else {
        "unknown"
    };
    let workspace_path = task_sandbox_workdir.unwrap_or(DEFAULT_TASK_WORKDIR_FALLBACK);

    let mounts = vec![
        json!({"name": "in", "path": AGENTLAB_CONTRACT_IN_DIR, "writable": false}),
        json!({"name": "workdir", "path": workspace_path, "writable": true}),
        json!({"name": "out", "path": AGENTLAB_CONTRACT_OUT_DIR, "writable": true}),
        json!({"name": "tmp", "path": "/tmp", "writable": true}),
    ];
    let mut agent_runtime_mounts = vec![
        json!({"name": "in", "path": AGENTLAB_CONTRACT_IN_DIR, "writable": false}),
        json!({"name": "workdir", "path": workspace_path, "writable": true}),
        json!({"name": "out", "path": AGENTLAB_CONTRACT_OUT_DIR, "writable": true}),
        json!({"name": "tmp", "path": "/tmp", "writable": true}),
    ];
    agent_runtime_mounts.push(json!({
        "name": "agent_bundle",
        "path": "/opt/agent",
        "writable": false
    }));
    for secret in secret_file_mounts {
        agent_runtime_mounts.push(json!({
            "name": format!("secret:{}", secret.id),
            "path": secret.target_path,
            "writable": false,
            "secret": true
        }));
    }
    let output_mounts = agent_runtime
        .output_mounts
        .iter()
        .map(|mount| {
            json!({
                "id": mount.id,
                "kind": mount.kind,
                "path": mount.path,
                "container_path": mount.container_path(),
                "env": mount.env,
                "persist": mount.persist
            })
        })
        .collect::<Vec<_>>();
    let mut task_sandbox_mounts = vec![
        json!({"name": "in", "path": AGENTLAB_CONTRACT_IN_DIR, "writable": false}),
        json!({"name": "workdir", "path": workspace_path, "writable": true}),
        json!({"name": "out", "path": AGENTLAB_CONTRACT_OUT_DIR, "writable": true}),
        json!({"name": "tmp", "path": "/tmp", "writable": true}),
    ];
    let sandbox_profile = json_value
        .pointer("/policy/task_sandbox/profile")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    if sandbox_profile == "swebench_testbed" {
        task_sandbox_mounts.push(json!({
            "name": "testbed",
            "path": "/testbed",
            "writable": true
        }));
    }
    let agent_runtime_image = Some(agent_runtime.image.as_str());
    let agent_runtime_image_digest = agent_runtime_image.and_then(resolve_container_image_digest);
    let task_sandbox_image_digest = task_sandbox_image.and_then(resolve_container_image_digest);
    let event_sinks = agent_runtime
        .event_sinks
        .iter()
        .map(|sink| {
            json!({
                "id": sink.id,
                "format": sink.format,
                "path": sink.path,
                "mode": sink.mode,
                "persist": sink.persist,
                "ingest": sink.ingest
            })
        })
        .collect::<Vec<_>>();

    let state = json!({
        "schema_version": "state_inventory_v1",
        "sanitization_profile": sanitization_profile,
        "integration_level": integration_level,
        "mounts": mounts,
        "network": {
            "mode_requested": mode_requested,
            "mode_effective": mode_effective,
            "allowed_hosts": json_value
                .pointer("/policy/task_sandbox/allowed_hosts")
                .cloned()
                .unwrap_or(json!([])),
            "enforcement_effective": enforcement_effective,
            "egress_self_test": {
                "performed": false,
                "cases": []
            }
        },
        "harness_identity": {
            "name": agent_runtime.command_raw.first().cloned().unwrap_or("unknown".to_string()),
            "exec_digest": exec_digest,
            "entry_command": agent_runtime.command_raw.clone()
        },
        "planes": {
            "agent_runtime": {
                "executor": "docker",
                "image": agent_runtime_image,
                "image_digest": agent_runtime_image_digest,
                "workdir": workspace_path,
                "mounts": agent_runtime_mounts,
                "network_mode": agent_runtime.network,
                "event_sinks": event_sinks,
                "output_mounts": output_mounts
            },
            "task_sandbox": {
                "executor": "docker",
                "image": task_sandbox_image,
                "image_digest": task_sandbox_image_digest,
                "workdir": workspace_path,
                "mounts": task_sandbox_mounts,
                "network_mode": mode_effective
            }
        },
        "ext": {
            "agent_runtime_identity": {
                "invocation_source": invocation_source
            }
        },
        "violations": {
            "state_leak": false,
            "profile_invariant_violation": false,
            "notes": []
        }
    });
    ensure_dir(&trial_runner_dir(trial_dir))?;
    atomic_write_json_pretty(&trial_state_inventory_path(trial_dir), &state)?;
    Ok(())
}
