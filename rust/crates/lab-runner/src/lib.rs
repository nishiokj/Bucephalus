mod backend;
mod config;
mod experiment;
mod image;
mod model;
mod package;
mod perf;
mod persistence;
mod trial;
mod util;

pub static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub use experiment::control::{
    kill_run, pause_run, resume_trial, KillResult, PauseResult, ResumeMode, ResumeResult,
};
pub use experiment::preflight::{preflight_experiment, preflight_experiment_with_options};
pub use experiment::runner::{
    continue_run, continue_run_with_options, experiment_summary, experiment_summary_with_options,
    fork_trial, recover_run, replay_trial, run_experiment, run_experiment_strict,
    run_experiment_strict_with_options, run_experiment_with_options,
    run_smoke_test_strict_with_options, run_smoke_test_with_options,
};
pub use experiment::state::RunExecutionOptions;
pub use model::{
    BuildResult, ExperimentSummary, ForkResult, MaterializationMode, PreflightCheck,
    PreflightReport, PreflightSeverity, RecoverResult, ReplayResult, RunResult,
};
pub use package::checks::check_package;
pub use package::compile::build_experiment_package;
pub use package::validate::validate_knob_overrides;
pub use perf::CLI_INVOKED_AT_MS_ENV;
pub use persistence::store::{
    account_sqlite_path_for_run, active_account_id, experiment_bundle_validation,
    mark_experiment_bundle_smoke_tested, register_experiment_bundle, ExperimentBundleValidation,
};

#[cfg(test)]
include!("tests.rs");
