use anyhow::{anyhow, Context, Result};
use lab_core::{ensure_dir, sha256_file};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::util::{preserve_symlink, remove_path_if_exists};

const CAS_POINTER_SCHEMA: &str = "bucephalus_cas_pointer_v1";
const DEFAULT_LARGE_FILE_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const PACKAGE_BLOBS_DIR: &str = "blobs";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CasPointer {
    pub(crate) schema_version: String,
    pub(crate) kind: String,
    pub(crate) digest: String,
    pub(crate) size_bytes: u64,
}

pub(crate) fn agent_directory_artifact_excludes() -> &'static [&'static str] {
    &[
        ".lab",
        ".git",
        "node_modules",
        ".venv",
        "__pycache__",
        ".tox",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        "target",
        "rust/target",
        ".next",
        ".nuxt",
        ".turbo",
        ".nx",
        "coverage",
        ".gradle",
    ]
}

pub(crate) fn should_include_agent_artifact_path(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    if rel.as_os_str().is_empty() {
        return true;
    }
    !agent_directory_artifact_excludes()
        .iter()
        .any(|exclude| rel.starts_with(exclude))
}

pub(crate) fn large_file_threshold_bytes() -> Result<u64> {
    let Ok(value) = std::env::var("BUCEPHALUS_CAS_FILE_THRESHOLD_BYTES") else {
        return Ok(DEFAULT_LARGE_FILE_THRESHOLD_BYTES);
    };
    parse_large_file_threshold_bytes(&value)
}

pub(crate) fn parse_large_file_threshold_bytes(value: &str) -> Result<u64> {
    value.parse::<u64>().with_context(|| {
        format!("BUCEPHALUS_CAS_FILE_THRESHOLD_BYTES must be an unsigned byte count, got {value:?}")
    })
}

fn sha256_hex_from_digest(digest: &str) -> Result<String> {
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("CAS digest must start with sha256:"))?;
    if hex.len() != 64 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(anyhow!("CAS digest must be sha256:<64 hex chars>"));
    }
    Ok(hex.to_ascii_lowercase())
}

pub(crate) fn package_blob_path_for_digest(package_dir: &Path, digest: &str) -> Result<PathBuf> {
    let hex = sha256_hex_from_digest(digest)?;
    Ok(package_dir
        .join(PACKAGE_BLOBS_DIR)
        .join("sha256")
        .join(hex)
        .join("blob"))
}

pub(crate) fn put_file_in_package_cas(
    package_dir: &Path,
    source: &Path,
) -> Result<(String, PathBuf)> {
    let digest = sha256_file(source)?;
    let blob = package_blob_path_for_digest(package_dir, &digest)?;
    if !blob.exists() {
        if let Some(parent) = blob.parent() {
            ensure_dir(parent)?;
        }
        fs::copy(source, &blob).with_context(|| {
            format!(
                "failed to copy large runtime asset {} into CAS blob {}",
                source.display(),
                blob.display()
            )
        })?;
    }
    Ok((digest, blob))
}

pub(crate) fn write_cas_pointer(path: &Path, digest: String, size_bytes: u64) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let pointer = CasPointer {
        schema_version: CAS_POINTER_SCHEMA.to_string(),
        kind: "file".to_string(),
        digest,
        size_bytes,
    };
    let bytes = serde_json::to_vec_pretty(&pointer)?;
    crate::config::atomic_write_bytes(path, &bytes)
}

pub(crate) fn read_cas_pointer(path: &Path) -> Result<Option<CasPointer>> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if !meta.is_file() || meta.len() > 4096 {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    if bytes.first().is_none_or(|byte| *byte != b'{') {
        return Ok(None);
    }
    let Ok(pointer) = serde_json::from_slice::<CasPointer>(&bytes) else {
        return Ok(None);
    };
    if pointer.schema_version == CAS_POINTER_SCHEMA && pointer.kind == "file" {
        Ok(Some(pointer))
    } else {
        Ok(None)
    }
}

pub(crate) fn resolve_package_cas_pointer_blob(
    package_dir: &Path,
    pointer_path: &Path,
) -> Result<Option<PathBuf>> {
    let Some(pointer) = read_cas_pointer(pointer_path)? else {
        return Ok(None);
    };
    let blob = package_blob_path_for_digest(package_dir, &pointer.digest)?;
    let meta = fs::metadata(&blob).with_context(|| {
        format!(
            "package CAS pointer {} references missing package blob {}",
            pointer_path.display(),
            blob.display()
        )
    })?;
    if !meta.is_file() {
        return Err(anyhow!(
            "package CAS pointer {} references non-file package blob {}",
            pointer_path.display(),
            blob.display()
        ));
    }
    if meta.len() != pointer.size_bytes {
        return Err(anyhow!(
            "package CAS pointer {} size mismatch for {} (expected {}, got {})",
            pointer_path.display(),
            blob.display(),
            pointer.size_bytes,
            meta.len()
        ));
    }
    let actual = sha256_file(&blob)?;
    if !actual.eq_ignore_ascii_case(&pointer.digest) {
        return Err(anyhow!(
            "package CAS pointer {} digest mismatch for {} (expected {}, got {})",
            pointer_path.display(),
            blob.display(),
            pointer.digest,
            actual
        ));
    }
    Ok(Some(blob))
}

pub(crate) fn path_contains_cas_pointer(path: &Path) -> Result<bool> {
    if path.is_file() {
        return Ok(read_cas_pointer(path)?.is_some());
    }
    if !path.is_dir() {
        return Ok(false);
    }
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry?;
        if entry.file_type().is_file() && read_cas_pointer(entry.path())?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn materialize_package_cas_backed_path(
    package_dir: &Path,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    remove_path_if_exists(destination)?;
    if source.is_file() {
        let resolved = resolve_package_cas_pointer_blob(package_dir, source)?
            .unwrap_or_else(|| source.to_path_buf());
        copy_or_link_file(&resolved, destination)?;
        return Ok(());
    }
    if !source.is_dir() {
        return Err(anyhow!(
            "CAS materialization source must be a file or directory: {}",
            source.display()
        ));
    }
    ensure_dir(destination)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(source).with_context(|| {
            format!(
                "package CAS materialization path {} escaped source root {}",
                path.display(),
                source.display()
            )
        })?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(rel);
        if entry.file_type().is_dir() {
            ensure_dir(&target)?;
        } else if entry.file_type().is_symlink() {
            if let Some(parent) = target.parent() {
                ensure_dir(parent)?;
            }
            preserve_symlink(path, &target)?;
        } else if entry.file_type().is_file() {
            let resolved = resolve_package_cas_pointer_blob(package_dir, path)?
                .unwrap_or_else(|| path.to_path_buf());
            copy_or_link_file(&resolved, &target)?;
        }
    }
    Ok(())
}

pub(crate) fn copy_or_link_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        ensure_dir(parent)?;
    }
    if destination.exists() {
        remove_path_if_exists(destination)?;
    }
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to materialize {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}
