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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "../cloud_auth_ux.rs"]
mod cloud_auth_ux;
#[path = "../cloud_login.rs"]
mod cloud_login;
use cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV;

const BUCEPHALUS_CLOUD_API_URL_ENV: &str = "BUCEPHALUS_CLOUD_API_URL";
const DEFAULT_MAX_AUTHORING_CONTEXT_ARCHIVE_ENTRIES: u64 = 10_000;
const DEFAULT_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
const PROJECT_MANIFEST_YAML: &str = "bucephalus.project.yaml";
const PROJECT_MANIFEST_YML: &str = "bucephalus.project.yml";
const PROJECT_MANIFEST_SCHEMA_VERSION: &str = "bucephalus_project_v1";
const DEFAULT_RUNTIME_ACCESS_WAIT_SECONDS: u64 = 30;
const RUNTIME_ACCESS_WAIT_POLL_MS: u64 = 1_000;
const RUNTIME_AUDIT_EVENT_TYPES: &str = concat!(
    "runtime.resource.runner_instance.cordoned,",
    "runtime.resource.runner_instance.drained,",
    "runtime.resource.runner_instance.offline,",
    "runtime.resource.runner_instance.unhealthy,",
    "runtime.resource.runner_instance.online,",
    "runtime.resource.runner_instance.heartbeat_restored,",
    "runtime.resource.runner_instance.uncordoned,",
    "worker.runtime.image_pull.pulling,",
    "worker.runtime.image_pull.pulled,",
    "worker.runtime.image_pull.failed,",
    "worker.runtime.secret_binding.materialized,",
    "worker.runtime.sidecar_requirement.checking,",
    "worker.runtime.sidecar_requirement.available,",
    "worker.runtime.sidecar_requirement.failed,",
    "worker.runtime.accelerator_requirement.checking,",
    "worker.runtime.accelerator_requirement.available,",
    "worker.runtime.accelerator_requirement.failed,",
    "worker.runtime.network_perimeter.applying,",
    "worker.runtime.network_perimeter.applied,",
    "worker.runtime.network_perimeter.failed,",
    "runtime.resource.operation.reviewed,",
    "runtime.resource.operation.review.failed,",
    "runtime.api_resources.read,",
    "runtime.api_resources.read.failed,",
    "runtime.resource.list.read,",
    "runtime.resource.list.read.failed,",
    "runtime.resource.watch.read,",
    "runtime.resource.watch.read.failed,",
    "runtime.resource.health.read,",
    "runtime.resource.health.read.failed,",
    "runtime.resource.describe.read,",
    "runtime.resource.describe.read.failed,",
    "runtime.resource.get.read,",
    "runtime.resource.get.read.failed,",
    "runtime.resource.events.read,",
    "runtime.resource.events.read.failed,",
    "runtime.resource.status.read,",
    "runtime.resource.status.read.failed,",
    "runtime.resource.metrics.read,",
    "runtime.resource.metrics.read.failed,",
    "runtime.resource.metrics.list.read,",
    "runtime.resource.metrics.list.read.failed,",
    "runtime.inspect.bundle.read,",
    "runtime.inspect.bundle.read.failed,",
    "runtime.access.port_forward.requested,",
    "runtime.access.port_forward.accepted,",
    "runtime.access.port_forward.active,",
    "runtime.access.port_forward.completed,",
    "runtime.access.port_forward.failed,",
    "runtime.access.port_forward.expired,",
    "runtime.access.port_forward.cancelled,",
    "runtime.access.exec.requested,",
    "runtime.access.exec.accepted,",
    "runtime.access.exec.active,",
    "runtime.access.exec.completed,",
    "runtime.access.exec.failed,",
    "runtime.access.exec.expired,",
    "runtime.access.exec.cancelled,",
    "runtime.resource.logs.read,",
    "runtime.resource.logs.read.failed,",
    "runtime.resource.content.read,",
    "runtime.resource.content.read.failed"
);

#[derive(Clone, Debug)]
struct CliContext {
    api_url: String,
    user_token: Option<String>,
    args: Vec<String>,
    client: Client,
}

#[derive(Debug)]
struct RawCloudResponse {
    bytes: Vec<u8>,
    headers: HeaderMap,
}

#[derive(Debug)]
struct JsonCloudResponse {
    status: u16,
    payload: Value,
}

#[derive(Clone, Debug)]
struct RuntimePrinterColumn {
    name: String,
    json_path: String,
    priority: i64,
}

#[derive(Clone, Copy, Debug)]
enum RuntimeAccessWaitKind {
    PortForward,
    Exec,
}

#[derive(Clone, Debug)]
enum RuntimeWaitPredicate {
    Condition { kind: String, status: String },
    Phase(String),
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunGetTarget {
    RunRecord,
    ResourceList,
    ResourceItem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeResourceListOutput {
    Summary,
    Name,
    Wide,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimePortForwardAttachSpec {
    project_id: String,
    zone: String,
    instance_name: String,
    target_host: String,
    target_port: u16,
    local_port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimePortForwardClientEndpointAttachSpec {
    endpoint: String,
    local_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RuntimePortForwardAttachPlan {
    GceIap(RuntimePortForwardAttachSpec),
    ClientEndpoint(RuntimePortForwardClientEndpointAttachSpec),
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
    project_manifest: Value,
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
struct ProjectBuildContext {
    project_root: PathBuf,
    manifest_rel: String,
    manifest_digest: String,
    project_id: String,
    package_source: String,
    source_root: String,
    entrypoint: String,
    include: Vec<String>,
    exclude: Vec<String>,
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
        (Some("login"), _) => login(alias_args(&context, 1)),
        (Some("logout"), _) => logout(alias_args(&context, 1)),
        (Some("auth"), Some("status")) => auth_status(with_args(&context, rest)),
        (Some("health"), None) => health(with_args(&context, rest)),
        (Some("build"), _) => experiment_build(alias_args(&context, 1)),
        (Some("doctor"), _) => experiment_doctor(alias_args(&context, 1)),
        (Some("run"), _) => run_create(alias_args(&context, 1)),
        (Some("cancel"), _) => run_cancel(alias_args(&context, 1)),
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
        (Some("runs"), Some("api-resources")) => run_api_resources(with_args(&context, rest)),
        (Some("runs"), Some("explain")) => run_explain(with_args(&context, rest)),
        (Some("runs"), Some("inspect")) => run_inspect(with_args(&context, rest)),
        (Some("runs"), Some("resources")) => run_resources(with_args(&context, rest)),
        (Some("runs"), Some("tree")) => run_tree(with_args(&context, rest)),
        (Some("runs"), Some("describe")) => run_describe(with_args(&context, rest)),
        (Some("runs"), Some("status")) => run_status(with_args(&context, rest)),
        (Some("runs"), Some("wait")) => run_wait(with_args(&context, rest)),
        (Some("runs"), Some("can-i")) => run_can_i(with_args(&context, rest)),
        (Some("runs"), Some("health")) => run_health(with_args(&context, rest)),
        (Some("runs"), Some("metrics")) => run_metrics(with_args(&context, rest)),
        (Some("runs"), Some("top")) => run_top(with_args(&context, rest)),
        (Some("runs"), Some("watch")) => run_watch(with_args(&context, rest)),
        (Some("runs"), Some("events")) => run_events(with_args(&context, rest)),
        (Some("runs"), Some("audit")) => run_audit(with_args(&context, rest)),
        (Some("runs"), Some("logs")) => run_logs(with_args(&context, rest)),
        (Some("runs"), Some("content")) => run_content(with_args(&context, rest)),
        (Some("runs"), Some("port-forward")) => run_port_forward(with_args(&context, rest)),
        (Some("runs"), Some("exec")) => run_exec(with_args(&context, rest)),
        (Some("runs"), Some("action")) => run_resource_action(with_args(&context, rest), None),
        (Some("runs"), Some("cordon")) => {
            run_resource_action(with_args(&context, rest), Some("cordon"))
        }
        (Some("runs"), Some("drain")) => {
            run_resource_action(with_args(&context, rest), Some("drain"))
        }
        (Some("runs"), Some("uncordon")) => {
            run_resource_action(with_args(&context, rest), Some("uncordon"))
        }
        (Some("runs"), Some("cancel")) => {
            run_resource_action(with_args(&context, rest), Some("cancel"))
        }
        (Some("runs"), Some("complete")) => {
            run_resource_action(with_args(&context, rest), Some("complete"))
        }
        (Some("runs"), Some("delete")) => run_resource_delete(with_args(&context, rest)),
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
            | (Some("login" | "logout"), _)
            | (Some("auth"), Some("status"))
            | (Some("build" | "doctor" | "run" | "cancel" | "inspect"), _)
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
                Some(
                    "list"
                        | "create"
                        | "get"
                        | "api-resources"
                        | "explain"
                        | "inspect"
                        | "resources"
                        | "tree"
                        | "describe"
                        | "status"
                        | "wait"
                        | "can-i"
                        | "health"
                        | "metrics"
                        | "top"
                        | "watch"
                        | "events"
                        | "audit"
                        | "logs"
                        | "content"
                        | "port-forward"
                        | "exec"
                        | "action"
                        | "cordon"
                        | "drain"
                        | "uncordon"
                        | "cancel"
                        | "complete"
                        | "delete"
                )
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
                    | "cancel"
                    | "inspect"
                    | "auth"
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
    let mut api_url = env_non_empty(BUCEPHALUS_CLOUD_API_URL_ENV).unwrap_or_default();
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
        user_token = cloud_login::shared_cloud_user_token()?;
    }
    if api_url.trim().is_empty() {
        if let Ok(home) = lab_core::bucephalus_home() {
            if let Some(url) = lab_core::cloud_profile_string(&home, "/api_url") {
                api_url = url;
            }
        }
    }
    if api_url.trim().is_empty() {
        api_url = cloud_login::default_bucephalus_cloud_api_url().to_string();
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
            "buc needs a hosted API URL. This build defaults to {}. Override it with --api-url or {} for dev/staging/self-hosted Cloud.",
            cloud_login::default_bucephalus_cloud_api_url(),
            BUCEPHALUS_CLOUD_API_URL_ENV
        );
    }
    Ok(())
}

fn health(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &[], &[])?;
    reject_no_positionals(&context.args, "buc health")?;
    ensure_api_configured(&context)?;
    print_json(&cloud_fetch(&context, Method::GET, "/readyz", None, None)?)
}

fn login(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--issuer",
            "--client-id",
            "--audience",
            "--api-url",
            "--resource",
            "--scope",
        ],
        &["--no-browser", "--json"],
    )?;
    reject_no_positionals(&context.args, "buc login")?;
    let api_url = match option_value(&context.args, "--api-url")? {
        Some(value) => Some(value),
        None => option_value(&context.args, "--resource")?,
    }
    .or_else(|| non_empty(context.api_url.clone()));
    let result = cloud_login::run_login(cloud_login::DeviceLoginOptions {
        issuer: option_value(&context.args, "--issuer")?,
        client_id: option_value(&context.args, "--client-id")?,
        audience: option_value(&context.args, "--audience")?,
        api_url,
        scope: option_value(&context.args, "--scope")?,
        no_browser: flag_present(&context.args, "--no-browser"),
    })?;
    if json_requested(&context.args) {
        print_json(&result)
    } else {
        println!("login: ready");
        println!(
            "api_url: {}",
            result["api_url"].as_str().unwrap_or("unknown")
        );
        println!(
            "token_path: {}",
            result["token_path"].as_str().unwrap_or("unknown")
        );
        if let Some(path) = result["refresh_token_path"].as_str() {
            println!("refresh_token_path: {path}");
        }
        println!("next: buc health");
        Ok(())
    }
}

fn logout(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &[], &["--dry-run", "--json"])?;
    reject_no_positionals(&context.args, "buc logout")?;
    let result = cloud_login::run_logout(flag_present(&context.args, "--dry-run"))?;
    if json_requested(&context.args) {
        print_json(&result)
    } else {
        println!("logout: {}", result["status"].as_str().unwrap_or("unknown"));
        if let Some(files) = result["files"].as_array() {
            for file in files {
                if let (Some(kind), Some(status), Some(path)) = (
                    file["kind"].as_str(),
                    file["status"].as_str(),
                    file["path"].as_str(),
                ) {
                    println!("auth_file: {kind} {status} {path}");
                }
            }
        }
        if let Some(note) = result["env"]["note"].as_str() {
            println!("note: {note}");
        }
        Ok(())
    }
}

fn auth_status(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &[], &["--json"])?;
    reject_no_positionals(&context.args, "buc auth status")?;
    let result = cloud_login::auth_status()?;
    if json_requested(&context.args) {
        print_json(&result)
    } else {
        let auth = &result["auth"];
        println!("auth: {}", auth["status"].as_str().unwrap_or("unknown"));
        if let Some(source) = auth["source"].as_str() {
            println!("source: {source}");
        }
        if let Some(api_url) = auth["api_url"].as_str() {
            println!("api_url: {api_url}");
        }
        if auth["status"].as_str() == Some("missing") {
            println!("auth_next: buc login");
        }
        Ok(())
    }
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
    let limit = bounded_number_option_string(&context.args, "--limit", 200)?;
    ensure_api_configured(&context)?;
    let packages = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query("/v1/packages", &[("limit", limit)]),
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
    let label = option_value(&context.args, "--label")?;
    let json_output = json_requested(&context.args);
    let expected_runtime_options = Value::Object(runtime_options_from_args(&context.args)?);
    let path = Path::new(&path);
    let (
        build,
        source_label,
        expected_build_kind,
        expected_source_kind,
        expected_entrypoint,
        expected_project_manifest,
    ) = if is_authoring_yaml_path(path) {
        let prepared = prepare_authoring_context_input(path)?;
        let entrypoint = prepared.entrypoint.clone();
        let project_manifest = prepared.project_manifest.clone();
        ensure_api_configured(&context)?;
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
                    "project_manifest": prepared.project_manifest,
                })),
            )?,
            prepared.source_label.clone(),
            "hosted_authoring_build",
            "authoring_context",
            Some(entrypoint),
            Some(project_manifest),
        )
    } else {
        let prepared = prepare_sealed_package_input(path)?;
        ensure_api_configured(&context)?;
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
    ensure_build_source_project_manifest_matches(
        &build.response,
        expected_project_manifest.as_ref(),
    )?;
    ensure_build_runtime_options_match(&build.response, &expected_runtime_options)?;
    ensure_cloud_readiness_runtime_options_match(&build.response, &expected_runtime_options)?;
    ensure_build_target_matches(&build.response)?;
    ensure_cloud_readiness_target_matches(&build.response)?;
    ensure_build_package_contract_matches(&build.response, expected_source_kind)?;
    ensure_build_execution_environment_matches(&build.response, expected_source_kind)?;
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

fn run_cancel(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--run-id", "--reason"], &["--json"])?;
    let run_id = run_id_arg(&context.args)?;
    let reason = option_value(&context.args, "--reason")?;
    ensure_api_configured(&context)?;
    let body = match reason {
        Some(reason) => json!({ "reason": reason }),
        None => json!({}),
    };
    let run = cloud_fetch(
        &context,
        Method::POST,
        &format!("/v1/runs/{}/cancel", encode_path_segment(&run_id)),
        Some(body),
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&run)
    } else {
        print_run_summary(&run)
    }
}

fn run_list(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--limit", "--output"], &["--json"])?;
    reject_no_positionals(&context.args, "buc runs list")?;
    let limit = bounded_number_option_string(&context.args, "--limit", 200)?;
    let output = option_value_alias(&context.args, "--output", "-o")?;
    if let Some(ref value) = output {
        if value != "id" {
            bail!("--output supports only id for runs list; use --json for other formats");
        }
        if json_requested(&context.args) {
            bail!("--output id cannot be combined with --json");
        }
    }
    ensure_api_configured(&context)?;
    let runs = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query("/v1/runs", &[("limit", limit)]),
        None,
        None,
    )?;
    if json_requested(&context.args) {
        print_json(&runs)
    } else if output.as_deref() == Some("id") {
        print_run_id_list(&runs)
    } else {
        print_run_list_summary(&runs)
    }
}

fn run_get(context: CliContext) -> Result<()> {
    match run_get_target(&context.args)? {
        RunGetTarget::ResourceList => {
            return run_resources(with_args(
                &context,
                canonical_run_get_resource_list_args(&context.args)?,
            ))
        }
        RunGetTarget::ResourceItem => {
            return run_describe(with_args(
                &context,
                canonical_run_get_resource_item_args(&context.args)?,
            ))
        }
        RunGetTarget::RunRecord => {}
    }
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

fn run_get_target(args: &[String]) -> Result<RunGetTarget> {
    reject_unknown_options(
        args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--category",
            "--label-selector",
            "--field-selector",
            "--limit",
            "--continue",
            "--view",
            "--event-limit",
            "--output",
            "-o",
        ],
        &["--wide", "--json"],
    )?;
    let run_id_option = option_value(args, "--run-id")?;
    let resource_option = option_value(args, "--resource")?;
    let kind_option = option_value(args, "--kind")?;
    let name_option = option_value(args, "--name")?;
    if resource_option.is_some() && (kind_option.is_some() || name_option.is_some()) {
        bail!("use either --resource Kind/name or --kind KIND --name NAME");
    }
    if name_option.is_some() && kind_option.is_none() {
        bail!("--kind and --name must be provided together");
    }
    let item_option = resource_option.is_some()
        || name_option.is_some()
        || option_value(args, "--view")?.is_some()
        || option_value(args, "--event-limit")?.is_some();
    let list_option = kind_option.is_some()
        || !runtime_category_filters(args)?.is_empty()
        || option_value(args, "--label-selector")?.is_some()
        || option_value(args, "--field-selector")?.is_some()
        || option_value(args, "--limit")?.is_some()
        || option_value(args, "--continue")?.is_some()
        || runtime_resource_output_option(args)?.is_some()
        || flag_present(args, "--wide");
    let positionals = positional_args(args);
    match (run_id_option.is_some(), positionals.as_slice()) {
        (true, []) => {
            if item_option {
                Ok(RunGetTarget::ResourceItem)
            } else if list_option {
                Ok(RunGetTarget::ResourceList)
            } else {
                Ok(RunGetTarget::RunRecord)
            }
        }
        (true, [resource_or_kind]) => {
            if item_option || resource_or_kind.contains('/') {
                Ok(RunGetTarget::ResourceItem)
            } else {
                Ok(RunGetTarget::ResourceList)
            }
        }
        (true, [_, _]) => Ok(RunGetTarget::ResourceItem),
        (true, _) => Ok(RunGetTarget::ResourceItem),
        (false, []) => {
            if item_option {
                Ok(RunGetTarget::ResourceItem)
            } else if list_option {
                Ok(RunGetTarget::ResourceList)
            } else {
                Ok(RunGetTarget::RunRecord)
            }
        }
        (false, [_run_id]) => {
            if item_option {
                Ok(RunGetTarget::ResourceItem)
            } else if list_option {
                Ok(RunGetTarget::ResourceList)
            } else {
                Ok(RunGetTarget::RunRecord)
            }
        }
        (false, [_run_id, resource_or_kind]) => {
            if item_option || resource_or_kind.contains('/') {
                Ok(RunGetTarget::ResourceItem)
            } else {
                Ok(RunGetTarget::ResourceList)
            }
        }
        (false, [_, _, _]) => Ok(RunGetTarget::ResourceItem),
        (false, _) => Ok(RunGetTarget::ResourceItem),
    }
}

fn canonical_run_get_resource_list_args(args: &[String]) -> Result<Vec<String>> {
    let run_id_option = option_value(args, "--run-id")?;
    let kind_option = option_value(args, "--kind")?;
    let positionals = positional_args(args);
    let (run_id, positional_kind) = match (run_id_option, positionals.as_slice()) {
        (Some(run_id), []) => (Some(run_id), None),
        (Some(run_id), [kind]) => (Some(run_id), Some(kind.clone())),
        (None, []) => (None, None),
        (None, [run_id]) => (Some(run_id.clone()), None),
        (None, [run_id, kind]) => (Some(run_id.clone()), Some(kind.clone())),
        (Some(_), _) => bail!("buc runs get accepts --run-id <run-id> [kind] for resource lists"),
        (None, _) => bail!("buc runs get accepts <run-id> [kind] for resource lists"),
    };
    if positional_kind.is_some() && kind_option.is_some() {
        bail!(
            "runtime resource kind must be provided either positionally or with --kind, not both"
        );
    }
    if positional_kind.is_some() && !runtime_category_filters(args)?.is_empty() {
        bail!("runtime resource filters must use either a kind or --category, not both");
    }
    let mut out = Vec::new();
    if let Some(run_id) = run_id {
        out.push(run_id);
    }
    if let Some(kind) = positional_kind.or(kind_option) {
        out.push("--kind".to_string());
        out.push(kind);
    }
    for option in [
        "--label-selector",
        "--field-selector",
        "--limit",
        "--continue",
    ] {
        if let Some(value) = option_value(args, option)? {
            out.push(option.to_string());
            out.push(value);
        }
    }
    if let Some(value) = runtime_resource_output_option(args)? {
        out.push("--output".to_string());
        out.push(value);
    }
    for category in option_values(args, "--category")? {
        out.push("--category".to_string());
        out.push(category);
    }
    if flag_present(args, "--wide") {
        out.push("--wide".to_string());
    }
    if json_requested(args) {
        out.push("--json".to_string());
    }
    Ok(out)
}

fn canonical_run_get_resource_item_args(args: &[String]) -> Result<Vec<String>> {
    if !runtime_category_filters(args)?.is_empty() {
        bail!("--category only applies to runtime resource lists");
    }
    if flag_present(args, "--wide") {
        bail!("--wide only applies to runtime resource lists");
    }
    if runtime_resource_output_option(args)?.is_some() {
        bail!("--output only applies to runtime resource lists");
    }
    let mut out = args.to_vec();
    if option_value(args, "--view")?.is_none() {
        out.push("--view".to_string());
        out.push("resource".to_string());
    }
    Ok(out)
}

fn run_api_resources(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--run-id", "--kind"], &["--json"])?;
    let (run_id, kind) = run_id_and_optional_kind_args(&context.args)?;
    ensure_api_configured(&context)?;
    let path = if let Some(kind) = kind {
        format!(
            "/v1/runs/{}/runtime/api-resources/{}",
            encode_path_segment(&run_id),
            encode_path_segment(&kind)
        )
    } else {
        format!(
            "/v1/runs/{}/runtime/api-resources",
            encode_path_segment(&run_id)
        )
    };
    let response = cloud_fetch(&context, Method::GET, &path, None, None)?;
    ensure_runtime_response_matches_if_present(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_runtime_api_resources_summary(&response)
    }
}

fn fetch_runtime_api_resources(context: &CliContext, run_id: &str) -> Result<Value> {
    let response = cloud_fetch(
        context,
        Method::GET,
        &format!(
            "/v1/runs/{}/runtime/api-resources",
            encode_path_segment(run_id)
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches_if_present(&response, run_id)?;
    Ok(response)
}

fn run_explain(context: CliContext) -> Result<()> {
    reject_unknown_options(&context.args, &["--run-id", "--kind"], &["--json"])?;
    let (run_id, kind) = run_id_and_optional_kind_args(&context.args)?;
    let kind = kind.ok_or_else(|| anyhow!("runtime resource kind is required for explain"))?;
    ensure_api_configured(&context)?;
    let path = format!(
        "/v1/runs/{}/runtime/api-resources/{}",
        encode_path_segment(&run_id),
        encode_path_segment(&kind)
    );
    let response = cloud_fetch(&context, Method::GET, &path, None, None)?;
    ensure_runtime_response_matches_if_present(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_runtime_api_resource_explain(&response)
    }
}

fn run_inspect(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--kind",
            "--category",
            "--label-selector",
            "--field-selector",
            "--event-limit",
        ],
        &["--json"],
    )?;
    let run_id = run_id_arg(&context.args)?;
    let event_limit = bounded_number_option_string(&context.args, "--event-limit", 1000)?;
    let query_params =
        runtime_selector_query_params(&context.args, vec![("event_limit", event_limit)])?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query(
            &format!("/v1/runs/{}/runtime/inspect", encode_path_segment(&run_id)),
            &query_params,
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_runtime_inspect_summary(&response)
    }
}

fn runtime_resource_list_output_arg(args: &[String]) -> Result<RuntimeResourceListOutput> {
    let output = runtime_resource_output_option(args)?;
    let wide = flag_present(args, "--wide");
    let json = json_requested(args);
    if let Some(output) = output {
        let output = output.trim().to_ascii_lowercase();
        if output.is_empty() {
            bail!("--output must be name for runtime resource lists");
        }
        if output != "name" {
            bail!("--output supports only name for runtime resource lists; use --json or --wide for other formats");
        }
        if json {
            bail!("--output name cannot be combined with --json");
        }
        if wide {
            bail!("--output name cannot be combined with --wide");
        }
        return Ok(RuntimeResourceListOutput::Name);
    }
    if json && wide {
        bail!("--wide cannot be combined with --json");
    }
    if wide {
        Ok(RuntimeResourceListOutput::Wide)
    } else {
        Ok(RuntimeResourceListOutput::Summary)
    }
}

fn run_resources(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--kind",
            "--category",
            "--label-selector",
            "--field-selector",
            "--limit",
            "--continue",
            "--output",
            "-o",
        ],
        &["--wide", "--json"],
    )?;
    let output = runtime_resource_list_output_arg(&context.args)?;
    let run_id = run_id_arg(&context.args)?;
    let limit = bounded_number_option_string(&context.args, "--limit", 1000)?;
    let continue_token = option_value(&context.args, "--continue")?;
    let query_params = runtime_selector_query_params(
        &context.args,
        vec![("limit", limit), ("continue", continue_token)],
    )?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query(
            &format!(
                "/v1/runs/{}/runtime/resources",
                encode_path_segment(&run_id)
            ),
            &query_params,
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        match output {
            RuntimeResourceListOutput::Summary => print_runtime_resources_summary(&response),
            RuntimeResourceListOutput::Name => print_runtime_resources_name_summary(&response),
            RuntimeResourceListOutput::Wide => {
                let api_resources = fetch_runtime_api_resources(&context, &run_id)?;
                print_runtime_resources_wide_summary(&response, &api_resources)
            }
        }
    }
}

fn run_tree(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--kind",
            "--category",
            "--label-selector",
            "--field-selector",
            "--limit",
            "--continue",
        ],
        &["--json"],
    )?;
    let run_id = run_id_arg(&context.args)?;
    let limit = bounded_number_option_string(&context.args, "--limit", 1000)?
        .or_else(|| Some("1000".to_string()));
    let continue_token = option_value(&context.args, "--continue")?;
    let query_params = runtime_selector_query_params(
        &context.args,
        vec![("limit", limit), ("continue", continue_token)],
    )?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query(
            &format!(
                "/v1/runs/{}/runtime/resources",
                encode_path_segment(&run_id)
            ),
            &query_params,
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_runtime_resource_tree_summary(&response)
    }
}

fn run_describe(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--view",
            "--event-limit",
        ],
        &["--json"],
    )?;
    let (run_id, kind, name) = run_id_and_resource_args(&context.args, "runtime resource")?;
    let view = option_value(&context.args, "--view")?;
    let event_limit = bounded_number_option_string(&context.args, "--event-limit", 1000)?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query(
            &format!(
                "/v1/runs/{}/runtime/resources/{}/{}",
                encode_path_segment(&run_id),
                encode_path_segment(&kind),
                encode_path_segment(&name)
            ),
            &[("view", view), ("event_limit", event_limit)],
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches_if_present(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)?;
    } else {
        print_runtime_resource_summary(&response)?;
    }
    Ok(())
}

fn run_status(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &["--run-id", "--resource", "--kind", "--name"],
        &["--json"],
    )?;
    let (run_id, kind, name) = run_id_and_resource_args(&context.args, "runtime resource")?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::GET,
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}/status",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name)
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_runtime_resource_status_summary(&response)
    }
}

fn run_wait(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--for",
            "--timeout-seconds",
            "--interval-seconds",
        ],
        &["--json"],
    )?;
    let (run_id, kind, name) = run_id_and_resource_args(&context.args, "runtime resource")?;
    let predicate = runtime_wait_predicate(&context.args)?;
    let timeout_seconds =
        bounded_number_option(&context.args, "--timeout-seconds", 86_400)?.unwrap_or(300);
    let interval_seconds =
        bounded_number_option(&context.args, "--interval-seconds", 3_600)?.unwrap_or(1);
    ensure_api_configured(&context)?;
    let path = format!(
        "/v1/runs/{}/runtime/resources/{}/{}/status",
        encode_path_segment(&run_id),
        encode_path_segment(&kind),
        encode_path_segment(&name)
    );
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    loop {
        let response = cloud_fetch_json_response(&context, Method::GET, &path, None, None)?;
        if matches!(predicate, RuntimeWaitPredicate::Delete) && response.status == 404 {
            if json_requested(&context.args) {
                return print_json(&json!({
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceWait",
                    "cloud_run_id": &run_id,
                    "resource_ref": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": &kind,
                        "name": &name,
                    },
                    "predicate": "delete",
                    "deleted": true,
                }));
            }
            println!("wait: {kind}/{name} deleted");
            return Ok(());
        }
        if response.status < 200 || response.status >= 300 {
            bail!(
                "{}",
                cloud_fetch_error_message(&context, response.status, &response.payload)
            );
        }
        let latest = response.payload;
        ensure_runtime_response_matches(&latest, &run_id)?;
        if runtime_wait_predicate_matches(&latest, &predicate) {
            if json_requested(&context.args) {
                return print_json(&latest);
            }
            println!(
                "wait: {}/{} reached {}",
                kind,
                name,
                runtime_wait_predicate_label(&predicate)
            );
            return print_runtime_resource_status_summary(&latest);
        }
        if runtime_wait_terminal_failure(&latest, &predicate) {
            bail!(
                "runtime resource {}/{} reached terminal phase={} before {}; latest {}",
                kind,
                name,
                runtime_status_phase(&latest).unwrap_or_else(|| "unknown".to_string()),
                runtime_wait_predicate_label(&predicate),
                runtime_wait_latest_status_summary(&latest, &predicate)
            );
        }
        let now = Instant::now();
        if now >= deadline {
            bail!(
                "timed out waiting for {}/{} after {}s; latest {}",
                kind,
                name,
                timeout_seconds,
                runtime_wait_latest_status_summary(&latest, &predicate)
            );
        }
        std::thread::sleep((deadline - now).min(Duration::from_secs(interval_seconds.max(1))));
    }
}

fn run_can_i(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &["--run-id", "--resource", "--kind", "--name", "--operation"],
        &["--json"],
    )?;
    let (run_id, operation, kind, name) = run_id_operation_and_resource_args(&context.args)?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::GET,
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}/operations/{}",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name),
            encode_path_segment(&operation)
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)?;
    } else {
        print_runtime_operation_review_summary(&response, &operation, &kind, &name)?;
    }
    ensure_runtime_operation_review_supported(&response, &operation, &kind, &name)
}

fn ensure_runtime_operation_review_supported(
    value: &Value,
    fallback_operation: &str,
    fallback_kind: &str,
    fallback_name: &str,
) -> Result<()> {
    if value
        .get("supported")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(());
    }
    let operation = value
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or(fallback_operation);
    let resource = value
        .get("resource_ref")
        .map(runtime_resource_ref_line)
        .unwrap_or_else(|| format!("{fallback_kind}/{fallback_name}"));
    let detail = value
        .get("message")
        .or_else(|| value.get("reason"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("runtime operation is not supported right now");
    bail!("runtime operation {operation} is not supported for {resource}: {detail}");
}

fn run_health(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--kind",
            "--category",
            "--label-selector",
            "--field-selector",
        ],
        &["--json"],
    )?;
    let run_id = run_id_arg(&context.args)?;
    let query_params = runtime_selector_query_params(&context.args, vec![])?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::GET,
        &path_with_query(
            &format!(
                "/v1/runs/{}/runtime/resources/health",
                encode_path_segment(&run_id)
            ),
            &query_params,
        ),
        None,
        None,
    )?;
    ensure_runtime_response_matches(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_runtime_health_summary(&response)
    }
}

fn run_metrics(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--category",
            "--label-selector",
            "--field-selector",
            "--limit",
            "--continue",
        ],
        &["--json"],
    )?;
    let run_id = run_id_arg_allowing_optional_resource(&context.args)?;
    let limit = bounded_number_option_string(&context.args, "--limit", 1000)?;
    let continue_token = option_value(&context.args, "--continue")?;
    let resource_target = optional_resource_args(&context.args)?;
    if resource_target.is_some() && !runtime_category_filters(&context.args)?.is_empty() {
        bail!("--category only applies to runtime metrics collection lists");
    }
    let query_params = if resource_target.is_none() {
        Some(runtime_selector_query_params(
            &context.args,
            vec![("limit", limit), ("continue", continue_token.clone())],
        )?)
    } else {
        None
    };
    ensure_api_configured(&context)?;
    let path = if let Some((kind, name)) = resource_target {
        format!(
            "/v1/runs/{}/runtime/resources/{}/{}/metrics",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name)
        )
    } else {
        path_with_query(
            &format!(
                "/v1/runs/{}/runtime/resources/metrics",
                encode_path_segment(&run_id)
            ),
            query_params
                .as_ref()
                .expect("runtime metrics collection query params should be built"),
        )
    };
    let response = cloud_fetch(&context, Method::GET, &path, None, None)?;
    ensure_runtime_response_matches(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_runtime_metrics_summary(&response)
    }
}

fn run_top(context: CliContext) -> Result<()> {
    run_metrics(context)
}

fn run_watch(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--kind",
            "--category",
            "--label-selector",
            "--field-selector",
            "--resource-version",
            "--known-resource",
            "--interval-seconds",
            "--max-polls",
        ],
        &["--json", "--follow"],
    )?;
    let run_id = run_id_arg(&context.args)?;
    let follow = flag_present(&context.args, "--follow");
    if !follow
        && (option_value(&context.args, "--interval-seconds")?.is_some()
            || option_value(&context.args, "--max-polls")?.is_some())
    {
        bail!("--interval-seconds and --max-polls require --follow");
    }
    if follow && json_requested(&context.args) {
        bail!("--follow cannot be combined with --json; use --resource-version and --known-resource for machine polling");
    }
    let interval_seconds =
        bounded_non_negative_number_option(&context.args, "--interval-seconds", 3_600)?
            .unwrap_or(2);
    let max_polls = bounded_number_option(&context.args, "--max-polls", 1_000_000)?;
    let mut resource_version = option_value(&context.args, "--resource-version")?;
    let mut known_resources = option_values(&context.args, "--known-resource")?;
    let _ = runtime_selector_query_params(&context.args, vec![])?;
    ensure_api_configured(&context)?;
    let mut polls = 0_u64;
    loop {
        polls += 1;
        let mut query_params =
            runtime_watch_query_params(&context.args, resource_version.clone(), &known_resources)?;
        if follow {
            query_params.push(("allow_bookmarks", Some("true".to_string())));
        }
        let response = cloud_fetch(
            &context,
            Method::GET,
            &path_with_query(
                &format!(
                    "/v1/runs/{}/runtime/resources/watch",
                    encode_path_segment(&run_id)
                ),
                &query_params,
            ),
            None,
            None,
        )?;
        ensure_runtime_response_matches(&response, &run_id)?;
        if json_requested(&context.args) {
            return print_json(&response);
        }
        print_runtime_watch_summary(&response)?;
        if !follow || max_polls.is_some_and(|max| polls >= max) {
            return Ok(());
        }
        let (next_resource_version, next_known_resources) = runtime_watch_follow_cursors(&response);
        if next_resource_version.is_none() && next_known_resources.is_empty() {
            bail!("runtime resource watch response did not include resource_inventory.metadata.resourceVersion or resource_versions for --follow");
        }
        resource_version = next_resource_version;
        known_resources = next_known_resources;
        if interval_seconds > 0 {
            std::thread::sleep(Duration::from_secs(interval_seconds));
        }
    }
}

fn run_events(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--limit",
            "--after-row-seq",
            "--continue",
            "--event-type",
            "--source",
            "--resource-kind",
            "--resource-name",
            "--trial-id",
            "--task-id",
            "--interval-seconds",
            "--max-polls",
        ],
        &["--json", "--follow"],
    )?;
    let run_id = run_id_arg_allowing_optional_resource(&context.args)?;
    let follow = context.args.iter().any(|arg| arg == "--follow");
    if !follow
        && (option_value(&context.args, "--interval-seconds")?.is_some()
            || option_value(&context.args, "--max-polls")?.is_some())
    {
        bail!("--interval-seconds and --max-polls require --follow");
    }
    if follow && json_requested(&context.args) {
        bail!("--follow cannot be combined with --json; use --continue for machine pagination");
    }
    let interval_seconds =
        bounded_non_negative_number_option(&context.args, "--interval-seconds", 3_600)?
            .unwrap_or(2);
    let max_polls = bounded_number_option(&context.args, "--max-polls", 1_000_000)?;
    ensure_api_configured(&context)?;
    let path = if let Some((kind, name)) = optional_resource_args(&context.args)? {
        format!(
            "/v1/runs/{}/runtime/resources/{}/{}/events",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name)
        )
    } else {
        format!("/v1/runs/{}/runtime/events", encode_path_segment(&run_id))
    };
    if !follow {
        let params = runtime_event_query_params(&context.args, None)?;
        let events = cloud_fetch(
            &context,
            Method::GET,
            &path_with_query(&path, &params),
            None,
            None,
        )?;
        ensure_runtime_response_matches(&events, &run_id)?;
        return if json_requested(&context.args) {
            print_json(&events)
        } else {
            print_runtime_events_summary(&events)
        };
    }

    let mut cursor: Option<String> = None;
    let mut polls = 0_u64;
    loop {
        polls += 1;
        let params = runtime_event_query_params(&context.args, cursor.as_deref())?;
        let events = cloud_fetch(
            &context,
            Method::GET,
            &path_with_query(&path, &params),
            None,
            None,
        )?;
        ensure_runtime_response_matches(&events, &run_id)?;
        print_runtime_events_summary(&events)?;
        if max_polls.is_some_and(|max| polls >= max) {
            return Ok(());
        }
        cursor = runtime_events_follow_cursor(&events);
        if cursor.is_none() {
            bail!("runtime event response did not include metadata.continue or metadata.resourceVersion for --follow");
        }
        if interval_seconds > 0 {
            std::thread::sleep(Duration::from_secs(interval_seconds));
        }
    }
}

fn run_audit(mut context: CliContext) -> Result<()> {
    if option_values(&context.args, "--event-type")?.is_empty() {
        context.args.push("--event-type".to_string());
        context.args.push(RUNTIME_AUDIT_EVENT_TYPES.to_string());
    }
    run_events(context)
}

fn runtime_event_query_params(
    args: &[String],
    follow_cursor: Option<&str>,
) -> Result<Vec<(&'static str, Option<String>)>> {
    let limit = bounded_number_option_string(args, "--limit", 1000)?;
    let after_row_seq = if follow_cursor.is_some() {
        None
    } else {
        non_negative_number_option_string(args, "--after-row-seq")?
    };
    let continue_token = match follow_cursor {
        Some(cursor) => Some(cursor.to_string()),
        None => option_value(args, "--continue")?,
    };
    let mut params = vec![
        ("limit", limit),
        ("after_row_seq", after_row_seq),
        ("continue", continue_token),
    ];
    for event_type in option_values(args, "--event-type")? {
        params.push(("event_type", Some(event_type)));
    }
    for source in option_values(args, "--source")? {
        params.push(("source", Some(source)));
    }
    params.push(("resource_kind", option_value(args, "--resource-kind")?));
    params.push(("resource_name", option_value(args, "--resource-name")?));
    params.push(("trial_id", option_value(args, "--trial-id")?));
    params.push(("task_id", option_value(args, "--task-id")?));
    Ok(params)
}

fn run_logs(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--stream",
            "--tail-lines",
            "--out",
            "--metadata-out",
            "--interval-seconds",
            "--max-polls",
        ],
        &["--follow"],
    )?;
    let (run_id, kind, name) = run_id_and_resource_args(&context.args, "runtime resource")?;
    let stream = runtime_log_stream_arg(&context.args)?;
    let tail_lines = bounded_number_option_string(&context.args, "--tail-lines", 1_000_000)?;
    let out = option_value(&context.args, "--out")?;
    let metadata_out = option_value(&context.args, "--metadata-out")?;
    let follow = flag_present(&context.args, "--follow");
    if !follow
        && (option_value(&context.args, "--interval-seconds")?.is_some()
            || option_value(&context.args, "--max-polls")?.is_some())
    {
        bail!("--interval-seconds and --max-polls require --follow");
    }
    let interval_seconds =
        bounded_non_negative_number_option(&context.args, "--interval-seconds", 3_600)?
            .unwrap_or(2);
    let max_polls = bounded_number_option(&context.args, "--max-polls", 1_000_000)?;
    ensure_api_configured(&context)?;
    let path = path_with_query(
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}/logs",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name)
        ),
        &[("stream", stream), ("tail_lines", tail_lines)],
    );
    if !follow {
        let response = cloud_fetch_raw(&context, Method::GET, &path, None, None)?;
        return write_raw_output(&response, out.as_deref(), metadata_out.as_deref());
    }

    let mut polls = 0_u64;
    let mut previous = Vec::new();
    loop {
        polls += 1;
        let response = cloud_fetch_raw(&context, Method::GET, &path, None, None)?;
        let bytes = appended_raw_log_bytes(&previous, &response.bytes);
        write_raw_bytes(bytes, out.as_deref(), polls > 1)?;
        write_runtime_raw_metadata(&response, metadata_out.as_deref())?;
        previous = response.bytes;
        if max_polls.is_some_and(|max| polls >= max) {
            return Ok(());
        }
        if interval_seconds > 0 {
            std::thread::sleep(Duration::from_secs(interval_seconds));
        }
    }
}

fn run_content(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--out",
            "--metadata-out",
        ],
        &[],
    )?;
    let (run_id, kind, name) = run_id_and_resource_args(&context.args, "runtime resource")?;
    let out = option_value(&context.args, "--out")?;
    let metadata_out = option_value(&context.args, "--metadata-out")?;
    ensure_api_configured(&context)?;
    let response = cloud_fetch_raw(
        &context,
        Method::GET,
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}/content",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name)
        ),
        None,
        None,
    )?;
    write_raw_output(&response, out.as_deref(), metadata_out.as_deref())
}

fn run_port_forward(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--target-port",
            "--local-port",
            "--protocol",
            "--ttl-seconds",
            "--wait-seconds",
            "--reason",
            "--resource-version",
        ],
        &["--json", "--no-wait", "--attach"],
    )?;
    let (run_id, kind, name) = run_id_and_resource_args(&context.args, "runtime resource")?;
    let target_port = bounded_number_option(&context.args, "--target-port", 65535)?
        .ok_or_else(|| anyhow!("--target-port is required"))?;
    let local_port = bounded_number_option(&context.args, "--local-port", 65535)?;
    let ttl_seconds = bounded_number_option(&context.args, "--ttl-seconds", i32::MAX as u64)?;
    let wait_seconds = runtime_access_wait_seconds(&context.args)?;
    let attach = flag_present(&context.args, "--attach");
    if attach && wait_seconds.is_none() {
        bail!("--attach requires waiting for the PortForward to become active; remove --no-wait");
    }
    if attach && json_requested(&context.args) {
        bail!("--attach cannot be combined with --json; use --json without --attach to inspect the resource");
    }
    let protocol = option_value(&context.args, "--protocol")?;
    let reason = option_value(&context.args, "--reason")?;
    let resource_version = required_runtime_resource_version(&context.args, "port-forward")?;
    let mut body = Map::new();
    body.insert("target_port".to_string(), json!(target_port));
    insert_option_u64(&mut body, "local_port", local_port);
    insert_option_u64(&mut body, "ttl_seconds", ttl_seconds);
    insert_option_string(&mut body, "protocol", protocol);
    insert_option_string(&mut body, "reason", reason);
    body.insert("resource_version".to_string(), json!(resource_version));
    ensure_api_configured(&context)?;
    let mut response = cloud_fetch(
        &context,
        Method::POST,
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}/port-forward",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name)
        ),
        Some(Value::Object(body)),
        None,
    )?;
    ensure_runtime_response_matches_if_present(&response, &run_id)?;
    ensure_resource_envelope(&response)?;
    if let Some(wait_seconds) = wait_seconds {
        response = wait_for_runtime_access_resource(
            &context,
            &run_id,
            &response,
            RuntimeAccessWaitKind::PortForward,
            wait_seconds,
        )?;
        ensure_runtime_response_matches_if_present(&response, &run_id)?;
    }
    if json_requested(&context.args) {
        print_json(&response)?;
        ensure_runtime_port_forward_success(&response)
    } else {
        print_runtime_resource_summary(&response)?;
        ensure_runtime_port_forward_success(&response)?;
        if attach {
            let attach_plan = runtime_port_forward_attach_plan(&response, local_port, target_port)?;
            let cleanup_on_exit = runtime_port_forward_attach_plan_requires_cleanup(&attach_plan);
            let attach_result = run_runtime_port_forward_attach(&attach_plan);
            let cleanup_result = if attach_result.is_ok() && cleanup_on_exit {
                complete_attached_runtime_port_forward(&context, &run_id, &response)
            } else if attach_result.is_err() && cleanup_on_exit {
                cleanup_attached_runtime_port_forward(&context, &run_id, &response)
            } else {
                Ok(())
            };
            match (attach_result, cleanup_result) {
                (Ok(()), Ok(())) => {}
                (Ok(()), Err(error)) => {
                    return Err(error.context("port-forward attach ended but cleanup failed"));
                }
                (Err(error), Ok(())) => return Err(error),
                (Err(error), Err(cleanup_error)) => {
                    eprintln!("port-forward: cleanup after attach error failed: {cleanup_error}");
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

fn run_exec(context: CliContext) -> Result<()> {
    let (prefix_args, command) = split_command_after_double_dash(&context.args, "buc runs exec")?;
    reject_unknown_options(
        &prefix_args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--ttl-seconds",
            "--wait-seconds",
            "--reason",
            "--resource-version",
        ],
        &["--json", "--no-wait"],
    )?;
    let (run_id, kind, name) = run_id_and_resource_args(&prefix_args, "runtime resource")?;
    let ttl_seconds = bounded_number_option(&prefix_args, "--ttl-seconds", i32::MAX as u64)?;
    let wait_seconds = runtime_access_wait_seconds(&prefix_args)?;
    let reason = option_value(&prefix_args, "--reason")?;
    let resource_version = required_runtime_resource_version(&prefix_args, "exec")?;
    let mut body = Map::new();
    body.insert("command".to_string(), json!(command));
    insert_option_u64(&mut body, "ttl_seconds", ttl_seconds);
    insert_option_string(&mut body, "reason", reason);
    body.insert("resource_version".to_string(), json!(resource_version));
    ensure_api_configured(&context)?;
    let mut response = cloud_fetch(
        &context,
        Method::POST,
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}/exec",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name)
        ),
        Some(Value::Object(body)),
        None,
    )?;
    ensure_runtime_response_matches_if_present(&response, &run_id)?;
    ensure_resource_envelope(&response)?;
    if let Some(wait_seconds) = wait_seconds {
        response = wait_for_runtime_access_resource(
            &context,
            &run_id,
            &response,
            RuntimeAccessWaitKind::Exec,
            wait_seconds,
        )?;
        ensure_runtime_response_matches_if_present(&response, &run_id)?;
    }
    if json_requested(&context.args) {
        print_json(&response)?;
    } else {
        print_runtime_resource_summary(&response)?;
    }
    ensure_runtime_exec_success(&response)
}

fn run_resource_action(context: CliContext, fixed_action: Option<&str>) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--reason",
            "--resource-version",
        ],
        &["--json"],
    )?;
    let (run_id, kind, name, action) =
        run_id_resource_and_action_args(&context.args, fixed_action)?;
    let resource_version = required_runtime_resource_version(&context.args, &action)?;
    let mut body = Map::new();
    insert_option_string(
        &mut body,
        "reason",
        option_value(&context.args, "--reason")?,
    );
    body.insert("resource_version".to_string(), json!(resource_version));
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::POST,
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}/actions/{}",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name),
            encode_path_segment(&action)
        ),
        Some(Value::Object(body)),
        None,
    )?;
    ensure_runtime_response_matches_if_present(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_runtime_resource_summary(&response)
    }
}

fn run_resource_delete(context: CliContext) -> Result<()> {
    reject_unknown_options(
        &context.args,
        &[
            "--run-id",
            "--resource",
            "--kind",
            "--name",
            "--reason",
            "--resource-version",
        ],
        &["--json"],
    )?;
    let (run_id, kind, name) = run_id_and_resource_args(&context.args, "runtime resource")?;
    let resource_version = required_runtime_resource_version(&context.args, "delete")?;
    let mut body = Map::new();
    insert_option_string(
        &mut body,
        "reason",
        option_value(&context.args, "--reason")?,
    );
    body.insert("resource_version".to_string(), json!(resource_version));
    ensure_api_configured(&context)?;
    let response = cloud_fetch(
        &context,
        Method::DELETE,
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}",
            encode_path_segment(&run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name)
        ),
        Some(Value::Object(body)),
        None,
    )?;
    ensure_runtime_response_matches_if_present(&response, &run_id)?;
    if json_requested(&context.args) {
        print_json(&response)
    } else {
        print_runtime_resource_summary(&response)
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
    let validation_level = validation_level_option(&context.args)?;
    let draft = draft_from_args(&context.args)?;
    let mut body = Map::new();
    body.insert("draft".to_string(), draft);
    insert_option_string(&mut body, "validation_level", validation_level);
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
    let limit = bounded_number_option(&context.args, "--limit", 100)?;
    let target = required_option(&context.args, "--target")?;
    let draft = draft_from_args(&context.args)?;
    let mut body = Map::new();
    body.insert("draft".to_string(), draft);
    body.insert("target".to_string(), json!(target));
    insert_option_string(&mut body, "q", option_value(&context.args, "--q")?);
    if let Some(limit) = limit {
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

fn run_id_and_optional_kind_args(args: &[String]) -> Result<(String, Option<String>)> {
    let positionals = positional_args(args);
    let run_id = option_value(args, "--run-id")?;
    let kind = option_value(args, "--kind")?;
    match (positionals.as_slice(), run_id, kind) {
        ([run_id], None, kind) => Ok((run_id.clone(), kind)),
        ([run_id, kind], None, None) => Ok((run_id.clone(), Some(kind.clone()))),
        ([], Some(run_id), kind) => Ok((run_id, kind)),
        ([kind], Some(run_id), None) => Ok((run_id, Some(kind.clone()))),
        ([], None, _) => Err(anyhow!("run id is required")),
        (_, Some(_), Some(_)) => bail!(
            "runtime API resource kind must be provided either positionally or with --kind, not both"
        ),
        _ => bail!("runtime API resources accepts <run-id> [kind] or --run-id <run-id> [--kind kind]"),
    }
}

fn run_id_arg_allowing_optional_resource(args: &[String]) -> Result<String> {
    let positionals = positional_args(args);
    let run_id = option_value(args, "--run-id")?;
    match (positionals.as_slice(), run_id) {
        ([run_id], None) | ([run_id, _], None) | ([run_id, _, _], None) => Ok(run_id.clone()),
        ([], Some(run_id)) | ([_], Some(run_id)) | ([_, _], Some(run_id)) => Ok(run_id),
        ([], None) => Err(anyhow!("run id is required")),
        (_, Some(_)) => {
            bail!("run id must be provided either positionally or with --run-id, not both")
        }
        _ => bail!("runtime command accepts <run-id> and at most one resource identity"),
    }
}

fn runtime_selector_query_params(
    args: &[String],
    mut extra: Vec<(&'static str, Option<String>)>,
) -> Result<Vec<(&'static str, Option<String>)>> {
    let kind = option_value(args, "--kind")?;
    let categories = runtime_category_filters(args)?;
    if kind.is_some() && !categories.is_empty() {
        bail!("runtime resource filters must use either --kind or --category, not both");
    }
    extra.push(("kind", kind));
    for category in categories {
        extra.push(("category", Some(category)));
    }
    extra.push(("label_selector", option_value(args, "--label-selector")?));
    extra.push(("field_selector", option_value(args, "--field-selector")?));
    Ok(extra)
}

fn runtime_category_filters(args: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for value in option_values(args, "--category")? {
        for category in value.split(',') {
            let category = category.trim();
            if category.is_empty() {
                continue;
            }
            if !out.iter().any(|existing| existing == category) {
                out.push(category.to_string());
            }
        }
    }
    Ok(out)
}

fn runtime_watch_query_params(
    args: &[String],
    resource_version: Option<String>,
    known_resources: &[String],
) -> Result<Vec<(&'static str, Option<String>)>> {
    let mut query_params =
        runtime_selector_query_params(args, vec![("resource_version", resource_version)])?;
    for known_resource in known_resources {
        query_params.push(("known_resource", Some(known_resource.clone())));
    }
    Ok(query_params)
}

fn runtime_watch_follow_cursors(value: &Value) -> (Option<String>, Vec<String>) {
    let resource_version = value
        .pointer("/resource_inventory/metadata/resourceVersion")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string);
    let mut known_resources = value
        .get("resource_versions")
        .and_then(Value::as_object)
        .map(|versions| {
            versions
                .iter()
                .filter_map(|(key, value)| {
                    let version = value.as_str()?.trim();
                    (!key.trim().is_empty() && !version.is_empty())
                        .then(|| format!("{}={version}", key.trim()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    known_resources.sort();
    (resource_version, known_resources)
}

fn run_id_and_resource_args(args: &[String], noun: &str) -> Result<(String, String, String)> {
    let run_id = run_id_arg_allowing_optional_resource(args)?;
    let (kind, name) = optional_resource_args(args)?
        .ok_or_else(|| anyhow!("{noun} is required; pass Kind/name or --kind KIND --name NAME"))?;
    Ok((run_id, kind, name))
}

fn run_id_resource_and_action_args(
    args: &[String],
    fixed_action: Option<&str>,
) -> Result<(String, String, String, String)> {
    if let Some(action) = fixed_action {
        let (run_id, kind, name) = run_id_and_resource_args(args, "runtime resource")?;
        return Ok((run_id, kind, name, action.to_string()));
    }

    let positionals = positional_args(args);
    let (resource_args, action) = match option_value(args, "--run-id")?.is_some() {
        false => match positionals.as_slice() {
            [run_id, resource, action] => (vec![run_id.clone(), resource.clone()], action.clone()),
            [run_id, kind, name, action] => (
                vec![run_id.clone(), kind.clone(), name.clone()],
                action.clone(),
            ),
            _ => bail!("buc runs action requires <run-id> <Kind/name> <action>"),
        },
        true => match positionals.as_slice() {
            [resource, action] => (vec![resource.clone()], action.clone()),
            [kind, name, action] => (vec![kind.clone(), name.clone()], action.clone()),
            _ => bail!("buc runs action requires --run-id <run-id> <Kind/name> <action>"),
        },
    };
    let mut scoped_args = Vec::new();
    if let Some(run_id) = option_value(args, "--run-id")? {
        scoped_args.push("--run-id".to_string());
        scoped_args.push(run_id);
    }
    if let Some(resource) = option_value(args, "--resource")? {
        scoped_args.push("--resource".to_string());
        scoped_args.push(resource);
    }
    if let Some(kind) = option_value(args, "--kind")? {
        scoped_args.push("--kind".to_string());
        scoped_args.push(kind);
    }
    if let Some(name) = option_value(args, "--name")? {
        scoped_args.push("--name".to_string());
        scoped_args.push(name);
    }
    scoped_args.extend(resource_args);
    let (run_id, kind, name) = run_id_and_resource_args(&scoped_args, "runtime resource")?;
    match action.as_str() {
        "cordon" | "drain" | "uncordon" | "cancel" | "complete" => Ok((run_id, kind, name, action)),
        _ => bail!(
            "runtime resource action must be one of cordon, drain, uncordon, cancel, complete"
        ),
    }
}

fn run_id_operation_and_resource_args(args: &[String]) -> Result<(String, String, String, String)> {
    let operation_option = option_value(args, "--operation")?;
    let positionals = positional_args(args);
    let (resource_args, operation) = match option_value(args, "--run-id")?.is_some() {
        false => match (positionals.as_slice(), operation_option) {
            ([run_id, operation, resource], None) => {
                (vec![run_id.clone(), resource.clone()], operation.clone())
            }
            ([run_id, operation, kind, name], None) => (
                vec![run_id.clone(), kind.clone(), name.clone()],
                operation.clone(),
            ),
            ([run_id, resource], Some(operation)) => {
                (vec![run_id.clone(), resource.clone()], operation)
            }
            ([run_id, kind, name], Some(operation)) => {
                (vec![run_id.clone(), kind.clone(), name.clone()], operation)
            }
            _ => bail!("buc runs can-i requires <run-id> <operation> <Kind/name>"),
        },
        true => match (positionals.as_slice(), operation_option) {
            ([operation, resource], None) => (vec![resource.clone()], operation.clone()),
            ([operation, kind, name], None) => {
                (vec![kind.clone(), name.clone()], operation.clone())
            }
            ([resource], Some(operation)) => (vec![resource.clone()], operation),
            ([kind, name], Some(operation)) => (vec![kind.clone(), name.clone()], operation),
            _ => bail!("buc runs can-i requires --run-id <run-id> <operation> <Kind/name>"),
        },
    };
    let mut scoped_args = Vec::new();
    if let Some(run_id) = option_value(args, "--run-id")? {
        scoped_args.push("--run-id".to_string());
        scoped_args.push(run_id);
    }
    if let Some(resource) = option_value(args, "--resource")? {
        scoped_args.push("--resource".to_string());
        scoped_args.push(resource);
    }
    if let Some(kind) = option_value(args, "--kind")? {
        scoped_args.push("--kind".to_string());
        scoped_args.push(kind);
    }
    if let Some(name) = option_value(args, "--name")? {
        scoped_args.push("--name".to_string());
        scoped_args.push(name);
    }
    scoped_args.extend(resource_args);
    let (run_id, kind, name) = run_id_and_resource_args(&scoped_args, "runtime resource")?;
    Ok((run_id, operation, kind, name))
}

fn runtime_wait_predicate(args: &[String]) -> Result<RuntimeWaitPredicate> {
    let raw = option_value(args, "--for")?.unwrap_or_else(|| "condition=Ready".to_string());
    if raw.trim().eq_ignore_ascii_case("delete") {
        return Ok(RuntimeWaitPredicate::Delete);
    }
    let Some((kind, value)) = raw.split_once('=') else {
        bail!("--for must be condition=<type>[=<status>], phase=<phase>, or delete");
    };
    match kind {
        "condition" => {
            let (condition, status) = if let Some((condition, status)) = value.split_once('=') {
                (condition.trim(), status.trim())
            } else {
                (value.trim(), "True")
            };
            if condition.is_empty() {
                bail!("--for condition requires a condition type");
            }
            if status.is_empty() {
                bail!("--for condition requires a condition status");
            }
            Ok(RuntimeWaitPredicate::Condition {
                kind: condition.to_string(),
                status: status.to_string(),
            })
        }
        "phase" => {
            let phase = value.trim();
            if phase.is_empty() {
                bail!("--for phase requires a phase value");
            }
            Ok(RuntimeWaitPredicate::Phase(phase.to_ascii_lowercase()))
        }
        _ => bail!("--for must be condition=<type>[=<status>], phase=<phase>, or delete"),
    }
}

fn runtime_wait_predicate_matches(value: &Value, predicate: &RuntimeWaitPredicate) -> bool {
    match predicate {
        RuntimeWaitPredicate::Phase(expected) => runtime_status_phase(value)
            .map(|phase| phase == expected.to_ascii_lowercase())
            .unwrap_or(false),
        RuntimeWaitPredicate::Condition { kind, status } => runtime_status_condition(value, kind)
            .map(|observed| observed.eq_ignore_ascii_case(status))
            .unwrap_or(false),
        RuntimeWaitPredicate::Delete => runtime_deletion_timestamp(value).is_some(),
    }
}

fn runtime_wait_terminal_failure(value: &Value, predicate: &RuntimeWaitPredicate) -> bool {
    if matches!(
        predicate,
        RuntimeWaitPredicate::Phase(_) | RuntimeWaitPredicate::Delete
    ) {
        return false;
    }
    matches!(
        runtime_status_phase(value).as_deref(),
        Some("failed" | "expired" | "cancelled")
    )
}

fn runtime_wait_predicate_label(predicate: &RuntimeWaitPredicate) -> String {
    match predicate {
        RuntimeWaitPredicate::Phase(phase) => format!("phase={phase}"),
        RuntimeWaitPredicate::Condition { kind, status } => format!("condition={kind}={status}"),
        RuntimeWaitPredicate::Delete => "delete".to_string(),
    }
}

fn runtime_wait_latest_status_summary(value: &Value, predicate: &RuntimeWaitPredicate) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "phase={}",
        runtime_status_phase(value).unwrap_or_else(|| "unknown".to_string())
    ));
    if let Some(reason) = runtime_status_reason(value) {
        parts.push(format!("reason={reason}"));
    }
    if let RuntimeWaitPredicate::Condition { kind, .. } = predicate {
        let observed =
            runtime_status_condition(value, kind).unwrap_or_else(|| "unknown".to_string());
        parts.push(format!("condition={kind}={observed}"));
    }
    if let Some(resource_version) = runtime_status_resource_version(value) {
        parts.push(format!("resource_version={resource_version}"));
    }
    if let Some(generation) = runtime_status_generation_summary(value) {
        parts.push(generation);
    }
    if let Some(deletion_timestamp) = runtime_deletion_timestamp(value) {
        parts.push(format!("deletion_timestamp={deletion_timestamp}"));
    }
    parts.join(" ")
}

fn runtime_status_phase(value: &Value) -> Option<String> {
    value
        .get("phase")
        .or_else(|| value.pointer("/status/phase"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|phase| !phase.is_empty())
        .map(|phase| phase.to_ascii_lowercase())
}

fn runtime_status_reason(value: &Value) -> Option<String> {
    value
        .get("reason")
        .or_else(|| value.pointer("/status/reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(ToString::to_string)
}

fn runtime_status_resource_version(value: &Value) -> Option<String> {
    value
        .get("resourceVersion")
        .or_else(|| value.pointer("/metadata/resourceVersion"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|resource_version| !resource_version.is_empty())
        .map(ToString::to_string)
}

fn runtime_status_generation_summary(value: &Value) -> Option<String> {
    let generation = value
        .get("generation")
        .or_else(|| value.pointer("/metadata/generation"))
        .and_then(Value::as_i64)?;
    let observed = value
        .get("observedGeneration")
        .or_else(|| value.pointer("/status/observedGeneration"))
        .and_then(Value::as_i64);
    match observed {
        Some(observed) if observed == generation => Some(format!(
            "generation={generation}/{observed} freshness=current"
        )),
        Some(observed) => Some(format!(
            "generation={generation}/{observed} freshness=stale"
        )),
        None => Some(format!("generation={generation}")),
    }
}

fn runtime_deletion_timestamp(value: &Value) -> Option<String> {
    value
        .get("deletionTimestamp")
        .or_else(|| value.pointer("/metadata/deletionTimestamp"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|timestamp| !timestamp.is_empty())
        .map(ToString::to_string)
}

fn runtime_status_condition(value: &Value, kind: &str) -> Option<String> {
    value
        .get("conditions")
        .or_else(|| value.pointer("/status/conditions"))
        .and_then(Value::as_array)?
        .iter()
        .find(|condition| {
            condition
                .get("type")
                .and_then(Value::as_str)
                .map(|observed| observed.eq_ignore_ascii_case(kind))
                .unwrap_or(false)
        })
        .and_then(|condition| condition.get("status").and_then(Value::as_str))
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .map(ToString::to_string)
}

fn optional_resource_args(args: &[String]) -> Result<Option<(String, String)>> {
    let resource = option_value(args, "--resource")?;
    let kind = option_value(args, "--kind")?;
    let name = option_value(args, "--name")?;
    match (resource, kind, name) {
        (Some(resource), None, None) => return Ok(Some(parse_resource_ref(&resource)?)),
        (None, Some(kind), Some(name)) => return Ok(Some((kind, name))),
        (None, None, None) => {}
        (Some(_), _, _) => bail!("use either --resource Kind/name or --kind KIND --name NAME"),
        (None, Some(_), None) => {}
        (None, None, Some(_)) => bail!("--kind and --name must be provided together"),
    }

    let positionals = positional_args(args);
    let uses_run_id_option = option_value(args, "--run-id")?.is_some();
    let resource_positionals = if uses_run_id_option {
        positionals.as_slice()
    } else {
        match positionals.as_slice() {
            [] | [_] => &[][..],
            [_, rest @ ..] => rest,
        }
    };
    match resource_positionals {
        [] => Ok(None),
        [resource] => Ok(Some(parse_resource_ref(resource)?)),
        [kind, name] => Ok(Some((kind.clone(), name.clone()))),
        _ => bail!("runtime resource must be Kind/name or KIND NAME"),
    }
}

fn parse_resource_ref(value: &str) -> Result<(String, String)> {
    let Some((kind, name)) = value.split_once('/') else {
        bail!("runtime resource must be written as Kind/name");
    };
    let kind = kind.trim();
    let name = name.trim();
    if kind.is_empty() || name.is_empty() {
        bail!("runtime resource must include both kind and name");
    }
    Ok((kind.to_string(), name.to_string()))
}

fn runtime_log_stream_arg(args: &[String]) -> Result<Option<String>> {
    let Some(stream) = option_value(args, "--stream")? else {
        return Ok(None);
    };
    match stream.as_str() {
        "stdout" | "stderr" => Ok(Some(stream)),
        _ => bail!("--stream must be stdout or stderr"),
    }
}

fn split_command_after_double_dash(
    args: &[String],
    command_name: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        bail!("{command_name} requires `-- COMMAND [ARG...]` after the runtime resource");
    };
    let command = args.iter().skip(separator + 1).cloned().collect::<Vec<_>>();
    if command.is_empty() {
        bail!("{command_name} requires a non-empty command after `--`");
    }
    Ok((args[..separator].to_vec(), command))
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
    let digest = single_positional_or_option(args, "--package-digest", "package digest")?;
    if !is_sha256_digest(&digest) {
        bail!("package digest must be sha256:<64 lowercase hex chars>");
    }
    Ok(digest)
}

fn is_sha256_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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

fn prepare_authoring_context_input(path: &Path) -> Result<PreparedAuthoringContextInput> {
    if !path.is_file() {
        bail!("authoring YAML path does not exist: {}", path.display());
    }
    let build_context = resolve_project_build_context(path)?;
    let temp_root = make_temp_dir("buc-authoring-context-upload")?;
    let archive_path = temp_root.join("authoring-context.tgz");
    create_authoring_context_archive(&build_context, &archive_path)?;
    Ok(PreparedAuthoringContextInput {
        archive_path,
        source_label: path.display().to_string(),
        entrypoint: build_context.entrypoint.clone(),
        project_manifest: project_manifest_evidence(&build_context),
        temp_root,
    })
}

fn resolve_project_build_context(path: &Path) -> Result<ProjectBuildContext> {
    let canonical_path = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve authoring YAML {}", path.display()))?;
    let search_dir = canonical_path
        .parent()
        .ok_or_else(|| anyhow!("authoring YAML has no parent directory: {}", path.display()))?;
    let manifest_path = find_project_manifest(search_dir)?.ok_or_else(|| {
        anyhow!(
            "hosted YAML builds require {PROJECT_MANIFEST_YAML} or {PROJECT_MANIFEST_YML}. Add a project manifest above {} that declares the build entrypoint and included files.",
            path.display()
        )
    })?;
    let project_root = manifest_path
        .parent()
        .ok_or_else(|| {
            anyhow!(
                "project manifest has no parent: {}",
                manifest_path.display()
            )
        })?
        .to_path_buf();
    let entrypoint_path = canonical_path
        .strip_prefix(&project_root)
        .with_context(|| {
            format!(
                "authoring YAML {} must be inside project manifest root {}",
                path.display(),
                project_root.display()
            )
        })?;
    let entrypoint = as_posix_relative_path(entrypoint_path)?;
    let manifest_rel = as_posix_relative_path(
        manifest_path
            .strip_prefix(&project_root)
            .with_context(|| "project manifest path escaped its root")?,
    )?;
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest_yaml: serde_yaml::Value = serde_yaml::from_str(&raw).with_context(|| {
        format!(
            "project manifest YAML is invalid: {}",
            manifest_path.display()
        )
    })?;
    let manifest: Value = serde_json::to_value(manifest_yaml)?;
    let object = manifest.as_object().ok_or_else(|| {
        anyhow!(
            "project manifest must contain a YAML object: {}",
            manifest_path.display()
        )
    })?;
    let schema_version = string_field(object, "schema_version", "project manifest")?;
    if schema_version != PROJECT_MANIFEST_SCHEMA_VERSION {
        bail!(
            "project manifest schema_version must be {PROJECT_MANIFEST_SCHEMA_VERSION}, got {schema_version}"
        );
    }
    let targets = object
        .get("targets")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("project manifest must declare targets.hosted_cloud"))?;
    if !targets.get("hosted_cloud").is_some_and(Value::is_object) {
        bail!("project manifest must declare targets.hosted_cloud");
    }
    let project_id = object
        .get("project")
        .and_then(Value::as_object)
        .and_then(|project| project.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("project manifest must declare project.id"))?
        .to_string();
    if !is_valid_project_id(&project_id) {
        bail!("project manifest project.id must start with an ASCII letter or digit and contain only ASCII letters, digits, '_', '.', or '-'");
    }
    let package_sources = object
        .get("package_sources")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("project manifest must declare package_sources"))?;
    let mut matches = Vec::new();
    for (name, source) in package_sources {
        let source = source
            .as_object()
            .ok_or_else(|| anyhow!("project manifest package_sources.{name} must be an object"))?;
        let source_root = optional_string_field(source, "root").unwrap_or_else(|| ".".to_string());
        let source_root =
            validate_manifest_rel_dir(&source_root, &format!("package_sources.{name}.root"))?;
        let entrypoints = string_array_field(
            source,
            "entrypoints",
            &format!("package_sources.{name}.entrypoints"),
        )?;
        if !entrypoints.iter().any(|candidate| candidate == &entrypoint) {
            continue;
        }
        if !entrypoint_in_source_root(&entrypoint, &source_root) {
            bail!(
                "project manifest package_sources.{name} declares entrypoint {entrypoint} outside root {source_root}"
            );
        }
        let include = string_array_field(
            source,
            "include",
            &format!("package_sources.{name}.include"),
        )?;
        if include.is_empty() {
            bail!("project manifest package_sources.{name}.include must not be empty");
        }
        let exclude = optional_string_array_field(
            source,
            "exclude",
            &format!("package_sources.{name}.exclude"),
        )?;
        matches.push(ProjectBuildContext {
            project_root: project_root.clone(),
            manifest_rel: manifest_rel.clone(),
            manifest_digest: sha256_digest(&fs::read(&manifest_path)?),
            project_id: project_id.clone(),
            package_source: name.clone(),
            source_root,
            entrypoint: entrypoint.clone(),
            include: normalize_manifest_patterns(
                include,
                &format!("package_sources.{name}.include"),
            )?,
            exclude: normalize_manifest_patterns(
                exclude,
                &format!("package_sources.{name}.exclude"),
            )?,
        });
    }
    match matches.len() {
        0 => bail!(
            "project manifest {} does not declare entrypoint {entrypoint} in any package source",
            manifest_path.display()
        ),
        1 => Ok(matches.remove(0)),
        _ => bail!(
            "project manifest {} declares entrypoint {entrypoint} in multiple package sources; make the source boundary unambiguous",
            manifest_path.display()
        ),
    }
}

fn find_project_manifest(start_dir: &Path) -> Result<Option<PathBuf>> {
    let mut current = Some(start_dir);
    while let Some(dir) = current {
        let yaml = dir.join(PROJECT_MANIFEST_YAML);
        let yml = dir.join(PROJECT_MANIFEST_YML);
        let has_yaml = yaml.is_file();
        let has_yml = yml.is_file();
        if has_yaml && has_yml {
            bail!(
                "project manifest is ambiguous: both {PROJECT_MANIFEST_YAML} and {PROJECT_MANIFEST_YML} exist in {}. Keep exactly one.",
                dir.display()
            );
        }
        if has_yaml {
            return Ok(Some(fs::canonicalize(yaml)?));
        }
        if has_yml {
            return Ok(Some(fs::canonicalize(yml)?));
        }
        current = dir.parent();
    }
    Ok(None)
}

fn project_manifest_evidence(context: &ProjectBuildContext) -> Value {
    json!({
        "schema_version": PROJECT_MANIFEST_SCHEMA_VERSION,
        "path": context.manifest_rel,
        "digest": context.manifest_digest,
        "project_id": context.project_id,
        "package_source": context.package_source,
        "source_root": context.source_root,
        "entrypoint": context.entrypoint,
    })
}

fn is_valid_project_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphanumeric())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-')
}

fn string_field<'a>(object: &'a Map<String, Value>, key: &str, context: &str) -> Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("{context}.{key} must be a non-empty string"))
}

fn optional_string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_array_field(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Vec<String>> {
    let value = object
        .get(key)
        .ok_or_else(|| anyhow!("{context} is required"))?;
    parse_string_array(value, context)
}

fn optional_string_array_field(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Vec<String>> {
    match object.get(key) {
        Some(value) => parse_string_array(value, context),
        None => Ok(Vec::new()),
    }
}

fn parse_string_array(value: &Value, context: &str) -> Result<Vec<String>> {
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("{context} must be a string array"))?;
    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let value = item
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("{context}[{idx}] must be a non-empty string"))?;
        out.push(value.to_string());
    }
    Ok(out)
}

fn validate_manifest_rel_dir(raw: &str, field: &str) -> Result<String> {
    let normalized = validate_manifest_rel_path(raw, field)?;
    Ok(if normalized == "." {
        ".".to_string()
    } else {
        normalized
    })
}

fn normalize_manifest_patterns(patterns: Vec<String>, field: &str) -> Result<Vec<String>> {
    patterns
        .into_iter()
        .map(|pattern| validate_manifest_rel_path(&pattern, field))
        .collect()
}

fn validate_manifest_rel_path(raw: &str, field: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.contains('\\') {
        bail!("{field} entries must be relative POSIX paths");
    }
    if trimmed
        .split('/')
        .any(|part| part.is_empty() || part == "..")
    {
        bail!("{field} entries cannot contain empty or parent path segments");
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

fn entrypoint_in_source_root(entrypoint: &str, source_root: &str) -> bool {
    source_root == "."
        || entrypoint == source_root
        || entrypoint.starts_with(&format!("{source_root}/"))
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

fn create_authoring_context_archive(
    context: &ProjectBuildContext,
    archive_path: &Path,
) -> Result<()> {
    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(archive_path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    let mut plan = AuthoringContextArchivePlan::default();
    collect_authoring_context_files(context, &context.project_root, &mut plan)?;
    if plan.files.is_empty() {
        bail!(
            "project manifest package source {} selected no uploadable files",
            context.package_source
        );
    }
    if !plan.files.iter().any(|file| file == &context.entrypoint) {
        bail!(
            "project manifest package source {} did not include entrypoint {}",
            context.package_source,
            context.entrypoint
        );
    }
    if !plan.files.iter().any(|file| file == &context.manifest_rel) {
        bail!(
            "project manifest package source {} did not include {}",
            context.package_source,
            context.manifest_rel
        );
    }
    plan.files.sort();
    for rel in plan.files {
        builder.append_path_with_name(context.project_root.join(&rel), Path::new(&rel))?;
    }
    builder.finish()?;
    Ok(())
}

fn collect_authoring_context_files(
    context: &ProjectBuildContext,
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
        let rel = path
            .strip_prefix(&context.project_root)
            .with_context(|| format!("authoring context path escaped root: {}", path.display()))?;
        let rel = as_posix_relative_path(rel)?;
        if file_type.is_dir() && project_context_excludes_dir(context, &rel) {
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
            collect_authoring_context_files(context, &path, plan)?;
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
        if !project_context_includes_file(context, &rel) {
            continue;
        }
        record_authoring_context_bytes(plan, &path, file_size)?;
        plan.files.push(rel);
    }
    Ok(())
}

fn project_context_includes_file(context: &ProjectBuildContext, rel: &str) -> bool {
    if rel == context.manifest_rel || rel == context.entrypoint {
        return true;
    }
    if context
        .exclude
        .iter()
        .any(|pattern| manifest_pattern_matches(pattern, rel))
    {
        return false;
    }
    context
        .include
        .iter()
        .any(|pattern| manifest_pattern_matches(pattern, rel))
}

fn project_context_excludes_dir(context: &ProjectBuildContext, rel: &str) -> bool {
    context
        .exclude
        .iter()
        .any(|pattern| manifest_pattern_excludes_dir(pattern, rel))
}

fn manifest_pattern_excludes_dir(pattern: &str, rel: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return rel == prefix || rel.starts_with(&format!("{prefix}/"));
    }
    manifest_pattern_matches(pattern, rel)
}

fn manifest_pattern_matches(pattern: &str, rel: &str) -> bool {
    if pattern == "**" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return rel == prefix || rel.starts_with(&format!("{prefix}/"));
    }
    glob_segments_match(
        &pattern.split('/').collect::<Vec<_>>(),
        &rel.split('/').collect::<Vec<_>>(),
    )
}

fn glob_segments_match(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        return glob_segments_match(&pattern[1..], path)
            || (!path.is_empty() && glob_segments_match(pattern, &path[1..]));
    }
    if path.is_empty() {
        return false;
    }
    glob_segment_match(pattern[0], path[0]) && glob_segments_match(&pattern[1..], &path[1..])
}

fn glob_segment_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == value;
    }
    let mut rest = value;
    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if idx == 0 {
            let Some(stripped) = rest.strip_prefix(part) else {
                return false;
            };
            rest = stripped;
            continue;
        }
        let Some(found) = rest.find(part) else {
            return false;
        };
        rest = &rest[found + part.len()..];
    }
    pattern.ends_with('*') || rest.is_empty()
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
            "authoring context has too many entries for hosted Cloud build: {} exceeds limit {}. Narrow bucephalus.project.yaml include patterns or remove generated files before running `buc build`.",
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
            "authoring context is too large for hosted Cloud build: {} pushes expanded size to {} bytes, above limit {}. Narrow bucephalus.project.yaml include patterns or remove generated files before running `buc build`.",
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

fn ensure_build_execution_environment_matches(
    value: &Value,
    expected_source_kind: &str,
) -> Result<()> {
    let environment = value
        .get("build_environment")
        .ok_or_else(|| anyhow!("hosted build response is missing build_environment"))?;
    let builder_kind = environment
        .get("builder")
        .and_then(|builder| builder.get("kind"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("hosted build response is missing build_environment.builder.kind")
        })?;
    let core = environment
        .get("core")
        .ok_or_else(|| anyhow!("hosted build response is missing build_environment.core"))?;
    let core_executed = core
        .get("executed")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow!("hosted build response is missing build_environment.core.executed")
        })?;

    match expected_source_kind {
        "authoring_context" => {
            if builder_kind != "hosted_authoring_builder" {
                bail!(
                    "hosted build builder mismatch: authoring_context builds must report builder.kind=hosted_authoring_builder, got {builder_kind}"
                );
            }
            if !core_executed {
                bail!(
                    "hosted build core execution mismatch: authoring_context builds must report core.executed=true"
                );
            }
            let command = core.get("command").and_then(Value::as_str).ok_or_else(|| {
                anyhow!("hosted build response is missing build_environment.core.command")
            })?;
            if command != "bucephalus build" {
                bail!(
                    "hosted build core command mismatch: authoring_context builds must run bucephalus build, got {command}"
                );
            }
            let path = core
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| {
                    anyhow!("hosted build response is missing build_environment.core.path")
                })?;
            if path.contains('\0') {
                bail!("hosted build response has invalid build_environment.core.path");
            }
            let timeout_ms = core
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    anyhow!("hosted build response is missing build_environment.core.timeout_ms")
                })?;
            if timeout_ms == 0 {
                bail!("hosted build response has invalid build_environment.core.timeout_ms");
            }
        }
        "sealed_package" => {
            if builder_kind != "sealed_package_importer" {
                bail!(
                    "hosted build builder mismatch: sealed_package imports must report builder.kind=sealed_package_importer, got {builder_kind}"
                );
            }
            if core_executed {
                bail!(
                    "hosted build core execution mismatch: sealed_package imports must report core.executed=false because Cloud did not author the package"
                );
            }
            for field in ["command", "path", "timeout_ms"] {
                if core.get(field).is_some_and(|value| !value.is_null()) {
                    bail!(
                        "hosted build core execution mismatch: sealed_package imports must report build_environment.core.{field}=null"
                    );
                }
            }
        }
        _ => bail!(
            "hosted build source kind mismatch: unsupported input_kind {expected_source_kind}"
        ),
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

fn ensure_build_source_project_manifest_matches(
    value: &Value,
    expected_project_manifest: Option<&Value>,
) -> Result<()> {
    let source_manifest = value
        .get("build_environment")
        .and_then(|environment| environment.get("source"))
        .and_then(|source| source.get("project_manifest"));
    match expected_project_manifest {
        Some(expected) => {
            let actual = source_manifest.ok_or_else(|| {
                anyhow!(
                    "hosted build response is missing build_environment.source.project_manifest"
                )
            })?;
            for field in [
                "schema_version",
                "path",
                "digest",
                "project_id",
                "package_source",
                "source_root",
                "entrypoint",
            ] {
                if actual.get(field) != expected.get(field) {
                    bail!(
                        "hosted build project manifest mismatch for {field}: requested {}, API built {}",
                        compact_json_lossy(expected),
                        compact_json_lossy(actual)
                    );
                }
            }
        }
        None => {
            if source_manifest.is_some() {
                bail!(
                    "hosted build source mismatch: sealed package builds must not report an authoring project_manifest"
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
    ensure_authoring_compiler_contract(contract, expected_source_kind)?;
    ensure_authoring_provenance_contract(contract, expected_source_kind)?;
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

fn ensure_authoring_compiler_contract(contract: &Value, expected_source_kind: &str) -> Result<()> {
    let compiler = contract
        .get("authoring_compiler")
        .ok_or_else(|| anyhow!("hosted build package contract is missing authoring_compiler"))?;
    match expected_source_kind {
        "authoring_context" => {
            let compiler = compiler.as_str().ok_or_else(|| {
                anyhow!("hosted build package contract has invalid authoring_compiler")
            })?;
            if compiler != "core_universal_v1" {
                bail!(
                    "hosted build package contract mismatch: authoring_compiler must be core_universal_v1, got {compiler}"
                );
            }
        }
        "sealed_package" => {
            if !compiler.is_null() {
                bail!(
                    "hosted build package contract mismatch: sealed package imports must report authoring_compiler=null because Cloud did not author the package"
                );
            }
        }
        _ => bail!("hosted build package contract input mismatch: unsupported input_kind {expected_source_kind}"),
    }
    Ok(())
}

fn ensure_authoring_provenance_contract(
    contract: &Value,
    expected_source_kind: &str,
) -> Result<()> {
    let provenance = contract
        .get("authoring_provenance")
        .ok_or_else(|| anyhow!("hosted build package contract is missing authoring_provenance"))?;
    let status = provenance
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("hosted build package contract has invalid authoring_provenance.status")
        })?;
    let source = provenance
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("hosted build package contract has invalid authoring_provenance.source")
        })?;
    match expected_source_kind {
        "authoring_context" => {
            if status != "hosted_attested" || source != "hosted_core" {
                bail!(
                    "hosted build package contract mismatch: authoring_context builds must report authoring_provenance=hosted_attested/hosted_core, got {status}/{source}"
                );
            }
        }
        "sealed_package" => {
            if status != "external_unattested" || source != "sealed_package_manifest" {
                bail!(
                    "hosted build package contract mismatch: sealed package imports must report authoring_provenance=external_unattested/sealed_package_manifest because Cloud did not author the package, got {status}/{source}"
                );
            }
        }
        _ => bail!(
            "hosted build package contract input mismatch: unsupported input_kind {expected_source_kind}"
        ),
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

fn ensure_runtime_response_matches_if_present(value: &Value, expected_run_id: &str) -> Result<()> {
    if value.get("cloud_run_id").is_some() {
        ensure_runtime_response_matches(value, expected_run_id)?;
    }
    Ok(())
}

fn ensure_resource_envelope(value: &Value) -> Result<()> {
    value
        .get("resource")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("runtime access response is missing resource object"))?;
    Ok(())
}

fn wait_for_runtime_access_resource(
    context: &CliContext,
    run_id: &str,
    initial: &Value,
    wait_kind: RuntimeAccessWaitKind,
    wait_seconds: u64,
) -> Result<Value> {
    let (kind, name) = runtime_resource_kind_name(initial)?;
    let expected_kind = match wait_kind {
        RuntimeAccessWaitKind::PortForward => "PortForward",
        RuntimeAccessWaitKind::Exec => "Exec",
    };
    if kind != expected_kind {
        bail!("runtime access wait expected {expected_kind}, API returned {kind}/{name}");
    }

    let mut latest = initial.clone();
    let deadline = Instant::now() + Duration::from_secs(wait_seconds);
    loop {
        let phase = runtime_resource_phase(&latest);
        if runtime_access_wait_finished(wait_kind, phase.as_deref()) {
            return Ok(latest);
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for {} after {}s; latest phase={}",
                runtime_resource_ref(&kind, &name),
                wait_seconds,
                phase.unwrap_or_else(|| "unknown".to_string())
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(Duration::from_millis(RUNTIME_ACCESS_WAIT_POLL_MS)));
        latest = cloud_fetch(
            context,
            Method::GET,
            &format!(
                "/v1/runs/{}/runtime/resources/{}/{}",
                encode_path_segment(run_id),
                encode_path_segment(&kind),
                encode_path_segment(&name)
            ),
            None,
            None,
        )?;
        ensure_resource_envelope(&latest)?;
    }
}

fn runtime_port_forward_attach_plan(
    value: &Value,
    requested_local_port: Option<u64>,
    fallback_target_port: u64,
) -> Result<RuntimePortForwardAttachPlan> {
    let resource = runtime_resource_object(value);
    let kind = resource
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if kind != "PortForward" {
        bail!("--attach expected a PortForward resource, API returned {kind}");
    }
    let phase = resource
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if !phase.eq_ignore_ascii_case("active") {
        bail!("--attach requires an active PortForward resource; latest phase={phase}");
    }
    let mode = runtime_connection_string(resource, "mode").unwrap_or("");
    if mode == "gcp_iap_ssh" {
        return runtime_gce_iap_port_forward_attach_spec(
            resource,
            requested_local_port,
            fallback_target_port,
        )
        .map(RuntimePortForwardAttachPlan::GceIap);
    }
    if let Some(endpoint) =
        runtime_port_forward_client_endpoint_attach_spec(resource, requested_local_port)?
    {
        return Ok(RuntimePortForwardAttachPlan::ClientEndpoint(endpoint));
    }
    bail!(
        "--attach requires a GCE IAP provider tunnel or a client-reachable PortForward endpoint; connection.mode={mode}"
    );
}

fn runtime_gce_iap_port_forward_attach_spec(
    resource: &Value,
    requested_local_port: Option<u64>,
    fallback_target_port: u64,
) -> Result<RuntimePortForwardAttachSpec> {
    let target_port =
        match runtime_port_forward_connection_port(resource, "/status/connection/target_port")? {
            Some(port) => port,
            None => runtime_port_forward_connection_port(resource, "/spec/target_port")?
                .ok_or_else(|| {
                    anyhow!("--attach requires connection.target_port or spec.target_port")
                })?,
        };
    let local_port = requested_local_port
        .map(runtime_port_from_u64)
        .transpose()?
        .or(runtime_port_forward_connection_port(
            resource,
            "/status/connection/local_port",
        )?)
        .or(runtime_port_forward_connection_port(
            resource,
            "/spec/local_port",
        )?)
        .unwrap_or(runtime_port_from_u64(fallback_target_port)?);
    Ok(RuntimePortForwardAttachSpec {
        project_id: runtime_required_connection_string(resource, "project_id")?,
        zone: runtime_required_connection_string(resource, "zone")?,
        instance_name: runtime_required_connection_string(resource, "instance_name")?,
        target_host: runtime_required_connection_string(resource, "target_host")?,
        target_port,
        local_port,
    })
}

fn runtime_port_forward_client_endpoint_attach_spec(
    resource: &Value,
    requested_local_port: Option<u64>,
) -> Result<Option<RuntimePortForwardClientEndpointAttachSpec>> {
    let local_port = requested_local_port
        .map(runtime_port_from_u64)
        .transpose()?
        .or(runtime_port_forward_connection_port(
            resource,
            "/status/connection/local_port",
        )?)
        .or(runtime_port_forward_connection_port(
            resource,
            "/spec/local_port",
        )?);
    let endpoint = runtime_connection_string(resource, "client_endpoint")
        .or_else(|| runtime_connection_string(resource, "client_listen"))
        .map(ToString::to_string)
        .or_else(|| {
            let client_reachable = resource
                .pointer("/status/connection/client_reachable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if client_reachable {
                local_port.map(|port| format!("tcp://127.0.0.1:{port}"))
            } else {
                None
            }
        });
    Ok(
        endpoint.map(|endpoint| RuntimePortForwardClientEndpointAttachSpec {
            endpoint,
            local_port,
        }),
    )
}

fn runtime_resource_object(value: &Value) -> &Value {
    value.get("resource").unwrap_or(value)
}

fn ensure_runtime_port_forward_success(value: &Value) -> Result<()> {
    let (kind, name) = runtime_resource_kind_name(value)?;
    if kind != "PortForward" {
        bail!("runtime port-forward expected PortForward resource, API returned {kind}/{name}");
    }
    let phase = runtime_resource_phase(value);
    if let Some("failed" | "expired" | "cancelled") = phase.as_deref() {
        let reason = runtime_resource_object(value)
            .pointer("/status/reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.trim().is_empty())
            .map(|reason| format!(" reason={reason}"))
            .unwrap_or_default();
        bail!(
            "runtime port-forward {kind}/{name} ended with phase={}{}",
            phase.unwrap(),
            reason
        );
    }
    Ok(())
}

fn ensure_runtime_exec_success(value: &Value) -> Result<()> {
    let (kind, name) = runtime_resource_kind_name(value)?;
    if kind != "Exec" {
        bail!("runtime exec expected Exec resource, API returned {kind}/{name}");
    }
    let phase = runtime_resource_phase(value);
    match phase.as_deref() {
        Some("completed") => {
            let exit_code = runtime_resource_object(value)
                .pointer("/status/connection/exit_code")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("runtime exec {kind}/{name} completed without exit_code"))?;
            if exit_code != 0 {
                bail!("runtime exec {kind}/{name} exited with code {exit_code}");
            }
        }
        Some("failed" | "expired" | "cancelled") => {
            let reason = runtime_resource_object(value)
                .pointer("/status/reason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.trim().is_empty())
                .map(|reason| format!(" reason={reason}"))
                .unwrap_or_default();
            bail!(
                "runtime exec {kind}/{name} ended with phase={}{}",
                phase.unwrap(),
                reason
            );
        }
        _ => {}
    }
    Ok(())
}

fn runtime_required_connection_string(resource: &Value, key: &str) -> Result<String> {
    runtime_connection_string(resource, key)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("--attach requires connection.{key}"))
}

fn runtime_port_forward_connection_port(resource: &Value, pointer: &str) -> Result<Option<u16>> {
    resource
        .pointer(pointer)
        .and_then(Value::as_u64)
        .map(runtime_port_from_u64)
        .transpose()
}

fn runtime_port_from_u64(value: u64) -> Result<u16> {
    if value == 0 || value > 65535 {
        bail!("port must be between 1 and 65535, got {value}");
    }
    Ok(value as u16)
}

fn runtime_port_forward_attach_plan_requires_cleanup(plan: &RuntimePortForwardAttachPlan) -> bool {
    matches!(plan, RuntimePortForwardAttachPlan::GceIap(_))
}

fn run_runtime_port_forward_attach(plan: &RuntimePortForwardAttachPlan) -> Result<()> {
    match plan {
        RuntimePortForwardAttachPlan::GceIap(spec) => run_gce_iap_port_forward_attach(spec),
        RuntimePortForwardAttachPlan::ClientEndpoint(spec) => {
            run_client_endpoint_port_forward_attach(spec)
        }
    }
}

fn run_gce_iap_port_forward_attach(spec: &RuntimePortForwardAttachSpec) -> Result<()> {
    let args = gcloud_iap_port_forward_args(spec);
    eprintln!(
        "port-forward: forwarding 127.0.0.1:{} -> {}:{} through GCE IAP instance {} (Ctrl-C to stop)",
        spec.local_port, spec.target_host, spec.target_port, spec.instance_name
    );
    let status = Command::new("gcloud")
        .args(&args)
        .status()
        .with_context(|| "failed to start gcloud for GCE IAP port-forward attach")?;
    if !status.success() {
        bail!("gcloud port-forward attach exited with status {status}");
    }
    Ok(())
}

fn run_client_endpoint_port_forward_attach(
    spec: &RuntimePortForwardClientEndpointAttachSpec,
) -> Result<()> {
    let local_port = spec
        .local_port
        .map(|port| format!(" local_port={port}"))
        .unwrap_or_default();
    eprintln!(
        "port-forward: worker reports client-reachable endpoint {}{}",
        spec.endpoint, local_port
    );
    eprintln!(
        "port-forward: this worker-managed tunnel is already active; leaving the PortForward resource active for explicit cleanup or TTL expiry",
    );
    Ok(())
}

fn cleanup_attached_runtime_port_forward(
    context: &CliContext,
    run_id: &str,
    value: &Value,
) -> Result<()> {
    let (kind, name) = runtime_resource_kind_name(value)?;
    if kind != "PortForward" {
        bail!("port-forward cleanup expected PortForward resource, API returned {kind}/{name}");
    }
    let resource_version = runtime_resource_metadata_resource_version(value)?;
    let mut body = Map::new();
    body.insert(
        "reason".to_string(),
        json!("local port-forward attach ended"),
    );
    body.insert("resource_version".to_string(), json!(resource_version));
    let response = cloud_fetch(
        context,
        Method::DELETE,
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}",
            encode_path_segment(run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name),
        ),
        Some(Value::Object(body)),
        None,
    )?;
    ensure_resource_envelope(&response)
}

fn complete_attached_runtime_port_forward(
    context: &CliContext,
    run_id: &str,
    value: &Value,
) -> Result<()> {
    let (kind, name) = runtime_resource_kind_name(value)?;
    if kind != "PortForward" {
        bail!("port-forward complete expected PortForward resource, API returned {kind}/{name}");
    }
    let resource_version = runtime_resource_metadata_resource_version(value)?;
    let mut body = Map::new();
    body.insert(
        "reason".to_string(),
        json!("local port-forward attach ended"),
    );
    body.insert("resource_version".to_string(), json!(resource_version));
    let response = cloud_fetch(
        context,
        Method::POST,
        &format!(
            "/v1/runs/{}/runtime/resources/{}/{}/actions/complete",
            encode_path_segment(run_id),
            encode_path_segment(&kind),
            encode_path_segment(&name),
        ),
        Some(Value::Object(body)),
        None,
    )?;
    ensure_resource_envelope(&response)
}

fn gcloud_iap_port_forward_args(spec: &RuntimePortForwardAttachSpec) -> Vec<String> {
    vec![
        "compute".to_string(),
        "ssh".to_string(),
        spec.instance_name.clone(),
        "--project".to_string(),
        spec.project_id.clone(),
        "--zone".to_string(),
        spec.zone.clone(),
        "--tunnel-through-iap".to_string(),
        "--".to_string(),
        "-N".to_string(),
        "-L".to_string(),
        format!(
            "127.0.0.1:{}:{}:{}",
            spec.local_port, spec.target_host, spec.target_port
        ),
    ]
}

fn runtime_resource_kind_name(value: &Value) -> Result<(String, String)> {
    let resource = value.get("resource").unwrap_or(value);
    let kind = resource
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.trim().is_empty())
        .ok_or_else(|| anyhow!("runtime resource response is missing kind"))?;
    let name = resource
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| anyhow!("runtime resource response is missing metadata.name"))?;
    Ok((kind.to_string(), name.to_string()))
}

fn runtime_resource_metadata_resource_version(value: &Value) -> Result<String> {
    let resource = value.get("resource").unwrap_or(value);
    resource
        .pointer("/metadata/resourceVersion")
        .or_else(|| resource.pointer("/metadata/resource_version"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|resource_version| !resource_version.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("runtime resource response is missing metadata.resourceVersion"))
}

fn runtime_resource_phase(value: &Value) -> Option<String> {
    value
        .get("resource")
        .unwrap_or(value)
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|phase| !phase.is_empty())
        .map(|phase| phase.to_ascii_lowercase())
}

fn runtime_access_wait_finished(wait_kind: RuntimeAccessWaitKind, phase: Option<&str>) -> bool {
    match (wait_kind, phase) {
        (RuntimeAccessWaitKind::PortForward, Some("active")) => true,
        (RuntimeAccessWaitKind::Exec, Some("completed")) => true,
        (_, Some("failed" | "expired" | "cancelled")) => true,
        _ => false,
    }
}

fn runtime_resource_ref(kind: &str, name: &str) -> String {
    format!("{kind}/{name}")
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
    let response = cloud_fetch_json_response(context, method, path, body, raw_body)?;
    if response.status < 200 || response.status >= 300 {
        bail!(
            "{}",
            cloud_fetch_error_message(context, response.status, &response.payload)
        );
    }
    Ok(response.payload)
}

fn cloud_fetch_json_response(
    context: &CliContext,
    method: Method,
    path: &str,
    body: Option<Value>,
    raw_body: Option<(Vec<u8>, &str)>,
) -> Result<JsonCloudResponse> {
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
    Ok(JsonCloudResponse {
        status: status.as_u16(),
        payload,
    })
}

fn cloud_fetch_raw(
    context: &CliContext,
    method: Method,
    path: &str,
    body: Option<Value>,
    raw_body: Option<(Vec<u8>, &str)>,
) -> Result<RawCloudResponse> {
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
    let headers = response.headers().clone();
    let bytes = response.bytes()?.to_vec();
    if !status.is_success() {
        let payload = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(
                |_| json!({ "message": String::from_utf8_lossy(&bytes).to_string() }),
            )
        };
        let mut message = cloud_error_message(status.as_u16(), &payload);
        if status.as_u16() == 401 {
            message = append_user_auth_hint(context, message);
        }
        bail!("{message}");
    }
    Ok(RawCloudResponse { bytes, headers })
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

fn cloud_fetch_error_message(context: &CliContext, status: u16, payload: &Value) -> String {
    let message = cloud_error_message(status, payload);
    if status == 401 {
        append_user_auth_hint(context, message)
    } else {
        message
    }
}

fn append_user_auth_hint(context: &CliContext, message: String) -> String {
    let token_path = lab_core::bucephalus_home()
        .ok()
        .map(|home| cloud_login::cloud_token_paths(&home).access);
    cloud_auth_ux::user_auth_hint(
        &message,
        context.user_token.is_some(),
        token_path.as_deref(),
    )
}

const CLOUD_RUNTIME_OPTION_KEYS: &[&str] = &[
    "backend",
    "arch",
    "cpu_count",
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
        "backend" | "arch" | "isolation" => {
            if value.trim().is_empty() {
                bail!("runtime option `{key}` requires a non-empty string");
            }
            let value = value.trim();
            validate_cloud_runtime_string_option(key, value)?;
            Ok(json!(value))
        }
        "cpu_count" | "memory_mb" | "disk_mb" | "timeout_ms" | "max_parallel_trials" => {
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
    runtime_options.insert(key.to_string(), value);
    Ok(())
}

fn validate_cloud_runtime_string_option(key: &str, value: &str) -> Result<()> {
    match key {
        "backend" => {
            if ![
                "runner-docker",
                "runner_docker",
                "local-docker",
                "local_docker",
                "modal",
            ]
            .contains(&value)
            {
                bail!("runtime option `backend` must be one of runner-docker, runner_docker, local-docker, local_docker, modal");
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

fn build_value_options() -> [&'static str; 11] {
    [
        "--file",
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

fn option_value_alias(
    args: &[String],
    canonical_name: &str,
    alias: &str,
) -> Result<Option<String>> {
    let matches = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| {
            (arg == canonical_name || arg == alias).then_some((index, arg.as_str()))
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        bail!("{canonical_name} can only be provided once");
    }
    if let Some((index, name)) = matches.first().copied() {
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

fn runtime_resource_output_option(args: &[String]) -> Result<Option<String>> {
    option_value_alias(args, "--output", "-o")
}

fn option_values(args: &[String], name: &str) -> Result<Vec<String>> {
    args.iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == name).then_some(index))
        .map(|index| {
            let value = args
                .get(index + 1)
                .ok_or_else(|| anyhow!("{name} requires a value"))?;
            if value.starts_with("--") {
                bail!("{name} requires a value, got option {value}");
            }
            Ok(value.clone())
        })
        .collect()
}

fn validation_level_option(args: &[String]) -> Result<Option<String>> {
    let Some(value) = option_value(args, "--validation-level")? else {
        return Ok(None);
    };
    match value.as_str() {
        "authoring" | "package" | "launch_hint" => Ok(Some(value)),
        _ => bail!("--validation-level must be one of authoring, package, launch_hint"),
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

fn bounded_number_option_string(args: &[String], name: &str, max: u64) -> Result<Option<String>> {
    Ok(bounded_number_option(args, name, max)?.map(|value| value.to_string()))
}

fn bounded_number_option(args: &[String], name: &str, max: u64) -> Result<Option<u64>> {
    let Some(value) = number_option(args, name)? else {
        return Ok(None);
    };
    if value > max {
        bail!("{name} must be <= {max}");
    }
    Ok(Some(value))
}

fn bounded_non_negative_number_option(
    args: &[String],
    name: &str,
    max: u64,
) -> Result<Option<u64>> {
    let Some(value) = option_value(args, name)? else {
        return Ok(None);
    };
    let parsed = value
        .parse::<u64>()
        .with_context(|| format!("{name} requires a non-negative integer"))?;
    if parsed > max {
        bail!("{name} must be <= {max}");
    }
    Ok(Some(parsed))
}

fn runtime_access_wait_seconds(args: &[String]) -> Result<Option<u64>> {
    let no_wait = args.iter().any(|arg| arg == "--no-wait");
    let wait_seconds = bounded_number_option(args, "--wait-seconds", 86_400)?;
    if no_wait && wait_seconds.is_some() {
        bail!("--no-wait and --wait-seconds are mutually exclusive");
    }
    if no_wait {
        Ok(None)
    } else {
        Ok(Some(
            wait_seconds.unwrap_or(DEFAULT_RUNTIME_ACCESS_WAIT_SECONDS),
        ))
    }
}

fn required_runtime_resource_version(args: &[String], operation: &str) -> Result<String> {
    let Some(resource_version) = option_value(args, "--resource-version")? else {
        bail!(
            "runtime {operation} requires --resource-version from `buc runs can-i` or `buc runs describe`; refresh the resource before mutating it"
        );
    };
    let resource_version = resource_version.trim();
    if resource_version.is_empty() {
        bail!("--resource-version must not be empty");
    }
    Ok(resource_version.to_string())
}

fn non_negative_number_option_string(args: &[String], name: &str) -> Result<Option<String>> {
    let Some(value) = option_value(args, name)? else {
        return Ok(None);
    };
    let parsed = value
        .parse::<u64>()
        .with_context(|| format!("{name} requires a non-negative integer"))?;
    Ok(Some(parsed.to_string()))
}

fn positional_args(args: &[String]) -> Vec<String> {
    let options_with_values = [
        "--api-url",
        "--user-token",
        "--issuer",
        "--client-id",
        "--audience",
        "--resource",
        "--scope",
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
        "--validation-level",
        "--target",
        "--q",
        "--limit",
        "--after-row-seq",
        "--continue",
        "--event-type",
        "--source",
        "--resource-kind",
        "--resource-name",
        "--trial-id",
        "--task-id",
        "--kind",
        "--category",
        "--name",
        "--resource",
        "--label-selector",
        "--field-selector",
        "--event-limit",
        "--view",
        "--resource-version",
        "--known-resource",
        "--operation",
        "--for",
        "--timeout-seconds",
        "--interval-seconds",
        "--max-polls",
        "--target-port",
        "--local-port",
        "--protocol",
        "--stream",
        "--tail-lines",
        "--out",
        "--metadata-out",
        "--output",
        "-o",
        "--ttl-seconds",
        "--reason",
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
        if options_with_values.contains(&arg.as_str()) {
            index += 2;
            continue;
        }
        if arg.starts_with("--") {
            index += 1;
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

fn insert_option_u64(object: &mut Map<String, Value>, key: &str, value: Option<u64>) {
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

fn write_raw_output(
    response: &RawCloudResponse,
    out: Option<&str>,
    metadata_out: Option<&str>,
) -> Result<()> {
    write_raw_bytes(&response.bytes, out, false)?;
    write_runtime_raw_metadata(response, metadata_out)
}

fn write_raw_bytes(bytes: &[u8], out: Option<&str>, append: bool) -> Result<()> {
    match out {
        Some("-") | None => {
            std::io::stdout().write_all(bytes)?;
            Ok(())
        }
        Some(path) => {
            if append {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut file| file.write_all(bytes))
                    .with_context(|| format!("failed to append runtime content to {path}"))?;
            } else {
                fs::write(path, bytes)
                    .with_context(|| format!("failed to write runtime content to {path}"))?;
            }
            Ok(())
        }
    }
}

fn appended_raw_log_bytes<'a>(previous: &[u8], current: &'a [u8]) -> &'a [u8] {
    if previous.is_empty() {
        return current;
    }
    if previous == current {
        return &current[current.len()..];
    }
    let max_overlap = previous.len().min(current.len());
    for overlap in (1..=max_overlap).rev() {
        if previous[previous.len() - overlap..] == current[..overlap] {
            return &current[overlap..];
        }
    }
    current
}

fn write_runtime_raw_metadata(
    response: &RawCloudResponse,
    metadata_out: Option<&str>,
) -> Result<()> {
    let Some(path) = metadata_out else {
        return Ok(());
    };
    if path == "-" {
        bail!("--metadata-out must be a file path; stdout is reserved for raw runtime bytes");
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(&runtime_raw_response_metadata(response))?,
    )
    .with_context(|| format!("failed to write runtime response metadata to {path}"))?;
    Ok(())
}

fn runtime_raw_response_metadata(response: &RawCloudResponse) -> Value {
    let mut metadata = Map::new();
    insert_header_metadata(
        &mut metadata,
        "run_id",
        &response.headers,
        "x-bucephalus-run-id",
    );
    insert_header_metadata(
        &mut metadata,
        "log_stream",
        &response.headers,
        "x-bucephalus-log-stream",
    );
    insert_header_metadata(
        &mut metadata,
        "core_run_id",
        &response.headers,
        "x-bucephalus-core-run-id",
    );
    insert_header_metadata(
        &mut metadata,
        "trial_id",
        &response.headers,
        "x-bucephalus-trial-id",
    );
    insert_header_metadata(
        &mut metadata,
        "artifact_role",
        &response.headers,
        "x-bucephalus-artifact-role",
    );
    insert_header_metadata(
        &mut metadata,
        "object_ref",
        &response.headers,
        "x-bucephalus-object-ref",
    );
    if let Some(sha256) = raw_header_string(&response.headers, "x-bucephalus-artifact-sha256")
        .or_else(|| raw_header_string(&response.headers, "x-bucephalus-sha256"))
    {
        metadata.insert("sha256".to_string(), json!(sha256));
    }
    insert_header_metadata(
        &mut metadata,
        "media_type",
        &response.headers,
        "content-type",
    );
    let byte_size = raw_header_string(&response.headers, "content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(response.bytes.len() as u64);
    metadata.insert("byte_size".to_string(), json!(byte_size));

    let mut resource = Map::new();
    insert_header_metadata(
        &mut resource,
        "kind",
        &response.headers,
        "x-bucephalus-resource-kind",
    );
    insert_header_metadata(
        &mut resource,
        "name",
        &response.headers,
        "x-bucephalus-resource-name",
    );
    insert_header_metadata(
        &mut resource,
        "resource_version",
        &response.headers,
        "x-bucephalus-resource-version",
    );
    if !resource.is_empty() {
        metadata.insert("resource".to_string(), Value::Object(resource));
    }
    Value::Object(metadata)
}

fn insert_header_metadata(
    metadata: &mut Map<String, Value>,
    key: &str,
    headers: &HeaderMap,
    header: &str,
) {
    if let Some(value) = raw_header_string(headers, header) {
        metadata.insert(key.to_string(), json!(value));
    }
}

fn raw_header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
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
        if let Some(project_manifest) = source.get("project_manifest") {
            if let Some(project_id) = project_manifest.get("project_id").and_then(Value::as_str) {
                lines.push(format!("build_source_project: {project_id}"));
            }
            if let Some(package_source) = project_manifest
                .get("package_source")
                .and_then(Value::as_str)
            {
                lines.push(format!("build_source_package_source: {package_source}"));
            }
            if let Some(path) = project_manifest.get("path").and_then(Value::as_str) {
                if let Some(digest) = project_manifest.get("digest").and_then(Value::as_str) {
                    lines.push(format!("build_source_manifest: {path} {digest}"));
                } else {
                    lines.push(format!("build_source_manifest: {path}"));
                }
            }
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
        if let Some(provenance) = contract.get("authoring_provenance") {
            if let Some(status) = provenance.get("status").and_then(Value::as_str) {
                let source = provenance
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)");
                lines.push(format!("authoring_provenance: {status}/{source}"));
            }
            if let Some(message) = provenance.get("message").and_then(Value::as_str) {
                lines.push(format!("authoring_provenance_detail: {message}"));
            }
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
        if core.get("executed").and_then(Value::as_bool) == Some(false) {
            let reason = core
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("sealed package input was imported directly");
            lines.push(format!("builder_core: not_run ({reason})"));
        } else {
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
    lines.extend(package_provenance_summary_lines(value));
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
    lines.extend(package_provenance_summary_lines(value));
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
    let mut lines = vec![format!("run_id: {run_id}"), format!("status: {status}")];
    lines.extend(package_provenance_summary_lines(value));
    lines.push(format!("next: buc runs get {run_id}"));
    println!("{}", lines.join("\n"));
    Ok(())
}

fn package_provenance_summary_lines(value: &Value) -> Vec<String> {
    let Some(provenance) = value.get("package_provenance") else {
        return Vec::new();
    };
    let status = provenance
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let source = provenance
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let mut lines = vec![format!("package_provenance: {status}/{source}")];
    if let Some(input_kind) = provenance.get("input_kind").and_then(Value::as_str) {
        lines.push(format!("package_input_kind: {input_kind}"));
    }
    if let Some(message) = provenance.get("message").and_then(Value::as_str) {
        lines.push(format!("package_provenance_detail: {message}"));
    }
    lines
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
            lines.push(format!("{run_id}  [{status}]  {digest}"));
        } else {
            lines.push(format!("{run_id}  [{status}]  {digest}  label={label}"));
        }
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_run_id_list(value: &Value) -> Result<()> {
    let runs = value
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    for run in runs {
        if let Some(id) = run.get("run_id").and_then(Value::as_str) {
            println!("{id}");
        }
    }
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

fn print_runtime_api_resources_summary(value: &Value) -> Result<()> {
    if let Some(lines) = runtime_api_resources_summary_lines(value) {
        println!("{}", lines.join("\n"));
    } else {
        println!("api_resource: {}", compact_json(value)?);
    }
    Ok(())
}

fn runtime_api_resources_summary_lines(value: &Value) -> Option<Vec<String>> {
    let resources = value.get("resources").and_then(Value::as_array)?;
    let mut lines = vec![format!("api_resources: {}", resources.len())];
    if let Some(generated_at) = value.get("generated_at").and_then(Value::as_str) {
        lines.push(format!("generated_at: {generated_at}"));
    }
    let core_run_ids = runtime_explain_string_array(value, "core_run_ids");
    if !core_run_ids.is_empty() {
        lines.push(format!("core_run_ids: {}", core_run_ids.join(",")));
    }
    for resource in resources {
        let kind = resource
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let name = resource.get("name").and_then(Value::as_str).unwrap_or("");
        let mut parts = vec![format!("{kind} {name}")];
        if let Some(count) = resource.get("count").and_then(Value::as_u64) {
            parts.push(format!("count={count}"));
        }
        for (label, key) in [
            ("short", "shortNames"),
            ("categories", "categories"),
            ("verbs", "verbs"),
            ("subresources", "subresources"),
            ("actions", "actions"),
            ("access", "access"),
        ] {
            let values = runtime_explain_string_array(resource, key);
            if !values.is_empty() {
                parts.push(format!("{label}={}", values.join(",")));
            }
        }
        if let Some(supports) = resource.get("supports").and_then(Value::as_object) {
            let mut selectors = Vec::new();
            if supports
                .get("labelSelector")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                selectors.push("label");
            }
            if supports
                .get("fieldSelector")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                selectors.push("field");
            }
            if !selectors.is_empty() {
                parts.push(format!("selectors={}", selectors.join(",")));
            }
        }
        lines.push(format!("  - {}", parts.join(" ")));
    }
    Some(lines)
}

fn print_runtime_api_resource_explain(value: &Value) -> Result<()> {
    println!("{}", runtime_api_resource_explain_lines(value).join("\n"));
    Ok(())
}

fn runtime_api_resource_explain_lines(value: &Value) -> Vec<String> {
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let name = value.get("name").and_then(Value::as_str).unwrap_or("");
    let singular = value
        .get("singularName")
        .and_then(Value::as_str)
        .unwrap_or("");
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or("");
    let scope = value.get("scope").and_then(Value::as_str).unwrap_or("run");
    let count = value.get("count").and_then(Value::as_u64).unwrap_or(0);
    let mut lines = vec![format!("explain: {kind}")];
    if !description.is_empty() {
        lines.push(format!("description: {description}"));
    }
    if let Some(generated_at) = value.get("generated_at").and_then(Value::as_str) {
        lines.push(format!("generated_at: {generated_at}"));
    }
    let core_run_ids = runtime_explain_string_array(value, "core_run_ids");
    if !core_run_ids.is_empty() {
        lines.push(format!("core_run_ids: {}", core_run_ids.join(",")));
    }
    lines.push(format!(
        "resource: {name} singular={singular} scope={scope} count={count}"
    ));
    for (label, key) in [
        ("short_names", "shortNames"),
        ("categories", "categories"),
        ("verbs", "verbs"),
        ("subresources", "subresources"),
        ("actions", "actions"),
        ("access", "access"),
        ("field_selectors", "fieldSelectors"),
        ("label_selectors", "labelSelectors"),
    ] {
        let values = runtime_explain_string_array(value, key);
        if !values.is_empty() {
            lines.push(format!("{label}: {}", values.join(",")));
        }
    }
    if let Some(supports) = value.get("supports").and_then(Value::as_object) {
        let mut supported = supports
            .iter()
            .filter_map(|(key, value)| value.as_bool().filter(|flag| *flag).map(|_| key.clone()))
            .collect::<Vec<_>>();
        supported.sort();
        if !supported.is_empty() {
            lines.push(format!("supports: {}", supported.join(",")));
        }
    }
    if let Some(columns) = value.get("printerColumns").and_then(Value::as_array) {
        if !columns.is_empty() {
            lines.push("columns:".to_string());
            for column in columns {
                let name = column
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let json_path = column.get("jsonPath").and_then(Value::as_str).unwrap_or("");
                let value_type = column
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("string");
                let priority = column.get("priority").and_then(Value::as_i64).unwrap_or(0);
                let description = column
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                lines.push(format!(
                    "  - {name} {json_path} type={value_type} priority={priority}{}",
                    if description.is_empty() {
                        String::new()
                    } else {
                        format!(" description={description}")
                    }
                ));
            }
        }
    }
    if let Some(paths) = value.get("pathTemplates").and_then(Value::as_object) {
        let mut rows = paths
            .iter()
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(|path| (key.to_string(), path.to_string()))
            })
            .collect::<Vec<_>>();
        if let Some(subresources) = paths.get("subresources").and_then(Value::as_object) {
            rows.extend(subresources.iter().filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(|path| (format!("subresource/{key}"), path.to_string()))
            }));
        }
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        if !rows.is_empty() {
            lines.push("paths:".to_string());
            for (key, path) in rows {
                lines.push(format!("  - {key}: {path}"));
            }
        }
    }
    if let Some(commands) = value.get("exampleCommands").and_then(Value::as_array) {
        if !commands.is_empty() {
            lines.push("commands:".to_string());
            for command in commands {
                let purpose = command
                    .get("purpose")
                    .and_then(Value::as_str)
                    .unwrap_or("example");
                let text = command.get("command").and_then(Value::as_str).unwrap_or("");
                if !text.is_empty() {
                    lines.push(format!("  - {purpose}: {text}"));
                }
            }
        }
    }
    lines
}

fn runtime_explain_string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn print_runtime_inspect_summary(value: &Value) -> Result<()> {
    println!("{}", runtime_inspect_summary_lines(value).join("\n"));
    Ok(())
}

fn runtime_inspect_summary_lines(value: &Value) -> Vec<String> {
    let resources = value
        .pointer("/resource_inventory/resources")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let events = value
        .pointer("/event_list/events")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let api_resources = value
        .pointer("/api_resources/resources")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let log_refs = value
        .get("log_refs")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let metrics_resources = value
        .pointer("/resource_metrics/resources")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let mut lines = vec![format!(
        "inspect: resources={resources} api_resources={api_resources} events={events} metrics_resources={metrics_resources} log_refs={log_refs}"
    )];
    if let Some(filter) = runtime_inspect_filter_line(value) {
        lines.push(format!("filter: {filter}"));
    }
    if let Some(inventory) = value.get("resource_inventory") {
        for line in runtime_list_metadata_summary_lines(inventory) {
            lines.push(format!("inventory_{line}"));
        }
    }
    if let Some(health) = value
        .get("resource_health")
        .and_then(|health| health.get("summary"))
    {
        lines.push(format!(
            "health: {}",
            runtime_inspect_health_summary_line(health)
        ));
    }
    if let Some(metrics) = value
        .get("resource_metrics")
        .and_then(|metrics| metrics.get("summary"))
    {
        lines.push(format!(
            "metrics: {}",
            runtime_inspect_metrics_summary_line(metrics)
        ));
    }
    if let Some(event_list) = value.get("event_list") {
        for line in runtime_events_summary_lines(event_list)
            .into_iter()
            .filter(|line| !line.starts_with("  - "))
        {
            lines.push(format!("event_{line}"));
        }
    }
    if let Some(refs) = value.get("log_refs").and_then(Value::as_array) {
        for log_ref in refs.iter().take(10) {
            let resource = log_ref
                .get("resource")
                .map(runtime_resource_ref_line)
                .unwrap_or_else(|| "unknown".to_string());
            let streams = log_ref
                .get("streams")
                .and_then(Value::as_array)
                .map(|streams| {
                    streams
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|stream| !stream.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            lines.push(format!("log_ref: {resource} streams={}", streams.join(",")));
        }
        if refs.len() > 10 {
            lines.push(format!("log_ref: ... {} more", refs.len() - 10));
        }
    }
    lines
}

fn runtime_inspect_filter_line(value: &Value) -> Option<String> {
    let filter = value.get("resource_filter")?;
    let mut parts = Vec::new();
    for (label, key) in [("kinds", "kinds"), ("categories", "categories")] {
        let values = runtime_explain_string_array(filter, key);
        if !values.is_empty() {
            parts.push(format!("{label}={}", values.join(",")));
        }
    }
    for (label, pointer) in [
        ("label_selector", "/label_selector"),
        ("field_selector", "/field_selector"),
    ] {
        if let Some(value) = filter
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            parts.push(format!("{label}={value}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn runtime_inspect_health_summary_line(summary: &Value) -> String {
    runtime_inspect_summary_numbers(
        summary,
        &[
            ("total", "/total"),
            ("ready", "/ready"),
            ("degraded", "/degraded"),
            ("problem", "/problem"),
            ("unknown", "/unknown"),
            ("access_targets", "/access_targets"),
            ("reachable", "/reachable_access_targets"),
            ("actions", "/actions_available"),
            ("observed_stale", "/observed_stale"),
        ],
    )
}

fn runtime_inspect_metrics_summary_line(summary: &Value) -> String {
    runtime_inspect_summary_numbers(
        summary,
        &[
            ("resources_total", "/resources_total"),
            ("resources_returned", "/resources_returned"),
            ("metrics_total", "/metrics_total"),
            ("events_total", "/events_total"),
        ],
    )
}

fn runtime_inspect_summary_numbers(summary: &Value, fields: &[(&str, &str)]) -> String {
    let parts = fields
        .iter()
        .filter_map(|(label, pointer)| {
            summary
                .pointer(pointer)
                .and_then(Value::as_i64)
                .map(|value| format!("{label}={value}"))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        compact_json_lossy(summary)
    } else {
        parts.join(" ")
    }
}

fn print_runtime_resources_summary(value: &Value) -> Result<()> {
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("resources: {}", resources.len())];
    lines.extend(runtime_list_metadata_summary_lines(value));
    for resource in resources.iter().take(50) {
        lines.push(format!("  - {}", runtime_resource_line(resource)));
    }
    if resources.len() > 50 {
        lines.push(format!(
            "  ... {} more; rerun with --json for full output",
            resources.len() - 50
        ));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn print_runtime_resources_name_summary(value: &Value) -> Result<()> {
    println!("{}", runtime_resources_name_lines(value)?.join("\n"));
    Ok(())
}

fn runtime_resources_name_lines(value: &Value) -> Result<Vec<String>> {
    value
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|resource| {
            let (kind, name) = runtime_resource_kind_name(resource)?;
            Ok(format!("{kind}/{name}"))
        })
        .collect()
}

fn print_runtime_resources_wide_summary(value: &Value, api_resources: &Value) -> Result<()> {
    println!(
        "{}",
        runtime_resources_wide_lines(value, api_resources).join("\n")
    );
    Ok(())
}

fn runtime_resources_wide_lines(value: &Value, api_resources: &Value) -> Vec<String> {
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("resources: {}", resources.len())];
    lines.extend(runtime_list_metadata_summary_lines(value));
    let columns_by_kind = runtime_printer_columns_by_kind(api_resources);
    let mut resources_by_kind: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for resource in resources {
        let kind = resource
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        resources_by_kind.entry(kind).or_default().push(resource);
    }
    for (kind, kind_resources) in resources_by_kind {
        lines.push(String::new());
        lines.push(format!("{kind}: {}", kind_resources.len()));
        let columns = columns_by_kind
            .get(&kind)
            .cloned()
            .unwrap_or_else(runtime_default_printer_columns);
        let columns = runtime_visible_printer_columns(columns);
        let headers = columns
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        let rows = kind_resources
            .iter()
            .map(|resource| {
                columns
                    .iter()
                    .map(|column| runtime_printer_column_value(resource, &column.json_path))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        lines.extend(runtime_table_lines(&headers, &rows));
    }
    lines
}

fn runtime_printer_columns_by_kind(
    api_resources: &Value,
) -> BTreeMap<String, Vec<RuntimePrinterColumn>> {
    let mut out = BTreeMap::new();
    for resource in api_resources
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(kind) = resource.get("kind").and_then(Value::as_str) else {
            continue;
        };
        let columns = resource
            .get("printerColumns")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(runtime_printer_column_from_value)
            .collect::<Vec<_>>();
        if !columns.is_empty() {
            out.insert(kind.to_string(), columns);
        }
    }
    out
}

fn runtime_printer_column_from_value(value: &Value) -> Option<RuntimePrinterColumn> {
    let name = value.get("name")?.as_str()?.trim();
    let json_path = value.get("jsonPath")?.as_str()?.trim();
    if name.is_empty() || json_path.is_empty() {
        return None;
    }
    Some(RuntimePrinterColumn {
        name: name.to_string(),
        json_path: json_path.to_string(),
        priority: value.get("priority").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn runtime_default_printer_columns() -> Vec<RuntimePrinterColumn> {
    [
        ("Name", ".metadata.name", 0),
        ("Phase", ".status.phase", 0),
        (
            "Ready",
            r#".status.conditions[?(@.type=="Ready")].status"#,
            0,
        ),
        (
            "Observed",
            r#".status.conditions[?(@.type=="Observed")].status"#,
            0,
        ),
        ("Reason", ".status.reason", 1),
        ("Source", ".audit.source", 1),
    ]
    .into_iter()
    .map(|(name, json_path, priority)| RuntimePrinterColumn {
        name: name.to_string(),
        json_path: json_path.to_string(),
        priority,
    })
    .collect()
}

fn runtime_visible_printer_columns(
    columns: Vec<RuntimePrinterColumn>,
) -> Vec<RuntimePrinterColumn> {
    let mut seen = BTreeSet::new();
    let mut visible = columns
        .into_iter()
        .filter(|column| column.priority <= 1)
        .filter(|column| seen.insert(column.name.clone()))
        .collect::<Vec<_>>();
    if visible.is_empty() {
        visible = runtime_default_printer_columns();
    }
    visible
}

fn runtime_printer_column_value(resource: &Value, json_path: &str) -> String {
    if let Some(value) = runtime_condition_json_path_value(resource, json_path) {
        return runtime_cell_value(value);
    }
    let Some(value) = runtime_simple_json_path_value(resource, json_path) else {
        return "-".to_string();
    };
    if json_path == ".spec.command" {
        return runtime_command_cell_value(value);
    }
    runtime_cell_value(value)
}

fn runtime_command_cell_value(value: &Value) -> String {
    if let Some(command_parts) = value.as_array() {
        let parts = command_parts
            .iter()
            .filter_map(|part| part.as_str().map(ToString::to_string))
            .collect::<Vec<_>>();
        if !parts.is_empty() && parts.len() == command_parts.len() {
            return runtime_shell_command_parts_display(&parts);
        }
    }
    runtime_cell_value(value)
}

fn runtime_condition_json_path_value<'a>(
    resource: &'a Value,
    json_path: &str,
) -> Option<&'a Value> {
    let rest = json_path.strip_prefix(r#".status.conditions[?(@.type==""#)?;
    let (condition_type, suffix) = rest.split_once(r#"")]"#)?;
    let field = suffix.strip_prefix('.')?;
    resource
        .pointer("/status/conditions")
        .and_then(Value::as_array)?
        .iter()
        .find(|condition| {
            condition
                .get("type")
                .and_then(Value::as_str)
                .map(|value| value == condition_type)
                .unwrap_or(false)
        })
        .and_then(|condition| condition.get(field))
}

fn runtime_simple_json_path_value<'a>(resource: &'a Value, json_path: &str) -> Option<&'a Value> {
    let mut current = resource;
    let path = json_path.strip_prefix('.')?;
    if path.contains('[') {
        return None;
    }
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = current.get(segment)?;
    }
    Some(current)
}

fn runtime_cell_value(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => runtime_normalize_cell(value),
        Value::Array(values) => {
            let items = values
                .iter()
                .map(runtime_cell_value)
                .filter(|value| value != "-")
                .collect::<Vec<_>>();
            if items.is_empty() {
                "-".to_string()
            } else {
                runtime_normalize_cell(&items.join(","))
            }
        }
        Value::Object(_) => runtime_normalize_cell(&compact_json_lossy(value)),
    }
}

fn runtime_normalize_cell(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.trim().is_empty() {
        "-".to_string()
    } else {
        normalized
    }
}

fn runtime_table_lines(headers: &[String], rows: &[Vec<String>]) -> Vec<String> {
    if headers.is_empty() {
        return Vec::new();
    }
    let header_cells = headers
        .iter()
        .map(|header| runtime_table_cell(&runtime_table_header_label(header)))
        .collect::<Vec<_>>();
    let row_cells = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| runtime_table_cell(cell))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut widths = header_cells
        .iter()
        .map(|cell| cell.len())
        .collect::<Vec<_>>();
    for row in &row_cells {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.len());
            }
        }
    }
    let mut lines = Vec::new();
    lines.push(runtime_table_row_line(&header_cells, &widths));
    for row in row_cells {
        lines.push(runtime_table_row_line(&row, &widths));
    }
    lines
}

fn runtime_table_header_label(value: &str) -> String {
    let mut out = String::new();
    let chars = value.chars().collect::<Vec<_>>();
    for (index, ch) in chars.iter().copied().enumerate() {
        if index > 0 {
            let previous = chars[index - 1];
            let next = chars.get(index + 1).copied();
            let starts_word = ch.is_ascii_uppercase()
                && (previous.is_ascii_lowercase()
                    || previous.is_ascii_digit()
                    || next.map(|next| next.is_ascii_lowercase()).unwrap_or(false)
                        && previous.is_ascii_uppercase());
            if starts_word || ch == '_' || ch == '-' {
                out.push(' ');
            }
        }
        if ch != '_' && ch != '-' {
            out.push(ch.to_ascii_uppercase());
        }
    }
    runtime_normalize_cell(&out)
}

fn runtime_table_cell(value: &str) -> String {
    const MAX_CELL_CHARS: usize = 56;
    let value = runtime_normalize_cell(value);
    if value.chars().count() <= MAX_CELL_CHARS {
        return value;
    }
    let mut out = value
        .chars()
        .take(MAX_CELL_CHARS.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn runtime_table_row_line(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            let width = widths.get(index).copied().unwrap_or(cell.len());
            format!("{cell:<width$}")
        })
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_string()
}

fn print_runtime_resource_tree_summary(value: &Value) -> Result<()> {
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("resource_tree: {}", resources.len())];
    lines.extend(runtime_list_metadata_summary_lines(value));
    for (resource_index, depth) in runtime_resource_tree_rows(&resources) {
        let Some(resource) = resources.get(resource_index) else {
            continue;
        };
        let mut line = format!(
            "{}- {}",
            "  ".repeat(depth),
            runtime_resource_line(resource)
        );
        if depth == 0 {
            let owners = runtime_resource_owner_ref_lines(resource);
            if !owners.is_empty() {
                line.push_str(&format!(" owners={}", owners.join(",")));
            }
        }
        lines.push(line);
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

fn runtime_resource_tree_rows(resources: &[&Value]) -> Vec<(usize, usize)> {
    let mut sorted = (0..resources.len()).collect::<Vec<_>>();
    sorted.sort_by(|left, right| {
        runtime_resource_identity(resources[*left])
            .cmp(&runtime_resource_identity(resources[*right]))
    });
    let mut roots = Vec::new();
    let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for resource_index in sorted.iter().copied() {
        if let Some(parent_index) = runtime_resource_tree_parent_index(resources, resource_index) {
            children
                .entry(parent_index)
                .or_default()
                .push(resource_index);
        } else {
            roots.push(resource_index);
        }
    }
    let mut rows = Vec::new();
    let mut visited = vec![false; resources.len()];
    for root in roots {
        runtime_resource_tree_push_rows(root, 0, &children, &mut visited, &mut rows);
    }
    for resource_index in sorted {
        runtime_resource_tree_push_rows(resource_index, 0, &children, &mut visited, &mut rows);
    }
    rows
}

fn runtime_resource_tree_push_rows(
    resource_index: usize,
    depth: usize,
    children: &BTreeMap<usize, Vec<usize>>,
    visited: &mut [bool],
    rows: &mut Vec<(usize, usize)>,
) {
    if visited.get(resource_index).copied().unwrap_or(true) {
        return;
    }
    visited[resource_index] = true;
    rows.push((resource_index, depth));
    if let Some(child_indices) = children.get(&resource_index) {
        for child_index in child_indices {
            runtime_resource_tree_push_rows(*child_index, depth + 1, children, visited, rows);
        }
    }
}

fn runtime_resource_tree_parent_index(resources: &[&Value], child_index: usize) -> Option<usize> {
    let child = resources.get(child_index)?;
    for owner in child
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(parent_index) =
            resources
                .iter()
                .enumerate()
                .find_map(|(resource_index, resource)| {
                    (resource_index != child_index
                        && runtime_resource_matches_owner_ref(resource, owner))
                    .then_some(resource_index)
                })
        {
            return Some(parent_index);
        }
    }
    None
}

fn runtime_resource_matches_owner_ref(resource: &Value, owner: &Value) -> bool {
    let resource_kind = resource.get("kind").and_then(Value::as_str).unwrap_or("");
    let owner_kind = owner.get("kind").and_then(Value::as_str).unwrap_or("");
    if resource_kind != owner_kind {
        return false;
    }
    let resource_uid = resource
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .unwrap_or("");
    let owner_uid = owner.get("uid").and_then(Value::as_str).unwrap_or("");
    if !resource_uid.is_empty() && !owner_uid.is_empty() {
        return resource_uid == owner_uid;
    }
    resource
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("")
        == owner.get("name").and_then(Value::as_str).unwrap_or("")
}

fn runtime_resource_identity(resource: &Value) -> String {
    format!(
        "{}/{}",
        resource
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        resource
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    )
}

fn runtime_resource_owner_ref_lines(resource: &Value) -> Vec<String> {
    resource
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .map(|owners| {
            owners
                .iter()
                .filter_map(|owner| {
                    let kind = owner.get("kind").and_then(Value::as_str)?.trim();
                    let name = owner.get("name").and_then(Value::as_str)?.trim();
                    (!kind.is_empty() && !name.is_empty()).then(|| format!("{kind}/{name}"))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn print_runtime_resource_summary(value: &Value) -> Result<()> {
    println!("{}", runtime_resource_summary_lines(value).join("\n"));
    Ok(())
}

fn runtime_resource_summary_lines(value: &Value) -> Vec<String> {
    let resource = value.get("resource").unwrap_or(value);
    let mut lines = vec![format!("resource: {}", runtime_resource_line(resource))];
    if let Some(generated_at) = value
        .get("generated_at")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|generated_at| !generated_at.is_empty())
    {
        lines.push(format!("generated_at: {generated_at}"));
    }
    let core_run_ids = runtime_explain_string_array(value, "core_run_ids");
    if !core_run_ids.is_empty() {
        lines.push(format!("core_run_ids: {}", core_run_ids.join(",")));
    }
    lines.extend(runtime_resource_metadata_summary_lines(resource));
    lines.extend(runtime_resource_condition_summary_lines(resource));
    lines.extend(runtime_access_detail_lines(
        resource,
        runtime_resource_summary_run_id(value),
    ));
    lines.extend(runtime_related_resource_summary_lines(value));
    if let Some(operations) = value.get("operations").and_then(Value::as_array) {
        let supported = operations
            .iter()
            .filter(|operation| operation.get("supported").and_then(Value::as_bool) == Some(true))
            .filter_map(|operation| operation.get("purpose").and_then(Value::as_str))
            .collect::<Vec<_>>();
        if !supported.is_empty() {
            lines.push(format!("operations: {}", supported.join(",")));
        }
        lines.extend(runtime_resource_operation_summary_lines(operations));
    }
    if let Some(event_list) = value.get("event_list") {
        lines.extend(runtime_resource_event_summary_lines(event_list));
    }
    lines
}

fn runtime_resource_event_summary_lines(event_list: &Value) -> Vec<String> {
    let events = event_list
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("events: {}", events.len())];
    for line in runtime_events_summary_lines(event_list)
        .into_iter()
        .skip(1)
        .filter(|line| !line.starts_with("  - "))
    {
        lines.push(format!("event_{line}"));
    }
    let visible_events = events
        .iter()
        .filter(|event| runtime_event_has_type(event))
        .take(10)
        .map(|event| {
            format!(
                "event: {}",
                runtime_event_summary_line(event).trim_start_matches("  - ")
            )
        })
        .collect::<Vec<_>>();
    lines.extend(visible_events);
    let typed_event_count = events
        .iter()
        .filter(|event| runtime_event_has_type(event))
        .count();
    if typed_event_count > 10 {
        lines.push(format!(
            "event: ... {} more; run buc runs events for full output",
            typed_event_count - 10
        ));
    }
    lines
}

fn runtime_event_has_type(event: &Value) -> bool {
    event
        .get("event_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(|event_type| !event_type.is_empty())
        .unwrap_or(false)
}

fn runtime_resource_operation_summary_lines(operations: &[Value]) -> Vec<String> {
    operations
        .iter()
        .filter_map(runtime_resource_operation_summary_line)
        .collect()
}

fn runtime_resource_operation_summary_line(operation: &Value) -> Option<String> {
    let purpose = operation
        .get("purpose")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|purpose| !purpose.is_empty())?;
    let supported = operation
        .get("supported")
        .and_then(Value::as_bool)
        .map(|value| if value { "yes" } else { "no" })
        .unwrap_or("unknown");
    let mut parts = vec![format!("operation: {purpose} supported={supported}")];
    for key in ["verb", "subresource", "action", "reason", "message"] {
        if let Some(value) = operation
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            parts.push(format!("{key}={}", runtime_shell_quote(value)));
        }
    }
    if let Some(requires_running_run) = operation
        .get("requires_running_run")
        .and_then(Value::as_bool)
    {
        parts.push(format!("requires_running_run={requires_running_run}"));
    }
    if let Some(command) = operation
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
    {
        parts.push(format!("command={}", runtime_shell_quote(command)));
    }
    Some(parts.join(" "))
}

fn runtime_related_resource_summary_lines(value: &Value) -> Vec<String> {
    value
        .get("related_resources")
        .and_then(Value::as_array)
        .map(|related_resources| {
            related_resources
                .iter()
                .filter_map(|related| {
                    let relationship = related
                        .get("relationship")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|relationship| !relationship.is_empty())
                        .unwrap_or("related");
                    let resource = related.get("resource")?;
                    let mut line = format!(
                        "related: {relationship} {}",
                        runtime_resource_line(resource)
                    );
                    if let Some(resource_version) = resource
                        .pointer("/metadata/resourceVersion")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|resource_version| !resource_version.is_empty())
                    {
                        line.push_str(&format!(" resource_version={resource_version}"));
                    }
                    Some(line)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn runtime_resource_summary_run_id(value: &Value) -> Option<&str> {
    value
        .get("cloud_run_id")
        .or_else(|| value.get("run_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
}

fn runtime_resource_metadata_summary_lines(resource: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(uid) = resource
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|uid| !uid.is_empty())
    {
        lines.push(format!("uid: {uid}"));
    }
    if let Some(resource_version) = resource
        .pointer("/metadata/resourceVersion")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|resource_version| !resource_version.is_empty())
    {
        lines.push(format!("resource_version: {resource_version}"));
    }
    let generation = resource
        .pointer("/metadata/generation")
        .and_then(Value::as_i64);
    let observed_generation = resource
        .pointer("/status/observedGeneration")
        .and_then(Value::as_i64);
    match (generation, observed_generation) {
        (Some(generation), Some(observed_generation)) => {
            let freshness = if generation == observed_generation {
                "current"
            } else {
                "stale"
            };
            lines.push(format!(
                "generation: {generation} observed={observed_generation} freshness={freshness}"
            ));
        }
        (Some(generation), None) => lines.push(format!("generation: {generation}")),
        (None, Some(observed_generation)) => {
            lines.push(format!("observed_generation: {observed_generation}"));
        }
        (None, None) => {}
    }
    let owners = runtime_resource_owner_ref_lines(resource);
    if !owners.is_empty() {
        lines.push(format!("owners: {}", owners.join(",")));
    }
    lines
}

fn runtime_resource_condition_summary_lines(resource: &Value) -> Vec<String> {
    runtime_condition_summary_lines(
        resource
            .pointer("/status/conditions")
            .and_then(Value::as_array),
    )
}

fn runtime_condition_summary_lines(conditions: Option<&Vec<Value>>) -> Vec<String> {
    conditions
        .map(|conditions| {
            conditions
                .iter()
                .filter_map(|condition| {
                    let condition_type = condition.get("type").and_then(Value::as_str)?.trim();
                    let status = condition
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown")
                        .trim();
                    if condition_type.is_empty() {
                        return None;
                    }
                    let mut line = format!("condition: {condition_type}={status}");
                    if let Some(reason) = condition
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|reason| !reason.is_empty())
                    {
                        line.push_str(&format!(" reason={reason}"));
                    }
                    if let Some(message) = condition
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|message| !message.is_empty())
                    {
                        line.push_str(&format!(" message={message}"));
                    }
                    Some(line)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn runtime_access_detail_lines(resource: &Value, run_id: Option<&str>) -> Vec<String> {
    let kind = resource
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if kind != "PortForward" && kind != "Exec" {
        return Vec::new();
    }
    let mut lines = Vec::new();
    if let Some(target) = runtime_access_target_line(resource) {
        lines.push(format!("target: {target}"));
    }
    if let Some(request) = runtime_access_request_line(resource) {
        lines.push(format!("request: {request}"));
    }
    if let Some(binding) = runtime_access_runner_binding_line(resource) {
        lines.push(format!("runner_binding: {binding}"));
    }
    if let Some(connection_mode) = runtime_connection_string(resource, "mode")
        .or_else(|| runtime_connection_string(resource, "kind"))
    {
        lines.push(format!("connection_mode: {connection_mode}"));
    }
    if let Some(connection) = resource.pointer("/status/connection") {
        if !connection.is_null() {
            lines.push(format!("connection: {}", compact_json_lossy(connection)));
        }
    }
    if kind == "PortForward" {
        if let Some(target_port) = resource
            .pointer("/spec/target_port")
            .and_then(Value::as_u64)
        {
            lines.push(format!("target_port: {target_port}"));
        }
        if let Some(local_port) = resource
            .pointer("/status/connection/local_port")
            .and_then(Value::as_u64)
            .or_else(|| resource.pointer("/spec/local_port").and_then(Value::as_u64))
        {
            lines.push(format!("local_port: {local_port}"));
        }
        if let Some(endpoint) = runtime_connection_string(resource, "client_endpoint")
            .or_else(|| runtime_connection_string(resource, "client_listen"))
        {
            lines.push(format!("client_endpoint: {endpoint}"));
        }
        if let Some(tunnel) = runtime_connection_string(resource, "tunnel") {
            lines.push(format!("tunnel: {tunnel}"));
        }
        if let Some(provider_tunnel_url) =
            runtime_connection_string(resource, "provider_tunnel_url")
                .or_else(|| runtime_connection_string(resource, "client_url"))
        {
            lines.push(format!("provider_tunnel_url: {provider_tunnel_url}"));
        }
        if let Some(attach_command) = runtime_port_forward_attach_command_line(resource) {
            lines.push(format!("attach_command: {attach_command}"));
        }
    }
    if kind == "Exec" {
        if let Some(command) = runtime_exec_command_line(resource) {
            lines.push(format!("command: {command}"));
        }
        if let Some(exit_code) = resource
            .pointer("/status/connection/exit_code")
            .and_then(Value::as_i64)
        {
            lines.push(format!("exit_code: {exit_code}"));
        }
        if let Some(stdout) = runtime_connection_string(resource, "stdout_tail") {
            lines.push(format!("stdout_tail:\n{stdout}"));
        } else if let Some(stdout) = runtime_connection_string(resource, "stdout") {
            lines.push(format!("stdout:\n{stdout}"));
        }
        if let Some(stdout_evidence) = runtime_exec_stream_evidence_line(resource, "stdout") {
            lines.push(format!("stdout_evidence: {stdout_evidence}"));
        }
        if let Some(stderr) = runtime_connection_string(resource, "stderr_tail") {
            lines.push(format!("stderr_tail:\n{stderr}"));
        } else if let Some(stderr) = runtime_connection_string(resource, "stderr") {
            lines.push(format!("stderr:\n{stderr}"));
        }
        if let Some(stderr_evidence) = runtime_exec_stream_evidence_line(resource, "stderr") {
            lines.push(format!("stderr_evidence: {stderr_evidence}"));
        }
    }
    if let Some(cleanup_command) = runtime_access_cleanup_command_line(resource, run_id) {
        lines.push(format!("cleanup_command: {cleanup_command}"));
    }
    lines
}

fn runtime_exec_stream_evidence_line(resource: &Value, stream: &str) -> Option<String> {
    let bytes = resource
        .pointer(&format!("/status/connection/{stream}_bytes"))
        .and_then(Value::as_u64);
    let tail_bytes = resource
        .pointer(&format!("/status/connection/{stream}_tail_bytes"))
        .and_then(Value::as_u64);
    let truncated = resource
        .pointer(&format!("/status/connection/{stream}_tail_truncated"))
        .and_then(Value::as_bool);
    if bytes.is_none() && tail_bytes.is_none() && truncated.is_none() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(bytes) = bytes {
        parts.push(format!("bytes={bytes}"));
    }
    if let Some(tail_bytes) = tail_bytes {
        parts.push(format!("tail_bytes={tail_bytes}"));
    }
    if let Some(truncated) = truncated {
        parts.push(format!("truncated={truncated}"));
    }
    Some(parts.join(" "))
}

fn runtime_access_cleanup_command_line(resource: &Value, run_id: Option<&str>) -> Option<String> {
    let phase = runtime_resource_phase(resource);
    if matches!(
        phase.as_deref(),
        Some("completed" | "failed" | "expired" | "cancelled")
    ) {
        return None;
    }
    let (kind, name) = runtime_resource_kind_name(resource).ok()?;
    if kind != "PortForward" && kind != "Exec" {
        return None;
    }
    let run_id = run_id.map(str::trim).filter(|run_id| !run_id.is_empty())?;
    let resource_version = runtime_resource_metadata_resource_version(resource).ok()?;
    let command = if kind == "PortForward" && phase.as_deref() == Some("active") {
        "complete"
    } else {
        "delete"
    };
    Some(runtime_shell_command_parts_display(&[
        "buc".to_string(),
        "runs".to_string(),
        command.to_string(),
        run_id.to_string(),
        format!("{kind}/{name}"),
        "--reason".to_string(),
        "cleanup".to_string(),
        "--resource-version".to_string(),
        resource_version,
    ]))
}

fn runtime_access_target_line(resource: &Value) -> Option<String> {
    let target_ref = resource
        .pointer("/spec/target_ref")
        .or_else(|| resource.pointer("/audit/target_ref"))?;
    let kind = target_ref.get("kind").and_then(Value::as_str)?.trim();
    let name = target_ref.get("name").and_then(Value::as_str)?.trim();
    if kind.is_empty() || name.is_empty() {
        return None;
    }
    let mut parts = vec![format!("{kind}/{name}")];
    if let Some(uid) = target_ref
        .get("uid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("uid={uid}"));
    }
    if let Some(resource_version) = target_ref
        .get("resourceVersion")
        .and_then(Value::as_str)
        .or_else(|| {
            resource
                .pointer("/audit/target_resource_version")
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("resource_version={resource_version}"));
    }
    Some(parts.join(" "))
}

fn runtime_access_request_line(resource: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(requester) = runtime_resource_string(resource, "/audit/requester") {
        parts.push(format!("requester={requester}"));
    }
    if let Some(reason) = runtime_resource_string(resource, "/spec/reason") {
        parts.push(format!("reason={reason}"));
    }
    if let Some(expires_at) = runtime_resource_string(resource, "/status/expires_at") {
        parts.push(format!("expires_at={expires_at}"));
    }
    if let Some(source) = runtime_resource_string(resource, "/audit/source") {
        parts.push(format!("source={source}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn runtime_access_runner_binding_line(resource: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(runner) = runtime_access_runner_binding_string(resource, "runner_instance_id") {
        parts.push(format!("runner={runner}"));
    }
    if let Some(attempt) = runtime_access_runner_binding_string(resource, "attempt_id") {
        parts.push(format!("attempt={attempt}"));
    }
    if let Some(worker) = runtime_access_runner_binding_string(resource, "worker_id") {
        parts.push(format!("worker={worker}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn runtime_access_runner_binding_string<'a>(resource: &'a Value, key: &str) -> Option<&'a str> {
    resource
        .pointer(&format!("/status/runner_binding/{key}"))
        .and_then(Value::as_str)
        .or_else(|| {
            resource
                .pointer(&format!("/audit/runner_binding/{key}"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            resource
                .pointer(&format!("/status/{key}"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn runtime_port_forward_attach_command_line(resource: &Value) -> Option<String> {
    let fallback_target_port = resource
        .pointer("/status/connection/target_port")
        .and_then(Value::as_u64)
        .or_else(|| {
            resource
                .pointer("/spec/target_port")
                .and_then(Value::as_u64)
        })?;
    let plan = runtime_port_forward_attach_plan(resource, None, fallback_target_port).ok()?;
    match plan {
        RuntimePortForwardAttachPlan::GceIap(spec) => {
            let args = gcloud_iap_port_forward_args(&spec);
            Some(runtime_shell_command_display("gcloud", &args))
        }
        RuntimePortForwardAttachPlan::ClientEndpoint(_) => None,
    }
}

fn runtime_exec_command_line(resource: &Value) -> Option<String> {
    let command = resource.pointer("/spec/command")?;
    if let Some(command_parts) = command.as_array() {
        let parts = command_parts
            .iter()
            .filter_map(|part| part.as_str().map(ToString::to_string))
            .collect::<Vec<_>>();
        if !parts.is_empty() && parts.len() == command_parts.len() {
            return Some(runtime_shell_command_parts_display(&parts));
        }
    }
    command
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| Some(compact_json_lossy(command)).filter(|value| value != "null"))
}

fn runtime_shell_command_display(program: &str, args: &[String]) -> String {
    let mut parts = vec![program.to_string()];
    parts.extend(args.iter().cloned());
    runtime_shell_command_parts_display(&parts)
}

fn runtime_shell_command_parts_display(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| runtime_shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn runtime_shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "-_./:=,@%".contains(ch))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn runtime_resource_string<'a>(resource: &'a Value, pointer: &str) -> Option<&'a str> {
    resource
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn runtime_connection_string<'a>(resource: &'a Value, key: &str) -> Option<&'a str> {
    resource
        .pointer(&format!("/status/connection/{key}"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn print_runtime_resource_status_summary(value: &Value) -> Result<()> {
    println!(
        "{}",
        runtime_resource_status_summary_lines(value).join("\n")
    );
    Ok(())
}

fn runtime_resource_status_summary_lines(value: &Value) -> Vec<String> {
    let resource = value
        .get("resource_ref")
        .map(runtime_resource_ref_line)
        .unwrap_or_else(|| "unknown".to_string());
    let phase = value
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let reason = value.get("reason").and_then(Value::as_str).unwrap_or("");
    let mut lines = vec![format!("status: {resource} phase={phase} reason={reason}")];
    if let Some(message) = value
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        lines.push(format!("message: {message}"));
    }
    if let Some(resource_version) = value
        .get("resourceVersion")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|resource_version| !resource_version.is_empty())
    {
        lines.push(format!("resource_version: {resource_version}"));
    }
    if let Some(generated_at) = value
        .get("generated_at")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|generated_at| !generated_at.is_empty())
    {
        lines.push(format!("generated_at: {generated_at}"));
    }
    let generation = value.get("generation").and_then(Value::as_i64);
    let observed_generation = value.get("observedGeneration").and_then(Value::as_i64);
    match (generation, observed_generation) {
        (Some(generation), Some(observed_generation)) => {
            let freshness = if generation == observed_generation {
                "current"
            } else {
                "stale"
            };
            lines.push(format!(
                "generation: {generation} observed={observed_generation} freshness={freshness}"
            ));
        }
        (Some(generation), None) => lines.push(format!("generation: {generation}")),
        (None, Some(observed_generation)) => {
            lines.push(format!("observed_generation: {observed_generation}"));
        }
        (None, None) => {}
    }
    if let Some(deletion_timestamp) = value
        .get("deletionTimestamp")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|timestamp| !timestamp.is_empty())
    {
        lines.push(format!("deletion_timestamp: {deletion_timestamp}"));
    }
    lines.extend(runtime_condition_summary_lines(
        value.get("conditions").and_then(Value::as_array),
    ));
    if let Some(actions) = value.get("actions").and_then(Value::as_array) {
        let actions = actions
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|action| !action.is_empty())
            .collect::<Vec<_>>();
        lines.push(format!(
            "actions: {}",
            if actions.is_empty() {
                "none".to_string()
            } else {
                actions.join(",")
            }
        ));
    }
    if let Some(audit_source) = value
        .pointer("/audit/source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|source| !source.is_empty())
    {
        lines.push(format!("audit_source: {audit_source}"));
    }
    lines
}

fn print_runtime_operation_review_summary(
    value: &Value,
    fallback_operation: &str,
    fallback_kind: &str,
    fallback_name: &str,
) -> Result<()> {
    println!(
        "{}",
        runtime_operation_review_summary(value, fallback_operation, fallback_kind, fallback_name)
            .join("\n")
    );
    Ok(())
}

fn runtime_operation_review_summary(
    value: &Value,
    fallback_operation: &str,
    fallback_kind: &str,
    fallback_name: &str,
) -> Vec<String> {
    let operation = value
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or(fallback_operation);
    let resource = value
        .get("resource_ref")
        .map(runtime_resource_ref_line)
        .unwrap_or_else(|| format!("{fallback_kind}/{fallback_name}"));
    let supported = value
        .get("supported")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reason = value.get("reason").and_then(Value::as_str).unwrap_or("");
    let mut lines = vec![format!(
        "can-i: {} {} {}{}",
        if supported { "yes" } else { "no" },
        operation,
        resource,
        if reason.is_empty() {
            String::new()
        } else {
            format!(" reason={reason}")
        }
    )];
    if let Some(command) = value
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
    {
        lines.push(format!(
            "command: {}",
            runtime_review_command_with_resource_version(value, operation, command)
        ));
    }
    let mut review_parts = Vec::new();
    if let Some(matched_operation) = value
        .get("matched_operation")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|matched_operation| {
            !matched_operation.is_empty() && *matched_operation != operation
        })
    {
        review_parts.push(format!("matched={matched_operation}"));
    }
    if let Some(verb) = value
        .get("verb")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|verb| !verb.is_empty())
    {
        review_parts.push(format!("verb={verb}"));
    }
    if let Some(subresource) = value
        .get("subresource")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|subresource| !subresource.is_empty())
    {
        review_parts.push(format!("subresource={subresource}"));
    }
    if let Some(action) = value
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|action| !action.is_empty())
    {
        review_parts.push(format!("action={action}"));
    }
    if let Some(requires_running_run) = value.get("requires_running_run").and_then(Value::as_bool) {
        review_parts.push(format!("requires_running_run={requires_running_run}"));
    }
    let generation = value.get("resource_generation").and_then(Value::as_i64);
    let observed_generation = value.get("observed_generation").and_then(Value::as_i64);
    match (generation, observed_generation) {
        (Some(generation), Some(observed_generation)) => {
            review_parts.push(format!("generation={generation}/{observed_generation}"));
        }
        (Some(generation), None) => review_parts.push(format!("generation={generation}")),
        (None, Some(observed_generation)) => {
            review_parts.push(format!("observed_generation={observed_generation}"));
        }
        (None, None) => {}
    }
    if !review_parts.is_empty() {
        lines.push(format!("review: {}", review_parts.join(" ")));
    }
    if let Some(generated_at) = value
        .get("generated_at")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|generated_at| !generated_at.is_empty())
    {
        lines.push(format!("generated_at: {generated_at}"));
    }
    let core_run_ids = runtime_explain_string_array(value, "core_run_ids");
    if !core_run_ids.is_empty() {
        lines.push(format!("core_run_ids: {}", core_run_ids.join(",")));
    }
    if let Some(resource_version) = value
        .get("resource_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|resource_version| !resource_version.is_empty())
    {
        lines.push(format!("resource_version: {resource_version}"));
    }
    lines
}

fn runtime_review_command_with_resource_version(
    value: &Value,
    operation: &str,
    command: &str,
) -> String {
    let Some(resource_version) = value
        .get("resource_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|resource_version| !resource_version.is_empty())
    else {
        return command.to_string();
    };
    if !runtime_operation_accepts_resource_version(operation) {
        return command.to_string();
    }
    let trimmed = command.trim();
    let version_placeholder = "--resource-version <metadata.resourceVersion>";
    if trimmed.contains(version_placeholder) {
        return trimmed.replace(
            version_placeholder,
            &format!(
                "--resource-version {}",
                runtime_shell_quote(resource_version)
            ),
        );
    }
    if trimmed
        .split_whitespace()
        .any(|part| part == "--resource-version")
    {
        return command.to_string();
    }
    let resource_version_arg = format!(
        "--resource-version {}",
        runtime_shell_quote(resource_version)
    );
    if let Some(command_separator) = trimmed.find(" -- ") {
        return format!(
            "{} {}{}",
            &trimmed[..command_separator],
            resource_version_arg,
            &trimmed[command_separator..]
        );
    }
    format!("{trimmed} {resource_version_arg}")
}

fn runtime_operation_accepts_resource_version(operation: &str) -> bool {
    matches!(
        operation.trim().to_ascii_lowercase().as_str(),
        "port-forward"
            | "exec"
            | "delete"
            | "cancel"
            | "complete"
            | "cordon"
            | "drain"
            | "uncordon"
    )
}

fn print_runtime_health_summary(value: &Value) -> Result<()> {
    let summary = value.get("summary").cloned().unwrap_or(Value::Null);
    println!("health: {}", compact_json_lossy(&summary));
    if let Some(resources) = value.get("resources").and_then(Value::as_array) {
        for resource in resources.iter().take(20) {
            let name = resource
                .get("resource")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let health = resource
                .get("health")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let phase = resource.get("phase").and_then(Value::as_str).unwrap_or("");
            let message = resource
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("");
            println!("  - {name} health={health} phase={phase} {message}");
        }
    }
    Ok(())
}

fn print_runtime_metrics_summary(value: &Value) -> Result<()> {
    println!("{}", runtime_metrics_summary_lines(value).join("\n"));
    Ok(())
}

fn runtime_metrics_summary_lines(value: &Value) -> Vec<String> {
    if let Some(resources) = value.get("resources").and_then(Value::as_array) {
        let summary = value.get("summary").cloned().unwrap_or(Value::Null);
        let mut lines = vec![format!(
            "metrics: resources={} summary={}",
            resources.len(),
            compact_json_lossy(&summary)
        )];
        lines.extend(runtime_list_metadata_summary_lines(value));
        for resource in resources.iter().take(20) {
            let name = resource
                .get("resource_ref")
                .map(runtime_resource_ref_line)
                .unwrap_or_else(|| "unknown".to_string());
            let metrics = resource
                .get("metrics")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            lines.push(format!("  - {name} metrics={metrics}"));
        }
        lines
    } else {
        let name = value
            .get("resource_ref")
            .map(runtime_resource_ref_line)
            .unwrap_or_else(|| "unknown".to_string());
        let metrics = value
            .get("metrics")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        vec![format!("metrics: {name} metrics={metrics}")]
    }
}

fn runtime_list_metadata_summary_lines(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(resource_version) = value
        .pointer("/metadata/resourceVersion")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("resource_version: {resource_version}"));
    }
    if let Some(continue_token) = value
        .pointer("/metadata/continue")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("continue: {continue_token}"));
    }
    for (label, pointer) in [
        ("remaining", "/metadata/remainingItemCount"),
        ("total", "/metadata/total"),
        ("returned", "/metadata/returned"),
        ("limit", "/metadata/limit"),
    ] {
        if let Some(value) = value.pointer(pointer).and_then(Value::as_i64) {
            lines.push(format!("{label}: {value}"));
        }
    }
    lines
}

fn print_runtime_watch_summary(value: &Value) -> Result<()> {
    println!("{}", runtime_watch_summary_lines(value).join("\n"));
    Ok(())
}

fn runtime_watch_summary_lines(value: &Value) -> Vec<String> {
    let events = value
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("watch_events: {}", events.len())];
    if let Some(inventory) = value.get("resource_inventory") {
        lines.extend(runtime_list_metadata_summary_lines(inventory));
        let inventory_resources = inventory
            .get("resources")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        lines.push(format!("inventory_resources: {inventory_resources}"));
    }
    let known_resources = runtime_watch_known_resource_lines(value);
    for known_resource in known_resources.iter().take(50) {
        lines.push(format!("known_resource: {known_resource}"));
    }
    if known_resources.len() > 50 {
        lines.push(format!(
            "known_resource: ... {} more; rerun with --json for full output",
            known_resources.len() - 50
        ));
    }
    for event in events.iter().take(50) {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN");
        let resource = event
            .get("resource_ref")
            .map(runtime_resource_ref_line)
            .unwrap_or_else(|| "unknown".to_string());
        let mut line = format!("  - {event_type} {resource}");
        if let Some(resource_version) = event
            .get("resource_version")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            line.push_str(&format!(" rv={resource_version}"));
        }
        if let Some(previous_resource_version) = event
            .get("previous_resource_version")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            line.push_str(&format!(" previous={previous_resource_version}"));
        }
        lines.push(line);
    }
    if events.len() > 50 {
        lines.push(format!(
            "  ... {} more; rerun with --json for full output",
            events.len() - 50
        ));
    }
    lines
}

fn runtime_watch_known_resource_lines(value: &Value) -> Vec<String> {
    let mut lines = value
        .get("resource_versions")
        .and_then(Value::as_object)
        .map(|versions| {
            versions
                .iter()
                .filter_map(|(key, value)| {
                    let key = key.trim();
                    let version = value.as_str()?.trim();
                    (!key.is_empty() && !version.is_empty()).then(|| format!("{key}={version}"))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    lines.sort();
    lines
}

fn print_runtime_events_summary(value: &Value) -> Result<()> {
    println!("{}", runtime_events_summary_lines(value).join("\n"));
    Ok(())
}

fn runtime_events_summary_lines(value: &Value) -> Vec<String> {
    let events = value
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut lines = vec![format!("events: {}", events.len())];
    if let Some(resource_version) = value
        .pointer("/metadata/resourceVersion")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("resource_version: {resource_version}"));
    }
    if let Some(continue_token) = value
        .pointer("/metadata/continue")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("continue: {continue_token}"));
    }
    for (label, pointer) in [
        ("after_row_seq", "/metadata/after_row_seq"),
        ("next_after_row_seq", "/metadata/next_after_row_seq"),
        ("remaining", "/metadata/remainingItemCount"),
        ("limit", "/metadata/limit"),
        ("returned", "/metadata/returned"),
    ] {
        if let Some(value) = value.pointer(pointer).and_then(Value::as_i64) {
            lines.push(format!("{label}: {value}"));
        }
    }
    for event in events.iter().take(20) {
        lines.push(runtime_event_summary_line(event));
    }
    if events.len() > 20 {
        lines.push(format!(
            "  ... {} more; rerun with --json for full output",
            events.len() - 20
        ));
    }
    lines
}

fn runtime_event_summary_line(event: &Value) -> String {
    let event_type = event
        .get("event_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let payload = event.get("payload").unwrap_or(&Value::Null);
    let mut parts = Vec::new();
    if let Some(row_seq) = event.get("row_seq").and_then(Value::as_i64) {
        parts.push(format!("row={row_seq}"));
    }
    parts.push(event_type.to_string());
    if let Some(source) = event
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("source={source}"));
    }
    if let Some(actor) = runtime_event_payload_string(payload, "requester")
        .or_else(|| runtime_event_payload_string(payload, "requested_by"))
    {
        parts.push(format!("actor={actor}"));
    }
    let mut emitted_resource_refs = Vec::new();
    if let Some(resource) = runtime_event_top_level_resource_ref_identity(event)
        .or_else(|| runtime_event_ref_identity(payload, "resource_ref"))
        .or_else(|| runtime_event_payload_resource_identity(payload))
    {
        parts.push(format!("resource={resource}"));
        emitted_resource_refs.push(resource);
    }
    if let Some(access) = runtime_event_ref_identity(payload, "access_resource_ref") {
        if !runtime_event_ref_identity_seen(&emitted_resource_refs, &access) {
            parts.push(format!("access={access}"));
            emitted_resource_refs.push(access);
        }
    }
    if let Some(target) = runtime_event_ref_identity(payload, "resolved_target")
        .or_else(|| runtime_event_ref_identity(payload, "target_ref"))
    {
        if !runtime_event_ref_identity_seen(&emitted_resource_refs, &target) {
            parts.push(format!("target={target}"));
            emitted_resource_refs.push(target);
        }
    }
    if let Some(involved) =
        runtime_event_top_level_unemitted_refs_summary(event, &emitted_resource_refs)
    {
        parts.push(format!("involved={involved}"));
    }
    if let Some(runner) =
        runtime_event_payload_string(payload, "resolved_target.runner_binding.runner_instance_id")
            .or_else(|| runtime_event_payload_string(payload, "resolved_target.runner_instance_id"))
            .or_else(|| runtime_event_payload_string(payload, "runner_binding.runner_instance_id"))
    {
        parts.push(format!("runner={runner}"));
    }
    if let Some(worker) =
        runtime_event_payload_string(payload, "resolved_target.runner_binding.worker_id")
            .or_else(|| runtime_event_payload_string(payload, "resolved_target.worker_id"))
            .or_else(|| runtime_event_payload_string(payload, "runner_binding.worker_id"))
    {
        parts.push(format!("worker={worker}"));
    }
    if let Some(reviewed_version) =
        runtime_event_payload_string(payload, "resource_version_precondition")
    {
        parts.push(format!("reviewed-rv={reviewed_version}"));
    }
    if event_type.starts_with("runtime.resource.operation.review") {
        if let Some(operation) = runtime_event_payload_string(payload, "operation") {
            parts.push(format!("operation={operation}"));
        }
        if let Some(matched_operation) = runtime_event_payload_string(payload, "matched_operation")
        {
            parts.push(format!("matched={matched_operation}"));
        }
    }
    if runtime_event_is_api_resources_read(event_type) {
        if let Some(operation) = runtime_event_payload_string(payload, "operation") {
            parts.push(format!("operation={operation}"));
        }
        if let Some(selected_kind) = runtime_event_payload_string(payload, "selected_kind") {
            parts.push(format!("selected={selected_kind}"));
        }
        if let Some(api_resources_returned) =
            runtime_event_payload_i64(payload, "api_resources_returned")
        {
            parts.push(format!("api_resources={api_resources_returned}"));
        }
        if let Some(api_resource_kind) = runtime_event_payload_string(payload, "api_resource_kind")
        {
            parts.push(format!("api_kind={api_resource_kind}"));
        }
        if let Some(api_resource_name) = runtime_event_payload_string(payload, "api_resource_name")
        {
            parts.push(format!("api_name={api_resource_name}"));
        }
        if let Some(api_resource_count) = runtime_event_payload_i64(payload, "api_resource_count") {
            parts.push(format!("count={api_resource_count}"));
        }
        if let Some(categories) =
            runtime_event_payload_string_array(payload, "api_resource_categories")
        {
            parts.push(format!("categories={categories}"));
        }
        if let Some(verbs) = runtime_event_payload_string_array(payload, "api_resource_verbs") {
            parts.push(format!("verbs={verbs}"));
        }
        if let Some(subresources) =
            runtime_event_payload_string_array(payload, "api_resource_subresources")
        {
            parts.push(format!("subresources={subresources}"));
        }
        if let Some(actions) = runtime_event_payload_string_array(payload, "api_resource_actions") {
            parts.push(format!("actions={actions}"));
        }
        if let Some(access) = runtime_event_payload_string_array(payload, "api_resource_access") {
            parts.push(format!("access={access}"));
        }
        if let Some(core_run_ids) = runtime_event_payload_string_array(payload, "core_run_ids") {
            parts.push(format!("core_runs={core_run_ids}"));
        }
    }
    if runtime_event_is_resource_query_read(event_type) {
        if let Some(operation) = runtime_event_payload_string(payload, "operation") {
            parts.push(format!("operation={operation}"));
        }
        if let Some(kinds) = runtime_event_payload_string_array(payload, "resource_filter.kinds") {
            parts.push(format!("kinds={kinds}"));
        }
        if let Some(categories) =
            runtime_event_payload_string_array(payload, "resource_filter.categories")
        {
            parts.push(format!("categories={categories}"));
        }
        if let Some(label_selector) =
            runtime_event_payload_string(payload, "resource_filter.label_selector")
        {
            parts.push(format!("label_selector={label_selector}"));
        }
        if let Some(field_selector) =
            runtime_event_payload_string(payload, "resource_filter.field_selector")
        {
            parts.push(format!("field_selector={field_selector}"));
        }
        if let Some(limit) = runtime_event_payload_i64(payload, "limit") {
            parts.push(format!("limit={limit}"));
        }
        if let Some(event_limit) = runtime_event_payload_i64(payload, "event_limit") {
            parts.push(format!("event_limit={event_limit}"));
        }
        if let Some(resource_version) = runtime_event_payload_string(payload, "resource_version") {
            parts.push(format!("rv={resource_version}"));
        }
        if let Some(resource_version_cursor) =
            runtime_event_payload_string(payload, "resource_version_cursor")
        {
            parts.push(format!("cursor-rv={resource_version_cursor}"));
        }
        if let Some(continue_token) = runtime_event_payload_string(payload, "continue") {
            parts.push(format!("continue={continue_token}"));
        }
        if let Some(known_resources) = runtime_event_payload_i64(payload, "known_resources") {
            parts.push(format!("known={known_resources}"));
        }
        if let Some(allow_bookmarks) = payload.get("allow_bookmarks").and_then(Value::as_bool) {
            parts.push(format!("bookmarks={allow_bookmarks}"));
        }
        match (
            runtime_event_payload_i64(payload, "returned"),
            runtime_event_payload_i64(payload, "total"),
        ) {
            (Some(returned), Some(total)) => parts.push(format!("returned={returned}/{total}")),
            (Some(returned), None) => parts.push(format!("returned={returned}")),
            (None, Some(total)) => parts.push(format!("total={total}")),
            (None, None) => {}
        }
        if let Some(remaining) = runtime_event_payload_i64(payload, "remaining") {
            parts.push(format!("remaining={remaining}"));
        }
        if let Some(watch_events) = runtime_event_payload_i64(payload, "watch_events_returned") {
            parts.push(format!("watch_events={watch_events}"));
        }
        if let Some(event_resource_version) =
            runtime_event_payload_string(payload, "event_resource_version")
        {
            parts.push(format!("event-rv={event_resource_version}"));
        }
        if let Some(event_returned) = runtime_event_payload_i64(payload, "event_returned") {
            parts.push(format!("events={event_returned}"));
        }
        if let Some(metrics_total) = runtime_event_payload_i64(payload, "metrics_total") {
            parts.push(format!("metrics={metrics_total}"));
        }
        if let Some(metrics_resources) =
            runtime_event_payload_i64(payload, "metrics_resources_returned")
        {
            parts.push(format!("metrics_resources={metrics_resources}"));
        }
        match (
            runtime_event_payload_i64(payload, "health_ready"),
            runtime_event_payload_i64(payload, "health_degraded"),
            runtime_event_payload_i64(payload, "health_problem"),
            runtime_event_payload_i64(payload, "health_unknown"),
        ) {
            (Some(ready), Some(degraded), Some(problem), Some(unknown)) => {
                parts.push(format!("health={ready}/{degraded}/{problem}/{unknown}"))
            }
            _ => {}
        }
    }
    if event_type.starts_with("runtime.inspect.bundle.read") {
        if let Some(operation) = runtime_event_payload_string(payload, "operation") {
            parts.push(format!("operation={operation}"));
        }
        if let Some(kinds) = runtime_event_payload_string_array(payload, "resource_filter.kinds") {
            parts.push(format!("kinds={kinds}"));
        }
        if let Some(categories) =
            runtime_event_payload_string_array(payload, "resource_filter.categories")
        {
            parts.push(format!("categories={categories}"));
        }
        if let Some(label_selector) =
            runtime_event_payload_string(payload, "resource_filter.label_selector")
        {
            parts.push(format!("label_selector={label_selector}"));
        }
        if let Some(field_selector) =
            runtime_event_payload_string(payload, "resource_filter.field_selector")
        {
            parts.push(format!("field_selector={field_selector}"));
        }
        if let Some(event_limit) = runtime_event_payload_i64(payload, "event_limit") {
            parts.push(format!("event_limit={event_limit}"));
        }
        if let Some(inventory_resource_version) =
            runtime_event_payload_string(payload, "inventory_resource_version")
        {
            parts.push(format!("inventory-rv={inventory_resource_version}"));
        }
        match (
            runtime_event_payload_i64(payload, "inventory_returned"),
            runtime_event_payload_i64(payload, "inventory_total"),
        ) {
            (Some(returned), Some(total)) => parts.push(format!("inventory={returned}/{total}")),
            (Some(returned), None) => parts.push(format!("inventory_returned={returned}")),
            (None, Some(total)) => parts.push(format!("inventory_total={total}")),
            (None, None) => {}
        }
        if let Some(event_resource_version) =
            runtime_event_payload_string(payload, "event_resource_version")
        {
            parts.push(format!("event-rv={event_resource_version}"));
        }
        if let Some(events_returned) = runtime_event_payload_i64(payload, "event_returned") {
            parts.push(format!("events={events_returned}"));
        }
        if let Some(api_resources_returned) =
            runtime_event_payload_i64(payload, "api_resources_returned")
        {
            parts.push(format!("api_resources={api_resources_returned}"));
        }
        if let Some(health_total) = runtime_event_payload_i64(payload, "health_summary.total") {
            parts.push(format!("health_total={health_total}"));
        }
        match (
            runtime_event_payload_i64(payload, "metrics_summary.resources_returned"),
            runtime_event_payload_i64(payload, "metrics_summary.resources_total"),
        ) {
            (Some(returned), Some(total)) => {
                parts.push(format!("metrics_resources={returned}/{total}"))
            }
            (Some(returned), None) => parts.push(format!("metrics_resources_returned={returned}")),
            (None, Some(total)) => parts.push(format!("metrics_resources_total={total}")),
            (None, None) => {}
        }
        if let Some(metrics_events_total) =
            runtime_event_payload_i64(payload, "metrics_summary.events_total")
        {
            parts.push(format!("metric_events={metrics_events_total}"));
        }
        if let Some(log_refs) = runtime_event_payload_i64(payload, "log_refs") {
            parts.push(format!("log_refs={log_refs}"));
        }
    }
    if let Some(stream) = runtime_event_payload_string(payload, "stream") {
        parts.push(format!("stream={stream}"));
    }
    if let Some(object_ref) = runtime_event_payload_string(payload, "object_ref") {
        parts.push(format!("object={object_ref}"));
    }
    if let Some(sha256) = runtime_event_payload_string(payload, "sha256") {
        parts.push(format!("sha256={sha256}"));
    }
    if let Some(byte_size) = payload.get("byte_size").and_then(Value::as_i64) {
        parts.push(format!("bytes={byte_size}"));
    }
    if let Some(media_type) = runtime_event_payload_string(payload, "media_type") {
        parts.push(format!("media={media_type}"));
    }
    if let Some(exit_code) = runtime_event_payload_i64(payload, "connection.exit_code") {
        parts.push(format!("exit={exit_code}"));
    }
    if let Some(stdout) = runtime_event_exec_stream_summary(payload, "stdout") {
        parts.push(stdout);
    }
    if let Some(stderr) = runtime_event_exec_stream_summary(payload, "stderr") {
        parts.push(stderr);
    }
    if let Some(action) = runtime_event_payload_string(payload, "action") {
        parts.push(format!("action={action}"));
    }
    if let Some(reason) = runtime_event_payload_string(payload, "reason") {
        parts.push(format!("reason={reason}"));
    }
    if let Some(error_code) = runtime_event_payload_string(payload, "error_code") {
        parts.push(format!("error={error_code}"));
    }
    if let Some(error_status) = payload.get("error_status").and_then(Value::as_i64) {
        parts.push(format!("http={error_status}"));
    }
    if let Some(transition) = runtime_event_lifecycle_transition(payload) {
        parts.push(transition);
    } else if let Some(status) = runtime_event_payload_string(payload, "status") {
        parts.push(format!("status={status}"));
    }
    if let Some(message) = runtime_event_payload_string(payload, "message")
        .or_else(|| runtime_event_payload_string(payload, "error_message"))
        .or_else(|| runtime_event_payload_string(payload, "error"))
    {
        parts.push(format!("message={message}"));
    }
    format!("  - {}", parts.join(" "))
}

fn runtime_event_exec_stream_summary(payload: &Value, stream: &str) -> Option<String> {
    let bytes = runtime_event_payload_i64(payload, &format!("connection.{stream}_bytes"));
    let tail_bytes = runtime_event_payload_i64(payload, &format!("connection.{stream}_tail_bytes"));
    let truncated =
        runtime_event_payload_bool(payload, &format!("connection.{stream}_tail_truncated"));
    if bytes.is_none() && tail_bytes.is_none() && truncated.is_none() {
        return None;
    }
    let mut facts = Vec::new();
    if let Some(bytes) = bytes {
        facts.push(format!("bytes={bytes}"));
    }
    if let Some(tail_bytes) = tail_bytes {
        facts.push(format!("tail={tail_bytes}"));
    }
    if let Some(truncated) = truncated {
        facts.push(format!("truncated={truncated}"));
    }
    Some(format!("{stream}={}", facts.join(",")))
}

fn runtime_event_is_api_resources_read(event_type: &str) -> bool {
    matches!(
        event_type,
        "runtime.api_resources.read" | "runtime.api_resources.read.failed"
    )
}

fn runtime_event_is_resource_query_read(event_type: &str) -> bool {
    matches!(
        event_type,
        "runtime.resource.list.read"
            | "runtime.resource.list.read.failed"
            | "runtime.resource.watch.read"
            | "runtime.resource.watch.read.failed"
            | "runtime.resource.health.read"
            | "runtime.resource.health.read.failed"
            | "runtime.resource.describe.read"
            | "runtime.resource.describe.read.failed"
            | "runtime.resource.get.read"
            | "runtime.resource.get.read.failed"
            | "runtime.resource.events.read"
            | "runtime.resource.events.read.failed"
            | "runtime.resource.status.read"
            | "runtime.resource.status.read.failed"
            | "runtime.resource.metrics.read"
            | "runtime.resource.metrics.read.failed"
            | "runtime.resource.metrics.list.read"
            | "runtime.resource.metrics.list.read.failed"
    )
}

fn runtime_event_payload_string(payload: &Value, path: &str) -> Option<String> {
    let mut current = payload;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn runtime_event_payload_i64(payload: &Value, path: &str) -> Option<i64> {
    let mut current = payload;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    current.as_i64()
}

fn runtime_event_payload_bool(payload: &Value, path: &str) -> Option<bool> {
    let mut current = payload;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    current.as_bool()
}

fn runtime_event_payload_string_array(payload: &Value, path: &str) -> Option<String> {
    let mut current = payload;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    let values = current
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join(","))
}

fn runtime_event_ref_identity(payload: &Value, path: &str) -> Option<String> {
    let kind = runtime_event_payload_string(payload, &format!("{path}.kind"))?;
    let name = runtime_event_payload_string(payload, &format!("{path}.name"))?;
    Some(format!("{kind}/{name}"))
}

fn runtime_event_top_level_resource_ref_identity(event: &Value) -> Option<String> {
    runtime_event_top_level_resource_ref_identities(event)
        .into_iter()
        .next()
}

fn runtime_event_top_level_unemitted_refs_summary(
    event: &Value,
    emitted: &[String],
) -> Option<String> {
    let refs = runtime_event_top_level_resource_ref_identities(event)
        .into_iter()
        .filter(|identity| !runtime_event_ref_identity_seen(emitted, identity))
        .collect::<Vec<_>>();
    (!refs.is_empty()).then(|| refs.join(","))
}

fn runtime_event_top_level_resource_ref_identities(event: &Value) -> Vec<String> {
    event
        .get("resource_refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|resource| {
            let kind = resource.get("kind").and_then(Value::as_str)?.trim();
            let name = resource.get("name").and_then(Value::as_str)?.trim();
            (!kind.is_empty() && !name.is_empty()).then(|| format!("{kind}/{name}"))
        })
        .collect()
}

fn runtime_event_payload_resource_identity(payload: &Value) -> Option<String> {
    let kind = runtime_event_payload_string(payload, "resource_kind")?;
    let name = runtime_event_payload_string(payload, "resource_name")?;
    Some(format!("{kind}/{name}"))
}

fn runtime_event_ref_identity_seen(emitted: &[String], identity: &str) -> bool {
    emitted.iter().any(|existing| existing == identity)
}

fn runtime_event_lifecycle_transition(payload: &Value) -> Option<String> {
    let previous = runtime_event_payload_string(payload, "previous_status");
    let current = runtime_event_payload_string(payload, "status");
    match (previous, current) {
        (Some(previous), Some(current)) if previous != current => {
            Some(format!("transition={previous}->{current}"))
        }
        (Some(previous), Some(current)) => Some(format!("status={current} previous={previous}")),
        (None, Some(current)) => Some(format!("status={current}")),
        _ => None,
    }
}

fn runtime_events_follow_cursor(value: &Value) -> Option<String> {
    value
        .pointer("/metadata/continue")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            value
                .pointer("/metadata/resourceVersion")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .map(ToString::to_string)
}

fn runtime_resource_line(resource: &Value) -> String {
    let kind = resource
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let name = resource
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let phase = resource
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let reason = resource
        .pointer("/status/reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let ready = resource
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .and_then(|conditions| {
            conditions
                .iter()
                .find(|condition| condition.get("type").and_then(Value::as_str) == Some("Ready"))
        })
        .and_then(|condition| condition.get("status").and_then(Value::as_str))
        .unwrap_or("Unknown");
    let mut parts = vec![format!("{kind}/{name}")];
    if !phase.is_empty() {
        parts.push(format!("phase={phase}"));
    }
    parts.push(format!("ready={ready}"));
    if !reason.is_empty() {
        parts.push(format!("reason={reason}"));
    }
    parts.join(" ")
}

fn runtime_resource_ref_line(resource: &Value) -> String {
    let kind = resource
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let name = resource
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!("{kind}/{name}")
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

fn help_text() -> String {
    r#"buc - Bucephalus hosted Cloud CLI

`buc` talks to the hosted Cloud API. It does not run local Core builds, start
local runners, or manage Cloud operator pools.

Usage:
  buc login [--no-browser] [--json]
  buc logout [--dry-run] [--json]
  buc auth status [--json]
  buc health
  buc build <experiment.yaml|package-dir|package.tgz> [--label TEXT] [--json]
  buc doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]
  buc run <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]
  buc cancel <run-id> [--reason TEXT] [--json]
  buc inspect <package-digest> [--json]
  buc author canonicalize <draft.yaml|draft.json> [--json]
  buc author resolve <draft.yaml|draft.json> [--json]
  buc author validate <draft.yaml|draft.json> [--validation-level authoring|package|launch_hint] [--json]
  buc author suggest <draft.yaml|draft.json> --target VALUE [--q TEXT] [--limit N] [--json]
  buc author diff <left.yaml|json> <right.yaml|json> [--json]
  buc author export <draft.yaml|draft.json> [--format yaml|resolved_json] [--json]
  buc author preview <draft.yaml|draft.json> [--json]

Long-form nouns:
  buc drafts canonicalize <draft.yaml|draft.json> [--json]
  buc drafts resolve <draft.yaml|draft.json> [--json]
  buc drafts validate <draft.yaml|draft.json> [--validation-level authoring|package|launch_hint] [--json]
  buc drafts suggest <draft.yaml|draft.json> --target VALUE [--q TEXT] [--limit N] [--json]
  buc drafts diff <left.yaml|json> <right.yaml|json> [--json]
  buc drafts export <draft.yaml|draft.json> [--format yaml|resolved_json] [--json]
  buc drafts preview-schedule <draft.yaml|draft.json> [--json]
  buc packages list [--limit N] [--json]
  buc packages upload <package-dir|package.tgz> [--label TEXT] [--json]
  buc packages inspect <package-digest> [--json]
  buc secrets list [--json]
  buc secrets put <name> (--value-file PATH|--from-env ENV|--stdin) [--json]
  buc secrets delete <name> [--json]
  buc experiments build <experiment.yaml|package-dir|package.tgz> [--label TEXT] [--json]
  buc experiments doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]
  buc runs list [--limit N] [--json]
  buc runs create <package-digest> [--secret-ref NAME=REF ...] [--label TEXT] [--json]
  buc runs get <run-id> [--json]
  buc runs get <run-id> [kind|--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--wide|--output name|-o name] [--json]
  buc runs get <run-id> <Kind/name|KIND NAME> [--view resource|describe] [--json]
  buc runs api-resources <run-id> [kind] [--json]
  buc runs explain <run-id> <kind> [--json]
  buc runs inspect <run-id> [--kind KIND|--category CATEGORY] [--json]
  buc runs resources <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--wide|--output name|-o name] [--json]
  buc runs tree <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]
  buc runs describe <run-id> <Kind/name> [--json]
  buc runs status <run-id> <Kind/name> [--json]
  buc runs wait <run-id> <Kind/name> [--for condition=Ready|phase=completed|delete] [--json]
  buc runs can-i <run-id> <operation> <Kind/name> [--json]
  buc runs health <run-id> [--kind KIND|--category CATEGORY] [--json]
  buc runs metrics <run-id> [Kind/name] [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]
  buc runs top <run-id> [Kind/name] [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]
  buc runs watch <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--resource-version VERSION] [--known-resource Kind/name=VERSION] [--follow] [--interval-seconds N] [--max-polls N] [--json]
  buc runs events <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]
  buc runs audit <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]
  buc runs logs <run-id> <Kind/name> [--stream stdout|stderr] [--tail-lines N] [--out FILE] [--metadata-out FILE] [--follow] [--interval-seconds N] [--max-polls N]
  buc runs content <run-id> <TrialArtifact/name> [--out FILE] [--metadata-out FILE]
  buc runs port-forward <run-id> <Kind/name> --target-port PORT [--local-port PORT] [--attach] [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json]
  buc runs exec <run-id> <Kind/name> [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json] -- COMMAND [ARG...]
  buc runs cordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs drain <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs uncordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs cancel <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs complete <run-id> <PortForward/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs delete <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]

Cloud package boundary:
  build accepts authoring YAML or a sealed package directory/archive. YAML is
  uploaded as an authoring context and built by the hosted API with bundled
  Core. Sealed packages are uploaded/imported directly. Both paths report
  hosted Cloud readiness for the default Cloud target.

Authoring context:
  YAML builds require bucephalus.project.yaml above the entrypoint. The project
  manifest declares package sources, entrypoints, include/exclude rules, and
  the hosted Cloud target. Local generated and credential material such as .env,
  .npmrc, .ssh, .aws, node_modules, and target is excluded before upload; the
  hosted API rejects those paths too.

Auth:
  Sign in with `buc login`. buc reuses the shared
  Bucephalus Cloud profile and cached tokens from BUCEPHALUS_HOME, refreshing
  cached OAuth tokens when a refresh token is present.

Advanced overrides:
  --api-url URL        Development, staging, or self-hosted Cloud API.
  --user-token TOKEN   OAuth access token override for automation.

Runtime options:
  --backend VALUE --arch VALUE --isolation VALUE --cpu-count N --memory-mb N
  --disk-mb N --timeout-ms N --max-parallel-trials N
  --runtime-option KEY=VALUE (only supported hosted Cloud runtime keys)
  For array values use comma-separated lists, e.g. sidecars=redis,postgres.
  For network use JSON, e.g. network={"default":"allowlist_enforced","egress":["api.openai.com"]}.

Environment:
  BUCEPHALUS_CLOUD_API_URL       Development, staging, or self-hosted API
                                 override. Hosted Cloud defaults to
                                 {{HOSTED_API_URL}}
  BUCEPHALUS_CLOUD_USER_TOKEN    OAuth access token override
"#
    .replace(
        "{{HOSTED_API_URL}}",
        cloud_login::default_bucephalus_cloud_api_url(),
    )
}

fn command_help_text(group: Option<&str>, command: Option<&str>) -> Option<&'static str> {
    match (group, command) {
        (Some("login"), _) => Some(LOGIN_HELP),
        (Some("logout"), _) => Some(LOGOUT_HELP),
        (Some("auth"), None) | (Some("auth"), Some("--help" | "-h")) => Some(AUTH_HELP),
        (Some("auth"), Some("status")) => Some(AUTH_STATUS_HELP),
        (Some("health"), None) | (Some("health"), Some("--help" | "-h")) => Some(HEALTH_HELP),
        (Some("build"), _) => Some(BUILD_HELP),
        (Some("doctor"), _) => Some(DOCTOR_HELP),
        (Some("run"), _) => Some(RUN_HELP),
        (Some("cancel"), _) => Some(CANCEL_HELP),
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
        (Some("runs"), Some("api-resources")) => Some(RUNS_API_RESOURCES_HELP),
        (Some("runs"), Some("explain")) => Some(RUNS_EXPLAIN_HELP),
        (Some("runs"), Some("inspect")) => Some(RUNS_INSPECT_HELP),
        (Some("runs"), Some("resources")) => Some(RUNS_RESOURCES_HELP),
        (Some("runs"), Some("tree")) => Some(RUNS_TREE_HELP),
        (Some("runs"), Some("describe")) => Some(RUNS_DESCRIBE_HELP),
        (Some("runs"), Some("status")) => Some(RUNS_STATUS_HELP),
        (Some("runs"), Some("wait")) => Some(RUNS_WAIT_HELP),
        (Some("runs"), Some("can-i")) => Some(RUNS_CAN_I_HELP),
        (Some("runs"), Some("health")) => Some(RUNS_HEALTH_HELP),
        (Some("runs"), Some("metrics")) => Some(RUNS_METRICS_HELP),
        (Some("runs"), Some("top")) => Some(RUNS_TOP_HELP),
        (Some("runs"), Some("watch")) => Some(RUNS_WATCH_HELP),
        (Some("runs"), Some("events")) => Some(RUNS_EVENTS_HELP),
        (Some("runs"), Some("audit")) => Some(RUNS_AUDIT_HELP),
        (Some("runs"), Some("logs")) => Some(RUNS_LOGS_HELP),
        (Some("runs"), Some("content")) => Some(RUNS_CONTENT_HELP),
        (Some("runs"), Some("port-forward")) => Some(RUNS_PORT_FORWARD_HELP),
        (Some("runs"), Some("exec")) => Some(RUNS_EXEC_HELP),
        (
            Some("runs"),
            Some("action" | "cordon" | "drain" | "uncordon" | "cancel" | "complete"),
        ) => Some(RUNS_ACTION_HELP),
        (Some("runs"), Some("delete")) => Some(RUNS_DELETE_HELP),
        _ => None,
    }
}

const HEALTH_HELP: &str = r#"buc health

Check hosted API readiness.

Usage:
  buc health
"#;

const LOGIN_HELP: &str = r#"buc login

Authenticate to the hosted Cloud product and persist the Cloud profile.

Usage:
  buc login [--no-browser] [--json]

Notes:
  For normal hosted Cloud, no API URL is required. The command stores Cloud auth
  under BUCEPHALUS_HOME and reuses it for later buc commands.

Advanced:
  --api-url URL is for dev/staging/self-hosted Cloud. Normal hosted users do
  not need an API URL, issuer, audience, or client id. --resource is accepted
  only as a backwards-compatible alias for --api-url; new scripts should use
  --api-url.
"#;

const LOGOUT_HELP: &str = r#"buc logout

Remove cached hosted Cloud auth files from BUCEPHALUS_HOME.

Usage:
  buc logout [--dry-run] [--json]
"#;

const AUTH_HELP: &str = r#"buc auth

Inspect hosted Cloud auth state.

Usage:
  buc auth status [--json]
"#;

const AUTH_STATUS_HELP: &str = r#"buc auth status

Show whether buc can find local hosted Cloud auth state.

Usage:
  buc auth status [--json]
"#;

const BUILD_HELP: &str = r#"buc build

Build authoring YAML in hosted Cloud or import a sealed package.

Usage:
  buc build <experiment.yaml|package-dir|package.tgz> [--label TEXT] [--json]

Boundary:
  YAML inputs require bucephalus.project.yaml above the entrypoint. The manifest
  declares the upload boundary, entrypoints, package source, and target; hosted
  Core builds from that declared project context. Package inputs upload/import
  an existing sealed package. Both paths fail if the package is not runnable on
  the hosted Cloud target.
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

const CANCEL_HELP: &str = r#"buc cancel

Cancel a hosted run. The run is marked cancelled so the scheduler will not
claim it again; a worker already executing it aborts on its next heartbeat.
Already-completed or already-failed runs cannot be cancelled.

Usage:
  buc cancel <run-id> [--reason TEXT] [--json]
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
  buc experiments build <experiment.yaml|package-dir|package.tgz> [--label TEXT] [--json]
  buc experiments doctor <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--json]

Secrets:
  Prefer hosted secrets. Upload once with `buc secrets put NAME --from-env NAME`,
  then pass `--secret-ref NAME=bucephalus://NAME`.
"#;

const EXPERIMENTS_BUILD_HELP: &str = r#"buc experiments build

Build authoring YAML in hosted Cloud or import a sealed package.

Usage:
  buc experiments build <experiment.yaml|package-dir|package.tgz> [--label TEXT] [--json]

Boundary:
  This command calls POST /v1/experiments/builds after upload. YAML inputs are
  built by hosted Core from the bucephalus.project.yaml-declared authoring
  context. Sealed package inputs are imported directly. Both paths report
  hosted Cloud readiness.
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
  buc runs list [--limit N] [--output id|-o id] [--json]
  buc runs create <package-digest> [--secret-ref NAME=REF ...] [--secret-ref-file secrets.yaml] [--label TEXT] [--json]
  buc runs get <run-id> [--json]
  buc runs get <run-id> [kind|--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--wide|--output name|-o name] [--json]
  buc runs get <run-id> <Kind/name|KIND NAME> [--view resource|describe] [--json]
  buc runs api-resources <run-id> [kind] [--json]
  buc runs explain <run-id> <kind> [--json]
  buc runs inspect <run-id> [--kind KIND|--category CATEGORY] [--json]
  buc runs resources <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--wide|--output name|-o name] [--json]
  buc runs tree <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]
  buc runs describe <run-id> <Kind/name> [--json]
  buc runs status <run-id> <Kind/name> [--json]
  buc runs wait <run-id> <Kind/name> [--for condition=Ready|phase=completed|delete] [--timeout-seconds N] [--json]
  buc runs can-i <run-id> <operation> <Kind/name> [--json]
  buc runs health <run-id> [--kind KIND|--category CATEGORY] [--json]
  buc runs metrics <run-id> [Kind/name] [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]
  buc runs top <run-id> [Kind/name] [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]
  buc runs watch <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--resource-version VERSION] [--known-resource Kind/name=VERSION] [--follow] [--interval-seconds N] [--max-polls N] [--json]
  buc runs events <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]
  buc runs audit <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]
  buc runs logs <run-id> <Kind/name> [--stream stdout|stderr] [--tail-lines N] [--out FILE] [--metadata-out FILE] [--follow] [--interval-seconds N] [--max-polls N]
  buc runs content <run-id> <TrialArtifact/name> [--out FILE] [--metadata-out FILE]
  buc runs port-forward <run-id> <Kind/name> --target-port PORT [--local-port PORT] [--attach] [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json]
  buc runs exec <run-id> <Kind/name> [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json] -- COMMAND [ARG...]
  buc runs cordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs drain <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs uncordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs cancel <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs complete <run-id> <PortForward/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs delete <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]

Secrets:
  Prefer hosted secrets. Upload once with `buc secrets put NAME --from-env NAME`,
  then pass `--secret-ref NAME=bucephalus://NAME`.
"#;

const RUNS_LIST_HELP: &str = r#"buc runs list

List recent hosted run records visible to the authenticated user.

Usage:
  buc runs list [--limit N] [--output id|-o id] [--json]

Options:
  --output id, -o id
            Print one run id per line for scripts and shell pipelines.
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

Fetch hosted run status or Kubernetes-shaped runtime resources.

Usage:
  buc runs get <run-id> [--json]
  buc runs get <run-id> [kind|--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--wide|--output name|-o name] [--json]
  buc runs get <run-id> <Kind/name|KIND NAME> [--view resource|describe] [--event-limit N] [--json]

Resource forms:
  With only <run-id>, get returns the Cloud run record.
  With kind or selectors, get lists runtime resources from /runtime/resources.
  --category sends server-advertised API resource categories as selectors.
  Add --wide to render API-discovered printer columns for resource lists, or
  --output name or -o name to print only Kind/name refs for scripts and shell
  pipelines.
  With Kind/name or KIND NAME, get fetches the raw runtime resource by default.
"#;

const RUNS_API_RESOURCES_HELP: &str = r#"buc runs api-resources

Discover runtime resource kinds, verbs, subresources, actions, selectors, and
low-level access operations for a hosted run.

Usage:
  buc runs api-resources <run-id> [kind] [--json]
"#;

const RUNS_EXPLAIN_HELP: &str = r#"buc runs explain

Explain one runtime resource kind from the server-owned API resource contract:
aliases, categories, verbs, subresources, actions, access, printer columns,
paths, and example commands.

Usage:
  buc runs explain <run-id> <kind> [--json]
"#;

const RUNS_INSPECT_HELP: &str = r#"buc runs inspect

Fetch a bounded runtime inspect bundle: API discovery, inventory, health,
metrics, recent audit events, and log refs.

Usage:
  buc runs inspect <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--event-limit N] [--json]
"#;

const RUNS_RESOURCES_HELP: &str = r#"buc runs resources

List Kubernetes-shaped runtime resources for a hosted run.

Usage:
  buc runs resources <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--wide|--output name|-o name] [--json]

Options:
  --category CATEGORY
            Select server-advertised runtime API resource categories.
  --wide    Render API-discovered printer columns, grouped by runtime kind.
  --output name, -o name
            Print one Kind/name runtime resource ref per line for pipelines.
"#;

const RUNS_TREE_HELP: &str = r#"buc runs tree

Print a Kubernetes-style owner-reference tree for runtime resources. The tree is
built from the current /runtime/resources inventory; filtered-out parents are
shown as owner references on root rows.

Usage:
  buc runs tree <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]
"#;

const RUNS_DESCRIBE_HELP: &str = r#"buc runs describe

Describe one runtime resource and its related lifecycle/audit events.

Usage:
  buc runs describe <run-id> <Kind/name> [--view describe|resource] [--event-limit N] [--json]
"#;

const RUNS_STATUS_HELP: &str = r#"buc runs status

Fetch the status subresource for one runtime resource.
Human output includes resourceVersion, generation freshness, conditions,
available actions, deletion timestamp, and audit source when present.

Usage:
  buc runs status <run-id> <Kind/name> [--json]
"#;

const RUNS_WAIT_HELP: &str = r#"buc runs wait

Wait for one runtime resource status predicate through the status subresource.
On success, human output prints the same status summary as `buc runs status`
so follow-up commands can reuse the returned resourceVersion.

Usage:
  buc runs wait <run-id> <Kind/name> [--for condition=Ready[=True]|phase=completed|delete] [--timeout-seconds N] [--interval-seconds N] [--json]
"#;

const RUNS_CAN_I_HELP: &str = r#"buc runs can-i

Ask the server whether one runtime resource operation is supported right now.
Exits zero when supported and non-zero when the server denies the operation.
Human output includes the reviewed command and resource version when available.
Use it for mutating, low-level access, and observability operations.

Usage:
  buc runs can-i <run-id> <operation> <Kind/name> [--json]

Examples:
  buc runs can-i <run-id> port-forward RunnerInstance/<runner-name>
  buc runs can-i <run-id> exec TrialContainer/<container-name>
  buc runs can-i <run-id> top TrialContainer/<container-name>
  buc runs can-i <run-id> audit RunnerInstance/<runner-name>
  buc runs can-i <run-id> logs/stdout TrialContainer/<container-name>
  buc runs can-i <run-id> logs/stderr TrialContainer/<container-name>
  buc runs can-i <run-id> content TrialArtifact/<artifact-name>
  buc runs can-i <run-id> cordon RunnerInstance/<runner-name>
  buc runs can-i <run-id> cancel Exec/<exec-name>
  buc runs can-i <run-id> complete PortForward/<port-forward-name>
"#;

const RUNS_HEALTH_HELP: &str = r#"buc runs health

Summarize runtime resource health for a hosted run.

Usage:
  buc runs health <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--json]
"#;

const RUNS_METRICS_HELP: &str = r#"buc runs metrics

Fetch collection metrics or one runtime resource metrics subresource.

Usage:
  buc runs metrics <run-id> [Kind/name] [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]
"#;

const RUNS_TOP_HELP: &str = r#"buc runs top

Show top-style collection metrics for runtime resources in a hosted run.

Usage:
  buc runs top <run-id> [Kind/name] [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]
"#;

const RUNS_WATCH_HELP: &str = r#"buc runs watch

Fetch a Kubernetes-style watch snapshot for runtime resource changes.

Usage:
  buc runs watch <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--resource-version VERSION] [--known-resource Kind/name=VERSION] [--follow] [--interval-seconds N] [--max-polls N] [--json]
"#;

const RUNS_EVENTS_HELP: &str = r#"buc runs events

List run-wide or resource-scoped runtime audit/events.

Usage:
  buc runs events <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]
"#;

const RUNS_AUDIT_HELP: &str = r#"buc runs audit

List runtime access, resource lifecycle, catalog read, resource read, inspect-bundle read, and raw-byte read audit events for a hosted run.

Usage:
  buc runs audit <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]
"#;

const RUNS_LOGS_HELP: &str = r#"buc runs logs

Fetch raw logs from a runtime resource logs subresource.

Usage:
  buc runs logs <run-id> <Kind/name> [--stream stdout|stderr] [--tail-lines N] [--out FILE] [--metadata-out FILE] [--follow] [--interval-seconds N] [--max-polls N]

Notes:
  Omit --out or pass --out - to write to stdout.
  Use --metadata-out FILE to write the response provenance from Cloud headers
  without mixing it into raw stdout.
  --follow repeats the logs request and prints only appended bytes, using
  overlap detection so sliding --tail-lines windows do not duplicate output.
"#;

const RUNS_CONTENT_HELP: &str = r#"buc runs content

Fetch raw artifact content from a TrialArtifact resource content subresource.

Usage:
  buc runs content <run-id> <TrialArtifact/name> [--out FILE] [--metadata-out FILE]

Notes:
  Omit --out or pass --out - to write to stdout.
  Use --metadata-out FILE to write the response provenance from Cloud headers
  without mixing it into raw stdout.
"#;

const RUNS_PORT_FORWARD_HELP: &str = r#"buc runs port-forward

Create an audited PortForward resource for a concrete runtime target and wait
for the runner to report an active tunnel by default.
Pass --attach to start a GCE IAP local TCP forward when a provider tunnel is
available, or to surface a worker-reported client endpoint when the worker owns
the tunnel.

Usage:
  buc runs port-forward <run-id> <Kind/name> --target-port PORT [--local-port PORT] [--attach] [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json]
"#;

const RUNS_EXEC_HELP: &str = r#"buc runs exec

Create an audited Exec resource for a concrete runtime target and wait for the
runner to report the command result by default.

Usage:
  buc runs exec <run-id> <Kind/name> [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json] -- COMMAND [ARG...]
"#;

const RUNS_ACTION_HELP: &str = r#"buc runs resource verbs

Perform runtime resource lifecycle verbs with reviewed resource versions.

Usage:
  buc runs cordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs drain <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs uncordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs cancel <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]
  buc runs complete <run-id> <PortForward/name> [--reason TEXT] --resource-version VERSION [--json]

Compatibility:
  buc runs action <run-id> <Kind/name> <cordon|drain|uncordon|cancel|complete> [--reason TEXT] --resource-version VERSION [--json]
"#;

const RUNS_DELETE_HELP: &str = r#"buc runs delete

Delete a cancelable runtime access resource.

Usage:
  buc runs delete <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]
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

fn flag_present(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
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

    fn write_project_manifest(root: &Path, entrypoint: &str, include: &[&str]) {
        let include_lines = include
            .iter()
            .map(|pattern| format!("      - {pattern}"))
            .collect::<Vec<_>>();
        let mut lines = vec![
            format!("schema_version: {PROJECT_MANIFEST_SCHEMA_VERSION}"),
            "project:".to_string(),
            "  id: test_project".to_string(),
            "package_sources:".to_string(),
            "  default:".to_string(),
            "    root: .".to_string(),
            "    entrypoints:".to_string(),
            format!("      - {entrypoint}"),
            "    include:".to_string(),
        ];
        lines.extend(include_lines);
        lines.extend([
            "    exclude:".to_string(),
            "      - ignored/**".to_string(),
            "targets:".to_string(),
            "  hosted_cloud: {}".to_string(),
            "".to_string(),
        ]);
        fs::write(root.join(PROJECT_MANIFEST_YAML), lines.join("\n")).unwrap();
    }

    #[test]
    fn hosted_authoring_yaml_commands_are_recognized_before_cloud_calls() {
        let _lock = lock_env();
        let home = temp_dir("authoring_api_config");
        let root = temp_dir("authoring_api_config_project");
        fs::create_dir_all(&root).unwrap();
        let experiment = root.join("experiment.yaml");
        let modal_experiment = root.join("experiment.modal.yaml");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        let err = run(vec![
            "experiments".to_string(),
            "build".to_string(),
            experiment.display().to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("authoring YAML path does not exist"));
        assert!(!err.contains("unknown hosted command"));

        let natural_err = run(vec![
            "build".to_string(),
            modal_experiment.display().to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(natural_err.contains("authoring YAML path does not exist"));
        assert!(
            !natural_err.contains("unknown hosted command"),
            "natural build command should be recognized before failing: {natural_err}"
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn build_rejects_missing_local_inputs_before_api_config() {
        let _lock = lock_env();
        let home = temp_dir("build_missing_input_home");
        let root = temp_dir("build_missing_input_project");
        fs::create_dir_all(&root).unwrap();
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        let missing_yaml_err = run(vec![
            "build".to_string(),
            root.join("missing.yaml").display().to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(missing_yaml_err.contains("authoring YAML path does not exist"));
        assert!(!missing_yaml_err.contains("hosted API URL"));

        let missing_package_err = run(vec![
            "build".to_string(),
            root.join("missing-package.tgz").display().to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(missing_package_err.contains("sealed package path does not exist"));
        assert!(!missing_package_err.contains("hosted API URL"));

        let _ = fs::remove_dir_all(root);
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
        write_project_manifest(
            &root,
            "experiment.yaml",
            &["experiment.yaml", "cases.jsonl"],
        );

        let prepared = prepare_authoring_context_input(&root.join("experiment.yaml")).unwrap();

        assert_eq!(prepared.entrypoint, "experiment.yaml");
        let entries = archive_entries(&prepared.archive_path);
        assert!(entries.contains(&PROJECT_MANIFEST_YAML.to_string()));
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
        write_project_manifest(
            &root,
            "experiment.yaml",
            &["experiment.yaml", "cases.jsonl"],
        );

        let err = prepare_authoring_context_input(&root.join("experiment.yaml"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("authoring context has too many entries"));
        assert!(err.contains("Narrow bucephalus.project.yaml include patterns"));
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);

        let err = prepare_authoring_context_input(&root.join("experiment.yaml"))
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
        write_project_manifest(
            &root,
            "experiments/peter/experiment.yaml",
            &["experiments/peter/**", "shared/**"],
        );

        let prepared =
            prepare_authoring_context_input(&root.join("experiments/peter/experiment.yaml"))
                .unwrap();

        assert_eq!(prepared.entrypoint, "experiments/peter/experiment.yaml");
        let entries = archive_entries(&prepared.archive_path);
        assert!(entries.contains(&PROJECT_MANIFEST_YAML.to_string()));
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
    fn authoring_context_rejects_yaml_not_declared_by_project_manifest() {
        let root = temp_dir("authoring_context_root_reject");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(root.join("experiments/other")).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        fs::write(
            root.join("experiments/other/experiment.yaml"),
            "experiment: {}\n",
        )
        .unwrap();
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);

        let err = prepare_authoring_context_input(&root.join("experiments/other/experiment.yaml"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("does not declare entrypoint experiments/other/experiment.yaml"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authoring_context_rejects_project_manifest_without_hosted_target() {
        let root = temp_dir("authoring_context_missing_hosted_target");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("experiment.yaml"), "experiment: {}\n").unwrap();
        fs::write(
            root.join(PROJECT_MANIFEST_YAML),
            [
                "schema_version: bucephalus_project_v1",
                "project:",
                "  id: test_project",
                "package_sources:",
                "  default:",
                "    root: .",
                "    entrypoints:",
                "      - experiment.yaml",
                "    include:",
                "      - experiment.yaml",
                "",
            ]
            .join("\n"),
        )
        .unwrap();

        let err = prepare_authoring_context_input(&root.join("experiment.yaml"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("project manifest must declare targets.hosted_cloud"));
        fs::remove_dir_all(root).unwrap();
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
        write_project_manifest(
            &root,
            "experiments/peter/experiment.yaml",
            &["experiments/peter/**", "shared/**"],
        );

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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
                    let project_manifest = project_manifest_from_build_request(request);
                    json!({
                        "build_id": "build-1",
                        "build_kind": "hosted_authoring_build",
                        "build_environment": {
                            "source": {
                                "upload_id": "upload-1",
                                "input_kind": "authoring_context",
                                "content_digest": source.as_ref().map(|(digest, _)| digest.as_str()).unwrap_or("sha256:missing-upload-content"),
                                "byte_size": source.as_ref().map(|(_, byte_size)| *byte_size).unwrap_or(0),
                                "entrypoint": "experiment.yaml",
                                "project_manifest": project_manifest
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
                    let project_manifest = project_manifest_from_build_request(request);
                    json!({
                        "build_id": "build-1",
                        "build_kind": "hosted_authoring_build",
                        "build_environment": {
                            "source": {
                                "upload_id": "upload-1",
                                "input_kind": "authoring_context",
                                "content_digest": source.as_ref().map(|(digest, _)| digest.as_str()).unwrap_or("sha256:missing-upload-content"),
                                "byte_size": source.as_ref().map(|(_, byte_size)| *byte_size).unwrap_or(0),
                                "entrypoint": "experiment.yaml",
                                "project_manifest": project_manifest
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
                    let project_manifest = project_manifest_from_build_request(request);
                    json!({
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
                                "entrypoint": "experiment.yaml",
                                "project_manifest": project_manifest
                            },
                            "runtime_options": {},
                            "package_contract": {
                                "input_kind": "authoring_context",
                                "authoring_compiler": "core_universal_v1",
                                "authoring_provenance": {
                                    "status": "hosted_attested",
                                    "source": "hosted_core"
                                },
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
                    })
                }
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
        write_project_manifest(&root, "experiment.yaml", &["experiment.yaml"]);
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
                    let project_manifest = project_manifest_from_build_request(request);
                    json!({
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
                                "entrypoint": "experiment.yaml",
                                "project_manifest": project_manifest
                            },
                            "runtime_options": {},
                            "package_contract": {
                                "input_kind": "sealed_package",
                                "authoring_compiler": "core_universal_v1",
                                "authoring_provenance": {
                                    "status": "external_unattested",
                                    "source": "sealed_package_manifest"
                                },
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
                    })
                }
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

        let suggest_limit_err = run(vec![
            "author".to_string(),
            "suggest".to_string(),
            "missing-draft.yaml".to_string(),
            "--target".to_string(),
            "variant".to_string(),
            "--limit".to_string(),
            "101".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(suggest_limit_err.contains("--limit must be <= 100"));
        assert!(!suggest_limit_err.contains("failed to read"));

        let suggest_missing_target_err = run(vec![
            "author".to_string(),
            "suggest".to_string(),
            "missing-draft.yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(suggest_missing_target_err.contains("--target is required"));
        assert!(!suggest_missing_target_err.contains("failed to read"));

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

        let invalid_validation_level_err = run(vec![
            "author".to_string(),
            "validate".to_string(),
            "missing-draft.yaml".to_string(),
            "--validation-level".to_string(),
            "runtime".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(invalid_validation_level_err
            .contains("--validation-level must be one of authoring, package, launch_hint"));
        assert!(!invalid_validation_level_err.contains("failed to read"));

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
    fn author_commands_reject_bad_local_drafts_before_api_config() {
        let _lock = lock_env();
        let home = temp_dir("author_bad_draft_home");
        let root = temp_dir("author_bad_draft_project");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("invalid.yaml"), "experiment: [\n").unwrap();
        fs::write(root.join("array.json"), "[]").unwrap();
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);

        let missing_err = run(vec![
            "author".to_string(),
            "validate".to_string(),
            root.join("missing.yaml").display().to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(missing_err.contains("failed to read"));
        assert!(!missing_err.contains("hosted API URL"));

        let invalid_yaml_err = run(vec![
            "drafts".to_string(),
            "resolve".to_string(),
            root.join("invalid.yaml").display().to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(invalid_yaml_err.contains("draft YAML is invalid"));
        assert!(!invalid_yaml_err.contains("hosted API URL"));

        let non_object_err = run(vec![
            "author".to_string(),
            "canonicalize".to_string(),
            root.join("array.json").display().to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(non_object_err.contains("draft file must contain a JSON/YAML object"));
        assert!(!non_object_err.contains("hosted API URL"));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(home);
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
    fn list_and_runtime_commands_reject_bad_pagination_before_api_calls() {
        let packages_err = run(vec![
            "packages".to_string(),
            "list".to_string(),
            "--limit".to_string(),
            "potato".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(packages_err.contains("--limit requires a positive integer"));
        assert!(!packages_err.contains("hosted API URL"));

        let runs_err = run(vec![
            "runs".to_string(),
            "list".to_string(),
            "--limit".to_string(),
            "0".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(runs_err.contains("--limit requires a positive integer"));
        assert!(!runs_err.contains("hosted API URL"));

        let too_large_limit_err = run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--limit".to_string(),
            "1001".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(too_large_limit_err.contains("--limit must be <= 1000"));
        assert!(!too_large_limit_err.contains("hosted API URL"));

        let wide_json_err = run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--wide".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(wide_json_err.contains("--wide cannot be combined with --json"));
        assert!(!wide_json_err.contains("hosted API URL"));

        let output_json_err = run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--output".to_string(),
            "name".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(output_json_err.contains("--output name cannot be combined with --json"));
        assert!(!output_json_err.contains("hosted API URL"));

        let short_output_json_err = run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "-o".to_string(),
            "name".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(short_output_json_err.contains("--output name cannot be combined with --json"));
        assert!(!short_output_json_err.contains("hosted API URL"));

        let unsupported_output_err = run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--output".to_string(),
            "yaml".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(unsupported_output_err.contains("--output supports only name"));
        assert!(!unsupported_output_err.contains("hosted API URL"));

        let wide_item_err = run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial/trial-1".to_string(),
            "--wide".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(wide_item_err.contains("--wide only applies to runtime resource lists"));
        assert!(!wide_item_err.contains("hosted API URL"));

        let output_item_err = run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial/trial-1".to_string(),
            "--output".to_string(),
            "name".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(output_item_err.contains("--output only applies to runtime resource lists"));
        assert!(!output_item_err.contains("hosted API URL"));

        let short_output_item_err = run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial/trial-1".to_string(),
            "-o".to_string(),
            "name".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(short_output_item_err.contains("--output only applies to runtime resource lists"));
        assert!(!short_output_item_err.contains("hosted API URL"));

        let duplicate_output_err = run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--output".to_string(),
            "name".to_string(),
            "-o".to_string(),
            "name".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(duplicate_output_err.contains("--output can only be provided once"));
        assert!(!duplicate_output_err.contains("hosted API URL"));

        let category_kind_err = run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial".to_string(),
            "--category".to_string(),
            "runner".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(category_kind_err
            .contains("runtime resource filters must use either a kind or --category"));
        assert!(!category_kind_err.contains("hosted API URL"));

        let category_item_err = run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial/trial-1".to_string(),
            "--category".to_string(),
            "runner".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(category_item_err.contains("--category only applies to runtime resource lists"));
        assert!(!category_item_err.contains("hosted API URL"));

        let metrics_category_item_err = run(vec![
            "runs".to_string(),
            "metrics".to_string(),
            "run-1".to_string(),
            "Trial/trial-1".to_string(),
            "--category".to_string(),
            "trial".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(metrics_category_item_err
            .contains("--category only applies to runtime metrics collection lists"));
        assert!(!metrics_category_item_err.contains("hosted API URL"));

        let events_err = run(vec![
            "runs".to_string(),
            "events".to_string(),
            "run-1".to_string(),
            "--after-row-seq".to_string(),
            "nan".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(events_err.contains("--after-row-seq requires a non-negative integer"));
        assert!(!events_err.contains("hosted API URL"));

        let follow_json_err = run(vec![
            "runs".to_string(),
            "events".to_string(),
            "run-1".to_string(),
            "--follow".to_string(),
            "--json".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(follow_json_err.contains("--follow cannot be combined with --json"));
        assert!(!follow_json_err.contains("hosted API URL"));

        let max_polls_without_follow_err = run(vec![
            "runs".to_string(),
            "events".to_string(),
            "run-1".to_string(),
            "--max-polls".to_string(),
            "2".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(max_polls_without_follow_err
            .contains("--interval-seconds and --max-polls require --follow"));
        assert!(!max_polls_without_follow_err.contains("hosted API URL"));

        let log_max_polls_without_follow_err = run(vec![
            "runs".to_string(),
            "logs".to_string(),
            "run-1".to_string(),
            "Trial/trial-1".to_string(),
            "--max-polls".to_string(),
            "2".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(log_max_polls_without_follow_err
            .contains("--interval-seconds and --max-polls require --follow"));
        assert!(!log_max_polls_without_follow_err.contains("hosted API URL"));

        let bad_wait_predicate_err = run(vec![
            "runs".to_string(),
            "wait".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--for".to_string(),
            "deleted".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(bad_wait_predicate_err
            .contains("--for must be condition=<type>[=<status>], phase=<phase>, or delete"));
        assert!(!bad_wait_predicate_err.contains("hosted API URL"));
    }

    #[test]
    fn runs_raw_resource_commands_fetch_logs_and_content() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_raw_header_handler(2, |request, _index| {
            match (request.method.as_str(), request.path.as_str()) {
                (
                    "GET",
                    "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/logs?stream=stderr&tail%5Flines=2",
                ) => (
                    200,
                    "text/plain; charset=utf-8",
                    vec![
                        ("x-bucephalus-run-id", "run-1"),
                        ("x-bucephalus-resource-kind", "RunnerInstance"),
                        ("x-bucephalus-resource-name", "runner-1"),
                        ("x-bucephalus-resource-version", "sha256:runner-rv"),
                        ("x-bucephalus-log-stream", "stderr"),
                        ("x-bucephalus-core-run-id", "core-run-1"),
                        ("x-bucephalus-trial-id", ""),
                        ("x-bucephalus-artifact-role", "stderr"),
                        (
                            "x-bucephalus-object-ref",
                            "runtime://cloud-run/run-1/runner-instance/runner-1/stderr",
                        ),
                        (
                            "x-bucephalus-artifact-sha256",
                            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        ),
                    ],
                    b"line 2\nline 3\n".to_vec(),
                ),
                (
                    "GET",
                    "/v1/runs/run%2D1/runtime/resources/TrialArtifact/artifact%2D1/content",
                ) => (
                    200,
                    "application/json",
                    vec![
                        ("x-bucephalus-run-id", "run-1"),
                        ("x-bucephalus-resource-kind", "TrialArtifact"),
                        ("x-bucephalus-resource-name", "artifact-1"),
                        ("x-bucephalus-resource-version", "sha256:artifact-rv"),
                        ("x-bucephalus-core-run-id", "core-run-1"),
                        ("x-bucephalus-trial-id", "trial-1"),
                        ("x-bucephalus-artifact-role", "agent_result"),
                        (
                            "x-bucephalus-object-ref",
                            "artifact://sha256/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        ),
                        (
                            "x-bucephalus-artifact-sha256",
                            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        ),
                    ],
                    br#"{"ok":true}"#.to_vec(),
                ),
                _ => panic!(
                    "unexpected mock Cloud API raw request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_raw_runtime_home");
        let log_path = home.join("runner.log");
        let log_metadata_path = home.join("runner.log.metadata.json");
        let content_path = home.join("artifact.json");
        let content_metadata_path = home.join("artifact.json.metadata.json");
        fs::create_dir_all(&home).unwrap();
        let home_s = home.display().to_string();
        let log_path_s = log_path.display().to_string();
        let log_metadata_path_s = log_metadata_path.display().to_string();
        let content_path_s = content_path.display().to_string();
        let content_metadata_path_s = content_metadata_path.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "logs".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--stream".to_string(),
            "stderr".to_string(),
            "--tail-lines".to_string(),
            "2".to_string(),
            "--out".to_string(),
            log_path_s,
            "--metadata-out".to_string(),
            log_metadata_path_s,
        ])
        .expect("hosted run logs should fetch raw resource logs");
        run(vec![
            "runs".to_string(),
            "content".to_string(),
            "run-1".to_string(),
            "TrialArtifact/artifact-1".to_string(),
            "--out".to_string(),
            content_path_s,
            "--metadata-out".to_string(),
            content_metadata_path_s,
        ])
        .expect("hosted run content should fetch raw artifact content");

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        assert_eq!(fs::read_to_string(&log_path).unwrap(), "line 2\nline 3\n");
        assert_eq!(fs::read_to_string(&content_path).unwrap(), r#"{"ok":true}"#);
        let log_metadata: Value =
            serde_json::from_str(&fs::read_to_string(&log_metadata_path).unwrap()).unwrap();
        assert_eq!(
            log_metadata,
            json!({
                "run_id": "run-1",
                "log_stream": "stderr",
                "core_run_id": "core-run-1",
                "artifact_role": "stderr",
                "object_ref": "runtime://cloud-run/run-1/runner-instance/runner-1/stderr",
                "sha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "media_type": "text/plain; charset=utf-8",
                "byte_size": 14,
                "resource": {
                    "kind": "RunnerInstance",
                    "name": "runner-1",
                    "resource_version": "sha256:runner-rv"
                }
            })
        );
        let content_metadata: Value =
            serde_json::from_str(&fs::read_to_string(&content_metadata_path).unwrap()).unwrap();
        assert_eq!(
            content_metadata,
            json!({
                "run_id": "run-1",
                "core_run_id": "core-run-1",
                "trial_id": "trial-1",
                "artifact_role": "agent_result",
                "object_ref": "artifact://sha256/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "sha256": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "media_type": "application/json",
                "byte_size": 11,
                "resource": {
                    "kind": "TrialArtifact",
                    "name": "artifact-1",
                    "resource_version": "sha256:artifact-rv"
                }
            })
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_logs_follow_prints_only_appended_tail_bytes() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_raw_handler(3, |request, index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/v1/runs/run%2D1/runtime/resources/Trial/trial%2D1/logs?stream=stdout&tail%5Flines=2"
            );
            let body = match index {
                0 => b"line 1\nline 2\n".to_vec(),
                1 => b"line 1\nline 2\nline 3\n".to_vec(),
                2 => b"line 2\nline 3\nline 4\n".to_vec(),
                _ => unreachable!("unexpected mock request"),
            };
            (200, "text/plain; charset=utf-8", body)
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_logs_follow_home");
        let log_path = home.join("trial.log");
        fs::create_dir_all(&home).unwrap();
        let home_s = home.display().to_string();
        let log_path_s = log_path.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "logs".to_string(),
            "run-1".to_string(),
            "Trial/trial-1".to_string(),
            "--stream".to_string(),
            "stdout".to_string(),
            "--tail-lines".to_string(),
            "2".to_string(),
            "--follow".to_string(),
            "--max-polls".to_string(),
            "3".to_string(),
            "--interval-seconds".to_string(),
            "0".to_string(),
            "--out".to_string(),
            log_path_s,
        ])
        .expect("hosted run logs should follow raw resource logs");

        let requests = server.join();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            fs::read_to_string(&log_path).unwrap(),
            "line 1\nline 2\nline 3\nline 4\n"
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn appended_raw_log_bytes_uses_suffix_prefix_overlap() {
        assert_eq!(appended_raw_log_bytes(b"", b"line 1\n"), b"line 1\n");
        assert_eq!(appended_raw_log_bytes(b"line 1\n", b"line 1\n"), b"");
        assert_eq!(
            appended_raw_log_bytes(b"line 1\nline 2\n", b"line 2\nline 3\n"),
            b"line 3\n"
        );
        assert_eq!(appended_raw_log_bytes(b"old\n", b"new\n"), b"new\n");
    }

    #[test]
    fn runs_wait_for_delete_treats_404_as_success() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_raw_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/v1/runs/run%2D1/runtime/resources/Exec/exec%2D1/status"
            );
            (
                404,
                "application/json",
                br#"{"message":"runtime resource not found","code":"not_found"}"#.to_vec(),
            )
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_wait_delete_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "wait".to_string(),
            "run-1".to_string(),
            "Exec/exec-1".to_string(),
            "--for".to_string(),
            "delete".to_string(),
            "--timeout-seconds".to_string(),
            "1".to_string(),
        ])
        .expect("hosted run wait --for delete should succeed after status returns 404");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_wait_for_delete_treats_deletion_timestamp_as_success() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2D1/status"
            );
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceStatus",
                "cloud_run_id": "run-1",
                "core_run_ids": [],
                "resource_ref": {
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "PortForward",
                    "name": "pf-1",
                    "uid": "pf-1"
                },
                "generation": 2,
                "observedGeneration": 2,
                "resourceVersion": "sha256:pf-cancelled",
                "deletionTimestamp": "2026-06-19T12:00:00Z",
                "phase": "cancelled",
                "reason": "Cancelled",
                "message": "Access request was cancelled",
                "conditions": [],
                "actions": [],
                "status": { "phase": "cancelled" },
                "audit": { "source": "cloud.runtime_access_requests" }
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_wait_delete_timestamp_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "wait".to_string(),
            "run-1".to_string(),
            "PortForward/pf-1".to_string(),
            "--for".to_string(),
            "delete".to_string(),
            "--timeout-seconds".to_string(),
            "1".to_string(),
        ])
        .expect(
            "hosted run wait --for delete should succeed after status returns deletionTimestamp",
        );

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_wait_terminal_failure_surfaces_latest_status_evidence() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/status"
            );
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceStatus",
                "cloud_run_id": "run-1",
                "core_run_ids": ["core-run-1"],
                "resource_ref": {
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RunnerInstance",
                    "name": "runner-1",
                    "uid": "runner-uid-1"
                },
                "generation": 9,
                "observedGeneration": 7,
                "resourceVersion": "sha256:runner-waiting",
                "phase": "failed",
                "reason": "WorkerLost",
                "message": "runner VM stopped while waiting",
                "conditions": [
                    {
                        "type": "Ready",
                        "status": "False",
                        "reason": "Reconciling",
                        "message": "runner not ready"
                    }
                ],
                "actions": ["cordon"],
                "status": { "phase": "running" },
                "audit": { "source": "cloud.runtime_resources" }
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_wait_terminal_failure_evidence_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "runs".to_string(),
            "wait".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--for".to_string(),
            "condition=Ready".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains(
            "runtime resource RunnerInstance/runner-1 reached terminal phase=failed before condition=Ready=True"
        ));
        assert!(err.contains("latest phase=failed"));
        assert!(err.contains("reason=WorkerLost"));
        assert!(err.contains("condition=Ready=False"));
        assert!(err.contains("resource_version=sha256:runner-waiting"));
        assert!(err.contains("generation=9/7 freshness=stale"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_explain_fetches_runtime_api_resource_contract() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/v1/runs/run%2D1/runtime/api-resources/container"
            );
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "cloud_run_id": "run-1",
                "generated_at": "2026-06-18T00:00:00Z",
                "core_run_ids": ["core-run-1"],
                "group": "bucephalus.dev",
                "version": "v1alpha1",
                "name": "trialcontainers",
                "singularName": "trialcontainer",
                "namespaced": false,
                "scope": "run",
                "kind": "TrialContainer",
                "shortNames": ["container"],
                "categories": ["trial", "access-target"],
                "verbs": ["list", "get", "watch", "describe"],
                "subresources": ["logs", "port-forward", "exec"],
                "actions": [],
                "access": ["logs", "port-forward", "exec"],
                "supports": { "list": true, "get": true, "watch": true, "describe": true, "create": false, "delete": false, "actions": false, "access": true },
                "pathTemplates": {
                    "list": "/v1/runs/{run_id}/runtime/resources?kind=TrialContainer",
                    "get": "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}",
                    "logs": "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}/logs"
                },
                "exampleCommands": [
                    { "purpose": "list", "command": "buc runs get {run_id} TrialContainer" },
                    { "purpose": "wait", "command": "buc runs wait {run_id} TrialContainer/{name} --for condition=Ready" }
                ],
                "printerColumns": [
                    { "name": "Phase", "type": "string", "jsonPath": ".status.phase", "description": "Lifecycle phase.", "priority": 0 },
                    { "name": "Exec", "type": "boolean", "jsonPath": ".status.access.exec", "description": "Whether exec is supported.", "priority": 0 }
                ],
                "fieldSelectors": ["status.phase", "status.access.exec"],
                "labelSelectors": ["bucephalus.dev/run-id"],
                "labelSelector": true,
                "count": 3,
                "description": "Trial container identity with stdout/stderr log access."
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_explain_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "explain".to_string(),
            "run-1".to_string(),
            "container".to_string(),
        ])
        .expect("hosted run explain should fetch one runtime API resource contract");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runtime_api_resource_explain_surfaces_subresource_path_templates() {
        let lines = runtime_api_resource_explain_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "cloud_run_id": "run-1",
            "generated_at": "2026-06-18T00:00:00Z",
            "core_run_ids": ["core-run-1"],
            "kind": "RunnerInstance",
            "name": "runnerinstances",
            "singularName": "runnerinstance",
            "scope": "run",
            "count": 1,
            "verbs": ["list", "get", "watch", "describe"],
            "subresources": ["status", "logs", "actions/cordon"],
            "pathTemplates": {
                "collection": "/v1/runs/{run_id}/runtime/resources?kind=RunnerInstance",
                "resource": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}?view=resource",
                "describe": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}",
                "operationReview": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/operations/{operation}",
                "watch": "/v1/runs/{run_id}/runtime/resources/watch?kind=RunnerInstance",
                "subresources": {
                    "status": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/status",
                    "logs": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/logs",
                    "actions/cordon": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/actions/cordon"
                }
            }
        }));

        assert!(lines.contains(&"paths:".to_string()), "{lines:?}");
        assert!(
            lines.contains(&"generated_at: 2026-06-18T00:00:00Z".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"core_run_ids: core-run-1".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"  - subresource/actions/cordon: /v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/actions/cordon".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"  - subresource/logs: /v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/logs".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"  - subresource/status: /v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/status".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn runtime_api_resources_summary_surfaces_operator_catalog_fields() {
        let lines = runtime_api_resources_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeApiResourceList",
            "cloud_run_id": "run-1",
            "generated_at": "2026-06-18T00:00:00Z",
            "core_run_ids": ["core-run-1"],
            "resources": [
                {
                    "kind": "RunnerInstance",
                    "name": "runnerinstances",
                    "shortNames": ["runner"],
                    "categories": ["runner", "access-target"],
                    "verbs": ["list", "get", "watch", "describe"],
                    "subresources": ["status", "events", "metrics", "logs", "actions/cordon"],
                    "actions": ["cordon", "drain", "uncordon"],
                    "access": ["logs", "port-forward", "exec"],
                    "supports": { "labelSelector": true, "fieldSelector": true },
                    "count": 2
                },
                {
                    "kind": "PortForward",
                    "name": "portforwards",
                    "shortNames": ["pf"],
                    "categories": ["access"],
                    "verbs": ["list", "get", "watch", "describe", "create", "delete"],
                    "subresources": ["status", "events", "metrics", "actions/cancel", "actions/complete"],
                    "actions": ["cancel", "complete"],
                    "access": [],
                    "supports": { "labelSelector": true, "fieldSelector": true },
                    "count": 1
                }
            ]
        }))
        .expect("api resource list lines");

        assert_eq!(
            lines,
            vec![
                "api_resources: 2".to_string(),
                "generated_at: 2026-06-18T00:00:00Z".to_string(),
                "core_run_ids: core-run-1".to_string(),
                "  - RunnerInstance runnerinstances count=2 short=runner categories=runner,access-target verbs=list,get,watch,describe subresources=status,events,metrics,logs,actions/cordon actions=cordon,drain,uncordon access=logs,port-forward,exec selectors=label,field".to_string(),
                "  - PortForward portforwards count=1 short=pf categories=access verbs=list,get,watch,describe,create,delete subresources=status,events,metrics,actions/cancel,actions/complete actions=cancel,complete selectors=label,field".to_string(),
            ]
        );
    }

    #[test]
    fn runs_tree_fetches_bounded_owner_reference_inventory() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                path_with_query(
                    "/v1/runs/run%2D1/runtime/resources",
                    &[
                        ("limit", Some("1000".to_string())),
                        ("continue", None),
                        ("kind", Some("Trial,TrialContainer".to_string())),
                        ("label_selector", None),
                        (
                            "field_selector",
                            Some("metadata.ownerReferences.kind=Trial".to_string())
                        ),
                    ],
                )
            );
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceList",
                "cloud_run_id": "run-1",
                "core_run_ids": ["core-run-1"],
                "metadata": { "resourceVersion": "rv-tree", "continue": null, "remainingItemCount": 0, "total": 2, "returned": 2 },
                "resources": [{
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "Trial",
                    "metadata": { "name": "trial-1", "uid": "trial-1", "ownerReferences": [] },
                    "status": { "phase": "running", "conditions": [{ "type": "Ready", "status": "True" }] }
                }, {
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "TrialContainer",
                    "metadata": {
                        "name": "trial-1.agent.container-1",
                        "uid": "container-1",
                        "ownerReferences": [{ "apiVersion": "bucephalus.dev/v1alpha1", "kind": "Trial", "name": "trial-1", "uid": "trial-1" }]
                    },
                    "status": { "phase": "running", "conditions": [{ "type": "Ready", "status": "True" }] }
                }]
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_tree_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "tree".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "Trial,TrialContainer".to_string(),
            "--field-selector".to_string(),
            "metadata.ownerReferences.kind=Trial".to_string(),
        ])
        .expect("hosted run tree should fetch runtime resources and render owner refs");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_resource_commands_get_hosted_runtime_paths() {
        let _lock = lock_env();
        let server = MockCloudServer::start(17);
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
            "api-resources".to_string(),
            "run-1".to_string(),
        ])
        .expect("hosted run API resource discovery should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "RunnerInstance".to_string(),
            "--limit".to_string(),
            "9".to_string(),
        ])
        .expect("hosted run resources should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "describe".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
        ])
        .expect("hosted run resource describe should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "status".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
        ])
        .expect("hosted run resource status should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "wait".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--for".to_string(),
            "phase=running".to_string(),
            "--timeout-seconds".to_string(),
            "1".to_string(),
        ])
        .expect("hosted run resource wait should use the status subresource");
        run(vec![
            "runs".to_string(),
            "can-i".to_string(),
            "run-1".to_string(),
            "port-forward".to_string(),
            "RunnerInstance/runner-1".to_string(),
        ])
        .expect("hosted run operation review should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "health".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "RunnerInstance".to_string(),
        ])
        .expect("hosted run resource health should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "top".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "RunnerInstance".to_string(),
            "--limit".to_string(),
            "3".to_string(),
        ])
        .expect("hosted run top should use collection runtime metrics");
        run(vec![
            "runs".to_string(),
            "events".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--limit".to_string(),
            "5".to_string(),
            "--after-row-seq".to_string(),
            "12".to_string(),
        ])
        .expect("hosted run resource events should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "audit".to_string(),
            "run-1".to_string(),
            "--limit".to_string(),
            "7".to_string(),
        ])
        .expect("hosted run audit should filter runtime lifecycle/access events");
        run(vec![
            "runs".to_string(),
            "port-forward".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--target-port".to_string(),
            "8080".to_string(),
            "--resource-version".to_string(),
            "sha256:runner-pf".to_string(),
        ])
        .expect("hosted run port-forward should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "exec".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--resource-version".to_string(),
            "sha256:runner-exec".to_string(),
            "--".to_string(),
            "python".to_string(),
            "-V".to_string(),
        ])
        .expect("hosted run exec should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "action".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "cordon".to_string(),
            "--resource-version".to_string(),
            "sha256:runner-cordon".to_string(),
        ])
        .expect("hosted run resource action should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "delete".to_string(),
            "run-1".to_string(),
            "PortForward/pf-1".to_string(),
            "--resource-version".to_string(),
            "sha256:pf-delete".to_string(),
        ])
        .expect("hosted run resource delete should complete against mock Cloud API");
        run(vec![
            "runs".to_string(),
            "inspect".to_string(),
            "run-1".to_string(),
            "--event-limit".to_string(),
            "25".to_string(),
        ])
        .expect("hosted run inspect should complete against mock Cloud API");

        let requests = server.join();
        assert_eq!(requests.len(), 17);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/v1/runs/run%2D1/runtime/api-resources");
        assert_eq!(requests[1].method, "GET");
        assert_eq!(
            requests[1].path,
            "/v1/runs/run%2D1/runtime/resources?limit=9&kind=RunnerInstance"
        );
        assert_eq!(
            requests[2].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1"
        );
        assert_eq!(
            requests[3].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/status"
        );
        assert_eq!(
            requests[4].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/status"
        );
        assert_eq!(
            requests[5].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/operations/port%2Dforward"
        );
        assert_eq!(
            requests[6].path,
            "/v1/runs/run%2D1/runtime/resources/health?kind=RunnerInstance"
        );
        assert_eq!(
            requests[7].path,
            "/v1/runs/run%2D1/runtime/resources/metrics?limit=3&kind=RunnerInstance"
        );
        assert_eq!(
            requests[8].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/events?limit=5&after%5Frow%5Fseq=12"
        );
        assert_eq!(
            requests[9].path,
            path_with_query(
                "/v1/runs/run%2D1/runtime/events",
                &[
                    ("limit", Some("7".to_string())),
                    ("event_type", Some(RUNTIME_AUDIT_EVENT_TYPES.to_string())),
                ],
            )
        );
        assert_eq!(requests[10].method, "POST");
        assert_eq!(
            requests[10].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/port-forward"
        );
        let port_forward_body: Value = serde_json::from_slice(&requests[10].body).unwrap();
        assert_eq!(port_forward_body["target_port"], json!(8080));
        assert_eq!(
            port_forward_body["resource_version"],
            json!("sha256:runner-pf")
        );
        assert_eq!(requests[11].method, "GET");
        assert_eq!(
            requests[11].path,
            "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2D1"
        );
        assert_eq!(requests[12].method, "POST");
        assert_eq!(
            requests[12].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/exec"
        );
        let exec_body: Value = serde_json::from_slice(&requests[12].body).unwrap();
        assert_eq!(exec_body["command"], json!(["python", "-V"]));
        assert_eq!(exec_body["resource_version"], json!("sha256:runner-exec"));
        assert_eq!(requests[13].method, "GET");
        assert_eq!(
            requests[13].path,
            "/v1/runs/run%2D1/runtime/resources/Exec/exec%2D1"
        );
        assert_eq!(requests[14].method, "POST");
        assert_eq!(
            requests[14].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/actions/cordon"
        );
        let action_body: Value = serde_json::from_slice(&requests[14].body).unwrap();
        assert_eq!(
            action_body["resource_version"],
            json!("sha256:runner-cordon")
        );
        assert_eq!(requests[15].method, "DELETE");
        assert_eq!(
            requests[15].path,
            "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2D1"
        );
        let delete_body: Value = serde_json::from_slice(&requests[15].body).unwrap();
        assert_eq!(delete_body["resource_version"], json!("sha256:pf-delete"));
        assert_eq!(requests[16].method, "GET");
        assert_eq!(
            requests[16].path,
            "/v1/runs/run%2D1/runtime/inspect?event%5Flimit=25"
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_events_forwards_repeated_filter_options() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                path_with_query(
                    "/v1/runs/run%2D1/runtime/events",
                    &[
                        ("limit", Some("11".to_string())),
                        ("continue", Some("cursor-1".to_string())),
                        (
                            "event_type",
                            Some("runtime.access.exec.requested".to_string()),
                        ),
                        (
                            "event_type",
                            Some("runtime.access.exec.completed".to_string()),
                        ),
                        ("source", Some("cloud.run_events".to_string())),
                        ("source", Some("runtime.event_rows".to_string())),
                        ("resource_kind", Some("PortForward".to_string())),
                        ("resource_name", Some("pf-1".to_string())),
                        ("trial_id", Some("trial-1".to_string())),
                        ("task_id", Some("task-1".to_string())),
                    ],
                )
            );
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeEventList",
                "cloud_run_id": "run-1",
                "events": []
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_events_filter_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "events".to_string(),
            "run-1".to_string(),
            "--limit".to_string(),
            "11".to_string(),
            "--continue".to_string(),
            "cursor-1".to_string(),
            "--event-type".to_string(),
            "runtime.access.exec.requested".to_string(),
            "--event-type".to_string(),
            "runtime.access.exec.completed".to_string(),
            "--source".to_string(),
            "cloud.run_events".to_string(),
            "--source".to_string(),
            "runtime.event_rows".to_string(),
            "--resource-kind".to_string(),
            "PortForward".to_string(),
            "--resource-name".to_string(),
            "pf-1".to_string(),
            "--trial-id".to_string(),
            "trial-1".to_string(),
            "--task-id".to_string(),
            "task-1".to_string(),
            "--json".to_string(),
        ])
        .expect("hosted run events should forward repeated event/source filters");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_events_follow_uses_event_cursors() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(2, |request, index| {
            assert_eq!(request.method, "GET");
            match index {
                0 => {
                    assert_eq!(
                        request.path,
                        path_with_query(
                            "/v1/runs/run%2D1/runtime/events",
                            &[("limit", Some("1".to_string()))],
                        )
                    );
                    json!({
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RuntimeEventList",
                        "cloud_run_id": "run-1",
                        "metadata": {
                            "resourceVersion": "event-row-seq:10",
                            "continue": null,
                            "after_row_seq": null,
                            "next_after_row_seq": 10,
                            "remainingItemCount": 0,
                            "limit": 1,
                            "returned": 1
                        },
                        "events": [{ "row_seq": 10, "event_type": "runtime.access.exec.requested" }]
                    })
                }
                1 => {
                    assert_eq!(
                        request.path,
                        path_with_query(
                            "/v1/runs/run%2D1/runtime/events",
                            &[
                                ("limit", Some("1".to_string())),
                                ("continue", Some("event-row-seq:10".to_string())),
                            ],
                        )
                    );
                    json!({
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RuntimeEventList",
                        "cloud_run_id": "run-1",
                        "metadata": {
                            "resourceVersion": "event-row-seq:11",
                            "continue": null,
                            "after_row_seq": 10,
                            "next_after_row_seq": 11,
                            "remainingItemCount": 0,
                            "limit": 1,
                            "returned": 1
                        },
                        "events": [{ "row_seq": 11, "event_type": "runtime.access.exec.completed" }]
                    })
                }
                _ => unreachable!("unexpected mock request"),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_events_follow_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "events".to_string(),
            "run-1".to_string(),
            "--limit".to_string(),
            "1".to_string(),
            "--follow".to_string(),
            "--max-polls".to_string(),
            "2".to_string(),
            "--interval-seconds".to_string(),
            "0".to_string(),
        ])
        .expect("hosted run events should follow event cursors");

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_watch_forwards_selectors_and_repeated_known_resources() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                path_with_query(
                    "/v1/runs/run%2D1/runtime/resources/watch",
                    &[
                        ("resource_version", Some("rv-list".to_string())),
                        ("kind", Some("PortForward".to_string())),
                        (
                            "label_selector",
                            Some("bucephalus.dev/run-id=run-1".to_string()),
                        ),
                        (
                            "field_selector",
                            Some("status.conditions.ClientReachable!=True".to_string()),
                        ),
                        ("known_resource", Some("PortForward/pf-1=rv-pf".to_string()),),
                        ("known_resource", Some("Exec/exec-1=rv-exec".to_string()),),
                    ],
                )
            );
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceWatchList",
                "cloud_run_id": "run-1",
                "resource_versions": {},
                "events": [],
                "resource_inventory": {
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceList",
                    "metadata": { "resourceVersion": "rv-list", "continue": null, "remainingItemCount": 0, "total": 0, "returned": 0 },
                    "cloud_run_id": "run-1",
                    "core_run_ids": [],
                    "resources": []
                }
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_watch_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "watch".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "PortForward".to_string(),
            "--label-selector".to_string(),
            "bucephalus.dev/run-id=run-1".to_string(),
            "--field-selector".to_string(),
            "status.conditions.ClientReachable!=True".to_string(),
            "--resource-version".to_string(),
            "rv-list".to_string(),
            "--known-resource".to_string(),
            "PortForward/pf-1=rv-pf".to_string(),
            "--known-resource".to_string(),
            "Exec/exec-1=rv-exec".to_string(),
            "--json".to_string(),
        ])
        .expect("hosted run watch should forward selectors and all known resources");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_watch_follow_polls_with_returned_resource_cursors() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(2, |request, index| match index {
            0 => {
                assert_eq!(request.method, "GET");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/watch?kind=RunnerInstance&allow%5Fbookmarks=true"
                );
                json!({
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceWatchList",
                    "cloud_run_id": "run-1",
                    "resource_versions": {
                        "runnerinstance/runner-1": "rv-runner-1",
                        "exec/exec-1": "rv-exec-1"
                    },
                    "events": [{
                        "type": "ADDED",
                        "resource_ref": { "kind": "RunnerInstance", "name": "runner-1" },
                        "resource_version": "rv-runner-1"
                    }],
                    "resource_inventory": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RuntimeResourceList",
                        "metadata": { "resourceVersion": "rv-list-1", "continue": null, "remainingItemCount": 0, "total": 1, "returned": 1 },
                        "cloud_run_id": "run-1",
                        "core_run_ids": [],
                        "resources": []
                    }
                })
            }
            1 => {
                assert_eq!(request.method, "GET");
                assert_eq!(
                    request.path,
                    path_with_query(
                        "/v1/runs/run%2D1/runtime/resources/watch",
                        &[
                            ("resource_version", Some("rv-list-1".to_string())),
                            ("kind", Some("RunnerInstance".to_string())),
                            ("known_resource", Some("exec/exec-1=rv-exec-1".to_string())),
                            (
                                "known_resource",
                                Some("runnerinstance/runner-1=rv-runner-1".to_string())
                            ),
                            ("allow_bookmarks", Some("true".to_string())),
                        ],
                    )
                );
                json!({
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceWatchList",
                    "cloud_run_id": "run-1",
                    "resource_versions": {
                        "runnerinstance/runner-1": "rv-runner-2"
                    },
                    "events": [{
                        "type": "MODIFIED",
                        "resource_ref": { "kind": "RunnerInstance", "name": "runner-1" },
                        "resource_version": "rv-runner-2",
                        "previous_resource_version": "rv-runner-1"
                    }],
                    "resource_inventory": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RuntimeResourceList",
                        "metadata": { "resourceVersion": "rv-list-2", "continue": null, "remainingItemCount": 0, "total": 1, "returned": 1 },
                        "cloud_run_id": "run-1",
                        "core_run_ids": [],
                        "resources": []
                    }
                })
            }
            _ => unreachable!(),
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_watch_follow_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "watch".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "RunnerInstance".to_string(),
            "--follow".to_string(),
            "--max-polls".to_string(),
            "2".to_string(),
            "--interval-seconds".to_string(),
            "0".to_string(),
        ])
        .expect("hosted run resource watch should follow resource-version cursors");

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runtime_subresource_commands_reject_mismatched_cloud_run_ids() {
        fn assert_mismatched_runtime_response_fails(
            label: &str,
            args: &[&str],
            expected_path: &'static str,
            response: Value,
        ) {
            let server = MockCloudServer::start_with_stateful_handler(1, move |request, _index| {
                assert_eq!(request.method, "GET");
                assert_eq!(request.path, expected_path);
                response.clone()
            });
            let api_url = server.api_url();
            let home = temp_dir(label);
            let home_s = home.display().to_string();
            let _env = EnvVarGuard::set(&[
                ("BUCEPHALUS_HOME", Some(home_s.as_str())),
                (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
                (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
            ]);

            let err = run(args.iter().map(|arg| (*arg).to_string()).collect())
                .expect_err("mismatched runtime cloud_run_id should fail before rendering")
                .to_string();
            assert!(
                err.contains(
                    "runtime response run id mismatch: requested run-1, API returned run-2"
                ),
                "expected runtime run-id mismatch for {label}, got {err}"
            );
            let requests = server.join();
            assert_eq!(requests.len(), 1);
            let _ = fs::remove_dir_all(home);
        }

        assert_mismatched_runtime_response_fails(
            "runtime_status_mismatched_run_home",
            &["runs", "status", "run-1", "Trial/trial-1", "--json"],
            "/v1/runs/run%2D1/runtime/resources/Trial/trial%2D1/status",
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceStatus",
                "cloud_run_id": "run-2",
                "resource_ref": { "kind": "Trial", "name": "trial-1" },
                "phase": "running",
                "conditions": []
            }),
        );
        assert_mismatched_runtime_response_fails(
            "runtime_wait_mismatched_run_home",
            &[
                "runs",
                "wait",
                "run-1",
                "Trial/trial-1",
                "--for",
                "phase=running",
                "--timeout-seconds",
                "1",
                "--interval-seconds",
                "1",
                "--json",
            ],
            "/v1/runs/run%2D1/runtime/resources/Trial/trial%2D1/status",
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceStatus",
                "cloud_run_id": "run-2",
                "resource_ref": { "kind": "Trial", "name": "trial-1" },
                "phase": "running",
                "conditions": []
            }),
        );
        assert_mismatched_runtime_response_fails(
            "runtime_can_i_mismatched_run_home",
            &["runs", "can-i", "run-1", "exec", "Trial/trial-1", "--json"],
            "/v1/runs/run%2D1/runtime/resources/Trial/trial%2D1/operations/exec",
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceOperationReview",
                "cloud_run_id": "run-2",
                "resource_ref": { "kind": "Trial", "name": "trial-1" },
                "operation": "exec",
                "matched_operation": "exec",
                "supported": true,
                "resource_version": "sha256:review"
            }),
        );
        assert_mismatched_runtime_response_fails(
            "runtime_watch_mismatched_run_home",
            &["runs", "watch", "run-1", "--kind", "Trial", "--json"],
            "/v1/runs/run%2D1/runtime/resources/watch?kind=Trial",
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceWatchList",
                "cloud_run_id": "run-2",
                "resource_versions": {},
                "events": [],
                "resource_inventory": {
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceList",
                    "cloud_run_id": "run-2",
                    "core_run_ids": [],
                    "metadata": {
                        "resourceVersion": "rv-list",
                        "continue": null,
                        "remainingItemCount": 0,
                        "total": 0,
                        "returned": 0
                    },
                    "resources": []
                }
            }),
        );
    }

    #[test]
    fn runs_mutating_resource_commands_send_reason_and_resource_version() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(4, |request, index| match index {
            0 => {
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/Exec/exec%2D1/actions/cancel"
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body,
                    json!({
                        "reason": "stop debug command",
                        "resource_version": "sha256:exec-review"
                    })
                );
                json!({
                    "cloud_run_id": "run-1",
                    "resource": runtime_resource("Exec", "exec-1"),
                    "operations": [],
                    "related_resources": [],
                    "event_list": { "events": [] }
                })
            }
            1 => {
                assert_eq!(request.method, "DELETE");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2D1"
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body,
                    json!({
                        "reason": "done debugging",
                        "resource_version": "sha256:pf-review"
                    })
                );
                json!({
                    "cloud_run_id": "run-1",
                    "resource": runtime_resource("PortForward", "pf-1"),
                    "operations": [],
                    "related_resources": [],
                    "event_list": { "events": [] }
                })
            }
            2 => {
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2Dactive/actions/complete"
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body,
                    json!({
                        "reason": "local tunnel exited",
                        "resource_version": "sha256:pf-active-review"
                    })
                );
                json!({
                    "cloud_run_id": "run-1",
                    "resource": runtime_access_resource(
                        "PortForward",
                        "pf-active",
                        "completed",
                        json!({ "mode": "gcp_iap_ssh" }),
                    ),
                    "operations": [],
                    "related_resources": [],
                    "event_list": { "events": [] }
                })
            }
            3 => {
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2Dcompat/actions/complete"
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body,
                    json!({
                        "reason": "compat local tunnel exited",
                        "resource_version": "sha256:pf-compat-review"
                    })
                );
                json!({
                    "cloud_run_id": "run-1",
                    "resource": runtime_access_resource(
                        "PortForward",
                        "pf-compat",
                        "completed",
                        json!({ "mode": "gcp_iap_ssh" }),
                    ),
                    "operations": [],
                    "related_resources": [],
                    "event_list": { "events": [] }
                })
            }
            _ => unreachable!(),
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_mutating_resource_body_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "cancel".to_string(),
            "run-1".to_string(),
            "Exec/exec-1".to_string(),
            "--reason".to_string(),
            "stop debug command".to_string(),
            "--resource-version".to_string(),
            "sha256:exec-review".to_string(),
        ])
        .expect("runs cancel should send optimistic concurrency and audit reason");
        run(vec![
            "runs".to_string(),
            "delete".to_string(),
            "run-1".to_string(),
            "PortForward/pf-1".to_string(),
            "--reason".to_string(),
            "done debugging".to_string(),
            "--resource-version".to_string(),
            "sha256:pf-review".to_string(),
        ])
        .expect("runs delete should send optimistic concurrency and audit reason");
        run(vec![
            "runs".to_string(),
            "complete".to_string(),
            "run-1".to_string(),
            "PortForward/pf-active".to_string(),
            "--reason".to_string(),
            "local tunnel exited".to_string(),
            "--resource-version".to_string(),
            "sha256:pf-active-review".to_string(),
        ])
        .expect("runs complete should send optimistic concurrency and audit reason");
        run(vec![
            "runs".to_string(),
            "action".to_string(),
            "run-1".to_string(),
            "PortForward/pf-compat".to_string(),
            "complete".to_string(),
            "--reason".to_string(),
            "compat local tunnel exited".to_string(),
            "--resource-version".to_string(),
            "sha256:pf-compat-review".to_string(),
        ])
        .expect(
            "runs action should send complete compatibility command with optimistic concurrency",
        );

        let requests = server.join();
        assert_eq!(requests.len(), 4);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_mutating_resource_commands_require_reviewed_resource_version_before_api_request() {
        let _lock = lock_env();
        let home = temp_dir("runs_mutating_resource_version_required_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, None),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);
        let cases = [
            (
                vec![
                    "runs",
                    "port-forward",
                    "run-1",
                    "Trial/trial-1",
                    "--target-port",
                    "8080",
                    "--no-wait",
                ],
                "runtime port-forward requires --resource-version",
            ),
            (
                vec![
                    "runs",
                    "exec",
                    "run-1",
                    "Trial/trial-1",
                    "--no-wait",
                    "--",
                    "python",
                    "-V",
                ],
                "runtime exec requires --resource-version",
            ),
            (
                vec![
                    "runs",
                    "action",
                    "run-1",
                    "RunnerInstance/runner-1",
                    "cordon",
                ],
                "runtime cordon requires --resource-version",
            ),
            (
                vec!["runs", "delete", "run-1", "PortForward/pf-1"],
                "runtime delete requires --resource-version",
            ),
            (
                vec!["runs", "complete", "run-1", "PortForward/pf-1"],
                "runtime complete requires --resource-version",
            ),
        ];

        for (args, expected) in cases {
            let err = run(args.into_iter().map(String::from).collect())
                .expect_err("runtime mutation without resource version should fail locally");
            let message = err.to_string();
            assert!(
                message.contains(expected),
                "expected {expected:?} in {message:?}"
            );
            assert!(
                message.contains("buc runs can-i") && message.contains("buc runs describe"),
                "missing resource-version error should point to review commands: {message}"
            );
        }
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_can_i_exits_nonzero_when_operation_review_denies() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/v1/runs/run%2D1/runtime/resources/Trial/trial%2D1/operations/exec"
            );
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceOperationReview",
                "cloud_run_id": "run-1",
                "resource_ref": {
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "Trial",
                    "name": "trial-1"
                },
                "operation": "exec",
                "matched_operation": "exec",
                "supported": false,
                "reason": "runtime_exec_unavailable",
                "message": "exec requires an active runner attempt whose runner advertises runtime_exec",
                "resource_version": "sha256:review"
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_can_i_denied_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "runs".to_string(),
            "can-i".to_string(),
            "run-1".to_string(),
            "exec".to_string(),
            "Trial/trial-1".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err.contains("runtime operation exec is not supported for Trial/trial-1"));
        assert!(err.contains("runner advertises runtime_exec"));
        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_can_i_reviews_observability_operations() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(5, |request, index| match index {
            0 => {
                assert_eq!(request.method, "GET");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/TrialContainer/trial%2D1%2Eagent%2Econtainer%2D1/operations/top"
                );
                json!({
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceOperationReview",
                    "cloud_run_id": "run-1",
                    "resource_ref": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "TrialContainer",
                        "name": "trial-1.agent.container-1"
                    },
                    "operation": "top",
                    "matched_operation": "top",
                    "supported": true,
                    "reason": null,
                    "message": null,
                    "verb": "get",
                    "subresource": "metrics",
                    "action": null,
                    "requires_running_run": false,
                    "resource_version": "sha256:top-review",
                    "command": "buc runs top run-1 --kind TrialContainer"
                })
            }
            1 => {
                assert_eq!(request.method, "GET");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/operations/audit"
                );
                json!({
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceOperationReview",
                    "cloud_run_id": "run-1",
                    "resource_ref": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RunnerInstance",
                        "name": "runner-1"
                    },
                    "operation": "audit",
                    "matched_operation": "audit",
                    "supported": true,
                    "reason": null,
                    "message": null,
                    "verb": "watch",
                    "subresource": "events",
                    "action": null,
                    "requires_running_run": false,
                    "resource_version": "sha256:audit-review",
                    "command": "buc runs audit run-1 RunnerInstance/runner-1"
                })
            }
            2 => {
                assert_eq!(request.method, "GET");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/TrialContainer/trial%2D1%2Eagent%2Econtainer%2D1/operations/logs%2Fstdout"
                );
                json!({
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceOperationReview",
                    "cloud_run_id": "run-1",
                    "resource_ref": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "TrialContainer",
                        "name": "trial-1.agent.container-1"
                    },
                    "operation": "logs/stdout",
                    "matched_operation": "logs/stdout",
                    "supported": true,
                    "reason": null,
                    "message": null,
                    "verb": "get",
                    "subresource": "logs",
                    "action": null,
                    "requires_running_run": false,
                    "resource_version": "sha256:stdout-review",
                    "command": "buc runs logs run-1 TrialContainer/trial-1.agent.container-1 --stream stdout --metadata-out FILE.metadata.json"
                })
            }
            3 => {
                assert_eq!(request.method, "GET");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/TrialContainer/trial%2D1%2Eagent%2Econtainer%2D1/operations/logs%2Fstderr"
                );
                json!({
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceOperationReview",
                    "cloud_run_id": "run-1",
                    "resource_ref": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "TrialContainer",
                        "name": "trial-1.agent.container-1"
                    },
                    "operation": "logs/stderr",
                    "matched_operation": "logs/stderr",
                    "supported": true,
                    "reason": null,
                    "message": null,
                    "verb": "get",
                    "subresource": "logs",
                    "action": null,
                    "requires_running_run": false,
                    "resource_version": "sha256:stderr-review",
                    "command": "buc runs logs run-1 TrialContainer/trial-1.agent.container-1 --stream stderr --metadata-out FILE.metadata.json"
                })
            }
            4 => {
                assert_eq!(request.method, "GET");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/TrialArtifact/trial%2D1%2Eagent%2Dresult%2Esha256%2Dbbbbbbbb/operations/content"
                );
                json!({
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceOperationReview",
                    "cloud_run_id": "run-1",
                    "resource_ref": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "TrialArtifact",
                        "name": "trial-1.agent-result.sha256-bbbbbbbb"
                    },
                    "operation": "content",
                    "matched_operation": "content",
                    "supported": true,
                    "reason": null,
                    "message": null,
                    "verb": "get",
                    "subresource": "content",
                    "action": null,
                    "requires_running_run": false,
                    "resource_version": "sha256:content-review",
                    "command": "buc runs content run-1 TrialArtifact/trial-1.agent-result.sha256-bbbbbbbb --out FILE --metadata-out FILE.metadata.json"
                })
            }
            _ => panic!(
                "unexpected mock Cloud API request #{index}: {} {}",
                request.method, request.path
            ),
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_can_i_observability_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "can-i".to_string(),
            "run-1".to_string(),
            "top".to_string(),
            "TrialContainer/trial-1.agent.container-1".to_string(),
        ])
        .expect("can-i should review runtime top through the operation subresource");
        run(vec![
            "runs".to_string(),
            "can-i".to_string(),
            "run-1".to_string(),
            "audit".to_string(),
            "RunnerInstance/runner-1".to_string(),
        ])
        .expect("can-i should review runtime audit through the operation subresource");
        run(vec![
            "runs".to_string(),
            "can-i".to_string(),
            "run-1".to_string(),
            "logs/stdout".to_string(),
            "TrialContainer/trial-1.agent.container-1".to_string(),
        ])
        .expect("can-i should review runtime stdout logs through the operation subresource");
        run(vec![
            "runs".to_string(),
            "can-i".to_string(),
            "run-1".to_string(),
            "logs/stderr".to_string(),
            "TrialContainer/trial-1.agent.container-1".to_string(),
        ])
        .expect("can-i should review runtime stderr logs through the operation subresource");
        run(vec![
            "runs".to_string(),
            "can-i".to_string(),
            "run-1".to_string(),
            "content".to_string(),
            "TrialArtifact/trial-1.agent-result.sha256-bbbbbbbb".to_string(),
        ])
        .expect("can-i should review runtime artifact content through the operation subresource");

        let requests = server.join();
        assert_eq!(requests.len(), 5);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_can_i_reviews_runtime_access_cancel_operation() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/v1/runs/run%2D1/runtime/resources/Exec/exec%2D1/operations/cancel"
            );
            json!({
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceOperationReview",
                "cloud_run_id": "run-1",
                "resource_ref": {
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "Exec",
                    "name": "exec-1"
                },
                "operation": "cancel",
                "matched_operation": "cancel",
                "supported": true,
                "reason": null,
                "message": null,
                "verb": null,
                "subresource": "actions/cancel",
                "action": "cancel",
                "requires_running_run": false,
                "resource_version": "sha256:exec-cancel-review",
                "command": "buc runs cancel run-1 Exec/exec-1 --resource-version sha256:exec-cancel-review"
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_can_i_cancel_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "can-i".to_string(),
            "run-1".to_string(),
            "cancel".to_string(),
            "Exec/exec-1".to_string(),
        ])
        .expect("can-i should review runtime access cancel through the operation subresource");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn can_i_summary_prints_reviewed_command_and_resource_version() {
        let summary = runtime_operation_review_summary(
            &json!({
                "kind": "RuntimeResourceOperationReview",
                "operation": "exec",
                "matched_operation": "exec",
                "supported": true,
                "verb": "create",
                "subresource": "exec",
                "action": null,
                "requires_running_run": true,
                "resource_generation": 12,
                "observed_generation": 12,
                "generated_at": "2026-06-18T00:00:00Z",
                "core_run_ids": ["core-run-1"],
                "resource_ref": {
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "TrialContainer",
                    "name": "trial-1.agent.container-1"
                },
                "resource_version": "sha256:reviewed",
                "command": "buc runs exec run-1 TrialContainer/trial-1.agent.container-1 --resource-version <metadata.resourceVersion> -- COMMAND [ARG...]"
            }),
            "exec",
            "TrialContainer",
            "trial-1.agent.container-1",
        );

        assert_eq!(
            summary,
            vec![
                "can-i: yes exec TrialContainer/trial-1.agent.container-1".to_string(),
                "command: buc runs exec run-1 TrialContainer/trial-1.agent.container-1 --resource-version sha256:reviewed -- COMMAND [ARG...]".to_string(),
                "review: verb=create subresource=exec requires_running_run=true generation=12/12".to_string(),
                "generated_at: 2026-06-18T00:00:00Z".to_string(),
                "core_run_ids: core-run-1".to_string(),
                "resource_version: sha256:reviewed".to_string(),
            ]
        );
    }

    #[test]
    fn runtime_events_summary_surfaces_follow_cursors() {
        let lines = runtime_events_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeEventList",
            "cloud_run_id": "run-1",
            "metadata": {
                "resourceVersion": "event-row-seq:41",
                "continue": "event-row-seq:42",
                "after_row_seq": 37,
                "next_after_row_seq": 42,
                "remainingItemCount": 3,
                "limit": 5,
                "returned": 2
            },
            "events": [
                {
                    "row_seq": 40,
                    "event_type": "runtime.access.exec.requested",
                    "source": "cloud.runtime_access_requests",
                    "resource_refs": [
                        { "kind": "Exec", "name": "exec-1" },
                        { "kind": "TrialContainer", "name": "trial-1.agent.container-1" }
                    ],
                    "payload": {
                        "requester": "issuer:user-a",
                        "resource_version_precondition": "sha256:reviewed",
                        "access_resource_ref": { "kind": "Exec", "name": "exec-1" },
                        "resolved_target": {
                            "kind": "TrialContainer",
                            "name": "trial-1.agent.container-1",
                            "runner_binding": {
                                "runner_instance_id": "runner-1",
                                "worker_id": "worker-1"
                            }
                        },
                        "status": "requested",
                        "reason": "debug"
                    }
                },
                {
                    "row_seq": 41,
                    "event_type": "runtime.access.exec.completed",
                    "payload": {
                        "resource_kind": "Exec",
                        "resource_name": "exec-1",
                        "previous_status": "active",
                        "status": "completed",
                        "connection": {
                            "exit_code": 0,
                            "stdout_bytes": 20000,
                            "stdout_tail_bytes": 16000,
                            "stdout_tail_truncated": true,
                            "stderr_bytes": 5,
                            "stderr_tail_bytes": 5,
                            "stderr_tail_truncated": false
                        },
                        "message": "exit code 0"
                    }
                }
            ]
        }));

        assert_eq!(lines[0], "events: 2");
        assert!(lines.contains(&"resource_version: event-row-seq:41".to_string()));
        assert!(lines.contains(&"continue: event-row-seq:42".to_string()));
        assert!(lines.contains(&"after_row_seq: 37".to_string()));
        assert!(lines.contains(&"next_after_row_seq: 42".to_string()));
        assert!(lines.contains(&"remaining: 3".to_string()));
        assert!(lines.contains(&"limit: 5".to_string()));
        assert!(lines.contains(&"returned: 2".to_string()));
        assert!(lines.contains(&"  - row=40 runtime.access.exec.requested source=cloud.runtime_access_requests actor=issuer:user-a resource=Exec/exec-1 target=TrialContainer/trial-1.agent.container-1 runner=runner-1 worker=worker-1 reviewed-rv=sha256:reviewed reason=debug status=requested".to_string()));
        assert!(lines.contains(&"  - row=41 runtime.access.exec.completed resource=Exec/exec-1 exit=0 stdout=bytes=20000,tail=16000,truncated=true stderr=bytes=5,tail=5,truncated=false transition=active->completed message=exit code 0".to_string()));
        assert_eq!(
            runtime_events_follow_cursor(&json!({
                "metadata": {
                    "resourceVersion": "event-row-seq:41",
                    "continue": "event-row-seq:42"
                }
            })),
            Some("event-row-seq:42".to_string())
        );
        assert_eq!(
            runtime_events_follow_cursor(&json!({
                "metadata": {
                    "resourceVersion": "event-row-seq:41",
                    "continue": null
                }
            })),
            Some("event-row-seq:41".to_string())
        );
    }

    #[test]
    fn runtime_events_summary_uses_primary_resource_refs_and_keeps_secondary_involved_refs() {
        let line = runtime_event_summary_line(&json!({
            "row_seq": 42,
            "event_type": "runtime.access.port_forward.active",
            "source": "cloud.run_events",
            "resource_refs": [
                { "kind": "PortForward", "name": "pf-1", "uid": "pf-1" },
                { "kind": "RunnerInstance", "name": "runner-1", "uid": "runner-1" }
            ],
            "payload": {
                "requester": "issuer:user-a",
                "access_resource_ref": { "kind": "PortForward", "name": "pf-1" },
                "status": "active",
                "message": "tunnel active"
            }
        }));

        assert_eq!(
            line,
            "  - row=42 runtime.access.port_forward.active source=cloud.run_events actor=issuer:user-a resource=PortForward/pf-1 involved=RunnerInstance/runner-1 status=active message=tunnel active"
        );
    }

    #[test]
    fn runtime_events_summary_surfaces_raw_byte_read_audit_metadata() {
        let lines = runtime_events_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeEventList",
            "cloud_run_id": "run-1",
            "events": [
                {
                    "row_seq": 12,
                    "event_type": "runtime.resource.logs.read",
                    "source": "cloud.run_events",
                    "resource_refs": [{ "kind": "Trial", "name": "trial-1", "uid": "trial-1" }],
                    "payload": {
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "Trial",
                            "name": "trial-1",
                            "uid": "trial-1"
                        },
                        "stream": "stdout",
                        "object_ref": "artifact://sha256/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "sha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "byte_size": 14,
                        "media_type": "text/plain; charset=utf-8"
                    }
                },
                {
                    "row_seq": 13,
                    "event_type": "runtime.resource.content.read",
                    "source": "cloud.run_events",
                    "resource_refs": [{ "kind": "TrialArtifact", "name": "trial-1.agent-result.sha256-bbbbbbbb" }],
                    "payload": {
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "TrialArtifact",
                            "name": "trial-1.agent-result.sha256-bbbbbbbb"
                        },
                        "object_ref": "artifact://sha256/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "sha256": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "byte_size": 11,
                        "media_type": "application/json; charset=utf-8"
                    }
                },
                {
                    "row_seq": 14,
                    "event_type": "runtime.resource.content.read.failed",
                    "source": "cloud.run_events",
                    "resource_refs": [{ "kind": "Trial", "name": "trial-1" }],
                    "payload": {
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "Trial",
                            "name": "trial-1"
                        },
                        "status": "failed",
                        "error_code": "runtime_resource_content_not_found",
                        "error_status": 404,
                        "error_message": "Runtime resource content subresource is only available for TrialArtifact resources"
                    }
                }
            ]
        }));

        assert_eq!(lines[0], "events: 3");
        assert!(lines.contains(&"  - row=12 runtime.resource.logs.read source=cloud.run_events actor=issuer:user-a resource=Trial/trial-1 stream=stdout object=artifact://sha256/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa sha256=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bytes=14 media=text/plain; charset=utf-8".to_string()));
        assert!(lines.contains(&"  - row=13 runtime.resource.content.read source=cloud.run_events actor=issuer:user-a resource=TrialArtifact/trial-1.agent-result.sha256-bbbbbbbb object=artifact://sha256/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb sha256=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb bytes=11 media=application/json; charset=utf-8".to_string()));
        assert!(lines.contains(&"  - row=14 runtime.resource.content.read.failed source=cloud.run_events actor=issuer:user-a resource=Trial/trial-1 error=runtime_resource_content_not_found http=404 status=failed message=Runtime resource content subresource is only available for TrialArtifact resources".to_string()));
    }

    #[test]
    fn runtime_events_summary_surfaces_api_resources_read_audit_metadata() {
        let lines = runtime_events_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeEventList",
            "cloud_run_id": "run-1",
            "events": [
                {
                    "row_seq": 15,
                    "event_type": "runtime.api_resources.read",
                    "source": "cloud.run_events",
                    "payload": {
                        "operation": "api-resources",
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "Run",
                            "name": "run-1",
                            "uid": "run-1"
                        },
                        "api_resources_returned": 37,
                        "core_run_ids": ["core-run-1", "core-run-2"]
                    }
                },
                {
                    "row_seq": 16,
                    "event_type": "runtime.api_resources.read.failed",
                    "source": "cloud.run_events",
                    "payload": {
                        "operation": "api-resource",
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "Run",
                            "name": "run-1",
                            "uid": "run-1"
                        },
                        "selected_kind": "missing-kind",
                        "api_resource_kind": "RunnerInstance",
                        "api_resource_name": "runnerinstances",
                        "api_resource_categories": ["runner", "access-target"],
                        "api_resource_verbs": ["list", "get", "watch", "describe"],
                        "api_resource_subresources": ["logs", "port-forward", "exec"],
                        "api_resource_actions": ["cordon", "drain", "uncordon"],
                        "api_resource_access": ["logs", "port-forward", "exec"],
                        "api_resource_count": 1,
                        "status": "failed",
                        "error_code": "runtime_api_resource_not_found",
                        "error_status": 404,
                        "error_message": "Runtime API resource kind not found: missing-kind"
                    }
                }
            ]
        }));

        assert_eq!(lines[0], "events: 2");
        assert!(lines.contains(&"  - row=15 runtime.api_resources.read source=cloud.run_events actor=issuer:user-a resource=Run/run-1 operation=api-resources api_resources=37 core_runs=core-run-1,core-run-2".to_string()));
        assert!(lines.contains(&"  - row=16 runtime.api_resources.read.failed source=cloud.run_events actor=issuer:user-a resource=Run/run-1 operation=api-resource selected=missing-kind api_kind=RunnerInstance api_name=runnerinstances count=1 categories=runner,access-target verbs=list,get,watch,describe subresources=logs,port-forward,exec actions=cordon,drain,uncordon access=logs,port-forward,exec error=runtime_api_resource_not_found http=404 status=failed message=Runtime API resource kind not found: missing-kind".to_string()));
    }

    #[test]
    fn runtime_events_summary_surfaces_inspect_bundle_read_audit_metadata() {
        let lines = runtime_events_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeEventList",
            "cloud_run_id": "run-1",
            "events": [
                {
                    "row_seq": 15,
                    "event_type": "runtime.inspect.bundle.read",
                    "source": "cloud.run_events",
                    "payload": {
                        "operation": "inspect",
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "Run",
                            "name": "run-1",
                            "uid": "run-1"
                        },
                        "resource_filter": {
                            "kinds": ["RunnerInstance", "Trial"],
                            "categories": ["runner", "access-target"],
                            "label_selector": "bucephalus.dev/run-id=run-1",
                            "field_selector": "status.phase!=completed"
                        },
                        "event_limit": 250,
                        "inventory_resource_version": "sha256:inspect-inventory",
                        "inventory_total": 12,
                        "inventory_returned": 10,
                        "event_resource_version": "event-row-seq:42",
                        "event_returned": 9,
                        "api_resources_returned": 37,
                        "health_summary": {
                            "total": 10,
                            "ready": 4,
                            "degraded": 1,
                            "problem": 1,
                            "unknown": 4
                        },
                        "metrics_summary": {
                            "resources_total": 10,
                            "resources_returned": 8,
                            "events_total": 24
                        },
                        "log_refs": 6
                    }
                },
                {
                    "row_seq": 16,
                    "event_type": "runtime.inspect.bundle.read.failed",
                    "source": "cloud.run_events",
                    "payload": {
                        "operation": "inspect",
                        "status": "failed",
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "Run",
                            "name": "run-1",
                            "uid": "run-1"
                        },
                        "resource_filter": {
                            "kinds": ["RunnerInstance"],
                            "categories": ["access-target"]
                        },
                        "event_limit": 250,
                        "error_code": "runtime_inventory_unavailable",
                        "error_status": 503,
                        "error_message": "Runtime inventory unavailable"
                    }
                }
            ]
        }));

        assert_eq!(lines[0], "events: 2");
        assert!(lines.contains(&"  - row=15 runtime.inspect.bundle.read source=cloud.run_events actor=issuer:user-a resource=Run/run-1 operation=inspect kinds=RunnerInstance,Trial categories=runner,access-target label_selector=bucephalus.dev/run-id=run-1 field_selector=status.phase!=completed event_limit=250 inventory-rv=sha256:inspect-inventory inventory=10/12 event-rv=event-row-seq:42 events=9 api_resources=37 health_total=10 metrics_resources=8/10 metric_events=24 log_refs=6".to_string()));
        assert!(lines.contains(&"  - row=16 runtime.inspect.bundle.read.failed source=cloud.run_events actor=issuer:user-a resource=Run/run-1 operation=inspect kinds=RunnerInstance categories=access-target event_limit=250 error=runtime_inventory_unavailable http=503 status=failed message=Runtime inventory unavailable".to_string()));
    }

    #[test]
    fn runtime_events_summary_surfaces_resource_query_read_audit_metadata() {
        let lines = runtime_events_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeEventList",
            "cloud_run_id": "run-1",
            "events": [
                {
                    "row_seq": 17,
                    "event_type": "runtime.resource.list.read",
                    "source": "cloud.run_events",
                    "payload": {
                        "operation": "list",
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "Run",
                            "name": "run-1",
                            "uid": "run-1"
                        },
                        "resource_filter": {
                            "kinds": ["RunnerInstance", "Trial"],
                            "categories": ["runner", "access-target"],
                            "label_selector": "bucephalus.dev/run-id=run-1",
                            "field_selector": "status.access.exec=true"
                        },
                        "limit": 10,
                        "resource_version": "sha256:list",
                        "total": 12,
                        "returned": 10,
                        "remaining": 2
                    }
                },
                {
                    "row_seq": 18,
                    "event_type": "runtime.resource.watch.read",
                    "source": "cloud.run_events",
                    "payload": {
                        "operation": "watch",
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "Run",
                            "name": "run-1"
                        },
                        "resource_filter": {
                            "kinds": ["RunnerInstance"],
                            "categories": ["runner"],
                            "field_selector": "status.phase=online"
                        },
                        "resource_version": "sha256:inventory",
                        "resource_version_cursor": "sha256:previous",
                        "known_resources": 7,
                        "allow_bookmarks": true,
                        "total": 3,
                        "returned": 3,
                        "watch_events_returned": 1
                    }
                },
                {
                    "row_seq": 19,
                    "event_type": "runtime.resource.describe.read.failed",
                    "source": "cloud.run_events",
                    "payload": {
                        "operation": "describe",
                        "requester": "issuer:user-a",
                        "resource_ref": {
                            "kind": "Trial",
                            "name": "missing-trial"
                        },
                        "resource_kind": "Trial",
                        "resource_name": "missing-trial",
                        "status": "failed",
                        "error_code": "runtime_resource_not_found",
                        "error_status": 404,
                        "error_message": "Runtime resource not found"
                    }
                }
            ]
        }));

        assert_eq!(lines[0], "events: 3");
        assert!(lines.contains(&"  - row=17 runtime.resource.list.read source=cloud.run_events actor=issuer:user-a resource=Run/run-1 operation=list kinds=RunnerInstance,Trial categories=runner,access-target label_selector=bucephalus.dev/run-id=run-1 field_selector=status.access.exec=true limit=10 rv=sha256:list returned=10/12 remaining=2".to_string()));
        assert!(lines.contains(&"  - row=18 runtime.resource.watch.read source=cloud.run_events actor=issuer:user-a resource=Run/run-1 operation=watch kinds=RunnerInstance categories=runner field_selector=status.phase=online rv=sha256:inventory cursor-rv=sha256:previous known=7 bookmarks=true returned=3/3 watch_events=1".to_string()));
        assert!(lines.contains(&"  - row=19 runtime.resource.describe.read.failed source=cloud.run_events actor=issuer:user-a resource=Trial/missing-trial operation=describe error=runtime_resource_not_found http=404 status=failed message=Runtime resource not found".to_string()));
    }

    #[test]
    fn runtime_events_summary_surfaces_operation_review_audit_metadata() {
        let lines = runtime_events_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeEventList",
            "cloud_run_id": "run-1",
            "events": [
                {
                    "row_seq": 21,
                    "event_type": "runtime.resource.operation.reviewed",
                    "source": "cloud.run_events",
                    "resource_refs": [{ "kind": "TrialContainer", "name": "trial-1.agent.container-1" }],
                    "payload": {
                        "requester": "issuer:user-a",
                        "operation": "audit",
                        "matched_operation": "audit",
                        "supported": true,
                        "status": "supported",
                        "resource_ref": {
                            "kind": "TrialContainer",
                            "name": "trial-1.agent.container-1"
                        },
                        "resource_version": "sha256:reviewed",
                        "command": "buc runs audit run-1 TrialContainer/trial-1.agent.container-1",
                        "verb": "watch",
                        "subresource": "events"
                    }
                },
                {
                    "row_seq": 22,
                    "event_type": "runtime.resource.operation.review.failed",
                    "source": "cloud.run_events",
                    "resource_refs": [{ "kind": "TrialContainer", "name": "missing-container" }],
                    "payload": {
                        "requester": "issuer:user-a",
                        "operation": "exec",
                        "status": "failed",
                        "resource_ref": {
                            "kind": "TrialContainer",
                            "name": "missing-container"
                        },
                        "error_code": "runtime_resource_not_found",
                        "error_status": 404,
                        "error_message": "Runtime resource not found"
                    }
                }
            ]
        }));

        assert_eq!(lines[0], "events: 2");
        assert!(lines.contains(&"  - row=21 runtime.resource.operation.reviewed source=cloud.run_events actor=issuer:user-a resource=TrialContainer/trial-1.agent.container-1 operation=audit matched=audit status=supported".to_string()));
        assert!(lines.contains(&"  - row=22 runtime.resource.operation.review.failed source=cloud.run_events actor=issuer:user-a resource=TrialContainer/missing-container operation=exec error=runtime_resource_not_found http=404 status=failed message=Runtime resource not found".to_string()));
    }

    #[test]
    fn runtime_list_metadata_summary_surfaces_list_cursors() {
        let lines = runtime_list_metadata_summary_lines(&json!({
            "metadata": {
                "resourceVersion": "rv-41",
                "continue": "cursor-42",
                "remainingItemCount": 3,
                "total": 10,
                "returned": 7,
                "limit": 7
            }
        }));

        assert_eq!(
            lines,
            vec![
                "resource_version: rv-41".to_string(),
                "continue: cursor-42".to_string(),
                "remaining: 3".to_string(),
                "total: 10".to_string(),
                "returned: 7".to_string(),
                "limit: 7".to_string(),
            ]
        );
    }

    #[test]
    fn runtime_inspect_summary_surfaces_bundle_observability() {
        let lines = runtime_inspect_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeInspectBundle",
            "cloud_run_id": "run-1",
            "resource_filter": {
                "kinds": ["RunnerInstance", "Trial"],
                "categories": ["access-target"],
                "label_selector": "bucephalus.dev/run-id=run-1",
                "field_selector": "status.phase!=completed"
            },
            "api_resources": {
                "resources": [
                    { "kind": "RunnerInstance" },
                    { "kind": "Trial" }
                ]
            },
            "resource_inventory": {
                "metadata": {
                    "resourceVersion": "sha256:inventory",
                    "continue": "runtime-resource:next",
                    "remainingItemCount": 4,
                    "total": 9,
                    "returned": 5,
                    "limit": 5
                },
                "resources": [
                    { "kind": "RunnerInstance" },
                    { "kind": "Trial" },
                    { "kind": "PortForward" }
                ]
            },
            "resource_health": {
                "summary": {
                    "total": 3,
                    "ready": 1,
                    "degraded": 1,
                    "problem": 1,
                    "unknown": 0,
                    "access_targets": 2,
                    "reachable_access_targets": 1,
                    "actions_available": 2,
                    "observed_stale": 1
                }
            },
            "resource_metrics": {
                "summary": {
                    "resources_total": 3,
                    "resources_returned": 2,
                    "metrics_total": 19,
                    "events_total": 7
                },
                "resources": [{}, {}]
            },
            "event_list": {
                "metadata": {
                    "resourceVersion": "event-row-seq:42",
                    "continue": "event-row-seq:43",
                    "remainingItemCount": 8,
                    "limit": 25,
                    "returned": 2,
                    "after_row_seq": 40,
                    "next_after_row_seq": 42
                },
                "events": [
                    { "row_seq": 41, "event_type": "runtime.resource.runner_instance.online" },
                    { "row_seq": 42, "event_type": "runtime.access.exec.completed" }
                ]
            },
            "log_refs": [
                {
                    "resource": { "kind": "RunnerInstance", "name": "runner-1" },
                    "streams": ["stdout", "stderr"]
                },
                {
                    "resource": { "kind": "Exec", "name": "exec-1" },
                    "streams": ["stdout"]
                }
            ]
        }));

        assert_eq!(
            lines,
            vec![
                "inspect: resources=3 api_resources=2 events=2 metrics_resources=2 log_refs=2".to_string(),
                "filter: kinds=RunnerInstance,Trial categories=access-target label_selector=bucephalus.dev/run-id=run-1 field_selector=status.phase!=completed".to_string(),
                "inventory_resource_version: sha256:inventory".to_string(),
                "inventory_continue: runtime-resource:next".to_string(),
                "inventory_remaining: 4".to_string(),
                "inventory_total: 9".to_string(),
                "inventory_returned: 5".to_string(),
                "inventory_limit: 5".to_string(),
                "health: total=3 ready=1 degraded=1 problem=1 unknown=0 access_targets=2 reachable=1 actions=2 observed_stale=1".to_string(),
                "metrics: resources_total=3 resources_returned=2 metrics_total=19 events_total=7".to_string(),
                "event_events: 2".to_string(),
                "event_resource_version: event-row-seq:42".to_string(),
                "event_continue: event-row-seq:43".to_string(),
                "event_after_row_seq: 40".to_string(),
                "event_next_after_row_seq: 42".to_string(),
                "event_remaining: 8".to_string(),
                "event_limit: 25".to_string(),
                "event_returned: 2".to_string(),
                "log_ref: RunnerInstance/runner-1 streams=stdout,stderr".to_string(),
                "log_ref: Exec/exec-1 streams=stdout".to_string(),
            ]
        );
    }

    #[test]
    fn runtime_resource_summary_surfaces_precondition_metadata() {
        let lines = runtime_resource_summary_lines(&json!({
            "generated_at": "2026-06-18T00:00:00Z",
            "core_run_ids": ["core-run-1", "core-run-2"],
            "resource": {
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RunnerInstance",
                "metadata": {
                    "name": "runner-1",
                    "uid": "runner-uid-1",
                    "resourceVersion": "sha256:runner-rv",
                    "generation": 9,
                    "ownerReferences": [{
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RunnerAttempt",
                        "name": "attempt-1",
                        "uid": "attempt-uid-1"
                    }]
                },
                "status": {
                    "phase": "Running",
                    "reason": "Ready",
                    "observedGeneration": 7,
                    "conditions": [{
                        "type": "Observed",
                        "status": "False",
                        "reason": "ObservedGenerationStale",
                        "message": "Status is behind desired state"
                    }]
                }
            },
            "operations": [
                {
                    "purpose": "cordon",
                    "command": "buc runs cordon run-1 RunnerInstance/runner-1 --resource-version <metadata.resourceVersion>",
                    "supported": true,
                    "verb": "update",
                    "subresource": "actions",
                    "action": "cordon",
                    "requires_running_run": false
                },
                {
                    "purpose": "exec",
                    "command": "buc runs exec run-1 RunnerInstance/runner-1 --resource-version <metadata.resourceVersion> -- COMMAND [ARG...]",
                    "supported": false,
                    "reason": "run_not_running",
                    "message": "exec requires a running Cloud run",
                    "verb": "create",
                    "subresource": "exec",
                    "requires_running_run": true
                }
            ],
            "event_list": { "events": [{ "row_seq": 1 }, { "row_seq": 2 }] }
        }));

        assert_eq!(
            lines,
            vec![
                "resource: RunnerInstance/runner-1 phase=Running ready=Unknown reason=Ready"
                    .to_string(),
                "generated_at: 2026-06-18T00:00:00Z".to_string(),
                "core_run_ids: core-run-1,core-run-2".to_string(),
                "uid: runner-uid-1".to_string(),
                "resource_version: sha256:runner-rv".to_string(),
                "generation: 9 observed=7 freshness=stale".to_string(),
                "owners: RunnerAttempt/attempt-1".to_string(),
                "condition: Observed=False reason=ObservedGenerationStale message=Status is behind desired state".to_string(),
                "operations: cordon".to_string(),
                "operation: cordon supported=yes verb=update subresource=actions action=cordon requires_running_run=false command='buc runs cordon run-1 RunnerInstance/runner-1 --resource-version <metadata.resourceVersion>'".to_string(),
                "operation: exec supported=no verb=create subresource=exec reason=run_not_running message='exec requires a running Cloud run' requires_running_run=true command='buc runs exec run-1 RunnerInstance/runner-1 --resource-version <metadata.resourceVersion> -- COMMAND [ARG...]'".to_string(),
                "events: 2".to_string(),
            ]
        );
    }

    #[test]
    fn runtime_resource_summary_surfaces_related_event_rows() {
        let lines = runtime_resource_summary_lines(&json!({
            "resource": {
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RunnerInstance",
                "metadata": {
                    "name": "runner-1",
                    "resourceVersion": "sha256:runner-rv"
                },
                "status": {
                    "phase": "Running",
                    "conditions": [{ "type": "Ready", "status": "True" }]
                }
            },
            "event_list": {
                "metadata": {
                    "resourceVersion": "event-row-seq:41",
                    "continue": "event-row-seq:42",
                    "after_row_seq": 37,
                    "next_after_row_seq": 42,
                    "remainingItemCount": 3,
                    "limit": 5,
                    "returned": 2
                },
                "events": [
                    {
                        "row_seq": 40,
                        "event_type": "runtime.resource.operation.reviewed",
                        "source": "cloud.run_events",
                        "payload": {
                            "requester": "issuer:user-a",
                            "resource_ref": {
                                "kind": "RunnerInstance",
                                "name": "runner-1"
                            },
                            "resource_version_precondition": "sha256:runner-rv",
                            "operation": "cordon",
                            "matched_operation": "cordon",
                            "reason": "maintenance",
                            "status": "supported"
                        }
                    },
                    {
                        "row_seq": 41,
                        "event_type": "runtime.access.port_forward.completed",
                        "payload": {
                            "access_resource_ref": {
                                "kind": "PortForward",
                                "name": "pf-1"
                            },
                            "resolved_target": {
                                "kind": "RunnerInstance",
                                "name": "runner-1",
                                "runner_binding": {
                                    "runner_instance_id": "runner-1",
                                    "worker_id": "worker-1"
                                }
                            },
                            "connection": {
                                "exit_code": 0
                            },
                            "previous_status": "active",
                            "status": "completed",
                            "message": "operator cleanup"
                        }
                    }
                ]
            }
        }));

        assert_eq!(
            lines,
            vec![
                "resource: RunnerInstance/runner-1 phase=Running ready=True".to_string(),
                "resource_version: sha256:runner-rv".to_string(),
                "condition: Ready=True".to_string(),
                "events: 2".to_string(),
                "event_resource_version: event-row-seq:41".to_string(),
                "event_continue: event-row-seq:42".to_string(),
                "event_after_row_seq: 37".to_string(),
                "event_next_after_row_seq: 42".to_string(),
                "event_remaining: 3".to_string(),
                "event_limit: 5".to_string(),
                "event_returned: 2".to_string(),
                "event: row=40 runtime.resource.operation.reviewed source=cloud.run_events actor=issuer:user-a resource=RunnerInstance/runner-1 reviewed-rv=sha256:runner-rv operation=cordon matched=cordon reason=maintenance status=supported".to_string(),
                "event: row=41 runtime.access.port_forward.completed access=PortForward/pf-1 target=RunnerInstance/runner-1 runner=runner-1 worker=worker-1 exit=0 transition=active->completed message=operator cleanup".to_string(),
            ]
        );
    }

    #[test]
    fn runtime_resource_summary_surfaces_related_resource_graph() {
        let lines = runtime_resource_summary_lines(&json!({
            "resource": {
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "TrialContainer",
                "metadata": {
                    "name": "trial-1.agent.container-1",
                    "resourceVersion": "sha256:container-rv"
                },
                "status": {
                    "phase": "Running",
                    "reason": "ContainerReady",
                    "conditions": [{ "type": "Ready", "status": "True" }]
                }
            },
            "related_resources": [
                {
                    "relationship": "owner",
                    "resource": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RunnerInstance",
                        "metadata": {
                            "name": "runner-1",
                            "resourceVersion": "sha256:runner-rv"
                        },
                        "status": {
                            "phase": "Online",
                            "reason": "RunnerReady",
                            "conditions": [{ "type": "Ready", "status": "True" }]
                        }
                    }
                },
                {
                    "relationship": "dependent",
                    "resource": {
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "PortForward",
                        "metadata": {
                            "name": "pf-1",
                            "resourceVersion": "sha256:pf-rv"
                        },
                        "status": {
                            "phase": "Active",
                            "reason": "TunnelReady",
                            "conditions": [{ "type": "Ready", "status": "True" }]
                        }
                    }
                }
            ],
            "event_list": { "events": [] }
        }));

        assert_eq!(
            lines,
            vec![
                "resource: TrialContainer/trial-1.agent.container-1 phase=Running ready=True reason=ContainerReady".to_string(),
                "resource_version: sha256:container-rv".to_string(),
                "condition: Ready=True".to_string(),
                "related: owner RunnerInstance/runner-1 phase=Online ready=True reason=RunnerReady resource_version=sha256:runner-rv".to_string(),
                "related: dependent PortForward/pf-1 phase=Active ready=True reason=TunnelReady resource_version=sha256:pf-rv".to_string(),
                "events: 0".to_string(),
            ]
        );
    }

    #[test]
    fn runtime_resource_status_summary_surfaces_freshness_conditions_actions_and_audit() {
        let lines = runtime_resource_status_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeResourceStatus",
            "cloud_run_id": "run-1",
            "generated_at": "2026-06-19T12:00:01Z",
            "core_run_ids": ["core-run-1"],
            "resource_ref": {
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RunnerInstance",
                "name": "runner-1",
                "uid": "runner-uid-1"
            },
            "generation": 9,
            "observedGeneration": 7,
            "resourceVersion": "sha256:runner-rv",
            "deletionTimestamp": "2026-06-19T12:00:00Z",
            "phase": "Running",
            "reason": "Ready",
            "message": "runner reachable",
            "conditions": [
                {
                    "type": "Ready",
                    "status": "True",
                    "reason": "RunnerReady",
                    "message": "runner reachable"
                },
                {
                    "type": "Observed",
                    "status": "False",
                    "reason": "ObservedGenerationStale",
                    "message": "status behind desired generation"
                }
            ],
            "actions": ["cordon", "drain"],
            "status": { "phase": "Running" },
            "audit": { "source": "cloud.runner_instances" }
        }));

        assert_eq!(
            lines,
            vec![
                "status: RunnerInstance/runner-1 phase=Running reason=Ready".to_string(),
                "message: runner reachable".to_string(),
                "resource_version: sha256:runner-rv".to_string(),
                "generated_at: 2026-06-19T12:00:01Z".to_string(),
                "generation: 9 observed=7 freshness=stale".to_string(),
                "deletion_timestamp: 2026-06-19T12:00:00Z".to_string(),
                "condition: Ready=True reason=RunnerReady message=runner reachable".to_string(),
                "condition: Observed=False reason=ObservedGenerationStale message=status behind desired generation".to_string(),
                "actions: cordon,drain".to_string(),
                "audit_source: cloud.runner_instances".to_string(),
            ]
        );
    }

    #[test]
    fn runtime_metrics_summary_surfaces_collection_cursors() {
        let lines = runtime_metrics_summary_lines(&json!({
            "metadata": {
                "resourceVersion": "metrics-rv-1",
                "continue": "metrics-cursor-2",
                "remainingItemCount": 1,
                "total": 3,
                "returned": 2
            },
            "summary": { "resources_total": 2, "metrics_total": 4 },
            "resources": [{
                "resource_ref": {
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RunnerInstance",
                    "name": "runner-1"
                },
                "metrics": [
                    { "name": "lifecycle.ready", "value": 1 },
                    { "name": "cpu.usage", "value": 0.42 }
                ]
            }]
        }));

        assert_eq!(
            lines,
            vec![
                "metrics: resources=1 summary={\"metrics_total\":4,\"resources_total\":2}"
                    .to_string(),
                "resource_version: metrics-rv-1".to_string(),
                "continue: metrics-cursor-2".to_string(),
                "remaining: 1".to_string(),
                "total: 3".to_string(),
                "returned: 2".to_string(),
                "  - RunnerInstance/runner-1 metrics=2".to_string(),
            ]
        );
    }

    #[test]
    fn runtime_watch_summary_surfaces_resource_cursors_and_event_versions() {
        let lines = runtime_watch_summary_lines(&json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": "RuntimeResourceWatchList",
            "cloud_run_id": "run-1",
            "resource_versions": {
                "runnerinstance/runner-1": "rv-runner-2",
                "exec/exec-1": "rv-exec-1"
            },
            "events": [{
                "type": "MODIFIED",
                "resource_ref": { "kind": "RunnerInstance", "name": "runner-1" },
                "resource_version": "rv-runner-2",
                "previous_resource_version": "rv-runner-1"
            }],
            "resource_inventory": {
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceList",
                "metadata": {
                    "resourceVersion": "rv-list-2",
                    "continue": null,
                    "remainingItemCount": 0,
                    "total": 1,
                    "returned": 1
                },
                "resources": [runtime_resource("RunnerInstance", "runner-1")]
            }
        }));

        assert_eq!(
            lines,
            vec![
                "watch_events: 1".to_string(),
                "resource_version: rv-list-2".to_string(),
                "remaining: 0".to_string(),
                "total: 1".to_string(),
                "returned: 1".to_string(),
                "inventory_resources: 1".to_string(),
                "known_resource: exec/exec-1=rv-exec-1".to_string(),
                "known_resource: runnerinstance/runner-1=rv-runner-2".to_string(),
                "  - MODIFIED RunnerInstance/runner-1 rv=rv-runner-2 previous=rv-runner-1"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn runs_access_commands_can_create_async_resources_without_waiting() {
        let _lock = lock_env();
        let server = MockCloudServer::start(2);
        let api_url = server.api_url();
        let home = temp_dir("runs_access_no_wait_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "port-forward".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--target-port".to_string(),
            "8080".to_string(),
            "--no-wait".to_string(),
            "--resource-version".to_string(),
            "sha256:runner-port-forward".to_string(),
        ])
        .expect("hosted run port-forward should support async creation");
        run(vec![
            "runs".to_string(),
            "exec".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--no-wait".to_string(),
            "--resource-version".to_string(),
            "sha256:runner-exec".to_string(),
            "--".to_string(),
            "python".to_string(),
            "-V".to_string(),
        ])
        .expect("hosted run exec should support async creation");

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            requests[0].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/port-forward"
        );
        assert_eq!(requests[1].method, "POST");
        assert_eq!(
            requests[1].path,
            "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/exec"
        );
        let port_forward_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            port_forward_body["resource_version"],
            json!("sha256:runner-port-forward")
        );
        let exec_body: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(exec_body["resource_version"], json!("sha256:runner-exec"));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_port_forward_attach_completes_active_resource_after_local_tunnel_exits() {
        let _lock = lock_env();
        let fake_bin = temp_dir("runs_port_forward_attach_fake_bin");
        fs::create_dir_all(&fake_bin).unwrap();
        let fake_gcloud = fake_bin.join("gcloud");
        fs::write(&fake_gcloud, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&fake_gcloud).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&fake_gcloud, permissions).unwrap();
        }

        let server = MockCloudServer::start_with_handler(2, |request, index| match index {
            0 => {
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/port-forward"
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["target_port"], json!(8080));
                assert_eq!(body["local_port"], json!(18080));
                assert_eq!(
                    body["resource_version"],
                    json!("sha256:runner-port-forward")
                );
                let mut resource = runtime_access_resource(
                    "PortForward",
                    "pf-1",
                    "active",
                    json!({
                        "mode": "gcp_iap_ssh",
                        "project_id": "buc-prod",
                        "zone": "us-central1-a",
                        "instance_name": "runner-vm-1",
                        "target_host": "127.0.0.1",
                        "target_port": 8080,
                        "local_port": 18080
                    }),
                );
                resource["metadata"]["resourceVersion"] = json!("sha256:pf-active");
                json!({
                    "cloud_run_id": "run-1",
                    "resource": resource,
                    "operations": [],
                    "related_resources": [],
                    "event_list": { "events": [] }
                })
            }
            1 => {
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2D1/actions/complete"
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body,
                    json!({
                        "reason": "local port-forward attach ended",
                        "resource_version": "sha256:pf-active"
                    })
                );
                json!({
                    "cloud_run_id": "run-1",
                    "resource": runtime_access_resource(
                        "PortForward",
                        "pf-1",
                        "completed",
                        json!({ "mode": "gcp_iap_ssh" }),
                    ),
                    "operations": [],
                    "related_resources": [],
                    "event_list": { "events": [] }
                })
            }
            _ => unreachable!(),
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_port_forward_attach_cleanup_home");
        let home_s = home.display().to_string();
        let old_path = std::env::var("PATH").unwrap_or_default();
        let path = if old_path.is_empty() {
            fake_bin.display().to_string()
        } else {
            format!("{}:{}", fake_bin.display(), old_path)
        };
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
            ("PATH", Some(path.as_str())),
        ]);

        run(vec![
            "runs".to_string(),
            "port-forward".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--target-port".to_string(),
            "8080".to_string(),
            "--local-port".to_string(),
            "18080".to_string(),
            "--attach".to_string(),
            "--resource-version".to_string(),
            "sha256:runner-port-forward".to_string(),
        ])
        .expect("attached hosted run port-forward should complete after local tunnel exits");

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        let _ = fs::remove_dir_all(home);
        let _ = fs::remove_dir_all(fake_bin);
    }

    #[test]
    fn runs_port_forward_attach_reports_worker_client_endpoint_without_cleanup() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "POST");
            assert_eq!(
                request.path,
                "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/port-forward"
            );
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["target_port"], json!(8080));
            assert_eq!(body["local_port"], json!(18080));
            assert_eq!(
                body["resource_version"],
                json!("sha256:runner-port-forward")
            );
            let mut resource = runtime_access_resource(
                "PortForward",
                "pf-1",
                "active",
                json!({
                    "kind": "loopback",
                    "local_port": 18080,
                    "client_reachable": true,
                    "client_endpoint": "tcp://127.0.0.1:18080"
                }),
            );
            resource["metadata"]["resourceVersion"] = json!("sha256:pf-active");
            json!({
                "cloud_run_id": "run-1",
                "resource": resource,
                "operations": [],
                "related_resources": [],
                "event_list": { "events": [] }
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_port_forward_client_endpoint_attach_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "port-forward".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--target-port".to_string(),
            "8080".to_string(),
            "--local-port".to_string(),
            "18080".to_string(),
            "--attach".to_string(),
            "--resource-version".to_string(),
            "sha256:runner-port-forward".to_string(),
        ])
        .expect("worker-managed client endpoint should be accepted by --attach");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_port_forward_exits_nonzero_when_tunnel_fails() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(2, |request, _index| {
            match (request.method.as_str(), request.path.as_str()) {
                (
                    "POST",
                    "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/port-forward",
                ) => {
                    json!({
                        "resource": runtime_access_resource(
                            "PortForward",
                            "pf-1",
                            "requested",
                            json!({ "mode": "runner_reverse_tunnel" }),
                        )
                    })
                }
                ("GET", "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2D1") => {
                    let mut resource = runtime_access_resource(
                        "PortForward",
                        "pf-1",
                        "failed",
                        json!({ "error": "helper failed" }),
                    );
                    if let Some(status) = resource.get_mut("status").and_then(Value::as_object_mut)
                    {
                        status.insert("reason".to_string(), json!("WorkerPortForwardFailed"));
                    }
                    json!({
                        "cloud_run_id": "run-1",
                        "resource": resource,
                        "operations": [],
                        "related_resources": [],
                        "event_list": { "events": [] }
                    })
                }
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_runtime_port_forward_failure_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "runs".to_string(),
            "port-forward".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--target-port".to_string(),
            "8080".to_string(),
            "--resource-version".to_string(),
            "sha256:runner-port-forward".to_string(),
        ])
        .unwrap_err()
        .to_string();

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        assert!(err.contains("runtime port-forward PortForward/pf-1 ended with phase=failed"));
        assert!(err.contains("reason=WorkerPortForwardFailed"));
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["target_port"], json!(8080));
        assert_eq!(
            body["resource_version"],
            json!("sha256:runner-port-forward")
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_exec_exits_nonzero_when_remote_command_fails() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(2, |request, _index| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/exec") => {
                    json!({
                        "resource": runtime_access_resource(
                            "Exec",
                            "exec-1",
                            "requested",
                            json!({ "mode": "worker_exec" }),
                        )
                    })
                }
                ("GET", "/v1/runs/run%2D1/runtime/resources/Exec/exec%2D1") => json!({
                    "cloud_run_id": "run-1",
                    "resource": runtime_access_resource(
                        "Exec",
                        "exec-1",
                        "completed",
                        json!({ "exit_code": 42, "stderr_tail": "boom\n" }),
                    ),
                    "operations": [],
                    "related_resources": [],
                    "event_list": { "events": [] }
                }),
                _ => panic!(
                    "unexpected mock Cloud API request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_runtime_exec_failure_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        let err = run(vec![
            "runs".to_string(),
            "exec".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--resource-version".to_string(),
            "sha256:runner-exec".to_string(),
            "--".to_string(),
            "false".to_string(),
        ])
        .unwrap_err()
        .to_string();

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        assert!(err.contains("runtime exec Exec/exec-1 exited with code 42"));
        let exec_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(exec_body["command"], json!(["false"]));
        assert_eq!(exec_body["resource_version"], json!("sha256:runner-exec"));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runtime_access_summaries_surface_connection_and_exec_output() {
        let mut port_forward = runtime_access_resource(
            "PortForward",
            "pf-1",
            "active",
            json!({
                "mode": "gcp_iap_ssh",
                "project_id": "buc-prod",
                "zone": "us-central1-a",
                "instance_name": "runner-vm-1",
                "target_host": "127.0.0.1",
                "target_port": 8080,
                "local_port": 18080,
                "client_endpoint": "tcp://127.0.0.1:18080",
                "provider_tunnel_url": "gce-iap://buc-prod/us-central1-a/runner-vm-1"
            }),
        );
        port_forward["metadata"]["resourceVersion"] = json!("sha256:pf-rv");
        port_forward["spec"] = json!({
            "target_ref": {
                "kind": "Trial",
                "name": "trial-1",
                "uid": "trial-uid-1",
                "resourceVersion": "sha256:trial-rv"
            },
            "target_port": 8080,
            "local_port": 18080,
            "reason": "debug web server"
        });
        port_forward["status"]["runner_binding"] = json!({
            "runner_instance_id": "runner-1",
            "attempt_id": "attempt-1",
            "worker_id": "worker-1"
        });
        port_forward["status"]["expires_at"] = json!("2026-06-20T12:00:00Z");
        port_forward["audit"] = json!({
            "source": "cloud.runtime_access_requests",
            "requester": "issuer:user-a",
            "target_ref": {
                "kind": "Trial",
                "name": "trial-1"
            },
            "target_resource_version": "sha256:trial-rv",
            "runner_binding": {
                "runner_instance_id": "runner-1",
                "attempt_id": "attempt-1",
                "worker_id": "worker-1"
            }
        });
        let port_lines = runtime_access_detail_lines(&port_forward, Some("run-1"));
        assert!(port_lines
            .iter()
            .any(|line| line
                == "target: Trial/trial-1 uid=trial-uid-1 resource_version=sha256:trial-rv"));
        assert!(port_lines.iter().any(|line| {
            line == "request: requester=issuer:user-a reason=debug web server expires_at=2026-06-20T12:00:00Z source=cloud.runtime_access_requests"
        }));
        assert!(port_lines.iter().any(
            |line| line == "runner_binding: runner=runner-1 attempt=attempt-1 worker=worker-1"
        ));
        assert!(port_lines
            .iter()
            .any(|line| line == "connection_mode: gcp_iap_ssh"));
        assert!(port_lines.iter().any(|line| line == "local_port: 18080"));
        assert!(port_lines
            .iter()
            .any(|line| line == "client_endpoint: tcp://127.0.0.1:18080"));
        assert!(port_lines.iter().any(|line| {
            line == "provider_tunnel_url: gce-iap://buc-prod/us-central1-a/runner-vm-1"
        }));
        assert!(port_lines.iter().any(|line| {
            line == "attach_command: gcloud compute ssh runner-vm-1 --project buc-prod --zone us-central1-a --tunnel-through-iap -- -N -L 127.0.0.1:18080:127.0.0.1:8080"
        }));
        assert!(port_lines.iter().any(|line| {
            line == "cleanup_command: buc runs complete run-1 PortForward/pf-1 --reason cleanup --resource-version sha256:pf-rv"
        }));

        let mut exec = runtime_access_resource(
            "Exec",
            "exec-1",
            "completed",
            json!({
                "exit_code": 0,
                "stdout_tail": "Python 3.12.0\n",
                "stdout_bytes": 14,
                "stdout_tail_bytes": 14,
                "stdout_tail_truncated": false,
                "stderr_tail": "warning\n",
                "stderr_bytes": 7,
                "stderr_tail_bytes": 7,
                "stderr_tail_truncated": false
            }),
        );
        exec["spec"] = json!({
            "target_ref": {
                "kind": "TrialContainer",
                "name": "trial-1.agent.container-1",
                "resourceVersion": "sha256:container-rv"
            },
            "command": ["python", "-V"],
            "reason": "check interpreter"
        });
        exec["status"]["runner_binding"] = json!({
            "runner_instance_id": "runner-1",
            "attempt_id": "attempt-1",
            "worker_id": "worker-1"
        });
        exec["audit"] = json!({
            "source": "cloud.runtime_access_requests",
            "requester": "issuer:user-a"
        });
        let exec_lines = runtime_access_detail_lines(&exec, Some("run-1"));
        assert!(exec_lines.iter().any(|line| {
            line == "target: TrialContainer/trial-1.agent.container-1 resource_version=sha256:container-rv"
        }));
        assert!(exec_lines
            .iter()
            .any(|line| line == "request: requester=issuer:user-a reason=check interpreter source=cloud.runtime_access_requests"));
        assert!(exec_lines.iter().any(
            |line| line == "runner_binding: runner=runner-1 attempt=attempt-1 worker=worker-1"
        ));
        assert!(exec_lines.iter().any(|line| line == "command: python -V"));
        assert!(exec_lines.iter().any(|line| line == "exit_code: 0"));
        assert!(exec_lines
            .iter()
            .any(|line| line.contains("stdout_tail:\nPython 3.12.0")));
        assert!(exec_lines
            .iter()
            .any(|line| line == "stdout_evidence: bytes=14 tail_bytes=14 truncated=false"));
        assert!(exec_lines
            .iter()
            .any(|line| line.contains("stderr_tail:\nwarning")));
        assert!(exec_lines
            .iter()
            .any(|line| line == "stderr_evidence: bytes=7 tail_bytes=7 truncated=false"));
        assert!(!exec_lines
            .iter()
            .any(|line| line.starts_with("cleanup_command:")));

        let stream_field_exec = runtime_access_resource(
            "Exec",
            "exec-stdout-fields",
            "completed",
            json!({
                "exit_code": 0,
                "stdout": "v22.0.0\n",
                "stderr": "node warning\n"
            }),
        );
        let stream_field_lines = runtime_access_detail_lines(&stream_field_exec, Some("run-1"));
        assert!(stream_field_lines
            .iter()
            .any(|line| line.contains("stdout:\nv22.0.0")));
        assert!(stream_field_lines
            .iter()
            .any(|line| line.contains("stderr:\nnode warning")));

        let mut running_exec =
            runtime_access_resource("Exec", "exec-2", "running", json!({ "mode": "ssh_exec" }));
        running_exec["metadata"]["resourceVersion"] = json!("sha256:exec-rv");
        let running_exec_lines = runtime_access_detail_lines(&running_exec, Some("run-1"));
        assert!(running_exec_lines.iter().any(|line| {
            line == "cleanup_command: buc runs delete run-1 Exec/exec-2 --reason cleanup --resource-version sha256:exec-rv"
        }));
    }

    #[test]
    fn runtime_port_forward_success_guard_reports_terminal_tunnel_failures() {
        let requested = json!({
            "resource": runtime_access_resource(
                "PortForward",
                "pf-1",
                "requested",
                json!({ "mode": "runner_reverse_tunnel" }),
            )
        });
        ensure_runtime_port_forward_success(&requested)
            .expect("async requested port-forward should not fail yet");

        let active = json!({
            "resource": runtime_access_resource(
                "PortForward",
                "pf-1",
                "active",
                json!({ "client_endpoint": "tcp://127.0.0.1:18080" }),
            )
        });
        ensure_runtime_port_forward_success(&active).expect("active port-forward should pass");

        let mut failed_resource =
            runtime_access_resource("PortForward", "pf-1", "failed", json!({ "error": "boom" }));
        if let Some(status) = failed_resource
            .get_mut("status")
            .and_then(Value::as_object_mut)
        {
            status.insert("reason".to_string(), json!("WorkerPortForwardFailed"));
        }
        let failed = json!({ "resource": failed_resource });
        let failed_err = ensure_runtime_port_forward_success(&failed)
            .unwrap_err()
            .to_string();
        assert!(
            failed_err.contains("runtime port-forward PortForward/pf-1 ended with phase=failed")
        );
        assert!(failed_err.contains("reason=WorkerPortForwardFailed"));
    }

    #[test]
    fn runtime_exec_success_guard_reports_terminal_command_failures() {
        let pending = json!({
            "resource": runtime_access_resource(
                "Exec",
                "exec-1",
                "accepted",
                json!({ "mode": "worker_exec" }),
            )
        });
        ensure_runtime_exec_success(&pending).expect("async accepted exec should not fail yet");

        let ok = json!({
            "resource": runtime_access_resource(
                "Exec",
                "exec-1",
                "completed",
                json!({ "exit_code": 0 }),
            )
        });
        ensure_runtime_exec_success(&ok).expect("zero exit code should be successful");

        let nonzero = json!({
            "resource": runtime_access_resource(
                "Exec",
                "exec-1",
                "completed",
                json!({ "exit_code": 42 }),
            )
        });
        let nonzero_err = ensure_runtime_exec_success(&nonzero)
            .unwrap_err()
            .to_string();
        assert!(nonzero_err.contains("runtime exec Exec/exec-1 exited with code 42"));

        let mut failed_resource =
            runtime_access_resource("Exec", "exec-1", "failed", json!({ "message": "boom" }));
        if let Some(status) = failed_resource
            .get_mut("status")
            .and_then(Value::as_object_mut)
        {
            status.insert("reason".to_string(), json!("WorkerExecFailed"));
        }
        let failed = json!({ "resource": failed_resource });
        let failed_err = ensure_runtime_exec_success(&failed)
            .unwrap_err()
            .to_string();
        assert!(failed_err.contains("runtime exec Exec/exec-1 ended with phase=failed"));
        assert!(failed_err.contains("reason=WorkerExecFailed"));
    }

    #[test]
    fn port_forward_attach_uses_gce_iap_connection_handle() {
        let response = json!({
            "resource": {
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "PortForward",
                "metadata": { "name": "pf-1" },
                "spec": {
                    "target_port": 8080
                },
                "status": {
                    "phase": "active",
                    "connection": {
                        "mode": "gcp_iap_ssh",
                        "project_id": "proj-1",
                        "zone": "us-central1-a",
                        "instance_name": "runner-vm-1",
                        "target_host": "10.0.0.2",
                        "target_port": 8080
                    }
                }
            }
        });

        let plan = runtime_port_forward_attach_plan(&response, Some(18080), 8080)
            .expect("gce iap port-forward attach spec should parse");
        let RuntimePortForwardAttachPlan::GceIap(spec) = plan else {
            panic!("expected GCE IAP attach plan");
        };

        assert_eq!(
            spec,
            RuntimePortForwardAttachSpec {
                project_id: "proj-1".to_string(),
                zone: "us-central1-a".to_string(),
                instance_name: "runner-vm-1".to_string(),
                target_host: "10.0.0.2".to_string(),
                target_port: 8080,
                local_port: 18080,
            }
        );
        assert_eq!(
            gcloud_iap_port_forward_args(&spec),
            vec![
                "compute",
                "ssh",
                "runner-vm-1",
                "--project",
                "proj-1",
                "--zone",
                "us-central1-a",
                "--tunnel-through-iap",
                "--",
                "-N",
                "-L",
                "127.0.0.1:18080:10.0.0.2:8080",
            ]
        );
    }

    #[test]
    fn port_forward_attach_accepts_client_reachable_handles() {
        let response = json!({
            "resource": {
                "kind": "PortForward",
                "metadata": { "name": "pf-1" },
                "spec": { "target_port": 8080, "local_port": 18080 },
                "status": {
                    "phase": "active",
                    "connection": {
                        "kind": "loopback",
                        "client_reachable": true,
                        "client_endpoint": "tcp://127.0.0.1:18080"
                    }
                }
            }
        });

        let plan = runtime_port_forward_attach_plan(&response, None, 8080)
            .expect("client-reachable port-forward attach handle should parse");

        assert_eq!(
            plan,
            RuntimePortForwardAttachPlan::ClientEndpoint(
                RuntimePortForwardClientEndpointAttachSpec {
                    endpoint: "tcp://127.0.0.1:18080".to_string(),
                    local_port: Some(18080),
                }
            )
        );
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
    fn runs_get_can_list_runtime_resources_by_kind() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(3, |request, index| {
            assert_eq!(request.method, "GET");
            match index {
                0 => assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources?limit=5&kind=Trial"
                ),
                1 => assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources?kind=Trial&label%5Fselector=app%3Ddemo"
                ),
                2 => assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources?kind=RunnerInstance%2CTrial"
                ),
                _ => unreachable!(),
            }
            json!({
                "cloud_run_id": "run-1",
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceList",
                "metadata": {
                    "resourceVersion": "rv-1",
                    "continue": null,
                    "remainingItemCount": 0,
                    "total": 1,
                    "returned": 1
                },
                "core_run_ids": [],
                "resources": [runtime_resource("Trial", "trial-1")]
            })
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_get_resource_list_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "Trial".to_string(),
            "--limit".to_string(),
            "5".to_string(),
        ])
        .expect("runs get --kind should list runtime resources");
        run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial".to_string(),
            "--label-selector".to_string(),
            "app=demo".to_string(),
        ])
        .expect("runs get <kind> should list runtime resources");
        run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "RunnerInstance,Trial".to_string(),
        ])
        .expect("runs get <kind,kind> should list multiple runtime resource kinds");

        let requests = server.join();
        assert_eq!(requests.len(), 3);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_resources_wide_fetches_discovery_and_renders_printer_columns() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(2, |request, index| {
            assert_eq!(request.method, "GET");
            match index {
                0 => {
                    assert_eq!(
                        request.path,
                        "/v1/runs/run%2D1/runtime/resources?kind=RunnerInstance"
                    );
                    let mut runner = runtime_resource("RunnerInstance", "runner-1");
                    if let Some(status) = runner.get_mut("status").and_then(Value::as_object_mut) {
                        status.insert(
                            "access".to_string(),
                            json!({
                                "reachable": true,
                                "port_forward": true,
                                "exec": false,
                                "runner_instance_id": "runner-instance-1"
                            }),
                        );
                        status.insert("provider".to_string(), json!("gcp"));
                        status.insert("instance_name".to_string(), json!("runner-vm-1"));
                        status.insert("actions".to_string(), json!(["cordon", "drain"]));
                    }
                    json!({
                        "cloud_run_id": "run-1",
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RuntimeResourceList",
                        "metadata": { "resourceVersion": "rv-1", "continue": null, "remainingItemCount": 0, "total": 1, "returned": 1 },
                        "core_run_ids": [],
                        "resources": [runner]
                    })
                }
                1 => {
                    assert_eq!(request.path, "/v1/runs/run%2D1/runtime/api-resources");
                    json!({
                        "cloud_run_id": "run-1",
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RuntimeApiResourceList",
                        "generated_at": "2026-06-18T00:00:00Z",
                        "core_run_ids": ["core-run-1"],
                        "resources": [{
                            "cloud_run_id": "run-1",
                            "generated_at": "2026-06-18T00:00:00Z",
                            "core_run_ids": ["core-run-1"],
                            "kind": "RunnerInstance",
                            "printerColumns": [
                                { "name": "Name", "type": "string", "jsonPath": ".metadata.name", "priority": 0 },
                                { "name": "Phase", "type": "string", "jsonPath": ".status.phase", "priority": 0 },
                                { "name": "Ready", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Ready\")].status", "priority": 0 },
                                { "name": "Reachable", "type": "boolean", "jsonPath": ".status.access.reachable", "priority": 0 },
                                { "name": "PortForward", "type": "boolean", "jsonPath": ".status.access.port_forward", "priority": 0 },
                                { "name": "Exec", "type": "boolean", "jsonPath": ".status.access.exec", "priority": 0 },
                                { "name": "Provider", "type": "string", "jsonPath": ".status.provider", "priority": 0 },
                                { "name": "VM", "type": "string", "jsonPath": ".status.instance_name", "priority": 0 },
                                { "name": "Actions", "type": "string", "jsonPath": ".status.actions", "priority": 1 }
                            ]
                        }]
                    })
                }
                _ => unreachable!(),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_resources_wide_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "RunnerInstance".to_string(),
            "--wide".to_string(),
        ])
        .expect("runs resources --wide should fetch discovery printer columns");

        let requests = server.join();
        assert_eq!(requests.len(), 2);

        let mut runner = runtime_resource("RunnerInstance", "runner-1");
        if let Some(status) = runner.get_mut("status").and_then(Value::as_object_mut) {
            status.insert(
                "access".to_string(),
                json!({
                    "reachable": true,
                    "port_forward": true,
                    "exec": false,
                    "runner_instance_id": "runner-instance-1"
                }),
            );
            status.insert("provider".to_string(), json!("gcp"));
            status.insert("instance_name".to_string(), json!("runner-vm-1"));
            status.insert("actions".to_string(), json!(["cordon", "drain"]));
        }
        let rendered = runtime_resources_wide_lines(
            &json!({
                "metadata": { "resourceVersion": "rv-1", "continue": null, "remainingItemCount": 0, "total": 1, "returned": 1 },
                "resources": [runner]
            }),
            &json!({
                "resources": [{
                    "kind": "RunnerInstance",
                    "printerColumns": [
                        { "name": "Name", "jsonPath": ".metadata.name", "priority": 0 },
                        { "name": "Ready", "jsonPath": ".status.conditions[?(@.type==\"Ready\")].status", "priority": 0 },
                        { "name": "Reachable", "jsonPath": ".status.access.reachable", "priority": 0 },
                        { "name": "Provider", "jsonPath": ".status.provider", "priority": 0 },
                        { "name": "Actions", "jsonPath": ".status.actions", "priority": 1 }
                    ]
                }]
            }),
        )
        .join("\n");
        assert!(rendered.contains("resource_version: rv-1"));
        assert!(rendered.contains("remaining: 0"));
        assert!(rendered.contains("total: 1"));
        assert!(rendered.contains("returned: 1"));
        assert!(rendered.contains("RunnerInstance: 1"));
        assert!(rendered.contains("NAME      READY  REACHABLE  PROVIDER  ACTIONS"));
        assert!(rendered.contains("runner-1  True   true       gcp       cordon,drain"));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runtime_resources_wide_renders_access_resource_operator_columns() {
        let mut port_forward = runtime_access_resource(
            "PortForward",
            "pf-1",
            "active",
            json!({
                "mode": "gcp_iap_ssh",
                "target_port": 8080,
                "local_port": 18080,
                "provider_tunnel_url": "gcp-iap-ssh://projects/p/zones/z/instances/i"
            }),
        );
        port_forward["spec"] = json!({
            "target_ref": {
                "kind": "Trial",
                "name": "trial-1",
                "resourceVersion": "sha256:trial-rv"
            },
            "target_port": 8080,
            "local_port": 18080
        });
        port_forward["status"]["runner_binding"] = json!({
            "runner_instance_id": "runner-1",
            "worker_id": "worker-1"
        });
        port_forward["status"]["expires_at"] = json!("2026-06-20T12:00:00Z");
        port_forward["status"]["conditions"] = json!([
            { "type": "Ready", "status": "True" },
            { "type": "ClientReachable", "status": "True" }
        ]);
        port_forward["audit"] = json!({
            "source": "cloud.runtime_access_requests",
            "requester": "issuer:user-a"
        });

        let mut exec = runtime_access_resource(
            "Exec",
            "exec-1",
            "completed",
            json!({
                "mode": "worker_exec",
                "exit_code": 0,
                "stdout_bytes": 20_000,
                "stdout_tail_bytes": 16_000,
                "stdout_tail_truncated": true,
                "stderr_bytes": 5,
                "stderr_tail_bytes": 5,
                "stderr_tail_truncated": false
            }),
        );
        exec["spec"] = json!({
            "target_ref": {
                "kind": "TrialContainer",
                "name": "trial-1.agent.container-1",
                "resourceVersion": "sha256:container-rv"
            },
            "command": ["python", "-V"]
        });
        exec["status"]["runner_binding"] = json!({
            "runner_instance_id": "runner-1",
            "worker_id": "worker-1"
        });
        exec["status"]["expires_at"] = json!("2026-06-20T12:05:00Z");
        exec["audit"] = json!({
            "source": "cloud.runtime_access_requests",
            "requester": "issuer:user-a"
        });

        let rendered = runtime_resources_wide_lines(
            &json!({
                "metadata": { "resourceVersion": "rv-access", "continue": null, "remainingItemCount": 0, "total": 2, "returned": 2 },
                "resources": [port_forward, exec]
            }),
            &json!({
                "resources": [
                    {
                        "kind": "PortForward",
                        "printerColumns": [
                            { "name": "Name", "jsonPath": ".metadata.name", "priority": 0 },
                            { "name": "Target", "jsonPath": ".spec.target_ref.name", "priority": 0 },
                            { "name": "TargetKind", "jsonPath": ".spec.target_ref.kind", "priority": 1 },
                            { "name": "TargetRV", "jsonPath": ".spec.target_ref.resourceVersion", "priority": 1 },
                            { "name": "TargetPort", "jsonPath": ".spec.target_port", "priority": 0 },
                            { "name": "LocalPort", "jsonPath": ".spec.local_port", "priority": 0 },
                            { "name": "ClientReachable", "jsonPath": ".status.conditions[?(@.type==\"ClientReachable\")].status", "priority": 0 },
                            { "name": "Runner", "jsonPath": ".status.runner_binding.runner_instance_id", "priority": 0 },
                            { "name": "Worker", "jsonPath": ".status.runner_binding.worker_id", "priority": 1 },
                            { "name": "Requester", "jsonPath": ".audit.requester", "priority": 1 },
                            { "name": "Mode", "jsonPath": ".status.connection.mode", "priority": 1 },
                            { "name": "ProviderTunnel", "jsonPath": ".status.connection.provider_tunnel_url", "priority": 1 },
                            { "name": "Expires", "jsonPath": ".status.expires_at", "priority": 1 }
                        ]
                    },
                    {
                        "kind": "Exec",
                        "printerColumns": [
                            { "name": "Name", "jsonPath": ".metadata.name", "priority": 0 },
                            { "name": "Target", "jsonPath": ".spec.target_ref.name", "priority": 0 },
                            { "name": "TargetKind", "jsonPath": ".spec.target_ref.kind", "priority": 1 },
                            { "name": "TargetRV", "jsonPath": ".spec.target_ref.resourceVersion", "priority": 1 },
                            { "name": "Command", "jsonPath": ".spec.command", "priority": 0 },
                            { "name": "Exit", "jsonPath": ".status.connection.exit_code", "priority": 0 },
                            { "name": "StdoutBytes", "jsonPath": ".status.connection.stdout_bytes", "priority": 1 },
                            { "name": "StdoutTailBytes", "jsonPath": ".status.connection.stdout_tail_bytes", "priority": 1 },
                            { "name": "StdoutTruncated", "jsonPath": ".status.connection.stdout_tail_truncated", "priority": 1 },
                            { "name": "StderrBytes", "jsonPath": ".status.connection.stderr_bytes", "priority": 1 },
                            { "name": "StderrTailBytes", "jsonPath": ".status.connection.stderr_tail_bytes", "priority": 1 },
                            { "name": "StderrTruncated", "jsonPath": ".status.connection.stderr_tail_truncated", "priority": 1 },
                            { "name": "Runner", "jsonPath": ".status.runner_binding.runner_instance_id", "priority": 0 },
                            { "name": "Worker", "jsonPath": ".status.runner_binding.worker_id", "priority": 1 },
                            { "name": "Requester", "jsonPath": ".audit.requester", "priority": 1 },
                            { "name": "Mode", "jsonPath": ".status.connection.mode", "priority": 1 },
                            { "name": "Expires", "jsonPath": ".status.expires_at", "priority": 1 }
                        ]
                    }
                ]
            }),
        )
        .join("\n");

        assert!(rendered.contains("resource_version: rv-access"));
        assert!(rendered.contains("PortForward: 1"));
        assert!(rendered.contains("Exec: 1"));
        assert!(rendered.contains("TARGET RV"));
        assert!(rendered.contains("PROVIDER TUNNEL"));
        assert!(rendered.contains("STDOUT BYTES"));
        assert!(rendered.contains("STDOUT TAIL BYTES"));
        assert!(rendered.contains("STDOUT TRUNCATED"));
        assert!(rendered.contains("STDERR BYTES"));
        assert!(rendered.contains("STDERR TAIL BYTES"));
        assert!(rendered.contains("STDERR TRUNCATED"));
        assert!(rendered.contains("pf-1"));
        assert!(rendered.contains("trial-1"));
        assert!(rendered.contains("sha256:trial-rv"));
        assert!(rendered.contains("gcp-iap-ssh://projects/p/zones/z/instances/i"));
        assert!(rendered.contains("exec-1"));
        assert!(rendered.contains("trial-1.agent.container-1"));
        assert!(rendered.contains("sha256:container-rv"));
        assert!(rendered.contains("python -V"));
        assert!(rendered.contains("20000"));
        assert!(rendered.contains("16000"));
        assert!(rendered.contains("true"));
        assert!(rendered.contains("false"));
        assert!(rendered.contains("worker_exec"));
        assert!(rendered.contains("issuer:user-a"));
    }

    #[test]
    fn runtime_resources_wide_renders_event_involved_object_columns() {
        let mut event = runtime_resource("Event", "event-runtime-access-port-forward-requested-7");
        event["spec"] = json!({
            "event_type": "runtime.access.port_forward.requested",
            "row_seq": 7,
            "involved_object": {
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "PortForward",
                "name": "pf-1",
                "uid": "pf-1"
            },
            "involved_resources": [
                { "kind": "PortForward", "name": "pf-1", "uid": "pf-1" },
                { "kind": "TrialContainer", "name": "trial-1.agent.container-1" }
            ]
        });
        event["status"] = json!({
            "phase": "recorded",
            "reason": "RuntimeAccessPortForwardRequested",
            "message": "Port forward requested",
            "involved": "PortForward/pf-1",
            "involved_kind": "PortForward",
            "involved_name": "pf-1",
            "involved_uid": "pf-1",
            "involved_count": 2
        });

        let rendered = runtime_resources_wide_lines(
            &json!({
                "metadata": { "resourceVersion": "rv-events", "continue": null, "remainingItemCount": 0, "total": 1, "returned": 1 },
                "resources": [event]
            }),
            &json!({
                "resources": [{
                    "kind": "Event",
                    "printerColumns": [
                        { "name": "Name", "jsonPath": ".metadata.name", "priority": 0 },
                        { "name": "Involved", "jsonPath": ".status.involved", "priority": 0 },
                        { "name": "Type", "jsonPath": ".spec.event_type", "priority": 0 },
                        { "name": "Seq", "jsonPath": ".spec.row_seq", "priority": 0 },
                        { "name": "InvolvedKind", "jsonPath": ".status.involved_kind", "priority": 1 },
                        { "name": "Message", "jsonPath": ".status.message", "priority": 1 }
                    ]
                }]
            }),
        )
        .join("\n");

        assert!(rendered.contains("resource_version: rv-events"));
        assert!(rendered.contains("Event: 1"));
        assert!(rendered.contains("INVOLVED"));
        assert!(rendered.contains("INVOLVED KIND"));
        assert!(rendered.contains("PortForward/pf-1"));
        assert!(rendered.contains("runtime.access.port_forward.requested"));
        assert!(rendered.contains("Port forward requested"));
    }

    #[test]
    fn runtime_resources_name_lines_render_pipeline_refs() {
        let lines = runtime_resources_name_lines(&json!({
            "metadata": { "resourceVersion": "rv-name" },
            "resources": [
                runtime_resource("RunnerInstance", "runner-1"),
                runtime_resource("Trial", "trial-1"),
                runtime_resource("Exec", "exec-1")
            ]
        }))
        .expect("runtime resource name output should render Kind/name refs");

        assert_eq!(
            lines,
            vec![
                "RunnerInstance/runner-1".to_string(),
                "Trial/trial-1".to_string(),
                "Exec/exec-1".to_string(),
            ]
        );
    }

    #[test]
    fn runs_resources_output_name_lists_refs_without_discovery() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(3, |request, index| {
            assert_eq!(request.method, "GET");
            match index {
                0..=2 => {
                    assert_eq!(
                        request.path,
                        "/v1/runs/run%2D1/runtime/resources?kind=Trial"
                    );
                    json!({
                        "cloud_run_id": "run-1",
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RuntimeResourceList",
                        "metadata": { "resourceVersion": "rv-name", "continue": null, "remainingItemCount": 0, "total": 1, "returned": 1 },
                        "core_run_ids": [],
                        "resources": [runtime_resource("Trial", "trial-1")]
                    })
                }
                _ => unreachable!(),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_resources_output_name_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "Trial".to_string(),
            "--output".to_string(),
            "name".to_string(),
        ])
        .expect("runs resources --output name should render resource refs");

        run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--kind".to_string(),
            "Trial".to_string(),
            "-o".to_string(),
            "name".to_string(),
        ])
        .expect("runs resources -o name should render resource refs");

        run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial".to_string(),
            "-o".to_string(),
            "name".to_string(),
        ])
        .expect("runs get <kind> -o name should render resource refs");

        let requests = server.join();
        assert_eq!(requests.len(), 3);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_resources_category_forwards_server_owned_category_selector() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, index| {
            assert_eq!(request.method, "GET");
            match index {
                0 => {
                    assert_eq!(
                        request.path,
                        "/v1/runs/run%2D1/runtime/resources?category=runner"
                    );
                    json!({
                        "cloud_run_id": "run-1",
                        "apiVersion": "bucephalus.dev/v1alpha1",
                        "kind": "RuntimeResourceList",
                        "metadata": { "resourceVersion": "rv-runner", "continue": null, "remainingItemCount": 0, "total": 2, "returned": 2 },
                        "core_run_ids": [],
                        "resources": [
                            runtime_resource("RunnerInstance", "runner-1"),
                            runtime_resource("RunnerAttempt", "attempt-1")
                        ]
                    })
                }
                _ => unreachable!(),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_resources_category_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "resources".to_string(),
            "run-1".to_string(),
            "--category".to_string(),
            "runner".to_string(),
        ])
        .expect("runs resources --category should forward API category selectors");

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_get_can_fetch_raw_runtime_resource_by_identity() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(2, |request, index| {
            assert_eq!(request.method, "GET");
            match index {
                0 => assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/Trial/trial%2D1?view=resource"
                ),
                1 => assert_eq!(
                    request.path,
                    "/v1/runs/run%2D1/runtime/resources/Trial/trial%2D2?view=describe"
                ),
                _ => unreachable!(),
            }
            if index == 0 {
                runtime_resource("Trial", "trial-1")
            } else {
                json!({
                    "cloud_run_id": "run-1",
                    "apiVersion": "bucephalus.dev/v1alpha1",
                    "kind": "RuntimeResourceDescribe",
                    "generated_at": "2026-06-18T00:00:00Z",
                    "core_run_ids": [],
                    "resource": runtime_resource("Trial", "trial-2"),
                    "operations": [],
                    "related_resources": [],
                    "event_list": { "events": [] }
                })
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("runs_get_resource_item_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("test-token")),
        ]);

        run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial/trial-1".to_string(),
        ])
        .expect("runs get Kind/name should fetch the raw runtime resource by default");
        run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial".to_string(),
            "trial-2".to_string(),
            "--view".to_string(),
            "describe".to_string(),
        ])
        .expect("runs get KIND NAME should preserve an explicit describe view");

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn runs_get_rejects_ambiguous_runtime_resource_kind_aliases_before_network() {
        let err = run(vec![
            "runs".to_string(),
            "get".to_string(),
            "run-1".to_string(),
            "Trial".to_string(),
            "--kind".to_string(),
            "RunnerInstance".to_string(),
        ])
        .unwrap_err()
        .to_string();

        assert!(err
            .contains("runtime resource kind must be provided either positionally or with --kind"));
        assert!(!err.contains("hosted API URL"));
    }

    #[test]
    fn runs_resources_rejects_mismatched_cloud_run_id() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(1, |request, _index| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/v1/runs/run%2D1/runtime/resources");
            json!({
                "cloud_run_id": "run-2",
                "resources": []
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
            "resources".to_string(),
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
            "--backend".to_string(),
            "runner-docker".to_string(),
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
                "backend": "runner-docker"
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

        for args in [
            vec!["inspect", "sha256:short"],
            vec![
                "doctor",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaA",
            ],
            vec!["run", "not-a-digest"],
            vec!["packages", "inspect", "sha256:short"],
            vec!["experiments", "doctor", "sha256:short"],
            vec!["runs", "create", "sha256:short"],
        ] {
            let err = run(args.into_iter().map(String::from).collect())
                .unwrap_err()
                .to_string();
            assert!(err.contains("package digest must be sha256:<64 lowercase hex chars>"));
            assert!(!err.contains("hosted API URL"));
        }

        let resource_identity_err = run(vec![
            "runs".to_string(),
            "describe".to_string(),
            "run-1".to_string(),
            "RunnerInstance".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(resource_identity_err.contains("runtime resource must be written as Kind/name"));

        let exec_separator_err = run(vec![
            "runs".to_string(),
            "exec".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(exec_separator_err.contains("requires `-- COMMAND [ARG...]`"));

        let wait_conflict_err = run(vec![
            "runs".to_string(),
            "port-forward".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--target-port".to_string(),
            "8080".to_string(),
            "--wait-seconds".to_string(),
            "1".to_string(),
            "--no-wait".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(wait_conflict_err.contains("--no-wait and --wait-seconds are mutually exclusive"));

        let bad_log_stream_err = run(vec![
            "runs".to_string(),
            "logs".to_string(),
            "run-1".to_string(),
            "RunnerInstance/runner-1".to_string(),
            "--stream".to_string(),
            "combined".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(bad_log_stream_err.contains("--stream must be stdout or stderr"));
        assert!(!bad_log_stream_err.contains("hosted API URL"));

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

        assert!(help.contains("buc build <experiment.yaml|package-dir|package.tgz>"));
        assert!(help.contains("buc login [--no-browser] [--json]"));
        assert!(help.contains("buc logout [--dry-run]"));
        assert!(help.contains("buc auth status"));
        assert!(help.contains("Authoring context:"));
        assert!(help.contains("Auth:"));
        assert!(help.contains("Sign in with `buc login`"));
        assert!(help.contains("Advanced overrides:"));
        assert!(help.contains("--api-url URL        Development"));
        assert!(!help.contains("buc [--api-url URL]"));
        assert!(help.contains("refreshing"));
        assert!(!help.contains("--context-root DIR"));
        assert!(help.contains("buc run <package-digest>"));
        assert!(help.contains("Long-form nouns:"));
        assert!(help.contains("hosted Cloud readiness"));
        assert!(help.contains("buc author canonicalize"));
        assert!(help.contains("buc author resolve"));
        assert!(help.contains("buc author validate"));
        assert!(help.contains("buc packages list"));
        assert!(help.contains("buc secrets put <name>"));
        assert!(help.contains("buc runs list"));
        assert!(help.contains("buc runs explain"));
        assert!(help.contains("buc runs resources"));
        assert!(help.contains("buc runs tree"));
        assert!(help.contains("buc runs describe"));
        assert!(help.contains("buc runs wait"));
        assert!(help.contains("buc runs can-i"));
        assert!(help.contains("buc runs port-forward"));
        assert!(help.contains("buc runs exec"));
        assert!(help.contains("buc runs logs"));
        assert!(help.contains("buc runs content"));
        assert!(help.contains("buc runs events"));
        assert!(help.contains("buc runs audit"));
        assert!(help.contains("buc runs top"));
        assert!(help.contains("buc runs get <run-id> [kind|--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--wide|--output name|-o name] [--json]"));
        assert!(RUNS_RESOURCES_HELP.contains("buc runs resources <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--wide|--output name|-o name] [--json]"));
        assert!(RUNS_METRICS_HELP.contains("buc runs metrics <run-id> [Kind/name] [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]"));
        assert!(RUNS_TOP_HELP.contains("buc runs top <run-id> [Kind/name] [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]"));
        assert!(RUNS_CAN_I_HELP
            .contains("buc runs can-i <run-id> port-forward RunnerInstance/<runner-name>"));
        assert!(
            RUNS_CAN_I_HELP.contains("buc runs can-i <run-id> top TrialContainer/<container-name>")
        );
        assert!(
            RUNS_CAN_I_HELP.contains("buc runs can-i <run-id> audit RunnerInstance/<runner-name>")
        );
        assert!(RUNS_CAN_I_HELP
            .contains("buc runs can-i <run-id> logs/stdout TrialContainer/<container-name>"));
        assert!(RUNS_CAN_I_HELP
            .contains("buc runs can-i <run-id> logs/stderr TrialContainer/<container-name>"));
        assert!(RUNS_CAN_I_HELP
            .contains("buc runs can-i <run-id> content TrialArtifact/<artifact-name>"));
        assert!(
            RUNS_CAN_I_HELP.contains("buc runs can-i <run-id> cordon RunnerInstance/<runner-name>")
        );
        assert!(RUNS_CAN_I_HELP.contains("buc runs can-i <run-id> cancel Exec/<exec-name>"));
        assert!(help.contains("buc runs events <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]"));
        assert!(help.contains("buc runs audit <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]"));
        assert!(RUNS_EVENTS_HELP.contains("List run-wide or resource-scoped runtime audit/events."));
        let retired_events_scope = format!(
            "List run-{}",
            "scoped or resource-scoped runtime audit/events."
        );
        assert!(!RUNS_EVENTS_HELP.contains(&retired_events_scope));
        assert!(RUNS_EVENTS_HELP.contains("buc runs events <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]"));
        assert!(RUNS_AUDIT_HELP.contains("buc runs audit <run-id> [Kind/name] [--limit N] [--after-row-seq N] [--continue TOKEN] [--event-type TYPE] [--source SOURCE] [--resource-kind KIND] [--resource-name NAME] [--trial-id ID] [--task-id ID] [--follow] [--interval-seconds N] [--max-polls N] [--json]"));
        assert!(help.contains("buc runs watch <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--resource-version VERSION] [--known-resource Kind/name=VERSION] [--follow] [--interval-seconds N] [--max-polls N] [--json]"));
        assert!(help.contains("buc runs explain <run-id> <kind> [--json]"));
        assert!(RUNS_EXPLAIN_HELP.contains(
            "aliases, categories, verbs, subresources, actions, access, printer columns"
        ));
        assert!(help.contains("buc runs tree <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--limit N] [--continue TOKEN] [--json]"));
        assert!(RUNS_TREE_HELP.contains("owner-reference tree"));
        assert!(RUNS_HELP.contains("buc runs watch <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--resource-version VERSION] [--known-resource Kind/name=VERSION] [--follow] [--interval-seconds N] [--max-polls N] [--json]"));
        assert!(RUNS_WATCH_HELP.contains("buc runs watch <run-id> [--kind KIND|--category CATEGORY] [--label-selector EXPR] [--field-selector EXPR] [--resource-version VERSION] [--known-resource Kind/name=VERSION] [--follow] [--interval-seconds N] [--max-polls N] [--json]"));
        assert!(!help.contains("buc runs results"));
        assert!(!help.contains("buc runs value"));
        assert!(!help.contains("runner-pool"));
        assert!(!help.contains("runner-instance"));
        assert!(!help.contains("build-upload"));
        assert!(!help.contains("--core-cmd"));
        assert!(!help.contains("bucephalus-cloud"));
    }

    #[test]
    fn runtime_action_help_exposes_reviewed_precondition_flags_on_aliases() {
        let help = help_text();

        assert!(help.contains("buc runs port-forward <run-id> <Kind/name> --target-port PORT [--local-port PORT] [--attach] [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(help.contains("buc runs exec <run-id> <Kind/name> [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json] -- COMMAND [ARG...]"));
        assert!(RUNS_HELP.contains("buc runs port-forward <run-id> <Kind/name> --target-port PORT [--local-port PORT] [--attach] [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_HELP.contains("buc runs exec <run-id> <Kind/name> [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json] -- COMMAND [ARG...]"));
        assert!(RUNS_PORT_FORWARD_HELP.contains("buc runs port-forward <run-id> <Kind/name> --target-port PORT [--local-port PORT] [--attach] [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_EXEC_HELP.contains("buc runs exec <run-id> <Kind/name> [--ttl-seconds N] [--wait-seconds N|--no-wait] [--reason TEXT] --resource-version VERSION [--json] -- COMMAND [ARG...]"));
        assert!(!RUNS_HELP.contains(
            "buc runs action <run-id> <Kind/name> <cordon|drain|uncordon|cancel|complete>"
        ));
        assert!(RUNS_ACTION_HELP.contains("Compatibility:"));
        assert!(RUNS_ACTION_HELP.contains("buc runs action <run-id> <Kind/name> <cordon|drain|uncordon|cancel|complete> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(help.contains("buc runs cordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_HELP.contains("buc runs cordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(help.contains("buc runs drain <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_HELP.contains("buc runs drain <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(help.contains("buc runs uncordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_HELP.contains("buc runs uncordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(help.contains("buc runs cancel <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_HELP.contains("buc runs cancel <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(help.contains("buc runs complete <run-id> <PortForward/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_HELP.contains("buc runs complete <run-id> <PortForward/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(help.contains("buc runs delete <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_HELP.contains("buc runs delete <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_DELETE_HELP.contains("buc runs delete <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_ACTION_HELP.contains("buc runs cordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_ACTION_HELP.contains("buc runs drain <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_ACTION_HELP.contains("buc runs uncordon <run-id> <RunnerInstance/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_ACTION_HELP.contains("buc runs cancel <run-id> <PortForward/name|Exec/name> [--reason TEXT] --resource-version VERSION [--json]"));
        assert!(RUNS_ACTION_HELP.contains("buc runs complete <run-id> <PortForward/name> [--reason TEXT] --resource-version VERSION [--json]"));
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
        assert!(known_hosted_command(Some("login"), None));
        assert!(known_hosted_command(Some("login"), Some("--api-url")));
        assert!(known_hosted_command(Some("logout"), None));
        assert!(known_hosted_command(Some("auth"), Some("status")));
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
        assert!(known_hosted_command(Some("runs"), Some("api-resources")));
        assert!(known_hosted_command(Some("runs"), Some("explain")));
        assert!(known_hosted_command(Some("runs"), Some("inspect")));
        assert!(known_hosted_command(Some("runs"), Some("resources")));
        assert!(known_hosted_command(Some("runs"), Some("tree")));
        assert!(known_hosted_command(Some("runs"), Some("describe")));
        assert!(known_hosted_command(Some("runs"), Some("status")));
        assert!(known_hosted_command(Some("runs"), Some("health")));
        assert!(known_hosted_command(Some("runs"), Some("metrics")));
        assert!(known_hosted_command(Some("runs"), Some("top")));
        assert!(known_hosted_command(Some("runs"), Some("watch")));
        assert!(known_hosted_command(Some("runs"), Some("events")));
        assert!(known_hosted_command(Some("runs"), Some("audit")));
        assert!(known_hosted_command(Some("runs"), Some("logs")));
        assert!(known_hosted_command(Some("runs"), Some("content")));
        assert!(known_hosted_command(Some("runs"), Some("wait")));
        assert!(known_hosted_command(Some("runs"), Some("can-i")));
        assert!(known_hosted_command(Some("runs"), Some("port-forward")));
        assert!(known_hosted_command(Some("runs"), Some("exec")));
        assert!(known_hosted_command(Some("runs"), Some("action")));
        assert!(known_hosted_command(Some("runs"), Some("complete")));
        assert!(known_hosted_command(Some("runs"), Some("delete")));
        assert!(!known_hosted_command(Some("runs"), Some("runtime")));
        assert!(!known_hosted_command(Some("runs"), Some("results")));
        assert!(!known_hosted_command(Some("runs"), Some("value")));
        assert!(!known_hosted_command(Some("runs"), Some("kv")));
        assert!(known_hosted_command(Some("drafts"), Some("diff")));
        assert!(!known_hosted_command(Some("runner-pool"), Some("create")));
        assert!(!known_hosted_command(Some("deploy"), None));
        assert!(!known_hosted_command(Some("build-upload"), None));
        assert!(!known_hosted_command(Some("draft"), Some("export")));
    }

    #[test]
    fn runs_audit_filter_covers_runner_lifecycle_and_access_events() {
        let event_types: Vec<&str> = RUNTIME_AUDIT_EVENT_TYPES.split(',').collect();
        for required in [
            "runtime.resource.runner_instance.cordoned",
            "runtime.resource.runner_instance.drained",
            "runtime.resource.runner_instance.offline",
            "runtime.resource.runner_instance.unhealthy",
            "runtime.resource.runner_instance.online",
            "runtime.resource.runner_instance.heartbeat_restored",
            "runtime.resource.runner_instance.uncordoned",
            "worker.runtime.image_pull.pulling",
            "worker.runtime.image_pull.pulled",
            "worker.runtime.image_pull.failed",
            "worker.runtime.secret_binding.materialized",
            "worker.runtime.sidecar_requirement.checking",
            "worker.runtime.sidecar_requirement.available",
            "worker.runtime.sidecar_requirement.failed",
            "worker.runtime.accelerator_requirement.checking",
            "worker.runtime.accelerator_requirement.available",
            "worker.runtime.accelerator_requirement.failed",
            "worker.runtime.network_perimeter.applying",
            "worker.runtime.network_perimeter.applied",
            "worker.runtime.network_perimeter.failed",
            "runtime.resource.operation.reviewed",
            "runtime.resource.operation.review.failed",
            "runtime.api_resources.read",
            "runtime.api_resources.read.failed",
            "runtime.resource.list.read",
            "runtime.resource.list.read.failed",
            "runtime.resource.watch.read",
            "runtime.resource.watch.read.failed",
            "runtime.resource.health.read",
            "runtime.resource.health.read.failed",
            "runtime.resource.describe.read",
            "runtime.resource.describe.read.failed",
            "runtime.resource.get.read",
            "runtime.resource.get.read.failed",
            "runtime.resource.events.read",
            "runtime.resource.events.read.failed",
            "runtime.resource.status.read",
            "runtime.resource.status.read.failed",
            "runtime.resource.metrics.read",
            "runtime.resource.metrics.read.failed",
            "runtime.resource.metrics.list.read",
            "runtime.resource.metrics.list.read.failed",
            "runtime.inspect.bundle.read",
            "runtime.inspect.bundle.read.failed",
            "runtime.access.port_forward.requested",
            "runtime.access.port_forward.accepted",
            "runtime.access.port_forward.active",
            "runtime.access.port_forward.completed",
            "runtime.access.port_forward.failed",
            "runtime.access.port_forward.expired",
            "runtime.access.port_forward.cancelled",
            "runtime.access.exec.requested",
            "runtime.access.exec.accepted",
            "runtime.access.exec.active",
            "runtime.access.exec.completed",
            "runtime.access.exec.failed",
            "runtime.access.exec.expired",
            "runtime.access.exec.cancelled",
            "runtime.resource.logs.read",
            "runtime.resource.logs.read.failed",
            "runtime.resource.content.read",
            "runtime.resource.content.read.failed",
        ] {
            assert!(
                event_types.contains(&required),
                "missing runtime audit event type: {required}"
            );
        }
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
        run(vec!["login".to_string(), "--help".to_string()])
            .expect("login help should render without API config");
        run(vec!["auth".to_string()]).expect("auth help should render without API config");
        run(vec![
            "auth".to_string(),
            "status".to_string(),
            "--help".to_string(),
        ])
        .expect("auth status help should render without API config");
        run(vec!["runs".to_string()]).expect("command group help should render without API config");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn auth_commands_are_first_class_buc_workflow() {
        let _lock = lock_env();
        let server = MockCloudServer::start_with_handler(4, |request, _index| {
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/v1/auth/config") => {
                    let host = request.header("host").expect("host header");
                    json!({
                        "schema_version": "bucephalus_cloud_auth_config_v1",
                        "issuer": format!("http://{host}"),
                        "client_id": "buc-client",
                        "audience": "buc-client",
                        "scope": "openid profile email"
                    })
                }
                ("GET", "/.well-known/oauth-authorization-server") => {
                    let host = request.header("host").expect("host header");
                    json!({
                        "device_authorization_endpoint": format!("http://{host}/oauth/device"),
                        "token_endpoint": format!("http://{host}/oauth/token")
                    })
                }
                ("POST", "/oauth/device") => json!({
                    "device_code": "device-1",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": "https://login.example/device",
                    "expires_in": 60,
                    "interval": 1
                }),
                ("POST", "/oauth/token") => json!({
                    "access_token": "access-123",
                    "refresh_token": "refresh-456",
                    "token_type": "Bearer",
                    "expires_in": 3600
                }),
                _ => panic!(
                    "unexpected auth workflow request: {} {}",
                    request.method, request.path
                ),
            }
        });
        let api_url = server.api_url();
        let home = temp_dir("auth_commands_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_API_URL_ENV, Some(api_url.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
            (cloud_login::BUCEPHALUS_CLOUD_OAUTH_ISSUER_ENV, None),
            (cloud_login::BUCEPHALUS_CLOUD_OAUTH_CLIENT_ID_ENV, None),
            (cloud_login::BUCEPHALUS_CLOUD_OAUTH_AUDIENCE_ENV, None),
        ]);

        run(vec!["auth".to_string(), "status".to_string()])
            .expect("buc auth status should read local auth state without API config");
        let missing_status = cloud_login::auth_status().unwrap();
        assert_eq!(missing_status["auth"]["status"], "missing");
        assert!(missing_status["auth"].get("oauth").is_none());
        assert_eq!(
            missing_status["auth"]["actions"][0]["description"],
            "Open the hosted Cloud sign-in flow and cache Cloud tokens for this user."
        );
        run(vec!["logout".to_string(), "--dry-run".to_string()])
            .expect("buc logout --dry-run should inspect local auth state without API config");

        run(vec!["login".to_string(), "--no-browser".to_string()])
            .expect("buc login should discover hosted auth config without an API URL argument");

        let status = cloud_login::auth_status().unwrap();
        assert_eq!(status["auth"]["status"], "ready");
        assert_eq!(status["auth"]["api_url"], api_url);
        let token = fs::read_to_string(home.join("auth/cloud_user_token")).unwrap();
        assert_eq!(token, "access-123\n");
        let requests = server.join();
        assert_eq!(requests[0].path, "/v1/auth/config");
        assert_eq!(requests[1].path, "/.well-known/oauth-authorization-server");
        assert_eq!(requests[2].path, "/oauth/device");
        assert_eq!(requests[3].path, "/oauth/token");

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
                    "kind": "sealed_package_importer",
                    "image_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "release_version": "0.3.37",
                    "git_sha": "abc123",
                    "os": "linux",
                    "arch": "x64"
                },
                "core": {
                    "executed": false,
                    "command": null,
                    "path": null,
                    "version": null,
                    "timeout_ms": null,
                    "reason": "Sealed package input was imported directly; Cloud did not run hosted Core authoring."
                },
                "package_contract": {
                    "input_kind": "sealed_package",
                    "authoring_compiler": null,
                    "authoring_provenance": {
                        "status": "external_unattested",
                        "source": "sealed_package_manifest",
                        "message": "Cloud verified sealed package integrity and hosted readiness, but sealed_run_package_v2 does not attest the package's original authoring environment."
                    },
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
        assert!(!text.contains("authoring_compiler: core_universal_v1"));
        assert!(text.contains("authoring_provenance: external_unattested/sealed_package_manifest"));
        assert!(text.contains("sealed_run_package_v2 does not attest"));
        assert!(text.contains("package_contract: sealed_run_package_v2"));
        assert!(text.contains("cloud_readiness_required: true"));
        assert!(text.contains("builder_core: not_run"));
        assert!(text.contains("Cloud did not run hosted Core authoring"));
        assert!(!text.contains("builder_core: bucephalus build version=0.3.37"));
        assert!(!text.contains("builder_timeout_ms: 600000"));
        assert!(text.contains("builder_image_digest: sha256:bbbb"));
        assert!(text.contains("build_environment_evidence_policy: warn"));
        assert!(text.contains("build_environment_evidence: partial"));
        assert!(text.contains("missing_build_evidence: builder_git_sha"));
        assert!(text.contains("cloud_run_requirements:"));
        assert!(text.contains("runner_capacity/run_unschedulable"));
    }

    #[test]
    fn package_provenance_summary_surfaces_authoring_status() {
        let lines = package_provenance_summary_lines(&json!({
            "package_provenance": {
                "schema_version": "cloud_package_provenance_v1",
                "status": "external_unattested",
                "source": "sealed_package_manifest",
                "input_kind": "sealed_package",
                "message": "Cloud verified readiness but did not author this package."
            }
        }));
        let text = lines.join("\n");

        assert!(text.contains("package_provenance: external_unattested/sealed_package_manifest"));
        assert!(text.contains("package_input_kind: sealed_package"));
        assert!(text.contains("did not author this package"));
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
    fn cloud_unauthorized_message_points_to_login_and_cached_state() {
        let _lock = lock_env();
        let home = temp_dir("cloud_unauthorized_auth_hint_home");
        let home_s = home.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(home_s.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);
        let context = CliContext {
            api_url: "https://api.example".to_string(),
            user_token: None,
            args: vec![],
            client: Client::new(),
        };

        let message = append_user_auth_hint(
            &context,
            "Bucephalus Cloud requires OAuth bearer authentication".to_string(),
        );

        assert!(message.contains("Cloud auth required"));
        assert!(message.contains("The CLI did not find a user bearer token"));
        assert!(message.contains("buc login"));
        assert!(message.contains("BUCEPHALUS_CLOUD_USER_TOKEN"));
        assert!(message.contains("auth/cloud_user_token"));
        assert!(message.contains("buc auth status"));
        assert!(message.contains("buc health"));
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
    fn sealed_package_contract_must_not_claim_authoring_compiler() {
        ensure_build_package_contract_matches(
            &json!({
                "build_environment": {
                    "package_contract": {
                        "input_kind": "sealed_package",
                        "authoring_compiler": null,
                        "authoring_provenance": {
                            "status": "external_unattested",
                            "source": "sealed_package_manifest"
                        },
                        "sealed_schema_version": "sealed_run_package_v2",
                        "readiness_schema_version": "hosted_cloud_readiness_v1",
                        "cloud_readiness_required": true
                    }
                }
            }),
            "sealed_package",
        )
        .expect("sealed package import contract should not claim hosted authoring");

        let claimed_compiler = ensure_build_package_contract_matches(
            &json!({
                "build_environment": {
                    "package_contract": {
                        "input_kind": "sealed_package",
                        "authoring_compiler": "core_universal_v1",
                        "authoring_provenance": {
                            "status": "external_unattested",
                            "source": "sealed_package_manifest"
                        },
                        "sealed_schema_version": "sealed_run_package_v2",
                        "readiness_schema_version": "hosted_cloud_readiness_v1",
                        "cloud_readiness_required": true
                    }
                }
            }),
            "sealed_package",
        )
        .unwrap_err()
        .to_string();
        assert!(
            claimed_compiler.contains("sealed package imports must report authoring_compiler=null")
        );

        let claimed_hosted_provenance = ensure_build_package_contract_matches(
            &json!({
                "build_environment": {
                    "package_contract": {
                        "input_kind": "sealed_package",
                        "authoring_compiler": null,
                        "authoring_provenance": {
                            "status": "hosted_attested",
                            "source": "hosted_core"
                        },
                        "sealed_schema_version": "sealed_run_package_v2",
                        "readiness_schema_version": "hosted_cloud_readiness_v1",
                        "cloud_readiness_required": true
                    }
                }
            }),
            "sealed_package",
        )
        .unwrap_err()
        .to_string();
        assert!(claimed_hosted_provenance.contains(
            "sealed package imports must report authoring_provenance=external_unattested"
        ));

        ensure_build_package_contract_matches(
            &json!({
                "build_environment": {
                    "package_contract": {
                        "input_kind": "authoring_context",
                        "authoring_compiler": "core_universal_v1",
                        "authoring_provenance": {
                            "status": "hosted_attested",
                            "source": "hosted_core"
                        },
                        "sealed_schema_version": "sealed_run_package_v2",
                        "readiness_schema_version": "hosted_cloud_readiness_v1",
                        "cloud_readiness_required": true
                    }
                }
            }),
            "authoring_context",
        )
        .expect("hosted authoring contract should report the compiler Cloud used");
    }

    #[test]
    fn build_execution_environment_must_match_source_kind() {
        ensure_build_execution_environment_matches(
            &json!({
                "build_environment": {
                    "builder": {
                        "kind": "hosted_authoring_builder"
                    },
                    "core": {
                        "executed": true,
                        "command": "bucephalus build",
                        "path": "/app/bin/bucephalus",
                        "timeout_ms": 600000
                    }
                }
            }),
            "authoring_context",
        )
        .expect("hosted authoring builds should prove Core execution");

        ensure_build_execution_environment_matches(
            &json!({
                "build_environment": {
                    "builder": {
                        "kind": "sealed_package_importer"
                    },
                    "core": {
                        "executed": false,
                        "command": null,
                        "path": null,
                        "version": null,
                        "timeout_ms": null
                    }
                }
            }),
            "sealed_package",
        )
        .expect("sealed package imports should prove Core did not execute");

        let authoring_importer = ensure_build_execution_environment_matches(
            &json!({
                "build_environment": {
                    "builder": {
                        "kind": "sealed_package_importer"
                    },
                    "core": {
                        "executed": false,
                        "command": null,
                        "path": null,
                        "timeout_ms": null
                    }
                }
            }),
            "authoring_context",
        )
        .unwrap_err()
        .to_string();
        assert!(authoring_importer.contains(
            "authoring_context builds must report builder.kind=hosted_authoring_builder"
        ));

        let sealed_executed = ensure_build_execution_environment_matches(
            &json!({
                "build_environment": {
                    "builder": {
                        "kind": "sealed_package_importer"
                    },
                    "core": {
                        "executed": true,
                        "command": "bucephalus build",
                        "path": "/app/bin/bucephalus",
                        "timeout_ms": 600000
                    }
                }
            }),
            "sealed_package",
        )
        .unwrap_err()
        .to_string();
        assert!(sealed_executed.contains("sealed_package imports must report core.executed=false"));
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

        ensure_resource_envelope(&json!({
            "resource": {
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "Exec",
                "metadata": { "name": "exec-1" }
            }
        }))
        .expect("runtime access resource envelopes should pass");

        let missing_resource = ensure_resource_envelope(&json!({ "accepted": true }))
            .unwrap_err()
            .to_string();
        assert!(missing_resource.contains("missing resource object"));
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

        let bad_executor_alias = runtime_options_from_args(&[
            "--runtime-option".to_string(),
            "executor=runner-docker".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(bad_executor_alias.contains("unsupported hosted Cloud runtime option `executor`"));

        let bad_cpu_alias =
            runtime_options_from_args(&["--runtime-option".to_string(), "cpu=4".to_string()])
                .unwrap_err()
                .to_string();
        assert!(bad_cpu_alias.contains("unsupported hosted Cloud runtime option `cpu`"));

        let bad_region = runtime_options_from_args(&[
            "--runtime-option".to_string(),
            "region=us-east-1".to_string(),
        ])
        .unwrap_err()
        .to_string();
        assert!(bad_region.contains("unsupported hosted Cloud runtime option `region`"));

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

        fn start_with_raw_handler(
            expected_requests: usize,
            handler: fn(&RecordedRequest, usize) -> (u16, &'static str, Vec<u8>),
        ) -> Self {
            Self::start_with_raw_header_handler(expected_requests, move |request, index| {
                let (status, content_type, body) = handler(request, index);
                (status, content_type, Vec::new(), body)
            })
        }

        fn start_with_raw_header_handler<F>(expected_requests: usize, mut handler: F) -> Self
        where
            F: FnMut(
                    &RecordedRequest,
                    usize,
                ) -> (
                    u16,
                    &'static str,
                    Vec<(&'static str, &'static str)>,
                    Vec<u8>,
                ) + Send
                + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let api_url = format!("http://{}", listener.local_addr().unwrap());
            let handle = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut requests = Vec::new();
                while requests.len() < expected_requests {
                    match listener.accept() {
                        Ok((mut stream, _addr)) => {
                            stream.set_nonblocking(false).expect(
                                "mock Cloud API stream should switch back to blocking reads",
                            );
                            let index = requests.len();
                            let request = read_http_request(&mut stream);
                            let (status, content_type, extra_headers, body) =
                                handler(&request, index);
                            write!(
                                stream,
                                "HTTP/1.1 {} OK\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
                                status,
                                content_type,
                                body.len()
                            )
                            .unwrap();
                            for (name, value) in extra_headers {
                                write!(stream, "{name}: {value}\r\n").unwrap();
                            }
                            write!(stream, "\r\n").unwrap();
                            stream.write_all(&body).unwrap();
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
                let project_manifest = project_manifest_from_build_request(request);
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
                            "entrypoint": "experiments/peter/experiment.yaml",
                            "project_manifest": project_manifest
                        },
                        "runtime_options": runtime_options,
                        "builder": {
                            "kind": "hosted_authoring_builder",
                            "image_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                            "release_version": "0.3.37",
                            "git_sha": "abc123",
                            "os": "linux",
                            "arch": "x64"
                        },
                        "core": {
                            "executed": true,
                            "command": "bucephalus build",
                            "path": "/app/bin/bucephalus",
                            "version": "0.3.37",
                            "timeout_ms": 600000
                        },
                        "package_contract": {
                            "input_kind": "authoring_context",
                            "authoring_compiler": "core_universal_v1",
                            "authoring_provenance": {
                                "status": "hosted_attested",
                                "source": "hosted_core"
                            },
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
            ("GET", "/v1/runs/run%2D1/runtime/api-resources") => json!({
                "cloud_run_id": "run-1",
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeApiResourceList",
                "generated_at": "2026-06-18T00:00:00Z",
                "core_run_ids": ["core-run-1"],
                "resources": [{
                    "cloud_run_id": "run-1",
                    "generated_at": "2026-06-18T00:00:00Z",
                    "core_run_ids": ["core-run-1"],
                    "kind": "RunnerInstance",
                    "name": "runnerinstances",
                    "verbs": ["get", "list"],
                    "access": ["port-forward", "exec"]
                }]
            }),
            ("GET", "/v1/runs/run%2D1/runtime/resources?limit=9&kind=RunnerInstance") => json!({
                "cloud_run_id": "run-1",
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceList",
                "metadata": { "resourceVersion": "rv-1", "continue": null, "remainingItemCount": 0, "total": 1, "returned": 1 },
                "core_run_ids": [],
                "resources": [runtime_resource("RunnerInstance", "runner-1")]
            }),
            ("GET", "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1") => json!({
                "cloud_run_id": "run-1",
                "apiVersion": "bucephalus.dev/v1alpha1",
                "kind": "RuntimeResourceDescribe",
                "generated_at": "2026-06-18T00:00:00Z",
                "core_run_ids": [],
                "resource": runtime_resource("RunnerInstance", "runner-1"),
                "operations": [],
                "related_resources": [],
                "event_list": { "events": [] }
            }),
            ("GET", "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/status") => json!({
                "cloud_run_id": "run-1",
                "resource_ref": { "apiVersion": "bucephalus.dev/v1alpha1", "kind": "RunnerInstance", "name": "runner-1" },
                "phase": "Running",
                "reason": null,
                "actions": ["cordon"]
            }),
            ("GET", "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/operations/port%2Dforward") => json!({
                "cloud_run_id": "run-1",
                "resource_ref": { "apiVersion": "bucephalus.dev/v1alpha1", "kind": "RunnerInstance", "name": "runner-1" },
                "operation": "port-forward",
                "supported": true,
                "reason": null
            }),
            ("GET", "/v1/runs/run%2D1/runtime/resources/health?kind=RunnerInstance") => json!({
                "cloud_run_id": "run-1",
                "summary": { "total": 1, "ready": 1, "problem": 0 },
                "resources": []
            }),
            ("GET", "/v1/runs/run%2D1/runtime/resources/metrics?limit=3&kind=RunnerInstance") => json!({
                "cloud_run_id": "run-1",
                "kind": "RuntimeResourceMetricsList",
                "metadata": { "resourceVersion": "metrics-rv-1", "continue": null, "remainingItemCount": 0, "total": 1, "returned": 1 },
                "summary": { "resources_total": 1, "metrics_total": 1 },
                "resources": [{
                    "resource_ref": { "apiVersion": "bucephalus.dev/v1alpha1", "kind": "RunnerInstance", "name": "runner-1" },
                    "summary": { "metrics_total": 1 },
                    "metrics": [{ "name": "lifecycle.ready", "value": 1, "unit": "state" }]
                }]
            }),
            ("GET", "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/events?limit=5&after%5Frow%5Fseq=12") => json!({
                "cloud_run_id": "run-1",
                "events": [{
                    "row_seq": 13,
                    "event_type": "trial_started"
                }]
            }),
            ("GET", path) if path == path_with_query(
                "/v1/runs/run%2D1/runtime/events",
                &[
                    ("limit", Some("7".to_string())),
                    ("event_type", Some(RUNTIME_AUDIT_EVENT_TYPES.to_string())),
                ],
            ) => json!({
                "cloud_run_id": "run-1",
                "kind": "RuntimeEventList",
                "event_filter": { "event_types": ["runtime.access.exec.requested"] },
                "events": [{
                    "row_seq": 14,
                    "event_type": "runtime.access.exec.requested",
                    "resource_refs": [{ "kind": "Exec", "name": "exec-1" }]
                }]
            }),
            ("POST", "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/port-forward") => json!({
                "resource": runtime_resource("PortForward", "pf-1")
            }),
            ("GET", "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2D1") => json!({
                "cloud_run_id": "run-1",
                "resource": runtime_access_resource(
                    "PortForward",
                    "pf-1",
                    "active",
                    json!({ "local_port": 18080, "client_endpoint": "tcp://127.0.0.1:18080" }),
                ),
                "operations": [],
                "related_resources": [],
                "event_list": { "events": [] }
            }),
            ("POST", "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/exec") => json!({
                "resource": runtime_resource("Exec", "exec-1")
            }),
            ("GET", "/v1/runs/run%2D1/runtime/resources/Exec/exec%2D1") => json!({
                "cloud_run_id": "run-1",
                "resource": runtime_access_resource(
                    "Exec",
                    "exec-1",
                    "completed",
                    json!({ "exit_code": 0, "stdout_tail": "Python 3.12.0\n" }),
                ),
                "operations": [],
                "related_resources": [],
                "event_list": { "events": [] }
            }),
            ("POST", "/v1/runs/run%2D1/runtime/resources/RunnerInstance/runner%2D1/actions/cordon") => json!({
                "cloud_run_id": "run-1",
                "resource": runtime_resource("RunnerInstance", "runner-1"),
                "operations": [],
                "related_resources": [],
                "event_list": { "events": [] }
            }),
            ("DELETE", "/v1/runs/run%2D1/runtime/resources/PortForward/pf%2D1") => json!({
                "cloud_run_id": "run-1",
                "resource": runtime_resource("PortForward", "pf-1"),
                "operations": [],
                "related_resources": [],
                "event_list": { "events": [] }
            }),
            ("GET", "/v1/runs/run%2D1/runtime/inspect?event%5Flimit=25") => json!({
                "cloud_run_id": "run-1",
                "resource_inventory": { "resources": [runtime_resource("RunnerInstance", "runner-1")] },
                "api_resources": { "resources": [] },
                "event_list": { "events": [] }
            }),
            _ => panic!(
                "unexpected mock Cloud API request #{index}: {} {}",
                request.method, request.path
            ),
        }
    }

    fn runtime_resource(kind: &str, name: &str) -> Value {
        json!({
            "apiVersion": "bucephalus.dev/v1alpha1",
            "kind": kind,
            "metadata": {
                "name": name,
                "labels": {},
                "annotations": {},
                "ownerReferences": []
            },
            "spec": {},
            "status": {
                "phase": "Running",
                "reason": null,
                "conditions": [{
                    "type": "Ready",
                    "status": "True",
                    "reason": "Ready",
                    "message": ""
                }]
            },
            "audit": {}
        })
    }

    fn runtime_access_resource(kind: &str, name: &str, phase: &str, connection: Value) -> Value {
        let mut resource = runtime_resource(kind, name);
        if let Some(status) = resource.get_mut("status").and_then(Value::as_object_mut) {
            status.insert("phase".to_string(), json!(phase));
            status.insert("connection".to_string(), connection);
        }
        resource
    }

    fn runtime_options_from_build_request(request: &RecordedRequest) -> Value {
        serde_json::from_slice::<Value>(&request.body)
            .ok()
            .and_then(|body| body.get("runtime_options").cloned())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    }

    fn project_manifest_from_build_request(request: &RecordedRequest) -> Value {
        serde_json::from_slice::<Value>(&request.body)
            .ok()
            .and_then(|body| body.get("project_manifest").cloned())
            .unwrap_or_else(|| json!(null))
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
