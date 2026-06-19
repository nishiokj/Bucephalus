use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuntimeSidecarHttpReadinessPlan {
    pub(crate) url: String,
    pub(crate) method: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Option<String>,
    pub(crate) json: Option<Value>,
    pub(crate) expect_status: u64,
    pub(crate) interval_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RuntimeSidecarReadinessPlan {
    Command {
        command: Vec<String>,
        timeout_ms: Option<u64>,
    },
    Http {
        http: RuntimeSidecarHttpReadinessPlan,
        timeout_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuntimeSidecarPlan {
    pub(crate) id: String,
    pub(crate) image: String,
    pub(crate) lifecycle: String,
    pub(crate) placement: String,
    pub(crate) command: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) workdir: Option<String>,
    pub(crate) expose: BTreeMap<String, String>,
    pub(crate) readiness: Option<RuntimeSidecarReadinessPlan>,
}

pub(crate) fn sidecar_stage_ids(experiment: &Value, stage: &str) -> Result<Vec<String>> {
    let Some(items) = experiment
        .pointer(&format!("/trial_runtime/{}/sidecars", stage))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                anyhow!("trial_runtime.{}.sidecars[{}] must be a string", stage, idx)
            })
        })
        .collect()
}

fn parse_string_map(value: Option<&Value>, context: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("{} must be an object", context))?;
    object
        .iter()
        .map(|(key, value)| {
            if key.trim().is_empty() {
                return Err(anyhow!("{} contains an empty key", context));
            }
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("{}.{} must be a string", context, key))?;
            Ok((key.clone(), value.to_string()))
        })
        .collect()
}

fn parse_http_readiness(value: &Value, context: &str) -> Result<RuntimeSidecarHttpReadinessPlan> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("{} must be an object", context))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "url" | "method" | "headers" | "body" | "json" | "expect_status" | "interval_ms"
        ) {
            return Err(anyhow!("{}.{} is not supported", context, key));
        }
    }
    let url = object
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("{}.url is required", context))?;
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("GET");
    if object.contains_key("body") && object.contains_key("json") {
        return Err(anyhow!("{} cannot declare both body and json", context));
    }
    let body = object
        .get("body")
        .map(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .ok_or_else(|| anyhow!("{}.body must be a string", context))
        })
        .transpose()?;
    let expect_status = object
        .get("expect_status")
        .and_then(Value::as_u64)
        .unwrap_or(200);
    if !(100..=599).contains(&expect_status) {
        return Err(anyhow!(
            "{}.expect_status must be an HTTP status code",
            context
        ));
    }
    Ok(RuntimeSidecarHttpReadinessPlan {
        url: url.to_string(),
        method: method.to_string(),
        headers: parse_string_map(object.get("headers"), &format!("{}.headers", context))?,
        body,
        json: object.get("json").cloned(),
        expect_status,
        interval_ms: object.get("interval_ms").and_then(Value::as_u64),
    })
}

fn parse_string_array(value: Option<&Value>, context: &str) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("{} must be an argv array", context))?;
    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let part = item
                .as_str()
                .ok_or_else(|| anyhow!("{}[{}] must be a string", context, idx))?;
            if part.trim().is_empty() {
                return Err(anyhow!("{}[{}] must not be empty", context, idx));
            }
            Ok(part.to_string())
        })
        .collect()
}

fn parse_readiness(
    value: Option<&Value>,
    context: &str,
) -> Result<Option<RuntimeSidecarReadinessPlan>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("{} must be an object", context))?;
    for key in object.keys() {
        if !matches!(key.as_str(), "command" | "http" | "timeout_ms") {
            return Err(anyhow!("{}.{} is not supported", context, key));
        }
    }
    let has_command = object.contains_key("command");
    let has_http = object.contains_key("http");
    match (has_command, has_http) {
        (true, true) => {
            return Err(anyhow!(
                "{} must declare exactly one readiness probe: command or http",
                context
            ));
        }
        (false, false) => {
            return Err(anyhow!(
                "{} must declare exactly one readiness probe: command or http",
                context
            ));
        }
        (true, false) => {
            let command =
                parse_string_array(object.get("command"), &format!("{}.command", context))?;
            if command.is_empty() {
                return Err(anyhow!("{}.command is required", context));
            }
            Ok(Some(RuntimeSidecarReadinessPlan::Command {
                command,
                timeout_ms: object.get("timeout_ms").and_then(Value::as_u64),
            }))
        }
        (false, true) => {
            let http = parse_http_readiness(
                object
                    .get("http")
                    .ok_or_else(|| anyhow!("{}.http is required", context))?,
                &format!("{}.http", context),
            )?;
            Ok(Some(RuntimeSidecarReadinessPlan::Http {
                http,
                timeout_ms: object.get("timeout_ms").and_then(Value::as_u64),
            }))
        }
    }
}

pub(crate) fn sidecar_plan(experiment: &Value, id: &str) -> Result<RuntimeSidecarPlan> {
    let config = experiment
        .pointer("/sidecars")
        .and_then(Value::as_object)
        .and_then(|sidecars| sidecars.get(id))
        .ok_or_else(|| anyhow!("sidecar '{}' is referenced but not declared", id))?;
    let config = config
        .as_object()
        .ok_or_else(|| anyhow!("sidecar '{}' config must be an object", id))?;
    for key in config.keys() {
        if !matches!(
            key.as_str(),
            "image"
                | "lifecycle"
                | "placement"
                | "command"
                | "env"
                | "workdir"
                | "expose"
                | "readiness"
        ) {
            return Err(anyhow!("sidecars.{}.{} is not supported", id, key));
        }
    }
    let image = config
        .get("image")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("sidecar '{}' image is required", id))?;
    Ok(RuntimeSidecarPlan {
        id: id.to_string(),
        image: image.to_string(),
        lifecycle: config
            .get("lifecycle")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("sidecar '{}' lifecycle is required", id))?
            .to_string(),
        placement: config
            .get("placement")
            .and_then(Value::as_str)
            .unwrap_or("separate_container")
            .to_string(),
        command: parse_string_array(config.get("command"), &format!("sidecars.{}.command", id))?,
        env: parse_string_map(config.get("env"), &format!("sidecars.{}.env", id))?,
        workdir: config
            .get("workdir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        expose: parse_string_map(config.get("expose"), &format!("sidecars.{}.expose", id))?,
        readiness: parse_readiness(
            config.get("readiness"),
            &format!("sidecars.{}.readiness", id),
        )?,
    })
}

pub(crate) fn sidecar_plans_for_stage(
    experiment: &Value,
    stage: &str,
) -> Result<Vec<RuntimeSidecarPlan>> {
    sidecar_stage_ids(experiment, stage)?
        .into_iter()
        .map(|id| sidecar_plan(experiment, &id))
        .collect()
}

pub(crate) fn trial_sidecar_plans(experiment: &Value) -> Result<Vec<RuntimeSidecarPlan>> {
    let mut ids = Vec::new();
    ids.extend(sidecar_stage_ids(experiment, "agent")?);
    ids.extend(sidecar_stage_ids(experiment, "grader")?);
    let mut seen = BTreeSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    ids.into_iter()
        .map(|id| sidecar_plan(experiment, &id))
        .collect()
}

pub(crate) fn sidecar_env_for_stage(
    experiment: &Value,
    stage: &str,
) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for sidecar in sidecar_plans_for_stage(experiment, stage)? {
        for (key, value) in sidecar.expose {
            if env.insert(key.clone(), value).is_some() {
                return Err(anyhow!(
                    "trial_runtime.{}.sidecars expose duplicate env '{}'",
                    stage,
                    key
                ));
            }
        }
    }
    Ok(env)
}
