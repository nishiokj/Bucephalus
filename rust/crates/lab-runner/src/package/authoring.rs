use anyhow::{anyhow, Context, Result};
use lab_core::{
    sha256_bytes, sha256_file, BUCEPHALUS_RESULT_PATH, BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER,
};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::config::{apply_experiment_overrides, find_project_root, normalize_path};
use crate::model::LoadedExperimentInput;
use crate::package::cas::should_include_agent_artifact_path;

pub(crate) fn load_authoring_input_for_build(
    path: &Path,
    overrides_path: Option<&Path>,
) -> Result<LoadedExperimentInput> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolve build input path '{}'", path.display()))?;
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
        .ok_or_else(|| anyhow!("build input has no parent directory"))?
        .canonicalize()
        .with_context(|| format!("resolve experiment directory for '{}'", canonical.display()))?;
    let project_root = find_project_root(&exp_dir);
    let project_root = project_root
        .canonicalize()
        .with_context(|| format!("resolve project root '{}'", project_root.display()))?;
    let raw_yaml = fs::read_to_string(&canonical)?;
    reject_duplicate_yaml_mapping_keys(&raw_yaml, &canonical)?;
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&raw_yaml)?;
    let json_value: Value = serde_json::to_value(yaml_value)?;
    let mut json_value = if let Some(overrides_path) = overrides_path {
        apply_experiment_overrides(json_value, overrides_path, &project_root)?
    } else {
        json_value
    };
    reject_legacy_authoring_surface(&json_value)?;
    validate_authoring_schema(&json_value)?;
    crate::package::validate::validate_required_fields(&json_value)?;
    normalize_authoring_vocabulary(&mut json_value)
        .map_err(|err| crate::package::validate::public_authoring_error(err, true))?;
    Ok(LoadedExperimentInput {
        json_value,
        exp_dir,
        project_root,
    })
}

pub fn reject_duplicate_yaml_mapping_keys(raw: &str, path: &Path) -> Result<()> {
    for document in serde_yaml::Deserializer::from_str(raw) {
        DuplicateYamlKeySeed {
            path: String::new(),
        }
        .deserialize(document)
        .map_err(|err| {
            anyhow!(
                "experiment authoring YAML {} contains duplicate mapping key: {}",
                path.display(),
                err
            )
        })?;
    }
    Ok(())
}

struct DuplicateYamlKeySeed {
    path: String,
}

impl<'de> DeserializeSeed<'de> for DuplicateYamlKeySeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DuplicateYamlKeyVisitor { path: self.path })
    }
}

struct DuplicateYamlKeyVisitor {
    path: String,
}

impl<'de> Visitor<'de> for DuplicateYamlKeyVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any YAML value")
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
            .next_element_seed(DuplicateYamlKeySeed {
                path: yaml_path_child(&self.path, &idx.to_string()),
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
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<serde_yaml::Value>()? {
            if !seen.insert(key.clone()) {
                let path = if self.path.is_empty() {
                    "/"
                } else {
                    &self.path
                };
                return Err(de::Error::custom(format!(
                    "duplicate key '{}' at {}",
                    yaml_key_label(&key),
                    path
                )));
            }
            map.next_value_seed(DuplicateYamlKeySeed {
                path: yaml_path_child(&self.path, &yaml_key_label(&key)),
            })?;
        }
        Ok(())
    }
}

fn yaml_key_label(key: &serde_yaml::Value) -> String {
    match key {
        serde_yaml::Value::String(value) => value.clone(),
        serde_yaml::Value::Bool(value) => value.to_string(),
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::Null => "null".to_string(),
        _ => format!("{key:?}"),
    }
}

fn yaml_path_child(parent: &str, raw: &str) -> String {
    let escaped = raw.replace('~', "~0").replace('/', "~1");
    if parent.is_empty() {
        format!("/{}", escaped)
    } else {
        format!("{}/{}", parent, escaped)
    }
}

fn validate_authoring_schema(json_value: &Value) -> Result<()> {
    let schema = lab_schemas::compile_schema("experiment_authoring_v1.jsonschema")?;
    let Some(errors) = schema.validate(json_value).err() else {
        return Ok(());
    };
    let mut messages = errors
        .map(|err| lab_schemas::format_validation_error(&err))
        .collect::<Vec<_>>();
    if json_value
        .pointer("/stages/agent")
        .and_then(Value::as_object)
        .is_some_and(|agent| !agent.contains_key("command") && !agent.contains_key("adapter"))
    {
        messages.push(
            "/stages/agent must declare exactly one launch contract: command or adapter"
                .to_string(),
        );
    }
    Err(anyhow!(
        "experiment authoring schema validation failed: {}",
        messages.join("; ")
    ))
}

pub(crate) fn reject_legacy_authoring_surface(json_value: &Value) -> Result<()> {
    for (pointer, replacement) in [
        ("/agent_builds", "/matrix/variants[].overrides"),
        ("/baseline", "/matrix/variants[] with baseline: true"),
        ("/cases", "/matrix/cases"),
        ("/variants", "/matrix/variants"),
        ("/task", "/stages/case"),
        ("/agent", "/stages/agent"),
        ("/execution", "/stages/execution"),
        ("/grader", "/stages/grader"),
        ("/trial_runtime", "/stages"),
        ("/sidecars", "/services"),
        ("/matrix/tasks", "/matrix/cases"),
        ("/runtime/externals", "/externals"),
        (
            "/runtime/storage",
            "nothing; storage is runner-owned today and local-fs is the default runtime behavior",
        ),
        (
            "/runtime/traces",
            "nothing; trace sinks are runner-owned today, and command traces should be declared with /traces when needed",
        ),
        ("/variant_plan", "/matrix/variants"),
        (
            "/overrides",
            "first-class v1 fields under /runtime, /policy, or /matrix",
        ),
        (
            "/knobs",
            "the external knob_manifest_v1 file referenced by --overrides",
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
    reject_metric_source_authoring(json_value)?;
    reject_mixed_network_default_authoring(json_value)?;
    reject_scheduling_comparison_authoring_overlap(json_value)?;
    reject_empty_grader_authoring(json_value)?;
    reject_noop_traces_authoring(json_value)?;
    reject_credential_cache_without_mount(json_value)?;
    reject_duplicate_external_accounting_lists(json_value)?;
    reject_duplicate_runtime_secret_names(json_value)?;
    reject_invalid_runtime_secret_provider_shapes(json_value)?;
    reject_duplicate_credential_cache_env(json_value)?;
    reject_nested_resolved_authoring_vocabulary(json_value)?;
    reject_uninferrable_agent_site_authoring(json_value)?;
    Ok(())
}

fn reject_nested_resolved_authoring_vocabulary(json_value: &Value) -> Result<()> {
    if json_value.pointer("/stages/task").is_some() {
        return Err(anyhow!(
            "/stages/task is resolved package vocabulary and is not accepted in authoring YAML; use /stages/case"
        ));
    }
    for stage in ["agent", "grader"] {
        let pointer = format!("/stages/{}/sidecars", stage);
        if json_value.pointer(&pointer).is_some() {
            return Err(anyhow!(
                "{} is resolved package vocabulary and is not accepted in authoring YAML; use /stages/{}/services",
                pointer,
                stage
            ));
        }
    }
    reject_agent_runtime_owned_fields(json_value, "/stages/agent", "/stages/agent")?;
    let Some(variants) = json_value
        .pointer("/matrix/variants")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for (idx, variant) in variants.iter().enumerate() {
        if variant.pointer("/overrides/task").is_some() {
            return Err(anyhow!(
                "/matrix/variants/{}/overrides/task is resolved package vocabulary and is not accepted in authoring YAML; use /matrix/variants/{}/overrides/case",
                idx,
                idx
            ));
        }
        for stage in ["agent", "grader"] {
            let pointer = format!("/overrides/{}/sidecars", stage);
            if variant.pointer(&pointer).is_some() {
                return Err(anyhow!(
                    "/matrix/variants/{}/overrides/{}/sidecars is resolved package vocabulary and is not accepted in authoring YAML; use /matrix/variants/{}/overrides/{}/services",
                    idx,
                    stage,
                    idx,
                    stage
                ));
            }
        }
        reject_agent_runtime_owned_fields(
            variant,
            "/overrides/agent",
            &format!("/matrix/variants/{}/overrides/agent", idx),
        )?;
    }
    Ok(())
}

fn reject_agent_runtime_owned_fields(
    root: &Value,
    pointer: &str,
    public_context: &str,
) -> Result<()> {
    for (field, replacement) in [
        (
            "artifact_type",
            "omit it; package build writes the canonical structured_json result contract",
        ),
        (
            "integration_level",
            "omit it; package build derives it from declared events or traces.source: protocol",
        ),
        (
            "telemetry",
            "use /traces.source: protocol or explicit /stages/agent/events",
        ),
        (
            "protocol",
            "use /stages/agent/command; command invocation is implied",
        ),
    ] {
        let full_pointer = format!("{}/{}", pointer, field);
        if root.pointer(&full_pointer).is_some() {
            return Err(anyhow!(
                "{}/{} is resolved package vocabulary and is not accepted in authoring YAML; {}",
                public_context,
                field,
                replacement
            ));
        }
    }
    Ok(())
}

fn reject_mixed_network_default_authoring(json_value: &Value) -> Result<()> {
    if json_value.pointer("/runtime/network/default").is_none() {
        return Ok(());
    }
    let explicit_planes = explicit_network_default_plane_overlaps(json_value);
    if explicit_planes.is_empty() {
        return Ok(());
    }
    Err(mixed_network_default_error(&explicit_planes))
}

fn reject_metric_source_authoring(json_value: &Value) -> Result<()> {
    let Some(metrics) = json_value.get("metrics") else {
        return Ok(());
    };
    let metrics = metrics
        .as_array()
        .ok_or_else(|| anyhow!("/metrics must be an array"))?;
    for (idx, metric) in metrics.iter().enumerate() {
        if metric.pointer("/source").is_some() {
            return Err(anyhow!(
                "/metrics/{} uses internal metric extraction field 'source'; use user-facing 'from', e.g. from: result.metrics.resolved or from: grader.report.resolved",
                idx
            ));
        }
    }
    Ok(())
}

fn reject_uninferrable_agent_site_authoring(json_value: &Value) -> Result<()> {
    let Some(stages) = json_value.get("stages").and_then(Value::as_object) else {
        return Ok(());
    };
    if stages
        .get("execution")
        .and_then(|execution| execution.get("agent_site"))
        .is_some()
    {
        return Ok(());
    }
    let Some(agent) = stages.get("agent").and_then(Value::as_object) else {
        return Ok(());
    };
    if agent
        .get("image")
        .and_then(Value::as_str)
        .is_some_and(|image| !image.trim().is_empty())
    {
        return Ok(());
    }
    let Some(case) = stages.get("case").and_then(Value::as_object) else {
        return Ok(());
    };
    if case
        .get("interface")
        .and_then(Value::as_str)
        .is_some_and(|interface| interface == "input_only")
        && !case.contains_key("files")
        && !case.contains_key("workspace")
    {
        return Ok(());
    }
    if case
        .get("workspace")
        .and_then(|workspace| workspace.get("source"))
        .and_then(Value::as_str)
        == Some("container_image")
    {
        return Ok(());
    }
    if case.contains_key("files")
        || case.contains_key("workspace")
        || case
            .get("interface")
            .and_then(Value::as_str)
            .is_some_and(|interface| interface == "readonly_files")
    {
        return Err(uninferrable_agent_site_error());
    }
    Ok(())
}

fn uninferrable_agent_site_error() -> anyhow::Error {
    anyhow!(
        "/stages.execution.agent_site is required when the agent runtime boundary cannot be inferred; declare agent_container, task_runtime, or host"
    )
}

fn reject_scheduling_comparison_authoring_overlap(json_value: &Value) -> Result<()> {
    let Some(comparison) = json_value
        .pointer("/scheduling/comparison")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if comparison != "paired" {
        return Err(unsupported_scheduling_comparison_error(comparison));
    }
    if json_value.pointer("/policy/policies/scheduling").is_some() {
        return Err(anyhow!(
            "/scheduling/comparison is exclusive authoring shorthand; do not combine it with /policy/policies/scheduling"
        ));
    }
    Ok(())
}

fn unsupported_scheduling_comparison_error(value: &str) -> anyhow::Error {
    anyhow!(
        "unsupported scheduling.comparison '{value}'; this authoring shorthand only accepts 'paired'. For the default run order, remove scheduling.comparison. For an explicit run order, set policy.policies.scheduling to one of: variant_sequential, paired_interleaved, randomized."
    )
}

fn reject_empty_grader_authoring(json_value: &Value) -> Result<()> {
    let Some(grader) = json_value.pointer("/stages/grader") else {
        return Ok(());
    };
    let grader = grader
        .as_object()
        .ok_or_else(|| anyhow!("/stages/grader must be an object"))?;
    if grader.is_empty() {
        return Err(anyhow!(
            "/stages/grader must not be empty; omit it for the no-grader default or declare strategy: none explicitly"
        ));
    }
    Ok(())
}

fn reject_noop_traces_authoring(json_value: &Value) -> Result<()> {
    if json_value.pointer("/traces/source").and_then(Value::as_str) == Some("none") {
        return Err(anyhow!(
            "/traces.source=none is not accepted; omit /traces for runner lifecycle events only, or use /traces.source=protocol to ingest command-agent traces"
        ));
    }
    Ok(())
}

fn reject_credential_cache_without_mount(json_value: &Value) -> Result<()> {
    let Some(secrets) = json_value
        .pointer("/runtime/secrets")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for (idx, secret) in secrets.iter().enumerate() {
        if secret.pointer("/credential_cache").is_some() && secret.pointer("/mount").is_none() {
            return Err(anyhow!(
                "/runtime/secrets/{} declares credential_cache without mount; credential caches are attached to file secrets, so declare mount.target",
                idx
            ));
        }
    }
    Ok(())
}

fn reject_duplicate_external_accounting_lists(json_value: &Value) -> Result<()> {
    for pointer in [
        "/externals/apis",
        "/externals/credentials",
        "/runtime/network/egress",
    ] {
        string_array_values(json_value.pointer(pointer), pointer)?;
    }
    Ok(())
}

fn reject_duplicate_runtime_secret_names(json_value: &Value) -> Result<()> {
    let Some(secrets) = json_value.pointer("/runtime/secrets") else {
        return Ok(());
    };
    let secrets = secrets
        .as_array()
        .ok_or_else(|| anyhow!("/runtime/secrets must be an array"))?;
    let mut seen = BTreeSet::new();
    for (idx, secret) in secrets.iter().enumerate() {
        let name = secret
            .pointer("/name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("/runtime/secrets/{} name must be a non-empty string", idx))?;
        if !seen.insert(name.to_string()) {
            return Err(anyhow!(
                "/runtime/secrets/{} duplicates secret name '{}'; runtime secret names must be unique",
                idx,
                name
            ));
        }
    }
    Ok(())
}

fn reject_invalid_runtime_secret_provider_shapes(json_value: &Value) -> Result<()> {
    let Some(secrets) = json_value.pointer("/runtime/secrets") else {
        return Ok(());
    };
    let secrets = secrets
        .as_array()
        .ok_or_else(|| anyhow!("/runtime/secrets must be an array"))?;
    for (idx, secret) in secrets.iter().enumerate() {
        let Some(from) = secret.pointer("/from").and_then(Value::as_str) else {
            continue;
        };
        match from {
            "env" => {
                if secret.pointer("/mount").is_some() {
                    return Err(anyhow!(
                        "/runtime/secrets/{} declares from=env with mount; env secrets are scalar values, so remove mount or set from: file",
                        idx
                    ));
                }
                if secret.pointer("/credential_cache").is_some() {
                    return Err(anyhow!(
                        "/runtime/secrets/{} declares from=env with credential_cache; credential caches are only valid for mounted file secrets",
                        idx
                    ));
                }
            }
            "file" if secret.pointer("/mount").is_none() => {
                return Err(anyhow!(
                    "/runtime/secrets/{} declares from=file without mount; file secrets must declare mount.target",
                    idx
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn reject_duplicate_credential_cache_env(json_value: &Value) -> Result<()> {
    let Some(secrets) = json_value.pointer("/runtime/secrets") else {
        return Ok(());
    };
    let secrets = secrets
        .as_array()
        .ok_or_else(|| anyhow!("/runtime/secrets must be an array"))?;
    let mut seen = BTreeSet::new();
    for (idx, secret) in secrets.iter().enumerate() {
        let Some(env) = secret
            .pointer("/credential_cache/env")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if !seen.insert(env.to_string()) {
            return Err(anyhow!(
                "/runtime/secrets/{}/credential_cache/env duplicates '{}'; credential cache env names must be unique",
                idx,
                env
            ));
        }
    }
    Ok(())
}

pub(crate) fn normalize_authoring_vocabulary(json_value: &mut Value) -> Result<()> {
    lower_top_level_value(json_value, "services", &["sidecars"])?;
    lower_top_level_value(json_value, "ephemerals", &["sidecars"])?;
    normalize_service_lifecycles(json_value, "/sidecars")?;
    lower_top_level_value(json_value, "externals", &["runtime", "externals"])?;
    if let Some(matrix) = json_value.get_mut("matrix") {
        lower_child_value(matrix, "cases", "tasks")?;
    }
    default_matrix_variants(json_value)?;
    normalize_variant_baselines(json_value)?;
    default_variant_configs(json_value)?;
    default_case_source(json_value)?;
    normalize_variant_overrides(json_value)?;
    default_variant_case_override_interfaces(json_value)?;
    normalize_authoring_defaults(json_value)?;
    default_secret_sources(json_value)?;
    reject_duplicate_runtime_secret_names(json_value)?;
    reject_invalid_runtime_secret_provider_shapes(json_value)?;
    reject_duplicate_credential_cache_env(json_value)?;
    derive_runtime_externals_from_contract(json_value)?;

    let Some(stages) = json_value.get("stages").cloned() else {
        default_trial_case_interface(json_value)?;
        lower_registry_image_rewrites(json_value)?;
        normalize_agent_site_default(json_value)?;
        normalize_grader_defaults(json_value)?;
        normalize_agent_result_output(json_value)?;
        normalize_output_capture_required_flags(json_value)?;
        normalize_grader_input_required_flags(json_value)?;
        normalize_metric_authoring(json_value)?;
        normalize_trace_policy(json_value)?;
        normalize_agent_observability_contract(json_value)?;
        normalize_agent_adapters(json_value)?;
        normalize_agent_integration_levels(json_value)?;
        drop_authoring_experiment_mode(json_value);
        return Ok(());
    };
    let mut trial_runtime = Map::new();
    let stages = stages
        .as_object()
        .ok_or_else(|| anyhow!("/stages must be an object"))?;
    for (stage_name, stage_value) in stages {
        let target = match stage_name.as_str() {
            "case" => "task",
            "agent" | "grader" | "execution" => stage_name.as_str(),
            other => other,
        };
        let mut normalized_stage = stage_value.clone();
        normalize_public_stage_ephemerals(stage_name, &mut normalized_stage)?;
        insert_lowered_value(&mut trial_runtime, target, normalized_stage, "/stages")?;
    }
    lower_object_value(json_value, &["trial_runtime"], Value::Object(trial_runtime))?;
    if let Some(object) = json_value.as_object_mut() {
        object.remove("stages");
    }
    default_trial_case_interface(json_value)?;
    lower_registry_image_rewrites(json_value)?;
    normalize_agent_site_default(json_value)?;
    normalize_grader_defaults(json_value)?;
    normalize_agent_result_output(json_value)?;
    normalize_output_capture_required_flags(json_value)?;
    normalize_grader_input_required_flags(json_value)?;
    normalize_metric_authoring(json_value)?;
    normalize_trace_policy(json_value)?;
    normalize_agent_observability_contract(json_value)?;
    normalize_agent_adapters(json_value)?;
    normalize_agent_integration_levels(json_value)?;
    drop_authoring_experiment_mode(json_value);
    Ok(())
}

fn drop_authoring_experiment_mode(json_value: &mut Value) {
    if let Some(experiment) = json_value
        .get_mut("experiment")
        .and_then(Value::as_object_mut)
    {
        experiment.remove("mode");
    }
}

fn normalize_authoring_defaults(json_value: &mut Value) -> Result<()> {
    default_object_path(json_value, &["runtime"])?;
    default_object_path(json_value, &["runtime", "compute"])?;
    default_object_path(json_value, &["runtime", "network"])?;
    default_object_path(json_value, &["policy"])?;
    default_object_path(json_value, &["policy", "policies"])?;
    default_object_path(json_value, &["policy", "policies", "retry"])?;
    default_object_path(json_value, &["policy", "policies", "pruning"])?;
    default_object_path(json_value, &["policy", "policies", "concurrency"])?;
    default_object_path(json_value, &["policy", "task_sandbox"])?;
    default_object_path(json_value, &["policy", "task_sandbox", "hardening"])?;
    default_object_path(json_value, &["scheduling"])?;
    default_object_path(json_value, &["evaluation"])?;
    default_object_path(json_value, &["evaluation", "policy"])?;

    default_network_planes_from_authoring_default(json_value)?;
    lower_scheduling_comparison_into_policy(json_value)?;
    default_experiment_name(json_value)?;
    insert_default_value(
        json_value,
        &["runtime", "compute", "backend"],
        json!("local-docker"),
    )?;
    insert_default_value(
        json_value,
        &["runtime", "network", "task_sandbox"],
        json!("none"),
    )?;
    insert_default_value(json_value, &["runtime", "network", "agent"], json!("none"))?;
    insert_default_value(json_value, &["matrix", "repeats"], json!(1))?;
    insert_default_value(json_value, &["scheduling", "max_concurrency"], json!(1))?;
    insert_default_value(json_value, &["scheduling", "random_seed"], json!(1))?;
    insert_default_value(json_value, &["policy", "timeout_ms"], json!(600000))?;
    insert_default_value(
        json_value,
        &["policy", "sanitization_profile"],
        json!("standard_runtime"),
    )?;
    insert_default_value(json_value, &["policy", "task_sandbox"], json!({}))?;
    insert_default_value(
        json_value,
        &["policy", "task_sandbox", "hardening", "no_new_privileges"],
        json!(true),
    )?;
    insert_default_value(
        json_value,
        &["policy", "task_sandbox", "hardening", "drop_all_caps"],
        json!(true),
    )?;
    insert_default_value(
        json_value,
        &["policy", "policies", "scheduling"],
        json!("variant_sequential"),
    )?;
    insert_default_value(
        json_value,
        &["policy", "policies", "state"],
        json!("isolate_per_trial"),
    )?;
    insert_default_value(
        json_value,
        &["policy", "policies", "retry", "max_attempts"],
        json!(1),
    )?;
    insert_default_value(
        json_value,
        &["policy", "policies", "retry", "retry_on"],
        json!([]),
    )?;
    insert_default_value(
        json_value,
        &["policy", "policies", "pruning", "max_consecutive_failures"],
        json!(0),
    )?;
    insert_default_value(
        json_value,
        &["policy", "policies", "concurrency", "require_chain_lease"],
        json!(true),
    )?;
    insert_default_value(
        json_value,
        &["evaluation", "policy", "task_model"],
        json!("independent"),
    )?;
    insert_default_value(
        json_value,
        &["evaluation", "policy", "scoring_lifecycle"],
        json!("predict_then_score"),
    )?;
    insert_default_value(
        json_value,
        &["evaluation", "policy", "chain_failure_policy"],
        json!("continue_with_flag"),
    )?;
    insert_default_value(
        json_value,
        &["evaluation", "policy", "required_evidence_classes"],
        json!([]),
    )?;
    Ok(())
}

fn lower_scheduling_comparison_into_policy(json_value: &mut Value) -> Result<()> {
    let comparison = json_value
        .pointer("/scheduling/comparison")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let Some(comparison) = comparison else {
        return Ok(());
    };
    if comparison != "paired" {
        return Err(unsupported_scheduling_comparison_error(&comparison));
    }
    if json_value.pointer("/policy/policies/scheduling").is_some() {
        return Err(anyhow!(
            "/scheduling/comparison is exclusive authoring shorthand; do not combine it with /policy/policies/scheduling"
        ));
    }
    let variant_count = json_value
        .pointer("/matrix/variants")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if variant_count < 2 {
        return Err(anyhow!(
            "/scheduling/comparison=paired requires at least two matrix variants"
        ));
    }
    insert_default_value(
        json_value,
        &["policy", "policies", "scheduling"],
        json!("paired_interleaved"),
    )?;
    if let Some(scheduling) = json_value
        .get_mut("scheduling")
        .and_then(Value::as_object_mut)
    {
        scheduling.remove("comparison");
    }
    Ok(())
}

fn default_network_planes_from_authoring_default(json_value: &mut Value) -> Result<()> {
    let Some(raw_default) = json_value.pointer("/runtime/network/default") else {
        return Ok(());
    };
    let explicit_planes = explicit_network_default_plane_overlaps(json_value);
    if !explicit_planes.is_empty() {
        return Err(mixed_network_default_error(&explicit_planes));
    }
    let default = raw_default
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("/runtime/network/default must be a non-empty string"))?
        .to_string();
    let network = ensure_object_path(json_value, &["runtime", "network"])?;
    network.insert("task_sandbox".to_string(), json!(default.clone()));
    network.insert("agent".to_string(), json!(default));
    network.remove("default");
    Ok(())
}

fn explicit_network_default_plane_overlaps(json_value: &Value) -> Vec<&'static str> {
    let mut explicit_planes = Vec::new();
    if json_value
        .pointer("/runtime/network/task_sandbox")
        .is_some()
    {
        explicit_planes.push("/runtime/network/task_sandbox");
    }
    if json_value.pointer("/runtime/network/agent").is_some() {
        explicit_planes.push("/runtime/network/agent");
    }
    explicit_planes
}

fn mixed_network_default_error(explicit_planes: &[&str]) -> anyhow::Error {
    anyhow!(
        "/runtime/network/default is exclusive shorthand; do not combine it with {}; write explicit task_sandbox and agent values for mixed network modes",
        explicit_planes.join(" or ")
    )
}

fn default_case_source(json_value: &mut Value) -> Result<()> {
    if json_value.pointer("/matrix/tasks/source").is_none()
        && json_value.pointer("/matrix/tasks/path").is_some()
    {
        insert_default_value(json_value, &["matrix", "tasks", "source"], json!("file"))?;
    }
    Ok(())
}

fn default_matrix_variants(json_value: &mut Value) -> Result<()> {
    let Some(matrix) = json_value.get_mut("matrix") else {
        return Ok(());
    };
    let matrix = matrix
        .as_object_mut()
        .ok_or_else(|| anyhow!("/matrix must be an object"))?;
    if matrix.contains_key("variants") {
        return Ok(());
    }
    matrix.insert(
        "variants".to_string(),
        json!([{ "id": "baseline", "baseline": true, "config": {} }]),
    );
    Ok(())
}

fn normalize_variant_baselines(json_value: &mut Value) -> Result<()> {
    let Some(variants) = json_value.pointer_mut("/matrix/variants") else {
        return Ok(());
    };
    let variants = variants
        .as_array_mut()
        .ok_or_else(|| anyhow!("/matrix/variants must be an array"))?;
    if variants.is_empty() {
        return Ok(());
    }

    let explicit_baseline_count = variants
        .iter()
        .filter(|variant| {
            variant
                .get("baseline")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    if explicit_baseline_count > 1 {
        return Err(anyhow!(
            "exactly one /matrix/variants[].baseline=true is required"
        ));
    }
    if explicit_baseline_count == 0 {
        if variants.len() == 1 {
            let variant = variants[0]
                .as_object_mut()
                .ok_or_else(|| anyhow!("/matrix/variants/0 must be an object"))?;
            if variant.get("baseline").is_some() {
                return Err(anyhow!(
                    "single-variant authoring cannot set /matrix/variants/0/baseline=false; omit baseline or set it to true"
                ));
            }
            variant.insert("baseline".to_string(), json!(true));
        } else {
            return Err(anyhow!(
                "multiple variants require exactly one explicit /matrix/variants[].baseline=true"
            ));
        }
    }

    for (idx, variant) in variants.iter_mut().enumerate() {
        let variant = variant
            .as_object_mut()
            .ok_or_else(|| anyhow!("/matrix/variants/{} must be an object", idx))?;
        if variant.get("baseline").is_none() {
            variant.insert("baseline".to_string(), json!(false));
        }
    }
    Ok(())
}

fn default_variant_configs(json_value: &mut Value) -> Result<()> {
    let Some(variants) = json_value.pointer_mut("/matrix/variants") else {
        return Ok(());
    };
    let variants = variants
        .as_array_mut()
        .ok_or_else(|| anyhow!("/matrix/variants must be an array"))?;
    for (idx, variant) in variants.iter_mut().enumerate() {
        let variant = variant
            .as_object_mut()
            .ok_or_else(|| anyhow!("/matrix/variants/{} must be an object", idx))?;
        variant
            .entry("config".to_string())
            .or_insert_with(|| json!({}));
    }
    Ok(())
}

fn default_secret_sources(json_value: &mut Value) -> Result<()> {
    let Some(secrets) = json_value.pointer_mut("/runtime/secrets") else {
        return Ok(());
    };
    let secrets = secrets
        .as_array_mut()
        .ok_or_else(|| anyhow!("/runtime/secrets must be an array"))?;
    for (idx, secret) in secrets.iter_mut().enumerate() {
        let secret = secret
            .as_object_mut()
            .ok_or_else(|| anyhow!("/runtime/secrets/{} must be an object", idx))?;
        if secret.contains_key("credential_cache") && !secret.contains_key("mount") {
            return Err(anyhow!(
                "/runtime/secrets/{} declares credential_cache without mount; credential caches are attached to file secrets, so declare mount.target",
                idx
            ));
        }
        if secret.contains_key("from") {
            continue;
        }
        let provider = if secret.contains_key("mount") || secret.contains_key("credential_cache") {
            "file"
        } else {
            "env"
        };
        secret.insert("from".to_string(), json!(provider));
    }
    Ok(())
}

fn default_trial_case_interface(json_value: &mut Value) -> Result<()> {
    let Some(task) = json_value.pointer_mut("/trial_runtime/task") else {
        return Ok(());
    };
    default_case_interface_for_stage(task, "/stages.case")
}

fn default_variant_case_override_interfaces(json_value: &mut Value) -> Result<()> {
    let Some(variants) = json_value
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for (idx, variant) in variants.iter_mut().enumerate() {
        let Some(task) = variant.pointer_mut("/overrides/task") else {
            continue;
        };
        default_case_interface_for_stage(
            task,
            &format!("/matrix/variants/{}/overrides/case", idx),
        )?;
    }
    Ok(())
}

fn default_case_interface_for_stage(stage: &mut Value, context: &str) -> Result<()> {
    let stage = stage
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must be an object", context))?;
    reject_inconsistent_case_resources(stage, context)?;
    if stage.contains_key("interface") {
        return Ok(());
    }
    let interface = if stage.contains_key("workspace") {
        "writable_workspace"
    } else if stage.contains_key("files") {
        "readonly_files"
    } else {
        "input_only"
    };
    stage.insert("interface".to_string(), json!(interface));
    Ok(())
}

fn reject_inconsistent_case_resources(stage: &Map<String, Value>, context: &str) -> Result<()> {
    let has_files = stage.contains_key("files");
    let has_workspace = stage.contains_key("workspace");
    if has_files && has_workspace {
        return Err(anyhow!(
            "{} cannot declare both files and workspace; choose one case resource",
            context
        ));
    }
    let Some(interface) = stage.get("interface").and_then(Value::as_str) else {
        return Ok(());
    };
    match interface {
        "input_only" if has_files || has_workspace => Err(anyhow!(
            "{} declares interface input_only with a case resource; remove files/workspace or choose the matching interface",
            context
        )),
        "readonly_files" if has_workspace => Err(anyhow!(
            "{} declares interface readonly_files with workspace; use files or choose writable_workspace",
            context
        )),
        "writable_workspace" if has_files => Err(anyhow!(
            "{} declares interface writable_workspace with files; use workspace or choose readonly_files",
            context
        )),
        _ => Ok(()),
    }
}

fn derive_runtime_externals_from_contract(json_value: &mut Value) -> Result<()> {
    let credentials = named_runtime_secret_values(json_value)?;
    let apis = string_array_values(
        json_value.pointer("/runtime/network/egress"),
        "/runtime/network/egress",
    )?;

    if let Some(declared) = json_value.pointer("/runtime/externals/credentials") {
        let declared = string_array_values(Some(declared), "/externals/credentials")?;
        reject_external_accounting_mismatch(
            "/externals/credentials",
            "/runtime/secrets names",
            &declared,
            &credentials,
        )?;
    } else if !credentials.is_empty() {
        let externals = ensure_object_path(json_value, &["runtime", "externals"])?;
        externals.insert("credentials".to_string(), json!(credentials));
    }
    if let Some(declared) = json_value.pointer("/runtime/externals/apis") {
        let declared = string_array_values(Some(declared), "/externals/apis")?;
        reject_external_accounting_mismatch(
            "/externals/apis",
            "/runtime/network/egress",
            &declared,
            &apis,
        )?;
    } else if !apis.is_empty() {
        let externals = ensure_object_path(json_value, &["runtime", "externals"])?;
        externals.insert("apis".to_string(), json!(apis));
    }
    Ok(())
}

fn reject_external_accounting_mismatch(
    external_path: &str,
    source_path: &str,
    declared: &[String],
    source: &[String],
) -> Result<()> {
    let declared = declared.iter().cloned().collect::<BTreeSet<_>>();
    let source = source.iter().cloned().collect::<BTreeSet<_>>();
    let missing_source = declared.difference(&source).cloned().collect::<Vec<_>>();
    let missing_external = source.difference(&declared).cloned().collect::<Vec<_>>();
    if missing_source.is_empty() && missing_external.is_empty() {
        return Ok(());
    }
    Err(anyhow!(
        "{} must match {}; missing source declarations: {}; missing externals: {}",
        external_path,
        source_path,
        format_authoring_list(&missing_source),
        format_authoring_list(&missing_external)
    ))
}

fn format_authoring_list(items: &[String]) -> String {
    if items.is_empty() {
        "<none>".to_string()
    } else {
        items.join(", ")
    }
}

fn named_runtime_secret_values(json_value: &Value) -> Result<Vec<String>> {
    let Some(secrets) = json_value.pointer("/runtime/secrets") else {
        return Ok(Vec::new());
    };
    let secrets = secrets
        .as_array()
        .ok_or_else(|| anyhow!("/runtime/secrets must be an array"))?;
    let mut names = BTreeSet::new();
    for (idx, secret) in secrets.iter().enumerate() {
        let name = secret
            .pointer("/name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("/runtime/secrets/{}/name must be a non-empty string", idx))?;
        names.insert(name.to_string());
    }
    Ok(names.into_iter().collect())
}

fn string_array_values(value: Option<&Value>, context: &str) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow!("{} must be an array", context))?;
    let mut seen = BTreeSet::new();
    let mut strings = Vec::with_capacity(values.len());
    for (idx, value) in values.iter().enumerate() {
        let value = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("{}/{} must be a non-empty string", context, idx))?;
        if !seen.insert(value.to_string()) {
            return Err(anyhow!(
                "{}/{} duplicates '{}'; {} values must be unique",
                context,
                idx,
                value,
                context
            ));
        }
        strings.push(value.to_string());
    }
    Ok(strings)
}

fn default_experiment_name(json_value: &mut Value) -> Result<()> {
    let Some(experiment) = json_value.pointer_mut("/experiment") else {
        return Ok(());
    };
    let experiment = experiment
        .as_object_mut()
        .ok_or_else(|| anyhow!("/experiment must be an object"))?;
    if experiment
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| !name.trim().is_empty())
    {
        return Ok(());
    }
    let id = experiment
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("/experiment/id is required to default /experiment/name"))?;
    experiment.insert("name".to_string(), json!(id));
    Ok(())
}

fn default_object_path(root: &mut Value, path: &[&str]) -> Result<()> {
    let Some((last, parents)) = path.split_last() else {
        return Ok(());
    };
    let parent = ensure_object_path(root, parents)?;
    match parent.get(*last) {
        Some(value) if value.is_object() => Ok(()),
        Some(_) => Err(anyhow!("/{} must be an object", path.join("/"))),
        None => {
            parent.insert((*last).to_string(), Value::Object(Map::new()));
            Ok(())
        }
    }
}

fn insert_default_value(root: &mut Value, path: &[&str], value: Value) -> Result<()> {
    let Some((last, parents)) = path.split_last() else {
        return Ok(());
    };
    let parent = ensure_object_path(root, parents)?;
    parent.entry((*last).to_string()).or_insert(value);
    Ok(())
}

fn normalize_agent_site_default(json_value: &mut Value) -> Result<()> {
    let agent_site = json_value.pointer("/trial_runtime/execution/agent_site");
    if agent_site.is_some() {
        return Ok(());
    }
    let default = inferred_agent_site(json_value);
    let Some(default) = default else {
        if json_value.pointer("/trial_runtime/agent").is_some()
            && json_value.pointer("/trial_runtime/task").is_some()
        {
            return Err(uninferrable_agent_site_error());
        }
        return Ok(());
    };
    let execution = ensure_object_path(json_value, &["trial_runtime", "execution"])?;
    execution.insert("agent_site".to_string(), json!(default));
    Ok(())
}

fn inferred_agent_site(json_value: &Value) -> Option<&'static str> {
    if json_value
        .pointer("/trial_runtime/agent/image")
        .and_then(Value::as_str)
        .is_some_and(|image| !image.trim().is_empty())
    {
        return Some("agent_container");
    }
    let interface = json_value
        .pointer("/trial_runtime/task/interface")
        .and_then(Value::as_str)?;
    match interface {
        "input_only" => Some("host"),
        "writable_workspace"
            if json_value
                .pointer("/trial_runtime/task/workspace/source")
                .and_then(Value::as_str)
                == Some("container_image") =>
        {
            Some("task_runtime")
        }
        _ => None,
    }
}

fn ensure_object_path<'a>(
    root: &'a mut Value,
    path: &[&str],
) -> Result<&'a mut Map<String, Value>> {
    let mut current = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("experiment authoring input must be an object"))?;
    for segment in path {
        let entry = current
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        current = entry
            .as_object_mut()
            .ok_or_else(|| anyhow!("/{} must be an object", segment))?;
    }
    Ok(current)
}

fn normalize_grader_defaults(json_value: &mut Value) -> Result<()> {
    let trial_runtime = ensure_object_path(json_value, &["trial_runtime"])?;
    if !trial_runtime.contains_key("grader") {
        trial_runtime.insert("grader".to_string(), json!({ "strategy": "none" }));
        return Ok(());
    }
    let Some(grader) = json_value.pointer_mut("/trial_runtime/grader") else {
        return Ok(());
    };
    let grader = grader
        .as_object_mut()
        .ok_or_else(|| anyhow!("/grader must be an object"))?;
    if grader.is_empty() {
        return Err(anyhow!(
            "/stages/grader must not be empty; omit it for the no-grader default or declare strategy: none explicitly"
        ));
    }
    if grader.get("strategy").and_then(Value::as_str) == Some("in_task_runtime")
        && grader.get("in_task_runtime").is_none()
    {
        grader.insert("in_task_runtime".to_string(), json!({}));
    }
    if grader.get("strategy").and_then(Value::as_str) != Some("none") {
        grader
            .entry("inputs".to_string())
            .or_insert_with(|| json!({}));
    }
    Ok(())
}

fn normalize_agent_result_output(json_value: &mut Value) -> Result<()> {
    let Some(agent) = json_value.pointer_mut("/trial_runtime/agent") else {
        return Ok(());
    };
    let agent = agent
        .as_object_mut()
        .ok_or_else(|| anyhow!("/agent must be an object"))?;
    if !agent.contains_key("artifact_type") {
        agent.insert("artifact_type".to_string(), json!("structured_json"));
    }
    if agent.remove("result").is_some() {
        return Err(anyhow!(
            "/stages.agent.result is not accepted; omit it because the canonical result output is added by default, and use /stages.agent.outputs only for additional captures"
        ));
    }
    ensure_default_agent_result_output(agent)?;
    Ok(())
}

fn ensure_default_agent_result_output(agent: &mut Map<String, Value>) -> Result<()> {
    let Some(outputs) = agent.get_mut("outputs") else {
        agent.insert("outputs".to_string(), default_agent_result_outputs());
        return Ok(());
    };
    let outputs = outputs
        .as_object_mut()
        .ok_or_else(|| anyhow!("/stages.agent.outputs must be an object"))?;
    if outputs.contains_key("result") {
        return Err(anyhow!(
            "/stages.agent.outputs.result is not accepted; omit it because the canonical result output is added by default"
        ));
    }
    let mut defaults = default_agent_result_outputs();
    let Some(default_result) = defaults
        .as_object_mut()
        .and_then(|defaults| defaults.remove("result"))
    else {
        return Err(anyhow!("internal default agent result output is malformed"));
    };
    outputs.insert("result".to_string(), default_result);
    Ok(())
}

fn default_agent_result_outputs() -> Value {
    json!({
        "result": {
            "capture": {
                "type": "file",
                "path": BUCEPHALUS_RESULT_PATH,
                "format": "json",
                "required": true
            }
        }
    })
}

fn normalize_output_capture_required_flags(json_value: &mut Value) -> Result<()> {
    normalize_output_capture_required_flags_at(json_value, "/trial_runtime/agent/outputs")?;
    normalize_output_capture_required_flags_at(json_value, "/trial_runtime/grader/outputs")?;
    let Some(variants) = json_value
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for (idx, variant) in variants.iter_mut().enumerate() {
        normalize_output_capture_required_flags_at(variant, "/overrides/agent/outputs")
            .with_context(|| format!("/matrix.variants[{}].overrides.agent.outputs", idx))?;
        normalize_output_capture_required_flags_at(variant, "/overrides/grader/outputs")
            .with_context(|| format!("/matrix.variants[{}].overrides.grader.outputs", idx))?;
    }
    Ok(())
}

fn normalize_output_capture_required_flags_at(root: &mut Value, pointer: &str) -> Result<()> {
    let Some(outputs) = root.pointer_mut(pointer) else {
        return Ok(());
    };
    let outputs = outputs
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must be an object", pointer))?;
    for (id, output) in outputs {
        let capture = output
            .pointer_mut("/capture")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow!("{}/{} must declare capture", pointer, id))?;
        let capture_type = capture.get("type").and_then(Value::as_str).map(str::trim);
        if matches!(capture_type, Some("file" | "result_json")) {
            capture
                .entry("required".to_string())
                .or_insert_with(|| json!(true));
        }
    }
    Ok(())
}

fn normalize_grader_input_required_flags(json_value: &mut Value) -> Result<()> {
    normalize_grader_input_required_flags_at(json_value, "/trial_runtime/grader/inputs")?;
    let Some(variants) = json_value
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for (idx, variant) in variants.iter_mut().enumerate() {
        normalize_grader_input_required_flags_at(variant, "/overrides/grader/inputs")
            .with_context(|| format!("/matrix.variants[{}].overrides.grader.inputs", idx))?;
    }
    Ok(())
}

fn normalize_grader_input_required_flags_at(root: &mut Value, pointer: &str) -> Result<()> {
    let Some(inputs) = root.pointer_mut(pointer) else {
        return Ok(());
    };
    let inputs = inputs
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must be an object", pointer))?;
    for input in inputs.values_mut() {
        let input = input
            .as_object_mut()
            .ok_or_else(|| anyhow!("{} values must be objects", pointer))?;
        input
            .entry("required".to_string())
            .or_insert_with(|| json!(true));
    }
    Ok(())
}

fn normalize_agent_integration_levels(json_value: &mut Value) -> Result<()> {
    normalize_agent_integration_level_at(json_value, &["trial_runtime", "agent"])?;
    let Some(variants) = json_value
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for variant in variants {
        let Some(agent) = variant.pointer_mut("/overrides/agent") else {
            continue;
        };
        normalize_agent_integration_level_for_patch(agent)?;
    }
    Ok(())
}

fn normalize_agent_integration_level_at(root: &mut Value, path: &[&str]) -> Result<()> {
    let Some(value) = root.pointer_mut(&format!("/{}", path.join("/"))) else {
        return Ok(());
    };
    normalize_agent_integration_level_for_object(value)
}

fn normalize_agent_integration_level_for_object(agent: &mut Value) -> Result<()> {
    let agent = agent
        .as_object_mut()
        .ok_or_else(|| anyhow!("/trial_runtime/agent must be an object"))?;
    if agent.contains_key("integration_level") {
        return Ok(());
    }
    let has_events = agent
        .get("events")
        .and_then(Value::as_array)
        .is_some_and(|events| !events.is_empty());
    let integration_level = if has_events {
        "cli_events"
    } else {
        "cli_basic"
    };
    agent.insert("integration_level".to_string(), json!(integration_level));
    Ok(())
}

fn normalize_agent_integration_level_for_patch(agent: &mut Value) -> Result<()> {
    let agent = agent
        .as_object_mut()
        .ok_or_else(|| anyhow!("/matrix/variants[].overrides.agent must be an object"))?;
    if agent.contains_key("integration_level") {
        return Ok(());
    }
    let has_events = agent
        .get("events")
        .and_then(Value::as_array)
        .is_some_and(|events| !events.is_empty());
    if has_events {
        agent.insert("integration_level".to_string(), json!("cli_events"));
    }
    Ok(())
}

fn normalize_metric_authoring(json_value: &mut Value) -> Result<()> {
    let Some(metrics) = json_value.get_mut("metrics") else {
        return Ok(());
    };
    let metrics = metrics
        .as_array_mut()
        .ok_or_else(|| anyhow!("/metrics must be an array"))?;
    let single_metric = metrics.len() == 1;
    for (idx, metric) in metrics.iter_mut().enumerate() {
        let context = format!("/metrics/{}", idx);
        let metric = metric
            .as_object_mut()
            .ok_or_else(|| anyhow!("{} must be an object", context))?;
        if single_metric && !metric.contains_key("primary") {
            metric.insert("primary".to_string(), json!(true));
        }
        metric
            .entry("primary".to_string())
            .or_insert_with(|| json!(false));
        metric
            .entry("required".to_string())
            .or_insert_with(|| json!(true));
        let Some(from) = metric.remove("from") else {
            continue;
        };
        if metric.get("source").is_some() {
            return Err(anyhow!(
                "{} declares both 'from' and internal 'source'; use only 'from'",
                context
            ));
        }
        let from = from
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("{}/from must be a non-empty string", context))?;
        let mut source = metric_source_from_public_ref(from, &context)?;
        if let Some(transform) = metric.remove("transform") {
            source
                .as_object_mut()
                .ok_or_else(|| anyhow!("{} lowered source must be an object", context))?
                .insert("transform".to_string(), transform);
        }
        metric.insert("source".to_string(), source);
    }
    Ok(())
}

fn metric_source_from_public_ref(raw: &str, context: &str) -> Result<Value> {
    if let Some(rest) = raw.strip_prefix("result.") {
        return Ok(json!({
            "type": "agent_response",
            "pointer": public_path_to_json_pointer(rest, context)?
        }));
    }
    if raw == "result" {
        return Ok(json!({ "type": "agent_response", "pointer": "" }));
    }
    if let Some(rest) = raw.strip_prefix("grader.") {
        let (output, pointer) = if let Some((output, path)) = rest.split_once('.') {
            (output, public_path_to_json_pointer(path, context)?)
        } else {
            (rest, String::new())
        };
        if output.trim().is_empty() {
            return Err(anyhow!("{}/from has an empty grader output id", context));
        }
        return Ok(json!({
            "type": "grader_output",
            "output": output,
            "pointer": pointer
        }));
    }
    Err(anyhow!(
        "{}/from='{}' is not understood; use result.<field> or grader.<output>.<field>",
        context,
        raw
    ))
}

fn public_path_to_json_pointer(raw: &str, context: &str) -> Result<String> {
    let mut pointer = String::new();
    for segment in raw.split('.') {
        let segment = segment.trim();
        if segment.is_empty() {
            return Err(anyhow!("{} contains an empty path segment", context));
        }
        append_public_path_segment(&mut pointer, segment, context)?;
    }
    Ok(pointer)
}

fn append_public_path_segment(
    pointer: &mut String,
    mut segment: &str,
    context: &str,
) -> Result<()> {
    loop {
        let Some(open) = segment.find('[') else {
            if !segment.is_empty() {
                push_json_pointer_segment(pointer, segment);
            }
            return Ok(());
        };
        let name = &segment[..open];
        if !name.is_empty() {
            push_json_pointer_segment(pointer, name);
        }
        let close = segment[open + 1..]
            .find(']')
            .map(|idx| idx + open + 1)
            .ok_or_else(|| anyhow!("{} contains an unclosed array index", context))?;
        let index = &segment[open + 1..close];
        if index.is_empty() || !index.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(anyhow!(
                "{} contains an invalid array index '{}'",
                context,
                index
            ));
        }
        push_json_pointer_segment(pointer, index);
        segment = &segment[close + 1..];
        if segment.starts_with('[') {
            continue;
        }
        if !segment.is_empty() {
            return Err(anyhow!(
                "{} contains invalid characters after an array index",
                context
            ));
        }
    }
}

fn push_json_pointer_segment(pointer: &mut String, segment: &str) {
    pointer.push('/');
    pointer.push_str(&segment.replace('~', "~0").replace('/', "~1"));
}

fn normalize_trace_policy(json_value: &mut Value) -> Result<()> {
    let Some(traces) = json_value.get("traces").cloned() else {
        return Ok(());
    };
    let traces = traces
        .as_object()
        .ok_or_else(|| anyhow!("/traces must be an object"))?;
    let source = traces
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("/traces.source is required when /traces is declared"))?;
    let retain = traces
        .get("retain")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("never");
    let retain_raw = match retain {
        "never" | "on_failure" => false,
        "always" => true,
        other => {
            return Err(anyhow!(
                "/traces.retain must be one of: never, on_failure, always (got '{}')",
                other
            ));
        }
    };
    if source == "none" {
        return Err(anyhow!(
            "/traces.source=none is not accepted; omit /traces for runner lifecycle events only, or use /traces.source=protocol to ingest command-agent traces"
        ));
    }
    if source != "protocol" {
        return Err(anyhow!(
            "/traces.source must be protocol; omit /traces for runner lifecycle events only (got '{}')",
            source
        ));
    }
    normalize_protocol_trace_source(json_value, retain_raw)?;
    if let Some(object) = json_value.as_object_mut() {
        object.remove("traces");
    }
    Ok(())
}

fn normalize_protocol_trace_source(json_value: &mut Value, retain_raw: bool) -> Result<()> {
    let agent = json_value
        .pointer_mut("/trial_runtime/agent")
        .ok_or_else(|| anyhow!("/traces.source=protocol requires /stages.agent"))?;
    let agent = agent
        .as_object_mut()
        .ok_or_else(|| anyhow!("/stages.agent must be an object"))?;
    let has_command = agent
        .get("command")
        .and_then(Value::as_array)
        .is_some_and(|parts| !parts.is_empty());
    let has_adapter = agent.get("adapter").is_some();
    if !has_command && !has_adapter {
        return Err(anyhow!(
            "/traces.source=protocol requires /stages.agent.command or /stages.agent.adapter"
        ));
    }
    if agent.get("events").is_some() {
        return Ok(());
    }
    agent.insert(
        "events".to_string(),
        json!([{
            "id": "trajectory",
            "format": "jsonl",
            "mode": "jsonl",
            "ingest": true,
            "retain_raw": retain_raw
        }]),
    );
    Ok(())
}

fn normalize_agent_observability_contract(json_value: &mut Value) -> Result<()> {
    normalize_agent_observability_contract_at(json_value, "/trial_runtime/agent")?;
    let Some(variants) = json_value
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for (idx, variant) in variants.iter_mut().enumerate() {
        normalize_agent_observability_contract_at(variant, "/overrides/agent")
            .with_context(|| format!("/matrix.variants[{}].overrides.agent", idx))?;
    }
    Ok(())
}

fn normalize_agent_observability_contract_at(root: &mut Value, pointer: &str) -> Result<()> {
    let Some(agent) = root.pointer_mut(pointer) else {
        return Ok(());
    };
    let agent = agent
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must be an object", pointer))?;
    if let Some(events) = agent.get_mut("events") {
        let events = events
            .as_array_mut()
            .ok_or_else(|| anyhow!("{}.events must be an array", pointer))?;
        for (idx, event) in events.iter_mut().enumerate() {
            let event = event
                .as_object_mut()
                .ok_or_else(|| anyhow!("{}.events[{}] must be an object", pointer, idx))?;
            event
                .entry("format".to_string())
                .or_insert_with(|| json!("jsonl"));
            event
                .entry("mode".to_string())
                .or_insert_with(|| json!("jsonl"));
            event
                .entry("ingest".to_string())
                .or_insert_with(|| json!(true));
            event
                .entry("retain_raw".to_string())
                .or_insert_with(|| json!(false));
        }
    }
    if let Some(output_mounts) = agent.get_mut("output_mounts") {
        let output_mounts = output_mounts
            .as_array_mut()
            .ok_or_else(|| anyhow!("{}.output_mounts must be an array", pointer))?;
        for (idx, mount) in output_mounts.iter_mut().enumerate() {
            let mount = mount
                .as_object_mut()
                .ok_or_else(|| anyhow!("{}.output_mounts[{}] must be an object", pointer, idx))?;
            mount
                .entry("kind".to_string())
                .or_insert_with(|| json!("directory"));
            mount
                .entry("persist".to_string())
                .or_insert_with(|| json!(true));
        }
    }
    Ok(())
}

fn normalize_agent_adapters(json_value: &mut Value) -> Result<()> {
    normalize_agent_adapter_at(json_value, "/trial_runtime/agent", false)?;
    let base_has_event_sink = agent_has_event_sink(json_value, "/trial_runtime/agent");
    let Some(variants) = json_value
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for (idx, variant) in variants.iter_mut().enumerate() {
        normalize_agent_adapter_at(variant, "/overrides/agent", base_has_event_sink)
            .with_context(|| format!("/matrix.variants[{}].overrides.agent", idx))?;
    }
    Ok(())
}

fn agent_has_event_sink(root: &Value, pointer: &str) -> bool {
    root.pointer(pointer)
        .and_then(|agent| agent.get("events"))
        .and_then(Value::as_array)
        .is_some_and(|events| !events.is_empty())
}

fn normalize_agent_adapter_at(
    root: &mut Value,
    pointer: &str,
    inherited_event_sink: bool,
) -> Result<()> {
    let Some(agent) = root.pointer_mut(pointer) else {
        return Ok(());
    };
    let agent = agent
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must be an object", pointer))?;
    let Some(adapter) = agent.get("adapter").cloned() else {
        return Ok(());
    };
    if agent.contains_key("command") {
        return Err(anyhow!(
            "{} cannot declare both command and adapter; adapter owns the command contract",
            pointer
        ));
    }
    let context = format!("{}.adapter", pointer);
    let has_local_event_sink = agent
        .get("events")
        .and_then(Value::as_array)
        .is_some_and(|events| !events.is_empty());
    let has_event_sink =
        has_local_event_sink || (agent.get("events").is_none() && inherited_event_sink);
    let adapter_events = adapter
        .as_object()
        .and_then(|adapter| adapter.get("events"))
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| anyhow!("{}.events must be a boolean", context))
        })
        .transpose()?;
    let event_argv_enabled = adapter_events.unwrap_or(has_event_sink);
    if event_argv_enabled && !has_event_sink {
        return Err(anyhow!(
            "{}.events=true requires /traces.source: protocol or an explicit /stages.agent.events sink",
            context
        ));
    }
    let command = nova_adapter_command(&adapter, &context, event_argv_enabled)?;
    agent.insert("command".to_string(), json!(command));
    Ok(())
}

fn adapter_string<'a>(
    adapter: &'a Map<String, Value>,
    key: &str,
    default: Option<&'a str>,
    context: &str,
) -> Result<Option<String>> {
    match adapter.get(key) {
        Some(value) => value
            .as_str()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(Some)
            .ok_or_else(|| anyhow!("{}.{} must be a non-empty string", context, key)),
        None => Ok(default.map(ToString::to_string)),
    }
}

fn adapter_bool(
    adapter: &Map<String, Value>,
    key: &str,
    default: bool,
    context: &str,
) -> Result<bool> {
    match adapter.get(key) {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| anyhow!("{}.{} must be a boolean", context, key)),
        None => Ok(default),
    }
}

fn adapter_u64(adapter: &Map<String, Value>, key: &str, context: &str) -> Result<Option<u64>> {
    match adapter.get(key) {
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| anyhow!("{}.{} must be a positive integer", context, key)),
        None => Ok(None),
    }
}

fn adapter_string_array(
    adapter: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Vec<String>> {
    let Some(value) = adapter.get(key) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| anyhow!("{}.{} must be an argv array", context, key))?;
    array
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            item.as_str()
                .map(ToString::to_string)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow!("{}.{}[{}] must be a non-empty string", context, key, idx))
        })
        .collect()
}

fn nova_adapter_command(
    adapter: &Value,
    context: &str,
    event_argv_enabled: bool,
) -> Result<Vec<String>> {
    let adapter = adapter
        .as_object()
        .ok_or_else(|| anyhow!("{} must be an object", context))?;
    for key in adapter.keys() {
        if !matches!(
            key.as_str(),
            "kind"
                | "executable"
                | "config"
                | "mcp_config"
                | "working_dir"
                | "timeout_ms"
                | "dangerous"
                | "events"
                | "provider"
                | "model"
                | "provider_env"
                | "result"
                | "args"
        ) {
            return Err(anyhow!("{}.{} is not supported", context, key));
        }
    }
    let kind = adapter_string(adapter, "kind", None, context)?
        .ok_or_else(|| anyhow!("{}.kind is required", context))?;
    if kind != "nova" {
        return Err(anyhow!(
            "{}.kind must be nova for the built-in adapter (got '{}')",
            context,
            kind
        ));
    }
    let result = adapter_string(adapter, "result", Some("structured_json"), context)?
        .ok_or_else(|| anyhow!("{}.result is required", context))?;
    if result != "structured_json" {
        return Err(anyhow!(
            "{}.result must be structured_json (got '{}')",
            context,
            result
        ));
    }

    let mut command = vec![
        adapter_string(adapter, "executable", Some("/usr/local/bin/nova"), context)?.unwrap(),
        "run".to_string(),
        "--input-file".to_string(),
        "__BUCEPHALUS_TRIAL_INPUT_PATH__".to_string(),
        "--output".to_string(),
        "__BUCEPHALUS_RESULT_PATH__".to_string(),
    ];
    if event_argv_enabled {
        command.extend([
            "--events".to_string(),
            "__BUCEPHALUS_TRAJECTORY_PATH__".to_string(),
        ]);
    }
    if let Some(config) = adapter_string(adapter, "config", None, context)? {
        command.extend(["--config".to_string(), config]);
    }
    if let Some(mcp_config) = adapter_string(adapter, "mcp_config", None, context)? {
        command.extend(["--mcp-config".to_string(), mcp_config]);
    }
    command.extend([
        "--working-dir".to_string(),
        adapter_string(
            adapter,
            "working_dir",
            Some(BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER),
            context,
        )?
        .unwrap(),
    ]);
    if let Some(timeout_ms) = adapter_u64(adapter, "timeout_ms", context)? {
        command.extend(["--timeout-ms".to_string(), timeout_ms.to_string()]);
    }
    if adapter_bool(adapter, "dangerous", false, context)? {
        command.push("--dangerous".to_string());
    }
    if let Some(provider_env) = adapter.get("provider_env") {
        let provider_env = provider_env
            .as_object()
            .ok_or_else(|| anyhow!("{}.provider_env must be an object", context))?;
        let mut entries = provider_env.iter().collect::<Vec<_>>();
        entries.sort_by_key(|(provider, _)| provider.as_str());
        for (provider, env_name) in entries {
            let env_name = env_name
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow!(
                        "{}.provider_env.{} must be a non-empty environment variable name",
                        context,
                        provider
                    )
                })?;
            command.extend([
                "--provider-env".to_string(),
                format!("{provider}={env_name}"),
            ]);
        }
    }
    command.extend([
        "--provider".to_string(),
        adapter_string(adapter, "provider", Some("$provider"), context)?.unwrap(),
        "--model".to_string(),
        adapter_string(adapter, "model", Some("$model"), context)?.unwrap(),
    ]);
    command.extend(adapter_string_array(adapter, "args", context)?);
    Ok(command)
}

fn normalize_public_stage_ephemerals(stage_name: &str, value: &mut Value) -> Result<()> {
    if !matches!(stage_name, "agent" | "grader") {
        return Ok(());
    }
    lower_child_value(value, "services", "sidecars")?;
    lower_child_value(value, "ephemerals", "sidecars")?;
    Ok(())
}

fn normalize_service_lifecycles(root: &mut Value, pointer: &str) -> Result<()> {
    let Some(services) = root.pointer_mut(pointer) else {
        return Ok(());
    };
    let services = services
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must be an object", pointer))?;
    for (id, service) in services {
        let service = service
            .as_object_mut()
            .ok_or_else(|| anyhow!("{}/{} must be an object", pointer, id))?;
        let Some(lifecycle) = service.get_mut("lifecycle") else {
            continue;
        };
        let Some(raw) = lifecycle.as_str() else {
            continue;
        };
        if raw == "trial" {
            *lifecycle = json!("per-trial");
        }
    }
    Ok(())
}

fn normalize_variant_overrides(json_value: &mut Value) -> Result<()> {
    let Some(variants) = json_value
        .pointer_mut("/matrix/variants")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for (idx, variant) in variants.iter_mut().enumerate() {
        let Some(overrides) = variant.get_mut("overrides") else {
            continue;
        };
        let overrides = overrides
            .as_object_mut()
            .ok_or_else(|| anyhow!("/matrix/variants/{}/overrides must be an object", idx))?;
        if let Some(case) = overrides.remove("case") {
            insert_lowered_value(
                overrides,
                "task",
                case,
                &format!("/matrix/variants/{}/overrides", idx),
            )?;
        }
        for stage_name in ["agent", "grader"] {
            if let Some(stage) = overrides.get_mut(stage_name) {
                normalize_public_stage_ephemerals(stage_name, stage)?;
            }
        }
    }
    Ok(())
}

fn lower_registry_image_rewrites(json_value: &mut Value) -> Result<()> {
    let Some(registry) = json_value
        .pointer_mut("/runtime/registry")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    let Some(rewrites) = registry.remove("image_rewrites") else {
        return Ok(());
    };
    let image = ensure_object_path(json_value, &["trial_runtime", "task", "workspace", "image"])?;
    insert_lowered_value(
        image,
        "rewrites",
        rewrites,
        "authoring lowering for runtime.registry.image_rewrites",
    )?;
    if json_value
        .pointer("/runtime/registry")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        if let Some(runtime) = json_value
            .pointer_mut("/runtime")
            .and_then(Value::as_object_mut)
        {
            runtime.remove("registry");
        }
    }
    Ok(())
}

fn lower_top_level_value(json_value: &mut Value, source: &str, target: &[&str]) -> Result<()> {
    let Some(value) = json_value.get(source).cloned() else {
        return Ok(());
    };
    lower_object_value(json_value, target, value)?;
    if let Some(object) = json_value.as_object_mut() {
        object.remove(source);
    }
    Ok(())
}

fn lower_object_value(root: &mut Value, target: &[&str], value: Value) -> Result<()> {
    if target.is_empty() {
        return Ok(());
    }
    let mut current = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("experiment authoring input must be an object"))?;
    for segment in &target[..target.len() - 1] {
        let entry = current
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        current = entry
            .as_object_mut()
            .ok_or_else(|| anyhow!("/{} must be an object", segment))?;
    }
    insert_lowered_value(
        current,
        target[target.len() - 1],
        value,
        "authoring lowering",
    )
}

fn lower_child_value(root: &mut Value, source: &str, target: &str) -> Result<()> {
    let Some(value) = root.get(source).cloned() else {
        return Ok(());
    };
    let object = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("stage authoring input must be an object"))?;
    insert_lowered_value(object, target, value, "authoring lowering")?;
    object.remove(source);
    Ok(())
}

fn insert_lowered_value(
    object: &mut Map<String, Value>,
    key: &str,
    value: Value,
    context: &str,
) -> Result<()> {
    if object.contains_key(key) {
        return Err(anyhow!(
            "{} target '{}' already exists; use the public authoring field only",
            context,
            key
        ));
    }
    object.insert(key.to_string(), value);
    Ok(())
}

pub(crate) fn resolve_agent_artifact_path(raw: &str, exp_dir: &Path) -> Result<PathBuf> {
    let trimmed = raw.trim();
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Ok(normalize_path(candidate));
    }
    if trimmed.starts_with("./") || trimmed.starts_with("../") || trimmed.contains('/') {
        return Ok(normalize_path(&exp_dir.join(candidate)));
    }

    let agents_root = crate::local_storage::default_agent_root()
        .context("resolve default agent root for shorthand artifact reference")?;
    let direct = agents_root.join(trimmed);
    if direct.exists() {
        return Ok(normalize_path(&direct));
    }
    for ext in [".tar.gz", ".tgz", ".tar"] {
        let with_ext = agents_root.join(format!("{}{}", trimmed, ext));
        if with_ext.exists() {
            return Ok(normalize_path(&with_ext));
        }
    }
    Ok(normalize_path(&direct))
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
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry?;
        let p = entry.path();
        if p == path {
            continue;
        }
        if !should_include_agent_artifact_path(path, p) {
            continue;
        }
        let rel = p
            .strip_prefix(path)
            .with_context(|| {
                format!(
                    "artifact entry {} escaped root {}",
                    p.display(),
                    path.display()
                )
            })?
            .to_string_lossy()
            .replace('\\', "/");
        let meta = fs::symlink_metadata(p)?;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(p)
                .with_context(|| format!("read artifact symlink target {}", p.display()))?
                .to_string_lossy()
                .to_string();
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
        || trimmed.starts_with(BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_case_stage_ephemeral_authoring_nouns() {
        let mut value = json!({
            "matrix": {
                "cases": { "path": "cases.jsonl" },
                "repeats": 1
            },
            "stages": {
                "case": {},
                "agent": {
                    "command": ["agent"],
                    "ephemerals": ["mcp"]
                },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            },
            "ephemerals": {
                "mcp": {
                    "image": "ghcr.io/acme/mcp:latest",
                    "lifecycle": "per-trial"
                }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/matrix/tasks/path"),
            Some(&json!("cases.jsonl"))
        );
        assert_eq!(value.pointer("/matrix/tasks/source"), Some(&json!("file")));
        assert_eq!(
            value.pointer("/trial_runtime/task/interface"),
            Some(&json!("input_only"))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0"),
            Some(&json!({ "id": "baseline", "baseline": true, "config": {} }))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/sidecars"),
            Some(&json!(["mcp"]))
        );
        assert!(value.pointer("/sidecars/mcp").is_some());
    }

    #[test]
    fn normalizes_services_authoring_nouns_and_trial_lifecycle() {
        let mut value = json!({
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": {
                    "command": ["agent"],
                    "services": ["pg-data-api"]
                },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            },
            "services": {
                "pg-data-api": {
                    "image": "python:3.11-slim",
                    "lifecycle": "trial",
                    "readiness": {
                        "http": {
                            "url": "http://127.0.0.1:9757",
                            "method": "POST",
                            "json": { "case_id": "pg_001", "command": "overview" }
                        },
                        "timeout_ms": 10000
                    }
                }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/agent/sidecars"),
            Some(&json!(["pg-data-api"]))
        );
        assert_eq!(
            value.pointer("/sidecars/pg-data-api/lifecycle"),
            Some(&json!("per-trial"))
        );
        assert_eq!(
            value.pointer("/sidecars/pg-data-api/readiness/http/json/case_id"),
            Some(&json!("pg_001"))
        );
        assert!(value.pointer("/services").is_none());
        assert!(value.pointer("/trial_runtime/agent/services").is_none());
    }

    #[test]
    fn normalizes_nova_agent_adapter_to_contract_command() {
        let mut value = json!({
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": {
                    "adapter": {
                        "kind": "nova",
                        "config": "/opt/agent/nova-config.json",
                        "mcp_config": "/opt/agent/mcp.json",
                        "timeout_ms": 540000,
                        "dangerous": true,
                        "provider_env": {
                            "gemini": "GOOGLE_API_KEY"
                        }
                    }
                },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            },
            "traces": { "source": "protocol" }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        let command = value
            .pointer("/trial_runtime/agent/command")
            .and_then(Value::as_array)
            .expect("adapter command");
        let command_has = |flag: &str, expected: &str| {
            command.windows(2).any(|pair| {
                pair.first() == Some(&json!(flag)) && pair.get(1) == Some(&json!(expected))
            })
        };
        assert_eq!(command.first(), Some(&json!("/usr/local/bin/nova")));
        assert!(command.contains(&json!("__BUCEPHALUS_TRIAL_INPUT_PATH__")));
        assert!(command.contains(&json!("__BUCEPHALUS_RESULT_PATH__")));
        assert!(command.contains(&json!("__BUCEPHALUS_TRAJECTORY_PATH__")));
        assert!(command_has("--mcp-config", "/opt/agent/mcp.json"));
        assert!(command_has("--provider-env", "gemini=GOOGLE_API_KEY"));
        assert_eq!(
            value.pointer("/trial_runtime/agent/adapter/kind"),
            Some(&json!("nova"))
        );
        assert!(value.pointer("/traces").is_none());
        assert!(value.pointer("/trial_runtime/agent/events/0/id").is_some());
    }

    #[test]
    fn nova_agent_adapter_without_traces_omits_event_argv() {
        let mut value = json!({
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": {
                    "adapter": {
                        "kind": "nova",
                        "config": "/opt/agent/nova-config.json"
                    }
                },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        let command = value
            .pointer("/trial_runtime/agent/command")
            .and_then(Value::as_array)
            .expect("adapter command");
        assert!(!command.contains(&json!("--events")));
        assert!(!command.contains(&json!("__BUCEPHALUS_TRAJECTORY_PATH__")));
        assert!(value.pointer("/trial_runtime/agent/events").is_none());
        assert_eq!(
            value.pointer("/trial_runtime/agent/integration_level"),
            Some(&json!("cli_basic"))
        );
    }

    #[test]
    fn variant_nova_agent_adapter_inherits_base_trace_sink() {
        let mut value = json!({
            "matrix": {
                "cases": { "path": "cases.jsonl" },
                "variants": [
                    { "id": "base", "baseline": true },
                    {
                        "id": "nova-treatment",
                        "overrides": {
                            "agent": {
                                "adapter": {
                                    "kind": "nova",
                                    "config": "/opt/agent/treatment-nova.json"
                                }
                            }
                        }
                    }
                ]
            },
            "stages": {
                "case": {},
                "agent": {
                    "command": ["baseline-agent"]
                },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            },
            "traces": { "source": "protocol" }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        let command = value
            .pointer("/matrix/variants/1/overrides/agent/command")
            .and_then(Value::as_array)
            .expect("variant adapter command");
        assert!(command.contains(&json!("--events")));
        assert!(command.contains(&json!("__BUCEPHALUS_TRAJECTORY_PATH__")));
        assert!(value
            .pointer("/matrix/variants/1/overrides/agent/events")
            .is_none());
    }

    #[test]
    fn nova_agent_adapter_events_true_requires_event_sink() {
        let mut value = json!({
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": {
                    "adapter": {
                        "kind": "nova",
                        "events": true
                    }
                },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        assert!(
            err.to_string()
                .contains("events=true requires /traces.source: protocol"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn defaults_case_interface_from_resource_shape() {
        let mut value = json!({
            "matrix": {
                "variants": [{
                    "id": "base",
                    "overrides": {
                        "case": {
                            "files": {
                                "source": "files",
                                "path": "case-files",
                                "mount_path": "/case"
                            }
                        }
                    }
                }]
            },
            "stages": {
                "case": {
                    "workspace": {
                        "source": "container_image",
                        "image": { "from": "case_row" },
                        "workdir": { "from": "case_row" }
                    }
                },
                "agent": {
                    "command": ["agent"]
                }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/task/interface"),
            Some(&json!("writable_workspace"))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/task/interface"),
            Some(&json!("readonly_files"))
        );
    }

    #[test]
    fn rejects_ambiguous_case_interface_default() {
        let mut value = json!({
            "stages": {
                "case": {
                    "files": {
                        "source": "files",
                        "path": "case-files",
                        "mount_path": "/case"
                    },
                    "workspace": {
                        "source": "empty"
                    }
                },
                "agent": {
                    "command": ["agent"]
                }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value)
            .expect_err("ambiguous case resources should fail");

        assert!(
            err.to_string()
                .contains("/stages.case cannot declare both files and workspace"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_case_interface_resource_mismatch() {
        let cases = [
            (
                json!({
                    "stages": {
                        "case": {
                            "interface": "input_only",
                            "files": {
                                "source": "files",
                                "path": "case-files",
                                "mount_path": "/case"
                            }
                        },
                        "agent": { "command": ["agent"] }
                    }
                }),
                "/stages.case declares interface input_only with a case resource",
            ),
            (
                json!({
                    "stages": {
                        "case": {
                            "interface": "readonly_files",
                            "workspace": { "source": "empty" }
                        },
                        "agent": { "command": ["agent"] }
                    }
                }),
                "/stages.case declares interface readonly_files with workspace",
            ),
            (
                json!({
                    "matrix": {
                        "variants": [{
                            "id": "base",
                            "overrides": {
                                "case": {
                                    "interface": "writable_workspace",
                                    "files": {
                                        "source": "files",
                                        "path": "case-files",
                                        "mount_path": "/case"
                                    }
                                }
                            }
                        }]
                    },
                    "stages": {
                        "case": { "interface": "input_only" },
                        "agent": { "command": ["agent"] }
                    }
                }),
                "/matrix/variants/0/overrides/case declares interface writable_workspace with files",
            ),
        ];

        for (mut value, expected) in cases {
            let err = normalize_authoring_vocabulary(&mut value)
                .expect_err("mismatched case interface resources should fail");

            assert!(
                err.to_string().contains(expected),
                "expected {expected:?}, got: {err}"
            );
        }
    }

    #[test]
    fn derives_externals_from_runtime_contract_when_omitted() {
        let mut value = json!({
            "runtime": {
                "secrets": [
                    { "name": "CODEX_OAUTH", "from": "file", "mount": { "target": "/root/.codex/auth.json" } },
                    { "name": "OPENAI_API_KEY", "from": "env" }
                ],
                "network": { "egress": ["api.openai.com"] }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/runtime/externals/credentials"),
            Some(&json!(["CODEX_OAUTH", "OPENAI_API_KEY"]))
        );
        assert_eq!(
            value.pointer("/runtime/externals/apis"),
            Some(&json!(["api.openai.com"]))
        );
    }

    #[test]
    fn rejects_duplicate_external_accounting_values() {
        for (mut value, expected) in [
            (
                json!({
                    "runtime": {
                        "network": { "egress": ["api.openai.com", "api.openai.com"] }
                    }
                }),
                "/runtime/network/egress/1 duplicates 'api.openai.com'",
            ),
            (
                json!({
                    "externals": {
                        "apis": ["api.openai.com", "api.openai.com"]
                    },
                    "runtime": {
                        "network": { "egress": ["api.openai.com"] }
                    }
                }),
                "/externals/apis/1 duplicates 'api.openai.com'",
            ),
            (
                json!({
                    "externals": {
                        "credentials": ["OPENAI_API_KEY", "OPENAI_API_KEY"]
                    },
                    "runtime": {
                        "secrets": [{ "name": "OPENAI_API_KEY", "from": "env" }]
                    }
                }),
                "/externals/credentials/1 duplicates 'OPENAI_API_KEY'",
            ),
        ] {
            let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
            let msg = err.to_string();

            assert!(
                msg.contains(expected) && msg.contains("must be unique"),
                "expected {expected:?}, got: {msg}"
            );
        }
    }

    #[test]
    fn defaults_secret_sources_from_provider_shape() {
        let mut value = json!({
            "runtime": {
                "secrets": [
                    { "name": "OPENAI_API_KEY" },
                    {
                        "name": "CODEX_OAUTH",
                        "mount": { "target": "/root/.codex/auth.json" }
                    },
                    {
                        "name": "EXPLICIT_FILE",
                        "from": "file",
                        "mount": { "target": "/run/secrets/explicit.json" }
                    }
                ]
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/runtime/secrets/0/from"),
            Some(&json!("env"))
        );
        assert_eq!(
            value.pointer("/runtime/secrets/1/from"),
            Some(&json!("file"))
        );
        assert_eq!(
            value.pointer("/runtime/secrets/2/from"),
            Some(&json!("file"))
        );
    }

    #[test]
    fn rejects_duplicate_runtime_secret_names() {
        let mut value = json!({
            "runtime": {
                "secrets": [
                    { "name": "OPENAI_API_KEY" },
                    { "name": "OPENAI_API_KEY", "from": "env" }
                ]
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/runtime/secrets/1 duplicates secret name 'OPENAI_API_KEY'")
                && msg.contains("must be unique"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn rejects_invalid_runtime_secret_provider_shapes() {
        for (mut value, expected) in [
            (
                json!({
                    "runtime": {
                        "secrets": [{
                            "name": "OPENAI_API_KEY",
                            "from": "env",
                            "mount": { "target": "/run/secrets/openai" }
                        }]
                    }
                }),
                "/runtime/secrets/0 declares from=env with mount",
            ),
            (
                json!({
                    "runtime": {
                        "secrets": [{
                            "name": "CODEX_AUTH",
                            "from": "env",
                            "mount": { "target": "/root/.codex/auth.json" },
                            "credential_cache": {
                                "kind": "run_scoped",
                                "target": "/bucephalus/credentials/codex/auth.json"
                            }
                        }]
                    }
                }),
                "/runtime/secrets/0 declares from=env with mount",
            ),
            (
                json!({
                    "runtime": {
                        "secrets": [{
                            "name": "CODEX_AUTH",
                            "from": "file"
                        }]
                    }
                }),
                "/runtime/secrets/0 declares from=file without mount",
            ),
        ] {
            let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
            let msg = err.to_string();

            assert!(msg.contains(expected), "expected {expected:?}, got: {msg}");
        }
    }

    #[test]
    fn rejects_duplicate_credential_cache_env_names() {
        let mut value = json!({
            "runtime": {
                "secrets": [
                    {
                        "name": "CODEX_AUTH_A",
                        "from": "file",
                        "mount": { "target": "/run/secrets/codex-a.json" },
                        "credential_cache": {
                            "kind": "run_scoped",
                            "target": "/bucephalus/credentials/codex-a/auth.json",
                            "env": "CODEX_AUTH_CACHE_FILE"
                        }
                    },
                    {
                        "name": "CODEX_AUTH_B",
                        "from": "file",
                        "mount": { "target": "/run/secrets/codex-b.json" },
                        "credential_cache": {
                            "kind": "run_scoped",
                            "target": "/bucephalus/credentials/codex-b/auth.json",
                            "env": "CODEX_AUTH_CACHE_FILE"
                        }
                    }
                ]
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains(
                "/runtime/secrets/1/credential_cache/env duplicates 'CODEX_AUTH_CACHE_FILE'"
            ) && msg.contains("must be unique"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn rejects_credential_cache_without_secret_mount() {
        let mut value = json!({
            "runtime": {
                "secrets": [{
                    "name": "CODEX_OAUTH",
                    "credential_cache": {
                        "kind": "run_scoped",
                        "target": "/bucephalus/credentials/codex_oauth/auth.json"
                    }
                }]
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/runtime/secrets/0 declares credential_cache without mount")
                && msg.contains("mount.target"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn preserves_matching_explicit_externals_when_deriving_missing_fields() {
        let mut value = json!({
            "externals": { "apis": ["api.openai.com"] },
            "runtime": {
                "secrets": [{ "name": "OPENAI_API_KEY", "from": "env" }],
                "network": { "egress": ["api.openai.com"] }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/runtime/externals/apis"),
            Some(&json!(["api.openai.com"]))
        );
        assert_eq!(
            value.pointer("/runtime/externals/credentials"),
            Some(&json!(["OPENAI_API_KEY"]))
        );
    }

    #[test]
    fn rejects_explicit_externals_that_drift_from_runtime_contract() {
        for (mut value, expected) in [
            (
                json!({
                    "externals": { "apis": ["api.anthropic.com"] },
                    "runtime": {
                        "network": { "egress": ["api.openai.com"] }
                    }
                }),
                "/externals/apis must match /runtime/network/egress",
            ),
            (
                json!({
                    "externals": { "credentials": ["ANTHROPIC_API_KEY"] },
                    "runtime": {
                        "secrets": [{ "name": "OPENAI_API_KEY", "from": "env" }]
                    }
                }),
                "/externals/credentials must match /runtime/secrets names",
            ),
        ] {
            let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
            let msg = err.to_string();

            assert!(
                msg.contains(expected) && !msg.contains("/runtime/externals"),
                "expected public externals error {expected:?}, got: {msg}"
            );
        }
    }

    #[test]
    fn normalizes_public_noun_authoring_and_metric_refs() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }],
                "repeats": 1
            },
            "stages": {
                "case": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": { "from": "case_row" },
                        "workdir": { "from": "case_row" }
                    }
                },
                "agent": {
                    "image": "python:3.11-slim",
                    "command": ["agent"]
                },
                "grader": { "strategy": "none" }
            },
            "metrics": [{
                "id": "risk",
                "from": "result.alerts[0].revenue_at_risk",
                "primary": true
            }, {
                "id": "pass_rate",
                "from": "grader.report",
                "transform": { "type": "pytest_json_report_pass_rate" }
            }]
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/matrix/tasks/path"),
            Some(&json!("cases.jsonl"))
        );
        assert_eq!(value.pointer("/matrix/variants/0/id"), Some(&json!("base")));
        assert_eq!(
            value.pointer("/trial_runtime/agent/artifact_type"),
            Some(&json!("structured_json"))
        );
        assert!(value.pointer("/trial_runtime/agent/protocol").is_none());
        assert_eq!(
            value.pointer("/trial_runtime/execution/agent_site"),
            Some(&json!("agent_container"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/outputs/result/capture/path"),
            Some(&json!(BUCEPHALUS_RESULT_PATH))
        );
        assert_eq!(
            value.pointer("/metrics/0/source"),
            Some(&json!({
                "type": "agent_response",
                "pointer": "/alerts/0/revenue_at_risk"
            }))
        );
        assert_eq!(
            value.pointer("/metrics/1/source"),
            Some(&json!({
                "type": "grader_output",
                "output": "report",
                "pointer": "",
                "transform": { "type": "pytest_json_report_pass_rate" }
            }))
        );
        assert!(value.pointer("/cases").is_none());
        assert!(value.pointer("/agent").is_none());
    }

    #[test]
    fn defaults_single_metric_to_primary_without_overriding_intent() {
        let mut single = json!({
            "metrics": [{
                "id": "resolved",
                "from": "result.metrics.resolved"
            }]
        });

        normalize_authoring_vocabulary(&mut single).unwrap();

        assert_eq!(single.pointer("/metrics/0/primary"), Some(&json!(true)));
        assert_eq!(single.pointer("/metrics/0/required"), Some(&json!(true)));

        let mut explicit_false = json!({
            "metrics": [{
                "id": "resolved",
                "from": "result.metrics.resolved",
                "primary": false
            }]
        });

        normalize_authoring_vocabulary(&mut explicit_false).unwrap();

        assert_eq!(
            explicit_false.pointer("/metrics/0/primary"),
            Some(&json!(false))
        );
        assert_eq!(
            explicit_false.pointer("/metrics/0/required"),
            Some(&json!(true))
        );

        let mut multiple = json!({
            "metrics": [{
                "id": "resolved",
                "from": "result.metrics.resolved"
            }, {
                "id": "score",
                "from": "result.metrics.score"
            }]
        });

        normalize_authoring_vocabulary(&mut multiple).unwrap();

        assert_eq!(multiple.pointer("/metrics/0/primary"), Some(&json!(false)));
        assert_eq!(multiple.pointer("/metrics/1/primary"), Some(&json!(false)));
        assert_eq!(multiple.pointer("/metrics/0/required"), Some(&json!(true)));
        assert_eq!(multiple.pointer("/metrics/1/required"), Some(&json!(true)));
    }

    #[test]
    fn preserves_optional_metric_required_intent() {
        let mut value = json!({
            "metrics": [{
                "id": "diagnostic_latency",
                "from": "result.metrics.latency",
                "required": false
            }]
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(value.pointer("/metrics/0/primary"), Some(&json!(true)));
        assert_eq!(value.pointer("/metrics/0/required"), Some(&json!(false)));
    }

    #[test]
    fn normalizes_default_result_output_with_extra_agent_outputs() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{
                    "id": "base",
                    "baseline": true,
                    "config": {},
                    "overrides": {
                        "agent": {
                            "outputs": {
                                "variant_answer": {
                                    "capture": {
                                        "type": "file",
                                        "path": "/bucephalus/out/variant-answer.json",
                                        "format": "json"
                                    }
                                }
                            }
                        },
                        "grader": {
                            "outputs": {
                                "variant_score": {
                                    "capture": {
                                        "type": "result_json",
                                        "path": "/bucephalus/out/variant-score.json",
                                        "field": "/score"
                                    }
                                }
                            }
                        }
                    }
                }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "outputs": {
                        "candidate_patch": {
                            "capture": {
                                "type": "workspace_diff",
                                "format": "unified_diff"
                            }
                        }
                    }
                },
                "grader": { "strategy": "none" }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/agent/outputs/result/capture/path"),
            Some(&json!(BUCEPHALUS_RESULT_PATH))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/outputs/candidate_patch/capture/type"),
            Some(&json!("workspace_diff"))
        );
        assert!(value
            .pointer("/trial_runtime/agent/outputs/candidate_patch/capture/required")
            .is_none());
        assert_eq!(
            value.pointer("/trial_runtime/execution/agent_site"),
            Some(&json!("host"))
        );
    }

    #[test]
    fn normalizes_file_output_required_flags_for_resolved_packages() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{
                    "id": "base",
                    "baseline": true,
                    "config": {},
                    "overrides": {
                        "agent": {
                            "outputs": {
                                "variant_answer": {
                                    "capture": {
                                        "type": "file",
                                        "path": "/bucephalus/out/variant-answer.txt"
                                    }
                                }
                            }
                        },
                        "grader": {
                            "outputs": {
                                "variant_score": {
                                    "capture": {
                                        "type": "result_json",
                                        "path": "/bucephalus/out/variant-score.json",
                                        "field": "/score"
                                    }
                                }
                            }
                        }
                    }
                }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "outputs": {
                        "answer": {
                            "capture": {
                                "type": "result_json",
                                "path": "/bucephalus/out/result.json",
                                "field": "/answer"
                            }
                        }
                    }
                },
                "grader": {
                    "strategy": "in_task_runtime",
                    "command": ["python3", "grade.py"],
                    "outputs": {
                        "score": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/score.json",
                                "format": "json"
                            }
                        }
                    }
                }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/agent/outputs/answer/capture/required"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/trial_runtime/grader/outputs/score/capture/required"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer(
                "/matrix/variants/0/overrides/agent/outputs/variant_answer/capture/required"
            ),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer(
                "/matrix/variants/0/overrides/grader/outputs/variant_score/capture/required"
            ),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/trial_runtime/grader/inputs"),
            Some(&json!({}))
        );
    }

    #[test]
    fn normalizes_grader_input_required_flags_for_resolved_packages() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{
                    "id": "base",
                    "baseline": true,
                    "config": {},
                    "overrides": {
                        "grader": {
                            "inputs": {
                                "variant_prompt": {
                                    "source": { "case": "input.prompt" },
                                    "materialize": {
                                        "as": "json_file",
                                        "path": "/bucephalus/out/grader_inputs/variant_prompt.json"
                                    }
                                }
                            }
                        }
                    }
                }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "grader": {
                    "strategy": "in_task_runtime",
                    "command": ["python3", "grade.py"],
                    "inputs": {
                        "prompt": {
                            "source": { "case": "input.prompt" },
                            "materialize": {
                                "as": "json_file",
                                "path": "/bucephalus/out/grader_inputs/prompt.json"
                            }
                        },
                        "notes": {
                            "source": { "case": "metadata.notes" },
                            "materialize": {
                                "as": "json_file",
                                "path": "/bucephalus/out/grader_inputs/notes.json"
                            },
                            "required": false
                        }
                    }
                }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/grader/inputs/prompt/required"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/trial_runtime/grader/inputs/notes/required"),
            Some(&json!(false))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/grader/inputs/variant_prompt/required"),
            Some(&json!(true))
        );
    }

    #[test]
    fn rejects_removed_agent_result_authoring_knob() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "result": "structured_json"
                },
                "grader": { "strategy": "none" }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/stages.agent.result is not accepted"),
            "agent result error should use public path: {msg}"
        );
        assert!(
            !msg.contains("trial_runtime"),
            "agent result error should not leak resolved runtime paths: {msg}"
        );
    }

    #[test]
    fn rejects_authored_canonical_agent_result_output() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/tmp/result.json",
                                "format": "json"
                            }
                        }
                    }
                },
                "grader": { "strategy": "none" }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/stages.agent.outputs.result is not accepted"),
            "canonical result output should be runner-owned: {msg}"
        );
        assert!(
            !msg.contains("trial_runtime"),
            "canonical result output error should not leak resolved runtime paths: {msg}"
        );
    }

    #[test]
    fn rejects_agent_result_metric_ref_alias() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "grader": { "strategy": "none" }
            },
            "metrics": [{
                "id": "score",
                "from": "agent.result.score"
            }]
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/metrics/0/from='agent.result.score' is not understood"),
            "agent.result metric ref should be rejected by name: {msg}"
        );
        assert!(
            msg.contains("use result.<field> or grader.<output>.<field>"),
            "agent.result metric ref should point users at public refs: {msg}"
        );
    }

    #[test]
    fn normalizes_safe_authoring_defaults() {
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "mode": "answer" },
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/experiment/name"),
            Some(&json!("smoke_eval"))
        );
        assert!(
            value.pointer("/experiment/mode").is_none(),
            "authoring-only mode should not survive normalization"
        );
        assert_eq!(
            value.pointer("/runtime/compute/backend"),
            Some(&json!("local-docker"))
        );
        assert_eq!(
            value.pointer("/runtime/storage"),
            None,
            "storage is runner-owned and should not be defaulted into packages"
        );
        assert_eq!(
            value.pointer("/runtime/traces"),
            None,
            "trace sinks are runner-owned and should not be defaulted into packages"
        );
        assert_eq!(
            value.pointer("/runtime/network/task_sandbox"),
            Some(&json!("none"))
        );
        assert_eq!(
            value.pointer("/runtime/network/agent"),
            Some(&json!("none"))
        );
        assert_eq!(value.pointer("/matrix/repeats"), Some(&json!(1)));
        assert_eq!(
            value.pointer("/scheduling/max_concurrency"),
            Some(&json!(1))
        );
        assert_eq!(value.pointer("/scheduling/random_seed"), Some(&json!(1)));
        assert!(value.pointer("/scheduling/comparison").is_none());
        assert_eq!(
            value.pointer("/policy/sanitization_profile"),
            Some(&json!("standard_runtime"))
        );
        assert_eq!(
            value.pointer("/evaluation/policy/task_model"),
            Some(&json!("independent"))
        );
        assert_eq!(
            value.pointer("/evaluation/policy/scoring_lifecycle"),
            Some(&json!("predict_then_score"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/integration_level"),
            Some(&json!("cli_basic"))
        );
        assert_eq!(
            value.pointer("/evaluation/policy/chain_failure_policy"),
            Some(&json!("continue_with_flag"))
        );
        assert_eq!(
            value.pointer("/evaluation/policy/required_evidence_classes"),
            Some(&json!([]))
        );
        assert_eq!(
            value.pointer("/policy/policies/scheduling"),
            Some(&json!("variant_sequential"))
        );
        assert_eq!(
            value.pointer("/policy/policies/state"),
            Some(&json!("isolate_per_trial"))
        );
        assert_eq!(
            value.pointer("/policy/policies/retry/max_attempts"),
            Some(&json!(1))
        );
        assert_eq!(
            value.pointer("/policy/policies/retry/retry_on"),
            Some(&json!([]))
        );
        assert_eq!(
            value.pointer("/policy/policies/pruning/max_consecutive_failures"),
            Some(&json!(0))
        );
        assert_eq!(
            value.pointer("/policy/policies/concurrency/require_chain_lease"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/policy/task_sandbox/hardening/no_new_privileges"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/policy/task_sandbox/hardening/drop_all_caps"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/trial_runtime/execution/agent_site"),
            Some(&json!("host"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/grader/strategy"),
            Some(&json!("none"))
        );
        assert_eq!(value.pointer("/policy/timeout_ms"), Some(&json!(600000)));
        assert_eq!(
            value.pointer("/policy/task_sandbox"),
            Some(&json!({
                "hardening": {
                    "no_new_privileges": true,
                    "drop_all_caps": true
                }
            }))
        );
    }

    #[test]
    fn normalizes_variant_baseline_flags_for_resolved_packages() {
        let mut single = json!({
            "matrix": {
                "variants": [{ "id": "control" }]
            }
        });
        normalize_authoring_vocabulary(&mut single).unwrap();
        assert_eq!(
            single.pointer("/matrix/variants/0/baseline"),
            Some(&json!(true))
        );
        assert_eq!(
            single.pointer("/matrix/variants/0/config"),
            Some(&json!({}))
        );

        let mut explicit_multi = json!({
            "matrix": {
                "variants": [
                    { "id": "baseline", "baseline": true },
                    { "id": "treatment" }
                ]
            }
        });
        normalize_authoring_vocabulary(&mut explicit_multi).unwrap();
        assert_eq!(
            explicit_multi.pointer("/matrix/variants/0/baseline"),
            Some(&json!(true))
        );
        assert_eq!(
            explicit_multi.pointer("/matrix/variants/1/baseline"),
            Some(&json!(false))
        );
        assert_eq!(
            explicit_multi.pointer("/matrix/variants/0/config"),
            Some(&json!({}))
        );
        assert_eq!(
            explicit_multi.pointer("/matrix/variants/1/config"),
            Some(&json!({}))
        );
    }

    #[test]
    fn rejects_ambiguous_variant_baseline_authoring() {
        for (mut value, expected) in [
            (
                json!({
                    "matrix": {
                        "variants": [
                            { "id": "control", "config": {} },
                            { "id": "treatment", "config": {} }
                        ]
                    }
                }),
                "multiple variants require exactly one explicit /matrix/variants[].baseline=true",
            ),
            (
                json!({
                    "matrix": {
                        "variants": [
                            { "id": "baseline", "config": {} },
                            { "id": "treatment", "config": {} }
                        ]
                    }
                }),
                "multiple variants require exactly one explicit /matrix/variants[].baseline=true",
            ),
            (
                json!({
                    "matrix": {
                        "variants": [
                            { "id": "control", "baseline": false }
                        ]
                    }
                }),
                "single-variant authoring cannot set /matrix/variants/0/baseline=false",
            ),
        ] {
            let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "expected {expected:?}, got: {err}"
            );
        }
    }

    #[test]
    fn lowers_paired_comparison_to_concrete_scheduling_policy() {
        let mut value = json!({
            "experiment": { "id": "paired_eval" },
            "matrix": {
                "cases": { "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true }, { "id": "treatment" }]
            },
            "scheduling": { "comparison": "paired" },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert!(value.pointer("/scheduling/comparison").is_none());
        assert_eq!(
            value.pointer("/policy/policies/scheduling"),
            Some(&json!("paired_interleaved"))
        );
    }

    #[test]
    fn rejects_paired_comparison_with_explicit_scheduling_policy() {
        let mut value = json!({
            "experiment": { "id": "paired_eval" },
            "matrix": {
                "cases": { "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true }, { "id": "treatment" }]
            },
            "scheduling": { "comparison": "paired" },
            "policy": {
                "policies": { "scheduling": "variant_sequential" }
            },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/scheduling/comparison is exclusive authoring shorthand")
                && msg.contains("/policy/policies/scheduling"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn rejects_noop_scheduling_comparison_values() {
        let mut value = json!({
            "experiment": { "id": "default_eval" },
            "matrix": {
                "cases": { "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true }]
            },
            "scheduling": { "comparison": "none" },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("unsupported scheduling.comparison 'none'")
                && msg.contains("remove scheduling.comparison")
                && msg.contains("policy.policies.scheduling")
                && msg.contains("variant_sequential")
                && msg.contains("paired_interleaved")
                && msg.contains("randomized"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn rejects_noop_runtime_backend_fields_before_schema_validation() {
        for (field, expected) in [
            (
                "storage",
                "storage is runner-owned today and local-fs is the default runtime behavior",
            ),
            (
                "traces",
                "trace sinks are runner-owned today, and command traces should be declared with /traces when needed",
            ),
        ] {
            let mut value = json!({
                "experiment": { "id": "default_eval" },
                "runtime": {
                    "compute": { "backend": "local-docker" }
                },
                "matrix": {
                    "cases": { "path": "cases.jsonl" },
                    "variants": [{ "id": "base", "baseline": true }]
                },
                "stages": {
                    "case": {},
                    "agent": { "command": ["agent"] }
                }
            });
            value["runtime"][field] = json!({ "backend": "local-fs" });

            let err = reject_legacy_authoring_surface(&value)
                .expect_err("retired runtime backend fields should fail pre-schema");
            let msg = err.to_string();
            assert!(msg.contains(&format!("/runtime/{field}")));
            assert!(msg.contains(expected), "unexpected error: {msg}");
            assert!(
                !msg.contains("Additional properties"),
                "pre-schema error should not leak JSON Schema wording: {msg}"
            );
        }
    }

    #[test]
    fn paired_comparison_requires_multiple_variants() {
        let mut value = json!({
            "experiment": { "id": "paired_eval" },
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "scheduling": { "comparison": "paired" },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();

        assert!(
            err.to_string()
                .contains("/scheduling/comparison=paired requires at least two matrix variants"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn normalizes_lowered_authoring_without_leaking_experiment_mode() {
        let mut value = json!({
            "experiment": { "id": "lowered_authoring", "mode": "patch" },
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "trial_runtime": {
                "task": {},
                "agent": { "command": ["agent"] }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert!(
            value.pointer("/experiment/mode").is_none(),
            "authoring-only mode should be dropped even when stages are already lowered"
        );
        assert_eq!(
            value.pointer("/trial_runtime/task/interface"),
            Some(&json!("input_only"))
        );
    }

    #[test]
    fn lowers_network_default_to_both_runtime_planes() {
        let mut value = json!({
            "experiment": { "id": "network_default" },
            "runtime": {
                "network": {
                    "default": "full"
                }
            },
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/runtime/network/task_sandbox"),
            Some(&json!("full"))
        );
        assert_eq!(
            value.pointer("/runtime/network/agent"),
            Some(&json!("full"))
        );
        assert!(value.pointer("/runtime/network/default").is_none());
    }

    #[test]
    fn rejects_network_default_mixed_with_explicit_planes() {
        let mut value = json!({
            "experiment": { "id": "network_default_mixed" },
            "runtime": {
                "network": {
                    "default": "full",
                    "agent": "none"
                }
            },
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/runtime/network/default is exclusive shorthand")
                && msg.contains("/runtime/network/agent"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn normalizes_public_variant_overrides_to_runtime_patch() {
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{
                    "id": "base",
                    "baseline": true,
                    "overrides": {
                        "case": {
                            "workspace": {
                                "image": { "from": "case_row" }
                            }
                        },
                        "agent": {
                            "ephemerals": ["mcp"],
                            "env": { "MODE": "variant" },
                            "events": [{
                                "id": "variant_events",
                                "format": "jsonl",
                                "mode": "jsonl",
                                "ingest": true,
                                "retain_raw": false
                            }]
                        },
                        "grader": {
                            "ephemerals": ["judge"]
                        }
                    }
                }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/task/workspace/image"),
            Some(&json!({ "from": "case_row" }))
        );
        assert!(value.pointer("/matrix/variants/0/overrides/case").is_none());
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/agent/sidecars"),
            Some(&json!(["mcp"]))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/agent/integration_level"),
            Some(&json!("cli_events"))
        );
        assert!(value
            .pointer("/matrix/variants/0/overrides/agent/ephemerals")
            .is_none());
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/grader/sidecars"),
            Some(&json!(["judge"]))
        );
    }

    #[test]
    fn variant_agent_env_override_does_not_inject_integration_level() {
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{
                    "id": "base",
                    "baseline": true,
                    "overrides": {
                        "agent": {
                            "env": { "MODE": "variant" }
                        }
                    }
                }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/agent/env/MODE"),
            Some(&json!("variant"))
        );
        assert!(
            value
                .pointer("/matrix/variants/0/overrides/agent/integration_level")
                .is_none(),
            "variant agent overrides are patches and must not receive full-object defaults"
        );
    }

    #[test]
    fn rejects_empty_grader_authoring() {
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base" }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "grader": {}
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/stages/grader must not be empty")
                && msg.contains("declare strategy: none"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn defaults_agent_site_to_task_runtime_for_container_workspace_agents() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }]
            },
            "stages": {
                "case": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": { "from": "case_row" },
                        "workdir": { "from": "case_row" }
                    }
                },
                "agent": { "command": ["agent"] },
                "grader": { "strategy": "none" }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/execution/agent_site"),
            Some(&json!("task_runtime"))
        );
    }

    #[test]
    fn rejects_uninferrable_agent_site_authoring() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }]
            },
            "stages": {
                "case": {
                    "files": {
                        "source": "files",
                        "path": "case-files",
                        "mount_path": "/case"
                    }
                },
                "agent": { "command": ["agent"] },
                "grader": { "strategy": "none" }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/stages.execution.agent_site is required")
                && msg.contains("agent runtime boundary cannot be inferred"),
            "unexpected error: {msg}"
        );
        assert!(
            !msg.contains("trial_runtime"),
            "authoring error should not leak resolved runtime paths: {msg}"
        );
    }

    #[test]
    fn explicit_agent_site_allows_readonly_file_cases() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }]
            },
            "stages": {
                "case": {
                    "files": {
                        "source": "files",
                        "path": "case-files",
                        "mount_path": "/case"
                    }
                },
                "agent": { "command": ["agent"] },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/execution/agent_site"),
            Some(&json!("host"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/task/interface"),
            Some(&json!("readonly_files"))
        );
    }

    #[test]
    fn rejects_removed_authoring_fallbacks_at_file_boundary() {
        for (value, expected) in [
            (
                json!({ "matrix": { "tasks": { "source": "file", "path": "cases.jsonl" } } }),
                "/matrix/tasks",
            ),
            (
                json!({ "cases": { "source": "file", "path": "cases.jsonl" } }),
                "/cases",
            ),
            (json!({ "variants": [{ "id": "base" }] }), "/variants"),
            (json!({ "task": {} }), "/task"),
            (json!({ "stages": { "task": {} } }), "/stages/task"),
            (json!({ "agent": {} }), "/agent"),
            (json!({ "trial_runtime": {} }), "/trial_runtime"),
            (json!({ "sidecars": {} }), "/sidecars"),
            (
                json!({ "stages": { "agent": { "sidecars": ["svc"] } } }),
                "/stages/agent/sidecars",
            ),
            (
                json!({ "stages": { "agent": { "command": ["agent"], "artifact_type": "structured_json" } } }),
                "/stages/agent/artifact_type",
            ),
            (
                json!({ "stages": { "agent": { "command": ["agent"], "integration_level": "cli_basic" } } }),
                "/stages/agent/integration_level",
            ),
            (
                json!({ "stages": { "agent": { "command": ["agent"], "telemetry": {} } } }),
                "/stages/agent/telemetry",
            ),
            (
                json!({ "stages": { "agent": { "command": ["agent"], "protocol": "command" } } }),
                "/stages/agent/protocol",
            ),
            (
                json!({ "matrix": { "variants": [{ "id": "base", "overrides": { "task": {} } }] } }),
                "/matrix/variants/0/overrides/task",
            ),
            (
                json!({ "matrix": { "variants": [{ "id": "base", "overrides": { "grader": { "sidecars": ["svc"] } } }] } }),
                "/matrix/variants/0/overrides/grader/sidecars",
            ),
            (
                json!({ "matrix": { "variants": [{ "id": "base", "overrides": { "agent": { "integration_level": "cli_events" } } }] } }),
                "/matrix/variants/0/overrides/agent/integration_level",
            ),
            (
                json!({ "runtime": { "externals": {} } }),
                "/runtime/externals",
            ),
            (
                json!({ "knobs": { "temperature": { "json_pointer": "/matrix/variants/0/config/temperature" } } }),
                "/knobs",
            ),
            (
                json!({ "metrics": [{ "id": "score", "source": { "type": "agent_response", "pointer": "/score" } }] }),
                "/metrics/0 uses internal metric extraction field 'source'",
            ),
        ] {
            let err = reject_legacy_authoring_surface(&value).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "expected {expected}, got {err}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_external_accounting_values_at_file_boundary() {
        for (value, expected) in [
            (
                json!({
                    "runtime": {
                        "network": { "egress": ["api.openai.com", "api.openai.com"] }
                    }
                }),
                "/runtime/network/egress/1 duplicates 'api.openai.com'",
            ),
            (
                json!({
                    "externals": {
                        "apis": ["api.openai.com", "api.openai.com"]
                    }
                }),
                "/externals/apis/1 duplicates 'api.openai.com'",
            ),
            (
                json!({
                    "externals": {
                        "credentials": ["OPENAI_API_KEY", "OPENAI_API_KEY"]
                    }
                }),
                "/externals/credentials/1 duplicates 'OPENAI_API_KEY'",
            ),
        ] {
            let err = reject_legacy_authoring_surface(&value).unwrap_err();
            let msg = err.to_string();

            assert!(
                msg.contains(expected) && msg.contains("must be unique"),
                "expected {expected:?}, got: {msg}"
            );
        }
    }

    #[test]
    fn authoring_lowering_rejects_public_and_resolved_field_collisions() {
        for (label, mut value, expected) in [
            (
                "services",
                json!({
                    "services": {
                        "svc": {
                            "image": "svc:latest",
                            "lifecycle": "trial"
                        }
                    },
                    "sidecars": {
                        "svc": {
                            "image": "svc:latest",
                            "lifecycle": "per-trial"
                        }
                    }
                }),
                "target 'sidecars' already exists",
            ),
            (
                "ephemerals",
                json!({
                    "ephemerals": {
                        "svc": {
                            "image": "svc:latest",
                            "lifecycle": "per-trial"
                        }
                    },
                    "sidecars": {
                        "svc": {
                            "image": "svc:latest",
                            "lifecycle": "per-trial"
                        }
                    }
                }),
                "target 'sidecars' already exists",
            ),
            (
                "externals",
                json!({
                    "externals": { "apis": ["api.openai.com"] },
                    "runtime": { "externals": { "apis": ["api.openai.com"] } }
                }),
                "target 'externals' already exists",
            ),
            (
                "matrix cases",
                json!({
                    "matrix": {
                        "cases": { "source": "file", "path": "cases.jsonl" },
                        "tasks": { "source": "file", "path": "cases.jsonl" }
                    }
                }),
                "target 'tasks' already exists",
            ),
            (
                "stage ephemerals",
                json!({
                    "stages": {
                        "agent": {
                            "command": ["agent"],
                            "ephemerals": ["svc"],
                            "sidecars": ["svc"]
                        }
                    }
                }),
                "target 'sidecars' already exists",
            ),
            (
                "stages",
                json!({
                    "stages": { "agent": { "command": ["agent"] } },
                    "trial_runtime": { "agent": { "command": ["agent"] } }
                }),
                "target 'trial_runtime' already exists",
            ),
        ] {
            let err = match normalize_authoring_vocabulary(&mut value) {
                Ok(()) => {
                    panic!("{label}: public and resolved fields should not be accepted together")
                }
                Err(err) => err,
            };
            assert!(
                err.to_string().contains(expected),
                "{label}: expected {expected}, got {err}"
            );
        }
    }

    #[test]
    fn trace_source_protocol_adds_default_command_event_sink() {
        let mut value = json!({
            "traces": { "source": "protocol", "retain": "always" },
            "matrix": {
                "variants": [{ "id": "base" }],
                "cases": { "source": "file", "path": "cases.jsonl" },
                "repeats": 1
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/agent/events/0/id"),
            Some(&json!("trajectory"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/events/0/retain_raw"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/integration_level"),
            Some(&json!("cli_events"))
        );
        assert!(value.pointer("/traces").is_none());
    }

    #[test]
    fn defaults_agent_observability_contract_for_resolved_packages() {
        let mut value = json!({
            "matrix": {
                "variants": [{
                    "id": "base",
                    "overrides": {
                        "agent": {
                            "events": [{
                                "id": "variant_events",
                                "ingest": false,
                                "retain_raw": true
                            }],
                            "output_mounts": [{
                                "id": "variant_context",
                                "path": "variant-context",
                                "persist": false
                            }]
                        }
                    }
                }],
                "cases": { "source": "file", "path": "cases.jsonl" },
                "repeats": 1
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "events": [{
                        "id": "agent_events"
                    }],
                    "output_mounts": [{
                        "id": "session_context",
                        "path": "session-context"
                    }]
                },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/agent/events/0/format"),
            Some(&json!("jsonl"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/events/0/mode"),
            Some(&json!("jsonl"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/events/0/ingest"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/events/0/retain_raw"),
            Some(&json!(false))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/output_mounts/0/persist"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/output_mounts/0/kind"),
            Some(&json!("directory"))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/agent/events/0/format"),
            Some(&json!("jsonl"))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/agent/events/0/mode"),
            Some(&json!("jsonl"))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/agent/events/0/ingest"),
            Some(&json!(false))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/agent/events/0/retain_raw"),
            Some(&json!(true))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/agent/output_mounts/0/persist"),
            Some(&json!(false))
        );
        assert_eq!(
            value.pointer("/matrix/variants/0/overrides/agent/output_mounts/0/kind"),
            Some(&json!("directory"))
        );
    }

    #[test]
    fn defaults_agent_output_mount_persist() {
        let mut value = json!({
            "matrix": {
                "variants": [{ "id": "base" }],
                "cases": { "source": "file", "path": "cases.jsonl" },
                "repeats": 1
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "output_mounts": [{
                        "id": "session_context",
                        "path": "session-context"
                    }]
                },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        normalize_authoring_vocabulary(&mut value).expect("normalize output mount");

        assert_eq!(
            value.pointer("/trial_runtime/agent/output_mounts/0/persist"),
            Some(&json!(true))
        );
    }

    #[test]
    fn omitted_trace_source_does_not_add_event_sink() {
        let mut value = json!({
            "matrix": {
                "variants": [{ "id": "base" }],
                "cases": { "source": "file", "path": "cases.jsonl" },
                "repeats": 1
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert!(value.pointer("/trial_runtime/agent/events").is_none());
        assert_eq!(
            value.pointer("/trial_runtime/agent/integration_level"),
            Some(&json!("cli_basic"))
        );
    }

    #[test]
    fn trace_source_none_is_rejected_as_noop_authoring() {
        let mut value = json!({
            "traces": { "source": "none" },
            "matrix": {
                "variants": [{ "id": "base" }],
                "cases": { "source": "file", "path": "cases.jsonl" },
                "repeats": 1
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/traces.source=none is not accepted") && msg.contains("omit /traces"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn in_task_runtime_grader_defaults_to_empty_config() {
        let mut value = json!({
            "stages": {
                "grader": {
                    "strategy": "in_task_runtime",
                    "command": ["pytest"]
                }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/grader/in_task_runtime"),
            Some(&json!({}))
        );
    }

    #[test]
    fn declared_traces_require_explicit_source() {
        fn assert_authoring_error(mut value: Value, expected: &str) {
            let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "unexpected error: {err}"
            );
        }
        assert_authoring_error(
            json!({"traces": {}, "stages": { "agent": { "command": ["agent"] } }}),
            "/traces.source is required when /traces is declared",
        );
    }

    #[test]
    fn trace_source_protocol_missing_agent_uses_public_stage_path() {
        let mut value = json!({
            "traces": { "source": "protocol" },
            "stages": {
                "case": { "interface": "input_only" },
                "grader": { "strategy": "none" }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("/stages.agent"),
            "trace error should use public stage path: {msg}"
        );
        assert!(
            !msg.contains("trial_runtime"),
            "trace error should not leak resolved runtime paths: {msg}"
        );
    }

    #[test]
    fn trace_source_protocol_uses_command_stage_without_protocol_field() {
        let mut value = json!({
            "traces": { "source": "protocol" },
            "stages": { "agent": { "command": ["agent"] } }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert!(value.pointer("/trial_runtime/agent/protocol").is_none());
        assert_eq!(
            value.pointer("/trial_runtime/agent/events/0/id"),
            Some(&json!("trajectory"))
        );
        assert!(value.pointer("/traces").is_none());
    }

    #[test]
    fn trace_source_protocol_requires_agent_launch_contract() {
        let mut value = json!({
            "traces": { "source": "protocol" },
            "matrix": {
                "variants": [{ "id": "base" }],
                "cases": { "source": "file", "path": "cases.jsonl" },
                "repeats": 1
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {},
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();

        assert!(
            err.to_string().contains(
                "/traces.source=protocol requires /stages.agent.command or /stages.agent.adapter"
            ),
            "unexpected error: {err}"
        );
    }
}
