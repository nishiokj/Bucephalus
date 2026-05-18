use anyhow::{anyhow, Result};
use lab_schemas::compile_schema;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path};

use crate::config::*;
use crate::model::*;
use crate::trial::plan::parse_trial_runtime_config;

pub(crate) fn validate_required_fields(json_value: &Value) -> Result<()> {
    if json_value
        .pointer("/version")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.trim() == "1.0")
    {
        return Err(anyhow!("experiment version '1.0' is not supported"));
    }
    for (pointer, message) in [
        (
            "/task_runtime",
            "define task behavior under /trial_runtime/task",
        ),
        (
            "/benchmark/grader",
            "define grader execution under /trial_runtime/grader",
        ),
        (
            "/trial_runtime/outputs",
            "declare runtime outputs under /trial_runtime/agent/outputs and downstream outputs under /trial_runtime/grader/outputs",
        ),
        (
            "/trial_runtime/grader/conclusion",
            "declare grader outputs and metrics instead of grader-owned trial_conclusion_v1 mapping",
        ),
        (
            "/benchmark/adapter",
            "benchmark adapters are not a public runtime surface",
        ),
        (
            "/benchmark/image_source",
            "define task image sourcing under /trial_runtime/task/workspace",
        ),
    ] {
        if json_value.pointer(pointer).is_some() {
            return Err(anyhow!("{} is not supported; {}", pointer, message));
        }
    }
    reject_v0_authoring_paths(json_value)?;
    validate_runtime_declarations(json_value)?;
    validate_sidecars(json_value)?;
    let required: &[&str] = &[
        "/matrix/variants",
        "/matrix/tasks/source",
        "/matrix/tasks/path",
        "/matrix/repeats",
        "/runtime/network/task_sandbox",
        "/policy/timeout_ms",
        "/trial_runtime/task/interface",
        "/trial_runtime/agent/command",
        "/trial_runtime/agent/outputs/result/capture/type",
        "/trial_runtime/agent/outputs/result/capture/path",
        "/trial_runtime/execution/agent_site",
        "/trial_runtime/grader/strategy",
    ];
    let mut missing = Vec::new();
    for pointer in required {
        let value = json_value.pointer(pointer);
        let is_missing = match value {
            None => true,
            Some(Value::String(s)) => s.is_empty(),
            Some(Value::Number(n)) => {
                n.as_u64() == Some(0)
                    && (*pointer == "/matrix/repeats" || *pointer == "/policy/timeout_ms")
            }
            _ => false,
        };
        if is_missing {
            missing.push(*pointer);
        }
    }
    if json_value.pointer("/policy/task_sandbox").is_none() {
        missing.push("/policy/task_sandbox");
    }
    let has_command = match json_value.pointer("/trial_runtime/agent/command") {
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(parts)) if !parts.is_empty() => parts
            .iter()
            .all(|part| part.as_str().map(|s| !s.trim().is_empty()).unwrap_or(false)),
        _ => false,
    };
    if !has_command {
        missing.push("/trial_runtime/agent/command");
    }
    let experiment_id = json_value
        .pointer("/experiment/id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if experiment_id.is_empty() {
        missing.push("/experiment/id");
    }
    if !missing.is_empty() {
        missing.sort_unstable();
        missing.dedup();
        return Err(anyhow!(
            "missing required experiment fields: {}",
            missing.join(", ")
        ));
    }

    validate_sanitization_profile_network_invariants(json_value, None)?;
    parse_trial_runtime_config(json_value)?;
    validate_benchmark_artifacts(json_value)?;
    Ok(())
}

fn reject_v0_authoring_paths(json_value: &Value) -> Result<()> {
    for (pointer, replacement) in [
        ("/baseline", "/matrix/variants[] with baseline: true"),
        ("/variant_plan", "/matrix/variants"),
        ("/variants", "/matrix/variants"),
        ("/dataset", "/matrix/tasks"),
        (
            "/design",
            "/matrix, /scheduling, and /policy/sanitization_profile",
        ),
        ("/validity", "/policy/validity"),
        ("/artifacts", "/extra_outputs"),
        (
            "/trial_runtime/agent/artifact",
            "/trial_runtime/agent/mount",
        ),
        ("/trial_runtime/agent/network", "/runtime/network/agent"),
        ("/trial_runtime/agent/secret_files", "/runtime/secrets"),
        (
            "/policy/task_sandbox/network",
            "/runtime/network/task_sandbox",
        ),
        (
            "/policy/task_sandbox/profile",
            "/policy/sanitization_profile",
        ),
        ("/experiment/workload_type", "<removed>"),
    ] {
        if json_value.pointer(pointer).is_some() {
            return Err(anyhow!(
                "{} is not supported in v1; move to {}",
                pointer,
                replacement
            ));
        }
    }
    if let Some(variants) = json_value
        .pointer("/matrix/variants")
        .and_then(Value::as_array)
    {
        for (idx, variant) in variants.iter().enumerate() {
            for (field, replacement) in [
                ("bindings", "config"),
                ("runtime_overrides", "overrides"),
                ("variant_id", "id"),
            ] {
                if variant.get(field).is_some() {
                    return Err(anyhow!(
                        "/matrix/variants/{}/{} is not supported in v1; move to /matrix/variants[].{}",
                        idx,
                        field,
                        replacement
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_runtime_declarations(json_value: &Value) -> Result<()> {
    let Some(runtime) = json_value.pointer("/runtime") else {
        return Err(anyhow!("missing /runtime"));
    };
    for (pointer, supported) in [
        ("/compute/backend", "local-docker"),
        ("/storage/backend", "local-fs"),
        ("/traces/backend", "local-stdout"),
    ] {
        if let Some(value) = runtime.pointer(pointer).and_then(Value::as_str) {
            if value != supported {
                return Err(anyhow!(
                    "/runtime{} backend '{}' is declared but not implemented yet; supported backend is '{}'",
                    pointer.trim_end_matches("/backend"),
                    value,
                    supported
                ));
            }
        }
    }
    validate_network_mode_pointer(json_value, "/runtime/network/default")?;
    validate_network_mode_pointer(json_value, "/runtime/network/task_sandbox")?;
    validate_network_mode_pointer(json_value, "/runtime/network/agent")?;
    if let Some(secrets) = json_value.pointer("/runtime/secrets") {
        let items = secrets
            .as_array()
            .ok_or_else(|| anyhow!("/runtime/secrets must be an array"))?;
        for (idx, item) in items.iter().enumerate() {
            let context = format!("/runtime/secrets/{}", idx);
            let obj = item
                .as_object()
                .ok_or_else(|| anyhow!("{} must be an object", context))?;
            let name = obj
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("{}/name is required", context))?;
            let from = obj
                .get("from")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("{}/from is required", context))?;
            if !matches!(from, "env" | "file") {
                return Err(anyhow!(
                    "{}/from '{}' is not supported yet; supported providers are env and file",
                    context,
                    from
                ));
            }
            if name.contains('=') {
                return Err(anyhow!("{}/name must not contain '='", context));
            }
        }
    }
    Ok(())
}

fn validate_network_mode_pointer(json_value: &Value, pointer: &str) -> Result<()> {
    let Some(mode) = json_value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if matches!(mode, "none" | "full" | "allowlist_enforced" | "llm_egress") {
        Ok(())
    } else {
        Err(anyhow!(
            "{} must be one of: none, full, allowlist_enforced, llm_egress (got '{}')",
            pointer,
            mode
        ))
    }
}

fn validate_sidecars(json_value: &Value) -> Result<()> {
    let declared = json_value
        .pointer("/sidecars")
        .and_then(Value::as_object)
        .map(|sidecars| {
            sidecars
                .iter()
                .map(|(id, config)| {
                    if id.trim().is_empty() {
                        return Err(anyhow!("/sidecars contains an empty id"));
                    }
                    let lifecycle = config
                        .pointer("/lifecycle")
                        .and_then(Value::as_str)
                        .unwrap_or("per-trial");
                    if lifecycle != "per-trial" {
                        return Err(anyhow!(
                            "/sidecars/{} lifecycle '{}' is not supported; use per-trial",
                            id,
                            lifecycle
                        ));
                    }
                    if config.pointer("/image").and_then(Value::as_str).is_none() {
                        return Err(anyhow!("/sidecars/{} image is required", id));
                    }
                    Ok(id.clone())
                })
                .collect::<Result<std::collections::BTreeSet<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    for stage in ["agent", "grader"] {
        let Some(items) = json_value
            .pointer(&format!("/trial_runtime/{}/sidecars", stage))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for (idx, item) in items.iter().enumerate() {
            let id = item.as_str().ok_or_else(|| {
                anyhow!("/trial_runtime/{}/sidecars/{} must be a string", stage, idx)
            })?;
            if !declared.contains(id) {
                return Err(anyhow!(
                    "/trial_runtime/{}/sidecars/{} references unknown sidecar '{}'",
                    stage,
                    idx,
                    id
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_sanitization_profile_network_invariants(
    json_value: &Value,
    effective_task_network: Option<&str>,
) -> Result<()> {
    for (pointer, label) in [(
        "/policy/sanitization_profile",
        "policy.sanitization_profile",
    )] {
        if let Some(profile) = json_value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !matches!(
                profile,
                "replay_strict" | "hermetic_functional" | "perf_benchmark"
            ) {
                return Err(anyhow!(
                    "{} must be one of: replay_strict, hermetic_functional, perf_benchmark (got '{}')",
                    label,
                    profile
                ));
            }
        }
    }

    let hermetic_sources = [(
        "/policy/sanitization_profile",
        "policy.sanitization_profile",
    )]
    .iter()
    .filter_map(|(pointer, label)| {
        json_value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| (*label, value))
    })
    .filter(|(_, value)| *value == "hermetic_functional")
    .map(|(label, _)| label)
    .collect::<Vec<_>>();

    if hermetic_sources.is_empty() {
        return Ok(());
    }

    let task_network = effective_task_network.or_else(|| {
        json_value
            .pointer("/runtime/network/task_sandbox")
            .or_else(|| json_value.pointer("/runtime/network/default"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    });
    if task_network != Some("none") {
        return Err(anyhow!(
            "sanitization_profile=hermetic_functional requires runtime.network.task_sandbox/effective task network 'none' (declared by {}; got {})",
            hermetic_sources.join(", "),
            task_network.unwrap_or("<missing>")
        ));
    }

    if let Some(agent_network) = json_value
        .pointer("/runtime/network/agent")
        .or_else(|| json_value.pointer("/runtime/network/default"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if agent_network != "none" {
            return Err(anyhow!(
                "sanitization_profile=hermetic_functional requires runtime.network.agent 'none' when declared (got {})",
                agent_network
            ));
        }
    }

    Ok(())
}

fn validate_benchmark_artifacts(json_value: &Value) -> Result<()> {
    let Some(items) = json_value
        .pointer("/benchmark/artifacts")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for (idx, item) in items.iter().enumerate() {
        let context = format!("benchmark.artifacts[{}]", idx);
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("{}.id must be a non-empty string", context))?;
        let source_path = item
            .get("source_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("benchmark artifact '{}' missing source_path", id))?;
        validate_relative_artifact_path(
            source_path,
            &format!("benchmark artifact '{}'.source_path", id),
        )?;
        if let Some(summary_path) = item
            .get("summary_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            validate_relative_artifact_path(
                summary_path,
                &format!("benchmark artifact '{}'.summary_path", id),
            )?;
        }
    }
    Ok(())
}

fn validate_relative_artifact_path(raw: &str, field: &str) -> Result<()> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(anyhow!("{} must be relative", field));
    }
    let mut saw_component = false;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => saw_component = true,
            Component::ParentDir => return Err(anyhow!("{} must not contain '..'", field)),
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("{} must be relative", field))
            }
        }
    }
    if !saw_component {
        return Err(anyhow!("{} cannot resolve to empty", field));
    }
    Ok(())
}

pub(crate) fn validate_schema_contract_value(value: &Value, context: &str) -> Result<()> {
    let Some(schema_version) = value.pointer("/schema_version").and_then(Value::as_str) else {
        return Ok(());
    };
    let schema_name = format!("{}.jsonschema", schema_version);
    compile_schema(&schema_name).map_err(|err| {
        anyhow!(
            "missing schema contract for schema_version '{}' in {} (expected schemas/{}): {}",
            schema_version,
            context,
            schema_name,
            err
        )
    })?;
    Ok(())
}

pub(crate) fn load_experiment_overrides(overrides_path: &Path) -> Result<ExperimentOverrides> {
    let overrides_schema = compile_schema("experiment_overrides_v1.jsonschema")?;
    let overrides_data = fs::read_to_string(overrides_path)?;
    let overrides_json: Value = serde_json::from_str(&overrides_data)?;
    if let Err(errors) = overrides_schema.validate(&overrides_json) {
        let mut msgs = Vec::new();
        for e in errors {
            msgs.push(e.to_string());
        }
        return Err(anyhow!(
            "overrides schema validation failed ({}): {}",
            overrides_path.display(),
            msgs.join("; ")
        ));
    }
    let overrides: ExperimentOverrides = serde_json::from_value(overrides_json)?;
    if overrides.schema_version != "experiment_overrides_v1" {
        return Err(anyhow!(
            "unsupported overrides schema_version: {}",
            overrides.schema_version
        ));
    }
    Ok(overrides)
}

pub(crate) fn load_knob_manifest(manifest_path: &Path) -> Result<KnobManifest> {
    let manifest_schema = compile_schema("knob_manifest_v1.jsonschema")?;
    let manifest_data = fs::read_to_string(manifest_path)?;
    let manifest_json: Value = serde_json::from_str(&manifest_data)?;
    if let Err(errors) = manifest_schema.validate(&manifest_json) {
        let mut msgs = Vec::new();
        for e in errors {
            msgs.push(e.to_string());
        }
        return Err(anyhow!(
            "knob manifest schema validation failed ({}): {}",
            manifest_path.display(),
            msgs.join("; ")
        ));
    }
    let manifest: KnobManifest = serde_json::from_value(manifest_json)?;
    if manifest.schema_version != "knob_manifest_v1" {
        return Err(anyhow!(
            "unsupported knob manifest schema_version: {}",
            manifest.schema_version
        ));
    }
    Ok(manifest)
}

pub(crate) fn validate_knob_value(knob: &KnobDef, value: &Value) -> Result<()> {
    if !value_matches_type(value, &knob.value_type) {
        return Err(anyhow!(
            "override value type mismatch for knob {}: expected {}, got {}",
            knob.id,
            knob.value_type,
            value_type_name(value)
        ));
    }
    if let Some(options) = knob.options.as_ref() {
        if !options.iter().any(|option| option == value) {
            return Err(anyhow!(
                "override value for knob {} is not in allowed options",
                knob.id
            ));
        }
    }
    if let Some(minimum) = knob.minimum {
        if let Some(v) = value.as_f64() {
            if v < minimum {
                return Err(anyhow!(
                    "override value for knob {} is below minimum {}",
                    knob.id,
                    minimum
                ));
            }
        }
    }
    if let Some(maximum) = knob.maximum {
        if let Some(v) = value.as_f64() {
            if v > maximum {
                return Err(anyhow!(
                    "override value for knob {} is above maximum {}",
                    knob.id,
                    maximum
                ));
            }
        }
    }
    Ok(())
}

pub fn validate_knob_overrides(manifest_path: &Path, overrides_path: &Path) -> Result<()> {
    let manifest = load_knob_manifest(manifest_path)?;
    let overrides = load_experiment_overrides(overrides_path)?;
    let mut by_id: BTreeMap<String, KnobDef> = BTreeMap::new();
    for knob in manifest.knobs {
        by_id.insert(knob.id.clone(), knob);
    }
    for (id, value) in overrides.values.iter() {
        let knob = by_id
            .get(id)
            .ok_or_else(|| anyhow!("override references unknown knob id: {}", id))?;
        validate_knob_value(knob, value)?;
    }
    Ok(())
}
