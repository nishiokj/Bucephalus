use anyhow::{anyhow, Context, Result};
use lab_core::{
    sha256_bytes, sha256_file, BUCEPHALUS_RESULT_PATH, BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER,
};
use serde_json::{json, Map, Value};
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
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&raw_yaml)?;
    let json_value: Value = serde_json::to_value(yaml_value)?;
    let mut json_value = if let Some(overrides_path) = overrides_path {
        apply_experiment_overrides(json_value, overrides_path, &project_root)?
    } else {
        json_value
    };
    reject_legacy_authoring_surface(&json_value)?;
    crate::package::validate::validate_required_fields(&json_value)?;
    validate_authoring_schema(&json_value)?;
    normalize_authoring_vocabulary(&mut json_value)
        .map_err(|err| crate::package::validate::public_authoring_error(err, true))?;
    Ok(LoadedExperimentInput {
        json_value,
        exp_dir,
        project_root,
    })
}

fn validate_authoring_schema(json_value: &Value) -> Result<()> {
    let schema = lab_schemas::compile_schema("experiment_authoring_v1.jsonschema")?;
    let Some(errors) = schema.validate(json_value).err() else {
        return Ok(());
    };
    let messages = errors
        .map(|err| {
            let path = err.instance_path.to_string();
            if path.is_empty() {
                err.to_string()
            } else {
                format!("{}: {}", path, err)
            }
        })
        .collect::<Vec<_>>();
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
        ("/sidecars", "/ephemerals"),
        ("/matrix/tasks", "/matrix/cases"),
        ("/runtime/externals", "/externals"),
        ("/variant_plan", "/matrix/variants"),
        (
            "/overrides",
            "first-class v1 fields under /runtime, /policy, or /matrix",
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
    Ok(())
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

pub(crate) fn normalize_authoring_vocabulary(json_value: &mut Value) -> Result<()> {
    alias_top_level_value(json_value, "ephemerals", &["sidecars"])?;
    alias_top_level_value(json_value, "externals", &["runtime", "externals"])?;
    if let Some(matrix) = json_value.get_mut("matrix") {
        alias_child_value(matrix, "cases", "tasks")?;
    }
    normalize_authoring_defaults(json_value)?;

    let Some(stages) = json_value.get("stages").cloned() else {
        normalize_stage_ephemerals(json_value.pointer_mut("/trial_runtime"))?;
        normalize_agent_site_default(json_value)?;
        normalize_agent_protocol_default(json_value)?;
        normalize_grader_defaults(json_value)?;
        normalize_agent_result_output(json_value)?;
        normalize_metric_authoring(json_value)?;
        normalize_trace_policy(json_value)?;
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
        normalize_stage_ephemerals(Some(&mut normalized_stage))?;
        insert_alias_value(&mut trial_runtime, target, normalized_stage, "/stages")?;
    }
    alias_object_value(json_value, &["trial_runtime"], Value::Object(trial_runtime))?;
    if let Some(object) = json_value.as_object_mut() {
        object.remove("stages");
    }
    normalize_stage_ephemerals(json_value.pointer_mut("/trial_runtime"))?;
    normalize_agent_site_default(json_value)?;
    normalize_agent_protocol_default(json_value)?;
    normalize_grader_defaults(json_value)?;
    normalize_agent_result_output(json_value)?;
    normalize_metric_authoring(json_value)?;
    normalize_trace_policy(json_value)?;
    Ok(())
}

fn normalize_authoring_defaults(json_value: &mut Value) -> Result<()> {
    default_object_path(json_value, &["runtime"])?;
    default_object_path(json_value, &["runtime", "compute"])?;
    default_object_path(json_value, &["runtime", "storage"])?;
    default_object_path(json_value, &["runtime", "traces"])?;
    default_object_path(json_value, &["runtime", "network"])?;
    default_object_path(json_value, &["policy"])?;
    default_object_path(json_value, &["scheduling"])?;

    insert_default_value(
        json_value,
        &["runtime", "compute", "backend"],
        json!("local-docker"),
    )?;
    insert_default_value(
        json_value,
        &["runtime", "storage", "backend"],
        json!("local-fs"),
    )?;
    insert_default_value(
        json_value,
        &["runtime", "traces", "backend"],
        json!("local-stdout"),
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
    insert_default_value(json_value, &["scheduling", "comparison"], json!("none"))?;
    insert_default_value(json_value, &["policy", "timeout_ms"], json!(600000))?;
    insert_default_value(json_value, &["policy", "task_sandbox"], json!({}))?;
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

fn normalize_agent_protocol_default(json_value: &mut Value) -> Result<()> {
    let Some(agent) = json_value.pointer_mut("/trial_runtime/agent") else {
        return Ok(());
    };
    let agent = agent
        .as_object_mut()
        .ok_or_else(|| anyhow!("/agent must be an object"))?;
    if agent.get("protocol").is_none() && agent.get("command").is_some() {
        agent.insert("protocol".to_string(), json!("command"));
    }
    Ok(())
}

fn normalize_grader_defaults(json_value: &mut Value) -> Result<()> {
    let Some(grader) = json_value.pointer_mut("/trial_runtime/grader") else {
        return Ok(());
    };
    let grader = grader
        .as_object_mut()
        .ok_or_else(|| anyhow!("/grader must be an object"))?;
    if grader.get("strategy").and_then(Value::as_str) == Some("in_task_runtime")
        && grader.get("in_task_runtime").is_none()
    {
        grader.insert("in_task_runtime".to_string(), json!({}));
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
    let result = agent.remove("result");
    let Some(result) = result else {
        ensure_default_agent_result_output(agent)?;
        return Ok(());
    };
    if agent.get("outputs").is_some() {
        return Err(anyhow!(
            "/agent declares both 'result' and 'outputs'; use the high-level 'result' field for the canonical agent result"
        ));
    }
    let result = match result {
        Value::String(kind) if kind.trim() == "structured_json" => default_agent_result_outputs(),
        Value::Object(mut obj) => {
            let kind = obj
                .remove("type")
                .or_else(|| obj.remove("format"))
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "structured_json".to_string());
            if kind.trim() != "structured_json" {
                return Err(anyhow!(
                    "/agent.result currently supports 'structured_json' only (got '{}')",
                    kind
                ));
            }
            default_agent_result_outputs()
        }
        other => {
            return Err(anyhow!(
                "/agent.result must be 'structured_json' or an object, got {}",
                value_kind(&other)
            ));
        }
    };
    agent.insert("outputs".to_string(), result);
    Ok(())
}

fn ensure_default_agent_result_output(agent: &mut Map<String, Value>) -> Result<()> {
    let Some(outputs) = agent.get_mut("outputs") else {
        agent.insert("outputs".to_string(), default_agent_result_outputs());
        return Ok(());
    };
    let outputs = outputs
        .as_object_mut()
        .ok_or_else(|| anyhow!("/agent.outputs must be an object"))?;
    if !outputs.contains_key("result") {
        let mut defaults = default_agent_result_outputs();
        let Some(default_result) = defaults
            .as_object_mut()
            .and_then(|defaults| defaults.remove("result"))
        else {
            return Err(anyhow!("internal default agent result output is malformed"));
        };
        outputs.insert("result".to_string(), default_result);
    }
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

fn normalize_metric_authoring(json_value: &mut Value) -> Result<()> {
    let Some(metrics) = json_value.get_mut("metrics") else {
        return Ok(());
    };
    let metrics = metrics
        .as_array_mut()
        .ok_or_else(|| anyhow!("/metrics must be an array"))?;
    for (idx, metric) in metrics.iter_mut().enumerate() {
        let context = format!("/metrics/{}", idx);
        let metric = metric
            .as_object_mut()
            .ok_or_else(|| anyhow!("{} must be an object", context))?;
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
    if let Some(rest) = raw
        .strip_prefix("result.")
        .or_else(|| raw.strip_prefix("agent.result."))
    {
        return Ok(json!({
            "type": "agent_response",
            "pointer": public_path_to_json_pointer(rest, context)?
        }));
    }
    if raw == "result" || raw == "agent.result" {
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

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn normalize_trace_policy(json_value: &mut Value) -> Result<()> {
    let Some(traces) = json_value.get("traces") else {
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
    match source {
        "none" => Ok(()),
        "protocol" => normalize_protocol_trace_source(json_value, retain_raw),
        other => Err(anyhow!(
            "/traces.source must be one of: none, protocol (got '{}')",
            other
        )),
    }
}

fn normalize_protocol_trace_source(json_value: &mut Value, retain_raw: bool) -> Result<()> {
    let agent = json_value
        .pointer_mut("/trial_runtime/agent")
        .ok_or_else(|| {
            anyhow!("/traces.source=protocol requires /stages.agent or /trial_runtime.agent")
        })?;
    let agent = agent
        .as_object_mut()
        .ok_or_else(|| anyhow!("/trial_runtime/agent must be an object"))?;
    let protocol = agent
        .get("protocol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("/traces.source=protocol requires agent.protocol"))?;
    if protocol != "command" {
        return Err(anyhow!(
            "/traces.source=protocol is only supported for agent protocol 'command' today (got '{}')",
            protocol
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

fn normalize_stage_ephemerals(value: Option<&mut Value>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    for stage in ["agent", "grader"] {
        let Some(stage_value) = object.get_mut(stage) else {
            continue;
        };
        if stage_value.is_object() {
            alias_child_value(stage_value, "ephemerals", "sidecars")?;
        }
    }
    Ok(())
}

fn alias_top_level_value(json_value: &mut Value, source: &str, target: &[&str]) -> Result<()> {
    let Some(value) = json_value.get(source).cloned() else {
        return Ok(());
    };
    alias_object_value(json_value, target, value)?;
    if let Some(object) = json_value.as_object_mut() {
        object.remove(source);
    }
    Ok(())
}

fn alias_object_value(root: &mut Value, target: &[&str], value: Value) -> Result<()> {
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
    insert_alias_value(
        current,
        target[target.len() - 1],
        value,
        "authoring vocabulary",
    )
}

fn alias_child_value(root: &mut Value, source: &str, target: &str) -> Result<()> {
    let Some(value) = root.get(source).cloned() else {
        return Ok(());
    };
    let object = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("stage authoring input must be an object"))?;
    insert_alias_value(object, target, value, "stage authoring vocabulary")?;
    object.remove(source);
    Ok(())
}

fn insert_alias_value(
    object: &mut Map<String, Value>,
    key: &str,
    value: Value,
    context: &str,
) -> Result<()> {
    if let Some(existing) = object.get(key) {
        if existing != &value {
            return Err(anyhow!(
                "{} declares both '{}' and its alias with different values",
                context,
                key
            ));
        }
        return Ok(());
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
                "variants": [{ "id": "base" }],
                "cases": { "source": "file", "path": "cases.jsonl" },
                "repeats": 1
            },
            "stages": {
                "case": { "interface": "input_only" },
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
            },
            "externals": { "apis": ["api.openai.com"] }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/matrix/tasks/path"),
            Some(&json!("cases.jsonl"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/task/interface"),
            Some(&json!("input_only"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/sidecars"),
            Some(&json!(["mcp"]))
        );
        assert!(value.pointer("/sidecars/mcp").is_some());
        assert_eq!(
            value.pointer("/runtime/externals/apis"),
            Some(&json!(["api.openai.com"]))
        );
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
                    "command": ["agent"],
                    "result": "structured_json"
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
        assert_eq!(
            value.pointer("/trial_runtime/agent/protocol"),
            Some(&json!("command"))
        );
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
    fn normalizes_default_result_output_with_extra_agent_outputs() {
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
        assert_eq!(
            value.pointer("/trial_runtime/execution/agent_site"),
            Some(&json!("host"))
        );
    }

    #[test]
    fn normalizes_safe_authoring_defaults() {
        let mut value = json!({
            "matrix": {
                "cases": { "source": "file", "path": "cases.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }]
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "grader": { "strategy": "none" }
            }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/runtime/compute/backend"),
            Some(&json!("local-docker"))
        );
        assert_eq!(
            value.pointer("/runtime/storage/backend"),
            Some(&json!("local-fs"))
        );
        assert_eq!(
            value.pointer("/runtime/traces/backend"),
            Some(&json!("local-stdout"))
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
        assert_eq!(
            value.pointer("/scheduling/comparison"),
            Some(&json!("none"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/execution/agent_site"),
            Some(&json!("host"))
        );
        assert_eq!(value.pointer("/policy/timeout_ms"), Some(&json!(600000)));
        assert_eq!(value.pointer("/policy/task_sandbox"), Some(&json!({})));
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
            (json!({ "agent": {} }), "/agent"),
            (json!({ "trial_runtime": {} }), "/trial_runtime"),
            (json!({ "sidecars": {} }), "/sidecars"),
            (
                json!({ "runtime": { "externals": {} } }),
                "/runtime/externals",
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
                "agent": { "protocol": "command", "command": ["agent"] },
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
    fn trace_source_protocol_uses_default_command_protocol() {
        let mut value = json!({
            "traces": { "source": "protocol" },
            "stages": { "agent": { "command": ["agent"] } }
        });

        normalize_authoring_vocabulary(&mut value).unwrap();

        assert_eq!(
            value.pointer("/trial_runtime/agent/protocol"),
            Some(&json!("command"))
        );
        assert_eq!(
            value.pointer("/trial_runtime/agent/events/0/id"),
            Some(&json!("trajectory"))
        );
    }

    #[test]
    fn trace_source_protocol_rejects_non_command_protocol_until_supported() {
        let mut value = json!({
            "traces": { "source": "protocol" },
            "matrix": {
                "variants": [{ "id": "base" }],
                "cases": { "source": "file", "path": "cases.jsonl" },
                "repeats": 1
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "protocol": "acp", "command": ["agent"] },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            }
        });

        let err = normalize_authoring_vocabulary(&mut value).unwrap_err();

        assert!(
            err.to_string().contains(
                "/traces.source=protocol is only supported for agent protocol 'command' today"
            ),
            "unexpected error: {err}"
        );
    }
}
