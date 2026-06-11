use anyhow::{anyhow, Result};
use include_dir::{include_dir, Dir};
use jsonschema::error::{ValidationError, ValidationErrorKind};
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

pub fn format_validation_error(err: &ValidationError<'_>) -> String {
    if let ValidationErrorKind::Required { property } = &err.kind {
        if let Some(property) = property.as_str() {
            let mut path = err.instance_path.to_string();
            path.push('/');
            path.push_str(&escape_json_pointer_segment(property));
            return format!("{} is required", path);
        }
    }
    let path = err.instance_path.to_string();
    if path.is_empty() {
        err.to_string()
    } else {
        format!("{}: {}", path, err)
    }
}

fn escape_json_pointer_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::{compile_schema, format_validation_error};
    use serde_json::json;
    use serde_json::Value;

    fn minimal_resolved_trial_runtime() -> Value {
        json!({
            "task": { "interface": "input_only" },
            "agent": {
                "command": ["agent"],
                "artifact_type": "structured_json",
                "integration_level": "cli_basic",
                "outputs": {
                    "result": {
                        "capture": {
                            "type": "file",
                            "path": "/bucephalus/out/result.json",
                            "format": "json",
                            "required": true
                        }
                    }
                }
            },
            "execution": { "agent_site": "host" },
            "grader": { "strategy": "none" }
        })
    }

    fn minimal_resolved_scheduling() -> Value {
        json!({
            "max_concurrency": 1,
            "random_seed": 1
        })
    }

    fn minimal_resolved_runtime() -> Value {
        json!({
            "compute": { "backend": "local-docker" },
            "network": { "task_sandbox": "none", "agent": "none" }
        })
    }

    fn minimal_resolved_evaluation() -> Value {
        json!({
            "policy": {
                "task_model": "independent",
                "scoring_lifecycle": "predict_then_score",
                "chain_failure_policy": "continue_with_flag",
                "required_evidence_classes": []
            }
        })
    }

    fn minimal_resolved_policy() -> Value {
        json!({
            "timeout_ms": 1,
            "sanitization_profile": "standard_runtime",
            "task_sandbox": {
                "hardening": {
                    "no_new_privileges": true,
                    "drop_all_caps": true
                }
            },
            "policies": {
                "scheduling": "variant_sequential",
                "state": "isolate_per_trial",
                "retry": {
                    "max_attempts": 1,
                    "retry_on": []
                },
                "pruning": {
                    "max_consecutive_failures": 0
                },
                "concurrency": {
                    "require_chain_lease": true
                }
            }
        })
    }

    #[test]
    fn compile_hard_cutover_schemas() {
        compile_schema("experiment_authoring_v1.jsonschema").expect("experiment authoring schema");
        compile_schema("case_v2.jsonschema").expect("case v2 schema");
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
    fn case_v2_schema_rejects_resources_environment_alias() {
        let schema = compile_schema("case_v2.jsonschema").expect("case v2 schema");
        let mut value = json!({
            "schema_version": "case_v2",
            "id": "case_env_alias",
            "inputs": {},
            "resources": {
                "workspace": {
                    "source": "container_image",
                    "image": "python:3.11-slim",
                    "workdir": "/workspace/task"
                }
            },
            "materialization": [],
            "metadata": {},
            "limits": {}
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "canonical case_v2 should validate: {errors:?}"
        );

        value["resources"]["environment"] = json!({
            "image": "python:3.11-slim",
            "workdir": "/workspace/task"
        });
        assert!(
            schema.validate(&value).is_err(),
            "case_v2 resources.environment should not alias resources.workspace"
        );

        let mut materialization_resource = json!({
            "schema_version": "case_v2",
            "id": "case_materialization_resource",
            "inputs": {},
            "resources": {
                "workspace": {
                    "source": "container_image",
                    "image": "python:3.11-slim",
                    "workdir": "/workspace/task"
                }
            },
            "materialization": [
                {
                    "id": "copy_input",
                    "stage": "case",
                    "operation": "copy",
                    "resource": "fixtures/input.txt",
                    "source": {},
                    "hidden": false,
                    "mount": {
                        "path": "/workspace/task/input.txt",
                        "read_only": false
                    }
                }
            ],
            "metadata": {},
            "limits": {}
        });
        assert!(
            schema.validate(&materialization_resource).is_err(),
            "case_v2 materialization.resource should not alias source.path"
        );

        materialization_resource["materialization"][0]
            .as_object_mut()
            .expect("materialization object")
            .remove("resource");
        materialization_resource["materialization"][0]["source"] =
            json!({ "path": "fixtures/input.txt" });
        let errors = schema
            .validate(&materialization_resource)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "canonical case_v2 materialization source.path should validate: {errors:?}"
        );

        materialization_resource["materialization"][0]["source"] = json!({
            "path": "fixtures/input.txt",
            "target": "/workspace/task/input.txt"
        });
        assert!(
            schema.validate(&materialization_resource).is_err(),
            "case_v2 materialization.source.target should not alias mount.path"
        );
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
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] },
                "grader": { "strategy": "none" }
            },
            "metrics": [
                {
                    "id": "resolved",
                    "label": "Resolved",
                    "semantic_key": "quality.resolved",
                    "value_type": "number",
                    "unit": "count",
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
    fn experiment_authoring_schema_validates_variant_baselines() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "cases": { "path": "cases.jsonl" },
                "variants": [{ "id": "control" }]
            },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        assert!(
            schema.validate(&value).is_ok(),
            "single variant may omit baseline and default to true"
        );

        value["matrix"]["variants"] = json!([
            { "id": "baseline", "baseline": true },
            { "id": "treatment" }
        ]);
        assert!(
            schema.validate(&value).is_ok(),
            "multiple variants should validate with one explicit baseline"
        );

        value["matrix"]["variants"] = json!([
            { "id": "baseline" },
            { "id": "treatment" }
        ]);
        assert!(
            schema.validate(&value).is_err(),
            "naming a variant baseline should not imply baseline: true"
        );

        value["matrix"]["variants"] = json!([{ "id": "control", "baseline": false }]);
        assert!(
            schema.validate(&value).is_err(),
            "single variant should not explicitly opt out of the required baseline"
        );
    }

    #[test]
    fn experiment_authoring_schema_requires_primary_for_multiple_metrics() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] },
                "grader": { "strategy": "none" }
            },
            "metrics": [
                { "id": "resolved", "from": "result.metrics.resolved" },
                { "id": "latency_ms", "from": "result.metrics.latency_ms" }
            ]
        });

        assert!(
            schema.validate(&value).is_err(),
            "multiple authored metrics should require exactly one primary marker"
        );

        value["metrics"][0]["primary"] = json!(true);
        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "one primary marker should validate for multiple authored metrics: {errors:?}"
        );

        value["metrics"] = json!([{ "id": "resolved", "from": "result.metrics.resolved" }]);
        assert!(
            schema.validate(&value).is_ok(),
            "a single authored metric may omit primary because authoring defaults it to true"
        );
    }

    #[test]
    fn experiment_authoring_schema_constrains_experiment_mode() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "mode": "patch" },
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(errors.is_empty(), "patch mode should validate: {errors:?}");

        value["experiment"]["mode"] = json!("future_magic");
        assert!(
            schema.validate(&value).is_err(),
            "experiment.mode should be a known authoring intent"
        );
    }

    #[test]
    fn experiment_authoring_schema_requires_meaningful_metadata() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": {
                "id": "smoke_eval",
                "description": "Smoke metadata contract",
                "owner": "eval-team",
                "tags": ["smoke", "local"]
            },
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        assert!(
            schema.validate(&value).is_ok(),
            "non-empty metadata should validate"
        );

        value["experiment"]["description"] = json!("");
        assert!(
            schema.validate(&value).is_err(),
            "empty experiment descriptions should be rejected"
        );

        value["experiment"]["description"] = json!("Smoke metadata contract");
        value["experiment"]["owner"] = json!("");
        assert!(
            schema.validate(&value).is_err(),
            "empty experiment owners should be rejected"
        );

        value["experiment"]["owner"] = json!("eval-team");
        value["experiment"]["tags"] = json!(["smoke", "smoke"]);
        assert!(
            schema.validate(&value).is_err(),
            "duplicate experiment tags should be rejected"
        );

        value["experiment"]["tags"] = json!(["smoke", ""]);
        assert!(
            schema.validate(&value).is_err(),
            "empty experiment tags should be rejected"
        );

        value["experiment"]["tags"] = json!(["smoke", "local"]);
        value["experiment"]["id"] = json!("   ");
        assert!(
            schema.validate(&value).is_err(),
            "whitespace-only experiment ids should be rejected"
        );

        value["experiment"]["id"] = json!("smoke_eval");
        value["stages"]["agent"]["command"] = json!(["agent", "   "]);
        assert!(
            schema.validate(&value).is_err(),
            "argv parts should reject whitespace-only values"
        );
    }

    #[test]
    fn experiment_authoring_schema_rejects_top_level_version() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "version": "1.0",
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "cases": { "path": "cases.jsonl" }
            },
            "stages": {
                "case": {},
                "agent": { "command": ["agent"] }
            }
        });

        assert!(
            schema.validate(&value).is_err(),
            "experiment files should not carry stale top-level version metadata"
        );
    }

    #[test]
    fn experiment_authoring_schema_closes_metric_transform_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "grader": {
                    "strategy": "separate",
                    "command": ["grade"],
                    "separate": {
                        "image": "ghcr.io/acme/grader:latest",
                        "workdir": "/grader"
                    }
                }
            },
            "metrics": [{
                "id": "pass_rate",
                "from": "grader.pytest_report",
                "transform": {
                    "type": "pytest_json_report_pass_rate",
                    "test_ids": {
                        "source": { "task": "commit0.test_ids" }
                    }
                }
            }]
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "implemented metric transform should validate: {errors:?}"
        );

        let mut no_grader_metric = value.clone();
        no_grader_metric["stages"]["grader"] = json!({ "strategy": "none" });
        assert!(
            schema.validate(&no_grader_metric).is_err(),
            "grader-backed metrics should require an active grader"
        );

        value["metrics"][0]["transform"]["test_ids"]["source"]["case"] = json!("commit0.test_ids");
        assert!(
            schema.validate(&value).is_err(),
            "metric transform should reject stale source.case alias"
        );

        value["metrics"][0]["transform"]["test_ids"]["source"]
            .as_object_mut()
            .expect("source object")
            .remove("case");
        value["metrics"][0]["transform"]["type"] = json!("custom_transform");
        assert!(
            schema.validate(&value).is_err(),
            "metric transform should reject unsupported transform types"
        );
    }

    #[test]
    fn experiment_authoring_schema_accepts_defaultable_name_and_grader() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();

        assert!(errors.is_empty(), "unexpected schema errors: {errors:?}");
    }

    #[test]
    fn experiment_authoring_schema_validates_case_stage_interfaces() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": {
                    "workspace": {
                        "source": "container_image",
                        "image": { "from": "case_row" },
                        "workdir": { "from": "case_row" }
                    }
                },
                "agent": { "command": ["agent"] }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "workspace should validate without an explicit interface: {errors:?}"
        );

        value["stages"]["execution"] = json!({ "agent_site": "task_runtime" });
        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "task_runtime should validate with a container-image case workspace: {errors:?}"
        );

        value["stages"]["case"]["workspace"]["source"] = json!("git");
        assert!(
            schema.validate(&value).is_err(),
            "task_runtime should require a container-image case workspace"
        );

        value["stages"]
            .as_object_mut()
            .expect("stages object")
            .remove("execution");
        value["stages"]["case"]["workspace"]["source"] = json!("container_image");

        value["stages"]["case"] = json!({
            "files": {
                "source": "files",
                "path": "case-files",
                "mount_path": "/case"
            }
        });
        value["stages"]["execution"] = json!({ "agent_site": "host" });
        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "files should validate without an explicit interface when agent_site is declared: {errors:?}"
        );

        value["stages"]["case"]["workspace"] = json!({ "source": "empty" });
        assert!(
            schema.validate(&value).is_err(),
            "readonly_files should not also declare workspace"
        );

        value["stages"]["case"] = json!({
            "interface": "writable_workspace",
            "workspace": {
                "source": "container_image",
                "image": { "from": "case_row" },
                "workdir": { "from": "case_row" },
                "imagee": { "from": "case_row" }
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "workspace should reject unknown keys"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_policy_policies_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            },
            "policy": {
                "policies": {
                    "scheduling": "randomized",
                    "state": "isolate_per_trial",
                    "retry": {
                        "max_attempts": 3,
                        "retry_on": ["error", "timeout"]
                    },
                    "pruning": { "max_consecutive_failures": 5 },
                    "concurrency": {
                        "max_in_flight_per_variant": 2,
                        "require_chain_lease": false
                    }
                }
            },
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "current policy.policies shape should validate: {errors:?}"
        );

        value["policy"]["policies"]["retry"]["backoff"] = json!("exponential");
        assert!(
            schema.validate(&value).is_err(),
            "retry policy should reject inert fallback keys"
        );

        value["policy"]["policies"]["retry"]
            .as_object_mut()
            .expect("retry object")
            .remove("backoff");
        value["policy"]["policies"]["scheduler"] = json!("paired_interleaved");
        assert!(
            schema.validate(&value).is_err(),
            "policy.policies should reject misspelled policy keys"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_runtime_registry_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "runtime": {
                "registry": {
                    "image_rewrites": [{
                        "match_prefix": "registry.example.invalid/project.",
                        "replace_prefix": "ghcr.io/acme/project.",
                        "platform": "linux/amd64"
                    }]
                }
            },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "runtime registry image rewrites should validate: {errors:?}"
        );
        value["runtime"]["registry"]["default"] = json!("ghcr.io/acme");
        assert!(
            schema.validate(&value).is_err(),
            "runtime registry should reject inert default prefixes"
        );
        value["runtime"]["registry"]
            .as_object_mut()
            .expect("registry object")
            .remove("default");
        let mut empty_registry = value.clone();
        empty_registry["runtime"]["registry"] = json!({});
        assert!(
            schema.validate(&empty_registry).is_err(),
            "runtime registry should not accept empty inert declarations"
        );

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "runtime registry image rewrites should validate: {errors:?}"
        );

        value["runtime"]["registry"]["image_rewrites"][0]["replace_prefix"] = json!("");
        assert!(
            schema.validate(&value).is_err(),
            "runtime registry image rewrites should require non-empty replacement prefixes"
        );
        value["runtime"]["registry"]["image_rewrites"] = json!([{
            "match_prefix": "registry.example.invalid/project.",
            "replace_prefix": "ghcr.io/acme/project."
        }]);
        value["runtime"]["registry"]["auth"] = json!({ "from": "env" });
        assert!(
            schema.validate(&value).is_err(),
            "runtime registry should reject unsupported auth blobs"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_runtime_network_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "runtime": {
                "network": {
                    "task_sandbox": "full",
                    "agent": "llm_egress",
                    "egress": ["api.openai.com"]
                }
            },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "runtime network declaration should validate: {errors:?}"
        );

        let mut egress_without_plane = value.clone();
        egress_without_plane["runtime"]["network"] = json!({ "egress": ["api.openai.com"] });
        assert!(
            schema.validate(&egress_without_plane).is_err(),
            "authoring egress declarations should require a network-capable runtime plane"
        );

        let mut egress_with_default = value.clone();
        egress_with_default["runtime"]["network"] =
            json!({ "default": "allowlist_enforced", "egress": ["api.openai.com"] });
        assert!(
            schema.validate(&egress_with_default).is_ok(),
            "authoring network.default should satisfy non-empty egress declarations"
        );

        let mut hermetic_defaulted = value.clone();
        hermetic_defaulted
            .as_object_mut()
            .expect("authoring root object")
            .remove("runtime");
        hermetic_defaulted["policy"] = json!({ "sanitization_profile": "hermetic_functional" });
        assert!(
            schema.validate(&hermetic_defaulted).is_ok(),
            "hermetic authoring should allow omitted network planes because authoring defaults them to none"
        );

        let mut hermetic_task_full = value.clone();
        hermetic_task_full["policy"] = json!({ "sanitization_profile": "hermetic_functional" });
        assert!(
            schema.validate(&hermetic_task_full).is_err(),
            "hermetic authoring should reject explicit task network access"
        );

        let mut hermetic_agent_full = value.clone();
        hermetic_agent_full["policy"] = json!({ "sanitization_profile": "hermetic_functional" });
        hermetic_agent_full["runtime"]["network"]["task_sandbox"] = json!("none");
        hermetic_agent_full["runtime"]["network"]["agent"] = json!("full");
        assert!(
            schema.validate(&hermetic_agent_full).is_err(),
            "hermetic authoring should reject explicit agent network access"
        );

        let mut hermetic_default_full = value.clone();
        hermetic_default_full["policy"] = json!({ "sanitization_profile": "hermetic_functional" });
        hermetic_default_full["runtime"]["network"] = json!({ "default": "full" });
        assert!(
            schema.validate(&hermetic_default_full).is_err(),
            "hermetic authoring should reject network defaults that expand to network access"
        );

        value["runtime"]["network"]["task_sandbox"] = json!("llm_egress");
        assert!(
            schema.validate(&value).is_err(),
            "task sandbox network should reject agent-only llm_egress"
        );
        value["runtime"]["network"]["task_sandbox"] = json!("full");

        value["runtime"]["network"]["default"] = json!("llm_egress");
        assert!(
            schema.validate(&value).is_err(),
            "network default should reject agent-only llm_egress because it also feeds the task sandbox"
        );
        value["runtime"]["network"]
            .as_object_mut()
            .expect("network object")
            .remove("default");

        value["runtime"]["network"]["proxy"] = json!("corp-proxy");
        assert!(
            schema.validate(&value).is_err(),
            "runtime network should reject unsupported keys"
        );

        value["runtime"]["network"]
            .as_object_mut()
            .expect("network object")
            .remove("proxy");
        value["runtime"]["network"]["egress"] = json!(["api.openai.com", "api.openai.com"]);
        assert!(
            schema.validate(&value).is_err(),
            "runtime network egress should reject duplicates"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_externals_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            },
            "externals": {
                "apis": ["api.openai.com"],
                "credentials": ["OPENAI_API_KEY"]
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "declared externals should validate: {errors:?}"
        );

        value["externals"]["services"] = json!([{ "name": "openai" }]);
        assert!(
            schema.validate(&value).is_err(),
            "externals should reject unsupported service blobs"
        );

        value["externals"]
            .as_object_mut()
            .expect("externals object")
            .remove("services");
        value["externals"]["apis"] = json!(["api.openai.com", "api.openai.com"]);
        assert!(
            schema.validate(&value).is_err(),
            "externals apis should reject duplicates"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_task_sandbox_policy_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            },
            "policy": {
                "task_sandbox": {
                    "resources": {
                        "cpu_count": 2,
                        "memory_mb": 2048
                    },
                    "hardening": {
                        "no_new_privileges": true,
                        "drop_all_caps": true
                    }
                }
            },
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "declared task sandbox policy should validate: {errors:?}"
        );

        value["policy"]["task_sandbox"]["resources"]["cpu_count"] = json!(0);
        assert!(
            schema.validate(&value).is_err(),
            "task sandbox cpu_count should reject non-positive values"
        );

        value["policy"]["task_sandbox"]["resources"]["cpu_count"] = json!(2);
        value["policy"]["task_sandbox"]["network"] = json!("none");
        assert!(
            schema.validate(&value).is_err(),
            "task sandbox policy should reject runtime-network aliases"
        );

        value["policy"]["task_sandbox"]
            .as_object_mut()
            .expect("task sandbox object")
            .remove("network");
        value["policy"]["task_sandbox"]["hardening"]["seccomp"] = json!("default");
        assert!(
            schema.validate(&value).is_err(),
            "task sandbox hardening should reject inert fallback keys"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_validity_policy_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            },
            "policy": {
                "validity": {
                    "fail_on_state_leak": true,
                    "fail_on_profile_invariant_violation": true
                }
            },
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "declared validity policy should validate: {errors:?}"
        );

        value["policy"]["validity"]["fail_on_unknown_mutation"] = json!(true);
        assert!(
            schema.validate(&value).is_err(),
            "validity policy should reject inert future-looking keys"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_extra_outputs_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            },
            "extra_outputs": [
                {
                    "id": "host_eval",
                    "source_path": "host_eval",
                    "summary_path": "grader/host_eval"
                }
            ]
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "declared extra outputs should validate: {errors:?}"
        );

        value["extra_outputs"][0]["source_path"] = json!("../private.json");
        assert!(
            schema.validate(&value).is_err(),
            "extra output paths should reject parent traversal"
        );

        value["extra_outputs"] = json!({
            "host_eval": {
                "source_path": "host_eval",
                "summary_path": "grader/host_eval"
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "extra_outputs should be a declared list, not an arbitrary map"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_agent_observability_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {
                    "command": ["agent", "__BUCEPHALUS_EVENT_PATH_agent_events__"],
                    "events": [
                        {
                            "id": "agent_events"
                        }
                    ],
                    "output_mounts": [
                        {
                            "id": "session_context",
                            "path": "session-context",
                            "env": "BUCEPHALUS_SESSION_CONTEXT_ROOT"
                        }
                    ]
                }
            },
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "current agent observability surface should validate: {errors:?}"
        );

        value["stages"]["agent"]["events"][0]["format"] = json!("ndjson");
        assert!(
            schema.validate(&value).is_err(),
            "authoring event sink format should default to jsonl or explicitly be jsonl"
        );
        value["stages"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("format");
        value["stages"]["agent"]["events"][0]["mode"] = json!("stream");
        assert!(
            schema.validate(&value).is_err(),
            "authoring event sink mode should default to jsonl or explicitly be jsonl"
        );
        value["stages"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("mode");

        value["stages"]["agent"]["events"][0]["ingest"] = json!("true");
        assert!(
            schema.validate(&value).is_err(),
            "authoring event sink ingest should be boolean when explicitly declared"
        );
        value["stages"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("ingest");
        value["stages"]["agent"]["events"][0]["retain_raw"] = json!("false");
        assert!(
            schema.validate(&value).is_err(),
            "authoring event sink retain_raw should be boolean when explicitly declared"
        );
        value["stages"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("retain_raw");
        value["stages"]["agent"]["output_mounts"][0]["persist"] = json!("true");
        assert!(
            schema.validate(&value).is_err(),
            "authoring output mount persist should be boolean when explicitly declared"
        );
        value["stages"]["agent"]["output_mounts"][0]
            .as_object_mut()
            .expect("output mount object")
            .remove("persist");
        value["stages"]["agent"]["output_mounts"][0]["kind"] = json!("file");
        assert!(
            schema.validate(&value).is_err(),
            "authoring output mount kind should only support directories"
        );
        value["stages"]["agent"]["output_mounts"][0]
            .as_object_mut()
            .expect("output mount object")
            .remove("kind");

        value["stages"]["agent"]["events"][0]["path"] = json!("/bucephalus/out/agent-events.jsonl");
        assert!(
            schema.validate(&value).is_err(),
            "event paths should be runner-owned"
        );

        value["stages"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("path");
        value["stages"]["agent"]["events"][0]["persist"] = json!(true);
        assert!(
            schema.validate(&value).is_err(),
            "event retain behavior should use retain_raw, not persist alias"
        );

        value["stages"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("persist");
        value["stages"]["agent"]["telemetry"] =
            json!({ "trajectory_path": "/bucephalus/out/trajectory.jsonl" });
        assert!(
            schema.validate(&value).is_err(),
            "authoring should use traces.source or explicit events, not agent telemetry internals"
        );

        value["stages"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("telemetry");
        value["stages"]["agent"]["protocol"] = json!("command");
        assert!(
            schema.validate(&value).is_err(),
            "authoring should not expose one-value agent protocol declarations"
        );

        value["stages"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("protocol");
        value["stages"]["agent"]["output_mounts"][0]["path"] = json!("../session-context");
        assert!(
            schema.validate(&value).is_err(),
            "output mount paths should reject parent traversal"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_secret_credential_cache_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "runtime": {
                "secrets": [
                    {
                        "name": "codex_oauth",
                        "from": "file",
                        "mount": {
                            "target": "/run/secrets/codex-auth.json",
                            "required_for_variants": ["codex_cli"]
                        },
                        "credential_cache": {
                            "kind": "run_scoped",
                            "target": "/root/.config/nova/codex-auth.json",
                            "env": "CODEX_AUTH_CACHE_FILE"
                        }
                    }
                ]
            },
            "matrix": {
                "variants": [{ "id": "codex_cli", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "current credential cache shape should validate: {errors:?}"
        );

        let mut env_short_form = value.clone();
        env_short_form["runtime"]["secrets"][0] = json!({ "name": "OPENAI_API_KEY" });
        assert!(
            schema.validate(&env_short_form).is_ok(),
            "name-only secrets should validate so authoring can default from=env"
        );

        env_short_form["runtime"]["secrets"][0]["name"] = json!("OPENAI_API_KEY=oops");
        assert!(
            schema.validate(&env_short_form).is_err(),
            "secret names should reject env assignment syntax"
        );

        env_short_form["runtime"]["secrets"][0]["name"] = json!("   ");
        assert!(
            schema.validate(&env_short_form).is_err(),
            "secret names should reject whitespace-only values"
        );

        let mut file_short_form = value.clone();
        file_short_form["runtime"]["secrets"][0]
            .as_object_mut()
            .expect("secret object")
            .remove("from");
        assert!(
            schema.validate(&file_short_form).is_ok(),
            "mounted secrets should validate so authoring can default from=file"
        );

        let mut env_with_mount = value.clone();
        env_with_mount["runtime"]["secrets"][0]["from"] = json!("env");
        assert!(
            schema.validate(&env_with_mount).is_err(),
            "from=env secrets must not declare file mounts"
        );

        let mut env_with_cache = value.clone();
        env_with_cache["runtime"]["secrets"][0]["from"] = json!("env");
        env_with_cache["runtime"]["secrets"][0]
            .as_object_mut()
            .expect("secret object")
            .remove("mount");
        assert!(
            schema.validate(&env_with_cache).is_err(),
            "from=env secrets must not declare credential caches"
        );

        let mut file_without_mount = value.clone();
        file_without_mount["runtime"]["secrets"][0]
            .as_object_mut()
            .expect("secret object")
            .remove("mount");
        assert!(
            schema.validate(&file_without_mount).is_err(),
            "from=file secrets must declare a mount target"
        );

        let mut cache_without_mount_or_from = value.clone();
        cache_without_mount_or_from["runtime"]["secrets"][0]
            .as_object_mut()
            .expect("secret object")
            .remove("from");
        cache_without_mount_or_from["runtime"]["secrets"][0]
            .as_object_mut()
            .expect("secret object")
            .remove("mount");
        assert!(
            schema.validate(&cache_without_mount_or_from).is_err(),
            "credential caches must be attached to mounted file secrets"
        );

        value["runtime"]["secrets"][0]["credential_cache"]["target_path"] =
            json!("/root/.config/nova/codex-auth.json");
        assert!(
            schema.validate(&value).is_err(),
            "credential_cache should reject unknown target aliases"
        );

        value["runtime"]["secrets"][0]["credential_cache"]
            .as_object_mut()
            .expect("credential cache object")
            .remove("target_path");
        value["runtime"]["secrets"][0]["credential_cache"]["env"] = json!("codex_auth_cache_file");
        assert!(
            schema.validate(&value).is_err(),
            "credential_cache.env should be a runtime env name"
        );

        value["runtime"]["secrets"][0]["credential_cache"]["env"] = json!("CODEX_AUTH_CACHE_FILE");
        value["runtime"]["secrets"][0]["mount"]["target"] = json!("/bucephalus/out/auth.json");
        assert!(
            schema.validate(&value).is_err(),
            "secret mount target should reject reserved runner paths"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_ephemeral_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {
                    "command": ["agentctl"],
                    "ephemerals": ["mcp-bash"]
                },
                "grader": { "strategy": "none" }
            },
            "policy": { "timeout_ms": 1 },
            "ephemerals": {
                "mcp-bash": {
                    "image": "ghcr.io/acme/mcp-bash-server:v0.4",
                    "lifecycle": "per-trial",
                    "command": ["mcp-bash-server", "--port", "8080"],
                    "workdir": "/srv/mcp",
                    "env": { "LOG_LEVEL": "info" },
                    "expose": { "MCP_URL": "http://mcp-bash:8080" }
                }
            },
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "authoring ephemeral shape should validate: {errors:?}"
        );

        let mut host_with_ephemeral = value.clone();
        host_with_ephemeral["stages"]["execution"] = json!({ "agent_site": "host" });
        assert!(
            schema.validate(&host_with_ephemeral).is_err(),
            "host agent execution should reject container ephemeral attachments"
        );

        let mut host_with_image = value.clone();
        host_with_image["stages"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("ephemerals");
        host_with_image["stages"]["agent"]["image"] = json!("ghcr.io/acme/agent:latest");
        host_with_image["stages"]["execution"] = json!({ "agent_site": "host" });
        assert!(
            schema.validate(&host_with_image).is_err(),
            "host agent execution should reject container images"
        );

        let mut task_runtime_with_image = host_with_image.clone();
        task_runtime_with_image["stages"]["execution"] = json!({ "agent_site": "task_runtime" });
        assert!(
            schema.validate(&task_runtime_with_image).is_err(),
            "task_runtime agent execution should reject separate agent images"
        );

        let mut task_runtime_without_workspace = value.clone();
        task_runtime_without_workspace["stages"]["execution"] =
            json!({ "agent_site": "task_runtime" });
        task_runtime_without_workspace["stages"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("ephemerals");
        assert!(
            schema.validate(&task_runtime_without_workspace).is_err(),
            "task_runtime agent execution should require a container-image case workspace"
        );

        let mut readonly_files_without_site = value.clone();
        readonly_files_without_site["stages"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("ephemerals");
        readonly_files_without_site["stages"]["case"] = json!({
            "files": {
                "source": "files",
                "path": "case-files",
                "mount_path": "/case"
            }
        });
        assert!(
            schema.validate(&readonly_files_without_site).is_err(),
            "readonly-file case agents without an image should declare agent_site"
        );
        readonly_files_without_site["stages"]["execution"] = json!({ "agent_site": "host" });
        assert!(
            schema.validate(&readonly_files_without_site).is_ok(),
            "explicit host agent_site should allow readonly-file cases without an image"
        );

        let mut non_container_workspace_without_site = value.clone();
        non_container_workspace_without_site["stages"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("ephemerals");
        non_container_workspace_without_site["stages"]["case"] =
            json!({ "workspace": { "source": "git", "repo": "https://example.invalid/repo.git" } });
        assert!(
            schema
                .validate(&non_container_workspace_without_site)
                .is_err(),
            "non-container workspaces without an agent image should declare agent_site"
        );

        let mut agent_container_without_image = value.clone();
        agent_container_without_image["stages"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("ephemerals");
        agent_container_without_image["stages"]["execution"] =
            json!({ "agent_site": "agent_container" });
        assert!(
            schema.validate(&agent_container_without_image).is_err(),
            "agent_container execution should require an agent image"
        );

        value["ephemerals"]["mcp_bash"] = value["ephemerals"]["mcp-bash"].clone();
        value["stages"]["agent"]["ephemerals"] = json!(["mcp_bash"]);
        value["ephemerals"]
            .as_object_mut()
            .expect("ephemerals object")
            .remove("mcp-bash");
        assert!(
            schema.validate(&value).is_err(),
            "ephemeral ids should use portable DNS-label syntax"
        );

        value["ephemerals"]["mcp-bash"] = value["ephemerals"]["mcp_bash"].clone();
        value["stages"]["agent"]["ephemerals"] = json!(["mcp-bash"]);
        value["ephemerals"]
            .as_object_mut()
            .expect("ephemerals object")
            .remove("mcp_bash");
        value["ephemerals"]["mcp-bash"]["restart"] = json!("always");
        assert!(
            schema.validate(&value).is_err(),
            "ephemerals should reject unknown fields"
        );
    }

    #[test]
    fn experiment_authoring_schema_rejects_resolved_package_nouns() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
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

    #[test]
    fn experiment_authoring_schema_rejects_inline_knobs_section() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            },
            "knobs": {
                "temperature": { "json_pointer": "/matrix/variants/0/config/temperature" }
            }
        });

        assert!(
            schema.validate(&value).is_err(),
            "authoring schema should reject inline knobs; use knob_manifest_v1 with --overrides"
        );
    }

    #[test]
    fn experiment_authoring_schema_rejects_unknown_grader_fields() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "grader": {
                    "strategy": "none",
                    "strategyy": "in_task_runtime"
                }
            },
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();

        assert!(
            errors.iter().any(|err| err.contains("strategyy")),
            "unknown grader field should be rejected by name: {errors:?}"
        );
    }

    #[test]
    fn experiment_authoring_schema_rejects_agent_result_knob() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
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

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();

        assert!(
            errors.iter().any(|err| err.contains("result")),
            "agent result knob should be rejected by name: {errors:?}"
        );
    }

    #[test]
    fn experiment_authoring_schema_rejects_authored_agent_outputs_result() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
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

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();

        assert!(
            errors.iter().any(|err| err.contains("result")),
            "authored canonical result output should be rejected by name: {errors:?}"
        );
    }

    #[test]
    fn experiment_authoring_schema_rejects_output_capture_fields_by_type() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let base = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "outputs": {
                        "candidate": {
                            "capture": {
                                "type": "file",
                                "path": "/tmp/result.json",
                                "format": "json",
                                "field": "/score"
                            }
                        }
                    }
                }
            }
        });

        assert!(
            schema.validate(&base).is_err(),
            "file captures should reject result_json-only field selectors"
        );

        let mut workspace = base.clone();
        workspace["stages"]["agent"]["outputs"]["candidate"]["capture"] = json!({
            "type": "workspace_diff",
            "format": "unified_diff",
            "path": "/tmp/candidate.patch"
        });
        assert!(
            schema.validate(&workspace).is_err(),
            "workspace_diff captures should reject runner-owned path"
        );

        let mut result_json = base;
        result_json["stages"]["agent"]["outputs"]["candidate"]["capture"] = json!({
            "type": "result_json",
            "path": "/tmp/result.json",
            "field": "/score"
        });
        let errors = schema
            .validate(&result_json)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "result_json captures should allow field selectors: {errors:?}"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_grader_input_transport_shape() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
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
                    "command": ["agent"],
                    "outputs": {
                        "candidate": {
                            "capture": {
                                "type": "workspace_diff",
                                "format": "unified_diff"
                            }
                        }
                    }
                },
                "grader": {
                    "strategy": "in_task_runtime",
                    "command": ["python3", "grade.py"],
                    "inputs": {
                        "payload": {
                            "source": {
                                "object": {
                                    "case_id": { "case": "id" },
                                    "candidate": {
                                        "output": "agent.candidate",
                                        "field": "patch"
                                    }
                                }
                            },
                            "materialize": {
                                "as": "json_file",
                                "path": "/bucephalus/out/grader_inputs/payload.json"
                            },
                            "required": true
                        }
                    },
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

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(errors.is_empty(), "unexpected schema errors: {errors:?}");

        value["stages"]["grader"]["inputs"]["payload"]["source"] = json!({
            "case": "id",
            "output": "agent.candidate"
        });
        assert!(
            schema.validate(&value).is_err(),
            "transport source should declare exactly one source variant"
        );

        value["stages"]["grader"]["inputs"]["payload"]["source"] = json!({ "case": "id" });
        value["stages"]["grader"]["inputs"]["payload"]["materialize"] = json!({
            "as": "stdin"
        });
        assert!(
            schema.validate(&value).is_err(),
            "reserved stdin materialization should be rejected by authoring schema"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_grader_strategy_configs() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
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
                "grader": {
                    "strategy": "in_task_runtime",
                    "command": ["python3", "grade.py"],
                    "max_concurrency": 2,
                    "in_task_runtime": {
                        "hidden_paths": ["/workspace/tests"],
                        "revealed_paths": ["/workspace/tests"]
                    }
                }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "in_task_runtime grader config should validate: {errors:?}"
        );

        let mut in_task_runtime_without_workspace = value.clone();
        in_task_runtime_without_workspace["stages"]["case"]["workspace"]["source"] = json!("git");
        assert!(
            schema.validate(&in_task_runtime_without_workspace).is_err(),
            "in_task_runtime grader should require a container-image case workspace"
        );

        value["stages"]["grader"]["in_task_runtime"]["hidden_path"] = json!(["/workspace/tests"]);
        assert!(
            schema.validate(&value).is_err(),
            "grader strategy config should reject misspelled fields"
        );

        value["stages"]["grader"]["in_task_runtime"]
            .as_object_mut()
            .expect("in_task_runtime object")
            .remove("hidden_path");
        value["stages"]["grader"]["strategy"] = json!("injected");
        value["stages"]["grader"]["injected"] = json!({
            "bundle": "./grader_bundle.tar.gz",
            "copy_dest": "/opt/grader"
        });
        value["stages"]["grader"]["in_task_runtime"] = json!(null);
        value["stages"]["grader"]
            .as_object_mut()
            .expect("grader object")
            .remove("in_task_runtime");
        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "injected grader config should validate: {errors:?}"
        );

        let mut injected_without_workspace = value.clone();
        injected_without_workspace["stages"]["case"] = json!({ "interface": "input_only" });
        assert!(
            schema.validate(&injected_without_workspace).is_err(),
            "injected grader should require a container-image case workspace"
        );

        value["stages"]["grader"]["injected"]["copy_dest"] = json!("relative/path");
        assert!(
            schema.validate(&value).is_err(),
            "injected.copy_dest should be an absolute container path"
        );

        value["stages"]["grader"]["injected"]
            .as_object_mut()
            .expect("injected object")
            .remove("copy_dest");
        value["stages"]["grader"]["strategy"] = json!("host");
        value["stages"]["grader"]
            .as_object_mut()
            .expect("grader object")
            .remove("injected");
        value["stages"]["grader"]["host"] = json!({ "capability": "official_grader" });
        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "host grader config should validate: {errors:?}"
        );

        value["stages"]["grader"]["host"]["capability"] = json!("nested/capability");
        assert!(
            schema.validate(&value).is_err(),
            "host capability should be a single path segment"
        );

        value["stages"]["grader"] = json!({
            "strategy": "host",
            "command": ["python3", "grade.py"],
            "host": { "capability": "official_grader" },
            "ephemerals": ["cache"]
        });
        assert!(
            schema.validate(&value).is_err(),
            "host grader should reject container ephemeral attachments"
        );

        value["stages"]["grader"] = json!({
            "strategy": "separate",
            "separate": {
                "image": "ghcr.io/acme/grader:latest",
                "workdir": "/grader"
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "active grader strategies should require command"
        );

        value["stages"]["grader"] = json!({
            "strategy": "injected",
            "command": ["python3", "grade.py"]
        });
        assert!(
            schema.validate(&value).is_err(),
            "injected grader should require injected config"
        );

        value["stages"]["grader"] = json!({
            "strategy": "none",
            "command": ["python3", "grade.py"]
        });
        assert!(
            schema.validate(&value).is_err(),
            "strategy=none grader should reject inert command declarations"
        );

        value["stages"]["grader"] = json!({
            "strategy": "none",
            "outputs": {
                "score": {
                    "capture": {
                        "type": "file",
                        "path": "/bucephalus/out/score.json",
                        "format": "json"
                    }
                }
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "strategy=none grader should reject inert output declarations"
        );
    }

    #[test]
    fn experiment_authoring_schema_validates_evaluation_policy_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            },
            "evaluation": {
                "policy": {
                    "task_model": "dependent",
                    "scoring_lifecycle": "predict_then_score",
                    "chain_failure_policy": "continue_with_flag",
                    "required_evidence_classes": ["agent_patch"]
                }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "current evaluation policy surface should validate: {errors:?}"
        );

        value["evaluation"]["policy"]["task_model"] = json!("mystery_model");
        assert!(
            schema.validate(&value).is_err(),
            "evaluation.policy.task_model should be a known model"
        );

        value["evaluation"]["policy"]["task_model"] = json!("dependent");
        value["evaluation"]["policy"]["required_evidence_class"] = json!("agent_patch");
        assert!(
            schema.validate(&value).is_err(),
            "evaluation.policy should reject misspelled fields"
        );

        value["evaluation"]["policy"]
            .as_object_mut()
            .expect("policy object")
            .remove("required_evidence_class");
        value["evaluation"]["policy"]["evaluator_mode"] = json!("custom");
        assert!(
            schema.validate(&value).is_err(),
            "evaluation.policy should reject removed inert evaluator_mode"
        );

        value["evaluation"]["policy"]
            .as_object_mut()
            .expect("policy object")
            .remove("evaluator_mode");
        value["evaluation"]["grader"] = json!({ "strategy": "host" });
        assert!(
            schema.validate(&value).is_err(),
            "evaluation should not accept legacy grader config"
        );
    }

    #[test]
    fn experiment_authoring_schema_rejects_object_case_source() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": {
                    "source": { "type": "file" },
                    "path": "cases.jsonl"
                }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "grader": { "strategy": "none" }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| {
                errors
                    .map(|err| format_validation_error(&err))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        assert!(
            errors
                .iter()
                .any(|err| err.contains("/matrix/cases/source")),
            "object case source should be rejected at public cases source: {errors:?}"
        );
    }

    #[test]
    fn experiment_authoring_schema_closes_backend_config_surface() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "runtime": {
                "compute": { "config": {} }
            },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "defaultable compute backend with empty config should validate: {errors:?}"
        );

        value["runtime"]["storage"] = json!({});
        assert!(
            schema.validate(&value).is_err(),
            "authoring schema should reject no-op runtime.storage backend declarations"
        );
        value["runtime"]
            .as_object_mut()
            .expect("runtime object")
            .remove("storage");
        value["runtime"]["traces"] = json!({});
        assert!(
            schema.validate(&value).is_err(),
            "authoring schema should reject no-op runtime.traces backend declarations"
        );
        value["runtime"]
            .as_object_mut()
            .expect("runtime object")
            .remove("traces");

        value["runtime"]["compute"] = json!({ "backend": "modal", "config": {} });
        assert!(
            schema.validate(&value).is_ok(),
            "implemented compute backend names should validate"
        );

        value["runtime"]["compute"]["config"] =
            json!({ "max_parallel": 4, "trial_timeout_ms": 600000 });
        assert!(
            schema.validate(&value).is_err(),
            "compute backend config should reject overlapping concurrency and timeout knobs"
        );

        value["runtime"]["compute"] = json!({ "backend": "kubernetes", "config": {} });
        assert!(
            schema.validate(&value).is_err(),
            "authoring schema should reject unsupported compute backend names"
        );
    }

    #[test]
    fn experiment_authoring_schema_closes_variant_overrides_to_public_stages() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{
                    "id": "baseline",
                    "baseline": true,
                    "overrides": {
                        "case": {
                            "workspace": {
                                "image": { "from": "case_row" }
                            }
                        },
                        "agent": {
                            "image": "python:3.11-slim",
                            "ephemerals": ["mcp"]
                        }
                    }
                }],
                "cases": { "source": "file", "path": "cases.jsonl" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "public variant stage overrides should validate: {errors:?}"
        );

        value["matrix"]["variants"][0]["overrides"]["execution"] = json!({ "agent_site": "host" });
        assert!(
            schema.validate(&value).is_err(),
            "authoring host agent overrides should reject container agent fields"
        );

        value["matrix"]["variants"][0]["overrides"]["execution"] =
            json!({ "agent_site": "task_runtime" });
        assert!(
            schema.validate(&value).is_err(),
            "authoring task_runtime overrides should reject non-container case workspaces"
        );

        value["matrix"]["variants"][0]["overrides"]
            .as_object_mut()
            .expect("overrides object")
            .remove("execution");

        value["matrix"]["variants"][0]["overrides"]["case"] = json!({
            "workspace": { "source": "git", "path": "variant-workspace" }
        });
        value["matrix"]["variants"][0]["overrides"]["grader"] = json!({
            "strategy": "in_task_runtime",
            "command": ["grade"]
        });
        assert!(
            schema.validate(&value).is_err(),
            "authoring in_task_runtime grader overrides should reject non-container case workspaces"
        );

        value["matrix"]["variants"][0]["overrides"]
            .as_object_mut()
            .expect("overrides object")
            .remove("grader");

        value["matrix"]["variants"][0]["overrides"]["task"] = json!({ "interface": "input_only" });
        assert!(
            schema.validate(&value).is_err(),
            "authoring overrides should reject resolved task noun"
        );

        value["matrix"]["variants"][0]["overrides"]
            .as_object_mut()
            .expect("overrides object")
            .remove("task");
        value["matrix"]["variants"][0]["overrides"]["policy"] = json!({ "timeout_ms": 1 });
        assert!(
            schema.validate(&value).is_err(),
            "authoring overrides should reject inert policy patches"
        );

        value["matrix"]["variants"][0]["overrides"]
            .as_object_mut()
            .expect("overrides object")
            .remove("policy");
        value["matrix"]["variants"][0]["overrides"]["case"] = json!({
            "files": {
                "source": "files",
                "path": "case-files",
                "mount_path": "/case"
            },
            "workspace": {
                "source": "empty"
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "authoring case overrides should not declare both files and workspace"
        );

        value["matrix"]["variants"][0]["overrides"]["case"] = json!({
            "interface": "input_only",
            "workspace": { "source": "empty" }
        });
        assert!(
            schema.validate(&value).is_err(),
            "authoring input_only case overrides should not declare workspace"
        );

        value["matrix"]["variants"][0]["overrides"]["case"] = json!({
            "interface": "readonly_files",
            "workspace": { "source": "empty" }
        });
        assert!(
            schema.validate(&value).is_err(),
            "authoring readonly_files case overrides should not declare workspace"
        );

        value["matrix"]["variants"][0]["overrides"]["case"] = json!({
            "interface": "writable_workspace",
            "files": {
                "source": "files",
                "path": "case-files",
                "mount_path": "/case"
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "authoring writable_workspace case overrides should not declare files"
        );

        value["matrix"]["variants"][0]["overrides"]["case"] = json!({
            "workspace": {
                "image": { "from": "case_row" }
            }
        });
        value["matrix"]["variants"][0]["overrides"]["agent"]["custom_image"] =
            json!({ "image": "example:bad" });
        assert!(
            schema.validate(&value).is_err(),
            "authoring overrides should reject unknown agent override fields"
        );
    }

    #[test]
    fn resolved_experiment_schema_rejects_object_task_source() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": { "type": "file" },
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| {
                errors
                    .map(|err| format_validation_error(&err))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        assert!(
            errors
                .iter()
                .any(|err| err.contains("/matrix/tasks/source")),
            "object task source should be rejected at resolved task source: {errors:?}"
        );
    }

    #[test]
    fn resolved_experiment_schema_enforces_task_interface_resources() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        assert!(
            schema.validate(&value).is_ok(),
            "minimal input_only runtime should validate"
        );

        let mut task_runtime_without_workspace = value.clone();
        task_runtime_without_workspace["trial_runtime"]["execution"] =
            json!({ "agent_site": "task_runtime" });
        assert!(
            schema.validate(&task_runtime_without_workspace).is_err(),
            "resolved task_runtime execution should require a container-image task workspace"
        );

        value["trial_runtime"]["task"]["files"] = json!({
            "source": "files",
            "path": "case-files",
            "mount_path": "/case"
        });
        assert!(
            schema.validate(&value).is_err(),
            "input_only resolved task must not declare files"
        );

        value["trial_runtime"]["task"]
            .as_object_mut()
            .expect("task object")
            .remove("files");
        value["trial_runtime"]["task"]["interface"] = json!("readonly_files");
        value["trial_runtime"]["task"]["files"] = json!({
            "source": "files",
            "path": "case-files",
            "mount_path": "/case"
        });
        assert!(
            schema.validate(&value).is_ok(),
            "readonly_files resolved task should require and allow files"
        );
        value["trial_runtime"]["task"]["workspace"] = json!({
            "source": "empty"
        });
        assert!(
            schema.validate(&value).is_err(),
            "readonly_files resolved task must not declare workspace"
        );

        value["trial_runtime"]["task"]
            .as_object_mut()
            .expect("task object")
            .remove("files");
        value["trial_runtime"]["task"]["interface"] = json!("writable_workspace");
        assert!(
            schema.validate(&value).is_ok(),
            "writable_workspace resolved task should require and allow workspace"
        );

        value["trial_runtime"]["execution"] = json!({ "agent_site": "task_runtime" });
        assert!(
            schema.validate(&value).is_err(),
            "resolved task_runtime execution should reject non-container workspaces"
        );

        value["trial_runtime"]["task"]["workspace"]["source"] = json!("container_image");
        assert!(
            schema.validate(&value).is_ok(),
            "resolved task_runtime execution should accept container-image workspaces"
        );

        value["trial_runtime"]["task"]["files"] = json!({
            "source": "files",
            "path": "case-files",
            "mount_path": "/case"
        });
        assert!(
            schema.validate(&value).is_err(),
            "writable_workspace resolved task must not declare files"
        );
    }

    #[test]
    fn resolved_experiment_schema_requires_meaningful_metadata() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": {
                "id": "smoke_eval",
                "name": "Smoke eval",
                "description": "Smoke metadata contract",
                "owner": "eval-team",
                "tags": ["smoke", "local"]
            },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        assert!(
            schema.validate(&value).is_ok(),
            "non-empty metadata should validate"
        );

        value["experiment"]["description"] = json!("");
        assert!(
            schema.validate(&value).is_err(),
            "empty resolved experiment descriptions should be rejected"
        );

        value["experiment"]["description"] = json!("Smoke metadata contract");
        value["experiment"]["owner"] = json!("");
        assert!(
            schema.validate(&value).is_err(),
            "empty resolved experiment owners should be rejected"
        );

        value["experiment"]["owner"] = json!("eval-team");
        value["experiment"]["tags"] = json!(["smoke", "smoke"]);
        assert!(
            schema.validate(&value).is_err(),
            "duplicate resolved experiment tags should be rejected"
        );

        value["experiment"]["tags"] = json!(["smoke", ""]);
        assert!(
            schema.validate(&value).is_err(),
            "empty resolved experiment tags should be rejected"
        );

        value["experiment"]["tags"] = json!(["smoke", "local"]);
        value["experiment"]["id"] = json!("   ");
        assert!(
            schema.validate(&value).is_err(),
            "whitespace-only resolved experiment ids should be rejected"
        );

        value["experiment"]["id"] = json!("smoke_eval");
        value["trial_runtime"]["agent"]["command"] = json!(["agent", "   "]);
        assert!(
            schema.validate(&value).is_err(),
            "resolved argv parts should reject whitespace-only values"
        );
    }

    #[test]
    fn resolved_experiment_schema_rejects_top_level_version() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let value = json!({
            "version": "1.0",
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        assert!(
            schema.validate(&value).is_err(),
            "resolved packages should not carry stale top-level version metadata"
        );
    }

    #[test]
    fn resolved_experiment_schema_rejects_public_authoring_aliases() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        value["stages"] = json!({});
        assert!(
            schema.validate(&value).is_err(),
            "resolved schema should reject public stages alias"
        );

        value.as_object_mut().expect("object").remove("stages");
        value["matrix"]["cases"] = json!({
            "source": "file",
            "path": "cases.jsonl"
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved schema should reject public matrix.cases alias"
        );

        value.as_object_mut().expect("object").remove("matrix");
        value["matrix"] = json!({
            "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
            "tasks": {
                "source": "file",
                "path": "tasks/tasks.jsonl"
            },
            "repeats": 1
        });
        value["experiment"]["mode"] = json!("answer");
        assert!(
            schema.validate(&value).is_err(),
            "resolved schema should reject authoring-only experiment.mode"
        );
    }

    #[test]
    fn resolved_experiment_schema_requires_explicit_variant_baseline_flags() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });
        let errors = schema
            .validate(&value)
            .expect_err("resolved variant baseline flag should be required")
            .map(|err| format_validation_error(&err))
            .collect::<Vec<_>>();
        assert!(
            errors
                .iter()
                .any(|err| err.contains("/matrix/variants/0/baseline is required")),
            "unexpected errors: {errors:?}"
        );
    }

    #[test]
    fn resolved_experiment_schema_requires_explicit_output_required_flags() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });
        let mut missing_result = value.clone();
        missing_result["trial_runtime"]["agent"]["outputs"]
            .as_object_mut()
            .expect("outputs object")
            .remove("result");
        let errors = schema
            .validate(&missing_result)
            .expect_err("resolved canonical result output should be required")
            .map(|err| format_validation_error(&err))
            .collect::<Vec<_>>();
        assert!(
            errors
                .iter()
                .any(|err| err.contains("/trial_runtime/agent/outputs/result is required")),
            "unexpected errors: {errors:?}"
        );

        let mut wrong_result_path = value.clone();
        wrong_result_path["trial_runtime"]["agent"]["outputs"]["result"]["capture"]["path"] =
            json!("/tmp/result.json");
        assert!(
            schema.validate(&wrong_result_path).is_err(),
            "resolved canonical result output should capture the runner-owned result path"
        );

        let mut result_override = value.clone();
        result_override["matrix"]["variants"][0]["overrides"] = json!({
            "agent": {
                "outputs": {
                    "result": {
                        "capture": {
                            "type": "file",
                            "path": "/bucephalus/out/result.json",
                            "format": "json",
                            "required": true
                        }
                    }
                }
            }
        });
        assert!(
            schema.validate(&result_override).is_err(),
            "resolved variant overrides should not mutate the canonical agent result output"
        );

        value["trial_runtime"]["agent"]["outputs"]["result"]["capture"]
            .as_object_mut()
            .expect("capture object")
            .remove("required");
        let errors = schema
            .validate(&value)
            .expect_err("resolved output capture required flag should be required")
            .map(|err| format_validation_error(&err))
            .collect::<Vec<_>>();
        assert!(
            errors
                .iter()
                .any(|err| err.contains("/trial_runtime/agent/outputs/result/capture")),
            "unexpected errors: {errors:?}"
        );
    }

    #[test]
    fn resolved_experiment_schema_requires_explicit_grader_input_required_flags() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });
        value["trial_runtime"]["task"] = json!({
            "interface": "writable_workspace",
            "workspace": { "source": "container_image" }
        });
        value["trial_runtime"]["grader"] = json!({
            "strategy": "in_task_runtime",
            "command": ["python3", "grade.py"],
            "in_task_runtime": {},
            "inputs": {
                "prompt": {
                    "source": { "case": "input.prompt" },
                    "materialize": {
                        "as": "json_file",
                        "path": "/bucephalus/out/grader_inputs/prompt.json"
                    }
                }
            },
            "outputs": {
                "report": {
                    "capture": {
                        "type": "file",
                        "path": "/bucephalus/out/grader-report.json",
                        "format": "json",
                        "required": true
                    }
                }
            }
        });

        let errors = schema
            .validate(&value)
            .expect_err("resolved grader input required flag should be required")
            .map(|err| format_validation_error(&err))
            .collect::<Vec<_>>();
        assert!(
            errors
                .iter()
                .any(|err| err.contains("/trial_runtime/grader/inputs/prompt/required")),
            "unexpected errors: {errors:?}"
        );

        value["trial_runtime"]["grader"]["inputs"]["prompt"]["required"] = json!(true);
        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved grader input required flag should validate: {errors:?}"
        );
    }

    #[test]
    fn resolved_experiment_schema_requires_explicit_metric_flags() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation(),
            "metrics": [{
                "id": "resolved",
                "source": { "type": "agent_response", "pointer": "/metrics/resolved" }
            }]
        });

        let errors = schema
            .validate(&value)
            .expect_err("resolved metric flags should be required")
            .map(|err| format_validation_error(&err))
            .collect::<Vec<_>>();
        assert!(
            errors.iter().any(|err| err.contains("/metrics/0/primary")),
            "unexpected errors: {errors:?}"
        );
        assert!(
            errors.iter().any(|err| err.contains("/metrics/0/required")),
            "unexpected errors: {errors:?}"
        );

        value["metrics"][0]["primary"] = json!(true);
        value["metrics"][0]["required"] = json!(true);
        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved metric flags should validate: {errors:?}"
        );

        value["metrics"][0]["primary"] = json!(false);
        assert!(
            schema.validate(&value).is_err(),
            "resolved metrics should require one primary metric when metrics are declared"
        );

        value["metrics"] = json!([
            {
                "id": "resolved",
                "source": { "type": "agent_response", "pointer": "/metrics/resolved" },
                "primary": true,
                "required": true
            },
            {
                "id": "latency_ms",
                "source": { "type": "agent_response", "pointer": "/metrics/latency_ms" },
                "primary": false,
                "required": true
            }
        ]);
        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved metrics should accept exactly one primary metric: {errors:?}"
        );
    }

    #[test]
    fn resolved_experiment_schema_closes_outer_contract_containers() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "minimal resolved package should validate: {errors:?}"
        );

        value["debug"] = json!({ "kept_by": "accident" });
        assert!(
            schema.validate(&value).is_err(),
            "resolved top-level should reject unknown fields"
        );

        value.as_object_mut().expect("object").remove("debug");
        value["runtime"]["compute_alias"] = json!("local-docker");
        assert!(
            schema.validate(&value).is_err(),
            "resolved runtime should reject unknown fields"
        );

        value["runtime"]
            .as_object_mut()
            .expect("runtime object")
            .remove("compute_alias");
        value["policy"]["validations"] = json!({});
        assert!(
            schema.validate(&value).is_err(),
            "resolved policy should reject unknown fields"
        );

        value["policy"]
            .as_object_mut()
            .expect("policy object")
            .remove("validations");
        value["trial_runtime"]["agent_runtime"] = json!({});
        assert!(
            schema.validate(&value).is_err(),
            "resolved trial_runtime should reject unknown fields"
        );
    }

    #[test]
    fn resolved_experiment_schema_rejects_stale_matrix_and_scheduling_fields() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl",
                    "suite_id": "smoke",
                    "split_id": "dev",
                    "limit": 1
                },
                "repeats": 1
            },
            "scheduling": {
                "max_concurrency": 1,
                "random_seed": 1
            },
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved matrix and scheduling fields should validate: {errors:?}"
        );

        value["matrix"]["seeds"] = json!([1, 2]);
        assert!(
            schema.validate(&value).is_err(),
            "resolved matrix should reject removed seeds field"
        );

        value["matrix"]
            .as_object_mut()
            .expect("matrix object")
            .remove("seeds");
        value["matrix"]["tasks"]["shuffle"] = json!(true);
        assert!(
            schema.validate(&value).is_err(),
            "resolved matrix.tasks should reject unsupported fields"
        );

        value["matrix"]["tasks"]
            .as_object_mut()
            .expect("tasks object")
            .remove("shuffle");
        value["scheduling"]["shuffle_tasks"] = json!(false);
        assert!(
            schema.validate(&value).is_err(),
            "resolved scheduling should reject removed shuffle_tasks field"
        );

        value["scheduling"]
            .as_object_mut()
            .expect("scheduling object")
            .remove("shuffle_tasks");
        value["scheduling"]["comparison"] = json!("paired");
        assert!(
            schema.validate(&value).is_err(),
            "resolved scheduling should reject authoring-only comparison intent"
        );
    }

    #[test]
    fn resolved_experiment_schema_closes_metric_transform_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation(),
            "metrics": [{
                "id": "pass_rate",
                "primary": true,
                "required": true,
                "source": {
                    "type": "grader_output",
                    "output": "pytest_report",
                    "pointer": "",
                    "transform": {
                        "type": "pytest_json_report_pass_rate",
                        "test_ids": {
                            "source": { "task": "commit0.test_ids" }
                        }
                    }
                }
            }]
        });
        value["trial_runtime"]["grader"] = json!({
            "strategy": "separate",
            "command": ["grade"],
            "inputs": {},
            "separate": {
                "image": "ghcr.io/acme/grader:latest",
                "workdir": "/grader"
            },
            "outputs": {
                "pytest_report": {
                    "capture": {
                        "type": "file",
                        "path": "/bucephalus/out/pytest-report.json",
                        "format": "json",
                        "required": true
                    }
                }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved metric transform should validate: {errors:?}"
        );

        let mut no_grader_metric = value.clone();
        no_grader_metric["trial_runtime"]["grader"] = json!({ "strategy": "none" });
        assert!(
            schema.validate(&no_grader_metric).is_err(),
            "resolved grader-backed metrics should require an active grader"
        );

        value["metrics"][0]["source"]["transform"]["test_ids"]["source"]["case"] =
            json!("commit0.test_ids");
        assert!(
            schema.validate(&value).is_err(),
            "resolved metric transform should reject stale source.case alias"
        );

        value["metrics"][0]["source"]["transform"]["test_ids"]["source"]
            .as_object_mut()
            .expect("source object")
            .remove("case");
        value["metrics"][0]["source"]["transform"]["labels"] = json!({ "suite": "unit" });
        assert!(
            schema.validate(&value).is_err(),
            "resolved metric transform should reject unknown transform fields"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_extra_outputs_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation(),
            "extra_outputs": [
                {
                    "id": "host_eval",
                    "source_path": "host_eval",
                    "summary_path": "grader/host_eval"
                }
            ]
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved extra outputs should validate: {errors:?}"
        );

        value["extra_outputs"][0]["summary_path"] = json!("/absolute/path");
        assert!(
            schema.validate(&value).is_err(),
            "resolved extra output paths should reject absolute paths"
        );

        value["extra_outputs"] = json!({
            "host_eval": {
                "source_path": "host_eval",
                "summary_path": "grader/host_eval"
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved extra_outputs should reject arbitrary maps"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_runtime_registry_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved runtime without registry declaration should validate: {errors:?}"
        );

        value["runtime"]["registry"] = json!({});
        assert!(
            schema.validate(&value).is_err(),
            "resolved runtime should reject authoring-only registry declarations"
        );
    }

    #[test]
    fn resolved_experiment_schema_closes_backend_config_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": {
                "compute": { "backend": "local-docker", "config": {} },
                "network": { "task_sandbox": "none", "agent": "none" }
            },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "implemented resolved compute backend with empty config should validate: {errors:?}"
        );

        value["runtime"]["storage"] = json!({ "backend": "local-fs", "config": {} });
        assert!(
            schema.validate(&value).is_err(),
            "resolved schema should reject no-op runtime.storage declarations"
        );
        value["runtime"]
            .as_object_mut()
            .expect("runtime object")
            .remove("storage");
        value["runtime"]["traces"] = json!({ "backend": "local-stdout", "config": {} });
        assert!(
            schema.validate(&value).is_err(),
            "resolved schema should reject no-op runtime.traces declarations"
        );
        value["runtime"]
            .as_object_mut()
            .expect("runtime object")
            .remove("traces");

        value["runtime"]["compute"]["config"] =
            json!({ "max_parallel": 4, "trial_timeout_ms": 600000 });
        assert!(
            schema.validate(&value).is_err(),
            "resolved compute backend config should reject stale knobs"
        );
    }

    #[test]
    fn resolved_experiment_schema_closes_variant_override_roots() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{
                    "id": "baseline",
                    "baseline": true,
                    "config": {},
                    "overrides": {
                        "task": { "workspace": { "path": "variant-workspace" } },
                        "agent": { "image": "python:3.11-slim" }
                    }
                }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved variant runtime overrides should validate: {errors:?}"
        );

        value["matrix"]["variants"][0]["overrides"]["case"] = json!({ "interface": "input_only" });
        assert!(
            schema.validate(&value).is_err(),
            "resolved overrides should reject public case noun"
        );

        value["matrix"]["variants"][0]["overrides"]
            .as_object_mut()
            .expect("overrides object")
            .remove("case");
        value["matrix"]["variants"][0]["overrides"]["policy"] = json!({ "timeout_ms": 1 });
        assert!(
            schema.validate(&value).is_err(),
            "resolved overrides should reject non-runtime policy roots"
        );

        value["matrix"]["variants"][0]["overrides"]
            .as_object_mut()
            .expect("overrides object")
            .remove("policy");
        value["matrix"]["variants"][0]["overrides"]["agent"]["custom_image"] =
            json!({ "image": "example:variant" });
        assert!(
            schema.validate(&value).is_err(),
            "resolved agent overrides should reject unknown fields"
        );

        value["matrix"]["variants"][0]["overrides"]["agent"]
            .as_object_mut()
            .expect("agent override object")
            .remove("custom_image");
        value["matrix"]["variants"][0]["overrides"]["task"] = json!({
            "files": {
                "source": "files",
                "path": "case-files",
                "mount_path": "/case"
            },
            "workspace": {
                "path": "variant-workspace"
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved task overrides should not declare both files and workspace"
        );

        value["matrix"]["variants"][0]["overrides"]["task"] = json!({
            "interface": "input_only",
            "workspace": { "path": "variant-workspace" }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved input_only task overrides should not declare workspace"
        );

        value["matrix"]["variants"][0]["overrides"]["task"] = json!({
            "interface": "readonly_files",
            "workspace": { "path": "variant-workspace" }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved readonly_files task overrides should not declare workspace"
        );

        value["matrix"]["variants"][0]["overrides"]["task"] = json!({
            "interface": "writable_workspace",
            "files": {
                "source": "files",
                "path": "case-files",
                "mount_path": "/case"
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved writable_workspace task overrides should not declare files"
        );

        value["matrix"]["variants"][0]["overrides"]["task"] =
            json!({ "workspace": { "path": "variant-workspace" } });
        value["matrix"]["variants"][0]["overrides"]["execution"] =
            json!({ "agent_site": "task_runtime" });
        value["matrix"]["variants"][0]["overrides"]["task"] = json!({
            "workspace": { "source": "git", "path": "variant-workspace" }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved task_runtime overrides should reject non-container task workspaces"
        );

        value["matrix"]["variants"][0]["overrides"]
            .as_object_mut()
            .expect("overrides object")
            .remove("execution");
        value["matrix"]["variants"][0]["overrides"]["task"] =
            json!({ "workspace": { "path": "variant-workspace" } });
        value["matrix"]["variants"][0]["overrides"]["task"] = json!({
            "workspace": { "source": "git", "path": "variant-workspace" }
        });
        value["matrix"]["variants"][0]["overrides"]["grader"] = json!({
            "strategy": "injected",
            "injected": { "bundle": "grader_bundle.tar.gz" }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved injected grader overrides should reject non-container task workspaces"
        );

        value["matrix"]["variants"][0]["overrides"]
            .as_object_mut()
            .expect("overrides object")
            .remove("grader");
        value["matrix"]["variants"][0]["overrides"]["task"] =
            json!({ "workspace": { "path": "variant-workspace" } });
        value["matrix"]["variants"][0]["overrides"]["grader"] = json!({
            "strategy": "separate",
            "separate": { "working_dir": "/grader" }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved grader overrides should reject unknown nested strategy config fields"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_runtime_network_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "network": {
                    "task_sandbox": "full",
                    "agent": "llm_egress",
                    "egress": ["api.openai.com"]
                }
            },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved runtime network declaration should validate: {errors:?}"
        );

        let mut egress_without_plane = value.clone();
        egress_without_plane["runtime"]["network"]["task_sandbox"] = json!("none");
        egress_without_plane["runtime"]["network"]["agent"] = json!("none");
        assert!(
            schema.validate(&egress_without_plane).is_err(),
            "resolved egress declarations should require a network-capable runtime plane"
        );

        let mut hermetic_networked = value.clone();
        hermetic_networked["policy"]["sanitization_profile"] = json!("hermetic_functional");
        assert!(
            schema.validate(&hermetic_networked).is_err(),
            "resolved hermetic experiments should reject networked task and agent planes"
        );

        let mut hermetic_agent_full = value.clone();
        hermetic_agent_full["policy"]["sanitization_profile"] = json!("hermetic_functional");
        hermetic_agent_full["runtime"]["network"]["task_sandbox"] = json!("none");
        hermetic_agent_full["runtime"]["network"]["agent"] = json!("full");
        assert!(
            schema.validate(&hermetic_agent_full).is_err(),
            "resolved hermetic experiments should reject agent network access"
        );

        let mut hermetic_none = value.clone();
        hermetic_none["policy"]["sanitization_profile"] = json!("hermetic_functional");
        hermetic_none["runtime"]["network"]["task_sandbox"] = json!("none");
        hermetic_none["runtime"]["network"]["agent"] = json!("none");
        hermetic_none["runtime"]["network"]
            .as_object_mut()
            .expect("runtime network")
            .remove("egress");
        assert!(
            schema.validate(&hermetic_none).is_ok(),
            "resolved hermetic experiments should validate with both network planes disabled"
        );

        value["runtime"]["network"]["default"] = json!("none");
        assert!(
            schema.validate(&value).is_err(),
            "resolved runtime network should reject authoring-only default shorthand"
        );

        value["runtime"]["network"]
            .as_object_mut()
            .expect("network object")
            .remove("default");
        value["runtime"]["network"]["proxy"] = json!("corp-proxy");
        assert!(
            schema.validate(&value).is_err(),
            "resolved runtime network should reject unsupported keys"
        );

        value["runtime"]["network"]
            .as_object_mut()
            .expect("network object")
            .remove("proxy");
        value["runtime"]["network"]["egress"] = json!(["api.openai.com", "api.openai.com"]);
        assert!(
            schema.validate(&value).is_err(),
            "resolved runtime network egress should reject duplicates"
        );

        value["runtime"]["network"]["egress"] = json!(["api.openai.com"]);
        value["runtime"]["network"]["task_sandbox"] = json!("llm_egress");
        assert!(
            schema.validate(&value).is_err(),
            "resolved task sandbox network should reject llm_egress"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_policy_policies_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": {
                "timeout_ms": 1,
                "sanitization_profile": "standard_runtime",
                "task_sandbox": {
                    "hardening": {
                        "no_new_privileges": true,
                        "drop_all_caps": true
                    }
                },
                "policies": {
                    "scheduling": "randomized",
                    "state": "isolate_per_trial",
                    "retry": {
                        "max_attempts": 3,
                        "retry_on": ["error", "timeout"]
                    },
                    "pruning": { "max_consecutive_failures": 5 },
                    "concurrency": {
                        "max_in_flight_per_variant": 2,
                        "require_chain_lease": false
                    }
                }
            },
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved policy.policies shape should validate: {errors:?}"
        );

        value["policy"]["policies"]["retry"]["backoff"] = json!("exponential");
        assert!(
            schema.validate(&value).is_err(),
            "resolved retry policy should reject inert fallback keys"
        );

        value["policy"]["policies"]["retry"]
            .as_object_mut()
            .expect("retry object")
            .remove("backoff");
        value["policy"]["policies"]["scheduler"] = json!("paired_interleaved");
        assert!(
            schema.validate(&value).is_err(),
            "resolved policy.policies should reject misspelled policy keys"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_externals_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "network": { "task_sandbox": "none", "agent": "none" },
                "externals": {
                    "apis": ["api.openai.com"],
                    "credentials": ["OPENAI_API_KEY"]
                }
            },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved externals should validate: {errors:?}"
        );

        value["runtime"]["externals"]["services"] = json!([{ "name": "openai" }]);
        assert!(
            schema.validate(&value).is_err(),
            "resolved externals should reject unsupported service blobs"
        );

        value["runtime"]["externals"]
            .as_object_mut()
            .expect("externals object")
            .remove("services");
        value["runtime"]["externals"]["credentials"] = json!(["OPENAI_API_KEY", "OPENAI_API_KEY"]);
        assert!(
            schema.validate(&value).is_err(),
            "resolved externals credentials should reject duplicates"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_task_sandbox_policy_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": {
                "timeout_ms": 1,
                "sanitization_profile": "standard_runtime",
                "task_sandbox": {
                    "resources": {
                        "cpu_count": 2,
                        "memory_mb": 2048
                    },
                    "hardening": {
                        "no_new_privileges": true,
                        "drop_all_caps": true
                    }
                },
                "policies": {
                    "scheduling": "variant_sequential",
                    "state": "isolate_per_trial",
                    "retry": {
                        "max_attempts": 1,
                        "retry_on": []
                    },
                    "pruning": {
                        "max_consecutive_failures": 0
                    },
                    "concurrency": {
                        "require_chain_lease": true
                    }
                }
            },
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved task sandbox policy should validate: {errors:?}"
        );

        value["policy"]["task_sandbox"]["resources"]["memory_mb"] = json!(0);
        assert!(
            schema.validate(&value).is_err(),
            "resolved task sandbox memory_mb should reject non-positive values"
        );

        value["policy"]["task_sandbox"]["resources"]["memory_mb"] = json!(2048);
        value["policy"]["task_sandbox"]["profile"] = json!("locked_down");
        assert!(
            schema.validate(&value).is_err(),
            "resolved task sandbox policy should reject profile aliases"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_validity_policy_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": {
                "timeout_ms": 1,
                "sanitization_profile": "standard_runtime",
                "task_sandbox": {
                    "hardening": {
                        "no_new_privileges": true,
                        "drop_all_caps": true
                    }
                },
                "validity": {
                    "fail_on_state_leak": true,
                    "fail_on_profile_invariant_violation": true
                },
                "policies": {
                    "scheduling": "variant_sequential",
                    "state": "isolate_per_trial",
                    "retry": {
                        "max_attempts": 1,
                        "retry_on": []
                    },
                    "pruning": {
                        "max_consecutive_failures": 0
                    },
                    "concurrency": {
                        "require_chain_lease": true
                    }
                }
            },
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved validity policy should validate: {errors:?}"
        );

        value["policy"]["validity"]["fail_on_unknown_mutation"] = json!(true);
        assert!(
            schema.validate(&value).is_err(),
            "resolved validity policy should reject inert future-looking keys"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_agent_observability_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "trial_runtime": {
                "task": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "artifact_type": "structured_json",
                    "integration_level": "cli_events",
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json",
                                "required": true
                            }
                        }
                    },
                    "events": [
                        {
                            "id": "agent_events",
                            "format": "jsonl",
                            "mode": "jsonl",
                            "ingest": true,
                            "retain_raw": false
                        }
                    ],
                    "output_mounts": [
                        {
                            "id": "session_context",
                            "kind": "directory",
                            "path": "session-context",
                            "env": "BUCEPHALUS_SESSION_CONTEXT_ROOT",
                            "persist": true
                        }
                    ],
                    "telemetry": {
                        "causal_extraction": "summary"
                    }
                },
                "execution": { "agent_site": "host" },
                "grader": { "strategy": "none" }
            },
            "scheduling": minimal_resolved_scheduling(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved agent observability surface should validate: {errors:?}"
        );

        value["trial_runtime"]["agent"]["events"][0]["path"] =
            json!("/bucephalus/out/agent-events.jsonl");
        assert!(
            schema.validate(&value).is_err(),
            "resolved event paths should be runner-owned"
        );

        value["trial_runtime"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("path");
        let ingest = value["trial_runtime"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("ingest")
            .expect("ingest");
        assert!(
            schema.validate(&value).is_err(),
            "resolved event sinks should require explicit ingest"
        );
        value["trial_runtime"]["agent"]["events"][0]["ingest"] = ingest;
        let format = value["trial_runtime"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("format")
            .expect("format");
        assert!(
            schema.validate(&value).is_err(),
            "resolved event sinks should require explicit format"
        );
        value["trial_runtime"]["agent"]["events"][0]["format"] = format;
        let mode = value["trial_runtime"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("mode")
            .expect("mode");
        assert!(
            schema.validate(&value).is_err(),
            "resolved event sinks should require explicit mode"
        );
        value["trial_runtime"]["agent"]["events"][0]["mode"] = mode;
        let retain_raw = value["trial_runtime"]["agent"]["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("retain_raw")
            .expect("retain_raw");
        assert!(
            schema.validate(&value).is_err(),
            "resolved event sinks should require explicit retain_raw"
        );
        value["trial_runtime"]["agent"]["events"][0]["retain_raw"] = retain_raw;
        value["trial_runtime"]["agent"]["telemetry"]["trajectory"] =
            json!("/bucephalus/out/legacy.jsonl");
        assert!(
            schema.validate(&value).is_err(),
            "resolved telemetry should reject unknown aliases"
        );

        value["trial_runtime"]["agent"]["telemetry"]
            .as_object_mut()
            .expect("telemetry object")
            .remove("trajectory");
        value["trial_runtime"]["agent"]["telemetry"]["trajectory_path"] =
            json!("/bucephalus/out/trajectory.jsonl");
        assert!(
            schema.validate(&value).is_err(),
            "resolved telemetry should reject trajectory_path overrides"
        );

        value["trial_runtime"]["agent"]["telemetry"]
            .as_object_mut()
            .expect("telemetry object")
            .remove("trajectory_path");
        value["trial_runtime"]["agent"]["telemetry"]["causal_extraction"] = json!("");
        assert!(
            schema.validate(&value).is_err(),
            "resolved telemetry values should be nonempty strings"
        );

        value["trial_runtime"]["agent"]["telemetry"]["causal_extraction"] = json!("summary");
        value["trial_runtime"]["agent"]["protocol"] = json!("command");
        assert!(
            schema.validate(&value).is_err(),
            "resolved schema should reject no-op agent protocol declarations"
        );

        value["trial_runtime"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("protocol");
        let persist = value["trial_runtime"]["agent"]["output_mounts"][0]
            .as_object_mut()
            .expect("output mount object")
            .remove("persist")
            .expect("persist");
        assert!(
            schema.validate(&value).is_err(),
            "resolved output mounts should require explicit persist"
        );
        value["trial_runtime"]["agent"]["output_mounts"][0]["persist"] = persist;
        let kind = value["trial_runtime"]["agent"]["output_mounts"][0]
            .as_object_mut()
            .expect("output mount object")
            .remove("kind")
            .expect("kind");
        assert!(
            schema.validate(&value).is_err(),
            "resolved output mounts should require explicit kind"
        );
        value["trial_runtime"]["agent"]["output_mounts"][0]["kind"] = kind;
        value["trial_runtime"]["agent"]["output_mounts"][0]["kind"] = json!("file");
        assert!(
            schema.validate(&value).is_err(),
            "resolved output mounts should only support directories"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_secret_credential_cache_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "network": { "task_sandbox": "none", "agent": "none" },
                "secrets": [
                    {
                        "name": "codex_oauth",
                        "from": "file",
                        "mount": {
                            "target": "/run/secrets/codex-auth.json",
                            "required_for_variants": ["codex_cli"]
                        },
                        "credential_cache": {
                            "kind": "run_scoped",
                            "target": "/root/.config/nova/codex-auth.json",
                            "env": "CODEX_AUTH_CACHE_FILE"
                        }
                    }
                ]
            },
            "matrix": {
                "variants": [{ "id": "codex_cli", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved credential cache shape should validate: {errors:?}"
        );

        let mut env_with_mount = value.clone();
        env_with_mount["runtime"]["secrets"][0]["from"] = json!("env");
        assert!(
            schema.validate(&env_with_mount).is_err(),
            "resolved from=env secrets must not declare file mounts"
        );

        let mut secret_assignment = value.clone();
        secret_assignment["runtime"]["secrets"][0]["name"] = json!("OPENAI_API_KEY=oops");
        assert!(
            schema.validate(&secret_assignment).is_err(),
            "resolved secret names should reject env assignment syntax"
        );

        secret_assignment["runtime"]["secrets"][0]["name"] = json!("   ");
        assert!(
            schema.validate(&secret_assignment).is_err(),
            "resolved secret names should reject whitespace-only values"
        );

        let mut env_with_cache = value.clone();
        env_with_cache["runtime"]["secrets"][0]["from"] = json!("env");
        env_with_cache["runtime"]["secrets"][0]
            .as_object_mut()
            .expect("secret object")
            .remove("mount");
        assert!(
            schema.validate(&env_with_cache).is_err(),
            "resolved from=env secrets must not declare credential caches"
        );

        let mut file_without_mount = value.clone();
        file_without_mount["runtime"]["secrets"][0]
            .as_object_mut()
            .expect("secret object")
            .remove("mount");
        assert!(
            schema.validate(&file_without_mount).is_err(),
            "resolved from=file secrets must declare a mount target"
        );

        let mut stale_variant_gate = value.clone();
        stale_variant_gate["runtime"]["secrets"][0]["required_for_variants"] = json!(["codex_cli"]);
        assert!(
            schema.validate(&stale_variant_gate).is_err(),
            "required_for_variants belongs under mount only"
        );

        let mut unknown_mount_field = value.clone();
        unknown_mount_field["runtime"]["secrets"][0]["mount"]["mode"] = json!("readonly");
        assert!(
            schema.validate(&unknown_mount_field).is_err(),
            "secret mounts should reject unknown fields"
        );

        value["runtime"]["secrets"][0]["credential_cache"]["kind"] = json!("trial_scoped");
        assert!(
            schema.validate(&value).is_err(),
            "credential_cache.kind should reject unsupported scopes"
        );

        value["runtime"]["secrets"][0]["credential_cache"]["kind"] = json!("run_scoped");
        value["runtime"]["secrets"][0]["credential_cache"]["target"] =
            json!("/workspace/task/auth.json");
        assert!(
            schema.validate(&value).is_err(),
            "credential_cache.target should reject reserved runtime paths"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_sidecar_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "trial_runtime": {
                "task": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "artifact_type": "structured_json",
                    "image": "ghcr.io/acme/agent:latest",
                    "integration_level": "cli_basic",
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json",
                                "required": true
                            }
                        }
                    },
                    "sidecars": ["mcp-bash"]
                },
                "execution": { "agent_site": "agent_container" },
                "grader": { "strategy": "none" }
            },
            "scheduling": minimal_resolved_scheduling(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation(),
            "sidecars": {
                "mcp-bash": {
                    "image": "ghcr.io/acme/mcp-bash-server:v0.4",
                    "lifecycle": "per-trial",
                    "command": ["mcp-bash-server", "--port", "8080"],
                    "workdir": "/srv/mcp",
                    "env": { "LOG_LEVEL": "info" },
                    "expose": { "MCP_URL": "http://mcp-bash:8080" }
                }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved sidecar shape should validate: {errors:?}"
        );

        let mut host_with_sidecar = value.clone();
        host_with_sidecar["trial_runtime"]["execution"] = json!({ "agent_site": "host" });
        assert!(
            schema.validate(&host_with_sidecar).is_err(),
            "resolved host agent execution should reject sidecar attachments"
        );

        let mut host_with_image = value.clone();
        host_with_image["trial_runtime"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("sidecars");
        host_with_image["trial_runtime"]["execution"] = json!({ "agent_site": "host" });
        assert!(
            schema.validate(&host_with_image).is_err(),
            "resolved host agent execution should reject container images"
        );

        let mut task_runtime_with_image = host_with_image.clone();
        task_runtime_with_image["trial_runtime"]["execution"] =
            json!({ "agent_site": "task_runtime" });
        assert!(
            schema.validate(&task_runtime_with_image).is_err(),
            "resolved task_runtime agent execution should reject separate agent images"
        );

        let mut agent_container_without_image = value.clone();
        agent_container_without_image["trial_runtime"]["agent"]
            .as_object_mut()
            .expect("agent object")
            .remove("image");
        assert!(
            schema.validate(&agent_container_without_image).is_err(),
            "resolved agent_container execution should require an agent image"
        );

        value["sidecars"]["mcp-bash"]["restart"] = json!("always");
        assert!(
            schema.validate(&value).is_err(),
            "resolved sidecars should reject unknown fields"
        );

        value["sidecars"]["mcp-bash"]
            .as_object_mut()
            .expect("sidecar object")
            .remove("restart");
        value["trial_runtime"]["agent"]["sidecars"] = json!(["mcp_bash"]);
        assert!(
            schema.validate(&value).is_err(),
            "resolved sidecar attachments should use portable DNS-label syntax"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_grader_strategy_configs() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "trial_runtime": {
                "task": { "interface": "input_only" },
                "agent": {
                    "command": ["agent"],
                    "artifact_type": "structured_json",
                    "integration_level": "cli_basic",
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json",
                                "required": true
                            }
                        }
                    }
                },
                "execution": { "agent_site": "host" },
                "grader": {
                    "strategy": "separate",
                    "command": ["grade"],
                    "max_concurrency": 2,
                    "inputs": {},
                    "outputs": {
                        "report": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/grader-report.json",
                                "format": "json",
                                "required": true
                            }
                        }
                    },
                    "separate": {
                        "image": "ghcr.io/acme/grader:latest",
                        "workdir": "/grader"
                    }
                }
            },
            "scheduling": minimal_resolved_scheduling(),
            "policy": minimal_resolved_policy(),
            "evaluation": minimal_resolved_evaluation()
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved separate grader config should validate: {errors:?}"
        );

        let mut injected_without_workspace = value.clone();
        injected_without_workspace["trial_runtime"]["grader"] = json!({
            "strategy": "injected",
            "command": ["grade"],
            "inputs": {},
            "outputs": {
                "report": {
                    "capture": {
                        "type": "file",
                        "path": "/bucephalus/out/grader-report.json",
                        "format": "json",
                        "required": true
                    }
                }
            },
            "injected": {
                "bundle": "grader_bundle.tar.gz",
                "copy_dest": "/opt/grader"
            }
        });
        assert!(
            schema.validate(&injected_without_workspace).is_err(),
            "resolved injected grader should require a container-image task workspace"
        );

        injected_without_workspace["trial_runtime"]["task"] = json!({
            "interface": "writable_workspace",
            "workspace": { "source": "container_image" }
        });
        assert!(
            schema.validate(&injected_without_workspace).is_ok(),
            "resolved injected grader should accept a container-image task workspace"
        );

        value["trial_runtime"]["grader"]["separate"]["working_dir"] = json!("/grader");
        assert!(
            schema.validate(&value).is_err(),
            "resolved separate grader config should reject unknown fields"
        );

        value["trial_runtime"]["grader"]["separate"]
            .as_object_mut()
            .expect("separate object")
            .remove("working_dir");
        value["trial_runtime"]["grader"]["separate"]["workdir"] = json!("../grader");
        assert!(
            schema.validate(&value).is_err(),
            "resolved separate.workdir should be an absolute container path"
        );

        value["trial_runtime"]["grader"] = json!({
            "strategy": "separate",
            "separate": {
                "image": "ghcr.io/acme/grader:latest",
                "workdir": "/grader"
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved active grader strategies should require command"
        );

        value["trial_runtime"]["grader"] = json!({
            "strategy": "injected",
            "command": ["grade"]
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved injected grader should require injected config"
        );

        value["trial_runtime"]["grader"] = json!({
            "strategy": "host",
            "command": ["grade"],
            "host": { "capability": "official_grader" },
            "sidecars": ["cache"]
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved host grader should reject sidecar attachments"
        );

        value["trial_runtime"]["grader"] = json!({
            "strategy": "none",
            "command": ["grade"]
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved strategy=none grader should reject inert commands"
        );

        value["trial_runtime"]["grader"] = json!({
            "strategy": "none",
            "sidecars": ["cache"]
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved strategy=none grader should reject sidecar attachments"
        );

        value["trial_runtime"]["grader"] = json!({
            "strategy": "separate",
            "command": ["grade"],
            "max_concurrency": 2,
            "inputs": {},
            "outputs": {
                "report": {
                    "capture": {
                        "type": "file",
                        "path": "/bucephalus/out/grader-report.json",
                        "format": "json",
                        "required": true
                    }
                }
            },
            "separate": {
                "image": "ghcr.io/acme/grader:latest",
                "workdir": "/grader"
            }
        });
        value["matrix"]["variants"][0]["overrides"] = json!({
            "grader": {
                "strategy": "none",
                "outputs": {
                    "score": {
                        "capture": {
                            "type": "file",
                            "path": "/bucephalus/out/score.json",
                            "format": "json",
                            "required": true
                        }
                    }
                }
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved strategy=none grader overrides should reject inert output declarations"
        );

        value["matrix"]["variants"][0]["overrides"] = json!({
            "grader": {
                "strategy": "host",
                "sidecars": ["cache"]
            }
        });
        assert!(
            schema.validate(&value).is_err(),
            "resolved host grader overrides should reject sidecar attachments"
        );
    }

    #[test]
    fn resolved_experiment_schema_validates_evaluation_policy_surface() {
        let schema = compile_schema("resolved_experiment.jsonschema").expect("resolved schema");
        let mut value = json!({
            "experiment": { "id": "smoke_eval", "name": "Smoke eval" },
            "runtime": minimal_resolved_runtime(),
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "tasks": {
                    "source": "file",
                    "path": "tasks/tasks.jsonl"
                },
                "repeats": 1
            },
            "scheduling": minimal_resolved_scheduling(),
            "trial_runtime": minimal_resolved_trial_runtime(),
            "policy": minimal_resolved_policy(),
            "evaluation": {
                "policy": {
                    "task_model": "independent",
                    "scoring_lifecycle": "predict_then_score",
                    "chain_failure_policy": "continue_with_flag",
                    "required_evidence_classes": []
                }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            errors.is_empty(),
            "resolved evaluation policy surface should validate: {errors:?}"
        );

        value["evaluation"]["policy"]["required_evidence_classes"] = json!([""]);
        assert!(
            schema.validate(&value).is_err(),
            "resolved evaluation required evidence names should be non-empty"
        );

        value["evaluation"]["policy"]["required_evidence_classes"] = json!([]);
        value["evaluation"]["policy"]["task_policy"] = json!("dependent");
        assert!(
            schema.validate(&value).is_err(),
            "resolved evaluation policy should reject unknown aliases"
        );

        value["evaluation"]["policy"]
            .as_object_mut()
            .expect("policy object")
            .remove("task_policy");
        value["evaluation"]["policy"]["evaluator_mode"] = json!("custom");
        assert!(
            schema.validate(&value).is_err(),
            "resolved evaluation policy should reject removed inert evaluator_mode"
        );
    }

    #[test]
    fn format_validation_error_points_required_fields_at_child_path() {
        let schema =
            compile_schema("experiment_authoring_v1.jsonschema").expect("authoring schema");
        let value = json!({
            "experiment": { "id": "smoke_eval" },
            "matrix": {
                "variants": [{ "id": "baseline", "baseline": true, "config": {} }],
                "cases": { "source": "file" }
            },
            "stages": {
                "case": { "interface": "input_only" },
                "agent": { "command": ["agent"] },
                "grader": { "strategy": "none" }
            }
        });

        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| {
                errors
                    .map(|err| format_validation_error(&err))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        assert!(
            errors
                .iter()
                .any(|err| err == "/matrix/cases/path is required"),
            "required field should point at child path: {errors:?}"
        );
    }
}
