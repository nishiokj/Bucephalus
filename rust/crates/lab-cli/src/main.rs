use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lab_runner::analysis;
use lab_runner::provenance;
use lab_runner::schemas;

mod tui;
mod view_layout;
mod view_spec;

use crate::view_spec::{
    present_table, renderer_for_resolved, resolve_requested_view, resolved_view_from_spec,
    standard_view_source_label, standard_views_for_set, ResolvedView, ResolvedViewPlan,
    ViewRenderer,
};

#[derive(Parser)]
#[command(name = "bucephalus", version = "0.3.0", about = "Bucephalus CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExecutorArg {
    #[value(name = "local_docker")]
    LocalDocker,
    #[value(name = "modal")]
    Modal,
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

#[derive(Subcommand)]
enum Commands {
    Build {
        experiment: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        overrides: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Run static package hygiene checks against a sealed package")]
    CheckPackage {
        package: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Build task+agent prepared runtime images and emit the runner map")]
    PrepareRuntimeImages {
        package: PathBuf,
        #[arg(long)]
        repository: String,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        push: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = true)]
        skip_existing: bool,
        #[arg(long)]
        json: bool,
    },
    BuildRun {
        experiment: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        overrides: Option<PathBuf>,
        #[arg(long, value_enum)]
        executor: Option<ExecutorArg>,
        #[arg(long, value_enum)]
        materialize: Option<MaterializeArg>,
        #[arg(long, hide = true)]
        run_root: Option<PathBuf>,
        #[arg(long = "env", value_name = "KEY=VALUE", action = ArgAction::Append)]
        runtime_env: Vec<String>,
        #[arg(long = "env-file", value_name = "PATH", action = ArgAction::Append)]
        runtime_env_file: Vec<PathBuf>,
        #[arg(long = "secret-file", value_name = "ID=PATH", action = ArgAction::Append)]
        secret_file: Vec<String>,
        #[arg(long)]
        smoke_test: bool,
        #[arg(long)]
        run_dangerously: bool,
        #[arg(long)]
        json: bool,
    },
    Run {
        package: PathBuf,
        #[arg(long, value_enum)]
        executor: Option<ExecutorArg>,
        #[arg(long, value_enum)]
        materialize: Option<MaterializeArg>,
        #[arg(long, hide = true)]
        run_root: Option<PathBuf>,
        #[arg(long = "env", value_name = "KEY=VALUE", action = ArgAction::Append)]
        runtime_env: Vec<String>,
        #[arg(long = "env-file", value_name = "PATH", action = ArgAction::Append)]
        runtime_env_file: Vec<PathBuf>,
        #[arg(long = "secret-file", value_name = "ID=PATH", action = ArgAction::Append)]
        secret_file: Vec<String>,
        #[arg(long)]
        smoke_test: bool,
        #[arg(long)]
        run_dangerously: bool,
        #[arg(long)]
        json: bool,
    },
    Replay {
        #[arg(long)]
        run_dir: PathBuf,
        #[arg(long)]
        trial_id: String,
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        json: bool,
    },
    Fork {
        #[arg(long)]
        run_dir: PathBuf,
        #[arg(long)]
        from_trial: String,
        #[arg(long)]
        at: String,
        #[arg(long = "set")]
        set_values: Vec<String>,
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        json: bool,
    },
    Pause {
        #[arg(long)]
        run_dir: PathBuf,
        #[arg(long)]
        trial_id: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, default_value_t = 60)]
        timeout_seconds: u64,
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "Resume one paused trial from its checkpoint; may unpause or fork that trial"
    )]
    Resume {
        #[arg(long)]
        run_dir: PathBuf,
        #[arg(long)]
        trial_id: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long = "set")]
        set_values: Vec<String>,
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Continue a run-level schedule after interruption or recovery")]
    Continue {
        #[arg(long)]
        run_dir: PathBuf,
        #[arg(long = "env", value_name = "KEY=VALUE", action = ArgAction::Append)]
        runtime_env: Vec<String>,
        #[arg(long = "env-file", value_name = "PATH", action = ArgAction::Append)]
        runtime_env_file: Vec<PathBuf>,
        #[arg(long = "secret-file", value_name = "ID=PATH", action = ArgAction::Append)]
        secret_file: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Recover a durable run after stale owner crash/interruption")]
    Recover {
        #[arg(long)]
        run_dir: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Kill a running or paused experiment immediately")]
    Kill {
        run: String,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Show standardized views for a run; omit run in a TTY to browse and pick")]
    Views {
        run: Option<String>,
        view: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long = "max-rows")]
        max_rows: Option<usize>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        csv: bool,
        #[arg(long, alias = "markdown")]
        md: bool,
        #[arg(long)]
        html: bool,
    },
    #[command(
        about = "Live refresh for a view; omit run/view in a TTY to browse active runs and views"
    )]
    ViewsLive {
        run: Option<String>,
        view: Option<String>,
        #[arg(long, default_value_t = 2)]
        interval_seconds: u64,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        no_clear: bool,
    },
    Query {
        run: String,
        sql: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        csv: bool,
    },
    Runs {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        csv: bool,
    },
    SchemaValidate {
        #[arg(long)]
        schema: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Publish {
        #[arg(long)]
        run_dir: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    Preflight {
        package: PathBuf,
        #[arg(long = "env", value_name = "KEY=VALUE", action = ArgAction::Append)]
        runtime_env: Vec<String>,
        #[arg(long = "env-file", value_name = "PATH", action = ArgAction::Append)]
        runtime_env_file: Vec<PathBuf>,
        #[arg(long = "secret-file", value_name = "ID=PATH", action = ArgAction::Append)]
        secret_file: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    Clean {
        #[arg(long)]
        runs: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableRenderFormat {
    Text,
    Csv,
    Markdown,
    Html,
}

#[derive(Clone, Debug, Default)]
struct RunControlSummary {
    status: String,
    status_display: String,
    live_summary: String,
    active_trials: usize,
    is_active: bool,
}

#[derive(Clone, Debug)]
struct RunInventoryEntry {
    run_id: String,
    run_dir: PathBuf,
    experiment: String,
    started_at: String,
    started_at_display: String,
    control: RunControlSummary,
}

#[derive(Clone, Debug)]
struct RunMetrics {
    variants: usize,
    pass_rate: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewsBrowserScreen {
    RunPicker,
    ViewPicker,
    Viewer,
    Detail,
}

#[derive(Clone, Debug)]
struct DetailSnapshot {
    view_name: String,
    run_id_label: String,
    row_label: String,
    fields: Vec<(String, String)>,
    payload: Option<String>,
}

fn cargo_manifest_dir_for_stale_binary_guard() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cargo_workspace_root_for_stale_binary_guard(manifest_dir: &Path) -> Option<PathBuf> {
    manifest_dir.ancestors().find_map(|candidate| {
        if candidate.join("Cargo.toml").exists() && candidate.join("Cargo.lock").exists() {
            Some(candidate.to_path_buf())
        } else {
            None
        }
    })
}

fn sibling_crate_dir(manifest_dir: &Path, crate_name: &str) -> Option<PathBuf> {
    let candidate = manifest_dir.parent()?.join(crate_name);
    if candidate.join("Cargo.toml").exists() {
        Some(candidate)
    } else {
        None
    }
}

fn stale_binary_watch_paths() -> Vec<PathBuf> {
    let manifest_dir = cargo_manifest_dir_for_stale_binary_guard();
    let mut paths = Vec::new();
    if let Some(workspace_root) = cargo_workspace_root_for_stale_binary_guard(&manifest_dir) {
        paths.push(workspace_root.join("Cargo.toml"));
        paths.push(workspace_root.join("Cargo.lock"));
        for crate_name in [
            "lab-analysis",
            "lab-cli",
            "lab-core",
            "lab-provenance",
            "lab-runner",
            "lab-schemas",
        ] {
            let crate_dir = workspace_root.join("rust").join("crates").join(crate_name);
            if crate_dir.join("Cargo.toml").exists() {
                paths.push(crate_dir.join("Cargo.toml"));
                paths.push(crate_dir.join("src"));
            }
            let views_dir = crate_dir.join("views");
            if views_dir.exists() {
                paths.push(views_dir);
            }
        }
    }
    paths.push(manifest_dir.join("Cargo.toml"));
    paths.push(manifest_dir.join("src"));
    if let Some(lab_runner_dir) = sibling_crate_dir(&manifest_dir, "lab-runner") {
        paths.push(lab_runner_dir.join("Cargo.toml"));
        paths.push(lab_runner_dir.join("src"));
    }
    paths
}

fn latest_mtime_in_path(path: &Path) -> Result<Option<(SystemTime, PathBuf)>> {
    if !path.exists() {
        return Ok(None);
    }
    if path.file_name().and_then(|name| name.to_str()) == Some("tests.rs") {
        return Ok(None);
    }
    let meta = std::fs::metadata(path).map_err(|err| {
        anyhow!(
            "failed to stat stale-binary watch path {}: {}",
            path.display(),
            err
        )
    })?;
    if meta.is_file() {
        let modified = meta
            .modified()
            .map_err(|err| anyhow!("failed to read mtime for {}: {}", path.display(), err))?;
        return Ok(Some((modified, path.to_path_buf())));
    }
    if !meta.is_dir() {
        return Ok(None);
    }

    let mut newest: Option<(SystemTime, PathBuf)> = None;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|err| {
            anyhow!(
                "failed to read stale-binary watch dir {}: {}",
                dir.display(),
                err
            )
        })?;
        for entry in entries {
            let entry = entry
                .map_err(|err| anyhow!("failed to read dir entry in {}: {}", dir.display(), err))?;
            let entry_path = entry.path();
            let entry_meta = entry
                .metadata()
                .map_err(|err| anyhow!("failed to stat {}: {}", entry_path.display(), err))?;
            if entry_meta.is_dir() {
                stack.push(entry_path);
                continue;
            }
            if !entry_meta.is_file() {
                continue;
            }
            if entry_path.file_name().and_then(|name| name.to_str()) == Some("tests.rs") {
                continue;
            }
            let modified = entry_meta.modified().map_err(|err| {
                anyhow!("failed to read mtime for {}: {}", entry_path.display(), err)
            })?;
            let replace = newest
                .as_ref()
                .map(|(current, _)| modified > *current)
                .unwrap_or(true);
            if replace {
                newest = Some((modified, entry_path));
            }
        }
    }
    Ok(newest)
}

fn newest_watch_mtime() -> Result<Option<(SystemTime, PathBuf)>> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for candidate in stale_binary_watch_paths() {
        let Some((modified, path)) = latest_mtime_in_path(&candidate)? else {
            continue;
        };
        let replace = newest
            .as_ref()
            .map(|(current, _)| modified > *current)
            .unwrap_or(true);
        if replace {
            newest = Some((modified, path));
        }
    }
    Ok(newest)
}

fn shell_quote_path(path: &Path) -> String {
    let raw = path.display().to_string();
    if raw
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '-' | '_'))
    {
        raw
    } else {
        format!("'{}'", raw.replace('\'', "'\\''"))
    }
}

fn stale_binary_rebuild_command(exe_path: &Path) -> String {
    let manifest_dir = cargo_manifest_dir_for_stale_binary_guard();
    let manifest_path =
        cargo_workspace_root_for_stale_binary_guard(&manifest_dir).unwrap_or(manifest_dir);
    let bin_name = exe_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| matches!(*stem, "bucephalus" | "lab"))
        .unwrap_or("bucephalus");
    let mut command = format!(
        "cargo build --manifest-path {} --bin {}",
        shell_quote_path(&manifest_path.join("Cargo.toml")),
        bin_name
    );
    if !exe_path
        .components()
        .any(|component| component.as_os_str() == "debug")
    {
        command.push_str(" --release");
    }
    command
}

fn stale_binary_guard_error(
    exe_path: &Path,
    exe_mtime: SystemTime,
    source_path: &Path,
    source_mtime: SystemTime,
) -> anyhow::Error {
    let exe_secs = exe_mtime
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let source_secs = source_mtime
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let rebuild_cmd = stale_binary_rebuild_command(exe_path);
    anyhow!(
        "stale Bucephalus binary detected: executable '{}' (mtime={}s) is older than source '{}' (mtime={}s). Rebuild with `{}` and rerun.",
        exe_path.display(),
        exe_secs,
        source_path.display(),
        source_secs,
        rebuild_cmd
    )
}

fn enforce_cli_binary_freshness(
    exe_path: &Path,
    exe_mtime: SystemTime,
    newest_source: Option<(SystemTime, PathBuf)>,
) -> Result<()> {
    let Some((source_mtime, source_path)) = newest_source else {
        return Ok(());
    };
    if source_mtime > exe_mtime {
        return Err(stale_binary_guard_error(
            exe_path,
            exe_mtime,
            &source_path,
            source_mtime,
        ));
    }
    Ok(())
}

fn ensure_cli_binary_is_fresh() -> Result<()> {
    let exe_path = std::env::current_exe()
        .map_err(|err| anyhow!("failed to resolve current executable path: {}", err))?;
    let exe_mtime = std::fs::metadata(&exe_path)
        .and_then(|meta| meta.modified())
        .map_err(|err| {
            anyhow!(
                "failed to read executable mtime for {}: {}",
                exe_path.display(),
                err
            )
        })?;
    enforce_cli_binary_freshness(&exe_path, exe_mtime, newest_watch_mtime()?)
}

fn main() -> Result<()> {
    std::env::set_var(
        lab_runner::CLI_INVOKED_AT_MS_ENV,
        current_unix_time_ms().to_string(),
    );
    ctrlc::set_handler(move || {
        if lab_runner::INTERRUPTED.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
        eprintln!("\nInterrupted. Persisting run state... (press Ctrl+C again to force quit)");
    })
    .ok();

    let cli = Cli::parse();
    let json_mode = command_json_mode(&cli.command);
    let result = ensure_cli_binary_is_fresh().and_then(|_| run_command(cli.command));
    match result {
        Ok(Some(payload)) => {
            emit_json(&payload);
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(err) => {
            if json_mode {
                let code = if err.to_string().contains("stale Bucephalus binary detected") {
                    "stale_binary"
                } else {
                    "command_failed"
                };
                emit_json(&json_error(code, err.to_string(), json!({})));
                std::process::exit(1);
            }
            Err(err)
        }
    }
}

fn current_unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunValidationAction {
    FullRun,
    SmokeTest,
    Cancel,
}

fn prompt_for_run_validation_action(
    package: &Path,
    validation: &lab_runner::ExperimentBundleValidation,
) -> Result<Option<RunValidationAction>> {
    let prompt = {
        let mut lines = Vec::new();
        lines.push(String::new());
        lines.push("This experiment bundle has not been smoke tested.".to_string());
        lines.push(format!("package_digest: {}", validation.package_digest));
        if let Some(experiment_id) = &validation.experiment_id {
            lines.push(format!("experiment_id: {}", experiment_id));
        }
        lines.push(format!("package_dir: {}", package.display()));
        lines.push(String::new());
        lines.push("1. Run smoke test now".to_string());
        lines.push("2. Run full experiment anyway".to_string());
        lines.push("3. Cancel".to_string());
        lines.push("Choose [1/2/3]: ".to_string());
        lines.join("\n")
    };

    let choice = if std::io::stdin().is_terminal() {
        print!("{}", prompt);
        std::io::stdout().flush()?;
        let mut choice = String::new();
        std::io::stdin().read_line(&mut choice)?;
        choice
    } else {
        let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") else {
            return Ok(None);
        };
        let mut writer = tty.try_clone()?;
        writer.write_all(prompt.as_bytes())?;
        writer.flush()?;
        let mut reader = BufReader::new(tty);
        let mut choice = String::new();
        reader.read_line(&mut choice)?;
        choice
    };

    match choice.trim() {
        "1" => Ok(Some(RunValidationAction::SmokeTest)),
        "2" => Ok(Some(RunValidationAction::FullRun)),
        "3" | "" => Ok(Some(RunValidationAction::Cancel)),
        other => Err(anyhow!("invalid validation choice '{}'", other)),
    }
}

fn build_run_temp_out_path(out: &Path) -> PathBuf {
    let parent = out.parent().unwrap_or_else(|| Path::new("."));
    let name = out
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("build");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    parent.join(format!(
        ".{}.build-run-tmp.{}.{}",
        name,
        std::process::id(),
        nanos
    ))
}

fn build_run_replaced_out_path(out: &Path) -> PathBuf {
    let parent = out.parent().unwrap_or_else(|| Path::new("."));
    let name = out
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("build");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    parent.join(format!(
        ".{}.build-run-replaced.{}.{}",
        name,
        std::process::id(),
        nanos
    ))
}

fn looks_like_bucephalus_package_dir(path: &Path) -> bool {
    path.join("manifest.json").is_file()
        || path.join("checksums.json").is_file()
        || path.join("package.lock").is_file()
}

fn publish_build_run_package(
    build: lab_runner::BuildResult,
    final_out: &Path,
) -> Result<lab_runner::BuildResult> {
    if let Some(parent) = final_out.parent() {
        fs::create_dir_all(parent)?;
    }
    if final_out.exists() {
        if !final_out.is_dir() {
            return Err(anyhow!(
                "build-run output path exists and is not a directory: {}",
                final_out.display()
            ));
        }
        if !looks_like_bucephalus_package_dir(final_out) {
            let mut entries = fs::read_dir(final_out)?;
            if entries.next().is_some() {
                return Err(anyhow!(
                    "build-run output directory is non-empty and does not look like a Bucephalus package: {}",
                    final_out.display()
                ));
            }
        }
        let replaced = build_run_replaced_out_path(final_out);
        fs::rename(final_out, &replaced)?;
        match fs::rename(&build.package_dir, final_out) {
            Ok(()) => {
                fs::remove_dir_all(replaced)?;
            }
            Err(err) => {
                let _ = fs::rename(&replaced, final_out);
                return Err(err.into());
            }
        }
    } else {
        fs::rename(&build.package_dir, final_out)?;
    }

    Ok(lab_runner::BuildResult {
        package_dir: final_out.to_path_buf(),
        manifest_path: final_out.join("manifest.json"),
        checksums_path: final_out.join("checksums.json"),
        package_checks_path: final_out.join("package_checks.json"),
    })
}

fn build_experiment_package_for_build_run(
    experiment: &Path,
    overrides: Option<&Path>,
    out: Option<&PathBuf>,
) -> Result<lab_runner::BuildResult> {
    let Some(final_out) = out else {
        return lab_runner::build_experiment_package(experiment, overrides, None);
    };
    let temp_out = build_run_temp_out_path(final_out);
    let build = match lab_runner::build_experiment_package(experiment, overrides, Some(&temp_out)) {
        Ok(build) => build,
        Err(err) => {
            let _ = fs::remove_dir_all(&temp_out);
            return Err(err);
        }
    };
    publish_build_run_package(build, final_out)
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

fn resolve_run_validation_action(
    package: &Path,
    validation: &lab_runner::ExperimentBundleValidation,
    smoke_test: bool,
    run_dangerously: bool,
    json: bool,
) -> Result<RunValidationAction> {
    if smoke_test && run_dangerously {
        return Err(anyhow!(
            "--smoke-test and --run-dangerously are mutually exclusive"
        ));
    }
    if smoke_test {
        return Ok(RunValidationAction::SmokeTest);
    }
    if run_dangerously || validation.smoke_tested {
        return Ok(RunValidationAction::FullRun);
    }
    if json {
        return Err(anyhow!(
            "experiment bundle {} is not smoke tested; run `bucephalus run {} --smoke-test`, or pass --run-dangerously to skip validation",
            validation.package_digest,
            package.display()
        ));
    }

    match prompt_for_run_validation_action(package, validation)? {
        Some(action) => Ok(action),
        None => Err(anyhow!(
            "experiment bundle {} is not smoke tested and no interactive terminal is available; rerun with --smoke-test, or pass --run-dangerously to skip validation",
            validation.package_digest
        )),
    }
}

fn run_command(command: Commands) -> Result<Option<Value>> {
    match command {
        Commands::Build {
            experiment,
            out,
            overrides,
            json,
        } => {
            if !json {
                eprintln!("building package from: {}", experiment.display());
            }
            let build = lab_runner::build_experiment_package(
                &experiment,
                overrides.as_deref(),
                out.as_deref(),
            )?;
            let validation = lab_runner::register_experiment_bundle(&build.package_dir)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "build",
                    "package_dir": build.package_dir.display().to_string(),
                    "manifest_path": build.manifest_path.display().to_string(),
                    "checksums_path": build.checksums_path.display().to_string(),
                    "package_checks_path": build.package_checks_path.display().to_string(),
                    "validation": experiment_bundle_validation_to_json(&validation),
                })));
            }
            println!("package_dir: {}", build.package_dir.display());
            println!("manifest: {}", build.manifest_path.display());
            println!("checksums: {}", build.checksums_path.display());
            println!("package_checks: {}", build.package_checks_path.display());
            println!("package_digest: {}", validation.package_digest);
            println!("smoke_tested: {}", validation.smoke_tested);
        }
        Commands::CheckPackage { package, json } => {
            let report = lab_runner::check_package(&package)?;
            if json {
                return Ok(Some(json!({
                    "ok": report.get("passed").and_then(Value::as_bool).unwrap_or(false),
                    "command": "check-package",
                    "package_dir": package.display().to_string(),
                    "report": report,
                })));
            }
            print_package_check_report(&report);
            if !report
                .get("passed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err(anyhow!("package checks failed"));
            }
        }
        Commands::PrepareRuntimeImages {
            package,
            repository,
            out,
            push,
            dry_run,
            skip_existing,
            json,
        } => {
            if !json {
                eprintln!(
                    "preparing runtime images from package: {}",
                    package.display()
                );
            }
            let report = lab_runner::prepare_runtime_images(
                &package,
                lab_runner::PreparedRuntimeImageOptions {
                    repository,
                    out,
                    push,
                    dry_run,
                    skip_existing,
                },
            )?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "prepare-runtime-images",
                    "map_path": report.map_path.display().to_string(),
                    "built": report.built,
                    "skipped": report.skipped,
                    "dry_run": report.dry_run,
                    "entries": report.entries,
                })));
            }
            println!("map: {}", report.map_path.display());
            println!("entries: {}", report.entries.len());
            println!("built: {}", report.built);
            println!("skipped: {}", report.skipped);
            println!("dry_run: {}", report.dry_run);
            println!(
                "runner_auto_discovers: {}",
                report.map_path.starts_with(&package)
            );
        }
        Commands::BuildRun {
            experiment,
            out,
            overrides,
            executor,
            materialize,
            run_root,
            runtime_env,
            runtime_env_file,
            secret_file,
            smoke_test,
            run_dangerously,
            json,
        } => {
            if !json {
                eprintln!("building package from: {}", experiment.display());
            }
            let build = build_experiment_package_for_build_run(
                &experiment,
                overrides.as_deref(),
                out.as_ref(),
            )?;
            let validation = lab_runner::register_experiment_bundle(&build.package_dir)?;
            let execution = build_run_execution_options(
                executor,
                materialize,
                run_root,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            let run_mode = resolve_run_validation_action(
                &build.package_dir,
                &validation,
                smoke_test,
                run_dangerously,
                json,
            )?;
            if matches!(run_mode, RunValidationAction::Cancel) {
                if json {
                    return Ok(Some(json!({
                        "ok": false,
                        "command": "build-run",
                        "cancelled": true,
                        "validation": experiment_bundle_validation_to_json(&validation),
                    })));
                }
                println!("cancelled");
                return Ok(None);
            }
            let summary =
                lab_runner::experiment_summary_with_options(&build.package_dir, &execution)?;
            if matches!(run_mode, RunValidationAction::SmokeTest) {
                if !json {
                    eprintln!("launching smoke test...");
                }
                let result =
                    lab_runner::run_smoke_test_with_options(&build.package_dir, execution.clone())?;
                let validation = lab_runner::mark_experiment_bundle_smoke_tested(
                    &build.package_dir,
                    &result.run_id,
                )?;
                if json {
                    return Ok(Some(json!({
                        "ok": true,
                        "command": "build-run",
                        "mode": "smoke_test",
                        "package_dir": build.package_dir.display().to_string(),
                        "manifest_path": build.manifest_path.display().to_string(),
                        "checksums_path": build.checksums_path.display().to_string(),
                        "package_checks_path": build.package_checks_path.display().to_string(),
                        "summary": summary_to_json(&summary),
                        "run": run_result_to_json(&result),
                        "executor": execution.executor.map(|e| e.as_str()),
                        "materialize": execution.materialize.map(|m| m.as_str()),
                        "validation": experiment_bundle_validation_to_json(&validation),
                    })));
                }
                println!("package_dir: {}", build.package_dir.display());
                println!("manifest: {}", build.manifest_path.display());
                println!("checksums: {}", build.checksums_path.display());
                println!("package_checks: {}", build.package_checks_path.display());
                println!("smoke_run_id: {}", result.run_id);
                println!("smoke_run_dir: {}", result.run_dir.display());
                println!("smoke_tested: true");
                return Ok(None);
            }
            if !json {
                print_summary(&summary);
                eprintln!("launching run...");
            }
            let result =
                lab_runner::run_experiment_with_options(&build.package_dir, execution.clone())?;
            if json {
                let post_run = try_post_run_stats_json(&result.run_dir);
                return Ok(Some(json!({
                    "ok": true,
                    "command": "build-run",
                    "package_dir": build.package_dir.display().to_string(),
                    "manifest_path": build.manifest_path.display().to_string(),
                    "checksums_path": build.checksums_path.display().to_string(),
                    "package_checks_path": build.package_checks_path.display().to_string(),
                    "summary": summary_to_json(&summary),
                    "run": run_result_to_json(&result),
                    "artifacts": run_artifacts_to_json(&result),
                    "executor": execution.executor.map(|e| e.as_str()),
                    "materialize": execution.materialize.map(|m| m.as_str()),
                    "validation": experiment_bundle_validation_to_json(&validation),
                    "post_run_stats": post_run
                })));
            }
            println!("package_dir: {}", build.package_dir.display());
            println!("manifest: {}", build.manifest_path.display());
            println!("checksums: {}", build.checksums_path.display());
            println!("package_checks: {}", build.package_checks_path.display());
            println!("run_id: {}", result.run_id);
            println!("run_dir: {}", result.run_dir.display());
            try_print_post_run_stats(&result.run_dir, &result.run_id);
        }
        Commands::Run {
            package,
            executor,
            materialize,
            run_root,
            runtime_env,
            runtime_env_file,
            secret_file,
            smoke_test,
            run_dangerously,
            json,
        } => {
            if !json {
                eprintln!("loading package: {}", package.display());
            }
            let validation = lab_runner::register_experiment_bundle(&package)?;
            let execution = build_run_execution_options(
                executor,
                materialize,
                run_root,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            let run_mode = resolve_run_validation_action(
                &package,
                &validation,
                smoke_test,
                run_dangerously,
                json,
            )?;
            if matches!(run_mode, RunValidationAction::Cancel) {
                if json {
                    return Ok(Some(json!({
                        "ok": false,
                        "command": "run",
                        "cancelled": true,
                        "validation": experiment_bundle_validation_to_json(&validation),
                    })));
                }
                println!("cancelled");
                return Ok(None);
            }
            let summary = lab_runner::experiment_summary_with_options(&package, &execution)?;
            if matches!(run_mode, RunValidationAction::SmokeTest) {
                if !json {
                    eprintln!("launching smoke test...");
                }
                let result = lab_runner::run_smoke_test_with_options(&package, execution.clone())?;
                let validation =
                    lab_runner::mark_experiment_bundle_smoke_tested(&package, &result.run_id)?;
                if json {
                    return Ok(Some(json!({
                        "ok": true,
                        "command": "run",
                        "mode": "smoke_test",
                        "summary": summary_to_json(&summary),
                        "run": run_result_to_json(&result),
                        "executor": execution.executor.map(|e| e.as_str()),
                        "materialize": execution.materialize.map(|m| m.as_str()),
                        "validation": experiment_bundle_validation_to_json(&validation),
                    })));
                }
                println!("smoke_run_id: {}", result.run_id);
                println!("smoke_run_dir: {}", result.run_dir.display());
                println!("smoke_tested: true");
                return Ok(None);
            }
            if !json {
                print_summary(&summary);
                eprintln!("launching run...");
            }
            let result = lab_runner::run_experiment_with_options(&package, execution.clone())?;
            if json {
                let post_run = try_post_run_stats_json(&result.run_dir);
                return Ok(Some(json!({
                    "ok": true,
                    "command": "run",
                    "summary": summary_to_json(&summary),
                    "run": run_result_to_json(&result),
                    "artifacts": run_artifacts_to_json(&result),
                    "executor": execution.executor.map(|e| e.as_str()),
                    "materialize": execution.materialize.map(|m| m.as_str()),
                    "validation": experiment_bundle_validation_to_json(&validation),
                    "post_run_stats": post_run
                })));
            }
            println!("run_id: {}", result.run_id);
            println!("run_dir: {}", result.run_dir.display());
            try_print_post_run_stats(&result.run_dir, &result.run_id);
        }
        Commands::Replay {
            run_dir,
            trial_id,
            strict,
            json,
        } => {
            let result = lab_runner::replay_trial(&run_dir, &trial_id, strict)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "replay",
                    "replay": replay_result_to_json(&result),
                })));
            }
            println!("replay_id: {}", result.replay_id);
            println!("replay_dir: {}", result.replay_dir.display());
            println!("parent_trial_id: {}", result.parent_trial_id);
            println!("strict: {}", result.strict);
            println!("replay_grade: {}", result.replay_grade);
            println!("harness_status: {}", result.harness_status);
        }
        Commands::Fork {
            run_dir,
            from_trial,
            at,
            set_values,
            strict,
            json,
        } => {
            let set_bindings = parse_set_bindings(&set_values)?;
            let result = lab_runner::fork_trial(&run_dir, &from_trial, &at, &set_bindings, strict)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "fork",
                    "fork": fork_result_to_json(&result),
                })));
            }
            println!("fork_id: {}", result.fork_id);
            println!("fork_dir: {}", result.fork_dir.display());
            println!("parent_trial_id: {}", result.parent_trial_id);
            println!("selector: {}", result.selector);
            println!("strict: {}", result.strict);
            println!(
                "source_checkpoint: {}",
                result.source_checkpoint.as_deref().unwrap_or("none")
            );
            println!("fallback_mode: {}", result.fallback_mode);
            println!("replay_grade: {}", result.replay_grade);
            println!("harness_status: {}", result.harness_status);
        }
        Commands::Pause {
            run_dir,
            trial_id,
            label,
            timeout_seconds,
            json,
        } => {
            let result = lab_runner::pause_run(
                &run_dir,
                trial_id.as_deref(),
                label.as_deref(),
                timeout_seconds,
            )?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "pause",
                    "pause": pause_result_to_json(&result),
                })));
            }
            println!("run_id: {}", result.run_id);
            println!("trial_id: {}", result.trial_id);
            println!("label: {}", result.label);
            println!("checkpoint_acked: {}", result.checkpoint_acked);
            println!("stop_acked: {}", result.stop_acked);
        }
        Commands::Resume {
            run_dir,
            trial_id,
            label,
            set_values,
            strict,
            json,
        } => {
            let set_bindings = parse_set_bindings(&set_values)?;
            let result = lab_runner::resume_trial(
                &run_dir,
                trial_id.as_deref(),
                label.as_deref(),
                &set_bindings,
                strict,
            )?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "resume",
                    "resume": resume_result_to_json(&result),
                })));
            }
            println!("trial_id: {}", result.trial_id);
            let mode = match result.mode {
                lab_runner::ResumeMode::RuntimeUnpause => "runtime_unpause",
                lab_runner::ResumeMode::Fork => "fork",
            };
            println!("mode: {}", mode);
            if let Some(selector) = result.selector.as_deref() {
                println!("selector: {}", selector);
            }
            if let Some(fork) = result.fork.as_ref() {
                println!("fork_id: {}", fork.fork_id);
                println!("fork_dir: {}", fork.fork_dir.display());
                println!("replay_grade: {}", fork.replay_grade);
                println!("harness_status: {}", fork.harness_status);
            }
        }
        Commands::Continue {
            run_dir,
            runtime_env,
            runtime_env_file,
            secret_file,
            json,
        } => {
            let execution = build_run_execution_options(
                None,
                None,
                None,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            let result = lab_runner::continue_run_with_options(&run_dir, execution)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "continue",
                    "run": run_result_to_json(&result),
                })));
            }
            println!("run_id: {}", result.run_id);
            println!("run_dir: {}", result.run_dir.display());
        }
        Commands::Recover {
            run_dir,
            force,
            json,
        } => {
            let result = lab_runner::recover_run(&run_dir, force)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "recover",
                    "recover": recover_result_to_json(&result),
                })));
            }
            println!("run_id: {}", result.run_id);
            println!("previous_status: {}", result.previous_status);
            println!("recovered_status: {}", result.recovered_status);
            println!(
                "rewound_to_schedule_idx: {}",
                result.rewound_to_schedule_idx
            );
            println!("active_trials_released: {}", result.active_trials_released);
            println!(
                "label_drift_containers_removed: {}",
                result.label_drift_containers_removed
            );
            println!(
                "committed_slots_verified: {}",
                result.committed_slots_verified
            );
            if result.notes.is_empty() {
                println!("notes: (none)");
            } else {
                println!("notes: {}", result.notes.join(" | "));
            }
        }
        Commands::Kill { run, json } => {
            let run_dir = resolve_run_dir_arg(&run)?;
            let result = lab_runner::kill_run(&run_dir)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "kill",
                    "run_id": result.run_id,
                    "run_dir": result.run_dir.display().to_string(),
                    "previous_status": result.previous_status,
                    "killed_trials": result.killed_trials,
                })));
            }
            println!("killed: {}", result.run_id);
            println!("run_dir: {}", result.run_dir.display());
            println!("previous_status: {}", result.previous_status);
            if result.killed_trials.is_empty() {
                println!("killed_trials: (none active)");
            } else {
                println!("killed_trials: {}", result.killed_trials.join(", "));
            }
        }
        Commands::Views {
            run,
            view,
            all,
            max_rows,
            json,
            csv,
            md,
            html,
        } => {
            let format_flags = [json, csv, md, html]
                .into_iter()
                .filter(|flag| *flag)
                .count();
            if format_flags > 1 {
                return Err(anyhow::anyhow!(
                    "--json, --csv, --md, and --html are mutually exclusive"
                ));
            }
            if all && view.is_some() {
                return Err(anyhow::anyhow!(
                    "--all cannot be combined with a specific view name"
                ));
            }

            let (run_dir, view) = if let Some(run_str) = run {
                (resolve_run_dir_arg(&run_str)?, view)
            } else {
                if all {
                    return Err(anyhow::anyhow!("--all requires a run id argument"));
                }
                if view.is_some() {
                    return Err(anyhow::anyhow!("view name requires a run id argument"));
                }
                if !stdout_is_tty() {
                    return Err(anyhow::anyhow!(
                        "run is required when not connected to a TTY; pass a run id or path"
                    ));
                }
                let project_root = resolve_project_root(std::env::current_dir()?.as_path());
                run_views_browser(&project_root)?;
                return Ok(None);
            };

            let run_view_set = analysis::run_view_set(&run_dir)?;
            let view_set = run_view_set.as_str().to_string();
            let raw_view_names = list_available_analysis_views(&run_dir);
            let standard_views = standard_views_for_set(run_view_set);
            let row_limit = max_rows.unwrap_or(0);
            let render_format = table_render_format(csv, md, html);

            if all {
                if json {
                    let mut payload = serde_json::Map::new();
                    for def in standard_views {
                        let resolved = resolved_view_from_spec(run_view_set, def);
                        let table = query_resolved_view(&run_dir, &resolved, row_limit)?;
                        payload.insert(def.name.to_string(), query_table_to_json(&table));
                    }
                    return Ok(Some(json!({
                        "ok": true,
                        "command": "views",
                        "run_dir": run_dir.display().to_string(),
                        "view_set": view_set,
                        "view_count": standard_views.len(),
                        "raw_view_count": raw_view_names.len(),
                        "views": Value::Object(payload),
                    })));
                }
                let mut rendered: Vec<(ResolvedView, analysis::QueryTable)> =
                    Vec::with_capacity(standard_views.len());
                for def in standard_views {
                    let resolved = resolved_view_from_spec(run_view_set, def);
                    let table = query_resolved_view(&run_dir, &resolved, row_limit)?;
                    rendered.push((resolved, table));
                }
                if matches!(render_format, TableRenderFormat::Csv) {
                    for (_, table) in rendered {
                        print_query_table_csv(&table);
                    }
                    return Ok(None);
                }
                if matches!(render_format, TableRenderFormat::Markdown) {
                    print_views_markdown_document(&run_dir, &view_set, &rendered);
                    return Ok(None);
                }
                if matches!(render_format, TableRenderFormat::Html) {
                    print_views_html_document(&run_dir, &view_set, &rendered);
                    return Ok(None);
                }
                println!("run_dir: {}", run_dir.display());
                println!("view_set: {}", view_set);
                for (resolved, table) in rendered {
                    println!("\n== {} ==", resolved.name);
                    if !print_special_split_view(&run_dir, &resolved.name, &table) {
                        let display = present_display_table(&resolved, &table);
                        print_query_table(&display);
                    }
                }
                return Ok(None);
            }

            if let Some(view_name) = view {
                let resolved = resolve_requested_view(run_view_set, &raw_view_names, &view_name)?;
                let table = query_resolved_view(&run_dir, &resolved, row_limit)?;
                if json {
                    return Ok(Some(json!({
                        "ok": true,
                        "command": "views",
                        "run_dir": run_dir.display().to_string(),
                        "view_set": view_set,
                        "view": resolved.name,
                        "source_view": resolved.source,
                        "result": query_table_to_json(&table),
                    })));
                }
                if matches!(render_format, TableRenderFormat::Csv) {
                    print_query_table_csv(&table);
                    return Ok(None);
                }
                if matches!(render_format, TableRenderFormat::Markdown) {
                    print_single_view_markdown(&run_dir, &view_set, &resolved, &table);
                    return Ok(None);
                }
                if matches!(render_format, TableRenderFormat::Html) {
                    print_single_view_html(&run_dir, &view_set, &resolved, &table);
                    return Ok(None);
                }
                println!("run_dir: {}", run_dir.display());
                println!("view_set: {}", view_set);
                println!("view: {}", resolved.name);
                if let Some(source) = resolved.source.as_deref() {
                    if source != resolved.name {
                        println!("source_view: {}", source);
                    }
                }
                if !print_special_split_view(&run_dir, &resolved.name, &table) {
                    let display = present_display_table(&resolved, &table);
                    print_query_table(&display);
                }
                return Ok(None);
            }

            let listing_table = analysis::QueryTable {
                columns: vec![
                    "view_name".to_string(),
                    "source_view".to_string(),
                    "purpose".to_string(),
                ],
                rows: standard_views
                    .iter()
                    .map(|def| {
                        vec![
                            Value::String(def.name.to_string()),
                            Value::String(standard_view_source_label(def).to_string()),
                            Value::String(def.purpose.to_string()),
                        ]
                    })
                    .collect(),
            };
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "views",
                    "run_dir": run_dir.display().to_string(),
                    "view_set": view_set,
                    "available_views": standard_views.iter().map(|def| json!({
                        "name": def.name,
                        "source_view": standard_view_source_label(def),
                        "purpose": def.purpose,
                    })).collect::<Vec<_>>(),
                    "raw_view_count": raw_view_names.len(),
                })));
            }
            if matches!(render_format, TableRenderFormat::Csv) {
                print_query_table_csv(&listing_table);
                return Ok(None);
            }
            if matches!(render_format, TableRenderFormat::Markdown) {
                print_table_markdown(&listing_table);
                return Ok(None);
            }
            if matches!(render_format, TableRenderFormat::Html) {
                print_table_html_document("available_views", &listing_table);
                return Ok(None);
            }
            println!("run_dir: {}", run_dir.display());
            println!("view_set: {}", view_set);
            print_query_table(&listing_table);
            let hidden = raw_view_names.len().saturating_sub(standard_views.len());
            println!();
            println!(
                "standardized view surface: {} views ({} internal/raw views hidden by default)",
                standard_views.len(),
                hidden
            );
            println!(
                "tip: use `bucephalus query <run> \"SELECT * FROM <raw_view>\"` for raw internals"
            );
        }
        Commands::ViewsLive {
            run,
            view,
            interval_seconds,
            limit,
            once,
            no_clear,
        } => {
            let sleep_interval = Duration::from_secs(interval_seconds.max(1));
            let resolved_limit = limit.max(1);
            let use_tui = !once && !no_clear && stdout_is_tty();
            let run_dir = match run.as_deref() {
                Some(run_arg) => Some(resolve_run_dir_arg(run_arg)?),
                None => None,
            };

            if use_tui {
                let project_root = resolve_project_root(std::env::current_dir()?.as_path());
                run_interactive_views_browser(
                    &project_root,
                    run_dir,
                    view.as_deref(),
                    sleep_interval,
                    resolved_limit,
                )?;
            } else {
                let run_dir = run_dir.ok_or_else(|| {
                    anyhow::anyhow!(
                        "run is required when interactive TUI selection is unavailable; pass a run id/path or use a TTY without --once/--no-clear"
                    )
                })?;
                let run_view_set = analysis::run_view_set(&run_dir)?;
                let raw_view_names = list_available_analysis_views(&run_dir);
                let resolved_view = match view.as_deref() {
                    Some(requested) => {
                        resolve_requested_view(run_view_set, &raw_view_names, requested)?
                    }
                    None => resolve_requested_view(run_view_set, &raw_view_names, "run_progress")?,
                };
                loop {
                    let table = query_resolved_view(&run_dir, &resolved_view, resolved_limit)?;
                    if !no_clear {
                        print!("\x1B[2J\x1B[H");
                        let _ = std::io::stdout().flush();
                    }
                    println!("run_dir: {}", run_dir.display());
                    println!("status: {}", read_run_status(&run_dir));
                    println!("updated_unix_s: {}", unix_now_seconds());
                    println!("view: {}", resolved_view.name);
                    if let Some(source) = resolved_view.source.as_deref() {
                        if source != resolved_view.name {
                            println!("source_view: {}", source);
                        }
                    }
                    println!("limit: {}", resolved_limit);
                    println!(
                        "refresh_interval_seconds: {} (Ctrl-C to stop)",
                        sleep_interval.as_secs()
                    );
                    println!();
                    if !print_special_split_view(&run_dir, &resolved_view.name, &table) {
                        let display = present_display_table(&resolved_view, &table);
                        print_query_table(&display);
                    }

                    if once {
                        break;
                    }
                    std::thread::sleep(sleep_interval);
                }
            }
        }
        Commands::Query {
            run,
            sql,
            json,
            csv,
        } => {
            if json && csv {
                return Err(anyhow::anyhow!("--json and --csv are mutually exclusive"));
            }
            let run_dir = resolve_run_dir_arg(&run)?;
            let table = analysis::query_run(&run_dir, &sql)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "query",
                    "run_dir": run_dir.display().to_string(),
                    "sql": sql,
                    "result": query_table_to_json(&table),
                })));
            }
            if csv {
                print_query_table_csv(&table);
                return Ok(None);
            }
            print_query_table(&table);
        }
        Commands::Runs { json, csv } => {
            if json && csv {
                return Err(anyhow::anyhow!("--json and --csv are mutually exclusive"));
            }
            let project_root = resolve_project_root(std::env::current_dir()?.as_path());
            let table = build_runs_table(&project_root)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "runs",
                    "project_root": project_root.display().to_string(),
                    "result": query_table_to_json(&table),
                })));
            }
            if csv {
                print_query_table_csv(&table);
                return Ok(None);
            }
            print_query_table(&table);
        }
        Commands::SchemaValidate { schema, file, json } => {
            let compiled = schemas::compile_schema(&schema)?;
            let data = std::fs::read_to_string(file)?;
            let value: serde_json::Value = serde_json::from_str(&data)?;
            if let Err(errors) = compiled.validate(&value) {
                for e in errors {
                    eprintln!("schema error: {}", e);
                }
                std::process::exit(1);
            }
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "schema-validate",
                    "valid": true,
                    "schema": schema
                })));
            }
            println!("ok");
        }
        Commands::Publish { run_dir, out, json } => {
            let out_path = out.unwrap_or(run_dir.join("debug_bundles").join("bundle.zip"));
            std::fs::create_dir_all(out_path.parent().unwrap())?;
            provenance::build_debug_bundle(&run_dir, &out_path)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "publish",
                    "bundle": out_path.display().to_string(),
                    "run_dir": run_dir.display().to_string()
                })));
            }
            println!("bundle: {}", out_path.display());
        }
        Commands::Preflight {
            package,
            runtime_env,
            runtime_env_file,
            secret_file,
            json,
        } => {
            if !json {
                eprintln!("running preflight: {}", package.display());
            }
            let execution = build_run_execution_options(
                None,
                None,
                None,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            let report = lab_runner::preflight_experiment_with_options(&package, &execution)?;
            if json {
                return Ok(Some(json!({
                    "ok": report.passed,
                    "command": "preflight",
                    "checks": report.checks.iter().map(|c| json!({
                        "name": c.name,
                        "passed": c.passed,
                        "severity": match c.severity {
                            lab_runner::PreflightSeverity::Error => "error",
                            lab_runner::PreflightSeverity::Warning => "warning",
                        },
                        "message": c.message,
                    })).collect::<Vec<_>>()
                })));
            }
            print_preflight_report(&report);
            if !report.passed {
                std::process::exit(1);
            }
        }
        Commands::Clean { runs } => {
            if runs {
                let runs_dir = lab_runner::default_run_root()?;
                if runs_dir.exists() {
                    std::fs::remove_dir_all(&runs_dir)?;
                    println!("removed: {}", runs_dir.display());
                }
            }
        }
    }
    Ok(None)
}

fn emit_json(value: &Value) {
    match serde_json::to_string(value) {
        Ok(s) => println!("{}", s),
        Err(_) => println!(
            "{{\"ok\":false,\"error\":{{\"code\":\"serialization_error\",\"message\":\"failed to serialize JSON payload\",\"details\":{{}}}}}}"
        ),
    }
}

fn json_error(code: &str, message: String, details: Value) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": code,
            "message": message,
            "details": details
        }
    })
}

fn command_json_mode(command: &Commands) -> bool {
    match command {
        Commands::Build { json, .. }
        | Commands::CheckPackage { json, .. }
        | Commands::BuildRun { json, .. }
        | Commands::Run { json, .. }
        | Commands::Replay { json, .. }
        | Commands::Fork { json, .. }
        | Commands::Pause { json, .. }
        | Commands::Resume { json, .. }
        | Commands::Continue { json, .. }
        | Commands::Recover { json, .. }
        | Commands::Kill { json, .. }
        | Commands::Views { json, .. }
        | Commands::Query { json, .. }
        | Commands::Runs { json, .. }
        | Commands::SchemaValidate { json, .. }
        | Commands::Publish { json, .. }
        | Commands::Preflight { json, .. } => *json,
        _ => false,
    }
}

fn run_result_to_json(result: &lab_runner::RunResult) -> Value {
    json!({
        "run_id": result.run_id,
        "run_dir": result.run_dir.display().to_string(),
        "account_sqlite_path": result.account_db_path.display().to_string()
    })
}

fn run_artifacts_to_json(result: &lab_runner::RunResult) -> Value {
    let objects = result.run_dir.join("objects");
    let benchmark = result.run_dir.join("benchmark");
    let summary_path = benchmark.join("summary.json");
    json!({
        "account_sqlite_path": result.account_db_path.display().to_string(),
        "objects_dir": objects.display().to_string(),
        "benchmark_dir": benchmark.display().to_string(),
        "benchmark_summary_path": if summary_path.exists() {
            Some(summary_path.display().to_string())
        } else {
            None::<String>
        }
    })
}

fn run_id_from_dir(run_dir: &Path) -> Option<String> {
    run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
}

fn load_run_control(run_dir: &Path) -> Option<Value> {
    load_runtime_value(run_dir, lab_runner::run_control_record_key())
}

fn load_engine_lease(run_dir: &Path) -> Option<Value> {
    load_runtime_value(run_dir, lab_runner::engine_lease_record_key())
}

fn list_available_analysis_views(run_dir: &Path) -> Vec<String> {
    analysis::list_views(run_dir).unwrap_or_default()
}

fn load_runtime_value(run_dir: &Path, key: &str) -> Option<Value> {
    if let Ok(sqlite_path) = lab_runner::account_sqlite_path_for_run(run_dir) {
        if sqlite_path.exists() {
            let account_id = lab_runner::active_account_id();
            let run_id = run_id_from_dir(run_dir)?;
            if let Ok(conn) = Connection::open(sqlite_path) {
                let raw: Option<String> = conn
                    .query_row(
                        "SELECT value_json FROM runtime_kv
                         WHERE account_id=?1 AND run_id=?2 AND key=?3",
                        params![account_id, run_id, key],
                        |row| row.get(0),
                    )
                    .optional()
                    .ok()?;
                if let Some(value) =
                    raw.and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
                {
                    return Some(value);
                }
            }
        }
    }
    load_runtime_value_file(run_dir, key)
}

fn load_runtime_value_file(run_dir: &Path, key: &str) -> Option<Value> {
    let file_name = if key == lab_runner::run_control_record_key() {
        "run_control.json"
    } else if key == lab_runner::schedule_progress_record_key() {
        "schedule_progress.json"
    } else if key == lab_runner::engine_lease_record_key() {
        "engine_lease.json"
    } else {
        return None;
    };
    read_json_file(&run_dir.join("runtime").join(file_name))
}

fn replay_result_to_json(result: &lab_runner::ReplayResult) -> Value {
    json!({
        "replay_id": result.replay_id,
        "replay_dir": result.replay_dir.display().to_string(),
        "parent_trial_id": result.parent_trial_id,
        "strict": result.strict,
        "replay_grade": result.replay_grade,
        "harness_status": result.harness_status,
    })
}

fn fork_result_to_json(result: &lab_runner::ForkResult) -> Value {
    json!({
        "fork_id": result.fork_id,
        "fork_dir": result.fork_dir.display().to_string(),
        "parent_trial_id": result.parent_trial_id,
        "selector": result.selector,
        "strict": result.strict,
        "source_checkpoint": result.source_checkpoint,
        "fallback_mode": result.fallback_mode,
        "replay_grade": result.replay_grade,
        "harness_status": result.harness_status,
    })
}

fn pause_result_to_json(result: &lab_runner::PauseResult) -> Value {
    json!({
        "run_id": result.run_id,
        "trial_id": result.trial_id,
        "label": result.label,
        "checkpoint_acked": result.checkpoint_acked,
        "stop_acked": result.stop_acked,
    })
}

fn resume_result_to_json(result: &lab_runner::ResumeResult) -> Value {
    json!({
        "trial_id": result.trial_id,
        "mode": result.mode,
        "selector": result.selector,
        "fork": result.fork.as_ref().map(fork_result_to_json),
    })
}

fn recover_result_to_json(result: &lab_runner::RecoverResult) -> Value {
    json!({
        "run_id": result.run_id,
        "previous_status": result.previous_status,
        "recovered_status": result.recovered_status,
        "rewound_to_schedule_idx": result.rewound_to_schedule_idx,
        "active_trials_released": result.active_trials_released,
        "label_drift_containers_removed": result.label_drift_containers_removed,
        "committed_slots_verified": result.committed_slots_verified,
        "notes": result.notes,
    })
}

fn parse_set_bindings(values: &[String]) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for raw in values {
        let (key, val_raw) = raw
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!(format!("invalid --set '{}': expected k=v", raw)))?;
        if key.trim().is_empty() {
            return Err(anyhow::anyhow!(format!(
                "invalid --set '{}': key cannot be empty",
                raw
            )));
        }
        let parsed =
            serde_json::from_str::<Value>(val_raw).unwrap_or(Value::String(val_raw.to_string()));
        out.insert(key.to_string(), parsed);
    }
    Ok(out)
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

fn build_run_execution_options(
    executor: Option<ExecutorArg>,
    materialize: Option<MaterializeArg>,
    run_root: Option<PathBuf>,
    runtime_env: &[String],
    runtime_env_files: &[PathBuf],
    secret_files: &[String],
) -> Result<lab_runner::RunExecutionOptions> {
    Ok(lab_runner::RunExecutionOptions {
        executor: executor.map(Into::into),
        materialize: materialize.map(Into::into),
        run_root,
        runtime_env: parse_runtime_env_bindings(runtime_env)?,
        runtime_env_files: runtime_env_files.to_vec(),
        secret_files: parse_secret_file_bindings(secret_files)?,
    })
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
        "comparison": summary.comparison,
        "retry_max_attempts": summary.retry_max_attempts,
        "preflight_warnings": summary.preflight_warnings
    })
}

fn print_summary(summary: &lab_runner::ExperimentSummary) {
    println!("experiment: {}", summary.exp_id);
    println!("workload_type: {}", summary.workload_type);
    println!("dataset: {}", summary.dataset_path.display());
    println!("tasks: {}", summary.task_count);
    println!("replications: {}", summary.replications);
    println!("variant_count: {}", summary.variant_count);
    println!("total_trials: {}", summary.total_trials);
    println!("agent_runtime: {:?}", summary.agent_runtime_command);
    if let Some(image) = &summary.image {
        println!("image: {}", image);
    }
    println!("network: {}", summary.network_mode);
    if let Some(path) = &summary.trajectory_path {
        println!("trajectory_path: {}", path);
    }
    if let Some(mode) = &summary.causal_extraction {
        println!("causal_extraction: {}", mode);
    }
    if !summary.preflight_warnings.is_empty() {
        println!("preflight_warnings:");
        for w in &summary.preflight_warnings {
            println!("  - {}", w);
        }
    }
}

fn print_preflight_report(report: &lab_runner::PreflightReport) {
    for check in &report.checks {
        let icon = if check.passed {
            "PASS"
        } else {
            match check.severity {
                lab_runner::PreflightSeverity::Error => "FAIL",
                lab_runner::PreflightSeverity::Warning => "WARN",
            }
        };
        println!("[{}] {}: {}", icon, check.name, check.message);
    }
    if report.passed {
        println!("\npreflight: all checks passed");
    } else {
        println!("\npreflight: FAILED — resolve errors above before running");
    }
}

fn print_package_check_report(report: &Value) {
    let summary = report.get("summary").unwrap_or(&Value::Null);
    println!(
        "package_checks: passed={} checks={} failed={} warnings={} skipped={}",
        report
            .get("passed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        summary.get("checks").and_then(Value::as_u64).unwrap_or(0),
        summary.get("failed").and_then(Value::as_u64).unwrap_or(0),
        summary.get("warnings").and_then(Value::as_u64).unwrap_or(0),
        summary.get("skipped").and_then(Value::as_u64).unwrap_or(0)
    );
    if let Some(checks) = report.get("checks").and_then(Value::as_array) {
        for check in checks {
            let status = check
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_ascii_uppercase();
            let id = check.get("id").and_then(Value::as_str).unwrap_or("<check>");
            let reason = check.get("reason").and_then(Value::as_str).unwrap_or("");
            println!("[{}] {}: {}", status, id, reason);
        }
    }
}

fn resolve_run_dir_arg(run: &str) -> Result<PathBuf> {
    let raw = PathBuf::from(run);
    if raw.exists() {
        return raw
            .canonicalize()
            .map_err(|_| anyhow::anyhow!(format!("run path not found: {}", raw.display())));
    }

    let cwd = std::env::current_dir()?;
    let db_path = lab_runner::account_sqlite_path_for_run(cwd.as_path())?;
    if db_path.exists() {
        let account_id = lab_runner::active_account_id();
        let conn = Connection::open(&db_path)?;
        let run_dir: Option<String> = conn
            .query_row(
                "SELECT run_dir FROM runs WHERE account_id=?1 AND run_id=?2",
                params![account_id, run],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(run_dir) = run_dir {
            let path = PathBuf::from(run_dir);
            return Ok(path.canonicalize().unwrap_or(path));
        }
    }

    Err(anyhow::anyhow!(format!(
        "run '{}' not found in account SQLite database {}",
        run,
        db_path.display()
    )))
}

fn resolve_project_root(start: &Path) -> PathBuf {
    start.to_path_buf()
}

fn table_render_format(csv: bool, md: bool, html: bool) -> TableRenderFormat {
    if csv {
        TableRenderFormat::Csv
    } else if md {
        TableRenderFormat::Markdown
    } else if html {
        TableRenderFormat::Html
    } else {
        TableRenderFormat::Text
    }
}

fn query_resolved_view(
    run_dir: &Path,
    resolved: &ResolvedView,
    limit: usize,
) -> Result<analysis::QueryTable> {
    let table = match &resolved.plan {
        ResolvedViewPlan::Source(source) => query_source_view(run_dir, source, limit)?,
        ResolvedViewPlan::AbComparisonSummary => query_ab_comparison_summary(run_dir)?,
        ResolvedViewPlan::Scoreboard => query_scoreboard(run_dir)?,
    };

    if resolved.standardize_ab_terms {
        return Ok(standardize_ab_table_columns(&table));
    }
    Ok(table)
}

fn present_display_table(
    resolved: &ResolvedView,
    table: &analysis::QueryTable,
) -> analysis::QueryTable {
    present_table(resolved.spec, table).table
}

fn run_interactive_views_browser(
    project_root: &Path,
    initial_run_dir: Option<PathBuf>,
    initial_view: Option<&str>,
    sleep_interval: Duration,
    limit: usize,
) -> Result<()> {
    let mut term = tui::Term::new()?;
    let can_return_to_run_picker = initial_run_dir.is_none();
    let mut run_entries = collect_run_inventory(project_root)?;
    let mut current_run_dir = initial_run_dir;
    let mut current_view = None;
    let mut selected_run_idx = 0usize;
    let mut selected_view_idx = 0usize;
    let mut detail_snapshot: Option<DetailSnapshot> = None;
    let mut viewer_table_cursor: usize = 0;

    if let Some(run_dir) = current_run_dir.as_ref() {
        if let Some(idx) = run_entries
            .iter()
            .position(|entry| entry.run_dir == *run_dir)
        {
            selected_run_idx = idx;
        }
        if let Some(requested_view) = initial_view {
            let run_view_set = analysis::run_view_set(run_dir)?;
            let raw_view_names = list_available_analysis_views(run_dir);
            let resolved = resolve_requested_view(run_view_set, &raw_view_names, requested_view)?;
            selected_view_idx = standard_views_for_set(run_view_set)
                .iter()
                .position(|def| def.name == resolved.name)
                .unwrap_or(0);
            current_view = Some(resolved);
        }
    }

    let mut screen = match (&current_run_dir, &current_view) {
        (Some(_), Some(_)) => ViewsBrowserScreen::Viewer,
        (Some(_), None) => ViewsBrowserScreen::ViewPicker,
        (None, _) => ViewsBrowserScreen::RunPicker,
    };
    term.set_selected(match screen {
        ViewsBrowserScreen::RunPicker => selection_for_len(
            selected_run_idx,
            run_entries
                .iter()
                .filter(|entry| show_in_live_run_picker(entry))
                .count(),
        ),
        ViewsBrowserScreen::ViewPicker => Some(selected_view_idx),
        ViewsBrowserScreen::Viewer | ViewsBrowserScreen::Detail => Some(0),
    });

    loop {
        match screen {
            ViewsBrowserScreen::RunPicker => {
                run_entries = collect_run_inventory(project_root)?;
                let active_run_entries = run_entries
                    .iter()
                    .filter(|entry| show_in_live_run_picker(entry))
                    .cloned()
                    .collect::<Vec<_>>();
                selected_run_idx = resolve_run_selection(
                    current_run_dir.as_deref(),
                    &active_run_entries,
                    selected_run_idx,
                );
                let run_items = build_run_browser_items(&active_run_entries);
                term.set_selected(selection_for_len(
                    selected_run_idx,
                    active_run_entries.len(),
                ));
                term.draw(&tui::Screen::RunBrowser(tui::RunBrowserState {
                    items: &run_items,
                    refresh_secs: sleep_interval.as_secs(),
                    chrome_title: "Bucephalus",
                    description: "Live and interrupted runs are pinned first. Pick one, then choose the exact view you want to inspect.",
                }))?;

                match term.poll(sleep_interval)? {
                    tui::Action::Quit => break,
                    tui::Action::Back => break,
                    tui::Action::Select => {
                        if let Some(entry) = active_run_entries.get(selected_run_idx) {
                            current_run_dir = Some(entry.run_dir.clone());
                            selected_view_idx = 0;
                            screen = ViewsBrowserScreen::ViewPicker;
                            term.set_selected(Some(0));
                        }
                    }
                    tui::Action::ScrollUp => {
                        term.scroll_up();
                        selected_run_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::ScrollDown => {
                        term.scroll_down(active_run_entries.len());
                        selected_run_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::PageUp => {
                        term.page_up();
                        selected_run_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::PageDown => {
                        term.page_down(active_run_entries.len());
                        selected_run_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::Refresh | tui::Action::Tick => {}
                }
            }
            ViewsBrowserScreen::ViewPicker => {
                let run_dir = current_run_dir
                    .as_ref()
                    .ok_or_else(|| anyhow!("interactive view picker requires a selected run"))?;
                let run_entry = lookup_run_inventory(&run_entries, run_dir)
                    .unwrap_or_else(|| inspect_run_inventory_entry(run_dir));
                let run_view_set = analysis::run_view_set(run_dir)?;
                let standard_views = standard_views_for_set(run_view_set);
                selected_view_idx = clamp_index(selected_view_idx, standard_views.len());
                let view_items = build_view_browser_items(run_view_set);
                term.set_selected(selection_for_len(selected_view_idx, standard_views.len()));
                term.draw(&tui::Screen::ViewBrowser(tui::ViewBrowserState {
                    run_id: &run_entry.run_id,
                    experiment: &run_entry.experiment,
                    started_at: &run_entry.started_at_display,
                    status: &run_entry.control.status_display,
                    items: &view_items,
                    refresh_secs: sleep_interval.as_secs(),
                    chrome_title: "Bucephalus",
                }))?;

                match term.poll(sleep_interval)? {
                    tui::Action::Quit => break,
                    tui::Action::Back => {
                        if can_return_to_run_picker {
                            current_run_dir = None;
                            screen = ViewsBrowserScreen::RunPicker;
                            let active_len = run_entries
                                .iter()
                                .filter(|entry| show_in_live_run_picker(entry))
                                .count();
                            term.set_selected(selection_for_len(selected_run_idx, active_len));
                        } else {
                            break;
                        }
                    }
                    tui::Action::Select => {
                        if let Some(def) = standard_views.get(selected_view_idx) {
                            current_view = Some(resolved_view_from_spec(run_view_set, def));
                            screen = ViewsBrowserScreen::Viewer;
                            term.set_selected(Some(0));
                        }
                    }
                    tui::Action::ScrollUp => {
                        term.scroll_up();
                        selected_view_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::ScrollDown => {
                        term.scroll_down(standard_views.len());
                        selected_view_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::PageUp => {
                        term.page_up();
                        selected_view_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::PageDown => {
                        term.page_down(standard_views.len());
                        selected_view_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::Refresh | tui::Action::Tick => {}
                }
            }
            ViewsBrowserScreen::Viewer => {
                let run_dir = current_run_dir
                    .as_ref()
                    .ok_or_else(|| anyhow!("interactive viewer requires a selected run"))?;
                let run_entry = lookup_run_inventory(&run_entries, run_dir)
                    .unwrap_or_else(|| inspect_run_inventory_entry(run_dir));
                let run_view_set = analysis::run_view_set(run_dir)?;
                let raw_view_names = list_available_analysis_views(run_dir);
                let resolved_view = match current_view.clone() {
                    Some(view) => view,
                    None => resolve_requested_view(run_view_set, &raw_view_names, "run_progress")?,
                };
                current_view = Some(resolved_view.clone());

                let table = query_resolved_view(run_dir, &resolved_view, limit)?;
                let display_mode = display_mode_for_view(&resolved_view);
                let (display, legend, split_labels) =
                    if resolved_view.name == "trace" && has_ab_trace_columns(&table) {
                        let (d, l, s) = prepare_trace_split_view(&table);
                        (d, l, Some(s))
                    } else {
                        let presented = present_table(resolved_view.spec, &table);
                        (presented.table, presented.legend, None)
                    };

                let hints = [
                    tui::KeyHint {
                        key: "Esc",
                        label: "views",
                    },
                    tui::KeyHint {
                        key: "q",
                        label: "quit",
                    },
                    tui::KeyHint {
                        key: "r",
                        label: "refresh",
                    },
                ];
                let split_refs = split_labels.as_ref().map(|(l, r)| (l.as_str(), r.as_str()));
                let hints_with_detail = [
                    tui::KeyHint {
                        key: "Enter",
                        label: "detail",
                    },
                    tui::KeyHint {
                        key: "Esc",
                        label: "views",
                    },
                    tui::KeyHint {
                        key: "q",
                        label: "quit",
                    },
                    tui::KeyHint {
                        key: "r",
                        label: "refresh",
                    },
                ];
                term.draw(&tui::Screen::LiveView(tui::ViewState {
                    run_id: &run_entry.run_id,
                    status: &run_entry.control.status_display,
                    started_at: &run_entry.started_at_display,
                    view_name: &resolved_view.name,
                    interval_secs: sleep_interval.as_secs(),
                    table: &display,
                    display_mode,
                    progress: read_run_progress(run_dir),
                    legend: &legend,
                    split_labels: split_refs,
                    hints: &hints_with_detail,
                }))?;
                let _ = hints; // silence unused-warning while detail hints win

                match term.poll(sleep_interval)? {
                    tui::Action::Quit => break,
                    tui::Action::Back => {
                        selected_view_idx = standard_views_for_set(run_view_set)
                            .iter()
                            .position(|def| def.name == resolved_view.name)
                            .unwrap_or(0);
                        screen = ViewsBrowserScreen::ViewPicker;
                        term.set_selected(Some(selected_view_idx));
                    }
                    tui::Action::ScrollUp => term.scroll_up(),
                    tui::Action::ScrollDown => term.scroll_down(display.rows.len()),
                    tui::Action::PageUp => term.page_up(),
                    tui::Action::PageDown => term.page_down(display.rows.len()),
                    tui::Action::Select => {
                        viewer_table_cursor = term.selected().unwrap_or(0);
                        if let Some(snap) = build_detail_snapshot(
                            &resolved_view.name,
                            &run_entry.run_id,
                            &table,
                            viewer_table_cursor,
                        ) {
                            detail_snapshot = Some(snap);
                            screen = ViewsBrowserScreen::Detail;
                        }
                    }
                    tui::Action::Refresh | tui::Action::Tick => {}
                }
            }
            ViewsBrowserScreen::Detail => {
                let Some(snap) = detail_snapshot.as_ref() else {
                    screen = ViewsBrowserScreen::Viewer;
                    continue;
                };
                let fields_borrow: &[(String, String)] = &snap.fields;
                term.draw(&tui::Screen::Detail(tui::DetailState {
                    run_id: &snap.run_id_label,
                    view_name: &snap.view_name,
                    row_label: &snap.row_label,
                    fields: fields_borrow,
                    payload: snap.payload.as_deref(),
                }))?;

                match term.poll(sleep_interval)? {
                    tui::Action::Quit => break,
                    tui::Action::Back => {
                        screen = ViewsBrowserScreen::Viewer;
                        term.set_selected(Some(viewer_table_cursor));
                        detail_snapshot = None;
                    }
                    tui::Action::Refresh
                    | tui::Action::Tick
                    | tui::Action::ScrollUp
                    | tui::Action::ScrollDown
                    | tui::Action::PageUp
                    | tui::Action::PageDown
                    | tui::Action::Select => {}
                }
            }
        }
    }

    Ok(())
}

fn run_views_browser(project_root: &Path) -> Result<()> {
    let mut term = tui::Term::new()?;
    let mut run_entries = collect_run_inventory(project_root)?;
    let mut selected_run_idx = 0usize;
    let mut selected_view_idx = 0usize;
    let mut current_run_dir: Option<PathBuf> = None;
    let mut current_view: Option<ResolvedView> = None;
    let poll_timeout = Duration::from_secs(120);
    let mut detail_snapshot: Option<DetailSnapshot> = None;
    let mut viewer_table_cursor: usize = 0;

    enum BrowserScreen {
        RunPicker,
        ViewPicker,
        Viewer,
        Detail,
    }
    let mut screen = BrowserScreen::RunPicker;
    term.set_selected(selection_for_len(0, run_entries.len()));

    loop {
        match screen {
            BrowserScreen::RunPicker => {
                selected_run_idx = resolve_run_selection(
                    current_run_dir.as_deref(),
                    &run_entries,
                    selected_run_idx,
                );
                let run_items = build_run_browser_items(&run_entries);
                term.set_selected(selection_for_len(selected_run_idx, run_entries.len()));
                term.draw(&tui::Screen::RunBrowser(tui::RunBrowserState {
                    items: &run_items,
                    refresh_secs: 0,
                    chrome_title: "Bucephalus",
                    description:
                        "Most recent runs first. Pick a run, then choose the view to display.",
                }))?;

                match term.poll(poll_timeout)? {
                    tui::Action::Quit | tui::Action::Back => break,
                    tui::Action::Select => {
                        if let Some(entry) = run_entries.get(selected_run_idx) {
                            current_run_dir = Some(entry.run_dir.clone());
                            selected_view_idx = 0;
                            screen = BrowserScreen::ViewPicker;
                            term.set_selected(Some(0));
                        }
                    }
                    tui::Action::ScrollUp => {
                        term.scroll_up();
                        selected_run_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::ScrollDown => {
                        term.scroll_down(run_entries.len());
                        selected_run_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::PageUp => {
                        term.page_up();
                        selected_run_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::PageDown => {
                        term.page_down(run_entries.len());
                        selected_run_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::Refresh => {
                        run_entries = collect_run_inventory(project_root)?;
                    }
                    tui::Action::Tick => {}
                }
            }
            BrowserScreen::ViewPicker => {
                let run_dir = current_run_dir.as_ref().unwrap();
                let run_entry = lookup_run_inventory(&run_entries, run_dir)
                    .unwrap_or_else(|| inspect_run_inventory_entry(run_dir));
                let run_view_set = analysis::run_view_set(run_dir)?;
                let standard_views = standard_views_for_set(run_view_set);
                selected_view_idx = clamp_index(selected_view_idx, standard_views.len());
                let view_items = build_view_browser_items(run_view_set);
                term.set_selected(selection_for_len(selected_view_idx, standard_views.len()));
                term.draw(&tui::Screen::ViewBrowser(tui::ViewBrowserState {
                    run_id: &run_entry.run_id,
                    experiment: &run_entry.experiment,
                    started_at: &run_entry.started_at_display,
                    status: &run_entry.control.status_display,
                    items: &view_items,
                    refresh_secs: 0,
                    chrome_title: "Bucephalus",
                }))?;

                match term.poll(poll_timeout)? {
                    tui::Action::Quit => break,
                    tui::Action::Back => {
                        current_run_dir = None;
                        screen = BrowserScreen::RunPicker;
                        term.set_selected(selection_for_len(selected_run_idx, run_entries.len()));
                    }
                    tui::Action::Select => {
                        if let Some(def) = standard_views.get(selected_view_idx) {
                            current_view = Some(resolved_view_from_spec(run_view_set, def));
                            screen = BrowserScreen::Viewer;
                            term.set_selected(Some(0));
                        }
                    }
                    tui::Action::ScrollUp => {
                        term.scroll_up();
                        selected_view_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::ScrollDown => {
                        term.scroll_down(standard_views.len());
                        selected_view_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::PageUp => {
                        term.page_up();
                        selected_view_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::PageDown => {
                        term.page_down(standard_views.len());
                        selected_view_idx = term.selected().unwrap_or(0);
                    }
                    tui::Action::Refresh | tui::Action::Tick => {}
                }
            }
            BrowserScreen::Viewer => {
                let run_dir = current_run_dir.as_ref().unwrap();
                let run_entry = lookup_run_inventory(&run_entries, run_dir)
                    .unwrap_or_else(|| inspect_run_inventory_entry(run_dir));
                let run_view_set = analysis::run_view_set(run_dir)?;
                let resolved_view = current_view.clone().unwrap_or_else(|| {
                    let raw = list_available_analysis_views(run_dir);
                    resolve_requested_view(run_view_set, &raw, "run_progress").unwrap_or(
                        ResolvedView {
                            name: "run_progress".to_string(),
                            source: None,
                            plan: ResolvedViewPlan::Source("run_progress".to_string()),
                            standardize_ab_terms: false,
                            spec: None,
                        },
                    )
                });
                current_view = Some(resolved_view.clone());

                let table = query_resolved_view(run_dir, &resolved_view, 0)?;
                let display_mode = display_mode_for_view(&resolved_view);
                let (display, legend, split_labels) =
                    if resolved_view.name == "trace" && has_ab_trace_columns(&table) {
                        let (d, l, s) = prepare_trace_split_view(&table);
                        (d, l, Some(s))
                    } else {
                        let presented = present_table(resolved_view.spec, &table);
                        (presented.table, presented.legend, None)
                    };

                let hints = [
                    tui::KeyHint {
                        key: "Esc",
                        label: "views",
                    },
                    tui::KeyHint {
                        key: "q",
                        label: "quit",
                    },
                    tui::KeyHint {
                        key: "r",
                        label: "refresh",
                    },
                ];
                let split_refs = split_labels.as_ref().map(|(l, r)| (l.as_str(), r.as_str()));
                let hints_with_detail = [
                    tui::KeyHint {
                        key: "Enter",
                        label: "detail",
                    },
                    tui::KeyHint {
                        key: "Esc",
                        label: "views",
                    },
                    tui::KeyHint {
                        key: "q",
                        label: "quit",
                    },
                    tui::KeyHint {
                        key: "r",
                        label: "refresh",
                    },
                ];
                term.draw(&tui::Screen::LiveView(tui::ViewState {
                    run_id: &run_entry.run_id,
                    status: &run_entry.control.status_display,
                    started_at: &run_entry.started_at_display,
                    view_name: &resolved_view.name,
                    interval_secs: 0,
                    table: &display,
                    display_mode,
                    progress: read_run_progress(run_dir),
                    legend: &legend,
                    split_labels: split_refs,
                    hints: &hints_with_detail,
                }))?;
                let _ = hints;

                match term.poll(poll_timeout)? {
                    tui::Action::Quit => break,
                    tui::Action::Back => {
                        selected_view_idx = standard_views_for_set(run_view_set)
                            .iter()
                            .position(|def| def.name == resolved_view.name)
                            .unwrap_or(0);
                        screen = BrowserScreen::ViewPicker;
                        term.set_selected(Some(selected_view_idx));
                    }
                    tui::Action::ScrollUp => term.scroll_up(),
                    tui::Action::ScrollDown => term.scroll_down(display.rows.len()),
                    tui::Action::PageUp => term.page_up(),
                    tui::Action::PageDown => term.page_down(display.rows.len()),
                    tui::Action::Select => {
                        viewer_table_cursor = term.selected().unwrap_or(0);
                        if let Some(snap) = build_detail_snapshot(
                            &resolved_view.name,
                            &run_entry.run_id,
                            &table,
                            viewer_table_cursor,
                        ) {
                            detail_snapshot = Some(snap);
                            screen = BrowserScreen::Detail;
                        }
                    }
                    tui::Action::Refresh | tui::Action::Tick => {}
                }
            }
            BrowserScreen::Detail => {
                let Some(snap) = detail_snapshot.as_ref() else {
                    screen = BrowserScreen::Viewer;
                    continue;
                };
                let fields_borrow: &[(String, String)] = &snap.fields;
                term.draw(&tui::Screen::Detail(tui::DetailState {
                    run_id: &snap.run_id_label,
                    view_name: &snap.view_name,
                    row_label: &snap.row_label,
                    fields: fields_borrow,
                    payload: snap.payload.as_deref(),
                }))?;
                match term.poll(poll_timeout)? {
                    tui::Action::Quit => break,
                    tui::Action::Back => {
                        screen = BrowserScreen::Viewer;
                        term.set_selected(Some(viewer_table_cursor));
                        detail_snapshot = None;
                    }
                    tui::Action::Refresh
                    | tui::Action::Tick
                    | tui::Action::ScrollUp
                    | tui::Action::ScrollDown
                    | tui::Action::PageUp
                    | tui::Action::PageDown
                    | tui::Action::Select => {}
                }
            }
        }
    }

    Ok(())
}

fn selection_for_len(index: usize, len: usize) -> Option<usize> {
    if len == 0 {
        None
    } else {
        Some(clamp_index(index, len))
    }
}

fn clamp_index(index: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        index.min(len.saturating_sub(1))
    }
}

fn resolve_run_selection(
    anchor_run_dir: Option<&Path>,
    entries: &[RunInventoryEntry],
    selected_idx: usize,
) -> usize {
    clamp_index(
        if let Some(run_dir) = anchor_run_dir {
            entries
                .iter()
                .position(|e| e.run_dir == *run_dir)
                .unwrap_or(selected_idx)
        } else {
            selected_idx
        },
        entries.len(),
    )
}

fn build_run_browser_items(entries: &[RunInventoryEntry]) -> Vec<tui::RunBrowserItem> {
    entries
        .iter()
        .map(|entry| tui::RunBrowserItem {
            run_id: entry.run_id.clone(),
            experiment: display_or_dash(&entry.experiment),
            started_at: entry.started_at_display.clone(),
            status: entry.control.status.clone(),
            status_detail: entry.control.status_display.clone(),
            active_trials: entry.control.active_trials,
        })
        .collect()
}

fn show_in_live_run_picker(entry: &RunInventoryEntry) -> bool {
    entry.control.is_active || entry.control.status == "interrupted"
}

fn build_view_browser_items(view_set: analysis::ViewSet) -> Vec<tui::ViewBrowserItem> {
    standard_views_for_set(view_set)
        .iter()
        .map(|def| tui::ViewBrowserItem {
            name: def.name.to_string(),
            purpose: def.purpose.to_string(),
            category: Some(def.category),
        })
        .collect()
}

fn build_detail_snapshot(
    view_name: &str,
    run_id_label: &str,
    table: &analysis::QueryTable,
    row_idx: usize,
) -> Option<DetailSnapshot> {
    let row = table.rows.get(row_idx)?;
    let row_label = ["trial_id", "task_id", "variant_id", "run_id", "row_seq"]
        .iter()
        .find_map(|key| {
            let idx = table.columns.iter().position(|c| c == key)?;
            let raw = row.get(idx).map(render_json_cell).unwrap_or_default();
            if raw.is_empty() {
                None
            } else {
                Some(format!("{key}={}", view_layout::compact_identifier(&raw)))
            }
        })
        .unwrap_or_else(|| format!("row {}", row_idx + 1));

    let mut fields = Vec::with_capacity(table.columns.len());
    let mut payload: Option<String> = None;
    for (idx, column) in table.columns.iter().enumerate() {
        let value = row.get(idx).cloned().unwrap_or(Value::Null);
        if column == "payload_json" || column == "payload" {
            let pretty = view_layout::pretty_payload(&value);
            if !pretty.trim().is_empty() && pretty != "null" {
                payload = Some(pretty);
            }
            continue;
        }
        let rendered = render_json_cell(&value);
        if rendered.is_empty() {
            continue;
        }
        fields.push((column.clone(), rendered));
    }

    Some(DetailSnapshot {
        view_name: view_name.to_string(),
        run_id_label: run_id_label.to_string(),
        row_label,
        fields,
        payload,
    })
}

fn lookup_run_inventory(
    entries: &[RunInventoryEntry],
    run_dir: &Path,
) -> Option<RunInventoryEntry> {
    entries
        .iter()
        .find(|entry| entry.run_dir == run_dir)
        .cloned()
}

fn query_source_view(
    run_dir: &Path,
    source_view: &str,
    limit: usize,
) -> Result<analysis::QueryTable> {
    if limit == 0 {
        let sql = format!("SELECT * FROM {}", sql_identifier(source_view));
        return analysis::query_run(run_dir, &sql)
            .or_else(|_| query_state_backed_source_view(run_dir, source_view));
    }
    analysis::query_view(run_dir, source_view, limit)
        .or_else(|_| query_state_backed_source_view(run_dir, source_view))
}

fn query_state_backed_source_view(
    run_dir: &Path,
    source_view: &str,
) -> Result<analysis::QueryTable> {
    match source_view {
        "run_progress" => Ok(build_state_run_progress_table(run_dir)),
        "contract_health" => Ok(build_state_contract_health_table(run_dir)),
        other => Err(anyhow!(
            "view '{}' requires the analysis query engine; live state fallback is available for run_progress, health, and scoreboard",
            other
        )),
    }
}

fn query_ab_comparison_summary(run_dir: &Path) -> Result<analysis::QueryTable> {
    let sql = "WITH delta AS (
            SELECT
                coalesce(max(CASE WHEN delta_type = 'regression' THEN n END), 0) AS variant_a_better_n,
                coalesce(max(CASE WHEN delta_type = 'improvement' THEN n END), 0) AS variant_b_better_n,
                coalesce(max(CASE WHEN delta_type = 'same' THEN n END), 0) AS same_outcome_n,
                coalesce(max(CASE WHEN delta_type = 'changed' THEN n END), 0) AS changed_outcome_n,
                coalesce(max(CASE WHEN delta_type = 'regression' THEN pct END), 0.0) AS variant_a_better_pct,
                coalesce(max(CASE WHEN delta_type = 'improvement' THEN pct END), 0.0) AS variant_b_better_pct,
                coalesce(max(CASE WHEN delta_type = 'same' THEN pct END), 0.0) AS same_outcome_pct,
                coalesce(max(CASE WHEN delta_type = 'changed' THEN pct END), 0.0) AS changed_outcome_pct
            FROM win_loss_tie
        ),
        effect AS (
            SELECT
                baseline_rate AS variant_a_rate,
                treatment_rate AS variant_b_rate,
                absolute_diff AS variant_b_minus_variant_a,
                cohens_h,
                magnitude
            FROM effect_size
        ),
        mcnemar AS (
            SELECT
                both_pass,
                base_only AS variant_a_only,
                treat_only AS variant_b_only,
                both_fail,
                mcnemar_chi2
            FROM mcnemar_contingency
        )
        SELECT
            effect.variant_a_rate,
            effect.variant_b_rate,
            effect.variant_b_minus_variant_a,
            delta.variant_a_better_n,
            delta.variant_b_better_n,
            delta.same_outcome_n,
            delta.changed_outcome_n,
            delta.variant_a_better_pct,
            delta.variant_b_better_pct,
            delta.same_outcome_pct,
            delta.changed_outcome_pct,
            mcnemar.both_pass,
            mcnemar.variant_a_only,
            mcnemar.variant_b_only,
            mcnemar.both_fail,
            mcnemar.mcnemar_chi2,
            effect.cohens_h,
            effect.magnitude
        FROM effect
        CROSS JOIN delta
        CROSS JOIN mcnemar";
    analysis::query_run(run_dir, sql)
}

fn standardize_ab_table_columns(table: &analysis::QueryTable) -> analysis::QueryTable {
    analysis::QueryTable {
        columns: table
            .columns
            .iter()
            .map(|name| standardize_ab_column_name(name))
            .collect(),
        rows: table.rows.clone(),
    }
}

fn standardize_ab_column_name(name: &str) -> String {
    match name {
        "baseline_id" | "a_variant_id" => "variant_a_id".to_string(),
        "treatment_id" | "b_variant_id" => "variant_b_id".to_string(),
        "treatment_variant_count" => "comparison_variant_count".to_string(),
        "baseline_outcome" => "variant_a_outcome".to_string(),
        "treatment_outcome" => "variant_b_outcome".to_string(),
        "baseline_metric" => "variant_a_metric".to_string(),
        "treatment_metric" => "variant_b_metric".to_string(),
        "baseline_rate" => "variant_a_rate".to_string(),
        "treatment_rate" => "variant_b_rate".to_string(),
        "base_only" => "variant_a_only".to_string(),
        "treat_only" => "variant_b_only".to_string(),
        "a_trial_id" => "variant_a_trial_id".to_string(),
        "b_trial_id" => "variant_b_trial_id".to_string(),
        other => {
            if let Some(rest) = other.strip_prefix("a_") {
                return format!("variant_a_{}", rest);
            }
            if let Some(rest) = other.strip_prefix("b_") {
                return format!("variant_b_{}", rest);
            }
            if let Some(rest) = other.strip_prefix("d_") {
                return format!("delta_{}", rest);
            }
            other.to_string()
        }
    }
}

fn build_live_scoreboard_table(
    run_dir: &Path,
    metric_limit: usize,
) -> Result<analysis::QueryTable> {
    let limit = metric_limit.clamp(1, 32);
    let metric_names = fetch_scoreboard_metric_names(run_dir, limit)?;
    let sql = build_scoreboard_sql(&metric_names);
    analysis::query_run(run_dir, &sql)
}

fn build_inflight_scoreboard_table(run_dir: &Path) -> Option<analysis::QueryTable> {
    let parsed = load_run_control(run_dir)?;
    let active_trials = parsed.get("active_trials").and_then(Value::as_object)?;
    if active_trials.is_empty() {
        return None;
    }

    let mut rows: Vec<(i64, String, Vec<Value>)> = Vec::with_capacity(active_trials.len());
    for (trial_key, entry) in active_trials {
        let trial_id = entry
            .get("trial_id")
            .and_then(Value::as_str)
            .unwrap_or(trial_key)
            .to_string();
        let variant_id = entry
            .get("variant_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let schedule_idx = entry
            .get("schedule_idx")
            .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)));
        let worker_id = entry
            .get("worker_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let started_at = entry
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let row = vec![
            Value::String(variant_id),
            Value::String(trial_id.clone()),
            schedule_idx.map_or(Value::Null, |idx| json!(idx)),
            Value::String(worker_id),
            Value::String(started_at),
            Value::String("in_flight".to_string()),
        ];
        rows.push((schedule_idx.unwrap_or(i64::MAX), trial_id, row));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let sorted_rows = rows.into_iter().map(|(_, _, row)| row).collect();

    Some(analysis::QueryTable {
        columns: vec![
            "variant_id".to_string(),
            "trial_id".to_string(),
            "schedule_idx".to_string(),
            "worker_id".to_string(),
            "started_at".to_string(),
            "lifecycle".to_string(),
        ],
        rows: sorted_rows,
    })
}

fn query_scoreboard(run_dir: &Path) -> Result<analysis::QueryTable> {
    if let Ok(table) = build_live_scoreboard_table(run_dir, 8) {
        if !table.rows.is_empty() {
            return Ok(table);
        }
    }
    if let Some(inflight) = build_inflight_scoreboard_table(run_dir) {
        return Ok(inflight);
    }
    Ok(analysis::QueryTable {
        columns: vec![
            "variant_id".to_string(),
            "trial_id".to_string(),
            "schedule_idx".to_string(),
            "worker_id".to_string(),
            "started_at".to_string(),
            "lifecycle".to_string(),
        ],
        rows: Vec::new(),
    })
}

fn fetch_scoreboard_metric_names(run_dir: &Path, metric_limit: usize) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT metric_name
         FROM metrics_long m
         WHERE m.metric_name <> 'status_code'
           AND m.metric_name <> 'success'
           AND NOT EXISTS (
             SELECT 1
             FROM trials t
             WHERE t.primary_metric_name = m.metric_name
           )
         GROUP BY metric_name
         ORDER BY metric_name
         LIMIT {}",
        metric_limit
    );
    let table = analysis::query_run(run_dir, &sql)?;
    let mut out = Vec::new();
    for row in table.rows {
        if let Some(name) = row.first().and_then(Value::as_str) {
            if !name.trim().is_empty() {
                out.push(name.to_string());
            }
        }
    }
    Ok(out)
}

fn build_scoreboard_sql(metric_names: &[String]) -> String {
    let mut columns = Vec::new();
    for metric_name in metric_names {
        let alias = format!("{}_mean", sanitize_scoreboard_alias(metric_name));
        columns.push(format!(
            "(SELECT round(m.mean_metric, 4)
             FROM metric_agg m
             WHERE m.variant_id = b.variant_id
               AND m.task_id = b.task_id
               AND m.metric_name = {}) AS {}",
            sql_string_literal(metric_name),
            sql_identifier(&alias)
        ));
    }
    let dynamic_cols = if columns.is_empty() {
        String::new()
    } else {
        format!(",\n    {}", columns.join(",\n    "))
    };
    format!(
        "WITH base AS (
            SELECT
                variant_id,
                task_id,
                count(*) AS n_trials,
                round(avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS success_rate,
                round(avg(try_cast(primary_metric_value AS DOUBLE)), 4) AS primary_metric_mean
            FROM trials
            GROUP BY variant_id, task_id
        ),
        metric_agg AS (
            SELECT
                variant_id,
                task_id,
                metric_name,
                avg(try_cast(metric_value AS DOUBLE)) AS mean_metric
            FROM metrics_long
            GROUP BY variant_id, task_id, metric_name
        )
        SELECT
            b.variant_id,
            b.task_id,
            b.n_trials,
            b.success_rate,
            b.primary_metric_mean{}
        FROM base b
        ORDER BY b.variant_id, b.task_id",
        dynamic_cols
    )
}

fn sanitize_scoreboard_alias(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn sql_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn terminal_width() -> usize {
    #[cfg(unix)]
    {
        use std::mem::MaybeUninit;
        let mut ws = MaybeUninit::<libc::winsize>::uninit();
        let ret = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, ws.as_mut_ptr()) };
        if ret == 0 {
            let ws = unsafe { ws.assume_init() };
            if ws.ws_col > 0 {
                return ws.ws_col as usize;
            }
        }
    }
    120
}

fn print_scoreboard_table(table: &analysis::QueryTable, term_width: usize) {
    if table.columns.is_empty() {
        println!("(ok)");
        return;
    }

    let rendered_rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|row| row.iter().map(render_json_cell).collect::<Vec<String>>())
        .collect();

    let numeric_cols: Vec<bool> = (0..table.columns.len())
        .map(|col_idx| {
            let mut has_number = false;
            for row in &table.rows {
                match row.get(col_idx) {
                    Some(Value::Number(_)) => has_number = true,
                    Some(Value::Null) => {}
                    _ => return false,
                }
            }
            has_number
        })
        .collect();

    let mut natural_widths: Vec<usize> = table.columns.iter().map(|c| c.chars().count()).collect();
    for row in &rendered_rows {
        for (idx, cell) in row.iter().enumerate() {
            if idx < natural_widths.len() {
                natural_widths[idx] = natural_widths[idx].max(cell.chars().count());
            }
        }
    }

    let core_count = table
        .columns
        .iter()
        .position(|c| c.ends_with("_mean") && c != "primary_metric_mean")
        .unwrap_or(table.columns.len());

    let (visible_count, sep, widths) =
        fit_columns_to_width(&natural_widths, core_count, term_width);

    let header: String = table.columns[..visible_count]
        .iter()
        .enumerate()
        .map(|(idx, col)| {
            pad_cell(
                &truncate_cell(col, widths[idx]),
                widths[idx],
                numeric_cols.get(idx).copied().unwrap_or(false),
            )
        })
        .collect::<Vec<_>>()
        .join(sep);
    println!("{}", header);

    let dash_join = if sep == " | " { "-+-" } else { "--" };
    let separator: String = widths[..visible_count]
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join(dash_join);
    println!("{}", separator);

    for row in &rendered_rows {
        let line: String = row[..visible_count.min(row.len())]
            .iter()
            .enumerate()
            .map(|(idx, cell)| {
                let ra = numeric_cols.get(idx).copied().unwrap_or(false);
                pad_cell(&truncate_cell(cell, widths[idx]), widths[idx], ra)
            })
            .collect::<Vec<_>>()
            .join(sep);
        println!("{}", line);
    }

    if visible_count < table.columns.len() {
        println!(
            "({} rows, {} cols hidden — widen terminal or use --metric-limit)",
            table.rows.len(),
            table.columns.len() - visible_count
        );
    } else {
        println!("({} rows)", table.rows.len());
    }
}

fn fit_columns_to_width(
    natural_widths: &[usize],
    core_count: usize,
    term_width: usize,
) -> (usize, &'static str, Vec<usize>) {
    let n = natural_widths.len();
    if n == 0 {
        return (0, " | ", Vec::new());
    }

    for sep in [" | ", "  "] {
        let sep_w = sep.len();
        let min_visible = core_count.min(n);
        for visible in (min_visible..=n).rev() {
            let sep_total = if visible > 1 {
                (visible - 1) * sep_w
            } else {
                0
            };
            let avail_for_cols = term_width.saturating_sub(sep_total);
            if avail_for_cols < visible {
                continue; // not even 1 char per column
            }
            let widths = cap_widths(&natural_widths[..visible], avail_for_cols);
            let total: usize = widths.iter().sum::<usize>() + sep_total;
            if total <= term_width {
                return (visible, sep, widths);
            }
        }
    }

    let visible = core_count.min(n).max(1);
    let sep = "  ";
    let sep_total = if visible > 1 { (visible - 1) * 2 } else { 0 };
    let avail = term_width.saturating_sub(sep_total);
    let widths = cap_widths(&natural_widths[..visible], avail);
    (visible, sep, widths)
}

fn cap_widths(natural: &[usize], budget: usize) -> Vec<usize> {
    let n = natural.len();
    if n == 0 {
        return Vec::new();
    }
    let total: usize = natural.iter().sum();
    if total <= budget {
        return natural.to_vec();
    }
    let min_w = 4_usize;
    let mut lo = min_w;
    let mut hi = *natural.iter().max().unwrap_or(&budget);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let used: usize = natural.iter().map(|&w| w.min(mid)).sum();
        if used <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    natural.iter().map(|&w| w.min(lo).max(min_w)).collect()
}

fn read_run_status(run_dir: &Path) -> String {
    summarize_run_lifecycle(
        load_run_control(run_dir).as_ref(),
        load_engine_lease(run_dir).as_ref(),
        Utc::now(),
    )
    .status_display
}

fn read_run_progress(run_dir: &Path) -> Option<(usize, usize)> {
    let value = load_runtime_value(run_dir, lab_runner::schedule_progress_record_key())?;
    let total = value.get("total_slots")?.as_u64()? as usize;
    let completed = value.get("completed_slots")?.as_array()?.len();
    if total == 0 {
        return None;
    }
    Some((completed, total))
}

fn build_state_run_progress_table(run_dir: &Path) -> analysis::QueryTable {
    let progress = load_runtime_value(run_dir, lab_runner::schedule_progress_record_key());
    let control_raw = load_run_control(run_dir);
    let control = summarize_run_lifecycle(
        control_raw.as_ref(),
        load_engine_lease(run_dir).as_ref(),
        Utc::now(),
    );
    let completed = progress
        .as_ref()
        .and_then(|value| value.get("completed_slots"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let total = progress
        .as_ref()
        .and_then(|value| value.get("total_slots"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let schedule = progress
        .as_ref()
        .and_then(|value| value.get("schedule"))
        .and_then(Value::as_array);
    let variants_seen = schedule
        .map(|items| {
            items
                .iter()
                .filter_map(|slot| slot.get("variant_idx").and_then(Value::as_u64))
                .collect::<BTreeSet<_>>()
                .len()
        })
        .unwrap_or(0);
    let tasks_seen = schedule
        .map(|items| {
            items
                .iter()
                .filter_map(|slot| slot.get("task_idx").and_then(Value::as_u64))
                .collect::<BTreeSet<_>>()
                .len()
        })
        .unwrap_or(0);
    let updated_at = progress
        .as_ref()
        .and_then(|value| value.get("updated_at"))
        .and_then(Value::as_str)
        .or_else(|| {
            control_raw
                .as_ref()
                .and_then(|value| value.get("updated_at"))
                .and_then(Value::as_str)
        })
        .unwrap_or("")
        .to_string();

    analysis::QueryTable {
        columns: vec![
            "run_id".to_string(),
            "status".to_string(),
            "completed_trials".to_string(),
            "active_trials".to_string(),
            "total_trials".to_string(),
            "variants_seen".to_string(),
            "tasks_seen".to_string(),
            "pass_rate".to_string(),
            "updated_at".to_string(),
        ],
        rows: vec![vec![
            Value::String(run_id_from_dir(run_dir).unwrap_or_default()),
            Value::String(control.status_display),
            json!(completed),
            json!(control.active_trials),
            json!(total),
            json!(variants_seen),
            json!(tasks_seen),
            Value::Null,
            Value::String(updated_at),
        ]],
    }
}

fn build_state_contract_health_table(run_dir: &Path) -> analysis::QueryTable {
    let (completed, total) = read_run_progress(run_dir).unwrap_or((0, 0));
    analysis::QueryTable {
        columns: vec![
            "completed_trials".to_string(),
            "total_trials".to_string(),
            "trusted_scores".to_string(),
            "untrusted_scores".to_string(),
            "warning_trials".to_string(),
            "error_trials".to_string(),
            "empty_predictions".to_string(),
            "grader_or_mapping_errors".to_string(),
            "source".to_string(),
        ],
        rows: vec![vec![
            json!(completed),
            json!(total),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::String("runtime_state".to_string()),
        ]],
    }
}

fn stdout_is_tty() -> bool {
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

fn unix_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn prepare_trace_split_view(
    table: &analysis::QueryTable,
) -> (
    analysis::QueryTable,
    Vec<(String, String)>,
    (String, String),
) {
    let idx = |name: &str| table.columns.iter().position(|c| c == name);
    let get = |row: &[Value], col: Option<usize>| -> Value {
        col.and_then(|i| row.get(i)).cloned().unwrap_or(Value::Null)
    };
    let task_id = idx("task_id");
    let a_id = idx("variant_a_id");
    let b_id = idx("variant_b_id");
    let a_event = idx("variant_a_event_type");
    let b_event = idx("variant_b_event_type");
    let a_turn = idx("variant_a_turn_index");
    let b_turn = idx("variant_b_turn_index");
    let a_tool = idx("variant_a_tool");
    let b_tool = idx("variant_b_tool");
    let a_status = idx("variant_a_status");
    let b_status = idx("variant_b_status");

    let (left_label, right_label) = table
        .rows
        .first()
        .map(|first| {
            let a = a_id
                .and_then(|i| first.get(i))
                .and_then(Value::as_str)
                .unwrap_or("variant a")
                .to_string();
            let b = b_id
                .and_then(|i| first.get(i))
                .and_then(Value::as_str)
                .unwrap_or("variant b")
                .to_string();
            (a, b)
        })
        .unwrap_or_else(|| ("variant a".into(), "variant b".into()));

    let to_dot = |row: &[Value], col: Option<usize>| -> Value {
        match col
            .and_then(|i| row.get(i))
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "" => Value::Null,
            s if s.contains("success") || s == "ok" || s.starts_with('2') || s == "pass" => {
                Value::String("●".to_string())
            }
            _ => Value::String("✗".to_string()),
        }
    };

    let columns = vec![
        "task".into(),
        "event".into(),
        "turn".into(),
        "tool".into(),
        "st".into(),
        "┃".into(),
        "event".into(),
        "turn".into(),
        "tool".into(),
        "st".into(),
    ];

    let rows: Vec<Vec<Value>> = table
        .rows
        .iter()
        .map(|row| {
            vec![
                get(row, task_id),
                get(row, a_event),
                get(row, a_turn),
                get(row, a_tool),
                to_dot(row, a_status),
                Value::String("┃".to_string()),
                get(row, b_event),
                get(row, b_turn),
                get(row, b_tool),
                to_dot(row, b_status),
            ]
        })
        .collect();

    let compact = analysis::QueryTable { columns, rows };

    let task_col = 0;
    let task_is_constant = compact.rows.len() > 1 && {
        let first = compact.rows[0].get(task_col);
        compact.rows.iter().all(|r| r.get(task_col) == first)
    };

    let (filtered, legend) = if task_is_constant {
        let val = compact.rows[0]
            .get(task_col)
            .map(render_json_cell)
            .unwrap_or_default();
        let legend = vec![("task".to_string(), val)];
        let columns = compact.columns[1..].to_vec();
        let rows = compact
            .rows
            .into_iter()
            .map(|mut r| {
                r.remove(0);
                r
            })
            .collect();
        (analysis::QueryTable { columns, rows }, legend)
    } else {
        (compact, Vec::new())
    };

    (filtered, legend, (left_label, right_label))
}

fn has_ab_trace_columns(table: &analysis::QueryTable) -> bool {
    let has = |name: &str| table.columns.iter().any(|c| c == name);
    has("variant_a_event_type") && has("variant_b_event_type")
}

fn display_mode_for_view(resolved: &ResolvedView) -> tui::DisplayMode {
    match renderer_for_resolved(resolved) {
        ViewRenderer::Overview => tui::DisplayMode::Overview,
        ViewRenderer::Timeline => tui::DisplayMode::Timeline,
        ViewRenderer::Comparison => tui::DisplayMode::Comparison,
        ViewRenderer::Scoreboard => tui::DisplayMode::Scoreboard,
        ViewRenderer::Table => tui::DisplayMode::Table,
    }
}

fn display_column_name(name: &str) -> String {
    let mapped = match name {
        "variant_id" => "variant",
        "task_id" => "task",
        "trial_id" => "trial",
        "experiment_id" => "experiment",
        "baseline_id" => "baseline",
        "primary_metric_mean" => "metric",
        "primary_metric_value" => "metric_val",
        "primary_metric_name" => "metric_name",
        "success_rate" => "pass%",
        "pass_rate" => "pass%",
        "n_trials" => "trials",
        "trial_count" => "trials",
        "variant_count" => "variants",
        "task_count" => "tasks",
        "active_trials" => "active",
        "completed_trials" => "done",
        "total_trials" => "total",
        "event_type" => "event",
        "turn_number" => "turn",
        "tool_name" => "tool",
        "status_code" => "status",
        "error_message" => "error",
        "metric_name" => "metric",
        "metric_value" => "value",
        "started_at" => "started",
        "completed_at" => "completed",
        "updated_at" => "updated",
        "duration_seconds" => "dur_s",
        "worker_id" => "worker",
        "win_rate" => "win%",
        "loss_rate" => "loss%",
        "tie_rate" => "tie%",
        "effect_size" => "effect",
        "mcnemar_p" => "p_val",
        "outcome" => "outcome",
        _ => "",
    };
    if !mapped.is_empty() {
        return mapped.to_string();
    }

    if let Some(rest) = name.strip_prefix("variant_a_") {
        return format!("a_{rest}");
    }
    if let Some(rest) = name.strip_prefix("variant_b_") {
        return format!("b_{rest}");
    }
    if let Some(rest) = name.strip_prefix("delta_") {
        return format!("d_{rest}");
    }

    if let Some(rest) = name.strip_suffix("_count") {
        return format!("{rest}s");
    }

    name.to_string()
}

fn shorten_display_columns(table: &analysis::QueryTable) -> analysis::QueryTable {
    analysis::QueryTable {
        columns: table
            .columns
            .iter()
            .map(|c| display_column_name(c))
            .collect(),
        rows: table.rows.clone(),
    }
}

fn query_table_to_json(table: &analysis::QueryTable) -> Value {
    let mut objects = Vec::with_capacity(table.rows.len());
    for row in &table.rows {
        let mut obj = serde_json::Map::new();
        for (idx, column) in table.columns.iter().enumerate() {
            obj.insert(column.clone(), row.get(idx).cloned().unwrap_or(Value::Null));
        }
        objects.push(Value::Object(obj));
    }
    json!({
        "columns": table.columns,
        "rows": objects,
        "row_count": table.rows.len()
    })
}

fn elide_constant_columns(
    table: &analysis::QueryTable,
) -> (analysis::QueryTable, Vec<(String, String)>) {
    if table.rows.len() <= 1 || table.columns.len() <= 1 {
        return (table.clone(), Vec::new());
    }

    let mut elided = Vec::new();
    let mut keep_indices = Vec::new();

    for (col_idx, col_name) in table.columns.iter().enumerate() {
        let first_val = table
            .rows
            .first()
            .and_then(|row| row.get(col_idx))
            .cloned()
            .unwrap_or(Value::Null);

        let all_same = table
            .rows
            .iter()
            .all(|row| row.get(col_idx).cloned().unwrap_or(Value::Null) == first_val);

        if all_same {
            elided.push((col_name.clone(), render_json_cell(&first_val)));
        } else {
            keep_indices.push(col_idx);
        }
    }

    if elided.is_empty() {
        return (table.clone(), Vec::new());
    }

    let new_columns: Vec<String> = keep_indices
        .iter()
        .map(|&idx| table.columns[idx].clone())
        .collect();
    let new_rows: Vec<Vec<Value>> = table
        .rows
        .iter()
        .map(|row| {
            keep_indices
                .iter()
                .map(|&idx| row.get(idx).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();

    (
        analysis::QueryTable {
            columns: new_columns,
            rows: new_rows,
        },
        elided,
    )
}

fn print_query_table(table: &analysis::QueryTable) {
    if table.columns.is_empty() {
        println!("(ok)");
        return;
    }

    let (filtered, elided) = elide_constant_columns(table);
    let display = shorten_display_columns(&filtered);

    if !elided.is_empty() {
        let meta_parts: Vec<String> = elided
            .iter()
            .map(|(k, v)| {
                let display_v = truncate_cell(v, 40);
                format!("{}={}", display_column_name(k), display_v)
            })
            .collect();
        println!("{}", meta_parts.join("  "));
        println!();
    }

    let term_w = terminal_width();
    if should_chunk_query_table(&display, term_w) {
        print_query_table_in_column_chunks(&display, term_w);
    } else {
        print_scoreboard_table(&display, term_w);
    }
}

fn print_special_split_view(
    _run_dir: &Path,
    view_name: &str,
    table: &analysis::QueryTable,
) -> bool {
    match view_name {
        "task_outcomes" | "ab_task_outcomes" => {
            print_ab_task_outcomes_table(table);
            true
        }
        "trace" | "trace_compare" | "ab_trace_row_side_by_side" => {
            print_trace_compare_by_task(table);
            true
        }
        "turn_compare" | "ab_turn_side_by_side" => {
            print_variant_prefixed_tables(
                table,
                &["task_id", "repl_idx", "turn_index"],
                "variant_a_",
                "variant_b_",
                "variant_a_turns",
                "variant_b_turns",
            );
            true
        }
        _ => false,
    }
}

fn print_ab_task_outcomes_table(table: &analysis::QueryTable) {
    let ordered = project_query_table_by_column_priority(
        table,
        &[
            "task_id",
            "variant_a_outcome",
            "a_outcome",
            "variant_b_outcome",
            "b_outcome",
            "variant_a_result_score",
            "a_result_score",
            "variant_b_result_score",
            "b_result_score",
            "variant_a_trial_id",
            "a_trial_id",
            "variant_b_trial_id",
            "b_trial_id",
            "delta_result_score",
            "d_result_score",
            "outcome_change",
            "repl_idx",
            "variant_a_id",
            "a_variant_id",
            "variant_b_id",
            "b_variant_id",
        ],
    );
    print_query_table_no_elision(&ordered);
}

#[derive(Clone, Debug)]
struct TraceSection {
    task_id: String,
    repl_idx: String,
    variant_a_id: String,
    variant_b_id: String,
    variant_a_trial_id: String,
    variant_b_trial_id: String,
    variant_a_table: analysis::QueryTable,
    variant_b_table: analysis::QueryTable,
}

fn first_non_null_column_value(table: &analysis::QueryTable, column_name: &str) -> String {
    let Some(idx) = table.columns.iter().position(|c| c == column_name) else {
        return String::new();
    };
    for row in &table.rows {
        let value = row.get(idx).unwrap_or(&Value::Null);
        match value {
            Value::Null => {}
            Value::String(s) if s.trim().is_empty() => {}
            Value::String(s) => return s.to_string(),
            other => return render_json_cell(other),
        }
    }
    String::new()
}

fn build_trace_side_table(table: &analysis::QueryTable, prefix: &str) -> analysis::QueryTable {
    let desired = vec![
        ("row_seq".to_string(), "row"),
        (format!("{}event_type", prefix), "evt"),
        (format!("{}turn_index", prefix), "turn"),
        (format!("{}model", prefix), "model"),
        (format!("{}tool", prefix), "tool"),
        (format!("{}status", prefix), "st"),
        (format!("{}call_id", prefix), "call"),
    ];
    let mut indices = Vec::new();
    let mut columns = Vec::new();
    let mut event_idx_in_projection = None;
    for (column_name, short_name) in desired {
        if let Some(idx) = table.columns.iter().position(|c| c == &column_name) {
            if !indices.contains(&idx) {
                if short_name == "evt" {
                    event_idx_in_projection = Some(indices.len());
                }
                indices.push(idx);
                columns.push(short_name.to_string());
            }
        }
    }
    let rows = table
        .rows
        .iter()
        .map(|row| {
            indices
                .iter()
                .map(|idx| row.get(*idx).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()
        })
        .filter(|projected| {
            match event_idx_in_projection
                .and_then(|idx| projected.get(idx))
                .unwrap_or(&Value::Null)
            {
                Value::Null => false,
                Value::String(s) if s.trim().is_empty() => false,
                _ => true,
            }
        })
        .collect::<Vec<_>>();
    analysis::QueryTable { columns, rows }
}

fn build_trace_sections(table: &analysis::QueryTable) -> Vec<TraceSection> {
    let task_col = table.columns.iter().position(|c| c == "task_id");
    let repl_col = table.columns.iter().position(|c| c == "repl_idx");
    let (Some(task_col), Some(repl_col)) = (task_col, repl_col) else {
        return Vec::new();
    };

    let mut grouped: BTreeMap<(String, String), Vec<Vec<Value>>> = BTreeMap::new();
    for row in &table.rows {
        let task = row
            .get(task_col)
            .map(render_json_cell)
            .unwrap_or_else(|| "unknown".to_string());
        let repl = row
            .get(repl_col)
            .map(render_json_cell)
            .unwrap_or_else(|| "unknown".to_string());
        grouped.entry((task, repl)).or_default().push(row.clone());
    }

    grouped
        .into_iter()
        .map(|((task_id, repl_idx), rows)| {
            let grouped_table = analysis::QueryTable {
                columns: table.columns.clone(),
                rows,
            };
            TraceSection {
                task_id,
                repl_idx,
                variant_a_id: first_non_null_column_value(&grouped_table, "variant_a_id"),
                variant_b_id: first_non_null_column_value(&grouped_table, "variant_b_id"),
                variant_a_trial_id: first_non_null_column_value(
                    &grouped_table,
                    "variant_a_trial_id",
                ),
                variant_b_trial_id: first_non_null_column_value(
                    &grouped_table,
                    "variant_b_trial_id",
                ),
                variant_a_table: build_trace_side_table(&grouped_table, "variant_a_"),
                variant_b_table: build_trace_side_table(&grouped_table, "variant_b_"),
            }
        })
        .collect()
}

fn print_trace_compare_by_task(table: &analysis::QueryTable) {
    let sections = build_trace_sections(table);
    if sections.is_empty() {
        print_query_table(table);
        return;
    }

    for section in sections {
        println!("== task={} repl={} ==", section.task_id, section.repl_idx);
        if !section.variant_a_id.is_empty() || !section.variant_a_trial_id.is_empty() {
            println!(
                "variant_a: {}  trial: {}",
                if section.variant_a_id.is_empty() {
                    "unknown"
                } else {
                    section.variant_a_id.as_str()
                },
                if section.variant_a_trial_id.is_empty() {
                    "unknown"
                } else {
                    section.variant_a_trial_id.as_str()
                }
            );
        }
        if !section.variant_b_id.is_empty() || !section.variant_b_trial_id.is_empty() {
            println!(
                "variant_b: {}  trial: {}",
                if section.variant_b_id.is_empty() {
                    "unknown"
                } else {
                    section.variant_b_id.as_str()
                },
                if section.variant_b_trial_id.is_empty() {
                    "unknown"
                } else {
                    section.variant_b_trial_id.as_str()
                }
            );
        }
        println!();
        println!("-- variant_a --");
        print_query_table(&section.variant_a_table);
        println!();
        println!("-- variant_b --");
        print_query_table(&section.variant_b_table);
        println!();
    }
}

fn print_query_table_no_elision(table: &analysis::QueryTable) {
    if table.columns.is_empty() {
        println!("(ok)");
        return;
    }
    let term_w = terminal_width();
    if should_chunk_query_table(table, term_w) {
        print_query_table_in_column_chunks(table, term_w);
    } else {
        print_scoreboard_table(table, term_w);
    }
}

fn project_query_table_by_column_priority(
    table: &analysis::QueryTable,
    priority_cols: &[&str],
) -> analysis::QueryTable {
    let mut indices = Vec::new();
    for name in priority_cols {
        if let Some(idx) = table.columns.iter().position(|col| col == name) {
            if !indices.contains(&idx) {
                indices.push(idx);
            }
        }
    }
    for idx in 0..table.columns.len() {
        if !indices.contains(&idx) {
            indices.push(idx);
        }
    }
    project_query_table_columns(table, &indices)
}

fn print_variant_prefixed_tables(
    table: &analysis::QueryTable,
    shared_priority_cols: &[&str],
    left_prefix: &str,
    right_prefix: &str,
    left_title: &str,
    right_title: &str,
) {
    let mut shared_indices = Vec::new();
    for &name in shared_priority_cols {
        if let Some(idx) = table.columns.iter().position(|col| col == name) {
            if !shared_indices.contains(&idx) {
                shared_indices.push(idx);
            }
        }
    }

    let left_indices: Vec<usize> = table
        .columns
        .iter()
        .enumerate()
        .filter_map(|(idx, col)| col.starts_with(left_prefix).then_some(idx))
        .collect();
    let right_indices: Vec<usize> = table
        .columns
        .iter()
        .enumerate()
        .filter_map(|(idx, col)| col.starts_with(right_prefix).then_some(idx))
        .collect();

    if left_indices.is_empty() || right_indices.is_empty() {
        print_query_table(table);
        return;
    }

    let left_table = project_query_table_columns_with_prefix_trim(
        table,
        &shared_indices,
        &left_indices,
        left_prefix,
    );
    let right_table = project_query_table_columns_with_prefix_trim(
        table,
        &shared_indices,
        &right_indices,
        right_prefix,
    );

    println!("== {} ==", left_title);
    print_query_table(&left_table);
    println!();
    println!("== {} ==", right_title);
    print_query_table(&right_table);
}

fn project_query_table_columns_with_prefix_trim(
    table: &analysis::QueryTable,
    shared_indices: &[usize],
    side_indices: &[usize],
    side_prefix: &str,
) -> analysis::QueryTable {
    let mut combined_indices = shared_indices.to_vec();
    combined_indices.extend(side_indices.iter().copied());

    let columns = combined_indices
        .iter()
        .filter_map(|idx| table.columns.get(*idx))
        .map(|col| {
            col.strip_prefix(side_prefix)
                .map(str::to_string)
                .unwrap_or_else(|| col.clone())
        })
        .collect::<Vec<_>>();

    let rows = table
        .rows
        .iter()
        .map(|row| {
            combined_indices
                .iter()
                .map(|idx| row.get(*idx).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    analysis::QueryTable { columns, rows }
}

fn should_chunk_query_table(table: &analysis::QueryTable, term_w: usize) -> bool {
    let col_count = table.columns.len();
    if col_count <= 12 {
        return false;
    }
    let min_required = col_count.saturating_mul(6) + col_count.saturating_sub(1).saturating_mul(2);
    min_required > term_w || col_count > 18
}

fn print_query_table_in_column_chunks(table: &analysis::QueryTable, term_w: usize) {
    let anchor_indices = choose_query_table_anchor_indices(&table.columns);
    let mut is_anchor = vec![false; table.columns.len()];
    for idx in &anchor_indices {
        if *idx < is_anchor.len() {
            is_anchor[*idx] = true;
        }
    }
    let trailing_indices: Vec<usize> = (0..table.columns.len())
        .filter(|idx| !is_anchor[*idx])
        .collect();

    let anchor_count = anchor_indices.len().max(1);
    let base_max_total_cols = if term_w < 120 {
        4
    } else if term_w < 170 {
        5
    } else {
        6
    };
    let max_cols_for_readable = base_max_total_cols.max(anchor_count + 1);
    let chunk_payload_cols = max_cols_for_readable.saturating_sub(anchor_count).max(1);
    let total_chunks = trailing_indices.len().div_ceil(chunk_payload_cols).max(1);

    if trailing_indices.is_empty() {
        print_scoreboard_table(table, term_w);
        return;
    }

    for (chunk_idx, payload_chunk) in trailing_indices.chunks(chunk_payload_cols).enumerate() {
        if chunk_idx > 0 {
            println!();
        }
        let mut selected_indices = anchor_indices.clone();
        selected_indices.extend(payload_chunk.iter().copied());
        selected_indices.sort_unstable();

        let projected = project_query_table_columns(table, &selected_indices);
        println!("-- column chunk {}/{} --", chunk_idx + 1, total_chunks);
        print_scoreboard_table(&projected, term_w);
    }
}

fn choose_query_table_anchor_indices(columns: &[String]) -> Vec<usize> {
    let mut out = Vec::new();
    let priorities = [
        "task_id",
        "repl_idx",
        "turn_index",
        "row_seq",
        "variant_a_id",
        "variant_b_id",
        "variant_a_trial_id",
        "variant_b_trial_id",
        "trial_id",
        "variant_id",
    ];
    for name in priorities {
        if out.len() >= 3 {
            break;
        }
        if let Some(idx) = columns.iter().position(|col| col == name) {
            if !out.contains(&idx) {
                out.push(idx);
            }
        }
    }
    if out.is_empty() {
        out.push(0);
        if columns.len() > 1 {
            out.push(1);
        }
    }
    out
}

fn project_query_table_columns(
    table: &analysis::QueryTable,
    indices: &[usize],
) -> analysis::QueryTable {
    let columns = indices
        .iter()
        .filter_map(|idx| table.columns.get(*idx).cloned())
        .collect::<Vec<_>>();
    let rows = table
        .rows
        .iter()
        .map(|row| {
            indices
                .iter()
                .map(|idx| row.get(*idx).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    analysis::QueryTable { columns, rows }
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn print_query_table_csv(table: &analysis::QueryTable) {
    let header = table
        .columns
        .iter()
        .map(|c| csv_escape(c))
        .collect::<Vec<_>>()
        .join(",");
    println!("{}", header);
    for row in &table.rows {
        let line = row
            .iter()
            .map(|v| csv_escape(&render_json_cell(v)))
            .collect::<Vec<_>>()
            .join(",");
        println!("{}", line);
    }
}

fn markdown_escape_cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\r', "")
        .replace('\n', "<br>")
}

fn render_query_table_markdown(table: &analysis::QueryTable) -> String {
    if table.columns.is_empty() {
        return "(ok)".to_string();
    }
    let header = format!(
        "| {} |",
        table
            .columns
            .iter()
            .map(|col| markdown_escape_cell(col))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let separator = format!(
        "| {} |",
        table
            .columns
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let mut lines = vec![header, separator];
    for row in &table.rows {
        let cells = table
            .columns
            .iter()
            .enumerate()
            .map(|(idx, _)| {
                let value = row.get(idx).unwrap_or(&Value::Null);
                markdown_escape_cell(&render_json_cell(value))
            })
            .collect::<Vec<_>>();
        lines.push(format!("| {} |", cells.join(" | ")));
    }
    lines.join("\n")
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn render_query_table_html_fragment(table: &analysis::QueryTable) -> String {
    if table.columns.is_empty() {
        return "<p>(ok)</p>".to_string();
    }
    let mut out = String::new();
    out.push_str("<table><thead><tr>");
    for col in &table.columns {
        out.push_str("<th>");
        out.push_str(&html_escape(col));
        out.push_str("</th>");
    }
    out.push_str("</tr></thead><tbody>");
    for row in &table.rows {
        out.push_str("<tr>");
        for (idx, _) in table.columns.iter().enumerate() {
            let value = row.get(idx).unwrap_or(&Value::Null);
            out.push_str("<td>");
            out.push_str(&html_escape(&render_json_cell(value)));
            out.push_str("</td>");
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>");
    out
}

fn print_table_markdown(table: &analysis::QueryTable) {
    println!("{}", render_query_table_markdown(table));
}

fn print_table_html_document(title: &str, table: &analysis::QueryTable) {
    println!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>body{{font-family:ui-monospace,SFMono-Regular,Menlo,Monaco,Consolas,monospace;padding:20px;line-height:1.4}}table{{border-collapse:collapse;width:100%}}th,td{{border:1px solid #bbb;padding:6px 8px;text-align:left;vertical-align:top}}th{{background:#f5f5f5;position:sticky;top:0}}tr:nth-child(even) td{{background:#fafafa}}</style></head><body><h1>{}</h1>{}</body></html>",
        html_escape(title),
        html_escape(title),
        render_query_table_html_fragment(table)
    );
}

fn is_trace_view(resolved: &ResolvedView) -> bool {
    if resolved.name == "trace" || resolved.name == "trace_compare" {
        return true;
    }
    matches!(
        resolved.source.as_deref(),
        Some("ab_trace_row_side_by_side")
    )
}

fn render_trace_sections_markdown(table: &analysis::QueryTable) -> Option<String> {
    let sections = build_trace_sections(table);
    if sections.is_empty() {
        return None;
    }
    let mut out = String::new();
    for section in sections {
        out.push_str(&format!(
            "### task `{}` repl `{}`\n\n",
            markdown_escape_cell(&section.task_id),
            markdown_escape_cell(&section.repl_idx)
        ));
        out.push_str(&format!(
            "- variant_a: `{}`  trial: `{}`\n",
            markdown_escape_cell(if section.variant_a_id.is_empty() {
                "unknown"
            } else {
                section.variant_a_id.as_str()
            }),
            markdown_escape_cell(if section.variant_a_trial_id.is_empty() {
                "unknown"
            } else {
                section.variant_a_trial_id.as_str()
            })
        ));
        out.push_str(&format!(
            "- variant_b: `{}`  trial: `{}`\n\n",
            markdown_escape_cell(if section.variant_b_id.is_empty() {
                "unknown"
            } else {
                section.variant_b_id.as_str()
            }),
            markdown_escape_cell(if section.variant_b_trial_id.is_empty() {
                "unknown"
            } else {
                section.variant_b_trial_id.as_str()
            })
        ));
        out.push_str("\n#### variant_a\n\n");
        out.push_str(&render_query_table_markdown(&section.variant_a_table));
        out.push_str("\n\n#### variant_b\n\n");
        out.push_str(&render_query_table_markdown(&section.variant_b_table));
        out.push_str("\n\n");
    }
    Some(out)
}

fn render_trace_sections_html(table: &analysis::QueryTable) -> Option<String> {
    let sections = build_trace_sections(table);
    if sections.is_empty() {
        return None;
    }
    let mut out = String::new();
    for section in sections {
        out.push_str("<section class=\"trace-task\">");
        out.push_str("<h3>task <code>");
        out.push_str(&html_escape(&section.task_id));
        out.push_str("</code> repl <code>");
        out.push_str(&html_escape(&section.repl_idx));
        out.push_str("</code></h3>");
        out.push_str("<p><strong>variant_a:</strong> <code>");
        out.push_str(&html_escape(if section.variant_a_id.is_empty() {
            "unknown"
        } else {
            section.variant_a_id.as_str()
        }));
        out.push_str("</code> <strong>trial:</strong> <code>");
        out.push_str(&html_escape(if section.variant_a_trial_id.is_empty() {
            "unknown"
        } else {
            section.variant_a_trial_id.as_str()
        }));
        out.push_str("</code></p>");
        out.push_str("<p><strong>variant_b:</strong> <code>");
        out.push_str(&html_escape(if section.variant_b_id.is_empty() {
            "unknown"
        } else {
            section.variant_b_id.as_str()
        }));
        out.push_str("</code> <strong>trial:</strong> <code>");
        out.push_str(&html_escape(if section.variant_b_trial_id.is_empty() {
            "unknown"
        } else {
            section.variant_b_trial_id.as_str()
        }));
        out.push_str("</code></p>");
        out.push_str("<div class=\"trace-grid\"><div><h4>variant_a</h4>");
        out.push_str(&render_query_table_html_fragment(&section.variant_a_table));
        out.push_str("</div><div><h4>variant_b</h4>");
        out.push_str(&render_query_table_html_fragment(&section.variant_b_table));
        out.push_str("</div></div></section>");
    }
    Some(out)
}

fn print_single_view_markdown(
    run_dir: &Path,
    view_set: &str,
    resolved: &ResolvedView,
    table: &analysis::QueryTable,
) {
    println!("# Bucephalus view");
    println!();
    println!("run_dir: `{}`", run_dir.display());
    println!();
    println!("view_set: `{}`", view_set);
    println!();
    println!("view: `{}`", resolved.name);
    if let Some(source) = resolved.source.as_deref() {
        if source != resolved.name {
            println!();
            println!("source_view: `{}`", source);
        }
    }
    println!();
    if is_trace_view(resolved) {
        if let Some(rendered) = render_trace_sections_markdown(table) {
            println!("{}", rendered.trim_end());
            return;
        }
    }
    println!("{}", render_query_table_markdown(table));
}

fn print_single_view_html(
    run_dir: &Path,
    view_set: &str,
    resolved: &ResolvedView,
    table: &analysis::QueryTable,
) {
    let mut out = String::new();
    out.push_str(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Bucephalus view</title><style>body{font-family:ui-monospace,SFMono-Regular,Menlo,Monaco,Consolas,monospace;padding:20px;line-height:1.4}table{border-collapse:collapse;width:100%}th,td{border:1px solid #bbb;padding:6px 8px;text-align:left;vertical-align:top}th{background:#f5f5f5;position:sticky;top:0}tr:nth-child(even) td{background:#fafafa}code{background:#f3f3f3;padding:1px 4px;border-radius:4px}.trace-grid{display:grid;grid-template-columns:1fr 1fr;gap:14px;align-items:start}.trace-task{margin-top:20px;padding-top:4px;border-top:1px solid #ddd}</style></head><body>",
    );
    out.push_str("<h1>Bucephalus view</h1>");
    out.push_str("<p><strong>run_dir:</strong> <code>");
    out.push_str(&html_escape(&run_dir.display().to_string()));
    out.push_str("</code></p>");
    out.push_str("<p><strong>view_set:</strong> <code>");
    out.push_str(&html_escape(view_set));
    out.push_str("</code></p>");
    out.push_str("<p><strong>view:</strong> <code>");
    out.push_str(&html_escape(&resolved.name));
    out.push_str("</code></p>");
    if let Some(source) = resolved.source.as_deref() {
        if source != resolved.name {
            out.push_str("<p><strong>source_view:</strong> <code>");
            out.push_str(&html_escape(source));
            out.push_str("</code></p>");
        }
    }
    if is_trace_view(resolved) {
        if let Some(rendered) = render_trace_sections_html(table) {
            out.push_str(&rendered);
        } else {
            out.push_str(&render_query_table_html_fragment(table));
        }
    } else {
        out.push_str(&render_query_table_html_fragment(table));
    }
    out.push_str("</body></html>");
    println!("{}", out);
}

fn print_views_markdown_document(
    run_dir: &Path,
    view_set: &str,
    rendered: &[(ResolvedView, analysis::QueryTable)],
) {
    println!("# Bucephalus views");
    println!();
    println!("run_dir: `{}`", run_dir.display());
    println!();
    println!("view_set: `{}`", view_set);
    for (resolved, table) in rendered {
        println!();
        println!("## {}", resolved.name);
        if let Some(source) = resolved.source.as_deref() {
            if source != resolved.name {
                println!();
                println!("source_view: `{}`", source);
            }
        }
        println!();
        if is_trace_view(resolved) {
            if let Some(rendered) = render_trace_sections_markdown(table) {
                println!("{}", rendered.trim_end());
                continue;
            }
        }
        println!("{}", render_query_table_markdown(table));
    }
}

fn print_views_html_document(
    run_dir: &Path,
    view_set: &str,
    rendered: &[(ResolvedView, analysis::QueryTable)],
) {
    let mut out = String::new();
    out.push_str(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Bucephalus views</title><style>body{font-family:ui-monospace,SFMono-Regular,Menlo,Monaco,Consolas,monospace;padding:20px;line-height:1.4}table{border-collapse:collapse;width:100%;margin-bottom:26px}th,td{border:1px solid #bbb;padding:6px 8px;text-align:left;vertical-align:top}th{background:#f5f5f5;position:sticky;top:0}tr:nth-child(even) td{background:#fafafa}code{background:#f3f3f3;padding:1px 4px;border-radius:4px}h2{margin-top:32px}.trace-grid{display:grid;grid-template-columns:1fr 1fr;gap:14px;align-items:start}.trace-task{margin-top:20px;padding-top:4px;border-top:1px solid #ddd}</style></head><body>",
    );
    out.push_str("<h1>Bucephalus views</h1>");
    out.push_str("<p><strong>run_dir:</strong> <code>");
    out.push_str(&html_escape(&run_dir.display().to_string()));
    out.push_str("</code></p>");
    out.push_str("<p><strong>view_set:</strong> <code>");
    out.push_str(&html_escape(view_set));
    out.push_str("</code></p>");
    for (resolved, table) in rendered {
        out.push_str("<h2>");
        out.push_str(&html_escape(&resolved.name));
        out.push_str("</h2>");
        if let Some(source) = resolved.source.as_deref() {
            if source != resolved.name {
                out.push_str("<p><strong>source_view:</strong> <code>");
                out.push_str(&html_escape(source));
                out.push_str("</code></p>");
            }
        }
        if is_trace_view(resolved) {
            if let Some(rendered) = render_trace_sections_html(table) {
                out.push_str(&rendered);
            } else {
                out.push_str(&render_query_table_html_fragment(table));
            }
        } else {
            out.push_str(&render_query_table_html_fragment(table));
        }
    }
    out.push_str("</body></html>");
    println!("{}", out);
}

fn render_json_cell(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => v.clone(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn truncate_cell(value: &str, width: usize) -> String {
    let value_len = value.chars().count();
    if value_len <= width {
        return value.to_string();
    }
    if width <= 1 {
        return ".".to_string();
    }
    let mut out: String = value.chars().take(width - 1).collect();
    out.push('.');
    out
}

fn pad_cell(value: &str, width: usize, right_align: bool) -> String {
    let value_len = value.chars().count();
    if value_len >= width {
        value.to_string()
    } else if right_align {
        format!("{:padding$}{value}", "", padding = width - value_len)
    } else {
        format!("{value}{:padding$}", "", padding = width - value_len)
    }
}

fn try_print_post_run_stats(run_dir: &Path, run_id: &str) {
    let Some((view_set, table)) = try_load_headline(run_dir) else {
        return;
    };
    println!();
    println!("--- post-run stats ({}) ---", view_set.as_str());
    print_query_table(&table);
    println!();
    println!("next steps:");
    println!("  bucephalus views {}", run_id);
    println!("  bucephalus views {} --all", run_id);
    println!("  bucephalus query {} \"SELECT * FROM trials\"", run_id);
}

fn try_post_run_stats_json(run_dir: &Path) -> Value {
    let Some((view_set, table)) = try_load_headline(run_dir) else {
        return Value::Null;
    };
    json!({
        "view_set": view_set.as_str(),
        "headline": query_table_to_json(&table),
    })
}

fn try_load_headline(run_dir: &Path) -> Option<(analysis::ViewSet, analysis::QueryTable)> {
    let view_set = analysis::run_view_set(run_dir).ok()?;
    let headline = view_set.headline_view()?;
    let table = analysis::query_view(run_dir, headline, 20).ok()?;
    Some((view_set, table))
}

fn build_runs_table(project_root: &Path) -> Result<analysis::QueryTable> {
    let entries = collect_run_inventory(project_root)?;
    let rows = entries
        .into_iter()
        .map(|entry| {
            let metrics = read_run_metrics(&entry.run_dir);
            vec![
                Value::String(entry.control.status_display),
                Value::String(entry.started_at_display),
                Value::String(entry.run_id),
                Value::String(display_or_dash(&entry.experiment)),
                Value::String(entry.control.live_summary),
                json!(metrics.variants),
                match metrics.pass_rate {
                    Some(pr) => json!((pr * 10000.0).round() / 10000.0),
                    None => Value::Null,
                },
            ]
        })
        .collect();

    Ok(analysis::QueryTable {
        columns: vec![
            "status".into(),
            "started_at".into(),
            "run_id".into(),
            "experiment".into(),
            "live".into(),
            "variants".into(),
            "pass_rate".into(),
        ],
        rows,
    })
}

fn collect_run_inventory(project_root: &Path) -> Result<Vec<RunInventoryEntry>> {
    let db_path = lab_runner::account_sqlite_path_for_run(project_root)?;
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let account_id = lab_runner::active_account_id();
    let conn = Connection::open(&db_path)?;
    let mut stmt = conn.prepare(
        "SELECT run_dir FROM runs
         WHERE account_id=?1
         ORDER BY updated_at_ms DESC",
    )?;
    let mut rows = stmt.query(params![account_id])?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next()? {
        let run_dir: String = row.get(0)?;
        entries.push(inspect_run_inventory_entry(&PathBuf::from(run_dir)));
    }

    entries.sort_by(|a, b| {
        b.control
            .is_active
            .cmp(&a.control.is_active)
            .then_with(|| b.started_at.cmp(&a.started_at))
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    Ok(entries)
}

fn inspect_run_inventory_entry(run_dir: &Path) -> RunInventoryEntry {
    let dir_name = run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown_run")
        .to_string();
    let manifest = read_json_file(&run_dir.join("manifest.json")).unwrap_or_else(|| json!({}));
    let resolved =
        read_json_file(&run_dir.join("resolved_experiment.json")).unwrap_or_else(|| json!({}));

    let run_id = manifest
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or(&dir_name)
        .to_string();
    let started_at = manifest
        .get("created_at")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| timestamp_from_run_id(&run_id))
        .unwrap_or_default();
    let experiment = resolved
        .pointer("/experiment/id")
        .or_else(|| resolved.pointer("/id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let control = summarize_run_lifecycle(
        load_run_control(run_dir).as_ref(),
        load_engine_lease(run_dir).as_ref(),
        Utc::now(),
    );

    RunInventoryEntry {
        run_id,
        run_dir: run_dir.to_path_buf(),
        experiment,
        started_at_display: format_timestamp_for_display(&started_at),
        started_at,
        control,
    }
}

fn read_json_file(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(test)]
fn summarize_run_control(parsed: Option<&Value>) -> RunControlSummary {
    summarize_run_lifecycle_inner(parsed, None, Utc::now(), false)
}

fn summarize_run_lifecycle(
    parsed: Option<&Value>,
    engine_lease: Option<&Value>,
    now: DateTime<Utc>,
) -> RunControlSummary {
    summarize_run_lifecycle_inner(parsed, engine_lease, now, true)
}

fn summarize_run_lifecycle_inner(
    parsed: Option<&Value>,
    engine_lease: Option<&Value>,
    now: DateTime<Utc>,
    stale_when_missing_lease: bool,
) -> RunControlSummary {
    let Some(parsed) = parsed else {
        return RunControlSummary {
            status: "unknown".to_string(),
            status_display: "unknown".to_string(),
            live_summary: "idle".to_string(),
            active_trials: 0,
            is_active: false,
        };
    };

    let status = parsed
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let active_trials_map = parsed
        .get("active_trials")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let active_trials = active_trials_map.len();
    let workers = active_trials_map
        .values()
        .filter_map(|entry| entry.get("worker_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let worker_list = workers.iter().cloned().collect::<Vec<_>>();
    let worker_suffix = if worker_list.is_empty() {
        None
    } else if worker_list.len() <= 3 {
        Some(worker_list.join(","))
    } else {
        Some(format!("{} total", worker_list.len()))
    };

    if status == "running"
        && engine_lease_is_stale_or_missing(engine_lease, now, stale_when_missing_lease)
    {
        let status_display = if active_trials == 0 {
            "interrupted (stale running lease)".to_string()
        } else {
            format!(
                "interrupted (stale running lease, stale_active_trials={})",
                active_trials
            )
        };
        let live_summary = if active_trials == 0 {
            "stale owner".to_string()
        } else {
            format!("stale owner / {} recorded", active_trials)
        };
        return RunControlSummary {
            status: "interrupted".to_string(),
            status_display,
            live_summary,
            active_trials: 0,
            is_active: false,
        };
    }

    let status_display = if active_trials == 0 {
        status.clone()
    } else if let Some(worker_text) = worker_suffix.as_deref() {
        format!(
            "{} (active_trials={}, workers={})",
            status, active_trials, worker_text
        )
    } else {
        format!("{} (active_trials={})", status, active_trials)
    };
    let live_summary = if active_trials == 0 {
        "idle".to_string()
    } else if worker_list.is_empty() {
        format!("{} active", active_trials)
    } else if worker_list.len() <= 3 {
        format!("{} active / {}", active_trials, worker_list.join(","))
    } else {
        format!("{} active / {} workers", active_trials, worker_list.len())
    };
    let is_active = matches!(status.as_str(), "running" | "paused") || active_trials > 0;

    RunControlSummary {
        status,
        status_display,
        live_summary,
        active_trials,
        is_active,
    }
}

fn engine_lease_is_stale_or_missing(
    engine_lease: Option<&Value>,
    now: DateTime<Utc>,
    stale_when_missing: bool,
) -> bool {
    let Some(expires_at) = engine_lease
        .and_then(|lease| lease.get("expires_at"))
        .and_then(Value::as_str)
    else {
        return stale_when_missing;
    };
    DateTime::parse_from_rfc3339(expires_at)
        .map(|expires_at| now > expires_at.with_timezone(&Utc))
        .unwrap_or(true)
}

fn read_run_metrics(run_dir: &Path) -> RunMetrics {
    let sqlite_path = match lab_runner::account_sqlite_path_for_run(run_dir) {
        Ok(path) => path,
        Err(_) => {
            return RunMetrics {
                variants: 0,
                pass_rate: None,
            };
        }
    };
    if sqlite_path.exists() {
        if let Ok(conn) = Connection::open(&sqlite_path) {
            let account_id = lab_runner::active_account_id();
            let Some(run_id) = run_id_from_dir(run_dir) else {
                return RunMetrics {
                    variants: 0,
                    pass_rate: None,
                };
            };
            let variants = conn
                .query_row(
                    "SELECT count(DISTINCT variant_id)
                     FROM trial_rows
                     WHERE account_id=?1 AND run_id=?2",
                    params![account_id, run_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0_i64) as usize;
            let baseline_id: Option<String> = conn
                .query_row(
                    "SELECT baseline_id FROM trial_rows
                     WHERE account_id=?1 AND run_id=?2
                     LIMIT 1",
                    params![account_id, run_id],
                    |row| row.get(0),
                )
                .optional()
                .unwrap_or(None);
            let pass_rate = match baseline_id {
                Some(baseline) => conn
                    .query_row(
                        "SELECT avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END)
                         FROM trial_rows
                         WHERE account_id=?1 AND run_id=?2 AND variant_id = ?3",
                        params![account_id, run_id, baseline],
                        |row| row.get::<_, Option<f64>>(0),
                    )
                    .unwrap_or(None),
                None => None,
            };
            return RunMetrics {
                variants,
                pass_rate,
            };
        }
    }

    RunMetrics {
        variants: 0,
        pass_rate: None,
    }
}

fn format_timestamp_for_display(value: &str) -> String {
    if value.trim().is_empty() {
        return "unknown".to_string();
    }
    let display = value.replacen('T', " ", 1).trim().to_string();
    if let Some(prefix) = display.strip_suffix("+00:00") {
        format!("{}Z", prefix)
    } else {
        display
    }
}

fn timestamp_from_run_id(run_id: &str) -> Option<String> {
    let rest = run_id.strip_prefix("run_")?;
    let mut parts = rest.split('_');
    let date = parts.next()?;
    let time = parts.next()?;
    let micros = parts.next()?;
    if date.len() != 8
        || time.len() != 6
        || micros.len() != 6
        || !date.chars().all(|ch| ch.is_ascii_digit())
        || !time.chars().all(|ch| ch.is_ascii_digit())
        || !micros.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }

    Some(format!(
        "{}-{}-{} {}:{}:{}.{}",
        &date[0..4],
        &date[4..6],
        &date[6..8],
        &time[0..2],
        &time[2..4],
        &time[4..6],
        micros
    ))
}

fn display_or_dash(value: &str) -> String {
    if value.trim().is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection as SqliteConnection;
    use std::sync::{Mutex, MutexGuard};

    static ACCOUNT_DB_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_account_db_env() -> MutexGuard<'static, ()> {
        ACCOUNT_DB_ENV_LOCK.lock().expect("lock BUCEPHALUS_DB env")
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "bucephalus_cli_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ))
    }

    #[test]
    fn enforce_cli_binary_freshness_blocks_stale_executable() {
        let exe_path = PathBuf::from("/tmp/bucephalus");
        let exe_mtime = UNIX_EPOCH + Duration::from_secs(100);
        let src_mtime = UNIX_EPOCH + Duration::from_secs(101);
        let err = enforce_cli_binary_freshness(
            &exe_path,
            exe_mtime,
            Some((src_mtime, PathBuf::from("/workspace/source/lib.rs"))),
        )
        .expect_err("stale binary should be rejected");
        let msg = err.to_string();
        assert!(msg.contains("stale Bucephalus binary detected"), "{}", msg);
        assert!(msg.contains("cargo build --manifest-path"), "{}", msg);
        assert!(msg.contains("--bin bucephalus --release"), "{}", msg);
    }

    #[test]
    fn enforce_cli_binary_freshness_allows_up_to_date_executable() {
        let exe_path = PathBuf::from("/tmp/lab");
        let exe_mtime = UNIX_EPOCH + Duration::from_secs(200);
        let src_mtime = UNIX_EPOCH + Duration::from_secs(199);
        enforce_cli_binary_freshness(
            &exe_path,
            exe_mtime,
            Some((src_mtime, PathBuf::from("/workspace/source/lib.rs"))),
        )
        .expect("fresh binary should pass");
    }

    fn configure_test_account_db(run_dir: &Path) -> PathBuf {
        let db_path = run_dir.join(".bucephalus").join("bucephalus.sqlite");
        std::fs::create_dir_all(db_path.parent().expect("account db parent"))
            .expect("create account db parent");
        std::env::set_var("BUCEPHALUS_DB", &db_path);
        db_path
    }

    #[cfg(feature = "duckdb_engine")]
    fn seed_sqlite_run_for_analysis_query(run_dir: &Path) {
        let sqlite_path = configure_test_account_db(run_dir);
        let account_id = lab_runner::active_account_id();
        let run_id = run_dir
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("run")
            .to_string();
        let conn = SqliteConnection::open(&sqlite_path).expect("open sqlite");
        conn.execute_batch(
            "CREATE TABLE runs(account_id TEXT NOT NULL, run_id TEXT NOT NULL, experiment_id TEXT, run_dir TEXT NOT NULL);
             CREATE TABLE trial_rows(account_id TEXT NOT NULL, run_id TEXT NOT NULL, row_json TEXT NOT NULL);
             CREATE TABLE metric_rows(account_id TEXT NOT NULL, run_id TEXT NOT NULL, metric_name TEXT NOT NULL, row_json TEXT NOT NULL);
             CREATE TABLE metric_definitions(
                 account_id TEXT NOT NULL,
                 experiment_id TEXT NOT NULL,
                 metric_id TEXT NOT NULL,
                 semantic_key TEXT,
                 label TEXT,
                 value_type TEXT,
                 unit TEXT,
                 direction TEXT,
                 source_type TEXT NOT NULL,
                 source_pointer TEXT,
                 required INTEGER NOT NULL,
                 primary_metric INTEGER NOT NULL,
                 definition_json TEXT NOT NULL
             );
             CREATE TABLE event_rows(account_id TEXT NOT NULL, run_id TEXT NOT NULL, payload_json TEXT NOT NULL, row_json TEXT NOT NULL);
             CREATE TABLE contract_stage_rows(account_id TEXT NOT NULL, run_id TEXT NOT NULL, row_json TEXT NOT NULL);
             CREATE TABLE variant_snapshot_rows(account_id TEXT NOT NULL, run_id TEXT NOT NULL, row_json TEXT NOT NULL);
             CREATE TABLE slot_commit_records(
                 account_id TEXT NOT NULL,
                 run_id TEXT NOT NULL,
                 schedule_idx INTEGER NOT NULL,
                 slot_commit_id TEXT NOT NULL,
                 attempt INTEGER NOT NULL,
                 record_type TEXT NOT NULL,
                 record_json TEXT NOT NULL
             );
             CREATE TABLE runtime_kv(
                 account_id TEXT NOT NULL,
                 run_id TEXT NOT NULL,
                 key TEXT PRIMARY KEY,
                 value_json TEXT NOT NULL
             );",
        )
        .expect("create sqlite schema");
        conn.execute(
            "INSERT INTO runs(account_id, run_id, experiment_id, run_dir) VALUES (?1, ?2, ?3, ?4)",
            (
                &account_id,
                &run_id,
                "exp_query",
                run_dir.display().to_string(),
            ),
        )
        .expect("insert run registry row");
        conn.execute(
            "INSERT INTO trial_rows(account_id, run_id, row_json) VALUES (?1, ?2, ?3)",
            (&account_id, &run_id, format!(r#"{{"run_id":"{}","trial_id":"trial_1","variant_id":"base","task_id":"task_1","outcome":"success","slot_commit_id":"slot_1","schedule_idx":0}}"#, run_id)),
        )
        .expect("insert trial row");
        conn.execute(
            "INSERT INTO metric_rows(account_id, run_id, metric_name, row_json) VALUES (?1, ?2, ?3, ?4)",
            (&account_id, &run_id, "latency_ms", format!(r#"{{"run_id":"{}","trial_id":"trial_1","variant_id":"base","task_id":"task_1","metric_name":"latency_ms","metric_value":12.3,"slot_commit_id":"slot_1","schedule_idx":0}}"#, run_id)),
        )
        .expect("insert metric row");
        conn.execute(
            "INSERT INTO metric_definitions(
                 account_id, experiment_id, metric_id, semantic_key, label, value_type, unit,
                 direction, source_type, source_pointer, required, primary_metric, definition_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            (
                &account_id,
                "exp_query",
                "latency_ms",
                "runtime.latency",
                "Latency",
                "duration",
                "ms",
                "minimize",
                "agent_response",
                "/metrics/latency_ms",
                0_i64,
                0_i64,
                r#"{"id":"latency_ms"}"#,
            ),
        )
        .expect("insert metric definition row");
        conn.execute(
            "INSERT INTO event_rows(account_id, run_id, payload_json, row_json) VALUES (?1, ?2, ?3, ?4)",
            (
                &account_id,
                &run_id,
                r#"{"event_type":"model_call_end","rex":{"request_id":"req_123","server_ms":42}}"#,
                format!(r#"{{"run_id":"{}","trial_id":"trial_1","variant_id":"base","task_id":"task_1","event_type":"model_call_end","slot_commit_id":"slot_1","schedule_idx":0,"payload":{{"event_type":"model_call_end","rex":{{"request_id":"req_123","server_ms":42}}}}}}"#, run_id)
            ),
        )
        .expect("insert event row");
        for (row_seq, (stage, status, detail)) in [
            ("task_mapping", "ok", r#"{"status":"ok"}"#),
            ("agent_execution", "ok", r#"{"status":"ok"}"#),
            ("artifact_extraction", "ok", r#"{"status":"ok","workspace_delta":{"captured_bytes":42,"scoped_bytes":21}}"#),
            ("grader_input_mapping", "ok", r#"{"status":"ok"}"#),
            ("grader_execution", "ok", r#"{"status":"ok"}"#),
            ("grade_mapping", "ok", r#"{"status":"ok","overall_status":"ok","score_trust":"trusted","score":{"source":"mapped_grader_output","official_status":"resolved"}}"#),
        ]
        .into_iter()
        .enumerate()
        {
            conn.execute(
                "INSERT INTO contract_stage_rows(account_id, run_id, row_json) VALUES (?1, ?2, ?3)",
                (
                    &account_id,
                    &run_id,
                    format!(
                        r#"{{"run_id":"{}","trial_id":"trial_1","variant_id":"base","task_id":"task_1","repl_idx":0,"stage":"{}","status":"{}","recorded_at":"2026-01-01T00:00:00Z","detail":{},"slot_commit_id":"slot_1","schedule_idx":0,"attempt":0,"row_seq":{}}}"#,
                        run_id, stage, status, detail, row_seq
                    ),
                ),
            )
            .expect("insert contract stage row");
        }
        conn.execute(
            "INSERT INTO variant_snapshot_rows(account_id, run_id, row_json) VALUES (?1, ?2, ?3)",
            (&account_id, &run_id, format!(r#"{{"run_id":"{}","variant_id":"base","task_id":"task_1","slot_commit_id":"slot_1","schedule_idx":0}}"#, run_id)),
        )
        .expect("insert variant snapshot row");
        conn.execute(
            "INSERT INTO slot_commit_records(account_id, run_id, schedule_idx, slot_commit_id, attempt, record_type, record_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                account_id,
                run_id,
                0_i64,
                "slot_1",
                0_i64,
                "commit",
                r#"{"record_type":"commit","schedule_idx":0,"slot_commit_id":"slot_1","attempt":0}"#,
            ),
        )
        .expect("insert slot commit record");
    }

    fn seed_runtime_value(run_dir: &Path, key: &str, value: &Value) {
        let sqlite_path = configure_test_account_db(run_dir);
        let account_id = lab_runner::active_account_id();
        let run_id = run_dir
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("run")
            .to_string();
        let conn = SqliteConnection::open(&sqlite_path).expect("open sqlite");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS runtime_kv(
                 account_id TEXT NOT NULL,
                 run_id TEXT NOT NULL,
                 key TEXT PRIMARY KEY,
                 value_json TEXT NOT NULL
             );",
        )
        .expect("create runtime_kv");
        conn.execute(
            "INSERT INTO runtime_kv(account_id, run_id, key, value_json) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json",
            (
                account_id,
                run_id,
                key,
                serde_json::to_string(value).expect("serialize runtime value"),
            ),
        )
        .expect("write runtime value");
    }

    fn seed_runtime_run_control(run_dir: &Path, control: &Value) {
        seed_runtime_value(run_dir, lab_runner::run_control_record_key(), control);
    }

    fn seed_runtime_engine_lease(run_dir: &Path, lease: &Value) {
        seed_runtime_value(run_dir, lab_runner::engine_lease_record_key(), lease);
    }

    #[cfg(feature = "duckdb_engine")]
    #[test]
    fn query_run_uses_account_sqlite_and_keeps_real_run_id_in_metadata() {
        let _env_guard = lock_account_db_env();
        let run_dir = temp_dir("sqlite_query_cleanup");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        seed_sqlite_run_for_analysis_query(&run_dir);

        let run_id = run_dir
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("run");

        let table = analysis::query_run(&run_dir, "SELECT run_id FROM analysis_metadata LIMIT 1")
            .expect("query run");
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0][0], Value::String(run_id.to_string()));
        let health = analysis::query_run(
            &run_dir,
            "SELECT trusted_scores, untrusted_scores, unknown_score_trust FROM contract_health",
        )
        .expect("query contract health");
        assert_eq!(health.rows[0], vec![json!(1.0), json!(0.0), json!(0.0)]);
        let events = analysis::query_run(
            &run_dir,
            "SELECT json_extract_string(payload_json, '$.rex.request_id') AS request_id FROM events",
        )
        .expect("query events payload");
        assert_eq!(events.rows[0][0], Value::String("req_123".to_string()));
        assert!(
            !run_dir.join("run.sqlite").exists(),
            "run-scoped sqlite database should not be created"
        );

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn read_run_status_renders_multiflight_active_trials() {
        let _env_guard = lock_account_db_env();
        let run_dir = temp_dir("run_status");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        let control = json!({
            "schema_version": "run_control_v2",
            "run_id": "run_1",
            "status": "running",
            "active_trials": {
                "trial_1": {
                    "trial_id": "trial_1",
                    "worker_id": "worker_2",
                    "schedule_idx": 1,
                    "variant_id": "base",
                    "started_at": "2026-02-22T00:00:00Z",
                    "control": null
                },
                "trial_2": {
                    "trial_id": "trial_2",
                    "worker_id": "worker_1",
                    "schedule_idx": 2,
                    "variant_id": "candidate",
                    "started_at": "2026-02-22T00:00:01Z",
                    "control": null
                }
            },
            "updated_at": "2026-02-22T00:00:02Z"
        });
        seed_runtime_run_control(&run_dir, &control);
        seed_runtime_engine_lease(
            &run_dir,
            &json!({
                "schema_version": "engine_lease_v1",
                "run_id": "run_1",
                "expires_at": "2999-01-01T00:00:00Z"
            }),
        );

        let status = read_run_status(&run_dir);
        assert_eq!(
            status,
            "running (active_trials=2, workers=worker_1,worker_2)"
        );

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn read_run_status_marks_stale_running_lease_inactive() {
        let _env_guard = lock_account_db_env();
        let run_dir = temp_dir("run_status_stale");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        let control = json!({
            "schema_version": "run_control_v2",
            "run_id": "run_1",
            "status": "running",
            "active_trials": {
                "trial_1": {
                    "trial_id": "trial_1",
                    "worker_id": "worker_1",
                    "schedule_idx": 1,
                    "variant_id": "base",
                    "started_at": "2026-02-22T00:00:00Z"
                }
            },
            "updated_at": "2026-02-22T00:00:02Z"
        });
        seed_runtime_run_control(&run_dir, &control);
        seed_runtime_engine_lease(
            &run_dir,
            &json!({
                "schema_version": "engine_lease_v1",
                "run_id": "run_1",
                "expires_at": "2000-01-01T00:00:00Z"
            }),
        );

        let status = read_run_status(&run_dir);
        assert_eq!(
            status,
            "interrupted (stale running lease, stale_active_trials=1)"
        );

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn summarize_run_control_exposes_live_summary_and_activity_flag() {
        let control = json!({
            "schema_version": "run_control_v2",
            "run_id": "run_1",
            "status": "running",
            "active_trials": {
                "trial_1": {
                    "trial_id": "trial_1",
                    "worker_id": "worker_a",
                    "schedule_idx": 0,
                    "variant_id": "base",
                    "started_at": "2026-03-09T17:00:00Z",
                    "control": null
                }
            },
            "updated_at": "2026-03-09T17:00:02Z"
        });

        let summary = summarize_run_control(Some(&control));
        assert_eq!(summary.status, "running");
        assert_eq!(summary.active_trials, 1);
        assert_eq!(
            summary.status_display,
            "running (active_trials=1, workers=worker_a)"
        );
        assert_eq!(summary.live_summary, "1 active / worker_a");
        assert!(summary.is_active);
    }

    #[test]
    fn summarize_run_lifecycle_marks_stale_running_lease_inactive() {
        let control = json!({
            "schema_version": "run_control_v2",
            "run_id": "run_1",
            "status": "running",
            "active_trials": {
                "trial_1": {
                    "trial_id": "trial_1",
                    "worker_id": "worker_a",
                    "schedule_idx": 0,
                    "variant_id": "base",
                    "started_at": "2026-03-09T17:00:00Z"
                }
            },
            "updated_at": "2026-03-09T17:00:02Z"
        });
        let lease = json!({
            "schema_version": "engine_lease_v1",
            "run_id": "run_1",
            "expires_at": "2026-03-09T17:00:05Z"
        });
        let now = DateTime::parse_from_rfc3339("2026-03-09T17:00:06Z")
            .unwrap()
            .with_timezone(&Utc);

        let summary = summarize_run_lifecycle(Some(&control), Some(&lease), now);

        assert_eq!(summary.status, "interrupted");
        assert_eq!(
            summary.status_display,
            "interrupted (stale running lease, stale_active_trials=1)"
        );
        assert_eq!(summary.live_summary, "stale owner / 1 recorded");
        assert_eq!(summary.active_trials, 0);
        assert!(!summary.is_active);
    }

    #[test]
    fn live_run_picker_keeps_interrupted_runtime_records_visible() {
        let entry = RunInventoryEntry {
            run_id: "run_1".to_string(),
            run_dir: PathBuf::from("/tmp/run_1"),
            experiment: "exp".to_string(),
            started_at: "2026-03-09T17:00:00Z".to_string(),
            started_at_display: "2026-03-09 17:00:00Z".to_string(),
            control: RunControlSummary {
                status: "interrupted".to_string(),
                status_display: "interrupted (stale running lease)".to_string(),
                live_summary: "stale owner".to_string(),
                active_trials: 0,
                is_active: false,
            },
        };

        assert!(show_in_live_run_picker(&entry));
    }

    #[test]
    fn summarize_run_lifecycle_keeps_fresh_running_lease_active() {
        let control = json!({
            "schema_version": "run_control_v2",
            "run_id": "run_1",
            "status": "running",
            "active_trials": {},
            "updated_at": "2026-03-09T17:00:02Z"
        });
        let lease = json!({
            "schema_version": "engine_lease_v1",
            "run_id": "run_1",
            "expires_at": "2026-03-09T17:00:08Z"
        });
        let now = DateTime::parse_from_rfc3339("2026-03-09T17:00:06Z")
            .unwrap()
            .with_timezone(&Utc);

        let summary = summarize_run_lifecycle(Some(&control), Some(&lease), now);

        assert_eq!(summary.status, "running");
        assert_eq!(summary.status_display, "running");
        assert!(summary.is_active);
    }

    #[test]
    fn inspect_run_inventory_entry_reads_manifest_timestamp_and_experiment() {
        let run_dir = temp_dir("inventory_entry");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("manifest.json"),
            r#"{
  "schema_version": "manifest_v1",
  "run_id": "run_123",
  "created_at": "2026-03-09T17:33:12Z"
}"#,
        )
        .expect("manifest");
        std::fs::write(
            run_dir.join("resolved_experiment.json"),
            r#"{
  "experiment": {
    "id": "exp_browser"
  }
}"#,
        )
        .expect("resolved");

        let entry = inspect_run_inventory_entry(&run_dir);
        assert_eq!(entry.run_id, "run_123");
        assert_eq!(entry.experiment, "exp_browser");
        assert_eq!(entry.started_at, "2026-03-09T17:33:12Z");
        assert_eq!(entry.started_at_display, "2026-03-09 17:33:12Z");
        assert_eq!(entry.control.status, "unknown");

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn build_inflight_scoreboard_table_reads_active_trials_when_facts_are_empty() {
        let _env_guard = lock_account_db_env();
        let run_dir = temp_dir("inflight_scoreboard");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        let control = json!({
            "schema_version": "run_control_v2",
            "run_id": "run_1",
            "status": "running",
            "active_trials": {
                "trial_2": {
                    "trial_id": "trial_2",
                    "worker_id": "worker_2",
                    "schedule_idx": 1,
                    "variant_id": "codex_spark",
                    "started_at": "2026-02-22T00:00:01Z",
                    "control": null
                },
                "trial_1": {
                    "trial_id": "trial_1",
                    "worker_id": "worker_1",
                    "schedule_idx": 0,
                    "variant_id": "glm_5",
                    "started_at": "2026-02-22T00:00:00Z",
                    "control": null
                }
            },
            "updated_at": "2026-02-22T00:00:02Z"
        });
        seed_runtime_run_control(&run_dir, &control);

        let table =
            build_inflight_scoreboard_table(&run_dir).expect("in-flight scoreboard should exist");
        assert_eq!(
            table.columns,
            vec![
                "variant_id".to_string(),
                "trial_id".to_string(),
                "schedule_idx".to_string(),
                "worker_id".to_string(),
                "started_at".to_string(),
                "lifecycle".to_string(),
            ]
        );
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0][0], Value::String("glm_5".to_string()));
        assert_eq!(table.rows[0][1], Value::String("trial_1".to_string()));
        assert_eq!(table.rows[0][5], Value::String("in_flight".to_string()));
        assert_eq!(table.rows[1][0], Value::String("codex_spark".to_string()));
        assert_eq!(table.rows[1][1], Value::String("trial_2".to_string()));

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn resolve_requested_view_accepts_current_ab_aliases() {
        let raw = vec!["run_progress".to_string()];
        let task_metrics = resolve_requested_view(analysis::ViewSet::AbTest, &raw, "task-compare")
            .expect("task-compare alias");
        assert_eq!(task_metrics.name, "task_metrics");
        assert_eq!(
            task_metrics.source.as_deref(),
            Some("ab_task_metrics_side_by_side")
        );
        assert!(task_metrics.standardize_ab_terms);

        let trace = resolve_requested_view(analysis::ViewSet::AbTest, &raw, "trace-diff")
            .expect("trace-diff alias");
        assert_eq!(trace.name, "trace");
        assert_eq!(trace.source.as_deref(), Some("ab_trace_row_side_by_side"));
        assert!(trace.standardize_ab_terms);
    }

    #[test]
    fn standardized_views_choose_dense_display_modes() {
        let raw = vec!["events".to_string(), "run_progress".to_string()];
        let events = resolve_requested_view(analysis::ViewSet::CoreOnly, &raw, "events")
            .expect("events view");
        assert_eq!(display_mode_for_view(&events), tui::DisplayMode::Timeline);

        let progress = resolve_requested_view(analysis::ViewSet::CoreOnly, &raw, "run_progress")
            .expect("progress view");
        assert_eq!(display_mode_for_view(&progress), tui::DisplayMode::Overview);

        let scoreboard = resolve_requested_view(analysis::ViewSet::CoreOnly, &raw, "scoreboard")
            .expect("scoreboard view");
        assert_eq!(
            display_mode_for_view(&scoreboard),
            tui::DisplayMode::Scoreboard
        );

        let task_metrics = resolve_requested_view(analysis::ViewSet::AbTest, &raw, "task_metrics")
            .expect("task metrics view");
        assert_eq!(
            display_mode_for_view(&task_metrics),
            tui::DisplayMode::Comparison
        );
    }

    #[test]
    fn standardize_ab_column_name_rewrites_mixed_terms() {
        assert_eq!(
            standardize_ab_column_name("baseline_outcome"),
            "variant_a_outcome"
        );
        assert_eq!(
            standardize_ab_column_name("treatment_outcome"),
            "variant_b_outcome"
        );
        assert_eq!(
            standardize_ab_column_name("a_result_score"),
            "variant_a_result_score"
        );
        assert_eq!(
            standardize_ab_column_name("b_result_score"),
            "variant_b_result_score"
        );
        assert_eq!(
            standardize_ab_column_name("d_result_score"),
            "delta_result_score"
        );
        assert_eq!(standardize_ab_column_name("a_variant_id"), "variant_a_id");
        assert_eq!(standardize_ab_column_name("b_variant_id"), "variant_b_id");
    }

    #[test]
    fn build_trace_sections_compacts_variant_columns() {
        let table = analysis::QueryTable {
            columns: vec![
                "task_id".to_string(),
                "repl_idx".to_string(),
                "variant_a_id".to_string(),
                "variant_b_id".to_string(),
                "variant_a_trial_id".to_string(),
                "variant_b_trial_id".to_string(),
                "row_seq".to_string(),
                "variant_a_event_type".to_string(),
                "variant_b_event_type".to_string(),
                "variant_a_turn_index".to_string(),
                "variant_b_turn_index".to_string(),
                "variant_a_model".to_string(),
                "variant_b_model".to_string(),
                "variant_a_tool".to_string(),
                "variant_b_tool".to_string(),
                "variant_a_status".to_string(),
                "variant_b_status".to_string(),
                "variant_a_call_id".to_string(),
                "variant_b_call_id".to_string(),
            ],
            rows: vec![vec![
                Value::String("TASK001".to_string()),
                json!(0),
                Value::String("a".to_string()),
                Value::String("b".to_string()),
                Value::String("trial_a".to_string()),
                Value::String("trial_b".to_string()),
                json!(1),
                Value::String("model_call_end".to_string()),
                Value::String("tool_call_end".to_string()),
                json!(0),
                Value::Null,
                Value::String("m-a".to_string()),
                Value::Null,
                Value::Null,
                Value::String("Bash".to_string()),
                Value::String("ok".to_string()),
                Value::String("ok".to_string()),
                Value::String("call-a".to_string()),
                Value::String("call-b".to_string()),
            ]],
        };

        let sections = build_trace_sections(&table);
        assert_eq!(sections.len(), 1);
        let section = &sections[0];
        assert_eq!(section.task_id, "TASK001");
        assert_eq!(section.repl_idx, "0");
        assert_eq!(section.variant_a_id, "a");
        assert_eq!(section.variant_b_id, "b");
        assert_eq!(
            section.variant_a_table.columns,
            vec!["row", "evt", "turn", "model", "tool", "st", "call"]
        );
        assert_eq!(
            section.variant_b_table.columns,
            vec!["row", "evt", "turn", "model", "tool", "st", "call"]
        );
    }

    #[test]
    fn build_trace_sections_drops_null_only_side_rows() {
        let table = analysis::QueryTable {
            columns: vec![
                "task_id".to_string(),
                "repl_idx".to_string(),
                "variant_a_id".to_string(),
                "variant_b_id".to_string(),
                "variant_a_trial_id".to_string(),
                "variant_b_trial_id".to_string(),
                "row_seq".to_string(),
                "variant_a_event_type".to_string(),
                "variant_b_event_type".to_string(),
                "variant_a_turn_index".to_string(),
                "variant_b_turn_index".to_string(),
                "variant_a_model".to_string(),
                "variant_b_model".to_string(),
                "variant_a_tool".to_string(),
                "variant_b_tool".to_string(),
                "variant_a_status".to_string(),
                "variant_b_status".to_string(),
                "variant_a_call_id".to_string(),
                "variant_b_call_id".to_string(),
            ],
            rows: vec![
                vec![
                    Value::String("TASK001".to_string()),
                    json!(0),
                    Value::String("a".to_string()),
                    Value::String("b".to_string()),
                    Value::String("trial_a".to_string()),
                    Value::String("trial_b".to_string()),
                    json!(1),
                    Value::String("model_call_end".to_string()),
                    Value::Null,
                    json!(0),
                    Value::Null,
                    Value::String("m-a".to_string()),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::String("ok".to_string()),
                    Value::Null,
                    Value::String("call-a".to_string()),
                    Value::Null,
                ],
                vec![
                    Value::String("TASK001".to_string()),
                    json!(0),
                    Value::String("a".to_string()),
                    Value::String("b".to_string()),
                    Value::String("trial_a".to_string()),
                    Value::String("trial_b".to_string()),
                    json!(2),
                    Value::Null,
                    Value::String("tool_call_end".to_string()),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::String("Bash".to_string()),
                    Value::Null,
                    Value::String("ok".to_string()),
                    Value::Null,
                    Value::String("call-b".to_string()),
                ],
            ],
        };
        let sections = build_trace_sections(&table);
        assert_eq!(sections.len(), 1);
        let section = &sections[0];
        assert_eq!(section.variant_a_table.rows.len(), 1);
        assert_eq!(section.variant_b_table.rows.len(), 1);
    }

    #[test]
    fn trace_markdown_renderer_emits_pure_markdown() {
        let table = analysis::QueryTable {
            columns: vec![
                "task_id".to_string(),
                "repl_idx".to_string(),
                "variant_a_id".to_string(),
                "variant_b_id".to_string(),
                "variant_a_trial_id".to_string(),
                "variant_b_trial_id".to_string(),
                "row_seq".to_string(),
                "variant_a_event_type".to_string(),
                "variant_b_event_type".to_string(),
                "variant_a_turn_index".to_string(),
                "variant_b_turn_index".to_string(),
                "variant_a_model".to_string(),
                "variant_b_model".to_string(),
                "variant_a_tool".to_string(),
                "variant_b_tool".to_string(),
                "variant_a_status".to_string(),
                "variant_b_status".to_string(),
                "variant_a_call_id".to_string(),
                "variant_b_call_id".to_string(),
            ],
            rows: vec![vec![
                Value::String("TASK001".to_string()),
                json!(0),
                Value::String("a".to_string()),
                Value::String("b".to_string()),
                Value::String("trial_a".to_string()),
                Value::String("trial_b".to_string()),
                json!(1),
                Value::String("model_call_end".to_string()),
                Value::String("model_call_end".to_string()),
                json!(0),
                json!(0),
                Value::String("m-a".to_string()),
                Value::String("m-b".to_string()),
                Value::Null,
                Value::Null,
                Value::String("ok".to_string()),
                Value::String("ok".to_string()),
                Value::String("call-a".to_string()),
                Value::String("call-b".to_string()),
            ]],
        };
        let rendered = render_trace_sections_markdown(&table).expect("trace markdown");
        assert!(rendered.contains("#### variant_a"));
        assert!(rendered.contains("#### variant_b"));
        assert!(!rendered.contains("<table>"));
        assert!(!rendered.contains("<td"));
    }

    #[test]
    fn choose_query_table_anchor_indices_prefers_task_context_columns() {
        let columns = vec![
            "delta_tokens_in".to_string(),
            "task_id".to_string(),
            "variant_b_outcome".to_string(),
            "repl_idx".to_string(),
            "turn_index".to_string(),
            "variant_a_id".to_string(),
            "variant_b_id".to_string(),
        ];
        let anchors = choose_query_table_anchor_indices(&columns);
        assert_eq!(anchors, vec![1, 3, 4]);
    }

    #[test]
    fn should_chunk_query_table_for_wide_views() {
        let columns = (0..24)
            .map(|idx| format!("col_{}", idx))
            .collect::<Vec<_>>();
        let table = analysis::QueryTable {
            columns,
            rows: vec![vec![Value::String("x".to_string()); 24]],
        };
        assert!(should_chunk_query_table(&table, 120));
        assert!(should_chunk_query_table(&table, 200));
    }

    #[test]
    fn project_query_table_columns_with_prefix_trim_trims_variant_prefix() {
        let table = analysis::QueryTable {
            columns: vec![
                "task_id".to_string(),
                "repl_idx".to_string(),
                "variant_a_trial_id".to_string(),
                "variant_a_event_type".to_string(),
                "variant_b_trial_id".to_string(),
            ],
            rows: vec![vec![
                Value::String("TASK001".to_string()),
                json!(0),
                Value::String("trial_1".to_string()),
                Value::String("model_call_start".to_string()),
                Value::String("trial_2".to_string()),
            ]],
        };

        let projected =
            project_query_table_columns_with_prefix_trim(&table, &[0, 1], &[2, 3], "variant_a_");
        assert_eq!(
            projected.columns,
            vec![
                "task_id".to_string(),
                "repl_idx".to_string(),
                "trial_id".to_string(),
                "event_type".to_string(),
            ]
        );
        assert_eq!(projected.rows.len(), 1);
    }

    #[test]
    fn project_query_table_by_column_priority_reorders_and_keeps_remaining() {
        let table = analysis::QueryTable {
            columns: vec![
                "a_result_score".to_string(),
                "task_id".to_string(),
                "b_outcome".to_string(),
                "a_outcome".to_string(),
                "d_result_score".to_string(),
            ],
            rows: vec![vec![
                json!(1.0),
                Value::String("TASK001".to_string()),
                Value::String("failure".to_string()),
                Value::String("success".to_string()),
                json!(-1.0),
            ]],
        };
        let projected =
            project_query_table_by_column_priority(&table, &["task_id", "a_outcome", "b_outcome"]);
        assert_eq!(
            projected.columns,
            vec![
                "task_id".to_string(),
                "a_outcome".to_string(),
                "b_outcome".to_string(),
                "a_result_score".to_string(),
                "d_result_score".to_string(),
            ]
        );
        assert_eq!(projected.rows.len(), 1);
    }

    #[test]
    fn display_column_name_maps_canonical_columns() {
        let cases = [
            ("variant_id", "variant"),
            ("task_id", "task"),
            ("trial_id", "trial"),
            ("experiment_id", "experiment"),
            ("baseline_id", "baseline"),
            ("primary_metric_mean", "metric"),
            ("primary_metric_value", "metric_val"),
            ("primary_metric_name", "metric_name"),
            ("success_rate", "pass%"),
            ("pass_rate", "pass%"),
            ("n_trials", "trials"),
            ("trial_count", "trials"),
            ("variant_count", "variants"),
            ("task_count", "tasks"),
            ("active_trials", "active"),
            ("completed_trials", "done"),
            ("total_trials", "total"),
            ("event_type", "event"),
            ("turn_number", "turn"),
            ("tool_name", "tool"),
            ("status_code", "status"),
            ("error_message", "error"),
            ("metric_name", "metric"),
            ("metric_value", "value"),
            ("started_at", "started"),
            ("completed_at", "completed"),
            ("updated_at", "updated"),
            ("duration_seconds", "dur_s"),
            ("worker_id", "worker"),
            ("win_rate", "win%"),
            ("loss_rate", "loss%"),
            ("tie_rate", "tie%"),
            ("effect_size", "effect"),
            ("mcnemar_p", "p_val"),
            ("outcome", "outcome"),
        ];
        for (canonical, expected) in cases {
            assert_eq!(
                display_column_name(canonical),
                expected,
                "display_column_name({:?}) should be {:?}",
                canonical,
                expected
            );
        }
    }

    #[test]
    fn display_column_name_strips_variant_prefixes() {
        assert_eq!(display_column_name("variant_a_outcome"), "a_outcome");
        assert_eq!(display_column_name("variant_b_outcome"), "b_outcome");
        assert_eq!(display_column_name("delta_pass_rate"), "d_pass_rate");
    }

    #[test]
    fn display_column_name_strips_count_suffix() {
        assert_eq!(display_column_name("error_count"), "errors");
        assert_eq!(display_column_name("retry_count"), "retrys");
    }

    #[test]
    fn display_column_name_passes_through_unknown_names() {
        assert_eq!(display_column_name("foo_bar"), "foo_bar");
        assert_eq!(display_column_name("custom_column"), "custom_column");
    }

    #[test]
    fn display_column_name_is_idempotent() {
        let columns = [
            "variant_id",
            "primary_metric_mean",
            "pass_rate",
            "n_trials",
            "outcome",
            "status_code",
            "effect_size",
            "unknown_col",
        ];
        for col in columns {
            let once = display_column_name(col);
            let twice = display_column_name(&once);
            assert_eq!(
                once, twice,
                "display_column_name is not idempotent for {:?}: first={:?}, second={:?}",
                col, once, twice
            );
        }
    }

    #[test]
    fn display_column_name_preserves_metric_color_coding() {
        let metric_columns = [
            "pass_rate",
            "success_rate",
            "primary_metric_mean",
            "win_rate",
            "loss_rate",
            "tie_rate",
            "effect_size",
        ];
        for canonical in metric_columns {
            let display = display_column_name(canonical);
            assert!(
                tui::is_metric_column(&display),
                "display name {:?} (from {:?}) is not recognized as a metric column",
                display,
                canonical
            );
        }
    }

    #[test]
    fn display_column_name_preserves_outcome_color_coding() {
        assert!(tui::is_outcome_column(&display_column_name("outcome")));
        assert!(tui::is_outcome_column(&display_column_name(
            "variant_a_outcome"
        )));
        assert!(tui::is_outcome_column(&display_column_name(
            "variant_b_outcome"
        )));
    }

    #[test]
    fn display_column_name_preserves_status_color_coding() {
        assert!(tui::is_status_column(&display_column_name("status_code")));
    }

    fn fake_run_entries(dirs: &[&str]) -> Vec<RunInventoryEntry> {
        dirs.iter()
            .map(|d| RunInventoryEntry {
                run_dir: PathBuf::from(d),
                run_id: d.to_string(),
                experiment: "test".to_string(),
                started_at: "2026-01-01T00:00:00Z".to_string(),
                started_at_display: "now".to_string(),
                control: RunControlSummary {
                    status: "completed".to_string(),
                    status_display: "completed".to_string(),
                    live_summary: String::new(),
                    active_trials: 0,
                    is_active: false,
                },
            })
            .collect()
    }

    #[test]
    fn resolve_run_selection_preserves_index_when_no_anchor() {
        let entries = fake_run_entries(&["/a", "/b", "/c", "/d"]);
        assert_eq!(resolve_run_selection(None, &entries, 0), 0);
        assert_eq!(resolve_run_selection(None, &entries, 2), 2);
        assert_eq!(resolve_run_selection(None, &entries, 3), 3);
    }

    #[test]
    fn resolve_run_selection_snaps_to_anchor_position() {
        let entries = fake_run_entries(&["/a", "/b", "/c", "/d"]);
        assert_eq!(resolve_run_selection(Some(Path::new("/c")), &entries, 0), 2);
        assert_eq!(resolve_run_selection(Some(Path::new("/a")), &entries, 3), 0);
    }

    #[test]
    fn resolve_run_selection_falls_back_when_anchor_missing() {
        let entries = fake_run_entries(&["/a", "/b", "/c"]);
        assert_eq!(
            resolve_run_selection(Some(Path::new("/missing")), &entries, 1),
            1
        );
    }

    #[test]
    fn resolve_run_selection_clamps_to_bounds() {
        let entries = fake_run_entries(&["/a", "/b"]);
        assert_eq!(resolve_run_selection(None, &entries, 99), 1);
        assert_eq!(resolve_run_selection(None, &[], 5), 0);
    }

    #[test]
    fn resolve_run_selection_scroll_free_after_anchor_cleared() {
        let entries = fake_run_entries(&["/a", "/b", "/c", "/d"]);

        let idx = resolve_run_selection(Some(Path::new("/b")), &entries, 0);
        assert_eq!(idx, 1);

        let idx = resolve_run_selection(None, &entries, idx);
        assert_eq!(idx, 1);

        let scrolled = idx + 1; // simulate scroll_down
        let idx = resolve_run_selection(None, &entries, scrolled);
        assert_eq!(
            idx, 2,
            "scroll must not be overridden after anchor is cleared"
        );
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!(
            "bucephalus_cli_{}_{}_{}",
            name,
            std::process::id(),
            nanos
        ))
    }

    fn fake_build_result(package_dir: PathBuf) -> lab_runner::BuildResult {
        lab_runner::BuildResult {
            manifest_path: package_dir.join("manifest.json"),
            checksums_path: package_dir.join("checksums.json"),
            package_checks_path: package_dir.join("package_checks.json"),
            package_dir,
        }
    }

    #[test]
    fn build_run_publish_replaces_previous_package_output() {
        let root = unique_test_dir("build_run_replace");
        let final_out = root.join("package");
        let temp_out = root.join("temp_package");
        fs::create_dir_all(&final_out).expect("final package dir");
        fs::write(final_out.join("manifest.json"), "{}\n").expect("old manifest");
        fs::write(final_out.join("old.txt"), "old\n").expect("old payload");
        fs::create_dir_all(&temp_out).expect("temp package dir");
        fs::write(temp_out.join("manifest.json"), "{}\n").expect("new manifest");
        fs::write(temp_out.join("new.txt"), "new\n").expect("new payload");

        let published = publish_build_run_package(fake_build_result(temp_out), &final_out)
            .expect("publish package");

        assert_eq!(published.package_dir, final_out);
        assert!(final_out.join("new.txt").is_file());
        assert!(!final_out.join("old.txt").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn build_run_publish_refuses_non_package_output_dir() {
        let root = unique_test_dir("build_run_refuse_non_package");
        let final_out = root.join("package");
        let temp_out = root.join("temp_package");
        fs::create_dir_all(&final_out).expect("final dir");
        fs::write(final_out.join("notes.txt"), "not a package\n").expect("non-package file");
        fs::create_dir_all(&temp_out).expect("temp package dir");
        fs::write(temp_out.join("manifest.json"), "{}\n").expect("new manifest");

        let err = publish_build_run_package(fake_build_result(temp_out), &final_out)
            .expect_err("non-package output dir must be refused");

        assert!(
            err.to_string()
                .contains("does not look like a Bucephalus package"),
            "unexpected error: {}",
            err
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
