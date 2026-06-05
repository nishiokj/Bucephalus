use anyhow::{anyhow, Context, Result};
use lab_core::{canonical_json_digest, sha256_file};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::*;
use crate::model::STAGING_MANIFEST_FILE;
use crate::model::*;
use crate::package::cas::{package_blob_path_for_digest, read_cas_pointer, PACKAGE_BLOBS_DIR};
use crate::package::checks::{PACKAGE_CHECKS_FILE, PACKAGE_CHECKS_SCHEMA_VERSION};
use crate::package::compile::as_portable_rel;

struct VerifiedPackageIntegrity {
    resolved_experiment: Value,
    checksums: Value,
}

pub(crate) fn resolve_package_path_under_root(
    package_dir: &Path,
    rel_path: &str,
    field_name: &str,
) -> Result<PathBuf> {
    let trimmed = rel_path.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{} must be a non-empty relative path", field_name));
    }
    if Path::new(trimmed).is_absolute() {
        return Err(anyhow!("{} must be relative to package root", field_name));
    }
    let resolved = normalize_path(&package_dir.join(trimmed));
    let root = canonicalize_best_effort(package_dir);
    let resolved_cmp = canonicalize_best_effort(&resolved);
    if !resolved_cmp.starts_with(&root) {
        return Err(anyhow!(
            "{} escapes package root: '{}' (root: {})",
            field_name,
            rel_path,
            root.display()
        ));
    }
    Ok(resolved)
}

fn require_sealed_manifest_keys(manifest: &Value) -> Result<()> {
    let obj = manifest
        .as_object()
        .ok_or_else(|| anyhow!("sealed package manifest must be an object"))?;
    let allowed = [
        "schema_version",
        "created_at",
        "resolved_experiment",
        "checksums_ref",
        "package_checks_ref",
        "package_digest",
    ];
    for key in obj.keys() {
        if !allowed.iter().any(|expected| *expected == key) {
            return Err(anyhow!(
                "sealed package manifest contains unknown key '{}'",
                key
            ));
        }
    }
    for key in [
        "schema_version",
        "created_at",
        "resolved_experiment",
        "checksums_ref",
        "package_digest",
    ] {
        if !obj.contains_key(key) {
            return Err(anyhow!(
                "sealed package manifest missing required key '{}'",
                key
            ));
        }
    }
    Ok(())
}

pub(crate) fn verify_sealed_package_integrity(
    package_dir: &Path,
    manifest: &Value,
) -> Result<Value> {
    Ok(verify_sealed_package_integrity_snapshot(package_dir, manifest)?.resolved_experiment)
}

fn verify_sealed_package_integrity_snapshot(
    package_dir: &Path,
    manifest: &Value,
) -> Result<VerifiedPackageIntegrity> {
    require_sealed_manifest_keys(manifest)?;
    if manifest.pointer("/schema_version").and_then(Value::as_str) != Some("sealed_run_package_v2")
    {
        return Err(anyhow!(
            "preflight_failed: manifest schema_version must be 'sealed_run_package_v2'"
        ));
    }
    let checksums_ref = manifest
        .pointer("/checksums_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("sealed package manifest missing checksums_ref"))?;
    validate_metadata_ref_outside_runtime_payload(checksums_ref, "checksums_ref")?;
    let checksums_path =
        resolve_package_path_under_root(package_dir, checksums_ref, "checksums_ref")?;
    let checksums = load_json_file(&checksums_path)?;
    if checksums.pointer("/schema_version").and_then(Value::as_str)
        != Some("sealed_package_checksums_v2")
    {
        return Err(anyhow!(
            "preflight_failed: checksums schema_version must be 'sealed_package_checksums_v2'"
        ));
    }
    let files = checksums
        .pointer("/files")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("preflight_failed: checksums.json missing object field 'files'"))?;
    for (rel, expected_digest) in files {
        let expected = expected_digest.as_str().ok_or_else(|| {
            anyhow!(
                "preflight_failed: checksums entry '{}' must be a string digest",
                rel
            )
        })?;
        let file_path = resolve_package_path_under_root(package_dir, rel, "checksums.files")?;
        if !file_path.is_file() {
            return Err(anyhow!(
                "preflight_failed: checksummed file missing: {}",
                file_path.display()
            ));
        }
        let actual = sha256_file(&file_path)?;
        if !expected.eq_ignore_ascii_case(actual.as_str()) {
            return Err(anyhow!(
                "preflight_failed: checksum mismatch for '{}' (expected {}, got {})",
                rel,
                expected,
                actual
            ));
        }
    }
    if !files.contains_key("resolved_experiment.json") {
        return Err(anyhow!(
            "preflight_failed: checksums must include 'resolved_experiment.json'"
        ));
    }
    if !files.contains_key(STAGING_MANIFEST_FILE) {
        return Err(anyhow!(
            "preflight_failed: checksums must include '{}'",
            STAGING_MANIFEST_FILE
        ));
    }
    verify_no_unsealed_package_payload_entries(package_dir, manifest, files)?;
    verify_package_cas_pointers(package_dir, files)?;
    let computed_digest = canonical_json_digest(
        checksums
            .pointer("/files")
            .ok_or_else(|| anyhow!("preflight_failed: checksums missing files object"))?,
    );
    let manifest_digest = manifest
        .pointer("/package_digest")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("sealed package manifest missing package_digest"))?;
    if computed_digest != manifest_digest {
        return Err(anyhow!(
            "preflight_failed: package digest mismatch (manifest={}, computed={})",
            manifest_digest,
            computed_digest
        ));
    }
    let lock_path = package_dir.join("package.lock");
    let lock = load_json_file(&lock_path).map_err(|err| {
        anyhow!(
            "preflight_failed: package.lock missing or unreadable at {}: {}",
            lock_path.display(),
            err
        )
    })?;
    if lock.pointer("/package_digest").and_then(Value::as_str) != Some(manifest_digest) {
        return Err(anyhow!(
            "preflight_failed: package.lock digest does not match manifest package_digest"
        ));
    }
    if let Some(package_checks_ref) = manifest
        .pointer("/package_checks_ref")
        .and_then(Value::as_str)
    {
        validate_metadata_ref_outside_runtime_payload(package_checks_ref, "package_checks_ref")?;
        let package_checks_path =
            resolve_package_path_under_root(package_dir, package_checks_ref, "package_checks_ref")?;
        let package_checks = load_json_file(&package_checks_path).map_err(|err| {
            anyhow!(
                "preflight_failed: package checks missing or unreadable at {}: {}",
                package_checks_path.display(),
                err
            )
        })?;
        if package_checks
            .pointer("/schema_version")
            .and_then(Value::as_str)
            != Some(PACKAGE_CHECKS_SCHEMA_VERSION)
        {
            return Err(anyhow!(
                "preflight_failed: package checks schema_version must be '{}'",
                PACKAGE_CHECKS_SCHEMA_VERSION
            ));
        }
    }
    let resolved_path = resolve_package_path_under_root(
        package_dir,
        "resolved_experiment.json",
        "checksums.files",
    )?;
    let resolved_experiment = load_json_file(&resolved_path).map_err(|err| {
        anyhow!(
            "preflight_failed: resolved_experiment.json missing or unreadable at {}: {}",
            resolved_path.display(),
            err
        )
    })?;
    let staging_manifest_path =
        resolve_package_path_under_root(package_dir, STAGING_MANIFEST_FILE, "checksums.files")?;
    load_json_file(&staging_manifest_path).map_err(|err| {
        anyhow!(
            "preflight_failed: {} missing or unreadable at {}: {}",
            STAGING_MANIFEST_FILE,
            staging_manifest_path.display(),
            err
        )
    })?;
    Ok(VerifiedPackageIntegrity {
        resolved_experiment,
        checksums,
    })
}

fn run_payload_roots() -> [&'static str; 6] {
    [
        "tasks",
        "files",
        "agent_builds",
        PACKAGE_BLOBS_DIR,
        PACKAGED_RUNTIME_ASSETS_DIR,
        HOST_GRADER_CAPABILITIES_DIR,
    ]
}

fn checksum_entry_is_run_payload(rel: &str) -> bool {
    if rel == STAGING_MANIFEST_FILE {
        return true;
    }
    Path::new(rel)
        .components()
        .next()
        .and_then(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .is_some_and(|first| run_payload_roots().contains(&first))
}

pub(crate) fn copy_verified_package_payload_for_run(
    package_dir: &Path,
    run_dir: &Path,
) -> Result<()> {
    let manifest_path = package_dir.join("manifest.json");
    let manifest = load_json_file(&manifest_path)?;
    let verified = verify_sealed_package_integrity_snapshot(package_dir, &manifest)?;
    let files = verified
        .checksums
        .pointer("/files")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("preflight_failed: checksums.json missing object field 'files'"))?;
    for (rel, expected_digest) in files {
        if !checksum_entry_is_run_payload(rel) {
            continue;
        }
        let expected = expected_digest.as_str().ok_or_else(|| {
            anyhow!(
                "preflight_failed: checksums entry '{}' must be a string digest",
                rel
            )
        })?;
        let source = resolve_package_path_under_root(package_dir, rel, "checksums.files")?;
        let source_meta = fs::symlink_metadata(&source)?;
        if source_meta.file_type().is_symlink() || !source_meta.is_file() {
            return Err(anyhow!(
                "preflight_failed: package payload '{}' must be a regular file at copy time",
                rel
            ));
        }
        let destination = normalize_path(&run_dir.join(rel));
        let lexical_run_root = normalize_path(run_dir);
        if !destination.starts_with(&lexical_run_root) {
            return Err(anyhow!(
                "preflight_failed: package payload '{}' resolves outside run directory: {}",
                rel,
                destination.display()
            ));
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
            let run_root = fs::canonicalize(run_dir)?;
            let parent_root = fs::canonicalize(parent)?;
            if !parent_root.starts_with(&run_root) {
                return Err(anyhow!(
                    "preflight_failed: package payload '{}' destination parent resolves outside run directory: {}",
                    rel,
                    parent.display()
                ));
            }
        }
        match fs::symlink_metadata(&destination) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                return Err(anyhow!(
                    "preflight_failed: package payload '{}' destination already exists with unsupported file type: {}",
                    rel,
                    destination.display()
                ));
            }
            Ok(_) => {
                return Err(anyhow!(
                    "preflight_failed: package payload '{}' destination already exists: {}",
                    rel,
                    destination.display()
                ));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        fs::copy(&source, &destination)?;
        let actual = sha256_file(&destination)?;
        if !expected.eq_ignore_ascii_case(actual.as_str()) {
            return Err(anyhow!(
                "preflight_failed: copied package payload '{}' checksum mismatch (expected {}, got {})",
                rel,
                expected,
                actual
            ));
        }
    }
    Ok(())
}

fn package_relative_path(package_dir: &Path, path: &Path) -> String {
    path.strip_prefix(package_dir)
        .map(as_portable_rel)
        .unwrap_or_else(|_| path.display().to_string())
}

fn validate_metadata_ref_outside_runtime_payload(raw: &str, field_name: &str) -> Result<()> {
    let path = Path::new(raw.trim());
    if path.is_absolute() {
        return Err(anyhow!("{} must be relative to package root", field_name));
    }
    let normalized = normalize_path(path);
    let first = normalized
        .components()
        .next()
        .and_then(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .unwrap_or_default();
    if [
        "tasks",
        "files",
        "agent_builds",
        PACKAGE_BLOBS_DIR,
        PACKAGED_RUNTIME_ASSETS_DIR,
        HOST_GRADER_CAPABILITIES_DIR,
    ]
    .contains(&first)
    {
        return Err(anyhow!(
            "preflight_failed: {} must not point inside runtime payload directory '{}'",
            field_name,
            first
        ));
    }
    Ok(())
}

fn package_metadata_paths(manifest: &Value) -> BTreeSet<String> {
    let mut allowed = BTreeSet::from([
        "manifest.json".to_string(),
        "package.lock".to_string(),
        "checksums.json".to_string(),
        PACKAGE_CHECKS_FILE.to_string(),
    ]);
    if let Some(checksums_ref) = manifest.pointer("/checksums_ref").and_then(Value::as_str) {
        allowed.insert(as_portable_rel(&normalize_path(Path::new(checksums_ref))));
    }
    if let Some(package_checks_ref) = manifest
        .pointer("/package_checks_ref")
        .and_then(Value::as_str)
    {
        allowed.insert(as_portable_rel(&normalize_path(Path::new(
            package_checks_ref,
        ))));
    }
    allowed
}

fn verify_no_unsealed_package_payload_entries(
    package_dir: &Path,
    manifest: &Value,
    checksum_files: &serde_json::Map<String, Value>,
) -> Result<()> {
    let metadata_paths = package_metadata_paths(manifest);
    for entry in walkdir::WalkDir::new(package_dir) {
        let entry = entry?;
        let rel = package_relative_path(package_dir, entry.path());
        if rel.is_empty() {
            continue;
        }
        let file_type = entry.file_type();
        if file_type.is_dir() {
            continue;
        }
        if file_type.is_symlink() {
            return Err(anyhow!(
                "preflight_failed: sealed package contains unsealed symlink '{}'",
                rel
            ));
        }
        if !file_type.is_file() {
            return Err(anyhow!(
                "preflight_failed: sealed package contains unsupported file type '{}'",
                rel
            ));
        }
        if metadata_paths.contains(&rel) || checksum_files.contains_key(&rel) {
            continue;
        }
        return Err(anyhow!(
            "preflight_failed: sealed package contains unchecksummed payload file '{}'",
            rel
        ));
    }
    Ok(())
}

pub(crate) fn verify_package_cas_pointers(
    package_dir: &Path,
    checksum_files: &serde_json::Map<String, Value>,
) -> Result<()> {
    for entry in walkdir::WalkDir::new(package_dir) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let pointer_path = entry.path();
        let Some(pointer) = read_cas_pointer(pointer_path)? else {
            continue;
        };
        let pointer_rel = pointer_path
            .strip_prefix(package_dir)
            .map(as_portable_rel)
            .unwrap_or_else(|_| pointer_path.display().to_string());
        let blob_path =
            package_blob_path_for_digest(package_dir, &pointer.digest).map_err(|err| {
                anyhow!(
                    "preflight_failed: package CAS pointer '{}' has invalid digest: {}",
                    pointer_rel,
                    err
                )
            })?;
        let root = canonicalize_best_effort(package_dir);
        let blob_cmp = canonicalize_best_effort(&blob_path);
        if !blob_cmp.starts_with(&root) {
            return Err(anyhow!(
                "preflight_failed: package CAS pointer '{}' resolves outside package root: {}",
                pointer_rel,
                blob_path.display()
            ));
        }
        let blob_meta = blob_path.metadata().map_err(|err| {
            anyhow!(
                "preflight_failed: package CAS pointer '{}' references missing package blob {}: {}",
                pointer_rel,
                blob_path.display(),
                err
            )
        })?;
        if !blob_meta.is_file() {
            return Err(anyhow!(
                "preflight_failed: package CAS pointer '{}' references non-file package blob {}",
                pointer_rel,
                blob_path.display()
            ));
        }
        if blob_meta.len() != pointer.size_bytes {
            return Err(anyhow!(
                "preflight_failed: package CAS pointer '{}' blob size mismatch for {} (expected {}, got {})",
                pointer_rel,
                blob_path.display(),
                pointer.size_bytes,
                blob_meta.len()
            ));
        }
        let blob_rel = blob_path
            .strip_prefix(package_dir)
            .map(as_portable_rel)
            .unwrap_or_else(|_| blob_path.display().to_string());
        let checksum_digest = checksum_files
            .get(&blob_rel)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!(
                    "preflight_failed: package CAS blob '{}' referenced by '{}' is missing from checksums.json",
                    blob_rel,
                    pointer_rel
                )
            })?;
        let actual = sha256_file(&blob_path)?;
        if !actual.eq_ignore_ascii_case(&pointer.digest) {
            return Err(anyhow!(
                "preflight_failed: package CAS pointer '{}' blob digest mismatch for '{}' (expected {}, got {})",
                pointer_rel,
                blob_rel,
                pointer.digest,
                actual
            ));
        }
        if !checksum_digest.eq_ignore_ascii_case(&pointer.digest) {
            return Err(anyhow!(
                "preflight_failed: package CAS blob '{}' checksum digest mismatch for pointer '{}' (pointer {}, checksums {})",
                blob_rel,
                pointer_rel,
                pointer.digest,
                checksum_digest
            ));
        }
    }
    Ok(())
}

pub(crate) fn load_sealed_package_for_run(path: &Path) -> Result<LoadedExperimentInput> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolve run input path '{}'", path.display()))?;
    let (manifest_path, exp_dir) = if canonical.is_dir() {
        let manifest = canonical.join("manifest.json");
        if !manifest.is_file() {
            return Err(anyhow!(
                "run_input_invalid_kind: expected sealed package dir or manifest"
            ));
        }
        (manifest, canonical)
    } else if canonical
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "manifest.json")
    {
        let exp_dir = canonical
            .parent()
            .ok_or_else(|| anyhow!("manifest has no parent directory"))?
            .to_path_buf();
        (canonical, exp_dir)
    } else {
        return Err(anyhow!(
            "run_input_invalid_kind: expected sealed package dir or manifest"
        ));
    };
    let manifest = load_json_file(&manifest_path)?;
    let json_value = verify_sealed_package_integrity(&exp_dir, &manifest)?;
    let project_root = find_project_root(&exp_dir);
    let project_root = project_root
        .canonicalize()
        .with_context(|| format!("resolve project root '{}'", project_root.display()))?;
    Ok(LoadedExperimentInput {
        json_value,
        exp_dir,
        project_root,
    })
}

pub(crate) fn resolve_dataset_path_in_package(
    json_value: &Value,
    package_dir: &Path,
) -> Result<PathBuf> {
    let rel = json_value
        .pointer("/matrix/tasks/path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("matrix.tasks.path missing"))?;
    resolve_package_path_under_root(package_dir, rel, "matrix.tasks.path")
}
