use anyhow::{anyhow, Context, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lab_analysis as analysis;
use lab_core::{sha256_bytes, sha256_file};
use lab_provenance as provenance;
use lab_schemas as schemas;

mod cloud_auth_ux;
mod latch_daemon;
mod tui;
mod view_layout;
mod view_spec;

use crate::cloud_auth_ux::BUCEPHALUS_CLOUD_USER_TOKEN_ENV;
use crate::view_spec::{
    present_table, renderer_for_resolved, resolve_requested_view, resolved_view_from_spec,
    standard_view_source_label, standard_views_for_set, ResolvedView, ResolvedViewPlan,
    ViewRenderer,
};

#[derive(Parser)]
#[command(name = "bucephalus", version = env!("CARGO_PKG_VERSION"), about = "Bucephalus CLI")]
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum InitClientArg {
    #[value(name = "cli")]
    Cli,
    #[value(name = "api")]
    Api,
    #[value(name = "acp")]
    Acp,
    #[value(name = "mcp")]
    Mcp,
    #[value(name = "sdk")]
    Sdk,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum InitLanguageArg {
    #[value(name = "python")]
    Python,
    #[value(name = "typescript")]
    TypeScript,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum InitMcpRoleArg {
    #[value(name = "target")]
    Target,
    #[value(name = "tool")]
    Tool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum InitStreamArg {
    #[value(name = "none")]
    None,
    #[value(name = "sse")]
    Sse,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SetupMcpClientArg {
    #[value(name = "auto")]
    Auto,
    #[value(name = "claude-code")]
    ClaudeCode,
    #[value(name = "claude-desktop")]
    ClaudeDesktop,
    #[value(name = "cursor-project")]
    CursorProject,
}

#[derive(Subcommand)]
enum SetupCommands {
    #[command(about = "Show Tier-1 daemon, MCP, and auth readiness")]
    Status {
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Unload the local daemon service and remove MCP registration")]
    Uninstall {
        #[arg(long = "client", value_enum, action = ArgAction::Append)]
        client: Vec<SetupMcpClientArg>,
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        no_daemon_service: bool,
        #[arg(long)]
        no_mcp: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
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
    #[command(about = "Create an experiment from an agent client workflow")]
    Init {
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
        #[arg(long, value_enum)]
        client: Option<InitClientArg>,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long, value_enum)]
        stream: Option<InitStreamArg>,
        #[arg(long, value_enum)]
        language: Option<InitLanguageArg>,
        #[arg(long, value_enum)]
        mcp_role: Option<InitMcpRoleArg>,
        #[arg(long)]
        mcp_tool: Option<String>,
        #[arg(long, default_value = "answer")]
        mode: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Run the Bucephalus MCP adapter over stdio")]
    Mcp,
    #[command(about = "Authenticate Bucephalus Cloud with OAuth device login")]
    Login {
        #[arg(long)]
        issuer: Option<String>,
        #[arg(long)]
        client_id: Option<String>,
        #[arg(long)]
        audience: Option<String>,
        #[arg(long)]
        resource: Option<String>,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        no_browser: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Remove cached Bucephalus Cloud OAuth tokens")]
    Logout {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Update the installed Bucephalus release")]
    Update {
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        install_dir: Option<PathBuf>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        setup: bool,
        #[arg(long)]
        modify_path: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Run the local Bucephalus latch daemon")]
    Daemon,
    #[command(about = "Install local Tier-1 daemon service and MCP client registration")]
    Setup {
        #[command(subcommand)]
        command: Option<SetupCommands>,
        #[arg(long = "client", value_enum, action = ArgAction::Append)]
        client: Vec<SetupMcpClientArg>,
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        no_daemon_service: bool,
        #[arg(long)]
        no_start: bool,
        #[arg(long)]
        no_mcp: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Build, preflight, and smoke-test an experiment from YAML")]
    Dev {
        target: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        overrides: Option<PathBuf>,
        #[arg(long, value_enum)]
        executor: Option<ExecutorArg>,
        #[arg(long, hide = true)]
        run_root: Option<PathBuf>,
        #[arg(long = "env", value_name = "KEY=VALUE", action = ArgAction::Append)]
        runtime_env: Vec<String>,
        #[arg(long = "env-file", value_name = "PATH", action = ArgAction::Append)]
        runtime_env_file: Vec<PathBuf>,
        #[arg(long = "secret-file", value_name = "ID=PATH", action = ArgAction::Append)]
        secret_file: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Diagnose a YAML experiment or sealed package without launching a full run")]
    Doctor {
        target: Option<PathBuf>,
        #[arg(long)]
        overrides: Option<PathBuf>,
        #[arg(long, value_enum)]
        executor: Option<ExecutorArg>,
        #[arg(long, hide = true)]
        run_root: Option<PathBuf>,
        #[arg(long = "env", value_name = "KEY=VALUE", action = ArgAction::Append)]
        runtime_env: Vec<String>,
        #[arg(long = "env-file", value_name = "PATH", action = ArgAction::Append)]
        runtime_env_file: Vec<PathBuf>,
        #[arg(long = "secret-file", value_name = "ID=PATH", action = ArgAction::Append)]
        secret_file: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    Build {
        experiment: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        overrides: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "Lint an experiment YAML by building a sealed package and running static checks"
    )]
    Lint {
        target: Option<PathBuf>,
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
    #[command(about = "Run a resolved Tier-1 latch manifest on this host")]
    Latch {
        #[command(subcommand)]
        command: LatchCommands,
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
    #[command(about = "Show per-trial scores and numeric means for a run")]
    Scores {
        run: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        csv: bool,
    },
    #[command(about = "Explain declared metrics, captured rows, and scoreboard columns for a run")]
    ExplainMetrics {
        run: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        csv: bool,
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
        #[arg(long, default_value = "experiment_authoring_v1.jsonschema")]
        schema: String,
        #[arg(long, default_value = "experiment.yaml")]
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
        #[arg(long)]
        force: bool,
        #[arg(long)]
        include_active: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum LatchCommands {
    #[command(about = "Validate a resolved latch manifest")]
    Validate {
        #[arg(value_name = "MANIFEST")]
        manifest: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Create a local demo latch manifest and seed workspace")]
    Demo {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Resolve and run a local Tier-1 latch smoke benchmark fixture")]
    Smoke {
        #[arg(long, default_value = "local:file-edit-smoke")]
        benchmark: String,
        #[arg(long, default_value_t = 2)]
        cases: usize,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        run_root: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(last = true, value_name = "ARGV", allow_hyphen_values = true)]
        argv: Vec<String>,
    },
    #[command(about = "Run resolved latch cases using a local headless agent command")]
    Run {
        #[arg(value_name = "MANIFEST")]
        manifest: PathBuf,
        #[arg(long)]
        run_root: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(last = true, value_name = "ARGV", allow_hyphen_values = true)]
        argv: Vec<String>,
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

#[derive(Clone, Debug)]
struct PostRunSection {
    name: &'static str,
    table: analysis::QueryTable,
}

#[derive(Clone, Debug)]
struct PostRunReport {
    view_set: analysis::ViewSet,
    sections: Vec<PostRunSection>,
    evaluation_summary_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct CleanRunsReport {
    runs_dir: PathBuf,
    exists: bool,
    dry_run: bool,
    force: bool,
    include_active: bool,
    run_count: usize,
    active_runs: Vec<String>,
    removed: bool,
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

fn main() -> Result<()> {
    lab_runner::telemetry::init();
    std::env::set_var(
        lab_runner::PROCESS_INVOKED_AT_MS_ENV,
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
    let result = run_command(cli.command);
    match result {
        Ok(Some(payload)) => {
            emit_json(&payload);
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(err) => {
            if json_mode {
                emit_json(&json_error("command_failed", err.to_string(), json!({})));
                std::process::exit(1);
            }
            Err(err)
        }
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

    let mut choice_source = if std::io::stdin().is_terminal() {
        print!("{}", prompt);
        std::io::stdout().flush()?;
        RunValidationChoiceSource::Stdin
    } else {
        let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") else {
            return Ok(None);
        };
        let mut writer = tty.try_clone()?;
        writer.write_all(prompt.as_bytes())?;
        writer.flush()?;
        RunValidationChoiceSource::Tty(BufReader::new(tty))
    };

    loop {
        let mut choice = String::new();
        choice_source.read_line(&mut choice)?;
        match parse_run_validation_choice(&choice) {
            Ok(action) => return Ok(Some(action)),
            Err(err) => {
                choice_source.write_retry_prompt(&format!("{err}\nChoose [1/2/3]: "))?;
            }
        }
    }
}

enum RunValidationChoiceSource {
    Stdin,
    Tty(BufReader<std::fs::File>),
}

impl RunValidationChoiceSource {
    fn read_line(&mut self, choice: &mut String) -> Result<()> {
        match self {
            Self::Stdin => {
                std::io::stdin().read_line(choice)?;
            }
            Self::Tty(reader) => {
                reader.read_line(choice)?;
            }
        }
        Ok(())
    }

    fn write_retry_prompt(&mut self, message: &str) -> Result<()> {
        match self {
            Self::Stdin => {
                print!("{message}");
                std::io::stdout().flush()?;
            }
            Self::Tty(reader) => {
                reader.get_mut().write_all(message.as_bytes())?;
                reader.get_mut().flush()?;
            }
        }
        Ok(())
    }
}

fn parse_run_validation_choice(choice: &str) -> Result<RunValidationAction> {
    match choice.trim() {
        "1" => Ok(RunValidationAction::SmokeTest),
        "2" => Ok(RunValidationAction::FullRun),
        "3" | "" => Ok(RunValidationAction::Cancel),
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
                if let Err(rollback_err) = fs::rename(&replaced, final_out) {
                    return Err(anyhow!(
                        "failed to publish build-run package to {}; also failed to restore replaced output from {}: {}",
                        final_out.display(),
                        replaced.display(),
                        rollback_err
                    ));
                }
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
            match fs::remove_dir_all(&temp_out) {
                Ok(()) => {}
                Err(cleanup_err) if cleanup_err.kind() == std::io::ErrorKind::NotFound => {}
                Err(cleanup_err) => {
                    eprintln!(
                        "warning: failed to remove temporary build-run output {}: {}",
                        temp_out.display(),
                        cleanup_err
                    );
                }
            }
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

fn package_checks_passed(report: &Value) -> bool {
    report
        .get("passed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn preflight_report_to_json(report: &lab_runner::PreflightReport) -> Value {
    json!({
        "ok": report.passed,
        "checks": report.checks.iter().map(|check| json!({
            "name": check.name,
            "passed": check.passed,
            "severity": match check.severity {
                lab_runner::PreflightSeverity::Error => "error",
                lab_runner::PreflightSeverity::Warning => "warning",
            },
            "message": check.message,
        })).collect::<Vec<_>>()
    })
}

fn resolve_experiment_target(target: Option<&Path>) -> Result<PathBuf> {
    resolve_experiment_target_for_command("dev", target)
}

fn resolve_experiment_target_for_command(command: &str, target: Option<&Path>) -> Result<PathBuf> {
    let path = target
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("experiment.yaml"));
    if path.is_dir() {
        let experiment = path.join("experiment.yaml");
        if experiment.is_file() {
            return Ok(experiment);
        }
        return Err(experiment_target_error(
            command,
            &path,
            "no experiment.yaml was found in directory",
        ));
    }
    if path.is_file() {
        return Ok(path);
    }
    Err(experiment_target_error(
        command,
        &path,
        "path does not exist",
    ))
}

fn is_yaml_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("yaml" | "yml")
    )
}

#[derive(Debug)]
struct InitOptions {
    dir: PathBuf,
    client: InitClientArg,
    command: Option<String>,
    url: Option<String>,
    stream: InitStreamArg,
    language: InitLanguageArg,
    mcp_role: Option<InitMcpRoleArg>,
    mcp_tool: Option<String>,
    mode: String,
    name: String,
    force: bool,
}

#[derive(Debug, Default)]
struct InitOptionArgs {
    dir: Option<PathBuf>,
    client: Option<InitClientArg>,
    command: Option<String>,
    url: Option<String>,
    stream: Option<InitStreamArg>,
    language: Option<InitLanguageArg>,
    mcp_role: Option<InitMcpRoleArg>,
    mcp_tool: Option<String>,
    mode: String,
    name: Option<String>,
    force: bool,
}

fn init_client_label(client: InitClientArg) -> &'static str {
    match client {
        InitClientArg::Cli => "cli",
        InitClientArg::Api => "api",
        InitClientArg::Acp => "acp",
        InitClientArg::Mcp => "mcp",
        InitClientArg::Sdk => "sdk",
    }
}

fn init_language_label(language: InitLanguageArg) -> &'static str {
    match language {
        InitLanguageArg::Python => "python",
        InitLanguageArg::TypeScript => "typescript",
    }
}

fn init_stream_label(stream: InitStreamArg) -> &'static str {
    match stream {
        InitStreamArg::None => "none",
        InitStreamArg::Sse => "sse",
    }
}

fn init_mcp_role_label(role: InitMcpRoleArg) -> &'static str {
    match role {
        InitMcpRoleArg::Target => "target",
        InitMcpRoleArg::Tool => "tool",
    }
}

fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut input = String::new();
    std::io::stdin().lock().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn prompt_init_client() -> Result<InitClientArg> {
    eprintln!("How does someone run your agent today?");
    eprintln!("  1. CLI command");
    eprintln!("  2. HTTP API");
    eprintln!("  3. ACP-compatible agent");
    eprintln!("  4. MCP server/tool");
    eprintln!("  5. SDK/client library");
    loop {
        match prompt_line("Choose 1-5: ")?.as_str() {
            "1" | "cli" | "CLI" => return Ok(InitClientArg::Cli),
            "2" | "api" | "http" | "API" => return Ok(InitClientArg::Api),
            "3" | "acp" | "ACP" => return Ok(InitClientArg::Acp),
            "4" | "mcp" | "MCP" => return Ok(InitClientArg::Mcp),
            "5" | "sdk" | "SDK" => return Ok(InitClientArg::Sdk),
            _ => eprintln!("Enter 1, 2, 3, 4, or 5."),
        }
    }
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_underscore = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore && !slug.is_empty() {
            slug.push('_');
            last_underscore = true;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        "my_eval".to_string()
    } else {
        slug
    }
}

fn resolve_init_options(args: InitOptionArgs) -> Result<InitOptions> {
    let InitOptionArgs {
        dir,
        client,
        command,
        url,
        stream,
        language,
        mcp_role,
        mcp_tool,
        mode,
        name,
        force,
    } = args;
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let client = match client {
        Some(client) => client,
        None if interactive => prompt_init_client()?,
        None => {
            return Err(anyhow!(
                "init requires --client when stdin is not interactive; use --client cli|api|acp|mcp|sdk"
            ));
        }
    };
    let dir = dir.unwrap_or_else(|| PathBuf::from("."));
    let default_name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != ".")
        .unwrap_or("Bucephalus Eval")
        .to_string();
    let name = name.unwrap_or(default_name);
    let command = if matches!(client, InitClientArg::Cli | InitClientArg::Acp)
        && command.is_none()
        && interactive
    {
        let value = prompt_line("Command Buc should run: ")?;
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    } else {
        command
    };
    let url = if matches!(client, InitClientArg::Api) && url.is_none() && interactive {
        let value = prompt_line("HTTP URL Buc should call: ")?;
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    } else {
        url
    };
    if matches!(client, InitClientArg::Api) && url.is_none() {
        return Err(anyhow!("init --client api requires --url"));
    }
    if matches!(client, InitClientArg::Cli | InitClientArg::Acp) && command.is_none() {
        return Err(anyhow!(
            "init --client {} requires --command",
            init_client_label(client)
        ));
    }
    Ok(InitOptions {
        dir,
        client,
        command,
        url,
        stream: stream.unwrap_or(InitStreamArg::None),
        language: language.unwrap_or(InitLanguageArg::Python),
        mcp_role,
        mcp_tool,
        mode,
        name,
        force,
    })
}

fn init_write_file(path: &Path, content: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        return Err(anyhow!(
            "refusing to overwrite {}; pass --force to replace generated files",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

fn generate_init_experiment_yaml(options: &InitOptions) -> String {
    let id = slugify(&options.name);
    format!(
        r#"experiment:
  id: {id}
  name: {name}
  mode: {mode}
  description: Generated by `bucephalus init`.
  tags: [starter]

runtime:
  network:
    agent: full

matrix:
  variants:
    - id: baseline
      baseline: true
      config: {{}}
  cases:
    source: file
    path: cases.jsonl

stages:
  case:
    interface: input_only
  agent:
    image: python:3.11-slim
    mount:
      source: ./agent
      mount:
        path: /opt/agent
        read_only: true
    command: ["python3", "/opt/agent/buc_agent.py"]
  grader:
    strategy: none

metrics:
  - id: resolved
    from: result.metrics.resolved
    direction: maximize
    primary: true

policy:
  timeout_ms: 120000
  sanitization_profile: standard_runtime
"#,
        id = id,
        name = options.name,
        mode = options.mode
    )
}

fn generate_init_cases_jsonl() -> String {
    concat!(
        r#"{"schema_version":"case_v2","id":"case-1","inputs":{"prompt":"Explain what this agent is supposed to do.","expected_keywords":["agent"]},"metadata":{"split":"smoke"}}"#,
        "\n",
        r#"{"schema_version":"case_v2","id":"case-2","inputs":{"prompt":"Return a concise answer for the second smoke case.","expected_keywords":["answer"]},"metadata":{"split":"smoke"}}"#,
        "\n"
    )
    .to_string()
}

fn generate_init_agent(options: &InitOptions) -> Result<String> {
    let command = serde_json::to_string(&options.command)?;
    let url = serde_json::to_string(&options.url)?;
    let mcp_role = serde_json::to_string(&options.mcp_role.map(init_mcp_role_label))?;
    let mcp_tool = serde_json::to_string(&options.mcp_tool)?;
    Ok(format!(
        r#"#!/usr/bin/env python3
import json
import os
import subprocess
import urllib.request

CLIENT = {client:?}
LANGUAGE = {language:?}
STREAM = {stream:?}
USER_COMMAND = {command}
API_URL = {url}
MCP_ROLE = {mcp_role}
MCP_TOOL = {mcp_tool}


def load_trial():
    path = os.environ.get("BUCEPHALUS_TRIAL_INPUT_PATH")
    if not path:
        raise RuntimeError("BUCEPHALUS_TRIAL_INPUT_PATH is required")
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def write_result(payload):
    path = os.environ.get("BUCEPHALUS_RESULT_PATH", "/bucephalus/out/result.json")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2)


def result_from_text(trial, text, resolved=1.0):
    return {{
        "answer": {{
            "summary": text.strip() or "agent completed",
            "case_id": trial.get("ids", {{}}).get("case_id"),
        }},
        "metrics": {{"resolved": resolved}},
    }}


def invoke_api(trial):
    body = json.dumps({{"trial": trial, "case": trial.get("case"), "variant": trial.get("variant")}}).encode("utf-8")
    request = urllib.request.Request(
        API_URL,
        data=body,
        headers={{"Content-Type": "application/json"}},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=120) as response:
        data = response.read().decode("utf-8")
    payload = json.loads(data)
    return payload if "metrics" in payload or "answer" in payload else result_from_text(trial, data)


def invoke_command(trial):
    input_path = os.environ["BUCEPHALUS_TRIAL_INPUT_PATH"]
    output_path = os.environ.get("BUCEPHALUS_RESULT_PATH", "/bucephalus/out/result.json")
    command = USER_COMMAND.replace("{{input}}", input_path).replace("{{output}}", output_path)
    completed = subprocess.run(command, shell=True, text=True, input=json.dumps(trial), capture_output=True)
    if os.path.exists(output_path):
        return None
    if completed.stdout.strip():
        try:
            return json.loads(completed.stdout)
        except json.JSONDecodeError:
            pass
    return {{
        "answer": {{
            "summary": completed.stdout.strip() or completed.stderr.strip() or f"command exited {{completed.returncode}}",
            "case_id": trial.get("ids", {{}}).get("case_id"),
            "client": CLIENT,
        }},
        "metrics": {{"resolved": 1.0 if completed.returncode == 0 else 0.0}},
    }}


def invoke_scaffold(trial):
    inputs = trial.get("case", {{}}).get("inputs") or trial.get("case", {{}}).get("input") or {{}}
    prompt = inputs.get("prompt", "")
    return result_from_text(trial, f"Generated {{CLIENT}} agent scaffold received: {{prompt}}", resolved=1.0)


def main():
    trial = load_trial()
    if API_URL:
        result = invoke_api(trial)
    elif USER_COMMAND:
        result = invoke_command(trial)
    else:
        result = invoke_scaffold(trial)
    if result is not None:
        write_result(result)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        write_result({{
            "answer": {{"summary": str(exc), "client": CLIENT}},
            "metrics": {{"resolved": 0.0}},
            "error": {{"message": str(exc)}},
        }})
        raise
"#,
        client = init_client_label(options.client),
        language = init_language_label(options.language),
        stream = init_stream_label(options.stream),
        command = command,
        url = url,
        mcp_role = mcp_role,
        mcp_tool = mcp_tool
    ))
}

fn generate_init_agent_readme(options: &InitOptions) -> String {
    let client = init_client_label(options.client);
    let invocation = match options.client {
        InitClientArg::Cli => "CLI: edit `buc_agent.py` to map Buc trial input into your command and parse its output.",
        InitClientArg::Api => "API: `buc_agent.py` POSTs the trial payload to your URL and writes the JSON response as the Buc result.",
        InitClientArg::Acp => "ACP: native ACP client support is not enabled in the runner yet; this generated agent is the bridge until that lands.",
        InitClientArg::Mcp => "MCP: use this agent scaffold to call the MCP server/tool for each case, or model the MCP server as a resource your agent consumes.",
        InitClientArg::Sdk => "SDK: put your SDK client calls in `buc_agent.py`; Buc owns scheduling, packaging, result capture, and comparisons.",
    };
    format!(
        r#"# Buc Agent

Client kind: `{client}`

{invocation}

Contract:
- read trial JSON from `BUCEPHALUS_TRIAL_INPUT_PATH`
- write Buc result JSON to `BUCEPHALUS_RESULT_PATH`
- return metrics under `metrics`, e.g. `{{"resolved": 1.0}}`

If your agent exposes live events, add trace collection explicitly after the
basic result path works. Do not add trace plumbing until `bucephalus dev` passes.
"#
    )
}

fn run_init(options: InitOptions) -> Result<Value> {
    let experiment_path = options.dir.join("experiment.yaml");
    let cases_path = options.dir.join("cases.jsonl");
    let agent_path = options.dir.join("agent").join("buc_agent.py");
    let agent_readme_path = options.dir.join("agent").join("README.md");
    fs::create_dir_all(&options.dir)?;
    init_write_file(
        &experiment_path,
        &generate_init_experiment_yaml(&options),
        options.force,
    )?;
    init_write_file(&cases_path, &generate_init_cases_jsonl(), options.force)?;
    init_write_file(&agent_path, &generate_init_agent(&options)?, options.force)?;
    init_write_file(
        &agent_readme_path,
        &generate_init_agent_readme(&options),
        options.force,
    )?;
    Ok(json!({
        "ok": true,
        "command": "init",
        "client": init_client_label(options.client),
        "dir": options.dir.display().to_string(),
        "experiment": experiment_path.display().to_string(),
        "cases": cases_path.display().to_string(),
        "agent": agent_path.display().to_string(),
        "next": [
            format!("bucephalus dev {}", options.dir.display()),
            format!("bucephalus run {}", experiment_path.display())
        ]
    }))
}

fn read_json_or_yaml_value(path: &Path) -> Result<Value> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("failed to read structured data file '{}'", path.display()))?;
    if let Ok(value) = serde_json::from_str::<Value>(&data) {
        return Ok(value);
    }
    lab_runner::reject_duplicate_yaml_mapping_keys(&data, path)?;
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&data)
        .with_context(|| format!("failed to parse '{}' as JSON or YAML", path.display()))?;
    serde_json::to_value(yaml_value)
        .with_context(|| format!("failed to convert YAML '{}' to JSON value", path.display()))
}

fn run_mcp_stdio() -> Result<()> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("{}", direct_mcp_invocation_message());
        return Ok(());
    }
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout();
    while let Some(message) = read_mcp_message(&mut reader)? {
        let Some(response) = handle_mcp_message(message) else {
            continue;
        };
        write_mcp_message(&mut stdout, &response)?;
    }
    Ok(())
}

fn direct_mcp_invocation_message() -> &'static str {
    "bucephalus mcp is a stdio MCP server, not an interactive command.\n\
It waits for JSON-RPC messages from an MCP host such as Claude Code, Claude Desktop, or Cursor.\n\n\
To install or refresh the local daemon and MCP registration, run:\n\
  bucephalus setup\n\n\
To check readiness, run:\n\
  bucephalus setup status\n\n\
To inspect machine-readable readiness, run:\n\
  bucephalus setup status --json"
}

const BUCEPHALUS_MCP_SERVER_NAME: &str = "bucephalus";
const LATCH_DAEMON_SERVICE_LABEL: &str = "dev.bucephalus.latchd";
const BUCEPHALUS_CLOUD_API_URL_ENV: &str = "BUCEPHALUS_CLOUD_API_URL";
const BUCEPHALUS_CLOUD_OAUTH_ISSUER_ENV: &str = "BUCEPHALUS_CLOUD_OAUTH_ISSUER";
const BUCEPHALUS_CLOUD_OAUTH_CLIENT_ID_ENV: &str = "BUCEPHALUS_CLOUD_OAUTH_CLIENT_ID";
const BUCEPHALUS_CLOUD_OAUTH_AUDIENCE_ENV: &str = "BUCEPHALUS_CLOUD_OAUTH_AUDIENCE";
const BUCEPHALUS_CLOUD_OAUTH_SCOPE_ENV: &str = "BUCEPHALUS_CLOUD_OAUTH_SCOPE";
const DEFAULT_BUCEPHALUS_REPO: &str = "nishiokj/Bucephalus";
const DISPATCH_SCHEMA: &str = "latch_dispatch_v1";

#[derive(Debug, Clone)]
struct CloudTokenPaths {
    access: PathBuf,
    refresh: PathBuf,
    cache: PathBuf,
}

#[derive(Debug, Clone)]
struct DeviceLoginOptions {
    issuer: Option<String>,
    client_id: Option<String>,
    audience: Option<String>,
    resource: Option<String>,
    scope: Option<String>,
    no_browser: bool,
}

#[derive(Debug, Clone)]
struct UpdateOptions {
    version: Option<String>,
    install_dir: Option<PathBuf>,
    repo: Option<String>,
    base_url: Option<String>,
    setup: bool,
    no_modify_path: bool,
    dry_run: bool,
}

fn run_setup(
    clients: Vec<SetupMcpClientArg>,
    project: Option<PathBuf>,
    no_daemon_service: bool,
    no_start: bool,
    no_mcp: bool,
    dry_run: bool,
) -> Result<Value> {
    let exe = std::env::current_exe()
        .map_err(|err| anyhow!("failed to resolve current executable path: {}", err))?;
    let home = lab_runner::bucephalus_home()?;
    if !dry_run {
        fs::create_dir_all(&home)?;
    }

    let daemon = if no_daemon_service {
        json!({
            "status": "skipped",
            "reason": "--no-daemon-service"
        })
    } else {
        install_latch_daemon_service(&exe, &home, !no_start, dry_run)?
    };

    let mcp = if no_mcp {
        json!({
            "status": "skipped",
            "reason": "--no-mcp"
        })
    } else {
        register_mcp_clients(&exe, clients, project.as_deref(), dry_run)?
    };

    let daemon_status = if dry_run || no_start {
        json!({
            "status": "not_checked",
            "reason": if dry_run { "dry_run" } else { "--no-start" }
        })
    } else {
        match latch_daemon::ensure_latch_daemon() {
            Ok(info) => json!({
                "status": "ready",
                "pid": info.pid,
                "address": info.address,
                "state_path": info.state_path,
                "log_path": info.log_path
            }),
            Err(err) => json!({
                "status": "error",
                "error": err.to_string()
            }),
        }
    };

    Ok(json!({
        "schema_version": "bucephalus_setup_v1",
        "ok": true,
        "dry_run": dry_run,
        "binary": exe,
        "home": home,
        "daemon_service": daemon,
        "daemon_status": daemon_status,
        "mcp": mcp,
        "auth": auth_status(&home)
    }))
}

fn run_setup_status(project: Option<&Path>) -> Result<Value> {
    let exe = std::env::current_exe()
        .map_err(|err| anyhow!("failed to resolve current executable path: {}", err))?;
    let home = lab_runner::bucephalus_home()?;
    let daemon_service = latch_daemon_service_status()?;
    let daemon_status = match latch_daemon::current_latch_daemon() {
        Ok(Some(info)) => json!({
            "status": "ready",
            "pid": info.pid,
            "address": info.address,
            "state_path": info.state_path,
            "log_path": info.log_path
        }),
        Ok(None) => json!({
            "status": "not_running"
        }),
        Err(err) => json!({
            "status": "error",
            "error": err.to_string()
        }),
    };
    Ok(json!({
        "schema_version": "bucephalus_setup_status_v1",
        "ok": true,
        "binary": exe,
        "home": home,
        "daemon_service": daemon_service,
        "daemon_status": daemon_status,
        "mcp": mcp_registration_status(project),
        "auth": auth_status(&home)
    }))
}

fn run_setup_uninstall(
    project: Option<&Path>,
    clients: Vec<SetupMcpClientArg>,
    no_daemon_service: bool,
    no_mcp: bool,
    dry_run: bool,
) -> Result<Value> {
    let home = lab_runner::bucephalus_home()?;
    let daemon_service = if no_daemon_service {
        json!({
            "status": "skipped",
            "reason": "--no-daemon-service"
        })
    } else {
        uninstall_latch_daemon_service(dry_run)?
    };
    let mcp = if no_mcp {
        json!({
            "status": "skipped",
            "reason": "--no-mcp"
        })
    } else {
        unregister_mcp_clients(clients, project, dry_run)?
    };
    Ok(json!({
        "schema_version": "bucephalus_setup_uninstall_v1",
        "ok": true,
        "dry_run": dry_run,
        "home": home,
        "daemon_service": daemon_service,
        "mcp": mcp,
        "auth": auth_status(&home)
    }))
}

fn cloud_token_paths(home: &Path) -> CloudTokenPaths {
    let auth_dir = home.join("auth");
    CloudTokenPaths {
        access: auth_dir.join("cloud_user_token"),
        refresh: auth_dir.join("cloud_refresh_token"),
        cache: auth_dir.join("cloud_user_token.json"),
    }
}

fn auth_status(home: &Path) -> Value {
    let paths = cloud_token_paths(home);
    if std::env::var_os(BUCEPHALUS_CLOUD_USER_TOKEN_ENV).is_some() {
        return json!({
            "status": "ready",
            "source": "env",
            "env": BUCEPHALUS_CLOUD_USER_TOKEN_ENV,
            "api_url": cloud_api_base_url()
        });
    }
    if paths.access.is_file() {
        return json!({
            "status": "ready",
            "source": "file",
            "path": paths.access,
            "refresh_token_path": if paths.refresh.is_file() { Some(paths.refresh.display().to_string()) } else { None },
            "cache_path": if paths.cache.is_file() { Some(paths.cache.display().to_string()) } else { None },
            "api_url": cloud_api_base_url()
        });
    }
    json!({
        "status": "missing",
        "source": null,
        "expected": [
            BUCEPHALUS_CLOUD_USER_TOKEN_ENV,
            paths.access.display().to_string()
        ],
        "api_url": cloud_api_base_url(),
        "note": "Local Core and latch smoke fixtures do not require Cloud auth. Cloud-backed benchmark resolution and result submission require first-party user auth.",
        "actions": [
            {
                "type": "cli_command",
                "command": "bucephalus login",
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

fn run_logout(dry_run: bool) -> Result<Value> {
    let home = lab_runner::bucephalus_home()?;
    let paths = cloud_token_paths(&home);
    let env_token_present = std::env::var_os(BUCEPHALUS_CLOUD_USER_TOKEN_ENV).is_some();
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
            "name": BUCEPHALUS_CLOUD_USER_TOKEN_ENV,
            "present": env_token_present,
            "note": if env_token_present {
                Some(format!("{} is still set in this process; unset it in your shell or environment manager to fully log out.", BUCEPHALUS_CLOUD_USER_TOKEN_ENV))
            } else {
                None
            }
        },
        "auth": auth_status(&home)
    }))
}

fn run_login(options: DeviceLoginOptions) -> Result<Value> {
    let home = lab_runner::bucephalus_home()?;
    let paths = cloud_token_paths(&home);
    // Resolution order everywhere: explicit flag, then env, then the cloud
    // profile persisted by a previous login. First login pins the deployment;
    // later logins are zero-configuration.
    let issuer = options
        .issuer
        .or_else(|| env_trimmed(BUCEPHALUS_CLOUD_OAUTH_ISSUER_ENV))
        .or_else(|| lab_runner::cloud_profile_string(&home, "/oauth/issuer"))
        .ok_or_else(|| {
            anyhow!(
                "OAuth issuer is required; pass --issuer or set {}",
                BUCEPHALUS_CLOUD_OAUTH_ISSUER_ENV
            )
        })?;
    let audience = options
        .audience
        .or_else(|| env_trimmed(BUCEPHALUS_CLOUD_OAUTH_AUDIENCE_ENV))
        .or_else(|| lab_runner::cloud_profile_string(&home, "/oauth/audience"));
    let resource = options.resource.or_else(cloud_api_base_url);
    let scope = options
        .scope
        .or_else(|| env_trimmed(BUCEPHALUS_CLOUD_OAUTH_SCOPE_ENV))
        .or_else(|| lab_runner::cloud_profile_string(&home, "/oauth/scope"))
        .unwrap_or_else(|| "openid profile email".to_string());
    let (metadata_url, metadata) = fetch_oauth_metadata(&issuer)?;
    let device_authorization_endpoint = metadata
        .get("device_authorization_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!(
                "OAuth metadata {} does not include device_authorization_endpoint",
                metadata_url
            )
        })?
        .to_string();
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
        .or_else(|| lab_runner::cloud_profile_string(&home, "/oauth/client_id"))
        .map(Ok)
        .unwrap_or_else(|| dynamic_register_oauth_client(&metadata, &issuer, &scope))?;

    let device = begin_device_authorization(
        &device_authorization_endpoint,
        &client_id,
        &scope,
        audience.as_deref(),
        resource.as_deref(),
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

    let token = poll_device_token(&token_endpoint, &client_id, &device)?;
    write_cloud_token_cache(
        &paths,
        &issuer,
        &client_id,
        audience.as_deref(),
        resource.as_deref(),
        &scope,
        &token_endpoint,
        &token,
    )?;
    lab_runner::write_cloud_profile(
        &home,
        &json!({
            "schema_version": "bucephalus_cloud_profile_v1",
            "api_url": resource,
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
        "resource": resource,
        "scope": scope,
        "token_path": paths.access,
        "refresh_token_path": if paths.refresh.is_file() { Some(paths.refresh.display().to_string()) } else { None },
        "cache_path": paths.cache
    }))
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

fn http_get_json(url: &str) -> Result<Value> {
    let response = http_request(Method::GET, url, None, None)?;
    if !(200..300).contains(&response.status) {
        let message = String::from_utf8_lossy(&response.body);
        return Err(anyhow!(
            "GET {} failed with status {}: {}",
            url,
            response.status,
            message.trim()
        ));
    }
    Ok(serde_json::from_slice(&response.body)?)
}

fn dynamic_register_oauth_client(metadata: &Value, issuer: &str, scope: &str) -> Result<String> {
    let registration_endpoint = metadata
        .get("registration_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!(
                "OAuth client_id is required; set {} or use an issuer with dynamic client registration",
                BUCEPHALUS_CLOUD_OAUTH_CLIENT_ID_ENV
            )
        })?;
    let body = dynamic_client_registration_body(scope);
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(registration_endpoint)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body)?)
        .send()
        .with_context(|| format!("failed to register OAuth client with {}", issuer))?;
    let status = response.status().as_u16();
    let bytes = response.bytes()?.to_vec();
    if !(200..300).contains(&status) {
        let message = String::from_utf8_lossy(&bytes);
        return Err(anyhow!(
            "OAuth dynamic client registration failed with status {}: {}",
            status,
            message.trim()
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    value
        .get("client_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("OAuth dynamic client registration response missing client_id"))
}

fn dynamic_client_registration_body(scope: &str) -> Value {
    json!({
        "client_name": "Bucephalus CLI",
        "application_type": "native",
        "grant_types": ["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"],
        "token_endpoint_auth_method": "none",
        "scope": scope
    })
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
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(endpoint)
        .form(&form)
        .send()
        .with_context(|| format!("failed to start device authorization at {}", endpoint))?;
    let status = response.status().as_u16();
    let bytes = response.bytes()?.to_vec();
    if !(200..300).contains(&status) {
        let message = String::from_utf8_lossy(&bytes);
        return Err(anyhow!(
            "device authorization failed with status {}: {}",
            status,
            message.trim()
        ));
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
    let client = reqwest::blocking::Client::new();
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
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("token response missing access_token"))?;
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

fn run_update(options: UpdateOptions) -> Result<Value> {
    let install_dir = options
        .install_dir
        .map(Ok)
        .unwrap_or_else(default_update_install_dir)?;
    let repo = options
        .repo
        .or_else(|| env_trimmed("BUCEPHALUS_REPO"))
        .unwrap_or_else(|| DEFAULT_BUCEPHALUS_REPO.to_string());
    let version = options
        .version
        .or_else(|| env_trimmed("BUCEPHALUS_VERSION"))
        .unwrap_or_else(|| "latest".to_string());
    let installer_url = install_script_url(&repo)?;
    let mut env = BTreeMap::new();
    env.insert(
        "BUCEPHALUS_INSTALL_DIR".to_string(),
        install_dir.display().to_string(),
    );
    env.insert("BUCEPHALUS_REPO".to_string(), repo.clone());
    env.insert("BUCEPHALUS_VERSION".to_string(), version.clone());
    env.insert(
        "BUCEPHALUS_SETUP".to_string(),
        if options.setup { "1" } else { "0" }.to_string(),
    );
    if options.no_modify_path {
        env.insert("BUCEPHALUS_NO_MODIFY_PATH".to_string(), "1".to_string());
    }
    if let Some(base_url) = options
        .base_url
        .or_else(|| env_trimmed("BUCEPHALUS_BASE_URL"))
    {
        env.insert("BUCEPHALUS_BASE_URL".to_string(), base_url);
    }
    let plan = json!({
        "schema_version": "bucephalus_update_v1",
        "ok": true,
        "dry_run": options.dry_run,
        "installer_url": installer_url,
        "install_dir": install_dir,
        "version": version,
        "repo": repo,
        "setup": options.setup,
        "no_modify_path": options.no_modify_path,
        "env": env
    });
    if options.dry_run {
        return Ok(plan);
    }

    let script = http_download_text(&installer_url)?;
    let tmp_dir = update_temp_dir()?;
    let script_path = tmp_dir.join("install.sh");
    let result = (|| {
        fs::write(&script_path, script)?;
        let mut command = Command::new("sh");
        command.arg(&script_path);
        for (key, value) in &env {
            command.env(key, value);
        }
        let status = command.status()?;
        if !status.success() {
            return Err(anyhow!("installer failed with status {}", status));
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&tmp_dir);
    result?;
    let installed_version = installed_bucephalus_version(&install_dir);

    Ok(json!({
        "schema_version": "bucephalus_update_v1",
        "ok": true,
        "dry_run": false,
        "installer_url": plan["installer_url"],
        "install_dir": plan["install_dir"],
        "version": plan["version"],
        "repo": plan["repo"],
        "setup": plan["setup"],
        "no_modify_path": plan["no_modify_path"],
        "installed_version": installed_version,
        "updated": true
    }))
}

fn installed_bucephalus_version(install_dir: &Path) -> Option<String> {
    let output = Command::new(install_dir.join("bucephalus"))
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    text.split_whitespace().nth(1).map(str::to_string)
}

fn default_update_install_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|err| anyhow!("failed to resolve current executable path: {}", err))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("failed to resolve install directory from {}", exe.display()))
}

fn install_script_url(repo: &str) -> Result<String> {
    let repo = validate_github_repo_slug(repo)?;
    Ok(format!(
        "https://raw.githubusercontent.com/{repo}/main/scripts/install.sh"
    ))
}

fn validate_github_repo_slug(repo: &str) -> Result<String> {
    let repo = repo.trim();
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(anyhow!(
            "invalid BUCEPHALUS_REPO value '{}': expected GitHub owner/repo",
            repo
        ));
    };
    if owner.is_empty()
        || name.is_empty()
        || name.contains('/')
        || matches!(owner, "." | "..")
        || matches!(name, "." | "..")
    {
        return Err(anyhow!(
            "invalid BUCEPHALUS_REPO value '{}': expected GitHub owner/repo",
            repo
        ));
    }
    if !owner
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
    {
        return Err(anyhow!(
            "invalid BUCEPHALUS_REPO owner '{}': use letters, numbers, '.', '_', or '-'",
            owner
        ));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
    {
        return Err(anyhow!(
            "invalid BUCEPHALUS_REPO repo '{}': use letters, numbers, '.', '_', or '-'",
            name
        ));
    }
    Ok(format!("{owner}/{name}"))
}

fn http_download_text(url: &str) -> Result<String> {
    let response = http_request(Method::GET, url, None, None)?;
    if !(200..300).contains(&response.status) {
        let message = String::from_utf8_lossy(&response.body);
        return Err(anyhow!(
            "download {} failed with status {}: {}",
            url,
            response.status,
            message.trim()
        ));
    }
    String::from_utf8(response.body).context("downloaded installer was not valid UTF-8")
}

fn update_temp_dir() -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "bucephalus-update-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn install_latch_daemon_service(
    exe: &Path,
    home: &Path,
    start: bool,
    dry_run: bool,
) -> Result<Value> {
    #[cfg(target_os = "macos")]
    {
        install_launchd_latch_daemon_service(exe, home, start, dry_run)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        install_systemd_latch_daemon_service(exe, home, start, dry_run)
    }

    #[cfg(not(unix))]
    {
        let _ = (exe, home, start, dry_run);
        Ok(json!({
            "status": "unsupported",
            "reason": "Tier-1 daemon service setup is currently implemented for macOS launchd and Linux systemd user services"
        }))
    }
}

fn latch_daemon_service_status() -> Result<Value> {
    #[cfg(target_os = "macos")]
    {
        launchd_latch_daemon_service_status()
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        systemd_latch_daemon_service_status()
    }

    #[cfg(not(unix))]
    {
        Ok(json!({
            "status": "unsupported",
            "reason": "Tier-1 daemon service setup is currently implemented for macOS launchd and Linux systemd user services"
        }))
    }
}

fn uninstall_latch_daemon_service(dry_run: bool) -> Result<Value> {
    #[cfg(target_os = "macos")]
    {
        uninstall_launchd_latch_daemon_service(dry_run)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        uninstall_systemd_latch_daemon_service(dry_run)
    }

    #[cfg(not(unix))]
    {
        let _ = dry_run;
        Ok(json!({
            "status": "unsupported",
            "reason": "Tier-1 daemon service setup is currently implemented for macOS launchd and Linux systemd user services"
        }))
    }
}

#[cfg(target_os = "macos")]
fn launchd_latch_daemon_plist_path() -> Result<PathBuf> {
    Ok(user_home_dir()?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LATCH_DAEMON_SERVICE_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> Result<String> {
    Ok(format!("gui/{}", current_uid_string()?))
}

#[cfg(target_os = "macos")]
fn launchd_latch_daemon_service_status() -> Result<Value> {
    let plist_path = launchd_latch_daemon_plist_path()?;
    let domain = launchd_domain()?;
    let service = format!("{domain}/{LATCH_DAEMON_SERVICE_LABEL}");
    let loaded = Command::new("launchctl")
        .arg("print")
        .arg(&service)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    Ok(json!({
        "manager": "launchd",
        "label": LATCH_DAEMON_SERVICE_LABEL,
        "path": plist_path,
        "installed": plist_path.is_file(),
        "loaded": loaded,
        "status": if loaded { "loaded" } else if plist_path.is_file() { "installed" } else { "missing" }
    }))
}

#[cfg(target_os = "macos")]
fn uninstall_launchd_latch_daemon_service(dry_run: bool) -> Result<Value> {
    let plist_path = launchd_latch_daemon_plist_path()?;
    let domain = launchd_domain()?;
    let commands = vec![vec![
        "launchctl".to_string(),
        "bootout".to_string(),
        domain,
        plist_path.display().to_string(),
    ]];
    if !dry_run {
        let _ = run_command_status(&commands[0]);
        if plist_path.exists() {
            fs::remove_file(&plist_path)?;
        }
    }
    Ok(json!({
        "status": if dry_run { "planned" } else { "removed" },
        "manager": "launchd",
        "label": LATCH_DAEMON_SERVICE_LABEL,
        "path": plist_path,
        "commands": commands
    }))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_latch_daemon_service_path() -> PathBuf {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            user_home_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".config")
        });
    config_home
        .join("systemd")
        .join("user")
        .join("bucephalus-latchd.service")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_latch_daemon_service_status() -> Result<Value> {
    let service_path = systemd_latch_daemon_service_path();
    let active = Command::new("systemctl")
        .arg("--user")
        .arg("is-active")
        .arg("bucephalus-latchd.service")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    let enabled = Command::new("systemctl")
        .arg("--user")
        .arg("is-enabled")
        .arg("bucephalus-latchd.service")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    Ok(json!({
        "manager": "systemd-user",
        "label": "bucephalus-latchd.service",
        "path": service_path,
        "installed": service_path.is_file(),
        "loaded": active,
        "enabled": enabled,
        "status": if active { "loaded" } else if service_path.is_file() { "installed" } else { "missing" }
    }))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn uninstall_systemd_latch_daemon_service(dry_run: bool) -> Result<Value> {
    let service_path = systemd_latch_daemon_service_path();
    let commands = vec![
        vec![
            "systemctl".to_string(),
            "--user".to_string(),
            "disable".to_string(),
            "--now".to_string(),
            "bucephalus-latchd.service".to_string(),
        ],
        vec![
            "systemctl".to_string(),
            "--user".to_string(),
            "daemon-reload".to_string(),
        ],
    ];
    if !dry_run {
        let _ = run_command_status(&commands[0]);
        if service_path.exists() {
            fs::remove_file(&service_path)?;
        }
        let _ = run_command_status(&commands[1]);
    }
    Ok(json!({
        "status": if dry_run { "planned" } else { "removed" },
        "manager": "systemd-user",
        "label": "bucephalus-latchd.service",
        "path": service_path,
        "commands": commands
    }))
}

#[cfg(target_os = "macos")]
fn install_launchd_latch_daemon_service(
    exe: &Path,
    home: &Path,
    start: bool,
    dry_run: bool,
) -> Result<Value> {
    let home_dir = user_home_dir()?;
    let launch_agents = home_dir.join("Library").join("LaunchAgents");
    let plist_path = launch_agents.join(format!("{LATCH_DAEMON_SERVICE_LABEL}.plist"));
    let daemon_dir = home.join("daemon");
    let stdout_path = daemon_dir.join("latchd.launchd.out.log");
    let stderr_path = daemon_dir.join("latchd.launchd.err.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>BUCEPHALUS_HOME</key>
    <string>{}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
        xml_escape(LATCH_DAEMON_SERVICE_LABEL),
        xml_escape(&exe.display().to_string()),
        xml_escape(&home.display().to_string()),
        xml_escape(&stdout_path.display().to_string()),
        xml_escape(&stderr_path.display().to_string())
    );
    let uid = current_uid_string()?;
    let domain = format!("gui/{uid}");
    let service = format!("{domain}/{LATCH_DAEMON_SERVICE_LABEL}");
    let commands = vec![
        vec![
            "launchctl".to_string(),
            "bootout".to_string(),
            domain.clone(),
            plist_path.display().to_string(),
        ],
        vec![
            "launchctl".to_string(),
            "bootstrap".to_string(),
            domain.clone(),
            plist_path.display().to_string(),
        ],
        vec![
            "launchctl".to_string(),
            "kickstart".to_string(),
            "-k".to_string(),
            service,
        ],
    ];

    if !dry_run {
        fs::create_dir_all(&launch_agents)?;
        fs::create_dir_all(&daemon_dir)?;
        fs::write(&plist_path, plist)?;
        if start {
            let _ = run_command_status(&commands[0]);
            run_command_status(&commands[1])?;
            run_command_status(&commands[2])?;
        }
    }

    Ok(json!({
        "status": if dry_run { "planned" } else { "installed" },
        "manager": "launchd",
        "label": LATCH_DAEMON_SERVICE_LABEL,
        "path": plist_path,
        "start": start,
        "commands": commands,
    }))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_systemd_latch_daemon_service(
    exe: &Path,
    home: &Path,
    start: bool,
    dry_run: bool,
) -> Result<Value> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            user_home_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".config")
        });
    let systemd_dir = config_home.join("systemd").join("user");
    let service_name = "bucephalus-latchd.service";
    let service_path = systemd_dir.join(service_name);
    let daemon_dir = home.join("daemon");
    let service = format!(
        r#"[Unit]
Description=Bucephalus Tier-1 latch daemon

[Service]
Type=simple
ExecStart={} daemon
Environment=BUCEPHALUS_HOME={}
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
"#,
        systemd_quote(&exe.display().to_string()),
        systemd_quote(&home.display().to_string())
    );
    let commands = vec![
        vec![
            "systemctl".to_string(),
            "--user".to_string(),
            "daemon-reload".to_string(),
        ],
        vec![
            "systemctl".to_string(),
            "--user".to_string(),
            "enable".to_string(),
            "--now".to_string(),
            service_name.to_string(),
        ],
    ];
    if !dry_run {
        fs::create_dir_all(&systemd_dir)?;
        fs::create_dir_all(&daemon_dir)?;
        fs::write(&service_path, service)?;
        if start {
            run_command_status(&commands[0])?;
            run_command_status(&commands[1])?;
        }
    }
    Ok(json!({
        "status": if dry_run { "planned" } else { "installed" },
        "manager": "systemd-user",
        "label": service_name,
        "path": service_path,
        "start": start,
        "commands": commands,
    }))
}

fn register_mcp_clients(
    exe: &Path,
    requested_clients: Vec<SetupMcpClientArg>,
    project: Option<&Path>,
    dry_run: bool,
) -> Result<Value> {
    let clients = resolve_setup_clients(requested_clients, project)?;
    let server_config = json!({
        "type": "stdio",
        "command": exe.display().to_string(),
        "args": ["mcp"]
    });
    if clients.is_empty() {
        return Ok(json!({
            "status": "skipped",
            "server_name": BUCEPHALUS_MCP_SERVER_NAME,
            "server_config": server_config,
            "clients": [],
            "reason": "no supported MCP clients were detected for automatic registration",
            "actions": [
                {
                    "type": "cli_command",
                    "command": "bucephalus setup --client claude-code",
                    "description": "Register with Claude Code after the claude CLI is installed."
                },
                {
                    "type": "cli_command",
                    "command": "bucephalus setup --client cursor-project --project <project-dir>",
                    "description": "Register a project-local Cursor MCP config."
                }
            ]
        }));
    }
    let mut results = Vec::new();
    for client in clients {
        results.push(register_mcp_client(
            exe,
            client,
            project,
            &server_config,
            dry_run,
        )?);
    }
    Ok(json!({
        "status": "configured",
        "server_name": BUCEPHALUS_MCP_SERVER_NAME,
        "server_config": server_config,
        "clients": results
    }))
}

fn mcp_registration_status(project: Option<&Path>) -> Value {
    let server_config = json!({
        "type": "stdio",
        "args": ["mcp"]
    });
    let mut clients = Vec::new();
    let claude_code_present = command_exists("claude");
    clients.push(json!({
        "client": "claude-code",
        "status": if claude_code_present { "available" } else { "not_detected" },
        "configured": null,
        "note": "Claude Code registration is managed by the claude CLI; run setup to refresh it."
    }));
    if let Some(path) = claude_desktop_config_path() {
        clients.push(json!({
            "client": "claude-desktop",
            "status": if mcp_config_has_server(&path, BUCEPHALUS_MCP_SERVER_NAME) { "configured" } else { "missing" },
            "configured": mcp_config_has_server(&path, BUCEPHALUS_MCP_SERVER_NAME),
            "path": path
        }));
    }
    if let Some(project) = project {
        let path = project.join(".cursor").join("mcp.json");
        clients.push(json!({
            "client": "cursor-project",
            "status": if mcp_config_has_server(&path, BUCEPHALUS_MCP_SERVER_NAME) { "configured" } else { "missing" },
            "configured": mcp_config_has_server(&path, BUCEPHALUS_MCP_SERVER_NAME),
            "path": path
        }));
    }
    json!({
        "status": "checked",
        "server_name": BUCEPHALUS_MCP_SERVER_NAME,
        "expected_server_config": server_config,
        "clients": clients
    })
}

fn cloud_api_base_url() -> Option<String> {
    std::env::var(BUCEPHALUS_CLOUD_API_URL_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let home = lab_runner::bucephalus_home().ok()?;
            lab_runner::cloud_profile_string(&home, "/api_url")
                .map(|value| value.trim_end_matches('/').to_string())
        })
}

fn cloud_bearer_token() -> Result<Option<String>> {
    if let Ok(value) = std::env::var(BUCEPHALUS_CLOUD_USER_TOKEN_ENV) {
        let token = value.trim();
        if !token.is_empty() {
            return Ok(Some(token.to_string()));
        }
    }
    let paths = match lab_runner::bucephalus_home() {
        Ok(home) => cloud_token_paths(&home),
        Err(_) => return Ok(None),
    };
    if let Some(cache) = read_cloud_token_cache(&paths) {
        if cloud_token_cache_needs_refresh(&cache) {
            return refresh_cloud_token_cache(&paths, &cache)
                .map(Some)
                .context("failed to refresh cached Cloud OAuth token");
        } else {
            if let Some(token) = cache.get("access_token").and_then(Value::as_str) {
                return Ok(Some(token.to_string()));
            }
        }
    }
    Ok(fs::read_to_string(paths.access)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
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
    let client = reqwest::blocking::Client::new();
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
    let issuer = cache.get("issuer").and_then(Value::as_str).unwrap_or("");
    let audience = cache.get("audience").and_then(Value::as_str);
    let resource = cache.get("resource").and_then(Value::as_str);
    let scope = cache.get("scope").and_then(Value::as_str).unwrap_or("");
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
    write_cloud_token_cache(
        paths,
        issuer,
        client_id,
        audience,
        resource,
        scope,
        token_endpoint,
        &merged,
    )?;
    merged
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Cloud token refresh response missing access_token"))
}

fn cloud_json_post(path: &str, body: &Value) -> Result<Value> {
    let base = cloud_api_base_url().ok_or_else(|| {
        anyhow!(
            "{} is required for remote benchmark resolution",
            BUCEPHALUS_CLOUD_API_URL_ENV
        )
    })?;
    let url = format!("{base}{path}");
    let bytes = serde_json::to_vec(body)?;
    let bearer = cloud_bearer_token()?;
    if bearer.is_none() {
        return Err(cloud_user_auth_required_error(
            "remote benchmark resolution or Cloud upload",
            false,
            None,
        ));
    }
    let response = http_request(Method::POST, &url, Some(bytes), bearer.clone())?;
    if !(200..300).contains(&response.status) {
        let message = String::from_utf8_lossy(&response.body);
        if response.status == 401 {
            return Err(cloud_user_auth_required_error(
                "Cloud request",
                bearer.is_some(),
                Some(message.trim()),
            ));
        }
        return Err(cloud_request_error(
            "Cloud request",
            path,
            response.status,
            message.trim(),
        ));
    }
    Ok(serde_json::from_slice(&response.body)?)
}

fn cloud_bytes_put(path: &str, bytes: Vec<u8>, media_type: &str) -> Result<Value> {
    let base = cloud_api_base_url().ok_or_else(|| {
        anyhow!(
            "{} is required for Cloud upload",
            BUCEPHALUS_CLOUD_API_URL_ENV
        )
    })?;
    let url = format!("{base}{path}");
    let bearer = cloud_bearer_token()?;
    if bearer.is_none() {
        return Err(cloud_user_auth_required_error("Cloud upload", false, None));
    }
    let response = http_request_with_content_type(
        Method::PUT,
        &url,
        Some(bytes),
        bearer.clone(),
        Some(media_type),
    )?;
    if !(200..300).contains(&response.status) {
        let message = String::from_utf8_lossy(&response.body);
        if response.status == 401 {
            return Err(cloud_user_auth_required_error(
                "Cloud upload",
                bearer.is_some(),
                Some(message.trim()),
            ));
        }
        return Err(cloud_request_error(
            "Cloud upload",
            path,
            response.status,
            message.trim(),
        ));
    }
    Ok(serde_json::from_slice(&response.body)?)
}

fn cloud_request_error(kind: &str, path: &str, status: u16, message: &str) -> anyhow::Error {
    anyhow!("{kind} {path} failed with status {status}: {message}")
}

fn cloud_user_auth_required_error(
    operation: &str,
    sent_token: bool,
    server_message: Option<&str>,
) -> anyhow::Error {
    let detail = cloud_user_auth_hint(operation, sent_token, server_message);
    anyhow!("{detail}")
}

fn cloud_user_auth_hint(operation: &str, sent_token: bool, server_message: Option<&str>) -> String {
    let token_path = lab_runner::bucephalus_home()
        .ok()
        .map(|home| cloud_token_paths(&home).access);
    let message = server_message
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|message| {
            format!("{operation} requires Cloud authentication.\n\nCloud response: {message}")
        })
        .unwrap_or_else(|| format!("{operation} requires Cloud authentication."));
    cloud_auth_ux::user_auth_hint(&message, sent_token, token_path.as_deref())
}

fn http_download(url: &str) -> Result<Vec<u8>> {
    let response = http_request(Method::GET, url, None, material_download_bearer(url)?)?;
    if !(200..300).contains(&response.status) {
        return Err(anyhow!(
            "download {} failed with status {}",
            url,
            response.status
        ));
    }
    Ok(response.body)
}

fn material_download_bearer(url: &str) -> Result<Option<String>> {
    if !is_same_cloud_origin(url) {
        return Ok(None);
    }
    cloud_bearer_token()
}

fn is_same_cloud_origin(url: &str) -> bool {
    let Ok(target) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(base) = cloud_api_base_url() else {
        return false;
    };
    let Ok(base) = reqwest::Url::parse(&base) else {
        return false;
    };
    target.scheme() == base.scheme()
        && target.host_str() == base.host_str()
        && target.port_or_known_default() == base.port_or_known_default()
}

struct HttpResponseBody {
    status: u16,
    body: Vec<u8>,
}

fn http_request(
    method: Method,
    url: &str,
    body: Option<Vec<u8>>,
    bearer: Option<String>,
) -> Result<HttpResponseBody> {
    http_request_with_content_type(method, url, body, bearer, Some("application/json"))
}

fn http_request_with_content_type(
    method: Method,
    url: &str,
    body: Option<Vec<u8>>,
    bearer: Option<String>,
    content_type: Option<&str>,
) -> Result<HttpResponseBody> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("invalid URL {}", url))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(anyhow!(
            "unsupported URL scheme for {}; expected http:// or https://",
            url
        ));
    }
    let client = reqwest::blocking::Client::new();
    let mut request = client.request(method, parsed);
    if let Some(token) = bearer
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.header("content-length", body.len().to_string());
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        request = request.body(body);
    }
    let response = request
        .send()
        .with_context(|| format!("failed to send request to {}", url))?;
    let status = response.status().as_u16();
    let body = response
        .bytes()
        .with_context(|| format!("failed to read response from {}", url))?
        .to_vec();
    Ok(HttpResponseBody { status, body })
}

fn dispatch_root() -> Result<PathBuf> {
    Ok(lab_runner::bucephalus_home()?.join("dispatches"))
}

fn dispatch_dir(dispatch_id: &str) -> Result<PathBuf> {
    Ok(dispatch_root()?.join(sanitize_local_id(dispatch_id)?))
}

fn sanitize_local_id(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("id must not be empty"));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(anyhow!("id contains unsupported characters: {}", value));
    }
    Ok(trimmed.to_string())
}

fn dispatch_record_path(dispatch_id: &str) -> Result<PathBuf> {
    Ok(dispatch_dir(dispatch_id)?.join("dispatch.json"))
}

fn write_dispatch_record(record: &Value) -> Result<()> {
    let path = record
        .get("record_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("dispatch record missing record_path"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(record)?)?;
    Ok(())
}

fn read_dispatch_record(dispatch_id: &str) -> Result<Value> {
    let path = dispatch_record_path(dispatch_id)?;
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read dispatch record {}", path.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

fn public_dispatch_record(record: &Value) -> Value {
    let mut public = record.clone();
    if let Some(object) = public.as_object_mut() {
        object.remove("internal");
        object.remove("record_path");
        if let Some(paths) = object.get_mut("paths").and_then(Value::as_object_mut) {
            paths.remove("dispatch_dir");
            paths.remove("manifest");
            paths.remove("resolution");
            paths.remove("run_root");
        }
        if let Some(summary) = object.get_mut("summary").and_then(Value::as_object_mut) {
            summary.remove("run_dir");
        }
    }
    public
}

fn dispatch_status_from_daemon_status(daemon_status: &str) -> &'static str {
    match daemon_status {
        "running" => "running",
        "completed" => "local_completed",
        "failed" => "failed",
        "cancelled" => "cancelled",
        _ => "unknown",
    }
}

fn refresh_dispatch(dispatch_id: &str) -> Result<Value> {
    let mut record = read_dispatch_record(dispatch_id)?;
    let job_id = record
        .pointer("/internal/job_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("dispatch '{}' is missing internal job id", dispatch_id))?;
    let daemon = match latch_daemon::call_latch_daemon(latch_daemon::LatchDaemonRequest {
        method: "progress".to_string(),
        params: json!({ "job_id": job_id }),
    }) {
        Ok(value) => value,
        Err(err) => fallback_dispatch_progress_from_run_root(&record, &err)?,
    };
    let status = daemon
        .get("status")
        .and_then(Value::as_str)
        .map(dispatch_status_from_daemon_status)
        .unwrap_or("unknown");
    let now = Utc::now().to_rfc3339();
    let cases = daemon
        .pointer("/result/cases")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let completed_cases = cases
        .iter()
        .filter(|case| case.get("status").and_then(Value::as_str) == Some("completed"))
        .count();
    let failed_cases = cases
        .iter()
        .filter(|case| {
            matches!(
                case.get("status").and_then(Value::as_str),
                Some("errored" | "timed_out" | "idle_timed_out")
            )
        })
        .count();
    let case_count = record
        .pointer("/summary/case_count")
        .and_then(Value::as_u64)
        .unwrap_or(cases.len() as u64);
    let lifecycle_resolution = record
        .pointer("/lifecycle/resolution")
        .cloned()
        .unwrap_or_else(|| json!({"status": "completed"}));
    let lifecycle_materials = record
        .pointer("/lifecycle/materials")
        .cloned()
        .unwrap_or_else(|| json!({"status": "not_required"}));
    let lifecycle_submission = dispatch_submission_lifecycle(&mut record, &daemon, status);
    let lifecycle_grading = dispatch_grading_lifecycle(&daemon, status);
    if let Some(object) = record.as_object_mut() {
        object.insert("status".to_string(), Value::String(status.to_string()));
        object.insert("updated_at".to_string(), Value::String(now));
        object.insert(
            "summary".to_string(),
            json!({
                "case_count": case_count,
                "completed_cases": completed_cases,
                "failed_cases": failed_cases,
                "exit_code": daemon.get("exit_code").cloned().unwrap_or(Value::Null),
                "run_dir": daemon.pointer("/result/run_dir").cloned().unwrap_or(Value::Null),
                "run_id": daemon.pointer("/result/run_id").cloned().unwrap_or(Value::Null),
            }),
        );
        object.insert(
            "lifecycle".to_string(),
            json!({
                "resolution": lifecycle_resolution,
                "materials": lifecycle_materials,
                "local_runtime": {
                    "status": status,
                    "exit_code": daemon.get("exit_code").cloned().unwrap_or(Value::Null),
                    "completed_at": daemon.get("ended_at").cloned().unwrap_or(Value::Null)
                },
                "submission": lifecycle_submission,
                "grading": lifecycle_grading
            }),
        );
        if let Some(internal) = object.get_mut("internal").and_then(Value::as_object_mut) {
            internal.insert("last_daemon_status".to_string(), daemon);
        }
    }
    write_dispatch_live_view(&record)?;
    write_dispatch_record(&record)?;
    Ok(public_dispatch_record(&record))
}

fn fallback_dispatch_progress_from_run_root(
    record: &Value,
    daemon_error: &anyhow::Error,
) -> Result<Value> {
    let Some(run_root) = record.pointer("/paths/run_root").and_then(Value::as_str) else {
        return Err(anyhow!("{}", daemon_error));
    };
    let Some(result) = latest_latch_result(Path::new(run_root))? else {
        return Err(anyhow!("{}", daemon_error));
    };
    let cases = result
        .get("cases")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let failed = cases.iter().any(|case| {
        matches!(
            case.get("status").and_then(Value::as_str),
            Some("errored" | "timed_out" | "idle_timed_out")
        )
    });
    Ok(json!({
        "status": if failed { "failed" } else { "completed" },
        "exit_code": if failed { 1 } else { 0 },
        "ended_at": result.get("ended_at").cloned().unwrap_or(Value::Null),
        "result": result,
        "source": {
            "kind": "dispatch_run_root_fallback",
            "daemon_error": daemon_error.to_string()
        }
    }))
}

fn dispatch_submission_lifecycle(
    record: &mut Value,
    daemon: &Value,
    dispatch_status: &str,
) -> Value {
    if let Some(submission) = record.pointer("/lifecycle/submission") {
        if matches!(
            submission.get("status").and_then(Value::as_str),
            Some("completed" | "uploading")
        ) {
            return submission.clone();
        }
    }
    if !matches!(dispatch_status, "local_completed" | "failed") {
        return json!({
            "status": "waiting_for_local_runtime",
            "source": "cloud_upload"
        });
    }
    if cloud_api_base_url().is_none() {
        return json!({
            "status": "not_configured",
            "reason": format!("{} is not set", BUCEPHALUS_CLOUD_API_URL_ENV),
            "source": "cloud_upload"
        });
    }

    match submit_dispatch_result(record, daemon) {
        Ok(submission) => submission,
        Err(err) => json!({
            "status": "failed",
            "reason": err.to_string(),
            "source": "cloud_upload",
            "failed_at": Utc::now().to_rfc3339()
        }),
    }
}

fn submit_dispatch_result(record: &mut Value, daemon: &Value) -> Result<Value> {
    let archive = create_dispatch_submission_archive(record, daemon)?;
    let bytes = fs::read(&archive.path).with_context(|| {
        format!(
            "failed to read dispatch submission archive {}",
            archive.path.display()
        )
    })?;
    let expected_digest = sha256_bytes(bytes.as_slice());
    let filename = format!("{}-latch-result.tgz", archive.dispatch_id);
    let upload = cloud_json_post(
        "/v1/uploads",
        &json!({
            "filename": filename,
            "media_type": "application/gzip",
            "expected_digest": expected_digest,
            "byte_size": bytes.len()
        }),
    )?;
    let upload_id = upload
        .get("upload_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Cloud upload response did not include upload_id"))?
        .to_string();
    let content = cloud_bytes_put(
        &format!("/v1/uploads/{upload_id}/content"),
        bytes,
        "application/gzip",
    )?;
    let complete = cloud_json_post(&format!("/v1/uploads/{upload_id}/complete"), &json!({}))?;
    let submission = register_latch_submission(record, daemon, &upload_id, &expected_digest)?;
    Ok(json!({
        "status": "completed",
        "source": "cloud_upload",
        "upload_id": upload_id,
        "submission_id": submission.get("submission_id").cloned().unwrap_or(Value::Null),
        "filename": filename,
        "media_type": "application/gzip",
        "byte_size": archive.byte_size,
        "archive_digest": expected_digest,
        "completed_at": Utc::now().to_rfc3339(),
        "create": upload,
        "content": content,
        "complete": complete,
        "submission": submission
    }))
}

fn register_latch_submission(
    record: &Value,
    daemon: &Value,
    upload_id: &str,
    archive_digest: &str,
) -> Result<Value> {
    let resolution = dispatch_resolution_for_submission(record)?;
    let benchmark = resolution
        .get("benchmark")
        .cloned()
        .or_else(|| record.get("benchmark").cloned())
        .unwrap_or_else(|| json!({}));
    let cases = daemon
        .pointer("/result/cases")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let case_count = record
        .pointer("/summary/case_count")
        .and_then(Value::as_u64)
        .unwrap_or(cases.len() as u64);
    let completed_cases = cases
        .iter()
        .filter(|case| case.get("status").and_then(Value::as_str) == Some("completed"))
        .count();
    let failed_cases = cases
        .iter()
        .filter(|case| {
            matches!(
                case.get("status").and_then(Value::as_str),
                Some("errored" | "timed_out" | "idle_timed_out")
            )
        })
        .count();
    let summary = json!({
        "case_count": case_count,
        "completed_cases": completed_cases,
        "failed_cases": failed_cases,
        "exit_code": daemon.get("exit_code").cloned().unwrap_or(Value::Null),
        "run_id": daemon.pointer("/result/run_id").cloned().unwrap_or(Value::Null),
    });
    let local_status = daemon
        .get("status")
        .and_then(Value::as_str)
        .map(dispatch_status_from_daemon_status)
        .unwrap_or("unknown");
    let grading = dispatch_grading_lifecycle(daemon, local_status);
    let lifecycle = json!({
        "resolution": record.pointer("/lifecycle/resolution").cloned().unwrap_or_else(|| json!({"status": "completed"})),
        "materials": record.pointer("/lifecycle/materials").cloned().unwrap_or_else(|| json!({"status": "not_required"})),
        "local_runtime": {
            "status": local_status,
            "exit_code": daemon.get("exit_code").cloned().unwrap_or(Value::Null),
            "completed_at": daemon.get("ended_at").cloned().unwrap_or(Value::Null)
        },
        "grading": grading,
    });
    cloud_json_post(
        "/v1/latch/submissions",
        &json!({
            "dispatch_id": record.get("dispatch_id").cloned().unwrap_or(Value::Null),
            "upload_id": upload_id,
            "archive_digest": archive_digest,
            "benchmark": benchmark,
            "resolution": {
                "resolution_id": resolution.get("resolution_id").cloned().unwrap_or(Value::Null),
                "schema_version": resolution.get("schema_version").cloned().unwrap_or(Value::Null)
            },
            "summary": summary,
            "lifecycle": lifecycle,
            "grading": grading,
            "result": daemon
                .get("result")
                .map(latch_result_for_cloud_submission)
                .unwrap_or_else(|| json!({}))
        }),
    )
}

fn dispatch_record_for_cloud_submission(record: &Value) -> Value {
    let mut public = public_dispatch_record(record);
    if let Some(object) = public.as_object_mut() {
        object.remove("paths");
    }
    public
}

fn daemon_summary_for_cloud_submission(daemon: &Value) -> Value {
    let mut public = daemon.clone();
    redact_local_path_fields(&mut public);
    public
}

fn latch_result_for_cloud_submission(result: &Value) -> Value {
    let mut public = result.clone();
    redact_local_path_fields(&mut public);
    public
}

fn redact_local_path_fields(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for key in [
                "manifest_path",
                "run_root",
                "run_dir",
                "workspace_dir",
                "stdout_path",
                "stderr_path",
                "result_path",
                "workspace_diff_path",
                "output_path",
                "state_path",
                "log_path",
                "record_path",
                "dispatch_dir",
                "live_view",
                "resolution_path",
                "seed_dir",
            ] {
                if object.contains_key(key) {
                    object.insert(
                        key.to_string(),
                        Value::String("<local-path-redacted>".to_string()),
                    );
                }
            }
            for child in object.values_mut() {
                redact_local_path_fields(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_local_path_fields(item);
            }
        }
        _ => {}
    }
}

fn dispatch_resolution_for_submission(record: &Value) -> Result<Value> {
    let Some(path) = record.pointer("/paths/resolution").and_then(Value::as_str) else {
        return Ok(json!({}));
    };
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(value)
}

struct DispatchSubmissionArchive {
    dispatch_id: String,
    path: PathBuf,
    byte_size: u64,
}

fn create_dispatch_submission_archive(
    record: &Value,
    daemon: &Value,
) -> Result<DispatchSubmissionArchive> {
    let dispatch_id = record
        .get("dispatch_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("dispatch record is missing dispatch_id"))?
        .to_string();
    let dispatch_dir = record
        .pointer("/paths/dispatch_dir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("dispatch record is missing paths.dispatch_dir"))?;
    let run_dir = dispatch_result_run_dir(record, daemon)?;
    let submission_dir = dispatch_dir.join("submission");
    fs::create_dir_all(&submission_dir)?;
    let metadata_path = submission_dir.join("metadata.json");
    fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "latch_dispatch_submission_v1",
            "dispatch": dispatch_record_for_cloud_submission(record),
            "daemon": daemon_summary_for_cloud_submission(daemon),
            "created_at": Utc::now().to_rfc3339()
        }))?,
    )?;
    let archive_path = submission_dir.join("latch_result.tgz");
    let file = fs::File::create(&archive_path)
        .with_context(|| format!("failed to create {}", archive_path.display()))?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = tar::Builder::new(encoder);
    archive.append_path_with_name(&metadata_path, "metadata.json")?;
    if let Some(manifest) = record.pointer("/paths/manifest").and_then(Value::as_str) {
        append_existing_path(&mut archive, Path::new(manifest), "manifest.json")?;
    }
    if let Some(resolution) = record.pointer("/paths/resolution").and_then(Value::as_str) {
        append_existing_path(&mut archive, Path::new(resolution), "resolution.json")?;
    }
    archive.append_dir_all("run", &run_dir)?;
    archive.finish()?;
    let encoder = archive.into_inner()?;
    encoder.finish()?;
    let byte_size = fs::metadata(&archive_path)?.len();
    Ok(DispatchSubmissionArchive {
        dispatch_id,
        path: archive_path,
        byte_size,
    })
}

fn append_existing_path(
    archive: &mut tar::Builder<GzEncoder<fs::File>>,
    path: &Path,
    name: &str,
) -> Result<()> {
    if path.exists() {
        archive.append_path_with_name(path, name)?;
    }
    Ok(())
}

fn dispatch_result_run_dir(record: &Value, daemon: &Value) -> Result<PathBuf> {
    if let Some(run_dir) = daemon.pointer("/result/run_dir").and_then(Value::as_str) {
        let path = PathBuf::from(run_dir);
        if path.exists() {
            return Ok(path);
        }
    }
    let run_root = record
        .pointer("/paths/run_root")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("dispatch record is missing paths.run_root"))?;
    if let Some(run_id) = daemon.pointer("/result/run_id").and_then(Value::as_str) {
        let path = run_root.join(sanitize_local_id(run_id)?);
        if path.exists() {
            return Ok(path);
        }
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&run_root)
        .with_context(|| format!("failed to read run root {}", run_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.join("latch_result.json").exists() {
            continue;
        }
        let modified = fs::metadata(path.join("latch_result.json"))
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        candidates.push((modified, path));
    }
    candidates.sort_by_key(|(modified, _)| *modified);
    candidates
        .pop()
        .map(|(_, path)| path)
        .ok_or_else(|| anyhow!("dispatch local result directory was not found"))
}

fn dispatch_grading_lifecycle(daemon: &Value, dispatch_status: &str) -> Value {
    let cases = daemon
        .pointer("/result/cases")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if cases.is_empty() {
        return match dispatch_status {
            "running" => json!({
                "status": "waiting_for_local_runtime",
                "source": "local_latch_result"
            }),
            "failed" | "cancelled" => json!({
                "status": "not_completed",
                "reason": "local runtime did not produce case results",
                "source": "local_latch_result"
            }),
            _ => json!({
                "status": "not_started",
                "source": "local_latch_result"
            }),
        };
    }

    let mut graded_cases = 0usize;
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut errors = 0usize;
    let mut declined = 0usize;
    for case in &cases {
        let Some(status) = case.pointer("/grade/status").and_then(Value::as_str) else {
            continue;
        };
        graded_cases += 1;
        match status {
            "passed" => passed += 1,
            "failed" => failed += 1,
            "error" => errors += 1,
            "declined" => declined += 1,
            _ => {}
        }
    }

    if graded_cases == 0 {
        return if matches!(dispatch_status, "local_completed") {
            json!({
                "status": "not_required",
                "case_count": cases.len(),
                "graded_cases": 0,
                "source": "local_latch_result"
            })
        } else {
            json!({
                "status": "waiting_for_local_runtime",
                "case_count": cases.len(),
                "graded_cases": 0,
                "source": "local_latch_result"
            })
        };
    }

    let status = if errors > 0 {
        "error"
    } else if failed > 0 {
        "failed"
    } else if declined > 0 {
        "declined"
    } else if graded_cases == cases.len() || matches!(dispatch_status, "local_completed") {
        "passed"
    } else {
        "running"
    };

    json!({
        "status": status,
        "case_count": cases.len(),
        "graded_cases": graded_cases,
        "passed": passed,
        "failed": failed,
        "errors": errors,
        "declined": declined,
        "source": "local_latch_result"
    })
}

fn latest_latch_result(run_root: &Path) -> Result<Option<Value>> {
    let Ok(entries) = fs::read_dir(run_root) else {
        return Ok(None);
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path().join("latch_result.json");
        if !path.exists() {
            continue;
        }
        let modified = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        candidates.push((modified, path));
    }
    candidates.sort_by_key(|(modified, _)| *modified);
    let Some((_, path)) = candidates.pop() else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

fn resolve_dispatch_benchmark(
    out: &Path,
    benchmark: &str,
    cases: usize,
    argv: Vec<String>,
) -> Result<Value> {
    if normalize_latch_smoke_benchmark(benchmark).is_ok() {
        return resolve_latch_smoke_fixture(out, benchmark, cases, Some(argv));
    }
    if cloud_api_base_url().is_none() {
        return Err(anyhow!(
            "Cloud benchmark '{}' requires {}; use {} for local Core smoke",
            benchmark,
            BUCEPHALUS_CLOUD_API_URL_ENV,
            LOCAL_LATCH_SMOKE_BENCHMARK
        ));
    }
    let argv_digest = sha256_bytes(serde_json::to_vec(&argv)?.as_slice());
    let response = cloud_json_post(
        "/v1/latch/resolve",
        &json!({
            "benchmark": benchmark,
            "case_limit": cases,
            "manifest_schema": lab_runner::LATCH_MANIFEST_SCHEMA,
            "headless_command": {
                "argv_digest": argv_digest
            }
        }),
    )?;
    materialize_cloud_latch_resolution(out, benchmark, cases, argv, response)
}

fn materialize_cloud_latch_resolution(
    out: &Path,
    benchmark: &str,
    cases: usize,
    argv: Vec<String>,
    response: Value,
) -> Result<Value> {
    fs::create_dir_all(out)?;
    let resolution_path = out.join("resolution.json");
    fs::write(&resolution_path, serde_json::to_vec_pretty(&response)?)?;
    let mut manifest = response
        .get("manifest")
        .or_else(|| response.get("latch_manifest"))
        .cloned()
        .ok_or_else(|| anyhow!("Cloud latch resolution did not include manifest"))?;
    if manifest.get("schema_version").and_then(Value::as_str)
        != Some(lab_runner::LATCH_MANIFEST_SCHEMA)
    {
        return Err(anyhow!(
            "Cloud latch resolution manifest must use schema_version {}",
            lab_runner::LATCH_MANIFEST_SCHEMA
        ));
    }
    inject_dispatch_launch(&mut manifest, argv)?;
    let materials = materialize_latch_materials(out, response.get("materials"), &mut manifest)?;
    let manifest_path = out.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    let case_count = manifest
        .get("cases")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    Ok(json!({
        "schema_version": "latch_resolution_v1",
        "resolution_path": resolution_path,
        "resolution": {
            "schema_version": response.get("schema_version").and_then(Value::as_str).unwrap_or("latch_resolution_v1"),
            "resolution_id": response.get("resolution_id").cloned().unwrap_or_else(|| Value::String(format!("cloud_latch_{}", Utc::now().format("%Y%m%d_%H%M%S_%6f")))),
            "resolver": {
                "kind": "cloud",
                "api_url": cloud_api_base_url()
            },
            "benchmark": response.get("benchmark").cloned().unwrap_or_else(|| json!({
                "id": benchmark,
                "staging_shape": "file",
                "grader_shape": "artifact_pure",
                "tier_1_eligible": true
            })),
            "case_count": case_count,
            "case_limit": cases,
            "materials": materials
        },
        "manifest_path": manifest_path,
        "materials": materials,
        "next": [
            format!("bucephalus latch validate {}", manifest_path.display()),
            format!("bucephalus latch run {} --json", manifest_path.display())
        ]
    }))
}

fn inject_dispatch_launch(manifest: &mut Value, argv: Vec<String>) -> Result<()> {
    let object = manifest
        .as_object_mut()
        .ok_or_else(|| anyhow!("latch manifest must be an object"))?;
    let defaults = object
        .entry("defaults".to_string())
        .or_insert_with(|| json!({}));
    let defaults = defaults
        .as_object_mut()
        .ok_or_else(|| anyhow!("latch manifest defaults must be an object"))?;
    let mut launch = defaults.get("launch").cloned().unwrap_or_else(|| json!({}));
    let launch_object = launch
        .as_object_mut()
        .ok_or_else(|| anyhow!("latch manifest defaults.launch must be an object"))?;
    launch_object.insert("argv".to_string(), serde_json::to_value(argv)?);
    launch_object
        .entry("task_injection".to_string())
        .or_insert_with(|| Value::String("file".to_string()));
    launch_object
        .entry("cwd".to_string())
        .or_insert_with(|| Value::String("workspace".to_string()));
    defaults.insert("launch".to_string(), launch);
    Ok(())
}

fn materialize_latch_materials(
    out: &Path,
    materials: Option<&Value>,
    manifest: &mut Value,
) -> Result<Value> {
    let Some(materials) = materials else {
        return Ok(json!({
            "status": "not_required",
            "count": 0,
            "items": []
        }));
    };
    let materials_array = materials
        .as_array()
        .ok_or_else(|| anyhow!("Cloud latch resolution materials must be an array"))?;
    let material_root = out.join("materials");
    fs::create_dir_all(&material_root)?;
    let mut refs = BTreeMap::new();
    let mut report_items = Vec::new();
    for (idx, material) in materials_array.iter().enumerate() {
        let object = material
            .as_object()
            .ok_or_else(|| anyhow!("materials[{}] must be an object", idx))?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("material_{}", idx + 1));
        let output_rel = material_output_rel(&id, material)?;
        let output_path = safe_material_output_path(&material_root, &output_rel)?;
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = material_bytes(material)?;
        fs::write(&output_path, bytes)?;
        let digest = sha256_file(&output_path)?;
        if let Some(expected) = object.get("digest").and_then(Value::as_str) {
            if expected != digest {
                return Err(anyhow!(
                    "material '{}' digest mismatch: expected {}, got {}",
                    id,
                    expected,
                    digest
                ));
            }
        }
        let manifest_rel = PathBuf::from("materials").join(&output_rel);
        let manifest_rel = manifest_rel.to_string_lossy().to_string();
        refs.insert(id.clone(), manifest_rel.clone());
        report_items.push(json!({
            "id": id,
            "path": manifest_rel,
            "digest": digest
        }));
    }
    rewrite_material_refs(manifest, &refs);
    Ok(json!({
        "status": "completed",
        "count": report_items.len(),
        "items": report_items
    }))
}

fn material_output_rel(id: &str, material: &Value) -> Result<PathBuf> {
    let candidate = ["target_path", "local_path", "filename", "path"]
        .into_iter()
        .find_map(|key| material.get(key).and_then(Value::as_str))
        .unwrap_or(id);
    let rel = PathBuf::from(candidate);
    if rel.is_absolute() {
        return Err(anyhow!("material '{}' output path must be relative", id));
    }
    Ok(rel)
}

fn material_bytes(material: &Value) -> Result<Vec<u8>> {
    if let Some(text) = material.get("text").and_then(Value::as_str) {
        return Ok(text.as_bytes().to_vec());
    }
    for key in ["content_base64", "bytes_base64"] {
        if let Some(encoded) = material.get(key).and_then(Value::as_str) {
            return base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|err| anyhow!("failed to decode material {}: {}", key, err));
        }
    }
    if let Some(url) = material
        .get("download_url")
        .or_else(|| material.get("url"))
        .and_then(Value::as_str)
    {
        return http_download(url);
    }
    if let Some(source) = material.get("source").and_then(Value::as_object) {
        if let Some(text) = source.get("text").and_then(Value::as_str) {
            return Ok(text.as_bytes().to_vec());
        }
        if let Some(encoded) = source.get("content_base64").and_then(Value::as_str) {
            return base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|err| {
                    anyhow!("failed to decode material source.content_base64: {}", err)
                });
        }
        if let Some(url) = source
            .get("download_url")
            .or_else(|| source.get("url"))
            .and_then(Value::as_str)
        {
            return http_download(url);
        }
        if source.get("path").and_then(Value::as_str).is_some() {
            return Err(anyhow!(
                "remote latch materials must not use source.path; use text, content_base64, url, or download_url"
            ));
        }
    }
    Err(anyhow!(
        "material requires one of text, content_base64, bytes_base64, url, download_url, or source"
    ))
}

fn safe_material_output_path(root: &Path, rel: &Path) -> Result<PathBuf> {
    let mut out = root.to_path_buf();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            _ => {
                return Err(anyhow!(
                    "material output path must not escape material root"
                ))
            }
        }
    }
    Ok(out)
}

fn rewrite_material_refs(value: &mut Value, refs: &BTreeMap<String, String>) {
    match value {
        Value::String(text) => {
            if let Some((scheme, id)) = text.split_once("://") {
                if matches!(scheme, "material" | "artifact" | "cloud" | "package") {
                    if let Some(path) = refs.get(id) {
                        *text = path.clone();
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_material_refs(item, refs);
            }
        }
        Value::Object(object) => {
            for item in object.values_mut() {
                rewrite_material_refs(item, refs);
            }
        }
        _ => {}
    }
}

fn start_smoke_dispatch(arguments: &Value) -> Result<Value> {
    let benchmark = arguments
        .get("benchmark")
        .and_then(Value::as_str)
        .unwrap_or(LOCAL_LATCH_SMOKE_BENCHMARK);
    let cases = arguments
        .get("cases")
        .and_then(Value::as_u64)
        .map(usize::try_from)
        .transpose()?
        .unwrap_or(2);
    let command_value = arguments
        .get("headless_command")
        .or_else(|| arguments.get("command"))
        .ok_or_else(|| anyhow!("dispatch_benchmark requires headless_command.argv"))?;
    let argv = command_value
        .get("argv")
        .map(parse_mcp_string_array)
        .transpose()?
        .ok_or_else(|| anyhow!("dispatch_benchmark requires headless_command.argv"))?;
    if argv.is_empty() {
        return Err(anyhow!(
            "dispatch_benchmark headless_command.argv must not be empty"
        ));
    }
    let label = arguments
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("dispatch");
    let dispatch_id = format!("dispatch_{}", Utc::now().format("%Y%m%d_%H%M%S_%6f"));
    let dir = dispatch_dir(&dispatch_id)?;
    fs::create_dir_all(&dir)?;
    let resolution_dir = dir.join("resolution");
    let run_root = dir.join("runs");
    let live_view_path = dir.join("live.html");
    let record_path = dir.join("dispatch.json");
    let resolution = resolve_dispatch_benchmark(&resolution_dir, benchmark, cases, argv.clone())?;
    let manifest_path = resolution["manifest_path"]
        .as_str()
        .ok_or_else(|| anyhow!("latch resolver did not return manifest_path"))?;
    let daemon_job = latch_daemon::call_latch_daemon(latch_daemon::LatchDaemonRequest {
        method: "start".to_string(),
        params: json!({
            "manifest_path": manifest_path,
            "run_root": run_root,
        }),
    })?;
    let job_id = daemon_job
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("daemon did not return job_id for dispatch"))?;
    let now = Utc::now().to_rfc3339();
    let argv_bytes = serde_json::to_vec(&argv)?;
    let record = json!({
        "schema_version": DISPATCH_SCHEMA,
        "dispatch_id": dispatch_id,
        "label": label,
        "status": "running",
        "created_at": now,
        "updated_at": now,
        "benchmark": {
            "id": benchmark,
            "case_limit": cases,
            "resolver": resolution.pointer("/resolution/resolver/kind").and_then(Value::as_str).unwrap_or("unknown")
        },
        "headless_command": {
            "argv_digest": sha256_bytes(argv_bytes.as_slice())
        },
        "summary": {
            "case_count": resolution.pointer("/resolution/case_count").and_then(Value::as_u64).unwrap_or(cases as u64),
            "completed_cases": 0,
            "failed_cases": 0,
            "exit_code": null,
            "run_dir": null,
            "run_id": null
        },
        "paths": {
            "dispatch_dir": dir,
            "live_view": live_view_path,
            "resolution": resolution["resolution_path"].clone(),
            "manifest": manifest_path,
            "run_root": run_root
        },
        "lifecycle": {
            "resolution": {
                "status": "completed",
                "kind": resolution.pointer("/resolution/resolver/kind").and_then(Value::as_str).unwrap_or("unknown"),
                "resolution_id": resolution.pointer("/resolution/resolution_id").cloned().unwrap_or(Value::Null)
            },
            "materials": resolution.get("materials").cloned().or_else(|| resolution.pointer("/resolution/materials").cloned()).unwrap_or_else(|| json!({
                "status": "not_required",
                "count": 0,
                "items": []
            })),
            "local_runtime": {
                "status": "running",
                "started_at": now
            },
            "submission": {
                "status": "waiting_for_local_runtime",
                "source": "cloud_upload"
            },
            "grading": {
                "status": "waiting_for_local_runtime",
                "source": "local_latch_result"
            }
        },
        "record_path": record_path,
        "internal": {
            "job_id": job_id,
            "daemon_job": daemon_job
        }
    });
    write_dispatch_live_view(&record)?;
    write_dispatch_record(&record)?;
    Ok(public_dispatch_record(&record))
}

fn write_dispatch_live_view(record: &Value) -> Result<()> {
    let path = record
        .pointer("/paths/live_view")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("dispatch record missing live view path"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let dispatch_id = record
        .get("dispatch_id")
        .and_then(Value::as_str)
        .unwrap_or("dispatch");
    let status = record
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let label = record.get("label").and_then(Value::as_str).unwrap_or("");
    let benchmark = record
        .pointer("/benchmark/id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let summary = record.get("summary").cloned().unwrap_or_else(|| json!({}));
    let mut public_summary = summary.clone();
    if let Some(summary) = public_summary.as_object_mut() {
        summary.remove("run_dir");
    }
    let lifecycle = record
        .get("lifecycle")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let updated_at = record
        .get("updated_at")
        .and_then(Value::as_str)
        .unwrap_or("");
    let html = format!(
        r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta http-equiv="refresh" content="5">
  <title>Bucephalus Dispatch {}</title>
  <style>
    body {{ margin: 0; font: 14px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: #f7f7f4; color: #171717; }}
    main {{ max-width: 880px; margin: 0 auto; padding: 32px 20px; }}
    h1 {{ font-size: 24px; margin: 0 0 8px; }}
    .status {{ display: inline-block; padding: 4px 8px; border-radius: 6px; background: #111; color: #fff; text-transform: uppercase; font-size: 12px; letter-spacing: .04em; }}
    dl {{ display: grid; grid-template-columns: 160px 1fr; gap: 10px 16px; margin-top: 24px; }}
    dt {{ color: #666; }}
    dd {{ margin: 0; word-break: break-word; }}
    pre {{ background: #fff; border: 1px solid #ddd; border-radius: 8px; padding: 14px; overflow: auto; }}
  </style>
</head>
<body>
<main>
  <p class="status">{}</p>
  <h1>{}</h1>
  <dl>
    <dt>Dispatch</dt><dd>{}</dd>
    <dt>Benchmark</dt><dd>{}</dd>
    <dt>Updated</dt><dd>{}</dd>
  </dl>
  <h2>Lifecycle</h2>
  <pre>{}</pre>
  <h2>Summary</h2>
  <pre>{}</pre>
</main>
</body>
</html>
"#,
        html_escape(dispatch_id),
        html_escape(status),
        html_escape(if label.is_empty() { dispatch_id } else { label }),
        html_escape(dispatch_id),
        html_escape(benchmark),
        html_escape(updated_at),
        html_escape(&serde_json::to_string_pretty(&lifecycle)?),
        html_escape(&serde_json::to_string_pretty(&public_summary)?)
    );
    fs::write(path, html)?;
    Ok(())
}

fn unregister_mcp_clients(
    requested_clients: Vec<SetupMcpClientArg>,
    project: Option<&Path>,
    dry_run: bool,
) -> Result<Value> {
    let resolved_clients = resolve_setup_clients(requested_clients, project)?;
    if resolved_clients.is_empty() {
        return Ok(json!({
            "status": "skipped",
            "server_name": BUCEPHALUS_MCP_SERVER_NAME,
            "clients": [],
            "reason": "no supported MCP clients were detected for automatic cleanup",
            "actions": [
                {
                    "type": "cli_command",
                    "command": "bucephalus setup uninstall --client claude-code",
                    "description": "Remove the Claude Code registration after the claude CLI is installed or on PATH."
                },
                {
                    "type": "cli_command",
                    "command": "bucephalus setup uninstall --client cursor-project --project <project-dir>",
                    "description": "Remove a project-local Cursor MCP registration."
                }
            ]
        }));
    }
    let mut clients = Vec::new();
    for client in resolved_clients {
        clients.push(unregister_mcp_client(client, project, dry_run)?);
    }
    Ok(json!({
        "status": summarize_mcp_unregistration_status(&clients, dry_run),
        "server_name": BUCEPHALUS_MCP_SERVER_NAME,
        "clients": clients
    }))
}

fn resolve_setup_clients(
    requested_clients: Vec<SetupMcpClientArg>,
    project: Option<&Path>,
) -> Result<Vec<SetupMcpClientArg>> {
    let requested_clients = if requested_clients.is_empty() {
        vec![SetupMcpClientArg::Auto]
    } else {
        requested_clients
    };
    let mut clients = Vec::new();
    for client in requested_clients {
        match client {
            SetupMcpClientArg::Auto => {
                if command_exists("claude") {
                    clients.push(SetupMcpClientArg::ClaudeCode);
                }
                if claude_desktop_config_path().is_some() {
                    clients.push(SetupMcpClientArg::ClaudeDesktop);
                }
                if project.is_some() {
                    clients.push(SetupMcpClientArg::CursorProject);
                }
            }
            other => clients.push(other),
        }
    }
    let mut deduped = Vec::new();
    for client in clients {
        if !deduped
            .iter()
            .any(|existing| setup_client_name(*existing) == setup_client_name(client))
        {
            deduped.push(client);
        }
    }
    Ok(deduped)
}

fn register_mcp_client(
    _exe: &Path,
    client: SetupMcpClientArg,
    project: Option<&Path>,
    server_config: &Value,
    dry_run: bool,
) -> Result<Value> {
    match client {
        SetupMcpClientArg::Auto => Err(anyhow!("internal setup error: unresolved auto client")),
        SetupMcpClientArg::ClaudeCode => {
            let config_json = serde_json::to_string(server_config)?;
            let command = vec![
                "claude".to_string(),
                "mcp".to_string(),
                "add-json".to_string(),
                BUCEPHALUS_MCP_SERVER_NAME.to_string(),
                config_json,
            ];
            if !command_exists("claude") {
                return Ok(json!({
                    "client": setup_client_name(client),
                    "status": "skipped",
                    "reason": "claude command not found on PATH",
                    "manual_config": server_config
                }));
            }
            if !dry_run {
                let outcome = run_command_status_capture(&command)?;
                if !outcome.success && !claude_mcp_server_already_exists(&outcome.output) {
                    return Err(anyhow!(
                        "command failed: {}\n{}",
                        shell_join(&command),
                        outcome.output.trim()
                    ));
                }
            }
            Ok(json!({
                "client": setup_client_name(client),
                "status": if dry_run { "planned" } else { "configured" },
                "method": "claude mcp add-json",
                "command": command,
            }))
        }
        SetupMcpClientArg::ClaudeDesktop => {
            let Some(path) = claude_desktop_config_path() else {
                return Ok(json!({
                    "client": setup_client_name(client),
                    "status": "unsupported",
                    "reason": "Claude Desktop config path is known for macOS and Windows only in this setup flow",
                    "manual_config": server_config
                }));
            };
            if !dry_run {
                merge_mcp_server_config(&path, BUCEPHALUS_MCP_SERVER_NAME, server_config)?;
            }
            Ok(json!({
                "client": setup_client_name(client),
                "status": if dry_run { "planned" } else { "configured" },
                "path": path,
            }))
        }
        SetupMcpClientArg::CursorProject => {
            let project = project
                .map(Path::to_path_buf)
                .unwrap_or(std::env::current_dir()?);
            let path = project.join(".cursor").join("mcp.json");
            if !dry_run {
                merge_mcp_server_config(&path, BUCEPHALUS_MCP_SERVER_NAME, server_config)?;
            }
            Ok(json!({
                "client": setup_client_name(client),
                "status": if dry_run { "planned" } else { "configured" },
                "scope": "project",
                "path": path,
            }))
        }
    }
}

fn unregister_mcp_client(
    client: SetupMcpClientArg,
    project: Option<&Path>,
    dry_run: bool,
) -> Result<Value> {
    match client {
        SetupMcpClientArg::Auto => Err(anyhow!("internal setup error: unresolved auto client")),
        SetupMcpClientArg::ClaudeCode => {
            let command = vec![
                "claude".to_string(),
                "mcp".to_string(),
                "remove".to_string(),
                BUCEPHALUS_MCP_SERVER_NAME.to_string(),
            ];
            if !command_exists("claude") {
                return Ok(json!({
                    "client": setup_client_name(client),
                    "status": "skipped",
                    "reason": "claude command not found on PATH",
                    "action": "Install Claude Code or remove the bucephalus MCP server from Claude Code manually."
                }));
            }
            if !dry_run {
                let _ = run_command_status(&command);
            }
            Ok(json!({
                "client": setup_client_name(client),
                "status": if dry_run { "planned" } else { "removed" },
                "command": command
            }))
        }
        SetupMcpClientArg::ClaudeDesktop => {
            let Some(path) = claude_desktop_config_path() else {
                return Ok(json!({
                    "client": setup_client_name(client),
                    "status": "unsupported",
                    "reason": "Claude Desktop config path is known for macOS and Windows only in this setup flow"
                }));
            };
            let existed = mcp_config_has_server(&path, BUCEPHALUS_MCP_SERVER_NAME);
            if !dry_run {
                remove_mcp_server_config(&path, BUCEPHALUS_MCP_SERVER_NAME)?;
            }
            Ok(json!({
                "client": setup_client_name(client),
                "status": if dry_run { "planned" } else if existed { "removed" } else { "missing" },
                "path": path
            }))
        }
        SetupMcpClientArg::CursorProject => {
            let project = project
                .map(Path::to_path_buf)
                .unwrap_or(std::env::current_dir()?);
            let path = project.join(".cursor").join("mcp.json");
            let existed = mcp_config_has_server(&path, BUCEPHALUS_MCP_SERVER_NAME);
            if !dry_run {
                remove_mcp_server_config(&path, BUCEPHALUS_MCP_SERVER_NAME)?;
            }
            Ok(json!({
                "client": setup_client_name(client),
                "status": if dry_run { "planned" } else if existed { "removed" } else { "missing" },
                "scope": "project",
                "path": path
            }))
        }
    }
}

fn summarize_mcp_unregistration_status(clients: &[Value], dry_run: bool) -> &'static str {
    if dry_run {
        return "planned";
    }
    if clients
        .iter()
        .any(|client| client["status"].as_str() == Some("removed"))
    {
        return "removed";
    }
    if clients
        .iter()
        .any(|client| client["status"].as_str() == Some("missing"))
    {
        return "missing";
    }
    if clients
        .iter()
        .any(|client| client["status"].as_str() == Some("unsupported"))
    {
        return "unsupported";
    }
    "skipped"
}

fn merge_mcp_server_config(path: &Path, name: &str, server_config: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut root = if path.exists() {
        serde_json::from_str::<Value>(&fs::read_to_string(path)?)?
    } else {
        json!({})
    };
    if !root.is_object() {
        return Err(anyhow!(
            "MCP config {} is not a JSON object",
            path.display()
        ));
    }
    let root_object = root.as_object_mut().expect("checked object");
    let mcp_servers = root_object
        .entry("mcpServers".to_string())
        .or_insert_with(|| json!({}));
    if !mcp_servers.is_object() {
        return Err(anyhow!(
            "MCP config {} has non-object mcpServers",
            path.display()
        ));
    }
    mcp_servers
        .as_object_mut()
        .expect("checked object")
        .insert(name.to_string(), server_config.clone());
    fs::write(path, serde_json::to_vec_pretty(&root)?)?;
    Ok(())
}

fn mcp_config_has_server(path: &Path, name: &str) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    root.pointer(&format!("/mcpServers/{name}")).is_some()
}

fn remove_mcp_server_config(path: &Path, name: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut root = serde_json::from_str::<Value>(&fs::read_to_string(path)?)?;
    let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    servers.remove(name);
    fs::write(path, serde_json::to_vec_pretty(&root)?)?;
    Ok(())
}

fn claude_desktop_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        return user_home_dir().ok().map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("Claude")
                .join("claude_desktop_config.json")
        });
    }

    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA").map(|appdata| {
            PathBuf::from(appdata)
                .join("Claude")
                .join("claude_desktop_config.json")
        });
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

fn setup_client_name(client: SetupMcpClientArg) -> &'static str {
    match client {
        SetupMcpClientArg::Auto => "auto",
        SetupMcpClientArg::ClaudeCode => "claude-code",
        SetupMcpClientArg::ClaudeDesktop => "claude-desktop",
        SetupMcpClientArg::CursorProject => "cursor-project",
    }
}

fn command_exists(command: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let path = dir.join(command);
        path.is_file()
    })
}

fn run_command_status(argv: &[String]) -> Result<()> {
    let Some((program, args)) = argv.split_first() else {
        return Err(anyhow!("cannot run empty command"));
    };
    let status = Command::new(program).args(args).status()?;
    if !status.success() {
        return Err(anyhow!("command failed: {}", shell_join(argv)));
    }
    Ok(())
}

struct CommandStatusOutput {
    success: bool,
    output: String,
}

fn run_command_status_capture(argv: &[String]) -> Result<CommandStatusOutput> {
    let Some((program, args)) = argv.split_first() else {
        return Err(anyhow!("cannot run empty command"));
    };
    let output = Command::new(program).args(args).output()?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(CommandStatusOutput {
        success: output.status.success(),
        output: combined,
    })
}

fn claude_mcp_server_already_exists(output: &str) -> bool {
    let normalized = output.to_ascii_lowercase();
    normalized.contains("mcp server")
        && normalized.contains(BUCEPHALUS_MCP_SERVER_NAME)
        && normalized.contains("already exists")
}

fn current_uid_string() -> Result<String> {
    let output = Command::new("id").arg("-u").output()?;
    if !output.status.success() {
        return Err(anyhow!("failed to resolve current uid with id -u"));
    }
    let uid = String::from_utf8(output.stdout)?.trim().to_string();
    if uid.is_empty() {
        return Err(anyhow!("id -u returned an empty uid"));
    }
    Ok(uid)
}

fn user_home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set"))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "/._+-:@".contains(ch))
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            if arg
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "-_./:=@".contains(ch))
            {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const LOCAL_LATCH_SMOKE_SCHEMA: &str = "latch_local_resolution_v1";
const LOCAL_LATCH_SMOKE_BENCHMARK: &str = "local:file-edit-smoke";

fn default_latch_smoke_argv() -> Vec<String> {
    let script = r#"set -eu
case "$BUCEPHALUS_CASE_ID" in
  readme-title)
    cat > README.md <<'EOF'
# Latch Smoke Passed

The local Tier-1 smoke fixture edited this workspace.
EOF
    ;;
  config-toggle)
    printf '{\n  "smoke_mode": true,\n  "case_id": "%s"\n}\n' "$BUCEPHALUS_CASE_ID" > app.config.json
    printf 'smoke fixture completed\n' > SMOKE.md
    ;;
  *)
    echo "unknown smoke case: $BUCEPHALUS_CASE_ID" >&2
    exit 2
    ;;
esac
printf '{"status":"completed","case_id":"%s","metrics":{"resolved":1.0}}\n' "$BUCEPHALUS_CASE_ID" > "$BUCEPHALUS_RESULT_PATH"
"#;
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

fn normalize_latch_smoke_benchmark(value: &str) -> Result<&'static str> {
    match value {
        LOCAL_LATCH_SMOKE_BENCHMARK | "local:file-edit" | "demo" => Ok(LOCAL_LATCH_SMOKE_BENCHMARK),
        other => Err(anyhow!(
            "unsupported local latch smoke benchmark '{}'; supported: {}",
            other,
            LOCAL_LATCH_SMOKE_BENCHMARK
        )),
    }
}

fn resolve_latch_smoke_fixture(
    out: &Path,
    benchmark: &str,
    case_limit: usize,
    launch_override: Option<Vec<String>>,
) -> Result<Value> {
    if case_limit == 0 {
        return Err(anyhow!("case limit must be at least 1"));
    }
    let benchmark_id = normalize_latch_smoke_benchmark(benchmark)?;
    let seed_dir = out.join("seed");
    fs::create_dir_all(seed_dir.join("readme-title"))?;
    fs::create_dir_all(seed_dir.join("config-toggle"))?;
    fs::write(
        seed_dir.join("readme-title").join("README.md"),
        "# Unresolved Smoke Fixture\n\nReplace this heading.\n",
    )?;
    fs::write(
        seed_dir.join("config-toggle").join("app.config.json"),
        "{\n  \"smoke_mode\": false\n}\n",
    )?;

    let fixture_cases = vec![
        json!({
            "case_id": "readme-title",
            "task_id": "smoke-readme-title",
            "task_prompt": "Update README.md so the heading is exactly '# Latch Smoke Passed'.",
            "workspace_seed": {
                "kind": "files",
                "path": "seed/readme-title"
            },
            "expected_output": {
                "kind": "workspace_diff"
            },
            "metadata": {
                "benchmark_id": benchmark_id,
                "resolver_kind": "local_fixture",
                "staging_shape": "file",
                "grader_shape": "artifact_pure"
            }
        }),
        json!({
            "case_id": "config-toggle",
            "task_id": "smoke-config-toggle",
            "task_prompt": "Set app.config.json smoke_mode to true and add a short SMOKE.md note.",
            "workspace_seed": {
                "kind": "files",
                "path": "seed/config-toggle"
            },
            "expected_output": {
                "kind": "workspace_diff"
            },
            "metadata": {
                "benchmark_id": benchmark_id,
                "resolver_kind": "local_fixture",
                "staging_shape": "file",
                "grader_shape": "artifact_pure"
            }
        }),
    ];
    let selected_cases = fixture_cases
        .into_iter()
        .take(case_limit)
        .collect::<Vec<_>>();
    let launch_source = if launch_override.is_some() {
        "user_supplied"
    } else {
        "local_fixture"
    };
    let launch_argv = launch_override.unwrap_or_else(default_latch_smoke_argv);
    let resolution_id = format!("latch_smoke_{}", Utc::now().format("%Y%m%d_%H%M%S_%6f"));
    let manifest_path = out.join("manifest.json");
    let resolution_path = out.join("resolution.json");
    let manifest = json!({
        "schema_version": lab_runner::LATCH_MANIFEST_SCHEMA,
        "run_id": resolution_id,
        "defaults": {
            "launch": {
                "argv": launch_argv,
                "task_injection": "file",
                "cwd": "workspace",
                "timeout_seconds": 60
            }
        },
        "cases": selected_cases
    });
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    let resolution = json!({
        "schema_version": LOCAL_LATCH_SMOKE_SCHEMA,
        "resolution_id": resolution_id,
        "resolver": {
            "kind": "local_fixture",
            "note": "Local-only stand-in for the cloud benchmark case-resolution API. It produces a normal latch_manifest_v1 and does not alter runner behavior."
        },
        "benchmark": {
            "id": benchmark_id,
            "requested_id": benchmark,
            "staging_shape": "file",
            "grader_shape": "artifact_pure",
            "tier_1_eligible": true
        },
        "case_count": manifest["cases"].as_array().map(Vec::len).unwrap_or(0),
        "case_limit": case_limit,
        "launch_source": launch_source,
        "manifest_path": manifest_path,
        "seed_dir": seed_dir
    });
    fs::write(&resolution_path, serde_json::to_vec_pretty(&resolution)?)?;
    Ok(json!({
        "schema_version": LOCAL_LATCH_SMOKE_SCHEMA,
        "resolution_path": resolution_path,
        "resolution": resolution,
        "manifest_path": manifest_path,
        "seed_dir": seed_dir,
        "next": [
            format!("bucephalus latch validate {}", manifest_path.display()),
            format!("bucephalus latch run {} --json", manifest_path.display())
        ]
    }))
}

fn write_latch_demo(out: &Path) -> Result<Value> {
    let seed_dir = out.join("seed");
    fs::create_dir_all(&seed_dir)?;
    fs::write(seed_dir.join("answer.txt"), "unanswered\n")?;
    let manifest_path = out.join("manifest.json");
    let manifest = json!({
        "schema_version": lab_runner::LATCH_MANIFEST_SCHEMA,
        "defaults": {
            "launch": {
                "argv": ["sh", "-c", "printf '%s\\n' \"$LATCH_TASK_PROMPT\" > answer.txt"],
                "task_injection": "argv",
                "cwd": "workspace",
                "timeout_seconds": 60
            },
            "workspace_seed": {
                "kind": "files",
                "path": "seed"
            }
        },
        "cases": [
            {
                "case_id": "demo-1",
                "task_prompt": "Write a cheerful one-line answer for demo case 1.",
                "metadata": {"source": "local_demo"}
            },
            {
                "case_id": "demo-2",
                "task_prompt": "Write a concise one-line answer for demo case 2.",
                "metadata": {"source": "local_demo"}
            }
        ]
    });
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(json!({
        "schema_version": "latch_demo_v1",
        "manifest_path": manifest_path,
        "seed_dir": seed_dir,
        "next": [
            format!("bucephalus latch validate {}", manifest_path.display()),
            format!("bucephalus latch run {} --json", manifest_path.display())
        ]
    }))
}

fn read_mcp_message<R: BufRead + Read>(reader: &mut R) -> Result<Option<Value>> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(raw_len) = trimmed.strip_prefix("Content-Length:") {
            let len = raw_len
                .trim()
                .parse::<usize>()
                .map_err(|_| anyhow!("invalid MCP Content-Length header"))?;
            loop {
                line.clear();
                let n = reader.read_line(&mut line)?;
                if n == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let mut bytes = vec![0_u8; len];
            reader.read_exact(&mut bytes)?;
            return Ok(Some(serde_json::from_slice(&bytes)?));
        }
        return Ok(Some(serde_json::from_str(trimmed)?));
    }
}

fn write_mcp_message<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

fn handle_mcp_message(message: Value) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "bucephalus-latch",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "tools/list" => Ok(json!({
            "tools": [
                {
                    "name": "status",
                    "description": "Check local Bucephalus latch readiness and installation state.",
                    "inputSchema": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {}
                    }
                },
                {
                    "name": "dispatch_benchmark",
                    "description": "Dispatch a Tier-1 benchmark on this host and return a live viewing surface. The local runtime is managed internally; provide the agent command as headless_command.argv.",
                    "inputSchema": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["headless_command"],
                        "properties": {
                            "benchmark": {
                                "type": "string",
                                "description": "Benchmark id or alias. Use local:file-edit-smoke for local rehearsal; remote ids resolve through the Cloud latch API."
                            },
                            "cases": {"type": "integer", "minimum": 1},
                            "label": {"type": "string"},
                            "headless_command": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["argv"],
                                "properties": {
                                    "argv": {
                                        "type": "array",
                                        "minItems": 1,
                                        "items": {"type": "string"}
                                    }
                                }
                            }
                        }
                    }
                },
                {
                    "name": "dispatch_status",
                    "description": "Refresh a benchmark dispatch and its live viewing surface.",
                    "inputSchema": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["dispatch_id"],
                        "properties": {
                            "dispatch_id": {"type": "string"}
                        }
                    }
                }
            ]
        })),
        "tools/call" => handle_mcp_tool_call(message.get("params").cloned().unwrap_or(Value::Null)),
        _ => Err(anyhow!("unsupported MCP method '{}'", method)),
    };
    Some(match result {
        Ok(result) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }),
        Err(err) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32000,
                "message": err.to_string()
            }
        }),
    })
}

fn handle_mcp_tool_call(params: Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("tools/call requires params.name"))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "status" => {
            let home_path = lab_runner::bucephalus_home().ok();
            let home = home_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unavailable".to_string());
            let local_runtime = match latch_daemon::ensure_latch_daemon() {
                Ok(_) => json!({
                    "status": "ready",
                    "mode": "managed_local_runtime",
                }),
                Err(err) => json!({
                    "status": "error",
                    "error": err.to_string(),
                }),
            };
            let auth = home_path.as_deref().map(auth_status).unwrap_or_else(|| {
                json!({
                    "status": "error",
                    "error": "Bucephalus home unavailable"
                })
            });
            mcp_tool_result(json!({
                "ok": true,
                "binary": "bucephalus",
                "version": env!("CARGO_PKG_VERSION"),
                "home": home,
                "local_runtime": local_runtime,
                "latch": {
                    "status": "ready",
                    "supported_manifest_schema": lab_runner::LATCH_MANIFEST_SCHEMA
                },
                "auth": auth
            }))
        }
        "dispatch_benchmark" => mcp_tool_result(start_smoke_dispatch(&arguments)?),
        "dispatch_status" => {
            let dispatch_id = arguments
                .get("dispatch_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("dispatch_status requires dispatch_id"))?;
            mcp_tool_result(refresh_dispatch(dispatch_id)?)
        }
        "latch_run_manifest" | "latch_progress" | "latch_cancel" | "latch_tail"
        | "latch_demo_manifest" | "latch_smoke_test"
            if std::env::var_os("BUCEPHALUS_MCP_DEBUG_LATCH_TOOLS").is_none() =>
        {
            Err(anyhow!(
                "low-level latch MCP tools are disabled; use dispatch_benchmark or set BUCEPHALUS_MCP_DEBUG_LATCH_TOOLS=1 for local debugging"
            ))
        }
        "latch_run_manifest" => {
            let manifest_path = arguments
                .get("manifest_path")
                .or_else(|| arguments.get("manifest"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("latch_run_manifest requires manifest_path or manifest"))?;
            let run_root = arguments
                .get("run_root")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            let argv = arguments
                .get("argv")
                .map(parse_mcp_string_array)
                .transpose()?;
            let mut params = serde_json::Map::new();
            params.insert(
                "manifest_path".to_string(),
                Value::String(manifest_path.to_string()),
            );
            if let Some(run_root) = run_root {
                params.insert(
                    "run_root".to_string(),
                    Value::String(run_root.display().to_string()),
                );
            }
            if let Some(argv) = argv.filter(|items| !items.is_empty()) {
                params.insert("argv".to_string(), serde_json::to_value(argv)?);
            }
            let result = latch_daemon::call_latch_daemon(latch_daemon::LatchDaemonRequest {
                method: "start".to_string(),
                params: Value::Object(params),
            })?;
            mcp_tool_result(result)
        }
        "latch_progress" => {
            let job_id = arguments
                .get("job_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("latch_progress requires job_id"))?;
            mcp_tool_result(latch_daemon::call_latch_daemon(
                latch_daemon::LatchDaemonRequest {
                    method: "progress".to_string(),
                    params: json!({ "job_id": job_id }),
                },
            )?)
        }
        "latch_cancel" => {
            let job_id = arguments
                .get("job_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("latch_cancel requires job_id"))?;
            mcp_tool_result(latch_daemon::call_latch_daemon(
                latch_daemon::LatchDaemonRequest {
                    method: "cancel".to_string(),
                    params: json!({ "job_id": job_id }),
                },
            )?)
        }
        "latch_tail" => {
            let job_id = arguments
                .get("job_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("latch_tail requires job_id"))?;
            let stream = arguments
                .get("stream")
                .and_then(Value::as_str)
                .unwrap_or("stderr");
            let max_lines = arguments
                .get("max_lines")
                .and_then(Value::as_u64)
                .unwrap_or(80);
            mcp_tool_result(latch_daemon::call_latch_daemon(
                latch_daemon::LatchDaemonRequest {
                    method: "tail".to_string(),
                    params: json!({
                        "job_id": job_id,
                        "stream": stream,
                        "max_lines": max_lines,
                    }),
                },
            )?)
        }
        "latch_demo_manifest" => {
            let out = arguments
                .get("out")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("latch_demo_manifest requires out"))?;
            mcp_tool_result(write_latch_demo(&PathBuf::from(out))?)
        }
        "latch_smoke_test" => {
            let benchmark = arguments
                .get("benchmark")
                .and_then(Value::as_str)
                .unwrap_or(LOCAL_LATCH_SMOKE_BENCHMARK);
            let cases = arguments
                .get("cases")
                .and_then(Value::as_u64)
                .map(usize::try_from)
                .transpose()?
                .unwrap_or(2);
            let out = arguments
                .get("out")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    lab_runner::bucephalus_home()
                        .unwrap_or_else(|_| std::env::temp_dir().join("bucephalus"))
                        .join("latch_smoke")
                        .join(format!(
                            "resolution_{}",
                            Utc::now().format("%Y%m%d_%H%M%S_%6f")
                        ))
                });
            let argv = arguments
                .get("argv")
                .map(parse_mcp_string_array)
                .transpose()?;
            let resolution = resolve_latch_smoke_fixture(
                &out,
                benchmark,
                cases,
                argv.filter(|items| !items.is_empty()),
            )?;
            let manifest_path = resolution["manifest_path"].as_str().ok_or_else(|| {
                anyhow!("local latch smoke resolver did not return manifest_path")
            })?;
            let run_root = arguments
                .get("run_root")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| out.join("runs"));
            let result = latch_daemon::call_latch_daemon(latch_daemon::LatchDaemonRequest {
                method: "start".to_string(),
                params: json!({
                    "manifest_path": manifest_path,
                    "run_root": run_root,
                }),
            })?;
            mcp_tool_result(json!({
                "schema_version": "latch_smoke_job_v1",
                "resolution": resolution,
                "job": result
            }))
        }
        other => Err(anyhow!("unknown MCP tool '{}'", other)),
    }
}

fn parse_mcp_string_array(value: &Value) -> Result<Vec<String>> {
    let array = value
        .as_array()
        .ok_or_else(|| anyhow!("argv must be an array of strings"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("argv entries must be strings"))
        })
        .collect()
}

fn mcp_tool_result(value: Value) -> Result<Value> {
    Ok(json!({
        "content": [
            {
                "type": "text",
                "text": serde_json::to_string_pretty(&value)?
            }
        ],
        "structuredContent": value,
        "isError": false
    }))
}

fn package_directory_for_input(path: &Path) -> PathBuf {
    if path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "manifest.json")
    {
        return path.parent().unwrap_or(path).to_path_buf();
    }
    path.to_path_buf()
}

fn resolve_package_command_target(command: &str, path: &Path) -> Result<PathBuf> {
    if path.is_dir() {
        if looks_like_bucephalus_package_dir(path) {
            return Ok(path.to_path_buf());
        }
        if path.join("experiment.yaml").is_file() {
            return Err(package_command_target_error(
                command,
                path,
                "directory contains experiment.yaml but no sealed package metadata",
            ));
        }
        return Err(package_command_target_error(
            command,
            path,
            "directory is not a sealed package",
        ));
    }
    if path.is_file() {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "manifest.json")
        {
            return Ok(path.parent().unwrap_or(path).to_path_buf());
        }
        if is_yaml_file(path) {
            return Err(package_command_target_error(
                command,
                path,
                "target is an experiment YAML, not a sealed package",
            ));
        }
        return Err(package_command_target_error(
            command,
            path,
            "file is not manifest.json from a sealed package",
        ));
    }
    if !path.exists() {
        return Err(package_command_target_error(
            command,
            path,
            "path does not exist",
        ));
    }
    Err(package_command_target_error(
        command,
        path,
        "path is not a directory or file",
    ))
}

fn experiment_input_path(path: &Path) -> Result<Option<PathBuf>> {
    if path.is_dir() {
        if looks_like_bucephalus_package_dir(path) {
            return Ok(None);
        }
        let experiment = path.join("experiment.yaml");
        if experiment.is_file() {
            return Ok(Some(experiment));
        }
        return Err(run_input_target_error(
            path,
            "found neither experiment.yaml nor sealed package metadata",
        ));
    }
    if path.is_file() && is_yaml_file(path) {
        return Ok(Some(path.to_path_buf()));
    }
    if path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "manifest.json")
    {
        return Ok(None);
    }
    if path.is_file() {
        return Err(run_input_target_error(
            path,
            "file is not an experiment YAML or manifest.json",
        ));
    }
    if !path.exists() {
        return Err(run_input_target_error(path, "path does not exist"));
    }
    Ok(None)
}

#[derive(Debug)]
enum DoctorTarget {
    Experiment(PathBuf),
    Package(PathBuf),
}

fn resolve_doctor_target(target: Option<&Path>) -> Result<DoctorTarget> {
    let Some(path) = target else {
        return Ok(DoctorTarget::Experiment(resolve_experiment_target(None)?));
    };
    if path.is_dir() {
        if looks_like_bucephalus_package_dir(path) {
            return Ok(DoctorTarget::Package(path.to_path_buf()));
        }
        let experiment = path.join("experiment.yaml");
        if experiment.is_file() {
            return Ok(DoctorTarget::Experiment(experiment));
        }
        return Err(doctor_target_error(
            path,
            "found neither experiment.yaml nor sealed package metadata",
        ));
    }
    if path.is_file() {
        if is_yaml_file(path) {
            return Ok(DoctorTarget::Experiment(path.to_path_buf()));
        }
        return Ok(DoctorTarget::Package(path.to_path_buf()));
    }
    Err(doctor_target_error(path, "path does not exist"))
}

fn experiment_target_error(command: &str, path: &Path, reason: &str) -> anyhow::Error {
    anyhow!(
        "{command} expected an experiment YAML, but {reason}: {}\n\nNext steps:\n  bucephalus init <new-eval-dir>\n  bucephalus {command} <new-eval-dir>\n\nIf this is a sealed package, use:\n  bucephalus check-package {}\n  bucephalus doctor {}",
        path.display(),
        path.display(),
        path.display()
    )
}

fn doctor_target_error(path: &Path, reason: &str) -> anyhow::Error {
    anyhow!(
        "doctor expected an experiment YAML or sealed package, but {reason}: {}\n\nNext steps:\n  bucephalus doctor experiment.yaml\n  bucephalus doctor <package-dir>\n\nTo create a starter eval:\n  bucephalus init <new-eval-dir>",
        path.display()
    )
}

fn run_input_target_error(path: &Path, reason: &str) -> anyhow::Error {
    anyhow!(
        "run expected an experiment YAML, a sealed package directory, or manifest.json, but {reason}: {}\n\nNext steps:\n  bucephalus run experiment.yaml\n  bucephalus run <package-dir> --smoke-test\n  bucephalus doctor <same-target>",
        path.display()
    )
}

fn package_command_target_error(command: &str, path: &Path, reason: &str) -> anyhow::Error {
    anyhow!(
        "{command} expected a sealed package directory or package manifest.json, but {reason}: {}\n\nNext steps:\n  bucephalus build experiment.yaml --out <package-dir>\n  bucephalus {command} <package-dir>\n  bucephalus doctor experiment.yaml",
        path.display()
    )
}

fn run_command(command: Commands) -> Result<Option<Value>> {
    match command {
        Commands::Init {
            dir,
            client,
            command,
            url,
            stream,
            language,
            mcp_role,
            mcp_tool,
            mode,
            name,
            force,
            json,
        } => {
            let result = run_init(resolve_init_options(InitOptionArgs {
                dir,
                client,
                command,
                url,
                stream,
                language,
                mcp_role,
                mcp_tool,
                mode,
                name,
                force,
            })?)?;
            if json {
                return Ok(Some(result));
            }
            println!(
                "created: {}",
                result["experiment"].as_str().unwrap_or("experiment.yaml")
            );
            println!(
                "agent: {}",
                result["agent"].as_str().unwrap_or("agent/buc_agent.py")
            );
            if let Some(next) = result["next"].as_array() {
                for command in next {
                    if let Some(command) = command.as_str() {
                        println!("next: {command}");
                    }
                }
            }
        }
        Commands::Mcp => {
            run_mcp_stdio()?;
        }
        Commands::Login {
            issuer,
            client_id,
            audience,
            resource,
            scope,
            no_browser,
            json,
        } => {
            let result = run_login(DeviceLoginOptions {
                issuer,
                client_id,
                audience,
                resource,
                scope,
                no_browser,
            })?;
            if json {
                return Ok(Some(result));
            }
            println!("login: ready");
            println!(
                "token_path: {}",
                result["token_path"].as_str().unwrap_or("unknown")
            );
            if let Some(path) = result["refresh_token_path"].as_str() {
                println!("refresh_token_path: {path}");
            }
            return Ok(Some(result));
        }
        Commands::Logout { dry_run, json } => {
            let result = run_logout(dry_run)?;
            if json {
                return Ok(Some(result));
            }
            println!("logout: {}", result["status"].as_str().unwrap_or("unknown"));
            println!("dry_run: {}", result["dry_run"].as_bool().unwrap_or(false));
            if let Some(files) = result["files"].as_array() {
                for file in files {
                    println!(
                        "auth_file: {} {}",
                        file["kind"].as_str().unwrap_or("unknown"),
                        file["status"].as_str().unwrap_or("unknown")
                    );
                    if let Some(path) = file["path"].as_str() {
                        println!("auth_file_path: {path}");
                    }
                }
            }
            if result["env"]["present"].as_bool().unwrap_or(false) {
                println!(
                    "env_token: still_set ({})",
                    result["env"]["name"]
                        .as_str()
                        .unwrap_or(BUCEPHALUS_CLOUD_USER_TOKEN_ENV)
                );
                if let Some(note) = result["env"]["note"].as_str() {
                    println!("env_next: {note}");
                }
            }
            return Ok(Some(result));
        }
        Commands::Update {
            version,
            install_dir,
            repo,
            base_url,
            setup,
            modify_path,
            dry_run,
            json,
        } => {
            let result = run_update(UpdateOptions {
                version,
                install_dir,
                repo,
                base_url,
                setup,
                no_modify_path: !modify_path,
                dry_run,
            })?;
            if json {
                return Ok(Some(result));
            }
            println!("update: {}", if dry_run { "planned" } else { "complete" });
            let requested_version = result["version"].as_str().unwrap_or("unknown");
            let installed_version = result["installed_version"]
                .as_str()
                .unwrap_or(requested_version);
            println!(
                "version: {}",
                installed_version
            );
            if installed_version != requested_version {
                println!("requested_version: {}", requested_version);
            }
            println!(
                "install_dir: {}",
                result["install_dir"].as_str().unwrap_or("unknown")
            );
            println!("setup: {}", result["setup"].as_bool().unwrap_or(false));
            return Ok(Some(result));
        }
        Commands::Daemon => {
            latch_daemon::run_latch_daemon()?;
        }
        Commands::Setup {
            command,
            client,
            project,
            no_daemon_service,
            no_start,
            no_mcp,
            dry_run,
            json,
        } => {
            let result = match command {
                Some(SetupCommands::Status { project, json }) => {
                    let result = run_setup_status(project.as_deref())?;
                    if json {
                        return Ok(Some(result));
                    }
                    result
                }
                Some(SetupCommands::Uninstall {
                    client,
                    project,
                    no_daemon_service,
                    no_mcp,
                    dry_run,
                    json,
                }) => {
                    let result = run_setup_uninstall(
                        project.as_deref(),
                        client,
                        no_daemon_service,
                        no_mcp,
                        dry_run,
                    )?;
                    if json {
                        return Ok(Some(result));
                    }
                    result
                }
                None => {
                    let result = run_setup(
                        client,
                        project,
                        no_daemon_service,
                        no_start,
                        no_mcp,
                        dry_run,
                    )?;
                    if json {
                        return Ok(Some(result));
                    }
                    result
                }
            };
            if result["schema_version"] == "bucephalus_setup_status_v1" {
                println!("binary: {}", result["binary"].as_str().unwrap_or(""));
                println!("home: {}", result["home"].as_str().unwrap_or(""));
                println!(
                    "daemon_service: {}",
                    result["daemon_service"]["status"]
                        .as_str()
                        .unwrap_or("unknown")
                );
                println!(
                    "daemon_status: {}",
                    result["daemon_status"]["status"]
                        .as_str()
                        .unwrap_or("unknown")
                );
                println!(
                    "auth: {}",
                    result["auth"]["status"].as_str().unwrap_or("unknown")
                );
                if result["auth"]["status"].as_str() == Some("missing") {
                    println!("auth_next: bucephalus login");
                }
                if let Some(clients) = result["mcp"]["clients"].as_array() {
                    for client in clients {
                        println!(
                            "mcp_client: {} {}",
                            client["client"].as_str().unwrap_or("unknown"),
                            client["status"].as_str().unwrap_or("unknown")
                        );
                    }
                }
                return Ok(Some(result));
            }
            if result["schema_version"] == "bucephalus_setup_uninstall_v1" {
                println!(
                    "daemon_service: {}",
                    result["daemon_service"]["status"]
                        .as_str()
                        .unwrap_or("unknown")
                );
                println!(
                    "mcp: {}",
                    result["mcp"]["status"].as_str().unwrap_or("unknown")
                );
                if let Some(reason) = result["mcp"]["reason"].as_str() {
                    println!("mcp_reason: {reason}");
                }
                if let Some(clients) = result["mcp"]["clients"].as_array() {
                    for client in clients {
                        println!(
                            "mcp_client: {} {}",
                            client["client"].as_str().unwrap_or("unknown"),
                            client["status"].as_str().unwrap_or("unknown")
                        );
                        if let Some(path) = client["path"].as_str() {
                            println!("mcp_config_path: {path}");
                        }
                        if let Some(reason) = client["reason"].as_str() {
                            println!("mcp_client_reason: {reason}");
                        }
                    }
                }
                if let Some(actions) = result["mcp"]["actions"].as_array() {
                    for action in actions {
                        if let Some(command) = action["command"].as_str() {
                            println!("mcp_next: {command}");
                        }
                    }
                }
                return Ok(Some(result));
            }
            println!("binary: {}", result["binary"].as_str().unwrap_or(""));
            println!("home: {}", result["home"].as_str().unwrap_or(""));
            println!(
                "daemon_service: {}",
                result["daemon_service"]["status"]
                    .as_str()
                    .unwrap_or("unknown")
            );
            if let Some(path) = result["daemon_service"]["path"].as_str() {
                println!("daemon_service_path: {path}");
            }
            println!(
                "daemon_status: {}",
                result["daemon_status"]["status"]
                    .as_str()
                    .unwrap_or("unknown")
            );
            if let Some(clients) = result["mcp"]["clients"].as_array() {
                for client in clients {
                    println!(
                        "mcp_client: {} {}",
                        client["client"].as_str().unwrap_or("unknown"),
                        client["status"].as_str().unwrap_or("unknown")
                    );
                    if let Some(path) = client["path"].as_str() {
                        println!("mcp_config_path: {path}");
                    }
                }
            }
            println!(
                "auth: {}",
                result["auth"]["status"].as_str().unwrap_or("unknown")
            );
            if result["auth"]["status"].as_str() == Some("missing") {
                println!("auth_next: bucephalus login");
            }
            return Ok(Some(result));
        }
        Commands::Dev {
            target,
            out,
            overrides,
            executor,
            run_root,
            runtime_env,
            runtime_env_file,
            secret_file,
            json,
        } => {
            let experiment = resolve_experiment_target(target.as_deref())?;
            if !json {
                eprintln!("building package from: {}", experiment.display());
            }
            let build = build_experiment_package_for_build_run(
                &experiment,
                overrides.as_deref(),
                out.as_ref(),
            )?;
            let package_checks = lab_runner::check_package(&build.package_dir)?;
            if !json {
                print_package_check_report(&package_checks);
            }
            if !package_checks_passed(&package_checks) {
                return Err(anyhow!("package checks failed"));
            }
            let mut execution = build_run_execution_options(
                executor,
                Some(MaterializeArg::Full),
                run_root,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            if !json {
                execution.stdout_progress = true;
                eprintln!("running preflight...");
            }
            let preflight =
                lab_runner::preflight_experiment_with_options(&build.package_dir, &execution)?;
            if !json {
                print_preflight_report(&preflight);
            }
            if !preflight.passed {
                return Err(anyhow!("preflight failed"));
            }
            let summary =
                lab_runner::experiment_summary_with_options(&build.package_dir, &execution)?;
            if !json {
                print_summary(&summary);
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
                    "command": "dev",
                    "package_dir": build.package_dir.display().to_string(),
                    "manifest_path": build.manifest_path.display().to_string(),
                    "checksums_path": build.checksums_path.display().to_string(),
                    "package_checks_path": build.package_checks_path.display().to_string(),
                    "package_checks": package_checks,
                    "preflight": preflight_report_to_json(&preflight),
                    "summary": summary_to_json(&summary),
                    "run": run_result_to_json(&result),
                    "validation": experiment_bundle_validation_to_json(&validation),
                })));
            }
            println!("package_dir: {}", build.package_dir.display());
            println!("manifest: {}", build.manifest_path.display());
            println!("package_checks: {}", build.package_checks_path.display());
            println!("package_digest: {}", validation.package_digest);
            println!("smoke_run_id: {}", result.run_id);
            println!("smoke_run_dir: {}", result.run_dir.display());
            println!("smoke_tested: true");
        }
        Commands::Doctor {
            target,
            overrides,
            executor,
            run_root,
            runtime_env,
            runtime_env_file,
            secret_file,
            json,
        } => {
            let doctor_target = resolve_doctor_target(target.as_deref())?;
            let build = match doctor_target {
                DoctorTarget::Experiment(experiment) => {
                    if !json {
                        eprintln!("building package from: {}", experiment.display());
                    }
                    build_experiment_package_for_build_run(&experiment, overrides.as_deref(), None)?
                }
                DoctorTarget::Package(package) => {
                    if overrides.is_some() {
                        return Err(anyhow!(
                            "--overrides can only be used with an experiment YAML target"
                        ));
                    }
                    if !json {
                        eprintln!("checking package: {}", package.display());
                    }
                    let package_dir = package_directory_for_input(&package);
                    lab_runner::BuildResult {
                        manifest_path: package_dir.join("manifest.json"),
                        checksums_path: package_dir.join("checksums.json"),
                        package_checks_path: package_dir.join("package_checks.json"),
                        package_dir,
                    }
                }
            };
            let validation = lab_runner::register_experiment_bundle(&build.package_dir)?;
            let package_checks = lab_runner::check_package(&build.package_dir)?;
            if !json {
                print_package_check_report(&package_checks);
            }
            if !package_checks_passed(&package_checks) {
                if json {
                    return Ok(Some(json!({
                        "ok": false,
                        "command": "doctor",
                        "failed_at": "package_checks",
                        "package_dir": build.package_dir.display().to_string(),
                        "validation": experiment_bundle_validation_to_json(&validation),
                        "package_checks": package_checks,
                    })));
                }
                return Err(anyhow!("doctor found package check failures"));
            }
            let execution = build_run_execution_options(
                executor,
                Some(MaterializeArg::Full),
                run_root,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            if !json {
                eprintln!("running preflight...");
            }
            let preflight =
                lab_runner::preflight_experiment_with_options(&build.package_dir, &execution)?;
            if !json {
                print_preflight_report(&preflight);
            }
            if !preflight.passed {
                if json {
                    return Ok(Some(json!({
                        "ok": false,
                        "command": "doctor",
                        "failed_at": "preflight",
                        "package_dir": build.package_dir.display().to_string(),
                        "validation": experiment_bundle_validation_to_json(&validation),
                        "package_checks": package_checks,
                        "preflight": preflight_report_to_json(&preflight),
                    })));
                }
                return Err(anyhow!("doctor found preflight failures"));
            }
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "doctor",
                    "package_dir": build.package_dir.display().to_string(),
                    "manifest_path": build.manifest_path.display().to_string(),
                    "checksums_path": build.checksums_path.display().to_string(),
                    "package_checks_path": build.package_checks_path.display().to_string(),
                    "validation": experiment_bundle_validation_to_json(&validation),
                    "package_checks": package_checks,
                    "preflight": preflight_report_to_json(&preflight),
                })));
            }
            println!("doctor_ok: true");
            println!("package_dir: {}", build.package_dir.display());
            println!("package_digest: {}", validation.package_digest);
        }
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
        Commands::Lint {
            target,
            out,
            overrides,
            json,
        } => {
            let experiment = resolve_experiment_target_for_command("lint", target.as_deref())?;
            if !json {
                eprintln!("linting experiment: {}", experiment.display());
                eprintln!("building package...");
            }
            let build = build_experiment_package_for_build_run(
                &experiment,
                overrides.as_deref(),
                out.as_ref(),
            )?;
            let package_checks = lab_runner::check_package(&build.package_dir)?;
            let passed = package_checks_passed(&package_checks);
            let package_digest = package_checks
                .get("package_digest")
                .and_then(Value::as_str)
                .unwrap_or("");
            if json {
                return Ok(Some(json!({
                    "ok": passed,
                    "command": "lint",
                    "package_dir": build.package_dir.display().to_string(),
                    "manifest_path": build.manifest_path.display().to_string(),
                    "checksums_path": build.checksums_path.display().to_string(),
                    "package_checks_path": build.package_checks_path.display().to_string(),
                    "package_digest": package_digest,
                    "package_checks": package_checks,
                })));
            }
            print_package_check_report(&package_checks);
            if !passed {
                return Err(anyhow!("lint found package check failures"));
            }
            println!("lint_ok: true");
            println!("package_dir: {}", build.package_dir.display());
            println!("package_digest: {}", package_digest);
        }
        Commands::CheckPackage { package, json } => {
            let package_dir = resolve_package_command_target("check-package", &package)?;
            let report = lab_runner::check_package(&package_dir)?;
            if json {
                return Ok(Some(json!({
                    "ok": report.get("passed").and_then(Value::as_bool).unwrap_or(false),
                    "command": "check-package",
                    "package_dir": package_dir.display().to_string(),
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
            let package_dir = resolve_package_command_target("prepare-runtime-images", &package)?;
            if !json {
                eprintln!(
                    "preparing runtime images from package: {}",
                    package_dir.display()
                );
            }
            let report = lab_runner::prepare_runtime_images(
                &package_dir,
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
            let mut validation = lab_runner::register_experiment_bundle(&build.package_dir)?;
            let mut execution = build_run_execution_options(
                executor,
                materialize,
                run_root,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            if !json {
                execution.stdout_progress = true;
            }
            let summary =
                lab_runner::experiment_summary_with_options(&build.package_dir, &execution)?;
            if smoke_test && run_dangerously {
                return Err(anyhow!(
                    "--smoke-test and --run-dangerously are mutually exclusive"
                ));
            }
            let run_mode = if smoke_test {
                RunValidationAction::SmokeTest
            } else {
                RunValidationAction::FullRun
            };
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
            if matches!(run_mode, RunValidationAction::SmokeTest) {
                if !json {
                    eprintln!("launching smoke test...");
                }
                let result =
                    lab_runner::run_smoke_test_with_options(&build.package_dir, execution.clone())?;
                validation = lab_runner::mark_experiment_bundle_smoke_tested(
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
            let smoke_result = if !run_dangerously && !validation.smoke_tested {
                if !json {
                    eprintln!("launching smoke test...");
                }
                let result =
                    lab_runner::run_smoke_test_with_options(&build.package_dir, execution.clone())?;
                validation = lab_runner::mark_experiment_bundle_smoke_tested(
                    &build.package_dir,
                    &result.run_id,
                )?;
                if !json {
                    println!("smoke_run_id: {}", result.run_id);
                    println!("smoke_run_dir: {}", result.run_dir.display());
                    println!("smoke_tested: true");
                }
                Some(result)
            } else {
                None
            };
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
                        "smoke_run": smoke_result.as_ref().map(run_result_to_json),
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
            let built = if let Some(experiment) = experiment_input_path(&package)? {
                if !json {
                    eprintln!("building package from: {}", experiment.display());
                }
                Some(build_experiment_package_for_build_run(
                    &experiment,
                    None,
                    None,
                )?)
            } else {
                if !json {
                    eprintln!("loading package: {}", package.display());
                }
                None
            };
            let package_input_dir = package_directory_for_input(&package);
            let package_dir = built
                .as_ref()
                .map(|build| build.package_dir.as_path())
                .unwrap_or(package_input_dir.as_path());
            let yaml_input = built.is_some();
            let materialize = if yaml_input && materialize.is_none() {
                Some(MaterializeArg::Full)
            } else {
                materialize
            };
            let mut validation = lab_runner::register_experiment_bundle(package_dir)?;
            let mut execution = build_run_execution_options(
                executor,
                materialize,
                run_root,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            if !json {
                execution.stdout_progress = true;
            }
            let summary = lab_runner::experiment_summary_with_options(package_dir, &execution)?;
            let run_mode = if yaml_input {
                if smoke_test && run_dangerously {
                    return Err(anyhow!(
                        "--smoke-test and --run-dangerously are mutually exclusive"
                    ));
                }
                if smoke_test {
                    RunValidationAction::SmokeTest
                } else {
                    RunValidationAction::FullRun
                }
            } else {
                resolve_run_validation_action(
                    package_dir,
                    &validation,
                    smoke_test,
                    run_dangerously,
                    json,
                )?
            };
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
            if matches!(run_mode, RunValidationAction::SmokeTest) {
                if !json {
                    eprintln!("launching smoke test...");
                }
                let result =
                    lab_runner::run_smoke_test_with_options(package_dir, execution.clone())?;
                validation =
                    lab_runner::mark_experiment_bundle_smoke_tested(package_dir, &result.run_id)?;
                if json {
                    return Ok(Some(json!({
                        "ok": true,
                        "command": "run",
                        "mode": "smoke_test",
                        "input": if yaml_input { "experiment" } else { "package" },
                        "package_dir": package_dir.display().to_string(),
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
            let smoke_result = if yaml_input && !run_dangerously && !validation.smoke_tested {
                if !json {
                    eprintln!("launching smoke test...");
                }
                let result =
                    lab_runner::run_smoke_test_with_options(package_dir, execution.clone())?;
                validation =
                    lab_runner::mark_experiment_bundle_smoke_tested(package_dir, &result.run_id)?;
                if !json {
                    println!("smoke_run_id: {}", result.run_id);
                    println!("smoke_run_dir: {}", result.run_dir.display());
                    println!("smoke_tested: true");
                }
                Some(result)
            } else {
                None
            };
            if !json {
                print_summary(&summary);
                eprintln!("launching run...");
            }
            let result = lab_runner::run_experiment_with_options(package_dir, execution.clone())?;
            if json {
                let post_run = try_post_run_stats_json(&result.run_dir);
                return Ok(Some(json!({
                    "ok": true,
                    "command": "run",
                    "input": if yaml_input { "experiment" } else { "package" },
                    "package_dir": package_dir.display().to_string(),
                    "summary": summary_to_json(&summary),
                    "smoke_run": smoke_result.as_ref().map(run_result_to_json),
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
        Commands::Latch {
            command: LatchCommands::Validate { manifest, json },
        } => {
            let result = lab_runner::validate_latch_manifest_file(&manifest)?;
            let result_json = serde_json::to_value(&result)?;
            if json {
                return Ok(Some(result_json));
            }
            println!("manifest: {}", result.manifest_path.display());
            println!("schema_version: {}", result.schema_version);
            println!("case_count: {}", result.case_count);
            println!("default_launch_present: {}", result.default_launch_present);
            println!(
                "default_workspace_seed_present: {}",
                result.default_workspace_seed_present
            );
            return Ok(Some(result_json));
        }
        Commands::Latch {
            command: LatchCommands::Demo { out, json },
        } => {
            let result = write_latch_demo(&out)?;
            if json {
                return Ok(Some(result));
            }
            println!(
                "manifest: {}",
                result["manifest_path"].as_str().unwrap_or("")
            );
            println!("seed_dir: {}", result["seed_dir"].as_str().unwrap_or(""));
            if let Some(next) = result["next"].as_array() {
                for command in next {
                    if let Some(command) = command.as_str() {
                        println!("next: {command}");
                    }
                }
            }
            return Ok(Some(result));
        }
        Commands::Latch {
            command:
                LatchCommands::Smoke {
                    benchmark,
                    cases,
                    out,
                    run_root,
                    json,
                    argv,
                },
        } => {
            let resolution = resolve_latch_smoke_fixture(
                &out,
                &benchmark,
                cases,
                (!argv.is_empty()).then_some(argv),
            )?;
            let manifest_path = resolution["manifest_path"]
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| {
                    anyhow!("local latch smoke resolver did not return manifest_path")
                })?;
            let result = lab_runner::run_latch_manifest(lab_runner::LatchRunOptions {
                manifest_path,
                run_root,
                launch_override: None,
            })?;
            let run_json = serde_json::to_value(&result)?;
            let result_json = json!({
                "schema_version": "latch_smoke_result_v1",
                "resolution": resolution,
                "run": run_json
            });
            if json {
                return Ok(Some(result_json));
            }
            println!(
                "resolution: {}",
                result_json["resolution"]["resolution_path"]
                    .as_str()
                    .unwrap_or("")
            );
            println!(
                "manifest: {}",
                result_json["resolution"]["manifest_path"]
                    .as_str()
                    .unwrap_or("")
            );
            println!("latch_run_id: {}", result.run_id);
            println!("run_dir: {}", result.run_dir.display());
            for case in &result.cases {
                println!(
                    "case {}: {:?} exit={:?}",
                    case.case_id, case.status, case.exit_code
                );
            }
            return Ok(Some(result_json));
        }
        Commands::Latch {
            command:
                LatchCommands::Run {
                    manifest,
                    run_root,
                    json,
                    argv,
                },
        } => {
            let result = lab_runner::run_latch_manifest(lab_runner::LatchRunOptions {
                manifest_path: manifest,
                run_root,
                launch_override: (!argv.is_empty()).then_some(argv),
            })?;
            let result_json = serde_json::to_value(&result)?;
            if json {
                return Ok(Some(result_json));
            }
            println!("latch_run_id: {}", result.run_id);
            println!("run_dir: {}", result.run_dir.display());
            for case in &result.cases {
                let patch = case
                    .workspace_diff_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "no diff".to_string());
                println!(
                    "case {}: {:?} exit={:?} patch={}",
                    case.case_id, case.status, case.exit_code, patch
                );
                if let Some(error) = case.capture_error.as_ref() {
                    println!("  capture: {error}");
                }
            }
            return Ok(Some(result_json));
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
            let mut execution = build_run_execution_options(
                None,
                None,
                None,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            if !json {
                execution.stdout_progress = true;
            }
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
        Commands::Scores { run, json, csv } => {
            if json && csv {
                return Err(anyhow::anyhow!("--json and --csv are mutually exclusive"));
            }
            let run_dir = resolve_run_dir_arg(&run)?;
            let table = build_scores_table(&run_dir)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "scores",
                    "run_dir": run_dir.display().to_string(),
                    "result": query_table_to_json(&table),
                })));
            }
            if csv {
                print_query_table_csv(&table);
                return Ok(None);
            }
            if table.rows.is_empty() {
                println!("No score rows found for this run.");
                println!("Try: bucephalus explain-metrics {}", run);
                return Ok(None);
            }
            print_query_table(&table);
        }
        Commands::ExplainMetrics { run, json, csv } => {
            if json && csv {
                return Err(anyhow::anyhow!("--json and --csv are mutually exclusive"));
            }
            let run_dir = resolve_run_dir_arg(&run)?;
            let table = build_metric_explanation_table(&run_dir)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "explain-metrics",
                    "run_dir": run_dir.display().to_string(),
                    "result": query_table_to_json(&table),
                })));
            }
            if csv {
                print_query_table_csv(&table);
                return Ok(None);
            }
            if table.rows.is_empty() {
                println!("No declared or observed metrics found for this run.");
                return Ok(None);
            }
            print_query_table(&table);
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
                        if let Err(err) = std::io::stdout().flush() {
                            eprintln!("warning: failed to flush live view clear: {}", err);
                        }
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
                let run_count = table.rows.len();
                return Ok(Some(json!({
                    "ok": true,
                    "command": "runs",
                    "project_root": project_root.display().to_string(),
                    "run_count": run_count,
                    "result": query_table_to_json(&table),
                })));
            }
            if csv {
                print_query_table_csv(&table);
                return Ok(None);
            }
            if table.rows.is_empty() {
                print_empty_runs_hint(&project_root);
                return Ok(None);
            }
            print_query_table(&table);
        }
        Commands::SchemaValidate { schema, file, json } => {
            let compiled = schemas::compile_schema(&schema)?;
            let value = read_json_or_yaml_value(&file)?;
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
            let out_path = out.unwrap_or_else(|| default_debug_bundle_path(&run_dir));
            std::fs::create_dir_all(out_path.parent().unwrap_or_else(|| Path::new(".")))?;
            provenance::build_debug_bundle(&run_dir, &out_path)?;
            if json {
                return Ok(Some(json!({
                    "ok": true,
                    "command": "publish",
                    "artifact_kind": "local_debug_bundle",
                    "bundle": out_path.display().to_string(),
                    "run_dir": run_dir.display().to_string(),
                    "review_before_sharing": true,
                })));
            }
            println!("bundle: {}", out_path.display());
            println!("artifact_kind: local_debug_bundle");
            println!("review_before_sharing: true");
            println!(
                "note: structured JSON is redacted for common local-path and secret fields, but logs and agent outputs may still contain user data."
            );
        }
        Commands::Preflight {
            package,
            runtime_env,
            runtime_env_file,
            secret_file,
            json,
        } => {
            let package_dir = resolve_package_command_target("preflight", &package)?;
            if !json {
                eprintln!("running preflight: {}", package_dir.display());
            }
            let execution = build_run_execution_options(
                None,
                None,
                None,
                &runtime_env,
                &runtime_env_file,
                &secret_file,
            )?;
            let report = lab_runner::preflight_experiment_with_options(&package_dir, &execution)?;
            if json {
                return Ok(Some(json!({
                    "ok": report.passed,
                    "command": "preflight",
                    "package_dir": package_dir.display().to_string(),
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
        Commands::Clean {
            runs,
            force,
            include_active,
            dry_run,
            json,
        } => {
            if !runs {
                return Err(anyhow!(
                    "nothing selected to clean; pass --runs to clean local run artifacts"
                ));
            }
            let runs_dir = lab_runner::default_run_root()?;
            let entries = collect_run_inventory_under_root(&runs_dir)?;
            let mut report = clean_runs_preflight(
                &runs_dir,
                runs_dir.exists(),
                &entries,
                force,
                include_active,
                dry_run,
            )?;
            if !dry_run && report.exists {
                std::fs::remove_dir_all(&runs_dir)?;
                report.removed = true;
            }
            if json {
                return Ok(Some(clean_runs_report_to_json(&report)));
            }
            print_clean_runs_report(&report);
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
        Commands::Init { json, .. }
        | Commands::Login { json, .. }
        | Commands::Logout { json, .. }
        | Commands::Update { json, .. }
        | Commands::Dev { json, .. }
        | Commands::Doctor { json, .. }
        | Commands::Build { json, .. }
        | Commands::Lint { json, .. }
        | Commands::CheckPackage { json, .. }
        | Commands::PrepareRuntimeImages { json, .. }
        | Commands::BuildRun { json, .. }
        | Commands::Run { json, .. }
        | Commands::Replay { json, .. }
        | Commands::Fork { json, .. }
        | Commands::Pause { json, .. }
        | Commands::Resume { json, .. }
        | Commands::Continue { json, .. }
        | Commands::Recover { json, .. }
        | Commands::Kill { json, .. }
        | Commands::Scores { json, .. }
        | Commands::ExplainMetrics { json, .. }
        | Commands::Views { json, .. }
        | Commands::Query { json, .. }
        | Commands::Runs { json, .. }
        | Commands::SchemaValidate { json, .. }
        | Commands::Publish { json, .. }
        | Commands::Preflight { json, .. }
        | Commands::Clean { json, .. } => *json,
        Commands::Setup { command, json, .. } => match command {
            Some(SetupCommands::Status { json, .. })
            | Some(SetupCommands::Uninstall { json, .. }) => *json,
            None => *json,
        },
        Commands::Latch {
            command:
                LatchCommands::Validate { json, .. }
                | LatchCommands::Demo { json, .. }
                | LatchCommands::Smoke { json, .. }
                | LatchCommands::Run { json, .. },
        } => *json,
        _ => false,
    }
}

fn run_result_to_json(result: &lab_runner::RunResult) -> Value {
    json!({
        "run_id": result.run_id,
        "run_dir": result.run_dir.display().to_string(),
        "run_store_location": result.account_db_path.display().to_string()
    })
}

fn run_artifacts_to_json(result: &lab_runner::RunResult) -> Value {
    let objects = result.run_dir.join("objects");
    let summary_dir = result.run_dir.join("evaluation");
    let summary_path = existing_evaluation_summary_path(&result.run_dir);
    json!({
        "run_store_location": result.account_db_path.display().to_string(),
        "objects_dir": objects.display().to_string(),
        "evaluation_summary_dir": summary_dir.display().to_string(),
        "evaluation_summary_path": summary_path
            .as_ref()
            .map(|path| path.display().to_string())
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
    if let Ok(Some(value)) = lab_runner::load_runtime_value_from_store(run_dir, key) {
        return Some(value);
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
        let (key_raw, val_raw) = parse_key_value_arg("--set", raw, "KEY=VALUE")?;
        if key_raw.trim().is_empty() {
            return Err(anyhow!("invalid --set entry: key cannot be empty"));
        }
        validate_cli_set_key(key_raw)
            .map_err(|err| anyhow!("invalid --set key '{}': {}", key_raw, err))?;
        if out.contains_key(key_raw) {
            return Err(anyhow!("duplicate --set key '{}'", key_raw));
        }
        let parsed =
            serde_json::from_str::<Value>(val_raw).unwrap_or(Value::String(val_raw.to_string()));
        out.insert(key_raw.to_string(), parsed);
    }
    Ok(out)
}

fn validate_cli_set_key(key: &str) -> Result<()> {
    for segment in key.split('.') {
        if segment.trim().is_empty() {
            return Err(anyhow!("dotted path segments cannot be empty"));
        }
        if segment.trim() != segment {
            return Err(anyhow!(
                "dotted path segments cannot contain leading or trailing whitespace"
            ));
        }
    }
    Ok(())
}

fn parse_key_value_arg<'a>(flag: &str, raw: &'a str, expected: &str) -> Result<(&'a str, &'a str)> {
    raw.split_once('=')
        .ok_or_else(|| anyhow!("invalid {flag} entry: expected {expected}"))
}

fn validate_cli_env_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(anyhow!("key cannot be empty"));
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(anyhow!(
            "key must be a portable environment variable name like OPENAI_API_KEY"
        ));
    }
    if !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        return Err(anyhow!(
            "key must be a portable environment variable name like OPENAI_API_KEY"
        ));
    }
    Ok(())
}

fn parse_runtime_env_bindings(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for raw in values {
        let (key_raw, value_raw) = parse_key_value_arg("--env", raw, "KEY=VALUE")?;
        let key = key_raw.trim();
        if key.is_empty() {
            return Err(anyhow!("invalid --env entry: key cannot be empty"));
        }
        validate_cli_env_name(key)
            .map_err(|err| anyhow!("invalid --env key '{}': {}", key, err))?;
        if out.contains_key(key) {
            return Err(anyhow!("duplicate --env key '{}'", key));
        }
        out.insert(key.to_string(), value_raw.to_string());
    }
    Ok(out)
}

fn parse_secret_file_bindings(values: &[String]) -> Result<BTreeMap<String, PathBuf>> {
    let mut out = BTreeMap::new();
    for raw in values {
        let (key_raw, value_raw) = parse_key_value_arg("--secret-file", raw, "ID=PATH")?;
        let key = key_raw.trim();
        if key.is_empty() {
            return Err(anyhow!("invalid --secret-file entry: id cannot be empty"));
        }
        let value = value_raw.trim();
        if value.is_empty() {
            return Err(anyhow!(
                "invalid --secret-file id '{}': path cannot be empty",
                key
            ));
        }
        if out.contains_key(key) {
            return Err(anyhow!("duplicate --secret-file id '{}'", key));
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
        stdout_progress: false,
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
        "causal_extraction": summary.causal_extraction,
        "scheduling": summary.scheduling,
        "state_policy": summary.state_policy,
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
        "package_checks: passed={} checks={} failed={} warnings={}",
        report
            .get("passed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        summary.get("checks").and_then(Value::as_u64).unwrap_or(0),
        summary.get("failed").and_then(Value::as_u64).unwrap_or(0),
        summary.get("warnings").and_then(Value::as_u64).unwrap_or(0)
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
    if let Some(run_dir) = lab_runner::resolve_run_dir_from_store(run, cwd.as_path())? {
        return run_dir.canonicalize().map_err(|err| {
            anyhow::anyhow!(
                "stored run path for '{}' is not accessible: {} ({})",
                run,
                run_dir.display(),
                err
            )
        });
    }

    Err(anyhow::anyhow!(format!(
        "run '{}' not found in the configured runtime store",
        run
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
                    .unwrap_or_else(|| inspect_run_inventory_entry(run_dir, None));
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
                    .unwrap_or_else(|| inspect_run_inventory_entry(run_dir, None));
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
                let run_dir = current_run_dir
                    .as_ref()
                    .ok_or_else(|| anyhow!("interactive view picker requires a selected run"))?;
                let run_entry = lookup_run_inventory(&run_entries, run_dir)
                    .unwrap_or_else(|| inspect_run_inventory_entry(run_dir, None));
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
                let run_dir = current_run_dir
                    .as_ref()
                    .ok_or_else(|| anyhow!("interactive viewer requires a selected run"))?;
                let run_entry = lookup_run_inventory(&run_entries, run_dir)
                    .unwrap_or_else(|| inspect_run_inventory_entry(run_dir, None));
                let run_view_set = analysis::run_view_set(run_dir)?;
                let resolved_view = match current_view.clone() {
                    Some(view) => view,
                    None => {
                        let raw = list_available_analysis_views(run_dir);
                        resolve_requested_view(run_view_set, &raw, "run_progress")?
                    }
                };
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
        "latest_agent_output" => Ok(build_latest_agent_output_table(run_dir)),
        other => Err(anyhow!(
            "view '{}' requires the analysis query engine; live state views are available for run_progress, health, latest_agent_output, and scoreboard",
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

    let mut rows: Vec<(Option<i64>, String, Vec<Value>)> = Vec::with_capacity(active_trials.len());
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
        let schedule_idx = entry.get("schedule_idx").and_then(json_i64);
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
        rows.push((schedule_idx, trial_id, row));
    }

    rows.sort_by(|a, b| {
        match (a.0, b.0) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| a.1.cmp(&b.1))
    });
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

const SCORE_DIAGNOSTIC_METRICS: &[&str] = &[
    "status_code",
    "success",
    "mapped_grader_output_state",
    "trial_conclusion_reported_outcome",
    "trial_conclusion_grader",
    "trial_conclusion_grader_strategy",
    "trial_conclusion_payload",
    "grade_error",
    "grade_error_reason",
    "primary_metric_auto_selected",
    "primary_metric_auto_selected_reason",
];

fn score_metric_filter_sql(alias: &str) -> String {
    let names = SCORE_DIAGNOSTIC_METRICS
        .iter()
        .map(|name| sql_string_literal(name))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}.metric_name NOT IN ({})", alias, names)
}

fn is_score_diagnostic_metric(name: &str) -> bool {
    SCORE_DIAGNOSTIC_METRICS.contains(&name)
}

fn build_scores_table(run_dir: &Path) -> Result<analysis::QueryTable> {
    let metric_names = fetch_score_metric_names(run_dir, 64)?;
    let sql = build_scores_sql(&metric_names);
    let table = analysis::query_run(run_dir, &sql)?;
    Ok(with_score_mean_row(table))
}

fn fetch_score_metric_names(run_dir: &Path, metric_limit: usize) -> Result<Vec<String>> {
    let filter = score_metric_filter_sql("m");
    let sql = format!(
        "SELECT m.metric_name
        FROM metrics_long m
        WHERE {}
          AND m.metric_name IS NOT NULL
          AND trim(m.metric_name) <> ''
        GROUP BY metric_name
        ORDER BY metric_name
        LIMIT {}",
        filter, metric_limit
    );
    let table = analysis::query_run(run_dir, &sql)?;
    Ok(table
        .rows
        .into_iter()
        .filter_map(|row| row.first().and_then(Value::as_str).map(str::to_string))
        .filter(|name| !name.trim().is_empty())
        .collect())
}

fn build_scores_sql(metric_names: &[String]) -> String {
    let mut columns = Vec::new();
    for metric_name in metric_names {
        columns.push(format!(
            "(SELECT m.metric_value
              FROM metrics_long m
              WHERE m.trial_id = t.trial_id
                AND m.metric_name = {}
              ORDER BY m.row_seq
              LIMIT 1) AS {}",
            sql_string_literal(metric_name),
            sql_identifier(metric_name)
        ));
    }
    let dynamic_cols = if columns.is_empty() {
        String::new()
    } else {
        format!(",\n    {}", columns.join(",\n    "))
    };
    format!(
        "SELECT
            t.trial_id,
            t.task_id,
            t.variant_id,
            t.outcome,
            t.primary_metric_name,
            t.primary_metric_value{}
        FROM trials t
        ORDER BY t.schedule_idx, t.trial_id, t.variant_id",
        dynamic_cols
    )
}

fn with_score_mean_row(mut table: analysis::QueryTable) -> analysis::QueryTable {
    if table.rows.is_empty() {
        return table;
    }
    let numeric_start = table
        .columns
        .iter()
        .position(|column| column == "primary_metric_value")
        .unwrap_or(table.columns.len());
    let mut mean_row = vec![Value::Null; table.columns.len()];
    if !mean_row.is_empty() {
        mean_row[0] = json!("mean");
    }

    let mut has_numeric_mean = false;
    for col_idx in numeric_start..table.columns.len() {
        let values = table
            .rows
            .iter()
            .filter_map(|row| row.get(col_idx).and_then(json_number_value))
            .collect::<Vec<_>>();
        if values.is_empty() {
            continue;
        }
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        mean_row[col_idx] = json!(round_four(mean));
        has_numeric_mean = true;
    }
    if has_numeric_mean {
        table.rows.push(mean_row);
    }
    table
}

#[derive(Clone, Debug, Default)]
struct MetricExplanation {
    metric_id: String,
    label: String,
    value_type: String,
    direction: String,
    source_type: String,
    source_pointer: String,
    source_output: String,
    required: bool,
    primary: bool,
    observed_rows: i64,
    resolved_rows: i64,
    numeric_rows: i64,
    numeric_sum: f64,
    mean_value: Option<f64>,
    first_value: Value,
}

fn build_metric_explanation_table(run_dir: &Path) -> Result<analysis::QueryTable> {
    let mut metrics: BTreeMap<String, MetricExplanation> = BTreeMap::new();
    let definitions = analysis::query_run(
        run_dir,
        "SELECT
            metric_id,
            label,
            value_type,
            direction,
            source_type,
            source_pointer,
            definition_json,
            required,
            primary_metric
         FROM metric_definitions
         ORDER BY COALESCE(CAST(primary_metric AS INTEGER), 0) DESC, metric_id",
    )?;
    for row in definitions.rows {
        let metric_id = row_value_str(&row, 0);
        if metric_id.is_empty() {
            continue;
        }
        let source_output = row
            .get(6)
            .and_then(metric_definition_source_output)
            .unwrap_or_default();
        metrics.insert(
            metric_id.clone(),
            MetricExplanation {
                metric_id,
                label: row_value_str(&row, 1),
                value_type: row_value_str(&row, 2),
                direction: row_value_str(&row, 3),
                source_type: row_value_str(&row, 4),
                source_pointer: row_value_str(&row, 5),
                source_output,
                required: row_value_bool(&row, 7),
                primary: row_value_bool(&row, 8),
                ..MetricExplanation::default()
            },
        );
    }

    let observed = analysis::query_run(
        run_dir,
        "SELECT metric_name, metric_value
         FROM metrics_long
         ORDER BY metric_name, row_seq",
    )?;
    for row in observed.rows {
        let metric_id = row_value_str(&row, 0);
        if metric_id.is_empty() {
            continue;
        }
        let entry = metrics
            .entry(metric_id.clone())
            .or_insert_with(|| MetricExplanation {
                metric_id,
                ..MetricExplanation::default()
            });
        let metric_value = row.get(1).cloned().unwrap_or(Value::Null);
        if entry.observed_rows == 0 {
            entry.first_value = metric_value.clone();
        }
        entry.observed_rows += 1;
        if !metric_value.is_null() {
            entry.resolved_rows += 1;
        }
        if let Some(numeric_value) = json_number_value(&metric_value) {
            entry.numeric_rows += 1;
            entry.numeric_sum += numeric_value;
            entry.mean_value = Some(entry.numeric_sum / entry.numeric_rows as f64);
        }
    }

    let rows = metrics
        .into_values()
        .map(|metric| {
            let status = metric_explanation_status(&metric);
            let source = source_display(&metric);
            let scoreboard_column =
                if metric.numeric_rows > 0 && !is_score_diagnostic_metric(&metric.metric_id) {
                    if metric.primary {
                        format!(
                            "primary_metric_mean, {}_mean",
                            sanitize_scoreboard_alias(&metric.metric_id)
                        )
                    } else {
                        format!("{}_mean", sanitize_scoreboard_alias(&metric.metric_id))
                    }
                } else {
                    String::new()
                };
            vec![
                json!(status),
                json!(metric.metric_id),
                empty_string_as_null(metric.label),
                json!(source),
                empty_string_as_null(metric.value_type),
                empty_string_as_null(metric.direction),
                json!(metric.required),
                json!(metric.primary),
                json!(metric.observed_rows),
                json!(metric.resolved_rows),
                json!(metric.numeric_rows),
                metric
                    .mean_value
                    .map(|value| json!(round_four(value)))
                    .unwrap_or(Value::Null),
                metric.first_value,
                empty_string_as_null(scoreboard_column),
            ]
        })
        .collect();

    Ok(analysis::QueryTable {
        columns: vec![
            "status".to_string(),
            "metric_id".to_string(),
            "label".to_string(),
            "source".to_string(),
            "value_type".to_string(),
            "direction".to_string(),
            "required".to_string(),
            "primary_metric".to_string(),
            "metric_rows".to_string(),
            "resolved_rows".to_string(),
            "numeric_rows".to_string(),
            "mean_value".to_string(),
            "first_value".to_string(),
            "scoreboard_column".to_string(),
        ],
        rows,
    })
}

fn metric_explanation_status(metric: &MetricExplanation) -> &'static str {
    if is_score_diagnostic_metric(&metric.metric_id) {
        "diagnostic"
    } else if metric.source_type.is_empty() && metric.observed_rows > 0 {
        "observed_without_definition"
    } else if metric.observed_rows > 0 {
        "declared_observed"
    } else {
        "declared_missing"
    }
}

fn source_display(metric: &MetricExplanation) -> String {
    let pointer_suffix = metric
        .source_pointer
        .strip_prefix('/')
        .unwrap_or(&metric.source_pointer)
        .replace('/', ".");
    match metric.source_type.as_str() {
        "grader_output" if !metric.source_output.is_empty() && !pointer_suffix.is_empty() => {
            format!("grader.{}.{}", metric.source_output, pointer_suffix)
        }
        "grader_output" if !metric.source_output.is_empty() => {
            format!("grader.{}", metric.source_output)
        }
        "runtime_output" if !metric.source_output.is_empty() && !pointer_suffix.is_empty() => {
            format!("runtime.{}.{}", metric.source_output, pointer_suffix)
        }
        "runtime_output" if !metric.source_output.is_empty() => {
            format!("runtime.{}", metric.source_output)
        }
        "agent_response" if !metric.source_pointer.is_empty() => {
            format!("agent_response{}", metric.source_pointer)
        }
        other if !other.is_empty() => other.to_string(),
        _ => String::new(),
    }
}

fn empty_string_as_null(value: String) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        Value::String(value)
    }
}

fn row_value_str(row: &[Value], idx: usize) -> String {
    row.get(idx)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn row_value_bool(row: &[Value], idx: usize) -> bool {
    row.get(idx).is_some_and(|value| match value {
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_i64().unwrap_or(0) != 0,
        Value::String(value) => matches!(value.as_str(), "true" | "1"),
        _ => false,
    })
}

fn metric_definition_source_output(value: &Value) -> Option<String> {
    let parsed = match value {
        Value::Object(_) => value.clone(),
        Value::String(raw) => serde_json::from_str(raw).ok()?,
        _ => return None,
    };
    parsed
        .pointer("/source/output")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn json_number_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(raw) => raw.parse::<f64>().ok(),
        Value::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn round_four(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn fetch_scoreboard_metric_names(run_dir: &Path, metric_limit: usize) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT metric_name
         FROM metrics_long m
         WHERE {}
         GROUP BY metric_name
         ORDER BY metric_name
         LIMIT {}",
        score_metric_filter_sql("m"),
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
                return usize::from(ws.ws_col);
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
    let total = usize::try_from(value.get("total_slots")?.as_u64()?).ok()?;
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

fn build_latest_agent_output_table(run_dir: &Path) -> analysis::QueryTable {
    let mut attempts = collect_trial_attempt_states(run_dir);
    attempts.sort_by(|a, b| {
        a.schedule_idx
            .cmp(&b.schedule_idx)
            .then_with(|| a.trial_id.cmp(&b.trial_id))
            .then_with(|| a.attempt.cmp(&b.attempt))
    });

    let mut rows = Vec::new();
    for attempt in attempts {
        append_latest_agent_output_rows(&mut rows, &attempt);
    }

    analysis::QueryTable {
        columns: latest_agent_output_columns(),
        rows,
    }
}

#[derive(Clone, Debug)]
struct TrialAttemptOutputState {
    trial_id: String,
    schedule_idx: Option<i64>,
    attempt: Option<i64>,
    variant_id: String,
    task_id: String,
    phase: String,
    updated_at: String,
    state: Value,
    state_path: PathBuf,
}

fn latest_agent_output_columns() -> Vec<String> {
    [
        "state",
        "agent_result_state",
        "trial_id",
        "variant_id",
        "task_id",
        "schedule_idx",
        "attempt",
        "phase",
        "output_id",
        "format",
        "preview",
        "agent_result_json",
        "agent_result_path",
        "candidate_artifact_state",
        "candidate_artifact_source",
        "candidate_payload_json",
        "agent_stdout_path",
        "agent_stderr_path",
        "state_path",
        "updated_at",
    ]
    .iter()
    .map(|value| value.to_string())
    .collect()
}

fn collect_trial_attempt_states(run_dir: &Path) -> Vec<TrialAttemptOutputState> {
    let mut attempts = Vec::new();
    let trials_dir = run_dir.join("trials");
    let Ok(entries) = fs::read_dir(&trials_dir) else {
        return attempts;
    };

    for entry in entries.flatten() {
        let trial_dir = entry.path();
        if !trial_dir.is_dir() {
            continue;
        }
        let trial_id = trial_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        if trial_id.is_empty() {
            continue;
        }
        for state_path in [
            trial_dir.join("runner").join("trial_runtime_state.json"),
            trial_dir.join("trial_runtime_state.json"),
        ] {
            let Some(raw) = read_json_file(&state_path) else {
                continue;
            };
            let state = raw.get("state").cloned().unwrap_or_else(|| raw.clone());
            attempts.push(TrialAttemptOutputState {
                trial_id: state
                    .pointer("/trial_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&trial_id)
                    .to_string(),
                schedule_idx: state
                    .pointer("/slot/schedule_idx")
                    .and_then(json_i64)
                    .or_else(|| state.pointer("/key/schedule_idx").and_then(json_i64)),
                attempt: state.pointer("/key/attempt").and_then(json_i64),
                variant_id: state
                    .pointer("/slot/variant_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                task_id: state
                    .pointer("/slot/task_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                phase: state
                    .get("phase")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                updated_at: raw
                    .get("updated_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                state,
                state_path,
            });
            break;
        }
    }

    attempts
}

fn append_latest_agent_output_rows(rows: &mut Vec<Vec<Value>>, attempt: &TrialAttemptOutputState) {
    let agent_result_path = agent_result_path(attempt);
    let agent_stdout_path = state_string(&attempt.state, "/agent_phase/stdout_path");
    let agent_stderr_path = state_string(&attempt.state, "/agent_phase/stderr_path");
    let candidate_state = state_string(&attempt.state, "/candidate_artifact/state");
    let candidate_source = state_string(&attempt.state, "/candidate_artifact/source");
    let candidate_payload = attempt
        .state
        .pointer("/candidate_artifact/payload")
        .cloned()
        .unwrap_or(Value::Null);
    let candidate_display_state =
        candidate_artifact_display_state(&candidate_state, &candidate_source, &candidate_payload);
    let mut push_row = |state: &str,
                        agent_result_state: &str,
                        output_id: &str,
                        format: &str,
                        preview: &str,
                        agent_result_json: Value,
                        agent_result_path: Option<&Path>,
                        candidate_payload_json: Value| {
        rows.push(vec![
            json!(state),
            json!(agent_result_state),
            json!(attempt.trial_id),
            json!(attempt.variant_id),
            json!(attempt.task_id),
            attempt
                .schedule_idx
                .map_or(Value::Null, |value| json!(value)),
            attempt.attempt.map_or(Value::Null, |value| json!(value)),
            json!(attempt.phase),
            json!(output_id),
            json!(format),
            json!(preview),
            agent_result_json,
            agent_result_path
                .map(|path| json!(path.display().to_string()))
                .unwrap_or(Value::Null),
            json!(candidate_display_state),
            if candidate_source.is_empty() {
                Value::Null
            } else {
                json!(candidate_source)
            },
            candidate_payload_json,
            if agent_stdout_path.is_empty() {
                Value::Null
            } else {
                json!(agent_stdout_path)
            },
            if agent_stderr_path.is_empty() {
                Value::Null
            } else {
                json!(agent_stderr_path)
            },
            json!(attempt.state_path.display().to_string()),
            if attempt.updated_at.is_empty() {
                Value::Null
            } else {
                json!(attempt.updated_at)
            },
        ]);
    };

    if let Some(path) = agent_result_path.as_ref() {
        if path.exists() {
            match read_json_file(path) {
                Some(result) => {
                    push_row(
                        "agent_result_file",
                        "valid",
                        "BUCEPHALUS_RESULT_PATH",
                        "json",
                        &preview_agent_output_value(&result),
                        result,
                        Some(path),
                        candidate_payload,
                    );
                    return;
                }
                None => {
                    push_row(
                        "invalid_agent_result_json",
                        "invalid",
                        "BUCEPHALUS_RESULT_PATH",
                        "json",
                        "agent result file exists but is not valid JSON",
                        Value::Null,
                        Some(path),
                        candidate_payload,
                    );
                    return;
                }
            }
        }
    }

    if !candidate_payload.is_null() {
        push_row(
            if candidate_state.is_empty() {
                "candidate_artifact"
            } else {
                candidate_state.as_str()
            },
            "missing",
            "candidate_artifact.payload",
            "",
            &preview_agent_output_value(&candidate_payload),
            Value::Null,
            agent_result_path.as_deref(),
            candidate_payload,
        );
        return;
    }

    push_row(
        "missing_agent_result_file",
        "missing",
        "BUCEPHALUS_RESULT_PATH",
        "",
        "no agent result file or candidate artifact payload found",
        Value::Null,
        agent_result_path.as_deref(),
        candidate_payload,
    );
}

fn candidate_artifact_display_state(
    candidate_state: &str,
    candidate_source: &str,
    candidate_payload: &Value,
) -> &'static str {
    let has_artifact = !candidate_source.trim().is_empty() || !candidate_payload.is_null();
    if !has_artifact {
        "none"
    } else if candidate_state.trim().is_empty() {
        "present"
    } else {
        match candidate_state {
            "valid" => "valid",
            "invalid" => "invalid",
            "missing" => "none",
            _ => "present",
        }
    }
}

fn agent_result_path(attempt: &TrialAttemptOutputState) -> Option<PathBuf> {
    let attempt_dir = attempt
        .state
        .pointer("/fs/attempt_dir")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    let attempt_dir = path_from_state(attempt_dir);
    Some(attempt_dir.join("agent").join("result.json"))
}

fn path_from_state(value: &str) -> PathBuf {
    PathBuf::from(value)
}

fn state_string(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

fn preview_agent_output_value(value: &Value) -> String {
    let rendered = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    rendered
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(400)
        .collect()
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
        "events" | "raw_events" => {
            print_raw_events_stdout(table);
            true
        }
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

fn print_raw_events_stdout(table: &analysis::QueryTable) {
    let lines = format_raw_events_stdout(table);
    if lines.is_empty() {
        println!("(no events)");
        return;
    }
    for line in lines {
        println!("{line}");
    }
}

fn format_raw_events_stdout(table: &analysis::QueryTable) -> Vec<String> {
    let mut lines = Vec::new();
    let event_idx = table
        .columns
        .iter()
        .position(|column| column == "event_json");
    let metadata = [
        "trial_id",
        "schedule_idx",
        "row_seq",
        "variant_id",
        "task_id",
        "event_type",
    ];
    for row in &table.rows {
        let mut header = String::new();
        for name in metadata {
            let Some(idx) = table.columns.iter().position(|column| column == name) else {
                continue;
            };
            let value = row.get(idx).unwrap_or(&Value::Null);
            if value.is_null() {
                continue;
            }
            let rendered = render_json_cell(value);
            if rendered.trim().is_empty() || rendered == "null" {
                continue;
            }
            if !header.is_empty() {
                header.push(' ');
            }
            header.push_str(name);
            header.push('=');
            header.push_str(&rendered);
        }
        if !header.is_empty() {
            lines.push(format!("# {header}"));
        }
        if let Some(idx) = event_idx {
            let value = row.get(idx).unwrap_or(&Value::Null);
            let rendered = render_json_cell(value);
            if !rendered.trim().is_empty() && rendered != "null" {
                lines.extend(pretty_event_payload(&rendered).lines().map(str::to_string));
            }
        }
    }
    lines
}

fn pretty_event_payload(payload: &str) -> String {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| payload.to_string())
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
    let Some(report) = try_load_post_run_report(run_dir) else {
        return;
    };
    let summary = summarize_post_run_report(&report);
    println!();
    println!("--- run report ({}) ---", report.view_set.as_str());
    for (label, value) in &summary {
        println!("{label}: {value}");
    }
    if let Some(variants) = post_run_section(&report, "variant_summary") {
        if !variants.rows.is_empty() {
            println!();
            println!("variants:");
            print_query_table(variants);
        }
    }
    if let Some(events) = post_run_section(&report, "events") {
        println!();
        println!("events:");
        print_raw_events_stdout(events);
    }
    if let Some(path) = &report.evaluation_summary_path {
        println!();
        println!("evaluation summary: {}", path.display());
    }
    println!();
    println!("inspect:");
    println!("  live:      bucephalus views-live {} run_progress", run_id);
    println!("  scores:    bucephalus scores {}", run_id);
    println!("  metrics:   bucephalus explain-metrics {}", run_id);
    println!("  proof:     bucephalus views {} observability", run_id);
    println!("  trials:    bucephalus views {} trial_diagnostics", run_id);
    println!("  health:    bucephalus views {} health", run_id);
    println!("  matrix:    bucephalus views {} scoreboard", run_id);
    println!("  events:    bucephalus views {} events", run_id);
    println!("  resources: bucephalus views {} token_usage", run_id);
    println!("  errors:    bucephalus views {} run_errors", run_id);
}

fn try_post_run_stats_json(run_dir: &Path) -> Value {
    let Some(report) = try_load_post_run_report(run_dir) else {
        return Value::Null;
    };
    let mut sections = serde_json::Map::new();
    for section in &report.sections {
        sections.insert(
            section.name.to_string(),
            query_table_to_json(&section.table),
        );
    }
    let summary = summarize_post_run_report(&report)
        .into_iter()
        .map(|(label, value)| (label.to_string(), Value::String(value)))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "view_set": report.view_set.as_str(),
        "summary": Value::Object(summary),
        "sections": sections,
        "evaluation_summary_path": report
            .evaluation_summary_path
            .map(|path| path.display().to_string()),
    })
}

fn try_load_post_run_report(run_dir: &Path) -> Option<PostRunReport> {
    let view_set = analysis::run_view_set(run_dir).unwrap_or(analysis::ViewSet::CoreOnly);
    let mut sections = Vec::new();
    push_post_run_section(
        run_dir,
        &mut sections,
        "run_progress",
        "completion, pass rate, active-worker state",
        "run_progress",
        20,
        true,
    );
    push_post_run_section(
        run_dir,
        &mut sections,
        "observability",
        "proof-oriented result/event/agent/grader coverage",
        "observability_summary",
        20,
        false,
    );
    push_post_run_section(
        run_dir,
        &mut sections,
        "health",
        "score trust, connector failures, grader/mapping errors",
        "contract_health",
        20,
        true,
    );
    push_post_run_section(
        run_dir,
        &mut sections,
        "trial_diagnostics",
        "one row per trial with runtime evidence and log paths",
        "trial_diagnostics",
        20,
        false,
    );
    push_post_run_section(
        run_dir,
        &mut sections,
        "variant_summary",
        "per-variant outcomes and primary metric means",
        "variant_summary",
        20,
        false,
    );
    push_post_run_section(
        run_dir,
        &mut sections,
        "scoreboard",
        "bounded task-by-variant matrix slice",
        "task_variant_matrix",
        20,
        false,
    );
    push_post_run_section(
        run_dir,
        &mut sections,
        "token_usage",
        "per-variant token totals from event streams",
        "token_usage_by_variant",
        20,
        false,
    );
    push_post_run_section(
        run_dir,
        &mut sections,
        "tool_usage",
        "per-variant tool-call counts from event streams",
        "tool_usage_by_variant",
        20,
        false,
    );
    push_post_run_section(
        run_dir,
        &mut sections,
        "run_errors",
        "event-stream errors/failures; empty means none were observed in events",
        "run_errors",
        20,
        false,
    );
    if let Ok(events) = analysis::query_run(
        run_dir,
        "SELECT * FROM raw_events ORDER BY schedule_idx, row_seq, trial_id",
    ) {
        sections.push(PostRunSection {
            name: "events",
            table: events,
        });
    }
    let evaluation_summary_path = existing_evaluation_summary_path(run_dir);
    if sections.is_empty() && evaluation_summary_path.is_none() {
        return None;
    }
    Some(PostRunReport {
        view_set,
        sections,
        evaluation_summary_path,
    })
}

fn existing_evaluation_summary_path(run_dir: &Path) -> Option<PathBuf> {
    let current = run_dir.join("evaluation").join("summary.json");
    current.exists().then_some(current)
}

fn summarize_post_run_report(report: &PostRunReport) -> Vec<(&'static str, String)> {
    let progress = post_run_section(report, "run_progress");
    let completed_i = progress
        .and_then(|table| first_cell_as_i64(table, "completed_trials"))
        .unwrap_or(0);
    let successful_i = progress
        .and_then(|table| first_cell_as_i64(table, "successful_trials"))
        .unwrap_or(0);
    let failed_i = progress
        .and_then(|table| first_cell_as_i64(table, "failed_trials"))
        .unwrap_or_else(|| completed_i.saturating_sub(successful_i));
    let pass_rate = progress
        .and_then(|table| first_cell(table, "pass_rate"))
        .map(|value| format!("{value} success rate"))
        .unwrap_or_else(|| "not available".to_string());
    let trust = post_run_section(report, "health")
        .map(format_trust_summary)
        .unwrap_or_else(|| "not available".to_string());
    let proof = post_run_section(report, "observability")
        .map(format_observability_summary)
        .unwrap_or_else(|| "not available".to_string());
    let variant_count = post_run_section(report, "variant_summary")
        .map(|table| table.rows.len().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let error_count = post_run_section(report, "run_errors")
        .map(|table| sum_column_as_i64(table, "count"))
        .unwrap_or(0);
    let resource_summary = format_resource_summary(
        post_run_section(report, "token_usage"),
        post_run_section(report, "tool_usage"),
    );
    let evaluation = if report.evaluation_summary_path.is_some() {
        "available".to_string()
    } else {
        "not emitted".to_string()
    };
    let failure_signal = format_failure_signal(completed_i, failed_i);
    vec![
        (
            "what happened",
            format!(
                "{completed_i} trials completed; {successful_i} succeeded; {failed_i} failed; {pass_rate}"
            ),
        ),
        ("failure signal", failure_signal),
        ("proof coverage", proof),
        ("can I trust it", trust),
        ("comparison", format!("{variant_count} variants; matrix available on demand")),
        (
            "unrelated errors",
            if error_count == 0 {
                "none observed in event stream".to_string()
            } else {
                format!("{error_count} error/failure events observed")
            },
        ),
        ("resource signal", resource_summary),
        ("evaluation detail", evaluation),
    ]
}
fn format_observability_summary(table: &analysis::QueryTable) -> String {
    let verdict = first_cell(table, "diagnostic_verdict").unwrap_or_else(|| "unknown".to_string());
    let seen = first_cell_as_i64(table, "trials_seen").unwrap_or(0);
    let completed = first_cell_as_i64(table, "completed_trials").unwrap_or(0);
    let with_events = first_cell_as_i64(table, "trials_with_events").unwrap_or(0);
    let with_tools = first_cell_as_i64(table, "trials_with_tool_events").unwrap_or(0);
    let missing_results = first_cell_as_i64(table, "missing_results").unwrap_or(0);
    let invalid_results = first_cell_as_i64(table, "invalid_results").unwrap_or(0);
    let timeouts = first_cell_as_i64(table, "agent_timeouts").unwrap_or(0);
    let nonzero_exits = first_cell_as_i64(table, "nonzero_agent_exits").unwrap_or(0);
    let grader = first_cell_as_i64(table, "grader_or_mapping_errors").unwrap_or(0);
    let connector = first_cell_as_i64(table, "connector_errors").unwrap_or(0);
    format!(
        "{verdict}; trials_seen={seen}, completed={completed}, event_trials={with_events}, tool_trials={with_tools}, missing_results={missing_results}, invalid_results={invalid_results}, timeouts={timeouts}, nonzero_exits={nonzero_exits}, grader_or_mapping={grader}, connector={connector}"
    )
}

fn format_failure_signal(completed: i64, failed: i64) -> String {
    if completed <= 0 {
        return "no completed trials yet".to_string();
    }
    if failed <= 0 {
        return "no failed trial outcomes".to_string();
    }
    if failed == completed {
        return format!(
            "all {completed} completed trials failed; treat this as a systemic setup/runtime failure until proven otherwise"
        );
    }
    format!("{failed}/{completed} completed trials failed")
}

fn format_trust_summary(table: &analysis::QueryTable) -> String {
    let completed = first_cell_as_i64(table, "completed_trials").unwrap_or(0);
    let trusted = first_cell_as_i64(table, "trusted_scores").unwrap_or(0);
    let untrusted = first_cell_as_i64(table, "untrusted_scores").unwrap_or(0);
    let unknown = first_cell_as_i64(table, "unknown_score_trust").unwrap_or(0);
    let warnings = first_cell_as_i64(table, "warning_trials").unwrap_or(0);
    let errors = first_cell_as_i64(table, "error_trials").unwrap_or(0);
    let empty = first_cell_as_i64(table, "empty_predictions").unwrap_or(0);
    let grader = first_cell_as_i64(table, "grader_or_mapping_errors").unwrap_or(0);
    let connector = first_cell_as_i64(table, "connector_errors").unwrap_or(0);
    let issues = untrusted
        .saturating_add(unknown)
        .saturating_add(warnings)
        .saturating_add(errors)
        .saturating_add(empty)
        .saturating_add(grader)
        .saturating_add(connector);
    if completed == 0 {
        return "no completed trials to assess".to_string();
    }
    if issues == 0 && trusted == completed {
        format!("{trusted}/{completed} trusted scores; no contract issues observed")
    } else {
        format!(
            "{trusted}/{completed} trusted scores; issues: untrusted={untrusted}, unknown={unknown}, warnings={warnings}, errors={errors}, empty={empty}, grader_or_mapping={grader}, connector={connector}"
        )
    }
}

fn format_resource_summary(
    token_table: Option<&analysis::QueryTable>,
    tool_table: Option<&analysis::QueryTable>,
) -> String {
    let tokens = token_table
        .map(|table| sum_column_as_i64(table, "total_tokens"))
        .unwrap_or(0);
    let tool_calls = tool_table
        .map(|table| sum_column_as_i64(table, "calls"))
        .unwrap_or(0);
    match (tokens, tool_calls) {
        (0, 0) => "not observed".to_string(),
        (tokens, 0) => format!("{tokens} total tokens; no tool calls observed"),
        (0, tool_calls) => format!("{tool_calls} tool calls; tokens not observed"),
        (tokens, tool_calls) => format!("{tokens} total tokens; {tool_calls} tool calls"),
    }
}

fn post_run_section<'a>(report: &'a PostRunReport, name: &str) -> Option<&'a analysis::QueryTable> {
    report
        .sections
        .iter()
        .find(|section| section.name == name)
        .map(|section| &section.table)
}

fn first_cell(table: &analysis::QueryTable, column: &str) -> Option<String> {
    let idx = table.columns.iter().position(|name| name == column)?;
    table
        .rows
        .first()
        .and_then(|row| row.get(idx))
        .map(render_json_cell)
}

fn first_cell_as_i64(table: &analysis::QueryTable, column: &str) -> Option<i64> {
    let idx = table.columns.iter().position(|name| name == column)?;
    table
        .rows
        .first()
        .and_then(|row| row.get(idx))
        .and_then(value_as_i64)
}

fn sum_column_as_i64(table: &analysis::QueryTable, column: &str) -> i64 {
    let Some(idx) = table.columns.iter().position(|name| name == column) else {
        return 0;
    };
    table
        .rows
        .iter()
        .filter_map(|row| row.get(idx))
        .filter_map(value_as_i64)
        .sum()
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value.as_i64()
}

fn push_post_run_section(
    run_dir: &Path,
    sections: &mut Vec<PostRunSection>,
    name: &'static str,
    _purpose: &'static str,
    source_view: &str,
    limit: usize,
    allow_state_snapshot: bool,
) {
    let table = if allow_state_snapshot {
        query_source_view(run_dir, source_view, limit)
    } else {
        analysis::query_view(run_dir, source_view, limit)
    };
    if let Ok(table) = table {
        sections.push(PostRunSection { name, table });
    }
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

fn default_debug_bundle_path(run_dir: &Path) -> PathBuf {
    let stem = run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| sanitize_local_id(name).ok())
        .unwrap_or_else(|| "run".to_string());
    run_dir
        .join("debug_bundles")
        .join(format!("{stem}-debug-bundle-{}.zip", unix_now_seconds()))
}

fn empty_runs_hint(project_root: &Path) -> String {
    format!(
        "No runs found in {}.\n\nCreate a starter eval:\n  bucephalus init --client cli --command '<your-command>'\n\nRun it locally:\n  bucephalus dev experiment.yaml\n\nThen inspect results:\n  bucephalus runs\n  bucephalus views <run_id> observability",
        project_root.display()
    )
}

fn print_empty_runs_hint(project_root: &Path) {
    println!("{}", empty_runs_hint(project_root));
}

fn collect_run_inventory(project_root: &Path) -> Result<Vec<RunInventoryEntry>> {
    let mut entries = lab_runner::list_run_store_inventory(project_root)?
        .into_iter()
        .map(|entry| inspect_run_inventory_entry(&entry.run_dir, Some(&entry)))
        .collect::<Vec<_>>();

    sort_run_inventory_entries(&mut entries);
    Ok(entries)
}

fn collect_run_inventory_under_root(runs_dir: &Path) -> Result<Vec<RunInventoryEntry>> {
    if !runs_dir.exists() {
        return Ok(Vec::new());
    }
    if !runs_dir.is_dir() {
        return Err(anyhow!(
            "run root exists but is not a directory: {}",
            runs_dir.display()
        ));
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(runs_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            entries.push(inspect_run_inventory_entry(&path, None));
        }
    }
    sort_run_inventory_entries(&mut entries);
    Ok(entries)
}

fn sort_run_inventory_entries(entries: &mut [RunInventoryEntry]) {
    entries.sort_by(|a, b| {
        b.control
            .is_active
            .cmp(&a.control.is_active)
            .then_with(|| b.started_at.cmp(&a.started_at))
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
}

fn clean_runs_preflight(
    runs_dir: &Path,
    exists: bool,
    entries: &[RunInventoryEntry],
    force: bool,
    include_active: bool,
    dry_run: bool,
) -> Result<CleanRunsReport> {
    let active_runs = entries
        .iter()
        .filter(|entry| entry.control.is_active)
        .map(|entry| entry.run_id.clone())
        .collect::<Vec<_>>();

    if exists && !dry_run && !active_runs.is_empty() && !include_active {
        return Err(anyhow!(
            "clean --runs found active run(s): {}. Stop active runs first with `bucephalus kill <run_id>` or inspect them with `bucephalus recover --run-dir <run_dir>`; pass --include-active --force only if you intentionally want to delete active run evidence.",
            active_runs.join(", ")
        ));
    }
    if exists && !dry_run && !force {
        return Err(anyhow!(
            "clean --runs requires --force before deleting {}; use --dry-run to inspect what would be removed",
            runs_dir.display()
        ));
    }

    Ok(CleanRunsReport {
        runs_dir: runs_dir.to_path_buf(),
        exists,
        dry_run,
        force,
        include_active,
        run_count: entries.len(),
        active_runs,
        removed: false,
    })
}

fn clean_runs_report_to_json(report: &CleanRunsReport) -> Value {
    json!({
        "ok": true,
        "command": "clean",
        "target": "runs",
        "runs_dir": report.runs_dir.display().to_string(),
        "exists": report.exists,
        "dry_run": report.dry_run,
        "force": report.force,
        "include_active": report.include_active,
        "run_count": report.run_count,
        "active_runs": &report.active_runs,
        "removed": report.removed,
    })
}

fn print_clean_runs_report(report: &CleanRunsReport) {
    println!("runs_dir: {}", report.runs_dir.display());
    println!("exists: {}", report.exists);
    println!("run_count: {}", report.run_count);
    if report.active_runs.is_empty() {
        println!("active_runs: (none)");
    } else {
        println!("active_runs: {}", report.active_runs.join(", "));
    }
    println!("removed: {}", report.removed);
    if report.dry_run && report.exists {
        println!("next: bucephalus clean --runs --force");
    } else if !report.exists {
        println!("status: nothing to clean");
    }
}

fn inspect_run_inventory_entry(
    run_dir: &Path,
    store_entry: Option<&lab_runner::RunStoreInventoryEntry>,
) -> RunInventoryEntry {
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
        .or_else(|| store_entry.map(|entry| entry.run_id.as_str()))
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
        .or_else(|| store_entry.and_then(|entry| entry.experiment_id.as_deref()))
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
    lab_runner::run_store_metrics(run_dir)
        .map(|metrics| RunMetrics {
            variants: metrics.variants,
            pass_rate: metrics.pass_rate,
        })
        .unwrap_or(RunMetrics {
            variants: 0,
            pass_rate: None,
        })
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
        ACCOUNT_DB_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvVarGuard {
        saved: Vec<(String, Option<String>)>,
    }

    impl EnvVarGuard {
        fn clear(vars: &[&str]) -> Self {
            let saved = vars
                .iter()
                .map(|name| ((*name).to_string(), std::env::var(name).ok()))
                .collect::<Vec<_>>();
            for name in vars {
                std::env::remove_var(name);
            }
            Self { saved }
        }

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

    fn isolate_account_db_env() -> EnvVarGuard {
        EnvVarGuard::clear(&[
            "BUCEPHALUS_DB",
            "BUCEPHALUS_RUN_STORE",
            "BUCEPHALUS_RUN_STORE_URL",
            "BUCEPHALUS_CLOUD_API_URL",
            "DATABASE_URL",
        ])
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
    fn empty_runs_hint_points_from_authoring_to_observation() {
        let hint = empty_runs_hint(Path::new("/tmp/bucephalus-empty-runs"));
        assert!(hint.contains("No runs found in /tmp/bucephalus-empty-runs."));
        assert!(hint.contains("bucephalus init --client cli --command '<your-command>'"));
        assert!(hint.contains("bucephalus dev experiment.yaml"));
        assert!(hint.contains("bucephalus runs"));
        assert!(hint.contains("bucephalus views <run_id> observability"));
    }

    #[test]
    fn oauth_metadata_url_uses_rfc8414_authorization_server_metadata() {
        assert_eq!(
            oauth_metadata_url("https://auth.example/tenant/").unwrap(),
            "https://auth.example/tenant/.well-known/oauth-authorization-server"
        );
        assert_eq!(
            oauth_metadata_url("https://auth.example/.well-known/openid-configuration").unwrap(),
            "https://auth.example/.well-known/openid-configuration"
        );
        assert!(oauth_metadata_url("file:///tmp/issuer").is_err());
    }

    #[test]
    fn dynamic_client_registration_body_uses_requested_scope() {
        let body = dynamic_client_registration_body("openid profile cloud.write");
        assert_eq!(body["scope"], "openid profile cloud.write");
        assert_eq!(body["token_endpoint_auth_method"], "none");
    }

    #[test]
    fn update_dry_run_targets_current_install_contract() {
        assert_eq!(
            install_script_url("nishiokj/Bucephalus").unwrap(),
            "https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh"
        );
        assert_eq!(
            validate_github_repo_slug(" owner-name/repo.name ").unwrap(),
            "owner-name/repo.name"
        );
        assert!(install_script_url("../bad").is_err());
        assert!(install_script_url("owner").is_err());
        assert!(install_script_url("owner/repo/extra").is_err());
        assert!(install_script_url("owner/repo?ref=main").is_err());
        let root = temp_dir("update_install_dir");
        let result = run_update(UpdateOptions {
            version: Some("0.3.1".to_string()),
            install_dir: Some(root.clone()),
            repo: Some("nishiokj/Bucephalus".to_string()),
            base_url: Some("https://example.com/releases".to_string()),
            setup: true,
            no_modify_path: true,
            dry_run: true,
        })
        .unwrap();
        assert_eq!(result["schema_version"], "bucephalus_update_v1");
        assert_eq!(result["version"], "0.3.1");
        assert_eq!(result["install_dir"], root.display().to_string());
        assert_eq!(result["setup"], true);
        assert_eq!(result["no_modify_path"], true);
        assert_eq!(result["env"]["BUCEPHALUS_SETUP"], "1");
        assert_eq!(result["env"]["BUCEPHALUS_NO_MODIFY_PATH"], "1");
        assert_eq!(
            result["env"]["BUCEPHALUS_BASE_URL"],
            "https://example.com/releases"
        );
    }

    #[test]
    fn claude_mcp_existing_server_output_is_idempotent() {
        assert!(claude_mcp_server_already_exists(
            "MCP server bucephalus already exists in local config"
        ));
        assert!(claude_mcp_server_already_exists(
            "Error: MCP server Bucephalus already exists"
        ));
        assert!(!claude_mcp_server_already_exists(
            "Error: claude command failed for another reason"
        ));
    }

    #[test]
    fn cloud_user_auth_hint_points_to_login() {
        let message = cloud_user_auth_hint(
            "Cloud upload",
            false,
            Some("Cloud API requires OAuth bearer authentication"),
        );
        assert!(message.contains("Cloud upload requires Cloud authentication"));
        assert!(message.contains("bucephalus login"));
        assert!(message.contains("BUCEPHALUS_CLOUD_USER_TOKEN"));
        assert!(message.contains("bucephalus setup status"));
    }

    #[test]
    fn direct_mcp_invocation_message_explains_stdio_mode() {
        let message = direct_mcp_invocation_message();
        assert!(message.contains("stdio MCP server"));
        assert!(message.contains("bucephalus setup"));
        assert!(message.contains("bucephalus setup status --json"));
    }

    #[test]
    fn targeted_cursor_uninstall_removes_project_mcp_registration() {
        let root = temp_dir("cursor_mcp_uninstall");
        let config_path = root.join(".cursor").join("mcp.json");
        merge_mcp_server_config(
            &config_path,
            "other-server",
            &json!({
                "type": "stdio",
                "command": "other",
                "args": []
            }),
        )
        .unwrap();
        merge_mcp_server_config(
            &config_path,
            BUCEPHALUS_MCP_SERVER_NAME,
            &json!({
                "type": "stdio",
                "command": "bucephalus",
                "args": ["mcp"]
            }),
        )
        .unwrap();

        let result =
            unregister_mcp_clients(vec![SetupMcpClientArg::CursorProject], Some(&root), false)
                .unwrap();

        assert_eq!(result["status"], "removed");
        assert_eq!(result["clients"][0]["client"], "cursor-project");
        assert_eq!(result["clients"][0]["status"], "removed");
        assert!(!mcp_config_has_server(
            &config_path,
            BUCEPHALUS_MCP_SERVER_NAME
        ));
        assert!(mcp_config_has_server(&config_path, "other-server"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mcp_unregistration_summary_distinguishes_noop_states() {
        assert_eq!(
            summarize_mcp_unregistration_status(&[json!({"status": "missing"})], false),
            "missing"
        );
        assert_eq!(
            summarize_mcp_unregistration_status(&[json!({"status": "unsupported"})], false),
            "unsupported"
        );
        assert_eq!(
            summarize_mcp_unregistration_status(&[json!({"status": "skipped"})], false),
            "skipped"
        );
        assert_eq!(
            summarize_mcp_unregistration_status(&[json!({"status": "missing"})], true),
            "planned"
        );
    }

    #[test]
    fn cloud_submission_views_redact_local_paths() {
        let record = json!({
            "dispatch_id": "dispatch_1",
            "status": "local_completed",
            "paths": {
                "dispatch_dir": "/Users/alice/.local/share/bucephalus/dispatches/dispatch_1",
                "live_view": "/Users/alice/.local/share/bucephalus/dispatches/dispatch_1/live.html"
            },
            "summary": {
                "run_dir": "/Users/alice/.local/share/bucephalus/dispatches/dispatch_1/runs/run_1",
                "run_id": "run_1"
            },
            "internal": {
                "job_id": "job_1"
            }
        });
        let public = dispatch_record_for_cloud_submission(&record);
        assert!(public.get("paths").is_none());
        assert!(public.get("internal").is_none());
        assert!(public.pointer("/summary/run_dir").is_none());

        let daemon = json!({
            "job_id": "job_1",
            "manifest_path": "/Users/alice/work/manifest.json",
            "stdout_path": "/Users/alice/.local/share/bucephalus/daemon/jobs/job_1/stdout.json",
            "result": {
                "run_dir": "/Users/alice/.local/share/bucephalus/runs/run_1",
                "cases": [
                    {
                        "case_id": "case-1",
                        "workspace_dir": "/Users/alice/.local/share/bucephalus/runs/run_1/cases/case-1/workspace",
                        "stdout_path": "/Users/alice/.local/share/bucephalus/runs/run_1/cases/case-1/out/stdout.log",
                        "grade": {
                            "output_path": "/Users/alice/.local/share/bucephalus/runs/run_1/cases/case-1/out/grade.json"
                        }
                    }
                ]
            }
        });
        let redacted = daemon_summary_for_cloud_submission(&daemon);
        let rendered = serde_json::to_string(&redacted).unwrap();
        assert!(!rendered.contains("/Users/alice"));
        assert!(rendered.contains("<local-path-redacted>"));
    }

    #[test]
    fn cli_binding_errors_do_not_echo_secret_values() {
        let env_err = parse_runtime_env_bindings(&["BAD-KEY=supersecret".to_string()])
            .expect_err("invalid env key should fail")
            .to_string();
        assert!(env_err.contains("invalid --env key"));
        assert!(!env_err.contains("supersecret"));

        let set_err = parse_set_bindings(&["supersecret".to_string()])
            .expect_err("missing set delimiter should fail")
            .to_string();
        assert!(set_err.contains("invalid --set entry"));
        assert!(!set_err.contains("supersecret"));

        let secret_err = parse_secret_file_bindings(&["/tmp/secret-token".to_string()])
            .expect_err("missing secret-file delimiter should fail")
            .to_string();
        assert!(secret_err.contains("invalid --secret-file entry"));
        assert!(!secret_err.contains("/tmp/secret-token"));
    }

    #[test]
    fn cli_runtime_env_bindings_reject_duplicate_keys() {
        let err = parse_runtime_env_bindings(&[
            "OPENAI_API_KEY=first".to_string(),
            "OPENAI_API_KEY=second".to_string(),
        ])
        .expect_err("duplicate --env keys should fail")
        .to_string();

        assert!(err.contains("duplicate --env key 'OPENAI_API_KEY'"));
    }

    #[test]
    fn cli_set_bindings_reject_duplicate_keys() {
        let err = parse_set_bindings(&["model=temp".to_string(), "model=other".to_string()])
            .expect_err("duplicate --set keys should fail")
            .to_string();

        assert!(err.contains("duplicate --set key 'model'"));
    }

    #[test]
    fn cli_set_bindings_reject_empty_path_segments() {
        for raw in ["model.=temp", ".model=temp", "model..name=temp"] {
            let err = parse_set_bindings(&[raw.to_string()])
                .expect_err("malformed --set dotted paths should fail")
                .to_string();

            assert!(
                err.contains("invalid --set key")
                    && err.contains("dotted path segments cannot be empty"),
                "unexpected error for {raw}: {err}"
            );
        }
    }

    #[test]
    fn cli_set_bindings_reject_padded_path_segments() {
        for raw in [
            " model=temp",
            "model =temp",
            "model. name=temp",
            "model.name =temp",
        ] {
            let err = parse_set_bindings(&[raw.to_string()])
                .expect_err("padded --set dotted paths should fail")
                .to_string();

            assert!(
                err.contains("invalid --set key") && err.contains("leading or trailing whitespace"),
                "unexpected error for {raw}: {err}"
            );
        }
    }

    #[test]
    fn cli_secret_file_bindings_reject_duplicate_ids() {
        let err = parse_secret_file_bindings(&[
            "codex_oauth=/tmp/first".to_string(),
            "codex_oauth=/tmp/second".to_string(),
        ])
        .expect_err("duplicate --secret-file ids should fail")
        .to_string();

        assert!(err.contains("duplicate --secret-file id 'codex_oauth'"));
        assert!(!err.contains("/tmp/first"));
        assert!(!err.contains("/tmp/second"));
    }

    #[test]
    fn parse_run_validation_choice_maps_expected_actions() {
        assert_eq!(
            parse_run_validation_choice("1").unwrap(),
            RunValidationAction::SmokeTest
        );
        assert_eq!(
            parse_run_validation_choice("2").unwrap(),
            RunValidationAction::FullRun
        );
        assert_eq!(
            parse_run_validation_choice("").unwrap(),
            RunValidationAction::Cancel
        );
        assert!(parse_run_validation_choice("nope").is_err());
    }

    #[test]
    fn write_cloud_token_cache_writes_legacy_and_refresh_files() {
        let root = temp_dir("cloud_token_cache");
        let paths = cloud_token_paths(&root);
        write_cloud_token_cache(
            &paths,
            "https://issuer.example",
            "client-1",
            Some("audience-1"),
            Some("https://api.example"),
            "openid profile email",
            "https://issuer.example/oauth/token",
            &json!({
                "access_token": "access-123",
                "refresh_token": "refresh-456",
                "token_type": "Bearer",
                "expires_in": 3600
            }),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&paths.access).unwrap(), "access-123\n");
        assert_eq!(fs::read_to_string(&paths.refresh).unwrap(), "refresh-456\n");
        let cache = read_cloud_token_cache(&paths).unwrap();
        assert_eq!(cache["schema_version"], "bucephalus_cloud_oauth_token_v1");
        assert_eq!(cache["client_id"], "client-1");
        assert_eq!(
            cache["token_endpoint"],
            "https://issuer.example/oauth/token"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            OpenOptions::new()
                .write(true)
                .mode(0o644)
                .open(&paths.access)
                .unwrap()
                .set_permissions(std::fs::Permissions::from_mode(0o644))
                .unwrap();
            write_secret_file(&paths.access, b"access-456\n").unwrap();
            let mode = fs::metadata(&paths.access).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn logout_removes_cached_cloud_auth_files() {
        let _env_lock = lock_account_db_env();
        let root = temp_dir("cloud_logout_remove");
        let root_string = root.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(root_string.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, None),
        ]);
        let paths = cloud_token_paths(&root);
        write_secret_file(&paths.access, b"access-token\n").unwrap();
        write_secret_file(&paths.refresh, b"refresh-token\n").unwrap();
        write_secret_file(&paths.cache, br#"{"access_token":"access-token"}"#).unwrap();

        let result = run_logout(false).unwrap();

        assert_eq!(result["schema_version"], "bucephalus_logout_v1");
        assert_eq!(result["status"], "removed");
        assert_eq!(result["removed_count"], 3);
        assert_eq!(result["auth"]["status"], "missing");
        assert!(!paths.access.exists());
        assert!(!paths.refresh.exists());
        assert!(!paths.cache.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn logout_dry_run_preserves_files_and_reports_env_override() {
        let _env_lock = lock_account_db_env();
        let root = temp_dir("cloud_logout_dry_run_env");
        let root_string = root.display().to_string();
        let _env = EnvVarGuard::set(&[
            ("BUCEPHALUS_HOME", Some(root_string.as_str())),
            (BUCEPHALUS_CLOUD_USER_TOKEN_ENV, Some("env-token")),
        ]);
        let paths = cloud_token_paths(&root);
        write_secret_file(&paths.access, b"access-token\n").unwrap();
        write_secret_file(&paths.refresh, b"refresh-token\n").unwrap();
        write_secret_file(&paths.cache, br#"{"access_token":"access-token"}"#).unwrap();

        let result = run_logout(true).unwrap();

        assert_eq!(result["status"], "env_override_present");
        assert_eq!(result["planned_count"], 3);
        assert_eq!(result["env"]["present"], true);
        assert!(result["env"]["note"]
            .as_str()
            .unwrap()
            .contains(BUCEPHALUS_CLOUD_USER_TOKEN_ENV));
        assert_eq!(result["auth"]["source"], "env");
        assert!(paths.access.exists());
        assert!(paths.refresh.exists());
        assert!(paths.cache.exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn configure_test_account_db(run_dir: &Path) -> PathBuf {
        let db_path = run_dir.join(".bucephalus").join("bucephalus.sqlite");
        std::fs::create_dir_all(db_path.parent().expect("account db parent"))
            .expect("create account db parent");
        std::env::set_var("BUCEPHALUS_DB", &db_path);
        db_path
    }

    fn seed_sqlite_run_for_analysis_query(run_dir: &Path) {
        let sqlite_path = configure_test_account_db(run_dir);
        let account_id = lab_runner::active_account_id().expect("active account id");
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
             CREATE TABLE trial_attempts(
                 account_id TEXT NOT NULL,
                 run_id TEXT NOT NULL,
                 trial_id TEXT NOT NULL,
                 schedule_idx INTEGER NOT NULL,
                 attempt INTEGER NOT NULL,
                 phase TEXT NOT NULL,
                 paused_from_phase TEXT,
                 variant_id TEXT NOT NULL,
                 task_id TEXT NOT NULL,
                 repl_idx INTEGER NOT NULL,
                 state_json TEXT NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             );
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
            (&account_id, &run_id, format!(r#"{{"run_id":"{}","trial_id":"trial_1","variant_id":"base","task_id":"task_1","outcome":"success","success":true,"slot_commit_id":"slot_1","schedule_idx":0}}"#, run_id)),
        )
        .expect("insert trial row");
        conn.execute(
            "INSERT INTO trial_attempts(
                account_id, run_id, trial_id, schedule_idx, attempt, phase, paused_from_phase,
                variant_id, task_id, repl_idx, state_json, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            (
                &account_id,
                &run_id,
                "trial_1",
                0_i64,
                0_i64,
                "committed",
                Option::<&str>::None,
                "base",
                "task_1",
                0_i64,
                r#"{"key":{"schedule_idx":0,"attempt":0},"slot":{"schedule_idx":0,"variant_id":"base","task_id":"task_1","repl_idx":0},"phase":"committed","fs":{"attempt_dir":"/tmp/trial_1","in_dir":"/tmp/trial_1/in","out_dir":"/tmp/trial_1/out","telemetry_mounts":[],"logs_dir":"/tmp/trial_1/logs"},"task_sandbox":{"container_id":"container_1","image":"python:3.11-slim","workdir":"/workspace","materialization":{"kind":"copy"}},"grading_sandbox":{"container_id":"container_1","strategy":"in_task_runtime","workdir":"/workspace"},"agent_phase":{"started_at":"2026-01-01T00:00:00Z","ended_at":"2026-01-01T00:00:01Z","exit_code":0,"timed_out":false,"result_state":"valid","stdout_path":"/tmp/trial_1/logs/agent.stdout","stderr_path":"/tmp/trial_1/logs/agent.stderr"},"grading_phase":{"started_at":"2026-01-01T00:00:01Z","ended_at":"2026-01-01T00:00:02Z","exit_code":0,"timed_out":false,"output_state":"valid","stdout_path":"/tmp/trial_1/logs/grader.stdout","stderr_path":"/tmp/trial_1/logs/grader.stderr"},"candidate_artifact":{"state":"valid","artifact_type":"answer","source":"result.inline","payload":{"summary":"ok"}},"cleanup":{"containers":[]}}"#,
                1_i64,
            ),
        )
        .expect("insert trial attempt");
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
                r#"{"event_type":"model_call_end","provider":{"request_id":"req_123","server_ms":42},"usage":{"tokens_in":100,"tokens_out":25}}"#,
                format!(r#"{{"run_id":"{}","trial_id":"trial_1","variant_id":"base","task_id":"task_1","event_type":"model_call_end","slot_commit_id":"slot_1","schedule_idx":0,"usage":{{"tokens_in":100,"tokens_out":25}},"payload":{{"event_type":"model_call_end","provider":{{"request_id":"req_123","server_ms":42}},"usage":{{"tokens_in":100,"tokens_out":25}}}}}}"#, run_id)
            ),
        )
        .expect("insert event row");
        conn.execute(
            "INSERT INTO event_rows(account_id, run_id, payload_json, row_json) VALUES (?1, ?2, ?3, ?4)",
            (
                &account_id,
                &run_id,
                r#"{"event_type":"tool_call_end","tool":{"name":"bash"},"outcome":{"status":"ok"}}"#,
                format!(r#"{{"run_id":"{}","trial_id":"trial_1","variant_id":"base","task_id":"task_1","event_type":"tool_call_end","slot_commit_id":"slot_1","schedule_idx":0,"tool":{{"name":"bash"}},"outcome":{{"status":"ok"}},"payload":{{"tool":{{"name":"bash"}},"outcome":{{"status":"ok"}}}}}}"#, run_id)
            ),
        )
        .expect("insert tool event row");
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
        let account_id = lab_runner::active_account_id().expect("active account id");
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

    #[test]
    fn query_run_uses_configured_run_store_and_keeps_real_run_id_in_metadata() {
        let _env_guard = lock_account_db_env();
        let _account_env = isolate_account_db_env();
        let run_dir = temp_dir("run_store_query_cleanup");
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
        assert_eq!(health.rows[0], vec![json!(1), json!(0), json!(0)]);
        let progress = analysis::query_run(
            &run_dir,
            "SELECT completed_trials, successful_trials, failed_trials FROM run_progress",
        )
        .expect("query run progress");
        assert_eq!(progress.rows[0], vec![json!(1), json!(1), json!(0)]);
        let events = analysis::query_run(
            &run_dir,
            "SELECT json_extract(payload_json, '$.provider.request_id') AS request_id FROM events",
        )
        .expect("query events payload");
        assert_eq!(events.rows[0][0], Value::String("req_123".to_string()));
        let raw_events = analysis::query_run(
            &run_dir,
            "SELECT trial_id, row_seq, event_type, event_json FROM raw_events ORDER BY row_seq",
        )
        .expect("query raw events");
        assert_eq!(raw_events.rows.len(), 2);
        assert_eq!(
            raw_events.rows[0],
            vec![
                json!("trial_1"),
                Value::Null,
                json!("model_call_end"),
                json!(
                    r#"{"event_type":"model_call_end","provider":{"request_id":"req_123","server_ms":42},"usage":{"tokens_in":100,"tokens_out":25}}"#
                )
            ]
        );
        let rendered_events = format_raw_events_stdout(&raw_events);
        assert!(rendered_events
            .iter()
            .any(|line| line == r#"  "event_type": "model_call_end","#));
        assert!(rendered_events
            .iter()
            .any(|line| line == r#"    "request_id": "req_123","#));
        let tokens = analysis::query_run(
            &run_dir,
            "SELECT variant_id, tokens_in, tokens_out, total_tokens FROM token_usage_by_variant",
        )
        .expect("query token usage");
        assert_eq!(
            tokens.rows[0],
            vec![json!("base"), json!(100.0), json!(25.0), json!(125.0)]
        );
        let tools = analysis::query_run(
            &run_dir,
            "SELECT variant_id, tool_name, calls FROM tool_usage_by_variant",
        )
        .expect("query tool usage");
        assert_eq!(tools.rows[0], vec![json!("base"), json!("bash"), json!(1)]);
        let observability = analysis::query_run(
            &run_dir,
            "SELECT diagnostic_verdict, trials_seen, completed_trials, trials_with_events, trials_with_tool_events FROM observability_summary",
        )
        .expect("query observability summary");
        assert_eq!(
            observability.rows[0],
            vec![
                json!("no_observed_runtime_gaps"),
                json!(1),
                json!(1),
                json!(1),
                json!(1)
            ]
        );
        let diagnostics = analysis::query_run(
            &run_dir,
            "SELECT phase, trial_outcome, trial_success, agent_exit_code, agent_timed_out, agent_result_state, candidate_artifact_state, candidate_artifact_source, task_workdir FROM trial_diagnostics",
        )
        .expect("query trial diagnostics");
        assert_eq!(
            diagnostics.rows[0],
            vec![
                json!("committed"),
                json!("success"),
                json!(1),
                json!(0),
                json!(0),
                json!("valid"),
                json!("valid"),
                json!("result.inline"),
                json!("/workspace")
            ]
        );
        let post_run = try_post_run_stats_json(&run_dir);
        assert_eq!(post_run.pointer("/view_set"), Some(&json!("ab_test")));
        assert!(post_run.pointer("/sections/observability").is_some());
        assert!(post_run.pointer("/sections/events").is_some());
        assert!(post_run.pointer("/sections/trial_diagnostics").is_some());
        assert!(post_run.pointer("/sections/health").is_some());
        assert!(post_run.pointer("/sections/token_usage").is_some());
        assert!(post_run.pointer("/sections/tool_usage").is_some());
        assert!(post_run.pointer("/sections/run_errors").is_some());
        assert!(
            !run_dir.join("run.sqlite").exists(),
            "run-scoped sqlite database should not be created"
        );

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn resolve_run_dir_arg_rejects_stale_store_path() {
        let _env_guard = lock_account_db_env();
        let _account_env = isolate_account_db_env();
        let anchor = temp_dir("stale_run_store_anchor");
        std::fs::create_dir_all(&anchor).expect("anchor dir");
        let db_path = configure_test_account_db(&anchor);
        let account_id = lab_runner::active_account_id().expect("active account id");
        let stale_run_dir = anchor.join("missing_run_1");
        let conn = SqliteConnection::open(&db_path).expect("open sqlite");
        conn.execute_batch(
            "CREATE TABLE runs(
                account_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                experiment_id TEXT,
                run_dir TEXT NOT NULL
            );",
        )
        .expect("create runs");
        conn.execute(
            "INSERT INTO runs(account_id, run_id, experiment_id, run_dir) VALUES (?1, ?2, ?3, ?4)",
            (
                account_id,
                "run_1",
                Option::<String>::None,
                stale_run_dir.display().to_string(),
            ),
        )
        .expect("insert stale run");

        let original_cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&anchor).expect("set cwd");
        let result = resolve_run_dir_arg("run_1");
        std::env::set_current_dir(original_cwd).expect("restore cwd");
        let err = result.expect_err("stale run path should fail");
        assert!(
            err.to_string()
                .contains("stored run path for 'run_1' is not accessible"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn latest_agent_output_view_reads_agent_result_file() {
        let run_dir = temp_dir("latest_agent_output");
        let trial_dir = run_dir.join("trials").join("trial_1");
        let runner_dir = trial_dir.join("runner");
        let agent_dir = trial_dir.join("agent");
        let out_dir = trial_dir.join("out");
        std::fs::create_dir_all(&runner_dir).expect("runner dir");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        std::fs::create_dir_all(&out_dir).expect("out dir");
        std::fs::write(
            runner_dir.join("trial_runtime_state.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": "trial_runtime_state_v1",
                "updated_at": "2026-06-02T00:00:00Z",
                "state": {
                    "key": {"schedule_idx": 0, "attempt": 1},
                    "slot": {"schedule_idx": 0, "variant_id": "base", "task_id": "task_1", "repl_idx": 0},
                    "phase": "committed",
                    "fs": {
                        "attempt_dir": trial_dir.display().to_string(),
                        "in_dir": trial_dir.join("in").display().to_string(),
                        "out_dir": out_dir.display().to_string(),
                        "telemetry_mounts": [],
                        "logs_dir": trial_dir.join("logs").display().to_string()
                    },
                    "agent_phase": {
                        "started_at": "2026-06-02T00:00:00Z",
                        "ended_at": "2026-06-02T00:00:01Z",
                        "exit_code": 0,
                        "timed_out": false,
                        "result_state": "valid",
                        "stdout_path": trial_dir.join("agent/stdout.log").display().to_string(),
                        "stderr_path": trial_dir.join("agent/stderr.log").display().to_string()
                    },
                    "cleanup": {"containers": []}
                }
            }))
            .expect("state json"),
        )
        .expect("write state");
        std::fs::write(
            agent_dir.join("result.json"),
            serde_json::to_vec_pretty(&json!({
                "answer": "raw agent result",
                "metrics": {"resolved": 1.0}
            }))
            .expect("agent result json"),
        )
        .expect("write agent result");

        let table = build_latest_agent_output_table(&run_dir);
        assert_eq!(table.rows.len(), 1);
        let col = |name: &str| table.columns.iter().position(|c| c == name).unwrap();
        assert_eq!(table.rows[0][col("state")], json!("agent_result_file"));
        assert_eq!(table.rows[0][col("agent_result_state")], json!("valid"));
        assert_eq!(
            table.rows[0][col("candidate_artifact_state")],
            json!("none")
        );
        assert_eq!(
            table.rows[0][col("output_id")],
            json!("BUCEPHALUS_RESULT_PATH")
        );
        assert_eq!(table.rows[0][col("format")], json!("json"));
        assert!(table.rows[0][col("preview")]
            .as_str()
            .unwrap()
            .contains("raw agent result"));
        assert_eq!(
            table.rows[0][col("agent_result_json")].pointer("/answer"),
            Some(&json!("raw agent result"))
        );

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn latest_agent_output_view_reports_missing_agent_result_file() {
        let run_dir = temp_dir("missing_agent_output");
        let trial_dir = run_dir.join("trials").join("trial_1");
        let runner_dir = trial_dir.join("runner");
        let out_dir = trial_dir.join("out");
        std::fs::create_dir_all(&runner_dir).expect("runner dir");
        std::fs::write(trial_dir.join("result.json"), "{}").expect("stale trial result");
        std::fs::write(
            runner_dir.join("trial_runtime_state.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": "trial_runtime_state_v1",
                "updated_at": "2026-06-02T00:00:00Z",
                "state": {
                    "key": {"schedule_idx": 0, "attempt": 0},
                    "slot": {"schedule_idx": 0, "variant_id": "base", "task_id": "task_1", "repl_idx": 0},
                    "phase": "agent_finished",
                    "fs": {
                        "attempt_dir": trial_dir.display().to_string(),
                        "in_dir": trial_dir.join("in").display().to_string(),
                        "out_dir": out_dir.display().to_string(),
                        "telemetry_mounts": [],
                        "logs_dir": trial_dir.join("logs").display().to_string()
                    },
                    "agent_phase": {
                        "started_at": "2026-06-02T00:00:00Z",
                        "ended_at": "2026-06-02T00:00:01Z",
                        "exit_code": 0,
                        "timed_out": false,
                        "result_state": "missing",
                        "stdout_path": trial_dir.join("agent/stdout.log").display().to_string(),
                        "stderr_path": trial_dir.join("agent/stderr.log").display().to_string()
                    },
                    "cleanup": {"containers": []}
                }
            }))
            .expect("state json"),
        )
        .expect("write state");

        let table = build_latest_agent_output_table(&run_dir);
        assert_eq!(table.rows.len(), 1);
        let col = |name: &str| table.columns.iter().position(|c| c == name).unwrap();
        assert_eq!(
            table.rows[0][col("state")],
            json!("missing_agent_result_file")
        );
        assert_eq!(table.rows[0][col("agent_result_state")], json!("missing"));
        assert_eq!(
            table.rows[0][col("candidate_artifact_state")],
            json!("none")
        );
        assert!(table.rows[0][col("preview")]
            .as_str()
            .unwrap()
            .contains("no agent result file"));

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn scores_table_pivots_declared_metrics_and_adds_mean_row() {
        let _env_guard = lock_account_db_env();
        let _account_env = isolate_account_db_env();
        let run_dir = temp_dir("scores_table");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        seed_sqlite_run_for_analysis_query(&run_dir);

        let table = build_scores_table(&run_dir).expect("scores table");
        let col = |name: &str| table.columns.iter().position(|c| c == name).unwrap();
        assert!(table.columns.contains(&"latency_ms".to_string()));
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0][col("trial_id")], json!("trial_1"));
        assert_eq!(
            json_number_value(&table.rows[0][col("latency_ms")]),
            Some(12.3)
        );
        assert_eq!(table.rows[1][col("trial_id")], json!("mean"));
        assert_eq!(
            json_number_value(&table.rows[1][col("latency_ms")]),
            Some(12.3)
        );

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn metric_explanation_table_shows_declared_observed_metric_dataflow() {
        let _env_guard = lock_account_db_env();
        let _account_env = isolate_account_db_env();
        let run_dir = temp_dir("metric_explanation");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        seed_sqlite_run_for_analysis_query(&run_dir);

        let table = build_metric_explanation_table(&run_dir).expect("metric explanation table");
        let col = |name: &str| table.columns.iter().position(|c| c == name).unwrap();
        let row = table
            .rows
            .iter()
            .find(|row| row[col("metric_id")] == json!("latency_ms"))
            .expect("latency metric row");
        assert_eq!(row[col("status")], json!("declared_observed"));
        assert_eq!(
            row[col("source")],
            json!("agent_response/metrics/latency_ms")
        );
        assert_eq!(row[col("metric_rows")], json!(1));
        assert_eq!(row[col("numeric_rows")], json!(1));
        assert_eq!(row[col("scoreboard_column")], json!("latency_ms_mean"));

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn post_run_summary_flags_systemic_trial_failures() {
        let report = PostRunReport {
            view_set: analysis::ViewSet::AbTest,
            sections: vec![
                PostRunSection {
                    name: "run_progress",
                    table: analysis::QueryTable {
                        columns: vec![
                            "completed_trials".to_string(),
                            "successful_trials".to_string(),
                            "failed_trials".to_string(),
                            "pass_rate".to_string(),
                        ],
                        rows: vec![vec![json!(16), json!(0), json!(16), json!(0.0)]],
                    },
                },
                PostRunSection {
                    name: "health",
                    table: analysis::QueryTable {
                        columns: vec![
                            "completed_trials".to_string(),
                            "trusted_scores".to_string(),
                            "untrusted_scores".to_string(),
                            "unknown_score_trust".to_string(),
                            "warning_trials".to_string(),
                            "error_trials".to_string(),
                            "empty_predictions".to_string(),
                            "grader_or_mapping_errors".to_string(),
                            "connector_errors".to_string(),
                        ],
                        rows: vec![vec![
                            json!(16),
                            json!(16),
                            json!(0),
                            json!(0),
                            json!(0),
                            json!(0),
                            json!(0),
                            json!(0),
                            json!(0),
                        ]],
                    },
                },
            ],
            evaluation_summary_path: None,
        };

        let summary = summarize_post_run_report(&report)
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        assert_eq!(
            summary.get("what happened").map(String::as_str),
            Some("16 trials completed; 0 succeeded; 16 failed; 0.0 success rate")
        );
        assert_eq!(
            summary.get("failure signal").map(String::as_str),
            Some(
                "all 16 completed trials failed; treat this as a systemic setup/runtime failure until proven otherwise"
            )
        );
        assert_eq!(
            summary.get("can I trust it").map(String::as_str),
            Some("16/16 trusted scores; no contract issues observed")
        );
    }

    #[test]
    fn read_run_status_renders_multiflight_active_trials() {
        let _env_guard = lock_account_db_env();
        let _account_env = isolate_account_db_env();
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
        let _account_env = isolate_account_db_env();
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

        let entry = inspect_run_inventory_entry(&run_dir, None);
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
        let _account_env = isolate_account_db_env();
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
    fn init_cli_generates_buildable_starter() {
        let root = unique_test_dir("init_cli_starter");
        let options = resolve_init_options(InitOptionArgs {
            dir: Some(root.clone()),
            client: Some(InitClientArg::Cli),
            command: Some("python3 agent.py --input {{input}} --output {{output}}".to_string()),
            mode: "answer".to_string(),
            name: Some("CLI Smoke".to_string()),
            ..Default::default()
        })
        .expect("init options");
        let result = run_init(options).expect("run init");

        assert_eq!(
            result.pointer("/client").and_then(Value::as_str),
            Some("cli")
        );
        assert!(root.join("experiment.yaml").is_file());
        assert!(root.join("cases.jsonl").is_file());
        assert!(root.join("agent").join("buc_agent.py").is_file());
        let experiment = fs::read_to_string(root.join("experiment.yaml")).expect("experiment yaml");
        assert!(experiment.contains("mode: answer"));
        assert!(!experiment.contains("protocol: command"));
        assert!(experiment.contains("command: [\"python3\", \"/opt/agent/buc_agent.py\"]"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn init_api_requires_url() {
        let root = unique_test_dir("init_api_requires_url");
        let err = resolve_init_options(InitOptionArgs {
            dir: Some(root),
            client: Some(InitClientArg::Api),
            mode: "answer".to_string(),
            name: Some("API Smoke".to_string()),
            ..Default::default()
        })
        .unwrap_err();

        assert!(
            err.to_string().contains("init --client api requires --url"),
            "unexpected error: {err}"
        );
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
    fn resolve_experiment_target_accepts_directory_default() {
        let root = unique_test_dir("resolve_experiment_directory");
        fs::create_dir_all(&root).expect("test dir");
        let experiment = root.join("experiment.yaml");
        fs::write(&experiment, "experiment:\n  id: smoke\n").expect("experiment yaml");

        let resolved = resolve_experiment_target(Some(&root)).expect("resolved experiment");

        assert_eq!(resolved, experiment);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn resolve_experiment_target_error_points_to_authoring_next_steps() {
        let root = unique_test_dir("resolve_experiment_missing");
        fs::create_dir_all(&root).expect("test dir");

        let err = resolve_experiment_target(Some(&root))
            .expect_err("missing experiment should fail")
            .to_string();

        assert!(err.contains("dev expected an experiment YAML"));
        assert!(err.contains("bucephalus init <new-eval-dir>"));
        assert!(err.contains("bucephalus dev <new-eval-dir>"));
        assert!(err.contains("bucephalus doctor"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn resolve_lint_target_accepts_directory_and_names_lint_in_errors() {
        let root = unique_test_dir("resolve_lint_directory");
        fs::create_dir_all(&root).expect("test dir");
        let experiment = root.join("experiment.yaml");
        fs::write(&experiment, "experiment:\n  id: smoke\n").expect("experiment yaml");

        let resolved =
            resolve_experiment_target_for_command("lint", Some(&root)).expect("resolved lint");

        assert_eq!(resolved, experiment);
        fs::remove_file(&resolved).expect("remove experiment");

        let err = resolve_experiment_target_for_command("lint", Some(&root))
            .expect_err("missing lint target should fail")
            .to_string();

        assert!(err.contains("lint expected an experiment YAML"));
        assert!(err.contains("bucephalus lint <new-eval-dir>"));
        assert!(err.contains("bucephalus check-package"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn init_experiment_yaml_uses_public_metric_refs() {
        let options = InitOptions {
            dir: PathBuf::from("."),
            client: InitClientArg::Cli,
            command: Some("agent --input {input} --output {output}".to_string()),
            url: None,
            stream: InitStreamArg::None,
            language: InitLanguageArg::Python,
            mcp_role: None,
            mcp_tool: None,
            mode: "answer".to_string(),
            name: "Smoke Eval".to_string(),
            force: false,
        };

        let yaml = generate_init_experiment_yaml(&options);

        assert!(yaml.contains("from: result.metrics.resolved"));
        assert!(!yaml.contains("type: agent_response"));
        assert!(!yaml.contains("pointer: /metrics/resolved"));
        assert!(!yaml.contains("compute: { backend: local-docker }"));
        assert!(!yaml.contains("storage: { backend: local-fs }"));
        assert!(!yaml.contains("traces: { backend: local-stdout }"));
        assert!(!yaml.contains("task_sandbox: none"));
        assert!(!yaml.contains("task_sandbox: {}"));
        assert!(!yaml.contains("repeats: 1"));
        assert!(!yaml.contains("seeds:"));
        assert!(!yaml.contains("scheduling:"));
        assert!(!yaml.contains("max_concurrency: 1"));
        assert!(!yaml.contains("random_seed: 1"));
        assert!(!yaml.contains("comparison: none"));
        assert!(!yaml.contains("agent_site: agent_container"));
        assert!(!yaml.contains("/bucephalus/out/result.json"));
    }

    #[test]
    fn schema_validate_accepts_generated_experiment_yaml() {
        let root = unique_test_dir("schema_validate_generated_yaml");
        let options = InitOptions {
            dir: root.clone(),
            client: InitClientArg::Cli,
            command: Some("agent --input {input} --output {output}".to_string()),
            url: None,
            stream: InitStreamArg::None,
            language: InitLanguageArg::Python,
            mcp_role: None,
            mcp_tool: None,
            mode: "answer".to_string(),
            name: "Smoke Eval".to_string(),
            force: true,
        };
        run_init(options).expect("init");
        let value = read_json_or_yaml_value(&root.join("experiment.yaml")).expect("read yaml");
        let schema = schemas::compile_schema("experiment_authoring_v1.jsonschema").expect("schema");
        let errors = schema
            .validate(&value)
            .err()
            .map(|errors| errors.map(|err| err.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();

        assert!(errors.is_empty(), "unexpected schema errors: {errors:?}");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn schema_validate_reader_rejects_duplicate_yaml_keys() {
        let root = unique_test_dir("schema_validate_duplicate_yaml_keys");
        fs::create_dir_all(&root).expect("root dir");
        let experiment = root.join("experiment.yaml");
        fs::write(
            &experiment,
            r#"
experiment:
  id: e
runtime:
  network:
    default: none
runtime:
  compute:
    backend: local-docker
"#,
        )
        .expect("experiment yaml");

        let err = read_json_or_yaml_value(&experiment)
            .expect_err("schema reader should reject duplicate YAML keys");
        let msg = err.to_string();

        assert!(
            msg.contains("duplicate mapping key") && msg.contains("duplicate key 'runtime' at /"),
            "unexpected error: {msg}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn schema_validate_defaults_to_authoring_schema() {
        let cli =
            Cli::try_parse_from(["bucephalus", "schema-validate"]).expect("parse schema validate");

        let Commands::SchemaValidate { schema, file, json } = cli.command else {
            panic!("expected schema-validate command");
        };

        assert_eq!(schema, "experiment_authoring_v1.jsonschema");
        assert_eq!(file, PathBuf::from("experiment.yaml"));
        assert!(!json);
    }

    #[test]
    fn lint_command_parses_experiment_and_json_mode() {
        let cli = Cli::try_parse_from([
            "bucephalus",
            "lint",
            "experiment.yaml",
            "--out",
            "package",
            "--json",
        ])
        .expect("parse lint");

        let Commands::Lint {
            target,
            out,
            overrides,
            json,
        } = cli.command
        else {
            panic!("expected lint command");
        };

        assert_eq!(target, Some(PathBuf::from("experiment.yaml")));
        assert_eq!(out, Some(PathBuf::from("package")));
        assert!(overrides.is_none());
        assert!(json);
    }

    #[test]
    fn lint_command_allows_default_target() {
        let cli = Cli::try_parse_from(["bucephalus", "lint"]).expect("parse lint");

        let Commands::Lint {
            target,
            out,
            overrides,
            json,
        } = cli.command
        else {
            panic!("expected lint command");
        };

        assert!(target.is_none());
        assert!(out.is_none());
        assert!(overrides.is_none());
        assert!(!json);
    }

    #[test]
    fn experiment_input_path_distinguishes_package_directory() {
        let root = unique_test_dir("experiment_input_package");
        let package = root.join("package");
        fs::create_dir_all(&package).expect("package dir");
        fs::write(package.join("manifest.json"), "{}\n").expect("manifest");
        fs::write(
            package.join("experiment.yaml"),
            "experiment:\n  id: ignored\n",
        )
        .expect("co-located yaml");

        let resolved = experiment_input_path(&package).expect("input classification");

        assert_eq!(resolved, None);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn experiment_input_path_accepts_yaml_file() {
        let root = unique_test_dir("experiment_input_yaml");
        fs::create_dir_all(&root).expect("test dir");
        let experiment = root.join("experiment.yaml");
        fs::write(&experiment, "experiment:\n  id: smoke\n").expect("experiment yaml");

        let resolved = experiment_input_path(&experiment).expect("input classification");

        assert_eq!(resolved, Some(experiment));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn experiment_input_path_error_explains_run_input_modes() {
        let root = unique_test_dir("experiment_input_missing");
        let missing = root.join("missing-target");

        let err = experiment_input_path(&missing)
            .expect_err("missing run input should fail early")
            .to_string();

        assert!(err.contains("run expected an experiment YAML"));
        assert!(err.contains("bucephalus run <package-dir> --smoke-test"));
        assert!(err.contains("bucephalus doctor <same-target>"));
    }

    #[test]
    fn package_command_target_accepts_package_dir_and_manifest_file() {
        let root = unique_test_dir("package_command_target_package");
        let package = root.join("package");
        fs::create_dir_all(&package).expect("package dir");
        fs::write(package.join("manifest.json"), "{}\n").expect("manifest");

        let from_dir =
            resolve_package_command_target("preflight", &package).expect("package dir target");
        let from_manifest =
            resolve_package_command_target("preflight", &package.join("manifest.json"))
                .expect("manifest target");

        assert_eq!(from_dir, package);
        assert_eq!(from_manifest, package);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn package_command_target_rejects_yaml_with_guided_next_steps() {
        let root = unique_test_dir("package_command_target_yaml");
        fs::create_dir_all(&root).expect("test dir");
        let experiment = root.join("experiment.yaml");
        fs::write(&experiment, "experiment:\n  id: smoke\n").expect("experiment yaml");

        let err = resolve_package_command_target("preflight", &experiment)
            .expect_err("yaml should not be accepted as sealed package")
            .to_string();

        assert!(err.contains("preflight expected a sealed package"));
        assert!(err.contains("target is an experiment YAML"));
        assert!(err.contains("bucephalus build experiment.yaml --out <package-dir>"));
        assert!(err.contains("bucephalus preflight <package-dir>"));
        assert!(err.contains("bucephalus doctor experiment.yaml"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn package_command_target_rejects_experiment_directory_with_guided_next_steps() {
        let root = unique_test_dir("package_command_target_experiment_dir");
        fs::create_dir_all(&root).expect("test dir");
        fs::write(root.join("experiment.yaml"), "experiment:\n  id: smoke\n")
            .expect("experiment yaml");

        let err = resolve_package_command_target("check-package", &root)
            .expect_err("experiment dir should not be accepted as sealed package")
            .to_string();

        assert!(err.contains("check-package expected a sealed package"));
        assert!(err.contains("directory contains experiment.yaml"));
        assert!(err.contains("bucephalus check-package <package-dir>"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn resolve_doctor_target_prefers_package_directory_over_colocated_yaml() {
        let root = unique_test_dir("doctor_target_package");
        let package = root.join("package");
        fs::create_dir_all(&package).expect("package dir");
        fs::write(package.join("manifest.json"), "{}\n").expect("manifest");
        fs::write(
            package.join("experiment.yaml"),
            "experiment:\n  id: ignored\n",
        )
        .expect("co-located yaml");

        let resolved = resolve_doctor_target(Some(&package)).expect("doctor target");

        match resolved {
            DoctorTarget::Package(path) => assert_eq!(path, package),
            DoctorTarget::Experiment(path) => panic!("expected package, got {}", path.display()),
        }
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn resolve_doctor_target_accepts_experiment_directory() {
        let root = unique_test_dir("doctor_target_experiment");
        fs::create_dir_all(&root).expect("test dir");
        let experiment = root.join("experiment.yaml");
        fs::write(&experiment, "experiment:\n  id: smoke\n").expect("experiment yaml");

        let resolved = resolve_doctor_target(Some(&root)).expect("doctor target");

        match resolved {
            DoctorTarget::Experiment(path) => assert_eq!(path, experiment),
            DoctorTarget::Package(path) => panic!("expected experiment, got {}", path.display()),
        }
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn resolve_doctor_target_error_explains_experiment_or_package_modes() {
        let root = unique_test_dir("doctor_target_missing");

        let err = resolve_doctor_target(Some(&root))
            .expect_err("missing doctor target should fail")
            .to_string();

        assert!(err.contains("doctor expected an experiment YAML or sealed package"));
        assert!(err.contains("bucephalus doctor experiment.yaml"));
        assert!(err.contains("bucephalus doctor <package-dir>"));
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
        dirs.iter().map(|d| fake_run_entry(d, false)).collect()
    }

    fn fake_run_entry(dir: &str, active: bool) -> RunInventoryEntry {
        RunInventoryEntry {
            run_dir: PathBuf::from(dir),
            run_id: d_to_run_id(dir),
            experiment: "test".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            started_at_display: "now".to_string(),
            control: RunControlSummary {
                status: if active { "running" } else { "completed" }.to_string(),
                status_display: if active { "running" } else { "completed" }.to_string(),
                live_summary: String::new(),
                active_trials: if active { 1 } else { 0 },
                is_active: active,
            },
        }
    }

    fn d_to_run_id(dir: &str) -> String {
        Path::new(dir)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(dir)
            .to_string()
    }

    #[test]
    fn clean_runs_preflight_requires_force_before_delete() {
        let entries = vec![fake_run_entry("/tmp/bucephalus-runs/run_done", false)];
        let err = clean_runs_preflight(
            Path::new("/tmp/bucephalus-runs"),
            true,
            &entries,
            false,
            false,
            false,
        )
        .expect_err("clean should require force")
        .to_string();
        assert!(err.contains("clean --runs requires --force"));
    }

    #[test]
    fn clean_runs_preflight_blocks_active_runs_without_override() {
        let entries = vec![fake_run_entry("/tmp/bucephalus-runs/run_active", true)];
        let err = clean_runs_preflight(
            Path::new("/tmp/bucephalus-runs"),
            true,
            &entries,
            true,
            false,
            false,
        )
        .expect_err("active run should block clean")
        .to_string();
        assert!(err.contains("clean --runs found active run(s): run_active"));
        assert!(err.contains("bucephalus kill <run_id>"));
    }

    #[test]
    fn clean_runs_preflight_allows_dry_run_and_explicit_active_override() {
        let entries = vec![fake_run_entry("/tmp/bucephalus-runs/run_active", true)];
        let dry = clean_runs_preflight(
            Path::new("/tmp/bucephalus-runs"),
            true,
            &entries,
            false,
            false,
            true,
        )
        .expect("dry-run should not require force");
        assert_eq!(dry.run_count, 1);
        assert_eq!(dry.active_runs, vec!["run_active".to_string()]);
        assert!(!dry.removed);

        let forced = clean_runs_preflight(
            Path::new("/tmp/bucephalus-runs"),
            true,
            &entries,
            true,
            true,
            false,
        )
        .expect("explicit override should allow active clean preflight");
        assert!(forced.force);
        assert!(forced.include_active);
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
