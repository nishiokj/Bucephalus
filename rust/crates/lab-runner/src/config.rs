use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use lab_core::{canonical_json_digest, ensure_dir};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};

use crate::model::*;
use crate::persistence::store::SqliteRunStore as BackingSqliteStore;
use crate::trial::spec::parse_task_boundary_from_packaged_task;

pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let ts = Utc::now().timestamp_micros();
    let pid = std::process::id();
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("tmpfile");
    let tmp = path.with_file_name(format!(".{}.tmp.{}.{}", name, pid, ts));
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

pub(crate) fn atomic_write_json_pretty(path: &Path, value: &Value) -> Result<()> {
    crate::package::validate::validate_schema_contract_value(
        value,
        format!("json write {}", path.display()).as_str(),
    )?;
    let bytes = serde_json::to_vec_pretty(value)?;
    atomic_write_bytes(path, &bytes)
}

pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub(crate) fn canonicalize_best_effort(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

pub(crate) fn load_json_file(path: &Path) -> Result<Value> {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let key = match file_name {
        "run_control.json" => Some(RUNTIME_KEY_RUN_CONTROL),
        "run_session_state.json" => Some(RUNTIME_KEY_RUN_SESSION_STATE),
        "schedule_progress.json" => Some(RUNTIME_KEY_SCHEDULE_PROGRESS),
        "engine_lease.json" => Some(RUNTIME_KEY_ENGINE_LEASE),
        _ => None,
    };
    if let Some(key) = key {
        let run_dir = path.parent().and_then(|p| p.parent()).ok_or_else(|| {
            anyhow!(
                "cannot resolve run_dir for runtime key '{}' from {}",
                key,
                path.display()
            )
        })?;
        let store = BackingSqliteStore::open(run_dir)?;
        return store.get_runtime_json(key)?.ok_or_else(|| {
            anyhow!(
                "runtime state '{}' not found in sqlite for {}",
                key,
                run_dir.display()
            )
        });
    }
    if path.exists() {
        let bytes = fs::read(path)?;
        return Ok(serde_json::from_slice(&bytes)?);
    }
    Err(anyhow!("json file not found: {}", path.display()))
}

pub(crate) fn experiment_workload_type(json_value: &Value) -> Result<String> {
    if json_value.pointer("/experiment/workload_type").is_some() {
        return Err(anyhow!(
            "/experiment/workload_type is not supported in v1; remove it"
        ));
    }
    Ok("agent_runtime".to_string())
}

pub(crate) fn experiment_random_seed(json_value: &Value) -> u64 {
    json_value
        .pointer("/scheduling/random_seed")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
}

pub(crate) fn experiment_max_concurrency(json_value: &Value) -> usize {
    let raw = json_value
        .pointer("/scheduling/max_concurrency")
        .or_else(|| json_value.pointer("/runtime/compute/config/max_parallel"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    (raw.max(1)).min(usize::MAX as u64) as usize
}

pub(crate) const DEFAULT_SANITIZATION_PROFILE: &str = "perf_benchmark";

pub(crate) fn configured_sanitization_profile(json_value: &Value) -> Option<&str> {
    json_value
        .pointer("/policy/sanitization_profile")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn effective_sanitization_profile(json_value: &Value) -> &str {
    configured_sanitization_profile(json_value).unwrap_or(DEFAULT_SANITIZATION_PROFILE)
}

pub(crate) fn parse_string_array_field(value: Option<&Value>, field: &str) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut parsed = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                let token = item
                    .as_str()
                    .ok_or_else(|| anyhow!("{}[{}] must be a string", field, idx))?;
                if token.trim().is_empty() {
                    return Err(anyhow!("{}[{}] must not be empty", field, idx));
                }
                parsed.push(token.to_string());
            }
            Ok(parsed)
        }
        Some(_) => Err(anyhow!("{} must be a string[]", field)),
    }
}

pub(crate) fn parse_string_map_field(
    value: Option<&Value>,
    field: &str,
) -> Result<BTreeMap<String, String>> {
    match value {
        None => Ok(BTreeMap::new()),
        Some(Value::Object(map)) => {
            let mut parsed = BTreeMap::new();
            for (key, value) in map {
                if key.trim().is_empty() {
                    return Err(anyhow!("{} contains an empty key", field));
                }
                let as_str = value
                    .as_str()
                    .ok_or_else(|| anyhow!("{}['{}'] must be a string", field, key))?;
                parsed.insert(key.clone(), as_str.to_string());
            }
            Ok(parsed)
        }
        Some(_) => Err(anyhow!("{} must be an object<string,string>", field)),
    }
}

pub(crate) fn parse_optional_nonempty_string(
    value: Option<&Value>,
    field: &str,
) -> Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(_) => Err(anyhow!("{} must be a string", field)),
    }
}

pub fn find_project_root(experiment_dir: &Path) -> PathBuf {
    let mut cur = Some(experiment_dir);
    while let Some(p) = cur {
        if p.file_name().and_then(|s| s.to_str()) == Some(".lab") {
            return p.parent().unwrap_or(experiment_dir).to_path_buf();
        }
        cur = p.parent();
    }
    experiment_dir.to_path_buf()
}

pub(crate) fn parse_policies(json_value: &Value) -> PolicyConfig {
    let default_scheduling = default_scheduling_for_design(json_value);
    let policies = json_value.pointer("/policy/policies");

    let scheduling = match policies
        .and_then(|p| p.pointer("/scheduling"))
        .and_then(|v| v.as_str())
    {
        Some("paired_interleaved") => SchedulingPolicy::PairedInterleaved,
        Some("variant_sequential") => SchedulingPolicy::VariantSequential,
        Some("randomized") => SchedulingPolicy::Randomized,
        _ => default_scheduling,
    };
    let state = parse_state_policy_value(
        policies
            .and_then(|p| p.pointer("/state"))
            .and_then(|v| v.as_str()),
    )
    .unwrap_or(StatePolicy::IsolatePerTrial);
    let retry_max_attempts = policies
        .and_then(|p| p.pointer("/retry/max_attempts"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as usize;
    let retry_on = policies
        .and_then(|p| p.pointer("/retry/retry_on"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let pruning_max_consecutive_failures = policies
        .and_then(|p| p.pointer("/pruning/max_consecutive_failures"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let max_in_flight_per_variant = policies
        .and_then(|p| p.pointer("/concurrency/max_in_flight_per_variant"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let require_chain_lease = policies
        .and_then(|p| p.pointer("/concurrency/require_chain_lease"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    PolicyConfig {
        scheduling,
        state,
        retry_max_attempts,
        retry_on,
        pruning_max_consecutive_failures,
        task_boundary: TaskBoundaryPolicy::default(),
        concurrency: ConcurrencyPolicyConfig {
            max_in_flight_per_variant,
            require_chain_lease,
        },
    }
}

fn default_scheduling_for_design(json_value: &Value) -> SchedulingPolicy {
    match json_value
        .pointer("/scheduling/comparison")
        .and_then(|v| v.as_str())
    {
        Some("paired") => SchedulingPolicy::PairedInterleaved,
        _ => SchedulingPolicy::VariantSequential,
    }
}

pub(crate) fn parse_task_model(value: Option<&str>) -> TaskModel {
    match value {
        Some("dependent") => TaskModel::Dependent,
        _ => TaskModel::Independent,
    }
}

pub(crate) fn parse_state_policy_value(value: Option<&str>) -> Option<StatePolicy> {
    match value {
        Some("isolate_per_trial") => Some(StatePolicy::IsolatePerTrial),
        Some("persist_per_task") => Some(StatePolicy::PersistPerTask),
        Some("accumulate") => Some(StatePolicy::Accumulate),
        _ => None,
    }
}

pub(crate) fn parse_benchmark_config(json_value: &Value) -> Result<BenchmarkConfig> {
    let benchmark_root = json_value.pointer("/benchmark");
    let trial_grader_root = json_value.pointer("/trial_runtime/grader");
    let root = benchmark_root;

    let policy = root.and_then(|value| value.pointer("/policy"));
    let mut policy_config = BenchmarkPolicyConfig::default();
    if let Some(p) = policy {
        policy_config.task_model =
            parse_task_model(p.pointer("/task_model").and_then(|v| v.as_str()));
        if let Some(v) = p.pointer("/scoring_lifecycle").and_then(|v| v.as_str()) {
            policy_config.scoring_lifecycle = v.to_string();
        }
        if let Some(v) = p.pointer("/evaluator_mode").and_then(|v| v.as_str()) {
            policy_config.evaluator_mode = v.to_string();
        }
        if let Some(v) = p.pointer("/chain_failure_policy").and_then(|v| v.as_str()) {
            policy_config.chain_failure_policy = v.to_string();
        }
        if let Some(arr) = p
            .pointer("/required_evidence_classes")
            .and_then(|v| v.as_array())
        {
            policy_config.required_evidence_classes = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
    }

    let grader = match trial_grader_root {
        Some(g) => parse_benchmark_grader_config(g, "/trial_runtime/grader")?,
        None => None,
    };

    Ok(BenchmarkConfig {
        policy: policy_config,
        grader,
    })
}

fn parse_benchmark_grader_config(g: &Value, field: &str) -> Result<Option<BenchmarkGraderConfig>> {
    if !g.is_object() {
        return Err(anyhow!("{} must be an object", field));
    }

    let strategy = match g.pointer("/strategy").and_then(Value::as_str) {
        Some(raw) => match raw.trim() {
            "none" => GradingStrategy::None,
            "in_task_runtime" => GradingStrategy::InTaskRuntime,
            "injected" => GradingStrategy::Injected,
            "separate" => GradingStrategy::Separate,
            "host" => GradingStrategy::Host,
            other => {
                return Err(anyhow!(
                    "{}.strategy must be one of: none, in_task_runtime, injected, separate, host (got '{}')",
                    field,
                    other
                ))
            }
        },
        None => return Err(anyhow!("{}.strategy is required", field)),
    };

    let command = parse_string_array_field(g.pointer("/command"), &format!("{}.command", field))?;
    if matches!(strategy, GradingStrategy::None) {
        if !command.is_empty() {
            return Err(anyhow!(
                "{}.command must be omitted when strategy=none",
                field
            ));
        }
        if g.pointer("/inputs").is_some() || g.pointer("/outputs").is_some() {
            return Err(anyhow!(
                "{} inputs/outputs must be omitted when strategy=none",
                field
            ));
        }
        return Ok(None);
    }

    if command.is_empty() {
        return Err(anyhow!(
            "{}.command is required when strategy={}",
            field,
            grader_strategy_name(&strategy)
        ));
    }

    let max_concurrency = g
        .pointer("/max_concurrency")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    if max_concurrency == Some(0) {
        return Err(anyhow!("{}.max_concurrency must be at least 1", field));
    }
    let in_task_runtime = g
        .pointer("/in_task_runtime")
        .map(|value| {
            Ok::<InTaskRuntimeGradingConfig, anyhow::Error>(InTaskRuntimeGradingConfig {
                hidden_paths: parse_string_array_field(
                    value.get("hidden_paths"),
                    &format!("{}.in_task_runtime.hidden_paths", field),
                )?,
                revealed_paths: parse_string_array_field(
                    value.get("revealed_paths"),
                    &format!("{}.in_task_runtime.revealed_paths", field),
                )?,
            })
        })
        .transpose()?;
    let injected = match strategy {
        GradingStrategy::Injected => Some(InjectedGradingConfig {
            bundle: parse_optional_nonempty_string(
                g.pointer("/injected/bundle"),
                &format!("{}.injected.bundle", field),
            )?
            .ok_or_else(|| {
                anyhow!(
                    "{}.injected.bundle is required when strategy=injected",
                    field
                )
            })?,
            copy_dest: parse_optional_nonempty_string(
                g.pointer("/injected/copy_dest"),
                &format!("{}.injected.copy_dest", field),
            )?
            .ok_or_else(|| {
                anyhow!(
                    "{}.injected.copy_dest is required when strategy=injected",
                    field
                )
            })?,
        }),
        _ => None,
    };
    let separate = match strategy {
        GradingStrategy::Separate => Some(SeparateGradingConfig {
            image: parse_optional_nonempty_string(
                g.pointer("/separate/image"),
                &format!("{}.separate.image", field),
            )?
            .ok_or_else(|| {
                anyhow!(
                    "{}.separate.image is required when strategy=separate",
                    field
                )
            })?,
            workdir: parse_optional_nonempty_string(
                g.pointer("/separate/workdir"),
                &format!("{}.separate.workdir", field),
            )?
            .ok_or_else(|| {
                anyhow!(
                    "{}.separate.workdir is required when strategy=separate",
                    field
                )
            })?,
        }),
        _ => None,
    };
    let host = match strategy {
        GradingStrategy::Host => Some(HostGradingConfig {
            capability: parse_optional_nonempty_string(
                g.pointer("/host/capability"),
                &format!("{}.host.capability", field),
            )?
            .ok_or_else(|| anyhow!("{}.host.capability is required when strategy=host", field))?,
        }),
        _ => None,
    };
    let inputs = g
        .pointer("/inputs")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .with_context(|| format!("invalid {}.inputs", field))?
        .unwrap_or_default();
    let outputs = g
        .pointer("/outputs")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .with_context(|| format!("invalid {}.outputs", field))?
        .unwrap_or_default();

    let is_in_task_runtime = matches!(strategy, GradingStrategy::InTaskRuntime);
    Ok(Some(BenchmarkGraderConfig {
        strategy,
        command,
        max_concurrency,
        in_task_runtime: if is_in_task_runtime {
            Some(in_task_runtime.unwrap_or_default())
        } else {
            in_task_runtime
        },
        injected,
        separate,
        host,
        inputs,
        outputs,
    }))
}

fn grader_strategy_name(strategy: &GradingStrategy) -> &'static str {
    match strategy {
        GradingStrategy::None => "none",
        GradingStrategy::InTaskRuntime => "in_task_runtime",
        GradingStrategy::Injected => "injected",
        GradingStrategy::Separate => "separate",
        GradingStrategy::Host => "host",
    }
}

fn parse_metric_source(value: &Value, field: &str) -> Result<MetricSourceConfig> {
    let source = value
        .get("source")
        .ok_or_else(|| anyhow!("{} source is required", field))?;
    if !source.is_object() {
        return Err(anyhow!("{} source must be an object", field));
    }
    let source_type = source
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| anyhow!("{} source.type is required", field))?;
    let pointer = source
        .get("pointer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_string);
    if !matches!(
        source_type,
        "agent_response" | "grader_output" | "runtime_output"
    ) {
        return Err(anyhow!(
            "{} source.type '{}' is not supported",
            field,
            source_type
        ));
    }
    if source_type == "agent_response" && pointer.is_none() {
        return Err(anyhow!(
            "{} source.pointer is required when source.type is agent_response",
            field
        ));
    }
    if matches!(source_type, "grader_output" | "runtime_output")
        && source
            .get("output")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .is_none()
    {
        return Err(anyhow!(
            "{} source.output is required when source.type is {}",
            field,
            source_type
        ));
    }

    Ok(MetricSourceConfig {
        source_type: source_type.to_string(),
        pointer,
    })
}

pub(crate) fn parse_metric_definitions(json_value: &Value) -> Result<Vec<MetricDefinition>> {
    let Some(items) = json_value.pointer("/metrics").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let field = format!("metrics[{}]", idx);
            if !item.is_object() {
                return Err(anyhow!("{} must be an object", field));
            }
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|raw| !raw.is_empty())
                .ok_or_else(|| anyhow!("{} id is required", field))?
                .to_string();
            let parse_string = |key: &str| {
                item.get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|raw| !raw.is_empty())
                    .map(str::to_string)
            };
            Ok(MetricDefinition {
                id,
                label: parse_string("label"),
                semantic_key: parse_string("semantic_key"),
                value_type: parse_string("value_type"),
                unit: parse_string("unit"),
                direction: parse_string("direction"),
                source: parse_metric_source(item, &field)?,
                required: item
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                primary: item
                    .get("primary")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                definition_json: item.clone(),
            })
        })
        .collect()
}

pub(crate) fn resolve_effective_task_policy(
    experiment_policy: &PolicyConfig,
    benchmark_policy: &BenchmarkPolicyConfig,
    task_payload: &Value,
) -> EffectiveTaskPolicy {
    let override_obj = task_payload
        .get("policy_override")
        .and_then(|v| v.as_object());

    let state_override = override_obj
        .and_then(|o| o.get("state_policy"))
        .and_then(|v| v.as_str())
        .and_then(|s| parse_state_policy_value(Some(s)));
    let task_model_override = override_obj
        .and_then(|o| o.get("task_model"))
        .and_then(|v| v.as_str())
        .map(|s| parse_task_model(Some(s)));
    let scoring_lifecycle_override = override_obj
        .and_then(|o| o.get("scoring_lifecycle"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let chain_failure_override = override_obj
        .and_then(|o| o.get("chain_failure_policy"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let required_evidence_override = override_obj
        .and_then(|o| o.get("required_evidence_classes"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        });

    EffectiveTaskPolicy {
        state_policy: state_override.unwrap_or(experiment_policy.state),
        task_model: task_model_override.unwrap_or(benchmark_policy.task_model),
        scoring_lifecycle: scoring_lifecycle_override
            .unwrap_or_else(|| benchmark_policy.scoring_lifecycle.clone()),
        required_evidence_classes: required_evidence_override
            .unwrap_or_else(|| benchmark_policy.required_evidence_classes.clone()),
        chain_failure_policy: chain_failure_override
            .unwrap_or_else(|| benchmark_policy.chain_failure_policy.clone()),
    }
}

pub(crate) fn validate_required_evidence_classes(
    record: &Value,
    required: &[String],
) -> Result<()> {
    if required.is_empty() {
        return Ok(());
    }
    for class_name in required {
        let pointer = format!("/evidence/{}", class_name);
        let value = record.pointer(&pointer);
        let missing = match value {
            None => true,
            Some(Value::Null) => true,
            Some(Value::String(s)) => s.trim().is_empty(),
            _ => false,
        };
        if missing {
            return Err(anyhow!(
                "missing required evidence class '{}'; pointer {}",
                class_name,
                pointer
            ));
        }
    }
    Ok(())
}

pub(crate) fn build_trial_schedule(
    variant_count: usize,
    task_count: usize,
    replications: usize,
    policy: SchedulingPolicy,
    random_seed: u64,
) -> Vec<TrialSlot> {
    let mut slots = Vec::with_capacity(variant_count * task_count * replications);

    match policy {
        SchedulingPolicy::VariantSequential => {
            for v in 0..variant_count {
                for t in 0..task_count {
                    for r in 0..replications {
                        slots.push(TrialSlot {
                            variant_idx: v,
                            task_idx: t,
                            repl_idx: r,
                        });
                    }
                }
            }
        }
        SchedulingPolicy::PairedInterleaved => {
            for t in 0..task_count {
                for r in 0..replications {
                    for v in 0..variant_count {
                        slots.push(TrialSlot {
                            variant_idx: v,
                            task_idx: t,
                            repl_idx: r,
                        });
                    }
                }
            }
        }
        SchedulingPolicy::Randomized => {
            for v in 0..variant_count {
                for t in 0..task_count {
                    for r in 0..replications {
                        slots.push(TrialSlot {
                            variant_idx: v,
                            task_idx: t,
                            repl_idx: r,
                        });
                    }
                }
            }
            let mut rng_state: u64 = random_seed;
            for i in (1..slots.len()).rev() {
                rng_state = rng_state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let j = (rng_state >> 33) as usize % (i + 1);
                slots.swap(i, j);
            }
        }
    }

    slots
}

pub(crate) fn resolved_variants_path(run_dir: &Path) -> PathBuf {
    run_dir.join("resolved_variants.json")
}

pub(crate) fn resolved_schedule_path(run_dir: &Path) -> PathBuf {
    run_dir.join("resolved_schedule.json")
}

pub(crate) fn write_resolved_variants(
    run_dir: &Path,
    experiment: &Value,
    baseline_id: &str,
    variants: &[Variant],
) -> Result<()> {
    let variants = variants
        .iter()
        .map(|variant| resolved_variant_manifest_entry(experiment, variant))
        .collect::<Result<Vec<_>>>()?;
    let manifest = ResolvedVariantsManifest {
        schema_version: "resolved_variants_v1".to_string(),
        generated_at: Utc::now().to_rfc3339(),
        baseline_id: baseline_id.to_string(),
        variants,
    };
    let value = serde_json::to_value(&manifest)?;
    atomic_write_json_pretty(&resolved_variants_path(run_dir), &value)
}

pub(crate) fn write_resolved_schedule(run_dir: &Path, schedule: &[TrialSlot]) -> Result<()> {
    let manifest = ResolvedScheduleManifest {
        schema_version: "resolved_schedule_v1".to_string(),
        generated_at: Utc::now().to_rfc3339(),
        total_slots: schedule.len(),
        schedule: schedule.to_vec(),
    };
    let value = serde_json::to_value(&manifest)?;
    atomic_write_json_pretty(&resolved_schedule_path(run_dir), &value)
}

pub(crate) fn load_run_variants(
    run_dir: &Path,
    experiment: &Value,
) -> Result<(Vec<Variant>, String)> {
    let manifest_path = resolved_variants_path(run_dir);
    if !manifest_path.exists() {
        return resolve_variant_plan(experiment);
    }

    let manifest: ResolvedVariantsManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    if manifest.schema_version != "resolved_variants_v1" {
        return Err(anyhow!(
            "unsupported resolved variants schema_version in {}: {}",
            manifest_path.display(),
            manifest.schema_version
        ));
    }
    if manifest.variants.is_empty() {
        return Err(anyhow!(
            "resolved variants manifest has no variants: {}",
            manifest_path.display()
        ));
    }
    if !manifest
        .variants
        .iter()
        .any(|variant| variant.variant.id == manifest.baseline_id)
    {
        return Err(anyhow!(
            "resolved variants manifest baseline '{}' not found in variants: {}",
            manifest.baseline_id,
            manifest_path.display()
        ));
    }
    Ok((
        manifest
            .variants
            .into_iter()
            .map(|variant| variant.variant)
            .collect(),
        manifest.baseline_id,
    ))
}

pub(crate) fn should_retry_outcome(outcome: &str, exit_status: &str, retry_on: &[String]) -> bool {
    if retry_on.is_empty() {
        return outcome == "error" || exit_status != "0";
    }
    for trigger in retry_on {
        match trigger.as_str() {
            "error" if outcome == "error" => return true,
            "failure" if exit_status != "0" => return true,
            "timeout" if outcome == "timeout" => return true,
            _ => {}
        }
    }
    false
}

pub(crate) fn benchmark_verdict_to_trial_outcome(verdict: &str) -> Option<&'static str> {
    match verdict {
        "pass" => Some("success"),
        "fail" => Some("failure"),
        "missing" => Some("missing"),
        "error" => Some("error"),
        _ => None,
    }
}

pub(crate) fn trial_conclusion_outcome_to_trial_outcome(outcome: &str) -> Option<&'static str> {
    match outcome {
        "success" => Some("success"),
        "failure" => Some("failure"),
        "missing" => Some("missing"),
        "error" => Some("error"),
        "timeout" => Some("timeout"),
        other => benchmark_verdict_to_trial_outcome(other),
    }
}

pub(crate) fn variant_bindings_for_summary(variant: &Variant) -> Value {
    if !variant.args.is_empty() || !variant.env.is_empty() || variant.image.is_some() {
        return json!({
            "args": variant.args,
            "env": variant.env,
            "image": variant.image,
        });
    }
    variant.bindings.clone()
}

pub(crate) fn variant_digest(variant: &Variant) -> Result<String> {
    let value = serde_json::to_value(variant)?;
    Ok(canonical_json_digest(&value))
}

pub(crate) fn resolved_variant_behavior_surface(
    experiment: &Value,
    variant: &Variant,
) -> Result<Value> {
    let mut trial_runtime = experiment
        .pointer("/trial_runtime")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !trial_runtime.is_object() {
        return Err(anyhow!(
            "invalid /trial_runtime in resolved experiment: expected object"
        ));
    }
    if let Some(runtime_overrides) = variant.runtime_overrides.as_ref() {
        if !runtime_overrides.is_object() {
            return Err(anyhow!(
                "variant '{}' runtime_overrides must be an object",
                variant.id
            ));
        }
        merge_json_value(&mut trial_runtime, runtime_overrides);
    }
    Ok(json!({
        "bindings": variant.bindings.clone(),
        "args": variant.args.clone(),
        "env": variant.env.clone(),
        "image": variant.image.clone(),
        "trial_runtime": trial_runtime,
    }))
}

pub(crate) fn resolved_variant_behavior_digest(
    experiment: &Value,
    variant: &Variant,
) -> Result<String> {
    Ok(canonical_json_digest(&resolved_variant_behavior_surface(
        experiment, variant,
    )?))
}

fn resolved_variant_manifest_entry(
    experiment: &Value,
    variant: &Variant,
) -> Result<ResolvedVariant> {
    Ok(ResolvedVariant {
        variant_digest: resolved_variant_behavior_digest(experiment, variant)?,
        variant: variant.clone(),
    })
}

pub(crate) fn resolve_variant_plan(json_value: &Value) -> Result<(Vec<Variant>, String)> {
    let variant_list = json_value
        .pointer("/matrix/variants")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing /matrix/variants"))?;
    if variant_list.is_empty() {
        return Err(anyhow!(
            "/matrix/variants must include at least one variant"
        ));
    }
    let mut variants = Vec::with_capacity(variant_list.len());
    let mut baseline_id = None;
    for (idx, item) in variant_list.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("/matrix/variants[{}] must include non-empty string id", idx))?
            .to_string();
        if item.get("bindings").is_some() {
            return Err(anyhow!(
                "/matrix/variants[{}]/bindings is not supported in v1; move to /matrix/variants[].config",
                idx
            ));
        }
        let config = item.get("config").cloned().unwrap_or(json!({}));
        if !config.is_object() {
            return Err(anyhow!(
                "/matrix/variants[{}].config must be an object",
                idx
            ));
        }
        let runtime_overrides = match item.get("overrides") {
            None | Some(Value::Null) => None,
            Some(Value::Object(_)) => item.get("overrides").cloned(),
            Some(_) => {
                return Err(anyhow!(
                    "/matrix/variants[{}].overrides must be an object",
                    idx
                ))
            }
        };
        if item.get("runtime_overrides").is_some() {
            return Err(anyhow!(
                "/matrix/variants[{}]/runtime_overrides is not supported in v1; move to /matrix/variants[].overrides",
                idx
            ));
        }
        if item.get("image").is_some() {
            return Err(anyhow!(
                "/matrix/variants[{}]/image is not supported in v1; use /matrix/variants[].overrides.agent.image",
                idx
            ));
        }
        if item
            .get("baseline")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if baseline_id.replace(id.clone()).is_some() {
                return Err(anyhow!(
                    "exactly one /matrix/variants[].baseline=true is required"
                ));
            }
        }
        variants.push(Variant {
            id,
            bindings: config,
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides,
        });
    }
    let baseline = baseline_id
        .or_else(|| (variants.len() == 1).then(|| variants[0].id.clone()))
        .ok_or_else(|| anyhow!("exactly one /matrix/variants[].baseline=true is required"))?;
    Ok((variants, baseline))
}

pub(crate) fn merge_json_value(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(base_map), Value::Object(patch_map)) => {
            for (key, patch_value) in patch_map {
                if let Some(base_value) = base_map.get_mut(key) {
                    merge_json_value(base_value, patch_value);
                } else {
                    base_map.insert(key.clone(), patch_value.clone());
                }
            }
        }
        (base_slot, patch_value) => {
            *base_slot = patch_value.clone();
        }
    }
}

pub(crate) fn value_matches_type(value: &Value, t: &str) -> bool {
    match t {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

pub(crate) fn value_type_name(value: &Value) -> &'static str {
    if value.is_string() {
        "string"
    } else if value.is_boolean() {
        "boolean"
    } else if value.is_number() {
        "number"
    } else if value.is_array() {
        "array"
    } else if value.is_object() {
        "object"
    } else {
        "null"
    }
}

fn decode_pointer_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

pub(crate) fn set_json_pointer_value(
    root: &mut Value,
    pointer: &str,
    new_value: Value,
) -> Result<()> {
    if pointer.is_empty() || pointer == "/" {
        *root = new_value;
        return Ok(());
    }
    if !pointer.starts_with('/') {
        return Err(anyhow!("json_pointer must start with '/': {}", pointer));
    }

    let tokens: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(decode_pointer_token)
        .collect();
    if tokens.is_empty() {
        *root = new_value;
        return Ok(());
    }

    let mut cur = root;
    for token in tokens.iter().take(tokens.len() - 1) {
        match cur {
            Value::Object(map) => {
                let entry = map.entry(token.clone()).or_insert_with(|| json!({}));
                cur = entry;
            }
            Value::Array(arr) => {
                let idx: usize = token.parse().map_err(|_| {
                    anyhow!(
                        "json_pointer token '{}' is not a valid array index in {}",
                        token,
                        pointer
                    )
                })?;
                if idx >= arr.len() {
                    return Err(anyhow!(
                        "json_pointer array index {} out of bounds in {}",
                        idx,
                        pointer
                    ));
                }
                cur = &mut arr[idx];
            }
            _ => {
                return Err(anyhow!(
                    "json_pointer traversal hit non-container at token '{}' in {}",
                    token,
                    pointer
                ));
            }
        }
    }

    let last = tokens.last().unwrap();
    match cur {
        Value::Object(map) => {
            map.insert(last.clone(), new_value);
            Ok(())
        }
        Value::Array(arr) => {
            let idx: usize = last.parse().map_err(|_| {
                anyhow!(
                    "json_pointer token '{}' is not a valid array index in {}",
                    last,
                    pointer
                )
            })?;
            if idx >= arr.len() {
                return Err(anyhow!(
                    "json_pointer array index {} out of bounds in {}",
                    idx,
                    pointer
                ));
            }
            arr[idx] = new_value;
            Ok(())
        }
        _ => Err(anyhow!(
            "json_pointer target is not an object/array for {}",
            pointer
        )),
    }
}

pub(crate) fn resolve_runtime_for_variant(experiment: &Value, variant: &Variant) -> Result<Value> {
    let mut resolved = experiment.clone();
    let Some(runtime_overrides) = variant.runtime_overrides.as_ref() else {
        return Ok(resolved);
    };

    let mut trial_runtime = resolved
        .pointer("/trial_runtime")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !trial_runtime.is_object() {
        return Err(anyhow!("invalid /trial_runtime: expected object"));
    }
    merge_json_value(&mut trial_runtime, runtime_overrides);
    set_json_pointer_value(&mut resolved, "/trial_runtime", trial_runtime)?;
    Ok(resolved)
}

pub(crate) fn find_variant_by_id<'a>(
    variants: &'a [Variant],
    variant_id: &str,
) -> Result<&'a Variant> {
    let trimmed = variant_id.trim();
    if trimmed.is_empty() {
        return variants
            .first()
            .ok_or_else(|| anyhow!("no variants available in experiment"));
    }
    variants
        .iter()
        .find(|variant| variant.id == trimmed)
        .ok_or_else(|| anyhow!("variant '{}' not found in experiment", trimmed))
}

pub(crate) fn apply_experiment_overrides(
    mut experiment: Value,
    overrides_path: &Path,
    project_root: &Path,
) -> Result<Value> {
    let overrides = crate::package::validate::load_experiment_overrides(overrides_path)?;
    if overrides.values.is_empty() {
        return Ok(experiment);
    }

    let manifest_rel = overrides
        .manifest_path
        .clone()
        .unwrap_or_else(|| ".lab/knobs/manifest.json".to_string());
    let manifest_path = if Path::new(&manifest_rel).is_absolute() {
        PathBuf::from(&manifest_rel)
    } else {
        project_root.join(&manifest_rel)
    };
    let manifest = crate::package::validate::load_knob_manifest(&manifest_path)?;

    let mut by_id: BTreeMap<String, KnobDef> = BTreeMap::new();
    for knob in manifest.knobs {
        by_id.insert(knob.id.clone(), knob);
    }

    for (id, value) in overrides.values.iter() {
        let knob = by_id
            .get(id)
            .ok_or_else(|| anyhow!("override references unknown knob id: {}", id))?;
        crate::package::validate::validate_knob_value(knob, value)?;
        set_json_pointer_value(&mut experiment, &knob.json_pointer, value.clone())?;
    }

    Ok(experiment)
}

pub(crate) fn validate_dataset_provider(json_value: &Value) -> Result<()> {
    match json_value.pointer("/matrix/tasks/source") {
        Some(Value::String(source)) if source == "file" => Ok(()),
        Some(Value::String(source)) => Err(anyhow!(
            "matrix.tasks.source='{}' is not supported; use source: file",
            source
        )),
        Some(Value::Object(obj)) => match obj.get("type").and_then(Value::as_str) {
            Some("file") => Ok(()),
            Some(other) => Err(anyhow!(
                "matrix.tasks.source.type='{}' is not supported; use type: file",
                other
            )),
            None => Err(anyhow!("matrix.tasks.source.type is required")),
        },
        Some(_) => Err(anyhow!("matrix.tasks.source must be 'file' or an object")),
        None => Err(anyhow!("missing /matrix/tasks/source")),
    }
}

pub(crate) fn load_tasks(path: &Path, json_value: &Value) -> Result<Vec<Value>> {
    validate_dataset_provider(json_value)?;
    let limit = json_value
        .pointer("/matrix/tasks/limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).with_context(|| {
        format!(
            "failed to open dataset file '{}' referenced by matrix.tasks.path during build",
            path.display()
        )
    })?;
    let reader = BufReader::new(file);
    let mut tasks = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if limit.is_some_and(|max| tasks.len() >= max) {
            break;
        }
        let task: Value = serde_json::from_str(trimmed)?;
        let task_id = task
            .pointer("/task/id")
            .or_else(|| task.pointer("/id"))
            .and_then(Value::as_str)
            .unwrap_or("<unknown_task>");
        if parse_task_boundary_from_packaged_task(&task).is_err() {
            return Err(anyhow!(
                "dataset row {} task '{}' is not a valid packaged task_row_v2 or task_case_v1",
                idx + 1,
                task_id
            ));
        }
        tasks.push(task);
    }
    Ok(tasks)
}
