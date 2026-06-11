use anyhow::{anyhow, Result};
use lab_schemas::{compile_schema, format_validation_error};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

use crate::config::{
    parse_metric_definitions, parse_string_array_field, value_matches_type, value_type_name,
};
use crate::model::{ExperimentOverrides, KnobDef, KnobManifest};
use crate::package::authoring::{normalize_authoring_vocabulary, reject_legacy_authoring_surface};
use crate::trial::plan::parse_trial_runtime_config;

pub(crate) fn validate_required_fields(json_value: &Value) -> Result<()> {
    let normalized;
    let authoring_surface = json_value.pointer("/cases").is_some()
        || json_value.pointer("/matrix/cases").is_some()
        || json_value.pointer("/stages").is_some()
        || json_value.pointer("/ephemerals").is_some()
        || json_value.pointer("/externals").is_some();
    let json_value = if authoring_surface {
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
    reject_v0_authoring_paths(json_value)
        .map_err(|err| public_authoring_error(err, authoring_surface))?;
    reject_unknown_top_level_sections(json_value)
        .map_err(|err| public_authoring_error(err, authoring_surface))?;
    validate_sidecars(json_value).map_err(|err| public_authoring_error(err, authoring_surface))?;
    let mut contract_value = json_value.clone();
    strip_packaging_only_trial_runtime_catalogs(&mut contract_value);
    let deferred_schema_error =
        match validate_resolved_experiment_schema(&contract_value, "experiment validation") {
            Ok(()) => None,
            Err(err) if err.to_string().contains("'oneOf'") => Some(err),
            Err(err) => return Err(public_authoring_error(err, authoring_surface)),
        };
    parse_trial_runtime_config(&contract_value)
        .map_err(|err| public_authoring_error(err, authoring_surface))?;
    if let Some(err) = deferred_schema_error {
        return Err(public_authoring_error(err, authoring_surface));
    }
    Ok(())
}

pub(crate) fn strip_packaging_only_trial_runtime_catalogs(experiment: &mut Value) {
    if let Some(trial_runtime) = experiment.pointer_mut("/trial_runtime") {
        strip_packaging_only_trial_runtime_fields(trial_runtime);
    }
    if let Some(variants) = experiment
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    {
        for variant in variants {
            if let Some(overrides) = variant.get_mut("overrides") {
                strip_packaging_only_trial_runtime_fields(overrides);
            }
        }
    }
}

fn strip_packaging_only_trial_runtime_fields(trial_runtime_root: &mut Value) {
    if let Some(grader) = trial_runtime_root
        .pointer_mut("/grader")
        .and_then(Value::as_object_mut)
    {
        grader.remove("_runtime_assets");
    }
    if let Some(image) = trial_runtime_root
        .pointer_mut("/task/workspace/image")
        .and_then(Value::as_object_mut)
    {
        image.remove("rewrites");
    }
}

pub(crate) fn public_authoring_error(err: anyhow::Error, authoring_surface: bool) -> anyhow::Error {
    if !authoring_surface {
        return err;
    }
    let mut message = err.to_string();
    for (internal, public) in [
        ("/trial_runtime/agent/sidecars", "/stages/agent/ephemerals"),
        (
            "/trial_runtime/grader/sidecars",
            "/stages/grader/ephemerals",
        ),
        ("/trial_runtime/task", "/stages/case"),
        ("/trial_runtime/agent", "/stages/agent"),
        ("/trial_runtime/execution", "/stages/execution"),
        ("/trial_runtime/grader", "/stages/grader"),
        ("/trial_runtime", "/stages"),
        ("/matrix/tasks", "/matrix/cases"),
        ("/sidecars", "/ephemerals"),
        ("trial_runtime.agent.sidecars", "stages.agent.ephemerals"),
        ("trial_runtime.grader.sidecars", "stages.grader.ephemerals"),
        ("trial_runtime.task", "stages.case"),
        ("trial_runtime.agent", "stages.agent"),
        ("trial_runtime.execution", "stages.execution"),
        ("trial_runtime.grader", "stages.grader"),
        ("trial_runtime", "stages"),
        ("matrix.tasks", "matrix.cases"),
        ("sidecars", "ephemerals"),
        ("sidecar", "ephemeral"),
    ] {
        message = message.replace(internal, public);
    }
    anyhow!(message)
}

fn normalized_authoring_value(json_value: &Value) -> Result<Value> {
    reject_legacy_authoring_surface(json_value)?;
    let mut normalized = json_value.clone();
    normalize_authoring_vocabulary(&mut normalized)
        .map_err(|err| public_authoring_error(err, true))?;
    Ok(normalized)
}

fn reject_v0_authoring_paths(json_value: &Value) -> Result<()> {
    for (pointer, replacement) in legacy_v0_path_replacements() {
        if json_value.pointer(pointer).is_some() {
            return Err(anyhow!(
                "{} is not supported in v1; move to {}",
                pointer,
                replacement
            ));
        }
    }
    Ok(())
}

fn legacy_v0_path_replacements() -> &'static [(&'static str, &'static str)] {
    &[
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
    ]
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
                | "matrix"
                | "metrics"
                | "policy"
                | "runtime"
                | "scheduling"
                | "sidecars"
                | "stages"
                | "task_runtime"
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

fn validate_sidecars(json_value: &Value) -> Result<()> {
    let mut declared = BTreeSet::new();
    let sidecars = json_value.pointer("/sidecars").and_then(Value::as_object);
    if let Some(sidecars) = sidecars {
        for id in sidecars.keys() {
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
        let mut seen_refs = BTreeSet::new();
        let mut duplicate_refs = BTreeSet::new();
        let mut exposed_env = BTreeMap::new();
        let mut duplicate_env = BTreeSet::new();
        for (idx, item) in items.iter().enumerate() {
            let Some(id) = item.as_str() else {
                continue;
            };
            if !seen_refs.insert(id.to_string()) {
                duplicate_refs.insert(id.to_string());
            }
            if !declared.contains(id) {
                return Err(anyhow!(
                    "/trial_runtime/{}/sidecars/{} references unknown sidecar '{}'",
                    stage,
                    idx,
                    id
                ));
            }
            let Some(expose) = sidecars
                .and_then(|sidecars| sidecars.get(id))
                .and_then(|sidecar| sidecar.get("expose"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            for env_name in expose.keys().map(String::as_str).map(str::trim) {
                if env_name.is_empty() {
                    continue;
                }
                if let Some(previous) = exposed_env.insert(env_name.to_string(), id.to_string()) {
                    duplicate_env.insert(format!("{} ({} and {})", env_name, previous, id));
                }
            }
        }
        if !duplicate_refs.is_empty() {
            return Err(anyhow!(
                "/trial_runtime/{}/sidecars must reference each sidecar at most once (duplicates: {})",
                stage,
                format_set(&duplicate_refs)
            ));
        }
        if !duplicate_env.is_empty() {
            return Err(anyhow!(
                "/trial_runtime/{}/sidecars expose duplicate env names: {}",
                stage,
                format_set(&duplicate_env)
            ));
        }
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

pub(crate) fn validate_resolved_experiment_schema(value: &Value, context: &str) -> Result<()> {
    reject_resolved_legacy_v0_surfaces(value, context)?;
    reject_resolved_authoring_only_surfaces(value, context)?;
    reject_resolved_unsupported_surfaces(value, context)?;
    reject_resolved_variant_authoring_syntax(value, context)?;
    reject_resolved_metric_authoring_syntax(value, context)?;
    validate_resolved_runtime_accounting_uniqueness(value, context)?;
    let schema = compile_schema("resolved_experiment.jsonschema")?;
    if let Some(errors) = schema.validate(value).err() {
        let messages = errors
            .map(|err| format_validation_error(&err))
            .collect::<Vec<_>>();
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): {}",
            context,
            messages.join("; ")
        ));
    }
    validate_sidecars(value)?;
    validate_resolved_variant_contract(value, context)?;
    validate_resolved_metric_ids(value, context)?;
    validate_resolved_metric_primary_contract(value, context)?;
    validate_resolved_metric_source_contract(value, context)?;
    validate_resolved_external_accounting_contract(value, context)?;
    validate_resolved_agent_output_mount_contract(value, context)?;
    validate_resolved_runtime_template_bindings(value, context)?;
    validate_resolved_event_placeholders(value, context)?;
    validate_resolved_hidden_path_contract(value, context)
}

fn reject_resolved_legacy_v0_surfaces(value: &Value, context: &str) -> Result<()> {
    if let Some(version) = value.pointer("/version").and_then(Value::as_str) {
        if version.trim() == "1.0" {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): experiment version '1.0' is not supported",
                context
            ));
        }
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): /version is not supported in v1; resolved packages are schema-versioned by the package manifest, not the experiment document",
            context
        ));
    }
    for (pointer, replacement) in legacy_v0_path_replacements() {
        if value.pointer(pointer).is_some() {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): {} is legacy v0 vocabulary; resolved packages must use {}",
                context,
                pointer,
                replacement
            ));
        }
    }
    Ok(())
}

fn reject_resolved_authoring_only_surfaces(value: &Value, context: &str) -> Result<()> {
    for (pointer, message) in [
        (
            "/stages",
            "/stages is authoring-only stage vocabulary; resolved packages must carry concrete stage runtime under /trial_runtime",
        ),
        (
            "/ephemerals",
            "/ephemerals is authoring-only service vocabulary; resolved packages must carry concrete service declarations under /sidecars",
        ),
        (
            "/externals",
            "/externals is authoring-only accounting vocabulary; resolved packages must carry derived external accounting under /runtime/externals",
        ),
        (
            "/matrix/cases",
            "/matrix/cases is authoring-only case matrix vocabulary; resolved packages must carry concrete task input under /matrix/tasks",
        ),
        (
            "/traces",
            "/traces is authoring-only trace intent; resolved packages must carry concrete trace ingestion sinks under /trial_runtime/agent/events",
        ),
        (
            "/runtime/network/default",
            "/runtime/network/default is authoring-only shorthand; resolved packages must declare /runtime/network/task_sandbox and /runtime/network/agent explicitly",
        ),
        (
            "/runtime/registry",
            "/runtime/registry is authoring-only package-build input; resolved packages must carry rewritten case images directly",
        ),
        (
            "/experiment/mode",
            "/experiment/mode is authoring-only evaluation intent; resolved packages must carry explicit metrics and grader contracts instead",
        ),
        (
            "/scheduling/comparison",
            "/scheduling/comparison is authoring-only design intent; resolved packages must carry the concrete run order under /policy/policies/scheduling",
        ),
    ] {
        if value.pointer(pointer).is_some() {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): {}",
                context,
                message
            ));
        }
    }
    Ok(())
}

fn reject_resolved_unsupported_surfaces(value: &Value, context: &str) -> Result<()> {
    for (pointer, message) in [
        (
            "/runtime/storage",
            "/runtime/storage is not part of the experiment contract; storage and trace sinks are runner-owned today, so remove this no-op backend declaration",
        ),
        (
            "/runtime/traces",
            "/runtime/traces is not part of the experiment contract; storage and trace sinks are runner-owned today, so remove this no-op backend declaration",
        ),
        (
            "/task_runtime",
            "/task_runtime is not supported; define task behavior under /trial_runtime/task",
        ),
        (
            "/trial_runtime/outputs",
            "/trial_runtime/outputs is not supported; declare runtime outputs under /trial_runtime/agent/outputs and downstream outputs under /trial_runtime/grader/outputs",
        ),
        (
            "/trial_runtime/grader/conclusion",
            "/trial_runtime/grader/conclusion is not supported; declare grader outputs and metrics instead of grader-owned trial_conclusion_v1 mapping",
        ),
        (
            "/trial_runtime/agent/protocol",
            "/trial_runtime/agent/protocol is not supported; command invocation is implied by /trial_runtime/agent/command",
        ),
    ] {
        if value.pointer(pointer).is_some() {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): {}",
                context,
                message
            ));
        }
    }
    Ok(())
}

fn reject_resolved_variant_authoring_syntax(value: &Value, context: &str) -> Result<()> {
    let Some(variants) = value.pointer("/matrix/variants").and_then(Value::as_array) else {
        return Ok(());
    };
    for (idx, variant) in variants.iter().enumerate() {
        let Some(variant) = variant.as_object() else {
            continue;
        };
        for (field, replacement) in [
            ("bindings", "config"),
            ("runtime_overrides", "overrides"),
            ("variant_id", "id"),
        ] {
            if variant.contains_key(field) {
                return Err(anyhow!(
                    "resolved experiment schema validation failed ({}): /matrix/variants/{}/{} is legacy variant vocabulary; resolved packages must use /matrix/variants/{}/{}",
                    context,
                    idx,
                    field,
                    idx,
                    replacement
                ));
            }
        }
        if variant.contains_key("image") {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): /matrix/variants/{}/image is legacy variant vocabulary; resolved packages must use /matrix/variants/{}/overrides/agent/image",
                context,
                idx,
                idx
            ));
        }
        let Some(overrides) = variant.get("overrides").and_then(Value::as_object) else {
            continue;
        };
        for key in overrides.keys() {
            if matches!(key.as_str(), "agent" | "task" | "execution" | "grader") {
                continue;
            }
            let hint = if key == "case" {
                "authoring YAML should use /matrix/variants[].overrides.case, but resolved packages must contain lowered /matrix/variants[].overrides.task"
            } else {
                "resolved variant overrides patch /trial_runtime only"
            };
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): /matrix/variants/{}/overrides/{} is not supported; {}",
                context,
                idx,
                key,
                hint
            ));
        }
    }
    Ok(())
}

fn reject_resolved_metric_authoring_syntax(value: &Value, context: &str) -> Result<()> {
    let Some(metrics) = value.pointer("/metrics").and_then(Value::as_array) else {
        return Ok(());
    };
    for (idx, metric) in metrics.iter().enumerate() {
        if metric.pointer("/from").is_some() {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): /metrics/{} uses authoring-only field 'from'; resolved packages must carry /metrics/{}/source",
                context,
                idx,
                idx
            ));
        }
        if metric.pointer("/transform").is_some() {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): /metrics/{} uses authoring-only field 'transform'; resolved packages must carry /metrics/{}/source/transform",
                context,
                idx,
                idx
            ));
        }
    }
    Ok(())
}

fn validate_resolved_runtime_accounting_uniqueness(value: &Value, context: &str) -> Result<()> {
    reject_duplicate_string_array(
        value.pointer("/runtime/externals/credentials"),
        "/runtime/externals/credentials",
        context,
    )?;
    reject_duplicate_string_array(
        value.pointer("/runtime/externals/apis"),
        "/runtime/externals/apis",
        context,
    )?;
    reject_duplicate_string_array(
        value.pointer("/runtime/network/egress"),
        "/runtime/network/egress",
        context,
    )?;
    reject_duplicate_runtime_secret_names(value, context)?;
    reject_duplicate_credential_cache_env(value, context)?;
    Ok(())
}

fn reject_duplicate_string_array(
    value: Option<&Value>,
    pointer: &str,
    context: &str,
) -> Result<()> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Ok(());
    };
    let mut seen = BTreeSet::new();
    for (idx, item) in items.iter().enumerate() {
        let Some(item) = item.as_str().map(str::trim).filter(|item| !item.is_empty()) else {
            continue;
        };
        if !seen.insert(item.to_string()) {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): {}/{} duplicates '{}'; {} values must be unique",
                context,
                pointer,
                idx,
                item,
                pointer
            ));
        }
    }
    Ok(())
}

fn reject_duplicate_runtime_secret_names(value: &Value, context: &str) -> Result<()> {
    let Some(secrets) = value.pointer("/runtime/secrets").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut seen = BTreeSet::new();
    for (idx, secret) in secrets.iter().enumerate() {
        let Some(name) = secret
            .pointer("/name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        if !seen.insert(name.to_string()) {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): /runtime/secrets/{} duplicates secret name '{}'; runtime secret names must be unique",
                context,
                idx,
                name
            ));
        }
    }
    Ok(())
}

fn reject_duplicate_credential_cache_env(value: &Value, context: &str) -> Result<()> {
    let Some(secrets) = value.pointer("/runtime/secrets").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut seen = BTreeSet::new();
    for (idx, secret) in secrets.iter().enumerate() {
        let Some(env) = secret
            .pointer("/credential_cache/env")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|env| !env.is_empty())
        else {
            continue;
        };
        if !seen.insert(env.to_string()) {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): /runtime/secrets/{}/credential_cache/env duplicates '{}'; credential cache env names must be unique",
                context,
                idx,
                env
            ));
        }
    }
    Ok(())
}

fn validate_resolved_variant_contract(value: &Value, context: &str) -> Result<()> {
    let Some(variants) = value.pointer("/matrix/variants").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    let mut baseline_count = 0usize;
    for variant in variants {
        if let Some(id) = variant
            .pointer("/id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !seen.insert(id.to_string()) {
                duplicates.insert(id.to_string());
            }
        }
        if variant.pointer("/baseline").and_then(Value::as_bool) == Some(true) {
            baseline_count += 1;
        }
    }
    if !duplicates.is_empty() {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): /matrix/variants must declare unique ids (duplicates: {})",
            context,
            duplicates.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if baseline_count != 1 {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): /matrix/variants must declare exactly one baseline variant (got {})",
            context,
            baseline_count
        ));
    }
    Ok(())
}

fn validate_resolved_metric_ids(value: &Value, context: &str) -> Result<()> {
    let Some(metrics) = value.pointer("/metrics").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for metric in metrics {
        if let Some(id) = metric
            .pointer("/id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !seen.insert(id.to_string()) {
                duplicates.insert(id.to_string());
            }
        }
    }
    if !duplicates.is_empty() {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): /metrics must declare unique ids (duplicates: {})",
            context,
            duplicates.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(())
}

fn validate_resolved_agent_output_mount_contract(value: &Value, context: &str) -> Result<()> {
    validate_output_mount_uniqueness(
        value.pointer("/trial_runtime/agent/output_mounts"),
        "base agent output_mounts",
        context,
    )?;

    if let Some(variants) = value.pointer("/matrix/variants").and_then(Value::as_array) {
        for (idx, variant) in variants.iter().enumerate() {
            let variant_id = variant
                .pointer("/id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("<unknown>");
            validate_output_mount_uniqueness(
                variant.pointer("/overrides/agent/output_mounts"),
                &format!("variant {variant_id} agent output_mounts at matrix.variants[{idx}]"),
                context,
            )?;
        }
    }

    Ok(())
}

fn validate_output_mount_uniqueness(
    value: Option<&Value>,
    label: &str,
    context: &str,
) -> Result<()> {
    let Some(mounts) = value.and_then(Value::as_array) else {
        return Ok(());
    };
    let mut seen_ids = BTreeSet::new();
    let mut duplicate_ids = BTreeSet::new();
    let mut seen_paths = BTreeSet::new();
    let mut duplicate_paths = BTreeSet::new();
    let mut seen_envs = BTreeSet::new();
    let mut duplicate_envs = BTreeSet::new();

    for mount in mounts {
        if let Some(id) = mount
            .pointer("/id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !seen_ids.insert(id.to_string()) {
                duplicate_ids.insert(id.to_string());
            }
        }
        if let Some(path) = mount
            .pointer("/path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !seen_paths.insert(path.to_string()) {
                duplicate_paths.insert(path.to_string());
            }
        }
        if let Some(env) = mount
            .pointer("/env")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !seen_envs.insert(env.to_string()) {
                duplicate_envs.insert(env.to_string());
            }
        }
    }

    if !duplicate_ids.is_empty() || !duplicate_paths.is_empty() || !duplicate_envs.is_empty() {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): {} must declare unique ids, paths, and env names (duplicate ids: {}; duplicate paths: {}; duplicate env: {})",
            context,
            label,
            format_set(&duplicate_ids),
            format_set(&duplicate_paths),
            format_set(&duplicate_envs)
        ));
    }

    Ok(())
}

fn validate_resolved_metric_primary_contract(value: &Value, context: &str) -> Result<()> {
    let Some(metrics) = value.pointer("/metrics").and_then(Value::as_array) else {
        return Ok(());
    };
    let primary_count = metrics
        .iter()
        .filter(|metric| metric.pointer("/primary").and_then(Value::as_bool) == Some(true))
        .count();
    if primary_count > 1 {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): /metrics must declare exactly one primary metric (got {})",
            context,
            primary_count
        ));
    }
    Ok(())
}

fn validate_resolved_metric_source_contract(value: &Value, context: &str) -> Result<()> {
    let metrics = parse_metric_definitions(value)?;
    let grader_outputs = object_keys_at(value, "/trial_runtime/grader/outputs");
    let runtime_outputs = object_keys_at(value, "/trial_runtime/agent/outputs");

    for metric in metrics {
        let Some(output) = metric
            .definition_json
            .pointer("/source/output")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        match metric.source.source_type.as_str() {
            "grader_output" => {
                if !grader_outputs.contains(output) {
                    return Err(anyhow!(
                        "resolved experiment schema validation failed ({}): metric '{}' uses from: grader.{}... but trial_runtime.grader.outputs.{} is not declared",
                        context,
                        metric.id,
                        output,
                        output
                    ));
                }
            }
            "runtime_output" => {
                let output_id = crate::trial::agent_output_id(output);
                if !runtime_outputs.contains(output_id) {
                    return Err(anyhow!(
                        "resolved experiment schema validation failed ({}): metrics.{} references unknown runtime output '{}'",
                        context,
                        metric.id,
                        output
                    ));
                }
                if output_id != "result" {
                    return Err(anyhow!(
                        "resolved experiment schema validation failed ({}): metrics.{} references runtime output '{}', but only the canonical result output is currently persisted into metric extraction without a grader",
                        context,
                        metric.id,
                        output
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn validate_resolved_external_accounting_contract(value: &Value, context: &str) -> Result<()> {
    let external_credentials =
        string_set_from_array(value.pointer("/runtime/externals/credentials"));
    let runtime_secrets = runtime_secret_names(value);
    let missing_secret_declarations = external_credentials
        .difference(&runtime_secrets)
        .cloned()
        .collect::<Vec<_>>();
    let missing_external_credential_accounting = runtime_secrets
        .difference(&external_credentials)
        .cloned()
        .collect::<Vec<_>>();
    if !missing_secret_declarations.is_empty() || !missing_external_credential_accounting.is_empty()
    {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): /runtime/externals/credentials must match /runtime/secrets names (missing secrets: {}; missing externals: {})",
            context,
            format_list(&missing_secret_declarations),
            format_list(&missing_external_credential_accounting)
        ));
    }

    let external_apis = string_set_from_array(value.pointer("/runtime/externals/apis"));
    let network_egress = string_set_from_array(value.pointer("/runtime/network/egress"));
    let missing_egress_policy = external_apis
        .difference(&network_egress)
        .cloned()
        .collect::<Vec<_>>();
    let missing_external_api_accounting = network_egress
        .difference(&external_apis)
        .cloned()
        .collect::<Vec<_>>();
    if !missing_egress_policy.is_empty() || !missing_external_api_accounting.is_empty() {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): /runtime/externals/apis must match /runtime/network/egress (missing egress: {}; missing externals: {})",
            context,
            format_list(&missing_egress_policy),
            format_list(&missing_external_api_accounting)
        ));
    }

    Ok(())
}

fn validate_resolved_hidden_path_contract(value: &Value, context: &str) -> Result<()> {
    let hidden_paths = parse_string_array_field(
        value.pointer("/trial_runtime/grader/in_task_runtime/hidden_paths"),
        "trial_runtime.grader.in_task_runtime.hidden_paths",
    )?;
    if hidden_paths.is_empty() {
        return Ok(());
    }
    let output_mount_paths =
        parse_output_mount_paths(value.pointer("/trial_runtime/agent/output_mounts"))?;
    let overlaps = hidden_paths
        .iter()
        .filter(|hidden| {
            output_mount_paths
                .iter()
                .any(|mount| path_prefix_overlaps(hidden, mount))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !overlaps.is_empty() {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): hidden grader paths overlap agent output mounts: {}",
            context,
            overlaps.join(", ")
        ));
    }
    Ok(())
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

fn path_prefix_overlaps(a: &str, b: &str) -> bool {
    let a = a.trim_matches('/');
    let b = b.trim_matches('/');
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.starts_with(&format!("{}/", b)) || b.starts_with(&format!("{}/", a))
}

fn object_keys_at(value: &Value, pointer: &str) -> BTreeSet<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_object)
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

fn runtime_secret_names(value: &Value) -> BTreeSet<String> {
    let Some(items) = value.pointer("/runtime/secrets").and_then(Value::as_array) else {
        return BTreeSet::new();
    };
    items
        .iter()
        .filter_map(|item| {
            item.pointer("/name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
        .collect()
}

fn string_set_from_array(value: Option<&Value>) -> BTreeSet<String> {
    let Some(items) = value.and_then(Value::as_array) else {
        return BTreeSet::new();
    };
    items
        .iter()
        .filter_map(|item| {
            item.as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .collect()
}

fn format_set(items: &BTreeSet<String>) -> String {
    if items.is_empty() {
        "<none>".to_string()
    } else {
        items.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn format_list(items: &[String]) -> String {
    if items.is_empty() {
        "<none>".to_string()
    } else {
        items.join(", ")
    }
}

fn validate_resolved_runtime_template_bindings(value: &Value, context: &str) -> Result<()> {
    let declared_env = declared_env_secret_names(value);
    let base_command_refs = template_refs_in_array(value.pointer("/trial_runtime/agent/command"));
    let base_env_refs = template_refs_in_env(value.pointer("/trial_runtime/agent/env"));
    let Some(variants) = value.pointer("/matrix/variants").and_then(Value::as_array) else {
        return Ok(());
    };

    let mut missing_bindings = Vec::new();
    let mut non_scalar_bindings = Vec::new();
    let mut removed_syntax = Vec::new();
    for (variant_idx, variant) in variants.iter().enumerate() {
        let variant_id = variant
            .pointer("/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("variant[{variant_idx}]"));
        let Some(config) = variant.pointer("/config").and_then(Value::as_object) else {
            return Err(anyhow!(
                "resolved experiment schema validation failed ({}): /matrix/variants[{}].config is required and must be an object",
                context,
                variant_idx
            ));
        };
        let override_command = variant.pointer("/overrides/agent/command");
        let command_refs = override_command
            .map(|value| template_refs_in_array(Some(value)))
            .unwrap_or_else(|| base_command_refs.clone());
        let mut env_refs = base_env_refs.clone();
        merge_template_refs(
            &mut env_refs,
            template_refs_in_env(variant.pointer("/overrides/agent/env")),
        );
        let mut refs = command_refs.clone();
        merge_template_refs(&mut refs, env_refs.clone());

        for refs_by_field in [&command_refs, &env_refs] {
            for (field, refs) in refs_by_field {
                if refs.removed_syntax {
                    removed_syntax.push(format!("{variant_id}: {field}"));
                }
            }
        }

        for name in unique_template_ref_names(&refs) {
            if name == "WORKSPACE" || declared_env.contains(&name) {
                continue;
            }
            match config.get(&name) {
                Some(value) if is_runtime_binding_scalar(value) => {}
                Some(_) => non_scalar_bindings.push(format!("{variant_id}: ${name}")),
                None => missing_bindings.push(format!("{variant_id}: ${name}")),
            }
        }
    }

    if !missing_bindings.is_empty() || !non_scalar_bindings.is_empty() || !removed_syntax.is_empty()
    {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): /trial_runtime/agent runtime templates must resolve from variant config, declared env secrets, or built-ins (missing: {}; non-scalar: {}; removed syntax: {})",
            context,
            format_list(&missing_bindings),
            format_list(&non_scalar_bindings),
            format_list(&removed_syntax)
        ));
    }

    Ok(())
}

fn validate_resolved_event_placeholders(value: &Value, context: &str) -> Result<()> {
    let base_command_refs =
        event_placeholders_in_array(value.pointer("/trial_runtime/agent/command"));
    let base_env_refs = event_placeholders_in_env(value.pointer("/trial_runtime/agent/env"));
    let base_event_ids = event_sink_ids(value.pointer("/trial_runtime/agent/events"));
    let Some(variants) = value.pointer("/matrix/variants").and_then(Value::as_array) else {
        return Ok(());
    };

    let mut removed_placeholders = Vec::new();
    let mut unknown_events = Vec::new();
    let mut malformed = Vec::new();
    let mut unsupported_env = Vec::new();
    for (variant_idx, variant) in variants.iter().enumerate() {
        let variant_id = variant
            .pointer("/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("variant[{variant_idx}]"));
        let event_ids = variant
            .pointer("/overrides/agent/events")
            .map(|events| event_sink_ids(Some(events)))
            .unwrap_or_else(|| base_event_ids.clone());
        let command_refs = variant
            .pointer("/overrides/agent/command")
            .map(|command| event_placeholders_in_array(Some(command)))
            .unwrap_or_else(|| base_command_refs.clone());
        let env_refs =
            merge_event_env_placeholders(&base_env_refs, variant.pointer("/overrides/agent/env"));

        for (field, refs) in command_refs {
            if refs.removed_trajectory_placeholder {
                removed_placeholders.push(format!(
                    "{variant_id}: {field}: __BUCEPHALUS_TRAJECTORY_PATH__"
                ));
            }
            if refs.malformed {
                malformed.push(format!("{variant_id}: {field}"));
            }
            for event_id in refs.event_ids {
                if !event_ids.contains(&event_id) {
                    unknown_events.push(format!("{variant_id}: {field}: {event_id}"));
                }
            }
        }
        for field in env_refs.keys() {
            unsupported_env.push(format!("{variant_id}: {field}"));
        }
    }

    if !removed_placeholders.is_empty()
        || !unknown_events.is_empty()
        || !malformed.is_empty()
        || !unsupported_env.is_empty()
    {
        return Err(anyhow!(
            "resolved experiment schema validation failed ({}): agent command event placeholders must use declared __BUCEPHALUS_EVENT_PATH_<id>__ sinks, and agent env values must not contain event path placeholders (removed placeholders: {}; unknown event ids: {}; malformed placeholders: {}; unsupported env placeholders: {})",
            context,
            format_list(&removed_placeholders),
            format_list(&unknown_events),
            format_list(&malformed),
            format_list(&unsupported_env)
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, Default)]
struct EventPlaceholders {
    removed_trajectory_placeholder: bool,
    event_ids: BTreeSet<String>,
    malformed: bool,
}

fn event_sink_ids(value: Option<&Value>) -> BTreeSet<String> {
    let Some(events) = value.and_then(Value::as_array) else {
        return BTreeSet::new();
    };
    events
        .iter()
        .filter_map(|event| {
            event
                .pointer("/id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        })
        .collect()
}

fn event_placeholders_in_array(value: Option<&Value>) -> BTreeMap<String, EventPlaceholders> {
    let mut refs = BTreeMap::new();
    let Some(items) = value.and_then(Value::as_array) else {
        return refs;
    };
    for (idx, item) in items.iter().enumerate() {
        if let Some(raw) = item.as_str() {
            let placeholders = event_placeholders(raw);
            if placeholders.removed_trajectory_placeholder
                || placeholders.malformed
                || !placeholders.event_ids.is_empty()
            {
                refs.insert(format!("agent.command[{idx}]"), placeholders);
            }
        }
    }
    refs
}

fn event_placeholders_in_env(value: Option<&Value>) -> BTreeMap<String, EventPlaceholders> {
    let mut refs = BTreeMap::new();
    let Some(env) = value.and_then(Value::as_object) else {
        return refs;
    };
    for (key, item) in env {
        if let Some(raw) = item.as_str() {
            let placeholders = event_placeholders(raw);
            if placeholders.removed_trajectory_placeholder
                || placeholders.malformed
                || !placeholders.event_ids.is_empty()
            {
                refs.insert(format!("agent.env.{key}"), placeholders);
            }
        }
    }
    refs
}

fn merge_event_env_placeholders(
    base: &BTreeMap<String, EventPlaceholders>,
    override_env: Option<&Value>,
) -> BTreeMap<String, EventPlaceholders> {
    let mut refs = base.clone();
    let Some(env) = override_env.and_then(Value::as_object) else {
        return refs;
    };
    for (key, item) in env {
        let field = format!("agent.env.{key}");
        refs.remove(&field);
        if let Some(raw) = item.as_str() {
            let placeholders = event_placeholders(raw);
            if placeholders.removed_trajectory_placeholder
                || placeholders.malformed
                || !placeholders.event_ids.is_empty()
            {
                refs.insert(field, placeholders);
            }
        }
    }
    refs
}

fn event_placeholders(raw: &str) -> EventPlaceholders {
    let mut placeholders = EventPlaceholders {
        removed_trajectory_placeholder: raw.contains("__BUCEPHALUS_TRAJECTORY_PATH__"),
        event_ids: BTreeSet::new(),
        malformed: false,
    };
    let marker = "__BUCEPHALUS_EVENT_PATH_";
    let mut rest = raw;
    while let Some(start) = rest.find(marker) {
        let after_marker = &rest[start + marker.len()..];
        let Some(end) = after_marker.find("__") else {
            placeholders.malformed = true;
            break;
        };
        let event_id = after_marker[..end].trim();
        if event_id.is_empty() {
            placeholders.malformed = true;
        } else {
            placeholders.event_ids.insert(event_id.to_string());
        }
        rest = &after_marker[end + 2..];
    }
    placeholders
}

#[derive(Debug, Clone, Default)]
struct TemplateRefs {
    names: BTreeSet<String>,
    removed_syntax: bool,
}

fn template_refs_in_array(value: Option<&Value>) -> BTreeMap<String, TemplateRefs> {
    let mut refs = BTreeMap::new();
    let Some(items) = value.and_then(Value::as_array) else {
        return refs;
    };
    for (idx, item) in items.iter().enumerate() {
        if let Some(raw) = item.as_str() {
            refs.insert(format!("agent.command[{idx}]"), runtime_template_refs(raw));
        }
    }
    refs
}

fn template_refs_in_env(value: Option<&Value>) -> BTreeMap<String, TemplateRefs> {
    let mut refs = BTreeMap::new();
    let Some(env) = value.and_then(Value::as_object) else {
        return refs;
    };
    for (key, item) in env {
        if let Some(raw) = item.as_str() {
            refs.insert(format!("agent.env.{key}"), runtime_template_refs(raw));
        }
    }
    refs
}

fn runtime_template_refs(raw: &str) -> TemplateRefs {
    let mut refs = TemplateRefs {
        names: BTreeSet::new(),
        removed_syntax: raw.contains("${"),
    };
    let chars = raw.chars().collect::<Vec<_>>();
    let mut idx = 0usize;
    while idx < chars.len() {
        if chars[idx] != '$' {
            idx += 1;
            continue;
        }
        if idx + 1 >= chars.len() {
            idx += 1;
            continue;
        }
        let start = chars[idx + 1];
        if !(start == '_' || start.is_ascii_alphabetic()) {
            idx += 1;
            continue;
        }
        let mut end = idx + 2;
        while end < chars.len() {
            let next = chars[end];
            if next == '_' || next.is_ascii_alphanumeric() {
                end += 1;
            } else {
                break;
            }
        }
        refs.names
            .insert(chars[idx + 1..end].iter().collect::<String>());
        idx = end;
    }
    refs
}

fn merge_template_refs(
    target: &mut BTreeMap<String, TemplateRefs>,
    source: BTreeMap<String, TemplateRefs>,
) {
    for (field, refs) in source {
        target.insert(field, refs);
    }
}

fn unique_template_ref_names(refs: &BTreeMap<String, TemplateRefs>) -> BTreeSet<String> {
    refs.values()
        .flat_map(|item| item.names.iter().cloned())
        .collect()
}

fn declared_env_secret_names(value: &Value) -> BTreeSet<String> {
    let Some(secrets) = value.pointer("/runtime/secrets").and_then(Value::as_array) else {
        return BTreeSet::new();
    };
    secrets
        .iter()
        .filter(|secret| secret.pointer("/from").and_then(Value::as_str) == Some("env"))
        .filter_map(|secret| {
            secret
                .pointer("/name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
        .collect()
}

fn is_runtime_binding_scalar(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

fn validates_payload_at_write_boundary(schema_version: &str) -> bool {
    matches!(
        schema_version,
        "artifact_envelope_v1"
            | "grader_input_v1"
            | "package_checks_v1"
            | "prepared_task_environment_v1"
            | "prepared_runtime_image_map_v1"
            | "resolved_schedule_v1"
            | "resolved_variants_v1"
            | "runtime_path_staging_manifest_v1"
            | "sealed_package_checksums_v2"
            | "sealed_package_lock_v1"
            | "sealed_run_package_v2"
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
    reject_duplicate_json_object_keys(
        &overrides_data,
        &format!("overrides {}", overrides_path.display()),
    )?;
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
    validate_override_manifest_path_field(overrides_path, &overrides)?;
    validate_override_value_keys(overrides_path, &overrides)?;
    Ok(overrides)
}

fn validate_override_manifest_path_field(
    overrides_path: &Path,
    overrides: &ExperimentOverrides,
) -> Result<()> {
    let Some(manifest_path) = overrides.manifest_path.as_ref() else {
        return Ok(());
    };
    if manifest_path.trim().is_empty() {
        return Err(anyhow!(
            "overrides {} declares blank manifest_path; omit manifest_path to use the default .lab/knobs/manifest.json",
            overrides_path.display()
        ));
    }
    if manifest_path.trim() != manifest_path {
        return Err(anyhow!(
            "overrides {} declares manifest_path '{}' with leading or trailing whitespace; manifest_path must be written exactly as a project-relative path",
            overrides_path.display(),
            manifest_path
        ));
    }
    Ok(())
}

fn validate_override_value_keys(
    overrides_path: &Path,
    overrides: &ExperimentOverrides,
) -> Result<()> {
    for id in overrides.values.keys() {
        if id.trim().is_empty() {
            return Err(anyhow!(
                "overrides {} declares blank override id; override value keys must be non-empty knob ids after trimming whitespace",
                overrides_path.display()
            ));
        }
        if id.trim() != id {
            return Err(anyhow!(
                "overrides {} declares override id '{}' with leading or trailing whitespace; override value keys must exactly match knob ids",
                overrides_path.display(),
                id
            ));
        }
    }
    Ok(())
}

pub(crate) fn load_knob_manifest(manifest_path: &Path) -> Result<KnobManifest> {
    let manifest_schema = compile_schema("knob_manifest_v1.jsonschema")?;
    let manifest_data = fs::read_to_string(manifest_path)?;
    reject_duplicate_json_object_keys(
        &manifest_data,
        &format!("knob manifest {}", manifest_path.display()),
    )?;
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
    let mut seen_ids = BTreeSet::new();
    let mut seen_pointers = BTreeMap::new();
    for (idx, knob) in manifest.knobs.iter().enumerate() {
        validate_knob_definition(manifest_path, idx, knob)?;
        if !seen_ids.insert(knob.id.clone()) {
            return Err(anyhow!(
                "knob manifest {} duplicates knob id '{}' at knobs[{}]; knob ids must be unique",
                manifest_path.display(),
                knob.id,
                idx
            ));
        }
        if let Some(previous_idx) = seen_pointers.insert(knob.json_pointer.clone(), idx) {
            return Err(anyhow!(
                "knob manifest {} maps multiple knobs to json_pointer '{}' at knobs[{}] and knobs[{}]; each knob must target a unique field",
                manifest_path.display(),
                knob.json_pointer,
                previous_idx,
                idx
            ));
        }
    }
    Ok(manifest)
}

fn validate_knob_definition(manifest_path: &Path, idx: usize, knob: &KnobDef) -> Result<()> {
    validate_knob_id(manifest_path, idx, knob)?;
    validate_knob_json_pointer(manifest_path, idx, knob)?;
    if knob.autotune.is_some() {
        return Err(anyhow!(
            "knob manifest {} declares autotune for knob '{}' at knobs[{}], but autotune is not supported by the build/package pipeline yet; remove autotune or materialize it into explicit override values",
            manifest_path.display(),
            knob.id,
            idx
        ));
    }
    let is_numeric = matches!(knob.value_type.as_str(), "integer" | "number");
    if !is_numeric && (knob.minimum.is_some() || knob.maximum.is_some() || knob.step.is_some()) {
        return Err(anyhow!(
            "knob manifest {} declares numeric controls for non-numeric knob '{}' at knobs[{}]; minimum, maximum, and step only apply to integer or number knobs",
            manifest_path.display(),
            knob.id,
            idx
        ));
    }
    if let Some(step) = knob.step {
        if step <= 0.0 {
            return Err(anyhow!(
                "knob manifest {} declares non-positive step for knob '{}' at knobs[{}]: step must be greater than 0",
                manifest_path.display(),
                knob.id,
                idx
            ));
        }
    }
    if knob.value_type == "integer" {
        for (field, value) in [
            ("minimum", knob.minimum),
            ("maximum", knob.maximum),
            ("step", knob.step),
        ] {
            if let Some(value) = value {
                if !is_integral_f64(value) {
                    return Err(anyhow!(
                        "knob manifest {} declares fractional {} for integer knob '{}' at knobs[{}]: integer knobs require integer minimum, maximum, and step values",
                        manifest_path.display(),
                        field,
                        knob.id,
                        idx
                    ));
                }
            }
        }
    }
    if let (Some(minimum), Some(maximum)) = (knob.minimum, knob.maximum) {
        if minimum > maximum {
            return Err(anyhow!(
                "knob manifest {} declares impossible bounds for knob '{}' at knobs[{}]: minimum {} is greater than maximum {}",
                manifest_path.display(),
                knob.id,
                idx,
                minimum,
                maximum
            ));
        }
    }
    if let Some(options) = knob.options.as_ref() {
        if options.is_empty() {
            return Err(anyhow!(
                "knob manifest {} declares empty options for knob '{}' at knobs[{}]; omit options or provide at least one allowed value",
                manifest_path.display(),
                knob.id,
                idx
            ));
        }
        let mut seen_options = BTreeMap::new();
        for (option_idx, option) in options.iter().enumerate() {
            let option_key = semantic_option_key(option);
            if let Some(previous_idx) = seen_options.insert(option_key, option_idx) {
                return Err(anyhow!(
                    "knob manifest {} duplicates option for knob '{}' at knobs[{}].options[{}] and knobs[{}].options[{}]",
                    manifest_path.display(),
                    knob.id,
                    idx,
                    previous_idx,
                    idx,
                    option_idx
                ));
            }
            if !value_matches_type(option, &knob.value_type) {
                return Err(anyhow!(
                    "knob manifest {} option type mismatch for knob '{}' at knobs[{}].options[{}]: expected {}, got {}",
                    manifest_path.display(),
                    knob.id,
                    idx,
                    option_idx,
                    knob.value_type,
                    value_type_name(option)
                ));
            }
            if let Some(minimum) = knob.minimum {
                if let Some(value) = option.as_f64() {
                    if value < minimum {
                        return Err(anyhow!(
                            "knob manifest {} option for knob '{}' at knobs[{}].options[{}] is below minimum {}",
                            manifest_path.display(),
                            knob.id,
                            idx,
                            option_idx,
                            minimum
                        ));
                    }
                }
            }
            if let Some(maximum) = knob.maximum {
                if let Some(value) = option.as_f64() {
                    if value > maximum {
                        return Err(anyhow!(
                            "knob manifest {} option for knob '{}' at knobs[{}].options[{}] is above maximum {}",
                            manifest_path.display(),
                            knob.id,
                            idx,
                            option_idx,
                            maximum
                        ));
                    }
                }
            }
            if let Some(step) = knob.step {
                if let Some(value) = option.as_f64() {
                    let base = knob.minimum.unwrap_or(0.0);
                    if !value_respects_step(value, base, step) {
                        return Err(anyhow!(
                            "knob manifest {} option for knob '{}' at knobs[{}].options[{}] does not align with step {} from base {}",
                            manifest_path.display(),
                            knob.id,
                            idx,
                            option_idx,
                            step,
                            base
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn semantic_option_key(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => format!("bool:{value}"),
        Value::Number(value) => format!("number:{}", semantic_number_key(value)),
        Value::String(value) => format!("string:{}:{value}", value.len()),
        Value::Array(values) => {
            let mut key = String::from("array:[");
            for value in values {
                let child = semantic_option_key(value);
                key.push_str(&child.len().to_string());
                key.push(':');
                key.push_str(&child);
                key.push(',');
            }
            key.push(']');
            key
        }
        Value::Object(object) => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut key = String::from("object:{");
            for (name, value) in entries {
                let child = semantic_option_key(value);
                key.push_str(&name.len().to_string());
                key.push(':');
                key.push_str(name);
                key.push('=');
                key.push_str(&child.len().to_string());
                key.push(':');
                key.push_str(&child);
                key.push(',');
            }
            key.push('}');
            key
        }
    }
}

fn semantic_number_key(value: &serde_json::Number) -> String {
    if let Some(value) = value.as_i64() {
        return value.to_string();
    }
    if let Some(value) = value.as_u64() {
        return value.to_string();
    }
    let value = value
        .as_f64()
        .expect("JSON numbers accepted by serde_json must convert to f64");
    if value == 0.0 {
        return "0".to_string();
    }
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        return format!("{value:.0}");
    }
    format!("f64:{:016x}", value.to_bits())
}

fn validate_knob_id(manifest_path: &Path, idx: usize, knob: &KnobDef) -> Result<()> {
    if knob.id.trim().is_empty() {
        return Err(anyhow!(
            "knob manifest {} declares blank knob id at knobs[{}]; knob ids must be non-empty after trimming whitespace",
            manifest_path.display(),
            idx
        ));
    }
    if knob.id.trim() != knob.id {
        return Err(anyhow!(
            "knob manifest {} declares knob id '{}' with leading or trailing whitespace at knobs[{}]; knob ids must be written exactly as override keys",
            manifest_path.display(),
            knob.id,
            idx
        ));
    }
    Ok(())
}

fn validate_knob_json_pointer(manifest_path: &Path, idx: usize, knob: &KnobDef) -> Result<()> {
    if knob.json_pointer == "/" {
        return Err(anyhow!(
            "knob manifest {} declares root json_pointer for knob '{}' at knobs[{}]; knob pointers must target a concrete experiment field",
            manifest_path.display(),
            knob.id,
            idx
        ));
    }
    let Some(rest) = knob.json_pointer.strip_prefix('/') else {
        return Err(anyhow!(
            "knob manifest {} declares invalid json_pointer '{}' for knob '{}' at knobs[{}]; pointer must start with '/'",
            manifest_path.display(),
            knob.json_pointer,
            knob.id,
            idx
        ));
    };
    for token in rest.split('/') {
        if token.is_empty() {
            return Err(anyhow!(
                "knob manifest {} declares invalid json_pointer '{}' for knob '{}' at knobs[{}]; empty path segments are not allowed",
                manifest_path.display(),
                knob.json_pointer,
                knob.id,
                idx
            ));
        }
        validate_json_pointer_token_escapes(manifest_path, idx, knob, token)?;
        if token.len() > 1 && token.starts_with('0') && token.bytes().all(|b| b.is_ascii_digit()) {
            return Err(anyhow!(
                "knob manifest {} declares invalid json_pointer '{}' for knob '{}' at knobs[{}]; numeric pointer segments must be canonical and must not use leading zeros",
                manifest_path.display(),
                knob.json_pointer,
                knob.id,
                idx
            ));
        }
    }
    Ok(())
}

fn validate_json_pointer_token_escapes(
    manifest_path: &Path,
    idx: usize,
    knob: &KnobDef,
    token: &str,
) -> Result<()> {
    let bytes = token.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'~' {
            let valid_escape = i + 1 < bytes.len() && matches!(bytes[i + 1], b'0' | b'1');
            if !valid_escape {
                return Err(anyhow!(
                    "knob manifest {} declares invalid json_pointer '{}' for knob '{}' at knobs[{}]; '~' must be escaped as '~0' or '~1'",
                    manifest_path.display(),
                    knob.json_pointer,
                    knob.id,
                    idx
                ));
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    Ok(())
}

fn is_integral_f64(value: f64) -> bool {
    value.fract() == 0.0
}

fn value_respects_step(value: f64, base: f64, step: f64) -> bool {
    let units = (value - base) / step;
    let nearest = units.round();
    let tolerance = 1e-9_f64.max(units.abs() * 1e-9);
    (units - nearest).abs() <= tolerance
}

pub(crate) fn reject_duplicate_json_object_keys(raw: &str, context: &str) -> Result<()> {
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    DuplicateJsonKeySeed {
        path: String::new(),
    }
    .deserialize(&mut deserializer)
    .map_err(|err| anyhow!("{} contains duplicate JSON object key: {}", context, err))?;
    deserializer
        .end()
        .map_err(|err| anyhow!("{} is not valid JSON: {}", context, err))?;
    Ok(())
}

struct DuplicateJsonKeySeed {
    path: String,
}

impl<'de> DeserializeSeed<'de> for DuplicateJsonKeySeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DuplicateJsonKeyVisitor { path: self.path })
    }
}

struct DuplicateJsonKeyVisitor {
    path: String,
}

impl<'de> Visitor<'de> for DuplicateJsonKeyVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_none<E>(self) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_unit<E>(self) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut idx = 0usize;
        while seq
            .next_element_seed(DuplicateJsonKeySeed {
                path: json_pointer_child(&self.path, &idx.to_string()),
            })?
            .is_some()
        {
            idx += 1;
        }
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                let path = if self.path.is_empty() {
                    "/"
                } else {
                    &self.path
                };
                return Err(de::Error::custom(format!(
                    "duplicate key '{}' at {}",
                    key, path
                )));
            }
            map.next_value_seed(DuplicateJsonKeySeed {
                path: json_pointer_child(&self.path, &key),
            })?;
        }
        Ok(())
    }
}

fn json_pointer_child(parent: &str, raw: &str) -> String {
    let escaped = raw.replace('~', "~0").replace('/', "~1");
    if parent.is_empty() {
        format!("/{}", escaped)
    } else {
        format!("{}/{}", parent, escaped)
    }
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
    if let Some(step) = knob.step {
        if let Some(v) = value.as_f64() {
            let base = knob.minimum.unwrap_or(0.0);
            if !value_respects_step(v, base, step) {
                return Err(anyhow!(
                    "override value for knob {} does not align with step {} from base {}",
                    knob.id,
                    step,
                    base
                ));
            }
        }
    }
    Ok(())
}

pub fn validate_knob_overrides(manifest_path: &Path, overrides_path: &Path) -> Result<()> {
    let manifest = load_knob_manifest(manifest_path)?;
    let overrides = load_experiment_overrides(overrides_path)?;
    if let Some(manifest_ref) = overrides.manifest_path.as_ref() {
        return Err(anyhow!(
            "overrides {} declares manifest_path '{}', but standalone knob override validation already received manifest {}; omit manifest_path from the overrides file or validate through the build/apply path",
            overrides_path.display(),
            manifest_ref,
            manifest_path.display()
        ));
    }
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
