use anyhow::{anyhow, Context, Result};
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use zip::write::FileOptions;
use zip::DateTime;

const REDACTED_SECRET: &str = "[REDACTED:secret]";
const REDACTED_CONTENT: &str = "[REDACTED:content]";
const REDACTED_ENV: &str = "[REDACTED:environment]";
const REDACTED_LOCAL_PATH: &str = "[REDACTED:local-path]";
const REDACTED_SECRET_LIKE: &str = "[REDACTED:secret-like]";
const DEBUG_BUNDLE_MANIFEST: &str = "debug-bundle-manifest.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugBundleReport {
    pub bundle_path: PathBuf,
    pub included: Vec<DebugBundleIncluded>,
    pub skipped: Vec<DebugBundleSkipped>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugBundleIncluded {
    pub path: String,
    pub kind: String,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugBundleSkipped {
    pub path: String,
    pub reason: String,
}

impl DebugBundleReport {
    fn new(bundle_path: PathBuf) -> Self {
        Self {
            bundle_path,
            included: Vec::new(),
            skipped: Vec::new(),
        }
    }

    pub fn included_count(&self) -> usize {
        self.included.len()
    }

    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    pub fn redacted_count(&self) -> usize {
        self.included.iter().filter(|entry| entry.redacted).count()
    }

    fn include(&mut self, path: String, kind: &str, redacted: bool) {
        self.included.push(DebugBundleIncluded {
            path,
            kind: kind.to_string(),
            redacted,
        });
    }

    fn skip(&mut self, path: String, reason: impl Into<String>) {
        self.skipped.push(DebugBundleSkipped {
            path,
            reason: reason.into(),
        });
    }
}

pub fn write_attestation(run_dir: &Path, payload: serde_json::Value) -> Result<()> {
    let path = run_dir.join("attestation.json");
    fs::write(path, serde_json::to_vec_pretty(&payload)?)?;
    Ok(())
}

pub fn default_attestation(
    resolved_digest: &str,
    image_digest: Option<&str>,
    grades: serde_json::Value,
    events_heads: Vec<(String, String)>,
    harness: serde_json::Value,
    trace_mode: &str,
) -> serde_json::Value {
    let heads: Vec<serde_json::Value> = events_heads
        .into_iter()
        .map(|(trial_id, head)| json!({"trial_id": trial_id, "head": head}))
        .collect();
    json!({
        "schema_version": "attestation_v1",
        "resolved_experiment_digest": resolved_digest,
        "image_digest": image_digest,
        "events_hashchain_heads": heads,
        "grades": grades,
        "harness": harness,
        "trace_ingestion": trace_mode,
    })
}

pub fn build_debug_bundle(run_dir: &Path, out_path: &Path) -> Result<DebugBundleReport> {
    if !run_dir.is_dir() {
        return Err(anyhow!(
            "run directory does not exist\nrun_ref: {}",
            public_run_ref(run_dir)
        ));
    }
    ensure_debug_bundle_output_allowed(run_dir, out_path)?;
    let output_ref = public_debug_bundle_output_ref(run_dir, out_path);
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| {
                anyhow!(
                    "failed to create debug bundle output directory\noutput_ref: {}\nerror: {}",
                    output_ref,
                    public_io_error(&err)
                )
            })?;
        }
    }
    let file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out_path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            return Err(anyhow!(
                "debug bundle already exists\noutput_ref: {}\n\nNext steps:\n  choose a different --out path or remove the existing file.",
                output_ref
            ));
        }
        Err(err) => {
            return Err(anyhow!(
                "failed to create debug bundle\noutput_ref: {}\nerror: {}",
                output_ref,
                public_io_error(&err)
            ));
        }
    };

    let mut report = DebugBundleReport::new(out_path.to_path_buf());
    let mut zip = zip::ZipWriter::new(file);
    let opts = debug_bundle_file_options()?;

    include_top_level_file(
        &mut zip,
        &mut report,
        opts,
        run_dir,
        &run_dir.join("manifest.json"),
        BundleFileKind::Json,
    )?;
    include_top_level_file(
        &mut zip,
        &mut report,
        opts,
        run_dir,
        &run_dir.join("resolved_experiment.json"),
        BundleFileKind::Json,
    )?;
    include_top_level_file(
        &mut zip,
        &mut report,
        opts,
        run_dir,
        &run_dir.join("resolved_experiment.digest"),
        BundleFileKind::PlainText,
    )?;
    include_top_level_file(
        &mut zip,
        &mut report,
        opts,
        run_dir,
        &run_dir.join("attestation.json"),
        BundleFileKind::Json,
    )?;

    let trials_dir = run_dir.join("trials");
    if trials_dir.exists() {
        for entry in walkdir::WalkDir::new(&trials_dir) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    report.skip(
                        public_walkdir_error_path(run_dir, &err),
                        public_walkdir_error_reason(&err),
                    );
                    continue;
                }
            };
            if entry.file_type().is_dir() {
                continue;
            }
            let path = entry.path();
            let name = bundle_name(run_dir, path)?;
            if !entry.file_type().is_file() {
                report.skip(name, "non_regular_file");
                continue;
            }
            match trial_artifact_decision(run_dir, path, &name) {
                TrialArtifactDecision::Include(kind) => {
                    include_redacted_file(&mut zip, &mut report, opts, path, name, kind)?;
                }
                TrialArtifactDecision::Skip(reason) => report.skip(name, reason),
            }
        }
    }

    report.include(DEBUG_BUNDLE_MANIFEST.to_string(), "bundle_manifest", false);
    let manifest = debug_bundle_manifest_json(&report);
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    write_zip_bytes(&mut zip, opts, DEBUG_BUNDLE_MANIFEST, &manifest_bytes)?;

    zip.finish()?;
    Ok(report)
}

fn ensure_debug_bundle_output_allowed(run_dir: &Path, out_path: &Path) -> Result<()> {
    let trials_dir = lexical_absolute(&run_dir.join("trials"))?;
    let out_path = lexical_absolute(out_path)?;
    if out_path.starts_with(&trials_dir) {
        return Err(anyhow!(
            "debug bundle output must not be inside the run trials directory; omit --out to use the default debug_bundles directory or choose a path outside run/trials"
        ));
    }
    Ok(())
}

fn debug_bundle_file_options() -> Result<FileOptions> {
    let timestamp = DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
        .map_err(|()| anyhow!("invalid fixed debug bundle timestamp"))?;
    Ok(FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(timestamp)
        .unix_permissions(0o644))
}

#[derive(Clone, Copy, Debug)]
enum BundleFileKind {
    Json,
    Jsonl,
    PlainText,
}

impl BundleFileKind {
    fn label(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Jsonl => "jsonl",
            Self::PlainText => "text",
        }
    }
}

fn include_top_level_file(
    zip: &mut zip::ZipWriter<fs::File>,
    report: &mut DebugBundleReport,
    opts: FileOptions,
    run_dir: &Path,
    path: &Path,
    kind: BundleFileKind,
) -> Result<()> {
    let name = bundle_name(run_dir, path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            report.skip(name, format!("inspect_failed: {err}"));
            return Ok(());
        }
    };
    if !metadata.is_file() {
        report.skip(name, "non_regular_file");
        return Ok(());
    }
    include_redacted_file(zip, report, opts, path, name, kind)
}

fn include_redacted_file(
    zip: &mut zip::ZipWriter<fs::File>,
    report: &mut DebugBundleReport,
    opts: FileOptions,
    path: &Path,
    name: String,
    kind: BundleFileKind,
) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            report.skip(name, "non_regular_file");
            return Ok(());
        }
        Err(err) => {
            report.skip(name, format!("inspect_failed: {err}"));
            return Ok(());
        }
    }
    let data = fs::read(path).with_context(|| format!("failed to read bundle entry {name}"))?;
    let (bytes, redacted) = match kind {
        BundleFileKind::Json => match redacted_json_bytes(&data) {
            Ok(result) => result,
            Err(err) => {
                report.skip(name, format!("invalid_json: {err}"));
                return Ok(());
            }
        },
        BundleFileKind::Jsonl => match redacted_jsonl_bytes(&data) {
            Ok(result) => result,
            Err(err) => {
                report.skip(name, format!("invalid_jsonl: {err}"));
                return Ok(());
            }
        },
        BundleFileKind::PlainText => (data, false),
    };
    write_zip_bytes(zip, opts, &name, &bytes)?;
    report.include(name, kind.label(), redacted);
    Ok(())
}

fn write_zip_bytes(
    zip: &mut zip::ZipWriter<fs::File>,
    opts: FileOptions,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    zip.start_file(name, opts)?;
    zip.write_all(bytes)?;
    Ok(())
}

fn redacted_json_bytes(data: &[u8]) -> Result<(Vec<u8>, bool)> {
    let mut value: Value = serde_json::from_slice(data)?;
    let redacted = redact_json_value(&mut value);
    Ok((serde_json::to_vec_pretty(&value)?, redacted))
}

fn redacted_jsonl_bytes(data: &[u8]) -> Result<(Vec<u8>, bool)> {
    let text = std::str::from_utf8(data).context("not utf-8")?;
    let mut out = String::new();
    let mut redacted = false;
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut value: Value = serde_json::from_str(trimmed)
            .with_context(|| format!("line {} is not JSON", idx + 1))?;
        redacted |= redact_json_value(&mut value);
        out.push_str(&serde_json::to_string(&value)?);
        out.push('\n');
    }
    Ok((out.into_bytes(), redacted))
}

fn redact_json_value(value: &mut Value) -> bool {
    match value {
        Value::Object(map) => {
            let mut redacted = false;
            for (key, child) in map.iter_mut() {
                if let Some(reason) = redaction_for_key(key) {
                    *child = Value::String(reason.to_string());
                    redacted = true;
                } else {
                    redacted |= redact_json_value(child);
                }
            }
            redacted
        }
        Value::Array(items) => items
            .iter_mut()
            .fold(false, |redacted, item| redact_json_value(item) || redacted),
        Value::String(text) => {
            if let Some(reason) = redaction_for_string(text) {
                *value = Value::String(reason.to_string());
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

fn redaction_for_key(key: &str) -> Option<&'static str> {
    let normalized = normalize_key(key);
    const SECRET_KEY_FRAGMENTS: &[&str] = &[
        "secret",
        "token",
        "password",
        "credential",
        "apikey",
        "authorization",
        "bearer",
        "privatekey",
        "clientsecret",
        "clientid",
        "cookie",
        "header",
        "session",
        "refresh",
    ];
    if normalized == "auth"
        || normalized.ends_with("auth")
        || SECRET_KEY_FRAGMENTS
            .iter()
            .any(|fragment| normalized.contains(fragment))
    {
        return Some(REDACTED_SECRET);
    }
    if normalized == "env" || normalized.ends_with("env") || normalized.contains("environment") {
        return Some(REDACTED_ENV);
    }
    if normalized == "path"
        || normalized.ends_with("path")
        || normalized.contains("filepath")
        || normalized.contains("localpath")
        || normalized.contains("workspace")
        || normalized.contains("workdir")
        || normalized.contains("mount")
    {
        return Some(REDACTED_LOCAL_PATH);
    }
    const CONTENT_KEYS: &[&str] = &[
        "prompt",
        "completion",
        "message",
        "content",
        "input",
        "output",
        "answer",
        "response",
        "stdout",
        "stderr",
        "raw",
        "rawline",
        "command",
        "args",
        "argv",
        "url",
        "uri",
        "endpoint",
        "request",
        "body",
        "payload",
    ];
    if CONTENT_KEYS.iter().any(|key| normalized == *key)
        || normalized.ends_with("input")
        || normalized.ends_with("output")
        || normalized.ends_with("command")
        || normalized.contains("url")
        || normalized.contains("uri")
        || normalized.contains("endpoint")
    {
        return Some(REDACTED_CONTENT);
    }
    None
}

fn redaction_for_string(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.starts_with("/Users/")
        || trimmed.starts_with("/home/")
        || trimmed.starts_with("/private/")
        || trimmed.starts_with("/tmp/")
        || lower.starts_with("file://")
        || lower.contains(" /users/")
        || lower.contains(" /home/")
        || lower.contains(" /private/")
    {
        return Some(REDACTED_LOCAL_PATH);
    }
    if lower.contains("authorization:")
        || lower.contains("bearer ")
        || lower.contains("api_key=")
        || lower.contains("apikey=")
        || lower.contains("token=")
        || lower.contains("password=")
        || trimmed.starts_with("sk-")
    {
        return Some(REDACTED_SECRET_LIKE);
    }
    if trimmed.len() > 512 && trimmed.contains('\n') {
        return Some(REDACTED_CONTENT);
    }
    None
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

enum TrialArtifactDecision {
    Include(BundleFileKind),
    Skip(&'static str),
}

fn trial_artifact_decision(run_dir: &Path, path: &Path, name: &str) -> TrialArtifactDecision {
    let relative = match path.strip_prefix(run_dir) {
        Ok(relative) => relative,
        Err(_) => return TrialArtifactDecision::Skip("outside_run_dir"),
    };
    if let Some(reason) = unsafe_trial_path_reason(relative, name) {
        return TrialArtifactDecision::Skip(reason);
    }

    let Some(file_name) = path
        .file_name()
        .map(|file_name| file_name.to_string_lossy().to_ascii_lowercase())
    else {
        return TrialArtifactDecision::Skip("missing_file_name");
    };
    if file_name == "events.jsonl" || file_name.ends_with("_events.jsonl") {
        return TrialArtifactDecision::Include(BundleFileKind::Jsonl);
    }
    if file_name.ends_with(".json") && allowed_trial_json_name(&file_name) {
        return TrialArtifactDecision::Include(BundleFileKind::Json);
    }
    TrialArtifactDecision::Skip("unsupported_trial_artifact")
}

fn unsafe_trial_path_reason(relative: &Path, name: &str) -> Option<&'static str> {
    for component in relative.components() {
        let Component::Normal(raw) = component else {
            return Some("non_normal_path");
        };
        let part = raw.to_string_lossy().to_ascii_lowercase();
        if excluded_path_component(&part) {
            return Some("excluded_path_component");
        }
    }
    let file_name = Path::new(name)
        .file_name()
        .map(|file_name| file_name.to_string_lossy().to_ascii_lowercase())?;
    if secret_like_file_name(&file_name) {
        return Some("secret_like_file_name");
    }
    if raw_log_file_name(&file_name) {
        return Some("raw_log_file");
    }
    None
}

fn excluded_path_component(part: &str) -> bool {
    matches!(
        part,
        ".git"
            | ".env"
            | "auth"
            | "credential"
            | "credentials"
            | "debug_bundles"
            | "node_modules"
            | "runtime"
            | "secret"
            | "secrets"
            | "state"
            | "temp"
            | "tmp"
            | "token"
            | "tokens"
            | "workspace"
            | "workspaces"
            | "workdir"
    )
}

fn secret_like_file_name(file_name: &str) -> bool {
    file_name == ".env"
        || file_name.ends_with(".env")
        || file_name.ends_with(".key")
        || file_name.ends_with(".pem")
        || file_name == "id_rsa"
        || file_name == "id_ed25519"
        || file_name.contains("secret")
        || file_name.contains("token")
        || file_name.contains("credential")
        || file_name.contains("password")
        || file_name.contains("authorization")
}

fn raw_log_file_name(file_name: &str) -> bool {
    file_name.ends_with(".log") || file_name.ends_with(".stdout") || file_name.ends_with(".stderr")
}

fn allowed_trial_json_name(file_name: &str) -> bool {
    matches!(
        file_name,
        "benchmark_preflight.json"
            | "case_manifest.json"
            | "claim_intent.json"
            | "contract_trace.json"
            | "harness_manifest.json"
            | "mapped_grader_output.json"
            | "mapped_output.json"
            | "result.json"
            | "summary.json"
            | "trace_manifest.json"
            | "trial_metadata.json"
            | "trial_preflight.json"
            | "trial_state.json"
    )
}

fn bundle_name(run_dir: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(run_dir).with_context(|| {
        format!(
            "bundle entry escaped run directory\nrun_ref: {}\nentry_ref: {}",
            public_run_ref(run_dir),
            REDACTED_LOCAL_PATH
        )
    })?;
    Ok(path_to_zip_name(relative))
}

fn public_run_ref(run_dir: &Path) -> String {
    let run_id = run_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("current");
    format!("run://{}", public_run_ref_component(run_id))
}

fn public_run_ref_component(run_id: &str) -> String {
    let trimmed = run_id.trim();
    if trimmed.is_empty() {
        return "current".to_string();
    }
    let normalized = trimmed.to_ascii_lowercase();
    let secret_like = [
        "secret",
        "token",
        "password",
        "credential",
        "api_key",
        "apikey",
        "private",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if secret_like || !is_plain_public_ref_component(trimmed) {
        "redacted".to_string()
    } else {
        public_ref_component(trimmed)
    }
}

fn public_run_path_ref(run_dir: &Path, path: &Path) -> String {
    let Ok(relative) = path.strip_prefix(run_dir) else {
        return REDACTED_LOCAL_PATH.to_string();
    };
    if relative.as_os_str().is_empty() {
        return public_run_ref(run_dir);
    }
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return REDACTED_LOCAL_PATH.to_string();
        };
        let Some(part) = part.to_str() else {
            return REDACTED_LOCAL_PATH.to_string();
        };
        if part.trim().is_empty() || !is_plain_public_ref_component(part) {
            return REDACTED_LOCAL_PATH.to_string();
        }
        parts.push(public_ref_component(part));
    }
    if parts.is_empty() {
        public_run_ref(run_dir)
    } else {
        format!("run://{}", parts.join("/"))
    }
}

fn is_plain_public_ref_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn public_ref_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn public_debug_bundle_output_ref(run_dir: &Path, out_path: &Path) -> String {
    public_run_path_ref(run_dir, out_path)
}

fn public_io_error(err: &std::io::Error) -> String {
    match err.raw_os_error() {
        Some(code) => format!("{:?} (os error {code})", err.kind()),
        None => format!("{:?}", err.kind()),
    }
}

fn public_walkdir_error_path(run_dir: &Path, err: &walkdir::Error) -> String {
    let fallback = run_dir.join("trials");
    let path = err.path().unwrap_or(fallback.as_path());
    match path.strip_prefix(run_dir) {
        Ok(relative) if !relative.as_os_str().is_empty() => path_to_zip_name(relative),
        _ => public_run_path_ref(run_dir, path),
    }
}

fn public_walkdir_error_reason(err: &walkdir::Error) -> String {
    err.io_error()
        .map(|err| format!("walk_failed: {}", public_io_error(err)))
        .unwrap_or_else(|| "walk_failed".to_string())
}

fn path_to_zip_name(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn lexical_absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(lexical_normalize(&path))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn debug_bundle_manifest_json(report: &DebugBundleReport) -> Value {
    let mut skipped_reasons = BTreeMap::<String, usize>::new();
    for skipped in &report.skipped {
        *skipped_reasons.entry(skipped.reason.clone()).or_insert(0) += 1;
    }
    json!({
        "schema_version": "debug_bundle_manifest_v1",
        "policy": {
            "mode": "redacted_support_bundle",
            "includes": "top-level run metadata plus curated trial JSON/JSONL diagnostics",
            "excludes": [
                "raw logs",
                "runtime, state, workspace, temp, auth, and secret-looking paths",
                "unsupported trial artifact types"
            ],
            "redacts": [
                "secret-looking keys",
                "environment-like keys",
                "local path fields and local path strings",
                "command, argument, and URL-like fields",
                "prompt/content/input/output/message-like fields"
            ]
        },
        "included_count": report.included_count(),
        "skipped_count": report.skipped_count(),
        "redacted_count": report.redacted_count(),
        "skipped_reasons": skipped_reasons,
        "included": report.included.iter().map(|entry| {
            json!({
                "path": &entry.path,
                "kind": &entry.kind,
                "redacted": entry.redacted
            })
        }).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        fn new(label: &str) -> Result<Self> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lab_provenance_{label}_{}_{}",
                std::process::id(),
                nanos
            ));
            fs::create_dir_all(&path)?;
            Ok(Self { path })
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_json(path: &Path, value: Value) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(&value)?)?;
        Ok(())
    }

    fn read_zip_strings(path: &Path) -> Result<BTreeMap<String, String>> {
        let file = fs::File::open(path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        let mut entries = BTreeMap::new();
        for idx in 0..archive.len() {
            let mut file = archive.by_index(idx)?;
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            entries.insert(file.name().to_string(), content);
        }
        Ok(entries)
    }

    fn write_minimal_run(run_dir: &Path) -> Result<()> {
        fs::create_dir_all(run_dir)?;
        write_json(
            &run_dir.join("manifest.json"),
            json!({"schema_version": "run"}),
        )?;
        write_json(
            &run_dir.join("trials").join("trial_1").join("summary.json"),
            json!({"status": "completed"}),
        )?;
        Ok(())
    }

    #[test]
    fn debug_bundle_redacts_json_and_skips_unsafe_trial_files() -> Result<()> {
        let guard = TempDirGuard::new("redaction")?;
        let run_dir = guard.path.join("run");
        fs::create_dir_all(&run_dir)?;
        write_json(
            &run_dir.join("manifest.json"),
            json!({
                "schema_version": "run_manifest_v1",
                "cloud_user_token": "raw-token",
                "package_path": "/Users/alice/project/package",
                "command": ["agent", "--token", "inline-command-secret"],
                "endpoint_url": "https://private.example.invalid",
                "status": "completed"
            }),
        )?;
        write_json(
            &run_dir.join("attestation.json"),
            json!({
                "schema_version": "attestation_v1",
                "events_hashchain_heads": [{"trial_id": "trial_1", "head": "sha256:abc"}]
            }),
        )?;
        fs::write(run_dir.join("resolved_experiment.digest"), "sha256:abc\n")?;

        let trial_dir = run_dir.join("trials").join("trial_1");
        write_json(
            &trial_dir.join("summary.json"),
            json!({
                "status": "failed",
                "stdout": "secret log line",
                "details": {
                    "password": "super-password",
                    "workspace": "/Users/alice/work"
                }
            }),
        )?;
        write_json(
            &trial_dir.join("result.json"),
            json!({
                "success": false,
                "answer": "private answer",
                "score": 0.42
            }),
        )?;
        write_json(
            &trial_dir.join("trial_state.json"),
            json!({
                "schema_version": "trial_state_v1",
                "status": "failed"
            }),
        )?;
        fs::write(
            trial_dir.join("events.jsonl"),
            "{\"event\":\"tool\",\"authorization\":\"Bearer raw-event-token\",\"path\":\"/home/alice/event\",\"raw_line\":\"not-json token=raw-raw-line-secret\"}\n",
        )?;
        fs::write(
            trial_dir.join("harness_stdout.log"),
            "OPENAI_API_KEY=raw-log-secret\n",
        )?;
        write_json(
            &trial_dir.join("state").join("lab_control.json"),
            json!({"active_trials": {"trial_1": {"worker_id": "worker_a"}}}),
        )?;
        fs::create_dir_all(trial_dir.join("secrets"))?;
        fs::write(
            trial_dir.join("secrets").join("token.txt"),
            "raw-secret-file-token",
        )?;
        fs::write(trial_dir.join("id_rsa"), "private-key")?;

        let out = guard.path.join("bundle.zip");
        let report = build_debug_bundle(&run_dir, &out)?;

        assert_eq!(report.bundle_path, out);
        assert!(report
            .included
            .iter()
            .any(|entry| entry.path == "trials/trial_1/summary.json" && entry.redacted));
        assert!(report
            .included
            .iter()
            .any(|entry| entry.path == "trials/trial_1/events.jsonl" && entry.redacted));
        assert!(report
            .skipped
            .iter()
            .any(|entry| entry.path == "trials/trial_1/harness_stdout.log"
                && entry.reason == "raw_log_file"));
        assert!(report.skipped.iter().any(|entry| entry.path
            == "trials/trial_1/state/lab_control.json"
            && entry.reason == "excluded_path_component"));
        assert!(report
            .skipped
            .iter()
            .any(|entry| entry.path == "trials/trial_1/secrets/token.txt"
                && entry.reason == "excluded_path_component"));
        assert!(report
            .skipped
            .iter()
            .any(|entry| entry.path == "trials/trial_1/id_rsa"
                && entry.reason == "secret_like_file_name"));

        let entries = read_zip_strings(&out)?;
        assert!(entries.contains_key("manifest.json"));
        assert!(entries.contains_key("attestation.json"));
        assert!(entries.contains_key("resolved_experiment.digest"));
        assert!(entries.contains_key("trials/trial_1/summary.json"));
        assert!(entries.contains_key("trials/trial_1/result.json"));
        assert!(entries.contains_key("trials/trial_1/events.jsonl"));
        assert!(entries.contains_key("trials/trial_1/trial_state.json"));
        assert!(entries.contains_key(DEBUG_BUNDLE_MANIFEST));
        assert!(!entries.contains_key("trials/trial_1/harness_stdout.log"));
        assert!(!entries.contains_key("trials/trial_1/state/lab_control.json"));
        assert!(!entries.contains_key("trials/trial_1/secrets/token.txt"));
        assert!(!entries.contains_key("trials/trial_1/id_rsa"));

        let combined = entries.values().cloned().collect::<Vec<_>>().join("\n");
        for forbidden in [
            "raw-token",
            "inline-command-secret",
            "private.example.invalid",
            "super-password",
            "/Users/alice",
            "/home/alice",
            "private answer",
            "secret log line",
            "raw-event-token",
            "raw-raw-line-secret",
            "OPENAI_API_KEY",
            "raw-secret-file-token",
            "private-key",
        ] {
            assert!(
                !combined.contains(forbidden),
                "bundle leaked forbidden text: {forbidden}"
            );
        }
        assert!(combined.contains(REDACTED_SECRET));
        assert!(combined.contains(REDACTED_LOCAL_PATH));
        assert!(combined.contains(REDACTED_CONTENT));

        let manifest: Value =
            serde_json::from_str(entries.get(DEBUG_BUNDLE_MANIFEST).expect("manifest entry"))?;
        assert_eq!(
            manifest["schema_version"],
            json!("debug_bundle_manifest_v1")
        );
        assert_eq!(manifest["skipped_reasons"]["raw_log_file"], json!(1));
        assert_eq!(
            manifest["skipped_reasons"]["excluded_path_component"],
            json!(2)
        );
        assert_eq!(manifest["policy"]["mode"], json!("redacted_support_bundle"));
        Ok(())
    }

    #[test]
    fn debug_bundle_rejects_output_inside_trials_tree() -> Result<()> {
        let guard = TempDirGuard::new("out_inside_trials")?;
        let run_dir = guard.path.join("run");
        write_minimal_run(&run_dir)?;
        let out = run_dir
            .join("trials")
            .join("trial_1")
            .join("support-bundle.zip");

        let err = build_debug_bundle(&run_dir, &out).expect_err("should reject trials output");

        assert!(err.to_string().contains("inside the run trials directory"));
        assert!(!err.to_string().contains(&guard.path.display().to_string()));
        assert!(!out.exists());
        Ok(())
    }

    #[test]
    fn debug_bundle_missing_run_dir_uses_public_ref() -> Result<()> {
        let guard = TempDirGuard::new("missing_run_dir")?;
        let run_dir = guard.path.join("private-run-dir");
        let out = guard.path.join("bundle.zip");

        let err = build_debug_bundle(&run_dir, &out).expect_err("missing run dir should fail");
        let message = err.to_string();

        assert!(message.contains("run directory does not exist"));
        assert!(message.contains("run_ref: run://redacted"));
        assert!(
            !message.contains(&guard.path.display().to_string()),
            "missing run dir error leaked fixture root: {message}"
        );
        assert!(
            !message.contains("private-run-dir") && !message.contains("bundle.zip"),
            "missing run dir error leaked private path text: {message}"
        );
        Ok(())
    }

    #[test]
    fn debug_bundle_output_parent_error_uses_public_output_ref() -> Result<()> {
        let guard = TempDirGuard::new("blocked_output_parent")?;
        let run_dir = guard.path.join("run");
        write_minimal_run(&run_dir)?;
        let blocked = guard.path.join("private-output-parent");
        fs::write(&blocked, "not a directory")?;
        let out = blocked.join("support-bundle.zip");

        let err =
            build_debug_bundle(&run_dir, &out).expect_err("blocked output parent should fail");
        let message = err.to_string();

        assert!(message.contains("failed to create debug bundle output directory"));
        assert!(message.contains("output_ref: [REDACTED:local-path]"));
        assert!(
            !message.contains(&guard.path.display().to_string()),
            "output parent error leaked fixture root: {message}"
        );
        assert!(
            !message.contains("private-output-parent") && !message.contains("support-bundle.zip"),
            "output parent error leaked private output path: {message}"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn debug_bundle_skips_symlinked_top_level_metadata_without_reading_target() -> Result<()> {
        use std::os::unix::fs::symlink;

        let guard = TempDirGuard::new("top_level_symlink")?;
        let run_dir = guard.path.join("run");
        fs::create_dir_all(&run_dir)?;
        let outside = guard.path.join("outside-secret.json");
        fs::write(
            &outside,
            r#"{"cloud_user_token":"raw-outside-token","path":"/Users/alice/private"}"#,
        )?;
        symlink(&outside, run_dir.join("manifest.json"))?;
        write_json(
            &run_dir.join("trials").join("trial_1").join("summary.json"),
            json!({"status": "completed"}),
        )?;
        let out = guard.path.join("bundle.zip");

        let report = build_debug_bundle(&run_dir, &out)?;
        let entries = read_zip_strings(&out)?;
        let combined = entries.values().cloned().collect::<Vec<_>>().join("\n");

        assert!(report
            .skipped
            .iter()
            .any(|entry| entry.path == "manifest.json" && entry.reason == "non_regular_file"));
        assert!(!entries.contains_key("manifest.json"));
        assert!(!combined.contains("raw-outside-token"));
        assert!(!combined.contains("/Users/alice"));
        Ok(())
    }

    #[test]
    fn debug_bundle_zip_entries_use_normalized_metadata() -> Result<()> {
        let guard = TempDirGuard::new("zip_metadata")?;
        let run_dir = guard.path.join("run");
        write_minimal_run(&run_dir)?;
        let out = guard.path.join("bundle.zip");

        build_debug_bundle(&run_dir, &out)?;

        let file = fs::File::open(&out)?;
        let mut archive = zip::ZipArchive::new(file)?;
        assert!(archive.len() > 0);
        for idx in 0..archive.len() {
            let file = archive.by_index(idx)?;
            let modified = file.last_modified();
            assert_eq!(modified.year(), 1980, "{}", file.name());
            assert_eq!(modified.month(), 1, "{}", file.name());
            assert_eq!(modified.day(), 1, "{}", file.name());
            assert_eq!(modified.hour(), 0, "{}", file.name());
            assert_eq!(modified.minute(), 0, "{}", file.name());
            assert_eq!(modified.second(), 0, "{}", file.name());
            assert_eq!(file.unix_mode().map(|mode| mode & 0o777), Some(0o644));
        }
        Ok(())
    }

    #[test]
    fn debug_bundle_refuses_to_overwrite_existing_output() -> Result<()> {
        let guard = TempDirGuard::new("overwrite")?;
        let run_dir = guard.path.join("run");
        fs::create_dir_all(&run_dir)?;
        write_json(
            &run_dir.join("manifest.json"),
            json!({"schema_version": "run"}),
        )?;
        let out = run_dir.join("debug_bundles").join("bundle.zip");
        fs::create_dir_all(out.parent().expect("bundle parent"))?;
        fs::write(&out, "existing bundle")?;

        let err = build_debug_bundle(&run_dir, &out).expect_err("should refuse overwrite");
        let message = err.to_string();

        assert!(message.contains("debug bundle already exists"));
        assert!(message.contains("output_ref: run://debug_bundles/bundle.zip"));
        assert!(
            !message.contains(&guard.path.display().to_string()),
            "overwrite error leaked fixture root: {message}"
        );
        assert_eq!(fs::read_to_string(&out)?, "existing bundle");
        Ok(())
    }
}
