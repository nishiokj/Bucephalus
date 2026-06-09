use anyhow::{anyhow, Result};
use clap::{Parser, ValueEnum};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(
    name = "bucephalus-worker-runner",
    version = "0.3.0",
    about = "Run a sealed Bucephalus package for Cloud workers"
)]
struct Args {
    package: PathBuf,
    #[arg(long, value_enum)]
    executor: Option<ExecutorArg>,
    #[arg(long, value_enum)]
    materialize: Option<MaterializeArg>,
    #[arg(long, hide = true)]
    run_root: Option<PathBuf>,
    #[arg(long = "env", value_name = "KEY=VALUE")]
    runtime_env: Vec<String>,
    #[arg(long = "env-file", value_name = "PATH")]
    runtime_env_file: Vec<PathBuf>,
    #[arg(long = "secret-file", value_name = "ID=PATH")]
    secret_file: Vec<String>,
    #[arg(long)]
    smoke_test: bool,
    #[arg(long)]
    run_dangerously: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExecutorArg {
    #[value(name = "local_docker")]
    LocalDocker,
    #[value(name = "modal")]
    Modal,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum MaterializeArg {
    #[value(name = "none")]
    None,
    #[value(name = "metadata_only")]
    MetadataOnly,
    #[value(name = "outputs_only")]
    OutputsOnly,
    #[value(name = "full")]
    Full,
}

impl From<ExecutorArg> for lab_runner::ExecutorKind {
    fn from(value: ExecutorArg) -> Self {
        match value {
            ExecutorArg::LocalDocker => lab_runner::ExecutorKind::LocalDocker,
            ExecutorArg::Modal => lab_runner::ExecutorKind::Modal,
        }
    }
}

impl From<MaterializeArg> for lab_runner::MaterializationMode {
    fn from(value: MaterializeArg) -> Self {
        match value {
            MaterializeArg::None => lab_runner::MaterializationMode::None,
            MaterializeArg::MetadataOnly => lab_runner::MaterializationMode::MetadataOnly,
            MaterializeArg::OutputsOnly => lab_runner::MaterializationMode::OutputsOnly,
            MaterializeArg::Full => lab_runner::MaterializationMode::Full,
        }
    }
}

fn main() -> Result<()> {
    lab_runner::telemetry::init();
    std::env::set_var(
        lab_runner::PROCESS_INVOKED_AT_MS_ENV,
        current_unix_time_ms().to_string(),
    );
    let args = parse_args();
    let json_mode = args.json;
    match run(args) {
        Ok(payload) => {
            if json_mode {
                emit_json(&payload);
            }
            Ok(())
        }
        Err(err) => {
            if json_mode {
                emit_json(&json!({
                    "ok": false,
                    "command": "run",
                    "error": {
                        "code": "command_failed",
                        "message": public_error_message(&err.to_string()),
                    },
                }));
                std::process::exit(1);
            }
            Err(err)
        }
    }
}

fn parse_args() -> Args {
    let mut raw: Vec<_> = std::env::args_os().collect();
    if raw.get(1).and_then(|value| value.to_str()) == Some("run") {
        raw.remove(1);
    }
    Args::parse_from(raw)
}

fn run(args: Args) -> Result<Value> {
    if args.smoke_test && args.run_dangerously {
        return Err(anyhow!(
            "--smoke-test and --run-dangerously are mutually exclusive"
        ));
    }
    if !args.smoke_test && !args.run_dangerously {
        return Err(anyhow!(
            "worker runner requires either --smoke-test or --run-dangerously"
        ));
    }
    if experiment_input_path(&args.package)?.is_some() {
        return Err(anyhow!(
            "worker runner accepts sealed package directories only, not experiment YAML"
        ));
    }

    let execution = lab_runner::RunExecutionOptions {
        executor: args.executor.map(Into::into),
        materialize: args.materialize.map(Into::into),
        run_root: args.run_root,
        runtime_env: parse_runtime_env_bindings(&args.runtime_env)?,
        runtime_env_files: args.runtime_env_file,
        secret_files: parse_secret_file_bindings(&args.secret_file)?,
        stdout_progress: false,
    };
    let package_dir = package_directory_for_input(&args.package);
    let mut validation = lab_runner::register_experiment_bundle(&package_dir)?;
    let summary = lab_runner::experiment_summary_with_options(&package_dir, &execution)?;

    if args.smoke_test {
        let result = lab_runner::run_smoke_test_with_options(&package_dir, execution.clone())?;
        validation = lab_runner::mark_experiment_bundle_smoke_tested(&package_dir, &result.run_id)?;
        return Ok(json!({
            "ok": true,
            "command": "run",
            "mode": "smoke_test",
            "input": "package",
            "package": package_to_json(&validation),
            "summary": summary_to_json(&summary, &package_dir),
            "run": run_result_to_json(&result),
            "executor": execution.executor.map(|e| e.as_str()),
            "materialize": execution.materialize.map(|m| m.as_str()),
            "validation": experiment_bundle_validation_to_json(&validation),
        }));
    }

    let result = lab_runner::run_experiment_with_options(&package_dir, execution.clone())?;
    Ok(json!({
        "ok": true,
        "command": "run",
        "input": "package",
        "package": package_to_json(&validation),
        "summary": summary_to_json(&summary, &package_dir),
        "run": run_result_to_json(&result),
        "artifacts": run_artifacts_to_json(&result),
        "executor": execution.executor.map(|e| e.as_str()),
        "materialize": execution.materialize.map(|m| m.as_str()),
        "validation": experiment_bundle_validation_to_json(&validation),
    }))
}

fn parse_runtime_env_bindings(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for raw in values {
        let (key_raw, value_raw) = raw
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --env: expected KEY=VALUE"))?;
        let key = key_raw.trim();
        if key.is_empty() {
            return Err(anyhow!("invalid --env: key cannot be empty"));
        }
        out.insert(key.to_string(), value_raw.to_string());
    }
    Ok(out)
}

fn parse_secret_file_bindings(values: &[String]) -> Result<BTreeMap<String, PathBuf>> {
    let mut out = BTreeMap::new();
    for raw in values {
        let (key_raw, value_raw) = raw
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --secret-file: expected ID=PATH"))?;
        let key = key_raw.trim();
        if key.is_empty() {
            return Err(anyhow!("invalid --secret-file: id cannot be empty"));
        }
        let value = value_raw.trim();
        if value.is_empty() {
            return Err(anyhow!("invalid --secret-file: path cannot be empty"));
        }
        out.insert(key.to_string(), PathBuf::from(value));
    }
    Ok(out)
}

fn package_directory_for_input(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    }
}

fn experiment_input_path(path: &Path) -> Result<Option<PathBuf>> {
    if path.is_dir() {
        return Ok(None);
    }
    let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
        return Ok(None);
    };
    if matches!(ext, "yaml" | "yml") {
        return Ok(Some(path.to_path_buf()));
    }
    Ok(None)
}

fn summary_to_json(summary: &lab_runner::ExperimentSummary, package_dir: &Path) -> Value {
    json!({
        "experiment": summary.exp_id,
        "workload_type": summary.workload_type,
        "dataset_ref": package_path_ref(package_dir, &summary.dataset_path),
        "tasks": summary.task_count,
        "replications": summary.replications,
        "variant_count": summary.variant_count,
        "total_trials": summary.total_trials,
        "agent_runtime": summary.agent_runtime_command,
        "image": summary.image,
        "network": summary.network_mode,
        "trajectory_ref": summary.trajectory_path.as_deref().map(runtime_path_ref),
        "causal_extraction": summary.causal_extraction,
        "scheduling": summary.scheduling,
        "state_policy": summary.state_policy,
    })
}

fn run_result_to_json(result: &lab_runner::RunResult) -> Value {
    json!({
        "run_id": result.run_id,
        "run_ref": run_ref(&result.run_id),
        "run_name": result
            .run_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(result.run_id.as_str()),
    })
}

fn run_artifacts_to_json(result: &lab_runner::RunResult) -> Value {
    json!({
        "run_ref": run_ref(&result.run_id),
        "results_ref": format!("{}/results", run_ref(&result.run_id)),
    })
}

fn package_to_json(validation: &lab_runner::ExperimentBundleValidation) -> Value {
    json!({
        "package_digest": validation.package_digest,
        "experiment_id": validation.experiment_id,
    })
}

fn experiment_bundle_validation_to_json(
    validation: &lab_runner::ExperimentBundleValidation,
) -> Value {
    json!({
        "package_digest": validation.package_digest,
        "experiment_id": validation.experiment_id,
        "smoke_tested": validation.smoke_tested,
        "smoke_run_id": validation.smoke_run_id,
        "smoke_tested_at_ms": validation.smoke_tested_at_ms,
    })
}

fn package_path_ref(package_dir: &Path, path: &Path) -> String {
    match path.strip_prefix(package_dir) {
        Ok(relative) => format!("package://{}", path_ref_components(relative)),
        Err(_) => "[redacted:local-path]".to_string(),
    }
}

fn runtime_path_ref(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty()
        || looks_like_local_path(trimmed)
        || trimmed.to_ascii_lowercase().starts_with("file://")
    {
        "[redacted:local-path]".to_string()
    } else {
        format!("runtime://{}", trimmed.trim_start_matches('/'))
    }
}

fn run_ref(run_id: &str) -> String {
    format!("run://{run_id}")
}

fn path_ref_components(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn emit_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
    );
}

fn public_error_message(message: &str) -> String {
    message
        .lines()
        .map(redact_public_error_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_public_error_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if lower.contains("authorization:") || lower.contains("bearer ") {
        return "[REDACTED:secret-like]".to_string();
    }
    let url_redacted = redact_urls_in_text(line);
    let path_redacted = redact_local_paths_in_text(&url_redacted);
    path_redacted
        .split_inclusive(char::is_whitespace)
        .map(|chunk| {
            if chunk.chars().last().is_some_and(char::is_whitespace) {
                let trailing = chunk.chars().last().unwrap();
                let token = &chunk[..chunk.len() - trailing.len_utf8()];
                format!("{}{}", redact_public_error_token(token), trailing)
            } else {
                redact_public_error_token(chunk)
            }
        })
        .collect::<String>()
}

fn redact_public_error_token(token: &str) -> String {
    let trimmed_start = token.trim_start_matches(public_error_token_prefix);
    let prefix_len = token.len() - trimmed_start.len();
    let trimmed_core = trimmed_start.trim_end_matches(public_error_token_suffix);
    let suffix_len = trimmed_start.len() - trimmed_core.len();
    let prefix = &token[..prefix_len];
    let suffix = &token[token.len() - suffix_len..];
    let lower = trimmed_core.to_ascii_lowercase();

    if trimmed_core.contains("[REDACTED:") {
        return token.to_string();
    }

    let redacted_core = if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("file://")
    {
        Some(redact_public_url(trimmed_core))
    } else if looks_like_local_path(trimmed_core) {
        Some("[REDACTED:local-path]".to_string())
    } else if let Some((key, value)) = trimmed_core.split_once('=') {
        let key_lower = key.to_ascii_lowercase();
        if key_lower.contains("token")
            || key_lower.contains("secret")
            || key_lower.contains("password")
            || key_lower.contains("apikey")
            || key_lower.contains("api_key")
            || key_lower.contains("credential")
        {
            Some(format!("{key}=[REDACTED:secret-like]"))
        } else if looks_like_local_path(value) {
            Some(format!("{key}=[REDACTED:local-path]"))
        } else if reqwest::Url::parse(value).is_ok() {
            Some(format!("{key}={}", redact_public_url(value)))
        } else {
            None
        }
    } else if lower.starts_with("sk-") {
        Some("[REDACTED:secret-like]".to_string())
    } else {
        None
    };

    match redacted_core {
        Some(core) => format!("{prefix}{core}{suffix}"),
        None => token.to_string(),
    }
}

fn public_error_token_prefix(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '<' | '"' | '\'' | '`')
}

fn public_error_token_suffix(ch: char) -> bool {
    matches!(
        ch,
        '.' | ',' | ';' | ')' | ']' | '}' | '>' | '"' | '\'' | '`'
    )
}

fn looks_like_local_path(value: &str) -> bool {
    earliest_local_path_start(value.trim()) == Some(0)
}

fn redact_urls_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = earliest_url_start(rest) {
        out.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let end = urlish_end(after_start);
        let candidate = &after_start[..end];
        if candidate.starts_with("file://[REDACTED:local-path]") {
            out.push_str(candidate);
            rest = &after_start[end..];
            continue;
        }
        let trimmed = candidate.trim_end_matches(public_error_token_suffix);
        let suffix = &candidate[trimmed.len()..];
        out.push_str(&redact_public_url(trimmed));
        out.push_str(suffix);
        rest = &after_start[end..];
    }
    out.push_str(rest);
    out
}

fn earliest_url_start(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    ["https://", "http://", "file://"]
        .iter()
        .filter_map(|scheme| lower.find(scheme))
        .min()
}

fn urlish_end(text: &str) -> usize {
    for (idx, ch) in text.char_indices() {
        if ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | '<' | '>') {
            return idx;
        }
    }
    text.len()
}

fn redact_local_paths_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = earliest_local_path_start(rest) {
        out.push_str(&rest[..start]);
        out.push_str("[REDACTED:local-path]");
        let after_start = &rest[start..];
        let end = local_path_end(after_start);
        rest = &after_start[end..];
    }
    out.push_str(rest);
    out
}

fn earliest_local_path_start(text: &str) -> Option<usize> {
    let mut best = None;
    for (idx, _) in text.char_indices() {
        if !local_path_start_boundary(text, idx) {
            continue;
        }
        if is_posix_local_path_start(text, idx)
            || is_wsl_windows_user_path_start(text, idx)
            || is_windows_drive_path_start(text, idx)
            || is_windows_profile_env_path_start(text, idx)
            || is_home_relative_path_start(text, idx)
        {
            best = Some(best.map_or(idx, |current: usize| current.min(idx)));
        }
    }
    best
}

fn local_path_start_boundary(text: &str, idx: usize) -> bool {
    idx == 0
        || text[..idx].chars().next_back().is_some_and(|ch| {
            ch.is_whitespace() || matches!(ch, '=' | '(' | '[' | '{' | '<' | '"' | '\'' | '`')
        })
}

fn is_posix_local_path_start(text: &str, idx: usize) -> bool {
    [
        "/Users/",
        "/home/",
        "/private/",
        "/tmp/",
        "/var/folders/",
        "/Volumes/",
    ]
    .iter()
    .any(|prefix| text[idx..].starts_with(prefix))
}

fn is_wsl_windows_user_path_start(text: &str, idx: usize) -> bool {
    let rest = &text[idx..];
    let bytes = rest.as_bytes();
    bytes.len() > 13
        && rest.starts_with("/mnt/")
        && bytes[5].is_ascii_alphabetic()
        && bytes[6] == b'/'
        && rest[7..].to_ascii_lowercase().starts_with("users/")
}

fn is_windows_drive_path_start(text: &str, idx: usize) -> bool {
    let bytes = text.as_bytes();
    idx + 3 < bytes.len()
        && bytes[idx].is_ascii_alphabetic()
        && bytes[idx + 1] == b':'
        && matches!(bytes[idx + 2], b'\\' | b'/')
        && !matches!(bytes[idx + 3], b'\\' | b'/')
}

fn is_windows_profile_env_path_start(text: &str, idx: usize) -> bool {
    let rest = &text[idx..];
    if !rest.starts_with('%') {
        return false;
    }
    let Some(end_percent) = rest[1..].find('%').map(|offset| offset + 1) else {
        return false;
    };
    let Some(after_percent) = rest.as_bytes().get(end_percent + 1) else {
        return false;
    };
    if !matches!(after_percent, b'\\' | b'/') {
        return false;
    }
    let env_name = rest[1..end_percent].to_ascii_uppercase();
    [
        "USERPROFILE",
        "HOME",
        "TEMP",
        "TMP",
        "APPDATA",
        "LOCALAPPDATA",
    ]
    .iter()
    .any(|name| env_name.contains(name))
}

fn is_home_relative_path_start(text: &str, idx: usize) -> bool {
    text[idx..].starts_with("~/") || text[idx..].starts_with("~\\")
}

fn local_path_end(text: &str) -> usize {
    let mut idx = 0;
    while idx < text.len() {
        let ch = text[idx..].chars().next().unwrap();
        if is_local_path_hard_delimiter(ch, idx) {
            return idx;
        }
        let next_idx = idx + ch.len_utf8();
        if ch.is_whitespace() && following_token_is_connector_boundary(&text[next_idx..]) {
            return idx;
        }
        if ch.is_whitespace() && following_token_is_assignment(&text[next_idx..]) {
            return idx;
        }
        idx = next_idx;
    }
    text.len()
}

fn is_local_path_hard_delimiter(ch: char, idx: usize) -> bool {
    matches!(
        ch,
        '\n' | '\r' | '\t' | '"' | '\'' | '`' | '<' | '>' | '|' | ',' | ';' | ')' | ']' | '}'
    ) || (ch == ':' && idx != 1)
}

fn following_token_is_connector_boundary(text: &str) -> bool {
    let trimmed = text.trim_start();
    for connector in ["with", "via", "using"] {
        let Some(after_connector) = trimmed.strip_prefix(connector) else {
            continue;
        };
        if !after_connector
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            continue;
        }
        let after_connector = after_connector.trim_start();
        if following_token_is_assignment(after_connector)
            || earliest_url_start(after_connector) == Some(0)
        {
            return true;
        }
    }
    false
}

fn following_token_is_assignment(text: &str) -> bool {
    let trimmed = text.trim_start();
    let key_len = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        .map(char::len_utf8)
        .sum::<usize>();
    key_len > 0 && trimmed.as_bytes().get(key_len) == Some(&b'=')
}

fn redact_public_url(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return "[REDACTED:url]".to_string();
    };
    if parsed.scheme() == "file" {
        return "file://[REDACTED:local-path]".to_string();
    }
    let redacted = !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some();
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    let mut public = parsed.to_string();
    if redacted {
        public.push_str(" [redacted URL credentials/query]");
    }
    public
}

fn current_unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| {
            i64::try_from(duration.as_millis()).expect("Unix timestamp milliseconds must fit i64")
        })
        .expect("system time must be after Unix epoch")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_env_parse_errors_do_not_echo_secret_values() {
        let raw_missing_equals = "OPENAI_API_KEY sk-worker-token".to_string();
        let err = parse_runtime_env_bindings(std::slice::from_ref(&raw_missing_equals))
            .expect_err("missing '=' must fail");
        let msg = err.to_string();
        assert!(msg.contains("expected KEY=VALUE"));
        assert!(!msg.contains("sk-worker-token"), "unexpected error: {msg}");
        assert!(
            !msg.contains(&raw_missing_equals),
            "unexpected error: {msg}"
        );

        let raw_empty_key = "=sk-worker-empty-key".to_string();
        let err = parse_runtime_env_bindings(std::slice::from_ref(&raw_empty_key))
            .expect_err("empty key must fail");
        let msg = err.to_string();
        assert!(msg.contains("key cannot be empty"));
        assert!(
            !msg.contains("sk-worker-empty-key"),
            "unexpected error: {msg}"
        );
        assert!(!msg.contains(&raw_empty_key), "unexpected error: {msg}");
    }

    #[test]
    fn secret_file_parse_errors_do_not_echo_raw_paths() {
        let raw_missing_equals = "/Users/alice/.config/bucephalus/codex-oauth.json".to_string();
        let err = parse_secret_file_bindings(std::slice::from_ref(&raw_missing_equals))
            .expect_err("missing '=' must fail");
        let msg = err.to_string();
        assert!(msg.contains("expected ID=PATH"));
        assert!(!msg.contains("/Users/alice"), "unexpected error: {msg}");
        assert!(
            !msg.contains(&raw_missing_equals),
            "unexpected error: {msg}"
        );

        let raw_empty_id = "=/Users/alice/.config/bucephalus/codex-oauth.json".to_string();
        let err = parse_secret_file_bindings(std::slice::from_ref(&raw_empty_id))
            .expect_err("empty id must fail");
        let msg = err.to_string();
        assert!(msg.contains("id cannot be empty"));
        assert!(!msg.contains("/Users/alice"), "unexpected error: {msg}");
        assert!(!msg.contains(&raw_empty_id), "unexpected error: {msg}");
    }

    #[test]
    fn worker_runner_json_error_message_is_public_boundary_safe() {
        let message = public_error_message(
            "failed package /Users/alice/work/package: permission denied\nsecret token=raw-worker-token\ncache file:///private/tmp/bucephalus/cache.json\nmirror https://mirror-user:mirror-secret@example.com/releases?token=raw-query#frag\nwindows C:\\Users\\Alice\\bench\\run.json\nwsl /mnt/c/Users/Alice/bench/run.json\nprofile %USERPROFILE%\\Documents\\bench\\run.json\nhome ~/private/bench/run.json via https://user:secret@example.com/upload?token=raw-upload",
        );

        assert!(message.contains("failed package"));
        assert!(message.contains("permission denied"));
        assert!(message.contains("token=[REDACTED:secret-like]"));
        assert!(message.contains("file://[REDACTED:local-path]"));
        assert!(message.contains("https://example.com/releases [redacted URL credentials/query]"));
        assert!(message.contains("via https://example.com/upload [redacted URL credentials/query]"));
        for forbidden in [
            "/Users/alice",
            "/private/tmp",
            "C:\\Users\\Alice",
            "/mnt/c/Users/Alice",
            "%USERPROFILE%",
            "~/private",
            "mirror-user",
            "mirror-secret",
            "raw-query",
            "raw-upload",
            "user:secret",
            "raw-worker-token",
            "work/package",
        ] {
            assert!(
                !message.contains(forbidden),
                "worker JSON error leaked forbidden text: {forbidden}\n{message}"
            );
        }
    }

    #[test]
    fn worker_runner_public_json_uses_refs_instead_of_host_paths() {
        let package_dir = PathBuf::from("/Users/alice/work/package");
        let summary = lab_runner::ExperimentSummary {
            exp_id: "exp_1".to_string(),
            workload_type: "evaluation".to_string(),
            dataset_path: package_dir.join("tasks").join("tasks.jsonl"),
            task_count: 3,
            replications: 1,
            variant_count: 2,
            total_trials: 6,
            agent_runtime_command: vec!["agent".to_string()],
            image: None,
            network_mode: "none".to_string(),
            trajectory_path: Some("/tmp/trajectory.jsonl".to_string()),
            causal_extraction: None,
            scheduling: "matrix".to_string(),
            state_policy: "ephemeral".to_string(),
            comparison: "pairwise".to_string(),
            retry_max_attempts: 1,
            preflight_warnings: Vec::new(),
        };
        let run = lab_runner::RunResult {
            run_id: "run_20260609_000001".to_string(),
            run_dir: PathBuf::from("/Users/alice/work/runs/run_20260609_000001"),
            account_db_path: PathBuf::from(
                "/Users/alice/work/runs/run_20260609_000001/account.sqlite",
            ),
        };
        let validation = lab_runner::ExperimentBundleValidation {
            package_digest: "sha256:abc".to_string(),
            experiment_id: Some("exp_1".to_string()),
            package_dir,
            smoke_tested: true,
            smoke_run_id: Some("run_smoke".to_string()),
            smoke_tested_at_ms: Some(42),
        };

        let payload = json!({
            "package": package_to_json(&validation),
            "summary": summary_to_json(&summary, &validation.package_dir),
            "run": run_result_to_json(&run),
            "artifacts": run_artifacts_to_json(&run),
            "validation": experiment_bundle_validation_to_json(&validation),
        });
        let combined = serde_json::to_string_pretty(&payload).unwrap();

        assert_eq!(
            payload["summary"]["dataset_ref"],
            "package://tasks/tasks.jsonl"
        );
        assert_eq!(
            payload["summary"]["trajectory_ref"],
            "[redacted:local-path]"
        );
        assert_eq!(
            runtime_path_ref("C:\\Users\\Alice\\bench\\trajectory.jsonl"),
            "[redacted:local-path]"
        );
        assert_eq!(
            runtime_path_ref("%USERPROFILE%\\bench\\trajectory.jsonl"),
            "[redacted:local-path]"
        );
        assert_eq!(payload["run"]["run_ref"], "run://run_20260609_000001");
        assert_eq!(payload["run"]["run_name"], "run_20260609_000001");
        assert_eq!(
            payload["artifacts"]["results_ref"],
            "run://run_20260609_000001/results"
        );
        for forbidden in [
            "/Users/alice",
            "account.sqlite",
            "package_dir",
            "account_db_path",
            "run_dir",
            "/tmp/trajectory.jsonl",
        ] {
            assert!(
                !combined.contains(forbidden),
                "worker runner public JSON leaked forbidden text: {forbidden}"
            );
        }
    }
}
