use anyhow::{anyhow, Result};
use chrono::Utc;
use lab_core::{
    ensure_dir, AGENTLAB_CONTRACT_IN_DIR, AGENTLAB_CONTRACT_RUNTIME_AUX_DIR,
    AGENTLAB_RUNNER_SUPPORT_REL_DIR, AGENTLAB_TASK_WORKDIR_PLACEHOLDER,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Instant;

use crate::config::*;
use crate::experiment::commit::load_optional_json_record_with_schema;
use crate::experiment::runtime::*;
use crate::experiment::state::{RunBehavior, RunExecutionOptions};
use crate::image::{
    ImageRequirement, ImageRequirementRole, ImageResolutionMode, ImageResolveRequest,
    ImageResolver, ImageResolverChain, ReferenceOnlyImageResolver, ScopedImageResolverCache,
};
use crate::model::*;
use crate::package::sealed::*;
use crate::package::validate::*;
use crate::trial::env::resolve_host_grader_command;
use crate::trial::execution::{
    AdapterRunRequest, ExecutionBackend, LocalDockerExecutionBackend, TrialRuntimeExecutionRequest,
};
use crate::trial::grade::task_grading_enabled;
use crate::trial::layout::{trial_agent_stderr_path, trial_agent_stdout_path};
use crate::trial::plan::{
    parse_trial_runtime_config, validate_task_boundary_for_trial_runtime, AgentSite, TaskInterface,
    WorkspaceSource,
};
use crate::trial::prepare::{
    build_runtime_contract_env, load_prepared_task_environment_manifest, prepare_io_paths,
    prepare_task_environment, resolve_trial_timeout_ms, TrialPaths,
};
use crate::trial::spec::{
    parse_task_boundary_from_packaged_task, TaskBoundaryMaterialization, TaskMaterializationKind,
    TaskMaterializationSpec,
};
use crate::util::sanitize_for_fs;

pub(crate) fn parse_bool_env(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub(crate) fn progress_logs_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if let Ok(value) = env::var(AGENTLAB_PROGRESS_LOG_ENV) {
            if let Some(parsed) = parse_bool_env(&value) {
                return parsed;
            }
        }
        !cfg!(test)
    })
}

pub(crate) fn emit_progress_log(scope: &str, message: impl AsRef<str>) {
    if !progress_logs_enabled() {
        return;
    }
    eprintln!("[{}] {}", scope, message.as_ref());
    let _ = std::io::stderr().flush();
}

pub(crate) fn emit_preflight_log(message: impl AsRef<str>) {
    emit_progress_log("preflight", message);
}

pub(crate) fn emit_run_log(run_id: &str, message: impl AsRef<str>) {
    emit_progress_log("run", format!("{}: {}", run_id, message.as_ref()));
}

pub(crate) fn should_emit_image_probe_progress(index: usize, total: usize) -> bool {
    if total <= 5 {
        return true;
    }
    index == 1 || index == total || index % 5 == 0
}

pub(crate) fn parse_parallelism(raw: &str) -> Option<usize> {
    raw.trim().parse::<usize>().ok().and_then(|value| {
        if value == 0 {
            None
        } else {
            Some(value.min(MAX_PREFLIGHT_IMAGE_PROBE_PARALLELISM))
        }
    })
}

pub(crate) fn preflight_image_probe_parallelism() -> usize {
    match env::var(AGENTLAB_PREFLIGHT_IMAGE_PROBE_PARALLELISM_ENV) {
        Ok(raw) => parse_parallelism(&raw).unwrap_or(DEFAULT_PREFLIGHT_IMAGE_PROBE_PARALLELISM),
        Err(_) => DEFAULT_PREFLIGHT_IMAGE_PROBE_PARALLELISM,
    }
}

pub(crate) fn preflight_max_unique_images() -> Result<Option<usize>> {
    match env::var(AGENTLAB_MAX_PREFLIGHT_IMAGES_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = trimmed.parse::<usize>().map_err(|_| {
                anyhow!(
                    "{} must be a positive integer when set (got: {})",
                    AGENTLAB_MAX_PREFLIGHT_IMAGES_ENV,
                    raw
                )
            })?;
            if parsed == 0 {
                return Err(anyhow!(
                    "{} must be > 0 when set",
                    AGENTLAB_MAX_PREFLIGHT_IMAGES_ENV
                ));
            }
            Ok(Some(parsed))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(anyhow!(
            "failed reading {}: {}",
            AGENTLAB_MAX_PREFLIGHT_IMAGES_ENV,
            err
        )),
    }
}

pub(crate) fn run_bounded_image_probes<T, F>(images: &[String], label: &str, probe: F) -> Vec<T>
where
    T: Send,
    F: Fn(usize, &str) -> T + Sync,
{
    if images.is_empty() {
        return Vec::new();
    }
    let configured = preflight_image_probe_parallelism();
    let parallelism = configured.min(images.len()).max(1);
    if parallelism <= 1 || images.len() <= 1 {
        return images
            .iter()
            .enumerate()
            .map(|(idx, image)| probe(idx, image))
            .collect();
    }
    emit_preflight_log(format!(
        "{}: bounded probe parallelism={}",
        label, parallelism
    ));
    let next_index = AtomicUsize::new(0);
    let results = Mutex::new(
        std::iter::repeat_with(|| None)
            .take(images.len())
            .collect::<Vec<Option<T>>>(),
    );
    thread::scope(|scope| {
        for _ in 0..parallelism {
            let results_ref = &results;
            let next_index_ref = &next_index;
            let probe_ref = &probe;
            scope.spawn(move || loop {
                let idx = next_index_ref.fetch_add(1, Ordering::SeqCst);
                if idx >= images.len() {
                    break;
                }
                let result = probe_ref(idx, &images[idx]);
                let mut guard = results_ref
                    .lock()
                    .expect("preflight image probe results lock poisoned");
                guard[idx] = Some(result);
            });
        }
    });
    let collected = results
        .into_inner()
        .expect("preflight image probe results lock poisoned");
    collected
        .into_iter()
        .map(|entry| entry.expect("preflight image probe result missing"))
        .collect()
}

pub fn preflight_experiment(path: &Path) -> Result<PreflightReport> {
    preflight_experiment_with_options(path, &RunExecutionOptions::default())
}

pub fn preflight_experiment_with_options(
    path: &Path,
    execution: &RunExecutionOptions,
) -> Result<PreflightReport> {
    emit_preflight_log(format!("resolving sealed package {}", path.display()));
    let preflight_started = Instant::now();
    let LoadedExperimentInput {
        json_value,
        exp_dir,
        project_root,
    } = load_sealed_package_for_run(path)?;
    validate_required_fields(&json_value)?;

    let dataset_path = resolve_dataset_path_in_package(&json_value, &exp_dir)?;
    let tasks = load_tasks(&dataset_path, &json_value)?;
    let (variants, _baseline_id) = resolve_variant_plan(&json_value)?;
    emit_preflight_log(format!(
        "resolved {} task(s) and {} variant(s); running checks",
        tasks.len(),
        variants.len()
    ));
    emit_preflight_log(format!(
        "[PROFILE] config_resolution (yaml parse + dataset load + variant resolve) took {:.3}s",
        preflight_started.elapsed().as_secs_f64()
    ));
    let benchmark_config = parse_benchmark_config(&json_value)?;
    let _runtime_resolve_t = Instant::now();
    let mut variant_runtime_profiles = Vec::with_capacity(variants.len());
    for variant in &variants {
        variant_runtime_profiles.push(resolve_variant_runtime_profile(
            &json_value,
            variant,
            &exp_dir,
            &RunBehavior::default(),
            execution,
        )?);
    }
    emit_preflight_log(format!(
        "[PROFILE] runtime_profile_resolution ({} variants) took {:.3}s",
        variants.len(),
        _runtime_resolve_t.elapsed().as_secs_f64()
    ));
    let checks = collect_preflight_checks(
        &json_value,
        &exp_dir,
        &exp_dir,
        &project_root,
        &tasks,
        &benchmark_config,
        &variants,
        &variant_runtime_profiles,
    );

    let passed = checks
        .iter()
        .all(|c| c.passed || matches!(c.severity, PreflightSeverity::Warning));
    let failed_count = checks
        .iter()
        .filter(|check| !check.passed && matches!(check.severity, PreflightSeverity::Error))
        .count();
    let warning_count = checks
        .iter()
        .filter(|check| !check.passed && matches!(check.severity, PreflightSeverity::Warning))
        .count();
    emit_preflight_log(format!(
        "completed {} checks in {:.1}s (warnings={}, failures={})",
        checks.len(),
        preflight_started.elapsed().as_secs_f32(),
        warning_count,
        failed_count
    ));

    Ok(PreflightReport { passed, checks })
}

#[derive(Debug, Default)]
pub(crate) struct PerTaskImageScanResult {
    pub(crate) unique_images: Vec<String>,
    pub(crate) missing_task_ids: Vec<String>,
    pub(crate) parse_errors: Vec<String>,
}

pub(crate) fn format_preview(items: &[String], limit: usize) -> String {
    if items.is_empty() {
        return "(none)".to_string();
    }
    let shown = items.iter().take(limit).cloned().collect::<Vec<_>>();
    if items.len() <= limit {
        shown.join(", ")
    } else {
        format!("{}, ... (+{} more)", shown.join(", "), items.len() - limit)
    }
}

pub(crate) fn check_agent_runtime_hermetic_for_variants(
    variants: &[Variant],
    variant_runtime_profiles: &[VariantRuntimeProfile],
) -> Vec<PreflightCheck> {
    if variants.len() != variant_runtime_profiles.len() {
        return vec![PreflightCheck {
            name: "agent_runtime_hermetic",
            passed: false,
            severity: PreflightSeverity::Error,
            message: "internal error: variant/runtime profile count mismatch".to_string(),
        }];
    }

    variants
        .iter()
        .zip(variant_runtime_profiles.iter())
        .map(|(variant, profile)| {
            let mut check = check_agent_runtime_hermetic(profile);
            check.message = format!("variant '{}': {}", variant.id, check.message);
            check
        })
        .collect()
}

pub(crate) fn check_agent_runtime_hermetic(
    runtime_profile: &VariantRuntimeProfile,
) -> PreflightCheck {
    let name = "agent_runtime_hermetic";
    if runtime_profile.agent_runtime.image.trim().is_empty() {
        return PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: "trial_runtime.agent.image is required in scientific runs".to_string(),
        };
    }
    PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Error,
        message: "agent runtime is pinned to a container image".to_string(),
    }
}

pub(crate) fn check_agent_bundle_container_compatible_for_variants(
    variants: &[Variant],
    variant_runtime_profiles: &[VariantRuntimeProfile],
) -> Vec<PreflightCheck> {
    if variants.len() != variant_runtime_profiles.len() {
        return vec![PreflightCheck {
            name: "agent_bundle_container_compatible",
            passed: false,
            severity: PreflightSeverity::Error,
            message: "internal error: variant/runtime profile count mismatch".to_string(),
        }];
    }

    variants
        .iter()
        .zip(variant_runtime_profiles.iter())
        .map(|(variant, profile)| {
            let mut check = check_agent_bundle_container_compatible(profile);
            check.message = format!("variant '{}': {}", variant.id, check.message);
            check
        })
        .collect()
}

pub(crate) fn check_agent_bundle_container_compatible(
    runtime_profile: &VariantRuntimeProfile,
) -> PreflightCheck {
    let name = "agent_bundle_container_compatible";
    let Some(agent_artifact) = runtime_profile.agent_runtime.agent_artifact.as_ref() else {
        return PreflightCheck {
            name,
            passed: true,
            severity: PreflightSeverity::Warning,
            message: "skipped because trial_runtime.agent.mount is not declared".to_string(),
        };
    };
    let artifact_name = agent_artifact
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if artifact_name.ends_with(".host.tar.gz") || artifact_name.ends_with(".host.tgz") {
        return PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "host-specific trial_runtime.agent.mount '{}' is forbidden in scientific runs",
                artifact_name
            ),
        };
    }
    PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Error,
        message: "trial_runtime.agent.mount is compatible with container execution".to_string(),
    }
}

pub(crate) fn check_benchmark_grader_reachable_for_variants(
    benchmark_config: &BenchmarkConfig,
    variants: &[Variant],
    variant_runtime_profiles: &[VariantRuntimeProfile],
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
    package_root: &Path,
    project_root: &Path,
) -> Vec<PreflightCheck> {
    if variants.len() != variant_runtime_profiles.len() {
        return vec![PreflightCheck {
            name: "benchmark_grader_reachable",
            passed: false,
            severity: PreflightSeverity::Error,
            message: "internal error: variant/runtime profile count mismatch".to_string(),
        }];
    }
    variants
        .iter()
        .zip(variant_runtime_profiles.iter())
        .map(|(variant, runtime_profile)| {
            let mut check = check_benchmark_grader_reachable_with_scan(
                benchmark_config,
                runtime_profile,
                variant,
                tasks,
                per_task_scan,
                package_root,
                project_root,
            );
            check.message = format!("variant '{}': {}", variant.id, check.message);
            check
        })
        .collect()
}

pub(crate) fn check_container_ready_for_variants(
    variants: &[Variant],
    variant_runtime_profiles: &[VariantRuntimeProfile],
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
    skip_idle_probe: bool,
) -> Vec<PreflightCheck> {
    if variants.is_empty() && variant_runtime_profiles.is_empty() {
        return Vec::new();
    }
    if variants.len() != variant_runtime_profiles.len() {
        return vec![PreflightCheck {
            name: "container_ready",
            passed: false,
            severity: PreflightSeverity::Error,
            message: "internal error: variant/runtime profile count mismatch".to_string(),
        }];
    }
    let mut checks = Vec::new();
    for (variant, runtime_profile) in variants.iter().zip(variant_runtime_profiles.iter()) {
        let mut scoped_checks =
            check_container_ready(runtime_profile, tasks, per_task_scan, skip_idle_probe);
        for check in &mut scoped_checks {
            check.message = format!("variant '{}': {}", variant.id, check.message);
        }
        checks.extend(scoped_checks);
    }
    checks
}

pub(crate) fn check_modal_container_ready_for_variants(
    variants: &[Variant],
    variant_runtime_profiles: &[VariantRuntimeProfile],
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
) -> Vec<PreflightCheck> {
    if variants.is_empty() && variant_runtime_profiles.is_empty() {
        return Vec::new();
    }
    if variants.len() != variant_runtime_profiles.len() {
        return vec![PreflightCheck {
            name: "container_ready",
            passed: false,
            severity: PreflightSeverity::Error,
            message: "internal error: variant/runtime profile count mismatch".to_string(),
        }];
    }
    let mut checks = Vec::new();
    for (variant, runtime_profile) in variants.iter().zip(variant_runtime_profiles.iter()) {
        let mut scoped_checks = check_modal_container_ready(runtime_profile, tasks, per_task_scan);
        for check in &mut scoped_checks {
            check.message = format!("variant '{}': {}", variant.id, check.message);
        }
        checks.extend(scoped_checks);
    }
    checks
}

pub(crate) fn check_modal_container_ready(
    runtime_profile: &VariantRuntimeProfile,
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
) -> Vec<PreflightCheck> {
    let name = "container_ready";
    let mut checks = Vec::new();
    let images = match resolve_preflight_images(
        name,
        runtime_profile,
        tasks,
        per_task_scan,
        "no task images resolved for Modal execution",
    ) {
        Ok(images) => images,
        Err(check) => {
            checks.push(check);
            return checks;
        }
    };

    checks.push(PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Error,
        message: if images.len() == 1 {
            format!("Modal execution resolved required image '{}'", images[0])
        } else {
            format!(
                "Modal execution resolved {} required image(s)",
                images.len()
            )
        },
    });
    checks.push(PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Warning,
        message:
            "local Docker daemon/image probes skipped because executor=modal; Modal pulls the registry image at sandbox launch"
                .to_string(),
    });
    checks
}

pub(crate) fn collect_per_task_images_for_preflight(tasks: &[Value]) -> PerTaskImageScanResult {
    let mut unique_images = HashSet::new();
    let mut result = PerTaskImageScanResult::default();
    for (idx, task) in tasks.iter().enumerate() {
        let boundary = match parse_task_boundary_from_packaged_task(task) {
            Ok(boundary) => boundary,
            Err(err) => {
                result
                    .parse_errors
                    .push(format!("line {}: {}", idx + 1, err));
                continue;
            }
        };
        let task_id = boundary
            .task_payload
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("line {}", idx + 1));
        let image = boundary.task_image.trim();
        if image.is_empty() {
            result.missing_task_ids.push(task_id);
        } else if unique_images.insert(image.to_string()) {
            result.unique_images.push(image.to_string());
        }
    }
    result.unique_images.sort();
    result
}

pub(crate) fn resolve_preflight_images(
    check_name: &'static str,
    runtime_profile: &VariantRuntimeProfile,
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
    missing_global_image_message: &'static str,
) -> Result<Vec<String>, PreflightCheck> {
    let requirements = resolve_preflight_image_requirements(
        check_name,
        runtime_profile,
        tasks,
        per_task_scan,
        missing_global_image_message,
    )?;
    let resolver = ReferenceOnlyImageResolver;
    let scoped_cache = ScopedImageResolverCache::new(&resolver);
    let resolver_chain = ImageResolverChain::new(vec![&scoped_cache]);
    requirements
        .into_iter()
        .map(|requirement| {
            resolver_chain
                .resolve(&ImageResolveRequest {
                    requirement,
                    mode: ImageResolutionMode::ReferenceOnly,
                })
                .map(|report| report.requirement.image.raw().to_string())
                .map_err(|err| PreflightCheck {
                    name: check_name,
                    passed: false,
                    severity: PreflightSeverity::Error,
                    message: err.to_string(),
                })
        })
        .collect()
}

pub(crate) fn resolve_preflight_image_requirements(
    check_name: &'static str,
    runtime_profile: &VariantRuntimeProfile,
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
    missing_global_image_message: &'static str,
) -> Result<Vec<ImageRequirement>, PreflightCheck> {
    let owned_scan;
    let scan = if let Some(scan) = per_task_scan {
        scan
    } else {
        owned_scan = collect_per_task_images_for_preflight(tasks);
        &owned_scan
    };
    if !scan.parse_errors.is_empty() {
        return Err(PreflightCheck {
            name: check_name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "failed to parse packaged case rows while collecting task images: {}",
                format_preview(&scan.parse_errors, 3)
            ),
        });
    }
    if !scan.missing_task_ids.is_empty() {
        return Err(PreflightCheck {
            name: check_name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "tasks missing image: {}",
                format_preview(&scan.missing_task_ids, 5)
            ),
        });
    }
    if scan.unique_images.is_empty() {
        let fallback_image = runtime_profile.agent_runtime.image.trim();
        if !fallback_image.is_empty() {
            return ImageRequirement::new(
                ImageRequirementRole::AgentRuntime,
                fallback_image.to_string(),
                None,
            )
            .map(|requirement| vec![requirement])
            .map_err(|err| PreflightCheck {
                name: check_name,
                passed: false,
                severity: PreflightSeverity::Error,
                message: err.to_string(),
            });
        }
        return Err(PreflightCheck {
            name: check_name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: missing_global_image_message.to_string(),
        });
    }
    match preflight_max_unique_images() {
        Ok(Some(max_images)) if scan.unique_images.len() > max_images => {
            return Err(PreflightCheck {
                name: check_name,
                passed: false,
                severity: PreflightSeverity::Error,
                message: format!(
                    "too many unique task images for configured preflight image budget: unique_images={} max={} env_var={} examples={}",
                    scan.unique_images.len(),
                    max_images,
                    AGENTLAB_MAX_PREFLIGHT_IMAGES_ENV,
                    format_preview(&scan.unique_images, 3)
                ),
            });
        }
        Ok(_) => {}
        Err(err) => {
            return Err(PreflightCheck {
                name: check_name,
                passed: false,
                severity: PreflightSeverity::Error,
                message: err.to_string(),
            });
        }
    }
    scan.unique_images
        .iter()
        .map(|image| {
            ImageRequirement::new(ImageRequirementRole::TaskSandbox, image.clone(), None).map_err(
                |err| PreflightCheck {
                    name: check_name,
                    passed: false,
                    severity: PreflightSeverity::Error,
                    message: err.to_string(),
                },
            )
        })
        .collect()
}

pub(crate) fn has_blocking_preflight_error(checks: &[PreflightCheck], name: &str) -> bool {
    checks.iter().any(|check| {
        check.name == name && !check.passed && matches!(check.severity, PreflightSeverity::Error)
    })
}

pub(crate) fn check_trial_runtime_support_matrix(
    trial_runtime: &crate::trial::plan::TrialRuntimeConfig,
    per_task_scan: Option<&PerTaskImageScanResult>,
) -> Vec<PreflightCheck> {
    let name = "trial_runtime_support_matrix";
    let mut failures = Vec::new();

    match trial_runtime.execution.agent_site {
        AgentSite::Host => {
            if trial_runtime.task.interface != TaskInterface::InputOnly {
                failures.push(
                    "execution.agent_site=host currently supports task.interface=input_only only; host agent execution does not materialize task files or task workspaces"
                        .to_string(),
                );
            }
            if trial_runtime.grader.strategy != GradingStrategy::None {
                failures.push(
                    "execution.agent_site=host currently supports grader.strategy=none only; host-agent plus grader is not wired into execution"
                        .to_string(),
                );
            }
        }
        AgentSite::TaskRuntime | AgentSite::AgentContainer => {}
    }

    match &trial_runtime.task.interface {
        TaskInterface::InputOnly => {
            if trial_runtime.execution.agent_site != AgentSite::Host {
                failures.push(
                    "task.interface=input_only is currently executable only with execution.agent_site=host"
                        .to_string(),
                );
            }
        }
        TaskInterface::ReadonlyFiles => {
            failures.push(
                "task.interface=readonly_files is parsed but task.files materialization is not executable yet"
                    .to_string(),
            );
        }
        TaskInterface::WritableWorkspace => {
            let source = trial_runtime
                .task
                .workspace
                .as_ref()
                .map(|workspace| &workspace.source);
            if source != Some(&WorkspaceSource::ContainerImage) {
                failures.push(
                    "task.interface=writable_workspace is currently executable only with workspace.source=container_image"
                        .to_string(),
                );
            }
        }
    }

    if trial_runtime.execution.agent_site == AgentSite::AgentContainer {
        let declared_agent_image = trial_runtime
            .agent
            .image
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let (Some(agent_image), Some(scan)) = (declared_agent_image, per_task_scan) {
            let mismatched = scan
                .unique_images
                .iter()
                .filter(|image| image.as_str() != agent_image)
                .cloned()
                .collect::<Vec<_>>();
            if !mismatched.is_empty() {
                failures.push(format!(
                    "execution.agent_site=agent_container declares trial_runtime.agent.image='{}', but execution currently uses task row image(s); mismatched task image(s): {}",
                    agent_image,
                    format_preview(&mismatched, 3)
                ));
            }
        }
    }

    if failures.is_empty() {
        vec![PreflightCheck {
            name,
            passed: true,
            severity: PreflightSeverity::Error,
            message: "trial_runtime shape is within the currently executable support matrix"
                .to_string(),
        }]
    } else {
        vec![PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format_preview(&failures, 4),
        }]
    }
}

pub(crate) fn check_grader_transport_integration(
    experiment: &Value,
    trial_runtime: &crate::trial::plan::TrialRuntimeConfig,
    benchmark_config: &BenchmarkConfig,
) -> Vec<PreflightCheck> {
    let name = "grader_transport_integration";
    match &trial_runtime.grader.strategy {
        GradingStrategy::None => {
            if benchmark_config.grader.is_some() {
                return vec![PreflightCheck {
                    name,
                    passed: false,
                    severity: PreflightSeverity::Error,
                    message:
                        "trial_runtime.grader.strategy=none but benchmark grader config resolved"
                            .to_string(),
                }];
            }
            let metric_counts = crate::config::parse_metric_definitions(experiment)
                .map(|metrics| {
                    let agent_response = metrics
                        .iter()
                        .filter(|metric| metric.source.source_type == "agent_response")
                        .count();
                    let runtime_output = metrics
                        .iter()
                        .filter(|metric| metric.source.source_type == "runtime_output")
                        .count();
                    (agent_response, runtime_output)
                })
                .unwrap_or((0, 0));
            vec![PreflightCheck {
                name,
                passed: true,
                severity: PreflightSeverity::Warning,
                message: format!(
                    "grader disabled; metrics will be extracted from agent_response/runtime_output result sources (agent_response={}, runtime_output={})",
                    metric_counts.0, metric_counts.1
                ),
            }]
        }
        strategy => {
            let Some(grader) = benchmark_config.grader.as_ref() else {
                return vec![PreflightCheck {
                    name,
                    passed: false,
                    severity: PreflightSeverity::Error,
                    message: format!(
                        "trial_runtime.grader.strategy={} did not resolve to an executable benchmark grader config; check command, strategy-specific config, inputs, and outputs",
                        grader_strategy_label(strategy)
                    ),
                }];
            };
            if grader.strategy != *strategy {
                return vec![PreflightCheck {
                    name,
                    passed: false,
                    severity: PreflightSeverity::Error,
                    message: format!(
                        "trial_runtime grader strategy {} resolved as {}",
                        grader_strategy_label(strategy),
                        grader_strategy_label(&grader.strategy)
                    ),
                }];
            }
            vec![PreflightCheck {
                name,
                passed: true,
                severity: PreflightSeverity::Error,
                message: format!(
                    "grader transport resolved with {} input(s) and {} output(s)",
                    grader.inputs.len(),
                    grader.outputs.len()
                ),
            }]
        }
    }
}

fn grader_strategy_label(strategy: &GradingStrategy) -> &'static str {
    match strategy {
        GradingStrategy::None => "none",
        GradingStrategy::InTaskRuntime => "in_task_runtime",
        GradingStrategy::Injected => "injected",
        GradingStrategy::Separate => "separate",
        GradingStrategy::Host => "host",
    }
}

pub(crate) fn resolve_min_free_bytes() -> Result<u64> {
    match env::var(AGENTLAB_MIN_FREE_BYTES_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(anyhow!(
                    "{} must be a positive integer when set",
                    AGENTLAB_MIN_FREE_BYTES_ENV
                ));
            }
            let parsed = trimmed.parse::<u64>().map_err(|_| {
                anyhow!(
                    "{} must be a positive integer when set (got: {})",
                    AGENTLAB_MIN_FREE_BYTES_ENV,
                    raw
                )
            })?;
            if parsed == 0 {
                return Err(anyhow!(
                    "{} must be > 0 when set",
                    AGENTLAB_MIN_FREE_BYTES_ENV
                ));
            }
            Ok(parsed)
        }
        Err(env::VarError::NotPresent) => Ok(DEFAULT_MIN_FREE_BYTES),
        Err(err) => Err(anyhow!(
            "failed reading {}: {}",
            AGENTLAB_MIN_FREE_BYTES_ENV,
            err
        )),
    }
}

pub(crate) fn free_bytes_for_path(path: &Path) -> Result<u64> {
    let probe = path.to_string_lossy().to_string();
    let out = Command::new("df").args(["-Pk", probe.as_str()]).output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "df -Pk {} failed: {}",
            probe,
            crate::util::output_error_detail(&out)
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|candidate| {
            let trimmed = candidate.trim();
            !trimmed.is_empty() && !trimmed.starts_with("Filesystem")
        })
        .ok_or_else(|| anyhow!("unable to parse df output for {}", probe))?;
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 4 {
        return Err(anyhow!(
            "unable to parse df output for {} (expected >=4 columns): {}",
            probe,
            line
        ));
    }
    let available_kb = fields[3].parse::<u64>().map_err(|_| {
        anyhow!(
            "unable to parse available blocks from df output for {}: {}",
            probe,
            line
        )
    })?;
    Ok(available_kb.saturating_mul(1024))
}

pub(crate) fn dir_size_bytes(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    let walker = walkdir::WalkDir::new(path).into_iter();
    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        total = total.saturating_add(entry.metadata()?.len());
    }
    Ok(total)
}

pub(crate) fn enforce_runtime_disk_headroom(probe_path: &Path, min_free_bytes: u64) -> Result<()> {
    let available = free_bytes_for_path(probe_path)?;
    if available < min_free_bytes {
        return Err(anyhow!(
            "runtime disk headroom breached at '{}': required={} available={}",
            probe_path.display(),
            min_free_bytes,
            available
        ));
    }
    Ok(())
}

pub(crate) fn enforce_runtime_run_size_budget(run_dir: &Path, max_run_bytes: u64) -> Result<()> {
    let used = dir_size_bytes(run_dir)?;
    if used > max_run_bytes {
        return Err(anyhow!(
            "runtime run size budget exceeded at '{}': max={} used={}",
            run_dir.display(),
            max_run_bytes,
            used
        ));
    }
    Ok(())
}

pub(crate) fn check_disk_headroom_with_threshold(
    probe_path: &Path,
    min_free_bytes: u64,
) -> PreflightCheck {
    let name = "disk_headroom";
    let available = match free_bytes_for_path(probe_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return PreflightCheck {
                name,
                passed: false,
                severity: PreflightSeverity::Error,
                message: format!(
                    "failed to determine free disk bytes at '{}': {}",
                    probe_path.display(),
                    err
                ),
            }
        }
    };
    if available < min_free_bytes {
        return PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "insufficient free disk bytes at '{}': required={} available={}",
                probe_path.display(),
                min_free_bytes,
                available
            ),
        };
    }
    PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Error,
        message: format!(
            "disk headroom ok at '{}': required={} available={}",
            probe_path.display(),
            min_free_bytes,
            available
        ),
    }
}

pub(crate) fn check_disk_headroom(probe_path: &Path) -> PreflightCheck {
    let name = "disk_headroom";
    match resolve_min_free_bytes() {
        Ok(min_free_bytes) => check_disk_headroom_with_threshold(probe_path, min_free_bytes),
        Err(err) => PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: err.to_string(),
        },
    }
}

pub(crate) fn collect_preflight_checks(
    json_value: &Value,
    package_root: &Path,
    disk_probe_path: &Path,
    project_root: &Path,
    tasks: &[Value],
    benchmark_config: &BenchmarkConfig,
    variants: &[Variant],
    variant_runtime_profiles: &[VariantRuntimeProfile],
) -> Vec<PreflightCheck> {
    collect_preflight_checks_for_executor(
        json_value,
        package_root,
        disk_probe_path,
        project_root,
        tasks,
        benchmark_config,
        variants,
        variant_runtime_profiles,
        ExecutorKind::LocalDocker,
    )
}

pub(crate) fn collect_preflight_checks_for_executor(
    json_value: &Value,
    package_root: &Path,
    disk_probe_path: &Path,
    project_root: &Path,
    tasks: &[Value],
    benchmark_config: &BenchmarkConfig,
    variants: &[Variant],
    variant_runtime_profiles: &[VariantRuntimeProfile],
    executor_kind: ExecutorKind,
) -> Vec<PreflightCheck> {
    let mut checks = Vec::new();
    let trial_runtime = match parse_trial_runtime_config(json_value) {
        Ok(config) => config,
        Err(err) => {
            checks.push(PreflightCheck {
                name: "trial_runtime_schema",
                passed: false,
                severity: PreflightSeverity::Error,
                message: err.to_string(),
            });
            return checks;
        }
    };
    let mut compatibility_errors = Vec::new();
    for (idx, task) in tasks.iter().enumerate() {
        match parse_task_boundary_from_packaged_task(task) {
            Ok(boundary) => {
                if let Err(err) =
                    validate_task_boundary_for_trial_runtime(&trial_runtime, &boundary)
                {
                    compatibility_errors.push(format!("line {}: {}", idx + 1, err));
                }
            }
            Err(err) => compatibility_errors.push(format!("line {}: {}", idx + 1, err)),
        }
    }
    if !compatibility_errors.is_empty() {
        checks.push(PreflightCheck {
            name: "trial_runtime_compatibility",
            passed: false,
            severity: PreflightSeverity::Error,
            message: format_preview(&compatibility_errors, 5),
        });
        return checks;
    }
    checks.push(PreflightCheck {
        name: "trial_runtime_compatibility",
        passed: true,
        severity: PreflightSeverity::Error,
        message: "trial runtime is compatible with packaged task rows".to_string(),
    });
    if variants.is_empty() {
        checks.push(PreflightCheck {
            name: "variant_runtime_profiles",
            passed: false,
            severity: PreflightSeverity::Error,
            message: "no variants available in experiment".to_string(),
        });
        return checks;
    }
    if variant_runtime_profiles.is_empty() {
        checks.push(PreflightCheck {
            name: "variant_runtime_profiles",
            passed: false,
            severity: PreflightSeverity::Error,
            message: "failed to resolve runtime profile for baseline variant".to_string(),
        });
        return checks;
    }
    let per_task_scan = if !tasks.is_empty() {
        Some(collect_per_task_images_for_preflight(tasks))
    } else {
        None
    };
    let skip_container_idle_probe = benchmark_config.grader.is_some();

    checks.extend(check_dataset_task_ids(
        tasks,
        benchmark_config,
        variant_runtime_profiles,
    ));
    checks.extend(check_trial_runtime_support_matrix(
        &trial_runtime,
        per_task_scan.as_ref(),
    ));
    checks.extend(check_grader_transport_integration(
        json_value,
        &trial_runtime,
        benchmark_config,
    ));

    if has_blocking_preflight_error(&checks, "trial_runtime_support_matrix")
        || has_blocking_preflight_error(&checks, "grader_transport_integration")
    {
        checks.push(PreflightCheck {
            name: "agent_runtime_reachable",
            passed: true,
            severity: PreflightSeverity::Warning,
            message: "skipped because trial_runtime support/transport validation failed"
                .to_string(),
        });
        checks.push(PreflightCheck {
            name: "benchmark_grader_reachable",
            passed: true,
            severity: PreflightSeverity::Warning,
            message: "skipped because trial_runtime support/transport validation failed"
                .to_string(),
        });
        return checks;
    }

    macro_rules! timed_check {
        ($label:expr, $body:expr) => {{
            let _t = Instant::now();
            let result = $body;
            emit_preflight_log(format!(
                "[PROFILE] {} took {:.3}s",
                $label,
                _t.elapsed().as_secs_f64()
            ));
            result
        }};
    }

    emit_preflight_log("running check: disk_headroom");
    timed_check!(
        "disk_headroom",
        checks.push(check_disk_headroom(disk_probe_path))
    );
    let agent_runs_in_container = matches!(
        trial_runtime.execution.agent_site,
        AgentSite::TaskRuntime | AgentSite::AgentContainer
    );
    let agent_runs_on_host = trial_runtime.execution.agent_site == AgentSite::Host;
    let grader_configured = trial_runtime.grader.strategy != GradingStrategy::None;

    if matches!(
        trial_runtime.execution.agent_site,
        AgentSite::AgentContainer
    ) {
        emit_preflight_log("running check: agent_runtime_hermetic");
        timed_check!(
            "agent_runtime_hermetic",
            checks.extend(check_agent_runtime_hermetic_for_variants(
                variants,
                variant_runtime_profiles,
            ))
        );
    } else {
        checks.push(PreflightCheck {
            name: "agent_runtime_hermetic",
            passed: true,
            severity: PreflightSeverity::Warning,
            message:
                "skipped because trial_runtime.execution.agent_site does not use an agent container"
                    .to_string(),
        });
    }
    if agent_runs_in_container {
        emit_preflight_log("running check: agent_bundle_container_compatible");
        timed_check!(
            "agent_bundle_container_compatible",
            checks.extend(check_agent_bundle_container_compatible_for_variants(
                variants,
                variant_runtime_profiles,
            ))
        );
    } else {
        checks.push(PreflightCheck {
            name: "agent_bundle_container_compatible",
            passed: true,
            severity: PreflightSeverity::Warning,
            message:
                "skipped because trial_runtime.execution.agent_site runs the agent on the host"
                    .to_string(),
        });
    }
    let container_runtime_required = agent_runs_in_container
        || (grader_configured && !matches!(trial_runtime.grader.strategy, GradingStrategy::Host))
        || per_task_scan
            .as_ref()
            .is_some_and(|scan| !scan.unique_images.is_empty());
    if container_runtime_required {
        emit_preflight_log("running check: container_ready");
        timed_check!(
            "container_ready",
            if executor_kind == ExecutorKind::Modal {
                checks.extend(check_modal_container_ready_for_variants(
                    variants,
                    variant_runtime_profiles,
                    tasks,
                    per_task_scan.as_ref(),
                ))
            } else {
                checks.extend(check_container_ready_for_variants(
                    variants,
                    variant_runtime_profiles,
                    tasks,
                    per_task_scan.as_ref(),
                    skip_container_idle_probe,
                ))
            }
        );
    } else {
        checks.push(PreflightCheck {
            name: "container_ready",
            passed: true,
            severity: PreflightSeverity::Warning,
            message: "skipped because trial_runtime does not require a container runtime"
                .to_string(),
        });
    }
    if has_blocking_preflight_error(&checks, "container_ready") {
        checks.push(PreflightCheck {
            name: "agent_runtime_reachable",
            passed: true,
            severity: PreflightSeverity::Warning,
            message: "skipped because container_ready reported blocking failures".to_string(),
        });
        checks.push(PreflightCheck {
            name: "benchmark_grader_reachable",
            passed: true,
            severity: PreflightSeverity::Warning,
            message: "skipped because container_ready reported blocking failures".to_string(),
        });
    } else {
        if agent_runs_on_host {
            emit_preflight_log("running check: agent_runtime_reachable (host)");
            timed_check!(
                "agent_runtime_reachable",
                checks.extend(check_host_agent_runtime_reachable_for_variants(
                    variants,
                    variant_runtime_profiles,
                ))
            );
        } else if executor_kind == ExecutorKind::Modal {
            checks.push(PreflightCheck {
                name: "agent_runtime_reachable",
                passed: true,
                severity: PreflightSeverity::Warning,
                message:
                    "skipped because executor=modal validates the agent command during Modal sandbox launch"
                        .to_string(),
            });
        } else {
            emit_preflight_log("running check: agent_runtime_reachable");
            timed_check!(
                "agent_runtime_reachable",
                checks.extend(check_agent_runtime_reachable_for_variants(
                    variants,
                    variant_runtime_profiles,
                    tasks,
                    per_task_scan.as_ref(),
                    package_root,
                    project_root,
                ))
            );
        }
        if has_blocking_preflight_error(&checks, "agent_runtime_reachable") {
            checks.push(PreflightCheck {
                name: "benchmark_grader_reachable",
                passed: true,
                severity: PreflightSeverity::Warning,
                message: "skipped because agent_runtime_reachable reported blocking failures"
                    .to_string(),
            });
        } else if grader_configured && executor_kind == ExecutorKind::Modal {
            checks.push(PreflightCheck {
                name: "benchmark_grader_reachable",
                passed: true,
                severity: PreflightSeverity::Warning,
                message:
                    "skipped because executor=modal validates grader reachability during Modal sandbox launch"
                        .to_string(),
            });
        } else if grader_configured {
            emit_preflight_log("running check: benchmark_grader_reachable");
            timed_check!(
                "benchmark_grader_reachable",
                checks.extend(check_benchmark_grader_reachable_for_variants(
                    benchmark_config,
                    variants,
                    variant_runtime_profiles,
                    tasks,
                    per_task_scan.as_ref(),
                    package_root,
                    project_root,
                ))
            );
        } else {
            checks.push(PreflightCheck {
                name: "benchmark_grader_reachable",
                passed: true,
                severity: PreflightSeverity::Warning,
                message: "skipped because trial_runtime.grader.strategy=none".to_string(),
            });
        }
    }
    checks
}

pub(crate) fn check_dataset_task_ids(
    tasks: &[Value],
    benchmark_config: &BenchmarkConfig,
    _variant_runtime_profiles: &[VariantRuntimeProfile],
) -> Vec<PreflightCheck> {
    let mut checks = Vec::new();
    let mut seen_ids: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut malformed_boundary_rows = Vec::new();
    let mut missing_ids = Vec::new();
    let mut grading_disabled_lines = Vec::new();
    let has_benchmark = benchmark_config.grader.is_some();

    for (idx, task) in tasks.iter().enumerate() {
        let line_num = idx + 1;
        let parsed = match parse_task_boundary_from_packaged_task(task) {
            Ok(parsed) => parsed,
            Err(err) => {
                malformed_boundary_rows.push(format!("line {}: {}", line_num, err));
                continue;
            }
        };
        let id = Some(parsed.task_id.as_str());

        match id {
            Some(id_str) if !id_str.is_empty() => {
                seen_ids
                    .entry(id_str.to_string())
                    .or_default()
                    .push(line_num);
            }
            _ => {
                missing_ids.push(line_num);
            }
        }

        if has_benchmark && !task_grading_enabled(&parsed.task_payload) {
            grading_disabled_lines.push(line_num);
        }
    }

    if !malformed_boundary_rows.is_empty() {
        checks.push(PreflightCheck {
            name: "dataset_task_ids",
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "malformed case rows (expected packaged case_v1, case_v2, or task_row_v2): {}",
                format_preview(&malformed_boundary_rows, 3)
            ),
        });
    }

    if !missing_ids.is_empty() {
        checks.push(PreflightCheck {
            name: "dataset_task_ids",
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!("tasks missing 'id' field at lines: {:?}", missing_ids),
        });
    }
    let duplicates: Vec<_> = seen_ids
        .iter()
        .filter(|(_, lines)| lines.len() > 1)
        .collect();
    if !duplicates.is_empty() {
        let dup_details: Vec<String> = duplicates
            .iter()
            .map(|(id, lines)| format!("'{}' at lines {:?}", id, lines))
            .collect();
        checks.push(PreflightCheck {
            name: "dataset_task_ids",
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!("duplicate task IDs: {}", dup_details.join(", ")),
        });
    }

    if missing_ids.is_empty() && duplicates.is_empty() && malformed_boundary_rows.is_empty() {
        checks.push(PreflightCheck {
            name: "dataset_task_ids",
            passed: true,
            severity: PreflightSeverity::Error,
            message: format!("all {} tasks have unique IDs", tasks.len()),
        });
    }

    if !grading_disabled_lines.is_empty() {
        checks.push(PreflightCheck {
            name: "dataset_task_ids",
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "benchmark configured but grading.enabled=false at lines: {:?}; benchmark tasks require mapped grading output",
                grading_disabled_lines
            ),
        });
    }

    checks
}

pub(crate) fn check_benchmark_grader_reachable(
    benchmark_config: &BenchmarkConfig,
    runtime_profile: &VariantRuntimeProfile,
    variant: &Variant,
    tasks: &[Value],
    project_root: &Path,
) -> PreflightCheck {
    check_benchmark_grader_reachable_with_scan(
        benchmark_config,
        runtime_profile,
        variant,
        tasks,
        None,
        project_root,
        project_root,
    )
}

pub(crate) fn check_agent_runtime_reachable_for_variants(
    variants: &[Variant],
    variant_runtime_profiles: &[VariantRuntimeProfile],
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
    package_root: &Path,
    project_root: &Path,
) -> Vec<PreflightCheck> {
    if variants.len() != variant_runtime_profiles.len() {
        return vec![PreflightCheck {
            name: "agent_runtime_reachable",
            passed: false,
            severity: PreflightSeverity::Error,
            message: "internal error: variant/runtime profile count mismatch".to_string(),
        }];
    }

    let mut checks = Vec::with_capacity(variant_runtime_profiles.len());
    for (variant, runtime_profile) in variants.iter().zip(variant_runtime_profiles.iter()) {
        let mut check = check_agent_runtime_reachable_with_scan(
            runtime_profile,
            variant,
            tasks,
            per_task_scan,
            package_root,
            project_root,
        );
        check.message = format!("variant '{}': {}", variant.id, check.message);
        checks.push(check);
    }
    checks
}

pub(crate) fn check_host_agent_runtime_reachable_for_variants(
    variants: &[Variant],
    variant_runtime_profiles: &[VariantRuntimeProfile],
) -> Vec<PreflightCheck> {
    if variants.len() != variant_runtime_profiles.len() {
        return vec![PreflightCheck {
            name: "agent_runtime_reachable",
            passed: false,
            severity: PreflightSeverity::Error,
            message: "internal error: variant/runtime profile count mismatch".to_string(),
        }];
    }

    variants
        .iter()
        .zip(variant_runtime_profiles.iter())
        .map(|(variant, runtime_profile)| {
            let mut check = check_host_agent_runtime_reachable(runtime_profile);
            check.message = format!("variant '{}': {}", variant.id, check.message);
            check
        })
        .collect()
}

pub(crate) fn check_host_agent_runtime_reachable(
    runtime_profile: &VariantRuntimeProfile,
) -> PreflightCheck {
    let name = "agent_runtime_reachable";
    if let Err(err) = ensure_required_runtime_env_present(
        &runtime_profile.agent_runtime,
        &runtime_profile.agent_runtime_env,
    ) {
        return PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: err.to_string(),
        };
    }
    let command = preview_agent_command(runtime_profile);
    let Some(program) = command
        .first()
        .map(String::as_str)
        .filter(|value| !value.is_empty())
    else {
        return PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: "trial_runtime.agent.command must not be empty".to_string(),
        };
    };
    if Path::new(program).is_absolute() && !Path::new(program).exists() {
        return PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!("host agent executable not found: {}", program),
        };
    }
    PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Error,
        message: "host agent command is configured; container contract smoke is not required"
            .to_string(),
    }
}

pub(crate) fn check_agent_runtime_reachable_with_scan(
    runtime_profile: &VariantRuntimeProfile,
    variant: &Variant,
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
    package_root: &Path,
    project_root: &Path,
) -> PreflightCheck {
    let name = "agent_runtime_reachable";
    if let Err(err) = ensure_required_runtime_env_present(
        &runtime_profile.agent_runtime,
        &runtime_profile.agent_runtime_env,
    ) {
        return PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: err.to_string(),
        };
    }
    let images = match resolve_preflight_images(
        name,
        runtime_profile,
        tasks,
        per_task_scan,
        "task images are required for contract smoke",
    ) {
        Ok(images) => images,
        Err(check) => return check,
    };
    emit_preflight_log(format!(
        "agent_runtime_reachable: running contract smoke in {} image(s)",
        images.len()
    ));
    let failures = run_bounded_image_probes(&images, "agent_runtime_reachable", |idx, image| {
        if should_emit_image_probe_progress(idx + 1, images.len()) {
            emit_preflight_log(format!(
                "agent_runtime_reachable: image {}/{} ({})",
                idx + 1,
                images.len(),
                image
            ));
        }
        let context = match build_preflight_probe_context(
            runtime_profile,
            variant,
            tasks,
            image,
            package_root,
            project_root,
        ) {
            Ok(context) => context,
            Err(err) => return Some(format!("{} ({})", image, err)),
        };
        let request = build_preflight_probe_request(&context, runtime_profile, None, false);
        match run_preflight_contract_smoke(&request) {
            Ok(report) => {
                let smoke_failures = collect_preflight_contract_smoke_failures(&request, &report);
                if smoke_failures.is_empty() {
                    None
                } else {
                    Some(format!(
                        "{} ({})",
                        image,
                        format_preview(&smoke_failures, 3)
                    ))
                }
            }
            Err(err) => Some(format!("{} ({})", image, err)),
        }
    });

    let failures: Vec<String> = failures.into_iter().flatten().collect();
    if !failures.is_empty() {
        return PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "runtime agent contract smoke failed in required images: {}",
                format_preview(&failures, 3)
            ),
        };
    }

    PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Error,
        message: if images.len() == 1 {
            format!(
                "runtime agent contract smoke passed against task image '{}'",
                images[0]
            )
        } else {
            format!(
                "runtime agent contract smoke passed against all {} required task images",
                images.len(),
            )
        },
    }
}

pub(crate) fn check_benchmark_grader_reachable_with_scan(
    benchmark_config: &BenchmarkConfig,
    runtime_profile: &VariantRuntimeProfile,
    variant: &Variant,
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
    package_root: &Path,
    project_root: &Path,
) -> PreflightCheck {
    let name = "benchmark_grader_reachable";

    let grader = match benchmark_config.grader.as_ref() {
        Some(grader) => grader,
        None => {
            return PreflightCheck {
                name,
                passed: true,
                severity: PreflightSeverity::Warning,
                message: "no benchmark grader configured — grading skipped".to_string(),
            };
        }
    };
    if matches!(grader.strategy, GradingStrategy::Host) {
        let resolved_command = match resolve_host_grader_command(grader, package_root) {
            Ok(command) => command,
            Err(err) => {
                return PreflightCheck {
                    name,
                    passed: false,
                    severity: PreflightSeverity::Error,
                    message: err.to_string(),
                };
            }
        };
        let Some(program) = resolved_command
            .first()
            .map(String::as_str)
            .filter(|value| !value.is_empty())
        else {
            return PreflightCheck {
                name,
                passed: false,
                severity: PreflightSeverity::Error,
                message: "host benchmark grader command is empty".to_string(),
            };
        };
        if Path::new(program).is_absolute() && !Path::new(program).exists() {
            return PreflightCheck {
                name,
                passed: false,
                severity: PreflightSeverity::Error,
                message: format!("host benchmark grader executable not found: {}", program),
            };
        }
        for token in resolved_command.iter().skip(1) {
            let path = Path::new(token);
            if path.is_absolute() && !path.exists() {
                return PreflightCheck {
                    name,
                    passed: false,
                    severity: PreflightSeverity::Error,
                    message: format!("host benchmark grader capability file not found: {}", token),
                };
            }
        }
        return PreflightCheck {
            name,
            passed: true,
            severity: PreflightSeverity::Error,
            message: "host benchmark grader command is configured; trial grading will run on the runner host".to_string(),
        };
    }

    let images = match resolve_preflight_images(
        name,
        runtime_profile,
        tasks,
        per_task_scan,
        "benchmark grader configured but no task images specified",
    ) {
        Ok(images) => images,
        Err(check) => return check,
    };
    emit_preflight_log(format!(
        "benchmark_grader_reachable: running benchmark contract smoke in {} image(s)",
        images.len()
    ));
    let failures = run_bounded_image_probes(&images, "benchmark_grader_reachable", |idx, image| {
        if should_emit_image_probe_progress(idx + 1, images.len()) {
            emit_preflight_log(format!(
                "benchmark_grader_reachable: image {}/{} ({})",
                idx + 1,
                images.len(),
                image
            ));
        }
        let context = match build_preflight_probe_context(
            runtime_profile,
            variant,
            tasks,
            image,
            package_root,
            project_root,
        ) {
            Ok(context) => context,
            Err(err) => return Some(format!("{} ({})", image, err)),
        };
        let request = build_preflight_probe_request(&context, runtime_profile, Some(grader), true);
        match run_preflight_contract_smoke(&request) {
            Ok(report) => {
                let smoke_failures = collect_preflight_contract_smoke_failures(&request, &report);
                if smoke_failures.is_empty() {
                    None
                } else {
                    Some(format!(
                        "{} ({})",
                        image,
                        format_preview(&smoke_failures, 3)
                    ))
                }
            }
            Err(err) => Some(format!("{} ({})", image, err)),
        }
    });

    let failures: Vec<String> = failures.into_iter().flatten().collect();
    if !failures.is_empty() {
        return PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "benchmark grader contract smoke failed in required task images: {}",
                format_preview(&failures, 3)
            ),
        };
    }

    PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Error,
        message: if images.len() == 1 {
            format!(
                "benchmark grader contract smoke passed in image '{}'",
                images[0]
            )
        } else {
            format!(
                "benchmark grader contract smoke passed in all {} required images",
                images.len()
            )
        },
    }
}

pub(crate) fn is_runner_staged_script_path(path: &str) -> bool {
    path == AGENTLAB_CONTRACT_IN_DIR
        || path.starts_with(&format!("{}/", AGENTLAB_CONTRACT_IN_DIR))
        || path
            == format!(
                "{}/{}",
                AGENTLAB_TASK_WORKDIR_PLACEHOLDER, AGENTLAB_RUNNER_SUPPORT_REL_DIR
            )
        || path.starts_with(&format!(
            "{}/{}/",
            AGENTLAB_TASK_WORKDIR_PLACEHOLDER, AGENTLAB_RUNNER_SUPPORT_REL_DIR
        ))
        || path == AGENTLAB_CONTRACT_RUNTIME_AUX_DIR
        || path.starts_with(&format!("{}/", AGENTLAB_CONTRACT_RUNTIME_AUX_DIR))
}

pub(crate) fn check_container_ready(
    runtime_profile: &VariantRuntimeProfile,
    tasks: &[Value],
    per_task_scan: Option<&PerTaskImageScanResult>,
    skip_idle_probe: bool,
) -> Vec<PreflightCheck> {
    let name = "container_ready";

    let mut checks = Vec::new();
    let _docker_total = Instant::now();
    let _docker_daemon_t = Instant::now();
    emit_preflight_log("container_ready: checking Docker daemon reachability");
    let docker = match crate::backend::docker::DockerRuntime::connect() {
        Ok(runtime) => runtime,
        Err(err) => {
            checks.push(PreflightCheck {
                name,
                passed: false,
                severity: PreflightSeverity::Error,
                message: format!("Docker backend could not initialize: {}", err),
            });
            return checks;
        }
    };
    let docker_ok = match docker.ping() {
        Ok(()) => {
            checks.push(PreflightCheck {
                name,
                passed: true,
                severity: PreflightSeverity::Error,
                message: "Docker daemon is reachable".to_string(),
            });
            true
        }
        Err(err) => {
            checks.push(PreflightCheck {
                name,
                passed: false,
                severity: PreflightSeverity::Error,
                message: format!(
                    "Docker daemon is not reachable — container execution will fail: {}",
                    err
                ),
            });
            false
        }
    };

    emit_preflight_log(format!(
        "[PROFILE] container_ready/daemon_check took {:.3}s",
        _docker_daemon_t.elapsed().as_secs_f64()
    ));

    if !docker_ok {
        return checks;
    }

    let images = match resolve_preflight_images(
        name,
        runtime_profile,
        tasks,
        per_task_scan,
        "no task images resolved for container execution",
    ) {
        Ok(images) => images,
        Err(check) => {
            checks.push(check);
            return checks;
        }
    };

    checks.push(PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Error,
        message: if images.len() == 1 {
            format!(
                "container execution resolved required image '{}'",
                images[0]
            )
        } else {
            format!(
                "container execution resolved {} required image(s)",
                images.len()
            )
        },
    });

    emit_preflight_log(format!(
        "container_ready: probing {} image(s) for availability",
        images.len()
    ));
    let _image_probe_t = Instant::now();
    let mut missing_images = Vec::new();
    for (idx, image) in images.iter().enumerate() {
        if should_emit_image_probe_progress(idx + 1, images.len()) {
            emit_preflight_log(format!(
                "container_ready: image availability {}/{} ({})",
                idx + 1,
                images.len(),
                image
            ));
        }
        if let Err(err) = docker.ensure_image(image) {
            missing_images.push(format!("{} ({})", image, err));
        }
    }
    emit_preflight_log(format!(
        "[PROFILE] container_ready/image_probe ({} images) took {:.3}s",
        images.len(),
        _image_probe_t.elapsed().as_secs_f64()
    ));
    if !missing_images.is_empty() {
        checks.push(PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "container images not available: {}",
                format_preview(&missing_images, 3)
            ),
        });
        return checks;
    }
    checks.push(PreflightCheck {
        name,
        passed: true,
        severity: PreflightSeverity::Error,
        message: format!("all {} task image(s) available for execution", images.len()),
    });

    if skip_idle_probe {
        checks.push(PreflightCheck {
            name,
            passed: true,
            severity: PreflightSeverity::Error,
            message: "idle container probe deferred to benchmark_grader_reachable".to_string(),
        });
        emit_preflight_log(format!(
            "[PROFILE] container_ready/total took {:.3}s",
            _docker_total.elapsed().as_secs_f64()
        ));
        return checks;
    }

    let mut idle_probe_failures = Vec::new();
    emit_preflight_log(format!(
        "container_ready: probing runner idle container command in {} image(s)",
        images.len()
    ));
    let _idle_probe_t = Instant::now();
    let idle_probe_results =
        run_bounded_image_probes(&images, "container_ready/idle_probe", |idx, image| {
            if should_emit_image_probe_progress(idx + 1, images.len()) {
                emit_preflight_log(format!(
                    "container_ready: idle container probe {}/{} ({})",
                    idx + 1,
                    images.len(),
                    image
                ));
            }
            match docker.probe_image_idle_command(image) {
                Ok(()) => None,
                Err(err) => Some(format!("{} ({})", image, err)),
            }
        });
    for failure in idle_probe_results.into_iter().flatten() {
        idle_probe_failures.push(failure);
    }
    emit_preflight_log(format!(
        "[PROFILE] container_ready/idle_probe ({} images) took {:.3}s",
        images.len(),
        _idle_probe_t.elapsed().as_secs_f64()
    ));
    if idle_probe_failures.is_empty() {
        checks.push(PreflightCheck {
            name,
            passed: true,
            severity: PreflightSeverity::Error,
            message: format!(
                "runner idle container command started in all {} task image(s)",
                images.len()
            ),
        });
    } else {
        checks.push(PreflightCheck {
            name,
            passed: false,
            severity: PreflightSeverity::Error,
            message: format!(
                "runner idle container command failed in required images: {} — container execution cannot keep the sandbox alive for exec-based agent/grader steps",
                format_preview(&idle_probe_failures, 3)
            ),
        });
    }

    emit_preflight_log(format!(
        "[PROFILE] container_ready/total took {:.3}s",
        _docker_total.elapsed().as_secs_f64()
    ));
    checks
}

pub(crate) fn resolve_dataset_path(json_value: &Value, exp_dir: &Path) -> Result<PathBuf> {
    let rel = json_value
        .pointer("/matrix/tasks/path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("matrix.tasks.path missing"))?;
    let path = exp_dir.join(rel);
    Ok(path)
}

pub(crate) fn count_tasks(path: &Path, json_value: &Value) -> Result<usize> {
    let limit = json_value
        .pointer("/matrix/tasks/limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    if limit == Some(0) {
        return Ok(0);
    }
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut count = 0usize;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if limit.is_some_and(|max| count >= max) {
            break;
        }
        count += 1;
    }
    Ok(count)
}

pub(crate) struct PreflightProbeRoot {
    path: PathBuf,
}

impl Drop for PreflightProbeRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub(crate) struct PreflightProbeContext {
    _root: PreflightProbeRoot,
    package_root: PathBuf,
    trial_paths: TrialPaths,
    io_paths: PreparedTrialIo,
    dynamic_mounts: Vec<ResolvedMountReference>,
    runtime_env: BTreeMap<String, String>,
    task_image: String,
    task_workdir: String,
    task_materialization_kind: TaskMaterializationKind,
}

pub(crate) fn create_preflight_probe_root(label: &str) -> Result<PreflightProbeRoot> {
    let root = std::env::temp_dir().join(format!(
        "agentlab_preflight_probe_{}_{}_{}",
        sanitize_for_fs(label),
        std::process::id(),
        Utc::now().timestamp_micros()
    ));
    ensure_dir(&root)?;
    Ok(PreflightProbeRoot { path: root })
}

pub(crate) fn select_preflight_probe_task(
    tasks: &[Value],
    image: &str,
) -> Result<(usize, TaskBoundaryMaterialization)> {
    let mut parse_errors = Vec::new();
    for (idx, task) in tasks.iter().enumerate() {
        let boundary = match parse_task_boundary_from_packaged_task(task) {
            Ok(boundary) => boundary,
            Err(err) => {
                parse_errors.push(format!("line {}: {}", idx + 1, err));
                continue;
            }
        };
        if boundary.task_image.as_str() == image {
            return Ok((idx, boundary));
        }
    }
    if !parse_errors.is_empty() {
        return Err(anyhow!(
            "failed to parse representative task boundary rows: {}",
            format_preview(&parse_errors, 3)
        ));
    }
    Err(anyhow!(
        "no representative task row found for image '{}'",
        image
    ))
}

pub(crate) fn build_preflight_probe_context(
    runtime_profile: &VariantRuntimeProfile,
    variant: &Variant,
    tasks: &[Value],
    image: &str,
    package_root: &Path,
    _project_root: &Path,
) -> Result<PreflightProbeContext> {
    let (task_idx, task_boundary) = match select_preflight_probe_task(tasks, image) {
        Ok(selected) => selected,
        Err(err) if tasks.is_empty() => (
            0,
            TaskBoundaryMaterialization {
                declaration: json!({
                    "schema_version": "task_row_v2",
                    "id": "preflight_probe_task",
                    "task": {},
                    "runtime": {
                        "container_image": {
                            "image": image,
                            "workdir": "/workspace/task"
                        }
                    }
                }),
                task_payload: json!({}),
                workspace: WorkspaceSpec {
                    mode: WorkspaceMode::Scratch,
                    base: WorkspaceBaseSpec {
                        kind: WorkspaceBaseKind::Empty,
                        dataset_pack_ref: None,
                        repo: None,
                        commit: None,
                    },
                    overlays: Vec::new(),
                    aux_mounts: Vec::new(),
                },
                dependencies: json!({}),
                materialization: TaskMaterializationSpec {
                    kind: TaskMaterializationKind::TaskImage,
                    task_bundle_ref: None,
                    platform: None,
                },
                case_materialization: Vec::new(),
                task_id: "preflight_probe_task".to_string(),
                task_image: image.to_string(),
                task_workdir: "/workspace/task".to_string(),
                time_limit_ms: None,
            },
        ),
        Err(err) => return Err(err),
    };
    let probe_root = create_preflight_probe_root(&format!("{}_{}", variant.id, image))?;
    let trial_dir = probe_root.path.join("trial_1");
    ensure_dir(&trial_dir)?;
    let prepared = prepare_task_environment(
        package_root,
        &trial_dir,
        "preflight_probe",
        "trial_preflight",
        &runtime_profile.experiment,
        variant,
        task_idx,
        0,
        &task_boundary,
        &runtime_profile.agent_runtime,
    )?;
    let probe_task_image = prepared.manifest.task_sandbox_image()?.to_string();
    let probe_task_workdir = prepared.manifest.task_sandbox_workdir()?.to_string();
    let mut input = prepared.trial_input.clone();
    set_json_pointer_value(
        &mut input,
        "/runtime/control_plane/path",
        json!(DEFAULT_CONTAINER_CONTROL_PATH),
    )?;
    set_json_pointer_value(&mut input, "/runtime/control_plane/mode", json!("file"))?;
    let input_bytes = serde_json::to_vec_pretty(&input)?;
    let io_paths = prepare_io_paths(&prepared.trial_paths, &input_bytes)?;
    let smoke_timeout_ms = resolve_trial_timeout_ms(&input)
        .map(|value| value.min(DEFAULT_PREFLIGHT_CONTRACT_SMOKE_TIMEOUT_MS));
    let mut runtime_env = build_runtime_contract_env(
        "preflight_probe",
        &input,
        &io_paths,
        Some(probe_task_image.as_str()),
        smoke_timeout_ms,
    );
    runtime_env.insert(AGENTLAB_ENV_PREFLIGHT_SMOKE.to_string(), "1".to_string());
    Ok(PreflightProbeContext {
        _root: probe_root,
        package_root: package_root.to_path_buf(),
        trial_paths: prepared.trial_paths,
        io_paths,
        dynamic_mounts: prepared.dynamic_mounts,
        runtime_env,
        task_image: probe_task_image,
        task_workdir: probe_task_workdir,
        task_materialization_kind: task_boundary.materialization.kind.clone(),
    })
}

pub(crate) fn build_preflight_probe_request<'a>(
    context: &'a PreflightProbeContext,
    runtime_profile: &'a VariantRuntimeProfile,
    benchmark_grader: Option<&'a BenchmarkGraderConfig>,
    benchmark_grading_enabled: bool,
) -> AdapterRunRequest<'a> {
    AdapterRunRequest {
        package_root: &context.package_root,
        runtime_experiment: &runtime_profile.experiment,
        runtime: &runtime_profile.agent_runtime,
        variant_args: &runtime_profile.variant_args,
        runtime_env: &context.runtime_env,
        runtime_overrides_env: &runtime_profile.agent_runtime_env,
        trial_paths: &context.trial_paths,
        dynamic_mounts: &context.dynamic_mounts,
        secret_file_mounts: &runtime_profile.secret_file_mounts,
        io_paths: &context.io_paths,
        network_mode: runtime_profile.effective_network_mode.as_str(),
        benchmark_grader,
        benchmark_grading_enabled,
        run_id: "preflight_probe",
        task_image: context.task_image.as_str(),
        task_workdir: context.task_workdir.as_str(),
        task_materialization_kind: context.task_materialization_kind.clone(),
        agent_artifact: runtime_profile.agent_runtime.agent_artifact.as_deref(),
        agent_artifact_mount_path: runtime_profile
            .agent_runtime
            .agent_artifact_mount_path
            .as_deref(),
        agent_artifact_read_only: runtime_profile.agent_runtime.agent_artifact_read_only,
    }
}

pub(crate) struct PreflightContractSmokeExecution {
    status: String,
    stdout: String,
    stderr: String,
}

pub(crate) fn read_optional_text_file(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    Ok(fs::read_to_string(path)?)
}

pub(crate) fn run_preflight_contract_smoke(
    request: &AdapterRunRequest<'_>,
) -> Result<PreflightContractSmokeExecution> {
    let prepared_manifest =
        load_prepared_task_environment_manifest(&request.trial_paths.trial_dir)?;
    let executor = LocalDockerExecutionBackend::new();
    let runtime_outcome = executor.execute_attempt(TrialRuntimeExecutionRequest {
        trial_dir: &request.trial_paths.trial_dir,
        schedule_idx: 0,
        attempt_no: 1,
        adapter: request,
        task_id: &prepared_manifest.task_id,
        variant_id: &prepared_manifest.variant_id,
        repl_idx: prepared_manifest.repl_idx,
        task_sandbox_plan: prepared_manifest
            .task_sandbox_plan
            .as_ref()
            .ok_or_else(|| anyhow!("preflight probe missing task sandbox plan"))?,
    })?;
    let stdout = read_optional_text_file(&trial_agent_stdout_path(&request.trial_paths.trial_dir))?;
    let stderr = read_optional_text_file(&trial_agent_stderr_path(&request.trial_paths.trial_dir))?;
    Ok(PreflightContractSmokeExecution {
        status: runtime_outcome.agent_exit_status,
        stdout,
        stderr,
    })
}

pub(crate) fn detect_known_probe_output_blockers(stdout: &str, stderr: &str) -> Vec<String> {
    let mut blockers = Vec::new();
    for line in stdout.lines().chain(stderr.lines()) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("cpu lacks avx support") {
            blockers.push(trimmed.to_string());
            continue;
        }
        if lower.contains("missing env var ") || lower.contains("fatal error: missing env var") {
            blockers.push(trimmed.to_string());
            continue;
        }
        if lower.contains("references tool") && lower.contains("which is not available") {
            blockers.push(trimmed.to_string());
        }
    }
    blockers.sort();
    blockers.dedup();
    blockers
}

pub(crate) fn summarize_preflight_failure_logs(stdout: &str, stderr: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in stderr.lines().chain(stdout.lines()) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        lines.push(trimmed.to_string());
        if lines.len() >= 3 {
            break;
        }
    }
    lines
}

pub(crate) fn validate_preflight_result_payload(path: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    if !path.exists() {
        failures.push(format!(
            "contract smoke did not write result payload: {}",
            path.display()
        ));
        return failures;
    }
    if !path.is_file() {
        failures.push(format!(
            "contract smoke result payload path is not a file: {}",
            path.display()
        ));
        return failures;
    }
    let raw = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(err) => {
            failures.push(format!(
                "failed to read contract smoke result payload at {}: {}",
                path.display(),
                err
            ));
            return failures;
        }
    };
    if raw.trim().is_empty() {
        failures.push(format!(
            "contract smoke wrote an empty result payload: {}",
            path.display()
        ));
        return failures;
    }
    let value = match serde_json::from_str::<Value>(&raw) {
        Ok(value) => value,
        Err(err) => {
            failures.push(format!(
                "failed to parse agent result JSON at {}: {}",
                path.display(),
                err
            ));
            return failures;
        }
    };
    let _ = value;
    failures
}

pub(crate) fn validate_preflight_benchmark_smoke_outputs(
    request: &AdapterRunRequest<'_>,
    status: &str,
) -> Vec<String> {
    let mut failures = Vec::new();
    let mapped_grader_output_path = request.trial_paths.out.join(MAPPED_GRADER_OUTPUT_FILENAME);
    let grade_error_path = request.trial_paths.out.join(BENCHMARK_GRADE_ERROR_FILENAME);

    let mapped_output_valid = match load_optional_json_record_with_schema(
        "trial_conclusion_v1.jsonschema",
        &mapped_grader_output_path,
    ) {
        Ok(Some(_)) => true,
        Ok(None) => {
            failures.push(format!(
                "contract smoke did not write mapped grader output: {}",
                mapped_grader_output_path.display()
            ));
            false
        }
        Err(err) => {
            failures.push(format!("mapped grader output invalid: {}", err));
            false
        }
    };
    if !mapped_output_valid && grade_error_path.exists() {
        let marker_reason =
            fs::read_to_string(&grade_error_path).unwrap_or_else(|_| "grade_error".to_string());
        let reason = marker_reason.trim();
        failures.push(format!(
            "benchmark smoke recorded grade error: {}",
            if reason.is_empty() {
                "grade_error"
            } else {
                reason
            }
        ));
    } else if !mapped_output_valid && status == BENCHMARK_GRADING_POLICY_EXIT_CODE.to_string() {
        failures.push(format!(
            "benchmark smoke exited with grading policy code {} without a grade error marker",
            BENCHMARK_GRADING_POLICY_EXIT_CODE
        ));
    }
    failures
}

pub(crate) fn collect_preflight_contract_smoke_failures(
    request: &AdapterRunRequest<'_>,
    execution: &PreflightContractSmokeExecution,
) -> Vec<String> {
    let mut failures = detect_known_probe_output_blockers(&execution.stdout, &execution.stderr);
    let log_summaries = summarize_preflight_failure_logs(&execution.stdout, &execution.stderr);
    let mut contract_failed = false;
    if execution.status != "0" {
        failures.push(format!(
            "contract smoke exited with status {}",
            execution.status
        ));
        contract_failed = true;
    }
    let result_failures = validate_preflight_result_payload(&request.io_paths.result_host);
    if !result_failures.is_empty() {
        contract_failed = true;
    }
    failures.extend(result_failures);
    if contract_failed {
        failures.extend(log_summaries.clone());
    }
    if request.benchmark_grading_enabled {
        let benchmark_failures =
            validate_preflight_benchmark_smoke_outputs(request, &execution.status);
        failures.extend(benchmark_failures);
    }
    let mut seen = HashSet::new();
    failures
        .into_iter()
        .filter(|failure| seen.insert(failure.clone()))
        .collect()
}
