use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use lab_core::{canonical_json_digest, ensure_dir};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{atomic_write_json_pretty, load_run_variants, load_tasks};
use crate::experiment::runtime::{
    resolve_variant_runtime_profile, validate_agent_artifact_pin, VariantRuntimeProfile,
};
use crate::experiment::state::{RunBehavior, RunExecutionOptions};
use crate::model::PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION;
use crate::package::authoring::compute_artifact_content_digest;
use crate::package::sealed::{load_sealed_package_for_run, resolve_dataset_path_in_package};
use crate::trial::prepare::PREPARED_RUNTIME_IMAGE_MAP_PACKAGE_REL_PATH;
use crate::trial::spec::parse_task_boundary_from_packaged_task;
use crate::util::{remove_path_if_exists, sanitize_for_fs};

const PREPARED_RUNTIME_IMAGE_MAP_SCHEMA_VERSION: &str = "prepared_runtime_image_map_v1";

#[derive(Debug, Clone)]
pub struct PreparedRuntimeImageOptions {
    pub repository: String,
    pub out: Option<PathBuf>,
    pub push: bool,
    pub dry_run: bool,
    pub skip_existing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedRuntimeImageMapEntry {
    pub base_image: String,
    pub agent_artifact_digest: String,
    pub agent_artifact_mount_path: String,
    pub runner_contract_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    pub prepared_image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedRuntimeImageMapFile {
    pub schema_version: String,
    pub generated_at: String,
    pub entries: Vec<PreparedRuntimeImageMapEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreparedRuntimeImageReport {
    pub map_path: PathBuf,
    pub entries: Vec<PreparedRuntimeImageMapEntry>,
    pub built: usize,
    pub skipped: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PreparedRuntimeImageKey {
    base_image: String,
    platform: Option<String>,
    agent_artifact: PathBuf,
    agent_artifact_digest: String,
    agent_artifact_mount_path: String,
    runner_contract_version: String,
}

#[derive(Debug, Clone)]
struct PreparedRuntimeImagePlan {
    key: PreparedRuntimeImageKey,
    prepared_image: String,
}

trait PreparedRuntimeImageBuilder {
    fn image_exists(&self, image: &str) -> Result<bool>;
    fn build(&mut self, plan: &PreparedRuntimeImagePlan, push: bool) -> Result<()>;
}

struct DockerPreparedRuntimeImageBuilder;

impl PreparedRuntimeImageBuilder for DockerPreparedRuntimeImageBuilder {
    fn image_exists(&self, image: &str) -> Result<bool> {
        let local_status = Command::new("docker")
            .arg("image")
            .arg("inspect")
            .arg(image)
            .status()
            .with_context(|| format!("failed to run docker image inspect for {}", image))?;
        if local_status.success() {
            return Ok(true);
        }
        let status = Command::new("docker")
            .arg("manifest")
            .arg("inspect")
            .arg(image)
            .status()
            .with_context(|| format!("failed to run docker manifest inspect for {}", image))?;
        Ok(status.success())
    }

    fn build(&mut self, plan: &PreparedRuntimeImagePlan, push: bool) -> Result<()> {
        build_prepared_runtime_image_with_docker(plan, push)
    }
}

fn normalize_repository(repository: &str) -> Result<String> {
    let trimmed = repository
        .trim()
        .trim_end_matches(':')
        .trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(anyhow!("prepared runtime image repository is required"));
    }
    if trimmed.contains(char::is_whitespace) {
        return Err(anyhow!(
            "prepared runtime image repository must not contain whitespace: {}",
            repository
        ));
    }
    Ok(trimmed.to_string())
}

fn normalize_optional_nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn digest_hex(digest: &str) -> String {
    digest
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(digest.trim())
        .to_string()
}

fn prepared_image_ref(repository: &str, key: &PreparedRuntimeImageKey) -> String {
    let key_digest = canonical_json_digest(&json!({
        "base_image": key.base_image,
        "platform": key.platform,
        "agent_artifact_digest": key.agent_artifact_digest,
        "agent_artifact_mount_path": key.agent_artifact_mount_path,
        "runner_contract_version": key.runner_contract_version,
    }));
    let hex = digest_hex(&key_digest);
    format!("{}:prepared-{}", repository, &hex[..24])
}

fn prepared_image_map_path(package_dir: &Path, out: Option<&Path>) -> PathBuf {
    out.map(Path::to_path_buf)
        .unwrap_or_else(|| package_dir.join(PREPARED_RUNTIME_IMAGE_MAP_PACKAGE_REL_PATH))
}

fn runtime_profile_agent_key(
    profile: &VariantRuntimeProfile,
) -> Result<Option<(PathBuf, String, String)>> {
    let Some(agent_artifact) = profile.agent_runtime.agent_artifact.as_ref() else {
        return Ok(None);
    };
    let Some(mount_path) = profile.agent_runtime.agent_artifact_mount_path.as_ref() else {
        return Err(anyhow!(
            "trial_runtime.agent.mount.mount path is required when preparing runtime images"
        ));
    };
    validate_agent_artifact_pin(&profile.agent_runtime)?;
    let digest = if let Some(digest) = profile
        .agent_runtime
        .agent_artifact_digest
        .as_deref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        digest
    } else {
        compute_artifact_content_digest(agent_artifact).with_context(|| {
            format!(
                "failed to compute agent artifact digest for {}",
                agent_artifact.display()
            )
        })?
    };
    Ok(Some((
        agent_artifact.clone(),
        digest,
        mount_path.trim().to_string(),
    )))
}

fn collect_prepared_runtime_image_plans(
    package_dir: &Path,
    repository: &str,
) -> Result<Vec<PreparedRuntimeImagePlan>> {
    let loaded = load_sealed_package_for_run(package_dir)?;
    let dataset_path = resolve_dataset_path_in_package(&loaded.json_value, &loaded.exp_dir)?;
    let tasks = load_tasks(&dataset_path, &loaded.json_value)?;
    let (variants, _) = load_run_variants(&loaded.exp_dir, &loaded.json_value)?;
    let behavior = RunBehavior::default();
    let execution = RunExecutionOptions::default();
    let mut plans = BTreeMap::new();
    for variant in &variants {
        let profile = resolve_variant_runtime_profile(
            &loaded.json_value,
            variant,
            &loaded.exp_dir,
            &behavior,
            &execution,
        )
        .with_context(|| {
            format!(
                "failed to resolve runtime profile for variant '{}'",
                variant.id
            )
        })?;
        let Some((agent_artifact, agent_artifact_digest, agent_artifact_mount_path)) =
            runtime_profile_agent_key(&profile)?
        else {
            continue;
        };
        for task in &tasks {
            let boundary = parse_task_boundary_from_packaged_task(task)?;
            let key = PreparedRuntimeImageKey {
                base_image: boundary.task_image,
                platform: normalize_optional_nonempty(boundary.materialization.platform.as_deref()),
                agent_artifact: agent_artifact.clone(),
                agent_artifact_digest: agent_artifact_digest.clone(),
                agent_artifact_mount_path: agent_artifact_mount_path.clone(),
                runner_contract_version: PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION.to_string(),
            };
            let prepared_image = prepared_image_ref(repository, &key);
            plans
                .entry(key.clone())
                .or_insert(PreparedRuntimeImagePlan {
                    key,
                    prepared_image,
                });
        }
    }
    Ok(plans.into_values().collect())
}

fn map_entries_from_plans(plans: &[PreparedRuntimeImagePlan]) -> Vec<PreparedRuntimeImageMapEntry> {
    plans
        .iter()
        .map(|plan| PreparedRuntimeImageMapEntry {
            base_image: plan.key.base_image.clone(),
            agent_artifact_digest: plan.key.agent_artifact_digest.clone(),
            agent_artifact_mount_path: plan.key.agent_artifact_mount_path.clone(),
            runner_contract_version: plan.key.runner_contract_version.clone(),
            platform: plan.key.platform.clone(),
            prepared_image: plan.prepared_image.clone(),
        })
        .collect()
}

fn write_prepared_runtime_image_map(
    path: &Path,
    entries: Vec<PreparedRuntimeImageMapEntry>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let map = PreparedRuntimeImageMapFile {
        schema_version: PREPARED_RUNTIME_IMAGE_MAP_SCHEMA_VERSION.to_string(),
        generated_at: Utc::now().to_rfc3339(),
        entries,
    };
    atomic_write_json_pretty(path, &serde_json::to_value(map)?)?;
    Ok(())
}

fn dockerfile_for_plan(plan: &PreparedRuntimeImagePlan, context_is_directory: bool) -> String {
    let mount_path = format!(
        "{}/",
        plan.key.agent_artifact_mount_path.trim_end_matches('/')
    );
    let source_line = if context_is_directory {
        format!(
            "COPY {}\n",
            serde_json::to_string(&vec![".", mount_path.as_str()]).expect("dockerfile copy json")
        )
    } else {
        format!(
            "ADD {}\n",
            serde_json::to_string(&vec!["agent_artifact", mount_path.as_str()])
                .expect("dockerfile add json")
        )
    };
    format!(
        concat!(
            "FROM {}\n",
            "{}",
            "LABEL org.bucephalus.prepared_runtime_image.contract={}\n",
            "LABEL org.bucephalus.prepared_runtime_image.base={}\n",
            "LABEL org.bucephalus.prepared_runtime_image.agent_digest={}\n",
            "LABEL org.bucephalus.prepared_runtime_image.agent_mount={}\n"
        ),
        plan.key.base_image,
        source_line,
        serde_json::to_string(&plan.key.runner_contract_version).expect("label json"),
        serde_json::to_string(&plan.key.base_image).expect("label json"),
        serde_json::to_string(&plan.key.agent_artifact_digest).expect("label json"),
        serde_json::to_string(&plan.key.agent_artifact_mount_path).expect("label json")
    )
}

fn hard_link_or_copy(source: &Path, dest: &Path) -> Result<()> {
    if fs::hard_link(source, dest).is_ok() {
        return Ok(());
    }
    fs::copy(source, dest).with_context(|| {
        format!(
            "failed to stage agent artifact {} into docker build context {}",
            source.display(),
            dest.display()
        )
    })?;
    Ok(())
}

fn build_prepared_runtime_image_with_docker(
    plan: &PreparedRuntimeImagePlan,
    push: bool,
) -> Result<()> {
    let artifact = &plan.key.agent_artifact;
    if !artifact.exists() {
        return Err(anyhow!(
            "agent artifact for prepared runtime image is missing: {}",
            artifact.display()
        ));
    }
    let context_is_directory = artifact.is_dir();
    let scratch = std::env::temp_dir().join(format!(
        "bucephalus-prepared-runtime-image-{}-{}",
        std::process::id(),
        sanitize_for_fs(&plan.prepared_image)
    ));
    remove_path_if_exists(&scratch)?;
    ensure_dir(&scratch)?;
    let dockerfile = scratch.join("Dockerfile");
    fs::write(&dockerfile, dockerfile_for_plan(plan, context_is_directory))?;
    let context_dir = if context_is_directory {
        artifact.clone()
    } else {
        let staged = scratch.join("agent_artifact");
        hard_link_or_copy(artifact, &staged)?;
        scratch.clone()
    };
    let mut build = Command::new("docker");
    build.arg("build");
    if let Some(platform) = plan.key.platform.as_deref() {
        build.arg("--platform").arg(platform);
    }
    build
        .arg("--pull")
        .arg("-t")
        .arg(&plan.prepared_image)
        .arg("-f")
        .arg(&dockerfile)
        .arg(&context_dir);
    let status = build.status().with_context(|| {
        format!(
            "failed to launch docker build for prepared runtime image {}",
            plan.prepared_image
        )
    })?;
    if !status.success() {
        return Err(anyhow!(
            "docker build failed for prepared runtime image {}",
            plan.prepared_image
        ));
    }
    if push {
        let status = Command::new("docker")
            .arg("push")
            .arg(&plan.prepared_image)
            .status()
            .with_context(|| {
                format!(
                    "failed to launch docker push for prepared runtime image {}",
                    plan.prepared_image
                )
            })?;
        if !status.success() {
            return Err(anyhow!(
                "docker push failed for prepared runtime image {}",
                plan.prepared_image
            ));
        }
    }
    remove_path_if_exists(&scratch)?;
    Ok(())
}

fn prepare_runtime_images_with_builder<B: PreparedRuntimeImageBuilder>(
    package_dir: &Path,
    options: &PreparedRuntimeImageOptions,
    builder: &mut B,
) -> Result<PreparedRuntimeImageReport> {
    let repository = normalize_repository(&options.repository)?;
    let map_path = prepared_image_map_path(package_dir, options.out.as_deref());
    let plans = collect_prepared_runtime_image_plans(package_dir, &repository)?;
    if plans.is_empty() {
        return Err(anyhow!(
            "no agent artifact mounts were found; prepared runtime images are only needed for trial_runtime.agent.mount"
        ));
    }
    let mut built = 0;
    let mut skipped = 0;
    if !options.dry_run {
        for plan in &plans {
            if options.skip_existing && builder.image_exists(&plan.prepared_image)? {
                skipped += 1;
                continue;
            }
            builder.build(plan, options.push)?;
            built += 1;
        }
    }
    let entries = map_entries_from_plans(&plans);
    write_prepared_runtime_image_map(&map_path, entries.clone())?;
    Ok(PreparedRuntimeImageReport {
        map_path,
        entries,
        built,
        skipped,
        dry_run: options.dry_run,
    })
}

pub fn prepare_runtime_images(
    package_dir: &Path,
    options: PreparedRuntimeImageOptions,
) -> Result<PreparedRuntimeImageReport> {
    let mut builder = DockerPreparedRuntimeImageBuilder;
    prepare_runtime_images_with_builder(package_dir, &options, &mut builder)
}

#[cfg(test)]
pub(crate) fn load_prepared_runtime_image_map_for_test(
    path: &Path,
) -> Result<PreparedRuntimeImageMapFile> {
    serde_json::from_value(crate::config::load_json_file(path)?).map_err(Into::into)
}
