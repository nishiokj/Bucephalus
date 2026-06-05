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
    std::env::set_var(
        lab_runner::CLI_INVOKED_AT_MS_ENV,
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
                        "message": err.to_string(),
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
            "package_dir": package_dir.display().to_string(),
            "summary": summary_to_json(&summary),
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
        "package_dir": package_dir.display().to_string(),
        "summary": summary_to_json(&summary),
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
            .ok_or_else(|| anyhow!("invalid --env '{}': expected KEY=VALUE", raw))?;
        let key = key_raw.trim();
        if key.is_empty() {
            return Err(anyhow!("invalid --env '{}': key cannot be empty", raw));
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
            .ok_or_else(|| anyhow!("invalid --secret-file '{}': expected ID=PATH", raw))?;
        let key = key_raw.trim();
        if key.is_empty() {
            return Err(anyhow!(
                "invalid --secret-file '{}': id cannot be empty",
                raw
            ));
        }
        let value = value_raw.trim();
        if value.is_empty() {
            return Err(anyhow!(
                "invalid --secret-file '{}': path cannot be empty",
                raw
            ));
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

fn summary_to_json(summary: &lab_runner::ExperimentSummary) -> Value {
    json!({
        "experiment": summary.exp_id,
        "workload_type": summary.workload_type,
        "dataset": summary.dataset_path.display().to_string(),
        "tasks": summary.task_count,
        "replications": summary.replications,
        "variant_count": summary.variant_count,
        "total_trials": summary.total_trials,
        "agent_runtime": summary.agent_runtime_command,
        "image": summary.image,
        "network": summary.network_mode,
        "trajectory_path": summary.trajectory_path,
        "causal_extraction": summary.causal_extraction,
        "scheduling": summary.scheduling,
        "state_policy": summary.state_policy,
    })
}

fn run_result_to_json(result: &lab_runner::RunResult) -> Value {
    json!({
        "run_id": result.run_id,
        "run_dir": result.run_dir.display().to_string(),
        "account_db_path": result.account_db_path.display().to_string(),
    })
}

fn run_artifacts_to_json(result: &lab_runner::RunResult) -> Value {
    json!({
        "run_dir": result.run_dir.display().to_string(),
        "results": result.run_dir.join("results").display().to_string(),
    })
}

fn experiment_bundle_validation_to_json(
    validation: &lab_runner::ExperimentBundleValidation,
) -> Value {
    json!({
        "package_digest": validation.package_digest,
        "experiment_id": validation.experiment_id,
        "package_dir": validation.package_dir.display().to_string(),
        "smoke_tested": validation.smoke_tested,
        "smoke_run_id": validation.smoke_run_id,
        "smoke_tested_at_ms": validation.smoke_tested_at_ms,
    })
}

fn emit_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
    );
}

fn current_unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
