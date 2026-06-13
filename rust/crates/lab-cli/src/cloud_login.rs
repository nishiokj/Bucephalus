use anyhow::{anyhow, Context, Result};
use base64::Engine;
use percent_encoding::utf8_percent_encode;
use percent_encoding::NON_ALPHANUMERIC;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use sha2::Digest;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const BUCEPHALUS_CLOUD_API_URL_ENV: &str = "BUCEPHALUS_CLOUD_API_URL";
pub const BUCEPHALUS_CLOUD_OAUTH_ISSUER_ENV: &str = "BUCEPHALUS_CLOUD_OAUTH_ISSUER";
pub const BUCEPHALUS_CLOUD_OAUTH_CLIENT_ID_ENV: &str = "BUCEPHALUS_CLOUD_OAUTH_CLIENT_ID";
pub const BUCEPHALUS_CLOUD_OAUTH_AUDIENCE_ENV: &str = "BUCEPHALUS_CLOUD_OAUTH_AUDIENCE";
pub const BUCEPHALUS_CLOUD_OAUTH_SCOPE_ENV: &str = "BUCEPHALUS_CLOUD_OAUTH_SCOPE";
const FALLBACK_BUCEPHALUS_CLOUD_API_URL: &str = "https://api.bucephalus.dev";
const LOGIN_CALLBACK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub fn default_bucephalus_cloud_api_url() -> &'static str {
    option_env!("BUCEPHALUS_HOSTED_API_URL")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(FALLBACK_BUCEPHALUS_CLOUD_API_URL)
}

#[derive(Debug, Clone)]
pub struct DeviceLoginOptions {
    pub issuer: Option<String>,
    pub client_id: Option<String>,
    pub audience: Option<String>,
    pub api_url: Option<String>,
    pub scope: Option<String>,
    pub no_browser: bool,
}

#[derive(Debug, Clone)]
pub struct CloudTokenPaths {
    pub access: PathBuf,
    pub refresh: PathBuf,
    pub cache: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct CloudAuthConfig {
    issuer: Option<String>,
    client_id: Option<String>,
    audience: Option<String>,
    scope: Option<String>,
}

pub fn run_login(options: DeviceLoginOptions) -> Result<Value> {
    let home = lab_core::bucephalus_home()?;
    let paths = cloud_token_paths(&home);
    let api_url = options
        .api_url
        .or_else(cloud_api_base_url)
        .unwrap_or_else(|| default_bucephalus_cloud_api_url().to_string())
        .trim_end_matches('/')
        .to_string();
    let mut discovery_error = None;
    let discovered = match fetch_cloud_auth_config(&api_url) {
        Ok(config) => config,
        Err(err) => {
            discovery_error = Some(err);
            CloudAuthConfig::default()
        }
    };
    let issuer = options
        .issuer
        .or_else(|| env_trimmed(BUCEPHALUS_CLOUD_OAUTH_ISSUER_ENV))
        .or_else(|| lab_core::cloud_profile_string(&home, "/oauth/issuer"))
        .or(discovered.issuer)
        .ok_or_else(|| {
            hosted_auth_discovery_error(&api_url, discovery_error.as_ref())
        })?;
    let audience = options
        .audience
        .or_else(|| env_trimmed(BUCEPHALUS_CLOUD_OAUTH_AUDIENCE_ENV))
        .or_else(|| lab_core::cloud_profile_string(&home, "/oauth/audience"))
        .or(discovered.audience);
    let scope = options
        .scope
        .or_else(|| env_trimmed(BUCEPHALUS_CLOUD_OAUTH_SCOPE_ENV))
        .or_else(|| lab_core::cloud_profile_string(&home, "/oauth/scope"))
        .or(discovered.scope)
        .unwrap_or_else(|| "openid profile email".to_string());
    let (metadata_url, metadata) = fetch_oauth_metadata(&issuer)?;
    let token_endpoint = metadata
        .get("token_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!(
                "OAuth metadata {} does not include token_endpoint",
                metadata_url
            )
        })?
        .to_string();
    let client_id = options
        .client_id
        .or_else(|| env_trimmed(BUCEPHALUS_CLOUD_OAUTH_CLIENT_ID_ENV))
        .or_else(|| lab_core::cloud_profile_string(&home, "/oauth/client_id"))
        .or(discovered.client_id)
        .map(Ok)
        .unwrap_or_else(|| dynamic_register_oauth_client(&metadata, &issuer, &scope))?;

    let token = if let Some(authorization_endpoint) = metadata
        .get("authorization_endpoint")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        browser_authorization_code_login(
            authorization_endpoint,
            &token_endpoint,
            &client_id,
            &scope,
            options.no_browser,
        )?
    } else {
        let device_authorization_endpoint = metadata
            .get("device_authorization_endpoint")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!(
                    "OAuth metadata {} does not include authorization_endpoint or device_authorization_endpoint",
                    metadata_url
                )
            })?
            .to_string();
        let device = begin_device_authorization(
            &device_authorization_endpoint,
            &client_id,
            &scope,
            audience.as_deref(),
            Some(&api_url),
        )?;
        let verification_uri = device
            .get("verification_uri")
            .or_else(|| device.get("verification_url"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("device authorization response missing verification_uri"))?;
        let verification_uri_complete = device
            .get("verification_uri_complete")
            .and_then(Value::as_str);
        let user_code = device
            .get("user_code")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("device authorization response missing user_code"))?;

        if !options.no_browser {
            let _ = open_login_url(verification_uri_complete.unwrap_or(verification_uri));
        }
        eprintln!("Bucephalus Cloud login");
        eprintln!(
            "Open: {}",
            verification_uri_complete.unwrap_or(verification_uri)
        );
        eprintln!("Code: {user_code}");
        eprintln!("Waiting for authorization...");

        poll_device_token(&token_endpoint, &client_id, &device)?
    };
    write_cloud_token_cache(
        &paths,
        &issuer,
        &client_id,
        audience.as_deref(),
        Some(&api_url),
        &scope,
        &token_endpoint,
        &token,
    )?;
    lab_core::write_cloud_profile(
        &home,
        &json!({
            "schema_version": "bucephalus_cloud_profile_v1",
            "api_url": api_url,
            "oauth": {
                "issuer": issuer,
                "client_id": client_id,
                "audience": audience,
                "scope": scope,
            },
        }),
    )?;
    Ok(json!({
        "schema_version": "bucephalus_login_v1",
        "ok": true,
        "home": home,
        "issuer": issuer,
        "client_id": client_id,
        "audience": audience,
        "api_url": api_url,
        "scope": scope,
        "token_path": paths.access,
        "refresh_token_path": if paths.refresh.is_file() { Some(paths.refresh.display().to_string()) } else { None },
        "cache_path": paths.cache
    }))
}

pub fn run_logout(dry_run: bool) -> Result<Value> {
    let home = lab_core::bucephalus_home()?;
    let paths = cloud_token_paths(&home);
    let env_token_present =
        std::env::var_os(crate::cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV).is_some();
    let auth_files = [
        ("access_token", paths.access.clone()),
        ("refresh_token", paths.refresh.clone()),
        ("token_cache", paths.cache.clone()),
    ];
    let mut files = Vec::new();
    let mut removed_count = 0usize;
    let mut planned_count = 0usize;
    let mut missing_count = 0usize;

    for (kind, path) in auth_files {
        let existed = path.exists();
        let status = if existed {
            if !path.is_file() {
                return Err(anyhow!(
                    "Cloud auth cleanup expected a file but found a non-file path at {}; inspect this path manually before retrying",
                    path.display()
                ));
            }
            if dry_run {
                planned_count += 1;
                "planned"
            } else {
                fs::remove_file(&path)?;
                removed_count += 1;
                "removed"
            }
        } else {
            missing_count += 1;
            "missing"
        };
        files.push(json!({
            "kind": kind,
            "path": path,
            "existed": existed,
            "status": status
        }));
    }

    let status = if env_token_present {
        "env_override_present"
    } else if dry_run && planned_count > 0 {
        "planned"
    } else if removed_count > 0 {
        "removed"
    } else {
        "missing"
    };

    Ok(json!({
        "schema_version": "bucephalus_logout_v1",
        "ok": true,
        "dry_run": dry_run,
        "status": status,
        "home": home,
        "files": files,
        "removed_count": removed_count,
        "planned_count": planned_count,
        "missing_count": missing_count,
        "env": {
            "name": crate::cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV,
            "present": env_token_present,
            "note": if env_token_present {
                Some(format!("{} is still set in this process; unset it in your shell or environment manager to fully log out.", crate::cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV))
            } else {
                None
            }
        },
        "auth": auth_status_for_home(&home)
    }))
}

pub fn auth_status() -> Result<Value> {
    let home = lab_core::bucephalus_home()?;
    Ok(json!({
        "schema_version": "bucephalus_cloud_auth_status_v1",
        "ok": true,
        "home": home,
        "auth": auth_status_for_home(&home)
    }))
}

pub fn shared_cloud_user_token() -> Result<Option<String>> {
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

pub fn cloud_token_paths(home: &Path) -> CloudTokenPaths {
    let auth_dir = home.join("auth");
    CloudTokenPaths {
        access: auth_dir.join("cloud_user_token"),
        refresh: auth_dir.join("cloud_refresh_token"),
        cache: auth_dir.join("cloud_user_token.json"),
    }
}

pub fn read_cloud_token_cache(paths: &CloudTokenPaths) -> Option<Value> {
    let raw = fs::read_to_string(&paths.cache).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn cloud_token_cache_needs_refresh(cache: &Value) -> bool {
    let Some(expires_at_ms) = cache.get("expires_at_ms").and_then(Value::as_i64) else {
        return false;
    };
    let Some(refresh_token) = cache.get("refresh_token").and_then(Value::as_str) else {
        return false;
    };
    !refresh_token.trim().is_empty() && expires_at_ms <= current_unix_time_ms() + 60_000
}

pub fn refresh_cloud_token_cache(paths: &CloudTokenPaths, cache: &Value) -> Result<String> {
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
        bail_with_status("Cloud token refresh failed", status, &bytes)?;
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
    write_cloud_token_cache_from_existing(paths, cache, &merged)?;
    merged
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Cloud token refresh response missing access_token"))
}

pub fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
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

fn auth_status_for_home(home: &Path) -> Value {
    let paths = cloud_token_paths(home);
    let api_url = effective_cloud_api_base_url();
    if std::env::var_os(crate::cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV).is_some() {
        return json!({
            "status": "ready",
            "source": "env",
            "env": crate::cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV,
            "api_url": api_url
        });
    }
    if paths.access.is_file() {
        return json!({
            "status": "ready",
            "source": "file",
            "path": paths.access,
            "refresh_token_path": if paths.refresh.is_file() { Some(paths.refresh.display().to_string()) } else { None },
            "cache_path": if paths.cache.is_file() { Some(paths.cache.display().to_string()) } else { None },
            "api_url": api_url
        });
    }
    json!({
        "status": "missing",
        "source": null,
        "expected": [
            crate::cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV,
            paths.access.display().to_string()
        ],
        "api_url": api_url,
        "actions": [
            {
                "type": "cli_command",
                "command": "buc login",
                "description": "Start OAuth device login and cache Cloud tokens for this user."
            }
        ],
        "oauth": {
            "issuer_env": BUCEPHALUS_CLOUD_OAUTH_ISSUER_ENV,
            "client_id_env": BUCEPHALUS_CLOUD_OAUTH_CLIENT_ID_ENV,
            "audience_env": BUCEPHALUS_CLOUD_OAUTH_AUDIENCE_ENV,
            "scope_env": BUCEPHALUS_CLOUD_OAUTH_SCOPE_ENV
        }
    })
}

fn cloud_api_base_url() -> Option<String> {
    env_trimmed(BUCEPHALUS_CLOUD_API_URL_ENV)
        .map(|value| value.trim_end_matches('/').to_string())
        .or_else(|| {
            let home = lab_core::bucephalus_home().ok()?;
            lab_core::cloud_profile_string(&home, "/api_url")
                .map(|value| value.trim_end_matches('/').to_string())
        })
}

fn effective_cloud_api_base_url() -> String {
    cloud_api_base_url().unwrap_or_else(|| default_bucephalus_cloud_api_url().to_string())
}

fn hosted_auth_discovery_error(api_url: &str, err: Option<&anyhow::Error>) -> anyhow::Error {
    let detail = err
        .map(|err| format!("\nCause: {err:#}"))
        .unwrap_or_default();
    anyhow!(
        "Bucephalus Cloud login is unavailable because the hosted auth configuration could not be read from {api_url}/v1/auth/config.{detail}\nThis is a hosted service or release configuration problem. A normal hosted user should only need to run `buc login`."
    )
}

fn fetch_cloud_auth_config(api_url: &str) -> Result<CloudAuthConfig> {
    let url = format!("{}/v1/auth/config", api_url.trim_end_matches('/'));
    let value = http_get_json(&url)?;
    Ok(CloudAuthConfig {
        issuer: string_field(&value, "issuer"),
        client_id: string_field(&value, "client_id"),
        audience: string_field(&value, "audience"),
        scope: string_field(&value, "scope"),
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn env_trimmed(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn oauth_metadata_url(issuer: &str) -> Result<String> {
    let issuer = issuer.trim().trim_end_matches('/');
    if issuer.is_empty() {
        return Err(anyhow!("OAuth issuer must not be empty"));
    }
    if is_oauth_metadata_url(issuer) {
        return Ok(issuer.to_string());
    }
    let parsed = reqwest::Url::parse(issuer)
        .with_context(|| format!("invalid OAuth issuer URL {}", issuer))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(anyhow!("OAuth issuer URL must use http or https"));
    }
    Ok(format!("{issuer}/.well-known/oauth-authorization-server"))
}

fn openid_metadata_url(issuer: &str) -> Result<String> {
    let issuer = issuer.trim().trim_end_matches('/');
    if is_oauth_metadata_url(issuer) {
        return Ok(issuer.to_string());
    }
    let parsed = reqwest::Url::parse(issuer)
        .with_context(|| format!("invalid OAuth issuer URL {}", issuer))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(anyhow!("OAuth issuer URL must use http or https"));
    }
    Ok(format!("{issuer}/.well-known/openid-configuration"))
}

fn is_oauth_metadata_url(url: &str) -> bool {
    url.ends_with("/.well-known/oauth-authorization-server")
        || url.ends_with("/.well-known/openid-configuration")
}

fn fetch_oauth_metadata(issuer: &str) -> Result<(String, Value)> {
    let metadata_url = oauth_metadata_url(issuer)?;
    match http_get_json(&metadata_url) {
        Ok(metadata) => Ok((metadata_url, metadata)),
        Err(err) if !is_oauth_metadata_url(issuer.trim().trim_end_matches('/')) => {
            let openid_url = openid_metadata_url(issuer)?;
            http_get_json(&openid_url)
                .map(|metadata| (openid_url, metadata))
                .with_context(|| {
                    format!(
                        "failed to fetch OAuth metadata from {} or OpenID metadata fallback",
                        metadata_url
                    )
                })
        }
        Err(err) => Err(err),
    }
}

fn browser_authorization_code_login(
    authorization_endpoint: &str,
    token_endpoint: &str,
    client_id: &str,
    scope: &str,
    no_browser: bool,
) -> Result<Value> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("failed to bind local OAuth callback listener on 127.0.0.1")?;
    listener
        .set_nonblocking(true)
        .context("failed to configure local OAuth callback listener")?;
    let redirect_uri = format!("http://{}/callback", listener.local_addr()?);
    let state = random_url_token(32)?;
    let code_verifier = random_url_token(64)?;
    let code_challenge = pkce_challenge(&code_verifier);
    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline",
        authorization_endpoint,
        url_encode(client_id),
        url_encode(&redirect_uri),
        url_encode(scope),
        url_encode(&state),
        url_encode(&code_challenge),
    );

    if !no_browser {
        let _ = open_login_url(&auth_url);
    }
    eprintln!("Bucephalus Cloud login");
    eprintln!("Open: {auth_url}");
    eprintln!("Waiting for browser authorization...");

    let code = wait_for_authorization_code(&listener, &state)?;
    exchange_authorization_code(
        token_endpoint,
        client_id,
        &redirect_uri,
        &code_verifier,
        &code,
    )
}

fn wait_for_authorization_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    let deadline = SystemTime::now() + LOGIN_CALLBACK_TIMEOUT;
    while SystemTime::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let mut buffer = [0u8; 8192];
                let size = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..size]);
                let Some(first_line) = request.lines().next() else {
                    write_oauth_callback_response(&mut stream, 400, "Missing OAuth callback request");
                    return Err(anyhow!("OAuth callback request was empty"));
                };
                let mut parts = first_line.split_whitespace();
                let method = parts.next().unwrap_or("");
                let target = parts.next().unwrap_or("");
                if method != "GET" {
                    write_oauth_callback_response(&mut stream, 405, "Unsupported OAuth callback method");
                    return Err(anyhow!("OAuth callback used unsupported method {method}"));
                }
                let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}"))
                    .context("OAuth callback URL was invalid")?;
                if url.path() != "/callback" {
                    write_oauth_callback_response(&mut stream, 404, "Unknown OAuth callback path");
                    continue;
                }
                let mut code = None;
                let mut state = None;
                let mut error = None;
                let mut error_description = None;
                for (key, value) in url.query_pairs() {
                    match key.as_ref() {
                        "code" => code = Some(value.into_owned()),
                        "state" => state = Some(value.into_owned()),
                        "error" => error = Some(value.into_owned()),
                        "error_description" => error_description = Some(value.into_owned()),
                        _ => {}
                    }
                }
                if let Some(error) = error {
                    write_oauth_callback_response(&mut stream, 400, "Bucephalus Cloud login was denied.");
                    return Err(anyhow!(
                        "OAuth authorization failed: {}",
                        error_description.unwrap_or(error)
                    ));
                }
                if state.as_deref() != Some(expected_state) {
                    write_oauth_callback_response(&mut stream, 400, "Bucephalus Cloud login state did not match.");
                    return Err(anyhow!("OAuth callback state did not match"));
                }
                let code = code.ok_or_else(|| anyhow!("OAuth callback was missing code"))?;
                write_oauth_callback_response(&mut stream, 200, "Bucephalus Cloud login complete. You can return to the terminal.");
                return Ok(code);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => return Err(err).context("failed while waiting for OAuth callback"),
        }
    }
    Err(anyhow!("OAuth browser login timed out waiting for authorization"))
}

fn write_oauth_callback_response(stream: &mut std::net::TcpStream, status: u16, message: &str) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let body = format!(
        "<!doctype html><title>Bucephalus Cloud Login</title><main><h1>{}</h1><p>{}</p></main>",
        reason,
        html_escape(message)
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
}

fn exchange_authorization_code(
    token_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    code_verifier: &str,
    code: &str,
) -> Result<Value> {
    let response = Client::new()
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ])
        .send()
        .with_context(|| format!("failed to exchange OAuth authorization code at {}", token_endpoint))?;
    let status = response.status().as_u16();
    let bytes = response.bytes()?.to_vec();
    if !(200..300).contains(&status) {
        bail_with_status("OAuth authorization code exchange failed", status, &bytes)?;
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn random_url_token(byte_count: usize) -> Result<String> {
    let mut bytes = vec![0u8; byte_count];
    match fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)) {
        Ok(()) => {}
        Err(_) => {
            let seed = format!(
                "{}:{}:{:?}",
                std::process::id(),
                current_unix_time_ms(),
                SystemTime::now()
            );
            let mut cursor = 0usize;
            while cursor < bytes.len() {
                let digest = sha2::Sha256::digest(format!("{seed}:{cursor}").as_bytes());
                for byte in digest {
                    if cursor >= bytes.len() {
                        break;
                    }
                    bytes[cursor] = byte;
                    cursor += 1;
                }
            }
        }
    }
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn url_encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn http_get_json(url: &str) -> Result<Value> {
    let response = Client::new()
        .get(url)
        .send()
        .with_context(|| format!("failed to send request to {}", url))?;
    let status = response.status().as_u16();
    let bytes = response.bytes()?.to_vec();
    if !(200..300).contains(&status) {
        bail_with_status(&format!("GET {} failed", url), status, &bytes)?;
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn dynamic_register_oauth_client(metadata: &Value, issuer: &str, scope: &str) -> Result<String> {
    let registration_endpoint = metadata
        .get("registration_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!(
                "OAuth client_id is required; pass --client-id, set {}, or use an issuer with dynamic client registration",
                BUCEPHALUS_CLOUD_OAUTH_CLIENT_ID_ENV
            )
        })?;
    let body = json!({
        "client_name": "Bucephalus CLI",
        "application_type": "native",
        "grant_types": ["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"],
        "token_endpoint_auth_method": "none",
        "scope": scope
    });
    let response = Client::new()
        .post(registration_endpoint)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body)?)
        .send()
        .with_context(|| format!("failed to register OAuth client with {}", issuer))?;
    let status = response.status().as_u16();
    let bytes = response.bytes()?.to_vec();
    if !(200..300).contains(&status) {
        bail_with_status("OAuth dynamic client registration failed", status, &bytes)?;
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    value
        .get("client_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("OAuth dynamic client registration response missing client_id"))
}

fn begin_device_authorization(
    endpoint: &str,
    client_id: &str,
    scope: &str,
    audience: Option<&str>,
    resource: Option<&str>,
) -> Result<Value> {
    let mut form = vec![
        ("client_id".to_string(), client_id.to_string()),
        ("scope".to_string(), scope.to_string()),
    ];
    if let Some(audience) = audience.filter(|value| !value.trim().is_empty()) {
        form.push(("audience".to_string(), audience.to_string()));
    }
    if let Some(resource) = resource.filter(|value| !value.trim().is_empty()) {
        form.push(("resource".to_string(), resource.to_string()));
    }
    let response = Client::new()
        .post(endpoint)
        .form(&form)
        .send()
        .with_context(|| format!("failed to start device authorization at {}", endpoint))?;
    let status = response.status().as_u16();
    let bytes = response.bytes()?.to_vec();
    if !(200..300).contains(&status) {
        bail_with_status("device authorization failed", status, &bytes)?;
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn poll_device_token(token_endpoint: &str, client_id: &str, device: &Value) -> Result<Value> {
    let device_code = device
        .get("device_code")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("device authorization response missing device_code"))?;
    let expires_in = device
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(900);
    let mut interval = device
        .get("interval")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .max(1);
    let deadline = SystemTime::now() + Duration::from_secs(expires_in);
    let client = Client::new();
    while SystemTime::now() < deadline {
        std::thread::sleep(Duration::from_secs(interval));
        let form = vec![
            (
                "grant_type".to_string(),
                "urn:ietf:params:oauth:grant-type:device_code".to_string(),
            ),
            ("device_code".to_string(), device_code.to_string()),
            ("client_id".to_string(), client_id.to_string()),
        ];
        let response = client
            .post(token_endpoint)
            .form(&form)
            .send()
            .with_context(|| format!("failed to poll token endpoint {}", token_endpoint))?;
        let status = response.status().as_u16();
        let bytes = response.bytes()?.to_vec();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            json!({
                "error": "invalid_response",
                "error_description": String::from_utf8_lossy(&bytes).to_string()
            })
        });
        if (200..300).contains(&status) {
            if value.get("access_token").and_then(Value::as_str).is_some() {
                return Ok(value);
            }
            return Err(anyhow!("token endpoint response missing access_token"));
        }
        match value.get("error").and_then(Value::as_str).unwrap_or("") {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            "access_denied" => return Err(anyhow!("OAuth device login was denied")),
            "expired_token" => return Err(anyhow!("OAuth device login expired")),
            other => {
                let detail = value
                    .get("error_description")
                    .and_then(Value::as_str)
                    .unwrap_or(other);
                return Err(anyhow!(
                    "token endpoint failed with status {}: {}",
                    status,
                    detail
                ));
            }
        }
    }
    Err(anyhow!("OAuth device login expired"))
}

fn write_cloud_token_cache(
    paths: &CloudTokenPaths,
    issuer: &str,
    client_id: &str,
    audience: Option<&str>,
    resource: Option<&str>,
    scope: &str,
    token_endpoint: &str,
    token: &Value,
) -> Result<()> {
    let access_token = token
        .get("id_token")
        .and_then(Value::as_str)
        .or_else(|| token.get("access_token").and_then(Value::as_str))
        .ok_or_else(|| anyhow!("token response missing id_token or access_token"))?;
    let oauth_access_token = token
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let refresh_token = token.get("refresh_token").and_then(Value::as_str);
    let issued_at = current_unix_time_ms();
    let expires_at_ms = token
        .get("expires_in")
        .and_then(Value::as_i64)
        .map(|seconds| issued_at + seconds.saturating_mul(1000));
    let cache = json!({
        "schema_version": "bucephalus_cloud_oauth_token_v1",
        "issuer": issuer,
        "client_id": client_id,
        "audience": audience,
        "resource": resource,
        "scope": scope,
        "token_endpoint": token_endpoint,
        "token_type": token.get("token_type").and_then(Value::as_str).unwrap_or("Bearer"),
        "access_token": access_token,
        "oauth_access_token": oauth_access_token,
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

fn write_cloud_token_cache_from_existing(
    paths: &CloudTokenPaths,
    existing: &Value,
    token: &Value,
) -> Result<()> {
    let access_token = token
        .get("id_token")
        .and_then(Value::as_str)
        .or_else(|| token.get("access_token").and_then(Value::as_str))
        .ok_or_else(|| anyhow!("token response missing id_token or access_token"))?;
    let oauth_access_token = token
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
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
        "oauth_access_token": oauth_access_token,
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

fn open_login_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .status()
        .with_context(|| format!("failed to open browser for {}", url))?;
    Ok(())
}

fn current_unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| {
            i64::try_from(duration.as_millis()).expect("Unix timestamp milliseconds must fit i64")
        })
        .expect("system time must be after Unix epoch")
}

fn bail_with_status(prefix: &str, status: u16, bytes: &[u8]) -> Result<()> {
    Err(anyhow!(
        "{} with status {}: {}",
        prefix,
        status,
        String::from_utf8_lossy(bytes).trim()
    ))
}
