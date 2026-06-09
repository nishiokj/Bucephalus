use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use lab_core::{canonical_json_digest, ensure_dir, sha256_file};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
use crate::trial::spec::{parse_task_boundary_from_packaged_task, parse_task_row, TaskRow};
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

fn build_package_temp_out_path(out: &Path) -> PathBuf {
    let parent = out.parent().unwrap_or_else(|| Path::new("."));
    let name = out
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("package");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    parent.join(format!(
        ".{}.package-build-tmp.{}.{}",
        name,
        std::process::id(),
        nanos
    ))
}

fn ensure_final_build_output_ready(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(anyhow!(
                "build output target is a symlink\n\noutput_ref: {}\n\nRemove the symlink, choose an empty directory, or pass a different output path.",
                public_build_output_target_ref(path)
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(anyhow!(
                "build output target exists and is not a directory\n\noutput_ref: {}\n\nChoose an empty directory for the package output, remove the existing file, or pass a different output path.",
                public_build_output_target_ref(path)
            ));
        }
        Ok(_) => {
            let mut entries = fs::read_dir(path)?;
            if entries.next().is_some() {
                return Err(anyhow!(
                    "build output target directory must be empty\n\noutput_ref: {}\n\nMove or remove the existing contents, or pass a different output path.",
                    public_build_output_target_ref(path)
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(anyhow!(
                "failed to inspect build output target\n\noutput_ref: {}\n\nerror: {}",
                public_build_output_target_ref(path),
                err
            ));
        }
    }
    Ok(())
}

fn public_build_output_target_ref(_path: &Path) -> &'static str {
    "build-output://target"
}

fn public_build_output_temporary_ref(_path: &Path) -> &'static str {
    "build-output://temporary"
}

fn public_build_output_staged_ref(_path: &Path) -> &'static str {
    "build-output://staged"
}

fn build_result_for_package_dir(package_dir: PathBuf) -> BuildResult {
    BuildResult {
        manifest_path: package_dir.join("manifest.json"),
        checksums_path: package_dir.join("checksums.json"),
        package_checks_path: package_dir.join(PACKAGE_CHECKS_FILE),
        package_dir,
    }
}

fn cleanup_failed_build_output(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            eprintln!(
                "warning: failed to remove temporary build output {}: {}",
                public_build_output_temporary_ref(path),
                err
            );
        }
    }
}

fn publish_staged_build_output(staged: &Path, final_out: &Path) -> Result<BuildResult> {
    ensure_final_build_output_ready(final_out)?;
    if final_out.exists() {
        fs::remove_dir(final_out).with_context(|| {
            format!(
                "failed to replace empty build output directory {}",
                public_build_output_target_ref(final_out)
            )
        })?;
    }
    fs::rename(staged, final_out).with_context(|| {
        format!(
            "failed to publish staged build output {} to {}",
            public_build_output_staged_ref(staged),
            public_build_output_target_ref(final_out)
        )
    })?;
    Ok(build_result_for_package_dir(final_out.to_path_buf()))
}

pub(crate) fn as_portable_rel(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(crate) fn public_source_input_ref(_path: &Path) -> &'static str {
    "source://input"
}

pub(crate) fn public_runtime_asset_source_ref(source_root: &Path, path: &Path) -> String {
    let Ok(rel) = path.strip_prefix(source_root) else {
        return "source://outside-tree".to_string();
    };
    if rel.as_os_str().is_empty() {
        return "source://.".to_string();
    }
    format!("source://{}", as_portable_rel(rel))
}

pub(crate) fn public_package_output_ref(_path: &Path) -> &'static str {
    "package://output"
}

pub(crate) fn public_package_output_path_ref(package_dir: &Path, path: &Path) -> String {
    let Ok(rel) = path.strip_prefix(package_dir) else {
        return "package://output".to_string();
    };
    if rel.as_os_str().is_empty() {
        return "package://.".to_string();
    }
    format!("package://{}", as_portable_rel(rel))
}

fn public_ref_path_component(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "item".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn public_declared_path_ref(scheme: &str, raw: &str) -> String {
    let normalized = raw.trim().replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.starts_with('~')
        || normalized.contains("://")
        || normalized.contains(':')
    {
        return format!("{scheme}://input");
    }
    let parts = normalized
        .split('/')
        .filter_map(|part| match part {
            "" | "." => None,
            ".." => Some("parent".to_string()),
            value => Some(public_ref_path_component(value)),
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        format!("{scheme}://input")
    } else {
        format!("{scheme}://{}", parts.join("/"))
    }
}

pub(crate) fn public_relative_path_ref(scheme: &str, rel: &Path) -> String {
    let rel = as_portable_rel(rel);
    if rel.trim().is_empty() {
        return format!("{scheme}://.");
    }
    let parts = rel
        .split('/')
        .filter_map(|part| match part {
            "" | "." => None,
            ".." => Some("parent".to_string()),
            value => Some(public_ref_path_component(value)),
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        format!("{scheme}://.")
    } else {
        format!("{scheme}://{}", parts.join("/"))
    }
}

fn public_case_asset_declared_ref(raw_source: &str) -> String {
    public_declared_path_ref("case-asset", raw_source)
}

fn public_case_asset_resolved_ref(dataset_dir: &Path, resolved: &Path) -> String {
    resolved
        .strip_prefix(dataset_dir)
        .map(|rel| public_relative_path_ref("dataset", rel))
        .unwrap_or_else(|_| "case-asset://outside-dataset".to_string())
}

fn public_dataset_declared_ref(raw_source: &str) -> String {
    public_declared_path_ref("dataset", raw_source)
}

fn public_dataset_resolved_ref(_path: &Path) -> &'static str {
    "dataset://resolved"
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
        "package build expected a file or directory source\n\nsource_ref: {}\noutput_ref: {}",
        public_source_input_ref(source),
        public_package_output_ref(destination)
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
    let large_file_threshold = large_file_threshold_bytes()?;
    if source.is_file() {
        return copy_runtime_asset_file_into_package(
            source,
            destination,
            package_dir,
            large_file_threshold,
        );
    }
    if !source.is_dir() {
        return Err(anyhow!(
            "package build expected a runtime asset file or directory source\n\nsource_ref: {}\npackage_ref: {}",
            public_source_input_ref(source),
            public_package_output_path_ref(package_dir, destination)
        ));
    }
    let source_root = fs::canonicalize(source).with_context(|| {
        format!(
            "failed to canonicalize runtime asset source directory {}",
            public_source_input_ref(source)
        )
    })?;
    ensure_dir(destination)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(source).with_context(|| {
            format!(
                "runtime asset path {} escaped source root {}",
                public_runtime_asset_source_ref(source, path),
                public_runtime_asset_source_ref(source, source)
            )
        })?;
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
                    public_runtime_asset_source_ref(source, path),
                    public_runtime_asset_source_ref(source, source)
                )
            })?;
            if !resolved.starts_with(&source_root) {
                return Err(anyhow!(
                    "runtime asset symlink resolves outside its source tree\n\nsource_ref: {}\nsource_root_ref: {}\nresolved_ref: {}",
                    public_runtime_asset_source_ref(source, path),
                    public_runtime_asset_source_ref(source, source),
                    public_runtime_asset_source_ref(&source_root, &resolved)
                ));
            }
            let resolved_meta = fs::metadata(&resolved)?;
            if resolved_meta.is_dir() {
                return Err(anyhow!(
                    "runtime asset directory symlink is not supported\n\nsource_ref: {}\nresolved_ref: {}",
                    public_runtime_asset_source_ref(source, path),
                    public_runtime_asset_source_ref(&source_root, &resolved)
                ));
            }
            if !resolved_meta.is_file() {
                return Err(anyhow!(
                    "runtime asset symlink resolves to an unsupported file type\n\nsource_ref: {}\nresolved_ref: {}",
                    public_runtime_asset_source_ref(source, path),
                    public_runtime_asset_source_ref(&source_root, &resolved)
                ));
            }
            copy_runtime_asset_file_into_package(
                &resolved,
                &target,
                package_dir,
                large_file_threshold,
            )?;
        } else if entry.file_type().is_file() {
            copy_runtime_asset_file_into_package(path, &target, package_dir, large_file_threshold)?;
        }
    }
    Ok(())
}

fn copy_runtime_asset_file_into_package(
    source: &Path,
    destination: &Path,
    package_dir: &Path,
    large_file_threshold: u64,
) -> Result<()> {
    let meta = fs::metadata(source)?;
    if meta.len() >= large_file_threshold {
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

fn apply_case_image_rewrites(task: &mut Value, rules: &[TaskImageRewriteRule]) {
    let Some(workspace) = task.pointer("/resources/workspace") else {
        return;
    };
    let is_container_workspace = workspace
        .get("type")
        .or_else(|| workspace.get("kind"))
        .or_else(|| workspace.get("source"))
        .and_then(Value::as_str)
        == Some("container_image");
    if !is_container_workspace {
        return;
    }
    let workspace_platform_missing = workspace.get("platform").and_then(Value::as_str).is_none();
    let image_pointer = if workspace.get("image").and_then(Value::as_str).is_some() {
        "/resources/workspace/image"
    } else if task
        .pointer("/resources/environment/image")
        .and_then(Value::as_str)
        .is_some()
    {
        "/resources/environment/image"
    } else {
        return;
    };
    let Some(image) = task
        .pointer(image_pointer)
        .and_then(Value::as_str)
        .map(ToString::to_string)
    else {
        return;
    };
    for rule in rules {
        let Some(suffix) = image.strip_prefix(&rule.match_prefix) else {
            continue;
        };
        if let Some(slot) = task.pointer_mut(image_pointer) {
            *slot = Value::String(format!("{}{}", rule.replace_prefix, suffix));
        }
        if workspace_platform_missing {
            if let Some(platform) = rule.platform.clone() {
                task["resources"]["workspace"]["platform"] = Value::String(platform);
            }
        }
        break;
    }
}

const PACKAGED_TASK_ASSETS_DIR: &str = "tasks/assets";

fn stage_case_asset_into_package(
    raw_source: &str,
    kind: &str,
    dataset_dir: &Path,
    package_dir: &Path,
    copies: &mut BTreeMap<String, String>,
    counter: &mut usize,
) -> Result<String> {
    let raw_path = PathBuf::from(raw_source);
    let resolved = if raw_path.is_absolute() {
        normalize_path(&raw_path)
    } else {
        normalize_path(&dataset_dir.join(raw_path))
    };
    let key = resolved.to_string_lossy().to_string();
    if let Some(existing) = copies.get(&key) {
        return Ok(existing.clone());
    }
    let asset_ref = public_case_asset_declared_ref(raw_source);
    let resolved_ref = public_case_asset_resolved_ref(dataset_dir, &resolved);
    let meta = fs::metadata(&resolved).with_context(|| {
        format!(
            "package build failed to read case asset\n\nasset_ref: {asset_ref}\nresolved_ref: {resolved_ref}"
        )
    })?;
    if kind == "file" && !meta.is_file() {
        return Err(anyhow!(
            "case asset declares type=file but resolved source is not a file\n\nasset_ref: {}\nresolved_ref: {}",
            asset_ref,
            resolved_ref
        ));
    }
    if kind == "directory" && !meta.is_dir() {
        return Err(anyhow!(
            "case asset declares type=directory but resolved source is not a directory\n\nasset_ref: {}\nresolved_ref: {}",
            asset_ref,
            resolved_ref
        ));
    }
    let name = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            anyhow!("case asset must name a file or directory\n\nasset_ref: {asset_ref}")
        })?;
    let rel_path =
        PathBuf::from(PACKAGED_TASK_ASSETS_DIR).join(format!("{:03}_{}", *counter, name));
    let destination = package_dir.join(&rel_path);
    copy_runtime_asset_into_package(&resolved, &destination, package_dir)?;
    *counter += 1;
    let rel_portable = as_portable_rel(&rel_path);
    copies.insert(key, rel_portable.clone());
    Ok(rel_portable)
}

fn case_asset_kind(value: &Value) -> Option<&str> {
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

fn rewrite_case_input_assets_value(
    value: &mut Value,
    dataset_dir: &Path,
    package_dir: &Path,
    copies: &mut BTreeMap<String, String>,
    counter: &mut usize,
    context: &str,
) -> Result<()> {
    if let Some(items) = value.as_array_mut() {
        for (idx, item) in items.iter_mut().enumerate() {
            rewrite_case_input_assets_value(
                item,
                dataset_dir,
                package_dir,
                copies,
                counter,
                &format!("{}[{}]", context, idx),
            )?;
        }
        return Ok(());
    }
    let kind = case_asset_kind(value).map(str::to_string);
    let Some(obj) = value.as_object_mut() else {
        return Ok(());
    };
    if let Some(kind) = kind {
        if obj.get("package_path").and_then(Value::as_str).is_some()
            || obj
                .get("uri")
                .and_then(Value::as_str)
                .is_some_and(|uri| uri.starts_with("package://"))
        {
            return Ok(());
        }
        let raw_path = obj
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow!(
                    "{} declares {} input but does not provide a local authoring path",
                    context,
                    kind
                )
            })?;
        let packaged_rel = stage_case_asset_into_package(
            &raw_path,
            &kind,
            dataset_dir,
            package_dir,
            copies,
            counter,
        )
        .map_err(|err| {
            anyhow!(
                "failed to stage {} local asset into sealed package\n\nasset_ref: {}\n\n{}",
                context,
                public_case_asset_declared_ref(&raw_path),
                err
            )
        })?;
        obj.remove("path");
        obj.insert(
            "uri".to_string(),
            Value::String(format!("package://{}", packaged_rel)),
        );
        obj.insert("package_path".to_string(), Value::String(packaged_rel));
        return Ok(());
    }
    for (key, nested) in obj.iter_mut() {
        rewrite_case_input_assets_value(
            nested,
            dataset_dir,
            package_dir,
            copies,
            counter,
            &format!("{}.{}", context, key),
        )?;
    }
    Ok(())
}

fn rewrite_case_input_assets(
    task_case: &mut Value,
    dataset_dir: &Path,
    package_dir: &Path,
    copies: &mut BTreeMap<String, String>,
    counter: &mut usize,
    row_number: usize,
) -> Result<()> {
    let case_label = task_case
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("case '{}'", id))
        .unwrap_or_else(|| format!("case row {}", row_number));
    let Some(inputs) = task_case.get_mut("inputs") else {
        return Ok(());
    };
    rewrite_case_input_assets_value(
        inputs,
        dataset_dir,
        package_dir,
        copies,
        counter,
        &format!("{}.inputs", case_label),
    )
}

pub(crate) fn compile_tasks_for_package(
    tasks: &[Value],
    dataset_path: &Path,
    package_dir: &Path,
    experiment: &Value,
) -> Result<Vec<Value>> {
    let image_rewrites = load_task_image_rewrite_rules(experiment)?;
    let dataset_dir = dataset_path.parent().ok_or_else(|| {
        anyhow!(
            "dataset path has no parent\n\ndataset_ref: {}",
            public_dataset_resolved_ref(dataset_path)
        )
    })?;
    let mut case_asset_copies: BTreeMap<String, String> = BTreeMap::new();
    let mut case_asset_counter = 0usize;
    let mut compiled = Vec::with_capacity(tasks.len());
    for (idx, task) in tasks.iter().enumerate() {
        match task.get("schema_version").and_then(Value::as_str) {
            Some("task_row_v2") => {
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
            Some("case_v1") | Some("case_v2") | Some("task_case_v1") => {
                let mut task_case = task.clone();
                apply_case_image_rewrites(&mut task_case, &image_rewrites);
                rewrite_case_input_assets(
                    &mut task_case,
                    dataset_dir,
                    package_dir,
                    &mut case_asset_copies,
                    &mut case_asset_counter,
                    idx + 1,
                )?;
                parse_task_boundary_from_packaged_task(&task_case).with_context(|| {
                    format!(
                        "package build case {} is not a valid case row after image rewrite",
                        idx + 1
                    )
                })?;
                compiled.push(task_case);
            }
            _ => {
                parse_task_boundary_from_packaged_task(task).with_context(|| {
                    format!("package build case {} must be case_v1 or case_v2", idx + 1)
                })?;
            }
        }
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
    let limit = optional_json_usize_field(
        json_value.pointer("/matrix/tasks/limit"),
        "matrix.tasks.limit",
    )?;
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let dataset_ref = json_value
        .pointer("/matrix/tasks/path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let file = fs::File::open(path).with_context(|| match dataset_ref {
        Some(path_ref) => format!(
            "failed to open dataset file\n\ndataset_ref: {}\nconfigured_ref: {}",
            public_dataset_resolved_ref(path),
            public_dataset_declared_ref(path_ref)
        ),
        None => format!(
            "failed to open dataset file\n\ndataset_ref: {}",
            public_dataset_resolved_ref(path)
        ),
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
        let case_label = task
            .pointer("/task/id")
            .or_else(|| task.pointer("/id"))
            .and_then(Value::as_str)
            .map(|id| format!("case '{}'", id))
            .unwrap_or_else(|| format!("row {}", idx + 1));
        if let Err(err) = parse_task_boundary_from_packaged_task(&task) {
            return Err(anyhow!(
                "dataset {} is not a valid case_v1 or case_v2: {}",
                case_label,
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
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("missing required /experiment/id after validation"))?;
    let final_package_dir = if let Some(out_dir) = out_dir {
        out_dir.to_path_buf()
    } else {
        let ts = Utc::now().format("%Y%m%d_%H%M%S_%6f");
        crate::local_storage::default_build_root()?.join(format!(
            "{}_{}",
            sanitize_name_for_path(experiment_id),
            ts
        ))
    };
    ensure_final_build_output_ready(&final_package_dir)?;
    let stage_for_publish = out_dir.is_some();
    let package_dir = if stage_for_publish {
        build_package_temp_out_path(&final_package_dir)
    } else {
        final_package_dir.clone()
    };
    if stage_for_publish && package_dir.exists() {
        return Err(anyhow!(
            "temporary build output path already exists\n\noutput_ref: {}\n\nRetry the build, or remove the stale temporary output if the problem persists.",
            public_build_output_temporary_ref(&package_dir)
        ));
    }

    let build_result = (|| -> Result<BuildResult> {
        ensure_dir(&package_dir)?;

        ensure_dir(&package_dir.join("agent_builds"))?;
        ensure_dir(&package_dir.join("tasks"))?;
        ensure_dir(&package_dir.join("files"))?;
        ensure_dir(&package_dir.join(PACKAGE_BLOBS_DIR))?;
        ensure_dir(&package_dir.join(PACKAGED_RUNTIME_ASSETS_DIR))?;
        ensure_dir(&package_dir.join(HOST_GRADER_CAPABILITIES_DIR))?;

        let dataset_path = resolve_dataset_path(&json_value, &loaded.exp_dir)?;
        let dataset_target = package_dir.join("tasks").join("tasks.jsonl");
        let raw_tasks = load_task_rows_for_build(&dataset_path, &json_value)?;
        let packaged_tasks =
            compile_tasks_for_package(&raw_tasks, &dataset_path, &package_dir, &json_value)?;
        write_packaged_tasks(&dataset_target, &packaged_tasks)?;
        let dataset_rel = PathBuf::from("tasks").join("tasks.jsonl");
        set_json_pointer_value(
            &mut json_value,
            "/matrix/tasks/path",
            json!(as_portable_rel(&dataset_rel)),
        )?;

        let mut runtime_path_rewrite =
            RuntimePathRewriteContext::new(&loaded.exp_dir, &package_dir);

        if let Some(trial_runtime) = json_value.pointer_mut("/trial_runtime") {
            rewrite_trial_runtime_paths_for_package(trial_runtime, &mut runtime_path_rewrite)?;
        }
        if let Some(variants) = json_value
            .pointer_mut("/matrix/variants")
            .and_then(Value::as_array_mut)
        {
            for variant in variants.iter_mut() {
                if let Some(runtime_overrides) = variant.get_mut("overrides") {
                    rewrite_trial_runtime_paths_for_package(
                        runtime_overrides,
                        &mut runtime_path_rewrite,
                    )?;
                }
            }
        }
        write_runtime_staging_manifest(
            &package_dir,
            &json_value,
            &runtime_path_rewrite.staging_manifest_entries,
        )?;
        strip_packaging_only_trial_runtime_catalogs(&mut json_value);
        strip_public_authoring_aliases_from_resolved_package(&mut json_value);
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
        for entry in walkdir::WalkDir::new(&package_dir) {
            let entry = entry?;
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
                .with_context(|| {
                    format!(
                        "package entry {} escaped root {}",
                        public_build_output_staged_ref(path),
                        public_build_output_staged_ref(&package_dir)
                    )
                })?;
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

        Ok(build_result_for_package_dir(package_dir.clone()))
    })();

    let build = match build_result {
        Ok(build) => build,
        Err(err) => {
            cleanup_failed_build_output(&package_dir);
            return Err(err);
        }
    };

    if stage_for_publish {
        match publish_staged_build_output(&build.package_dir, &final_package_dir) {
            Ok(published) => Ok(published),
            Err(err) => {
                cleanup_failed_build_output(&build.package_dir);
                Err(err)
            }
        }
    } else {
        Ok(build)
    }
}

fn strip_public_authoring_aliases_from_resolved_package(json_value: &mut Value) {
    if let Some(object) = json_value.as_object_mut() {
        object.remove("stages");
        object.remove("cases");
        object.remove("ephemerals");
        object.remove("externals");
    }
    if let Some(matrix) = json_value
        .pointer_mut("/matrix")
        .and_then(Value::as_object_mut)
    {
        matrix.remove("cases");
    }
}
