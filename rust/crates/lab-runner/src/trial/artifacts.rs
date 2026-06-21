use anyhow::{anyhow, Context, Result};
use lab_core::BUCEPHALUS_CONTRACT_OUT_DIR;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::Path;

use crate::model::{
    ArtifactEnvelopeV1, ArtifactType, CandidateArtifactRecord, CandidateArtifactSource,
    CandidateArtifactState, DEFAULT_CONTAINER_RESULT_PATH,
};

pub(crate) struct AgentResponseRead {
    pub(crate) response: Value,
    pub(crate) result_present: bool,
    pub(crate) parse_error: Option<String>,
}

pub(crate) fn load_agent_response_resilient(path: &Path) -> Result<AgentResponseRead> {
    if !path.exists() {
        return Ok(AgentResponseRead {
            response: Value::Null,
            result_present: false,
            parse_error: None,
        });
    }

    let bytes = fs::read(path)?;
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(AgentResponseRead {
            response: value,
            result_present: true,
            parse_error: None,
        }),
        Err(err) => {
            let detail = format!(
                "failed to parse agent response JSON at {}: {}",
                path.display(),
                err
            );
            Ok(AgentResponseRead {
                response: Value::Null,
                result_present: true,
                parse_error: Some(detail),
            })
        }
    }
}

pub(crate) fn normalize_agent_result_adapter(
    runtime_experiment: &Value,
    result_path: &Path,
) -> Result<()> {
    let Some(adapter) = runtime_experiment.pointer("/trial_runtime/agent/adapter") else {
        return Ok(());
    };
    let result_kind = adapter
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("structured_json");
    if result_kind != "structured_json" || !result_path.exists() {
        return Ok(());
    }
    let raw: Value = serde_json::from_slice(&fs::read(result_path)?).with_context(|| {
        format!(
            "failed to parse adapter result at {}",
            result_path.display()
        )
    })?;
    if raw.get("schema_version").and_then(Value::as_str) == Some("artifact_envelope_v1") {
        return Ok(());
    }
    let normalized = normalize_structured_json_result(&raw).with_context(|| {
        format!(
            "failed to normalize adapter result at {}",
            result_path.display()
        )
    })?;
    fs::write(result_path, serde_json::to_vec_pretty(&normalized)?)?;
    Ok(())
}

fn normalize_structured_json_result(raw: &Value) -> Result<Value> {
    let response = raw
        .get("response")
        .ok_or_else(|| anyhow!("adapter result missing /response"))?;
    let mut artifact = parse_response_artifact(response)?;
    let usage = normalized_usage(raw);
    artifact.insert("usage".to_string(), usage.clone());
    let mut metadata = Map::new();
    if let Some(raw_object) = raw.as_object() {
        for (key, value) in raw_object {
            if key != "response" {
                metadata.insert(key.clone(), value.clone());
            }
        }
    }
    metadata.insert("metrics".to_string(), normalized_metrics(raw, &usage));
    Ok(json!({
        "schema_version": "artifact_envelope_v1",
        "artifact_type": "structured_json",
        "artifact": Value::Object(artifact),
        "metadata": Value::Object(metadata),
    }))
}

fn parse_response_artifact(response: &Value) -> Result<Map<String, Value>> {
    if let Some(object) = response.as_object() {
        return Ok(object.clone());
    }
    let Some(text) = response.as_str() else {
        return Err(anyhow!(
            "adapter /response must be an object or JSON string"
        ));
    };
    let mut candidates = vec![text.trim().to_string(), strip_code_fence(text)];
    if let Some(embedded) = first_json_object(text) {
        candidates.push(embedded);
    }
    let mut errors = Vec::new();
    for candidate in candidates {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(candidate) {
            Ok(Value::Object(object)) => return Ok(object),
            Ok(other) => errors.push(format!(
                "parsed response was {}, not object",
                value_type_name(&other)
            )),
            Err(err) => errors.push(err.to_string()),
        }
    }
    let detail = errors
        .into_iter()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("; ");
    Err(anyhow!(
        "adapter response did not contain a JSON object{}",
        if detail.is_empty() {
            "".to_string()
        } else {
            format!(": {detail}")
        }
    ))
}

fn strip_code_fence(text: &str) -> String {
    let stripped = text.trim();
    if !stripped.starts_with("```") {
        return stripped.to_string();
    }
    let lines = stripped.lines().collect::<Vec<_>>();
    if lines.len() >= 2 && lines.last().is_some_and(|line| line.trim() == "```") {
        return lines[1..lines.len() - 1].join("\n").trim().to_string();
    }
    stripped.to_string()
}

fn first_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let mut depth = 0_i64;
    let mut in_string = false;
    let mut escape = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..start + offset + ch.len_utf8()].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn normalized_usage(raw: &Value) -> Value {
    let source = raw
        .get("usage")
        .and_then(Value::as_object)
        .or_else(|| raw.get("metrics").and_then(Value::as_object));
    json!({
        "latency_ms": numeric_metric(source, &["latency_ms"]),
        "model_calls": numeric_metric(source, &["model_calls", "model_call_count", "turn_count"]),
        "tokens_in": numeric_metric(source, &["tokens_in", "input_tokens", "prompt_tokens"]),
        "tokens_out": numeric_metric(source, &["tokens_out", "output_tokens", "completion_tokens"]),
        "tool_calls": numeric_metric(source, &["tool_calls", "tool_call_count"]),
    })
}

fn normalized_metrics(raw: &Value, usage: &Value) -> Value {
    let mut metrics = raw
        .get("metrics")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(usage) = usage.as_object() {
        metrics.insert(
            "latency_ms".to_string(),
            usage.get("latency_ms").cloned().unwrap_or_else(|| json!(0)),
        );
        metrics.insert(
            "turn_count".to_string(),
            usage
                .get("model_calls")
                .cloned()
                .unwrap_or_else(|| json!(0)),
        );
        metrics.insert(
            "tokens_in".to_string(),
            usage.get("tokens_in").cloned().unwrap_or_else(|| json!(0)),
        );
        metrics.insert(
            "tokens_out".to_string(),
            usage.get("tokens_out").cloned().unwrap_or_else(|| json!(0)),
        );
        metrics.insert(
            "tool_call_count".to_string(),
            usage.get("tool_calls").cloned().unwrap_or_else(|| json!(0)),
        );
    }
    Value::Object(metrics)
}

fn numeric_metric(source: Option<&Map<String, Value>>, keys: &[&str]) -> Value {
    let Some(source) = source else {
        return json!(0);
    };
    for key in keys {
        if let Some(value) = source.get(*key) {
            if value.is_number() {
                return value.clone();
            }
        }
    }
    json!(0)
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(crate) fn agent_response_payload_view(agent_response: &Value) -> Result<&Value> {
    if agent_response.get("schema_version").and_then(Value::as_str) == Some("artifact_envelope_v1")
    {
        agent_response
            .get("artifact")
            .ok_or_else(|| anyhow!("artifact_envelope_v1 missing /artifact"))
    } else {
        Ok(agent_response)
    }
}

fn result_file_ref_path(result_value: &Value) -> Option<&str> {
    result_value
        .get("artifact")
        .and_then(Value::as_str)
        .or_else(|| {
            result_value
                .pointer("/artifact/path")
                .and_then(Value::as_str)
        })
}

pub(crate) fn artifact_type_from_trial_input(trial_input: &Value) -> Result<ArtifactType> {
    let raw = trial_input
        .pointer("/artifact_type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("trial_input_v1 missing required string field artifact_type"))?;
    serde_json::from_value::<ArtifactType>(Value::String(raw.to_string()))
        .map_err(|_| anyhow!("trial_input_v1 artifact_type '{}' is not supported", raw))
}

pub(crate) fn artifact_type_from_trial_input_path(path: &Path) -> Result<ArtifactType> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read trial input {}", path.display()))?;
    let trial_input: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse trial input JSON {}", path.display()))?;
    artifact_type_from_trial_input(&trial_input)
        .with_context(|| format!("failed to resolve artifact_type from {}", path.display()))
}

pub(crate) fn extract_candidate_artifact_record(
    result_value: &Value,
    result_present: bool,
    expected_artifact_type: ArtifactType,
) -> CandidateArtifactRecord {
    if !result_present {
        return CandidateArtifactRecord {
            state: CandidateArtifactState::Missing,
            artifact_type: expected_artifact_type,
            source: CandidateArtifactSource::None,
            payload: None,
        };
    }

    match serde_json::from_value::<ArtifactEnvelopeV1>(result_value.clone()) {
        Ok(envelope) if envelope.artifact_type == expected_artifact_type => {
            let source = if matches!(envelope.artifact_type, ArtifactType::FileRef) {
                CandidateArtifactSource::ResultFileRef
            } else {
                CandidateArtifactSource::ResultInline
            };
            let state = if matches!(envelope.artifact_type, ArtifactType::FileRef) {
                match result_file_ref_path(result_value) {
                    Some(path)
                        if path == DEFAULT_CONTAINER_RESULT_PATH
                            || path.starts_with(&format!("{}/", BUCEPHALUS_CONTRACT_OUT_DIR)) =>
                    {
                        CandidateArtifactState::Valid
                    }
                    _ => CandidateArtifactState::Invalid,
                }
            } else {
                CandidateArtifactState::Valid
            };
            CandidateArtifactRecord {
                state,
                artifact_type: envelope.artifact_type,
                source,
                payload: Some(envelope.artifact),
            }
        }
        Ok(envelope) => CandidateArtifactRecord {
            state: CandidateArtifactState::Invalid,
            artifact_type: envelope.artifact_type,
            source: CandidateArtifactSource::None,
            payload: Some(envelope.artifact),
        },
        Err(_) => CandidateArtifactRecord {
            state: CandidateArtifactState::Invalid,
            artifact_type: expected_artifact_type,
            source: CandidateArtifactSource::None,
            payload: Some(result_value.clone()),
        },
    }
}
