use anyhow::{anyhow, Result};
use chrono::Utc;
use lab_core::canonical_json_digest;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{atomic_write_json_pretty, parse_metric_definitions, parse_policies};
use crate::model::GradingStrategy;
use crate::trial::plan::parse_trial_runtime_config;
use crate::trial::spec::parse_task_row;

pub(crate) const PACKAGE_CHECKS_FILE: &str = "package_checks.json";
pub(crate) const PACKAGE_CHECKS_SCHEMA_VERSION: &str = "package_checks_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl CheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skip => "skip",
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
    resolved: &Value,
    tasks: &[Value],
    package_digest: &str,
) -> Result<Value> {
    let report = collect_package_checks(resolved, tasks, package_digest)?;
    atomic_write_json_pretty(&package_dir.join(PACKAGE_CHECKS_FILE), &report)?;
    Ok(report)
}

pub fn check_package(package_dir: &Path) -> Result<Value> {
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
    let package_digest = read_package_digest(package_dir).unwrap_or_else(|| {
        canonical_json_digest(&json!({
            "resolved_experiment": resolved,
            "task_count": tasks.len(),
        }))
    });
    let report = collect_package_checks(&resolved, &tasks, &package_digest)?;
    atomic_write_json_pretty(&package_dir.join(PACKAGE_CHECKS_FILE), &report)?;
    Ok(report)
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
    checks.extend(check_variants_and_schedule(resolved));
    checks.extend(check_task_rows(tasks));
    checks.extend(check_metrics_and_grader(resolved));
    checks.extend(check_agent_outputs_and_events(resolved));
    checks.extend(check_mount_and_leakage_surface(resolved));
    checks.extend(check_epistemic_hygiene_declaration(resolved));

    let failed_count = checks
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("fail"))
        .count();
    let warn_count = checks
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("warn"))
        .count();
    let skip_count = checks
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("skip"))
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
            "skipped": skip_count,
        },
        "checks": checks,
    }))
}

fn check_variants_and_schedule(resolved: &Value) -> Vec<Value> {
    let mut checks = Vec::new();
    let mut ids = Vec::new();
    if let Some(id) = resolved
        .pointer("/baseline/variant_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        ids.push(id.to_string());
    }
    if let Some(items) = resolved.pointer("/variant_plan").and_then(Value::as_array) {
        for item in items {
            if let Some(id) = item
                .get("variant_id")
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
        .pointer("/design/comparison")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let policy = parse_policies(resolved);
    let scheduling = match policy.scheduling {
        crate::model::SchedulingPolicy::PairedInterleaved => "paired_interleaved",
        crate::model::SchedulingPolicy::VariantSequential => "variant_sequential",
        crate::model::SchedulingPolicy::Randomized => "randomized",
    };
    let variant_count = ids.len();
    let status = if comparison == "paired" && variant_count < 2 {
        CheckStatus::Fail
    } else if comparison == "paired" && scheduling != "paired_interleaved" {
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
    checks
}

fn check_task_rows(tasks: &[Value]) -> Vec<Value> {
    let mut ids = Vec::new();
    let mut malformed = Vec::new();
    for (idx, task) in tasks.iter().enumerate() {
        match parse_task_row(task) {
            Ok(row) => ids.push(row.id),
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
            format!("{} packaged task rows are valid with unique ids", ids.len())
        } else {
            "packaged task rows are missing, malformed, or duplicated".to_string()
        },
        json!({
            "task_count": tasks.len(),
            "malformed": malformed,
            "duplicates": duplicates,
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
            0 => "no explicit primary metric; consumers may rely on fallback behavior".to_string(),
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
    checks.push(check(
        "grader.conditional_integrity",
        if !grader_enabled && !grader_metric_ids.is_empty() {
            CheckStatus::Fail
        } else if grader_enabled {
            CheckStatus::Pass
        } else {
            CheckStatus::Skip
        },
        if grader_enabled {
            "grader is declared; grader-specific package checks apply".to_string()
        } else if grader_metric_ids.is_empty() {
            "skipped because grader.strategy=none; no grader_output metrics declared".to_string()
        } else {
            "grader.strategy=none but grader_output metrics are declared".to_string()
        },
        json!({
            "grader_strategy": format!("{:?}", trial_runtime.grader.strategy),
            "grader_output_metric_ids": grader_metric_ids,
        }),
    ));
    checks
}

fn check_agent_outputs_and_events(resolved: &Value) -> Vec<Value> {
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

    let events = resolved
        .pointer("/trial_runtime/agent/events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    checks.push(check(
        "events.declaration_present",
        if events.is_empty() {
            CheckStatus::Skip
        } else {
            CheckStatus::Pass
        },
        if events.is_empty() {
            "no agent event streams declared".to_string()
        } else {
            format!("{} agent event stream(s) declared", events.len())
        },
        json!({ "event_count": events.len() }),
    ));
    checks
}

fn check_mount_and_leakage_surface(resolved: &Value) -> Vec<Value> {
    let mut checks = Vec::new();
    let hidden_paths = resolved
        .pointer("/trial_runtime/grader/in_task_runtime/hidden_paths")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let output_mount_paths = resolved
        .pointer("/trial_runtime/agent/output_mounts")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("path").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
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
            if hidden_paths.is_empty() {
                CheckStatus::Skip
            } else {
                CheckStatus::Pass
            }
        } else {
            CheckStatus::Fail
        },
        if !overlaps.is_empty() {
            format!(
                "hidden grader paths overlap agent output mounts: {}",
                overlaps.join(", ")
            )
        } else if hidden_paths.is_empty() {
            "skipped because no hidden grader paths are declared".to_string()
        } else {
            "declared hidden grader paths do not overlap agent output mounts".to_string()
        },
        json!({
            "hidden_paths": hidden_paths,
            "output_mount_paths": output_mount_paths,
            "overlaps": overlaps,
        }),
    ));
    checks
}

fn check_epistemic_hygiene_declaration(resolved: &Value) -> Vec<Value> {
    let declared = resolved.pointer("/epistemic_hygiene");
    checks_with_single(check(
        "epistemic_hygiene.qa_engineer",
        CheckStatus::Skip,
        if declared.is_some() {
            "epistemic_hygiene declaration is present but dynamic QA engineer scans are not implemented in package checks yet"
        } else {
            "no epistemic_hygiene declaration; dynamic QA engineer scans are optional future work"
        },
        json!({ "declared": declared.is_some() }),
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

fn path_prefix_overlaps(a: &str, b: &str) -> bool {
    let a = a.trim_matches('/');
    let b = b.trim_matches('/');
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.starts_with(&format!("{}/", b)) || b.starts_with(&format!("{}/", a))
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

fn read_package_digest(package_dir: &Path) -> Option<String> {
    let lock_path = package_dir.join("package.lock");
    let value: Value = serde_json::from_slice(&std::fs::read(lock_path).ok()?).ok()?;
    value
        .get("package_digest")
        .and_then(Value::as_str)
        .map(str::to_string)
}
