use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use lab_core::{canonical_json_digest, ensure_dir};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{atomic_write_json_pretty, load_run_variants, load_tasks};
use crate::experiment::runtime::{
    resolve_variant_runtime_profile, validate_agent_artifact_pin, VariantRuntimeProfile,
};
use crate::experiment::state::{RunBehavior, RunExecutionOptions};
use crate::model::{ExecutorKind, PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION};
use crate::package::authoring::{compute_artifact_content_digest, public_agent_artifact_ref};
use crate::package::sealed::{load_sealed_package_for_run, resolve_dataset_path_in_package};
use crate::trial::prepare::PREPARED_RUNTIME_IMAGE_MAP_PACKAGE_REL_PATH;
use crate::trial::spec::parse_task_boundary_from_packaged_task;
use crate::util::{remove_path_if_exists, sanitize_for_fs};

const PREPARED_RUNTIME_IMAGE_MAP_SCHEMA_VERSION: &str = "prepared_runtime_image_map_v1";
static PREPARED_RUNTIME_BUILD_CONTEXT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

#[cfg(test)]
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
            "prepared runtime image repository must not contain whitespace"
        ));
    }
    if trimmed.contains("://") {
        return Err(anyhow!(
            "prepared runtime image repository must be a Docker image repository prefix, not a URL"
        ));
    }
    if trimmed.contains('@') || trimmed.contains('?') || trimmed.contains('#') {
        return Err(anyhow!(
            "prepared runtime image repository must be a repository prefix without credentials, digests, query strings, or fragments"
        ));
    }
    if repository_has_tag(trimmed) {
        return Err(anyhow!(
            "prepared runtime image repository must be a repository prefix without a tag"
        ));
    }
    if looks_like_local_repository_path(trimmed) {
        return Err(anyhow!(
            "prepared runtime image repository must be an image repository, not a local filesystem path"
        ));
    }
    Ok(trimmed.to_string())
}

fn repository_has_tag(repository: &str) -> bool {
    let name_start = repository.rfind('/').map(|idx| idx + 1).unwrap_or(0);
    repository[name_start..].contains(':')
}

fn looks_like_local_repository_path(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("/Users/")
        || trimmed.starts_with("/home/")
        || trimmed.starts_with("/private/")
        || trimmed.starts_with("/tmp/")
        || trimmed.starts_with("/var/folders/")
        || trimmed.starts_with("/Volumes/")
        || trimmed.starts_with("~/")
        || trimmed.starts_with("~\\")
        || looks_like_windows_drive_path(trimmed)
        || looks_like_wsl_user_path(trimmed)
}

fn looks_like_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
        && !matches!(bytes[3], b'\\' | b'/')
}

fn looks_like_wsl_user_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() > 13
        && value.starts_with("/mnt/")
        && bytes[5].is_ascii_alphabetic()
        && bytes[6] == b'/'
        && value[7..].to_ascii_lowercase().starts_with("users/")
}

fn normalize_optional_nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn prepared_image_ref(repository: &str, key: &PreparedRuntimeImageKey) -> String {
    let key_digest = canonical_json_digest(&json!({
        "base_image": key.base_image,
        "platform": key.platform,
        "agent_artifact_digest": key.agent_artifact_digest,
        "agent_artifact_mount_path": key.agent_artifact_mount_path,
        "runner_contract_version": key.runner_contract_version,
    }));
    let hex = key_digest
        .strip_prefix("sha256:")
        .expect("canonical_json_digest returns a sha256-prefixed digest");
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
                "failed to compute agent artifact digest\n\nartifact_ref: {}",
                public_agent_artifact_ref(agent_artifact)
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
    let execution = RunExecutionOptions {
        executor: Some(ExecutorKind::LocalDocker),
        ..Default::default()
    };
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
    entries: &[PreparedRuntimeImageMapEntry],
) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    atomic_write_json_pretty(
        path,
        &json!({
            "schema_version": PREPARED_RUNTIME_IMAGE_MAP_SCHEMA_VERSION,
            "generated_at": Utc::now().to_rfc3339(),
            "entries": entries,
        }),
    )?;
    Ok(())
}

fn dockerfile_for_plan(
    plan: &PreparedRuntimeImagePlan,
    context_is_directory: bool,
) -> Result<String> {
    let mount_path = format!(
        "{}/",
        plan.key.agent_artifact_mount_path.trim_end_matches('/')
    );
    let source_line = if context_is_directory {
        format!(
            "COPY {}\n",
            serde_json::to_string(&vec![".", mount_path.as_str()])
                .context("serialize Dockerfile COPY instruction")?
        )
    } else {
        format!(
            "ADD {}\n",
            serde_json::to_string(&vec!["agent_artifact", mount_path.as_str()])
                .context("serialize Dockerfile ADD instruction")?
        )
    };
    Ok(format!(
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
        serde_json::to_string(&plan.key.runner_contract_version)
            .context("serialize prepared runtime contract label")?,
        serde_json::to_string(&plan.key.base_image)
            .context("serialize prepared runtime base label")?,
        serde_json::to_string(&plan.key.agent_artifact_digest)
            .context("serialize prepared runtime artifact digest label")?,
        serde_json::to_string(&plan.key.agent_artifact_mount_path)
            .context("serialize prepared runtime artifact mount label")?
    ))
}

fn public_prepared_runtime_build_context_ref(_path: &Path) -> &'static str {
    "build-context://agent-artifact"
}

struct PreparedRuntimeBuildScratch {
    path: PathBuf,
    remove_on_drop: bool,
}

impl PreparedRuntimeBuildScratch {
    fn create(prepared_image: &str) -> Result<Self> {
        let seq = PREPARED_RUNTIME_BUILD_CONTEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bucephalus-prepared-runtime-image-{}-{}-{}",
            std::process::id(),
            seq,
            sanitize_for_fs(prepared_image)
        ));
        remove_path_if_exists(&path).with_context(|| {
            format!(
                "failed to reset prepared runtime image build context\n\ncontext_ref: {}",
                public_prepared_runtime_build_context_ref(&path)
            )
        })?;
        ensure_dir(&path).with_context(|| {
            format!(
                "failed to create prepared runtime image build context\n\ncontext_ref: {}",
                public_prepared_runtime_build_context_ref(&path)
            )
        })?;
        Ok(Self {
            path,
            remove_on_drop: true,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(mut self) -> Result<()> {
        let result = remove_path_if_exists(&self.path).with_context(|| {
            format!(
                "failed to clean prepared runtime image build context\n\ncontext_ref: {}",
                public_prepared_runtime_build_context_ref(&self.path)
            )
        });
        if result.is_ok() {
            self.remove_on_drop = false;
        }
        result
    }
}

impl Drop for PreparedRuntimeBuildScratch {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = remove_path_if_exists(&self.path);
        }
    }
}

pub(crate) fn ensure_prepared_runtime_agent_artifact_exists(artifact: &Path) -> Result<()> {
    if artifact.exists() {
        return Ok(());
    }
    Err(anyhow!(
        "agent artifact for prepared runtime image is missing\n\nartifact_ref: {}",
        public_agent_artifact_ref(artifact)
    ))
}

pub(crate) fn hard_link_or_copy(source: &Path, dest: &Path) -> Result<()> {
    if fs::hard_link(source, dest).is_ok() {
        return Ok(());
    }
    fs::copy(source, dest).with_context(|| {
        format!(
            "failed to stage agent artifact into docker build context\n\nartifact_ref: {}\ncontext_ref: {}",
            public_agent_artifact_ref(source),
            public_prepared_runtime_build_context_ref(dest)
        )
    })?;
    Ok(())
}

fn build_prepared_runtime_image_with_docker(
    plan: &PreparedRuntimeImagePlan,
    push: bool,
) -> Result<()> {
    let artifact = &plan.key.agent_artifact;
    ensure_prepared_runtime_agent_artifact_exists(artifact)?;
    let context_is_directory = artifact.is_dir();
    let scratch = PreparedRuntimeBuildScratch::create(&plan.prepared_image)?;
    let dockerfile = scratch.path().join("Dockerfile");
    fs::write(
        &dockerfile,
        dockerfile_for_plan(plan, context_is_directory)?,
    )?;
    let context_dir = if context_is_directory {
        artifact.as_path()
    } else {
        let staged = scratch.path().join("agent_artifact");
        hard_link_or_copy(artifact, &staged)?;
        scratch.path()
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
        .arg(context_dir);
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
    scratch.cleanup()?;
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
    write_prepared_runtime_image_map(&map_path, &entries)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_runtime_repository_validation_keeps_errors_public() {
        for raw in [
            "registry.example/acme/repo token=raw-repository-token",
            "https://user:secret@example.com/acme/repo?token=raw-query#frag",
            "registry.example/acme/repo@sha256:abc",
            "registry.example/acme/repo:latest",
            "/Users/alice/private/repo",
            "C:\\Users\\Alice\\private\\repo",
            "/mnt/c/Users/Alice/private/repo",
            "~/private/repo",
        ] {
            let err = normalize_repository(raw).expect_err("repository should be rejected");
            let msg = err.to_string();
            assert!(msg.contains("prepared runtime image repository"));
            for forbidden in [
                "raw-repository-token",
                "user:secret",
                "raw-query",
                "/Users/alice",
                "C:\\Users\\Alice",
                "/mnt/c/Users/Alice",
                "~/private",
            ] {
                assert!(
                    !msg.contains(forbidden),
                    "repository error leaked forbidden text: {forbidden}\n{msg}"
                );
            }
        }
    }

    #[test]
    fn prepared_runtime_repository_allows_registry_port_prefixes() {
        let repository = normalize_repository(" localhost:5000/acme/prepared/ ")
            .expect("localhost registry port should be allowed");

        assert_eq!(repository, "localhost:5000/acme/prepared");
    }

    #[test]
    fn prepared_runtime_build_scratch_is_unique_and_cleans_after_error() {
        let mut leaked_path = None;
        let result: Result<()> = (|| {
            let scratch =
                PreparedRuntimeBuildScratch::create("registry.example/repo:prepared-test")?;
            let scratch_path = scratch.path().to_path_buf();
            fs::write(scratch_path.join("agent_artifact"), b"private agent bytes")?;
            leaked_path = Some(scratch_path);
            Err(anyhow!("simulated docker build failure"))
        })();

        let leaked_path = leaked_path.expect("scratch path");
        assert!(result.is_err());
        assert!(
            !leaked_path.exists(),
            "prepared runtime build scratch was not cleaned after error: {}",
            leaked_path.display()
        );

        let first = PreparedRuntimeBuildScratch::create("registry.example/repo:prepared-test")
            .expect("first scratch");
        let second = PreparedRuntimeBuildScratch::create("registry.example/repo:prepared-test")
            .expect("second scratch");
        let first_path = first.path().to_path_buf();
        let second_path = second.path().to_path_buf();

        assert_ne!(first_path, second_path);
        first.cleanup().expect("cleanup first scratch");
        second.cleanup().expect("cleanup second scratch");
        assert!(!first_path.exists());
        assert!(!second_path.exists());
    }
}
