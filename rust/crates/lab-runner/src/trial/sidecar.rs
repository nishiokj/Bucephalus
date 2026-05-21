use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeSidecarPlan {
    pub(crate) id: String,
    pub(crate) image: String,
    pub(crate) lifecycle: String,
    pub(crate) command: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) workdir: Option<String>,
    pub(crate) expose: BTreeMap<String, String>,
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
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("{}.{} must be a string", context, key))?;
            Ok((key.clone(), value.to_string()))
        })
        .collect()
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
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("{}[{}] must be a string", context, idx))
        })
        .collect()
}

pub(crate) fn sidecar_plan(experiment: &Value, id: &str) -> Result<RuntimeSidecarPlan> {
    let config = experiment
        .pointer("/sidecars")
        .and_then(Value::as_object)
        .and_then(|sidecars| sidecars.get(id))
        .ok_or_else(|| anyhow!("sidecar '{}' is referenced but not declared", id))?;
    let image = config
        .pointer("/image")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("sidecar '{}' image is required", id))?;
    Ok(RuntimeSidecarPlan {
        id: id.to_string(),
        image: image.to_string(),
        lifecycle: config
            .pointer("/lifecycle")
            .and_then(Value::as_str)
            .unwrap_or("per-trial")
            .to_string(),
        command: parse_string_array(
            config.pointer("/command"),
            &format!("sidecars.{}.command", id),
        )?,
        env: parse_string_map(config.pointer("/env"), &format!("sidecars.{}.env", id))?,
        workdir: config
            .pointer("/workdir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        expose: parse_string_map(
            config.pointer("/expose"),
            &format!("sidecars.{}.expose", id),
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
