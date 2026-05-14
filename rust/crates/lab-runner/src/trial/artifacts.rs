use anyhow::Result;
use lab_core::AGENTLAB_CONTRACT_OUT_DIR;
use serde_json::Value;
use std::fs;
use std::path::Path;

use crate::model::*;

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

pub(crate) fn agent_response_payload_view<'a>(agent_response: &'a Value) -> &'a Value {
    if agent_response.get("schema_version").and_then(Value::as_str) == Some("artifact_envelope_v1")
    {
        agent_response.get("artifact").unwrap_or(agent_response)
    } else {
        agent_response
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

pub(crate) fn artifact_type_from_trial_input(trial_input: &Value) -> ArtifactType {
    match trial_input
        .pointer("/artifact_type")
        .and_then(Value::as_str)
        .unwrap_or("structured_json")
    {
        "patch_submission" => ArtifactType::PatchSubmission,
        "text_response" => ArtifactType::TextResponse,
        "file_ref" => ArtifactType::FileRef,
        _ => ArtifactType::StructuredJson,
    }
}

pub(crate) fn artifact_type_from_trial_input_path(path: &Path) -> Result<ArtifactType> {
    let trial_input: Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(artifact_type_from_trial_input(&trial_input))
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
                            || path.starts_with(&format!("{}/", AGENTLAB_CONTRACT_OUT_DIR)) =>
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
