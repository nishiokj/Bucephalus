extern crate self as lab_analysis;
extern crate self as lab_core;
extern crate self as lab_provenance;
extern crate self as lab_schemas;

#[path = "../../lab-analysis/src/lib.rs"]
pub mod analysis;
#[path = "../../lab-core/src/lib.rs"]
mod lab_core_impl;
#[path = "../../lab-provenance/src/lib.rs"]
pub mod provenance;
#[path = "../../lab-schemas/src/lib.rs"]
pub mod schemas;

pub use analysis::*;
pub use lab_core_impl::*;
pub use provenance::*;
pub use schemas::*;

#[path = "../../lab-runner/src/backend/mod.rs"]
mod backend;
#[path = "../../lab-runner/src/config.rs"]
mod config;
#[path = "../../lab-runner/src/experiment/mod.rs"]
mod experiment;
#[path = "../../lab-runner/src/image.rs"]
mod image;
#[path = "../../lab-runner/src/model.rs"]
mod model;
#[path = "../../lab-runner/src/package/mod.rs"]
mod package;
#[path = "../../lab-runner/src/perf.rs"]
mod perf;
#[path = "../../lab-runner/src/persistence/mod.rs"]
mod persistence;
#[path = "../../lab-runner/src/trial/mod.rs"]
mod trial;
#[path = "../../lab-runner/src/util.rs"]
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
    BuildResult, ExecutorKind, ExperimentSummary, ForkResult, MaterializationMode, PreflightCheck,
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

pub fn run_control_record_key() -> &'static str {
    model::RUNTIME_KEY_RUN_CONTROL
}

pub fn engine_lease_record_key() -> &'static str {
    model::RUNTIME_KEY_ENGINE_LEASE
}

pub fn schedule_progress_record_key() -> &'static str {
    model::RUNTIME_KEY_SCHEDULE_PROGRESS
}

#[cfg(test)]
include!("../../lab-runner/src/tests.rs");
