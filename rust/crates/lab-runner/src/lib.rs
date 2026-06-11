mod backend;
mod config;
mod experiment;
mod image;
mod latch;
mod local_storage;
mod model;
mod package;
mod perf;
mod persistence;
pub mod telemetry;
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
pub use latch::{
    run_latch_manifest, validate_latch_manifest_file, EnforcementLevel, ExpectedOutput,
    LatchCaseManifest, LatchCaseResult, LatchCaseStatus, LatchDefaults, LatchGradeResult,
    LatchGradeStatus, LatchGraderSpec, LatchManifest, LatchManifestValidation, LatchRequirement,
    LatchRequirementKind, LatchRequirementObject, LatchRequirementProbe,
    LatchRequirementProbeStatus, LatchRunOptions, LatchRunResult, LaunchEnv, LaunchSpec,
    TaskInjection, UploadSpec, WorkspaceSeed, LATCH_MANIFEST_SCHEMA, LATCH_RESULT_SCHEMA,
};
pub use local_storage::{
    account_sqlite_path, bucephalus_home, cloud_profile_path, cloud_profile_string,
    default_agent_root, default_build_root, default_run_root, read_cloud_profile,
    write_cloud_profile,
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
pub use perf::PROCESS_INVOKED_AT_MS_ENV;
pub use persistence::backend::{RunStoreInventoryEntry, RunStoreMetrics};
pub use persistence::store::{
    account_sqlite_path_for_run, active_account_id, experiment_bundle_validation,
    mark_experiment_bundle_smoke_tested, register_experiment_bundle, ExperimentBundleValidation,
};
pub use trial::spec::{
    CaseMaterializationMountPlan, CaseMaterializationOperation, CaseMaterializationStage,
    CaseMaterializationStepPlan,
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

#[cfg(test)]
include!("tests.rs");
