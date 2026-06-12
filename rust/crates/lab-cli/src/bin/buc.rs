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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "../cloud_auth_ux.rs"]
mod cloud_auth_ux;
use cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV;

const BUCEPHALUS_CLOUD_API_URL_ENV: &str = "BUCEPHALUS_CLOUD_API_URL";

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
        (Some("packages"), Some("upload")) => package_upload(with_args(&context, rest)),
        (Some("packages"), Some("inspect")) => package_inspect(with_args(&context, rest)),
        (Some("experiments"), Some("build")) => experiment_build(with_args(&context, rest)),
        (Some("experiments"), Some("doctor")) => experiment_doctor(with_args(&context, rest)),
        (Some("runs"), Some("create")) => run_create(with_args(&context, rest)),
        (Some("runs"), Some("get")) => run_get(with_args(&context, rest)),
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
            | (Some("packages"), Some("upload" | "inspect"))
            | (Some("experiments"), Some("build" | "doctor"))
            | (Some("runs"), Some("create" | "get"))
    )
}

fn group_command_without_leaf(group: Option<&str>, command: Option<&str>) -> bool {
    command.is_none() && matches!(group, Some("packages" | "experiments" | "runs"))
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
    ensure_api_configured(&context)?;
    print_json(&cloud_fetch(&context, Method::GET, "/readyz", None, None)?)
}

fn package_upload(context: CliContext) -> Result<()> {
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
    )?;
    if json_output {
        print_json(&imported)?;
    } else {
        print_import_summary(&imported, Some(&prepared.source_label))?;
    }
    ensure_import_accepted(&imported, "package upload")
}

fn experiment_build(context: CliContext) -> Result<()> {
    let path = package_path_arg(&context.args)?;
    let label = option_value(&context.args, "--label")?;
    let json_output = json_requested(&context.args);
    let prepared = prepare_sealed_package_input(Path::new(&path))?;
    ensure_api_configured(&context)?;
    let build = upload_sealed_package_artifact(
        &context,
        &prepared.archive_path,
        label.as_deref(),
        "/v1/experiments/builds",
    )?;
    if json_output {
        print_json(&build)?;
    } else {
        print_build_summary(&build, &prepared.source_label)?;
    }
    ensure_import_accepted(&build, "hosted build")
}

fn package_inspect(context: CliContext) -> Result<()> {
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
    if json_requested(&context.args) {
        print_json(&diagnosis)
    } else {
        print_doctor_summary(&diagnosis)
    }
}

fn run_create(context: CliContext) -> Result<()> {
    let digest = package_digest_arg(&context.args)?;
    let secret_refs = secret_refs_from_options(&context.args)?;
    let runtime_options = runtime_options_from_args(&context.args)?;
    ensure_api_configured(&context)?;

    cloud_fetch(
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

    let run = cloud_fetch(
        &context,
        Method::POST,
        "/v1/runs",
        Some(json!({
            "package_digest": digest,
            "run_label": option_value(&context.args, "--label")?,
            "env": key_value_options(&context.args, "--env")?,
            "secret_refs": secret_refs,
            "runtime_options": Value::Object(runtime_options)
        })),
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&run)
    } else {
        print_run_summary(&run)
    }
}

fn run_get(context: CliContext) -> Result<()> {
    let run_id = positional_arg(&context.args).or(required_option(&context.args, "--run-id").ok());
    let run_id = run_id.ok_or_else(|| anyhow!("run id is required"))?;
    ensure_api_configured(&context)?;
    let run = cloud_fetch(
        &context,
        Method::GET,
        &format!("/v1/runs/{}", encode_path_segment(&run_id)),
        None,
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&run)
    } else {
        print_run_summary(&run)
    }
}

fn package_path_arg(args: &[String]) -> Result<String> {
    positional_arg(args)
        .or(required_option(args, "--file").ok())
        .ok_or_else(|| anyhow!("sealed package directory/archive is required"))
}

fn package_digest_arg(args: &[String]) -> Result<String> {
    positional_arg(args)
        .or(required_option(args, "--package-digest").ok())
        .ok_or_else(|| anyhow!("package digest is required"))
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
    let lower = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        bail!(
            "buc expects a sealed package, not authoring YAML. Hosted authoring build from experiment.yaml is not implemented in the Cloud API yet. Today: run `bucephalus build experiment.yaml --out <package-dir>` locally, then `buc experiments build <package-dir>`. This command never shells out to local Core."
        );
    }
    Ok(())
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
            "manifest.json is not a sealed_run_package_v2 manifest. `buc` uploads sealed packages produced by `bucephalus build`; for authoring YAML run `bucephalus build experiment.yaml --out <package-dir>` first."
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
            "sealed package directory is missing resolved_experiment.json. Rebuild with `bucephalus build experiment.yaml --out <package-dir>` before `buc experiments build`."
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
    cloud_fetch(
        context,
        Method::POST,
        import_path,
        Some(json!({ "upload_id": upload_id, "label": label })),
        None,
    )
}

fn ensure_import_accepted(value: &Value, noun: &str) -> Result<()> {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("import")
                .and_then(|import| import.get("status"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown");
    if status == "accepted" {
        return Ok(());
    }
    let import = value.get("import").unwrap_or(value);
    let detail = import
        .get("error_message")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("the Cloud importer rejected the sealed package");
    bail!("{noun} failed: {detail}");
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
    Ok(value)
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
        let mut message = payload
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Cloud API request failed: {}", status.as_u16()));
        if status.as_u16() == 401 {
            message = append_user_auth_hint(context, message);
        }
        bail!("{message}");
    }
    Ok(payload)
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

fn runtime_options_from_args(args: &[String]) -> Result<Map<String, Value>> {
    let mut runtime_options = Map::new();
    insert_option_string(
        &mut runtime_options,
        "backend",
        option_value(args, "--backend")?,
    );
    insert_option_string(&mut runtime_options, "arch", option_value(args, "--arch")?);
    insert_option_string(
        &mut runtime_options,
        "isolation",
        option_value(args, "--isolation")?,
    );
    insert_option_number(
        &mut runtime_options,
        "cpu_count",
        number_option(args, "--cpu-count")?,
    );
    insert_option_number(
        &mut runtime_options,
        "memory_mb",
        number_option(args, "--memory-mb")?,
    );
    insert_option_number(
        &mut runtime_options,
        "disk_mb",
        number_option(args, "--disk-mb")?,
    );
    insert_option_number(
        &mut runtime_options,
        "timeout_ms",
        number_option(args, "--timeout-ms")?,
    );
    insert_option_number(
        &mut runtime_options,
        "max_parallel_trials",
        number_option(args, "--max-parallel-trials")?,
    );
    if args.iter().any(|arg| arg == "--smoke-test") {
        runtime_options.insert("smoke_test".to_string(), json!(true));
    }
    for (key, value) in key_value_options(args, "--runtime-option")? {
        runtime_options.insert(key, json!(value));
    }
    Ok(runtime_options)
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
        if key.trim().is_empty() {
            bail!("{name} requires KEY=VALUE");
        }
        out.insert(key.trim().to_string(), value.trim().to_string());
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

fn positional_arg(args: &[String]) -> Option<String> {
    let options_with_values = [
        "--api-url",
        "--user-token",
        "--label",
        "--file",
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
        lines.push(format!(
            "next: buc experiments doctor {package_digest} --secret-ref NAME=provider://ref"
        ));
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
    if let Some(package_digest) = value.get("package_digest").and_then(Value::as_str) {
        lines.push(format!("package_digest: {package_digest}"));
        if status == "accepted" {
            lines.push(format!(
                "next: buc experiments doctor {package_digest} --secret-ref NAME=provider://ref"
            ));
        }
    }
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
    if status != "accepted" {
        lines.push(
            "next: fix the package diagnostics, rebuild locally with `bucephalus build`, then rerun `buc experiments build <package-dir>`."
                .to_string(),
        );
    }
    Ok(lines)
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
        for requirement in requirements {
            let target = if requirement.target.is_empty() {
                "(runtime env)".to_string()
            } else {
                requirement.target
            };
            let variants = if requirement.required_for_variants.is_empty() {
                String::new()
            } else {
                format!(" variants={}", requirement.required_for_variants.join(","))
            };
            lines.push(format!("  - {} -> {}{}", requirement.id, target, variants));
        }
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
    lines.push(format!(
        "next: buc runs create {digest} --secret-ref NAME=provider://ref"
    ));
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

fn compact_json(value: &Value) -> Result<String> {
    Ok(serde_json::to_string(value)?)
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
  buc [--api-url URL] [--user-token TOKEN] packages upload <package-dir|package.tgz> [--label TEXT] [--json]
  buc [--api-url URL] [--user-token TOKEN] packages inspect <package-digest> [--json]
  buc [--api-url URL] [--user-token TOKEN] experiments build <package-dir|package.tgz> [--label TEXT] [--json]
  buc [--api-url URL] [--user-token TOKEN] experiments doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]
  buc [--api-url URL] [--user-token TOKEN] runs create <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]
  buc [--api-url URL] [--user-token TOKEN] runs get <run-id> [--json]

Cloud package boundary:
  experiments build accepts a sealed package directory/archive and calls
  POST /v1/experiments/builds after upload. Passing experiment.yaml is rejected
  because hosted authoring build is not implemented in the Cloud API yet.

Runtime options:
  --backend VALUE --arch VALUE --isolation VALUE --cpu-count N --memory-mb N
  --disk-mb N --timeout-ms N --max-parallel-trials N --smoke-test

Environment:
  BUCEPHALUS_CLOUD_API_URL       Hosted API base URL; falls back to the profile
                                 persisted by `bucephalus login`
  BUCEPHALUS_CLOUD_USER_TOKEN    OAuth access token override
"#
}

fn command_help_text(group: Option<&str>, command: Option<&str>) -> Option<&'static str> {
    match (group, command) {
        (Some("health"), None) | (Some("health"), Some("--help" | "-h")) => Some(HEALTH_HELP),
        (Some("packages"), None) | (Some("packages"), Some("--help" | "-h")) => Some(PACKAGES_HELP),
        (Some("packages"), Some("upload")) => Some(PACKAGES_UPLOAD_HELP),
        (Some("packages"), Some("inspect")) => Some(PACKAGES_INSPECT_HELP),
        (Some("experiments"), None) | (Some("experiments"), Some("--help" | "-h")) => {
            Some(EXPERIMENTS_HELP)
        }
        (Some("experiments"), Some("build")) => Some(EXPERIMENTS_BUILD_HELP),
        (Some("experiments"), Some("doctor")) => Some(EXPERIMENTS_DOCTOR_HELP),
        (Some("runs"), None) | (Some("runs"), Some("--help" | "-h")) => Some(RUNS_HELP),
        (Some("runs"), Some("create")) => Some(RUNS_CREATE_HELP),
        (Some("runs"), Some("get")) => Some(RUNS_GET_HELP),
        _ => None,
    }
}

const HEALTH_HELP: &str = r#"buc health

Check hosted API readiness.

Usage:
  buc health
"#;

const PACKAGES_HELP: &str = r#"buc packages

Hosted package commands.

Usage:
  buc packages upload <package-dir|package.tgz> [--label TEXT] [--json]
  buc packages inspect <package-digest> [--json]
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

const EXPERIMENTS_HELP: &str = r#"buc experiments

Hosted experiment workflow commands.

Usage:
  buc experiments build <package-dir|package.tgz> [--label TEXT] [--json]
  buc experiments doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]
"#;

const EXPERIMENTS_BUILD_HELP: &str = r#"buc experiments build

Upload a sealed package and create a hosted build/import record.

Usage:
  buc experiments build <package-dir|package.tgz> [--label TEXT] [--json]

Boundary:
  This command calls POST /v1/experiments/builds after upload. It does not
  compile experiment.yaml in the Cloud yet, and it never shells out to local
  `bucephalus build`. Pass a sealed package from `bucephalus build`.
"#;

const EXPERIMENTS_DOCTOR_HELP: &str = r#"buc experiments doctor

Ask the hosted API whether a package can run with the supplied secrets and
runtime options. This uses the same gates as `buc runs create`.

Usage:
  buc experiments doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]
"#;

const RUNS_HELP: &str = r#"buc runs

Hosted run commands.

Usage:
  buc runs create <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]
  buc runs get <run-id> [--json]
"#;

const RUNS_CREATE_HELP: &str = r#"buc runs create

Preflight with Cloud doctor, then queue a hosted run.

Usage:
  buc runs create <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]
"#;

const RUNS_GET_HELP: &str = r#"buc runs get

Fetch hosted run status.

Usage:
  buc runs get <run-id> [--json]
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
        std::env::temp_dir().join(format!("buc_cli_{label}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn hosted_authoring_yaml_is_rejected_before_api_config() {
        let _lock = lock_env();
        let home = temp_dir("authoring_reject");
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

        assert!(err.contains("Hosted authoring build from experiment.yaml is not implemented"));
        assert!(err.contains("never shells out to local Core"));
        assert!(!err.contains("hosted API URL"));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn help_is_hosted_product_cli_not_operator_cli() {
        let help = help_text();

        assert!(help.contains("experiments build <package-dir|package.tgz>"));
        assert!(help.contains("runs create <package-digest>"));
        assert!(help.contains("POST /v1/experiments/builds"));
        assert!(!help.contains("runner-pool"));
        assert!(!help.contains("runner-instance"));
        assert!(!help.contains("build-upload"));
        assert!(!help.contains("--core-cmd"));
        assert!(!help.contains("bucephalus-cloud"));
    }

    #[test]
    fn product_command_set_excludes_retired_operator_and_wrapper_commands() {
        assert!(known_hosted_command(Some("experiments"), Some("build")));
        assert!(known_hosted_command(Some("experiments"), Some("doctor")));
        assert!(known_hosted_command(Some("runs"), Some("create")));
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
        assert!(err.contains("uploads sealed packages produced by `bucephalus build`"));
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
        assert!(text.contains("fix the package diagnostics"));
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
    fn runtime_options_are_typed_for_cloud_api() {
        let args = vec![
            "--backend".to_string(),
            "cloud-runner".to_string(),
            "--cpu-count".to_string(),
            "4".to_string(),
            "--smoke-test".to_string(),
            "--runtime-option".to_string(),
            "materialize=copy".to_string(),
        ];

        let options = runtime_options_from_args(&args).unwrap();

        assert_eq!(options["backend"], json!("cloud-runner"));
        assert_eq!(options["cpu_count"], json!(4));
        assert_eq!(options["smoke_test"], json!(true));
        assert_eq!(options["materialize"], json!("copy"));
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
}
