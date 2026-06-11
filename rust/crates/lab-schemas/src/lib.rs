use anyhow::{anyhow, Result};
use include_dir::{include_dir, Dir};
use jsonschema::{Draft, JSONSchema};
use serde_json::Value;

static SCHEMAS_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../../schemas");

pub fn schema_names() -> Vec<String> {
    SCHEMAS_DIR
        .files()
        .filter_map(|f| {
            f.path()
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .collect()
}

pub fn load_schema(name: &str) -> Result<Value> {
    if let Some(file) = SCHEMAS_DIR.get_file(name) {
        let data = std::str::from_utf8(file.contents())?;
        return Ok(serde_json::from_str(data)?);
    }

    Err(anyhow!("schema not found: {}", name))
}

pub fn compile_schema(name: &str) -> Result<JSONSchema> {
    let schema = load_schema(name)?;
    let schema = Box::leak(Box::new(schema));
    let compiled = JSONSchema::options()
        .with_draft(Draft::Draft7)
        .compile(schema)?;
    Ok(compiled)
}

#[cfg(test)]
mod tests {
    use super::compile_schema;
    use serde_json::json;

    #[test]
    fn compile_hard_cutover_schemas() {
        compile_schema("experiment_authoring_v1.jsonschema").expect("experiment authoring schema");
        compile_schema("task_row_v2.jsonschema").expect("task row schema");
        compile_schema("trial_input_v1.jsonschema").expect("trial input schema");
        compile_schema("artifact_envelope_v1.jsonschema").expect("artifact envelope schema");
        compile_schema("grader_input_v1.jsonschema").expect("grader input schema");
        compile_schema("trial_conclusion_v1.jsonschema").expect("trial conclusion schema");
        compile_schema("trial_claim_intent_v1.jsonschema").expect("trial claim intent schema");
        compile_schema("sealed_package_lock_v1.jsonschema").expect("sealed package lock schema");
        compile_schema("package_checks_v1.jsonschema").expect("package checks schema");
        compile_schema("resolved_experiment.jsonschema").expect("resolved experiment schema");
        compile_schema("state_inventory_v1.jsonschema").expect("state inventory schema");
        compile_schema("prepared_task_environment_v1.jsonschema")
            .expect("prepared task environment schema");
    }

    #[test]
    fn experiment_authoring_schema_accepts_public_yaml_shape() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": {
                "id": "smoke_eval",
                "name": "Smoke eval",
                "mode": "answer"
            },
            "matrix": {
                "variants": [
                    { "id": "baseline", "baseline": true, "config": {} }
                ],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "grader": { "strategy": "none" }
            },
            "metrics": [
                {
                    "id": "resolved",
                    "from": "result.metrics.resolved",
                    "primary": true
                }
            ]
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();

        assert!(errors.is_empty(), "unexpected schema errors: {errors:?}");
    }

    #[test]
    fn experiment_authoring_schema_rejects_resolved_package_nouns() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline" }],
                "tasks": { "source": "file", "path": "cases.jsonl" }
            },
            "trial_runtime": {
                "agent": { "command": ["agent"] }
            },
            "metrics": [
                {
                    "id": "resolved",
                    "source": { "type": "agent_response", "pointer": "/metrics/resolved" }
                }
            ]
        });

        assert!(
            schema.validate(&value).is_err(),
            "authoring schema should reject resolved-package internals"
        );
    }
}
