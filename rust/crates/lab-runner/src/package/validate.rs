use anyhow::{anyhow, Result};
use lab_schemas::compile_schema;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};

use crate::config::{declared_extra_outputs, value_matches_type, value_type_name};
use crate::model::{ExperimentOverrides, KnobDef, KnobManifest};
use crate::package::authoring::normalize_authoring_vocabulary;
use crate::trial::plan::parse_trial_runtime_config;

pub(crate) fn validate_required_fields(json_value: &Value) -> Result<()> {
    let normalized;
    let json_value = if json_value.pointer("/cases").is_some()
        || json_value.pointer("/matrix/cases").is_some()
        || json_value.pointer("/stages").is_some()
        || json_value.pointer("/ephemerals").is_some()
        || json_value.pointer("/externals").is_some()
    {
        normalized = normalized_authoring_value(json_value)?;
        &normalized
    } else {
        json_value
    };
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
            "/trial_runtime/outputs",
            "declare runtime outputs under /trial_runtime/agent/outputs and downstream outputs under /trial_runtime/grader/outputs",
        ),
        (
            "/trial_runtime/grader/conclusion",
            "declare grader outputs and metrics instead of grader-owned trial_conclusion_v1 mapping",
        ),
    ] {
        if json_value.pointer(pointer).is_some() {
            return Err(anyhow!("{} is not supported; {}", pointer, message));
        }
    }
    reject_v0_authoring_paths(json_value)?;
    reject_unknown_top_level_sections(json_value)?;
    validate_runtime_declarations(json_value)?;
    validate_sidecars(json_value)?;
    let required: &[&str] = &[
        "/matrix/variants",
        "/matrix/tasks/source",
        "/matrix/tasks/path",
        "/matrix/repeats",
        "/runtime/network/task_sandbox",
        "/runtime/network/agent",
        "/policy/timeout_ms",
        "/trial_runtime/task/interface",
        "/trial_runtime/agent/command",
        "/trial_runtime/agent/artifact_type",
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
            .all(|part| part.as_str().is_some_and(|s| !s.trim().is_empty())),
        _ => false,
    };
    if !has_command {
        missing.push("/trial_runtime/agent/command");
    }
    if json_value
        .pointer("/experiment/id")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
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
    validate_extra_outputs(json_value)?;
    Ok(())
}

fn normalized_authoring_value(json_value: &Value) -> Result<Value> {
    let mut normalized = json_value.clone();
    normalize_authoring_vocabulary(&mut normalized)?;
    Ok(normalized)
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

fn reject_unknown_top_level_sections(json_value: &Value) -> Result<()> {
    let Some(object) = json_value.as_object() else {
        return Ok(());
    };
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "evaluation"
                | "experiment"
                | "extra_outputs"
                | "knobs"
                | "matrix"
                | "metrics"
                | "policy"
                | "runtime"
                | "scheduling"
                | "sidecars"
                | "stages"
                | "traces"
                | "trial_runtime"
                | "version"
        ) {
            return Err(anyhow!(
                "/{} is not supported in v1; remove it or move its fields under the v1 experiment model",
                key
            ));
        }
    }
    Ok(())
}

fn validate_runtime_declarations(json_value: &Value) -> Result<()> {
    let Some(runtime) = json_value.pointer("/runtime") else {
        return Err(anyhow!("missing /runtime"));
    };
    for (pointer, supported) in [
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
    if let Some(value) = runtime.pointer("/compute/backend").and_then(Value::as_str) {
        crate::experiment::state::executor_kind_from_compute_backend(value)?;
    }
    validate_network_mode_pointer(json_value, "/runtime/network/default", true)?;
    validate_network_mode_pointer(json_value, "/runtime/network/task_sandbox", false)?;
    validate_network_mode_pointer(json_value, "/runtime/network/agent", true)?;
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

fn validate_network_mode_pointer(
    json_value: &Value,
    pointer: &str,
    allow_llm_egress: bool,
) -> Result<()> {
    let Some(mode) = json_value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if matches!(mode, "none" | "full" | "allowlist_enforced")
        || (allow_llm_egress && mode == "llm_egress")
    {
        Ok(())
    } else {
        let allowed = if allow_llm_egress {
            "none, full, allowlist_enforced, llm_egress"
        } else {
            "none, full, allowlist_enforced"
        };
        Err(anyhow!(
            "{} must be one of: {} (got '{}')",
            pointer,
            allowed,
            mode
        ))
    }
}

fn validate_sidecars(json_value: &Value) -> Result<()> {
    let mut declared = BTreeSet::new();
    if let Some(sidecars) = json_value.pointer("/sidecars").and_then(Value::as_object) {
        for (id, config) in sidecars {
            if id.trim().is_empty() {
                return Err(anyhow!("/sidecars contains an empty id"));
            }
            if !is_portable_sidecar_id(id) {
                return Err(anyhow!(
                    "/sidecars/{} id must be a portable runtime alias: lowercase letters, numbers, and '-' only; it must start and end with a letter or number",
                    id
                ));
            }
            let lifecycle = config
                .pointer("/lifecycle")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("/sidecars/{} lifecycle is required", id))?;
            if lifecycle != "per-trial" {
                return Err(anyhow!(
                    "/sidecars/{} lifecycle '{}' is not supported; use per-trial",
                    id,
                    lifecycle
                ));
            }
            if config
                .pointer("/image")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
            {
                return Err(anyhow!("/sidecars/{} image is required", id));
            }
            if let Some(command) = config.pointer("/command") {
                let items = command
                    .as_array()
                    .ok_or_else(|| anyhow!("/sidecars/{} command must be an argv array", id))?;
                for (idx, item) in items.iter().enumerate() {
                    let Some(part) = item.as_str() else {
                        return Err(anyhow!("/sidecars/{} command/{} must be a string", id, idx));
                    };
                    if part.trim().is_empty() {
                        return Err(anyhow!(
                            "/sidecars/{} command/{} must not be empty",
                            id,
                            idx
                        ));
                    }
                }
            }
            if let Some(workdir) = config.pointer("/workdir") {
                let Some(workdir) = workdir.as_str() else {
                    return Err(anyhow!("/sidecars/{} workdir must be a string", id));
                };
                if workdir.trim().is_empty() {
                    return Err(anyhow!("/sidecars/{} workdir must not be empty", id));
                }
            }
            for field in ["env", "expose"] {
                let Some(object) = config.pointer(&format!("/{}", field)) else {
                    continue;
                };
                let object = object
                    .as_object()
                    .ok_or_else(|| anyhow!("/sidecars/{} {} must be an object", id, field))?;
                for (key, value) in object {
                    if key.trim().is_empty() {
                        return Err(anyhow!("/sidecars/{} {} contains an empty key", id, field));
                    }
                    if value.as_str().is_none() {
                        return Err(anyhow!(
                            "/sidecars/{} {}/{} must be a string",
                            id,
                            field,
                            key
                        ));
                    }
                }
            }
            declared.insert(id.clone());
        }
    }
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

fn is_portable_sidecar_id(id: &str) -> bool {
    let id = id.trim();
    if id.is_empty() || id.len() > 63 {
        return false;
    }
    let Some(first) = id.chars().next() else {
        return false;
    };
    let Some(last) = id.chars().next_back() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && (last.is_ascii_lowercase() || last.is_ascii_digit())
        && id
            .chars()
            .all(|ch| ch == '-' || ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

pub(crate) fn validate_sanitization_profile_network_invariants(
    json_value: &Value,
    effective_task_network: Option<&str>,
) -> Result<()> {
    let profile = json_value
        .pointer("/policy/sanitization_profile")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(profile) = profile {
        if !matches!(
            profile,
            "replay_strict" | "hermetic_functional" | "standard_runtime"
        ) {
            return Err(anyhow!(
                "policy.sanitization_profile must be one of: replay_strict, hermetic_functional, standard_runtime (got '{}')",
                profile
            ));
        }
    }
    if profile != Some("hermetic_functional") {
        return Ok(());
    }

    let task_network = effective_task_network.or_else(|| {
        json_value
            .pointer("/runtime/network/task_sandbox")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    });
    if task_network != Some("none") {
        return Err(anyhow!(
            "sanitization_profile=hermetic_functional requires runtime.network.task_sandbox/effective task network 'none' (declared by {}; got {}). For provider-backed or other networked agents, omit policy.sanitization_profile or set it to standard_runtime.",
            "policy.sanitization_profile",
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
                "sanitization_profile=hermetic_functional requires runtime.network.agent 'none' when declared (got {}). For provider-backed or other networked agents, omit policy.sanitization_profile or set it to standard_runtime.",
                agent_network
            ));
        }
    }

    Ok(())
}

fn validate_extra_outputs(json_value: &Value) -> Result<()> {
    let Some(items) = declared_extra_outputs(json_value) else {
        return Ok(());
    };
    for (idx, item) in items.iter().enumerate() {
        let context = format!("extra_outputs[{}]", idx);
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
            .ok_or_else(|| anyhow!("extra output '{}' missing source_path", id))?;
        validate_relative_extra_output_path(
            source_path,
            &format!("extra output '{}'.source_path", id),
        )?;
        let summary_path = item
            .get("summary_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("extra output '{}' missing summary_path", id))?;
        validate_relative_extra_output_path(
            summary_path,
            &format!("extra output '{}'.summary_path", id),
        )?;
    }
    Ok(())
}

fn validate_relative_extra_output_path(raw: &str, field: &str) -> Result<()> {
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
    let schema = compile_schema(&schema_name).map_err(|err| {
        anyhow!(
            "missing schema contract for schema_version '{}' in {} (expected schemas/{}): {}",
            schema_version,
            context,
            schema_name,
            err
        )
    })?;
    if !validates_payload_at_write_boundary(schema_version) {
        return Ok(());
    }
    if let Err(errors) = schema.validate(value) {
        let messages = errors.map(|err| err.to_string()).collect::<Vec<_>>();
        return Err(anyhow!(
            "{} schema validation failed ({}): {}",
            schema_version,
            context,
            messages.join("; ")
        ));
    }
    Ok(())
}

fn validates_payload_at_write_boundary(schema_version: &str) -> bool {
    matches!(
        schema_version,
        "artifact_envelope_v1"
            | "grader_input_v1"
            | "package_checks_v1"
            | "prepared_task_environment_v1"
            | "sealed_package_lock_v1"
            | "state_inventory_v1"
            | "task_row_v2"
            | "trial_claim_intent_v1"
            | "trial_conclusion_v1"
            | "trial_input_v1"
    )
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
