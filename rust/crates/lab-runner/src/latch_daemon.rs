use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::latch::LATCH_MANIFEST_SCHEMA;
use crate::local_storage::bucephalus_home;
use crate::util::{remove_path_if_exists, sanitize_for_fs};

const DAEMON_STATE_FILE: &str = "latchd.json";
const DAEMON_LOG_FILE: &str = "latchd.log";
const DAEMON_SOCKET_FILE: &str = "latchd.sock";
const DAEMON_START_ATTEMPTS: usize = 50;
const DAEMON_START_SLEEP: Duration = Duration::from_millis(100);

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
    fs::create_dir_all(&jobs_dir)?;
    let log_path = daemon_dir.join(DAEMON_LOG_FILE);
    let state_path = daemon_dir.join(DAEMON_STATE_FILE);
    let socket_path = daemon_dir.join(DAEMON_SOCKET_FILE);
    let _ = remove_path_if_exists(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind latch daemon socket at {}", socket_path.display()))?;
    let info = LatchDaemonInfo {
        schema_version: "latch_daemon_v1".to_string(),
        pid: std::process::id(),
        address: socket_path.display().to_string(),
        state_path: state_path.clone(),
        log_path: log_path.clone(),
        started_at: Utc::now().to_rfc3339(),
    };
    fs::write(&state_path, serde_json::to_vec_pretty(&info)?)?;
    append_daemon_log(&log_path, &format!("started {}", info.address));
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
    fs::create_dir_all(&daemon_dir)?;
    let log_path = daemon_dir.join(DAEMON_LOG_FILE);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open daemon log {}", log_path.display()))?;
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

pub fn call_latch_daemon(request: LatchDaemonRequest) -> Result<Value> {
    let info = ensure_latch_daemon()?;
    call_latch_daemon_at(&info.address, request)
}

fn call_latch_daemon_at(address: &str, request: LatchDaemonRequest) -> Result<Value> {
    let mut stream = UnixStream::connect(address)
        .with_context(|| format!("connect to latch daemon at {}", address))?;
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
        other => Err(anyhow!("unsupported latch daemon method '{}'", other)),
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
    let manifest_path = string_param(&params, "manifest_path")?;
    let run_root = optional_string_param(&params, "run_root").map(PathBuf::from);
    let argv = optional_string_array_param(&params, "argv")?;
    let job_id = format!("job_{}", Utc::now().format("%Y%m%d_%H%M%S_%6f"));
    let job_dir = jobs_dir.join(sanitize_for_fs(&job_id));
    fs::create_dir_all(&job_dir)?;
    let stdout_path = job_dir.join("stdout.json");
    let stderr_path = job_dir.join("stderr.log");
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;
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
            manifest_path: PathBuf::from(manifest_path),
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

fn daemon_progress(state: Arc<Mutex<DaemonState>>, params: Value) -> Result<Value> {
    let job_id = string_param(&params, "job_id")?;
    let mut state = state.lock().map_err(|_| anyhow!("daemon state poisoned"))?;
    refresh_jobs(&mut state)?;
    let job = state
        .jobs
        .get(&job_id)
        .ok_or_else(|| anyhow!("unknown latch daemon job '{}'", job_id))?;
    Ok(job_summary(job))
}

fn daemon_cancel(state: Arc<Mutex<DaemonState>>, params: Value) -> Result<Value> {
    let job_id = string_param(&params, "job_id")?;
    let mut state = state.lock().map_err(|_| anyhow!("daemon state poisoned"))?;
    let job = state
        .jobs
        .get_mut(&job_id)
        .ok_or_else(|| anyhow!("unknown latch daemon job '{}'", job_id))?;
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
    let max_lines = params
        .get("max_lines")
        .and_then(Value::as_u64)
        .unwrap_or(80)
        .min(500) as usize;
    let state = state.lock().map_err(|_| anyhow!("daemon state poisoned"))?;
    let job = state
        .jobs
        .get(&job_id)
        .ok_or_else(|| anyhow!("unknown latch daemon job '{}'", job_id))?;
    let path = match stream.as_str() {
        "stdout" => &job.stdout_path,
        "stderr" => &job.stderr_path,
        other => return Err(anyhow!("unsupported tail stream '{}'", other)),
    };
    Ok(json!({
        "job_id": job_id,
        "stream": stream,
        "path": path,
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
        "status": job.status.as_str(),
        "manifest_path": job.manifest_path,
        "run_root": job.run_root,
        "stdout_path": job.stdout_path,
        "stderr_path": job.stderr_path,
        "started_at": job.started_at,
        "ended_at": job.ended_at,
        "exit_code": job.exit_code,
        "result": job.result,
    })
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
    Ok(lines.join("\n"))
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
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} {}", Utc::now().to_rfc3339(), message);
    }
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

#[allow(dead_code)]
fn millis_since_epoch() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
