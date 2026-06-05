use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use lab_core::{
    ensure_dir, sha256_bytes, BUCEPHALUS_ENV_CASE_ID, BUCEPHALUS_ENV_RESULT_PATH,
    BUCEPHALUS_ENV_TASK_ID, BUCEPHALUS_ENV_TRAJECTORY_PATH, BUCEPHALUS_ENV_TRIAL_INPUT_PATH,
};
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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatchCaseStatus {
    Completed,
    Errored,
    TimedOut,
    IdleTimedOut,
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
    ensure_dir(&workspace_dir)?;
    ensure_dir(&out_dir)?;
    ensure_dir(&events_dir)?;
    fs::write(
        case_dir.join("case_manifest.json"),
        serde_json::to_vec_pretty(case)?,
    )?;

    let seed = case
        .workspace_seed
        .as_ref()
        .or(manifest.defaults.workspace_seed.as_ref())
        .cloned()
        .unwrap_or(WorkspaceSeed::Empty);
    materialize_workspace_seed(manifest_dir, &workspace_dir, &seed)?;
    let baseline = prepare_workspace_baseline(&workspace_dir);

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
    let ended_at = Utc::now().to_rfc3339();
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
    };
    fs::write(
        case_dir.join("case_result.json"),
        serde_json::to_vec_pretty(&case_result)?,
    )?;
    Ok(case_result)
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
}
