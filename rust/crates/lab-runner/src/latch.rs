use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use lab_core::{
    ensure_dir, sha256_bytes, BUCEPHALUS_ENV_CASE_ID, BUCEPHALUS_ENV_RESULT_PATH,
    BUCEPHALUS_ENV_TASK_ID, BUCEPHALUS_ENV_TRAJECTORY_PATH, BUCEPHALUS_ENV_TRIAL_INPUT_PATH,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::local_storage::default_run_root;
use crate::trial::materialization::{
    CaseMaterializationPhase, HostCaseMaterializationExecutor, HostCaseMaterializationRequest,
};
use crate::trial::spec::CaseMaterializationStepPlan;
use crate::util::{copy_dir_preserve_contents, copy_file_if_exists, sanitize_for_fs};

const LATCH_MANIFEST_SCHEMA_VERSION: &str = "latch_manifest_v1";
const LATCH_RESULT_SCHEMA_VERSION: &str = "latch_result_v1";
const DEFAULT_WALL_TIMEOUT_SECONDS: u64 = 15 * 60;
const WORKSPACE_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub const LATCH_MANIFEST_SCHEMA: &str = LATCH_MANIFEST_SCHEMA_VERSION;
pub const LATCH_RESULT_SCHEMA: &str = LATCH_RESULT_SCHEMA_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatchManifest {
    #[serde(default = "default_latch_manifest_schema_version")]
    pub schema_version: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub defaults: LatchDefaults,
    pub cases: Vec<LatchCaseManifest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatchDefaults {
    #[serde(default)]
    pub launch: Option<LaunchSpec>,
    #[serde(default)]
    pub workspace_seed: Option<WorkspaceSeed>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub idle_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub grader: Option<LatchGraderSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatchCaseManifest {
    pub case_id: String,
    pub task_prompt: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub workspace_seed: Option<WorkspaceSeed>,
    #[serde(default)]
    pub expected_output: Option<ExpectedOutput>,
    #[serde(default)]
    pub upload: Option<UploadSpec>,
    #[serde(default)]
    pub grader: Option<LatchGraderSpec>,
    #[serde(default)]
    pub materialization: Vec<CaseMaterializationStepPlan>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceSeed {
    Empty,
    Files {
        #[serde(default)]
        path: Option<String>,
        #[serde(default, rename = "ref")]
        ref_path: Option<String>,
    },
    Archive {
        #[serde(default)]
        path: Option<String>,
        #[serde(default, rename = "ref")]
        ref_path: Option<String>,
    },
    Git {
        repo: String,
        #[serde(default)]
        rev: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedOutput {
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LatchGraderSpec {
    Diff {
        #[serde(default)]
        requires: Vec<LatchRequirement>,
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        expected_empty: Option<bool>,
    },
    Match {
        #[serde(default)]
        requires: Vec<LatchRequirement>,
        path: String,
        #[serde(default)]
        expected: Option<String>,
        #[serde(default)]
        contains: Option<String>,
    },
    Regex {
        #[serde(default)]
        requires: Vec<LatchRequirement>,
        path: String,
        pattern: String,
    },
    Json {
        #[serde(default)]
        requires: Vec<LatchRequirement>,
        path: String,
        #[serde(default)]
        pointer: Option<String>,
        #[serde(default)]
        equals: Option<Value>,
    },
    FilePresence {
        #[serde(default)]
        requires: Vec<LatchRequirement>,
        path: String,
    },
    Command {
        #[serde(default)]
        requires: Vec<LatchRequirement>,
        command: Vec<String>,
        #[serde(default = "default_launch_cwd")]
        cwd: String,
        #[serde(default)]
        env: LaunchEnv,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },
    LlmJudge {
        #[serde(default)]
        requires: Vec<LatchRequirement>,
        #[serde(default)]
        endpoint_env: Option<String>,
        #[serde(default)]
        credential_env: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LatchRequirement {
    Command(String),
    Object(LatchRequirementObject),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatchRequirementObject {
    pub kind: LatchRequirementKind,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub endpoint_env: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatchRequirementKind {
    Command,
    Env,
    Credential,
    Network,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadSpec {
    #[serde(default)]
    pub result_url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSpec {
    pub argv: Vec<String>,
    #[serde(default = "default_task_injection")]
    pub task_injection: TaskInjection,
    #[serde(default = "default_launch_cwd")]
    pub cwd: String,
    #[serde(default)]
    pub env: LaunchEnv,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub idle_timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskInjection {
    Argv,
    Stdin,
    File,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchEnv {
    #[serde(default)]
    pub inherit: Option<Vec<String>>,
    #[serde(default)]
    pub set: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct LatchRunOptions {
    pub manifest_path: PathBuf,
    pub run_root: Option<PathBuf>,
    pub launch_override: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatchManifestValidation {
    pub schema_version: String,
    pub manifest_path: PathBuf,
    pub case_count: usize,
    pub default_launch_present: bool,
    pub default_workspace_seed_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatchRunResult {
    pub schema_version: String,
    pub run_id: String,
    pub run_dir: PathBuf,
    pub enforcement_level: EnforcementLevel,
    pub cases: Vec<LatchCaseResult>,
    pub started_at: String,
    pub ended_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatchCaseResult {
    pub case_id: String,
    pub task_id: String,
    pub status: LatchCaseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub enforcement_level: EnforcementLevel,
    pub started_at: String,
    pub ended_at: String,
    pub workspace_dir: PathBuf,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_diff_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_diff_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<UploadSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade: Option<LatchGradeResult>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatchCaseStatus {
    Completed,
    Errored,
    TimedOut,
    IdleTimedOut,
    Declined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatchGradeResult {
    pub status: LatchGradeStatus,
    pub grader_kind: String,
    pub locus: String,
    pub requires: Vec<LatchRequirementProbe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LatchGradeStatus {
    Passed,
    Failed,
    Error,
    Declined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatchRequirementProbe {
    pub requirement: Value,
    pub status: LatchRequirementProbeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LatchRequirementProbeStatus {
    Present,
    Absent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementLevel {
    Guarded,
    Contained,
}

fn default_latch_manifest_schema_version() -> String {
    LATCH_MANIFEST_SCHEMA_VERSION.to_string()
}

fn default_task_injection() -> TaskInjection {
    TaskInjection::Argv
}

fn default_launch_cwd() -> String {
    "workspace".to_string()
}

pub fn run_latch_manifest(options: LatchRunOptions) -> Result<LatchRunResult> {
    let manifest = load_latch_manifest(&options.manifest_path)?;
    validate_manifest(&manifest)?;
    let manifest_dir = options
        .manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let launch = resolve_launch(&manifest, options.launch_override)?;
    validate_launch(&launch)?;
    let run_id = manifest
        .run_id
        .clone()
        .unwrap_or_else(|| format!("latch_{}", Utc::now().format("%Y%m%d_%H%M%S_%6f")));
    let run_root = match options.run_root {
        Some(path) => path,
        None => default_run_root()?,
    };
    ensure_dir(&run_root)?;
    let run_dir = unique_run_dir(&run_root, &run_id)?;
    ensure_dir(&run_dir)?;
    let started_at = Utc::now().to_rfc3339();
    fs::write(
        run_dir.join("latch_manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    let mut cases = Vec::new();
    for case in &manifest.cases {
        cases.push(run_latch_case(
            &run_dir,
            &manifest_dir,
            &manifest,
            case,
            &launch,
        )?);
    }
    let ended_at = Utc::now().to_rfc3339();
    let result = LatchRunResult {
        schema_version: LATCH_RESULT_SCHEMA_VERSION.to_string(),
        run_id,
        run_dir: run_dir.clone(),
        enforcement_level: host_enforcement_level(),
        cases,
        started_at,
        ended_at,
    };
    fs::write(
        run_dir.join("latch_result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    Ok(result)
}

pub fn validate_latch_manifest_file(path: &Path) -> Result<LatchManifestValidation> {
    let manifest = load_latch_manifest(path)?;
    validate_manifest(&manifest)?;
    Ok(LatchManifestValidation {
        schema_version: manifest.schema_version,
        manifest_path: path.to_path_buf(),
        case_count: manifest.cases.len(),
        default_launch_present: manifest.defaults.launch.is_some(),
        default_workspace_seed_present: manifest.defaults.workspace_seed.is_some(),
    })
}

fn load_latch_manifest(path: &Path) -> Result<LatchManifest> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read latch manifest {}", path.display()))?;
    if matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("yaml" | "yml")
    ) {
        serde_yaml::from_str(&String::from_utf8(bytes)?)
            .with_context(|| format!("failed to parse latch manifest YAML {}", path.display()))
    } else {
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse latch manifest JSON {}", path.display()))
    }
}

fn validate_manifest(manifest: &LatchManifest) -> Result<()> {
    if manifest.schema_version != LATCH_MANIFEST_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported latch manifest schema_version '{}'",
            manifest.schema_version
        ));
    }
    if manifest.cases.is_empty() {
        return Err(anyhow!("latch manifest must include at least one case"));
    }
    for case in &manifest.cases {
        if case.case_id.trim().is_empty() {
            return Err(anyhow!("latch case_id must not be empty"));
        }
        if case.task_prompt.trim().is_empty() {
            return Err(anyhow!(
                "latch case '{}' task_prompt must not be empty",
                case.case_id
            ));
        }
        if let Some(grader) = case.grader.as_ref().or(manifest.defaults.grader.as_ref()) {
            validate_latch_grader(grader)
                .with_context(|| format!("latch case '{}' grader", case.case_id))?;
        }
    }
    Ok(())
}

fn validate_latch_grader(grader: &LatchGraderSpec) -> Result<()> {
    match grader {
        LatchGraderSpec::Diff { .. } => Ok(()),
        LatchGraderSpec::Match {
            path,
            expected,
            contains,
            ..
        } => {
            validate_latch_workspace_relative_path(path, "grader.path")?;
            if expected.is_none() && contains.is_none() {
                return Err(anyhow!("grader.kind=match requires expected or contains"));
            }
            Ok(())
        }
        LatchGraderSpec::Regex { path, pattern, .. } => {
            validate_latch_workspace_relative_path(path, "grader.path")?;
            Regex::new(pattern)
                .map(|_| ())
                .with_context(|| format!("invalid grader.pattern '{}'", pattern))
        }
        LatchGraderSpec::Json { path, pointer, .. } => {
            validate_latch_workspace_relative_path(path, "grader.path")?;
            if let Some(pointer) = pointer {
                if !pointer.is_empty() && !pointer.starts_with('/') {
                    return Err(anyhow!("grader.pointer must be empty or start with '/'"));
                }
            }
            Ok(())
        }
        LatchGraderSpec::FilePresence { path, .. } => {
            validate_latch_workspace_relative_path(path, "grader.path")
        }
        LatchGraderSpec::Command { command, cwd, .. } => {
            if command.is_empty() || command.iter().any(|part| part.trim().is_empty()) {
                return Err(anyhow!("grader.kind=command requires a non-empty command"));
            }
            if cwd.trim().is_empty() {
                return Err(anyhow!("grader.cwd must not be empty"));
            }
            Ok(())
        }
        LatchGraderSpec::LlmJudge { requires, .. } => {
            if requires.is_empty() {
                return Err(anyhow!(
                    "grader.kind=llm_judge requires explicit network and credential probes"
                ));
            }
            Ok(())
        }
    }
}

fn validate_latch_workspace_relative_path(raw: &str, field: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{} must not be empty", field));
    }
    let path = Path::new(trimmed);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(anyhow!(
            "{} must be a relative path inside the latch workspace",
            field
        ));
    }
    Ok(())
}

fn resolve_launch(
    manifest: &LatchManifest,
    override_argv: Option<Vec<String>>,
) -> Result<LaunchSpec> {
    let mut launch = manifest.defaults.launch.clone().ok_or_else(|| {
        anyhow!("latch launch argv is required; provide defaults.launch or pass argv after --")
    })?;
    if let Some(argv) = override_argv {
        launch.argv = argv;
    }
    Ok(launch)
}

fn validate_launch(launch: &LaunchSpec) -> Result<()> {
    if launch.argv.is_empty() || launch.argv.iter().any(|part| part.trim().is_empty()) {
        return Err(anyhow!("latch launch argv must be a non-empty argv array"));
    }
    if launch.cwd.trim().is_empty() {
        return Err(anyhow!("latch launch cwd must not be empty"));
    }
    Ok(())
}

fn unique_run_dir(root: &Path, run_id: &str) -> Result<PathBuf> {
    let safe = sanitize_for_fs(run_id);
    for attempt in 0..64 {
        let candidate = if attempt == 0 {
            root.join(&safe)
        } else {
            root.join(format!("{}_{:02}", safe, attempt))
        };
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(anyhow!(
        "failed to create unique latch run directory for {}",
        run_id
    ))
}

fn run_latch_case(
    run_dir: &Path,
    manifest_dir: &Path,
    manifest: &LatchManifest,
    case: &LatchCaseManifest,
    launch: &LaunchSpec,
) -> Result<LatchCaseResult> {
    let task_id = case.task_id.clone().unwrap_or_else(|| case.case_id.clone());
    let case_dir = run_dir.join("cases").join(sanitize_for_fs(&case.case_id));
    let workspace_dir = case_dir.join("workspace");
    let out_dir = case_dir.join("out");
    let events_dir = case_dir.join("events");
    let materialization_log_dir = case_dir.join("runner").join("case_materialization");
    ensure_dir(&workspace_dir)?;
    ensure_dir(&out_dir)?;
    ensure_dir(&events_dir)?;
    fs::write(
        case_dir.join("case_manifest.json"),
        serde_json::to_vec_pretty(case)?,
    )?;
    let grader = case.grader.as_ref().or(manifest.defaults.grader.as_ref());
    let grader_probe = grader.map(probe_latch_grader).transpose()?;
    if let (Some(grader), Some(probes)) = (grader, grader_probe.as_ref()) {
        if let Some(reason) = declined_probe_reason(probes) {
            let now = Utc::now().to_rfc3339();
            let case_result = LatchCaseResult {
                case_id: case.case_id.clone(),
                task_id,
                status: LatchCaseStatus::Declined,
                exit_code: None,
                enforcement_level: host_enforcement_level(),
                started_at: now.clone(),
                ended_at: now,
                workspace_dir,
                stdout_path: out_dir.join("stdout.log"),
                stderr_path: out_dir.join("stderr.log"),
                result_path: None,
                workspace_diff_path: None,
                workspace_diff_digest: None,
                capture_error: None,
                upload: case.upload.clone(),
                grade: Some(LatchGradeResult {
                    status: LatchGradeStatus::Declined,
                    grader_kind: grader.kind_name().to_string(),
                    locus: latch_grader_locus().to_string(),
                    requires: probes.clone(),
                    reason: Some(reason),
                    score: None,
                    stdout_path: None,
                    stderr_path: None,
                    output_path: None,
                }),
            };
            fs::write(
                case_dir.join("case_result.json"),
                serde_json::to_vec_pretty(&case_result)?,
            )?;
            return Ok(case_result);
        }
    }

    let seed = case
        .workspace_seed
        .as_ref()
        .or(manifest.defaults.workspace_seed.as_ref())
        .cloned()
        .unwrap_or(WorkspaceSeed::Empty);
    materialize_workspace_seed(manifest_dir, &workspace_dir, &seed)?;

    let task_file = case_dir.join("task_prompt.txt");
    fs::write(&task_file, &case.task_prompt)?;
    let trial_input_path = case_dir.join("in").join("trial_input.json");
    ensure_dir(trial_input_path.parent().unwrap_or(&case_dir))?;
    fs::write(
        &trial_input_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "latch_trial_input_v1",
            "case_id": case.case_id,
            "task_id": task_id,
            "task_prompt": case.task_prompt,
            "metadata": case.metadata,
        }))?,
    )?;

    let stdout_path = out_dir.join("stdout.log");
    let stderr_path = out_dir.join("stderr.log");
    let result_path = out_dir.join("result.json");
    let trajectory_path = events_dir.join("trajectory.jsonl");
    let materialization_env = latch_materialization_env(
        case,
        &task_id,
        &trial_input_path,
        &result_path,
        &trajectory_path,
    );
    HostCaseMaterializationExecutor.execute(
        HostCaseMaterializationRequest {
            manifest_dir,
            workspace_dir: &workspace_dir,
            log_dir: &materialization_log_dir,
            phase: CaseMaterializationPhase::AgentVisible,
            default_timeout_ms: launch_timeout_seconds(manifest, launch) * 1000,
            env: materialization_env.clone(),
        },
        &case.materialization,
    )?;
    let baseline = prepare_workspace_baseline(&workspace_dir);
    let started_at = Utc::now().to_rfc3339();
    let mut child = spawn_latch_process(
        launch,
        case,
        &task_id,
        &workspace_dir,
        &task_file,
        &trial_input_path,
        &result_path,
        &trajectory_path,
        &stdout_path,
        &stderr_path,
    )?;
    let completion = wait_for_latch_process(
        &mut child,
        &workspace_dir,
        launch_timeout_seconds(manifest, launch),
        launch_idle_timeout_seconds(manifest, launch),
    )?;
    let baseline_error = baseline.as_ref().err().map(|err| err.to_string());
    let (workspace_diff_path, workspace_diff_digest, capture_error) = match capture_workspace_diff(
        &workspace_dir,
        &out_dir.join("candidate.patch"),
        baseline.as_ref().ok(),
    ) {
        Ok(Some(path)) => {
            let digest = lab_core::sha256_file(&path).ok();
            (Some(path), digest, baseline_error)
        }
        Ok(None) => (None, None, baseline_error),
        Err(err) => (None, None, Some(err.to_string())),
    };
    let status = completion.status();
    let exit_code = completion.exit_code;
    HostCaseMaterializationExecutor.execute(
        HostCaseMaterializationRequest {
            manifest_dir,
            workspace_dir: &workspace_dir,
            log_dir: &materialization_log_dir,
            phase: CaseMaterializationPhase::GraderVisible,
            default_timeout_ms: launch_timeout_seconds(manifest, launch) * 1000,
            env: materialization_env.clone(),
        },
        &case.materialization,
    )?;
    let grade = match (grader, grader_probe) {
        (Some(grader), Some(probes)) => Some(run_latch_grader(
            grader,
            LatchGraderRunRequest {
                workspace_dir: &workspace_dir,
                out_dir: &out_dir,
                patch_path: workspace_diff_path.as_deref(),
                trial_input_path: &trial_input_path,
                result_path: &result_path,
                trajectory_path: &trajectory_path,
                agent_exit_code: exit_code,
                env: &materialization_env,
                probes,
            },
        )?),
        _ => None,
    };
    let ended_at = Utc::now().to_rfc3339();
    let case_result = LatchCaseResult {
        case_id: case.case_id.clone(),
        task_id,
        status,
        exit_code,
        enforcement_level: host_enforcement_level(),
        started_at,
        ended_at,
        workspace_dir,
        stdout_path,
        stderr_path,
        result_path: result_path.exists().then_some(result_path),
        workspace_diff_path,
        workspace_diff_digest,
        capture_error,
        upload: case.upload.clone(),
        grade,
    };
    fs::write(
        case_dir.join("case_result.json"),
        serde_json::to_vec_pretty(&case_result)?,
    )?;
    Ok(case_result)
}

fn latch_materialization_env(
    case: &LatchCaseManifest,
    task_id: &str,
    trial_input_path: &Path,
    result_path: &Path,
    trajectory_path: &Path,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        (BUCEPHALUS_ENV_CASE_ID.to_string(), case.case_id.clone()),
        (BUCEPHALUS_ENV_TASK_ID.to_string(), task_id.to_string()),
        (
            BUCEPHALUS_ENV_TRIAL_INPUT_PATH.to_string(),
            trial_input_path.to_string_lossy().to_string(),
        ),
        (
            BUCEPHALUS_ENV_RESULT_PATH.to_string(),
            result_path.to_string_lossy().to_string(),
        ),
        (
            BUCEPHALUS_ENV_TRAJECTORY_PATH.to_string(),
            trajectory_path.to_string_lossy().to_string(),
        ),
    ])
}

impl LatchGraderSpec {
    fn kind_name(&self) -> &'static str {
        match self {
            Self::Diff { .. } => "diff",
            Self::Match { .. } => "match",
            Self::Regex { .. } => "regex",
            Self::Json { .. } => "json",
            Self::FilePresence { .. } => "file_presence",
            Self::Command { .. } => "command",
            Self::LlmJudge { .. } => "llm_judge",
        }
    }

    fn declared_requires(&self) -> &[LatchRequirement] {
        match self {
            Self::Diff { requires, .. }
            | Self::Match { requires, .. }
            | Self::Regex { requires, .. }
            | Self::Json { requires, .. }
            | Self::FilePresence { requires, .. }
            | Self::Command { requires, .. }
            | Self::LlmJudge { requires, .. } => requires,
        }
    }
}

struct LatchGraderRunRequest<'a> {
    workspace_dir: &'a Path,
    out_dir: &'a Path,
    patch_path: Option<&'a Path>,
    trial_input_path: &'a Path,
    result_path: &'a Path,
    trajectory_path: &'a Path,
    agent_exit_code: Option<i32>,
    env: &'a BTreeMap<String, String>,
    probes: Vec<LatchRequirementProbe>,
}

fn latch_grader_locus() -> &'static str {
    "host_self_graded"
}

fn probe_latch_grader(grader: &LatchGraderSpec) -> Result<Vec<LatchRequirementProbe>> {
    let mut requirements = grader.declared_requires().to_vec();
    if let LatchGraderSpec::Command { command, .. } = grader {
        if let Some(exe) = command.first() {
            let implicit = LatchRequirement::Object(LatchRequirementObject {
                kind: LatchRequirementKind::Command,
                name: Some(exe.clone()),
                command: Some(exe.clone()),
                env: None,
                endpoint_env: None,
                url: None,
            });
            if !requirements.iter().any(|requirement| {
                requirement_command_name(requirement).as_deref() == Some(exe.as_str())
            }) {
                requirements.push(implicit);
            }
        }
    }
    if let LatchGraderSpec::LlmJudge {
        endpoint_env,
        credential_env,
        ..
    } = grader
    {
        if let Some(env) = endpoint_env {
            push_env_requirement(&mut requirements, LatchRequirementKind::Network, env);
        }
        if let Some(env) = credential_env {
            push_env_requirement(&mut requirements, LatchRequirementKind::Credential, env);
        }
    }
    requirements.iter().map(probe_latch_requirement).collect()
}

fn push_env_requirement(
    requirements: &mut Vec<LatchRequirement>,
    kind: LatchRequirementKind,
    env: &str,
) {
    if requirements
        .iter()
        .any(|requirement| requirement_env_name(requirement).as_deref() == Some(env))
    {
        return;
    }
    requirements.push(LatchRequirement::Object(LatchRequirementObject {
        kind,
        name: Some(env.to_string()),
        command: None,
        env: Some(env.to_string()),
        endpoint_env: None,
        url: None,
    }));
}

fn probe_latch_requirement(requirement: &LatchRequirement) -> Result<LatchRequirementProbe> {
    let value = serde_json::to_value(requirement)?;
    match requirement {
        LatchRequirement::Command(command) => Ok(probe_command_requirement(value, command)),
        LatchRequirement::Object(object) => match object.kind {
            LatchRequirementKind::Command => {
                let command = object
                    .command
                    .as_deref()
                    .or(object.name.as_deref())
                    .ok_or_else(|| anyhow!("command requirement requires command or name"))?;
                Ok(probe_command_requirement(value, command))
            }
            LatchRequirementKind::Env | LatchRequirementKind::Credential => {
                let env = object
                    .env
                    .as_deref()
                    .or(object.name.as_deref())
                    .ok_or_else(|| anyhow!("env requirement requires env or name"))?;
                Ok(probe_env_requirement(value, env))
            }
            LatchRequirementKind::Network => {
                let env = object.endpoint_env.as_deref().or(object.env.as_deref());
                Ok(probe_network_requirement(value, env, object.url.as_deref()))
            }
        },
    }
}

fn probe_command_requirement(requirement: Value, command: &str) -> LatchRequirementProbe {
    match find_command_on_path(command) {
        Some(path) => LatchRequirementProbe {
            requirement,
            status: LatchRequirementProbeStatus::Present,
            detail: Some(path.to_string_lossy().to_string()),
        },
        None => LatchRequirementProbe {
            requirement,
            status: LatchRequirementProbeStatus::Absent,
            detail: Some(format!("command '{}' was not found on PATH", command)),
        },
    }
}

fn probe_env_requirement(requirement: Value, env: &str) -> LatchRequirementProbe {
    match std::env::var(env) {
        Ok(value) if !value.trim().is_empty() => LatchRequirementProbe {
            requirement,
            status: LatchRequirementProbeStatus::Present,
            detail: Some(format!("environment variable '{}' is set", env)),
        },
        _ => LatchRequirementProbe {
            requirement,
            status: LatchRequirementProbeStatus::Absent,
            detail: Some(format!("environment variable '{}' is not set", env)),
        },
    }
}

fn probe_network_requirement(
    requirement: Value,
    endpoint_env: Option<&str>,
    url: Option<&str>,
) -> LatchRequirementProbe {
    if let Some(env) = endpoint_env {
        return match std::env::var(env) {
            Ok(value) if !value.trim().is_empty() => LatchRequirementProbe {
                requirement,
                status: LatchRequirementProbeStatus::Present,
                detail: Some(format!("network endpoint from '{}' is configured", env)),
            },
            _ => LatchRequirementProbe {
                requirement,
                status: LatchRequirementProbeStatus::Absent,
                detail: Some(format!("network endpoint env '{}' is not set", env)),
            },
        };
    }
    if let Some(url) = url.filter(|url| !url.trim().is_empty()) {
        return LatchRequirementProbe {
            requirement,
            status: LatchRequirementProbeStatus::Present,
            detail: Some(format!("network endpoint declared: {}", url)),
        };
    }
    LatchRequirementProbe {
        requirement,
        status: LatchRequirementProbeStatus::Absent,
        detail: Some("network requirement needs endpoint_env or url".to_string()),
    }
}

fn declined_probe_reason(probes: &[LatchRequirementProbe]) -> Option<String> {
    let missing = probes
        .iter()
        .filter(|probe| probe.status == LatchRequirementProbeStatus::Absent)
        .map(|probe| {
            probe
                .detail
                .clone()
                .unwrap_or_else(|| "required host dependency is absent".to_string())
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "declined because required host dependencies are absent: {}",
            missing.join("; ")
        ))
    }
}

fn requirement_command_name(requirement: &LatchRequirement) -> Option<String> {
    match requirement {
        LatchRequirement::Command(command) => Some(command.clone()),
        LatchRequirement::Object(object)
            if matches!(object.kind, LatchRequirementKind::Command) =>
        {
            object.command.clone().or_else(|| object.name.clone())
        }
        _ => None,
    }
}

fn requirement_env_name(requirement: &LatchRequirement) -> Option<String> {
    match requirement {
        LatchRequirement::Object(object)
            if matches!(
                object.kind,
                LatchRequirementKind::Env
                    | LatchRequirementKind::Credential
                    | LatchRequirementKind::Network
            ) =>
        {
            object
                .env
                .clone()
                .or_else(|| object.endpoint_env.clone())
                .or_else(|| object.name.clone())
        }
        _ => None,
    }
}

fn find_command_on_path(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return is_executable_file(command_path).then(|| command_path.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(command);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn run_latch_grader(
    grader: &LatchGraderSpec,
    request: LatchGraderRunRequest<'_>,
) -> Result<LatchGradeResult> {
    let probes = request.probes.clone();
    let mut grade = match grader {
        LatchGraderSpec::Diff {
            contains,
            expected_empty,
            ..
        } => grade_latch_diff(request.patch_path, contains.as_deref(), *expected_empty)?,
        LatchGraderSpec::Match {
            path,
            expected,
            contains,
            ..
        } => grade_latch_match(
            &request.workspace_dir.join(path),
            expected.as_deref(),
            contains.as_deref(),
        )?,
        LatchGraderSpec::Regex { path, pattern, .. } => {
            grade_latch_regex(&request.workspace_dir.join(path), pattern)?
        }
        LatchGraderSpec::Json {
            path,
            pointer,
            equals,
            ..
        } => grade_latch_json(
            &request.workspace_dir.join(path),
            pointer.as_deref(),
            equals.as_ref(),
        )?,
        LatchGraderSpec::FilePresence { path, .. } => {
            let path = request.workspace_dir.join(path);
            let passed = path.exists();
            LatchGradeResult {
                status: if passed {
                    LatchGradeStatus::Passed
                } else {
                    LatchGradeStatus::Failed
                },
                grader_kind: grader.kind_name().to_string(),
                locus: latch_grader_locus().to_string(),
                requires: probes.clone(),
                reason: (!passed)
                    .then(|| format!("required file is missing: {}", path.display())),
                score: Some(if passed { 1.0 } else { 0.0 }),
                stdout_path: None,
                stderr_path: None,
                output_path: None,
            }
        }
        LatchGraderSpec::Command {
            command,
            cwd,
            env,
            timeout_seconds,
            ..
        } => run_latch_command_grader(
            command,
            cwd,
            env,
            timeout_seconds.unwrap_or(DEFAULT_WALL_TIMEOUT_SECONDS),
            request,
        )?,
        LatchGraderSpec::LlmJudge { .. } => LatchGradeResult {
            status: LatchGradeStatus::Error,
            grader_kind: grader.kind_name().to_string(),
            locus: latch_grader_locus().to_string(),
            requires: probes.clone(),
            reason: Some(
                "llm_judge dependency probing is supported, but the Tier-1 judge runner is not implemented"
                    .to_string(),
            ),
            score: None,
            stdout_path: None,
            stderr_path: None,
            output_path: None,
        },
    };
    grade.grader_kind = grader.kind_name().to_string();
    grade.locus = latch_grader_locus().to_string();
    grade.requires = probes;
    Ok(grade)
}

fn grade_latch_diff(
    patch_path: Option<&Path>,
    contains: Option<&str>,
    expected_empty: Option<bool>,
) -> Result<LatchGradeResult> {
    let patch = patch_path
        .map(fs::read_to_string)
        .transpose()?
        .unwrap_or_default();
    let passed = if let Some(expected_empty) = expected_empty {
        patch.is_empty() == expected_empty
    } else if let Some(contains) = contains {
        patch.contains(contains)
    } else {
        !patch.is_empty()
    };
    Ok(builtin_grade(
        "diff",
        passed,
        if passed {
            None
        } else {
            Some("workspace diff did not satisfy grader expectation".to_string())
        },
        Vec::new(),
    ))
}

fn grade_latch_match(
    path: &Path,
    expected: Option<&str>,
    contains: Option<&str>,
) -> Result<LatchGradeResult> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read match grader file {}", path.display()))?;
    let passed = if let Some(expected) = expected {
        text == expected
    } else if let Some(contains) = contains {
        text.contains(contains)
    } else {
        false
    };
    Ok(builtin_grade(
        "match",
        passed,
        (!passed).then(|| format!("{} did not match grader expectation", path.display())),
        Vec::new(),
    ))
}

fn grade_latch_regex(path: &Path, pattern: &str) -> Result<LatchGradeResult> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read regex grader file {}", path.display()))?;
    let regex = Regex::new(pattern)?;
    let passed = regex.is_match(&text);
    Ok(builtin_grade(
        "regex",
        passed,
        (!passed).then(|| format!("{} did not match regex {}", path.display(), pattern)),
        Vec::new(),
    ))
}

fn grade_latch_json(
    path: &Path,
    pointer: Option<&str>,
    equals: Option<&Value>,
) -> Result<LatchGradeResult> {
    let value: Value = serde_json::from_slice(
        &fs::read(path)
            .with_context(|| format!("failed to read JSON grader file {}", path.display()))?,
    )
    .with_context(|| format!("failed to parse JSON grader file {}", path.display()))?;
    let observed = pointer
        .filter(|pointer| !pointer.is_empty())
        .map(|pointer| value.pointer(pointer))
        .unwrap_or(Some(&value));
    let passed = match (observed, equals) {
        (Some(observed), Some(expected)) => observed == expected,
        (Some(_), None) => true,
        (None, _) => false,
    };
    Ok(builtin_grade(
        "json",
        passed,
        (!passed).then(|| format!("{} did not satisfy JSON grader expectation", path.display())),
        Vec::new(),
    ))
}

fn builtin_grade(
    kind: &str,
    passed: bool,
    reason: Option<String>,
    requires: Vec<LatchRequirementProbe>,
) -> LatchGradeResult {
    LatchGradeResult {
        status: if passed {
            LatchGradeStatus::Passed
        } else {
            LatchGradeStatus::Failed
        },
        grader_kind: kind.to_string(),
        locus: latch_grader_locus().to_string(),
        requires,
        reason,
        score: Some(if passed { 1.0 } else { 0.0 }),
        stdout_path: None,
        stderr_path: None,
        output_path: None,
    }
}

fn run_latch_command_grader(
    command: &[String],
    cwd: &str,
    env: &LaunchEnv,
    timeout_seconds: u64,
    request: LatchGraderRunRequest<'_>,
) -> Result<LatchGradeResult> {
    let stdout_path = request.out_dir.join("grader_stdout.log");
    let stderr_path = request.out_dir.join("grader_stderr.log");
    let output_path = request.out_dir.join("grader_output.json");
    let cwd = launch_cwd(request.workspace_dir, cwd)?;
    let stdout = File::create(&stdout_path)
        .with_context(|| format!("failed to create {}", stdout_path.display()))?;
    let stderr = File::create(&stderr_path)
        .with_context(|| format!("failed to create {}", stderr_path.display()))?;
    let mut child_command = Command::new(&command[0]);
    child_command.args(&command[1..]).current_dir(cwd);
    apply_launch_env(&mut child_command, env);
    child_command.envs(request.env.clone());
    child_command.env("WORKSPACE", request.workspace_dir);
    child_command.env("LATCH_WORKSPACE_DIR", request.workspace_dir);
    child_command.env("LATCH_GRADE_OUTPUT_PATH", &output_path);
    child_command.env(
        "LATCH_AGENT_EXIT_CODE",
        exit_status_env(request.agent_exit_code),
    );
    child_command.env(
        "LATCH_WORKSPACE_DIFF_PATH",
        optional_path_env(request.patch_path),
    );
    child_command.env(BUCEPHALUS_ENV_TRIAL_INPUT_PATH, request.trial_input_path);
    child_command.env(BUCEPHALUS_ENV_RESULT_PATH, request.result_path);
    child_command.env(BUCEPHALUS_ENV_TRAJECTORY_PATH, request.trajectory_path);
    child_command.stdout(Stdio::from(stdout));
    child_command.stderr(Stdio::from(stderr));
    let mut child = child_command
        .spawn()
        .with_context(|| format!("failed to spawn latch grader command '{}'", command[0]))?;
    let completion = wait_for_grader_process(&mut child, timeout_seconds)?;
    let mut status = if completion.timed_out {
        LatchGradeStatus::Error
    } else if completion.exit_code == Some(0) {
        LatchGradeStatus::Passed
    } else {
        LatchGradeStatus::Failed
    };
    let mut score = Some(if status == LatchGradeStatus::Passed {
        1.0
    } else {
        0.0
    });
    let mut reason = match status {
        LatchGradeStatus::Passed => None,
        LatchGradeStatus::Failed => Some(format!(
            "grader command exited with status {}",
            exit_status_env(completion.exit_code)
        )),
        LatchGradeStatus::Error => Some("grader command timed out".to_string()),
        LatchGradeStatus::Declined => None,
    };
    if output_path.exists() {
        if let Ok(output) = serde_json::from_slice::<Value>(&fs::read(&output_path)?) {
            if let Some(passed) = output.get("passed").and_then(Value::as_bool) {
                status = if passed {
                    LatchGradeStatus::Passed
                } else {
                    LatchGradeStatus::Failed
                };
                score = Some(if passed { 1.0 } else { 0.0 });
            }
            if let Some(verdict) = output.get("verdict").and_then(Value::as_str) {
                status = match verdict {
                    "pass" | "passed" | "success" => LatchGradeStatus::Passed,
                    "fail" | "failed" | "failure" => LatchGradeStatus::Failed,
                    "error" => LatchGradeStatus::Error,
                    _ => status,
                };
            }
            if let Some(value) = output.get("score").and_then(Value::as_f64) {
                score = Some(value);
            }
            if let Some(value) = output.get("reason").and_then(Value::as_str) {
                reason = Some(value.to_string());
            }
        }
    }
    Ok(LatchGradeResult {
        status,
        grader_kind: "command".to_string(),
        locus: latch_grader_locus().to_string(),
        requires: request.probes,
        reason,
        score,
        stdout_path: Some(stdout_path),
        stderr_path: Some(stderr_path),
        output_path: output_path.exists().then_some(output_path),
    })
}

fn wait_for_grader_process(child: &mut Child, timeout_seconds: u64) -> Result<ProcessCompletion> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(ProcessCompletion {
                exit_code: status.code(),
                timed_out: false,
                idle_timed_out: false,
            });
        }
        if started.elapsed() >= Duration::from_secs(timeout_seconds) {
            terminate_child(child)?;
            return Ok(ProcessCompletion {
                exit_code: None,
                timed_out: true,
                idle_timed_out: false,
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn exit_status_env(exit_code: Option<i32>) -> String {
    exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn optional_path_env(path: Option<&Path>) -> String {
    path.map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn materialize_workspace_seed(
    manifest_dir: &Path,
    workspace_dir: &Path,
    seed: &WorkspaceSeed,
) -> Result<()> {
    match seed {
        WorkspaceSeed::Empty => Ok(()),
        WorkspaceSeed::Files { path, ref_path } => {
            let source = resolve_manifest_path(manifest_dir, path.as_ref().or(ref_path.as_ref()))?;
            if source.is_dir() {
                copy_dir_preserve_contents(&source, workspace_dir)
            } else if source.is_file() {
                let file_name = source.file_name().ok_or_else(|| {
                    anyhow!("workspace seed file has no file name: {}", source.display())
                })?;
                copy_file_if_exists(&source, &workspace_dir.join(file_name))
            } else {
                Err(anyhow!(
                    "workspace seed path not found: {}",
                    source.display()
                ))
            }
        }
        WorkspaceSeed::Archive { path, ref_path } => {
            let source = resolve_manifest_path(manifest_dir, path.as_ref().or(ref_path.as_ref()))?;
            unpack_archive(&source, workspace_dir)
        }
        WorkspaceSeed::Git { repo, rev } => {
            run_checked(
                Command::new("git")
                    .arg("clone")
                    .arg("--")
                    .arg(repo)
                    .arg(workspace_dir),
                "git clone workspace seed",
            )?;
            if let Some(rev) = rev {
                run_checked(
                    Command::new("git")
                        .arg("-C")
                        .arg(workspace_dir)
                        .arg("checkout")
                        .arg("--detach")
                        .arg(rev),
                    "git checkout workspace seed",
                )?;
            }
            Ok(())
        }
    }
}

fn resolve_manifest_path(manifest_dir: &Path, value: Option<&String>) -> Result<PathBuf> {
    let raw = value.ok_or_else(|| anyhow!("workspace seed requires path or ref"))?;
    let path = PathBuf::from(raw);
    Ok(if path.is_absolute() {
        path
    } else {
        manifest_dir.join(path)
    })
}

fn unpack_archive(source: &Path, workspace_dir: &Path) -> Result<()> {
    let mut file = File::open(source)
        .with_context(|| format!("failed to open workspace archive {}", source.display()))?;
    let mut magic = [0_u8; 2];
    let read = file.read(&mut magic)?;
    drop(file);
    let is_gz = read == 2 && magic == [0x1f, 0x8b]
        || source
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext == "gz" || ext == "tgz");
    let file = File::open(source)
        .with_context(|| format!("failed to open workspace archive {}", source.display()))?;
    if is_gz {
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(workspace_dir)?;
    } else {
        let mut archive = tar::Archive::new(file);
        archive.unpack(workspace_dir)?;
    }
    Ok(())
}

fn prepare_workspace_baseline(workspace_dir: &Path) -> Result<String> {
    if !workspace_dir.join(".git").exists() {
        run_checked(
            Command::new("git").arg("-C").arg(workspace_dir).arg("init"),
            "git init latch workspace",
        )?;
    }
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(workspace_dir)
            .arg("add")
            .arg("-A"),
        "git add latch workspace baseline",
    )?;
    let tree = run_capture(
        Command::new("git")
            .arg("-C")
            .arg(workspace_dir)
            .arg("write-tree"),
        "git write-tree latch workspace baseline",
    )?;
    Ok(tree.trim().to_string())
}

#[allow(clippy::too_many_arguments)]
fn spawn_latch_process(
    launch: &LaunchSpec,
    case: &LatchCaseManifest,
    task_id: &str,
    workspace_dir: &Path,
    task_file: &Path,
    trial_input_path: &Path,
    result_path: &Path,
    trajectory_path: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<Child> {
    let argv = launch_argv(launch, &case.task_prompt, task_file);
    let cwd = launch_cwd(workspace_dir, &launch.cwd)?;
    let stdout = File::create(stdout_path)
        .with_context(|| format!("failed to create {}", stdout_path.display()))?;
    let stderr = File::create(stderr_path)
        .with_context(|| format!("failed to create {}", stderr_path.display()))?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).current_dir(cwd);
    apply_launch_env(&mut command, &launch.env);
    command.env("LATCH_TASK_PROMPT", &case.task_prompt);
    command.env("LATCH_TASK_FILE", task_file);
    command.env(BUCEPHALUS_ENV_CASE_ID, &case.case_id);
    command.env(BUCEPHALUS_ENV_TASK_ID, task_id);
    command.env(BUCEPHALUS_ENV_TRIAL_INPUT_PATH, trial_input_path);
    command.env(BUCEPHALUS_ENV_RESULT_PATH, result_path);
    command.env(BUCEPHALUS_ENV_TRAJECTORY_PATH, trajectory_path);
    command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if matches!(launch.task_injection, TaskInjection::Stdin) {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to spawn latch command '{}'",
            argv.first().map(String::as_str).unwrap_or("")
        )
    })?;
    if matches!(launch.task_injection, TaskInjection::Stdin) {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(case.task_prompt.as_bytes())?;
        }
    }
    Ok(child)
}

fn launch_argv(launch: &LaunchSpec, task_prompt: &str, task_file: &Path) -> Vec<String> {
    launch
        .argv
        .iter()
        .map(|part| {
            part.replace("{TASK_PROMPT}", task_prompt)
                .replace("{TASK_FILE}", &task_file.to_string_lossy())
        })
        .collect()
}

fn launch_cwd(workspace_dir: &Path, raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed == "workspace" || trimmed == "." {
        return Ok(workspace_dir.to_path_buf());
    }
    let rel = Path::new(trimmed);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(anyhow!(
            "latch launch cwd must be 'workspace' or a relative path inside the workspace"
        ));
    }
    Ok(workspace_dir.join(rel))
}

fn apply_launch_env(command: &mut Command, env: &LaunchEnv) {
    match env.inherit.as_ref() {
        None => {}
        Some(keys) => {
            command.env_clear();
            for key in keys {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
        }
    }
    for (key, value) in &env.set {
        command.env(key, value);
    }
}

fn launch_timeout_seconds(manifest: &LatchManifest, launch: &LaunchSpec) -> u64 {
    launch
        .timeout_seconds
        .or(manifest.defaults.timeout_seconds)
        .unwrap_or(DEFAULT_WALL_TIMEOUT_SECONDS)
}

fn launch_idle_timeout_seconds(manifest: &LatchManifest, launch: &LaunchSpec) -> Option<u64> {
    launch
        .idle_timeout_seconds
        .or(manifest.defaults.idle_timeout_seconds)
}

struct ProcessCompletion {
    exit_code: Option<i32>,
    timed_out: bool,
    idle_timed_out: bool,
}

impl ProcessCompletion {
    fn status(&self) -> LatchCaseStatus {
        if self.idle_timed_out {
            LatchCaseStatus::IdleTimedOut
        } else if self.timed_out {
            LatchCaseStatus::TimedOut
        } else if self.exit_code == Some(0) {
            LatchCaseStatus::Completed
        } else {
            LatchCaseStatus::Errored
        }
    }
}

fn wait_for_latch_process(
    child: &mut Child,
    workspace_dir: &Path,
    timeout_seconds: u64,
    idle_timeout_seconds: Option<u64>,
) -> Result<ProcessCompletion> {
    let started = Instant::now();
    let mut last_fingerprint = workspace_fingerprint(workspace_dir).unwrap_or_default();
    let mut last_workspace_change = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(ProcessCompletion {
                exit_code: status.code(),
                timed_out: false,
                idle_timed_out: false,
            });
        }
        if started.elapsed() >= Duration::from_secs(timeout_seconds) {
            terminate_child(child)?;
            return Ok(ProcessCompletion {
                exit_code: None,
                timed_out: true,
                idle_timed_out: false,
            });
        }
        if let Some(idle_seconds) = idle_timeout_seconds {
            let current = workspace_fingerprint(workspace_dir).unwrap_or_default();
            if current != last_fingerprint {
                last_fingerprint = current;
                last_workspace_change = Instant::now();
            } else if last_workspace_change.elapsed() >= Duration::from_secs(idle_seconds) {
                terminate_child(child)?;
                return Ok(ProcessCompletion {
                    exit_code: None,
                    timed_out: false,
                    idle_timed_out: true,
                });
            }
        }
        thread::sleep(WORKSPACE_POLL_INTERVAL);
    }
}

fn terminate_child(child: &mut Child) -> Result<()> {
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn workspace_fingerprint(workspace_dir: &Path) -> Result<String> {
    let mut lines = Vec::new();
    for entry in walkdir::WalkDir::new(workspace_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let Ok(rel) = entry.path().strip_prefix(workspace_dir) else {
                return false;
            };
            !rel.starts_with(".git")
        })
    {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(workspace_dir)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let meta = fs::symlink_metadata(path)?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(system_time_millis)
            .unwrap_or_default();
        lines.push(format!(
            "{} {} {} {}",
            rel.to_string_lossy(),
            if meta.is_dir() { "d" } else { "f" },
            meta.len(),
            mtime
        ));
    }
    lines.sort();
    Ok(sha256_bytes(lines.join("\n").as_bytes()))
}

fn system_time_millis(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis())
}

fn capture_workspace_diff(
    workspace_dir: &Path,
    patch_path: &Path,
    baseline: Option<&String>,
) -> Result<Option<PathBuf>> {
    let tree = baseline.ok_or_else(|| anyhow!("workspace baseline was not captured"))?;
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(workspace_dir)
            .arg("add")
            .arg("-N")
            .arg("--")
            .arg("."),
        "git add intent-to-add for latch diff",
    )?;
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_dir)
        .arg("diff")
        .arg("--binary")
        .arg(tree)
        .arg("--")
        .arg(".")
        .output()
        .context("failed to run git diff for latch workspace")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git diff for latch workspace failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if output.stdout.is_empty() {
        return Ok(None);
    }
    fs::write(patch_path, output.stdout)?;
    Ok(Some(patch_path.to_path_buf()))
}

fn run_checked(command: &mut Command, context: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("failed to run {}", context))?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "{} failed: {}",
        context,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn run_capture(command: &mut Command, context: &str) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("failed to run {}", context))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{} failed: {}",
            context,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn host_enforcement_level() -> EnforcementLevel {
    EnforcementLevel::Guarded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "{}_{}_{}",
                prefix,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = crate::util::remove_path_if_exists(&self.path);
        }
    }

    #[test]
    fn latch_runs_manifest_and_captures_workspace_diff() {
        let root = TempDirGuard::new("bucephalus_latch_test");
        let seed = root.path.join("seed");
        fs::create_dir_all(&seed).unwrap();
        fs::write(seed.join("hello.txt"), "before\n").unwrap();
        let manifest_path = root.path.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "latch_manifest_v1",
                "defaults": {
                    "launch": {
                        "argv": ["sh", "-c", "printf '%s\\n' \"$LATCH_TASK_PROMPT\" > hello.txt"],
                        "task_injection": "argv"
                    },
                    "workspace_seed": {"kind": "files", "path": "seed"},
                    "timeout_seconds": 30
                },
                "cases": [
                    {"case_id": "case-1", "task_prompt": "after"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let result = run_latch_manifest(LatchRunOptions {
            manifest_path,
            run_root: Some(root.path.join("runs")),
            launch_override: None,
        })
        .unwrap();
        assert_eq!(result.cases.len(), 1);
        assert!(matches!(result.cases[0].status, LatchCaseStatus::Completed));
        let patch = fs::read_to_string(result.cases[0].workspace_diff_path.as_ref().unwrap())
            .expect("patch");
        assert!(patch.contains("-before"));
        assert!(patch.contains("+after"));
    }

    #[test]
    fn latch_runs_host_materialization_with_grader_steps_after_diff_capture() {
        let root = TempDirGuard::new("bucephalus_latch_materialization_test");
        let manifest_path = root.path.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "latch_manifest_v1",
                "defaults": {
                    "launch": {
                        "argv": ["sh", "-c", "cat pre.txt > answer.txt"],
                        "task_injection": "argv"
                    },
                    "workspace_seed": {"kind": "empty"},
                    "timeout_seconds": 30
                },
                "cases": [
                    {
                        "case_id": "case-1",
                        "task_prompt": "write answer",
                        "materialization": [
                            {
                                "id": "visible-setup",
                                "stage": "case",
                                "operation": "command",
                                "command": ["sh", "-c", "printf visible > pre.txt"],
                                "workdir": "/workspace/task",
                                "network": "none"
                            },
                            {
                                "id": "hidden-grader",
                                "stage": "grader",
                                "operation": "command",
                                "command": ["sh", "-c", "printf hidden > hidden.txt"],
                                "workdir": "/workspace/task",
                                "network": "none",
                                "hidden": true
                            }
                        ]
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let result = run_latch_manifest(LatchRunOptions {
            manifest_path,
            run_root: Some(root.path.join("runs")),
            launch_override: None,
        })
        .unwrap();
        let case = &result.cases[0];
        assert!(matches!(case.status, LatchCaseStatus::Completed));
        assert_eq!(
            fs::read_to_string(case.workspace_dir.join("hidden.txt")).unwrap(),
            "hidden"
        );
        let patch = fs::read_to_string(case.workspace_diff_path.as_ref().unwrap())
            .expect("candidate patch");
        assert!(patch.contains("answer.txt"));
        assert!(patch.contains("+visible"));
        assert!(!patch.contains("hidden.txt"));
    }

    #[test]
    fn latch_builtin_file_presence_grader_is_zero_dep_and_host_self_graded() {
        let root = TempDirGuard::new("bucephalus_latch_builtin_grader");
        let manifest_path = root.path.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "latch_manifest_v1",
                "defaults": {
                    "launch": {
                        "argv": ["sh", "-c", "printf ok > answer.txt"],
                        "task_injection": "argv"
                    },
                    "grader": {
                        "kind": "file_presence",
                        "path": "answer.txt"
                    },
                    "timeout_seconds": 30
                },
                "cases": [
                    {"case_id": "case-1", "task_prompt": "write answer"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let result = run_latch_manifest(LatchRunOptions {
            manifest_path,
            run_root: Some(root.path.join("runs")),
            launch_override: None,
        })
        .unwrap();
        let grade = result.cases[0].grade.as_ref().expect("grade");
        assert!(matches!(result.cases[0].status, LatchCaseStatus::Completed));
        assert_eq!(grade.status, LatchGradeStatus::Passed);
        assert_eq!(grade.grader_kind, "file_presence");
        assert_eq!(grade.locus, "host_self_graded");
        assert!(grade.requires.is_empty());
    }

    #[test]
    fn latch_missing_grader_requirement_declines_without_running_agent() {
        let root = TempDirGuard::new("bucephalus_latch_missing_grader_dep");
        let missing = format!(
            "bucephalus_missing_tool_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let manifest_path = root.path.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "latch_manifest_v1",
                "defaults": {
                    "launch": {
                        "argv": ["sh", "-c", "printf should-not-run > answer.txt"],
                        "task_injection": "argv"
                    },
                    "grader": {
                        "kind": "command",
                        "requires": [missing],
                        "command": ["sh", "-c", "exit 0"]
                    },
                    "timeout_seconds": 30
                },
                "cases": [
                    {"case_id": "case-1", "task_prompt": "write answer"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let result = run_latch_manifest(LatchRunOptions {
            manifest_path,
            run_root: Some(root.path.join("runs")),
            launch_override: None,
        })
        .unwrap();
        let case = &result.cases[0];
        let grade = case.grade.as_ref().expect("grade");
        assert!(matches!(case.status, LatchCaseStatus::Declined));
        assert_eq!(grade.status, LatchGradeStatus::Declined);
        assert!(grade
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("required host dependencies are absent"));
        assert!(!case.workspace_dir.join("answer.txt").exists());
    }

    #[test]
    fn latch_command_grader_runs_when_host_deps_probe_present() {
        let root = TempDirGuard::new("bucephalus_latch_command_grader");
        let manifest_path = root.path.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "latch_manifest_v1",
                "defaults": {
                    "launch": {
                        "argv": ["sh", "-c", "printf ok > answer.txt"],
                        "task_injection": "argv"
                    },
                    "grader": {
                        "kind": "command",
                        "requires": ["sh"],
                        "command": [
                            "sh",
                            "-c",
                            "test \"$(cat answer.txt)\" = ok && printf '{\"passed\":true,\"score\":1.0,\"reason\":\"ok\"}' > \"$LATCH_GRADE_OUTPUT_PATH\""
                        ]
                    },
                    "timeout_seconds": 30
                },
                "cases": [
                    {"case_id": "case-1", "task_prompt": "write answer"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let result = run_latch_manifest(LatchRunOptions {
            manifest_path,
            run_root: Some(root.path.join("runs")),
            launch_override: None,
        })
        .unwrap();
        let grade = result.cases[0].grade.as_ref().expect("grade");
        assert!(matches!(result.cases[0].status, LatchCaseStatus::Completed));
        assert_eq!(grade.status, LatchGradeStatus::Passed);
        assert_eq!(grade.grader_kind, "command");
        assert_eq!(grade.requires.len(), 1);
        assert_eq!(
            grade.requires[0].status,
            LatchRequirementProbeStatus::Present
        );
        assert!(grade.output_path.as_ref().is_some_and(|path| path.exists()));
    }
}
