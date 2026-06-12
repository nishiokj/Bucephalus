use anyhow::{anyhow, bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "../cloud_auth_ux.rs"]
mod cloud_auth_ux;
use cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV;

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
        _ if retired_product_cloud_command(group, command) => bail!(
            "{} is an internal Cloud operator utility and no longer supports product upload/run workflows. Use `buc` for hosted Cloud package upload, doctor, and runs.",
            local_command_name(group, command)
        ),
        _ if !known_operator_cloud_command(group, command) => bail!(
            "unknown command: {}",
            entered_command_name(group, command)
        ),
        _ if context.api_url.is_empty() => bail!(
            "no Cloud API configured. Product users should run `buc login`; operator/dev workflows must pass --api-url or set {}.",
            BUCEPHALUS_CLOUD_API_URL_ENV
        ),
        (Some("health"), None) => print_json(&cloud_fetch(
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
        (Some("runner-pool"), Some("create")) => runner_pool_create(with_args(&context, rest)),
        (Some("runner-pool"), Some("list")) => runner_pool_list(with_args(&context, rest)),
        (Some("runner-instance"), Some("drain")) => {
            runner_instance_drain(with_args(&context, rest))
        }
        _ => bail!("unknown command: {}", entered_command_name(group, command)),
    }
}

fn known_operator_cloud_command(group: Option<&str>, command: Option<&str>) -> bool {
    matches!(
        (group, command),
        (Some("health"), None)
            | (Some("runner-pool"), Some("create" | "list"))
            | (Some("runner-instance"), Some("drain"))
    )
}

fn retired_product_cloud_command(group: Option<&str>, command: Option<&str>) -> bool {
    matches!(
        (group, command),
        (Some("registry"), _)
            | (Some("draft"), _)
            | (Some("deploy"), _)
            | (Some("build-upload"), _)
            | (Some("import"), _)
            | (Some("package"), _)
            | (Some("run"), _)
    )
}

fn entered_command_name(group: Option<&str>, command: Option<&str>) -> String {
    [group, command]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

fn local_command_name(group: Option<&str>, command: Option<&str>) -> String {
    [
        "bucephalus-cloud",
        group.unwrap_or(""),
        command.unwrap_or(""),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ")
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
    if user_token.is_none() {
        user_token = shared_cloud_user_token()?;
    }
    if api_url.trim().is_empty() {
        if let Ok(home) = lab_core::bucephalus_home() {
            if let Some(url) = lab_core::cloud_profile_string(&home, "/api_url") {
                api_url = url;
            }
        }
    }

    Ok(CliContext {
        api_url: api_url.trim_end_matches('/').to_string(),
        user_token,
        worker_token,
        runner_admin_token,
        args,
        client: Client::new(),
    })
}

#[derive(Debug, Clone)]
struct CloudTokenPaths {
    access: PathBuf,
    refresh: PathBuf,
    cache: PathBuf,
}

fn shared_cloud_user_token() -> Result<Option<String>> {
    let home = match lab_core::bucephalus_home() {
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
        .with_context(|| format!("failed to refresh Cloud token at {}", token_endpoint))?;
    let status = response.status().as_u16();
    let bytes = response.bytes()?.to_vec();
    if !(200..300).contains(&status) {
        let message = String::from_utf8_lossy(&bytes);
        return Err(anyhow!(
            "Cloud token refresh failed with status {}: {}",
            status,
            message.trim()
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
    print_json(&json!({
        "exported": target.display().to_string(),
        "source": Path::new(&draft_path).file_name().and_then(|value| value.to_str()).unwrap_or(&draft_path),
        "format": response.get("format").cloned().unwrap_or_else(|| json!(format)),
        "issues": response.get("issues").cloned().unwrap_or_else(|| json!([]))
    }))
}

fn build_upload(context: CliContext) -> Result<()> {
    let experiment =
        positional_arg(&context.args).or(required_option(&context.args, "--file").ok());
    let experiment = experiment.ok_or_else(|| anyhow!("--file is required"))?;
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

    let result = (|| {
        run_core_build(
            &core_command,
            &experiment,
            &package_dir,
            overrides.as_deref(),
        )?;
        create_package_archive(&package_dir, &archive_path)?;
        let imported = upload_sealed_package_artifact(&context, &archive_path, label.as_deref())?;
        let mut output = imported.as_object().cloned().unwrap_or_else(|| {
            let mut object = Map::new();
            object.insert("import".to_string(), imported);
            object
        });
        if let Some(out_dir) = out_dir {
            output.insert(
                "package_dir".to_string(),
                json!(out_dir.display().to_string()),
            );
        }
        if let Some(archive_out) = archive_out {
            output.insert(
                "archive_path".to_string(),
                json!(archive_out.display().to_string()),
            );
        }
        print_json(&Value::Object(output))
    })();

    let _ = fs::remove_dir_all(&tmp_root);
    result
}

fn import_sealed_package(context: CliContext) -> Result<()> {
    let path = positional_arg(&context.args).or(required_option(&context.args, "--file").ok());
    let path = path.ok_or_else(|| anyhow!("--file is required"))?;
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
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let expected_digest = sha256_digest(&bytes);
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
            "byte_size": bytes.len()
        })),
        None,
        AuthMode::User,
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
        AuthMode::User,
    )?;
    cloud_fetch(
        context,
        Method::POST,
        &format!("/v1/uploads/{upload_id}/complete"),
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
    let mut entries = fs::read_dir(package_dir)
        .with_context(|| format!("failed to read build output {}", package_dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.is_empty() {
        bail!("build output directory is empty: {}", package_dir.display());
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

fn import_inspect(context: CliContext) -> Result<()> {
    let import_id =
        positional_arg(&context.args).or(required_option(&context.args, "--import-id").ok());
    let import_id = import_id.ok_or_else(|| anyhow!("--import-id is required"))?;
    let job = cloud_fetch(
        &context,
        Method::GET,
        &format!("/v1/imports/{import_id}"),
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
    let digest =
        positional_arg(&context.args).or(required_option(&context.args, "--package-digest").ok());
    let digest = digest.ok_or_else(|| anyhow!("--package-digest is required"))?;
    print_json(&package_get_object(&context, &digest)?)
}

fn package_secrets(context: CliContext) -> Result<()> {
    let digest =
        positional_arg(&context.args).or(required_option(&context.args, "--package-digest").ok());
    let digest = digest.ok_or_else(|| anyhow!("--package-digest is required"))?;
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
    let secret_refs = secret_refs_from_options(&context.args)?;
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
            "env": key_value_options(&context.args, "--env")?,
            "secret_refs": secret_refs,
            "runtime_options": Value::Object(runtime_options)
        })),
        None,
        AuthMode::User,
    )?)
}

fn run_get(context: CliContext) -> Result<()> {
    let run_id = positional_arg(&context.args).or(required_option(&context.args, "--run-id").ok());
    let run_id = run_id.ok_or_else(|| anyhow!("--run-id is required"))?;
    print_json(&cloud_fetch(
        &context,
        Method::GET,
        &format!("/v1/runs/{}", encode_path_segment(&run_id)),
        None,
        None,
        AuthMode::User,
    )?)
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
    let runner_instance_id =
        positional_arg(&context.args)
            .or(required_option(&context.args, "--runner-instance-id").ok());
    let runner_instance_id =
        runner_instance_id.ok_or_else(|| anyhow!("--runner-instance-id is required"))?;
    print_json(&cloud_fetch(
        &context,
        Method::POST,
        &format!(
            "/v1/runner-instances/{}/drain",
            encode_path_segment(&runner_instance_id)
        ),
        Some(json!({})),
        None,
        AuthMode::RunnerAdmin,
    )?)
}

fn package_get_object(context: &CliContext, digest: &str) -> Result<Value> {
    let value = cloud_fetch(
        context,
        Method::GET,
        &format!("/v1/packages/{}", encode_path_segment(digest)),
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
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed = if path.extension().and_then(|value| value.to_str()) == Some("json") {
        serde_json::from_str(&raw)?
    } else {
        let yaml: serde_yaml::Value = serde_yaml::from_str(&raw)?;
        serde_json::to_value(yaml)?
    };
    if !parsed.is_object() {
        bail!("draft file must parse to an object: {}", path.display());
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
        let mut message = payload
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Cloud API request failed: {}", status.as_u16()));
        if status.as_u16() == 401 && matches!(auth, AuthMode::User) {
            message = append_user_auth_hint(&context, message);
        }
        bail!("{message}");
    }
    Ok(payload)
}

fn append_user_auth_hint(context: &CliContext, message: String) -> String {
    let token_path = lab_core::bucephalus_home()
        .ok()
        .map(|home| cloud_token_paths(&home).access);
    cloud_auth_ux::user_auth_hint(
        &message,
        context.user_token.is_some(),
        token_path.as_deref(),
    )
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
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed: Value = if path.extension().and_then(|value| value.to_str()) == Some("json") {
        serde_json::from_str(&raw)?
    } else {
        serde_json::to_value(serde_yaml::from_str::<serde_yaml::Value>(&raw)?)?
    };
    let object = parsed.as_object().ok_or_else(|| {
        anyhow!(
            "secret ref file must be a map of NAME: provider-ref, got {}",
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
                anyhow!("secret ref file entry {key} must be a non-empty provider ref string")
            })?;
        refs.insert(key.clone(), value.to_string());
    }
    Ok(refs)
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
        lines.push(format!("Missing: {}", missing.join(", ")));
    }
    if !unknown.is_empty() {
        lines.push(format!("Unknown: {}", unknown.join(", ")));
    }
    if !unsupported.is_empty() {
        lines.push(format!(
            "Unsupported ref format: {}",
            unsupported.join(", ")
        ));
    }
    if !requirements.is_empty() {
        lines.push(String::new());
        lines.push("Required secrets:".to_string());
        for requirement in requirements {
            lines.push(format!("  {} -> {}", requirement.id, requirement.target));
        }
        lines.push(String::new());
        lines.push("Pass refs with:".to_string());
        for requirement in requirements {
            lines.push(format!(
                "  --secret-ref {}=gcp-secret-manager://projects/<project>/secrets/<secret>/versions/<version>",
                requirement.id
            ));
        }
    }
    bail!("{}", lines.join("\n"));
}

fn supported_secret_ref(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("gcp-secret-manager://") || value.starts_with("aws-secrets-manager://")
}

fn requirements_to_json(requirements: &[SecretRequirement]) -> Value {
    Value::Array(
        requirements
            .iter()
            .map(|item| {
                json!({
                    "id": item.id,
                    "target": item.target,
                    "required_for_variants": item.required_for_variants
                })
            })
            .collect(),
    )
}

fn required_option(args: &[String], name: &str) -> Result<String> {
    option_value(args, name)?.ok_or_else(|| anyhow!("{name} is required"))
}

fn option_value(args: &[String], name: &str) -> Result<Option<String>> {
    if let Some(index) = args.iter().position(|arg| arg == name) {
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{name} requires a value"))?;
        Ok(Some(value.clone()))
    } else {
        Ok(None)
    }
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

fn positional_arg(args: &[String]) -> Option<String> {
    let options_with_values = [
        "--import-id",
        "--action-file",
        "--aliases",
        "--label",
        "--file",
        "--package-digest",
        "--run-id",
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
        lines.push(format!("Error: {error_message}"));
    }
    if let Some(diagnostics) = object.get("diagnostics").and_then(Value::as_array) {
        if !diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Diagnostics:".to_string());
            for diagnostic in diagnostics {
                lines.push(format!(
                    "  - [{}] {} {}: {}",
                    diagnostic
                        .get("severity")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    diagnostic
                        .get("code")
                        .and_then(Value::as_str)
                        .unwrap_or("diagnostic"),
                    diagnostic
                        .get("pointer")
                        .and_then(Value::as_str)
                        .unwrap_or("/"),
                    diagnostic
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                ));
            }
        }
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_secret_requirements(
    package_digest: &str,
    requirements: &[SecretRequirement],
) -> Result<()> {
    if requirements.is_empty() {
        println!("Package {package_digest} does not declare runtime secrets.");
        return Ok(());
    }
    let mut lines = vec![
        format!("Package: {package_digest}"),
        "Required runtime secrets:".to_string(),
    ];
    for requirement in requirements {
        let variants = if requirement.required_for_variants.is_empty() {
            String::new()
        } else {
            format!(" variants={}", requirement.required_for_variants.join(","))
        };
        lines.push(format!(
            "  - {} -> {}{}",
            requirement.id, requirement.target, variants
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
            requirement.id
        ));
    }
    lines.extend([
        String::new(),
        "Queue with:".to_string(),
        format!("  bucephalus-cloud run create --package-digest {package_digest} --secret-ref-file secrets.yaml"),
    ]);
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_help() {
    println!(
        r#"Bucephalus Cloud operator utility

This binary is for internal Cloud service/operator checks. Product Cloud
upload/run workflows are intentionally not exposed here; use `buc` for package
upload, doctor, and Cloud runs.

Usage:
  bucephalus-cloud [--api-url URL] [--user-token TOKEN] health
  bucephalus-cloud [--api-url URL] [--worker-token TOKEN] runner-pool create --name cloud-runner-pool --executors runner-docker --resources core_runner,docker_daemon,registry_pull [--arch x86_64|arm64] [--cpu-count N] [--memory-mb N] [--disk-mb N] [--isolation reusable_vm]
  bucephalus-cloud [--api-url URL] [--worker-token TOKEN] runner-pool list
  bucephalus-cloud [--api-url URL] [--worker-token TOKEN] runner-instance drain <runner-instance-id>

Environment:
  BUCEPHALUS_CLOUD_API_URL       Cloud API base URL; falls back to the profile
                                 persisted by `bucephalus login` (no localhost default)
  BUCEPHALUS_CLOUD_USER_TOKEN    OAuth access token override for health checks
  BUCEPHALUS_CLOUD_WORKER_TOKEN  Required for runner pool and worker management commands
  BUCEPHALUS_CLOUD_RUNNER_ADMIN_TOKEN
                                 Optional token for runner pool/admin commands

User auth:
  Product upload/run auth is owned by `buc`.
"#
    );
}

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
    fn product_upload_commands_are_retired_from_operator_cli() {
        let _lock = lock_env();
        let home = temp_dir("retired_product_command");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        let err = run(vec![
            "deploy".to_string(),
            "experiment.yaml".to_string(),
            "--label".to_string(),
            "demo".to_string(),
        ])
        .expect_err("operator CLI must not expose product upload workflows");
        let message = err.to_string();

        assert!(
            message.contains("no longer supports product upload/run workflows"),
            "unexpected error: {message}"
        );
        assert!(
            !message.contains(BUCEPHALUS_CLOUD_API_URL_ENV),
            "retired command should fail before asking for an API URL: {message}"
        );
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn cli_ux_unknown_commands_report_unknown_before_cloud_configuration() {
        let _lock = lock_env();
        let home = temp_dir("unknown_before_api");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        let err = run(vec!["build".to_string(), "--help".to_string()])
            .expect_err("unknown command should not ask for Cloud config first");
        let message = err.to_string();

        assert!(
            message.contains("unknown command: build --help"),
            "unexpected error: {message}"
        );
        assert!(
            !message.contains(BUCEPHALUS_CLOUD_API_URL_ENV),
            "unknown command should fail before asking for an API URL: {message}"
        );
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn cli_ux_all_retired_product_commands_explain_retirement_before_api_config() {
        let _lock = lock_env();
        let home = temp_dir("retired_matrix");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        for args in [
            vec!["registry", "search"],
            vec!["draft", "validate"],
            vec!["deploy", "--help"],
            vec!["build-upload", "experiment.yaml"],
            vec!["import", "sealed-package"],
            vec!["package", "get"],
            vec!["run", "create"],
        ] {
            let err = run(args.iter().map(|arg| (*arg).to_string()).collect())
                .expect_err("retired product commands should be blocked");
            let message = err.to_string();
            assert!(
                message.contains("no longer supports product upload/run workflows"),
                "unexpected error for {args:?}: {message}"
            );
            assert!(
                !message.contains(BUCEPHALUS_CLOUD_API_URL_ENV),
                "retired command should fail before asking for an API URL for {args:?}: {message}"
            );
        }
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn cli_ux_operator_commands_require_cloud_configuration() {
        let _lock = lock_env();
        let home = temp_dir("operator_requires_api");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        let err = run(vec!["runner-pool".to_string(), "list".to_string()])
            .expect_err("operator commands need Cloud API config");
        let message = err.to_string();

        assert!(message.contains("no Cloud API configured"));
        assert!(message.contains("Product users should run `buc login`"));
        assert!(message.contains("operator/dev workflows must pass --api-url"));
        assert!(message.contains(BUCEPHALUS_CLOUD_API_URL_ENV));
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn cli_ux_help_does_not_require_cloud_configuration() {
        let _lock = lock_env();
        let home = temp_dir("help_no_api");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        run(vec!["help".to_string()]).expect("help should render without Cloud config");
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn cli_ux_build_is_not_a_cloud_operator_or_retired_product_command() {
        assert!(!known_operator_cloud_command(Some("build"), Some("--help")));
        assert!(!retired_product_cloud_command(
            Some("build"),
            Some("--help")
        ));
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
        assert!(message.contains("cloud_user_token"));
        assert!(message.contains("The CLI did not find a user bearer token"));
        fs::remove_dir_all(home).ok();
    }
}
