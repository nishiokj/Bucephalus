use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use lab_core::{canonical_json_digest, ensure_dir, sha256_file};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::config::*;
use crate::experiment::preflight::resolve_dataset_path;
use crate::experiment::runner::*;
use crate::model::*;
use crate::package::authoring::*;
use crate::package::cas::{
    agent_directory_artifact_excludes, large_file_threshold_bytes, put_file_in_package_cas,
    write_cas_pointer, PACKAGE_BLOBS_DIR,
};
use crate::package::checks::{write_package_checks, PACKAGE_CHECKS_FILE};
use crate::package::staging::*;
use crate::package::validate::*;
use crate::trial::spec::{parse_task_row, TaskRow};
use crate::util::copy_dir_preserve_all;

pub(crate) fn sanitize_name_for_path(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "experiment".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn as_portable_rel(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(crate) fn copy_path_into_package(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        ensure_dir(destination)?;
        return copy_dir_preserve_all(source, destination, &[]);
    }
    if source.is_file() {
        if let Some(parent) = destination.parent() {
            ensure_dir(parent)?;
        }
        fs::copy(source, destination)?;
        return Ok(());
    }
    Err(anyhow!(
        "package build expected file or directory source, got: {}",
        source.display()
    ))
}

pub(crate) fn copy_agent_artifact_into_package(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        ensure_dir(destination)?;
        return copy_dir_preserve_all(source, destination, agent_directory_artifact_excludes());
    }
    copy_path_into_package(source, destination)
}

pub(crate) fn copy_runtime_asset_into_package(
    source: &Path,
    destination: &Path,
    package_dir: &Path,
) -> Result<()> {
    if source.is_file() {
        return copy_runtime_asset_file_into_package(source, destination, package_dir);
    }
    if !source.is_dir() {
        return Err(anyhow!(
            "package build expected runtime asset file or directory source, got: {}",
            source.display()
        ));
    }
    let source_root = fs::canonicalize(source).with_context(|| {
        format!(
            "failed to canonicalize runtime asset source directory {}",
            source.display()
        )
    })?;
    ensure_dir(destination)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(source).unwrap_or(path);
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(rel);
        if entry.file_type().is_dir() {
            ensure_dir(&target)?;
        } else if entry.file_type().is_symlink() {
            let resolved = fs::canonicalize(path).with_context(|| {
                format!(
                    "runtime asset symlink {} must resolve inside source tree {}",
                    path.display(),
                    source.display()
                )
            })?;
            if !resolved.starts_with(&source_root) {
                return Err(anyhow!(
                    "runtime asset symlink {} resolves outside source tree {}: {}",
                    path.display(),
                    source.display(),
                    resolved.display()
                ));
            }
            let resolved_meta = fs::metadata(&resolved)?;
            if resolved_meta.is_dir() {
                return Err(anyhow!(
                    "runtime asset directory symlink is not supported: {} -> {}",
                    path.display(),
                    resolved.display()
                ));
            }
            if !resolved_meta.is_file() {
                return Err(anyhow!(
                    "runtime asset symlink {} resolves to unsupported file type: {}",
                    path.display(),
                    resolved.display()
                ));
            }
            copy_runtime_asset_file_into_package(&resolved, &target, package_dir)?;
        } else if entry.file_type().is_file() {
            copy_runtime_asset_file_into_package(path, &target, package_dir)?;
        }
    }
    Ok(())
}

fn copy_runtime_asset_file_into_package(
    source: &Path,
    destination: &Path,
    package_dir: &Path,
) -> Result<()> {
    let meta = fs::metadata(source)?;
    if meta.len() >= large_file_threshold_bytes() {
        let (digest, _) = put_file_in_package_cas(package_dir, source)?;
        write_cas_pointer(destination, digest, meta.len())?;
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        ensure_dir(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

#[derive(Debug, Clone)]
struct TaskImageRewriteRule {
    match_prefix: String,
    replace_prefix: String,
    platform: Option<String>,
}

fn load_task_image_rewrite_rules(json_value: &Value) -> Result<Vec<TaskImageRewriteRule>> {
    let Some(items) = json_value
        .pointer("/trial_runtime/task/workspace/image/rewrites")
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    let mut rules = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let context = format!("trial_runtime.task.workspace.image.rewrites[{}]", idx);
        let match_prefix = item
            .get("match_prefix")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("{}.match_prefix must be a non-empty string", context))?;
        let replace_prefix = item
            .get("replace_prefix")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("{}.replace_prefix must be a non-empty string", context))?;
        let platform = item
            .get("platform")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        rules.push(TaskImageRewriteRule {
            match_prefix: match_prefix.to_string(),
            replace_prefix: replace_prefix.to_string(),
            platform,
        });
    }
    Ok(rules)
}

fn apply_task_image_rewrites(task_row: &mut TaskRow, rules: &[TaskImageRewriteRule]) {
    let Some(container) = task_row.runtime.container_image.as_mut() else {
        return;
    };
    for rule in rules {
        let Some(suffix) = container.image.strip_prefix(&rule.match_prefix) else {
            continue;
        };
        container.image = format!("{}{}", rule.replace_prefix, suffix);
        if container.platform.is_none() {
            container.platform = rule.platform.clone();
        }
        break;
    }
}

pub(crate) fn compile_tasks_for_package(
    tasks: &[Value],
    _project_root: &Path,
    exp_dir: &Path,
    dataset_path: &Path,
    package_dir: &Path,
    experiment: &Value,
) -> Result<Vec<Value>> {
    let _ = (dataset_path, exp_dir, package_dir);
    let image_rewrites = load_task_image_rewrite_rules(experiment)?;
    let mut compiled = Vec::with_capacity(tasks.len());
    for (idx, task) in tasks.iter().enumerate() {
        let mut task_row = parse_task_row(task).with_context(|| {
            format!("package build task {} is not a valid task_row_v2", idx + 1)
        })?;
        apply_task_image_rewrites(&mut task_row, &image_rewrites);
        crate::trial::spec::validate_task_row(&task_row).with_context(|| {
            format!(
                "package build task {} is not a valid task_row_v2 after image rewrite",
                idx + 1
            )
        })?;
        compiled.push(serde_json::to_value(task_row)?);
    }
    Ok(compiled)
}

pub(crate) fn write_packaged_tasks(path: &Path, tasks: &[Value]) -> Result<()> {
    let mut bytes = Vec::new();
    for task in tasks {
        serde_json::to_writer(&mut bytes, task)?;
        bytes.push(b'\n');
    }
    atomic_write_bytes(path, &bytes)
}

pub(crate) fn load_task_rows_for_build(path: &Path, json_value: &Value) -> Result<Vec<Value>> {
    validate_dataset_provider(json_value)?;
    let limit = json_value
        .pointer("/matrix/tasks/limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let dataset_ref = json_value
        .pointer("/matrix/tasks/path")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    let dataset_suite = json_value
        .pointer("/matrix/tasks/suite_id")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let file = fs::File::open(path).with_context(|| {
        format!(
            "failed to open dataset file '{}' (resolved from matrix.tasks.path='{}', matrix.tasks.suite_id='{}')",
            path.display(),
            dataset_ref,
            dataset_suite
        )
    })?;
    let reader = BufReader::new(file);
    let mut tasks = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if limit.is_some_and(|max| tasks.len() >= max) {
            break;
        }
        let task: Value = serde_json::from_str(trimmed)?;
        let task_id = task
            .pointer("/task/id")
            .or_else(|| task.pointer("/id"))
            .and_then(Value::as_str)
            .unwrap_or("<unknown_task>");
        if let Err(err) = parse_task_row(&task) {
            return Err(anyhow!(
                "dataset row {} task '{}' is not a valid task_row_v2: {}",
                idx + 1,
                task_id,
                err
            ));
        }
        tasks.push(task);
    }
    Ok(tasks)
}

fn strip_grader_runtime_asset_catalog(trial_runtime_root: &mut Value) {
    if let Some(grader) = trial_runtime_root
        .pointer_mut("/grader")
        .and_then(Value::as_object_mut)
    {
        grader.remove("_runtime_assets");
    }
}

fn strip_task_image_rewrite_catalog(trial_runtime_root: &mut Value) {
    if let Some(image) = trial_runtime_root
        .pointer_mut("/task/workspace/image")
        .and_then(Value::as_object_mut)
    {
        image.remove("rewrites");
    }
}

fn strip_packaging_only_trial_runtime_fields(trial_runtime_root: &mut Value) {
    strip_grader_runtime_asset_catalog(trial_runtime_root);
    strip_task_image_rewrite_catalog(trial_runtime_root);
}

fn strip_packaging_only_trial_runtime_catalogs(experiment: &mut Value) {
    if let Some(trial_runtime) = experiment.pointer_mut("/trial_runtime") {
        strip_packaging_only_trial_runtime_fields(trial_runtime);
    }
    if let Some(variants) = experiment
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    {
        for variant in variants {
            if let Some(runtime_overrides) = variant.get_mut("overrides") {
                strip_packaging_only_trial_runtime_fields(runtime_overrides);
            }
        }
    }
}

pub fn build_experiment_package(
    path: &Path,
    overrides_path: Option<&Path>,
    out_dir: Option<&Path>,
) -> Result<BuildResult> {
    let loaded = load_authoring_input_for_build(path, overrides_path)?;
    let mut json_value = loaded.json_value.clone();
    let mut contract_validation_value = json_value.clone();
    strip_packaging_only_trial_runtime_catalogs(&mut contract_validation_value);
    validate_required_fields(&contract_validation_value)?;

    let experiment_id = json_value
        .pointer("/experiment/id")
        .and_then(Value::as_str)
        .unwrap_or("experiment");
    let package_dir = if let Some(out_dir) = out_dir {
        out_dir.to_path_buf()
    } else {
        let ts = Utc::now().format("%Y%m%d_%H%M%S_%6f");
        loaded
            .project_root
            .join(".lab")
            .join("builds")
            .join(format!("{}_{}", sanitize_name_for_path(experiment_id), ts))
    };
    if package_dir.exists() {
        if !package_dir.is_dir() {
            return Err(anyhow!(
                "build output path exists and is not a directory: {}",
                package_dir.display()
            ));
        }
        let mut entries = fs::read_dir(&package_dir)?;
        if entries.next().is_some() {
            return Err(anyhow!(
                "build output directory must be empty: {}",
                package_dir.display()
            ));
        }
    } else {
        ensure_dir(&package_dir)?;
    }

    ensure_dir(&package_dir.join("agent_builds"))?;
    ensure_dir(&package_dir.join("tasks"))?;
    ensure_dir(&package_dir.join("files"))?;
    ensure_dir(&package_dir.join(PACKAGE_BLOBS_DIR))?;
    ensure_dir(&package_dir.join(PACKAGED_RUNTIME_ASSETS_DIR))?;
    ensure_dir(&package_dir.join(HOST_GRADER_CAPABILITIES_DIR))?;

    let dataset_path = resolve_dataset_path(&json_value, &loaded.exp_dir)?;
    let dataset_target = package_dir.join("tasks").join("tasks.jsonl");
    let raw_tasks = load_task_rows_for_build(&dataset_path, &json_value)?;
    let packaged_tasks = compile_tasks_for_package(
        &raw_tasks,
        &loaded.project_root,
        &loaded.exp_dir,
        &dataset_path,
        &package_dir,
        &json_value,
    )?;
    write_packaged_tasks(&dataset_target, &packaged_tasks)?;
    let dataset_rel = PathBuf::from("tasks").join("tasks.jsonl");
    set_json_pointer_value(
        &mut json_value,
        "/matrix/tasks/path",
        json!(as_portable_rel(&dataset_rel)),
    )?;

    let mut artifact_copies: BTreeMap<String, String> = BTreeMap::new();
    let mut file_copies: BTreeMap<String, String> = BTreeMap::new();
    let mut public_path_copies: BTreeMap<String, String> = BTreeMap::new();
    let mut staging_manifest_entries = Vec::new();
    let mut artifact_counter = 0usize;
    let mut file_counter = 0usize;

    if let Some(trial_runtime) = json_value.pointer_mut("/trial_runtime") {
        rewrite_trial_runtime_paths_for_package(
            trial_runtime,
            &loaded.exp_dir,
            &package_dir,
            &mut artifact_copies,
            &mut file_copies,
            &mut public_path_copies,
            &mut staging_manifest_entries,
            &mut artifact_counter,
            &mut file_counter,
        )?;
    }
    if let Some(variants) = json_value
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    {
        for variant in variants.iter_mut() {
            if let Some(runtime_overrides) = variant.get_mut("overrides") {
                rewrite_trial_runtime_paths_for_package(
                    runtime_overrides,
                    &loaded.exp_dir,
                    &package_dir,
                    &mut artifact_copies,
                    &mut file_copies,
                    &mut public_path_copies,
                    &mut staging_manifest_entries,
                    &mut artifact_counter,
                    &mut file_counter,
                )?;
            }
        }
    }
    write_runtime_staging_manifest(&package_dir, &json_value, &staging_manifest_entries)?;
    strip_packaging_only_trial_runtime_catalogs(&mut json_value);
    validate_packaged_runtime_artifacts(&package_dir, &json_value)?;

    let resolved_for_manifest = json_value.clone();
    atomic_write_json_pretty(
        &package_dir.join("resolved_experiment.json"),
        &resolved_for_manifest,
    )?;

    let manifest_path = package_dir.join("manifest.json");
    let checksums_path = package_dir.join("checksums.json");
    let lock_path = package_dir.join("package.lock");
    let mut checksums: BTreeMap<String, String> = BTreeMap::new();
    for entry in walkdir::WalkDir::new(&package_dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        if path == checksums_path || path == manifest_path || path == lock_path {
            continue;
        }
        let rel = path
            .strip_prefix(&package_dir)
            .map(as_portable_rel)
            .unwrap_or_else(|_| path.display().to_string());
        checksums.insert(rel, sha256_file(path)?);
    }
    let checksums_value = json!({
        "schema_version": "sealed_package_checksums_v2",
        "files": checksums,
    });
    atomic_write_json_pretty(&checksums_path, &checksums_value)?;
    let package_digest = canonical_json_digest(
        checksums_value
            .pointer("/files")
            .ok_or_else(|| anyhow!("build failed to materialize checksums files map"))?,
    );
    let package_checks_path = package_dir.join(PACKAGE_CHECKS_FILE);
    atomic_write_json_pretty(
        &lock_path,
        &json!({
            "schema_version": "sealed_package_lock_v1",
            "package_digest": package_digest.clone(),
        }),
    )?;
    write_package_checks(
        &package_dir,
        &resolved_for_manifest,
        &packaged_tasks,
        &package_digest,
    )?;
    let package_manifest = json!({
        "schema_version": "sealed_run_package_v2",
        "created_at": Utc::now().to_rfc3339(),
        "resolved_experiment": resolved_for_manifest,
        "checksums_ref": "checksums.json",
        "package_checks_ref": PACKAGE_CHECKS_FILE,
        "package_digest": package_digest,
    });
    atomic_write_json_pretty(&manifest_path, &package_manifest)?;

    Ok(BuildResult {
        package_dir,
        manifest_path,
        checksums_path,
        package_checks_path,
    })
}
