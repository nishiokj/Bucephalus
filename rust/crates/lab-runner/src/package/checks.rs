use anyhow::{anyhow, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{
    atomic_write_json_pretty, parse_metric_definitions, parse_policies, parse_string_array_field,
};
use crate::model::{GradingStrategy, SchedulingPolicy};
use crate::package::sealed::verify_sealed_package_integrity;
use crate::trial::plan::parse_trial_runtime_config;
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

fn check(id: &str, status: CheckStatus, reason: impl Into<String>, mut evidence: Value) -> Value {
    redact_package_check_public_json(&mut evidence);
    json!({
        "id": id,
        "scope": "package",
        "status": status.as_str(),
        "reason": public_package_check_string(&reason.into()),
        "evidence": evidence,
    })
}

pub(crate) fn write_package_checks(
    package_dir: &Path,
    resolved: &Value,
    tasks: &[Value],
    package_digest: &str,
) -> Result<Value> {
    let report = collect_package_checks(resolved, tasks, package_digest)?;
    atomic_write_json_pretty(&package_dir.join(PACKAGE_CHECKS_FILE), &report)?;
    Ok(report)
}

pub fn check_package(package_dir: &Path) -> Result<Value> {
    let package_digest = read_package_digest(package_dir)?;
    let manifest_path = package_dir.join("manifest.json");
    if manifest_path.exists() {
        let manifest = load_package_json(package_dir, &manifest_path, "manifest")?;
        if manifest.pointer("/schema_version").and_then(Value::as_str)
            == Some("sealed_run_package_v2")
        {
            verify_sealed_package_integrity(package_dir, &manifest)?;
        }
    }
    let resolved_path = package_dir.join("resolved_experiment.json");
    let resolved = load_package_json(package_dir, &resolved_path, "resolved experiment")?;
    let tasks_path = package_dir.join("tasks").join("tasks.jsonl");
    let tasks = load_packaged_tasks(
        &tasks_path,
        &public_package_path_ref(package_dir, &tasks_path),
    )?;
    let report = collect_package_checks(&resolved, &tasks, &package_digest)?;
    atomic_write_json_pretty(&package_dir.join(PACKAGE_CHECKS_FILE), &report)?;
    Ok(report)
}

fn load_package_json(package_dir: &Path, path: &Path, description: &str) -> Result<Value> {
    let path_ref = public_package_path_ref(package_dir, path);
    let bytes = std::fs::read(path).map_err(|err| {
        anyhow!(
            "package check failed to read {} {}: {}",
            description,
            path_ref,
            err
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|err| {
        anyhow!(
            "package check failed to parse {} {}: {}",
            description,
            path_ref,
            err
        )
    })
}

fn collect_package_checks(
    resolved: &Value,
    tasks: &[Value],
    package_digest: &str,
) -> Result<Value> {
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
    checks.extend(check_variants_and_schedule(resolved)?);
    checks.extend(check_task_rows(tasks));
    checks.extend(check_task_image_refs(tasks));
    checks.extend(check_metrics_and_grader(resolved));
    checks.extend(check_agent_outputs(resolved));
    checks.extend(check_mount_and_leakage_surface(resolved)?);

    let failed_count = checks
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("fail"))
        .count();
    let warn_count = checks
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("warn"))
        .count();
    Ok(json!({
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
    }))
}

fn check_variants_and_schedule(resolved: &Value) -> Result<Vec<Value>> {
    let mut checks = Vec::new();
    let mut ids = Vec::new();
    if let Some(items) = resolved
        .pointer("/matrix/variants")
        .and_then(Value::as_array)
    {
        for item in items {
            if let Some(id) = item
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                ids.push(id.to_string());
            }
        }
    }
    let duplicate_ids = duplicate_strings(&ids);
    checks.push(check(
        "variants.unique_ids",
        if ids.is_empty() || !duplicate_ids.is_empty() {
            CheckStatus::Fail
        } else {
            CheckStatus::Pass
        },
        if ids.is_empty() {
            "no resolved variants found".to_string()
        } else if duplicate_ids.is_empty() {
            format!("{} resolved variants have unique ids", ids.len())
        } else {
            format!("duplicate variant ids: {}", duplicate_ids.join(", "))
        },
        json!({ "variant_ids": ids, "duplicates": duplicate_ids }),
    ));

    let comparison = resolved
        .pointer("/scheduling/comparison")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let policy = parse_policies(resolved)?;
    let paired_comparison = comparison == "paired";
    let paired_interleaved = matches!(policy.scheduling, SchedulingPolicy::PairedInterleaved);
    let scheduling = match policy.scheduling {
        SchedulingPolicy::PairedInterleaved => "paired_interleaved",
        SchedulingPolicy::VariantSequential => "variant_sequential",
        SchedulingPolicy::Randomized => "randomized",
    };
    let variant_count = ids.len();
    let status = if paired_comparison && variant_count < 2 {
        CheckStatus::Fail
    } else if paired_comparison && !paired_interleaved {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    };
    checks.push(check(
        "design.schedule_matches_comparison",
        status,
        format!(
            "comparison={} scheduling={} variant_count={}",
            comparison, scheduling, variant_count
        ),
        json!({
            "comparison": comparison,
            "scheduling": scheduling,
            "variant_count": variant_count,
        }),
    ));
    Ok(checks)
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

fn check_metrics_and_grader(resolved: &Value) -> Vec<Value> {
    let mut checks = Vec::new();
    let trial_runtime = match parse_trial_runtime_config(resolved) {
        Ok(value) => value,
        Err(err) => {
            checks.push(check(
                "trial_runtime.schema",
                CheckStatus::Fail,
                format!("trial runtime could not be parsed: {}", err),
                json!({}),
            ));
            return checks;
        }
    };
    checks.push(check(
        "trial_runtime.schema",
        CheckStatus::Pass,
        "trial runtime parsed from resolved package",
        json!({}),
    ));
    let metrics = match parse_metric_definitions(resolved) {
        Ok(value) => value,
        Err(err) => {
            checks.push(check(
                "metrics.parse",
                CheckStatus::Fail,
                format!("metric declarations could not be parsed: {}", err),
                json!({}),
            ));
            return checks;
        }
    };
    let primary_count = metrics.iter().filter(|metric| metric.primary).count();
    checks.push(check(
        "metrics.primary_declared",
        if primary_count == 1 {
            CheckStatus::Pass
        } else if primary_count == 0 {
            CheckStatus::Warn
        } else {
            CheckStatus::Fail
        },
        match primary_count {
            0 => "no explicit primary metric; ungraded trials report outcome success".to_string(),
            1 => "exactly one primary metric is declared".to_string(),
            _ => format!("{} primary metrics declared", primary_count),
        },
        json!({ "primary_count": primary_count, "metric_count": metrics.len() }),
    ));

    let grader_enabled = trial_runtime.grader.strategy != GradingStrategy::None;
    let grader_metric_ids = metrics
        .iter()
        .filter(|metric| metric.source.source_type == "grader_output")
        .map(|metric| metric.id.clone())
        .collect::<Vec<_>>();
    if !grader_enabled && !grader_metric_ids.is_empty() {
        checks.push(check(
            "grader.conditional_integrity",
            CheckStatus::Fail,
            "grader.strategy=none but grader_output metrics are declared".to_string(),
            json!({
                "grader_strategy": format!("{:?}", trial_runtime.grader.strategy),
                "grader_output_metric_ids": grader_metric_ids,
            }),
        ));
    }
    checks
}

fn check_agent_outputs(resolved: &Value) -> Vec<Value> {
    let mut checks = Vec::new();
    let result_capture = resolved.pointer("/trial_runtime/agent/outputs/result/capture");
    let result_path = result_capture
        .and_then(|capture| capture.get("path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    checks.push(check(
        "outputs.result_capture_declared",
        if result_path.is_some() {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        if result_path.is_some() {
            "agent result output capture has a path".to_string()
        } else {
            "agent result output capture is missing a path".to_string()
        },
        json!({ "path": result_path }),
    ));

    checks
}

fn check_mount_and_leakage_surface(resolved: &Value) -> Result<Vec<Value>> {
    let mut checks = Vec::new();
    let hidden_paths = parse_string_array_field(
        resolved.pointer("/trial_runtime/grader/in_task_runtime/hidden_paths"),
        "trial_runtime.grader.in_task_runtime.hidden_paths",
    )?;
    let output_mount_paths =
        parse_output_mount_paths(resolved.pointer("/trial_runtime/agent/output_mounts"))?;
    if hidden_paths.is_empty() {
        return Ok(checks);
    }
    let overlaps = hidden_paths
        .iter()
        .filter(|hidden| {
            output_mount_paths
                .iter()
                .any(|mount| path_prefix_overlaps(hidden, mount))
        })
        .cloned()
        .collect::<Vec<_>>();
    checks.push(check(
        "contamination.hidden_path_mount_overlap",
        if overlaps.is_empty() {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        if !overlaps.is_empty() {
            format!(
                "hidden grader paths overlap agent output mounts: {}",
                overlaps.join(", ")
            )
        } else {
            "declared hidden grader paths do not overlap agent output mounts".to_string()
        },
        json!({
            "hidden_paths": hidden_paths,
            "output_mount_paths": output_mount_paths,
            "overlaps": overlaps,
        }),
    ));
    Ok(checks)
}

fn parse_output_mount_paths(value: Option<&Value>) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("trial_runtime.agent.output_mounts must be an array"))?;
    let mut paths = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "trial_runtime.agent.output_mounts[{}].path is required",
                    idx
                )
            })?;
        paths.push(path.to_string());
    }
    Ok(paths)
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

fn path_prefix_overlaps(a: &str, b: &str) -> bool {
    let a = a.trim_matches('/');
    let b = b.trim_matches('/');
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.starts_with(&format!("{}/", b)) || b.starts_with(&format!("{}/", a))
}

fn load_packaged_tasks(path: &Path, path_ref: &str) -> Result<Vec<Value>> {
    let file = std::fs::File::open(path)
        .map_err(|err| anyhow!("package check failed to read {}: {}", path_ref, err))?;
    let reader = std::io::BufReader::new(file);
    let mut tasks = Vec::new();
    for (idx, line) in std::io::BufRead::lines(reader).enumerate() {
        let line = line.map_err(|err| {
            anyhow!(
                "package check failed to read {} line {}: {}",
                path_ref,
                idx + 1,
                err
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        tasks.push(serde_json::from_str::<Value>(&line).map_err(|err| {
            anyhow!(
                "package check failed to parse {} line {}: {}",
                path_ref,
                idx + 1,
                err
            )
        })?);
    }
    Ok(tasks)
}

fn read_package_digest(package_dir: &Path) -> Result<String> {
    let lock_path = package_dir.join("package.lock");
    let lock_ref = public_package_path_ref(package_dir, &lock_path);
    let value = load_package_json(package_dir, &lock_path, "package lock")?;
    crate::package::validate::validate_schema_contract_value(&value, "package lock")?;
    value
        .get("package_digest")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!(
                "package check failed to read package_digest from {}",
                lock_ref
            )
        })
}

fn public_package_path_ref(package_dir: &Path, path: &Path) -> String {
    let Ok(rel) = path.strip_prefix(package_dir) else {
        return "[REDACTED:local-path]".to_string();
    };
    if rel.as_os_str().is_empty() {
        return "package://.".to_string();
    }
    let rel = rel
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    format!("package://{rel}")
}

fn redact_package_check_public_json(value: &mut Value) {
    match value {
        Value::String(text) => {
            *text = public_package_check_string(text);
        }
        Value::Array(items) => {
            for item in items {
                redact_package_check_public_json(item);
            }
        }
        Value::Object(object) => {
            for (key, child) in object {
                if let Some(marker) = package_check_redaction_marker_for_key(key) {
                    *child = Value::String(marker.to_string());
                } else {
                    redact_package_check_public_json(child);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn package_check_redaction_marker_for_key(key: &str) -> Option<&'static str> {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if normalized.ends_with("present") || normalized.ends_with("ref") {
        return None;
    }
    if normalized == "env"
        || normalized == "environment"
        || normalized.ends_with("env")
        || normalized.ends_with("environment")
    {
        return Some("[REDACTED:environment]");
    }
    if normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("apikey")
        || normalized.contains("credential")
        || normalized.contains("authorization")
        || normalized.contains("bearer")
        || normalized.contains("cookie")
        || normalized.contains("privatekey")
    {
        return Some("[REDACTED:secret-like]");
    }
    None
}

fn public_package_check_string(message: &str) -> String {
    let mut redacted = message
        .lines()
        .map(redact_package_check_line)
        .collect::<Vec<_>>()
        .join("\n");
    if message.ends_with('\n') {
        redacted.push('\n');
    }
    redacted
}

fn redact_package_check_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if lower.contains("authorization:") || lower.contains("bearer ") {
        return "[REDACTED:secret-like]".to_string();
    }
    let token_redacted = line
        .split_inclusive(char::is_whitespace)
        .map(|chunk| {
            if let Some(last) = chunk.chars().last().filter(|ch| ch.is_whitespace()) {
                let token = &chunk[..chunk.len() - last.len_utf8()];
                format!("{}{}", redact_package_check_token(token), last)
            } else {
                redact_package_check_token(chunk)
            }
        })
        .collect::<String>();
    redact_local_paths_in_package_check_text(&token_redacted)
}

fn redact_package_check_token(token: &str) -> String {
    let trimmed_start = token.trim_start_matches(package_check_token_prefix);
    let prefix_len = token.len() - trimmed_start.len();
    let trimmed_core = trimmed_start.trim_end_matches(package_check_token_suffix);
    let suffix_len = trimmed_start.len() - trimmed_core.len();
    let prefix = &token[..prefix_len];
    let suffix = &token[token.len() - suffix_len..];
    let lower = trimmed_core.to_ascii_lowercase();

    if trimmed_core.contains("[REDACTED:") {
        return token.to_string();
    }

    let redacted_core = if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("file://")
    {
        Some(redact_package_check_url(trimmed_core))
    } else if looks_like_package_check_local_path(trimmed_core) {
        Some("[REDACTED:local-path]".to_string())
    } else if let Some((key, value)) = trimmed_core.split_once('=') {
        let key_lower = key.to_ascii_lowercase();
        if key_lower.contains("token")
            || key_lower.contains("secret")
            || key_lower.contains("password")
            || key_lower.contains("apikey")
            || key_lower.contains("api_key")
            || key_lower.contains("credential")
        {
            Some(format!("{key}=[REDACTED:secret-like]"))
        } else if looks_like_package_check_local_path(value) {
            Some(format!("{key}=[REDACTED:local-path]"))
        } else if looks_like_package_check_url(value) {
            Some(format!("{key}={}", redact_package_check_url(value)))
        } else {
            None
        }
    } else if lower.starts_with("sk-") {
        Some("[REDACTED:secret-like]".to_string())
    } else {
        None
    };

    match redacted_core {
        Some(core) => format!("{prefix}{core}{suffix}"),
        None => token.to_string(),
    }
}

fn package_check_token_prefix(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '<' | '"' | '\'' | '`')
}

fn package_check_token_suffix(ch: char) -> bool {
    matches!(
        ch,
        '.' | ',' | ';' | ')' | ']' | '}' | '>' | '"' | '\'' | '`'
    )
}

fn looks_like_package_check_local_path(value: &str) -> bool {
    value.starts_with("/Users/")
        || value.starts_with("/home/")
        || value.starts_with("/private/")
        || value.starts_with("/tmp/")
}

fn looks_like_package_check_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file://")
}

fn redact_package_check_url(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("file://") {
        return "file://[REDACTED:local-path]".to_string();
    }
    let Some(scheme_end) = value.find("://") else {
        return "[REDACTED:url]".to_string();
    };
    let scheme = &value[..scheme_end + 3];
    let mut remainder = &value[scheme_end + 3..];
    let authority_end = remainder
        .find(|ch: char| matches!(ch, '/' | '?' | '#'))
        .unwrap_or(remainder.len());
    let mut redacted = false;
    if let Some(at) = remainder[..authority_end].rfind('@') {
        remainder = &remainder[at + 1..];
        redacted = true;
    }
    if let Some(end) = remainder.find(|ch: char| matches!(ch, '?' | '#')) {
        remainder = &remainder[..end];
        redacted = true;
    }

    let mut public = format!("{scheme}{remainder}");
    if redacted {
        public.push_str(" [redacted URL credentials/query]");
    }
    public
}

fn redact_local_paths_in_package_check_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = earliest_package_check_local_path_start(rest) {
        out.push_str(&rest[..start]);
        out.push_str("[REDACTED:local-path]");
        let after_start = &rest[start..];
        let end = after_start
            .find(|ch: char| {
                ch.is_whitespace()
                    || matches!(ch, ':' | '"' | '\'' | '`' | '<' | '>' | '|' | ',' | ';')
            })
            .unwrap_or(after_start.len());
        rest = &after_start[end..];
    }
    out.push_str(rest);
    out
}

fn earliest_package_check_local_path_start(text: &str) -> Option<usize> {
    ["/Users/", "/home/", "/private/", "/tmp/"]
        .iter()
        .filter_map(|prefix| text.find(prefix))
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_check_values_are_public_boundary_safe() {
        let item = check(
            "boundary.public",
            CheckStatus::Fail,
            "failed to read /Users/alice/work/run.json: Permission denied\nmirror https://mirror-user:mirror-secret@mirror.example/releases?token=raw-query#frag\nworker token=raw-worker-token\nlocal file:///private/tmp/bucephalus/install.sh",
            json!({
                "api_token": "raw-evidence-token",
                "container_path": "/bucephalus/out/result.json",
                "env": {
                    "OPENAI_API_KEY": "live-env-secret"
                },
                "mutable_images": ["python:3.11-slim"],
                "notes": [
                    "Authorization: Bearer raw-header-token",
                    "workspace=/Users/alice/work"
                ],
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "result_path": "/Users/alice/work/result.json"
            }),
        );
        let encoded = serde_json::to_string(&item).expect("json");

        assert!(encoded.contains("failed to read"));
        assert!(encoded.contains("Permission denied"));
        assert!(encoded.contains("https://mirror.example/releases"));
        assert!(encoded.contains("[redacted URL credentials/query]"));
        assert!(encoded.contains("token=[REDACTED:secret-like]"));
        assert!(encoded.contains("file://[REDACTED:local-path]"));
        assert_eq!(
            item.pointer("/evidence/api_token").and_then(Value::as_str),
            Some("[REDACTED:secret-like]")
        );
        assert_eq!(
            item.pointer("/evidence/container_path")
                .and_then(Value::as_str),
            Some("/bucephalus/out/result.json")
        );
        assert_eq!(
            item.pointer("/evidence/env").and_then(Value::as_str),
            Some("[REDACTED:environment]")
        );
        assert_eq!(
            item.pointer("/evidence/mutable_images/0")
                .and_then(Value::as_str),
            Some("python:3.11-slim")
        );
        assert_eq!(
            item.pointer("/evidence/result_path")
                .and_then(Value::as_str),
            Some("[REDACTED:local-path]")
        );

        for forbidden in [
            "/Users/alice",
            "/private/tmp",
            "live-env-secret",
            "mirror-secret",
            "mirror-user",
            "raw-evidence-token",
            "raw-header-token",
            "raw-query",
            "raw-worker-token",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "package check leaked forbidden text {forbidden}: {encoded}"
            );
        }
    }
}
