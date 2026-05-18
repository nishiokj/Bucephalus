use anyhow::{anyhow, Context, Result};
use lab_core::{
    AGENTLAB_CONTRACT_RUNTIME_AUX_DIR, AGENTLAB_RUNNER_SUPPORT_REL_DIR,
    AGENTLAB_TASK_WORKDIR_PLACEHOLDER,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::config::*;
use crate::model::*;
use crate::package::authoring::{
    contains_removed_runtime_template, resolve_existing_public_path_reference,
    validate_public_authoring_relpath,
};
use crate::package::compile::*;
use crate::package::registry::load_grader_capability_for_exp_dir;
use crate::package::sealed::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimePathStagingManifestEntry {
    pub(crate) original_relative_path: String,
    pub(crate) packaged_path: String,
    pub(crate) runtime_path: String,
    pub(crate) required: bool,
    pub(crate) read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimePathStagingManifest {
    pub(crate) schema_version: String,
    pub(crate) variants: BTreeMap<String, Vec<RuntimePathStagingManifestEntry>>,
}

pub(crate) fn task_workdir_support_relative_path(rel_path: &str) -> String {
    let rel = rel_path.trim().trim_start_matches('/');
    if rel.is_empty() {
        AGENTLAB_RUNNER_SUPPORT_REL_DIR.to_string()
    } else {
        format!("{}/{}", AGENTLAB_RUNNER_SUPPORT_REL_DIR, rel)
    }
}

pub(crate) fn task_workdir_support_destination_path(rel_path: &str) -> String {
    format!(
        "{}/{}",
        AGENTLAB_TASK_WORKDIR_PLACEHOLDER,
        task_workdir_support_relative_path(rel_path)
    )
}

pub(crate) fn strip_task_workdir_support_destination_path(path: &str) -> Option<&str> {
    let prefix = format!(
        "{}/{}",
        AGENTLAB_TASK_WORKDIR_PLACEHOLDER, AGENTLAB_RUNNER_SUPPORT_REL_DIR
    );
    if path == prefix {
        return Some("");
    }
    let rest = path.strip_prefix(&prefix)?;
    if rest.starts_with('/') {
        Some(rest.trim_start_matches('/'))
    } else {
        None
    }
}

pub(crate) fn validate_runner_staged_destination_path(
    raw: &str,
    field_name: &str,
) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{} must not be empty", field_name));
    }
    let task_support_prefix = format!(
        "{}/{}",
        AGENTLAB_TASK_WORKDIR_PLACEHOLDER, AGENTLAB_RUNNER_SUPPORT_REL_DIR
    );
    if trimmed == task_support_prefix || trimmed.starts_with(&format!("{}/", task_support_prefix)) {
        let rest = trimmed
            .strip_prefix(AGENTLAB_TASK_WORKDIR_PLACEHOLDER)
            .unwrap_or_default();
        for component in Path::new(rest).components() {
            if matches!(component, Component::ParentDir) {
                return Err(anyhow!("{} cannot contain '..'", field_name));
            }
        }
        return Ok(trimmed.to_string());
    }
    let path = Path::new(trimmed);
    if !path.is_absolute() {
        return Err(anyhow!(
            "{} must be under {}/{} or {}",
            field_name,
            AGENTLAB_TASK_WORKDIR_PLACEHOLDER,
            AGENTLAB_RUNNER_SUPPORT_REL_DIR,
            AGENTLAB_CONTRACT_RUNTIME_AUX_DIR
        ));
    }
    if !(trimmed == AGENTLAB_CONTRACT_RUNTIME_AUX_DIR
        || trimmed.starts_with(&format!("{}/", AGENTLAB_CONTRACT_RUNTIME_AUX_DIR)))
    {
        return Err(anyhow!(
            "{} must be under {}/{} or {}",
            field_name,
            AGENTLAB_TASK_WORKDIR_PLACEHOLDER,
            AGENTLAB_RUNNER_SUPPORT_REL_DIR,
            AGENTLAB_CONTRACT_RUNTIME_AUX_DIR
        ));
    }
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(anyhow!("{} cannot contain '..'", field_name));
        }
    }
    Ok(trimmed.to_string())
}

pub(crate) fn stage_source_into_package(
    raw_source: &str,
    exp_dir: &Path,
    package_dir: &Path,
    subdir: &str,
    prefix: &str,
    copies: &mut BTreeMap<String, String>,
    counter: &mut usize,
) -> Result<String> {
    let raw_path = PathBuf::from(raw_source);
    let resolved = if raw_path.is_absolute() {
        normalize_path(&raw_path)
    } else {
        normalize_path(&exp_dir.join(raw_path))
    };
    let key = resolved.to_string_lossy().to_string();
    if let Some(existing) = copies.get(&key) {
        return Ok(existing.clone());
    }
    fs::metadata(&resolved).with_context(|| {
        format!(
            "package build failed to read staged source '{}' resolved from '{}'",
            resolved.display(),
            raw_source
        )
    })?;
    let name = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}_{}", prefix, counter));
    let rel_path = PathBuf::from(subdir).join(format!("{:03}_{}", *counter, name));
    let destination = package_dir.join(&rel_path);
    if subdir == "agent_builds" {
        copy_agent_artifact_into_package(&resolved, &destination)?;
    } else if subdir == PACKAGED_RUNTIME_ASSETS_DIR {
        copy_runtime_asset_into_package(&resolved, &destination, package_dir)?;
    } else {
        copy_path_into_package(&resolved, &destination)?;
    }
    *counter += 1;
    let rel_portable = as_portable_rel(&rel_path);
    copies.insert(key, rel_portable.clone());
    Ok(rel_portable)
}

pub(crate) fn stage_public_runtime_path_reference(
    rel: &Path,
    exp_dir: &Path,
    package_dir: &Path,
    copies: &mut BTreeMap<String, String>,
    manifest_entries: &mut Vec<RuntimePathStagingManifestEntry>,
    field_name: &str,
) -> Result<String> {
    let rel_portable = as_portable_rel(rel);
    let resolved = normalize_path(&exp_dir.join(rel));
    fs::metadata(&resolved).with_context(|| {
        format!(
            "package build failed to read {} public path reference '{}' resolved to '{}'",
            field_name,
            rel_portable,
            resolved.display()
        )
    })?;
    if copies.contains_key(&rel_portable) {
        return Ok(task_workdir_support_destination_path(&rel_portable));
    }
    let packaged_rel = PathBuf::from(PACKAGED_RUNTIME_ASSETS_DIR).join(rel);
    let packaged_rel_portable = as_portable_rel(&packaged_rel);
    let destination = package_dir.join(&packaged_rel);
    copy_runtime_asset_into_package(&resolved, &destination, package_dir)?;
    copies.insert(rel_portable.clone(), packaged_rel_portable.clone());
    manifest_entries.push(RuntimePathStagingManifestEntry {
        original_relative_path: rel_portable.clone(),
        packaged_path: packaged_rel_portable,
        runtime_path: task_workdir_support_destination_path(&rel_portable),
        required: true,
        read_only: true,
    });
    Ok(task_workdir_support_destination_path(&rel_portable))
}

pub(crate) fn is_runner_staged_destination_path(raw: &str) -> bool {
    strip_task_workdir_support_destination_path(raw).is_some()
        || raw == AGENTLAB_CONTRACT_RUNTIME_AUX_DIR
        || raw.starts_with(&format!("{}/", AGENTLAB_CONTRACT_RUNTIME_AUX_DIR))
}

pub(crate) fn rewrite_packaged_runtime_asset_entries(
    entries: Option<&mut Value>,
    field_name: &str,
    exp_dir: &Path,
    package_dir: &Path,
    file_copies: &mut BTreeMap<String, String>,
    file_counter: &mut usize,
) -> Result<()> {
    let Some(items) = entries.and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for (idx, item) in items.iter_mut().enumerate() {
        let raw = item
            .get("build_source_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{}[{}].build_source_path is required", field_name, idx))?;
        let rel = stage_source_into_package(
            raw,
            exp_dir,
            package_dir,
            PACKAGED_RUNTIME_ASSETS_DIR,
            "dep",
            file_copies,
            file_counter,
        )
        .with_context(|| {
            format!(
                "failed to stage {}[{}].build_source_path '{}' into sealed package",
                field_name, idx, raw
            )
        })?;
        if let Some(obj) = item.as_object_mut() {
            obj.remove("build_source_path");
        }
        set_json_pointer_value(item, "/packaged_path", json!(rel))?;
    }
    Ok(())
}

pub(crate) fn rewrite_optional_package_source_path(
    value: Option<&mut Value>,
    field_name: &str,
    exp_dir: &Path,
    package_dir: &Path,
    subdir: &str,
    prefix: &str,
    file_copies: &mut BTreeMap<String, String>,
    file_counter: &mut usize,
) -> Result<()> {
    let Some(item) = value else {
        return Ok(());
    };
    let Some(raw) = item
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let rel = stage_source_into_package(
        raw,
        exp_dir,
        package_dir,
        subdir,
        prefix,
        file_copies,
        file_counter,
    )
    .with_context(|| {
        format!(
            "failed to stage {} '{}' into sealed package",
            field_name, raw
        )
    })?;
    *item = Value::String(rel);
    Ok(())
}

pub(crate) fn stage_command_path_refs_for_package(
    command_root: Option<&mut Value>,
    field_name: &str,
    exp_dir: &Path,
    package_dir: &Path,
    public_path_copies: &mut BTreeMap<String, String>,
    staging_manifest_entries: &mut Vec<RuntimePathStagingManifestEntry>,
) -> Result<()> {
    let Some(items) = command_root.and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for idx in 0..items.len() {
        let token = items[idx]
            .as_str()
            .ok_or_else(|| anyhow!("{}[{}] must be a string", field_name, idx))?;
        if contains_removed_runtime_template(token) {
            return Err(anyhow!(
                "{}[{}] uses removed '${{...}}' syntax; use $NAME runtime bindings instead",
                field_name,
                idx
            ));
        }
        if idx == 0 {
            continue;
        }
        if is_runner_staged_destination_path(token) {
            continue;
        }
        let Some(rel) = resolve_existing_public_path_reference(
            token,
            exp_dir,
            &format!("{}[{}]", field_name, idx),
        )
        .with_context(|| format!("while staging command path refs for {}", field_name))?
        else {
            continue;
        };
        let contract_path = stage_public_runtime_path_reference(
            &rel,
            exp_dir,
            package_dir,
            public_path_copies,
            staging_manifest_entries,
            &format!("{}[{}]", field_name, idx),
        )?;
        items[idx] = Value::String(contract_path);
    }
    Ok(())
}

pub(crate) fn validate_host_grader_command_package_boundary(
    command_root: Option<&Value>,
    capability: &str,
    field_name: &str,
    exp_dir: &Path,
    package_dir: &Path,
) -> Result<()> {
    if capability.trim().is_empty() {
        return Err(anyhow!(
            "trial_runtime.grader.host.capability is required when strategy='host'"
        ));
    }
    validate_host_grader_capability_id(capability)?;
    let capability_manifest = load_grader_capability_for_exp_dir(exp_dir, capability)?;
    let Some(items) = command_root.and_then(Value::as_array) else {
        return Ok(());
    };
    let mut saw_capability_path = false;
    for (idx, item) in items.iter().enumerate() {
        let token = item
            .as_str()
            .ok_or_else(|| anyhow!("{}[{}] must be a string", field_name, idx))?;
        if contains_removed_runtime_template(token) {
            return Err(anyhow!(
                "{}[{}] uses removed '${{...}}' syntax; use $NAME runtime bindings instead",
                field_name,
                idx
            ));
        }
        if idx == 0 {
            continue;
        }
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == HOST_GRADER_CAPABILITY_PREFIX
            || trimmed.starts_with(&format!("{}/", HOST_GRADER_CAPABILITY_PREFIX))
        {
            let rel = trimmed
                .strip_prefix(HOST_GRADER_CAPABILITY_PREFIX)
                .unwrap_or_default()
                .trim_start_matches('/');
            let mut parts = rel.splitn(2, '/');
            let command_capability = parts.next().unwrap_or_default();
            let relative_path = parts.next().unwrap_or_default();
            if command_capability != capability {
                return Err(anyhow!(
                    "{}[{}] uses host grader capability '{}' but trial_runtime.grader.host.capability is '{}'",
                    field_name,
                    idx,
                    command_capability,
                    capability
                ));
            }
            let host_path = capability_manifest.resolve_host_path(relative_path)?;
            stage_host_grader_capability_path(&host_path, package_dir, capability, relative_path)?;
            saw_capability_path = true;
            continue;
        }
        if is_runner_staged_destination_path(trimmed)
            || trimmed.starts_with(AGENTLAB_TASK_WORKDIR_PLACEHOLDER)
            || trimmed.starts_with("/agentlab/")
        {
            return Err(anyhow!(
                "{}[{}] crosses runtime boundaries: host graders cannot execute task-workdir or runner-staged assets; declare a host grader capability instead",
                field_name,
                idx
            ));
        }
        if Path::new(trimmed).is_absolute() {
            return Err(anyhow!(
                "{}[{}] references an absolute host path '{}'; host grader code must be declared through trial_runtime.grader.host.capability",
                field_name,
                idx,
                trimmed
            ));
        }
        if let Some(rel) = resolve_existing_public_path_reference(
            trimmed,
            exp_dir,
            &format!("{}[{}]", field_name, idx),
        )
        .with_context(|| {
            format!(
                "while validating host grader command boundary for {}",
                field_name
            )
        })? {
            return Err(anyhow!(
                "{}[{}] references package-local file '{}'; host grader files cannot be staged into the task workspace. Use trial_runtime.grader.host.capability for package-scoped graders",
                field_name,
                idx,
                rel.display()
            ));
        }
    }
    if !saw_capability_path {
        return Err(anyhow!(
            "{} must reference a host grader capability under {}/{}/...",
            field_name,
            HOST_GRADER_CAPABILITY_PREFIX,
            capability
        ));
    }
    Ok(())
}

fn validate_host_grader_capability_id(raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || Path::new(trimmed).is_absolute() {
        return Err(anyhow!(
            "host grader capability id must be a non-empty path segment"
        ));
    }
    if Path::new(trimmed).components().count() != 1
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed == "."
        || trimmed == ".."
    {
        return Err(anyhow!(
            "host grader capability id must be a single path segment: '{}'",
            raw
        ));
    }
    Ok(())
}

fn stage_host_grader_capability_path(
    host_path: &Path,
    package_dir: &Path,
    capability: &str,
    relative_path: &str,
) -> Result<()> {
    let target = package_dir
        .join(HOST_GRADER_CAPABILITIES_DIR)
        .join(capability)
        .join(relative_path);
    copy_path_into_package(host_path, &target)
}

pub(crate) fn validate_grader_command_has_no_package_local_refs(
    command_root: Option<&Value>,
    field_name: &str,
    strategy: &str,
    exp_dir: &Path,
) -> Result<()> {
    let Some(items) = command_root.and_then(Value::as_array) else {
        return Ok(());
    };
    for (idx, item) in items.iter().enumerate() {
        let token = item
            .as_str()
            .ok_or_else(|| anyhow!("{}[{}] must be a string", field_name, idx))?;
        if contains_removed_runtime_template(token) {
            return Err(anyhow!(
                "{}[{}] uses removed '${{...}}' syntax; use $NAME runtime bindings instead",
                field_name,
                idx
            ));
        }
        if idx == 0 {
            continue;
        }
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_runner_staged_destination_path(trimmed)
            || trimmed.starts_with(AGENTLAB_TASK_WORKDIR_PLACEHOLDER)
            || trimmed.starts_with("/agentlab/")
        {
            return Err(anyhow!(
                "{}[{}] crosses runtime boundaries: strategy='{}' grader commands cannot reference task-workdir or runner-staged assets directly",
                field_name,
                idx,
                strategy
            ));
        }
        if let Some(rel) = resolve_existing_public_path_reference(
            trimmed,
            exp_dir,
            &format!("{}[{}]", field_name, idx),
        )
        .with_context(|| {
            format!(
                "while validating strategy='{}' grader command boundary for {}",
                strategy, field_name
            )
        })? {
            return Err(anyhow!(
                "{}[{}] references package-local file '{}'; strategy='{}' owns grader files through its explicit grader runtime fields, not generic command path staging",
                field_name,
                idx,
                rel.display(),
                strategy
            ));
        }
    }
    Ok(())
}

pub(crate) fn reject_grader_runtime_assets(value: Option<&Value>, strategy: &str) -> Result<()> {
    if value
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
    {
        return Err(anyhow!(
            "trial_runtime.grader._runtime_assets is not valid for strategy='{}'; declare grader-owned assets through that grader runtime strategy",
            strategy
        ));
    }
    Ok(())
}

pub(crate) fn rewrite_grader_paths_for_package(
    grader_root: &mut Value,
    exp_dir: &Path,
    package_dir: &Path,
    file_copies: &mut BTreeMap<String, String>,
    file_counter: &mut usize,
    public_path_copies: &mut BTreeMap<String, String>,
    staging_manifest_entries: &mut Vec<RuntimePathStagingManifestEntry>,
) -> Result<()> {
    let strategy = grader_root
        .pointer("/strategy")
        .and_then(Value::as_str)
        .unwrap_or("in_task_runtime");
    match strategy {
        "none" => {
            reject_grader_runtime_assets(grader_root.pointer("/_runtime_assets"), strategy)?;
        }
        "host" => {
            reject_grader_runtime_assets(grader_root.pointer("/_runtime_assets"), strategy)?;
            let capability = grader_root
                .pointer("/host/capability")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            validate_host_grader_command_package_boundary(
                grader_root.pointer("/command"),
                &capability,
                "trial_runtime.grader.command",
                exp_dir,
                package_dir,
            )?;
        }
        "in_task_runtime" => {
            rewrite_packaged_runtime_asset_entries(
                grader_root.pointer_mut("/_runtime_assets"),
                "trial_runtime.grader._runtime_assets",
                exp_dir,
                package_dir,
                file_copies,
                file_counter,
            )?;
            stage_command_path_refs_for_package(
                grader_root.pointer_mut("/command"),
                "trial_runtime.grader.command",
                exp_dir,
                package_dir,
                public_path_copies,
                staging_manifest_entries,
            )?;
        }
        "injected" => {
            reject_grader_runtime_assets(grader_root.pointer("/_runtime_assets"), strategy)?;
            validate_grader_command_has_no_package_local_refs(
                grader_root.pointer("/command"),
                "trial_runtime.grader.command",
                strategy,
                exp_dir,
            )?;
            rewrite_optional_package_source_path(
                grader_root.pointer_mut("/injected/bundle"),
                "trial_runtime.grader.injected.bundle",
                exp_dir,
                package_dir,
                "files",
                "grader_bundle",
                file_copies,
                file_counter,
            )?;
        }
        "separate" => {
            validate_grader_command_has_no_package_local_refs(
                grader_root.pointer("/command"),
                "trial_runtime.grader.command",
                strategy,
                exp_dir,
            )?;
            rewrite_packaged_runtime_asset_entries(
                grader_root.pointer_mut("/_runtime_assets"),
                "trial_runtime.grader._runtime_assets",
                exp_dir,
                package_dir,
                file_copies,
                file_counter,
            )?;
        }
        other => {
            return Err(anyhow!(
                "trial_runtime.grader.strategy '{}' is not supported",
                other
            ));
        }
    }
    Ok(())
}

pub(crate) fn stage_agent_command_env_path_refs_for_package(
    agent_root: &mut Value,
    exp_dir: &Path,
    package_dir: &Path,
    public_path_copies: &mut BTreeMap<String, String>,
    staging_manifest_entries: &mut Vec<RuntimePathStagingManifestEntry>,
) -> Result<()> {
    stage_command_path_refs_for_package(
        agent_root.pointer_mut("/command"),
        "trial_runtime.agent.command",
        exp_dir,
        package_dir,
        public_path_copies,
        staging_manifest_entries,
    )?;
    if let Some(items) = agent_root
        .pointer_mut("/env")
        .and_then(Value::as_object_mut)
    {
        let keys = items.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let raw = items
                .get(&key)
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("trial_runtime.agent.env.{} must be a string", key))?;
            if contains_removed_runtime_template(raw) {
                return Err(anyhow!(
                    "trial_runtime.agent.env.{} uses removed '${{...}}' syntax; use $NAME runtime bindings instead",
                    key
                ));
            }
            if raw.trim().starts_with("/agentlab/") {
                return Err(anyhow!(
                    "trial_runtime.agent.env.{} leaks runner topology; remove internal /agentlab paths from public authoring",
                    key
                ));
            }
            if is_runner_staged_destination_path(raw) {
                continue;
            }
            let Some(rel) = resolve_existing_public_path_reference(
                raw,
                exp_dir,
                &format!("trial_runtime.agent.env.{}", key),
            )?
            else {
                continue;
            };
            let contract_path = stage_public_runtime_path_reference(
                &rel,
                exp_dir,
                package_dir,
                public_path_copies,
                staging_manifest_entries,
                &format!("trial_runtime.agent.env.{}", key),
            )?;
            items.insert(key, Value::String(contract_path));
        }
    }
    Ok(())
}

pub(crate) fn collect_command_staging_entries(
    command_root: Option<&Value>,
    field_name: &str,
    catalog: &BTreeMap<String, RuntimePathStagingManifestEntry>,
    seen: &mut HashSet<String>,
    entries: &mut Vec<RuntimePathStagingManifestEntry>,
) -> Result<()> {
    let Some(items) = command_root.and_then(Value::as_array) else {
        return Ok(());
    };
    for (idx, item) in items.iter().enumerate() {
        if idx == 0 {
            continue;
        }
        let Some(runtime_path) = item.as_str().map(str::trim) else {
            return Err(anyhow!("{}[{}] must be a string", field_name, idx));
        };
        if strip_task_workdir_support_destination_path(runtime_path).is_none() {
            continue;
        }
        if !seen.insert(runtime_path.to_string()) {
            continue;
        }
        let entry = lookup_runtime_staging_entry(catalog, runtime_path).ok_or_else(|| {
            anyhow!(
                "{}[{}] references packaged dependency '{}' with no staging manifest entry",
                field_name,
                idx,
                runtime_path
            )
        })?;
        entries.push(entry);
    }
    Ok(())
}

pub(crate) fn collect_runtime_command_env_staging_entries(
    experiment: &Value,
    catalog: &BTreeMap<String, RuntimePathStagingManifestEntry>,
) -> Result<Vec<RuntimePathStagingManifestEntry>> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    collect_command_staging_entries(
        experiment.pointer("/trial_runtime/agent/command"),
        "trial_runtime.agent.command",
        catalog,
        &mut seen,
        &mut entries,
    )?;
    if experiment
        .pointer("/trial_runtime/grader/strategy")
        .and_then(Value::as_str)
        .unwrap_or("none")
        == "in_task_runtime"
    {
        collect_command_staging_entries(
            experiment.pointer("/trial_runtime/grader/command"),
            "trial_runtime.grader.command",
            catalog,
            &mut seen,
            &mut entries,
        )?;
    }

    if let Some(items) = experiment
        .pointer("/trial_runtime/agent/env")
        .and_then(Value::as_object)
    {
        for (key, value) in items {
            let Some(runtime_path) = value.as_str().map(str::trim) else {
                return Err(anyhow!("trial_runtime.agent.env.{} must be a string", key));
            };
            if strip_task_workdir_support_destination_path(runtime_path).is_none() {
                continue;
            }
            if !seen.insert(runtime_path.to_string()) {
                continue;
            }
            let entry = lookup_runtime_staging_entry(catalog, runtime_path).ok_or_else(|| {
                anyhow!(
                    "trial_runtime.agent.env.{} references packaged dependency '{}' with no staging manifest entry",
                    key,
                    runtime_path
                )
            })?;
            entries.push(entry);
        }
    }

    Ok(entries)
}

pub(crate) fn lookup_runtime_staging_entry(
    catalog: &BTreeMap<String, RuntimePathStagingManifestEntry>,
    runtime_path: &str,
) -> Option<RuntimePathStagingManifestEntry> {
    if let Some(entry) = catalog.get(runtime_path) {
        return Some(entry.clone());
    }
    catalog
        .values()
        .filter(|entry| matches_contract_runtime_root(runtime_path, &entry.runtime_path))
        .max_by_key(|entry| entry.runtime_path.len())
        .cloned()
}

pub(crate) fn matches_contract_runtime_root(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(crate) fn collect_packaged_runtime_asset_entries(
    value: Option<&Value>,
    field_name: &str,
) -> Result<Vec<RuntimePathStagingManifestEntry>> {
    let Some(items) = value else {
        return Ok(Vec::new());
    };
    let arr = items
        .as_array()
        .ok_or_else(|| anyhow!("{} must be an array", field_name))?;
    let mut entries = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| anyhow!("{}[{}] must be an object", field_name, idx))?;
        let packaged_path = obj
            .get("packaged_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{}[{}].packaged_path is required", field_name, idx))?;
        let runtime_path = obj
            .get("runtime_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{}[{}].runtime_path is required", field_name, idx))?;
        entries.push(RuntimePathStagingManifestEntry {
            original_relative_path: packaged_path.to_string(),
            packaged_path: validate_public_authoring_relpath(
                packaged_path,
                &format!("{}[{}].packaged_path", field_name, idx),
            )?,
            runtime_path: validate_runner_staged_destination_path(
                runtime_path,
                &format!("{}[{}].runtime_path", field_name, idx),
            )?,
            required: obj.get("required").and_then(Value::as_bool).unwrap_or(true),
            read_only: obj
                .get("read_only")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        });
    }
    Ok(entries)
}

pub(crate) fn merge_runtime_path_staging_entries(
    base: &mut Vec<RuntimePathStagingManifestEntry>,
    extra: Vec<RuntimePathStagingManifestEntry>,
) {
    for next in extra {
        if let Some(existing) = base
            .iter_mut()
            .find(|entry| entry.runtime_path == next.runtime_path)
        {
            *existing = next;
        } else {
            base.push(next);
        }
    }
}

pub(crate) fn write_runtime_staging_manifest(
    package_dir: &Path,
    experiment: &Value,
    entries: &[RuntimePathStagingManifestEntry],
) -> Result<()> {
    let (variants, _) = resolve_variant_plan(experiment)?;
    let mut variants_manifest: BTreeMap<String, Vec<RuntimePathStagingManifestEntry>> =
        BTreeMap::new();
    for variant in &variants {
        let variant_experiment = resolve_runtime_for_variant(experiment, variant)?;
        let mut variant_catalog_entries = entries.to_vec();
        merge_runtime_path_staging_entries(
            &mut variant_catalog_entries,
            collect_packaged_runtime_asset_entries(
                variant_experiment.pointer("/trial_runtime/grader/_runtime_assets"),
                "trial_runtime.grader._runtime_assets",
            )?,
        );
        let variant_catalog = variant_catalog_entries
            .iter()
            .cloned()
            .map(|entry| (entry.runtime_path.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        let mut variant_entries =
            collect_runtime_command_env_staging_entries(&variant_experiment, &variant_catalog)?;
        merge_runtime_path_staging_entries(&mut variant_entries, variant_catalog_entries);
        variant_entries.sort_by(|left, right| {
            left.runtime_path
                .cmp(&right.runtime_path)
                .then(left.packaged_path.cmp(&right.packaged_path))
        });
        for (idx, entry) in variant_entries.iter().enumerate() {
            let packaged_source = resolve_package_path_under_root(
                package_dir,
                &entry.packaged_path,
                &format!(
                    "staging_manifest.variants.{}[{}].packaged_path",
                    variant.id, idx
                ),
            )?;
            fs::metadata(&packaged_source).with_context(|| {
                format!(
                    "failed to read packaged runtime staging source '{}' for variant '{}'",
                    packaged_source.display(),
                    variant.id
                )
            })?;
        }
        variants_manifest.insert(variant.id.clone(), variant_entries);
    }
    let manifest_value = serde_json::to_value(RuntimePathStagingManifest {
        schema_version: STAGING_MANIFEST_SCHEMA_VERSION.to_string(),
        variants: variants_manifest,
    })?;
    atomic_write_json_pretty(&package_dir.join(STAGING_MANIFEST_FILE), &manifest_value)
}

pub(crate) fn rewrite_trial_runtime_paths_for_package(
    trial_runtime_root: &mut Value,
    exp_dir: &Path,
    package_dir: &Path,
    artifact_copies: &mut BTreeMap<String, String>,
    file_copies: &mut BTreeMap<String, String>,
    public_path_copies: &mut BTreeMap<String, String>,
    staging_manifest_entries: &mut Vec<RuntimePathStagingManifestEntry>,
    artifact_counter: &mut usize,
    file_counter: &mut usize,
) -> Result<()> {
    if trial_runtime_root.pointer("/agent/mount").is_some()
        && !trial_runtime_root
            .pointer("/agent/mount")
            .is_some_and(Value::is_object)
    {
        return Err(anyhow!(
            "trial_runtime.agent.mount must be an object with source and mount"
        ));
    }
    if let Some(raw) = trial_runtime_root
        .pointer("/agent/mount/source")
        .and_then(Value::as_str)
    {
        let rel = stage_source_into_package(
            raw,
            exp_dir,
            package_dir,
            "agent_builds",
            "build",
            artifact_copies,
            artifact_counter,
        )?;
        set_json_pointer_value(
            trial_runtime_root,
            "/agent/mount/source",
            json!(rel.clone()),
        )?;
        set_json_pointer_value(trial_runtime_root, "/agent/mount/resolved_path", json!(rel))?;
    }
    if let Some(agent_root) = trial_runtime_root.pointer_mut("/agent") {
        stage_agent_command_env_path_refs_for_package(
            agent_root,
            exp_dir,
            package_dir,
            public_path_copies,
            staging_manifest_entries,
        )?;
    }
    if let Some(grader_root) = trial_runtime_root.pointer_mut("/grader") {
        rewrite_grader_paths_for_package(
            grader_root,
            exp_dir,
            package_dir,
            file_copies,
            file_counter,
            public_path_copies,
            staging_manifest_entries,
        )?;
    }
    Ok(())
}
