use anyhow::{anyhow, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const BUCEPHALUS_HOME_ENV: &str = "BUCEPHALUS_HOME";

pub const BUCEPHALUS_CONTRACT_IN_DIR: &str = "/bucephalus/in";
pub const BUCEPHALUS_CONTRACT_OUT_DIR: &str = "/bucephalus/out";
pub const BUCEPHALUS_CONTRACT_STATE_DIR: &str = "/bucephalus/state";
pub const BUCEPHALUS_CONTRACT_EVENTS_DIR: &str = "/bucephalus-events";
pub const BUCEPHALUS_CONTRACT_WORKSPACE_DIR: &str = "/bucephalus/workspace";
pub const BUCEPHALUS_CONTRACT_METRICS_DIR: &str = "/bucephalus/metrics";
pub const BUCEPHALUS_CONTRACT_GRADER_AUX_DIR: &str = "/bucephalus/in/grader";
pub const BUCEPHALUS_CONTRACT_RUNTIME_AUX_DIR: &str = "/bucephalus/in/runtime";
pub const BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER: &str = "__BUCEPHALUS_TASK_WORKDIR__";
pub const BUCEPHALUS_RUNNER_SUPPORT_REL_DIR: &str = ".bucephalus/support";

pub const BUCEPHALUS_TRIAL_INPUT_PATH: &str = "/bucephalus/in/trial_input.json";
pub const BUCEPHALUS_GRADER_INPUT_PATH: &str = "/bucephalus/in/grader_input.json";
pub const BUCEPHALUS_RESULT_PATH: &str = "/bucephalus/out/result.json";
pub const BUCEPHALUS_RAW_GRADER_OUTPUT_PATH: &str = "/bucephalus/out/raw_grader_output.json";
pub const BUCEPHALUS_MAPPED_GRADER_OUTPUT_PATH: &str = "/bucephalus/out/mapped_grader_output.json";
pub const BUCEPHALUS_TRAJECTORY_PATH: &str = "/bucephalus-events/trajectory.jsonl";
pub const BUCEPHALUS_EVENTS_DURABLE_PATH: &str = "/bucephalus/out/events/trajectory.jsonl";

pub const BUCEPHALUS_ENV_TIMEOUT_MS: &str = "BUCEPHALUS_TIMEOUT_MS";
pub const BUCEPHALUS_ENV_RUN_ID: &str = "BUCEPHALUS_RUN_ID";
pub const BUCEPHALUS_ENV_TRIAL_ID: &str = "BUCEPHALUS_TRIAL_ID";
pub const BUCEPHALUS_ENV_VARIANT_ID: &str = "BUCEPHALUS_VARIANT_ID";
pub const BUCEPHALUS_ENV_CASE_ID: &str = "BUCEPHALUS_CASE_ID";
pub const BUCEPHALUS_ENV_TASK_ID: &str = "BUCEPHALUS_TASK_ID";
pub const BUCEPHALUS_ENV_REPL_IDX: &str = "BUCEPHALUS_REPL_IDX";
pub const BUCEPHALUS_ENV_TRIAL_INPUT_PATH: &str = "BUCEPHALUS_TRIAL_INPUT_PATH";
pub const BUCEPHALUS_ENV_GRADER_INPUT_PATH: &str = "BUCEPHALUS_GRADER_INPUT_PATH";
pub const BUCEPHALUS_ENV_RESULT_PATH: &str = "BUCEPHALUS_RESULT_PATH";
pub const BUCEPHALUS_ENV_RAW_GRADER_OUTPUT_PATH: &str = "BUCEPHALUS_RAW_GRADER_OUTPUT_PATH";
pub const BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH: &str = "BUCEPHALUS_MAPPED_GRADER_OUTPUT_PATH";
pub const BUCEPHALUS_ENV_TRAJECTORY_PATH: &str = "BUCEPHALUS_TRAJECTORY_PATH";

#[derive(Debug, Clone)]
pub struct RunnerRuntimeHostPaths {
    pub in_dir: PathBuf,
    pub out_dir: PathBuf,
    pub state_dir: PathBuf,
    pub workspace_dir: PathBuf,
    pub tmp_dir: PathBuf,
    pub events_dir: PathBuf,
    pub grader_input: PathBuf,
    pub result: PathBuf,
    pub raw_grader_output: PathBuf,
    pub mapped_grader_output: PathBuf,
    pub trajectory: PathBuf,
    pub trial_input: PathBuf,
    pub control: PathBuf,
}

pub fn runner_runtime_host_paths(trial_dir: &Path) -> RunnerRuntimeHostPaths {
    let in_dir = trial_dir.join("in");
    let out_dir = trial_dir.join("out");
    let state_dir = trial_dir.join("state");
    let workspace_dir = trial_dir.join("workspace");
    let events_dir = trial_dir.join("events");
    RunnerRuntimeHostPaths {
        grader_input: in_dir.join("grader_input.json"),
        result: out_dir.join("result.json"),
        raw_grader_output: out_dir.join("raw_grader_output.json"),
        mapped_grader_output: out_dir.join("mapped_grader_output.json"),
        trajectory: events_dir.join("trajectory.jsonl"),
        trial_input: in_dir.join("trial_input.json"),
        control: in_dir.join("runtime").join("lab_control.json"),
        in_dir,
        out_dir,
        state_dir,
        workspace_dir,
        tmp_dir: trial_dir.join("tmp"),
        events_dir,
    }
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical_json(value, &mut out);
    out
}

fn write_canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write!(out, "{}", n).expect("writing to String cannot fail"),
        Value::String(s) => {
            out.push_str(&serde_json::to_string(s).expect("serializing a JSON string cannot fail"))
        }
        Value::Array(arr) => {
            out.push('[');
            for (idx, item) in arr.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_canonical_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (idx, key) in keys.into_iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                out.push_str(
                    &serde_json::to_string(key).expect("serializing a JSON key cannot fail"),
                );
                out.push(':');
                let value = map
                    .get(key)
                    .expect("key collected from serde_json object must exist");
                write_canonical_json(value, out);
            }
            out.push('}');
        }
    }
}

pub fn canonical_json_digest(value: &Value) -> String {
    let canonical = canonical_json(value);
    sha256_bytes(canonical.as_bytes())
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

fn require_absolute_env_path(name: &str) -> Result<Option<PathBuf>> {
    let Some(raw) = std::env::var_os(name) else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(anyhow!("{} must be an absolute path", name));
    }
    Ok(Some(path))
}

pub fn bucephalus_home() -> Result<PathBuf> {
    if let Some(path) = require_absolute_env_path(BUCEPHALUS_HOME_ENV)? {
        return Ok(path);
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; set {}", BUCEPHALUS_HOME_ENV))?;
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Bucephalus"))
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            Ok(appdata.join("Bucephalus"))
        } else {
            let home = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .ok_or_else(|| {
                    anyhow!(
                        "APPDATA and USERPROFILE are not set; set {}",
                        BUCEPHALUS_HOME_ENV
                    )
                })?;
            Ok(home.join("AppData").join("Roaming").join("Bucephalus"))
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
            Ok(data_home.join("bucephalus"))
        } else {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("HOME is not set; set {}", BUCEPHALUS_HOME_ENV))?;
            Ok(home.join(".local").join("share").join("bucephalus"))
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; set {}", BUCEPHALUS_HOME_ENV))?;
        Ok(home.join(".bucephalus"))
    }
}

/// Persisted Cloud connection profile. `bucephalus login` writes it once;
/// every Cloud-facing command resolves its API URL and OAuth parameters from
/// flags, then env, then this profile.
pub fn cloud_profile_path(home: &Path) -> PathBuf {
    home.join("cloud.json")
}

pub fn read_cloud_profile(home: &Path) -> Option<Value> {
    let raw = fs::read_to_string(cloud_profile_path(home)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn write_cloud_profile(home: &Path, profile: &Value) -> Result<()> {
    fs::create_dir_all(home)?;
    let path = cloud_profile_path(home);
    fs::write(&path, format!("{:#}\n", profile))
        .map_err(|err| anyhow!("failed to write cloud profile {}: {err}", path.display()))
}

pub fn cloud_profile_string(home: &Path, pointer: &str) -> Option<String> {
    read_cloud_profile(home)?
        .pointer(pointer)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub struct ArtifactStore {
    root: PathBuf,
}

fn parse_artifact_sha256_ref(artifact_ref: &str) -> Result<&str> {
    let hex = artifact_ref
        .strip_prefix("artifact://sha256/")
        .ok_or_else(|| anyhow!("invalid artifact ref"))?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid artifact digest"));
    }
    Ok(hex)
}

impl ArtifactStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> Result<String> {
        let digest = sha256_bytes(bytes);
        let hex = digest
            .strip_prefix("sha256:")
            .expect("sha256_bytes must return a sha256-prefixed digest");
        let dir = self.root.join("sha256").join(hex);
        ensure_dir(&dir)?;
        let path = dir.join("blob");
        if !path.exists() {
            fs::write(&path, bytes)?;
        }
        Ok(format!("artifact://sha256/{}", hex))
    }

    pub fn put_file(&self, path: &Path) -> Result<String> {
        let bytes = fs::read(path)?;
        self.put_bytes(&bytes)
    }

    pub fn read_ref(&self, artifact_ref: &str) -> Result<Vec<u8>> {
        let hex = parse_artifact_sha256_ref(artifact_ref)?;
        let path = self.root.join("sha256").join(hex).join("blob");
        Ok(fs::read(path)?)
    }
}

pub fn hashchain(prev: Option<&str>, line: &str) -> String {
    let mut hasher = Sha256::new();
    if let Some(p) = prev {
        hasher.update(p.as_bytes());
    }
    hasher.update(line.as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}
