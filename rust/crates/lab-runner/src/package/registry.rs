use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::config::{canonicalize_best_effort, find_project_root, normalize_path};

const GRADER_CAPABILITY_REGISTRY_DIR: &str = "manifests/grader_capabilities";
const CAPABILITY_FILENAMES: &[&str] = &["capability.yaml", "capability.yml", "capability.json"];

#[derive(Debug, Clone)]
pub(crate) struct GraderCapabilityManifest {
    pub(crate) value: Value,
    pub(crate) registry_root: PathBuf,
}

fn runner_repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("failed to locate repository root from CARGO_MANIFEST_DIR"))
}

fn manifest_value(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read manifest {}", path.display()))?;
    match path.extension().and_then(|value| value.to_str()) {
        Some("json") => serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse JSON manifest {}", path.display())),
        _ => {
            let yaml: serde_yaml::Value = serde_yaml::from_str(&raw)
                .with_context(|| format!("failed to parse YAML manifest {}", path.display()))?;
            serde_json::to_value(yaml)
                .with_context(|| format!("failed to convert YAML manifest {}", path.display()))
        }
    }
}

fn registry_manifest_paths(
    root: &Path,
    registry_dir: &str,
    filenames: &[&str],
) -> Result<Vec<PathBuf>> {
    let dir = root.join(registry_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(&dir)
        .with_context(|| format!("failed to read registry directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            for filename in filenames {
                let candidate = path.join(filename);
                if candidate.is_file() {
                    paths.push(candidate);
                    break;
                }
            }
        } else if path.is_file()
            && path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| filenames.contains(&name))
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn required_string<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| anyhow!("{} missing non-empty {}", context, pointer))
}

fn string_array(value: Option<&Value>, context: &str) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("{} must be an array", context))?;
    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            item.as_str()
                .map(str::trim)
                .filter(|raw| !raw.is_empty())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("{}[{}] must be a non-empty string", context, idx))
        })
        .collect()
}

fn validate_relative_path(raw: &str, context: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{} must not be empty", context));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(anyhow!("{} must be relative", context));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => return Err(anyhow!("{} must not contain '..'", context)),
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("{} must be relative", context))
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(anyhow!("{} cannot resolve to empty", context));
    }
    Ok(normalized.to_string_lossy().replace('\\', "/"))
}

pub(crate) fn load_grader_capability_manifest(
    project_root: &Path,
    capability_id: &str,
) -> Result<GraderCapabilityManifest> {
    let capability_id = capability_id.trim();
    if capability_id.is_empty() {
        return Err(anyhow!("grader capability id must not be empty"));
    }
    let mut matches = Vec::new();
    for root in [project_root.to_path_buf(), runner_repo_root()?] {
        for path in
            registry_manifest_paths(&root, GRADER_CAPABILITY_REGISTRY_DIR, CAPABILITY_FILENAMES)?
        {
            let value = manifest_value(&path)?;
            let id = required_string(
                &value,
                "/id",
                &format!("grader capability manifest {}", path.display()),
            )?;
            if id == capability_id {
                matches.push((value, root.clone()));
            }
        }
    }
    matches.dedup();
    match matches.len() {
        1 => Ok(GraderCapabilityManifest {
            value: matches[0].0.clone(),
            registry_root: matches[0].1.clone(),
        }),
        0 => Err(anyhow!(
            "unknown host grader capability '{}': no capability manifest found under {}",
            capability_id,
            GRADER_CAPABILITY_REGISTRY_DIR
        )),
        _ => Err(anyhow!(
            "host grader capability '{}' resolved to multiple manifests; registry ids must be unique",
            capability_id
        )),
    }
}

pub(crate) fn load_grader_capability_for_exp_dir(
    exp_dir: &Path,
    capability_id: &str,
) -> Result<GraderCapabilityManifest> {
    let root = find_project_root(exp_dir);
    load_grader_capability_manifest(&root, capability_id)
}

impl GraderCapabilityManifest {
    pub(crate) fn resolve_host_path(&self, relative_path: &str) -> Result<PathBuf> {
        let runtime_kind = required_string(
            &self.value,
            "/runtime/kind",
            &format!("grader capability '{}'", self.id()),
        )?;
        if runtime_kind != "host" {
            return Err(anyhow!(
                "grader capability '{}' is not a host capability",
                self.id()
            ));
        }
        let rel = validate_relative_path(relative_path, "grader capability path")?;
        let allowed = string_array(
            self.value.pointer("/allowed_paths"),
            "grader_capability.allowed_paths",
        )?;
        if allowed.is_empty() {
            return Err(anyhow!(
                "grader capability '{}' must declare at least one allowed path",
                self.id()
            ));
        }
        if !allowed.is_empty() && !allowed.iter().any(|candidate| candidate == &rel) {
            return Err(anyhow!(
                "grader capability '{}' does not allow path '{}'",
                self.id(),
                rel
            ));
        }
        let root_raw = required_string(
            &self.value,
            "/root",
            &format!("grader capability '{}'", self.id()),
        )?;
        let root = Path::new(root_raw);
        let root_path = if root.is_absolute() {
            normalize_path(root)
        } else {
            normalize_path(&self.registry_root.join(root))
        };
        let resolved = normalize_path(&root_path.join(&rel));
        let root_cmp = canonicalize_best_effort(&root_path);
        let resolved_cmp = canonicalize_best_effort(&resolved);
        if !resolved_cmp.starts_with(&root_cmp) {
            return Err(anyhow!(
                "grader capability '{}' path '{}' resolves outside capability root {}",
                self.id(),
                rel,
                root_path.display()
            ));
        }
        fs::metadata(&resolved).with_context(|| {
            format!(
                "failed to read grader capability '{}' path '{}'",
                self.id(),
                resolved.display()
            )
        })?;
        Ok(resolved)
    }

    pub(crate) fn id(&self) -> String {
        self.value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string()
    }
}
