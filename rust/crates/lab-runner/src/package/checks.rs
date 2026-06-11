use anyhow::{anyhow, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{atomic_write_json_pretty, load_json_file};
use crate::package::sealed::verify_sealed_package_integrity;
use crate::trial::spec::parse_task_boundary_from_packaged_task;

pub(crate) const PACKAGE_CHECKS_FILE: &str = "package_checks.json";
pub(crate) const PACKAGE_CHECKS_SCHEMA_VERSION: &str = "package_checks_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

fn check(id: &str, status: CheckStatus, reason: impl Into<String>, evidence: Value) -> Value {
    json!({
        "id": id,
        "scope": "package",
        "status": status.as_str(),
        "reason": reason.into(),
        "evidence": evidence,
    })
}

pub(crate) fn write_package_checks(
    package_dir: &Path,
    tasks: &[Value],
    package_digest: &str,
) -> Result<Value> {
    let report = collect_package_checks(tasks, package_digest);
    atomic_write_json_pretty(&package_dir.join(PACKAGE_CHECKS_FILE), &report)?;
    Ok(report)
}

pub fn check_package(package_dir: &Path) -> Result<Value> {
    let package_digest = read_package_digest(package_dir)?;
    let manifest_path = package_dir.join("manifest.json");
    let manifest = load_json_file(&manifest_path).map_err(|err| {
        anyhow!(
            "package check failed to read sealed package manifest {}: {}",
            manifest_path.display(),
            err
        )
    })?;
    verify_sealed_package_integrity(package_dir, &manifest)?;
    let resolved_path = package_dir.join("resolved_experiment.json");
    let resolved: Value =
        serde_json::from_slice(&std::fs::read(&resolved_path).map_err(|err| {
            anyhow!(
                "package check failed to read {}: {}",
                resolved_path.display(),
                err
            )
        })?)?;
    let tasks_path = package_dir.join("tasks").join("tasks.jsonl");
    let tasks = load_packaged_tasks(&tasks_path)?;
    crate::package::validate::validate_resolved_experiment_schema(&resolved, "package check")?;
    let report = collect_package_checks(&tasks, &package_digest);
    atomic_write_json_pretty(&package_dir.join(PACKAGE_CHECKS_FILE), &report)?;
    Ok(report)
}

fn collect_package_checks(tasks: &[Value], package_digest: &str) -> Value {
    let mut checks = Vec::new();
    checks.push(check(
        "provenance.package_digest_present",
        if package_digest.trim().is_empty() {
            CheckStatus::Fail
        } else {
            CheckStatus::Pass
        },
        "package digest is recorded for immutable package checks",
        json!({ "package_digest": package_digest }),
    ));
    checks.extend(check_task_rows(tasks));
    checks.extend(check_task_image_refs(tasks));

    let failed_count = checks
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("fail"))
        .count();
    let warn_count = checks
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("warn"))
        .count();
    json!({
        "schema_version": PACKAGE_CHECKS_SCHEMA_VERSION,
        "generated_at": Utc::now().to_rfc3339(),
        "package_digest": package_digest,
        "passed": failed_count == 0,
        "summary": {
            "checks": checks.len(),
            "failed": failed_count,
            "warnings": warn_count,
        },
        "checks": checks,
    })
}

fn check_task_rows(tasks: &[Value]) -> Vec<Value> {
    let mut ids = Vec::new();
    let mut malformed = Vec::new();
    for (idx, task) in tasks.iter().enumerate() {
        match parse_task_boundary_from_packaged_task(task) {
            Ok(boundary) => ids.push(boundary.task_id),
            Err(err) => malformed.push(format!("line {}: {}", idx + 1, err)),
        }
    }
    let duplicates = duplicate_strings(&ids);
    let status = if malformed.is_empty() && !ids.is_empty() && duplicates.is_empty() {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    checks_with_single(check(
        "tasks.unique_valid_rows",
        status,
        if status == CheckStatus::Pass {
            format!(
                "{} packaged cases/tasks are valid with unique ids",
                ids.len()
            )
        } else {
            "packaged cases/tasks are missing, malformed, or duplicated".to_string()
        },
        json!({
            "task_count": tasks.len(),
            "malformed": malformed,
            "duplicates": duplicates,
        }),
    ))
}

fn image_ref_has_digest_pin(image: &str) -> bool {
    let Some((_, digest)) = image.rsplit_once("@sha256:") else {
        return false;
    };
    digest.len() == 64 && digest.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn check_task_image_refs(tasks: &[Value]) -> Vec<Value> {
    let mut images = BTreeSet::new();
    let mut malformed = 0usize;
    for task in tasks {
        match parse_task_boundary_from_packaged_task(task) {
            Ok(boundary) => {
                if !boundary.task_image.trim().is_empty() {
                    images.insert(boundary.task_image);
                }
            }
            Err(_) => malformed += 1,
        }
    }
    if images.is_empty() {
        if malformed == 0 {
            return Vec::new();
        }
        return checks_with_single(check(
            "images.task_refs_digest_pinned",
            CheckStatus::Fail,
            "task image refs could not be checked because packaged task rows are malformed"
                .to_string(),
            json!({ "malformed_task_rows": malformed }),
        ));
    }

    let (pinned, mutable): (Vec<_>, Vec<_>) = images
        .iter()
        .cloned()
        .partition(|image| image_ref_has_digest_pin(image));
    let status = if malformed > 0 {
        CheckStatus::Fail
    } else if mutable.is_empty() {
        CheckStatus::Pass
    } else {
        CheckStatus::Warn
    };
    checks_with_single(check(
        "images.task_refs_digest_pinned",
        status,
        if status == CheckStatus::Pass {
            format!(
                "all {} unique task image refs are digest-pinned",
                pinned.len()
            )
        } else if malformed > 0 {
            "task image refs could not be fully checked because packaged task rows are malformed"
                .to_string()
        } else {
            format!(
                "{} of {} unique task image refs are mutable tag refs; use digest refs or an image lock for reproducible runs",
                mutable.len(),
                images.len()
            )
        },
        json!({
            "unique_image_count": images.len(),
            "pinned_images": pinned,
            "mutable_images": mutable,
            "malformed_task_rows": malformed,
        }),
    ))
}

fn checks_with_single(value: Value) -> Vec<Value> {
    vec![value]
}

fn duplicate_strings(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for value in values {
        if !seen.insert(value.clone()) {
            duplicates.insert(value.clone());
        }
    }
    duplicates.into_iter().collect()
}

fn load_packaged_tasks(path: &Path) -> Result<Vec<Value>> {
    let file = std::fs::File::open(path)
        .map_err(|err| anyhow!("package check failed to read {}: {}", path.display(), err))?;
    let reader = std::io::BufReader::new(file);
    let mut tasks = Vec::new();
    for (idx, line) in std::io::BufRead::lines(reader).enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        tasks.push(serde_json::from_str::<Value>(&line).map_err(|err| {
            anyhow!(
                "package check failed to parse {} line {}: {}",
                path.display(),
                idx + 1,
                err
            )
        })?);
    }
    Ok(tasks)
}

fn read_package_digest(package_dir: &Path) -> Result<String> {
    let lock_path = package_dir.join("package.lock");
    let value = load_json_file(&lock_path).map_err(|err| {
        anyhow!(
            "package check failed to read package lock {}: {}",
            lock_path.display(),
            err
        )
    })?;
    crate::package::validate::validate_schema_contract_value(&value, "package lock")?;
    value
        .get("package_digest")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!(
                "package check failed to read package_digest from {}",
                lock_path.display()
            )
        })
}
