use anyhow::{anyhow, Context, Result};
use lab_core::{sha256_bytes, sha256_file, AGENTLAB_TASK_WORKDIR_PLACEHOLDER};
use serde_json::Value;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::config::{apply_experiment_overrides, find_project_root, normalize_path};
use crate::model::LoadedExperimentInput;
use crate::package::cas::should_include_agent_artifact_path;

pub(crate) fn load_authoring_input_for_build(
    path: &Path,
    overrides_path: Option<&Path>,
) -> Result<LoadedExperimentInput> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if canonical.is_dir() {
        return Err(anyhow!(
            "build_input_invalid_kind: expected v1 experiment YAML file, got directory '{}'",
            canonical.display()
        ));
    }

    if canonical
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "manifest.json")
    {
        return Err(anyhow!(
            "build_input_invalid_kind: expected v1 experiment YAML file, got sealed package manifest"
        ));
    }

    let exp_dir = canonical
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));
    let project_root = find_project_root(&exp_dir)
        .canonicalize()
        .unwrap_or_else(|_| find_project_root(&exp_dir));
    let raw_yaml = fs::read_to_string(&canonical)?;
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&raw_yaml)?;
    let json_value: Value = serde_json::to_value(yaml_value)?;
    let json_value = if let Some(overrides_path) = overrides_path {
        apply_experiment_overrides(json_value, overrides_path, &project_root)?
    } else {
        json_value
    };
    reject_legacy_authoring_surface(&json_value)?;
    Ok(LoadedExperimentInput {
        json_value,
        exp_dir,
        project_root,
    })
}

fn reject_legacy_authoring_surface(json_value: &Value) -> Result<()> {
    for (pointer, replacement) in [
        ("/agent", "/trial_runtime/agent"),
        ("/agent_builds", "/matrix/variants[].overrides"),
        ("/baseline", "/matrix/variants[] with baseline: true"),
        ("/variant_plan", "/matrix/variants"),
        ("/variants", "/matrix/variants"),
        (
            "/overrides",
            "first-class v1 fields under /runtime, /policy, or /matrix",
        ),
    ] {
        if json_value.pointer(pointer).is_some() {
            return Err(anyhow!(
                "{} is legacy authoring syntax and is not accepted; write the v1 noun-model YAML directly using {}",
                pointer,
                replacement
            ));
        }
    }
    if matches!(json_value.pointer("/benchmark"), Some(Value::String(_))) {
        return Err(anyhow!(
            "/benchmark as a string is legacy authoring syntax and is not accepted; write explicit v1 trial_runtime, matrix.tasks, metrics, and policy fields"
        ));
    }
    Ok(())
}

pub(crate) fn resolve_agent_artifact_path(
    raw: &str,
    exp_dir: &Path,
    project_root: &Path,
) -> PathBuf {
    let trimmed = raw.trim();
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return normalize_path(candidate);
    }
    if trimmed.starts_with("./") || trimmed.starts_with("../") || trimmed.contains('/') {
        return normalize_path(&exp_dir.join(candidate));
    }

    let agents_root = project_root.join(".lab").join("agents");
    let direct = agents_root.join(trimmed);
    if direct.exists() {
        return normalize_path(&direct);
    }
    for ext in [".tar.gz", ".tgz", ".tar"] {
        let with_ext = agents_root.join(format!("{}{}", trimmed, ext));
        if with_ext.exists() {
            return normalize_path(&with_ext);
        }
    }
    normalize_path(&direct)
}

pub(crate) fn compute_artifact_content_digest(path: &Path) -> Result<String> {
    if path.is_file() {
        return sha256_file(path);
    }
    if !path.is_dir() {
        return Err(anyhow!(
            "artifact path must be a file or directory: {}",
            path.display()
        ));
    }

    let mut lines = Vec::new();
    for entry in walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let p = entry.path();
        if p == path {
            continue;
        }
        if !should_include_agent_artifact_path(path, p) {
            continue;
        }
        let rel = p
            .strip_prefix(path)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        let meta = fs::symlink_metadata(p)?;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(p)
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_else(|_| "<unreadable>".to_string());
            lines.push(format!("L {} -> {}", rel, target));
        } else if meta.is_dir() {
            lines.push(format!("D {}", rel));
        } else if meta.is_file() {
            lines.push(format!("F {} {}", rel, sha256_file(p)?));
        }
    }
    lines.sort();
    Ok(sha256_bytes(lines.join("\n").as_bytes()))
}

pub(crate) fn contains_removed_runtime_template(raw: &str) -> bool {
    raw.contains("${")
}

pub(crate) fn resolve_existing_public_path_reference(
    raw: &str,
    exp_dir: &Path,
    field_name: &str,
) -> Result<Option<PathBuf>> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('-')
        || trimmed.starts_with(AGENTLAB_TASK_WORKDIR_PLACEHOLDER)
        || trimmed.contains('$')
        || trimmed.contains("://")
    {
        return Ok(None);
    }
    let rel = validate_public_authoring_relpath(trimmed, field_name)?;
    let resolved = normalize_path(&exp_dir.join(&rel));
    match fs::metadata(&resolved) {
        Ok(_) => Ok(Some(PathBuf::from(rel))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if trimmed.starts_with("./") || trimmed.contains('/') {
                return Err(anyhow!(
                    "{} public path '{}' resolved to missing source '{}'",
                    field_name,
                    trimmed,
                    resolved.display()
                ));
            }
            Ok(None)
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to read {} public path reference '{}' resolved to '{}'",
                field_name,
                trimmed,
                resolved.display()
            )
        }),
    }
}

pub(crate) fn validate_public_authoring_relpath(raw: &str, field_name: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{} must not be empty", field_name));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(anyhow!("{} must be relative", field_name));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(seg) => normalized.push(seg),
            Component::ParentDir => {
                return Err(anyhow!("{} cannot contain '..'", field_name));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("{} must be relative", field_name));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(anyhow!("{} cannot resolve to empty", field_name));
    }
    Ok(normalized.to_string_lossy().replace('\\', "/"))
}
