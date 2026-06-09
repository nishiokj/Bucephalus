use anyhow::{anyhow, bail, Context, Result};
use flate2::read::GzDecoder;
#[cfg(test)]
use flate2::write::GzEncoder;
use flate2::Compression;
use flate2::GzBuilder;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Method;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
struct CliContext {
    api_url: String,
    user_token: Option<String>,
    worker_token: Option<String>,
    runner_admin_token: Option<String>,
    args: Vec<String>,
    client: Client,
}

#[derive(Clone, Copy, Debug)]
enum AuthMode {
    User,
    RunnerAdmin,
}

#[derive(Clone, Debug)]
struct SecretRequirement {
    id: String,
    target: String,
    required_for_variants: Vec<String>,
}

const BUCEPHALUS_CLOUD_API_URL_ENV: &str = "BUCEPHALUS_CLOUD_API_URL";
const BUCEPHALUS_CLOUD_USER_TOKEN_ENV: &str = "BUCEPHALUS_CLOUD_USER_TOKEN";
const CLOUD_RUNTIME_LIMIT_MAX: u64 = 1000;

fn main() {
    if let Err(err) = run(std::env::args().skip(1).collect()) {
        eprintln!("{}", public_error_message(&err.to_string()));
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
        _ if context
            .args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h")) =>
        {
            print_help();
            Ok(())
        }
        _ if context.api_url.is_empty() => bail!(
            "{} or --api-url is required; bucephalus-cloud only targets an explicit Cloud API",
            BUCEPHALUS_CLOUD_API_URL_ENV
        ),
        (Some("health"), _) => print_json(&cloud_fetch(
            &context,
            Method::GET,
            "/readyz",
            None,
            None,
            AuthMode::User,
        )?),
        (Some("registry"), Some("search")) => registry_search(with_args(&context, rest)),
        (Some("draft"), Some("validate")) => draft_validate(with_args(&context, rest)),
        (Some("draft"), Some("preview")) => draft_preview(with_args(&context, rest)),
        (Some("draft"), Some("export")) => draft_export(with_args(&context, rest)),
        (Some("deploy"), _) | (Some("build-upload"), _) => {
            let mut args = Vec::new();
            if let Some(command) = command {
                args.push(command.to_string());
            }
            args.extend(rest);
            build_upload(with_args(&context, args))
        }
        (Some("import"), Some("sealed-package")) => {
            import_sealed_package(with_args(&context, rest))
        }
        (Some("import"), Some("inspect")) => import_inspect(with_args(&context, rest)),
        (Some("package"), Some("get")) => package_get(with_args(&context, rest)),
        (Some("package"), Some("secrets")) => package_secrets(with_args(&context, rest)),
        (Some("run"), Some("create")) => run_create(with_args(&context, rest)),
        (Some("run"), Some("get")) => run_get(with_args(&context, rest)),
        (Some("run"), Some("runtime")) => run_runtime(with_args(&context, rest)),
        (Some("run"), Some("events")) => run_events(with_args(&context, rest)),
        (Some("run"), Some("results")) => run_results(with_args(&context, rest)),
        (Some("runner-pool"), Some("create")) => runner_pool_create(with_args(&context, rest)),
        (Some("runner-pool"), Some("list")) => runner_pool_list(with_args(&context, rest)),
        (Some("runner-instance"), Some("drain")) => {
            runner_instance_drain(with_args(&context, rest))
        }
        _ => bail!(
            "unknown command: {}",
            [group, command]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

fn with_args(context: &CliContext, args: Vec<String>) -> CliContext {
    CliContext {
        api_url: context.api_url.clone(),
        user_token: context.user_token.clone(),
        worker_token: context.worker_token.clone(),
        runner_admin_token: context.runner_admin_token.clone(),
        args,
        client: context.client.clone(),
    }
}

fn parse_global_args(argv: Vec<String>) -> Result<CliContext> {
    let mut args = argv;
    let mut api_url = std::env::var(BUCEPHALUS_CLOUD_API_URL_ENV).unwrap_or_default();
    let mut user_token = env_non_empty(BUCEPHALUS_CLOUD_USER_TOKEN_ENV);
    let mut worker_token = env_non_empty("BUCEPHALUS_CLOUD_WORKER_TOKEN");
    let mut runner_admin_token =
        env_non_empty("BUCEPHALUS_CLOUD_RUNNER_ADMIN_TOKEN").or_else(|| worker_token.clone());

    let mut index = 0;
    while index < args.len() {
        let option = args[index].as_str();
        if !matches!(
            option,
            "--api-url" | "--user-token" | "--worker-token" | "--runner-admin-token"
        ) {
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
            "--worker-token" => {
                worker_token = non_empty(value);
                if runner_admin_token.is_none() {
                    runner_admin_token = worker_token.clone();
                }
            }
            "--runner-admin-token" => runner_admin_token = non_empty(value),
            _ => unreachable!(),
        }
        args.drain(index..=index + 1);
    }
    let help_requested = cloud_help_requested(&args);
    let api_url = if help_requested {
        api_url.trim().trim_end_matches('/').to_string()
    } else {
        normalize_cloud_api_url(&api_url)?
    };
    if !help_requested && user_token.is_none() {
        user_token = shared_cloud_user_token()?;
    }

    Ok(CliContext {
        api_url,
        user_token,
        worker_token,
        runner_admin_token,
        args,
        client: Client::new(),
    })
}

fn cloud_help_requested(args: &[String]) -> bool {
    args.is_empty()
        || args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
}

fn normalize_cloud_api_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.contains('\n') || trimmed.contains('\r') {
        return Err(anyhow!(
            "invalid Cloud API URL: expected an http:// or https:// base URL"
        ));
    }
    let parsed = reqwest::Url::parse(trimmed)
        .map_err(|_| anyhow!("invalid Cloud API URL: expected an http:// or https:// base URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(anyhow!(
            "invalid Cloud API URL: expected an http:// or https:// base URL"
        ));
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(anyhow!(
            "invalid Cloud API URL: expected a base URL without credentials, query, or fragment"
        ));
    }
    Ok(trimmed.to_string())
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
    let form = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("refresh_token".to_string(), refresh_token.to_string()),
        ("client_id".to_string(), client_id.to_string()),
    ];
    let client = Client::new();
    let response = client
        .post(token_endpoint)
        .form(&form)
        .send()
        .map_err(|_| {
            anyhow!(
                "failed to refresh Cloud token at {}: transport error",
                redact_public_url(token_endpoint)
            )
        })?;
    let status = response.status().as_u16();
    let bytes = response.bytes()?.to_vec();
    if !(200..300).contains(&status) {
        let message = redacted_response_body(&bytes);
        return Err(anyhow!(
            "Cloud token refresh failed with status {}: {}",
            status,
            message
        ));
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
        ensure_secret_parent_dir(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if secret_path_is_symlink(path)? {
            return Err(anyhow!(
                "refusing to write Cloud auth token through symlinked token file"
            ));
        }
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
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

fn ensure_secret_parent_dir(parent: &Path) -> Result<()> {
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(parent)?;
        if metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "refusing to write Cloud auth token through symlinked auth directory"
            ));
        }
        fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(unix)]
fn secret_path_is_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
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

fn registry_search(context: CliContext) -> Result<()> {
    let query = option_value(&context.args, "--query")?
        .or(option_value(&context.args, "-q")?)
        .or_else(|| positional_arg(&context.args));
    let query = query.ok_or_else(|| anyhow!("registry search requires --query <text>"))?;
    let mut params = vec![format!("q={}", encode_query(&query))];
    if let Some(kind) = option_value(&context.args, "--kind")? {
        params.push(format!("kind={}", encode_query(&kind)));
    }
    print_json(&cloud_fetch(
        &context,
        Method::GET,
        &format!("/v1/registry/search?{}", params.join("&")),
        None,
        None,
        AuthMode::User,
    )?)
}

fn draft_validate(context: CliContext) -> Result<()> {
    let draft = read_draft_from_options(&context.args)?;
    print_json(&cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/validate",
        Some(json!({ "draft": draft })),
        None,
        AuthMode::User,
    )?)
}

fn draft_preview(context: CliContext) -> Result<()> {
    let draft = read_draft_from_options(&context.args)?;
    print_json(&cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/preview-schedule",
        Some(json!({ "draft": draft })),
        None,
        AuthMode::User,
    )?)
}

fn draft_export(context: CliContext) -> Result<()> {
    let draft_path = required_option(&context.args, "--file")?;
    let out_dir = PathBuf::from(required_option(&context.args, "--out")?);
    let format = option_value(&context.args, "--format")?.unwrap_or_else(|| "yaml".to_string());
    let draft = read_draft_file(Path::new(&draft_path))?;
    let response = cloud_fetch(
        &context,
        Method::POST,
        "/v1/drafts/export",
        Some(json!({ "draft": draft, "format": format })),
        None,
        AuthMode::User,
    )?;
    let body = response
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("draft export response did not include a string body"))?;
    fs::create_dir_all(&out_dir)?;
    let filename = if format == "resolved_json" {
        "resolved_experiment.json"
    } else {
        "experiment.yaml"
    };
    let target = out_dir.join(filename);
    fs::write(&target, body)?;
    print_json(&draft_export_output(&response, &format, filename))
}

fn draft_export_output(response: &Value, requested_format: &str, filename: &str) -> Value {
    json!({
        "export_ref": public_draft_export_output_ref(),
        "source_ref": public_draft_source_ref(),
        "filename": filename,
        "format": response
            .get("format")
            .cloned()
            .unwrap_or_else(|| json!(requested_format)),
        "issues": response.get("issues").cloned().unwrap_or_else(|| json!([]))
    })
}

fn public_draft_export_output_ref() -> &'static str {
    "draft-export://output"
}

fn public_draft_source_ref() -> &'static str {
    "draft://source"
}

fn build_upload(context: CliContext) -> Result<()> {
    let experiment = positional_or_required_option(&context.args, "--file")?;
    let label = option_value(&context.args, "--label")?;
    let overrides = option_value(&context.args, "--overrides")?;
    let out_dir = option_value(&context.args, "--out")?.map(PathBuf::from);
    let archive_out = option_value(&context.args, "--archive-out")?.map(PathBuf::from);
    let core_command = option_value(&context.args, "--core-cmd")?
        .or_else(|| env_non_empty("BUCEPHALUS_CORE_CLI"))
        .or_else(|| env_non_empty("BUCEPHALUS_CORE_RUNNER_CMD"))
        .unwrap_or_else(|| "bucephalus".to_string());
    let tmp_root = make_temp_dir("buc-cloud-build-upload")?;
    let package_dir = out_dir.clone().unwrap_or_else(|| tmp_root.join("package"));
    let archive_path = archive_out
        .clone()
        .unwrap_or_else(|| tmp_root.join("package.tgz"));

    let result = (|| -> Result<Value> {
        run_core_build(
            &core_command,
            &experiment,
            &package_dir,
            overrides.as_deref(),
        )?;
        create_package_archive(&package_dir, &archive_path)?;
        let imported = upload_sealed_package_artifact(&context, &archive_path, label.as_deref())?;
        Ok(build_upload_output(
            imported,
            out_dir.as_deref(),
            archive_out.as_deref(),
        ))
    })();

    let cleanup = cleanup_temp_dir(&tmp_root, public_build_upload_temp_ref());
    match (result, cleanup) {
        (Ok(output), Ok(())) => print_json(&output),
        (Ok(_), Err(err)) => Err(err),
        (Err(err), Ok(())) => Err(err),
        (Err(err), Err(cleanup_err)) => Err(anyhow!("{err}\n{cleanup_err}")),
    }
}

fn import_sealed_package(context: CliContext) -> Result<()> {
    let path = positional_or_required_option(&context.args, "--file")?;
    let label = option_value(&context.args, "--label")?;
    print_json(&upload_sealed_package_artifact(
        &context,
        Path::new(&path),
        label.as_deref(),
    )?)
}

fn upload_sealed_package_artifact(
    context: &CliContext,
    path: &Path,
    label: Option<&str>,
) -> Result<Value> {
    validate_sealed_package_archive(path)?;
    let archive_label = public_path_label(path);
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read sealed package archive {archive_label}"))?;
    let expected_digest = sha256_digest(&bytes);
    let filename = upload_filename_for_sealed_package_archive(path);
    let upload = cloud_fetch(
        context,
        Method::POST,
        "/v1/uploads",
        Some(json!({
            "filename": filename,
            "media_type": media_type_for_path(path),
            "expected_digest": expected_digest,
            "byte_size": bytes.len()
        })),
        None,
        AuthMode::User,
    )?;
    let upload_id = upload
        .get("upload_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("upload response did not include a non-empty upload_id"))?;
    cloud_fetch(
        context,
        Method::PUT,
        &cloud_upload_content_path(upload_id),
        None,
        Some((bytes, "application/octet-stream")),
        AuthMode::User,
    )?;
    cloud_fetch(
        context,
        Method::POST,
        &cloud_upload_complete_path(upload_id),
        Some(json!({})),
        None,
        AuthMode::User,
    )?;
    cloud_fetch(
        context,
        Method::POST,
        "/v1/imports/sealed-package",
        Some(json!({ "upload_id": upload_id, "label": label })),
        None,
        AuthMode::User,
    )
}

fn build_upload_output(
    imported: Value,
    out_dir: Option<&Path>,
    archive_out: Option<&Path>,
) -> Value {
    let mut output = imported.as_object().cloned().unwrap_or_else(|| {
        let mut object = Map::new();
        object.insert("import".to_string(), imported);
        object
    });
    if out_dir.is_some() {
        output.insert(
            "package_ref".to_string(),
            json!(public_build_upload_package_ref()),
        );
    }
    if archive_out.is_some() {
        output.insert(
            "archive_ref".to_string(),
            json!(public_build_upload_archive_ref()),
        );
    }
    Value::Object(output)
}

fn public_build_upload_package_ref() -> &'static str {
    "cloud-upload://package"
}

fn public_build_upload_archive_ref() -> &'static str {
    "cloud-upload://archive"
}

fn public_build_upload_temp_ref() -> &'static str {
    "cloud-upload://temp"
}

fn cloud_import_path(import_id: &str) -> String {
    format!("/v1/imports/{}", encode_path_segment(import_id))
}

fn cloud_upload_content_path(upload_id: &str) -> String {
    format!("/v1/uploads/{}/content", encode_path_segment(upload_id))
}

fn cloud_upload_complete_path(upload_id: &str) -> String {
    format!("/v1/uploads/{}/complete", encode_path_segment(upload_id))
}

fn validate_sealed_package_archive(path: &Path) -> Result<()> {
    let archive_label = public_path_label(path);
    let file = File::open(path)
        .with_context(|| format!("failed to open sealed package archive {archive_label}"))?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = BTreeMap::<String, Vec<u8>>::new();

    for entry in archive
        .entries()
        .with_context(|| format!("failed to read sealed package archive {archive_label}"))?
    {
        let mut entry = entry.with_context(|| {
            format!("failed to read entry in sealed package archive {archive_label}")
        })?;
        let raw_path = entry.path()?;
        let raw_path = raw_path
            .to_str()
            .ok_or_else(|| anyhow!("sealed package archive entry path must be valid UTF-8"))?;
        let rel = package_archive_relative_path(raw_path, "archive entry")?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() {
            bail!(
                "sealed package archive must contain regular file entries only\n\nentry_ref: {}",
                public_archive_entry_ref(&rel)
            );
        }
        validate_sealed_package_archive_header(entry.header())?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).with_context(|| {
            format!(
                "failed to read archive entry {}",
                public_archive_entry_ref(&rel)
            )
        })?;
        if entries.insert(rel.clone(), bytes).is_some() {
            bail!(
                "sealed package archive contains duplicate entry\n\nentry_ref: {}",
                public_archive_entry_ref(&rel)
            );
        }
    }

    let manifest_bytes = entries.get("manifest.json").ok_or_else(|| {
        anyhow!(
            "sealed package archive must contain manifest.json at the archive root. Build one with `bucephalus build <experiment.yaml> --out <package-dir>` or `bucephalus-cloud deploy <experiment.yaml> --archive-out <package.tgz>`."
        )
    })?;
    let manifest = parse_archive_json(manifest_bytes, "manifest.json")?;
    if manifest.pointer("/schema_version").and_then(Value::as_str) != Some("sealed_run_package_v2")
    {
        bail!(
            "sealed package archive manifest must use schema_version sealed_run_package_v2\n\nentry_ref: {}",
            public_archive_entry_ref("manifest.json")
        );
    }

    let checksums_ref = manifest
        .pointer("/checksums_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("sealed package archive manifest is missing checksums_ref"))?;
    let checksums_rel = package_archive_relative_path(checksums_ref, "checksums_ref")?;
    let checksums_bytes = entries.get(&checksums_rel).ok_or_else(|| {
        anyhow!(
            "sealed package archive is missing checksums file referenced by manifest\n\nentry_ref: {}",
            public_archive_entry_ref(&checksums_rel)
        )
    })?;
    let checksums = parse_archive_json(checksums_bytes, &checksums_rel)?;
    if checksums.pointer("/schema_version").and_then(Value::as_str)
        != Some("sealed_package_checksums_v2")
    {
        bail!(
            "sealed package archive checksums must use schema_version sealed_package_checksums_v2\n\nentry_ref: {}",
            public_archive_entry_ref(&checksums_rel)
        );
    }
    let checksum_files = checksums
        .pointer("/files")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            anyhow!("sealed package archive checksums are missing object field files")
        })?;

    let mut expected_entries = BTreeSet::from([
        "manifest.json".to_string(),
        "package.lock".to_string(),
        checksums_rel,
    ]);
    if let Some(package_checks_ref) = manifest
        .pointer("/package_checks_ref")
        .and_then(Value::as_str)
    {
        expected_entries.insert(package_archive_relative_path(
            package_checks_ref,
            "package_checks_ref",
        )?);
    }

    for (raw_rel, expected_digest) in checksum_files {
        let rel = package_archive_relative_path(raw_rel, "checksums.files")?;
        let expected = expected_digest.as_str().ok_or_else(|| {
            anyhow!(
                "sealed package archive checksum entry must be a string digest\n\nentry_ref: {}",
                public_archive_entry_ref(&rel)
            )
        })?;
        let bytes = entries.get(&rel).ok_or_else(|| {
            anyhow!(
                "sealed package archive is missing checksummed file\n\nentry_ref: {}",
                public_archive_entry_ref(&rel)
            )
        })?;
        let actual = sha256_digest(bytes);
        if !actual.eq_ignore_ascii_case(expected) {
            bail!(
                "sealed package archive checksum mismatch\n\nentry_ref: {}\nexpected: {}\nactual: {}",
                public_archive_entry_ref(&rel),
                expected,
                actual
            );
        }
        expected_entries.insert(rel);
    }

    for expected in &expected_entries {
        if !entries.contains_key(expected) {
            bail!(
                "sealed package archive is missing required entry\n\nentry_ref: {}",
                public_archive_entry_ref(expected)
            );
        }
    }
    let extras = entries
        .keys()
        .filter(|entry| !expected_entries.contains(*entry))
        .cloned()
        .collect::<Vec<_>>();
    if !extras.is_empty() {
        bail!(
            "sealed package archive contains file(s) not declared by manifest/checksums\n\nextra_count: {}\nextra_refs: {}\n\nNext steps:\n  Rebuild the archive with `bucephalus-cloud deploy <experiment.yaml> --archive-out <package.tgz>`.",
            extras.len(),
            public_archive_entry_refs(&extras)
        );
    }

    Ok(())
}

fn validate_sealed_package_archive_header(header: &tar::Header) -> Result<()> {
    let uid = header.uid().context("failed to read archive entry uid")?;
    let gid = header.gid().context("failed to read archive entry gid")?;
    if uid != 0 || gid != 0 {
        bail!("sealed package archive tar entries must use normalized uid/gid 0");
    }
    let mtime = header
        .mtime()
        .context("failed to read archive entry mtime")?;
    if mtime != 0 {
        bail!("sealed package archive tar entries must use normalized mtime 0");
    }
    let mode = header.mode().context("failed to read archive entry mode")?;
    if mode & 0o777 != 0o644 {
        bail!("sealed package archive tar entries must use normalized file mode 0644");
    }
    Ok(())
}

fn parse_archive_json(bytes: &[u8], entry_name: &str) -> Result<Value> {
    serde_json::from_slice(bytes).with_context(|| {
        format!(
            "failed to parse sealed package archive JSON entry {}",
            public_archive_entry_ref(entry_name)
        )
    })
}

fn run_core_build(
    core_command: &str,
    experiment: &str,
    package_dir: &Path,
    overrides: Option<&str>,
) -> Result<()> {
    let mut command = Command::new(core_command);
    command
        .arg("build")
        .arg(experiment)
        .arg("--out")
        .arg(package_dir)
        .arg("--json");
    if let Some(overrides) = overrides {
        command.arg("--overrides").arg(overrides);
    }
    let output = command
        .output()
        .with_context(|| format!("failed to run Core build command {core_command}"))?;
    if !output.status.success() {
        let text = if output.stderr.is_empty() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).to_string()
        };
        bail!(
            "Core build failed with exit {}: {}",
            output.status.code().unwrap_or(1),
            tail(&text, 4000)
        );
    }
    Ok(())
}

fn create_package_archive(package_dir: &Path, archive_path: &Path) -> Result<()> {
    if !package_dir.is_dir() {
        bail!(
            "build output directory does not exist: {}",
            public_path_label(package_dir)
        );
    }
    ensure_archive_outside_package_dir(package_dir, archive_path)?;
    let entries = sealed_package_archive_entries(package_dir)?;
    let archive_label = public_path_label(archive_path);
    if let Some(parent) = archive_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        ensure_archive_output_parent(parent)?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory for {archive_label}"))?;
    }
    if let Ok(metadata) = fs::symlink_metadata(archive_path) {
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing to write sealed package archive through symlink\n\narchive_ref: {}",
                public_build_upload_archive_ref()
            );
        }
    }
    let file = File::create(archive_path)
        .with_context(|| format!("failed to create sealed package archive {archive_label}"))?;
    let encoder = GzBuilder::new()
        .mtime(0)
        .write(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    for entry in entries {
        let path = package_dir.join(&entry);
        let metadata = fs::symlink_metadata(&path).with_context(|| {
            format!(
                "failed to inspect package entry {}",
                public_archive_entry_ref(&entry)
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "sealed package archive entry must be a regular file\n\nentry_ref: {}",
                public_archive_entry_ref(&entry)
            );
        }
        let mut file = File::open(&path).with_context(|| {
            format!(
                "failed to open package entry {}",
                public_archive_entry_ref(&entry)
            )
        })?;
        let mut header = tar::Header::new_ustar();
        header.set_size(metadata.len());
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, Path::new(&entry), &mut file)
            .with_context(|| {
                format!(
                    "failed to append package entry {}",
                    public_archive_entry_ref(&entry)
                )
            })?;
    }
    builder
        .finish()
        .with_context(|| format!("failed to finish sealed package archive {archive_label}"))?;
    Ok(())
}

fn sealed_package_archive_entries(package_dir: &Path) -> Result<Vec<String>> {
    let manifest_path = package_dir.join("manifest.json");
    let manifest = read_json_file(&manifest_path, "manifest.json")?;
    if manifest.pointer("/schema_version").and_then(Value::as_str) != Some("sealed_run_package_v2")
    {
        bail!(
            "build output is not a sealed Bucephalus package: {}",
            public_path_label(package_dir)
        );
    }

    let checksums_ref = manifest
        .pointer("/checksums_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("sealed package manifest is missing checksums_ref"))?;
    let checksums_rel = package_archive_relative_path(checksums_ref, "checksums_ref")?;
    let checksums_path = package_dir.join(&checksums_rel);
    let checksums = read_json_file(&checksums_path, &checksums_rel)?;
    if checksums.pointer("/schema_version").and_then(Value::as_str)
        != Some("sealed_package_checksums_v2")
    {
        bail!("sealed package checksums must use schema_version sealed_package_checksums_v2");
    }
    let checksum_files = checksums
        .pointer("/files")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("sealed package checksums are missing object field files"))?;

    let mut entries = BTreeSet::from([
        "manifest.json".to_string(),
        "package.lock".to_string(),
        checksums_rel,
    ]);
    if let Some(package_checks_ref) = manifest
        .pointer("/package_checks_ref")
        .and_then(Value::as_str)
    {
        entries.insert(package_archive_relative_path(
            package_checks_ref,
            "package_checks_ref",
        )?);
    }

    for (raw_rel, expected_digest) in checksum_files {
        let rel = package_archive_relative_path(raw_rel, "checksums.files")?;
        let expected = expected_digest.as_str().ok_or_else(|| {
            anyhow!(
                "sealed package checksum entry must be a string digest\n\nentry_ref: {}",
                public_archive_entry_ref(&rel)
            )
        })?;
        let path = package_dir.join(&rel);
        let bytes = fs::read(&path).with_context(|| {
            format!(
                "failed to read checksummed package file {}",
                public_archive_entry_ref(&rel)
            )
        })?;
        let actual = sha256_digest(&bytes);
        if !actual.eq_ignore_ascii_case(expected) {
            bail!(
                "sealed package checksum mismatch\n\nentry_ref: {}\nexpected: {}\nactual: {}",
                public_archive_entry_ref(&rel),
                expected,
                actual
            );
        }
        entries.insert(rel);
    }

    let mut out = entries.into_iter().collect::<Vec<_>>();
    out.sort();
    if out.is_empty() {
        bail!(
            "sealed package archive would be empty: {}",
            public_path_label(package_dir)
        );
    }
    Ok(out)
}

fn read_json_file(path: &Path, label: &str) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("failed to read {label}"))?)
        .with_context(|| format!("failed to parse JSON {label}"))
}

fn public_path_label(path: &Path) -> String {
    path.file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "[REDACTED:local-path]".to_string())
}

fn public_archive_entry_ref(rel: &str) -> String {
    if archive_entry_ref_needs_redaction(rel) {
        return "archive-entry://redacted".to_string();
    }
    let parts = rel
        .replace('\\', "/")
        .split('/')
        .filter_map(|part| match part {
            "" | "." => None,
            ".." => Some("parent".to_string()),
            value => Some(public_ref_path_component(value)),
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "archive-entry://entry".to_string()
    } else {
        format!("archive-entry://{}", parts.join("/"))
    }
}

fn public_archive_entry_refs(entries: &[String]) -> String {
    let mut refs = entries
        .iter()
        .take(5)
        .map(|entry| public_archive_entry_ref(entry))
        .collect::<Vec<_>>();
    if entries.len() > refs.len() {
        refs.push(format!("+{} more", entries.len() - refs.len()));
    }
    refs.join(", ")
}

fn archive_entry_ref_needs_redaction(rel: &str) -> bool {
    rel.replace('\\', "/").split('/').any(|part| {
        let lower = part.to_ascii_lowercase();
        lower == ".env"
            || lower.ends_with(".env")
            || lower.contains("secret")
            || lower.contains("token")
            || lower.contains("password")
            || lower.contains("credential")
            || lower.contains("api_key")
            || lower.contains("private")
    })
}

fn public_ref_path_component(value: &str) -> String {
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
        "entry".to_string()
    } else {
        trimmed.to_string()
    }
}

fn package_archive_relative_path(raw: &str, field_name: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("{field_name} must not be empty");
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        bail!("{field_name} must be relative to the package root");
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| anyhow!("{field_name} must be valid UTF-8"))?;
                if part.is_empty() {
                    bail!("{field_name} must not contain empty path components");
                }
                parts.push(part.to_string());
            }
            Component::CurDir => {}
            _ => bail!("{field_name} must not contain traversal or non-normal path components"),
        }
    }
    if parts.is_empty() {
        bail!("{field_name} must name a file");
    }
    Ok(parts.join("/"))
}

fn ensure_archive_outside_package_dir(package_dir: &Path, archive_path: &Path) -> Result<()> {
    let package_abs = absolute_normalized_path(package_dir)?;
    let archive_abs = absolute_normalized_path(archive_path)?;
    if archive_abs == package_abs || archive_abs.starts_with(&package_abs) {
        bail!(
            "archive output must not be inside the sealed package directory: {}",
            public_path_label(archive_path)
        );
    }
    Ok(())
}

fn ensure_archive_output_parent(parent: &Path) -> Result<()> {
    let archive_ref = public_build_upload_archive_ref();
    for ancestor in parent.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "refusing to write sealed package archive under symlinked output directory\n\narchive_ref: {archive_ref}"
                );
            }
            Ok(metadata) if !metadata.is_dir() => {
                bail!(
                    "sealed package archive output parent exists but is not a directory\n\narchive_ref: {archive_ref}"
                );
            }
            Ok(_) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                bail!(
                    "failed to inspect sealed package archive output directory\n\narchive_ref: {archive_ref}\n\nerror: {}",
                    public_error_message(&err.to_string())
                );
            }
        }
    }
    Ok(())
}

fn absolute_normalized_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(normalize_path_components(&absolute))
}

fn normalize_path_components(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn import_inspect(context: CliContext) -> Result<()> {
    let import_id = positional_or_required_option(&context.args, "--import-id")?;
    let job = cloud_fetch(
        &context,
        Method::GET,
        &cloud_import_path(&import_id),
        None,
        None,
        AuthMode::User,
    )?;
    if context.args.iter().any(|arg| arg == "--json") {
        print_json(&job)
    } else {
        print_import_summary(&job)
    }
}

fn package_get(context: CliContext) -> Result<()> {
    let digest = positional_or_required_option(&context.args, "--package-digest")?;
    let digest = normalize_cloud_package_digest(&digest)?;
    print_json(&package_get_object(&context, &digest)?)
}

fn package_secrets(context: CliContext) -> Result<()> {
    let digest = positional_or_required_option(&context.args, "--package-digest")?;
    let digest = normalize_cloud_package_digest(&digest)?;
    let package = package_get_object(&context, &digest)?;
    let requirements = secret_requirements_from_package(&package);
    if context.args.iter().any(|arg| arg == "--json") {
        print_json(
            &json!({ "package_digest": digest, "secret_requirements": requirements_to_json(&requirements) }),
        )
    } else {
        print_secret_requirements(&digest, &requirements)
    }
}

fn run_create(context: CliContext) -> Result<()> {
    let package_digest = required_option(&context.args, "--package-digest")?;
    let package_digest = normalize_cloud_package_digest(&package_digest)?;
    let secret_refs = secret_refs_from_options(&context.args)?;
    let env = key_value_options(&context.args, "--env")?;
    let allow_secret_env = context.args.iter().any(|arg| arg == "--allow-secret-env");
    validate_cloud_run_plain_env(&env, allow_secret_env)?;
    if !context
        .args
        .iter()
        .any(|arg| arg == "--no-secret-preflight")
    {
        let package = package_get_object(&context, &package_digest)?;
        validate_secret_refs_for_package(
            &secret_refs,
            &secret_requirements_from_package(&package),
        )?;
    }

    let mut runtime_options = Map::new();
    insert_option_string(
        &mut runtime_options,
        "backend",
        option_value(&context.args, "--backend")?,
    );
    insert_option_string(
        &mut runtime_options,
        "materialize",
        option_value(&context.args, "--materialize")?,
    );
    insert_option_string(
        &mut runtime_options,
        "arch",
        option_value(&context.args, "--arch")?,
    );
    insert_option_string(
        &mut runtime_options,
        "isolation",
        option_value(&context.args, "--isolation")?,
    );
    insert_option_number(
        &mut runtime_options,
        "cpu_count",
        number_option(&context.args, "--cpu-count")?,
    );
    insert_option_number(
        &mut runtime_options,
        "memory_mb",
        number_option(&context.args, "--memory-mb")?,
    );
    insert_option_number(
        &mut runtime_options,
        "disk_mb",
        number_option(&context.args, "--disk-mb")?,
    );
    insert_option_number(
        &mut runtime_options,
        "timeout_ms",
        number_option(&context.args, "--timeout-ms")?,
    );
    insert_option_number(
        &mut runtime_options,
        "max_parallel_trials",
        number_option(&context.args, "--max-parallel-trials")?,
    );
    if context.args.iter().any(|arg| arg == "--smoke-test") {
        runtime_options.insert("smoke_test".to_string(), json!(true));
    }

    print_json(&cloud_fetch(
        &context,
        Method::POST,
        "/v1/runs",
        Some(json!({
            "package_digest": package_digest,
            "run_label": option_value(&context.args, "--label")?,
            "env": env,
            "allow_secret_env": allow_secret_env,
            "secret_refs": secret_refs,
            "runtime_options": Value::Object(runtime_options)
        })),
        None,
        AuthMode::User,
    )?)
}

fn run_get(context: CliContext) -> Result<()> {
    let run_id = positional_or_required_option(&context.args, "--run-id")?;
    print_json(&cloud_fetch(
        &context,
        Method::GET,
        &format!("/v1/runs/{}", encode_path_segment(&run_id)),
        None,
        None,
        AuthMode::User,
    )?)
}

fn run_runtime(context: CliContext) -> Result<()> {
    print_json(&cloud_fetch(
        &context,
        Method::GET,
        &cloud_run_runtime_path(&context.args, CloudRunRuntimeEndpoint::Summary)?,
        None,
        None,
        AuthMode::User,
    )?)
}

fn run_events(context: CliContext) -> Result<()> {
    print_json(&cloud_fetch(
        &context,
        Method::GET,
        &cloud_run_runtime_path(&context.args, CloudRunRuntimeEndpoint::Events)?,
        None,
        None,
        AuthMode::User,
    )?)
}

fn run_results(context: CliContext) -> Result<()> {
    print_json(&cloud_fetch(
        &context,
        Method::GET,
        &cloud_run_runtime_path(&context.args, CloudRunRuntimeEndpoint::Results)?,
        None,
        None,
        AuthMode::User,
    )?)
}

#[derive(Clone, Copy, Debug)]
enum CloudRunRuntimeEndpoint {
    Summary,
    Events,
    Results,
}

fn cloud_run_runtime_path(args: &[String], endpoint: CloudRunRuntimeEndpoint) -> Result<String> {
    let run_id = positional_or_required_option(args, "--run-id")?;
    let mut path = format!("/v1/runs/{}/runtime", encode_path_segment(&run_id));
    match endpoint {
        CloudRunRuntimeEndpoint::Summary => {
            if let Some(key) = option_value(args, "--key")? {
                if key.trim().is_empty() {
                    bail!("--key requires a non-empty runtime key");
                }
                path.push_str("/kv/");
                path.push_str(&encode_path_segment(&key));
            }
            reject_runtime_option(args, "--limit", "`run events` or `run results`")?;
            reject_runtime_option(args, "--after-row-seq", "`run events`")?;
        }
        CloudRunRuntimeEndpoint::Events => {
            reject_runtime_option(args, "--key", "`run runtime --key`")?;
            path.push_str("/events");
            append_runtime_query_params(args, &mut path, true)?;
        }
        CloudRunRuntimeEndpoint::Results => {
            reject_runtime_option(args, "--key", "`run runtime --key`")?;
            path.push_str("/results");
            append_runtime_query_params(args, &mut path, false)?;
        }
    }
    Ok(path)
}

fn append_runtime_query_params(
    args: &[String],
    path: &mut String,
    allow_after_row_seq: bool,
) -> Result<()> {
    let mut params = Vec::new();
    if let Some(limit) = bounded_number_option(args, "--limit", CLOUD_RUNTIME_LIMIT_MAX)? {
        params.push(format!("limit={limit}"));
    }
    if let Some(after_row_seq) = nonnegative_number_option(args, "--after-row-seq")? {
        if !allow_after_row_seq {
            bail!("--after-row-seq is only supported for `run events`");
        }
        params.push(format!("after_row_seq={after_row_seq}"));
    }
    if !params.is_empty() {
        path.push('?');
        path.push_str(&params.join("&"));
    }
    Ok(())
}

fn reject_runtime_option(args: &[String], name: &str, supported_by: &str) -> Result<()> {
    if option_value(args, name)?.is_some() {
        bail!("{name} is only supported for {supported_by}");
    }
    Ok(())
}

fn runner_pool_create(context: CliContext) -> Result<()> {
    let isolation = csv_option(&context.args, "--isolation", &[])?;
    let mut capabilities = Map::new();
    capabilities.insert(
        "executors".to_string(),
        json!(csv_option(
            &context.args,
            "--executors",
            &["runner-docker".to_string()]
        )?),
    );
    capabilities.insert(
        "resources".to_string(),
        json!(csv_option(
            &context.args,
            "--resources",
            &[
                "core_runner".to_string(),
                "docker_daemon".to_string(),
                "registry_pull".to_string()
            ]
        )?),
    );
    insert_option_string(
        &mut capabilities,
        "arch",
        option_value(&context.args, "--arch")?,
    );
    insert_option_number(
        &mut capabilities,
        "cpu_count",
        number_option(&context.args, "--cpu-count")?,
    );
    insert_option_number(
        &mut capabilities,
        "memory_mb",
        number_option(&context.args, "--memory-mb")?,
    );
    insert_option_number(
        &mut capabilities,
        "disk_mb",
        number_option(&context.args, "--disk-mb")?,
    );
    if !isolation.is_empty() {
        capabilities.insert("isolation".to_string(), json!(isolation));
    }
    print_json(&cloud_fetch(
        &context,
        Method::POST,
        "/v1/runner-pools",
        Some(json!({
            "name": required_option(&context.args, "--name")?,
            "capabilities": Value::Object(capabilities),
            "metadata": {}
        })),
        None,
        AuthMode::RunnerAdmin,
    )?)
}

fn runner_pool_list(context: CliContext) -> Result<()> {
    print_json(&cloud_fetch(
        &context,
        Method::GET,
        "/v1/runner-pools",
        None,
        None,
        AuthMode::RunnerAdmin,
    )?)
}

fn runner_instance_drain(context: CliContext) -> Result<()> {
    print_json(&cloud_fetch(
        &context,
        Method::POST,
        &runner_instance_drain_path(&context.args)?,
        Some(json!({})),
        None,
        AuthMode::RunnerAdmin,
    )?)
}

fn runner_instance_drain_path(args: &[String]) -> Result<String> {
    let runner_instance_id = positional_or_required_option(args, "--runner-instance-id")?;
    Ok(format!(
        "/v1/runner-instances/{}/drain",
        encode_path_segment(&runner_instance_id)
    ))
}

fn package_get_object(context: &CliContext, digest: &str) -> Result<Value> {
    let digest = normalize_cloud_package_digest(digest)?;
    let value = cloud_fetch(
        context,
        Method::GET,
        &format!("/v1/packages/{}", encode_path_segment(&digest)),
        None,
        None,
        AuthMode::User,
    )?;
    if !value.is_object() {
        bail!("package response was not an object");
    }
    Ok(value)
}

fn read_draft_from_options(args: &[String]) -> Result<Value> {
    read_draft_file(Path::new(&required_option(args, "--file")?))
}

fn read_draft_file(path: &Path) -> Result<Value> {
    let source_ref = public_draft_source_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read draft file {source_ref}"))?;
    let parsed = if path.extension().and_then(|value| value.to_str()) == Some("json") {
        serde_json::from_str(&raw)?
    } else {
        let yaml: serde_yaml::Value = serde_yaml::from_str(&raw)?;
        serde_json::to_value(yaml)?
    };
    if !parsed.is_object() {
        bail!("draft file must parse to an object: {source_ref}");
    }
    Ok(parsed)
}

fn cloud_fetch(
    context: &CliContext,
    method: Method,
    path: &str,
    body: Option<Value>,
    raw_body: Option<(Vec<u8>, &str)>,
    auth: AuthMode,
) -> Result<Value> {
    let mut headers = HeaderMap::new();
    if let Some(token) = match auth {
        AuthMode::User => context.user_token.as_ref(),
        AuthMode::RunnerAdmin => context
            .runner_admin_token
            .as_ref()
            .or(context.worker_token.as_ref()),
    } {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .context("invalid bearer token header")?,
        );
    }

    let url = format!("{}{}", context.api_url, path);
    let mut request = context.client.request(method, &url).headers(headers);
    if let Some((bytes, content_type)) = raw_body {
        request = request.header(CONTENT_TYPE, content_type).body(bytes);
    } else if let Some(body) = body {
        request = request
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&body)?);
    }
    let response = request.send().map_err(|_| {
        anyhow!(
            "failed to send Cloud API request to {}: transport error",
            redact_public_url(&url)
        )
    })?;
    let status = response.status();
    let text = response.text().with_context(|| {
        format!(
            "failed to read Cloud API response from {}",
            redact_public_url(&url)
        )
    })?;
    let payload = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| json!({ "message": text }))
    };
    if !status.is_success() {
        let mut public_payload = payload.clone();
        redact_response_json(&mut public_payload);
        let mut message = public_payload
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                if text.trim().is_empty() {
                    format!("Cloud API request failed: {}", status.as_u16())
                } else {
                    redacted_response_body(text.as_bytes())
                }
            });
        if status.as_u16() == 401 && matches!(auth, AuthMode::User) {
            message = append_user_auth_hint(&context, message);
        }
        bail!("{message}");
    }
    Ok(payload)
}

fn append_user_auth_hint(context: &CliContext, message: String) -> String {
    let token_source = if context.user_token.is_some() {
        "The CLI did send a user bearer token, so the token may be expired, malformed, or for the wrong Cloud API audience."
    } else {
        "The CLI did not find a user bearer token before making this request."
    };

    format!(
        "{message}\n\nCloud auth required.\n{token_source}\nAuthenticate with one of:\n  - bucephalus login\n  - export {BUCEPHALUS_CLOUD_USER_TOKEN_ENV}=<oauth-access-token>\n  - or write an access token to <BUCEPHALUS_HOME>/auth/cloud_user_token\n\nThen verify with: bucephalus setup status --json"
    )
}

fn redacted_response_body(bytes: &[u8]) -> String {
    let raw = String::from_utf8_lossy(bytes);
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }
    if let Ok(mut value) = serde_json::from_slice::<Value>(bytes) {
        redact_response_json(&mut value);
        let rendered = serde_json::to_string(&value).unwrap_or_else(|_| trimmed.to_string());
        return truncate_response_message(&rendered);
    }
    let redacted = trimmed
        .lines()
        .map(|line| {
            response_redaction_for_string(line)
                .map(str::to_string)
                .unwrap_or_else(|| line.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");
    truncate_response_message(&redacted)
}

fn truncate_response_message(message: &str) -> String {
    const MAX_RESPONSE_MESSAGE_CHARS: usize = 1000;
    let mut out = message
        .chars()
        .take(MAX_RESPONSE_MESSAGE_CHARS)
        .collect::<String>();
    if message.chars().count() > MAX_RESPONSE_MESSAGE_CHARS {
        out.push_str("... [truncated]");
    }
    out
}

fn public_error_message(message: &str) -> String {
    let redacted = message
        .lines()
        .map(redact_public_error_line)
        .collect::<Vec<_>>()
        .join("\n");
    truncate_response_message(&redacted)
}

fn redact_public_error_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if lower.contains("authorization:") || lower.contains("bearer ") {
        return "[REDACTED:secret-like]".to_string();
    }
    let path_redacted = redact_local_paths_in_text(line);
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
        return response_redaction_for_string(url)
            .unwrap_or("[REDACTED:url]")
            .to_string();
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

fn redact_response_json(value: &mut Value) -> usize {
    match value {
        Value::Object(map) => {
            let mut count = 0;
            for (key, child) in map.iter_mut() {
                if let Some(marker) = response_redaction_for_key(key) {
                    *child = Value::String(marker.to_string());
                    count += 1;
                } else {
                    count += redact_response_json(child);
                }
            }
            count
        }
        Value::Array(values) => values.iter_mut().map(redact_response_json).sum(),
        Value::String(text) => {
            if let Some(marker) = response_redaction_for_string(text) {
                *value = Value::String(marker.to_string());
                1
            } else {
                0
            }
        }
        _ => 0,
    }
}

fn response_redaction_for_key(key: &str) -> Option<&'static str> {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    if normalized == "env" || normalized.ends_with("env") || normalized.contains("environment") {
        return Some("[REDACTED:environment]");
    }
    if normalized == "path"
        || normalized.ends_with("path")
        || normalized.ends_with("dir")
        || normalized.contains("workspace")
        || normalized.contains("workdir")
        || normalized.contains("mount")
    {
        return Some("[REDACTED:local-path]");
    }
    const SECRET_FRAGMENTS: &[&str] = &[
        "secret",
        "token",
        "password",
        "credential",
        "apikey",
        "authorization",
        "bearer",
        "privatekey",
        "clientsecret",
        "cookie",
        "session",
        "refresh",
    ];
    if normalized == "auth"
        || normalized.ends_with("auth")
        || SECRET_FRAGMENTS
            .iter()
            .any(|fragment| normalized.contains(fragment))
    {
        return Some("[REDACTED:secret]");
    }
    None
}

fn response_redaction_for_string(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("file://") || earliest_local_path_start(trimmed).is_some() {
        return Some("[REDACTED:local-path]");
    }
    if lower.contains("authorization:")
        || lower.contains("bearer ")
        || lower.contains("api_key=")
        || lower.contains("apikey=")
        || lower.contains("token=")
        || lower.contains("password=")
        || trimmed.starts_with("sk-")
    {
        return Some("[REDACTED:secret-like]");
    }
    None
}

fn secret_refs_from_options(args: &[String]) -> Result<BTreeMap<String, String>> {
    let mut refs = key_value_options(args, "--secret-ref")?;
    refs.extend(key_value_options(args, "--secret")?);
    let file = option_value(args, "--secret-ref-file")?.or(option_value(args, "--secrets-file")?);
    if let Some(file) = file {
        let mut from_file = read_secret_ref_file(Path::new(&file))?;
        from_file.extend(refs);
        Ok(from_file)
    } else {
        Ok(refs)
    }
}

fn read_secret_ref_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let source_ref = public_secret_ref_file_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read secret ref file {source_ref}"))?;
    let parsed: Value = if path.extension().and_then(|value| value.to_str()) == Some("json") {
        serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse secret ref file {source_ref}"))?
    } else {
        let yaml = serde_yaml::from_str::<serde_yaml::Value>(&raw)
            .with_context(|| format!("failed to parse secret ref file {source_ref}"))?;
        serde_json::to_value(yaml)?
    };
    let object = parsed.as_object().ok_or_else(|| {
        anyhow!("secret ref file {source_ref} must be a map of NAME: provider-ref")
    })?;
    let mut refs = BTreeMap::new();
    for (key, value) in object {
        let value = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "secret ref file entry {} must be a non-empty provider ref string",
                    public_secret_text(key)
                )
            })?;
        refs.insert(key.clone(), value.to_string());
    }
    Ok(refs)
}

fn public_secret_ref_file_ref() -> &'static str {
    "secret-ref-file://source"
}

fn secret_requirements_from_package(package: &Value) -> Vec<SecretRequirement> {
    let mut requirements = package
        .get("secret_requirements")
        .and_then(Value::as_array)
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

fn validate_secret_refs_for_package(
    refs: &BTreeMap<String, String>,
    requirements: &[SecretRequirement],
) -> Result<()> {
    let required = requirements
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    let missing = requirements
        .iter()
        .filter(|item| {
            refs.get(&item.id)
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
        })
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let unknown = refs
        .keys()
        .filter(|id| !required.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let unsupported = refs
        .iter()
        .filter(|(id, value)| required.contains(id.as_str()) && !supported_secret_ref(value))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if missing.is_empty() && unknown.is_empty() && unsupported.is_empty() {
        return Ok(());
    }
    let mut lines = vec!["Secret refs do not match this package.".to_string()];
    if !missing.is_empty() {
        lines.push(format!("Missing: {}", public_secret_list(missing.iter())));
    }
    if !unknown.is_empty() {
        lines.push(format!("Unknown: {}", public_secret_list(unknown.iter())));
    }
    if !unsupported.is_empty() {
        lines.push(format!(
            "Unsupported ref format: {}",
            public_secret_list(unsupported.iter())
        ));
    }
    if !requirements.is_empty() {
        lines.push(String::new());
        lines.push("Required secrets:".to_string());
        for requirement in requirements {
            lines.push(format!(
                "  {} -> {}",
                public_secret_text(&requirement.id),
                public_secret_text(&requirement.target)
            ));
        }
        lines.push(String::new());
        lines.push("Pass refs with:".to_string());
        for requirement in requirements {
            lines.push(format!(
                "  --secret-ref {}=gcp-secret-manager://projects/<project>/secrets/<secret>/versions/<version>",
                public_secret_text(&requirement.id)
            ));
        }
    }
    bail!("{}", lines.join("\n"));
}

fn supported_secret_ref(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("gcp-secret-manager://") || value.starts_with("aws-secrets-manager://")
}

fn validate_cloud_run_plain_env(
    env: &BTreeMap<String, String>,
    allow_secret_env: bool,
) -> Result<()> {
    if allow_secret_env {
        return Ok(());
    }
    let secret_like = env
        .keys()
        .filter(|key| secret_like_env_key(key))
        .cloned()
        .collect::<Vec<_>>();
    if secret_like.is_empty() {
        return Ok(());
    }
    bail!(
        "Cloud run --env contains secret-looking key(s): {}.\nPlain --env values are sent to the Cloud API as run configuration. Use --secret-ref NAME=gcp-secret-manager://projects/<project>/secrets/<secret>/versions/<version> or --secret-ref-file secrets.yaml instead. If every listed value is intentionally non-secret, rerun with --allow-secret-env.",
        secret_like.join(", ")
    );
}

fn secret_like_env_key(key: &str) -> bool {
    let normalized = key.to_ascii_uppercase();
    let compact = normalized
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();
    if ["APIKEY", "ACCESSKEY", "PRIVATEKEY"]
        .iter()
        .any(|needle| compact.contains(needle))
    {
        return true;
    }
    normalized
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .any(|part| {
            matches!(
                part,
                "SECRET" | "TOKEN" | "PASSWORD" | "PASSWD" | "CREDENTIAL" | "CREDENTIALS" | "OAUTH"
            )
        })
}

fn requirements_to_json(requirements: &[SecretRequirement]) -> Value {
    Value::Array(
        requirements
            .iter()
            .map(|item| {
                json!({
                    "id": public_secret_text(&item.id),
                    "target": public_secret_text(&item.target),
                    "required_for_variants": item
                        .required_for_variants
                        .iter()
                        .map(|value| public_secret_text(value))
                        .collect::<Vec<_>>()
                })
            })
            .collect(),
    )
}

fn public_secret_list<'a>(items: impl IntoIterator<Item = &'a String>) -> String {
    items
        .into_iter()
        .map(|value| public_secret_text(value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn public_secret_text(value: &str) -> String {
    public_error_message(value)
}

fn required_option(args: &[String], name: &str) -> Result<String> {
    option_value(args, name)?.ok_or_else(|| anyhow!("{name} is required"))
}

fn positional_or_required_option(args: &[String], name: &str) -> Result<String> {
    let positional = positional_arg(args);
    let option = option_value(args, name)?;
    match (positional, option) {
        (Some(_), Some(_)) => {
            bail!("{name} must be provided either positionally or by flag, not both")
        }
        (Some(value), None) | (None, Some(value)) => Ok(value),
        (None, None) => bail!("{name} is required"),
    }
}

fn option_value(args: &[String], name: &str) -> Result<Option<String>> {
    let mut value = None;
    for (index, arg) in args.iter().enumerate() {
        if arg != name {
            continue;
        }
        if value.is_some() {
            bail!("{name} may only be provided once");
        }
        value = Some(
            args.get(index + 1)
                .ok_or_else(|| anyhow!("{name} requires a value"))?
                .clone(),
        );
    }
    Ok(value)
}

fn key_value_options(args: &[String], name: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] != name {
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{name} requires KEY=VALUE"))?;
        let Some((key, value)) = value.split_once('=') else {
            bail!("{name} requires KEY=VALUE");
        };
        if key.is_empty() {
            bail!("{name} requires KEY=VALUE");
        }
        out.insert(key.to_string(), value.to_string());
        index += 2;
    }
    Ok(out)
}

fn csv_option(args: &[String], name: &str, fallback: &[String]) -> Result<Vec<String>> {
    Ok(option_value(args, name)?
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| fallback.to_vec()))
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

fn bounded_number_option(args: &[String], name: &str, max: u64) -> Result<Option<u64>> {
    let Some(parsed) = number_option(args, name)? else {
        return Ok(None);
    };
    if parsed > max {
        bail!("{name} must be an integer from 1 to {max}");
    }
    Ok(Some(parsed))
}

fn nonnegative_number_option(args: &[String], name: &str) -> Result<Option<u64>> {
    let Some(value) = option_value(args, name)? else {
        return Ok(None);
    };
    value
        .parse::<u64>()
        .with_context(|| format!("{name} requires a non-negative integer"))
        .map(Some)
}

fn positional_arg(args: &[String]) -> Option<String> {
    let options_with_values = [
        "--import-id",
        "--action-file",
        "--aliases",
        "--label",
        "--file",
        "--package-digest",
        "--run-id",
        "--key",
        "--limit",
        "--after-row-seq",
        "--secret-ref-file",
        "--secrets-file",
        "--runner-instance-id",
        "--name",
        "--executors",
        "--resources",
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
        "--out",
        "--archive-out",
        "--overrides",
        "--core-cmd",
    ];
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
        return Some(arg.clone());
    }
    None
}

fn insert_option_string(object: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        object.insert(key.to_string(), json!(value));
    }
}

fn insert_option_number(object: &mut Map<String, Value>, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        object.insert(key.to_string(), json!(value));
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_import_summary(value: &Value) -> Result<()> {
    println!("{}", render_import_summary(value)?);
    Ok(())
}

fn render_import_summary(value: &Value) -> Result<String> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("import inspect response was not an object"))?;
    let mut lines = vec![
        format!(
            "Import: {}",
            object
                .get("import_id")
                .and_then(Value::as_str)
                .unwrap_or("(unknown)")
        ),
        format!(
            "Status: {}",
            object
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("(unknown)")
        ),
    ];
    if let Some(label) = object.get("label").and_then(Value::as_str) {
        lines.push(format!("Label: {label}"));
    }
    if let Some(package_digest) = object.get("package_digest").and_then(Value::as_str) {
        lines.push(format!("Package: {package_digest}"));
    }
    if let Some(error_message) = object.get("error_message").and_then(Value::as_str) {
        lines.push(format!("Error: {}", public_error_message(error_message)));
    }
    if let Some(diagnostics) = object.get("diagnostics").and_then(Value::as_array) {
        if !diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Diagnostics:".to_string());
            for diagnostic in diagnostics {
                lines.push(format!(
                    "  - [{}] {} {}: {}",
                    public_import_summary_text(
                        diagnostic.get("severity").and_then(Value::as_str),
                        "unknown"
                    ),
                    public_import_summary_text(
                        diagnostic.get("code").and_then(Value::as_str),
                        "diagnostic"
                    ),
                    public_import_summary_text(
                        diagnostic.get("pointer").and_then(Value::as_str),
                        "/"
                    ),
                    public_import_summary_text(
                        diagnostic.get("message").and_then(Value::as_str),
                        ""
                    )
                ));
            }
        }
    }
    Ok(lines.join("\n"))
}

fn public_import_summary_text(value: Option<&str>, fallback: &str) -> String {
    value
        .map(public_error_message)
        .unwrap_or_else(|| fallback.to_string())
}

fn print_secret_requirements(
    package_digest: &str,
    requirements: &[SecretRequirement],
) -> Result<()> {
    println!(
        "{}",
        render_secret_requirements(package_digest, requirements)
    );
    Ok(())
}

fn render_secret_requirements(package_digest: &str, requirements: &[SecretRequirement]) -> String {
    if requirements.is_empty() {
        return format!("Package {package_digest} does not declare runtime secrets.");
    }
    let mut lines = vec![
        format!("Package: {package_digest}"),
        "Required runtime secrets:".to_string(),
    ];
    for requirement in requirements {
        let variants = if requirement.required_for_variants.is_empty() {
            String::new()
        } else {
            format!(
                " variants={}",
                requirement
                    .required_for_variants
                    .iter()
                    .map(|value| public_secret_text(value))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        lines.push(format!(
            "  - {} -> {}{}",
            public_secret_text(&requirement.id),
            public_secret_text(&requirement.target),
            variants
        ));
    }
    lines.extend([
        String::new(),
        "Create a refs file:".to_string(),
        "  secrets.yaml".to_string(),
    ]);
    for requirement in requirements {
        lines.push(format!(
            "    {}: gcp-secret-manager://projects/<project>/secrets/<secret>/versions/<version>",
            public_secret_text(&requirement.id)
        ));
    }
    lines.extend([
        String::new(),
        "Queue with:".to_string(),
        format!("  bucephalus-cloud run create --package-digest {package_digest} --secret-ref-file secrets.yaml"),
    ]);
    lines.join("\n")
}

fn print_help() {
    println!(
        r#"Bucephalus Cloud CLI

Usage:
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] health
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] registry search --kind variant --query codex
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] draft validate --file experiment.yaml
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] draft preview --file experiment.yaml
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] draft export --file experiment.yaml --out ./exported
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] deploy experiment.yaml [--label LABEL] [--out ./package] [--archive-out ./package.tgz]
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] import sealed-package ./package.tgz
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] import inspect <import-id> [--json]
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] package get <package-digest>
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] package secrets <package-digest>
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] run create --package-digest sha256:... [--env KEY=VALUE] [--secret-ref NAME=REF] [--secret-ref-file secrets.yaml] [--allow-secret-env] [--backend runner-docker|modal] [--arch x86_64|arm64] [--cpu-count N] [--memory-mb N] [--disk-mb N] [--isolation reusable_vm|single_use_vm]
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] run get <run-id>
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] run runtime <run-id> [--key runtime_key]
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] run events <run-id> [--limit N] [--after-row-seq N]
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] run results <run-id> [--limit N]
  bucephalus-cloud [--api-url URL] [--worker-token TOKEN] runner-pool create --name cloud-runner-pool --executors runner-docker --resources core_runner,docker_daemon,registry_pull [--arch x86_64|arm64] [--cpu-count N] [--memory-mb N] [--disk-mb N] [--isolation reusable_vm]
  bucephalus-cloud [--api-url URL] [--worker-token TOKEN] runner-pool list
  bucephalus-cloud [--api-url URL] [--worker-token TOKEN] runner-instance drain <runner-instance-id>

Environment:
  BUCEPHALUS_CLOUD_API_URL       Required unless --api-url is set; no localhost default
  BUCEPHALUS_CLOUD_USER_TOKEN    OAuth access token override for user-facing Cloud APIs
  BUCEPHALUS_CLOUD_WORKER_TOKEN  Required for runner pool and worker management commands
  BUCEPHALUS_CLOUD_RUNNER_ADMIN_TOKEN
                                 Optional token for runner pool/admin commands

User auth:
  bucephalus-cloud uses the same per-user OAuth cache as `bucephalus login`
  when --user-token and BUCEPHALUS_CLOUD_USER_TOKEN are not set.

Cloud package upload:
  deploy and import sealed-package validate archive membership and checksums
  locally before uploading package bytes.

Cloud run secrets:
  run create rejects secret-looking --env keys by default because plain env is
  sent as run configuration. Use --secret-ref or --secret-ref-file for secrets.
"#
    );
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn normalize_cloud_package_digest(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let Some(hex) = trimmed.strip_prefix("sha256:") else {
        return Err(invalid_cloud_package_digest_error());
    };
    if hex.len() != 64 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(invalid_cloud_package_digest_error());
    }
    Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
}

fn invalid_cloud_package_digest_error() -> anyhow::Error {
    anyhow!(
        "invalid package digest: expected sha256:<64 hex characters>. Use the package_digest returned by `bucephalus build`, `bucephalus-cloud deploy`, or `bucephalus-cloud import sealed-package`."
    )
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

fn upload_filename_for_sealed_package_archive(path: &Path) -> &'static str {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".tar.gz") {
        "package.tar.gz"
    } else if lower.ends_with(".tgz") {
        "package.tgz"
    } else if lower.ends_with(".tar") {
        "package.tar"
    } else {
        "package.blob"
    }
}

fn encode_path_segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn encode_query(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn tail(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        value.to_string()
    } else {
        value[value.len() - max_len..].to_string()
    }
}

fn make_temp_dir(prefix: &str) -> Result<PathBuf> {
    let temp_root = std::env::temp_dir();
    for attempt in 0..128 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = temp_root.join(format!("{prefix}-{}-{nanos}-{attempt}", std::process::id()));
        match create_new_private_temp_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(anyhow!(
                    "failed to create temporary workspace {}: {}",
                    public_build_upload_temp_ref(),
                    public_error_message(&err.to_string())
                ));
            }
        }
    }
    Err(anyhow!(
        "failed to create temporary workspace {}: exhausted unique names",
        public_build_upload_temp_ref()
    ))
}

fn create_new_private_temp_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)?;
        if let Err(err) = fs::set_permissions(path, fs::Permissions::from_mode(0o700)) {
            let _ = fs::remove_dir_all(path);
            return Err(err);
        }
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn cleanup_temp_dir(path: &Path, temp_ref: &str) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(anyhow!(
            "failed to remove temporary workspace {temp_ref}: {}",
            public_error_message(&err.to_string())
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;
    use std::sync::{Mutex, MutexGuard};

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
        std::env::temp_dir().join(format!(
            "bucephalus_cloud_cli_{label}_{}_{}",
            std::process::id(),
            nanos
        ))
    }

    fn write_json(path: &Path, value: &Value) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("json parent");
        }
        fs::write(path, serde_json::to_vec_pretty(value).expect("json bytes")).expect("json file");
    }

    #[test]
    #[cfg(unix)]
    fn write_cloud_token_cache_secures_shared_auth_files() {
        use std::os::unix::fs::PermissionsExt;

        let home = temp_dir("shared_auth_file_permissions");
        let paths = cloud_token_paths(&home);
        let auth_dir = paths.access.parent().unwrap();
        fs::create_dir_all(auth_dir).unwrap();
        fs::set_permissions(auth_dir, fs::Permissions::from_mode(0o755)).unwrap();

        write_cloud_token_cache(
            &paths,
            &json!({
                "issuer": "https://issuer.example",
                "client_id": "client-1",
                "token_endpoint": "https://issuer.example/token"
            }),
            &json!({
                "access_token": "access-123",
                "refresh_token": "refresh-456",
                "token_type": "Bearer",
                "expires_in": 3600
            }),
        )
        .unwrap();

        let auth_dir_mode = fs::metadata(auth_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(auth_dir_mode, 0o700);
        for path in [&paths.access, &paths.refresh, &paths.cache] {
            let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn write_cloud_token_cache_refuses_symlinked_shared_auth_file() {
        use std::os::unix::fs::symlink;

        let home = temp_dir("shared_auth_symlink");
        let paths = cloud_token_paths(&home);
        let auth_dir = paths.access.parent().unwrap();
        fs::create_dir_all(auth_dir).unwrap();
        let target = home.join("target-token");
        fs::write(&target, "original-token\n").unwrap();
        symlink(&target, &paths.access).unwrap();

        let err = write_cloud_token_cache(
            &paths,
            &json!({}),
            &json!({
                "access_token": "replacement-token"
            }),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("symlinked token file"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "original-token\n");
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn response_body_errors_are_redacted_before_display() {
        let body = br#"{
            "message": "Authorization: Bearer live-token",
            "access_token": "sk-live-access",
            "refresh_token": "refresh-live",
            "details": {
                "workspace_path": "/Users/alice/project",
                "env": {"OPENAI_API_KEY": "sk-live-env"}
            },
            "notes": ["token=raw-query-token", "ordinary hint"]
        }"#;

        let redacted = redacted_response_body(body);

        for forbidden in [
            "live-token",
            "sk-live-access",
            "refresh-live",
            "/Users/alice",
            "sk-live-env",
            "raw-query-token",
        ] {
            assert!(
                !redacted.contains(forbidden),
                "response body leaked forbidden text: {forbidden}"
            );
        }
        assert!(redacted.contains("[REDACTED:secret]"));
        assert!(redacted.contains("[REDACTED:secret-like]"));
        assert!(redacted.contains("[REDACTED:local-path]"));
        assert!(redacted.contains("[REDACTED:environment]"));
        assert!(redacted.contains("ordinary hint"));

        let plain = redacted_response_body(b"token=plain-secret\nretry later");
        assert!(!plain.contains("plain-secret"));
        assert!(plain.contains("[REDACTED:secret-like]"));
        assert!(plain.contains("retry later"));
    }

    #[test]
    fn cloud_top_level_errors_are_public_boundary_safe() {
        let message = public_error_message(
            "failed to upload package /Users/alice/work/package.tgz: permission denied\nmirror https://mirror-user:mirror-secret@mirror.example/releases?token=raw-query#frag\nworker token=raw-cloud-token\ncache file:///private/tmp/bucephalus-cloud/cache.json",
        );

        assert!(message.contains("failed to upload package"));
        assert!(message.contains("permission denied"));
        assert!(message.contains("https://mirror.example/releases"));
        assert!(message.contains("[redacted URL credentials/query]"));
        assert!(message.contains("token=[REDACTED:secret-like]"));
        assert!(message.contains("file://[REDACTED:local-path]"));
        for forbidden in [
            "/Users/alice",
            "/private/tmp",
            "mirror-user",
            "mirror-secret",
            "?token=raw-query",
            "#frag",
            "raw-cloud-token",
            "work/package",
        ] {
            assert!(
                !message.contains(forbidden),
                "Cloud top-level error leaked forbidden text: {forbidden}\n{message}"
            );
        }
    }

    #[test]
    fn cloud_top_level_errors_redact_cross_platform_local_paths() {
        let message = public_error_message(
            &[
                "mac=/Volumes/Backup Drive/customer package/archive.tgz token=raw-cloud-token",
                r"win=C:\Users\Alice\AppData\Local\Temp\buc-cloud.log",
                r"env=%LOCALAPPDATA%\Bucephalus\cache.json",
                "home=~/Library/Application Support/bucephalus-cloud/state.json",
                "wsl=/mnt/c/Users/Alice/AppData/Local/Temp/buc-cloud.log",
            ]
            .join("\n"),
        );

        assert!(message.contains("mac=[REDACTED:local-path] token=[REDACTED:secret-like]"));
        assert!(message.contains("win=[REDACTED:local-path]"));
        assert!(message.contains("env=[REDACTED:local-path]"));
        assert!(message.contains("home=[REDACTED:local-path]"));
        assert!(message.contains("wsl=[REDACTED:local-path]"));
        for forbidden in [
            "/Volumes/Backup",
            "Drive/customer",
            r"C:\Users\Alice",
            "%LOCALAPPDATA%",
            "~/Library",
            "Application Support",
            "/mnt/c/Users/Alice",
            "raw-cloud-token",
        ] {
            assert!(
                !message.contains(forbidden),
                "Cloud top-level error leaked forbidden text {forbidden}: {message}"
            );
        }
    }

    #[test]
    fn import_summary_redacts_server_diagnostics_before_display() {
        let summary = render_import_summary(&json!({
            "import_id": "import_1",
            "status": "failed",
            "label": "nightly",
            "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "error_message": "failed reading /Users/alice/private/customer-a/package.tgz token=raw-import-token",
            "diagnostics": [{
                "severity": "error",
                "code": "missing-secret",
                "pointer": "/Users/alice/private/customer-a/package.json",
                "message": "mirror https://mirror-user:mirror-secret@mirror.example/import?token=raw-query#frag"
            }]
        }))
        .unwrap();

        assert!(summary.contains("Import: import_1"));
        assert!(summary.contains("Status: failed"));
        assert!(summary.contains("Label: nightly"));
        assert!(summary.contains("[REDACTED:local-path]"));
        assert!(summary.contains("token=[REDACTED:secret-like]"));
        assert!(summary.contains("https://mirror.example/import"));
        assert!(summary.contains("[redacted URL credentials/query]"));
        for forbidden in [
            "/Users/alice",
            "private/customer-a",
            "raw-import-token",
            "mirror-user",
            "mirror-secret",
            "?token=raw-query",
            "#frag",
        ] {
            assert!(
                !summary.contains(forbidden),
                "import summary leaked forbidden text {forbidden}: {summary}"
            );
        }
    }

    #[test]
    fn draft_export_output_uses_public_refs() {
        let output = draft_export_output(
            &json!({
                "format": "yaml",
                "issues": [{"severity": "warning", "message": "ordinary hint"}]
            }),
            "resolved_json",
            "experiment.yaml",
        );
        let encoded = serde_json::to_string_pretty(&output).unwrap();

        assert_eq!(output["export_ref"], "draft-export://output");
        assert_eq!(output["source_ref"], "draft://source");
        assert_eq!(output["filename"], "experiment.yaml");
        assert_eq!(output["format"], "yaml");
        assert!(output.get("exported").is_none());
        assert!(output.get("source").is_none());
        for forbidden in [
            "/Users/alice",
            "private/customer-a",
            "customer-secret-draft.yaml",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "draft export output leaked forbidden text {forbidden}: {encoded}"
            );
        }
    }

    #[test]
    fn read_draft_file_shape_error_uses_public_ref() {
        let root = temp_dir("draft_source_public_ref");
        let path = root
            .join("private")
            .join("customer-a")
            .join("customer-secret-draft.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, "[1, 2, 3]\n").unwrap();

        let err = read_draft_file(&path).expect_err("draft arrays should be rejected");
        let message = err.to_string();

        assert_eq!(
            message,
            "draft file must parse to an object: draft://source"
        );
        let root_text = root.to_string_lossy().to_string();
        for forbidden in [
            root_text.as_str(),
            "private/customer-a",
            "customer-secret-draft",
        ] {
            assert!(
                !message.contains(forbidden),
                "draft source error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn build_upload_output_uses_public_refs_for_local_outputs() {
        let package_dir = Path::new("/Users/alice/private/customer-a/package");
        let archive_path = Path::new("/Users/alice/private/customer-a/package-token-secret.tgz");

        let output = build_upload_output(
            json!({
                "import_id": "import_1",
                "status": "inspected",
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }),
            Some(package_dir),
            Some(archive_path),
        );
        let encoded = serde_json::to_string_pretty(&output).unwrap();

        assert_eq!(output["package_ref"], "cloud-upload://package");
        assert_eq!(output["archive_ref"], "cloud-upload://archive");
        assert!(output.get("package_dir").is_none());
        assert!(output.get("archive_path").is_none());
        for forbidden in ["/Users/alice", "private/customer-a", "package-token-secret"] {
            assert!(
                !encoded.contains(forbidden),
                "build-upload output leaked forbidden text {forbidden}: {encoded}"
            );
        }
    }

    #[test]
    fn upload_filename_omits_local_archive_filename() {
        assert_eq!(
            upload_filename_for_sealed_package_archive(Path::new(
                "/Users/alice/private/customer-a/package-token-secret.tgz"
            )),
            "package.tgz"
        );
        assert_eq!(
            upload_filename_for_sealed_package_archive(Path::new(
                "/Users/alice/private/customer-a/package-token-secret.tar.gz"
            )),
            "package.tar.gz"
        );
        assert_eq!(
            upload_filename_for_sealed_package_archive(Path::new(
                "/Users/alice/private/customer-a/package-token-secret.tar"
            )),
            "package.tar"
        );
        assert_eq!(
            upload_filename_for_sealed_package_archive(Path::new(
                "/Users/alice/private/customer-a/package-token-secret.bin"
            )),
            "package.blob"
        );
    }

    #[test]
    #[cfg(unix)]
    fn build_upload_temp_workspace_is_private_and_cleanup_errors_are_public() {
        use std::os::unix::fs::PermissionsExt;

        let temp = make_temp_dir("buc-cloud-test-temp").expect("temp workspace");
        let mode = fs::metadata(&temp).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        cleanup_temp_dir(&temp, public_build_upload_temp_ref()).expect("cleanup temp workspace");
        assert!(!temp.exists());

        let root = temp_dir("cloud_temp_cleanup_error");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("private-token-temp");
        fs::write(&file, "not a dir\n").unwrap();
        let err = cleanup_temp_dir(&file, public_build_upload_temp_ref())
            .expect_err("file path should not clean up as a temp dir");
        let message = err.to_string();

        assert!(message.contains("cloud-upload://temp"));
        for forbidden in [root.to_str().unwrap(), "private-token-temp"] {
            assert!(
                !message.contains(forbidden),
                "cleanup error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn build_upload_temp_workspace_refuses_existing_path() {
        let root = temp_dir("cloud_temp_existing");
        fs::create_dir_all(&root).unwrap();

        let err = create_new_private_temp_dir(&root)
            .expect_err("temp helper must not reuse an existing directory");

        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cloud_run_runtime_paths_are_encoded_and_query_safe() {
        assert_eq!(
            cloud_run_runtime_path(
                &["run/with space".to_string()],
                CloudRunRuntimeEndpoint::Summary
            )
            .unwrap(),
            "/v1/runs/run%2Fwith%20space/runtime"
        );
        assert_eq!(
            cloud_run_runtime_path(
                &[
                    "--run-id".to_string(),
                    "run-1".to_string(),
                    "--key".to_string(),
                    "run/session state".to_string()
                ],
                CloudRunRuntimeEndpoint::Summary
            )
            .unwrap(),
            "/v1/runs/run%2D1/runtime/kv/run%2Fsession%20state"
        );
        assert_eq!(
            cloud_run_runtime_path(
                &[
                    "--limit".to_string(),
                    "25".to_string(),
                    "--after-row-seq".to_string(),
                    "7".to_string(),
                    "run-2".to_string()
                ],
                CloudRunRuntimeEndpoint::Events
            )
            .unwrap(),
            "/v1/runs/run%2D2/runtime/events?limit=25&after_row_seq=7"
        );
        assert_eq!(
            cloud_run_runtime_path(
                &["run-3".to_string(), "--limit".to_string(), "10".to_string()],
                CloudRunRuntimeEndpoint::Results
            )
            .unwrap(),
            "/v1/runs/run%2D3/runtime/results?limit=10"
        );
    }

    #[test]
    fn cloud_import_and_upload_paths_encode_ids_as_path_segments() {
        assert_eq!(
            cloud_import_path("import/with space?token=raw-secret#frag"),
            "/v1/imports/import%2Fwith%20space%3Ftoken%3Draw%2Dsecret%23frag"
        );
        assert_eq!(
            cloud_upload_content_path("upload/with space?token=raw-secret#frag"),
            "/v1/uploads/upload%2Fwith%20space%3Ftoken%3Draw%2Dsecret%23frag/content"
        );
        assert_eq!(
            cloud_upload_complete_path("upload/with space?token=raw-secret#frag"),
            "/v1/uploads/upload%2Fwith%20space%3Ftoken%3Draw%2Dsecret%23frag/complete"
        );
    }

    #[test]
    fn runner_instance_drain_path_is_encoded_and_rejects_ambiguous_ids() {
        assert_eq!(
            runner_instance_drain_path(&["runner/with space".to_string()]).unwrap(),
            "/v1/runner-instances/runner%2Fwith%20space/drain"
        );

        let err = runner_instance_drain_path(&[
            "runner-1 token=raw-positional-runner-token".to_string(),
            "--runner-instance-id".to_string(),
            "runner-2 token=raw-flag-runner-token".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains(
            "--runner-instance-id must be provided either positionally or by flag, not both"
        ));
        for forbidden in ["raw-positional-runner-token", "raw-flag-runner-token"] {
            assert!(
                !err.contains(forbidden),
                "ambiguous runner instance id error leaked forbidden text {forbidden}: {err}"
            );
        }
    }

    #[test]
    fn cloud_run_runtime_paths_reject_ambiguous_flags() {
        let err = cloud_run_runtime_path(
            &[
                "run-1".to_string(),
                "--after-row-seq".to_string(),
                "0".to_string(),
            ],
            CloudRunRuntimeEndpoint::Results,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--after-row-seq is only supported for `run events`"));

        let err = cloud_run_runtime_path(
            &[
                "run-1".to_string(),
                "--key".to_string(),
                "state".to_string(),
            ],
            CloudRunRuntimeEndpoint::Events,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--key is only supported for `run runtime --key`"));

        let err = cloud_run_runtime_path(
            &["run-1".to_string(), "--limit".to_string(), "0".to_string()],
            CloudRunRuntimeEndpoint::Events,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--limit requires a positive integer"));

        let err = cloud_run_runtime_path(
            &[
                "run-1".to_string(),
                "--limit".to_string(),
                "1001".to_string(),
            ],
            CloudRunRuntimeEndpoint::Results,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--limit must be an integer from 1 to 1000"));

        let err = cloud_run_runtime_path(
            &[
                "run-1 token=raw-positional-run-token".to_string(),
                "--run-id".to_string(),
                "run-2 token=raw-flag-run-token".to_string(),
            ],
            CloudRunRuntimeEndpoint::Summary,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--run-id must be provided either positionally or by flag, not both"));
        for forbidden in ["raw-positional-run-token", "raw-flag-run-token"] {
            assert!(
                !err.contains(forbidden),
                "ambiguous run id error leaked forbidden text {forbidden}: {err}"
            );
        }

        let err = cloud_run_runtime_path(
            &[
                "run-1".to_string(),
                "--limit".to_string(),
                "10".to_string(),
                "--limit".to_string(),
                "token=raw-limit-token".to_string(),
            ],
            CloudRunRuntimeEndpoint::Events,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--limit may only be provided once"));
        assert!(
            !err.contains("raw-limit-token"),
            "duplicate limit error leaked forbidden text: {err}"
        );

        let err = cloud_run_runtime_path(
            &["run-1".to_string(), "--run-id".to_string()],
            CloudRunRuntimeEndpoint::Summary,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--run-id requires a value"));
    }

    #[test]
    fn cloud_token_refresh_transport_errors_redact_token_endpoint_url() {
        let root = temp_dir("cloud_token_refresh_redaction");
        let paths = cloud_token_paths(&root);
        let err = refresh_cloud_token_cache(
            &paths,
            &json!({
                "client_id": "client-1",
                "refresh_token": "refresh-live-secret",
                "token_endpoint": "http://user:super-secret@127.0.0.1:1/token?access_token=raw-query-token#frag"
            }),
        )
        .expect_err("dead localhost token endpoint should fail");
        let message = format!("{err:#}");

        for forbidden in [
            "super-secret",
            "raw-query-token",
            "refresh-live-secret",
            "user:super-secret",
        ] {
            assert!(
                !message.contains(forbidden),
                "refresh error leaked forbidden text: {forbidden}\n{message}"
            );
        }
        assert!(message.contains("http://127.0.0.1:1/token"));
        assert!(message.contains("[redacted URL credentials/query]"));
        fs::remove_dir_all(root).ok();
    }

    fn write_package_file(
        package_dir: &Path,
        rel: &str,
        bytes: &[u8],
        files: &mut Map<String, Value>,
    ) {
        let path = package_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("package file parent");
        }
        fs::write(&path, bytes).expect("package file");
        files.insert(rel.to_string(), json!(sha256_digest(bytes)));
    }

    fn write_minimal_sealed_package(package_dir: &Path) {
        fs::create_dir_all(package_dir).expect("package dir");
        let mut files = Map::new();
        write_package_file(
            package_dir,
            "resolved_experiment.json",
            br#"{"version":"0.5","experiment":{"id":"archive-smoke"},"matrix":{"tasks":{"path":"tasks/tasks.jsonl"},"variants":[{"id":"base"}]}}"#,
            &mut files,
        );
        write_package_file(
            package_dir,
            "tasks/tasks.jsonl",
            br#"{"schema_version":"task_row_v2","id":"task_1","task":{"id":"task_1"}}"#,
            &mut files,
        );
        write_package_file(
            package_dir,
            "staging_manifest.json",
            br#"{"schema_version":"runtime_staging_manifest_v1","entries":[]}"#,
            &mut files,
        );
        write_package_file(
            package_dir,
            "files/answer.txt",
            b"sealed answer\n",
            &mut files,
        );
        write_json(
            &package_dir.join("checksums.json"),
            &json!({
                "schema_version": "sealed_package_checksums_v2",
                "files": files
            }),
        );
        write_json(
            &package_dir.join("package.lock"),
            &json!({
                "schema_version": "sealed_package_lock_v1",
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }),
        );
        write_json(
            &package_dir.join("package_checks.json"),
            &json!({
                "schema_version": "package_checks_v1",
                "passed": true,
                "checks": []
            }),
        );
        write_json(
            &package_dir.join("manifest.json"),
            &json!({
                "schema_version": "sealed_run_package_v2",
                "created_at": "2026-06-08T00:00:00Z",
                "resolved_experiment": {},
                "checksums_ref": "checksums.json",
                "package_checks_ref": "package_checks.json",
                "package_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }),
        );
    }

    fn read_tgz_entries(path: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
        let file = File::open(path)?;
        let decoder = GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        let mut entries = BTreeMap::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let name = entry.path()?.to_string_lossy().to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            entries.insert(name, bytes);
        }
        Ok(entries)
    }

    #[derive(Debug)]
    struct TarHeaderSnapshot {
        uid: u64,
        gid: u64,
        mtime: u64,
        mode: u32,
        is_file: bool,
    }

    fn read_tgz_header_snapshots(path: &Path) -> Result<BTreeMap<String, TarHeaderSnapshot>> {
        let file = File::open(path)?;
        let decoder = GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        let mut entries = BTreeMap::new();
        for entry in archive.entries()? {
            let entry = entry?;
            let name = entry.path()?.to_string_lossy().to_string();
            let header = entry.header();
            entries.insert(
                name,
                TarHeaderSnapshot {
                    uid: header.uid()?,
                    gid: header.gid()?,
                    mtime: header.mtime()?,
                    mode: header.mode()? & 0o777,
                    is_file: header.entry_type().is_file(),
                },
            );
        }
        Ok(entries)
    }

    fn write_tgz_entries(path: &Path, entries: &BTreeMap<String, Vec<u8>>) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("archive parent");
        }
        let file = File::create(path).expect("archive file");
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_cksum();
            let mut content = bytes.as_slice();
            builder
                .append_data(&mut header, name.as_str(), &mut content)
                .expect("append archive entry");
        }
        builder.finish().expect("finish archive");
    }

    fn write_tgz_entries_with_local_metadata(path: &Path, entries: &BTreeMap<String, Vec<u8>>) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("archive parent");
        }
        let file = File::create(path).expect("archive file");
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_uid(501);
            header.set_gid(20);
            header.set_mtime(1_700_000_000);
            header.set_cksum();
            let mut content = bytes.as_slice();
            builder
                .append_data(&mut header, name.as_str(), &mut content)
                .expect("append archive entry");
        }
        builder.finish().expect("finish archive");
    }

    #[test]
    fn shared_cloud_user_token_reads_bucephalus_login_cache() {
        let _lock = lock_env();
        let home = temp_dir("shared_cache");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);
        let paths = cloud_token_paths(&home);
        fs::create_dir_all(paths.cache.parent().unwrap()).unwrap();
        fs::write(
            &paths.cache,
            serde_json::to_string_pretty(&json!({
                "schema_version": "bucephalus_cloud_oauth_token_v1",
                "client_id": "client-1",
                "token_endpoint": "https://issuer.example/token",
                "access_token": "cache-access-123",
                "refresh_token": "refresh-456",
                "expires_at_ms": current_unix_time_ms() + 3_600_000
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            shared_cloud_user_token().unwrap().as_deref(),
            Some("cache-access-123")
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn parse_global_args_prefers_explicit_user_token_over_shared_cache() {
        let _lock = lock_env();
        let home = temp_dir("explicit_token");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some("https://api.example")),
        ]);
        let paths = cloud_token_paths(&home);
        fs::create_dir_all(paths.cache.parent().unwrap()).unwrap();
        fs::write(
            &paths.cache,
            serde_json::to_string_pretty(&json!({
                "access_token": "cache-access-123",
                "refresh_token": "refresh-456",
                "client_id": "client-1",
                "token_endpoint": "http://127.0.0.1:1/token",
                "expires_at_ms": current_unix_time_ms() - 1
            }))
            .unwrap(),
        )
        .unwrap();

        let context = parse_global_args(vec![
            "--user-token".to_string(),
            "explicit-token".to_string(),
            "health".to_string(),
        ])
        .unwrap();
        assert_eq!(context.user_token.as_deref(), Some("explicit-token"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn parse_global_args_rejects_unsafe_api_url_without_leaking_values() {
        let _lock = lock_env();
        let home = temp_dir("unsafe_api_url");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
        ]);

        let err = parse_global_args(vec![
            "--api-url".to_string(),
            "https://api-user:api-secret@example.com/cloud?token=raw-api-token#frag".to_string(),
            "--user-token".to_string(),
            "explicit-token".to_string(),
            "health".to_string(),
        ])
        .expect_err("credential-bearing API base URL should fail");
        let message = err.to_string();

        assert!(message.contains("invalid Cloud API URL"));
        assert!(message.contains("without credentials, query, or fragment"));
        for forbidden in [
            "api-user",
            "api-secret",
            "raw-api-token",
            "?token=",
            "#frag",
            "explicit-token",
        ] {
            assert!(
                !message.contains(forbidden),
                "unsafe API URL error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn parse_global_args_does_not_treat_help_argument_value_as_help_request() {
        let _lock = lock_env();
        let home = temp_dir("help_value_validates_api_url");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
        ]);

        let err = parse_global_args(vec![
            "--api-url".to_string(),
            "file:///Users/alice/private/cloud-api?token=raw-api-token".to_string(),
            "--user-token".to_string(),
            "explicit-token".to_string(),
            "registry".to_string(),
            "search".to_string(),
            "--query".to_string(),
            "help".to_string(),
        ])
        .expect_err("ordinary argument value 'help' must not bypass API URL validation");
        let message = err.to_string();

        assert!(message.contains("invalid Cloud API URL"));
        for forbidden in [
            "/Users/alice",
            "private/cloud-api",
            "raw-api-token",
            "?token=",
            "explicit-token",
        ] {
            assert!(
                !message.contains(forbidden),
                "help value validation error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn cloud_help_does_not_require_valid_api_url_or_refresh_cached_auth() {
        let _lock = lock_env();
        let home = temp_dir("help_no_auth_refresh");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
            (
                BUCEPHALUS_CLOUD_API_URL_ENV,
                Some("https://api-user:api-secret@example.com/cloud?token=raw-api-token#frag"),
            ),
        ]);
        let paths = cloud_token_paths(&home);
        fs::create_dir_all(paths.cache.parent().unwrap()).unwrap();
        fs::write(
            &paths.cache,
            serde_json::to_string_pretty(&json!({
                "access_token": "expired-cache-access",
                "refresh_token": "refresh-secret",
                "client_id": "client-1",
                "token_endpoint": "http://127.0.0.1:1/token",
                "expires_at_ms": current_unix_time_ms() - 1
            }))
            .unwrap(),
        )
        .unwrap();

        run(vec!["run".to_string(), "--help".to_string()])
            .expect("help should print without validating API URL or refreshing cached auth");
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn user_auth_hint_names_login_env_and_token_file() {
        let _lock = lock_env();
        let home = temp_dir("auth_hint");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);
        let context = CliContext {
            api_url: "https://api.example".to_string(),
            user_token: None,
            worker_token: None,
            runner_admin_token: None,
            args: Vec::new(),
            client: Client::new(),
        };

        let message = append_user_auth_hint(
            &context,
            "Bucephalus Cloud requires OAuth bearer authentication".to_string(),
        );

        assert!(message.contains("bucephalus login"));
        assert!(message.contains("export BUCEPHALUS_CLOUD_USER_TOKEN=<oauth-access-token>"));
        assert!(message.contains("<BUCEPHALUS_HOME>/auth/cloud_user_token"));
        assert!(
            !message.contains(home.to_str().unwrap()),
            "auth hint must not expose the caller's local home path: {message}"
        );
        assert!(message.contains("The CLI did not find a user bearer token"));
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn cloud_package_digest_validation_normalizes_without_echoing_bad_values() {
        assert_eq!(
            normalize_cloud_package_digest(&format!(" sha256:{} ", "A".repeat(64))).unwrap(),
            format!("sha256:{}", "a".repeat(64))
        );

        for bad in [
            "/Users/alice/private/customer-a/package.tgz?token=raw-digest-token",
            "sha256:abc token=raw-digest-token",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaag?token=raw-query",
            "sha512:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let message = normalize_cloud_package_digest(bad)
                .expect_err("malformed package digest should fail locally")
                .to_string();

            assert!(message.contains("invalid package digest"));
            assert!(message.contains("sha256:<64 hex characters>"));
            for forbidden in [
                "/Users/alice",
                "private/customer-a",
                "raw-digest-token",
                "raw-query",
                "?token=",
                "sha512:",
            ] {
                assert!(
                    !message.contains(forbidden),
                    "package digest error leaked forbidden text {forbidden}: {message}"
                );
            }
        }
    }

    #[test]
    fn run_create_rejects_malformed_package_digest_before_cloud_request() {
        let context = CliContext {
            api_url: "http://127.0.0.1:1".to_string(),
            user_token: Some("user-token".to_string()),
            worker_token: None,
            runner_admin_token: None,
            args: vec![
                "--package-digest".to_string(),
                "/Users/alice/private/customer-a/package.tgz?token=raw-digest-token".to_string(),
                "--no-secret-preflight".to_string(),
            ],
            client: Client::new(),
        };

        let message = run_create(context)
            .expect_err("malformed package digest should fail before Cloud request")
            .to_string();

        assert!(message.contains("invalid package digest"));
        assert!(message.contains("sha256:<64 hex characters>"));
        for forbidden in [
            "127.0.0.1",
            "transport error",
            "/Users/alice",
            "private/customer-a",
            "raw-digest-token",
            "?token=",
            "user-token",
        ] {
            assert!(
                !message.contains(forbidden),
                "run create digest error leaked forbidden text {forbidden}: {message}"
            );
        }
    }

    #[test]
    fn secret_requirement_outputs_redact_package_supplied_text() {
        let requirements = vec![
            SecretRequirement {
                id: "OPENAI_API_KEY".to_string(),
                target: "/Users/alice/private/customer-a/env token=raw-target-token".to_string(),
                required_for_variants: vec![
                    "nightly token=raw-variant-token".to_string(),
                    "/private/tmp/customer-variant".to_string(),
                ],
            },
            SecretRequirement {
                id: "token=raw-id-token".to_string(),
                target: "https://target-user:target-secret@example.com/secret?token=raw-query#frag"
                    .to_string(),
                required_for_variants: Vec::new(),
            },
        ];

        let rendered =
            render_secret_requirements(&format!("sha256:{}", "a".repeat(64)), &requirements);
        let encoded_json = serde_json::to_string(&requirements_to_json(&requirements)).unwrap();
        let combined = format!("{rendered}\n{encoded_json}");

        assert!(combined.contains("OPENAI_API_KEY"));
        assert!(combined.contains("[REDACTED:local-path]"));
        assert!(combined.contains("token=[REDACTED:secret-like]"));
        assert!(combined.contains("https://example.com/secret"));
        assert!(combined.contains("[redacted URL credentials/query]"));
        for forbidden in [
            "/Users/alice",
            "/private/tmp",
            "customer-a",
            "customer-variant",
            "raw-target-token",
            "raw-variant-token",
            "raw-id-token",
            "target-user",
            "target-secret",
            "?token=raw-query",
            "#frag",
        ] {
            assert!(
                !combined.contains(forbidden),
                "secret requirement output leaked forbidden text {forbidden}: {combined}"
            );
        }
    }

    #[test]
    fn secret_preflight_error_redacts_requirement_metadata_and_ref_ids() {
        let refs = BTreeMap::from([
            (
                "/Users/alice/private/customer-a/ref-id".to_string(),
                "gcp-secret-manager://projects/proj/secrets/value/versions/latest".to_string(),
            ),
            (
                "token=raw-ref-id".to_string(),
                "plain-secret-value".to_string(),
            ),
        ]);
        let requirements = vec![
            SecretRequirement {
                id: "OPENAI_API_KEY".to_string(),
                target: "/Users/alice/private/customer-a/env token=raw-target-token".to_string(),
                required_for_variants: Vec::new(),
            },
            SecretRequirement {
                id: "token=raw-id-token".to_string(),
                target: "https://target-user:target-secret@example.com/secret?token=raw-query#frag"
                    .to_string(),
                required_for_variants: Vec::new(),
            },
        ];

        let message = validate_secret_refs_for_package(&refs, &requirements)
            .expect_err("refs should not satisfy the package")
            .to_string();

        assert!(message.contains("Secret refs do not match this package."));
        assert!(message.contains("OPENAI_API_KEY"));
        assert!(message.contains("[REDACTED:local-path]"));
        assert!(message.contains("token=[REDACTED:secret-like]"));
        assert!(message.contains("https://example.com/secret"));
        assert!(message.contains("[redacted URL credentials/query]"));
        for forbidden in [
            "/Users/alice",
            "private/customer-a",
            "raw-target-token",
            "raw-id-token",
            "raw-ref-id",
            "target-user",
            "target-secret",
            "?token=raw-query",
            "#frag",
            "plain-secret-value",
        ] {
            assert!(
                !message.contains(forbidden),
                "secret preflight error leaked forbidden text {forbidden}: {message}"
            );
        }
    }

    #[test]
    fn cloud_run_plain_env_rejects_secret_looking_keys_without_values() {
        let env = BTreeMap::from([
            ("OPENAI_API_KEY".to_string(), "sk-live-secret".to_string()),
            ("MODEL".to_string(), "gpt-4.1".to_string()),
        ]);

        let err = validate_cloud_run_plain_env(&env, false)
            .expect_err("secret-looking plain env should be blocked");
        let message = err.to_string();

        assert!(message.contains("OPENAI_API_KEY"));
        assert!(message.contains("--secret-ref"));
        assert!(message.contains("--allow-secret-env"));
        assert!(
            !message.contains("sk-live-secret"),
            "plain env error must not echo secret values: {message}"
        );
    }

    #[test]
    fn cloud_run_plain_env_allows_config_or_explicit_override() {
        let env = BTreeMap::from([
            ("MODEL".to_string(), "gpt-4.1".to_string()),
            ("TEMPERATURE".to_string(), "0".to_string()),
            ("TOKENIZERS_PARALLELISM".to_string(), "false".to_string()),
        ]);

        validate_cloud_run_plain_env(&env, false).expect("ordinary config env");

        let secret_env = BTreeMap::from([(
            "OPENAI_API_KEY".to_string(),
            "intentionally-public".to_string(),
        )]);
        validate_cloud_run_plain_env(&secret_env, true).expect("explicit override");
    }

    #[test]
    fn secret_ref_file_read_error_uses_public_ref() {
        let root = temp_dir("secret_ref_missing_public_ref");
        let path = root
            .join("private")
            .join("customer-a")
            .join("prod-openai-secrets.yaml");

        let err = read_secret_ref_file(&path).expect_err("missing secret ref file should fail");
        let message = format!("{err:#}");

        assert!(message.contains("failed to read secret ref file secret-ref-file://source"));
        let root_text = root.to_string_lossy().to_string();
        for forbidden in [
            root_text.as_str(),
            "private/customer-a",
            "prod-openai-secrets",
        ] {
            assert!(
                !message.contains(forbidden),
                "secret ref file read error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn secret_ref_file_shape_error_uses_public_ref() {
        let root = temp_dir("secret_ref_shape_public_ref");
        let path = root
            .join("private")
            .join("customer-a")
            .join("prod-openai-secrets.yaml");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, "- not-a-map\n").unwrap();

        let err = read_secret_ref_file(&path).expect_err("list secret ref file should fail");
        let message = err.to_string();

        assert_eq!(
            message,
            "secret ref file secret-ref-file://source must be a map of NAME: provider-ref"
        );
        let root_text = root.to_string_lossy().to_string();
        for forbidden in [
            root_text.as_str(),
            "private/customer-a",
            "prod-openai-secrets",
        ] {
            assert!(
                !message.contains(forbidden),
                "secret ref file shape error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn secret_ref_file_entry_error_redacts_private_key_name() {
        let root = temp_dir("secret_ref_entry_public_ref");
        let path = root.join("secrets.json");
        let private_key = "/Users/alice/private/customer-a/OPENAI_API_KEY token=raw-id-token";
        write_json(&path, &json!({ private_key: false }));

        let err = read_secret_ref_file(&path).expect_err("non-string secret ref entry should fail");
        let message = err.to_string();

        assert!(message.contains("secret ref file entry [REDACTED:local-path]"));
        assert!(message.contains("token=[REDACTED:secret-like]"));
        assert!(message.contains("must be a non-empty provider ref string"));
        for forbidden in [
            "/Users/alice",
            "private/customer-a",
            "OPENAI_API_KEY",
            "raw-id-token",
        ] {
            assert!(
                !message.contains(forbidden),
                "secret ref file entry error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn package_archive_uses_sealed_manifest_file_list() {
        let root = temp_dir("package_archive_manifest");
        let package_dir = root.join("package");
        let archive_path = root.join("package.tgz");
        write_minimal_sealed_package(&package_dir);
        fs::write(package_dir.join(".env"), "OPENAI_API_KEY=raw-secret\n").expect("stray env");
        fs::write(
            package_dir.join("files").join("unchecked.txt"),
            "unchecked-secret\n",
        )
        .expect("unchecked payload");

        create_package_archive(&package_dir, &archive_path).expect("create package archive");
        validate_sealed_package_archive(&archive_path).expect("valid package archive");
        let entries = read_tgz_entries(&archive_path).expect("read archive");
        let names = entries.keys().cloned().collect::<Vec<_>>();

        for expected in [
            "manifest.json",
            "checksums.json",
            "package.lock",
            "package_checks.json",
            "resolved_experiment.json",
            "tasks/tasks.jsonl",
            "staging_manifest.json",
            "files/answer.txt",
        ] {
            assert!(
                entries.contains_key(expected),
                "missing {expected}: {names:?}"
            );
        }
        assert!(!entries.contains_key(".env"));
        assert!(!entries.contains_key("files/unchecked.txt"));
        let combined = entries
            .values()
            .map(|bytes| String::from_utf8_lossy(bytes))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!combined.contains("raw-secret"));
        assert!(!combined.contains("unchecked-secret"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn package_archive_bad_checksum_digest_redacts_secret_like_entry() {
        let root = temp_dir("package_archive_bad_checksum_redaction");
        let package_dir = root.join("package");
        let archive_path = root.join("package.tgz");
        write_minimal_sealed_package(&package_dir);
        let checksums_path = package_dir.join("checksums.json");
        let mut checksums =
            read_json_file(&checksums_path, "checksums.json").expect("read checksums");
        checksums
            .pointer_mut("/files")
            .and_then(Value::as_object_mut)
            .expect("checksum files object")
            .insert(
                "private/customer-a/prod-openai-secrets.yaml".to_string(),
                json!(true),
            );
        write_json(&checksums_path, &checksums);

        let err = create_package_archive(&package_dir, &archive_path)
            .expect_err("bad checksum digest should be rejected");
        let message = err.to_string();
        assert!(message.contains("sealed package checksum entry must be a string digest"));
        assert!(message.contains("entry_ref: archive-entry://redacted"));
        let root_text = root.to_string_lossy().to_string();
        for forbidden in [
            "private/customer-a",
            "prod-openai-secrets",
            root_text.as_str(),
        ] {
            assert!(
                !message.contains(forbidden),
                "bad checksum digest error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn package_archive_normalizes_tar_headers() {
        let root = temp_dir("package_archive_headers");
        let package_dir = root.join("package");
        let archive_path = root.join("package.tgz");
        write_minimal_sealed_package(&package_dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                package_dir.join("files").join("answer.txt"),
                fs::Permissions::from_mode(0o600),
            )
            .expect("set source mode");
        }

        create_package_archive(&package_dir, &archive_path).expect("create package archive");
        validate_sealed_package_archive(&archive_path).expect("valid package archive");
        let headers = read_tgz_header_snapshots(&archive_path).expect("read tar headers");
        assert!(!headers.is_empty());
        for (name, header) in headers {
            assert!(header.is_file, "unexpected non-file entry: {name}");
            assert_eq!(header.uid, 0, "uid for {name}");
            assert_eq!(header.gid, 0, "gid for {name}");
            assert_eq!(header.mtime, 0, "mtime for {name}");
            assert_eq!(header.mode, 0o644, "mode for {name}");
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn sealed_package_archive_validation_rejects_local_tar_metadata() {
        let root = temp_dir("package_archive_local_metadata");
        let package_dir = root.join("package");
        let archive_path = root.join("package.tgz");
        write_minimal_sealed_package(&package_dir);
        create_package_archive(&package_dir, &archive_path).expect("create package archive");
        let entries = read_tgz_entries(&archive_path).expect("read archive");
        write_tgz_entries_with_local_metadata(&archive_path, &entries);

        let err = validate_sealed_package_archive(&archive_path)
            .expect_err("local tar metadata should be rejected");
        let message = err.to_string();
        assert!(
            message.contains("normalized uid/gid 0"),
            "unexpected error: {message}"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn sealed_package_archive_validation_rejects_undeclared_extra_file() {
        let root = temp_dir("package_archive_extra");
        let package_dir = root.join("package");
        let archive_path = root.join("package.tgz");
        write_minimal_sealed_package(&package_dir);
        create_package_archive(&package_dir, &archive_path).expect("create package archive");
        let mut entries = read_tgz_entries(&archive_path).expect("read archive");
        entries.insert(".env".to_string(), b"OPENAI_API_KEY=raw-secret\n".to_vec());
        write_tgz_entries(&archive_path, &entries);

        let err = validate_sealed_package_archive(&archive_path)
            .expect_err("extra archive file should be rejected");
        let message = err.to_string();
        assert!(
            message.contains("not declared by manifest/checksums"),
            "unexpected error: {message}"
        );
        assert!(message.contains("extra_count: 1"));
        assert!(message.contains("extra_refs: archive-entry://redacted"));
        assert!(
            !message.contains(".env"),
            "extra archive file error leaked private entry name: {message}"
        );
        assert!(
            !message.contains("OPENAI_API_KEY"),
            "extra archive file error leaked secret-like content: {message}"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn sealed_package_archive_validation_redacts_secret_like_extra_entries() {
        let root = temp_dir("package_archive_secret_extra_ref");
        let package_dir = root.join("package");
        let archive_path = root.join("package.tgz");
        write_minimal_sealed_package(&package_dir);
        create_package_archive(&package_dir, &archive_path).expect("create package archive");
        let mut entries = read_tgz_entries(&archive_path).expect("read archive");
        entries.insert(
            "private/customer-a/prod-openai-secrets.yaml".to_string(),
            b"OPENAI_API_KEY=raw-secret\n".to_vec(),
        );
        write_tgz_entries(&archive_path, &entries);

        let err = validate_sealed_package_archive(&archive_path)
            .expect_err("secret-like extra archive file should be rejected");
        let message = err.to_string();
        assert!(
            message.contains("not declared by manifest/checksums"),
            "unexpected error: {message}"
        );
        assert!(message.contains("extra_refs: archive-entry://redacted"));
        let root_text = root.to_string_lossy().to_string();
        for forbidden in [
            "private/customer-a",
            "prod-openai-secrets",
            "OPENAI_API_KEY",
            "raw-secret",
            root_text.as_str(),
        ] {
            assert!(
                !message.contains(forbidden),
                "secret-like extra archive error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn sealed_package_archive_validation_rejects_arbitrary_archive() {
        let root = temp_dir("package_archive_arbitrary");
        let archive_path = root.join("notes.tgz");
        let entries = BTreeMap::from([("notes.txt".to_string(), b"not a package\n".to_vec())]);
        write_tgz_entries(&archive_path, &entries);

        let err = validate_sealed_package_archive(&archive_path)
            .expect_err("arbitrary archive should be rejected");
        assert!(
            err.to_string().contains("must contain manifest.json"),
            "unexpected error: {}",
            err
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn sealed_package_archive_validation_error_omits_local_archive_path() {
        let root = temp_dir("package_archive_missing_redaction");
        let archive_path = root.join("private-workspace").join("package.tgz");

        let err = validate_sealed_package_archive(&archive_path)
            .expect_err("missing archive should fail");
        let message = format!("{err:#}");

        assert!(
            !message.contains(&root.display().to_string()),
            "error leaked temp root: {message}"
        );
        assert!(
            !message.contains("private-workspace"),
            "error leaked local directory name: {message}"
        );
        assert!(message.contains("failed to open sealed package archive package.tgz"));
    }

    #[test]
    fn package_archive_rejects_archive_output_inside_package_dir() {
        let root = temp_dir("package_archive_inside");
        let package_dir = root.join("package");
        write_minimal_sealed_package(&package_dir);

        let err = create_package_archive(&package_dir, &package_dir.join("package.tgz"))
            .expect_err("archive inside package dir should fail");
        let message = err.to_string();
        assert!(message.contains("archive output must not be inside the sealed package directory"));
        assert!(
            !message.contains(&root.display().to_string()),
            "error leaked temp root: {message}"
        );
        assert!(message.contains("package.tgz"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn package_archive_refuses_symlinked_archive_output_parent_without_write() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("package_archive_symlink_parent");
        let package_dir = root.join("package");
        let outside_dir = root.join("outside-secret-output");
        let link_parent = root.join("archive-link");
        let archive_path = link_parent.join("nested").join("package.tgz");
        write_minimal_sealed_package(&package_dir);
        fs::create_dir_all(&outside_dir).unwrap();
        symlink(&outside_dir, &link_parent).unwrap();

        let err = create_package_archive(&package_dir, &archive_path)
            .expect_err("symlinked archive output parent should fail");
        let message = err.to_string();

        assert!(message
            .contains("refusing to write sealed package archive under symlinked output directory"));
        assert!(message.contains("archive_ref: cloud-upload://archive"));
        assert!(
            !outside_dir.join("nested").exists(),
            "archive output should not be materialized through symlinked parent"
        );
        for forbidden in [
            root.to_str().unwrap(),
            "outside-secret-output",
            "archive-link",
            "package_archive_symlink_parent",
        ] {
            assert!(
                !message.contains(forbidden),
                "symlink archive parent error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn package_archive_refuses_symlinked_archive_output_without_overwrite() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("package_archive_symlink_output");
        let package_dir = root.join("package");
        let outside_archive = root.join("outside-secret-archive.tgz");
        let archive_path = root.join("package.tgz");
        write_minimal_sealed_package(&package_dir);
        fs::write(&outside_archive, "outside archive\n").unwrap();
        symlink(&outside_archive, &archive_path).unwrap();

        let err = create_package_archive(&package_dir, &archive_path)
            .expect_err("symlinked archive output should fail");
        let message = err.to_string();

        assert!(message.contains("refusing to write sealed package archive through symlink"));
        assert!(message.contains("archive_ref: cloud-upload://archive"));
        assert_eq!(
            fs::read_to_string(&outside_archive).unwrap(),
            "outside archive\n"
        );
        for forbidden in [
            root.to_str().unwrap(),
            "outside-secret-archive",
            "package_archive_symlink_output",
        ] {
            assert!(
                !message.contains(forbidden),
                "symlink archive output error leaked forbidden text {forbidden}: {message}"
            );
        }
        fs::remove_file(&archive_path).ok();
        fs::remove_dir_all(root).ok();
    }
}
