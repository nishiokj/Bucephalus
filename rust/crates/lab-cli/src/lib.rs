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
#[path = "../../lab-runner/src/latch.rs"]
mod latch;
#[path = "../../lab-runner/src/latch_daemon.rs"]
mod latch_daemon;
#[path = "../../lab-runner/src/local_storage.rs"]
mod local_storage;
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
pub use latch::{
    run_latch_manifest, validate_latch_manifest_file, EnforcementLevel, ExpectedOutput,
    LatchCaseManifest, LatchCaseResult, LatchCaseStatus, LatchDefaults, LatchGradeResult,
    LatchGradeStatus, LatchGraderSpec, LatchManifest, LatchManifestValidation, LatchRequirement,
    LatchRequirementKind, LatchRequirementObject, LatchRequirementProbe,
    LatchRequirementProbeStatus, LatchRunOptions, LatchRunResult, LaunchEnv, LaunchSpec,
    TaskInjection, UploadSpec, WorkspaceSeed, LATCH_MANIFEST_SCHEMA, LATCH_RESULT_SCHEMA,
};
pub use latch_daemon::{
    call_latch_daemon, current_latch_daemon, ensure_latch_daemon, run_latch_daemon,
    LatchDaemonInfo, LatchDaemonRequest,
};
pub use local_storage::{
    account_sqlite_path, bucephalus_home, default_agent_root, default_build_root, default_run_root,
};
pub use model::{
    BuildResult, ExecutorKind, ExperimentSummary, ForkResult, MaterializationMode, PreflightCheck,
    PreflightReport, PreflightSeverity, RecoverResult, ReplayResult, RunResult,
};
pub use package::checks::check_package;
pub use package::compile::build_experiment_package;
pub use package::prepared_image::{
    prepare_runtime_images, PreparedRuntimeImageMapEntry, PreparedRuntimeImageOptions,
    PreparedRuntimeImageReport,
};
pub use package::validate::validate_knob_overrides;
pub use perf::CLI_INVOKED_AT_MS_ENV;
pub use persistence::backend::{RunStoreInventoryEntry, RunStoreMetrics};
pub use persistence::store::{
    account_sqlite_path_for_run, active_account_id, experiment_bundle_validation,
    mark_experiment_bundle_smoke_tested, register_experiment_bundle, ExperimentBundleValidation,
};
pub use trial::spec::{
    CaseMaterializationMountPlan, CaseMaterializationOperation, CaseMaterializationStage,
    CaseMaterializationStepPlan,
};

pub fn load_runtime_value_from_store(
    run_dir: &std::path::Path,
    key: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    persistence::backend::load_runtime_json(run_dir, key)
}

pub fn resolve_run_dir_from_store(
    run_id: &str,
    anchor: &std::path::Path,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    persistence::backend::resolve_run_dir(run_id, anchor)
}

pub fn list_run_store_inventory(
    anchor: &std::path::Path,
) -> anyhow::Result<Vec<RunStoreInventoryEntry>> {
    persistence::backend::list_run_inventory(anchor)
}

pub fn run_store_metrics(run_dir: &std::path::Path) -> anyhow::Result<RunStoreMetrics> {
    persistence::backend::run_metrics(run_dir)
}

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
