use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lab_runner::{bucephalus_home, LATCH_MANIFEST_SCHEMA};

const DAEMON_STATE_FILE: &str = "latchd.json";
const DAEMON_LOG_FILE: &str = "latchd.log";
const DAEMON_SOCKET_FILE: &str = "latchd.sock";
const DAEMON_START_ATTEMPTS: usize = 50;
const DAEMON_START_SLEEP: Duration = Duration::from_millis(100);
const PRIVATE_DAEMON_DIR_MODE: u32 = 0o700;
const PRIVATE_DAEMON_FILE_MODE: u32 = 0o600;
const DAEMON_TAIL_DEFAULT_LINES: usize = 80;
const DAEMON_TAIL_MAX_LINES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatchDaemonInfo {
    pub schema_version: String,
    pub pid: u32,
    pub address: String,
    pub state_path: PathBuf,
    pub log_path: PathBuf,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatchDaemonRequest {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct LatchDaemonResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Default)]
struct DaemonState {
    jobs: BTreeMap<String, JobState>,
}

struct JobState {
    job_id: String,
    manifest_path: PathBuf,
    run_root: Option<PathBuf>,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    started_at: String,
    ended_at: Option<String>,
    status: JobStatus,
    child: Option<Child>,
    exit_code: Option<i32>,
    result: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

pub fn run_latch_daemon() -> Result<()> {
    let home = bucephalus_home()?;
    let daemon_dir = home.join("daemon");
    let jobs_dir = daemon_dir.join("jobs");
    ensure_private_daemon_dir(&daemon_dir)?;
    ensure_private_daemon_dir(&jobs_dir)?;
    let log_path = daemon_dir.join(DAEMON_LOG_FILE);
    let state_path = daemon_dir.join(DAEMON_STATE_FILE);
    let socket_path = daemon_dir.join(DAEMON_SOCKET_FILE);
    let _ = remove_path_if_exists(&socket_path);
    let listener =
        UnixListener::bind(&socket_path).context("bind latch daemon socket (daemon://socket)")?;
    let info = LatchDaemonInfo {
        schema_version: "latch_daemon_v1".to_string(),
        pid: std::process::id(),
        address: socket_path.display().to_string(),
        state_path: state_path.clone(),
        log_path: log_path.clone(),
        started_at: Utc::now().to_rfc3339(),
    };
    write_private_daemon_file(&state_path, &serde_json::to_vec_pretty(&info)?)?;
    append_daemon_log(&log_path, "started daemon://socket");
    let state = Arc::new(Mutex::new(DaemonState::default()));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = Arc::clone(&state);
                let jobs_dir = jobs_dir.clone();
                let log_path = log_path.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, state, jobs_dir, log_path.clone()) {
                        append_daemon_log(&log_path, &format!("client error: {err:#}"));
                    }
                });
            }
            Err(err) => append_daemon_log(&log_path, &format!("accept error: {err}")),
        }
    }
    Ok(())
}

pub fn ensure_latch_daemon() -> Result<LatchDaemonInfo> {
    if let Ok(info) = read_daemon_info() {
        if daemon_is_live(&info) {
            return Ok(info);
        }
        let _ = remove_path_if_exists(&info.state_path);
    }
    let exe = std::env::current_exe().context("resolve current bucephalus executable")?;
    let home = bucephalus_home()?;
    let daemon_dir = home.join("daemon");
    ensure_private_daemon_dir(&daemon_dir)?;
    let log_path = daemon_dir.join(DAEMON_LOG_FILE);
    let log = append_private_daemon_log_file(&log_path)
        .context("open latch daemon log (daemon://log)")?;
    let err = log.try_clone()?;
    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .spawn()
        .context("start latch daemon")?;
    for _ in 0..DAEMON_START_ATTEMPTS {
        if let Ok(info) = read_daemon_info() {
            if daemon_is_live(&info) {
                return Ok(info);
            }
        }
        thread::sleep(DAEMON_START_SLEEP);
    }
    Err(anyhow!("latch daemon did not become ready"))
}

pub fn current_latch_daemon() -> Result<Option<LatchDaemonInfo>> {
    let Ok(info) = read_daemon_info() else {
        return Ok(None);
    };
    if daemon_is_live(&info) {
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

pub fn call_latch_daemon(request: LatchDaemonRequest) -> Result<Value> {
    let info = ensure_latch_daemon()?;
    call_latch_daemon_at(&info.address, request)
}

fn call_latch_daemon_at(address: &str, request: LatchDaemonRequest) -> Result<Value> {
    let mut stream =
        UnixStream::connect(address).context("connect to latch daemon socket (daemon://socket)")?;
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err(anyhow!("latch daemon returned an empty response"));
    }
    let response: LatchDaemonResponse = serde_json::from_str(line.trim())?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(anyhow!(
            "{}",
            response.error.unwrap_or_else(|| "daemon error".to_string())
        ))
    }
}

fn handle_client(
    stream: UnixStream,
    state: Arc<Mutex<DaemonState>>,
    jobs_dir: PathBuf,
    log_path: PathBuf,
) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let request: LatchDaemonRequest = serde_json::from_str(line.trim())?;
    let response = match handle_request(request, state, &jobs_dir, &log_path) {
        Ok(result) => LatchDaemonResponse {
            ok: true,
            result: Some(result),
            error: None,
        },
        Err(err) => LatchDaemonResponse {
            ok: false,
            result: None,
            error: Some(err.to_string()),
        },
    };
    let mut writer = stream;
    serde_json::to_writer(&mut writer, &response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn handle_request(
    request: LatchDaemonRequest,
    state: Arc<Mutex<DaemonState>>,
    jobs_dir: &Path,
    log_path: &Path,
) -> Result<Value> {
    match request.method.as_str() {
        "status" => daemon_status(state),
        "start" => daemon_start(state, jobs_dir, log_path, request.params),
        "progress" => daemon_progress(state, request.params),
        "cancel" => daemon_cancel(state, request.params),
        "tail" => daemon_tail(state, request.params),
        other => Err(anyhow!(
            "unsupported latch daemon method\n\nmethod_ref: {}\nallowed_methods: status, start, progress, cancel, tail",
            public_daemon_method_ref(other)
        )),
    }
}

fn daemon_status(state: Arc<Mutex<DaemonState>>) -> Result<Value> {
    let mut state = state.lock().map_err(|_| anyhow!("daemon state poisoned"))?;
    refresh_jobs(&mut state)?;
    let jobs = state.jobs.values().map(job_summary).collect::<Vec<_>>();
    Ok(json!({
        "schema_version": "latch_daemon_status_v1",
        "pid": std::process::id(),
        "supported_manifest_schema": LATCH_MANIFEST_SCHEMA,
        "jobs": jobs,
    }))
}

fn daemon_start(
    state: Arc<Mutex<DaemonState>>,
    jobs_dir: &Path,
    log_path: &Path,
    params: Value,
) -> Result<Value> {
    let manifest_path = validate_start_manifest_path(&string_param(&params, "manifest_path")?)?;
    let run_root = optional_string_param(&params, "run_root")
        .map(|raw| validate_start_run_root(&raw))
        .transpose()?;
    let argv = optional_string_array_param(&params, "argv")?;
    let job_id = format!("job_{}", Utc::now().format("%Y%m%d_%H%M%S_%6f"));
    let job_dir = jobs_dir.join(sanitize_for_fs(&job_id));
    ensure_private_daemon_dir(&job_dir)?;
    let stdout_path = job_dir.join("stdout.json");
    let stderr_path = job_dir.join("stderr.log");
    let stdout = create_private_daemon_file(&stdout_path)?;
    let stderr = create_private_daemon_file(&stderr_path)?;
    let exe = std::env::current_exe().context("resolve current bucephalus executable")?;
    let mut command = Command::new(exe);
    command
        .arg("latch")
        .arg("run")
        .arg(&manifest_path)
        .arg("--json");
    if let Some(run_root) = run_root.as_ref() {
        command.arg("--run-root").arg(run_root);
    }
    if let Some(argv) = argv.as_ref() {
        if !argv.is_empty() {
            command.arg("--").args(argv);
        }
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("spawn latch run job")?;
    append_daemon_log(log_path, &format!("started job {}", job_id));
    let mut state = state.lock().map_err(|_| anyhow!("daemon state poisoned"))?;
    state.jobs.insert(
        job_id.clone(),
        JobState {
            job_id: job_id.clone(),
            manifest_path,
            run_root,
            stdout_path,
            stderr_path,
            started_at: Utc::now().to_rfc3339(),
            ended_at: None,
            status: JobStatus::Running,
            child: Some(child),
            exit_code: None,
            result: None,
        },
    );
    let summary = state
        .jobs
        .get(&job_id)
        .map(job_summary)
        .ok_or_else(|| anyhow!("job vanished after start"))?;
    Ok(summary)
}

fn validate_start_manifest_path(raw: &str) -> Result<PathBuf> {
    let manifest_path = PathBuf::from(raw);
    if !manifest_path.is_file() {
        return Err(anyhow!(
            "latch manifest does not exist; pass a manifest_path from dispatch_benchmark or latch_demo_manifest, or run `bucephalus latch validate <manifest>` before starting"
        ));
    }
    lab_runner::validate_latch_manifest_file(&manifest_path).map_err(|err| {
        let redacted = redact_path_in_message(&err.to_string(), &manifest_path);
        anyhow!(
            "latch manifest is invalid; run `bucephalus latch validate <manifest>` for details: {redacted}"
        )
    })?;
    Ok(manifest_path)
}

fn validate_start_run_root(raw: &str) -> Result<PathBuf> {
    let run_root = PathBuf::from(raw);
    if run_root.as_os_str().is_empty() {
        return Err(anyhow!(
            "latch run root is invalid\n\nrun_root_ref: latch://run-root"
        ));
    }
    let run_root_ref = public_latch_run_root_ref(&run_root);
    validate_run_root_ancestors(&run_root, &run_root_ref)?;
    match fs::symlink_metadata(&run_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow!(
            "refusing to start latch job with symlinked run root\n\nrun_root_ref: {run_root_ref}"
        )),
        Ok(metadata) if !metadata.is_dir() => Err(anyhow!(
            "latch run root exists but is not a directory\n\nrun_root_ref: {run_root_ref}"
        )),
        Ok(_) => Ok(run_root),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(run_root),
        Err(err) => Err(anyhow!(
            "failed to inspect latch run root\n\nrun_root_ref: {run_root_ref}\n\nerror: {err}"
        )),
    }
}

fn validate_run_root_ancestors(run_root: &Path, run_root_ref: &str) -> Result<()> {
    for ancestor in run_root.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(anyhow!(
                    "refusing to start latch job under symlinked run root parent\n\nrun_root_ref: {run_root_ref}"
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(anyhow!(
                    "latch run root parent exists but is not a directory\n\nrun_root_ref: {run_root_ref}"
                ));
            }
            Ok(_) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow!(
                    "failed to inspect latch run root parent\n\nrun_root_ref: {run_root_ref}\n\nerror: {err}"
                ));
            }
        }
    }
    Ok(())
}

fn redact_path_in_message(message: &str, path: &Path) -> String {
    message.replace(&path.display().to_string(), "[manifest_path]")
}

fn daemon_progress(state: Arc<Mutex<DaemonState>>, params: Value) -> Result<Value> {
    let job_id = string_param(&params, "job_id")?;
    let mut state = state.lock().map_err(|_| anyhow!("daemon state poisoned"))?;
    refresh_jobs(&mut state)?;
    let job = state
        .jobs
        .get(&job_id)
        .ok_or_else(|| unknown_job_error(&job_id))?;
    Ok(job_summary(job))
}

fn daemon_cancel(state: Arc<Mutex<DaemonState>>, params: Value) -> Result<Value> {
    let job_id = string_param(&params, "job_id")?;
    let mut state = state.lock().map_err(|_| anyhow!("daemon state poisoned"))?;
    let job = state
        .jobs
        .get_mut(&job_id)
        .ok_or_else(|| unknown_job_error(&job_id))?;
    if let Some(child) = job.child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    job.child = None;
    job.status = JobStatus::Cancelled;
    job.ended_at = Some(Utc::now().to_rfc3339());
    Ok(job_summary(job))
}

fn daemon_tail(state: Arc<Mutex<DaemonState>>, params: Value) -> Result<Value> {
    let job_id = string_param(&params, "job_id")?;
    let stream = optional_string_param(&params, "stream").unwrap_or_else(|| "stderr".to_string());
    let max_lines = optional_bounded_usize_param(
        &params,
        "max_lines",
        DAEMON_TAIL_DEFAULT_LINES,
        1,
        DAEMON_TAIL_MAX_LINES,
    )?;
    let state = state.lock().map_err(|_| anyhow!("daemon state poisoned"))?;
    let job = state
        .jobs
        .get(&job_id)
        .ok_or_else(|| unknown_job_error(&job_id))?;
    let path = match stream.as_str() {
        "stdout" => &job.stdout_path,
        "stderr" => &job.stderr_path,
        other => {
            return Err(anyhow!(
                "unsupported latch daemon tail stream\n\nstream_ref: {}\nallowed_streams: stdout, stderr",
                public_daemon_stream_ref(other)
            ))
        }
    };
    Ok(json!({
        "job_id": job_id,
        "job_ref": public_daemon_job_ref(&job.job_id),
        "stream": stream,
        "path_ref": public_daemon_output_path_ref(&job.job_id, path, "tail"),
        "text": tail_file(path, max_lines)?,
    }))
}

fn refresh_jobs(state: &mut DaemonState) -> Result<()> {
    for job in state.jobs.values_mut() {
        if job.status != JobStatus::Running {
            continue;
        }
        let Some(child) = job.child.as_mut() else {
            continue;
        };
        if let Some(status) = child.try_wait()? {
            job.exit_code = status.code();
            job.ended_at = Some(Utc::now().to_rfc3339());
            job.status = if status.success() {
                JobStatus::Completed
            } else {
                JobStatus::Failed
            };
            job.child = None;
            job.result = parse_job_stdout(&job.stdout_path).ok();
        }
    }
    Ok(())
}

fn job_summary(job: &JobState) -> Value {
    json!({
        "job_id": job.job_id,
        "job_ref": public_daemon_job_ref(&job.job_id),
        "status": job.status.as_str(),
        "manifest_ref": public_latch_manifest_ref(&job.manifest_path),
        "run_root_ref": job.run_root.as_ref().map(|path| public_latch_run_root_ref(path)),
        "stdout_ref": public_daemon_output_path_ref(&job.job_id, &job.stdout_path, "stdout"),
        "stderr_ref": public_daemon_output_path_ref(&job.job_id, &job.stderr_path, "stderr"),
        "started_at": job.started_at,
        "ended_at": job.ended_at,
        "exit_code": job.exit_code,
        "result": job.result,
    })
}

fn unknown_job_error(job_id: &str) -> anyhow::Error {
    anyhow!(
        "unknown latch daemon job\n\njob_ref: {}\nNext steps:\n  Call dispatch_benchmark or latch_run_manifest to start a job, then retry with the returned job_id.",
        public_daemon_job_ref(job_id)
    )
}

fn public_latch_manifest_ref(path: &Path) -> String {
    if let Some(dispatch_ref) = bucephalus_home()
        .ok()
        .as_deref()
        .and_then(|home| public_dispatch_path_ref_from_home(home, path))
    {
        return dispatch_ref;
    }
    "latch://manifest".to_string()
}

fn public_latch_run_root_ref(path: &Path) -> String {
    if let Some(dispatch_ref) = bucephalus_home()
        .ok()
        .as_deref()
        .and_then(|home| public_dispatch_path_ref_from_home(home, path))
    {
        return dispatch_ref;
    }
    "latch://run-root".to_string()
}

fn public_dispatch_path_ref_from_home(home: &Path, path: &Path) -> Option<String> {
    let dispatches = home.join("dispatches");
    let rel = path.strip_prefix(dispatches).ok()?;
    let mut components = rel.components();
    let dispatch_id = components.next()?.as_os_str().to_string_lossy().to_string();
    if dispatch_id.trim().is_empty() {
        return None;
    }
    let dispatch_id = public_dispatch_ref_component(&dispatch_id);
    let rest = components.as_path();
    if rest.as_os_str().is_empty() {
        return Some(format!("dispatch://{dispatch_id}"));
    }
    let rest = public_dispatch_path_ref(rest);
    Some(format!("dispatch://{dispatch_id}/{rest}"))
}

fn public_dispatch_ref_component(dispatch_id: &str) -> String {
    let trimmed = dispatch_id.trim();
    if trimmed.starts_with("dispatch_")
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        public_ref_component(trimmed)
    } else {
        "redacted".to_string()
    }
}

fn public_daemon_output_path_ref(job_id: &str, path: &Path, fallback_name: &str) -> String {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map(public_ref_component)
        .unwrap_or_else(|| public_ref_component(fallback_name));
    format!("{}/{}", public_daemon_job_ref(job_id), file_name)
}

fn public_daemon_job_ref(job_id: &str) -> String {
    let component = if job_id.starts_with("job_")
        && job_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        public_ref_component(job_id)
    } else {
        "unknown".to_string()
    };
    format!("daemon://jobs/{component}")
}

fn public_daemon_method_ref(method: &str) -> String {
    format!(
        "daemon-method://{}",
        public_secret_aware_ref_component(method)
    )
}

fn public_daemon_stream_ref(stream: &str) -> String {
    format!(
        "daemon-stream://{}",
        public_secret_aware_ref_component(stream)
    )
}

fn public_secret_aware_ref_component(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("credential")
        || lower.contains("api_key")
        || lower.contains("private")
    {
        "redacted".to_string()
    } else {
        public_ref_component(value)
    }
}

fn public_dispatch_path_ref(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => {
                part.to_str().map(public_secret_aware_ref_component)
            }
            std::path::Component::CurDir => None,
            std::path::Component::ParentDir => Some("parent".to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn public_ref_component(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "value".to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_job_stdout(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_str(trimmed)?)
}

fn tail_file(path: &Path, max_lines: usize) -> Result<String> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut lines = text.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    Ok(redact_daemon_tail_text(&lines.join("\n")))
}

fn redact_daemon_tail_text(text: &str) -> String {
    text.lines()
        .map(crate::redact_public_error_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_daemon_info() -> Result<LatchDaemonInfo> {
    let path = daemon_state_path()?;
    let bytes = fs::read(&path)?;
    let mut info: LatchDaemonInfo = serde_json::from_slice(&bytes)?;
    info.state_path = path;
    Ok(info)
}

fn daemon_state_path() -> Result<PathBuf> {
    Ok(bucephalus_home()?.join("daemon").join(DAEMON_STATE_FILE))
}

fn ensure_private_daemon_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "refusing to use symlinked latch daemon state directory"
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DAEMON_DIR_MODE))?;
    Ok(())
}

fn create_private_daemon_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        ensure_private_daemon_dir(parent)?;
    }
    if daemon_path_is_symlink(path)? {
        return Err(anyhow!(
            "refusing to write latch daemon state through symlinked file"
        ));
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(PRIVATE_DAEMON_FILE_MODE)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_DAEMON_FILE_MODE))?;
    Ok(file)
}

fn append_private_daemon_log_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        ensure_private_daemon_dir(parent)?;
    }
    if daemon_path_is_symlink(path)? {
        return Err(anyhow!(
            "refusing to write latch daemon state through symlinked file"
        ));
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(PRIVATE_DAEMON_FILE_MODE)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_DAEMON_FILE_MODE))?;
    Ok(file)
}

fn write_private_daemon_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = create_private_daemon_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn daemon_path_is_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

fn daemon_is_live(info: &LatchDaemonInfo) -> bool {
    call_latch_daemon_at(
        &info.address,
        LatchDaemonRequest {
            method: "status".to_string(),
            params: json!({}),
        },
    )
    .is_ok()
}

fn append_daemon_log(path: &Path, message: &str) {
    if let Ok(mut file) = append_private_daemon_log_file(path) {
        let _ = writeln!(file, "{} {}", Utc::now().to_rfc3339(), message);
    }
}

fn sanitize_for_fs(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "job".to_string()
    } else {
        out
    }
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || meta.is_file() => fs::remove_file(path)?,
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

fn string_param(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("{} is required", key))
}

fn optional_string_param(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn optional_string_array_param(params: &Value, key: &str) -> Result<Option<Vec<String>>> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| anyhow!("{} must be an array of strings", key))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("{} entries must be strings", key))
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn optional_bounded_usize_param(
    params: &Value,
    key: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize> {
    let Some(value) = params.get(key) else {
        return Ok(default);
    };
    let Some(raw) = value.as_u64() else {
        return Err(anyhow!("{key} must be an integer from {min} to {max}"));
    };
    if raw < min as u64 || raw > max as u64 {
        return Err(anyhow!("{key} must be an integer from {min} to {max}"));
    }
    usize::try_from(raw).map_err(|_| anyhow!("{key} must be an integer from {min} to {max}"))
}

#[allow(dead_code)]
fn millis_since_epoch() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "bucephalus_latch_daemon_{label}_{}_{}",
                std::process::id(),
                nanos
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn empty_state() -> Arc<Mutex<DaemonState>> {
        Arc::new(Mutex::new(DaemonState::default()))
    }

    fn assert_no_jobs_started(state: &Arc<Mutex<DaemonState>>, jobs_dir: &Path) {
        assert!(state.lock().expect("state").jobs.is_empty());
        let job_count = fs::read_dir(jobs_dir)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        assert_eq!(job_count, 0, "daemon must not create orphan job dirs");
    }

    #[test]
    fn daemon_private_file_helpers_tighten_modes() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDirGuard::new("private_file_modes");
        let daemon_dir = temp.path.join("daemon");
        fs::create_dir_all(&daemon_dir).unwrap();
        fs::set_permissions(&daemon_dir, fs::Permissions::from_mode(0o755)).unwrap();

        let state_path = daemon_dir.join("latchd.json");
        fs::write(&state_path, "{}\n").unwrap();
        fs::set_permissions(&state_path, fs::Permissions::from_mode(0o644)).unwrap();
        write_private_daemon_file(&state_path, b"{\"ok\":true}\n").unwrap();

        let log_path = daemon_dir.join("latchd.log");
        append_daemon_log(&log_path, "started from /Users/alice/private");

        let daemon_mode = fs::metadata(&daemon_dir).unwrap().permissions().mode() & 0o777;
        let state_mode = fs::metadata(&state_path).unwrap().permissions().mode() & 0o777;
        let log_mode = fs::metadata(&log_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(daemon_mode, PRIVATE_DAEMON_DIR_MODE);
        assert_eq!(state_mode, PRIVATE_DAEMON_FILE_MODE);
        assert_eq!(log_mode, PRIVATE_DAEMON_FILE_MODE);
    }

    #[test]
    fn daemon_private_file_helpers_refuse_symlinked_paths() {
        use std::os::unix::fs::symlink;

        let temp = TempDirGuard::new("private_symlinks");
        let real_daemon = temp.path.join("real-daemon");
        fs::create_dir_all(&real_daemon).unwrap();
        let daemon_link = temp.path.join("daemon-link");
        symlink(&real_daemon, &daemon_link).unwrap();

        let dir_err = ensure_private_daemon_dir(&daemon_link)
            .unwrap_err()
            .to_string();
        assert!(dir_err.contains("symlinked latch daemon state directory"));

        let daemon_dir = temp.path.join("daemon");
        ensure_private_daemon_dir(&daemon_dir).unwrap();
        let target = temp.path.join("target-state");
        fs::write(&target, "original\n").unwrap();
        let state_link = daemon_dir.join("latchd.json");
        symlink(&target, &state_link).unwrap();

        let file_err = write_private_daemon_file(&state_link, b"replacement\n")
            .unwrap_err()
            .to_string();
        assert!(file_err.contains("symlinked file"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "original\n");
    }

    fn sample_finished_job(temp: &TempDirGuard) -> JobState {
        let stdout_path = temp
            .path
            .join("daemon")
            .join("jobs")
            .join("job_1")
            .join("stdout.json");
        let stderr_path = temp
            .path
            .join("daemon")
            .join("jobs")
            .join("job_1")
            .join("stderr.log");
        if let Some(parent) = stdout_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&stdout_path, "{}\n").unwrap();
        fs::write(&stderr_path, "stderr\n").unwrap();
        JobState {
            job_id: "job_1".to_string(),
            manifest_path: temp
                .path
                .join("private")
                .join("customer-a")
                .join("manifest.json"),
            run_root: Some(temp.path.join("private").join("customer-a").join("runs")),
            stdout_path,
            stderr_path,
            started_at: "2026-06-09T00:00:00Z".to_string(),
            ended_at: Some("2026-06-09T00:00:01Z".to_string()),
            status: JobStatus::Completed,
            child: None,
            exit_code: Some(0),
            result: Some(json!({"ok": true})),
        }
    }

    fn write_valid_latch_manifest(path: &Path) {
        fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "latch_manifest_v1",
                "defaults": {
                    "launch": {
                        "argv": ["sh", "-c", "true"],
                        "task_injection": "argv"
                    },
                    "timeout_seconds": 30
                },
                "cases": [
                    {"case_id": "case-1", "task_prompt": "hello"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn job_summary_uses_public_refs_without_local_paths() {
        let temp = TempDirGuard::new("job_summary_public_refs");
        let summary = job_summary(&sample_finished_job(&temp));
        let encoded = serde_json::to_string_pretty(&summary).unwrap();

        assert_eq!(summary["job_ref"], "daemon://jobs/job_1");
        assert_eq!(summary["manifest_ref"], "latch://manifest");
        assert_eq!(summary["run_root_ref"], "latch://run-root");
        assert_eq!(summary["stdout_ref"], "daemon://jobs/job_1/stdout.json");
        assert_eq!(summary["stderr_ref"], "daemon://jobs/job_1/stderr.log");
        assert!(summary.get("manifest_path").is_none());
        assert!(summary.get("run_root").is_none());
        assert!(summary.get("stdout_path").is_none());
        assert!(summary.get("stderr_path").is_none());
        for forbidden in [
            temp.path.display().to_string(),
            "private/customer-a".to_string(),
            "manifest_path".to_string(),
            "stdout_path".to_string(),
            "stderr_path".to_string(),
        ] {
            assert!(
                !encoded.contains(&forbidden),
                "job summary leaked forbidden text {forbidden}: {encoded}"
            );
        }
    }

    #[test]
    fn dispatch_path_refs_redact_untrusted_ids_and_secret_like_components() {
        let temp = TempDirGuard::new("dispatch_public_refs");
        let path = temp
            .path
            .join("dispatches")
            .join("dispatch_sk-live-secret")
            .join("private")
            .join("customer-token")
            .join("manifest.json");

        let rendered = public_dispatch_path_ref_from_home(&temp.path, &path)
            .expect("path under dispatches should produce public ref");

        assert_eq!(
            rendered,
            "dispatch://redacted/redacted/redacted/manifest.json"
        );
        for forbidden in [
            temp.path.display().to_string(),
            "dispatch_sk-live-secret".to_string(),
            "sk-live-secret".to_string(),
            "private".to_string(),
            "customer-token".to_string(),
        ] {
            assert!(
                !rendered.contains(&forbidden),
                "dispatch ref leaked forbidden text {forbidden}: {rendered}"
            );
        }
    }

    #[test]
    fn unknown_job_errors_do_not_echo_caller_supplied_job_id() {
        let err = daemon_progress(
            empty_state(),
            json!({ "job_id": "/Users/alice/private/token-raw-job-id" }),
        )
        .expect_err("unknown job should fail");
        let message = err.to_string();

        assert!(message.contains("unknown latch daemon job"));
        assert!(message.contains("job_ref: daemon://jobs/unknown"));
        assert!(message.contains("dispatch_benchmark"));
        for forbidden in ["/Users/alice", "private/token-raw-job-id", "raw-job-id"] {
            assert!(
                !message.contains(forbidden),
                "unknown job error leaked forbidden text {forbidden}: {message}"
            );
        }
    }

    #[test]
    fn tail_rejects_unknown_stream_without_echoing_secret_like_value() {
        let temp = TempDirGuard::new("tail_stream_public_refs");
        let state = empty_state();
        state
            .lock()
            .unwrap()
            .jobs
            .insert("job_1".to_string(), sample_finished_job(&temp));

        let err = daemon_tail(
            state,
            json!({ "job_id": "job_1", "stream": "stderr-token=/Users/alice/private" }),
        )
        .expect_err("unsupported stream should fail");
        let message = err.to_string();

        assert!(message.contains("unsupported latch daemon tail stream"));
        assert!(message.contains("stream_ref: daemon-stream://redacted"));
        assert!(message.contains("allowed_streams: stdout, stderr"));
        for forbidden in ["/Users/alice", "stderr-token", "private"] {
            assert!(
                !message.contains(forbidden),
                "tail stream error leaked forbidden text {forbidden}: {message}"
            );
        }
    }

    #[test]
    fn tail_rejects_invalid_max_lines_without_echoing_value() {
        let temp = TempDirGuard::new("tail_max_lines_public_error");
        let state = empty_state();
        state
            .lock()
            .unwrap()
            .jobs
            .insert("job_1".to_string(), sample_finished_job(&temp));

        for params in [
            json!({ "job_id": "job_1", "stream": "stderr", "max_lines": 0 }),
            json!({ "job_id": "job_1", "stream": "stderr", "max_lines": "token=/Users/alice/private" }),
            json!({ "job_id": "job_1", "stream": "stderr", "max_lines": 501 }),
        ] {
            let err =
                daemon_tail(Arc::clone(&state), params).expect_err("invalid max_lines should fail");
            let message = err.to_string();

            assert!(message.contains("max_lines must be an integer from 1 to 500"));
            for forbidden in ["/Users/alice", "token=", "private"] {
                assert!(
                    !message.contains(forbidden),
                    "max_lines error leaked forbidden text {forbidden}: {message}"
                );
            }
        }
    }

    #[test]
    fn tail_redacts_local_paths_and_secret_like_text() {
        let temp = TempDirGuard::new("tail_text_redaction");
        let state = empty_state();
        let job = sample_finished_job(&temp);
        fs::write(
            &job.stderr_path,
            "token=raw-tail-token\nworkspace=/Users/alice/private/project\nAuthorization: Bearer live-token\ncallback=https://user:secret@example.com/cb?token=raw-url-token#frag\nlog=file:///private/tmp/latch.log\n",
        )
        .unwrap();
        state.lock().unwrap().jobs.insert(job.job_id.clone(), job);

        let response = daemon_tail(
            state,
            json!({ "job_id": "job_1", "stream": "stderr", "max_lines": 20 }),
        )
        .expect("tail should succeed");
        let text = response["text"].as_str().expect("tail text");
        let encoded = serde_json::to_string_pretty(&response).unwrap();

        assert_eq!(response["path_ref"], "daemon://jobs/job_1/stderr.log");
        assert!(text.contains("token=[REDACTED:secret-like]"));
        assert!(text.contains("workspace=[REDACTED:local-path]"));
        assert!(text.contains("[REDACTED:secret-like]"));
        assert!(text.contains("callback=https://example.com/cb [redacted URL credentials/query]"));
        assert!(text.contains("log=file://[REDACTED:local-path]"));
        for forbidden in [
            "/Users/alice",
            "/private/tmp",
            "raw-tail-token",
            "live-token",
            "raw-url-token",
            "user:secret",
            "#frag",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "daemon tail leaked forbidden text {forbidden}: {encoded}"
            );
        }
    }

    #[test]
    fn daemon_connect_errors_do_not_echo_socket_paths() {
        let temp = TempDirGuard::new("daemon_connect_public_error");
        let address = temp
            .path
            .join("daemon")
            .join("token=raw-socket-secret")
            .join("latchd.sock")
            .display()
            .to_string();

        let err = call_latch_daemon_at(
            &address,
            LatchDaemonRequest {
                method: "status".to_string(),
                params: json!({}),
            },
        )
        .expect_err("missing daemon socket should fail");
        let message = err.to_string();

        assert!(message.contains("connect to latch daemon socket"));
        assert!(message.contains("daemon://socket"));
        for forbidden in [
            temp.path.display().to_string(),
            "raw-socket-secret".to_string(),
            "token=raw-socket-secret".to_string(),
        ] {
            assert!(
                !message.contains(&forbidden),
                "daemon connect error leaked forbidden text {forbidden}: {message}"
            );
        }
    }

    #[test]
    fn unsupported_method_error_uses_public_ref_and_allowed_methods() {
        let temp = TempDirGuard::new("unsupported_method_public_ref");
        let err = handle_request(
            LatchDaemonRequest {
                method: "token=/Users/alice/private".to_string(),
                params: json!({}),
            },
            empty_state(),
            &temp.path.join("jobs"),
            &temp.path.join("latchd.log"),
        )
        .expect_err("unsupported method should fail");
        let message = err.to_string();

        assert!(message.contains("unsupported latch daemon method"));
        assert!(message.contains("method_ref: daemon-method://redacted"));
        assert!(message.contains("allowed_methods: status, start, progress, cancel, tail"));
        for forbidden in ["/Users/alice", "token=", "private"] {
            assert!(
                !message.contains(forbidden),
                "unsupported method error leaked forbidden text {forbidden}: {message}"
            );
        }
    }

    #[test]
    fn daemon_start_rejects_missing_manifest_without_creating_job_dir() {
        let temp = TempDirGuard::new("missing_manifest");
        let jobs_dir = temp.path.join("jobs");
        fs::create_dir_all(&jobs_dir).unwrap();
        let state = empty_state();

        let err = daemon_start(
            Arc::clone(&state),
            &jobs_dir,
            &temp.path.join("latchd.log"),
            json!({ "manifest_path": temp.path.join("missing.json") }),
        )
        .expect_err("missing manifest should be rejected before spawn");

        let message = err.to_string();
        assert!(message.contains("latch manifest does not exist"));
        assert!(!message.contains(&temp.path.display().to_string()));
        assert_no_jobs_started(&state, &jobs_dir);
    }

    #[test]
    fn daemon_start_rejects_malformed_manifest_without_path_leak_or_job_dir() {
        let temp = TempDirGuard::new("malformed_manifest");
        let jobs_dir = temp.path.join("jobs");
        fs::create_dir_all(&jobs_dir).unwrap();
        let manifest_path = temp.path.join("manifest.json");
        fs::write(&manifest_path, "{not-json").unwrap();
        let state = empty_state();

        let err = daemon_start(
            Arc::clone(&state),
            &jobs_dir,
            &temp.path.join("latchd.log"),
            json!({ "manifest_path": manifest_path }),
        )
        .expect_err("malformed manifest should be rejected before spawn");

        let message = err.to_string();
        assert!(message.contains("latch manifest is invalid"));
        assert!(message.contains("[manifest_path]"));
        assert!(!message.contains(&temp.path.display().to_string()));
        assert_no_jobs_started(&state, &jobs_dir);
    }

    #[test]
    fn daemon_start_rejects_unsafe_run_root_without_creating_job_dir() {
        use std::os::unix::fs::symlink;

        let temp = TempDirGuard::new("unsafe_run_root");
        let jobs_dir = temp.path.join("jobs");
        fs::create_dir_all(&jobs_dir).unwrap();
        let manifest_path = temp.path.join("manifest.json");
        write_valid_latch_manifest(&manifest_path);
        let outside = temp.path.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let direct_link = temp.path.join("runs-link");
        symlink(&outside, &direct_link).unwrap();
        let parent_link = temp.path.join("parent-link");
        symlink(&outside, &parent_link).unwrap();
        let nested_under_parent_link = parent_link.join("nested");
        let file_root = temp.path.join("runs-file");
        fs::write(&file_root, "not a directory\n").unwrap();

        for (run_root, expected) in [
            (&direct_link, "symlinked run root"),
            (&nested_under_parent_link, "symlinked run root parent"),
            (&file_root, "run root exists but is not a directory"),
        ] {
            let state = empty_state();
            let err = daemon_start(
                Arc::clone(&state),
                &jobs_dir,
                &temp.path.join("latchd.log"),
                json!({
                    "manifest_path": manifest_path.display().to_string(),
                    "run_root": run_root.display().to_string()
                }),
            )
            .expect_err("unsafe run root should be rejected before spawn");
            let message = err.to_string();

            assert!(message.contains(expected), "{message}");
            assert!(message.contains("run_root_ref: latch://run-root"));
            for forbidden in [
                temp.path.display().to_string(),
                outside.display().to_string(),
                "runs-link".to_string(),
                "parent-link".to_string(),
                "runs-file".to_string(),
            ] {
                assert!(
                    !message.contains(&forbidden),
                    "unsafe run root error leaked forbidden text {forbidden}: {message}"
                );
            }
            assert_no_jobs_started(&state, &jobs_dir);
        }
    }
}
