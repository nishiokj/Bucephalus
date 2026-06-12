use anyhow::{anyhow, bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Method;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "../cloud_auth_ux.rs"]
mod cloud_auth_ux;
use cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV;

const BUCEPHALUS_CLOUD_API_URL_ENV: &str = "BUCEPHALUS_CLOUD_API_URL";
const DEFAULT_MAX_AUTHORING_CONTEXT_ARCHIVE_ENTRIES: u64 = 10_000;
const DEFAULT_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
struct CliContext {
    api_url: String,
    user_token: Option<String>,
    args: Vec<String>,
    client: Client,
}

#[derive(Debug)]
struct PreparedPackageInput {
    archive_path: PathBuf,
    source_label: String,
    temp_root: Option<PathBuf>,
}

impl Drop for PreparedPackageInput {
    fn drop(&mut self) {
        if let Some(temp_root) = &self.temp_root {
            let _ = fs::remove_dir_all(temp_root);
        }
    }
}

#[derive(Debug)]
struct PreparedAuthoringContextInput {
    archive_path: PathBuf,
    source_label: String,
    entrypoint: String,
    temp_root: PathBuf,
}

impl Drop for PreparedAuthoringContextInput {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_root);
    }
}

#[derive(Debug, Default)]
struct AuthoringContextArchivePlan {
    files: Vec<String>,
    entries: u64,
    expanded_bytes: u64,
}

#[derive(Debug)]
struct UploadedArtifactResponse {
    response: Value,
    upload_id: String,
    expected_digest: String,
    expected_byte_size: u64,
}

#[derive(Clone, Debug)]
struct SecretRequirement {
    id: String,
    target: String,
    required_for_variants: Vec<String>,
}

fn main() {
    if let Err(err) = run(std::env::args().skip(1).collect()) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run(argv: Vec<String>) -> Result<()> {
    let context = parse_global_args(argv)?;
    let group = context.args.first().map(String::as_str);
    let command = context.args.get(1).map(String::as_str);
    let rest = context.args.iter().skip(2).cloned().collect::<Vec<_>>();

    match (group, command) {
        (None, _) | (Some("help" | "--help" | "-h"), _) => {
            print_help();
            Ok(())
        }
        _ if help_requested(&context.args) || group_command_without_leaf(group, command) => {
            if let Some(text) = command_help_text(group, command) {
                println!("{text}");
                Ok(())
            } else {
                bail!(
                    "unknown hosted command: {}\n\nRun `buc help` for the Cloud product commands.",
                    entered_command_name(group, command)
                )
            }
        }
        _ if !known_hosted_command(group, command) => bail!(
            "unknown hosted command: {}\n\nRun `buc help` for the Cloud product commands.",
            entered_command_name(group, command)
        ),
        (Some("health"), None) => health(with_args(&context, rest)),
        (Some("build"), _) => experiment_build(alias_args(&context, 1)),
        (Some("doctor"), _) => experiment_doctor(alias_args(&context, 1)),
        (Some("run"), _) => run_create(alias_args(&context, 1)),
        (Some("inspect"), _) => package_inspect(alias_args(&context, 1)),
        (Some("author"), Some("canonicalize")) | (Some("drafts"), Some("canonicalize")) => {
            draft_canonicalize(with_args(&context, rest))
        }
        (Some("author"), Some("resolve")) | (Some("drafts"), Some("resolve")) => {
            draft_resolve(with_args(&context, rest))
        }
        (Some("author"), Some("validate")) | (Some("drafts"), Some("validate")) => {
            draft_validate(with_args(&context, rest))
        }
        (Some("author"), Some("preview" | "preview-schedule"))
        | (Some("drafts"), Some("preview" | "preview-schedule")) => {
            draft_preview(with_args(&context, rest))
        }
        (Some("author"), Some("suggest")) | (Some("drafts"), Some("suggest")) => {
            draft_suggest(with_args(&context, rest))
        }
        (Some("author"), Some("export")) | (Some("drafts"), Some("export")) => {
            draft_export(with_args(&context, rest))
        }
        (Some("author"), Some("diff")) | (Some("drafts"), Some("diff")) => {
            draft_diff(with_args(&context, rest))
        }
        (Some("packages"), Some("list")) => package_list(with_args(&context, rest)),
        (Some("packages"), Some("upload")) => package_upload(with_args(&context, rest)),
        (Some("packages"), Some("inspect")) => package_inspect(with_args(&context, rest)),
        (Some("secrets"), Some("list")) => secret_list(with_args(&context, rest)),
        (Some("secrets"), Some("put" | "set")) => secret_put(with_args(&context, rest)),
        (Some("secrets"), Some("delete" | "rm")) => secret_delete(with_args(&context, rest)),
        (Some("experiments"), Some("build")) => experiment_build(with_args(&context, rest)),
        (Some("experiments"), Some("doctor")) => experiment_doctor(with_args(&context, rest)),
        (Some("runs"), Some("list")) => run_list(with_args(&context, rest)),
        (Some("runs"), Some("create")) => run_create(with_args(&context, rest)),
        (Some("runs"), Some("get")) => run_get(with_args(&context, rest)),
        (Some("runs"), Some("runtime")) => run_runtime(with_args(&context, rest)),
        (Some("runs"), Some("events")) => run_events(with_args(&context, rest)),
        (Some("runs"), Some("results")) => run_results(with_args(&context, rest)),
        (Some("runs"), Some("value" | "kv")) => run_value(with_args(&context, rest)),
        _ => bail!(
            "unknown hosted command: {}\n\nRun `buc help` for the Cloud product commands.",
            entered_command_name(group, command)
        ),
    }
}

fn known_hosted_command(group: Option<&str>, command: Option<&str>) -> bool {
    matches!(
        (group, command),
        (Some("health"), None)
            | (Some("build" | "doctor" | "run" | "inspect"), _)
            | (
                Some("author" | "drafts"),
                Some(
                    "canonicalize"
                        | "resolve"
                        | "validate"
                        | "preview"
                        | "preview-schedule"
                        | "suggest"
                        | "export"
                        | "diff"
                )
            )
            | (Some("packages"), Some("list" | "upload" | "inspect"))
            | (
                Some("secrets"),
                Some("list" | "put" | "set" | "delete" | "rm")
            )
            | (Some("experiments"), Some("build" | "doctor"))
            | (
                Some("runs"),
                Some("list" | "create" | "get" | "runtime" | "events" | "results" | "value" | "kv")
            )
    )
}

fn group_command_without_leaf(group: Option<&str>, command: Option<&str>) -> bool {
    command.is_none()
        && matches!(
            group,
            Some(
                "build"
                    | "doctor"
                    | "run"
                    | "inspect"
                    | "author"
                    | "drafts"
                    | "packages"
                    | "secrets"
                    | "experiments"
                    | "runs"
            )
        )
}

fn help_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--help" || arg == "-h")
}

fn entered_command_name(group: Option<&str>, command: Option<&str>) -> String {
    let name = [group, command]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        "buc".to_string()
    } else {
        name
    }
}

fn with_args(context: &CliContext, args: Vec<String>) -> CliContext {
    CliContext {
        api_url: context.api_url.clone(),
        user_token: context.user_token.clone(),
        args,
        client: context.client.clone(),
    }
}

fn alias_args(context: &CliContext, skip: usize) -> CliContext {
    with_args(context, context.args.iter().skip(skip).cloned().collect())
}

fn parse_global_args(argv: Vec<String>) -> Result<CliContext> {
    let mut args = argv;
    let mut api_url = std::env::var(BUCEPHALUS_CLOUD_API_URL_ENV).unwrap_or_default();
    let mut user_token = env_non_empty(BUCEPHALUS_CLOUD_USER_TOKEN_ENV);

    let mut index = 0;
    while index < args.len() {
        let option = args[index].as_str();
        if !matches!(option, "--api-url" | "--user-token") {
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{option} requires a value"))?
            .clone();
        match option {
            "--api-url" => api_url = value,
            "--user-token" => user_token = non_empty(value),
            _ => unreachable!(),
        }
        args.drain(index..=index + 1);
    }

    if user_token.is_none() {
        user_token = shared_cloud_user_token()?;
    }
    if api_url.trim().is_empty() {
        if let Ok(home) = lab_runner::bucephalus_home() {
            if let Some(url) = lab_runner::cloud_profile_string(&home, "/api_url") {
                api_url = url;
            }
        }
    }

    Ok(CliContext {
        api_url: api_url.trim_end_matches('/').to_string(),
        user_token,
        args,
        client: Client::new(),
    })
}

fn ensure_api_configured(context: &CliContext) -> Result<()> {
    if context.api_url.trim().is_empty() {
        bail!(
            "buc needs a hosted API URL. Run `bucephalus login --resource <api-url>` once to persist it, or pass --api-url / set {}.",
            BUCEPHALUS_CLOUD_API_URL_ENV
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct CloudTokenPaths {
    access: PathBuf,
    refresh: PathBuf,
    cache: PathBuf,
}

fn shared_cloud_user_token() -> Result<Option<String>> {
    let home = match lab_runner::bucephalus_home() {
        Ok(home) => home,
        Err(_) => return Ok(None),
    };
    let paths = cloud_token_paths(&home);
    if let Some(cache) = read_cloud_token_cache(&paths) {
        if cloud_token_cache_needs_refresh(&cache) {
            return refresh_cloud_token_cache(&paths, &cache)
                .map(Some)
                .context("failed to refresh cached Cloud OAuth token");
        }
        if let Some(token) = cache.get("access_token").and_then(Value::as_str) {
            let token = token.trim();
            if !token.is_empty() {
                return Ok(Some(token.to_string()));
            }
        }
    }
    Ok(fs::read_to_string(paths.access)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
}

fn cloud_token_paths(home: &Path) -> CloudTokenPaths {
    let auth_dir = home.join("auth");
    CloudTokenPaths {
        access: auth_dir.join("cloud_user_token"),
        refresh: auth_dir.join("cloud_refresh_token"),
        cache: auth_dir.join("cloud_user_token.json"),
    }
}

fn read_cloud_token_cache(paths: &CloudTokenPaths) -> Option<Value> {
    let raw = fs::read_to_string(&paths.cache).ok()?;
    serde_json::from_str(&raw).ok()
}

fn cloud_token_cache_needs_refresh(cache: &Value) -> bool {
    let Some(expires_at_ms) = cache.get("expires_at_ms").and_then(Value::as_i64) else {
        return false;
    };
    let Some(refresh_token) = cache.get("refresh_token").and_then(Value::as_str) else {
        return false;
    };
    !refresh_token.trim().is_empty() && expires_at_ms <= current_unix_time_ms() + 60_000
}

fn refresh_cloud_token_cache(paths: &CloudTokenPaths, cache: &Value) -> Result<String> {
    let token_endpoint = cache
        .get("token_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("cached Cloud token is missing token_endpoint"))?;
    let client_id = cache
        .get("client_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("cached Cloud token is missing client_id"))?;
    let refresh_token = cache
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("cached Cloud token is missing refresh_token"))?;
    let response = Client::new()
        .post(token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .with_context(|| format!("failed to refresh Cloud token at {}", token_endpoint))?;
    let status = response.status().as_u16();
    let bytes = response.bytes()?.to_vec();
    if !(200..300).contains(&status) {
        bail!(
            "Cloud token refresh failed with status {}: {}",
            status,
            String::from_utf8_lossy(&bytes).trim()
        );
    }
    let token: Value = serde_json::from_slice(&bytes)?;
    let mut merged = token.clone();
    if merged
        .get("refresh_token")
        .and_then(Value::as_str)
        .is_none()
    {
        if let Some(object) = merged.as_object_mut() {
            object.insert("refresh_token".to_string(), json!(refresh_token));
        }
    }
    write_cloud_token_cache(paths, cache, &merged)?;
    merged
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Cloud token refresh response missing access_token"))
}

fn write_cloud_token_cache(paths: &CloudTokenPaths, existing: &Value, token: &Value) -> Result<()> {
    let access_token = token
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("token response missing access_token"))?;
    let refresh_token = token
        .get("refresh_token")
        .and_then(Value::as_str)
        .or_else(|| existing.get("refresh_token").and_then(Value::as_str));
    let issued_at = current_unix_time_ms();
    let expires_at_ms = token
        .get("expires_in")
        .and_then(Value::as_i64)
        .map(|seconds| issued_at + seconds.saturating_mul(1000));
    let cache = json!({
        "schema_version": "bucephalus_cloud_oauth_token_v1",
        "issuer": existing.get("issuer").and_then(Value::as_str),
        "client_id": existing.get("client_id").and_then(Value::as_str),
        "audience": existing.get("audience").and_then(Value::as_str),
        "resource": existing.get("resource").and_then(Value::as_str),
        "scope": existing.get("scope").and_then(Value::as_str),
        "token_endpoint": existing.get("token_endpoint").and_then(Value::as_str),
        "token_type": token.get("token_type").and_then(Value::as_str).unwrap_or("Bearer"),
        "access_token": access_token,
        "refresh_token": refresh_token,
        "issued_at_ms": issued_at,
        "expires_at_ms": expires_at_ms
    });
    write_secret_file(&paths.access, format!("{access_token}\n").as_bytes())?;
    if let Some(refresh_token) = refresh_token {
        write_secret_file(&paths.refresh, format!("{refresh_token}\n").as_bytes())?;
    }
    write_secret_file(
        &paths.cache,
        serde_json::to_string_pretty(&cache)?.as_bytes(),
    )?;
    Ok(())
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }
}

fn health(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &[], &[])?;
    reject_no_positionals(&context.args, "buc health")?;
    ensure_api_configured(&context)?;
    print_json(&cloud_fetch(&context, Method::GET, "/readyz", None, None)?)
}

fn package_upload(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--file", "--label"], &["--json"])?;
    let path = package_path_arg(&context.args)?;
    let label = option_value(&context.args, "--label")?;
    let json_output = json_requested(&context.args);
    let prepared = prepare_sealed_package_input(Path::new(&path))?;
    ensure_api_configured(&context)?;
    let imported = upload_sealed_package_artifact(
        &context,
        &prepared.archive_path,
        label.as_deref(),
        "/v1/imports/sealed-package",
        None,
        None,
    )?;
    if json_output {
        print_json(&imported.response)?;
    } else {
        print_import_summary(&imported.response, Some(&prepared.source_label))?;
    }
    ensure_import_accepted(&imported.response, "package upload")
}

fn package_list(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--limit"], &["--json"])?;
    reject_no_positionals(&context.args, "buc packages list")?;
    ensure_api_configured(&context)?;
    let packages = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query(
            "/v1/packages",
            &[("limit", option_value(&context.args, "--limit")?)],
        ),
        None,
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&packages)
    } else {
        print_package_list_summary(&packages)
    }
}

fn experiment_build(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &build_value_options(), &["--json"])?;
    let path = build_input_path_arg(&context.args)?;
    let context_root = option_value(&context.args, "--context-root")?;
    let label = option_value(&context.args, "--label")?;
    let json_output = json_requested(&context.args);
    let expected_runtime_options = Value::Object(runtime_options_from_args(&context.args)?);
    ensure_api_configured(&context)?;
    let path = Path::new(&path);
    let (build, source_label, expected_build_kind, expected_source_kind, expected_entrypoint) =
        if is_authoring_yaml_path(path) {
            let prepared =
                prepare_authoring_context_input(path, context_root.as_deref().map(Path::new))?;
            let entrypoint = prepared.entrypoint.clone();
            (
                upload_sealed_package_artifact(
                    &context,
                    &prepared.archive_path,
                    label.as_deref(),
                    "/v1/experiments/builds",
                    Some(expected_runtime_options.clone()),
                    Some(json!({
                        "input_kind": "authoring_context",
                        "entrypoint": prepared.entrypoint,
                    })),
                )?,
                prepared.source_label.clone(),
                "hosted_authoring_build",
                "authoring_context",
                Some(entrypoint),
            )
        } else {
            let prepared = prepare_sealed_package_input(path)?;
            (
                upload_sealed_package_artifact(
                    &context,
                    &prepared.archive_path,
                    label.as_deref(),
                    "/v1/experiments/builds",
                    Some(expected_runtime_options.clone()),
                    None,
                )?,
                prepared.source_label.clone(),
                "sealed_package_import",
                "sealed_package",
                None,
            )
        };
    ensure_build_response_matches_input(
        &build.response,
        expected_build_kind,
        expected_source_kind,
    )?;
    ensure_build_source_upload_id_matches(&build.response, &build.upload_id)?;
    ensure_build_source_digest_matches(&build.response, &build.expected_digest)?;
    ensure_build_source_byte_size_matches(&build.response, build.expected_byte_size)?;
    ensure_build_source_entrypoint_matches(&build.response, expected_entrypoint.as_deref())?;
    ensure_build_runtime_options_match(&build.response, &expected_runtime_options)?;
    ensure_cloud_readiness_runtime_options_match(&build.response, &expected_runtime_options)?;
    ensure_build_target_matches(&build.response)?;
    ensure_cloud_readiness_target_matches(&build.response)?;
    ensure_build_package_contract_matches(&build.response, expected_source_kind)?;
    ensure_build_import_identity(&build.response)?;
    ensure_authoring_build_identity(
        &build.response,
        expected_build_kind,
        &build.upload_id,
        expected_entrypoint.as_deref(),
    )?;
    if json_output {
        print_json(&build.response)?;
    } else {
        print_build_summary(&build.response, &source_label)?;
    }
    ensure_import_accepted(&build.response, "hosted build")?;
    ensure_cloud_readiness(&build.response)
}

fn package_inspect(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--package-digest"], &["--json"])?;
    let digest = package_digest_arg(&context.args)?;
    ensure_api_configured(&context)?;
    let package = package_get_object(&context, &digest)?;
    if json_requested(&context.args) {
        print_json(&package)
    } else {
        print_package_summary(&package)
    }
}

fn experiment_doctor(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &doctor_value_options(), &["--json"])?;
    let digest = package_digest_arg(&context.args)?;
    let secret_refs = secret_refs_from_options(&context.args)?;
    let runtime_options = runtime_options_from_args(&context.args)?;
    ensure_api_configured(&context)?;
    let diagnosis = cloud_fetch(
        &context,
        Method::POST,
        "/v1/experiments/doctor",
        Some(json!({
            "package_digest": digest,
            "secret_refs": secret_refs,
            "runtime_options": Value::Object(runtime_options)
        })),
        None,
    )?;
    ensure_doctor_runnable(&diagnosis, &digest)
        .map_err(|err| anyhow!("Cloud doctor did not prove this package runnable: {err}"))?;
    if json_requested(&context.args) {
        print_json(&diagnosis)
    } else {
        print_doctor_summary(&diagnosis)
    }
}

fn run_create(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &run_create_value_options(), &["--json"])?;
    let digest = package_digest_arg(&context.args)?;
    let secret_refs = secret_refs_from_options(&context.args)?;
    let runtime_options = runtime_options_from_args(&context.args)?;
    let env = run_env_from_options(&context.args, &secret_refs)?;
    let label = option_value(&context.args, "--label")?;
    ensure_api_configured(&context)?;

    let diagnosis = cloud_fetch(
        &context,
        Method::POST,
        "/v1/experiments/doctor",
        Some(json!({
            "package_digest": digest,
            "secret_refs": secret_refs,
            "runtime_options": Value::Object(runtime_options.clone())
        })),
        None,
    )
    .context("Cloud doctor rejected this run before queueing it")?;
    ensure_doctor_runnable(&diagnosis, &digest)
        .map_err(|err| anyhow!("Cloud doctor rejected this run before queueing it: {err}"))?;

    let run = cloud_fetch(
        &context,
        Method::POST,
        "/v1/runs",
        Some(json!({
            "package_digest": digest,
            "run_label": label,
            "env": env,
            "secret_refs": secret_refs,
            "runtime_options": Value::Object(runtime_options)
        })),
        None,
    )?;
    ensure_run_created(&run, &digest)?;
    if json_requested(&context.args) {
        print_json(&run)
    } else {
        print_run_summary(&run)
    }
}

fn run_list(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--limit"], &["--json"])?;
    reject_no_positionals(&context.args, "buc runs list")?;
    ensure_api_configured(&context)?;
    let runs = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query(
            "/v1/runs",
            &[("limit", option_value(&context.args, "--limit")?)],
        ),
        None,
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&runs)
    } else {
        print_run_list_summary(&runs)
    }
}

fn run_get(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--run-id"], &["--json"])?;
    let run_id = run_id_arg(&context.args)?;
    ensure_api_configured(&context)?;
    let run = cloud_fetch(
        &context,
        Method::GET,
        &format!("/v1/runs/{}", encode_path_segment(&run_id)),
        None,
        None,
    )?;
    ensure_run_response_matches(&run, &run_id)?;
    if json_requested(&context.args) {
        print_json(&run)
    } else {
        print_run_summary(&run)
    }
}

fn run_runtime(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--run-id"], &["--json"])?;
    let run_id = run_id_arg(&context.args)?;
    ensure_api_configured(&context)?;
    let runtime = cloud_fetch(
        &context,
        Method::GET,
        &format!("/v1/runs/{}/runtime", encode_path_segment(&run_id)),
        None,
        None,
    )?;
    ensure_runtime_response_matches(&runtime, &run_id)?;
    if json_requested(&context.args) {
        print_json(&runtime)
    } else {
        print_runtime_summary(&runtime)
    }
}

fn run_events(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &["--run-id", "--limit", "--after-row-seq"],
        &["--json"],
    )?;
    let run_id = run_id_arg(&context.args)?;
    ensure_api_configured(&context)?;
    let events = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query(
            &format!("/v1/runs/{}/runtime/events", encode_path_segment(&run_id)),
            &[
                ("limit", option_value(&context.args, "--limit")?),
                (
                    "after_row_seq",
                    option_value(&context.args, "--after-row-seq")?,
                ),
            ],
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches(&events, &run_id)?;
    if json_requested(&context.args) {
        print_json(&events)
    } else {
        print_runtime_events_summary(&events)
    }
}

fn run_results(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--run-id", "--limit"], &["--json"])?;
    let run_id = run_id_arg(&context.args)?;
    ensure_api_configured(&context)?;
    let results = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query(
            &format!("/v1/runs/{}/runtime/results", encode_path_segment(&run_id)),
            &[("limit", option_value(&context.args, "--limit")?)],
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches(&results, &run_id)?;
    if json_requested(&context.args) {
        print_json(&results)
    } else {
        print_runtime_results_summary(&results)
    }
}

fn run_value(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--run-id", "--key"], &["--json"])?;
    let (run_id, key) = run_value_args(&context.args)?;
    ensure_api_configured(&context)?;
    let value = cloud_fetch(
        &context,
        Method::GET,
        &format!(
            "/v1/runs/{}/runtime/kv/{}",
            encode_path_segment(&run_id),
            encode_path_segment(&key)
        ),
        None,
        None,
    )?;
    ensure_runtime_value_response_matches(&value, &run_id, &key)?;
    if json_requested(&context.args) {
        print_json(&value)
    } else {
        print_runtime_value_summary(&value)
    }
}

fn secret_list(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &[], &["--json"])?;
    reject_no_positionals(&context.args, "buc secrets list")?;
    ensure_api_configured(&context)?;
    let secrets = cloud_fetch(&context, Method::GET, "/v1/secrets", None, None)?;
    ensure_secret_list_response(&secrets)?;
    if json_requested(&context.args) {
        print_json(&secrets)
    } else {
        print_secret_list_summary(&secrets)
    }
}

fn secret_put(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &["--value-file", "--from-env", "--value"],
        &["--json", "--stdin"],
    )?;
    let name = secret_name_arg(&context.args)?;
    let value = secret_value_from_args(&context.args)?;
    ensure_api_configured(&context)?;
    let secret = cloud_fetch(
        &context,
        Method::PUT,
        &format!("/v1/secrets/{}", encode_path_segment(&name)),
        Some(json!({ "value": value })),
        None,
    )?;
    ensure_hosted_secret_response(&secret, Some(&name), "secret put")?;
    if json_requested(&context.args) {
        print_json(&secret)
    } else {
        print_secret_summary(&secret, "stored")
    }
}

fn secret_delete(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &[], &["--json"])?;
    let name = secret_name_arg(&context.args)?;
    ensure_api_configured(&context)?;
    let deleted = cloud_fetch(
        &context,
        Method::DELETE,
        &format!("/v1/secrets/{}", encode_path_segment(&name)),
        None,
        None,
    )?;
    ensure_secret_response_matches(&deleted, &name, "secret delete")?;
    ensure_secret_delete_confirmed(&deleted, &name)?;
    if json_requested(&context.args) {
        print_json(&deleted)
    } else {
        print_secret_delete_summary(&deleted, &name)
    }
}

fn draft_canonicalize(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--file"], &["--json"])?;
    let draft = draft_from_args(&context.args)?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/canonicalize",
        Some(json!({ "draft": draft })),
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_draft_canonicalize_summary(&response)
    }
}

fn draft_resolve(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--file"], &["--json"])?;
    let draft = draft_from_args(&context.args)?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/resolve",
        Some(json!({ "draft": draft })),
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_draft_resolve_summary(&response)
    }
}

fn draft_validate(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &["--file", "--validation-level"],
        &["--json"],
    )?;
    let draft = draft_from_args(&context.args)?;
    let mut body = Map::new();
    body.insert("draft".to_string(), draft);
    insert_option_string(
        &mut body,
        "validation_level",
        option_value(&context.args, "--validation-level")?,
    );
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/validate",
        Some(Value::Object(body)),
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_draft_validate_summary(&response)
    }
}

fn draft_preview(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--file"], &["--json"])?;
    let draft = draft_from_args(&context.args)?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/preview-schedule",
        Some(json!({ "draft": draft })),
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_draft_preview_summary(&response)
    }
}

fn draft_suggest(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &["--file", "--target", "--q", "--limit"],
        &["--json"],
    )?;
    let draft = draft_from_args(&context.args)?;
    let target = required_option(&context.args, "--target")?;
    let mut body = Map::new();
    body.insert("draft".to_string(), draft);
    body.insert("target".to_string(), json!(target));
    insert_option_string(&mut body, "q", option_value(&context.args, "--q")?);
    if let Some(limit) = number_option(&context.args, "--limit")? {
        body.insert("limit".to_string(), json!(limit));
    }
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/suggest",
        Some(Value::Object(body)),
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_draft_suggest_summary(&response)
    }
}

fn draft_export(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--file", "--format"], &["--json"])?;
    let draft = draft_from_args(&context.args)?;
    let mut body = Map::new();
    body.insert("draft".to_string(), draft);
    insert_option_string(
        &mut body,
        "format",
        option_value(&context.args, "--format")?,
    );
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/export",
        Some(Value::Object(body)),
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_draft_export_summary(&response)
    }
}

fn draft_diff(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &[], &["--json"])?;
    let args = positional_args(&context.args);
    if args.len() != 2 {
        bail!(
            "buc author diff requires exactly two draft paths: <left.yaml|json> <right.yaml|json>"
        );
    }
    let left_path = &args[0];
    let right_path = &args[1];
    let left = read_draft_file(Path::new(left_path))?;
    let right = read_draft_file(Path::new(right_path))?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/diff",
        Some(json!({
            "left": { "draft": left },
            "right": { "draft": right }
        })),
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_draft_diff_summary(&response)
    }
}

fn package_path_arg(args: &[String]) -> Result<String> {
    single_positional_or_option(args, "--file", "sealed package directory/archive")
}

fn run_id_arg(args: &[String]) -> Result<String> {
    single_positional_or_option(args, "--run-id", "run id")
}

fn draft_from_args(args: &[String]) -> Result<Value> {
    let path = single_positional_or_option(args, "--file", "draft JSON/YAML path")?;
    read_draft_file(Path::new(&path))
}

fn read_draft_file(path: &Path) -> Result<Value> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: Value = if path.extension().and_then(|value| value.to_str()) == Some("json") {
        serde_json::from_str(&raw)
            .with_context(|| format!("draft JSON is invalid: {}", path.display()))?
    } else {
        serde_json::to_value(
            serde_yaml::from_str::<serde_yaml::Value>(&raw)
                .with_context(|| format!("draft YAML is invalid: {}", path.display()))?,
        )?
    };
    if !value.is_object() {
        bail!(
            "draft file must contain a JSON/YAML object: {}",
            path.display()
        );
    }
    Ok(value)
}

fn build_input_path_arg(args: &[String]) -> Result<String> {
    single_positional_or_option(
        args,
        "--file",
        "experiment.yaml or sealed package directory/archive",
    )
}

fn package_digest_arg(args: &[String]) -> Result<String> {
    single_positional_or_option(args, "--package-digest", "package digest")
}

fn prepare_sealed_package_input(path: &Path) -> Result<PreparedPackageInput> {
    reject_authoring_yaml(path)?;
    if path.is_dir() {
        return prepare_package_directory(path);
    }
    if path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name == "manifest.json")
    {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("manifest.json has no parent directory"))?;
        return prepare_package_directory(parent);
    }
    if path.is_file() && is_supported_package_archive(path) {
        return Ok(PreparedPackageInput {
            archive_path: path.to_path_buf(),
            source_label: path.display().to_string(),
            temp_root: None,
        });
    }
    if path.exists() {
        bail!(
            "buc expects a sealed package directory with manifest.json, or a .tgz/.tar.gz/.tar archive. Got: {}",
            path.display()
        );
    }
    bail!("sealed package path does not exist: {}", path.display());
}

fn reject_authoring_yaml(path: &Path) -> Result<()> {
    if is_authoring_yaml_path(path) {
        bail!(
            "buc packages upload expects a sealed package, not authoring YAML. To build YAML for hosted Cloud, run `buc build {}`.",
            path.display()
        );
    }
    Ok(())
}

fn is_authoring_yaml_path(path: &Path) -> bool {
    let lower = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    lower.ends_with(".yaml") || lower.ends_with(".yml")
}

fn prepare_authoring_context_input(
    path: &Path,
    context_root: Option<&Path>,
) -> Result<PreparedAuthoringContextInput> {
    if !path.is_file() {
        bail!("authoring YAML path does not exist: {}", path.display());
    }
    let context_root = context_root.map(Path::to_path_buf).unwrap_or_else(|| {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    });
    if !context_root.is_dir() {
        bail!(
            "authoring context root must be an existing directory: {}",
            context_root.display()
        );
    }
    let canonical_root = fs::canonicalize(&context_root).with_context(|| {
        format!(
            "failed to resolve authoring context root {}",
            context_root.display()
        )
    })?;
    let canonical_path = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve authoring YAML {}", path.display()))?;
    let entrypoint_path = canonical_path
        .strip_prefix(&canonical_root)
        .with_context(|| {
            format!(
                "authoring YAML {} must be inside --context-root {}",
                path.display(),
                context_root.display()
            )
        })?;
    let entrypoint = as_posix_relative_path(entrypoint_path)?;
    let temp_root = make_temp_dir("buc-authoring-context-upload")?;
    let archive_path = temp_root.join("authoring-context.tgz");
    create_authoring_context_archive(&canonical_root, &archive_path)?;
    Ok(PreparedAuthoringContextInput {
        archive_path,
        source_label: path.display().to_string(),
        entrypoint,
        temp_root,
    })
}

fn prepare_package_directory(package_dir: &Path) -> Result<PreparedPackageInput> {
    let manifest = package_dir.join("manifest.json");
    if !manifest.is_file() {
        bail!(
            "sealed package directory is missing manifest.json: {}",
            package_dir.display()
        );
    }
    preflight_sealed_package_directory(package_dir)?;
    let temp_root = make_temp_dir("buc-package-upload")?;
    let archive_path = temp_root.join("package.tgz");
    create_package_archive(package_dir, &archive_path)?;
    Ok(PreparedPackageInput {
        archive_path,
        source_label: package_dir.display().to_string(),
        temp_root: Some(temp_root),
    })
}

fn is_supported_package_archive(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    lower.ends_with(".tgz") || lower.ends_with(".tar.gz") || lower.ends_with(".tar")
}

fn create_package_archive(package_dir: &Path, archive_path: &Path) -> Result<()> {
    let mut entries = fs::read_dir(package_dir)
        .with_context(|| format!("failed to read package directory {}", package_dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.is_empty() {
        bail!("package directory is empty: {}", package_dir.display());
    }
    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(archive_path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    for entry in entries {
        let name = entry.file_name();
        let path = entry.path();
        if path.is_dir() {
            builder.append_dir_all(Path::new(&name), &path)?;
        } else {
            builder.append_path_with_name(&path, Path::new(&name))?;
        }
    }
    builder.finish()?;
    Ok(())
}

fn create_authoring_context_archive(context_root: &Path, archive_path: &Path) -> Result<()> {
    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(archive_path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    let mut plan = AuthoringContextArchivePlan::default();
    collect_authoring_context_files(context_root, context_root, &mut plan)?;
    if plan.files.is_empty() {
        bail!(
            "authoring context has no uploadable files: {}",
            context_root.display()
        );
    }
    plan.files.sort();
    for rel in plan.files {
        builder.append_path_with_name(context_root.join(&rel), Path::new(&rel))?;
    }
    builder.finish()?;
    Ok(())
}

fn collect_authoring_context_files(
    root: &Path,
    dir: &Path,
    plan: &mut AuthoringContextArchivePlan,
) -> Result<()> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| {
            format!(
                "failed to read authoring context directory {}",
                dir.display()
            )
        })?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_authoring_context_entry(&name, file_type.is_dir()) {
            continue;
        }
        record_authoring_context_entry(plan, &path)?;
        if file_type.is_symlink() {
            bail!(
                "authoring context contains symlink {}; hosted builds require regular files so the uploaded context is explicit",
                path.display()
            );
        }
        if file_type.is_dir() {
            collect_authoring_context_files(root, &path, plan)?;
            continue;
        }
        if !file_type.is_file() {
            bail!(
                "authoring context contains unsupported file type: {}",
                path.display()
            );
        }
        let file_size = entry
            .metadata()
            .with_context(|| format!("failed to stat authoring context file {}", path.display()))?
            .len();
        record_authoring_context_bytes(plan, &path, file_size)?;
        let rel = path
            .strip_prefix(root)
            .with_context(|| format!("authoring context path escaped root: {}", path.display()))?;
        plan.files.push(as_posix_relative_path(rel)?);
    }
    Ok(())
}

fn record_authoring_context_entry(
    plan: &mut AuthoringContextArchivePlan,
    path: &Path,
) -> Result<()> {
    plan.entries = plan
        .entries
        .checked_add(1)
        .ok_or_else(|| anyhow!("authoring context entry count overflowed"))?;
    let max_entries = max_authoring_context_archive_entries();
    if plan.entries > max_entries {
        bail!(
            "authoring context has too many entries for hosted Cloud build: {} exceeds limit {}. Narrow --context-root or remove generated files before running `buc build`.",
            path.display(),
            max_entries
        );
    }
    Ok(())
}

fn record_authoring_context_bytes(
    plan: &mut AuthoringContextArchivePlan,
    path: &Path,
    bytes: u64,
) -> Result<()> {
    plan.expanded_bytes = plan
        .expanded_bytes
        .checked_add(bytes)
        .ok_or_else(|| anyhow!("authoring context byte count overflowed"))?;
    let max_bytes = max_authoring_context_expanded_bytes();
    if plan.expanded_bytes > max_bytes {
        bail!(
            "authoring context is too large for hosted Cloud build: {} pushes expanded size to {} bytes, above limit {}. Narrow --context-root or remove generated files before running `buc build`.",
            path.display(),
            plan.expanded_bytes,
            max_bytes
        );
    }
    Ok(())
}

fn should_skip_authoring_context_entry(name: &str, is_dir: bool) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == ".git"
        || lower == ".env"
        || lower.starts_with(".env.")
        || lower == ".npmrc"
        || lower == ".pypirc"
        || lower == ".netrc"
        || lower == ".dockercfg"
        || lower == "id_rsa"
        || lower == "id_dsa"
        || lower == "id_ecdsa"
        || lower == "id_ed25519"
        || lower == "application_default_credentials.json"
    {
        return true;
    }
    if !is_dir {
        return lower == ".ds_store";
    }
    matches!(
        lower.as_str(),
        "target"
            | "node_modules"
            | ".bucephalus"
            | ".bucephalus-package"
            | ".ssh"
            | ".aws"
            | ".azure"
            | ".docker"
            | ".gnupg"
            | "gcloud"
    )
}

fn max_authoring_context_archive_entries() -> u64 {
    positive_u64_env(
        "BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_ENTRIES",
        DEFAULT_MAX_AUTHORING_CONTEXT_ARCHIVE_ENTRIES,
    )
}

fn max_authoring_context_expanded_bytes() -> u64 {
    positive_u64_env(
        "BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES",
        DEFAULT_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES,
    )
}

fn positive_u64_env(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

fn as_posix_relative_path(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    anyhow!(
                        "authoring context path must be valid UTF-8: {}",
                        path.display()
                    )
                })?;
                if part.is_empty()
                    || part == "."
                    || part == ".."
                    || part.contains('/')
                    || part.contains('\\')
                {
                    bail!(
                        "authoring context path contains unsafe segment: {}",
                        path.display()
                    );
                }
                parts.push(part.to_string());
            }
            _ => bail!(
                "authoring context path must be relative: {}",
                path.display()
            ),
        }
    }
    if parts.is_empty() {
        bail!(
            "authoring context path must name a file: {}",
            path.display()
        );
    }
    Ok(parts.join("/"))
}

fn preflight_sealed_package_directory(package_dir: &Path) -> Result<()> {
    let manifest_path = package_dir.join("manifest.json");
    let manifest_raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Value = serde_json::from_str(&manifest_raw).with_context(|| {
        format!(
            "manifest.json is not valid JSON: {}",
            manifest_path.display()
        )
    })?;
    if !manifest.is_object() {
        bail!(
            "manifest.json must be a JSON object: {}",
            manifest_path.display()
        );
    }
    if manifest.pointer("/schema_version").and_then(Value::as_str) != Some("sealed_run_package_v2")
    {
        bail!(
            "manifest.json is not a sealed_run_package_v2 manifest. For authoring YAML, run `buc build experiment.yaml`; for a sealed package, pass the directory produced by `bucephalus build experiment.yaml --out <package-dir>`."
        );
    }
    for (pointer, label) in [
        ("/checksums_ref", "checksums metadata"),
        ("/package_checks_ref", "package preflight report"),
    ] {
        let reference = manifest.pointer(pointer).and_then(Value::as_str).ok_or_else(|| {
            anyhow!(
                "sealed package manifest is missing {pointer}. This does not look like a complete `bucephalus build` output; rebuild locally before uploading."
            )
        })?;
        let path = resolve_metadata_ref(package_dir, reference, pointer)?;
        if !path.is_file() {
            bail!(
                "sealed package manifest {pointer} points to missing {label}: {}",
                path.display()
            );
        }
    }
    let resolved_experiment = package_dir.join("resolved_experiment.json");
    if !resolved_experiment.is_file() {
        bail!(
            "sealed package directory is missing resolved_experiment.json. For authoring YAML, run `buc build experiment.yaml`; for a sealed package, rebuild with `bucephalus build experiment.yaml --out <package-dir>` before `buc experiments build`."
        );
    }
    Ok(())
}

fn resolve_metadata_ref(package_dir: &Path, reference: &str, pointer: &str) -> Result<PathBuf> {
    let reference = reference.trim();
    if reference.is_empty() {
        bail!("sealed package manifest {pointer} must be a non-empty relative path");
    }
    let path = Path::new(reference);
    if path.is_absolute()
        || reference
            .split('/')
            .any(|part| part == ".." || part.is_empty())
    {
        bail!(
            "sealed package manifest {pointer} must be a simple relative path inside the package, got {reference:?}"
        );
    }
    Ok(package_dir.join(path))
}

fn upload_sealed_package_artifact(
    context: &CliContext,
    path: &Path,
    label: Option<&str>,
    import_path: &str,
    runtime_options: Option<Value>,
    extra_body: Option<Value>,
) -> Result<UploadedArtifactResponse> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let expected_digest = sha256_digest(&bytes);
    let expected_byte_size = bytes.len() as u64;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("package.tgz");
    let upload = cloud_fetch(
        context,
        Method::POST,
        "/v1/uploads",
        Some(json!({
            "filename": filename,
            "media_type": media_type_for_path(path),
            "expected_digest": expected_digest,
            "byte_size": expected_byte_size
        })),
        None,
    )?;
    let upload_id = upload
        .get("upload_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("upload response did not include upload_id"))?;
    cloud_fetch(
        context,
        Method::PUT,
        &format!("/v1/uploads/{upload_id}/content"),
        None,
        Some((bytes, "application/octet-stream")),
    )?;
    cloud_fetch(
        context,
        Method::POST,
        &format!("/v1/uploads/{upload_id}/complete"),
        Some(json!({})),
        None,
    )?;
    let mut body = Map::new();
    body.insert("upload_id".to_string(), json!(upload_id));
    body.insert("label".to_string(), json!(label));
    if let Some(runtime_options) = runtime_options {
        body.insert("runtime_options".to_string(), runtime_options);
    }
    if let Some(extra_body) = extra_body {
        let extra = extra_body
            .as_object()
            .ok_or_else(|| anyhow!("extra upload body must be a JSON object"))?;
        for (key, value) in extra {
            body.insert(key.clone(), value.clone());
        }
    }
    let response = cloud_fetch(
        context,
        Method::POST,
        import_path,
        Some(Value::Object(body)),
        None,
    )?;
    Ok(UploadedArtifactResponse {
        response,
        upload_id: upload_id.to_string(),
        expected_digest,
        expected_byte_size,
    })
}

fn ensure_import_accepted(value: &Value, noun: &str) -> Result<()> {
    let status = value
        .get("import")
        .and_then(|import| import.get("status"))
        .and_then(Value::as_str)
        .or_else(|| value.get("status").and_then(Value::as_str))
        .unwrap_or("unknown");
    if status == "accepted" {
        return Ok(());
    }
    if let Some(detail) = authoring_build_failure_detail(value) {
        bail!("{noun} failed: {detail}");
    }
    let import = value.get("import").unwrap_or(value);
    let detail = import
        .get("error_message")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("the Cloud importer rejected the sealed package");
    bail!("{noun} failed: {detail}");
}

fn ensure_cloud_readiness(value: &Value) -> Result<()> {
    let Some(readiness) = value.get("cloud_readiness") else {
        bail!("hosted build response is missing cloud_readiness; Cloud did not prove this package is runnable");
    };
    let status = readiness
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    if status == "cloud_runnable" {
        let package_digest = value
            .get("package_digest")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("hosted build response is missing package_digest"))?;
        let readiness_digest = readiness
            .get("package_digest")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("hosted build cloud_readiness is missing package_digest"))?;
        if readiness_digest != package_digest {
            bail!(
                "hosted build package_digest mismatch: build returned {package_digest}, readiness checked {readiness_digest}"
            );
        }
    }
    if status == "cloud_runnable" {
        return Ok(());
    }
    let blocked = readiness
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|check| {
            matches!(
                check.get("status").and_then(Value::as_str),
                Some("blocked") | Some("unavailable")
            )
        })
        .filter_map(format_cloud_check)
        .collect::<Vec<_>>();
    if blocked.is_empty() {
        if status == "unavailable" {
            bail!("hosted build Cloud readiness is unavailable");
        }
        bail!("hosted build is not runnable in Cloud: {status}");
    }
    if status == "unavailable" {
        bail!(
            "hosted build Cloud readiness is unavailable:\n{}",
            blocked.join("\n")
        );
    }
    bail!(
        "hosted build is not runnable in Cloud:\n{}",
        blocked.join("\n")
    );
}

fn ensure_build_response_matches_input(
    value: &Value,
    expected_build_kind: &str,
    expected_source_kind: &str,
) -> Result<()> {
    let build_kind = value
        .get("build_kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("hosted build response is missing build_kind"))?;
    if build_kind != expected_build_kind {
        bail!(
            "hosted build response kind mismatch: requested {expected_source_kind}, API returned {build_kind}"
        );
    }
    let source_kind = value
        .get("build_environment")
        .and_then(|environment| environment.get("source"))
        .and_then(|source| source.get("input_kind"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("hosted build response is missing build_environment.source.input_kind")
        })?;
    if source_kind != expected_source_kind {
        bail!(
            "hosted build source kind mismatch: requested {expected_source_kind}, API built {source_kind}"
        );
    }
    Ok(())
}

fn ensure_build_source_upload_id_matches(value: &Value, expected_upload_id: &str) -> Result<()> {
    let upload_id = value
        .get("build_environment")
        .and_then(|environment| environment.get("source"))
        .and_then(|source| source.get("upload_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("hosted build response is missing build_environment.source.upload_id")
        })?;
    if upload_id != expected_upload_id {
        bail!(
            "hosted build source upload mismatch: uploaded {expected_upload_id}, API built from {upload_id}"
        );
    }
    Ok(())
}

fn ensure_build_source_digest_matches(value: &Value, expected_digest: &str) -> Result<()> {
    let content_digest = value
        .get("build_environment")
        .and_then(|environment| environment.get("source"))
        .and_then(|source| source.get("content_digest"))
        .ok_or_else(|| {
            anyhow!("hosted build response is missing build_environment.source.content_digest")
        })?;
    let content_digest = content_digest
        .as_str()
        .filter(|digest| !digest.trim().is_empty())
        .ok_or_else(|| {
            anyhow!("hosted build response has invalid build_environment.source.content_digest")
        })?;
    if content_digest != expected_digest {
        bail!(
            "hosted build source digest mismatch: uploaded {expected_digest}, API built from {content_digest}"
        );
    }
    Ok(())
}

fn ensure_build_source_byte_size_matches(value: &Value, expected_byte_size: u64) -> Result<()> {
    let byte_size = value
        .get("build_environment")
        .and_then(|environment| environment.get("source"))
        .and_then(|source| source.get("byte_size"))
        .ok_or_else(|| {
            anyhow!("hosted build response is missing build_environment.source.byte_size")
        })?;
    let byte_size = byte_size.as_u64().ok_or_else(|| {
        anyhow!("hosted build response has invalid build_environment.source.byte_size")
    })?;
    if byte_size != expected_byte_size {
        bail!(
            "hosted build source byte_size mismatch: uploaded {expected_byte_size}, API built from {byte_size}"
        );
    }
    Ok(())
}

fn ensure_build_source_entrypoint_matches(
    value: &Value,
    expected_entrypoint: Option<&str>,
) -> Result<()> {
    let source_entrypoint = value
        .get("build_environment")
        .and_then(|environment| environment.get("source"))
        .and_then(|source| source.get("entrypoint"));
    match expected_entrypoint {
        Some(expected) => {
            let entrypoint = source_entrypoint
                .and_then(Value::as_str)
                .filter(|entrypoint| !entrypoint.trim().is_empty())
                .ok_or_else(|| {
                    anyhow!("hosted build response is missing build_environment.source.entrypoint")
                })?;
            if entrypoint != expected {
                bail!(
                    "hosted build source entrypoint mismatch: requested {expected}, API built {entrypoint}"
                );
            }
        }
        None => {
            if source_entrypoint.is_some() {
                bail!(
                    "hosted build source entrypoint mismatch: sealed package builds must not report an authoring entrypoint"
                );
            }
        }
    }
    Ok(())
}

fn ensure_build_runtime_options_match(
    value: &Value,
    expected_runtime_options: &Value,
) -> Result<()> {
    let runtime_options = value
        .get("build_environment")
        .and_then(|environment| environment.get("runtime_options"))
        .ok_or_else(|| {
            anyhow!("hosted build response is missing build_environment.runtime_options")
        })?;
    if !runtime_options.is_object() {
        bail!("hosted build response has invalid build_environment.runtime_options");
    }
    if runtime_options != expected_runtime_options {
        bail!(
            "hosted build runtime options mismatch: requested {}, API built {}",
            compact_json_lossy(expected_runtime_options),
            compact_json_lossy(runtime_options)
        );
    }
    Ok(())
}

fn ensure_cloud_readiness_runtime_options_match(
    value: &Value,
    expected_runtime_options: &Value,
) -> Result<()> {
    let runtime_options = value
        .get("cloud_readiness")
        .and_then(|readiness| readiness.get("runtime_options"))
        .ok_or_else(|| anyhow!("hosted build cloud_readiness is missing runtime_options"))?;
    if !runtime_options.is_object() {
        bail!("hosted build cloud_readiness has invalid runtime_options");
    }
    if runtime_options != expected_runtime_options {
        bail!(
            "hosted build Cloud readiness runtime options mismatch: requested {}, readiness checked {}",
            compact_json_lossy(expected_runtime_options),
            compact_json_lossy(runtime_options)
        );
    }
    Ok(())
}

fn ensure_build_target_matches(value: &Value) -> Result<()> {
    let target = value
        .get("build_environment")
        .and_then(|environment| environment.get("target"))
        .ok_or_else(|| anyhow!("hosted build response is missing build_environment.target"))?;
    ensure_hosted_target(target, "hosted build target", "API built")
}

fn ensure_cloud_readiness_target_matches(value: &Value) -> Result<()> {
    let target = value
        .get("cloud_readiness")
        .and_then(|readiness| readiness.get("target"))
        .ok_or_else(|| anyhow!("hosted build cloud_readiness is missing target"))?;
    ensure_hosted_target(
        target,
        "hosted build Cloud readiness target",
        "readiness checked",
    )
}

fn ensure_hosted_target(value: &Value, noun: &str, verb: &str) -> Result<()> {
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{noun} is missing kind"))?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{noun} is missing name"))?;
    if kind != "hosted_cloud" || name != "default" {
        bail!("{noun} mismatch: requested hosted_cloud/default, {verb} {kind}/{name}");
    }
    Ok(())
}

fn ensure_build_package_contract_matches(value: &Value, expected_source_kind: &str) -> Result<()> {
    let contract = value
        .get("build_environment")
        .and_then(|environment| environment.get("package_contract"))
        .ok_or_else(|| {
            anyhow!("hosted build response is missing build_environment.package_contract")
        })?;
    let input_kind = contract
        .get("input_kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("hosted build package contract is missing input_kind"))?;
    if input_kind != expected_source_kind {
        bail!(
            "hosted build package contract input mismatch: requested {expected_source_kind}, contract reports {input_kind}"
        );
    }
    ensure_contract_string(contract, "authoring_compiler", "core_universal_v1")?;
    ensure_contract_string(contract, "sealed_schema_version", "sealed_run_package_v2")?;
    ensure_contract_string(
        contract,
        "readiness_schema_version",
        "hosted_cloud_readiness_v1",
    )?;
    let readiness_required = contract
        .get("cloud_readiness_required")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow!("hosted build package contract is missing cloud_readiness_required")
        })?;
    if !readiness_required {
        bail!("hosted build package contract mismatch: cloud_readiness_required must be true");
    }
    Ok(())
}

fn ensure_contract_string(contract: &Value, field: &str, expected: &str) -> Result<()> {
    let actual = contract
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("hosted build package contract is missing {field}"))?;
    if actual != expected {
        bail!("hosted build package contract mismatch: {field} must be {expected}, got {actual}");
    }
    Ok(())
}

fn ensure_build_import_identity(value: &Value) -> Result<()> {
    let Some(import) = value.get("import") else {
        return Ok(());
    };
    if import.is_null() {
        return Ok(());
    }
    let import_status = import
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if import_status != "accepted" {
        return Ok(());
    }
    let build_id = value
        .get("build_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("hosted build response is missing build_id"))?;
    let import_id = import
        .get("import_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("hosted build import is missing import_id"))?;
    if import_id != build_id {
        bail!("hosted build import_id mismatch: build_id {build_id}, import_id {import_id}");
    }
    let build_digest = value.get("package_digest").and_then(Value::as_str);
    let import_digest = import.get("package_digest").and_then(Value::as_str);
    match (build_digest, import_digest) {
        (Some(build_digest), Some(import_digest)) if build_digest != import_digest => {
            bail!(
                "hosted build import package_digest mismatch: build returned {build_digest}, import recorded {import_digest}"
            );
        }
        (Some(build_digest), None) => {
            bail!(
                "hosted build import package_digest mismatch: build returned {build_digest}, import did not record a package digest"
            );
        }
        (None, Some(import_digest)) => {
            bail!(
                "hosted build import package_digest mismatch: build did not return a package digest, import recorded {import_digest}"
            );
        }
        _ => {}
    }
    Ok(())
}

fn ensure_authoring_build_identity(
    value: &Value,
    expected_build_kind: &str,
    expected_upload_id: &str,
    expected_entrypoint: Option<&str>,
) -> Result<()> {
    let authoring_build = value
        .get("authoring_build")
        .ok_or_else(|| anyhow!("hosted build response is missing authoring_build"))?;
    let status = authoring_build
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("hosted build authoring_build is missing status"))?;
    if expected_build_kind == "sealed_package_import" {
        if status != "unavailable" {
            bail!(
                "hosted build authoring_build mismatch: sealed package imports must report authoring_build.status=unavailable, got {status}"
            );
        }
        return Ok(());
    }
    if expected_build_kind != "hosted_authoring_build" {
        bail!("hosted build response kind mismatch: unsupported build_kind {expected_build_kind}");
    }
    match status {
        "failed" => Ok(()),
        "succeeded" => {
            let source_upload_id = authoring_build
                .get("source_upload_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    anyhow!(
                        "hosted authoring build response is missing authoring_build.source_upload_id"
                    )
                })?;
            if source_upload_id != expected_upload_id {
                bail!(
                    "hosted authoring build source upload mismatch: uploaded {expected_upload_id}, authoring build used {source_upload_id}"
                );
            }
            let expected_entrypoint = expected_entrypoint
                .ok_or_else(|| anyhow!("hosted authoring build expected entrypoint is missing"))?;
            let entrypoint = authoring_build
                .get("entrypoint")
                .and_then(Value::as_str)
                .filter(|entrypoint| !entrypoint.trim().is_empty())
                .ok_or_else(|| {
                    anyhow!("hosted authoring build response is missing authoring_build.entrypoint")
                })?;
            if entrypoint != expected_entrypoint {
                bail!(
                    "hosted authoring build entrypoint mismatch: requested {expected_entrypoint}, authoring build used {entrypoint}"
                );
            }
            Ok(())
        }
        _ => bail!(
            "hosted authoring build status mismatch: expected succeeded or failed, got {status}"
        ),
    }
}

fn ensure_doctor_runnable(value: &Value, expected_package_digest: &str) -> Result<()> {
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("(missing)");
    let detail = value
        .get("message")
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("Cloud doctor did not prove this package runnable");
    if !(ok && status == "runnable") {
        bail!("Cloud doctor status={status} ok={ok}: {detail}");
    }
    let doctor_digest = value
        .get("package_digest")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Cloud doctor response is missing package_digest"))?;
    if doctor_digest != expected_package_digest {
        bail!(
            "Cloud doctor package_digest mismatch: requested {expected_package_digest}, doctor checked {doctor_digest}"
        );
    }
    Ok(())
}

fn ensure_run_created(value: &Value, expected_package_digest: &str) -> Result<()> {
    let run_id = value
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|run_id| !run_id.trim().is_empty())
        .ok_or_else(|| anyhow!("Cloud run creation response is missing run_id"))?;
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Cloud run creation response for {run_id} is missing status"))?;
    if !matches!(
        status,
        "created" | "waiting_for_runner" | "running" | "queued"
    ) {
        bail!("Cloud run creation returned non-startable status for {run_id}: {status}");
    }
    let run_digest = value
        .get("package_digest")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("Cloud run creation response for {run_id} is missing package_digest")
        })?;
    if run_digest != expected_package_digest {
        bail!(
            "Cloud run creation package_digest mismatch for {run_id}: requested {expected_package_digest}, run uses {run_digest}"
        );
    }
    Ok(())
}

fn ensure_run_response_matches(value: &Value, expected_run_id: &str) -> Result<()> {
    let run_id = value
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|run_id| !run_id.trim().is_empty())
        .ok_or_else(|| anyhow!("run response is missing run_id"))?;
    if run_id != expected_run_id {
        bail!("run response id mismatch: requested {expected_run_id}, API returned {run_id}");
    }
    Ok(())
}

fn ensure_runtime_response_matches(value: &Value, expected_run_id: &str) -> Result<()> {
    let run_id = value
        .get("cloud_run_id")
        .and_then(Value::as_str)
        .filter(|run_id| !run_id.trim().is_empty())
        .ok_or_else(|| anyhow!("runtime response is missing cloud_run_id"))?;
    if run_id != expected_run_id {
        bail!(
            "runtime response run id mismatch: requested {expected_run_id}, API returned {run_id}"
        );
    }
    Ok(())
}

fn ensure_runtime_value_response_matches(
    value: &Value,
    expected_run_id: &str,
    expected_key: &str,
) -> Result<()> {
    ensure_runtime_response_matches(value, expected_run_id)?;
    let key = value
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("runtime value response is missing key"))?;
    if key != expected_key {
        bail!("runtime value response key mismatch: requested {expected_key}, API returned {key}");
    }
    Ok(())
}

fn ensure_secret_list_response(value: &Value) -> Result<()> {
    let secrets = value
        .get("secrets")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("secret list response is missing secrets array"))?;
    for (index, secret) in secrets.iter().enumerate() {
        ensure_hosted_secret_response(secret, None, &format!("secret list item #{index}"))?;
    }
    Ok(())
}

fn ensure_hosted_secret_response(
    value: &Value,
    expected_name: Option<&str>,
    action: &str,
) -> Result<()> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| anyhow!("{action} response is missing name"))?;
    if let Some(expected_name) = expected_name {
        if name != expected_name {
            bail!(
                "{action} response name mismatch: requested {expected_name}, API returned {name}"
            );
        }
    }
    value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("{action} response for {name} is missing numeric version"))?;
    for field in ["created_at", "updated_at"] {
        value
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("{action} response for {name} is missing {field}"))?;
    }
    Ok(())
}

fn ensure_secret_response_matches(value: &Value, expected_name: &str, action: &str) -> Result<()> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| anyhow!("{action} response is missing name"))?;
    if name != expected_name {
        bail!("{action} response name mismatch: requested {expected_name}, API returned {name}");
    }
    Ok(())
}

fn ensure_secret_delete_confirmed(value: &Value, expected_name: &str) -> Result<()> {
    let deleted = value
        .get("deleted")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow!(
                "secret delete response for {expected_name} is missing deleted=true confirmation"
            )
        })?;
    if !deleted {
        bail!("secret delete response for {expected_name} did not confirm deletion");
    }
    Ok(())
}

fn package_get_object(context: &CliContext, digest: &str) -> Result<Value> {
    let value = cloud_fetch(
        context,
        Method::GET,
        &format!("/v1/packages/{}", encode_path_segment(digest)),
        None,
        None,
    )?;
    if !value.is_object() {
        bail!("package response was not an object");
    }
    ensure_package_response_matches(&value, digest)?;
    Ok(value)
}

fn ensure_package_response_matches(value: &Value, expected_digest: &str) -> Result<()> {
    let actual_digest = value
        .get("package_digest")
        .or_else(|| value.get("digest"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("package response is missing package_digest"))?;
    if actual_digest != expected_digest {
        bail!("package response digest mismatch: requested {expected_digest}, API returned {actual_digest}");
    }
    Ok(())
}

fn cloud_fetch(
    context: &CliContext,
    method: Method,
    path: &str,
    body: Option<Value>,
    raw_body: Option<(Vec<u8>, &str)>,
) -> Result<Value> {
    let mut headers = HeaderMap::new();
    if let Some(token) = context.user_token.as_ref() {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .context("invalid bearer token header")?,
        );
    }

    let url = format!("{}{}", context.api_url, path);
    let mut request = context.client.request(method, url).headers(headers);
    if let Some((bytes, content_type)) = raw_body {
        request = request.header(CONTENT_TYPE, content_type).body(bytes);
    } else if let Some(body) = body {
        request = request
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&body)?);
    }
    let response = request.send()?;
    let status = response.status();
    let text = response.text()?;
    let payload = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| json!({ "message": text }))
    };
    if !status.is_success() {
        let mut message = cloud_error_message(status.as_u16(), &payload);
        if status.as_u16() == 401 {
            message = append_user_auth_hint(context, message);
        }
        bail!("{message}");
    }
    Ok(payload)
}

fn cloud_error_message(status: u16, payload: &Value) -> String {
    let mut lines = vec![payload
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("Cloud API request failed: {status}"))];
    if let Some(code) = payload.get("code").and_then(Value::as_str) {
        lines.push(format!("code: {code}"));
    }
    let detail = payload.get("detail").unwrap_or(&Value::Null);
    if let Some(commands) = detail.get("next_commands").and_then(Value::as_array) {
        for command in commands {
            if let Some(command) = command.as_str().filter(|value| !value.trim().is_empty()) {
                lines.push(format!("next: {}", command.trim()));
            }
        }
    }
    if let Some(next) = detail.get("next").and_then(Value::as_str) {
        if !next.trim().is_empty() {
            lines.push(format!("next: {}", next.trim()));
        }
    }
    if let Some(actions) = detail.get("required_actions").and_then(Value::as_array) {
        let formatted = actions
            .iter()
            .filter_map(format_cloud_action)
            .collect::<Vec<_>>();
        if !formatted.is_empty() {
            lines.push("actions:".to_string());
            lines.extend(formatted);
        }
    }
    lines.dedup();
    lines.join("\n")
}

fn append_user_auth_hint(context: &CliContext, message: String) -> String {
    let token_path = lab_runner::bucephalus_home()
        .ok()
        .map(|home| cloud_token_paths(&home).access);
    cloud_auth_ux::user_auth_hint(
        &message,
        context.user_token.is_some(),
        token_path.as_deref(),
    )
}

const CLOUD_RUNTIME_OPTION_KEYS: &[&str] = &[
    "backend",
    "executor",
    "arch",
    "cpu_count",
    "cpu",
    "memory_mb",
    "disk_mb",
    "isolation",
    "timeout_ms",
    "max_parallel_trials",
    "network",
    "sidecars",
    "accelerators",
];

fn runtime_options_from_args(args: &[String]) -> Result<Map<String, Value>> {
    let mut runtime_options = Map::new();
    if let Some(value) = option_value(args, "--backend")? {
        insert_runtime_option(
            &mut runtime_options,
            "backend",
            parse_cloud_runtime_option("backend", &value)?,
        )?;
    }
    if let Some(value) = option_value(args, "--arch")? {
        insert_runtime_option(
            &mut runtime_options,
            "arch",
            parse_cloud_runtime_option("arch", &value)?,
        )?;
    }
    if let Some(value) = option_value(args, "--isolation")? {
        insert_runtime_option(
            &mut runtime_options,
            "isolation",
            parse_cloud_runtime_option("isolation", &value)?,
        )?;
    }
    if let Some(value) = number_option(args, "--cpu-count")? {
        insert_runtime_option(&mut runtime_options, "cpu_count", json!(value))?;
    }
    if let Some(value) = number_option(args, "--memory-mb")? {
        insert_runtime_option(&mut runtime_options, "memory_mb", json!(value))?;
    }
    if let Some(value) = number_option(args, "--disk-mb")? {
        insert_runtime_option(&mut runtime_options, "disk_mb", json!(value))?;
    }
    if let Some(value) = number_option(args, "--timeout-ms")? {
        insert_runtime_option(&mut runtime_options, "timeout_ms", json!(value))?;
    }
    if let Some(value) = number_option(args, "--max-parallel-trials")? {
        insert_runtime_option(&mut runtime_options, "max_parallel_trials", json!(value))?;
    }
    for (key, value) in key_value_option_entries(args, "--runtime-option")? {
        let parsed = parse_cloud_runtime_option(&key, &value)?;
        insert_runtime_option(&mut runtime_options, &key, parsed)?;
    }
    Ok(runtime_options)
}

fn parse_cloud_runtime_option(key: &str, value: &str) -> Result<Value> {
    match key {
        "backend" | "executor" | "arch" | "isolation" => {
            if value.trim().is_empty() {
                bail!("runtime option `{key}` requires a non-empty string");
            }
            let value = value.trim();
            validate_cloud_runtime_string_option(key, value)?;
            Ok(json!(value))
        }
        "cpu_count" | "cpu" | "memory_mb" | "disk_mb" | "timeout_ms" | "max_parallel_trials" => {
            let parsed = value.trim().parse::<u64>().with_context(|| {
                format!("--runtime-option {key}=VALUE requires a positive integer")
            })?;
            if parsed == 0 {
                bail!("--runtime-option {key}=VALUE requires a positive integer");
            }
            Ok(json!(parsed))
        }
        "sidecars" | "accelerators" => {
            let items = value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| json!(item))
                .collect::<Vec<_>>();
            if items.is_empty() {
                bail!("--runtime-option {key}=VALUE requires a comma-separated list");
            }
            Ok(Value::Array(items))
        }
        "network" => {
            let parsed: Value = serde_json::from_str(value)
                .with_context(|| "--runtime-option network=VALUE requires a JSON object")?;
            if !parsed.is_object() {
                bail!("--runtime-option network=VALUE requires a JSON object");
            }
            validate_cloud_runtime_network_option(&parsed)?;
            Ok(parsed)
        }
        _ => bail!(
            "unsupported hosted Cloud runtime option `{key}`. Supported keys: {}",
            CLOUD_RUNTIME_OPTION_KEYS.join(", ")
        ),
    }
}

fn insert_runtime_option(
    runtime_options: &mut Map<String, Value>,
    key: &str,
    value: Value,
) -> Result<()> {
    if runtime_options.contains_key(key) {
        bail!("runtime option `{key}` was provided more than once");
    }
    if (key == "backend" && runtime_options.contains_key("executor"))
        || (key == "executor" && runtime_options.contains_key("backend"))
    {
        bail!("runtime options `backend` and `executor` cannot both be provided; use `backend`");
    }
    if (key == "cpu_count" && runtime_options.contains_key("cpu"))
        || (key == "cpu" && runtime_options.contains_key("cpu_count"))
    {
        bail!("runtime options `cpu_count` and `cpu` cannot both be provided; use `cpu_count`");
    }
    runtime_options.insert(key.to_string(), value);
    Ok(())
}

fn validate_cloud_runtime_string_option(key: &str, value: &str) -> Result<()> {
    match key {
        "backend" | "executor" => {
            if ![
                "runner-docker",
                "runner_docker",
                "local-docker",
                "local_docker",
                "modal",
            ]
            .contains(&value)
            {
                bail!("runtime option `{key}` must be one of runner-docker, runner_docker, local-docker, local_docker, modal");
            }
        }
        "arch" => {
            if !["x86_64", "amd64", "arm64", "aarch64"].contains(&value) {
                bail!("runtime option `arch` must be one of x86_64, amd64, arm64, aarch64");
            }
        }
        "isolation" => {
            if !["reusable_vm", "single_use_vm"].contains(&value) {
                bail!("runtime option `isolation` must be reusable_vm or single_use_vm");
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_cloud_runtime_network_option(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("--runtime-option network=VALUE requires a JSON object"))?;
    for (key, item) in object {
        match key.as_str() {
            "default" | "task_sandbox" | "agent" => {
                let mode = item.as_str().ok_or_else(|| {
                    anyhow!("--runtime-option network=VALUE field `{key}` must be a string")
                })?;
                if mode != "none" && mode != "allowlist_enforced" {
                    bail!("--runtime-option network=VALUE field `{key}` must be none or allowlist_enforced");
                }
            }
            "egress" => {
                let hosts = item.as_array().ok_or_else(|| {
                    anyhow!("--runtime-option network=VALUE field `egress` must be an array")
                })?;
                for host in hosts {
                    if host
                        .as_str()
                        .map(str::trim)
                        .filter(|host| !host.is_empty())
                        .is_none()
                    {
                        bail!("--runtime-option network=VALUE field `egress` entries must be non-empty strings");
                    }
                }
            }
            _ => bail!("--runtime-option network=VALUE contains unsupported field `{key}`"),
        }
    }
    Ok(())
}

fn secret_refs_from_options(args: &[String]) -> Result<BTreeMap<String, String>> {
    let mut refs = BTreeMap::new();
    merge_key_value_options(&mut refs, args, "--secret-ref", "secret ref")?;
    merge_key_value_options(&mut refs, args, "--secret", "secret ref")?;
    let secret_ref_file = option_value(args, "--secret-ref-file")?;
    let secrets_file = option_value(args, "--secrets-file")?;
    if secret_ref_file.is_some() && secrets_file.is_some() {
        bail!("provide either --secret-ref-file or --secrets-file, not both");
    }
    let file = secret_ref_file.or(secrets_file);
    if let Some(file) = file {
        let mut from_file = read_secret_ref_file(Path::new(&file))?;
        for (key, value) in refs {
            insert_unique_key_value(&mut from_file, key, value, "secret ref")?;
        }
        Ok(from_file)
    } else {
        Ok(refs)
    }
}

const RESERVED_CLOUD_RUN_ENV_NAMES: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "BUCEPHALUS_CLOUD_ALLOW_CONTROL_PLANE_SECRET_REFS",
    "BUCEPHALUS_CLOUD_ALLOW_LOCAL_IMAGE_REFS",
    "BUCEPHALUS_CLOUD_API_URL",
    "BUCEPHALUS_CLOUD_WORKER_TOKEN",
    "BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON",
    "BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON",
    "BUCEPHALUS_RUN_STORE",
    "BUCEPHALUS_RUN_STORE_SCHEMA",
    "BUCEPHALUS_RUN_STORE_URL",
    "BUCEPHALUS_RUNNER_INSTANCE_ID",
    "BUCEPHALUS_RUNNER_POOL_ID",
    "BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID",
    "BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID",
    "BUCEPHALUS_SECRET_RESOLVER_ALLOW_CONTROL_PLANE_REFS",
    "BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV",
    "BUCEPHALUS_SECRET_RESOLVER_AWS_CMD",
    "BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD",
    "BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH",
    "BUCEPHALUS_WORKER_DATABASE_URL",
    "BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON",
    "DATABASE_URL",
    "GOOGLE_APPLICATION_CREDENTIALS",
];

fn run_env_from_options(
    args: &[String],
    secret_refs: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let env = key_value_options(args, "--env")?;
    for (key, value) in &env {
        validate_cloud_run_env_key(key)?;
        if value.contains('\0') {
            bail!("--env {key}=VALUE must not contain NUL bytes");
        }
        if secret_refs.contains_key(key) {
            bail!("--env key `{key}` cannot also be supplied as a secret ref; use --env for plain config or --secret-ref for secrets");
        }
    }
    Ok(env)
}

fn validate_cloud_run_env_key(key: &str) -> Result<()> {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        bail!("--env requires a non-empty environment variable name");
    };
    if !(first == '_' || first.is_ascii_uppercase())
        || !chars.all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
    {
        bail!("--env key `{key}` must be an uppercase shell identifier matching [A-Z_][A-Z0-9_]*");
    }
    if RESERVED_CLOUD_RUN_ENV_NAMES.contains(&key) {
        bail!("--env key `{key}` is reserved for Cloud runtime/control-plane state");
    }
    Ok(())
}

fn merge_key_value_options(
    out: &mut BTreeMap<String, String>,
    args: &[String],
    option: &str,
    noun: &str,
) -> Result<()> {
    for (key, value) in key_value_option_entries(args, option)? {
        insert_unique_key_value(out, key, value, noun)?;
    }
    Ok(())
}

fn insert_unique_key_value(
    out: &mut BTreeMap<String, String>,
    key: String,
    value: String,
    noun: &str,
) -> Result<()> {
    if out.contains_key(&key) {
        bail!("{noun} `{key}` was provided more than once");
    }
    out.insert(key, value);
    Ok(())
}

fn read_secret_ref_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed: Value = if path.extension().and_then(|value| value.to_str()) == Some("json") {
        serde_json::from_str(&raw)?
    } else {
        serde_json::to_value(serde_yaml::from_str::<serde_yaml::Value>(&raw)?)?
    };
    let object = parsed.as_object().ok_or_else(|| {
        anyhow!(
            "secret ref file must be a map of NAME: bucephalus://NAME or provider ref, got {}",
            path.display()
        )
    })?;
    let mut refs = BTreeMap::new();
    for (key, value) in object {
        let value = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "secret ref file entry {key} must be a non-empty hosted/provider ref string"
                )
            })?;
        refs.insert(key.clone(), value.to_string());
    }
    Ok(refs)
}

fn secret_name_arg(args: &[String]) -> Result<String> {
    single_positional_arg(args, "secret name").and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            bail!("secret name is required");
        }
        Ok(value)
    })
}

fn secret_value_from_args(args: &[String]) -> Result<String> {
    let value_file = option_value(args, "--value-file")?;
    let from_env = option_value(args, "--from-env")?;
    let inline_value = option_value(args, "--value")?;
    let from_stdin = args.iter().any(|arg| arg == "--stdin");
    let source_count = value_file.is_some() as usize
        + from_env.is_some() as usize
        + inline_value.is_some() as usize
        + from_stdin as usize;
    if source_count == 0 {
        bail!(
            "secret value source is required: use --value-file PATH, --from-env ENV, or --stdin. Avoid --value except in automation where command history is controlled."
        );
    }
    if source_count > 1 {
        bail!(
            "choose exactly one secret value source: --value-file, --from-env, --stdin, or --value"
        );
    }
    if let Some(path) = value_file {
        return fs::read_to_string(&path)
            .with_context(|| format!("failed to read secret value file {path}"));
    }
    if let Some(name) = from_env {
        if name.trim().is_empty() {
            bail!("--from-env requires a non-empty environment variable name");
        }
        return std::env::var(&name)
            .with_context(|| format!("environment variable {name} is not set"));
    }
    if from_stdin {
        let mut value = String::new();
        std::io::stdin()
            .read_to_string(&mut value)
            .context("failed to read secret value from stdin")?;
        return Ok(value);
    }
    Ok(inline_value.unwrap_or_default())
}

fn secret_requirements_from_value(value: &Value) -> Vec<SecretRequirement> {
    let mut requirements = value
        .get("secret_requirements")
        .and_then(Value::as_array)
        .or_else(|| {
            value
                .get("package")
                .and_then(|p| p.get("secret_requirements"))
                .and_then(Value::as_array)
        })
        .or_else(|| {
            value
                .get("cloud_readiness")
                .and_then(|p| p.get("secret_requirements"))
                .and_then(Value::as_array)
        })
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(SecretRequirement {
                id: item.get("id")?.as_str()?.to_string(),
                target: item
                    .get("target")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                required_for_variants: item
                    .get("required_for_variants")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .filter(|value| !value.is_empty())
                    .collect(),
            })
        })
        .filter(|item| !item.id.is_empty())
        .collect::<Vec<_>>();
    requirements.sort_by(|left, right| left.id.cmp(&right.id));
    requirements
}

fn secret_setup_lines(requirements: &[SecretRequirement]) -> Vec<String> {
    requirements
        .iter()
        .map(|requirement| {
            format!(
                "next: buc secrets put {} --from-env {}",
                requirement.id, requirement.id
            )
        })
        .collect()
}

fn hosted_secret_ref_args(requirements: &[SecretRequirement]) -> String {
    if requirements.is_empty() {
        return String::new();
    }
    requirements
        .iter()
        .map(|requirement| {
            format!(
                " --secret-ref {}=bucephalus://{}",
                requirement.id, requirement.id
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn next_doctor_command(package_digest: &str, requirements: &[SecretRequirement]) -> String {
    format!(
        "next: buc doctor {package_digest}{}",
        hosted_secret_ref_args(requirements)
    )
}

fn next_run_command(package_digest: &str, requirements: &[SecretRequirement]) -> String {
    format!(
        "next: buc run {package_digest}{}",
        hosted_secret_ref_args(requirements)
    )
}

fn required_option(args: &[String], name: &str) -> Result<String> {
    option_value(args, name)?.ok_or_else(|| anyhow!("{name} is required"))
}

fn single_positional_arg(args: &[String], noun: &str) -> Result<String> {
    let positionals = positional_args(args);
    match positionals.as_slice() {
        [value] => Ok(value.clone()),
        [] => Err(anyhow!("{noun} is required")),
        _ => bail!("{noun} accepts exactly one positional argument"),
    }
}

fn single_positional_or_option(args: &[String], option: &str, noun: &str) -> Result<String> {
    let positionals = positional_args(args);
    let option_value = option_value(args, option)?;
    match (positionals.as_slice(), option_value) {
        ([value], None) => Ok(value.clone()),
        ([], Some(value)) => Ok(value),
        ([], None) => Err(anyhow!("{noun} is required")),
        (_, Some(_)) => {
            bail!("{noun} must be provided either positionally or with {option}, not both")
        }
        _ => bail!("{noun} accepts exactly one positional argument"),
    }
}

fn reject_no_positionals(args: &[String], command: &str) -> Result<()> {
    let positionals = positional_args(args);
    if positionals.is_empty() {
        Ok(())
    } else {
        bail!(
            "{command} does not accept positional arguments: {}",
            positionals.join(" ")
        );
    }
}

fn run_value_args(args: &[String]) -> Result<(String, String)> {
    let positionals = positional_args(args);
    let run_id = option_value(args, "--run-id")?;
    let key = option_value(args, "--key")?;
    match (positionals.as_slice(), run_id, key) {
        ([run_id, key], None, None) => Ok((run_id.clone(), key.clone())),
        ([], Some(run_id), Some(key)) => Ok((run_id, key)),
        ([], None, _) => Err(anyhow!("run id is required")),
        ([], _, None) => Err(anyhow!("runtime key is required")),
        (_, Some(_), _) | (_, _, Some(_)) => bail!(
            "runtime value lookup must be provided either as positional arguments `<run-id> <key>` or with --run-id and --key, not both"
        ),
        _ => bail!("runtime value lookup requires exactly two positional arguments: <run-id> <key>"),
    }
}

fn build_value_options() -> [&'static str; 12] {
    [
        "--file",
        "--context-root",
        "--label",
        "--backend",
        "--arch",
        "--isolation",
        "--cpu-count",
        "--memory-mb",
        "--disk-mb",
        "--timeout-ms",
        "--max-parallel-trials",
        "--runtime-option",
    ]
}

fn doctor_value_options() -> [&'static str; 14] {
    [
        "--package-digest",
        "--secret-ref",
        "--secret",
        "--secret-ref-file",
        "--secrets-file",
        "--backend",
        "--arch",
        "--isolation",
        "--cpu-count",
        "--memory-mb",
        "--disk-mb",
        "--timeout-ms",
        "--max-parallel-trials",
        "--runtime-option",
    ]
}

fn run_create_value_options() -> [&'static str; 16] {
    [
        "--package-digest",
        "--secret-ref",
        "--secret",
        "--secret-ref-file",
        "--secrets-file",
        "--env",
        "--label",
        "--backend",
        "--arch",
        "--isolation",
        "--cpu-count",
        "--memory-mb",
        "--disk-mb",
        "--timeout-ms",
        "--max-parallel-trials",
        "--runtime-option",
    ]
}

fn reject_unknown_options(
    args: &[String],
    value_options: &[&str],
    boolean_options: &[&str],
) -> Result<()> {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if !arg.starts_with("--") {
            index += 1;
            continue;
        }
        if arg.contains('=') {
            bail!(
                "{arg} is not supported; use `{} VALUE`",
                arg.split('=').next().unwrap_or(arg)
            );
        }
        if value_options.contains(&arg.as_str()) {
            let value = args
                .get(index + 1)
                .ok_or_else(|| anyhow!("{arg} requires a value"))?;
            if value.starts_with("--") {
                bail!("{arg} requires a value, got option {value}");
            }
            index += 2;
            continue;
        }
        if boolean_options.contains(&arg.as_str()) {
            index += 1;
            continue;
        }
        bail!("unknown option {arg}. Run `buc help` for supported Cloud product commands.");
    }
    Ok(())
}

fn option_value(args: &[String], name: &str) -> Result<Option<String>> {
    let matches = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == name).then_some(index))
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        bail!("{name} can only be provided once");
    }
    if let Some(index) = matches.first().copied() {
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{name} requires a value"))?;
        if value.starts_with("--") {
            bail!("{name} requires a value, got option {value}");
        }
        Ok(Some(value.clone()))
    } else {
        Ok(None)
    }
}

fn key_value_options(args: &[String], name: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (key, value) in key_value_option_entries(args, name)? {
        if out.contains_key(&key) {
            bail!("{name} key `{key}` was provided more than once");
        }
        out.insert(key, value);
    }
    Ok(out)
}

fn key_value_option_entries(args: &[String], name: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] != name {
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{name} requires KEY=VALUE"))?;
        if value.starts_with("--") {
            bail!("{name} requires KEY=VALUE, got option {value}");
        }
        let Some((key, value)) = value.split_once('=') else {
            bail!("{name} requires KEY=VALUE");
        };
        if key.trim().is_empty() {
            bail!("{name} requires KEY=VALUE");
        }
        out.push((key.trim().to_string(), value.trim().to_string()));
        index += 2;
    }
    Ok(out)
}

fn number_option(args: &[String], name: &str) -> Result<Option<u64>> {
    let Some(value) = option_value(args, name)? else {
        return Ok(None);
    };
    let parsed = value
        .parse::<u64>()
        .with_context(|| format!("{name} requires a positive integer"))?;
    if parsed == 0 {
        bail!("{name} requires a positive integer");
    }
    Ok(Some(parsed))
}

fn positional_args(args: &[String]) -> Vec<String> {
    let options_with_values = [
        "--api-url",
        "--user-token",
        "--label",
        "--file",
        "--context-root",
        "--package-digest",
        "--run-id",
        "--secret-ref-file",
        "--secrets-file",
        "--backend",
        "--arch",
        "--cpu-count",
        "--memory-mb",
        "--disk-mb",
        "--isolation",
        "--timeout-ms",
        "--max-parallel-trials",
        "--env",
        "--secret-ref",
        "--secret",
        "--runtime-option",
        "--validation-level",
        "--target",
        "--q",
        "--limit",
        "--after-row-seq",
        "--format",
        "--key",
        "--value-file",
        "--from-env",
        "--value",
    ];
    let mut out = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg.starts_with("--") {
            if options_with_values.contains(&arg.as_str()) {
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        out.push(arg.clone());
        index += 1;
    }
    out
}

fn insert_option_string(object: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        object.insert(key.to_string(), json!(value));
    }
}

fn json_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--json")
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_import_summary(value: &Value, source: Option<&str>) -> Result<()> {
    let mut lines = Vec::new();
    if let Some(source) = source {
        lines.push(format!("source: {source}"));
    }
    lines.push(format!(
        "import_id: {}",
        value
            .get("import_id")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)")
    ));
    lines.push(format!(
        "status: {}",
        value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)")
    ));
    if let Some(package_digest) = value.get("package_digest").and_then(Value::as_str) {
        lines.push(format!("package_digest: {package_digest}"));
        lines.push(format!("next: buc inspect {package_digest}"));
        lines.push(format!("next: buc doctor {package_digest}"));
    }
    if let Some(error_message) = value.get("error_message").and_then(Value::as_str) {
        lines.push(format!("error: {error_message}"));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_build_summary(value: &Value, source: &str) -> Result<()> {
    println!("{}", build_summary_lines(value, source)?.join("\n"));
    Ok(())
}

fn build_summary_lines(value: &Value, source: &str) -> Result<Vec<String>> {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let mut lines = vec![
        format!("source: {source}"),
        format!(
            "build_id: {}",
            value
                .get("build_id")
                .and_then(Value::as_str)
                .unwrap_or("(unknown)")
        ),
        format!("status: {status}"),
    ];
    if let Some(kind) = value.get("build_kind").and_then(Value::as_str) {
        lines.push(format!("build_kind: {kind}"));
    }
    if let Some(environment) = value.get("build_environment") {
        lines.extend(build_environment_summary_lines(environment));
    }
    if let Some(package_digest) = value.get("package_digest").and_then(Value::as_str) {
        lines.push(format!("package_digest: {package_digest}"));
        if status == "accepted" || status == "cloud_runnable" {
            let requirements = secret_requirements_from_value(value);
            let secret_actions = required_action_command_lines(value, Some("upload_hosted_secret"));
            if secret_actions.is_empty() {
                lines.extend(secret_setup_lines(&requirements));
            } else {
                lines.extend(secret_actions);
            }
            lines.push(next_doctor_command(package_digest, &requirements));
            if status == "cloud_runnable" {
                lines.push(next_run_command(package_digest, &requirements));
            }
        }
    }
    if let Some(readiness) = value.get("cloud_readiness") {
        lines.extend(cloud_readiness_summary_lines(readiness));
    }
    lines.extend(authoring_build_summary_lines(value));
    let import = value.get("import").unwrap_or(value);
    if let Some(error_message) = import.get("error_message").and_then(Value::as_str) {
        lines.push(format!("error: {error_message}"));
    }
    if let Some(diagnostics) = import.get("diagnostics").and_then(Value::as_array) {
        let diagnostics = diagnostics
            .iter()
            .filter_map(format_import_diagnostic)
            .collect::<Vec<_>>();
        if !diagnostics.is_empty() {
            lines.push("diagnostics:".to_string());
            lines.extend(diagnostics);
        }
    }
    if status == "failed" {
        lines.push(
            "next: fix the authoring/package diagnostics, then rerun `buc build <same-input>`."
                .to_string(),
        );
    } else if status == "cloud_blocked" {
        lines.push(
            "next: fix the blocked Cloud checks above, then rerun `buc build <same-input>`."
                .to_string(),
        );
    }
    Ok(lines)
}

fn build_environment_summary_lines(environment: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(target) = environment.get("target") {
        let kind = target
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("hosted_cloud");
        let name = target
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("default");
        lines.push(format!("build_target: {kind}/{name}"));
    }
    if let Some(source) = environment.get("source") {
        let input_kind = source
            .get("input_kind")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        if let Some(upload_id) = source.get("upload_id").and_then(Value::as_str) {
            lines.push(format!("build_source: {input_kind} upload={upload_id}"));
        } else {
            lines.push(format!("build_source: {input_kind}"));
        }
        if let Some(content_digest) = source.get("content_digest").and_then(Value::as_str) {
            lines.push(format!("build_source_digest: {content_digest}"));
        }
        if let Some(byte_size) = source.get("byte_size").and_then(Value::as_u64) {
            lines.push(format!("build_source_bytes: {byte_size}"));
        }
        if let Some(entrypoint) = source.get("entrypoint").and_then(Value::as_str) {
            lines.push(format!("build_source_entrypoint: {entrypoint}"));
        }
    }
    if let Some(runtime_options) = environment.get("runtime_options") {
        if runtime_options.is_object()
            && runtime_options
                .as_object()
                .is_some_and(|object| !object.is_empty())
        {
            lines.push(format!(
                "build_runtime_options: {}",
                compact_json_lossy(runtime_options)
            ));
        }
    }
    if let Some(contract) = environment.get("package_contract") {
        if let Some(input_kind) = contract.get("input_kind").and_then(Value::as_str) {
            lines.push(format!("build_input_kind: {input_kind}"));
        }
        if let Some(compiler) = contract.get("authoring_compiler").and_then(Value::as_str) {
            lines.push(format!("authoring_compiler: {compiler}"));
        }
        if let Some(schema) = contract
            .get("sealed_schema_version")
            .and_then(Value::as_str)
        {
            lines.push(format!("package_contract: {schema}"));
        }
        if let Some(required) = contract
            .get("cloud_readiness_required")
            .and_then(Value::as_bool)
        {
            lines.push(format!("cloud_readiness_required: {required}"));
        }
    }
    if let Some(core) = environment.get("core") {
        let command = core
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("bucephalus build");
        let version = core
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        lines.push(format!("builder_core: {command} version={version}"));
        if let Some(timeout_ms) = core.get("timeout_ms").and_then(Value::as_u64) {
            lines.push(format!("builder_timeout_ms: {timeout_ms}"));
        }
    }
    if let Some(builder) = environment.get("builder") {
        if let Some(image_digest) = builder.get("image_digest").and_then(Value::as_str) {
            lines.push(format!("builder_image_digest: {image_digest}"));
        }
        if let Some(release_version) = builder.get("release_version").and_then(Value::as_str) {
            lines.push(format!("builder_release_version: {release_version}"));
        }
    }
    if let Some(evidence) = environment.get("evidence") {
        if let Some(policy) = evidence.get("policy").and_then(Value::as_str) {
            lines.push(format!("build_environment_evidence_policy: {policy}"));
        }
        if let Some(status) = evidence.get("status").and_then(Value::as_str) {
            lines.push(format!("build_environment_evidence: {status}"));
        }
        let missing = evidence
            .get("missing")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            lines.push(format!("missing_build_evidence: {}", missing.join(", ")));
        }
    }
    lines
}

fn authoring_build_summary_lines(value: &Value) -> Vec<String> {
    let Some(authoring_build) = value.get("authoring_build") else {
        return Vec::new();
    };
    let status = authoring_build
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    if status == "unavailable" {
        return Vec::new();
    }
    let mut lines = vec![format!("authoring_build: {status}")];
    if let Some(code) = authoring_build.get("code").and_then(Value::as_str) {
        lines.push(format!("authoring_code: {code}"));
    }
    if let Some(error) = authoring_build.get("error").and_then(Value::as_str) {
        lines.push(format!("authoring_error: {error}"));
    }
    if let Some(detail) = authoring_build.get("detail") {
        if let Some(exit_code) = detail.get("exit_code").and_then(Value::as_i64) {
            lines.push(format!("authoring_exit_code: {exit_code}"));
        }
        if let Some(timeout_ms) = detail.get("timeout_ms").and_then(Value::as_u64) {
            lines.push(format!("authoring_timeout_ms: {timeout_ms}"));
        }
        if let Some(stderr) = non_empty_json_string(detail, "stderr_tail") {
            lines.push(format!("authoring_stderr_tail:\n{stderr}"));
        }
        if let Some(stdout) = non_empty_json_string(detail, "stdout_tail") {
            lines.push(format!("authoring_stdout_tail:\n{stdout}"));
        }
    }
    lines
}

fn authoring_build_failure_detail(value: &Value) -> Option<String> {
    let authoring_build = value.get("authoring_build")?;
    if authoring_build.get("status").and_then(Value::as_str) != Some("failed") {
        return None;
    }
    let code = authoring_build.get("code").and_then(Value::as_str);
    let error = authoring_build
        .get("error")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty());
    let mut parts = Vec::new();
    if let Some(code) = code {
        parts.push(format!("authoring build failed [{code}]"));
    } else {
        parts.push("authoring build failed".to_string());
    }
    if let Some(error) = error {
        parts.push(error.to_string());
    }
    if let Some(detail) = authoring_build.get("detail") {
        if let Some(stderr) = non_empty_json_string(detail, "stderr_tail") {
            parts.push(format!("stderr: {stderr}"));
        } else if let Some(stdout) = non_empty_json_string(detail, "stdout_tail") {
            parts.push(format!("stdout: {stdout}"));
        }
    }
    Some(parts.join(": "))
}

fn non_empty_json_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
}

fn cloud_readiness_summary_lines(readiness: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let status = readiness
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    lines.push(format!("cloud_readiness: {status}"));
    if let Some(requirements) = readiness.get("run_requirements") {
        if !requirements.is_null() {
            lines.push(format!(
                "cloud_run_requirements: {}",
                compact_json_lossy(requirements)
            ));
        }
    }
    let checks = readiness
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(format_cloud_check)
        .collect::<Vec<_>>();
    if !checks.is_empty() {
        lines.push("cloud_checks:".to_string());
        lines.extend(checks);
    }
    let actions = readiness
        .get("required_actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|action| {
            action.get("action").and_then(Value::as_str) != Some("upload_hosted_secret")
        })
        .filter_map(format_cloud_action)
        .collect::<Vec<_>>();
    if !actions.is_empty() {
        lines.push("cloud_actions:".to_string());
        lines.extend(actions);
    }
    lines
}

fn required_action_command_lines(value: &Value, action_filter: Option<&str>) -> Vec<String> {
    value
        .get("cloud_readiness")
        .and_then(|readiness| readiness.get("required_actions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|action| {
            action_filter
                .map(|expected| action.get("action").and_then(Value::as_str) == Some(expected))
                .unwrap_or(true)
        })
        .filter_map(|action| action.get("command").and_then(Value::as_str))
        .filter(|command| !command.trim().is_empty())
        .map(|command| format!("next: {}", command.trim()))
        .collect()
}

fn format_cloud_action(value: &Value) -> Option<String> {
    let stage = value
        .get("stage")
        .and_then(Value::as_str)
        .unwrap_or("before_run");
    let action = value
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("cloud_action");
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.trim().is_empty())
        .map(|command| format!(" command=`{}`", command.trim()))
        .unwrap_or_default();
    if description.is_empty() && command.is_empty() {
        None
    } else {
        Some(format!("  - [{stage}] {action}: {description}{command}"))
    }
}

fn format_cloud_check(value: &Value) -> Option<String> {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("blocked");
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("cloud_check");
    let name = value.get("name").and_then(Value::as_str).unwrap_or("cloud");
    let message = value.get("message").and_then(Value::as_str).unwrap_or("");
    if message.is_empty() {
        None
    } else {
        Some(format!("  - [{status}] {name}/{code}: {message}"))
    }
}

fn format_import_diagnostic(value: &Value) -> Option<String> {
    let severity = value
        .get("severity")
        .and_then(Value::as_str)
        .unwrap_or("error");
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("diagnostic");
    let pointer = value.get("pointer").and_then(Value::as_str).unwrap_or("/");
    let message = value.get("message").and_then(Value::as_str).unwrap_or("");
    if message.is_empty() {
        None
    } else {
        Some(format!("  - [{severity}] {code} {pointer}: {message}"))
    }
}

fn print_package_summary(value: &Value) -> Result<()> {
    let digest = value
        .get("package_digest")
        .or_else(|| value.get("digest"))
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("(unnamed)");
    let requirements = secret_requirements_from_value(value);
    let mut lines = vec![
        format!("package_digest: {digest}"),
        format!("status: {status}"),
        format!("name: {name}"),
    ];
    if requirements.is_empty() {
        lines.push("secret_requirements: none".to_string());
    } else {
        lines.push("secret_requirements:".to_string());
        for requirement in &requirements {
            let target = if requirement.target.is_empty() {
                "(runtime env)".to_string()
            } else {
                requirement.target.clone()
            };
            let variants = if requirement.required_for_variants.is_empty() {
                String::new()
            } else {
                format!(" variants={}", requirement.required_for_variants.join(","))
            };
            lines.push(format!("  - {} -> {}{}", requirement.id, target, variants));
        }
    }
    lines.extend(secret_setup_lines(&requirements));
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_package_list_summary(value: &Value) -> Result<()> {
    let packages = value
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("packages: {}", packages.len())];
    for package in packages {
        let digest = package
            .get("package_digest")
            .or_else(|| package.get("digest"))
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        let status = package
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        let name = package
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)");
        lines.push(format!("  - {digest} [{status}] {name}"));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_doctor_summary(value: &Value) -> Result<()> {
    let digest = value
        .get("package_digest")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let mut lines = vec![
        format!(
            "status: {}",
            value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("(unknown)")
        ),
        format!("package_digest: {digest}"),
        format!(
            "package_status: {}",
            value
                .get("package_status")
                .and_then(Value::as_str)
                .unwrap_or("(unknown)")
        ),
    ];
    if let Some(requirements) = value.get("run_requirements") {
        lines.push(format!("run_requirements: {}", compact_json(requirements)?));
    }
    let requirements = secret_requirements_from_value(value);
    lines.extend(secret_setup_lines(&requirements));
    lines.push(next_run_command(digest, &requirements));
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_run_summary(value: &Value) -> Result<()> {
    let run_id = value
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    println!("run_id: {run_id}\nstatus: {status}\nnext: buc runs get {run_id}");
    Ok(())
}

fn print_run_list_summary(value: &Value) -> Result<()> {
    let runs = value
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("runs: {}", runs.len())];
    for run in runs {
        let run_id = run
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        let status = run
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        let digest = run
            .get("package_digest")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        let label = run
            .get("run_label")
            .or_else(|| run.get("label"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if label.is_empty() {
            lines.push(format!("  - {run_id} [{status}] {digest}"));
        } else {
            lines.push(format!("  - {run_id} [{status}] {digest} label={label}"));
        }
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_secret_summary(value: &Value, verb: &str) -> Result<()> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "(unknown)".to_string());
    println!(
        "secret: {name}\nstatus: {verb}\nversion: {version}\nref: bucephalus://{name}\nnext: buc run <package-digest> --secret-ref {name}=bucephalus://{name}"
    );
    Ok(())
}

fn print_secret_list_summary(value: &Value) -> Result<()> {
    let secrets = value
        .get("secrets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("secrets: {}", secrets.len())];
    for secret in secrets {
        let name = secret
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        let version = secret
            .get("version")
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(unknown)".to_string());
        lines.push(format!(
            "  - {name} version={version} ref=bucephalus://{name}"
        ));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_secret_delete_summary(value: &Value, fallback_name: &str) -> Result<()> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(fallback_name);
    let deleted = value
        .get("deleted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    println!("secret: {name}\ndeleted: {deleted}");
    Ok(())
}

fn print_runtime_summary(value: &Value) -> Result<()> {
    if let Some(summary) = value.get("summary") {
        println!("summary: {}", compact_json(summary)?);
    } else {
        println!("runtime: {}", compact_json(value)?);
    }
    Ok(())
}

fn print_runtime_events_summary(value: &Value) -> Result<()> {
    let events = value
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("events: {}", events.len())];
    for event in events.iter().take(20) {
        lines.push(format!("  - {}", compact_json_lossy(event)));
    }
    if events.len() > 20 {
        lines.push(format!(
            "  ... {} more; rerun with --json for full output",
            events.len() - 20
        ));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_runtime_results_summary(value: &Value) -> Result<()> {
    let rows = value
        .get("results")
        .or_else(|| value.get("trial_results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("results: {}", rows.len())];
    for row in rows.iter().take(20) {
        lines.push(format!("  - {}", compact_json_lossy(row)));
    }
    if rows.len() > 20 {
        lines.push(format!(
            "  ... {} more; rerun with --json for full output",
            rows.len() - 20
        ));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_runtime_value_summary(value: &Value) -> Result<()> {
    if let Some(key) = value.get("key").and_then(Value::as_str) {
        println!("key: {key}");
    }
    if let Some(values) = value.get("values") {
        println!("values: {}", compact_json(values)?);
    } else {
        println!("{}", compact_json(value)?);
    }
    Ok(())
}

fn print_draft_canonicalize_summary(value: &Value) -> Result<()> {
    let digest = value
        .get("draft_digest")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let mut lines = vec![format!("draft_digest: {digest}")];
    lines.extend(binding_summary_lines(value, "digest_map"));
    lines.extend(issue_summary_lines(value));
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_draft_resolve_summary(value: &Value) -> Result<()> {
    let mut lines = binding_summary_lines(value, "bindings");
    let unresolved = value
        .get("unresolved")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if unresolved.is_empty() {
        lines.push("unresolved: none".to_string());
    } else {
        lines.push(format!("unresolved: {}", unresolved.len()));
        for item in unresolved {
            lines.push(format!("  - {}", compact_json_lossy(item)));
        }
    }
    lines.extend(issue_summary_lines(value));
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_draft_validate_summary(value: &Value) -> Result<()> {
    let valid = value
        .get("valid")
        .and_then(Value::as_bool)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "(unknown)".to_string());
    let mut lines = vec![format!("valid: {valid}")];
    lines.extend(issue_summary_lines(value));
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_draft_preview_summary(value: &Value) -> Result<()> {
    let mut lines = Vec::new();
    for key in [
        "total_slots",
        "variants",
        "cases",
        "repeats",
        "seeds",
        "max_concurrency",
    ] {
        if let Some(item) = value.get(key) {
            lines.push(format!("{key}: {}", compact_json_lossy(item)));
        }
    }
    lines.extend(issue_summary_lines_from_key(value, "warnings"));
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_draft_suggest_summary(value: &Value) -> Result<()> {
    let suggestions = value
        .get("suggestions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("suggestions: {}", suggestions.len())];
    for suggestion in suggestions {
        let kind = suggestion
            .get("suggestion_type")
            .and_then(Value::as_str)
            .unwrap_or("suggestion");
        let title = suggestion
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)");
        let detail = suggestion
            .get("detail")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| format!(" - {value}"))
            .unwrap_or_default();
        lines.push(format!("  - [{kind}] {title}{detail}"));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_draft_export_summary(value: &Value) -> Result<()> {
    if let Some(body) = value.get("body").and_then(Value::as_str) {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    } else {
        print_json(value)?;
    }
    Ok(())
}

fn print_draft_diff_summary(value: &Value) -> Result<()> {
    let changes = value
        .get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("changes: {}", changes.len())];
    for change in changes {
        let op = change.get("op").and_then(Value::as_str).unwrap_or("change");
        let pointer = change.get("pointer").and_then(Value::as_str).unwrap_or("/");
        let significance = change
            .get("significance")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        lines.push(format!("  - [{significance}] {op} {pointer}"));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn binding_summary_lines(value: &Value, key: &str) -> Vec<String> {
    let bindings = value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if bindings.is_empty() {
        return vec![format!("{key}: none")];
    }
    let mut lines = vec![format!("{key}: {}", bindings.len())];
    for binding in bindings {
        let pointer = binding
            .get("pointer")
            .and_then(Value::as_str)
            .unwrap_or("/");
        let kind = binding
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("(kind)");
        let digest = binding
            .get("content_digest")
            .and_then(Value::as_str)
            .unwrap_or("(digest)");
        let resolution = binding
            .get("resolution")
            .and_then(Value::as_str)
            .unwrap_or("(resolution)");
        lines.push(format!("  - {pointer}: {kind} {digest} ({resolution})"));
    }
    lines
}

fn issue_summary_lines(value: &Value) -> Vec<String> {
    issue_summary_lines_from_key(value, "issues")
}

fn issue_summary_lines_from_key(value: &Value, key: &str) -> Vec<String> {
    let issues = value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(format_issue)
        .collect::<Vec<_>>();
    if issues.is_empty() {
        vec![format!("{key}: none")]
    } else {
        let mut lines = vec![format!("{key}:")];
        lines.extend(issues);
        lines
    }
}

fn format_issue(value: &Value) -> Option<String> {
    let severity = value
        .get("severity")
        .and_then(Value::as_str)
        .unwrap_or("warning");
    let code = value.get("code").and_then(Value::as_str).unwrap_or("issue");
    let pointer = value.get("pointer").and_then(Value::as_str).unwrap_or("/");
    let message = value.get("message").and_then(Value::as_str).unwrap_or("");
    if message.is_empty() {
        None
    } else {
        Some(format!("  - [{severity}] {code} {pointer}: {message}"))
    }
}

fn compact_json(value: &Value) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn compact_json_lossy(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid-json>".to_string())
}

fn print_help() {
    println!("{}", help_text());
}

fn help_text() -> &'static str {
    r#"buc - Bucephalus hosted Cloud CLI

`buc` talks to the hosted Cloud API. It does not run local Core builds, start
local runners, or manage Cloud operator pools.

Usage:
  buc [--api-url URL] [--user-token TOKEN] health
  buc [--api-url URL] [--user-token TOKEN] build <experiment.yaml|package-dir|package.tgz> [--context-root DIR] [--label TEXT] [--json]
  buc [--api-url URL] [--user-token TOKEN] doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]
  buc [--api-url URL] [--user-token TOKEN] run <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]
  buc [--api-url URL] [--user-token TOKEN] inspect <package-digest> [--json]
  buc [--api-url URL] [--user-token TOKEN] author canonicalize <draft.yaml|draft.json> [--json]
  buc [--api-url URL] [--user-token TOKEN] author resolve <draft.yaml|draft.json> [--json]
  buc [--api-url URL] [--user-token TOKEN] author validate <draft.yaml|draft.json> [--validation-level authoring|package|launch_hint] [--json]
  buc [--api-url URL] [--user-token TOKEN] author suggest <draft.yaml|draft.json> --target VALUE [--q TEXT] [--limit N] [--json]
  buc [--api-url URL] [--user-token TOKEN] author diff <left.yaml|json> <right.yaml|json> [--json]
  buc [--api-url URL] [--user-token TOKEN] author export <draft.yaml|draft.json> [--format yaml|resolved_json] [--json]
  buc [--api-url URL] [--user-token TOKEN] author preview <draft.yaml|draft.json> [--json]

Long-form nouns:
  buc [--api-url URL] [--user-token TOKEN] drafts canonicalize <draft.yaml|draft.json> [--json]
  buc [--api-url URL] [--user-token TOKEN] drafts resolve <draft.yaml|draft.json> [--json]
  buc [--api-url URL] [--user-token TOKEN] drafts validate <draft.yaml|draft.json> [--validation-level authoring|package|launch_hint] [--json]
  buc [--api-url URL] [--user-token TOKEN] drafts suggest <draft.yaml|draft.json> --target VALUE [--q TEXT] [--limit N] [--json]
  buc [--api-url URL] [--user-token TOKEN] drafts diff <left.yaml|json> <right.yaml|json> [--json]
  buc [--api-url URL] [--user-token TOKEN] drafts export <draft.yaml|draft.json> [--format yaml|resolved_json] [--json]
  buc [--api-url URL] [--user-token TOKEN] drafts preview-schedule <draft.yaml|draft.json> [--json]
  buc [--api-url URL] [--user-token TOKEN] packages list [--limit N] [--json]
  buc [--api-url URL] [--user-token TOKEN] packages upload <package-dir|package.tgz> [--label TEXT] [--json]
  buc [--api-url URL] [--user-token TOKEN] packages inspect <package-digest> [--json]
  buc [--api-url URL] [--user-token TOKEN] secrets list [--json]
  buc [--api-url URL] [--user-token TOKEN] secrets put <name> (--value-file PATH|--from-env ENV|--stdin) [--json]
  buc [--api-url URL] [--user-token TOKEN] secrets delete <name> [--json]
  buc [--api-url URL] [--user-token TOKEN] experiments build <experiment.yaml|package-dir|package.tgz> [--context-root DIR] [--label TEXT] [--json]
  buc [--api-url URL] [--user-token TOKEN] experiments doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]
  buc [--api-url URL] [--user-token TOKEN] runs list [--limit N] [--json]
  buc [--api-url URL] [--user-token TOKEN] runs create <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]
  buc [--api-url URL] [--user-token TOKEN] runs get <run-id> [--json]
  buc [--api-url URL] [--user-token TOKEN] runs runtime <run-id> [--json]
  buc [--api-url URL] [--user-token TOKEN] runs events <run-id> [--limit N] [--after-row-seq N] [--json]
  buc [--api-url URL] [--user-token TOKEN] runs results <run-id> [--limit N] [--json]
  buc [--api-url URL] [--user-token TOKEN] runs value <run-id> <key> [--json]

Cloud package boundary:
  build accepts authoring YAML or a sealed package directory/archive. YAML is
  uploaded as an authoring context and built by the hosted API with bundled
  Core. Sealed packages are uploaded/imported directly. Both paths report
  hosted Cloud readiness for the default Cloud target.

Authoring context:
  YAML builds default to the YAML parent directory as the uploaded context. Use
  --context-root DIR when the experiment intentionally references shared files
  under a broader repository/workspace root. The YAML must be inside DIR, and
  hosted Core builds the YAML entrypoint relative to DIR. Local generated and
  credential material such as .env, .npmrc, .ssh, .aws, node_modules, and
  target is excluded before upload; the hosted API rejects those paths too.

Runtime options:
  --backend VALUE --arch VALUE --isolation VALUE --cpu-count N --memory-mb N
  --disk-mb N --timeout-ms N --max-parallel-trials N
  --runtime-option KEY=VALUE (only supported hosted Cloud runtime keys)
  For array values use comma-separated lists, e.g. sidecars=redis,postgres.
  For network use JSON, e.g. network={"default":"allowlist_enforced","egress":["api.openai.com"]}.

Environment:
  BUCEPHALUS_CLOUD_API_URL       Hosted API base URL; falls back to the profile
                                 persisted by `bucephalus login`
  BUCEPHALUS_CLOUD_USER_TOKEN    OAuth access token override
"#
}

fn command_help_text(group: Option<&str>, command: Option<&str>) -> Option<&'static str> {
    match (group, command) {
        (Some("health"), None) | (Some("health"), Some("--help" | "-h")) => Some(HEALTH_HELP),
        (Some("build"), _) => Some(BUILD_HELP),
        (Some("doctor"), _) => Some(DOCTOR_HELP),
        (Some("run"), _) => Some(RUN_HELP),
        (Some("inspect"), _) => Some(INSPECT_HELP),
        (Some("author"), None) | (Some("author"), Some("--help" | "-h")) => Some(AUTHOR_HELP),
        (Some("author"), Some("canonicalize")) => Some(AUTHOR_CANONICALIZE_HELP),
        (Some("author"), Some("resolve")) => Some(AUTHOR_RESOLVE_HELP),
        (Some("author"), Some("validate")) => Some(AUTHOR_VALIDATE_HELP),
        (Some("author"), Some("preview" | "preview-schedule")) => Some(AUTHOR_PREVIEW_HELP),
        (Some("author"), Some("suggest")) => Some(AUTHOR_SUGGEST_HELP),
        (Some("author"), Some("export")) => Some(AUTHOR_EXPORT_HELP),
        (Some("author"), Some("diff")) => Some(AUTHOR_DIFF_HELP),
        (Some("drafts"), None) | (Some("drafts"), Some("--help" | "-h")) => Some(AUTHOR_HELP),
        (Some("drafts"), Some("canonicalize")) => Some(AUTHOR_CANONICALIZE_HELP),
        (Some("drafts"), Some("resolve")) => Some(AUTHOR_RESOLVE_HELP),
        (Some("drafts"), Some("validate")) => Some(AUTHOR_VALIDATE_HELP),
        (Some("drafts"), Some("preview" | "preview-schedule")) => Some(AUTHOR_PREVIEW_HELP),
        (Some("drafts"), Some("suggest")) => Some(AUTHOR_SUGGEST_HELP),
        (Some("drafts"), Some("export")) => Some(AUTHOR_EXPORT_HELP),
        (Some("drafts"), Some("diff")) => Some(AUTHOR_DIFF_HELP),
        (Some("packages"), None) | (Some("packages"), Some("--help" | "-h")) => Some(PACKAGES_HELP),
        (Some("packages"), Some("list")) => Some(PACKAGES_LIST_HELP),
        (Some("packages"), Some("upload")) => Some(PACKAGES_UPLOAD_HELP),
        (Some("packages"), Some("inspect")) => Some(PACKAGES_INSPECT_HELP),
        (Some("secrets"), None) | (Some("secrets"), Some("--help" | "-h")) => Some(SECRETS_HELP),
        (Some("secrets"), Some("list")) => Some(SECRETS_LIST_HELP),
        (Some("secrets"), Some("put" | "set")) => Some(SECRETS_PUT_HELP),
        (Some("secrets"), Some("delete" | "rm")) => Some(SECRETS_DELETE_HELP),
        (Some("experiments"), None) | (Some("experiments"), Some("--help" | "-h")) => {
            Some(EXPERIMENTS_HELP)
        }
        (Some("experiments"), Some("build")) => Some(EXPERIMENTS_BUILD_HELP),
        (Some("experiments"), Some("doctor")) => Some(EXPERIMENTS_DOCTOR_HELP),
        (Some("runs"), None) | (Some("runs"), Some("--help" | "-h")) => Some(RUNS_HELP),
        (Some("runs"), Some("list")) => Some(RUNS_LIST_HELP),
        (Some("runs"), Some("create")) => Some(RUNS_CREATE_HELP),
        (Some("runs"), Some("get")) => Some(RUNS_GET_HELP),
        (Some("runs"), Some("runtime")) => Some(RUNS_RUNTIME_HELP),
        (Some("runs"), Some("events")) => Some(RUNS_EVENTS_HELP),
        (Some("runs"), Some("results")) => Some(RUNS_RESULTS_HELP),
        (Some("runs"), Some("value" | "kv")) => Some(RUNS_VALUE_HELP),
        _ => None,
    }
}

const HEALTH_HELP: &str = r#"buc health

Check hosted API readiness.

Usage:
  buc health
"#;

const BUILD_HELP: &str = r#"buc build

Build authoring YAML in hosted Cloud or import a sealed package.

Usage:
  buc build <experiment.yaml|package-dir|package.tgz> [--context-root DIR] [--label TEXT] [--json]

Boundary:
  YAML inputs upload the YAML directory as an authoring context and call hosted
  Core. Package inputs upload/import an existing sealed package. Both paths fail
  if the package is not runnable on the hosted Cloud target.

Options:
  --context-root DIR  For YAML inputs, upload DIR as the authoring context and
                      build the YAML path relative to DIR.
                      Local generated and credential material is excluded
                      before upload and rejected by the hosted API.
"#;

const DOCTOR_HELP: &str = r#"buc doctor

Ask the hosted API whether a package can run with the supplied secrets and
runtime options. This uses the same gates as `buc run`.

Usage:
  buc doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]

Secrets:
  Prefer hosted secrets. Upload once with:
    buc secrets put NAME --from-env NAME
  Then pass:
    --secret-ref NAME=bucephalus://NAME
  Secret ref files must be YAML/JSON maps of NAME: bucephalus://NAME.
"#;

const RUN_HELP: &str = r#"buc run

Preflight with Cloud doctor, then queue a hosted run.

Usage:
  buc run <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]

Secrets:
  Prefer hosted secrets. Upload once with:
    buc secrets put NAME --from-env NAME
  Then pass:
    --secret-ref NAME=bucephalus://NAME
  Secret ref files must be YAML/JSON maps of NAME: bucephalus://NAME.
"#;

const INSPECT_HELP: &str = r#"buc inspect

Fetch package metadata and secret requirements from the hosted API.

Usage:
  buc inspect <package-digest> [--json]
"#;

const AUTHOR_HELP: &str = r#"buc author

Hosted draft authoring commands. These call Cloud draft APIs only; they do not
run local Core and they do not create runnable packages. Use `buc build` for the
hosted package boundary.

Usage:
  buc author canonicalize <draft.yaml|draft.json> [--json]
  buc author resolve <draft.yaml|draft.json> [--json]
  buc author validate <draft.yaml|draft.json> [--validation-level authoring|package|launch_hint] [--json]
  buc author suggest <draft.yaml|draft.json> --target VALUE [--q TEXT] [--limit N] [--json]
  buc author diff <left.yaml|json> <right.yaml|json> [--json]
  buc author export <draft.yaml|draft.json> [--format yaml|resolved_json] [--json]
  buc author preview <draft.yaml|draft.json> [--json]
"#;

const AUTHOR_CANONICALIZE_HELP: &str = r#"buc author canonicalize

Canonicalize a draft through the hosted Cloud authoring API and return its
stable draft digest plus entity digest bindings.

Usage:
  buc author canonicalize <draft.yaml|draft.json> [--json]
"#;

const AUTHOR_RESOLVE_HELP: &str = r#"buc author resolve

Resolve registry refs and aliases in a draft through the hosted Cloud authoring
API.

Usage:
  buc author resolve <draft.yaml|draft.json> [--json]
"#;

const AUTHOR_VALIDATE_HELP: &str = r#"buc author validate

Validate a draft experiment through the hosted Cloud authoring API.

Usage:
  buc author validate <draft.yaml|draft.json> [--validation-level authoring|package|launch_hint] [--json]
"#;

const AUTHOR_PREVIEW_HELP: &str = r#"buc author preview

Preview schedule size, case/variant expansion, and authoring warnings.

Usage:
  buc author preview <draft.yaml|draft.json> [--json]
"#;

const AUTHOR_SUGGEST_HELP: &str = r#"buc author suggest

Request registry-backed hosted authoring suggestions for a draft target.

Usage:
  buc author suggest <draft.yaml|draft.json> --target variant [--q TEXT] [--limit N] [--json]
"#;

const AUTHOR_EXPORT_HELP: &str = r#"buc author export

Export a draft as hosted API YAML or resolved JSON.

Usage:
  buc author export <draft.yaml|draft.json> [--format yaml|resolved_json] [--json]
"#;

const AUTHOR_DIFF_HELP: &str = r#"buc author diff

Diff two local draft files through the hosted authoring API.

Usage:
  buc author diff <left.yaml|json> <right.yaml|json> [--json]
"#;

const PACKAGES_HELP: &str = r#"buc packages

Hosted package commands.

Usage:
  buc packages list [--limit N] [--json]
  buc packages upload <package-dir|package.tgz> [--label TEXT] [--json]
  buc packages inspect <package-digest> [--json]
"#;

const PACKAGES_LIST_HELP: &str = r#"buc packages list

List recent hosted package artifacts visible to the authenticated user.

Usage:
  buc packages list [--limit N] [--json]
"#;

const PACKAGES_UPLOAD_HELP: &str = r#"buc packages upload

Upload and import a sealed package directory/archive.

Usage:
  buc packages upload <package-dir|package.tgz> [--label TEXT] [--json]

Input:
  A directory produced by `bucephalus build ... --out <package-dir>`, or an
  archive of that directory. Authoring YAML is rejected before any API call.
"#;

const PACKAGES_INSPECT_HELP: &str = r#"buc packages inspect

Fetch package metadata and secret requirements from the hosted API.

Usage:
  buc packages inspect <package-digest> [--json]
"#;

const SECRETS_HELP: &str = r#"buc secrets

Hosted secret commands. Secret values are write-only: the API returns names,
versions, timestamps, and hosted refs, never plaintext or backing provider refs.

Usage:
  buc secrets list [--json]
  buc secrets put <name> (--value-file PATH|--from-env ENV|--stdin) [--json]
  buc secrets delete <name> [--json]
"#;

const SECRETS_LIST_HELP: &str = r#"buc secrets list

List hosted secret names and versions visible to the authenticated user.
Values are never returned.

Usage:
  buc secrets list [--json]
"#;

const SECRETS_PUT_HELP: &str = r#"buc secrets put

Create or rotate a hosted secret value. Prefer --value-file, --from-env, or
--stdin so plaintext does not land in shell history. After upload, refer to the
secret in doctor/run as NAME=bucephalus://NAME.

Usage:
  buc secrets put <name> (--value-file PATH|--from-env ENV|--stdin) [--json]
"#;

const SECRETS_DELETE_HELP: &str = r#"buc secrets delete

Delete a hosted secret and its backing store entry.

Usage:
  buc secrets delete <name> [--json]
"#;

const EXPERIMENTS_HELP: &str = r#"buc experiments

Hosted experiment workflow commands.

Usage:
  buc experiments build <experiment.yaml|package-dir|package.tgz> [--context-root DIR] [--label TEXT] [--json]
  buc experiments doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]

Secrets:
  Prefer hosted secrets. Upload once with `buc secrets put NAME --from-env NAME`,
  then pass `--secret-ref NAME=bucephalus://NAME`.
"#;

const EXPERIMENTS_BUILD_HELP: &str = r#"buc experiments build

Build authoring YAML in hosted Cloud or import a sealed package.

Usage:
  buc experiments build <experiment.yaml|package-dir|package.tgz> [--context-root DIR] [--label TEXT] [--json]

Boundary:
  This command calls POST /v1/experiments/builds after upload. YAML inputs are
  built by hosted Core from the uploaded authoring context. Sealed package
  inputs are imported directly. Both paths report hosted Cloud readiness.

Options:
  --context-root DIR  For YAML inputs, upload DIR as the authoring context and
                      build the YAML path relative to DIR.
"#;

const EXPERIMENTS_DOCTOR_HELP: &str = r#"buc experiments doctor

Ask the hosted API whether a package can run with the supplied secrets and
runtime options. This uses the same gates as `buc runs create`.

Usage:
  buc experiments doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]

Secrets:
  Prefer hosted secrets. Upload once with:
    buc secrets put NAME --from-env NAME
  Then pass:
    --secret-ref NAME=bucephalus://NAME
  Secret ref files must be YAML/JSON maps of NAME: bucephalus://NAME.
"#;

const RUNS_HELP: &str = r#"buc runs

Hosted run commands.

Usage:
  buc runs list [--limit N] [--json]
  buc runs create <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]
  buc runs get <run-id> [--json]
  buc runs runtime <run-id> [--json]
  buc runs events <run-id> [--limit N] [--after-row-seq N] [--json]
  buc runs results <run-id> [--limit N] [--json]
  buc runs value <run-id> <key> [--json]

Secrets:
  Prefer hosted secrets. Upload once with `buc secrets put NAME --from-env NAME`,
  then pass `--secret-ref NAME=bucephalus://NAME`.
"#;

const RUNS_LIST_HELP: &str = r#"buc runs list

List recent hosted run records visible to the authenticated user.

Usage:
  buc runs list [--limit N] [--json]
"#;

const RUNS_CREATE_HELP: &str = r#"buc runs create

Preflight with Cloud doctor, then queue a hosted run.

Usage:
  buc runs create <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]

Secrets:
  Prefer hosted secrets. Upload once with:
    buc secrets put NAME --from-env NAME
  Then pass:
    --secret-ref NAME=bucephalus://NAME
  Secret ref files must be YAML/JSON maps of NAME: bucephalus://NAME.
"#;

const RUNS_GET_HELP: &str = r#"buc runs get

Fetch hosted run status.

Usage:
  buc runs get <run-id> [--json]
"#;

const RUNS_RUNTIME_HELP: &str = r#"buc runs runtime

Fetch the latest live runtime summary for a hosted run.

Usage:
  buc runs runtime <run-id> [--json]
"#;

const RUNS_EVENTS_HELP: &str = r#"buc runs events

List live runtime event rows for a hosted run.

Usage:
  buc runs events <run-id> [--limit N] [--after-row-seq N] [--json]
"#;

const RUNS_RESULTS_HELP: &str = r#"buc runs results

Fetch runtime result rows for a hosted run.

Usage:
  buc runs results <run-id> [--limit N] [--json]
"#;

const RUNS_VALUE_HELP: &str = r#"buc runs value

Fetch live runtime values by key for a hosted run. `buc runs kv` is an alias.

Usage:
  buc runs value <run-id> <key> [--json]
  buc runs kv <run-id> <key> [--json]
"#;

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn media_type_for_path(path: &Path) -> &'static str {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        "application/gzip"
    } else if lower.ends_with(".tar") {
        "application/x-tar"
    } else {
        "application/octet-stream"
    }
}

fn encode_path_segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn path_with_query(path: &str, params: &[(&str, Option<String>)]) -> String {
    let query = params
        .iter()
        .filter_map(|(key, value)| {
            let value = value.as_ref()?.trim();
            if value.is_empty() {
                None
            } else {
                Some(format!(
                    "{}={}",
                    utf8_percent_encode(key, NON_ALPHANUMERIC),
                    utf8_percent_encode(value, NON_ALPHANUMERIC)
                ))
            }
        })
        .collect::<Vec<_>>();
    if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", query.join("&"))
    }
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(non_empty)
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn current_unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| {
            i64::try_from(duration.as_millis()).expect("Unix timestamp milliseconds must fit i64")
        })
        .expect("system time must be after Unix epoch")
}

fn make_temp_dir(prefix: &str) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Mutex, MutexGuard};
    use std::thread;
    use std::time::{Duration, Instant};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvVarGuard {
        saved: Vec<(String, Option<String>)>,
    }

    impl EnvVarGuard {
        fn set(vars: &[(&str, Option<&str>)]) -> Self {
            let saved = vars
                .iter()
                .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
                .collect::<Vec<_>>();
            for (name, value) in vars {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
            Self { saved }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.iter().rev() {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("buc_cli_{label}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn hosted_authoring_yaml_uses_build_command_and_requires_api_config() {
        let _lock = lock_env();
        let home = temp_dir("authoring_api_config");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        let err = run(vec![
            "experiments".to_string(),
            "build".to_string(),
            "experiment.yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted API URL"));
        assert!(!err.contains("unknown hosted command"));

        let natural_err = run(vec![
            "build".to_string(),
            "experiment.modal.yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(natural_err.contains("hosted API URL"));
        assert!(
            !natural_err.contains("unknown hosted command"),
            "natural build command should be recognized before failing: {natural_err}"
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn packages_upload_rejects_authoring_yaml_with_build_hint() {
        let root = temp_dir("package_upload_yaml");
        fs::create_dir_all(&root).unwrap();
        let experiment = root.join("experiment.yaml");
        fs::write(&experiment, "experiment: {}\n").unwrap();

        let err = prepare_sealed_package_input(&experiment)
            .unwrap_err()
            .to_string();

        assert!(err.contains("buc packages upload expects a sealed package"));
        assert!(err.contains("buc build"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authoring_context_archive_preserves_entrypoint_and_excludes_local_junk() {
        let _lock = lock_env();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_ENTRIES", None),
            (
                "BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES",
                None,
            ),
        ]);
        let root = temp_dir("authoring_context");
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join(".ssh")).unwrap();
        fs::create_dir_all(root.join(".aws")).unwrap();
        fs::create_dir_all(root.join(".docker")).unwrap();
        fs::create_dir_all(root.join(".config/gcloud")).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        fs::write(root.join("cases.jsonl"), "{}\n").unwrap();
        fs::write(root.join(".env"), "SECRET=oops\n").unwrap();
        fs::write(root.join(".npmrc"), "//registry.example/:_authToken=oops\n").unwrap();
        fs::write(
            root.join(".netrc"),
            "machine example login token password oops\n",
        )
        .unwrap();
        fs::write(root.join(".ssh/id_ed25519"), "private key\n").unwrap();
        fs::write(root.join(".aws/credentials"), "aws_access_key_id=oops\n").unwrap();
        fs::write(root.join(".docker/config.json"), "{\"auths\":{}}\n").unwrap();
        fs::write(
            root.join(".config/gcloud/application_default_credentials.json"),
            "{\"client_secret\":\"oops\"}\n",
        )
        .unwrap();
        fs::write(root.join("target/debug/blob"), "junk").unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "junk").unwrap();
        fs::write(root.join(".git/config"), "junk").unwrap();

        let prepared =
            prepare_authoring_context_input(&root.join("experiment.yaml"), None).unwrap();

        assert_eq!(prepared.entrypoint, "experiment.yaml");
        let entries = archive_entries(&prepared.archive_path);
        assert!(entries.contains(&"experiment.yaml".to_string()));
        assert!(entries.contains(&"cases.jsonl".to_string()));
        assert!(!entries.iter().any(|entry| entry.starts_with(".env")));
        assert!(!entries.iter().any(|entry| entry.starts_with(".npmrc")));
        assert!(!entries.iter().any(|entry| entry.starts_with(".netrc")));
        assert!(!entries.iter().any(|entry| entry.starts_with(".git/")));
        assert!(!entries.iter().any(|entry| entry.starts_with(".ssh/")));
        assert!(!entries.iter().any(|entry| entry.starts_with(".aws/")));
        assert!(!entries.iter().any(|entry| entry.starts_with(".docker/")));
        assert!(!entries
            .iter()
            .any(|entry| entry.starts_with(".config/gcloud/")));
        assert!(!entries.iter().any(|entry| entry.starts_with("target/")));
        assert!(!entries
            .iter()
            .any(|entry| entry.starts_with("node_modules/")));
        drop(prepared);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authoring_context_archive_rejects_contexts_over_entry_limit_before_upload() {
        let _lock = lock_env();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_ENTRIES", Some("1")),
            (
                "BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES",
                Some("1048576"),
            ),
        ]);
        let root = temp_dir("authoring_context_entry_limit");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        fs::write(root.join("cases.jsonl"), "{}\n").unwrap();

        let err = prepare_authoring_context_input(&root.join("experiment.yaml"), None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("authoring context has too many entries"));
        assert!(err.contains("Narrow --context-root"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authoring_context_archive_rejects_contexts_over_byte_limit_before_upload() {
        let _lock = lock_env();
        let _env = EnvVarGuard::set(&[
            (
                "BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_ENTRIES",
                Some("100"),
            ),
            (
                "BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES",
                Some("8"),
            ),
        ]);
        let root = temp_dir("authoring_context_byte_limit");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();

        let err = prepare_authoring_context_input(&root.join("experiment.yaml"), None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("authoring context is too large"));
        assert!(err.contains("expanded size"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authoring_context_root_supports_nested_experiment_with_shared_files() {
        let _lock = lock_env();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_ENTRIES", None),
            (
                "BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES",
                None,
            ),
        ]);
        let root = temp_dir("authoring_context_root");
        fs::create_dir_all(root.join("experiments/peter")).unwrap();
        fs::create_dir_all(root.join("shared")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(
            root.join("experiments/peter/experiment.yaml"),
            "dataset: ../../shared/cases.jsonl\n",
        )
        .unwrap();
        fs::write(root.join("shared/cases.jsonl"), "{}\n").unwrap();
        fs::write(root.join(".env.local"), "SECRET=oops\n").unwrap();
        fs::write(root.join("target/debug/blob"), "junk").unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "junk").unwrap();

        let prepared = prepare_authoring_context_input(
            &root.join("experiments/peter/experiment.yaml"),
            Some(&root),
        )
        .unwrap();

        assert_eq!(prepared.entrypoint, "experiments/peter/experiment.yaml");
        let entries = archive_entries(&prepared.archive_path);
        assert!(entries.contains(&"experiments/peter/experiment.yaml".to_string()));
        assert!(entries.contains(&"shared/cases.jsonl".to_string()));
        assert!(!entries.iter().any(|entry| entry.starts_with(".env")));
        assert!(!entries.iter().any(|entry| entry.starts_with("target/")));
        assert!(!entries
            .iter()
            .any(|entry| entry.starts_with("node_modules/")));
        drop(prepared);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authoring_context_root_rejects_yaml_outside_boundary() {
        let root = temp_dir("authoring_context_root_reject");
        let outside = temp_dir("authoring_context_outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("experiment.yaml"), "experiment: {}\n").unwrap();

        let err = prepare_authoring_context_input(&outside.join("experiment.yaml"), Some(&root))
            .unwrap_err()
            .to_string();

        assert!(err.contains("must be inside --context-root"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn build_yaml_posts_authoring_context_request_to_cloud_api() {
        let _lock = lock_env();
        let root = temp_dir("authoring_context_api");
        fs::create_dir_all(root.join("experiments/peter")).unwrap();
        fs::create_dir_all(root.join("shared")).unwrap();
        fs::write(
            root.join("experiments/peter/experiment.yaml"),
            "dataset: ../../shared/cases.jsonl\n",
        )
        .unwrap();
        fs::write(root.join("shared/cases.jsonl"), "{}\n").unwrap();
        fs::write(root.join(".env"), "SECRET=oops\n").unwrap();

        let server = MockCloudServer::start(4);
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_api_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
            ("BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_ENTRIES", None),
            (
                "BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES",
                None,
            ),
        ]);

        run(vec![
            "build".to_string(),
            root.join("experiments/peter/experiment.yaml")
                .display()
                .to_string(),
            "--context-root".to_string(),
            root.display().to_string(),
            "--label".to_string(),
            "demo".to_string(),
            "--memory-mb".to_string(),
            "131072".to_string(),
        ])
        .expect("hosted YAML build should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/uploads");
        assert_eq!(
            requests[0].header("authorization"),
            Some("Bearer test-token")
        );
        let upload_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(upload_body["filename"], json!("authoring-context.tgz"));
        assert_eq!(upload_body["media_type"], json!("application/gzip"));
        assert_eq!(requests[1].method, "PUT");
        assert_eq!(requests[1].path, "/v1/uploads/upload-1/content");
        let uploaded_entries = archive_entries_from_bytes(&requests[1].body);
        assert!(uploaded_entries.contains(&"experiments/peter/experiment.yaml".to_string()));
        assert!(uploaded_entries.contains(&"shared/cases.jsonl".to_string()));
        assert!(!uploaded_entries
            .iter()
            .any(|entry| entry.starts_with(".env")));
        assert_eq!(requests[2].method, "POST");
        assert_eq!(requests[2].path, "/v1/uploads/upload-1/complete");
        assert_eq!(requests[3].method, "POST");
        assert_eq!(requests[3].path, "/v1/experiments/builds");
        let build_body: Value = serde_json::from_slice(&requests[3].body).unwrap();
        assert_eq!(build_body["upload_id"], json!("upload-1"));
        assert_eq!(build_body["label"], json!("demo"));
        assert_eq!(build_body["input_kind"], json!("authoring_context"));
        assert_eq!(
            build_body["entrypoint"],
            json!("experiments/peter/experiment.yaml")
        );
        assert_eq!(build_body["runtime_options"]["memory_mb"], json!(131072));

        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_for_wrong_build_kind() {
        fn mismatched_authoring_build_kind(request: &RecordedRequest, _index: usize) -> Value {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "import-1",
                    "build_kind": "sealed_package_import",
                    "build_environment": {
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context"
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        }

        let _lock = lock_env();
        let root = temp_dir("authoring_context_wrong_kind");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let server = MockCloudServer::start_with_handler(4, mismatched_authoring_build_kind);
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_wrong_kind_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build response kind mismatch"));
        assert!(err.contains("requested authoring_context"));
        assert!(err.contains("API returned sealed_package_import"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_package_rejects_cloud_response_for_wrong_source_kind() {
        fn mismatched_package_source_kind(request: &RecordedRequest, _index: usize) -> Value {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "import-1",
                    "build_kind": "sealed_package_import",
                    "build_environment": {
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context"
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        }

        let _lock = lock_env();
        let root = temp_dir("sealed_package_wrong_source_kind");
        fs::create_dir_all(&root).unwrap();
        write_minimal_package(&root);
        let server = MockCloudServer::start_with_handler(4, mismatched_package_source_kind);
        let api_url = server.api_url();
        let home = temp_dir("sealed_package_wrong_source_kind_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build source kind mismatch"));
        assert!(err.contains("requested sealed_package"));
        assert!(err.contains("API built authoring_context"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_for_wrong_source_upload_id() {
        fn mismatched_authoring_upload_id(request: &RecordedRequest, _index: usize) -> Value {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "source": {
                            "upload_id": "upload-2",
                            "input_kind": "authoring_context"
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        }

        let _lock = lock_env();
        let root = temp_dir("authoring_context_wrong_upload_id");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let server = MockCloudServer::start_with_handler(4, mismatched_authoring_upload_id);
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_wrong_upload_id_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build source upload mismatch"));
        assert!(err.contains("uploaded upload-1"));
        assert!(err.contains("API built from upload-2"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_for_wrong_source_digest() {
        fn mismatched_authoring_digest(request: &RecordedRequest, _index: usize) -> Value {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context",
                            "content_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        }

        let _lock = lock_env();
        let root = temp_dir("authoring_context_wrong_source_digest");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let server = MockCloudServer::start_with_handler(4, mismatched_authoring_digest);
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_wrong_source_digest_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build source digest mismatch"));
        assert!(err.contains("uploaded sha256:"));
        assert!(err.contains("API built from sha256:bbbbbbbb"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_for_wrong_source_byte_size() {
        let _lock = lock_env();
        let root = temp_dir("authoring_context_wrong_source_byte_size");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let mut source_digest: Option<String> = None;
        let server = MockCloudServer::start_with_stateful_handler(4, move |request, _index| {
            if request.method == "PUT" && request.path == "/v1/uploads/upload-1/content" {
                source_digest = Some(sha256_digest(&request.body));
            }
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context",
                            "content_digest": source_digest.as_deref().unwrap_or("sha256:missing-upload-content"),
                            "byte_size": 1
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_wrong_source_byte_size_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build source byte_size mismatch"));
        assert!(err.contains("uploaded "));
        assert!(err.contains("API built from 1"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_missing_source_digest() {
        fn missing_authoring_digest(request: &RecordedRequest, _index: usize) -> Value {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context",
                            "byte_size": 123
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        }

        let _lock = lock_env();
        let root = temp_dir("authoring_context_missing_source_digest");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let server = MockCloudServer::start_with_handler(4, missing_authoring_digest);
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_missing_source_digest_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err
            .contains("hosted build response is missing build_environment.source.content_digest"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_missing_source_byte_size() {
        let _lock = lock_env();
        let root = temp_dir("authoring_context_missing_source_byte_size");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let mut source_digest: Option<String> = None;
        let server = MockCloudServer::start_with_stateful_handler(4, move |request, _index| {
            if request.method == "PUT" && request.path == "/v1/uploads/upload-1/content" {
                source_digest = Some(sha256_digest(&request.body));
            }
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context",
                            "content_digest": source_digest.as_deref().unwrap_or("sha256:missing-upload-content")
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_missing_source_byte_size_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build response is missing build_environment.source.byte_size"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_missing_source_entrypoint() {
        let _lock = lock_env();
        let root = temp_dir("authoring_context_missing_source_entrypoint");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let mut source: Option<(String, u64)> = None;
        let server = MockCloudServer::start_with_stateful_handler(4, move |request, _index| {
            if request.method == "PUT" && request.path == "/v1/uploads/upload-1/content" {
                source = Some((sha256_digest(&request.body), request.body.len() as u64));
            }
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context",
                            "content_digest": source.as_ref().map(|(digest, _)| digest.as_str()).unwrap_or("sha256:missing-upload-content"),
                            "byte_size": source.as_ref().map(|(_, byte_size)| *byte_size).unwrap_or(0)
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_missing_source_entrypoint_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("hosted build response is missing build_environment.source.entrypoint")
        );
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_for_wrong_source_entrypoint() {
        let _lock = lock_env();
        let root = temp_dir("authoring_context_wrong_source_entrypoint");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let mut source: Option<(String, u64)> = None;
        let server = MockCloudServer::start_with_stateful_handler(4, move |request, _index| {
            if request.method == "PUT" && request.path == "/v1/uploads/upload-1/content" {
                source = Some((sha256_digest(&request.body), request.body.len() as u64));
            }
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context",
                            "content_digest": source.as_ref().map(|(digest, _)| digest.as_str()).unwrap_or("sha256:missing-upload-content"),
                            "byte_size": source.as_ref().map(|(_, byte_size)| *byte_size).unwrap_or(0),
                            "entrypoint": "other.yaml"
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_wrong_source_entrypoint_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build source entrypoint mismatch"));
        assert!(err.contains("requested experiment.yaml"));
        assert!(err.contains("API built other.yaml"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_for_wrong_runtime_options() {
        let _lock = lock_env();
        let root = temp_dir("authoring_context_wrong_runtime_options");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let mut source: Option<(String, u64)> = None;
        let server = MockCloudServer::start_with_stateful_handler(4, move |request, _index| {
            if request.method == "PUT" && request.path == "/v1/uploads/upload-1/content" {
                source = Some((sha256_digest(&request.body), request.body.len() as u64));
            }
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => {
                    let requested_runtime = runtime_options_from_build_request(request);
                    json!({
                        "build_id": "build-1",
                        "build_kind": "hosted_authoring_build",
                        "build_environment": {
                            "source": {
                                "upload_id": "upload-1",
                                "input_kind": "authoring_context",
                                "content_digest": source.as_ref().map(|(digest, _)| digest.as_str()).unwrap_or("sha256:missing-upload-content"),
                                "byte_size": source.as_ref().map(|(_, byte_size)| *byte_size).unwrap_or(0),
                                "entrypoint": "experiment.yaml"
                            },
                            "runtime_options": {
                                "memory_mb": 4096
                            }
                        },
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "cloud_readiness": {
                            "status": "cloud_runnable",
                            "runtime_options": requested_runtime,
                            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        },
                        "import": {
                            "status": "accepted"
                        }
                    })
                }
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_wrong_runtime_options_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--memory-mb".to_string(),
            "8192".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build runtime options mismatch"));
        assert!(err.contains("\"memory_mb\":8192"));
        assert!(err.contains("\"memory_mb\":4096"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_for_wrong_readiness_runtime_options() {
        let _lock = lock_env();
        let root = temp_dir("authoring_context_wrong_readiness_runtime_options");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let mut source: Option<(String, u64)> = None;
        let server = MockCloudServer::start_with_stateful_handler(4, move |request, _index| {
            if request.method == "PUT" && request.path == "/v1/uploads/upload-1/content" {
                source = Some((sha256_digest(&request.body), request.body.len() as u64));
            }
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => {
                    let requested_runtime = runtime_options_from_build_request(request);
                    json!({
                        "build_id": "build-1",
                        "build_kind": "hosted_authoring_build",
                        "build_environment": {
                            "source": {
                                "upload_id": "upload-1",
                                "input_kind": "authoring_context",
                                "content_digest": source.as_ref().map(|(digest, _)| digest.as_str()).unwrap_or("sha256:missing-upload-content"),
                                "byte_size": source.as_ref().map(|(_, byte_size)| *byte_size).unwrap_or(0),
                                "entrypoint": "experiment.yaml"
                            },
                            "runtime_options": requested_runtime
                        },
                        "status": "cloud_runnable",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "cloud_readiness": {
                            "status": "cloud_runnable",
                            "runtime_options": {
                                "memory_mb": 4096
                            },
                            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        },
                        "import": {
                            "status": "accepted"
                        }
                    })
                }
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_wrong_readiness_runtime_options_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--memory-mb".to_string(),
            "8192".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build Cloud readiness runtime options mismatch"));
        assert!(err.contains("\"memory_mb\":8192"));
        assert!(err.contains("\"memory_mb\":4096"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_for_wrong_build_target() {
        let _lock = lock_env();
        let root = temp_dir("authoring_context_wrong_build_target");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let mut source: Option<(String, u64)> = None;
        let server = MockCloudServer::start_with_stateful_handler(4, move |request, _index| {
            if request.method == "PUT" && request.path == "/v1/uploads/upload-1/content" {
                source = Some((sha256_digest(&request.body), request.body.len() as u64));
            }
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "target": {
                            "kind": "local_core",
                            "name": "default"
                        },
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context",
                            "content_digest": source.as_ref().map(|(digest, _)| digest.as_str()).unwrap_or("sha256:missing-upload-content"),
                            "byte_size": source.as_ref().map(|(_, byte_size)| *byte_size).unwrap_or(0),
                            "entrypoint": "experiment.yaml"
                        },
                        "runtime_options": {},
                        "package_contract": {
                            "input_kind": "authoring_context",
                            "authoring_compiler": "core_universal_v1",
                            "sealed_schema_version": "sealed_run_package_v2",
                            "readiness_schema_version": "hosted_cloud_readiness_v1",
                            "cloud_readiness_required": true
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "target": {
                            "kind": "hosted_cloud",
                            "name": "default"
                        },
                        "runtime_options": {},
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_wrong_build_target_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build target mismatch"));
        assert!(err.contains("requested hosted_cloud/default"));
        assert!(err.contains("API built local_core/default"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_yaml_rejects_cloud_response_for_wrong_package_contract() {
        let _lock = lock_env();
        let root = temp_dir("authoring_context_wrong_package_contract");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        let mut source: Option<(String, u64)> = None;
        let server = MockCloudServer::start_with_stateful_handler(4, move |request, _index| {
            if request.method == "PUT" && request.path == "/v1/uploads/upload-1/content" {
                source = Some((sha256_digest(&request.body), request.body.len() as u64));
            }
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/uploads") => json!({ "upload_id": "upload-1" }),
                ("PUT", "/v1/uploads/upload-1/content") => json!({}),
                ("POST", "/v1/uploads/upload-1/complete") => json!({}),
                ("POST", "/v1/experiments/builds") => json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "target": {
                            "kind": "hosted_cloud",
                            "name": "default"
                        },
                        "source": {
                            "upload_id": "upload-1",
                            "input_kind": "authoring_context",
                            "content_digest": source.as_ref().map(|(digest, _)| digest.as_str()).unwrap_or("sha256:missing-upload-content"),
                            "byte_size": source.as_ref().map(|(_, byte_size)| *byte_size).unwrap_or(0),
                            "entrypoint": "experiment.yaml"
                        },
                        "runtime_options": {},
                        "package_contract": {
                            "input_kind": "sealed_package",
                            "authoring_compiler": "core_universal_v1",
                            "sealed_schema_version": "sealed_run_package_v2",
                            "readiness_schema_version": "hosted_cloud_readiness_v1",
                            "cloud_readiness_required": true
                        }
                    },
                    "status": "cloud_runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "target": {
                            "kind": "hosted_cloud",
                            "name": "default"
                        },
                        "runtime_options": {},
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "import": {
                        "status": "accepted"
                    }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("authoring_context_wrong_package_contract_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "build".to_string(),
            root.join("experiment.yaml").display().to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("hosted build package contract input mismatch"));
        assert!(err.contains("requested authoring_context"));
        assert!(err.contains("contract reports sealed_package"));
        let requests = server.join();
        assert_eq!(requests.len(), 4);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn author_validate_posts_draft_to_cloud_api() {
        let _lock = lock_env();
        let root = temp_dir("author_validate_api");
        fs::create_dir_all(&root).unwrap();
        let draft_path = root.join("draft.yaml");
        fs::write(
            &draft_path,
            [
                "experiment:",
                "  id: demo",
                "  name: Demo",
                "runtime:",
                "  compute:",
                "    backend: local-docker",
                "matrix:",
                "  variants:",
                "    - id: baseline",
                "  cases:",
                "    count: 2",
                "stages: {}",
                "policy: {}",
                "",
            ]
            .join("\n"),
        )
        .unwrap();

        let server = MockCloudServer::start(1);
        let api_url = server.api_url();
        let home = temp_dir("author_validate_api_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "author".to_string(),
            "validate".to_string(),
            draft_path.display().to_string(),
            "--validation-level".to_string(),
            "launch_hint".to_string(),
        ])
        .expect("hosted author validate should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/drafts/validate");
        assert_eq!(
            requests[0].header("authorization"),
            Some("Bearer test-token")
        );
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["validation_level"], json!("launch_hint"));
        assert_eq!(body["draft"]["experiment"]["id"], json!("demo"));
        assert_eq!(
            body["draft"]["runtime"]["compute"]["backend"],
            json!("local-docker")
        );

        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn author_commands_reject_unknown_or_misplaced_options_before_file_reads() {
        let suggest_err = run(vec![
            "author".to_string(),
            "suggest".to_string(),
            "missing-draft.yaml".to_string(),
            "--targt".to_string(),
            "variant".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(suggest_err.contains("unknown option --targt"));
        assert!(!suggest_err.contains("failed to read"));

        let validate_err = run(vec![
            "author".to_string(),
            "validate".to_string(),
            "missing-draft.yaml".to_string(),
            "--validation-level".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(validate_err.contains("--validation-level requires a value"));
        assert!(!validate_err.contains("failed to read"));

        let diff_err = run(vec![
            "author".to_string(),
            "diff".to_string(),
            "before.yaml".to_string(),
            "after.yaml".to_string(),
            "--format".to_string(),
            "yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(diff_err.contains("unknown option --format"));

        let extra_diff_arg_err = run(vec![
            "author".to_string(),
            "diff".to_string(),
            "before.yaml".to_string(),
            "after.yaml".to_string(),
            "third.yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(extra_diff_arg_err.contains("requires exactly two draft paths"));
        assert!(!extra_diff_arg_err.contains("failed to read"));

        let duplicate_draft_source_err = run(vec![
            "author".to_string(),
            "validate".to_string(),
            "draft.yaml".to_string(),
            "--file".to_string(),
            "other.yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(duplicate_draft_source_err
            .contains("draft JSON/YAML path must be provided either positionally or with --file"));
        assert!(!duplicate_draft_source_err.contains("failed to read"));
    }

    #[test]
    fn author_canonicalize_posts_draft_to_cloud_api() {
        let _lock = lock_env();
        let root = temp_dir("author_canonicalize_api");
        fs::create_dir_all(&root).unwrap();
        let draft_path = root.join("draft.yaml");
        write_cli_draft(&draft_path, "demo", "local-docker");

        let server = MockCloudServer::start(1);
        let api_url = server.api_url();
        let home = temp_dir("author_canonicalize_api_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "author".to_string(),
            "canonicalize".to_string(),
            draft_path.display().to_string(),
        ])
        .expect("hosted draft canonicalize should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/drafts/canonicalize");
        assert_eq!(
            requests[0].header("authorization"),
            Some("Bearer test-token")
        );
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["draft"]["experiment"]["id"], json!("demo"));

        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn author_resolve_posts_draft_to_cloud_api() {
        let _lock = lock_env();
        let root = temp_dir("author_resolve_api");
        fs::create_dir_all(&root).unwrap();
        let draft_path = root.join("draft.yaml");
        write_cli_draft(&draft_path, "demo", "local-docker");

        let server = MockCloudServer::start(1);
        let api_url = server.api_url();
        let home = temp_dir("author_resolve_api_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "drafts".to_string(),
            "resolve".to_string(),
            draft_path.display().to_string(),
        ])
        .expect("hosted draft resolve should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/drafts/resolve");
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body["draft"]["runtime"]["compute"]["backend"],
            json!("local-docker")
        );

        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn author_preview_posts_draft_to_cloud_api() {
        let _lock = lock_env();
        let root = temp_dir("author_preview_api");
        fs::create_dir_all(&root).unwrap();
        let draft_path = root.join("draft.yaml");
        write_cli_draft(&draft_path, "demo", "local-docker");

        let server = MockCloudServer::start(1);
        let api_url = server.api_url();
        let home = temp_dir("author_preview_api_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "drafts".to_string(),
            "preview-schedule".to_string(),
            draft_path.display().to_string(),
        ])
        .expect("hosted draft preview should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/drafts/preview-schedule");
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["draft"]["experiment"]["id"], json!("demo"));

        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn author_suggest_posts_target_query_and_limit_to_cloud_api() {
        let _lock = lock_env();
        let root = temp_dir("author_suggest_api");
        fs::create_dir_all(&root).unwrap();
        let draft_path = root.join("draft.yaml");
        write_cli_draft(&draft_path, "demo", "local-docker");

        let server = MockCloudServer::start(1);
        let api_url = server.api_url();
        let home = temp_dir("author_suggest_api_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "author".to_string(),
            "suggest".to_string(),
            draft_path.display().to_string(),
            "--target".to_string(),
            "variant".to_string(),
            "--q".to_string(),
            "base".to_string(),
            "--limit".to_string(),
            "2".to_string(),
        ])
        .expect("hosted draft suggest should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/drafts/suggest");
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["target"], json!("variant"));
        assert_eq!(body["q"], json!("base"));
        assert_eq!(body["limit"], json!(2));
        assert_eq!(body["draft"]["experiment"]["id"], json!("demo"));

        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn author_export_posts_format_to_cloud_api() {
        let _lock = lock_env();
        let root = temp_dir("author_export_api");
        fs::create_dir_all(&root).unwrap();
        let draft_path = root.join("draft.yaml");
        write_cli_draft(&draft_path, "demo", "local-docker");

        let server = MockCloudServer::start(1);
        let api_url = server.api_url();
        let home = temp_dir("author_export_api_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "drafts".to_string(),
            "export".to_string(),
            draft_path.display().to_string(),
            "--format".to_string(),
            "resolved_json".to_string(),
            "--json".to_string(),
        ])
        .expect("hosted draft export should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/drafts/export");
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["format"], json!("resolved_json"));
        assert_eq!(body["draft"]["experiment"]["id"], json!("demo"));

        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn author_diff_posts_two_inline_drafts_to_cloud_api() {
        let _lock = lock_env();
        let root = temp_dir("author_diff_api");
        fs::create_dir_all(&root).unwrap();
        let left_path = root.join("left.yaml");
        let right_path = root.join("right.yaml");
        write_cli_draft(&left_path, "left", "local-docker");
        write_cli_draft(&right_path, "right", "modal");

        let server = MockCloudServer::start(1);
        let api_url = server.api_url();
        let home = temp_dir("author_diff_api_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "author".to_string(),
            "diff".to_string(),
            left_path.display().to_string(),
            right_path.display().to_string(),
        ])
        .expect("hosted draft diff should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/drafts/diff");
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["left"]["draft"]["experiment"]["id"], json!("left"));
        assert_eq!(
            body["right"]["draft"]["runtime"]["compute"]["backend"],
            json!("modal")
        );

        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn packages_list_gets_hosted_packages_with_limit() {
        let _lock = lock_env();
        let server = MockCloudServer::start(1);
        let api_url = server.api_url();
        let home = temp_dir("packages_list_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "packages".to_string(),
            "list".to_string(),
            "--limit".to_string(),
            "7".to_string(),
        ])
        .expect("hosted packages list should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/v1/packages?limit=7");
        assert_eq!(
            requests[0].header("authorization"),
            Some("Bearer test-token")
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn package_inspect_rejects_mismatched_package_response_digest() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/v1/packages/sha256%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            );
            json!({
                "package_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "status": "accepted"
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("mismatched_package_inspect_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "inspect".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("package response digest mismatch"));
        assert!(err.contains("requested sha256:aaaaaaaa"));
        assert!(err.contains("API returned sha256:bbbbbbbb"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn package_inspect_rejects_missing_package_response_digest() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/v1/packages/sha256%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            );
            json!({
                "status": "accepted"
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("missing_package_inspect_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "inspect".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("package response is missing package_digest"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_list_gets_hosted_runs_with_limit() {
        let _lock = lock_env();
        let server = MockCloudServer::start(1);
        let api_url = server.api_url();
        let home = temp_dir("runs_list_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "list".to_string(),
            "--limit".to_string(),
            "3".to_string(),
        ])
        .expect("hosted runs list should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/v1/runs?limit=3");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_runtime_events_results_and_values_get_hosted_runtime_paths() {
        let _lock = lock_env();
        let server = MockCloudServer::start(4);
        let api_url = server.api_url();
        let home = temp_dir("runs_runtime_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "runtime".to_string(),
            "run-1".to_string(),
        ])
        .expect("hosted run runtime should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "events".to_string(),
            "run-1".to_string(),
            "--limit".to_string(),
            "5".to_string(),
            "--after-row-seq".to_string(),
            "12".to_string(),
        ])
        .expect("hosted run events should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "results".to_string(),
            "run-1".to_string(),
            "--limit".to_string(),
            "9".to_string(),
        ])
        .expect("hosted run results should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "value".to_string(),
            "run-1".to_string(),
            "trial/status".to_string(),
        ])
        .expect("hosted run value should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/v1/runs/run%2D1/runtime");
        assert_eq!(requests[1].method, "GET");
        assert_eq!(
            requests[1].path,
            "/v1/runs/run%2D1/runtime/events?limit=5&after%5Frow%5Fseq=12"
        );
        assert_eq!(requests[2].method, "GET");
        assert_eq!(requests[2].path, "/v1/runs/run%2D1/runtime/results?limit=9");
        assert_eq!(requests[3].method, "GET");
        assert_eq!(
            requests[3].path,
            "/v1/runs/run%2D1/runtime/kv/trial%2Fstatus"
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_get_rejects_mismatched_run_response_id() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/v1/runs/run%2D1");
            json!({
                "run_id": "run-2",
                "status": "completed"
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("mismatched_run_get_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("run response id mismatch"));
        assert!(err.contains("requested run-1"));
        assert!(err.contains("API returned run-2"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_runtime_rejects_mismatched_cloud_run_id() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/v1/runs/run%2D1/runtime");
            json!({
                "cloud_run_id": "run-2",
                "summary": {
                    "status": "running"
                }
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("mismatched_run_runtime_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "runs".to_string(),
            "runtime".to_string(),
            "run-1".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("runtime response run id mismatch"));
        assert!(err.contains("requested run-1"));
        assert!(err.contains("API returned run-2"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn secrets_put_list_and_delete_use_hosted_secret_api_without_provider_refs() {
        let _lock = lock_env();
        let server = MockCloudServer::start(3);
        let api_url = server.api_url();
        let home = temp_dir("secrets_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "secrets".to_string(),
            "put".to_string(),
            "OPENAI_API_KEY".to_string(),
            "--value".to_string(),
            "sk-test-secret".to_string(),
        ])
        .expect("hosted secret put should complete against mock Cloud API");
        run(vec!["secrets".to_string(), "list".to_string()])
            .expect("hosted secret list should complete against mock Cloud API");
        run(vec![
            "secrets".to_string(),
            "delete".to_string(),
            "OPENAI_API_KEY".to_string(),
        ])
        .expect("hosted secret delete should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].method, "PUT");
        assert_eq!(requests[0].path, "/v1/secrets/OPENAI%5FAPI%5FKEY");
        assert_eq!(
            requests[0].header("authorization"),
            Some("Bearer test-token")
        );
        let put_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(put_body, json!({ "value": "sk-test-secret" }));
        assert_eq!(requests[1].method, "GET");
        assert_eq!(requests[1].path, "/v1/secrets");
        assert_eq!(requests[2].method, "DELETE");
        assert_eq!(requests[2].path, "/v1/secrets/OPENAI%5FAPI%5FKEY");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn secrets_put_requires_exactly_one_value_source() {
        let no_source = run(vec![
            "--api-url".to_string(),
            "https://cloud.example".to_string(),
            "--user-token".to_string(),
            "token".to_string(),
            "secrets".to_string(),
            "put".to_string(),
            "OPENAI_API_KEY".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(no_source.contains("secret value source is required"));

        let duplicate_sources = run(vec![
            "--api-url".to_string(),
            "https://cloud.example".to_string(),
            "--user-token".to_string(),
            "token".to_string(),
            "secrets".to_string(),
            "put".to_string(),
            "OPENAI_API_KEY".to_string(),
            "--value".to_string(),
            "one".to_string(),
            "--from-env".to_string(),
            "OPENAI_API_KEY".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(duplicate_sources.contains("choose exactly one secret value source"));
    }

    #[test]
    fn secrets_put_rejects_mismatched_response_name() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "PUT");
            assert_eq!(request.path, "/v1/secrets/OPENAI%5FAPI%5FKEY");
            json!({
                "name": "OTHER_SECRET",
                "version": 1
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("secret_put_mismatch_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "secrets".to_string(),
            "put".to_string(),
            "OPENAI_API_KEY".to_string(),
            "--value".to_string(),
            "sk-test-secret".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("secret put response name mismatch"));
        assert!(err.contains("requested OPENAI_API_KEY"));
        assert!(err.contains("API returned OTHER_SECRET"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn secrets_put_rejects_missing_response_name() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "PUT");
            assert_eq!(request.path, "/v1/secrets/OPENAI%5FAPI%5FKEY");
            json!({
                "version": 1
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("secret_put_missing_name_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "secrets".to_string(),
            "put".to_string(),
            "OPENAI_API_KEY".to_string(),
            "--value".to_string(),
            "sk-test-secret".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("secret put response is missing name"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn secrets_put_rejects_incomplete_hosted_secret_metadata() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "PUT");
            assert_eq!(request.path, "/v1/secrets/OPENAI%5FAPI%5FKEY");
            json!({
                "name": "OPENAI_API_KEY",
                "created_at": "2026-06-12T00:00:00Z",
                "updated_at": "2026-06-12T00:00:00Z"
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("secret_put_incomplete_metadata_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "secrets".to_string(),
            "put".to_string(),
            "OPENAI_API_KEY".to_string(),
            "--value".to_string(),
            "sk-test-secret".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("secret put response for OPENAI_API_KEY is missing numeric version"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn secrets_list_rejects_missing_secrets_array() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/v1/secrets");
            json!({})
        });
        let api_url = server.api_url();
        let home = temp_dir("secret_list_missing_array_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec!["secrets".to_string(), "list".to_string()])
            .unwrap_err()
            .to_string();

        assert!(err.contains("secret list response is missing secrets array"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn secrets_list_rejects_malformed_secret_rows() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/v1/secrets");
            json!({
                "secrets": [{
                    "name": "OPENAI_API_KEY",
                    "version": 1,
                    "created_at": "2026-06-12T00:00:00Z"
                }]
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("secret_list_malformed_row_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "secrets".to_string(),
            "list".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("secret list item #0 response for OPENAI_API_KEY is missing updated_at")
        );
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn secrets_delete_rejects_mismatched_response_name() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "DELETE");
            assert_eq!(request.path, "/v1/secrets/OPENAI%5FAPI%5FKEY");
            json!({
                "name": "OTHER_SECRET",
                "deleted": true
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("secret_delete_mismatch_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "secrets".to_string(),
            "delete".to_string(),
            "OPENAI_API_KEY".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("secret delete response name mismatch"));
        assert!(err.contains("requested OPENAI_API_KEY"));
        assert!(err.contains("API returned OTHER_SECRET"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn secrets_delete_rejects_unconfirmed_delete_response() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "DELETE");
            assert_eq!(request.path, "/v1/secrets/OPENAI%5FAPI%5FKEY");
            json!({
                "name": "OPENAI_API_KEY",
                "deleted": false
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("secret_delete_unconfirmed_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "secrets".to_string(),
            "delete".to_string(),
            "OPENAI_API_KEY".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("secret delete response for OPENAI_API_KEY did not confirm deletion"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn run_create_sends_hosted_secret_refs_to_doctor_and_run_creation() {
        let _lock = lock_env();
        let server = MockCloudServer::start(2);
        let api_url = server.api_url();
        let home = temp_dir("hosted_secret_run_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);
        let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        run(vec![
            "run".to_string(),
            digest.to_string(),
            "--secret-ref".to_string(),
            "GEMINI_API_KEY=bucephalus://GEMINI_API_KEY".to_string(),
            "--memory-mb".to_string(),
            "4096".to_string(),
            "--max-parallel-trials".to_string(),
            "3".to_string(),
            "--runtime-option".to_string(),
            "executor=runner-docker".to_string(),
            "--env".to_string(),
            "PUBLIC_MODE=smoke".to_string(),
            "--label".to_string(),
            "hosted-secret-smoke".to_string(),
        ])
        .expect("hosted secret run creation should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/experiments/doctor");
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].path, "/v1/runs");
        for request in &requests {
            assert_eq!(request.header("authorization"), Some("Bearer test-token"));
            assert_eq!(request.header("content-type"), Some("application/json"));
        }
        let doctor_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(doctor_body["package_digest"], digest);
        assert_eq!(
            doctor_body["secret_refs"],
            json!({ "GEMINI_API_KEY": "bucephalus://GEMINI_API_KEY" })
        );
        assert_eq!(
            doctor_body["runtime_options"],
            json!({
                "memory_mb": 4096,
                "max_parallel_trials": 3,
                "executor": "runner-docker"
            })
        );
        assert!(doctor_body.get("env").is_none());
        assert!(doctor_body.get("run_label").is_none());
        let run_body: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(run_body["package_digest"], digest);
        assert_eq!(run_body["run_label"], "hosted-secret-smoke");
        assert_eq!(
            run_body["secret_refs"],
            json!({ "GEMINI_API_KEY": "bucephalus://GEMINI_API_KEY" })
        );
        assert_eq!(run_body["env"], json!({ "PUBLIC_MODE": "smoke" }));
        assert_eq!(run_body["runtime_options"], doctor_body["runtime_options"]);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn doctor_command_requires_runnable_response_for_requested_package() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/v1/experiments/doctor");
            json!({
                "ok": false,
                "status": "blocked",
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "message": "No active runner pool can satisfy this run."
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("blocked_doctor_command_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "doctor".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("Cloud doctor did not prove this package runnable"));
        assert!(err.contains("status=blocked ok=false"));
        assert!(err.contains("No active runner pool"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/v1/experiments/doctor");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn doctor_command_rejects_mismatched_package_digest() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/v1/experiments/doctor");
            json!({
                "ok": true,
                "status": "runnable",
                "package_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("mismatched_doctor_command_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "doctor".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("package_digest mismatch"));
        assert!(err.contains("requested sha256:aaaaaaaa"));
        assert!(err.contains("doctor checked sha256:bbbbbbbb"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/v1/experiments/doctor");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn run_create_stops_when_doctor_response_does_not_prove_runnable() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/v1/experiments/doctor");
            json!({
                "ok": false,
                "status": "blocked",
                "message": "No active runner pool can satisfy this run."
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("blocked_doctor_run_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("Cloud doctor rejected this run before queueing it"));
        assert!(err.contains("status=blocked ok=false"));
        assert!(err.contains("No active runner pool"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/v1/experiments/doctor");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn run_create_stops_when_run_response_does_not_match_request() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(2, |request, _index| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/experiments/doctor") => json!({
                    "ok": true,
                    "status": "runnable",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                }),
                ("POST", "/v1/runs") => json!({
                    "run_id": "run-1",
                    "status": "created",
                    "package_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("mismatched_run_response_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("Cloud run creation package_digest mismatch"));
        assert!(err.contains("requested sha256:aaaaaaaa"));
        assert!(err.contains("run uses sha256:bbbbbbbb"));
        let requests = server.join();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].path, "/v1/experiments/doctor");
        assert_eq!(requests[1].path, "/v1/runs");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn workflow_commands_reject_unknown_or_misplaced_options_before_api_calls() {
        let build_err = run(vec![
            "build".to_string(),
            "experiment.yaml".to_string(),
            "--secret-ref".to_string(),
            "OPENAI_API_KEY=bucephalus://OPENAI_API_KEY".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(build_err.contains("unknown option --secret-ref"));

        let doctor_err = run(vec![
            "doctor".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--label".to_string(),
            "ignored".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(doctor_err.contains("unknown option --label"));

        let run_err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--labell".to_string(),
            "typo".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(run_err.contains("unknown option --labell"));

        let missing_value_err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--label".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(missing_value_err.contains("--label requires a value"));

        let unsupported_smoke_test_err = run(vec![
            "build".to_string(),
            "experiment.yaml".to_string(),
            "--smoke-test".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(unsupported_smoke_test_err.contains("unknown option --smoke-test"));

        let unsupported_runtime_option_err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--runtime-option".to_string(),
            "materialize=copy".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(unsupported_runtime_option_err
            .contains("unsupported hosted Cloud runtime option `materialize`"));

        let duplicate_secret_ref_err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--secret-ref".to_string(),
            "OPENAI_API_KEY=bucephalus://OPENAI_API_KEY".to_string(),
            "--secret".to_string(),
            "OPENAI_API_KEY=bucephalus://OTHER_OPENAI_API_KEY".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(duplicate_secret_ref_err
            .contains("secret ref `OPENAI_API_KEY` was provided more than once"));

        let duplicate_env_err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--env".to_string(),
            "PUBLIC_MODE=smoke".to_string(),
            "--env".to_string(),
            "PUBLIC_MODE=prod".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(duplicate_env_err.contains("--env key `PUBLIC_MODE` was provided more than once"));

        let invalid_env_err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--env".to_string(),
            "bad-name=1".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(
            invalid_env_err.contains("--env key `bad-name` must be an uppercase shell identifier")
        );

        let reserved_env_err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--env".to_string(),
            "DATABASE_URL=postgres://example".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(reserved_env_err
            .contains("`DATABASE_URL` is reserved for Cloud runtime/control-plane state"));

        let env_secret_collision_err = run(vec![
            "run".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "--env".to_string(),
            "OPENAI_API_KEY=not-a-secret".to_string(),
            "--secret-ref".to_string(),
            "OPENAI_API_KEY=bucephalus://OPENAI_API_KEY".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(env_secret_collision_err
            .contains("--env key `OPENAI_API_KEY` cannot also be supplied as a secret ref"));

        let extra_build_arg_err = run(vec![
            "build".to_string(),
            "experiment.yaml".to_string(),
            "other.yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(extra_build_arg_err
            .contains("experiment.yaml or sealed package directory/archive accepts exactly one positional argument"));
        assert!(!extra_build_arg_err.contains("Cloud API"));

        let duplicate_build_source_err = run(vec![
            "build".to_string(),
            "experiment.yaml".to_string(),
            "--file".to_string(),
            "other.yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(duplicate_build_source_err.contains(
            "experiment.yaml or sealed package directory/archive must be provided either positionally or with --file"
        ));

        let duplicate_build_file_err = run(vec![
            "build".to_string(),
            "--file".to_string(),
            "experiment.yaml".to_string(),
            "--file".to_string(),
            "other.yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(duplicate_build_file_err.contains("--file can only be provided once"));

        let extra_doctor_arg_err = run(vec![
            "doctor".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "extra".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(
            extra_doctor_arg_err.contains("package digest accepts exactly one positional argument")
        );

        let run_value_extra_arg_err = run(vec![
            "runs".to_string(),
            "value".to_string(),
            "run-1".to_string(),
            "metric".to_string(),
            "extra".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(run_value_extra_arg_err
            .contains("runtime value lookup requires exactly two positional arguments"));

        let run_value_mixed_arg_err = run(vec![
            "runs".to_string(),
            "value".to_string(),
            "run-1".to_string(),
            "--key".to_string(),
            "metric".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(run_value_mixed_arg_err
            .contains("runtime value lookup must be provided either as positional arguments"));

        let list_extra_arg_err = run(vec![
            "runs".to_string(),
            "list".to_string(),
            "unexpected".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(list_extra_arg_err
            .contains("buc runs list does not accept positional arguments: unexpected"));

        let secret_extra_arg_err = run(vec![
            "secrets".to_string(),
            "put".to_string(),
            "OPENAI_API_KEY".to_string(),
            "EXTRA".to_string(),
            "--value".to_string(),
            "redacted".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(
            secret_extra_arg_err.contains("secret name accepts exactly one positional argument")
        );
    }

    #[test]
    fn help_is_hosted_product_cli_not_operator_cli() {
        let help = help_text();

        assert!(help.contains(
            "buc [--api-url URL] [--user-token TOKEN] build <experiment.yaml|package-dir|package.tgz> [--context-root DIR]"
        ));
        assert!(help.contains("Authoring context:"));
        assert!(help.contains("--context-root DIR"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] run <package-digest>"));
        assert!(help.contains("Long-form nouns:"));
        assert!(help.contains("hosted Cloud readiness"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] author canonicalize"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] author resolve"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] author validate"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] packages list"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] secrets put <name>"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] runs list"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] runs runtime"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] runs events"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] runs results"));
        assert!(help.contains("buc [--api-url URL] [--user-token TOKEN] runs value"));
        assert!(!help.contains("runner-pool"));
        assert!(!help.contains("runner-instance"));
        assert!(!help.contains("build-upload"));
        assert!(!help.contains("--core-cmd"));
        assert!(!help.contains("bucephalus-cloud"));
    }

    #[test]
    fn doctor_and_run_help_teach_hosted_secret_refs() {
        for help in [
            DOCTOR_HELP,
            RUN_HELP,
            EXPERIMENTS_DOCTOR_HELP,
            RUNS_CREATE_HELP,
        ] {
            assert!(help.contains("buc secrets put NAME --from-env NAME"));
            assert!(help.contains("--secret-ref NAME=bucephalus://NAME"));
            assert!(help.contains("Secret ref files must be YAML/JSON maps"));
            assert!(!help.contains("provider://ref"));
        }
        for help in [EXPERIMENTS_HELP, RUNS_HELP] {
            assert!(help.contains("buc secrets put NAME --from-env NAME"));
            assert!(help.contains("--secret-ref NAME=bucephalus://NAME"));
            assert!(!help.contains("provider://ref"));
        }
    }

    #[test]
    fn product_command_set_excludes_retired_operator_and_wrapper_commands() {
        assert!(known_hosted_command(Some("experiments"), Some("build")));
        assert!(known_hosted_command(Some("experiments"), Some("doctor")));
        assert!(known_hosted_command(Some("runs"), Some("create")));
        assert!(known_hosted_command(Some("build"), Some("package-dir")));
        assert!(known_hosted_command(Some("doctor"), Some("sha256:abc")));
        assert!(known_hosted_command(Some("run"), Some("sha256:abc")));
        assert!(known_hosted_command(Some("inspect"), Some("sha256:abc")));
        assert!(known_hosted_command(Some("author"), Some("canonicalize")));
        assert!(known_hosted_command(Some("author"), Some("resolve")));
        assert!(known_hosted_command(Some("author"), Some("validate")));
        assert!(known_hosted_command(Some("author"), Some("suggest")));
        assert!(known_hosted_command(Some("packages"), Some("list")));
        assert!(known_hosted_command(Some("secrets"), Some("list")));
        assert!(known_hosted_command(Some("secrets"), Some("put")));
        assert!(known_hosted_command(Some("secrets"), Some("set")));
        assert!(known_hosted_command(Some("secrets"), Some("delete")));
        assert!(known_hosted_command(Some("secrets"), Some("rm")));
        assert!(known_hosted_command(Some("runs"), Some("list")));
        assert!(known_hosted_command(Some("runs"), Some("runtime")));
        assert!(known_hosted_command(Some("runs"), Some("events")));
        assert!(known_hosted_command(Some("runs"), Some("results")));
        assert!(known_hosted_command(Some("runs"), Some("value")));
        assert!(known_hosted_command(Some("runs"), Some("kv")));
        assert!(known_hosted_command(Some("drafts"), Some("diff")));
        assert!(!known_hosted_command(Some("runner-pool"), Some("create")));
        assert!(!known_hosted_command(Some("deploy"), None));
        assert!(!known_hosted_command(Some("build-upload"), None));
        assert!(!known_hosted_command(Some("draft"), Some("export")));
    }

    #[test]
    fn package_directory_must_be_a_sealed_package() {
        let root = temp_dir("missing_manifest");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.json"), "{}").unwrap();

        let err = prepare_sealed_package_input(&root).unwrap_err().to_string();

        assert!(err.contains("missing manifest.json"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_path_uploads_parent_package_directory() {
        let root = temp_dir("manifest_parent");
        fs::create_dir_all(&root).unwrap();
        write_minimal_package(&root);

        let prepared = prepare_sealed_package_input(&root.join("manifest.json")).unwrap();

        assert!(prepared.archive_path.is_file());
        assert_eq!(prepared.source_label, root.display().to_string());
        drop(prepared);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sealed_package_directory_preflight_rejects_incomplete_build_output() {
        let root = temp_dir("incomplete_package");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("manifest.json"),
            r#"{"schema_version":"sealed_run_package_v2","checksums_ref":"checksums.json"}"#,
        )
        .unwrap();
        fs::write(
            root.join("checksums.json"),
            r#"{"schema_version":"sealed_package_checksums_v2","files":{}}"#,
        )
        .unwrap();

        let err = prepare_sealed_package_input(&root).unwrap_err().to_string();

        assert!(err.contains("missing /package_checks_ref"));
        assert!(err.contains("complete `bucephalus build` output"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sealed_package_directory_preflight_rejects_authoring_like_manifest() {
        let root = temp_dir("authoring_manifest");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("manifest.json"),
            r#"{"experiment":{"id":"not_a_package"}}"#,
        )
        .unwrap();

        let err = prepare_sealed_package_input(&root).unwrap_err().to_string();

        assert!(err.contains("not a sealed_run_package_v2 manifest"));
        assert!(err.contains("For authoring YAML, run `buc build experiment.yaml`"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_help_renders_without_api_or_package_args() {
        let _lock = lock_env();
        let home = temp_dir("nested_help");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        run(vec![
            "experiments".to_string(),
            "build".to_string(),
            "--help".to_string(),
        ])
        .expect("nested help should not require API config or package path");
        run(vec!["build".to_string(), "--help".to_string()])
            .expect("top-level build help should render without API config");
        run(vec!["health".to_string(), "--help".to_string()])
            .expect("health help should render without API config");
        run(vec!["runs".to_string()]).expect("command group help should render without API config");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_summary_for_failed_import_does_not_suggest_doctor_unknown_digest() {
        let response = json!({
            "build_id": "import-1",
            "status": "failed",
            "build_kind": "sealed_package_import",
            "package_digest": null,
            "import": {
                "error_message": "checksums.json missing object field 'files'",
                "diagnostics": [{
                    "severity": "error",
                    "code": "missing_checksums_files",
                    "pointer": "/checksums/files",
                    "message": "checksums.json must include a files object."
                }]
            }
        });

        let lines = build_summary_lines(&response, "/tmp/package").unwrap();
        let text = lines.join("\n");

        assert!(text.contains("status: failed"));
        assert!(text.contains("build_kind: sealed_package_import"));
        assert!(text.contains("checksums.json must include a files object"));
        assert!(!text.contains("buc experiments doctor (unknown)"));
        assert!(text.contains("fix the authoring/package diagnostics"));
    }

    #[test]
    fn build_summary_for_failed_authoring_surfaces_core_error() {
        let response = json!({
            "build_id": "build-1",
            "status": "failed",
            "build_kind": "hosted_authoring_build",
            "package_digest": null,
            "authoring_build": {
                "status": "failed",
                "code": "authoring_build_failed",
                "error": "Hosted Core build failed",
                "detail": {
                    "exit_code": 42,
                    "stderr_tail": "bad authoring\nmissing /policy",
                    "stdout_tail": "{\"ok\":false}"
                }
            },
            "cloud_readiness": {
                "status": "unavailable",
                "checks": []
            },
            "import": null
        });

        let lines = build_summary_lines(&response, "experiment.yaml").unwrap();
        let text = lines.join("\n");

        assert!(text.contains("authoring_build: failed"));
        assert!(text.contains("authoring_code: authoring_build_failed"));
        assert!(text.contains("authoring_error: Hosted Core build failed"));
        assert!(text.contains("authoring_exit_code: 42"));
        assert!(text.contains("bad authoring"));
        assert!(text.contains("missing /policy"));
        assert!(!text.contains("Cloud importer rejected"));
    }

    #[test]
    fn build_summary_surfaces_cloud_readiness_checks() {
        let response = json!({
            "build_id": "import-1",
            "status": "cloud_blocked",
            "build_kind": "sealed_package_import",
            "build_environment": {
                "schema_version": "hosted_build_environment_v1",
                "target": {
                    "kind": "hosted_cloud",
                    "name": "default"
                },
                "source": {
                    "input_kind": "sealed_package",
                    "upload_id": "upload-1",
                    "filename": "package.tgz",
                    "media_type": "application/gzip",
                    "content_digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                    "byte_size": 12345
                },
                "runtime_options": {
                    "memory_mb": 131072
                },
                "builder": {
                    "kind": "api_embedded_core",
                    "image_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "release_version": "0.3.37",
                    "git_sha": "abc123",
                    "os": "linux",
                    "arch": "x64"
                },
                "core": {
                    "command": "bucephalus build",
                    "path": "/app/bin/bucephalus",
                    "version": "0.3.37",
                    "timeout_ms": 600000
                },
                "package_contract": {
                    "input_kind": "sealed_package",
                    "authoring_compiler": "core_universal_v1",
                    "sealed_schema_version": "sealed_run_package_v2",
                    "readiness_schema_version": "hosted_cloud_readiness_v1",
                    "cloud_readiness_required": true
                },
                "evidence": {
                    "policy": "warn",
                    "status": "partial",
                    "missing": ["builder_git_sha"],
                    "checks": [{
                        "name": "builder_git_sha",
                        "status": "warning",
                        "code": "builder_git_sha_missing",
                        "message": "Build environment does not include a hosted release git SHA."
                    }]
                }
            },
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "cloud_readiness": {
                "status": "cloud_blocked",
                "run_requirements": {
                    "executor": "runner-docker",
                    "requires": ["core_runner", "docker_daemon", "registry_pull"],
                    "memory_mb": 131072
                },
                "checks": [{
                    "name": "runner_capacity",
                    "status": "blocked",
                    "code": "run_unschedulable",
                    "message": "No active runner pool can satisfy this run."
                }]
            },
            "import": {
                "status": "accepted"
            }
        });

        let lines = build_summary_lines(&response, "/tmp/package").unwrap();
        let text = lines.join("\n");

        assert!(text.contains("cloud_readiness: cloud_blocked"));
        assert!(text.contains("build_target: hosted_cloud/default"));
        assert!(text.contains("build_source: sealed_package upload=upload-1"));
        assert!(text.contains(
            "build_source_digest: sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        ));
        assert!(text.contains("build_source_bytes: 12345"));
        assert!(text.contains("build_runtime_options:"));
        assert!(text.contains("\"memory_mb\":131072"));
        assert!(text.contains("build_input_kind: sealed_package"));
        assert!(text.contains("authoring_compiler: core_universal_v1"));
        assert!(text.contains("package_contract: sealed_run_package_v2"));
        assert!(text.contains("cloud_readiness_required: true"));
        assert!(text.contains("builder_core: bucephalus build version=0.3.37"));
        assert!(text.contains("builder_timeout_ms: 600000"));
        assert!(text.contains("builder_image_digest: sha256:bbbb"));
        assert!(text.contains("build_environment_evidence_policy: warn"));
        assert!(text.contains("build_environment_evidence: partial"));
        assert!(text.contains("missing_build_evidence: builder_git_sha"));
        assert!(text.contains("cloud_run_requirements:"));
        assert!(text.contains("runner_capacity/run_unschedulable"));
    }

    #[test]
    fn build_summary_guides_secret_setup_for_cloud_readiness_requirements() {
        let response = json!({
            "build_id": "build-1",
            "status": "cloud_runnable",
            "build_kind": "hosted_authoring_build",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "cloud_readiness": {
                "status": "cloud_runnable",
                "secret_requirements": [{
                    "id": "OPENAI_API_KEY",
                    "target": "env:OPENAI_API_KEY",
                    "required_for_variants": ["baseline"]
                }],
                "required_actions": [{
                    "action": "upload_hosted_secret",
                    "stage": "before_run",
                    "requirement_id": "OPENAI_API_KEY",
                    "description": "Upload hosted secret 'OPENAI_API_KEY' before creating a run.",
                    "command": "buc secrets put OPENAI_API_KEY --from-env OPENAI_API_KEY",
                    "blocking": false
                }],
                "checks": []
            },
            "import": {
                "status": "accepted"
            }
        });

        let lines = build_summary_lines(&response, "experiment.yaml").unwrap();
        let text = lines.join("\n");

        assert!(text.contains("next: buc secrets put OPENAI_API_KEY --from-env OPENAI_API_KEY"));
        assert!(text.contains(
            "next: buc doctor sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --secret-ref OPENAI_API_KEY=bucephalus://OPENAI_API_KEY"
        ));
        assert!(text.contains(
            "next: buc run sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --secret-ref OPENAI_API_KEY=bucephalus://OPENAI_API_KEY"
        ));
        assert!(!text.contains("provider://ref"));
    }

    #[test]
    fn build_summary_guides_run_for_cloud_runnable_package_without_secrets() {
        let response = json!({
            "build_id": "build-1",
            "status": "cloud_runnable",
            "build_kind": "hosted_authoring_build",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "cloud_readiness": {
                "status": "cloud_runnable",
                "secret_requirements": [],
                "required_actions": [],
                "checks": []
            },
            "import": {
                "status": "accepted"
            }
        });

        let lines = build_summary_lines(&response, "experiment.yaml").unwrap();
        let text = lines.join("\n");

        assert!(text.contains(
            "next: buc doctor sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(text.contains(
            "next: buc run sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(!text.contains("--secret-ref"));
    }

    #[test]
    fn doctor_summary_guides_run_with_hosted_secret_refs() {
        let response = json!({
            "status": "runnable",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "package_status": "accepted",
            "run_requirements": {
                "executor": "runner-docker"
            },
            "secret_requirements": [{
                "id": "GEMINI_API_KEY",
                "target": "env:GEMINI_API_KEY",
                "required_for_variants": []
            }]
        });
        let requirements = secret_requirements_from_value(&response);
        let setup = secret_setup_lines(&requirements).join("\n");

        assert_eq!(
            setup,
            "next: buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY"
        );
        assert_eq!(
            next_run_command(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &requirements
            ),
            "next: buc run sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY"
        );
    }

    #[test]
    fn cloud_error_message_surfaces_api_next_commands_and_actions() {
        let payload = json!({
            "code": "invalid_secret_refs",
            "message": "Run secret refs must match the package secret requirements",
            "detail": {
                "missing_secret_ids": ["GEMINI_API_KEY"],
                "next_commands": [
                    "buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY",
                    "buc run <package-digest> --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY"
                ],
                "required_actions": [{
                    "action": "upload_hosted_secret",
                    "stage": "before_run",
                    "description": "Upload hosted secret 'GEMINI_API_KEY' before creating a run.",
                    "command": "buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY",
                    "blocking": true
                }]
            }
        });

        let message = cloud_error_message(400, &payload);

        assert!(message.contains("Run secret refs must match the package secret requirements"));
        assert!(message.contains("code: invalid_secret_refs"));
        assert!(message.contains("next: buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY"));
        assert!(message.contains(
            "next: buc run <package-digest> --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY"
        ));
        assert!(message.contains("actions:"));
        assert!(message.contains("[before_run] upload_hosted_secret"));
    }

    #[test]
    fn cloud_blocked_build_fails_after_successful_import_with_readiness_reason() {
        let response = json!({
            "status": "cloud_blocked",
            "import": {
                "status": "accepted"
            },
            "cloud_readiness": {
                "status": "cloud_blocked",
                "checks": [{
                    "name": "runtime_contract",
                    "status": "blocked",
                    "code": "package_images_not_cloud_pinned",
                    "message": "Package references image(s) that are not digest-pinned remote registry refs for Cloud runs: image:local"
                }],
                "required_actions": [{
                    "action": "package_images_not_cloud_pinned",
                    "stage": "before_rebuild",
                    "description": "Push and digest-pin the referenced images, then rebuild.",
                    "blocking": true
                }]
            }
        });

        ensure_import_accepted(&response, "hosted build")
            .expect("accepted import should not be confused with cloud readiness failure");
        let err = ensure_cloud_readiness(&response)
            .expect_err("cloud_blocked readiness should fail hosted build")
            .to_string();

        assert!(err.contains("hosted build is not runnable in Cloud"));
        assert!(err.contains("package_images_not_cloud_pinned"));
        assert!(err.contains("image:local"));

        let summary =
            cloud_readiness_summary_lines(response.get("cloud_readiness").unwrap()).join("\n");
        assert!(summary.contains("cloud_actions:"));
        assert!(summary.contains("[before_rebuild] package_images_not_cloud_pinned"));
    }

    #[test]
    fn unavailable_cloud_readiness_fails_after_successful_import() {
        let response = json!({
            "status": "accepted",
            "import": {
                "status": "accepted"
            },
            "cloud_readiness": {
                "status": "unavailable",
                "checks": [{
                    "name": "package_import",
                    "status": "unavailable",
                    "code": "package_import_not_accepted",
                    "message": "Hosted Cloud readiness is unavailable until the sealed package import is accepted."
                }]
            }
        });

        ensure_import_accepted(&response, "hosted build")
            .expect("accepted import should pass the import guard");
        let err = ensure_cloud_readiness(&response)
            .expect_err("unavailable readiness should fail hosted build")
            .to_string();

        assert!(err.contains("hosted build Cloud readiness is unavailable"));
        assert!(err.contains("package_import/package_import_not_accepted"));
    }

    #[test]
    fn cloud_runnable_build_requires_matching_readiness_digest() {
        let response = json!({
            "status": "cloud_runnable",
            "import": {
                "status": "accepted"
            },
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "cloud_readiness": {
                "status": "cloud_runnable",
                "package_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "checks": []
            }
        });

        ensure_import_accepted(&response, "hosted build")
            .expect("accepted import should pass the import guard");
        let err = ensure_cloud_readiness(&response)
            .expect_err("mismatched readiness digest should fail hosted build")
            .to_string();

        assert!(err.contains("package_digest mismatch"));
        assert!(err.contains("build returned sha256:aaaaaaaa"));
        assert!(err.contains("readiness checked sha256:bbbbbbbb"));
    }

    #[test]
    fn cloud_runnable_build_requires_readiness_digest() {
        let response = json!({
            "status": "cloud_runnable",
            "import": {
                "status": "accepted"
            },
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "cloud_readiness": {
                "status": "cloud_runnable",
                "checks": []
            }
        });

        ensure_import_accepted(&response, "hosted build")
            .expect("accepted import should pass the import guard");
        let err = ensure_cloud_readiness(&response)
            .expect_err("missing readiness digest should fail hosted build")
            .to_string();

        assert!(err.contains("cloud_readiness is missing package_digest"));
    }

    #[test]
    fn hosted_build_import_identity_must_match_build_fields() {
        ensure_build_import_identity(&json!({
            "build_id": "import-1",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "import": {
                "import_id": "import-1",
                "status": "accepted",
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }
        }))
        .expect("matching import identity should pass");

        ensure_build_import_identity(&json!({
            "build_id": "import-1",
            "package_digest": null,
            "import": {
                "import_id": "import-1",
                "status": "accepted",
                "package_digest": null
            }
        }))
        .expect("accepted imports without package digests are handled by readiness checks");

        let import_id_mismatch = ensure_build_import_identity(&json!({
            "build_id": "import-1",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "import": {
                "import_id": "import-2",
                "status": "accepted",
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }
        }))
        .unwrap_err()
        .to_string();
        assert!(import_id_mismatch.contains("import_id mismatch"));
        assert!(import_id_mismatch.contains("build_id import-1"));
        assert!(import_id_mismatch.contains("import_id import-2"));

        let digest_mismatch = ensure_build_import_identity(&json!({
            "build_id": "import-1",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "import": {
                "import_id": "import-1",
                "status": "accepted",
                "package_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            }
        }))
        .unwrap_err()
        .to_string();
        assert!(digest_mismatch.contains("import package_digest mismatch"));
        assert!(digest_mismatch.contains("build returned sha256:aaaaaaaa"));
        assert!(digest_mismatch.contains("import recorded sha256:bbbbbbbb"));

        let missing_import_digest = ensure_build_import_identity(&json!({
            "build_id": "import-1",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "import": {
                "import_id": "import-1",
                "status": "accepted",
                "package_digest": null
            }
        }))
        .unwrap_err()
        .to_string();
        assert!(missing_import_digest.contains("import did not record a package digest"));
    }

    #[test]
    fn hosted_authoring_build_identity_must_match_uploaded_source() {
        ensure_authoring_build_identity(
            &json!({
                "authoring_build": {
                    "status": "succeeded",
                    "source_upload_id": "upload-1",
                    "entrypoint": "experiments/peter/experiment.yaml"
                }
            }),
            "hosted_authoring_build",
            "upload-1",
            Some("experiments/peter/experiment.yaml"),
        )
        .expect("matching hosted authoring identity should pass");

        ensure_authoring_build_identity(
            &json!({
                "authoring_build": {
                    "status": "failed",
                    "code": "authoring_build_failed",
                    "error": "Core rejected the experiment YAML."
                }
            }),
            "hosted_authoring_build",
            "upload-1",
            Some("experiments/peter/experiment.yaml"),
        )
        .expect("failed authoring builds should preserve the authoring diagnostic");

        let source_mismatch = ensure_authoring_build_identity(
            &json!({
                "authoring_build": {
                    "status": "succeeded",
                    "source_upload_id": "upload-2",
                    "entrypoint": "experiments/peter/experiment.yaml"
                }
            }),
            "hosted_authoring_build",
            "upload-1",
            Some("experiments/peter/experiment.yaml"),
        )
        .unwrap_err()
        .to_string();
        assert!(source_mismatch.contains("source upload mismatch"));
        assert!(source_mismatch.contains("uploaded upload-1"));
        assert!(source_mismatch.contains("used upload-2"));

        let entrypoint_mismatch = ensure_authoring_build_identity(
            &json!({
                "authoring_build": {
                    "status": "succeeded",
                    "source_upload_id": "upload-1",
                    "entrypoint": "other/experiment.yaml"
                }
            }),
            "hosted_authoring_build",
            "upload-1",
            Some("experiments/peter/experiment.yaml"),
        )
        .unwrap_err()
        .to_string();
        assert!(entrypoint_mismatch.contains("entrypoint mismatch"));
        assert!(entrypoint_mismatch.contains("requested experiments/peter/experiment.yaml"));
        assert!(entrypoint_mismatch.contains("used other/experiment.yaml"));

        let missing_source_upload = ensure_authoring_build_identity(
            &json!({
                "authoring_build": {
                    "status": "succeeded",
                    "entrypoint": "experiments/peter/experiment.yaml"
                }
            }),
            "hosted_authoring_build",
            "upload-1",
            Some("experiments/peter/experiment.yaml"),
        )
        .unwrap_err()
        .to_string();
        assert!(missing_source_upload.contains("authoring_build.source_upload_id"));

        let missing_entrypoint = ensure_authoring_build_identity(
            &json!({
                "authoring_build": {
                    "status": "succeeded",
                    "source_upload_id": "upload-1"
                }
            }),
            "hosted_authoring_build",
            "upload-1",
            Some("experiments/peter/experiment.yaml"),
        )
        .unwrap_err()
        .to_string();
        assert!(missing_entrypoint.contains("authoring_build.entrypoint"));
    }

    #[test]
    fn sealed_package_build_identity_must_not_claim_authoring() {
        ensure_authoring_build_identity(
            &json!({
                "authoring_build": {
                    "status": "unavailable"
                }
            }),
            "sealed_package_import",
            "upload-1",
            None,
        )
        .expect("sealed package imports should explicitly skip hosted authoring");

        let wrong_status = ensure_authoring_build_identity(
            &json!({
                "authoring_build": {
                    "status": "succeeded",
                    "source_upload_id": "upload-1",
                    "entrypoint": "experiment.yaml"
                }
            }),
            "sealed_package_import",
            "upload-1",
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(wrong_status.contains("sealed package imports"));
        assert!(wrong_status.contains("authoring_build.status=unavailable"));
    }

    #[test]
    fn doctor_response_must_prove_runnable_before_run_creation() {
        ensure_doctor_runnable(&json!({
            "ok": true,
            "status": "runnable",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }), "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .expect("runnable doctor response should pass");

        let not_runnable = ensure_doctor_runnable(
            &json!({
                "ok": false,
                "status": "blocked",
                "message": "No active runner pool can satisfy this run."
            }),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap_err()
        .to_string();
        assert!(not_runnable.contains("status=blocked ok=false"));
        assert!(not_runnable.contains("No active runner pool"));

        let malformed = ensure_doctor_runnable(
            &json!({
                "status": "runnable"
            }),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap_err()
        .to_string();
        assert!(malformed.contains("status=runnable ok=false"));

        let legacy_ready = ensure_doctor_runnable(
            &json!({
                "ok": true,
                "status": "ready",
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap_err()
        .to_string();
        assert!(legacy_ready.contains("status=ready ok=true"));

        let mismatch = ensure_doctor_runnable(&json!({
            "ok": true,
            "status": "runnable",
            "package_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }), "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap_err()
        .to_string();
        assert!(mismatch.contains("package_digest mismatch"));
        assert!(mismatch.contains("requested sha256:aaaaaaaa"));
        assert!(mismatch.contains("doctor checked sha256:bbbbbbbb"));
    }

    #[test]
    fn run_creation_response_must_prove_queued_run_identity() {
        ensure_run_created(&json!({
            "run_id": "run-1",
            "status": "created",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }), "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .expect("created run response should pass");

        let missing_run_id = ensure_run_created(&json!({
            "status": "created",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }), "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap_err()
        .to_string();
        assert!(missing_run_id.contains("missing run_id"));

        let failed_status = ensure_run_created(&json!({
            "run_id": "run-1",
            "status": "failed",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }), "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap_err()
        .to_string();
        assert!(failed_status.contains("non-startable status"));
        assert!(failed_status.contains("failed"));

        let digest_mismatch = ensure_run_created(&json!({
            "run_id": "run-1",
            "status": "created",
            "package_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }), "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap_err()
        .to_string();
        assert!(digest_mismatch.contains("package_digest mismatch"));
    }

    #[test]
    fn direct_run_and_runtime_responses_must_match_requested_run() {
        ensure_run_response_matches(
            &json!({
                "run_id": "run-1",
                "status": "completed"
            }),
            "run-1",
        )
        .expect("matching run response should pass");

        let run_mismatch = ensure_run_response_matches(
            &json!({
                "run_id": "run-2",
                "status": "completed"
            }),
            "run-1",
        )
        .unwrap_err()
        .to_string();
        assert!(run_mismatch.contains("run response id mismatch"));

        ensure_runtime_response_matches(
            &json!({
                "cloud_run_id": "run-1",
                "summary": {}
            }),
            "run-1",
        )
        .expect("matching runtime response should pass");

        let runtime_mismatch = ensure_runtime_response_matches(
            &json!({
                "cloud_run_id": "run-2",
                "summary": {}
            }),
            "run-1",
        )
        .unwrap_err()
        .to_string();
        assert!(runtime_mismatch.contains("runtime response run id mismatch"));

        let missing_runtime_run_id = ensure_runtime_response_matches(
            &json!({
                "summary": {}
            }),
            "run-1",
        )
        .unwrap_err()
        .to_string();
        assert!(missing_runtime_run_id.contains("missing cloud_run_id"));

        let key_mismatch = ensure_runtime_value_response_matches(
            &json!({
                "cloud_run_id": "run-1",
                "key": "other/key",
                "values": []
            }),
            "run-1",
            "trial/status",
        )
        .unwrap_err()
        .to_string();
        assert!(key_mismatch.contains("runtime value response key mismatch"));
    }

    #[test]
    fn missing_cloud_readiness_fails_after_successful_import() {
        let response = json!({
            "status": "accepted",
            "import": {
                "status": "accepted"
            },
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });

        ensure_import_accepted(&response, "hosted build")
            .expect("accepted import should pass the import guard");
        let err = ensure_cloud_readiness(&response)
            .expect_err("missing readiness should fail hosted build")
            .to_string();

        assert!(err.contains("missing cloud_readiness"));
        assert!(err.contains("did not prove this package is runnable"));
    }

    #[test]
    fn failed_import_status_exits_nonzero_even_after_api_returns_json() {
        let response = json!({
            "build_id": "import-1",
            "status": "failed",
            "import": {
                "status": "failed",
                "error_message": "manifest.json is not a supported sealed_run_package_v2 manifest"
            }
        });

        let err = ensure_import_accepted(&response, "hosted build")
            .expect_err("failed import status should fail the CLI command")
            .to_string();

        assert!(err.contains("hosted build failed"));
        assert!(err.contains("manifest.json is not a supported"));
    }

    #[test]
    fn failed_authoring_build_status_exits_nonzero_with_authoring_reason() {
        let response = json!({
            "build_id": "build-1",
            "status": "failed",
            "authoring_build": {
                "status": "failed",
                "code": "authoring_build_timed_out",
                "error": "Hosted Core build timed out",
                "detail": {
                    "timeout_ms": 250,
                    "stderr_tail": "still resolving dependencies"
                }
            },
            "import": null
        });

        let err = ensure_import_accepted(&response, "hosted build")
            .expect_err("failed authoring build should fail with authoring diagnostics")
            .to_string();

        assert!(err.contains("hosted build failed"));
        assert!(err.contains("authoring_build_timed_out"));
        assert!(err.contains("Hosted Core build timed out"));
        assert!(err.contains("still resolving dependencies"));
        assert!(!err.contains("Cloud importer rejected"));
    }

    #[test]
    fn runtime_options_are_typed_for_cloud_api() {
        let args = vec![
            "--backend".to_string(),
            "runner-docker".to_string(),
            "--cpu-count".to_string(),
            "4".to_string(),
            "--runtime-option".to_string(),
            "isolation=single_use_vm".to_string(),
            "--runtime-option".to_string(),
            "memory_mb=8192".to_string(),
            "--runtime-option".to_string(),
            "sidecars=redis,postgres".to_string(),
            "--runtime-option".to_string(),
            r#"network={"default":"allowlist_enforced","egress":["api.openai.com"]}"#.to_string(),
        ];

        let options = runtime_options_from_args(&args).unwrap();

        assert_eq!(options["backend"], json!("runner-docker"));
        assert_eq!(options["cpu_count"], json!(4));
        assert_eq!(options["isolation"], json!("single_use_vm"));
        assert_eq!(options["memory_mb"], json!(8192));
        assert_eq!(options["sidecars"], json!(["redis", "postgres"]));
        assert_eq!(
            options["network"],
            json!({
                "default": "allowlist_enforced",
                "egress": ["api.openai.com"]
            })
        );

        let bad_key = runtime_options_from_args(&[
            "--runtime-option".to_string(),
            "materialize=copy".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(bad_key.contains("unsupported hosted Cloud runtime option `materialize`"));

        let bad_number = runtime_options_from_args(&[
            "--runtime-option".to_string(),
            "memory_mb=large".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(bad_number.contains("requires a positive integer"));

        let bad_network = runtime_options_from_args(&[
            "--runtime-option".to_string(),
            "network=none".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(bad_network.contains("requires a JSON object"));

        let duplicate_key = runtime_options_from_args(&[
            "--runtime-option".to_string(),
            "memory_mb=8192".to_string(),
            "--runtime-option".to_string(),
            "memory_mb=4096".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(duplicate_key.contains("runtime option `memory_mb` was provided more than once"));

        let conflicting_flag = runtime_options_from_args(&[
            "--memory-mb".to_string(),
            "4096".to_string(),
            "--runtime-option".to_string(),
            "memory_mb=8192".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(conflicting_flag.contains("runtime option `memory_mb` was provided more than once"));

        let conflicting_alias = runtime_options_from_args(&[
            "--backend".to_string(),
            "runner-docker".to_string(),
            "--runtime-option".to_string(),
            "executor=modal".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(conflicting_alias
            .contains("runtime options `backend` and `executor` cannot both be provided"));

        let bad_backend =
            runtime_options_from_args(&["--backend".to_string(), "cloud-runner".to_string()])
                .unwrap_err()
                .to_string();
        assert!(bad_backend.contains("must be one of runner-docker"));
    }

    #[test]
    fn secret_refs_reject_duplicate_sources_before_cloud_requests() {
        let root = temp_dir("secret_ref_duplicates");
        fs::create_dir_all(&root).unwrap();
        let secret_file = root.join("secrets.yaml");
        fs::write(
            &secret_file,
            "OPENAI_API_KEY: bucephalus://OPENAI_API_KEY\n",
        )
        .unwrap();

        let duplicate_file_and_inline = secret_refs_from_options(&[
            "--secret-ref-file".to_string(),
            secret_file.display().to_string(),
            "--secret-ref".to_string(),
            "OPENAI_API_KEY=bucephalus://OTHER_OPENAI_API_KEY".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(duplicate_file_and_inline
            .contains("secret ref `OPENAI_API_KEY` was provided more than once"));

        let both_file_aliases = secret_refs_from_options(&[
            "--secret-ref-file".to_string(),
            secret_file.display().to_string(),
            "--secrets-file".to_string(),
            secret_file.display().to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(both_file_aliases.contains("provide either --secret-ref-file or --secrets-file"));

        let _ = fs::remove_dir_all(root);
    }

    fn write_minimal_package(root: &Path) {
        fs::write(
            root.join("manifest.json"),
            r#"{"schema_version":"sealed_run_package_v2","checksums_ref":"checksums.json","package_checks_ref":"package_checks.json"}"#,
        )
        .unwrap();
        fs::write(
            root.join("checksums.json"),
            r#"{"schema_version":"sealed_package_checksums_v2","files":{}}"#,
        )
        .unwrap();
        fs::write(
            root.join("package_checks.json"),
            r#"{"schema_version":"package_checks_v1","passed":true,"checks":[],"summary":{"checks":0,"failed":0,"warnings":0}}"#,
        )
        .unwrap();
        fs::write(root.join("resolved_experiment.json"), "{}").unwrap();
    }

    fn write_cli_draft(path: &Path, id: &str, backend: &str) {
        fs::write(
            path,
            format!(
                "experiment:\n  id: {id}\n  name: {id} experiment\nruntime:\n  compute:\n    backend: {backend}\nmatrix:\n  variants:\n    - id: baseline\n  cases:\n    count: 2\n  repeats: 1\nstages: {{}}\npolicy: {{}}\n"
            ),
        )
        .unwrap();
    }

    fn archive_entries(path: &Path) -> Vec<String> {
        let file = File::open(path).unwrap();
        let decoder = GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        let mut entries = archive
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    fn archive_entries_from_bytes(bytes: &[u8]) -> Vec<String> {
        let decoder = GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(decoder);
        let mut entries = archive
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    struct MockCloudServer {
        api_url: String,
        handle: thread::JoinHandle<Vec<RecordedRequest>>,
    }

    impl MockCloudServer {
        fn start(expected_requests: usize) -> Self {
            let mut source: Option<(String, u64)> = None;
            Self::start_with_stateful_handler(expected_requests, move |request, index| {
                if request.method == "PUT" && request.path == "/v1/uploads/upload-1/content" {
                    source = Some((sha256_digest(&request.body), request.body.len() as u64));
                }
                handle_mock_cloud_request(request, index, source.as_ref())
            })
        }

        fn start_with_handler(
            expected_requests: usize,
            handler: fn(&RecordedRequest, usize) -> Value,
        ) -> Self {
            Self::start_with_stateful_handler(expected_requests, move |request, index| {
                handler(request, index)
            })
        }

        fn start_with_stateful_handler<F>(expected_requests: usize, mut handler: F) -> Self
        where
            F: FnMut(&RecordedRequest, usize) -> Value + Send + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let api_url = format!("http://{}", listener.local_addr().unwrap());
            let handle = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut requests = Vec::new();
                while requests.len() < expected_requests {
                    match listener.accept() {
                        Ok((stream, _addr)) => {
                            let index = requests.len();
                            let request = handle_mock_cloud_connection(stream, index, &mut handler);
                            requests.push(request);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                panic!(
                                    "mock Cloud API timed out after {} request(s)",
                                    requests.len()
                                );
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("mock Cloud API accept failed: {error}"),
                    }
                }
                requests
            });
            Self { api_url, handle }
        }

        fn api_url(&self) -> String {
            self.api_url.clone()
        }

        fn join(self) -> Vec<RecordedRequest> {
            self.handle.join().expect("mock Cloud API thread panicked")
        }
    }

    #[derive(Debug)]
    struct RecordedRequest {
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    }

    impl RecordedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .get(&name.to_ascii_lowercase())
                .map(String::as_str)
        }
    }

    fn handle_mock_cloud_connection(
        mut stream: TcpStream,
        index: usize,
        handler: &mut dyn FnMut(&RecordedRequest, usize) -> Value,
    ) -> RecordedRequest {
        stream
            .set_nonblocking(false)
            .expect("mock Cloud API stream should switch back to blocking reads");
        let request = read_http_request(&mut stream);
        let response_body = handler(&request, index);
        let response = serde_json::to_vec(&response_body).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            response.len()
        )
        .unwrap();
        stream.write_all(&response).unwrap();
        request
    }

    fn handle_mock_cloud_request(
        request: &RecordedRequest,
        index: usize,
        source: Option<&(String, u64)>,
    ) -> Value {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/v1/uploads") => json!({
                "upload_id": "upload-1"
            }),
            ("PUT", "/v1/uploads/upload-1/content") => json!({}),
            ("POST", "/v1/uploads/upload-1/complete") => json!({}),
            ("POST", "/v1/experiments/builds") => {
                let runtime_options = runtime_options_from_build_request(request);
                json!({
                    "build_id": "build-1",
                    "build_kind": "hosted_authoring_build",
                    "build_environment": {
                        "schema_version": "hosted_build_environment_v1",
                        "target": {
                            "kind": "hosted_cloud",
                            "name": "default"
                        },
                        "source": {
                            "input_kind": "authoring_context",
                            "upload_id": "upload-1",
                            "filename": "authoring-context.tgz",
                            "media_type": "application/gzip",
                            "content_digest": source
                                .map(|(digest, _byte_size)| digest.as_str())
                                .unwrap_or("sha256:missing-upload-content"),
                            "byte_size": source.map(|(_digest, byte_size)| *byte_size).unwrap_or(0),
                            "entrypoint": "experiments/peter/experiment.yaml"
                        },
                        "runtime_options": runtime_options,
                        "builder": {
                            "kind": "api_embedded_core",
                            "image_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                            "release_version": "0.3.37",
                            "git_sha": "abc123",
                            "os": "linux",
                            "arch": "x64"
                        },
                        "core": {
                            "command": "bucephalus build",
                            "path": "/app/bin/bucephalus",
                            "version": "0.3.37",
                            "timeout_ms": 600000
                        },
                        "package_contract": {
                            "input_kind": "authoring_context",
                            "authoring_compiler": "core_universal_v1",
                            "sealed_schema_version": "sealed_run_package_v2",
                            "readiness_schema_version": "hosted_cloud_readiness_v1",
                            "cloud_readiness_required": true
                        }
                    },
                    "authoring_build": {
                        "status": "succeeded",
                        "source_upload_id": "upload-1",
                        "entrypoint": "experiments/peter/experiment.yaml"
                    },
                    "status": "cloud_runnable",
                    "label": "demo",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "cloud_readiness": {
                        "status": "cloud_runnable",
                        "target": {
                            "kind": "hosted_cloud",
                            "name": "default"
                        },
                        "runtime_options": runtime_options,
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "package_status": "accepted",
                        "run_requirements": null,
                        "secret_requirements": [],
                        "checks": []
                    },
                    "import": {
                        "import_id": "build-1",
                        "status": "accepted",
                        "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    }
                })
            }
            ("POST", "/v1/drafts/validate") => json!({
                "valid": true,
                "issues": [],
                "resolved_refs": []
            }),
            ("POST", "/v1/drafts/canonicalize") => json!({
                "canonical_draft": {},
                "draft_digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "digest_map": [{
                    "pointer": "/matrix/variants/0",
                    "kind": "variant",
                    "content_digest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                    "resolution": "inline_existing",
                    "alias": null,
                    "display_name": "baseline"
                }],
                "issues": []
            }),
            ("POST", "/v1/drafts/resolve") => json!({
                "resolved_draft": {},
                "bindings": [{
                    "pointer": "/matrix/variants/0",
                    "kind": "variant",
                    "content_digest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                    "resolution": "inline_existing",
                    "alias": null,
                    "display_name": "baseline"
                }],
                "unresolved": [],
                "issues": []
            }),
            ("POST", "/v1/drafts/preview-schedule") => json!({
                "total_slots": 2,
                "variants": 1,
                "cases": 2,
                "repeats": 1,
                "seeds": 1,
                "max_concurrency": null,
                "warnings": []
            }),
            ("POST", "/v1/drafts/suggest") => json!({
                "suggestions": [{
                    "suggestion_type": "registry_entity",
                    "title": "Baseline Variant",
                    "detail": "aliases: baseline",
                    "score": 0.9,
                    "registry_hit": null,
                    "patch": null
                }]
            }),
            ("POST", "/v1/drafts/export") => json!({
                "format": "resolved_json",
                "body": "{\"experiment\":{\"id\":\"demo\"}}\n",
                "issues": []
            }),
            ("POST", "/v1/drafts/diff") => json!({
                "left": { "kind": "experiment_package", "inline": {} },
                "right": { "kind": "experiment_package", "inline": {} },
                "changes": [{
                    "op": "replace",
                    "pointer": "/experiment/id",
                    "left": "left",
                    "right": "right",
                    "significance": "behavior"
                }]
            }),
            ("GET", "/v1/packages?limit=7") => json!({
                "packages": [{
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "status": "accepted",
                    "name": "Demo"
                }]
            }),
            ("PUT", "/v1/secrets/OPENAI%5FAPI%5FKEY") => json!({
                "name": "OPENAI_API_KEY",
                "version": 1,
                "created_at": "2026-06-12T00:00:00Z",
                "updated_at": "2026-06-12T00:00:00Z"
            }),
            ("GET", "/v1/secrets") => json!({
                "secrets": [{
                    "name": "OPENAI_API_KEY",
                    "version": 1,
                    "created_at": "2026-06-12T00:00:00Z",
                    "updated_at": "2026-06-12T00:00:00Z"
                }]
            }),
            ("DELETE", "/v1/secrets/OPENAI%5FAPI%5FKEY") => json!({
                "name": "OPENAI_API_KEY",
                "deleted": true
            }),
            ("POST", "/v1/experiments/doctor") => json!({
                "ok": true,
                "status": "runnable",
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "package_status": "accepted",
                "name": "Demo",
                "image_refs": [],
                "secret_requirements": [{
                    "id": "GEMINI_API_KEY",
                    "target": "",
                    "required_for_variants": []
                }],
                "supplied_secret_ids": ["GEMINI_API_KEY"],
                "runtime_options": {},
                "run_requirements": {
                    "executor": "runner-docker",
                    "requires": ["core_runner", "docker_daemon", "registry_pull", "secret_resolver"]
                }
            }),
            ("POST", "/v1/runs") => json!({
                "run_id": "run-1",
                "status": "created",
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "run_label": "hosted-secret-smoke",
                "secret_ids": ["GEMINI_API_KEY"],
                "run_requirements": {
                    "executor": "runner-docker",
                    "requires": ["core_runner", "docker_daemon", "registry_pull", "secret_resolver"]
                }
            }),
            ("GET", "/v1/runs?limit=3") => json!({
                "runs": [{
                    "run_id": "run-1",
                    "status": "queued",
                    "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "run_label": "demo"
                }]
            }),
            ("GET", "/v1/runs/run%2D1/runtime") => json!({
                "cloud_run_id": "run-1",
                "summary": {
                    "status": "running"
                }
            }),
            ("GET", "/v1/runs/run%2D1/runtime/events?limit=5&after%5Frow%5Fseq=12") => json!({
                "cloud_run_id": "run-1",
                "events": [{
                    "row_seq": 13,
                    "event_type": "trial_started"
                }]
            }),
            ("GET", "/v1/runs/run%2D1/runtime/results?limit=9") => json!({
                "cloud_run_id": "run-1",
                "results": [{
                    "trial_id": "trial-1",
                    "outcome": "success"
                }]
            }),
            ("GET", "/v1/runs/run%2D1/runtime/kv/trial%2Fstatus") => json!({
                "cloud_run_id": "run-1",
                "key": "trial/status",
                "values": [{
                    "trial_id": "trial-1",
                    "value": "running"
                }]
            }),
            _ => panic!(
                "unexpected mock Cloud API request #{index}: {} {}",
                request.method, request.path
            ),
        }
    }

    fn runtime_options_from_build_request(request: &RecordedRequest) -> Value {
        serde_json::from_slice::<Value>(&request.body)
            .ok()
            .and_then(|body| body.get("runtime_options").cloned())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    }

    fn read_http_request(stream: &mut TcpStream) -> RecordedRequest {
        let mut reader = BufReader::new(stream);
        let mut first_line = String::new();
        reader.read_line(&mut first_line).unwrap();
        let mut parts = first_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();
        let mut headers = HashMap::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
        RecordedRequest {
            method,
            path,
            headers,
            body,
        }
    }
}
