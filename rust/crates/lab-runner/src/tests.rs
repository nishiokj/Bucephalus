#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
    use std::fs::{self, File};
    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result};
    use chrono::Utc;
    use crate::experiment::commit::SlotCommitState;
    use lab_schemas::compile_schema;
    use serde::Deserialize;
    use serde_json::{json, Value};
    use flate2::read::GzDecoder;
    use tar::Archive;

    use lab_core::{
        canonical_json_digest, ensure_dir, sha256_file, ArtifactStore,
        BUCEPHALUS_CONTRACT_IN_DIR, BUCEPHALUS_CONTRACT_OUT_DIR, BUCEPHALUS_ENV_CASE_ID,
        BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH, BUCEPHALUS_ENV_RESULT_PATH, BUCEPHALUS_ENV_RUN_ID,
        BUCEPHALUS_ENV_REPL_IDX, BUCEPHALUS_ENV_TASK_ID, BUCEPHALUS_ENV_TIMEOUT_MS,
        BUCEPHALUS_ENV_TRAJECTORY_PATH, BUCEPHALUS_ENV_TRIAL_ID, BUCEPHALUS_ENV_TRIAL_INPUT_PATH,
        BUCEPHALUS_ENV_VARIANT_ID, BUCEPHALUS_MAPPED_GRADER_OUTPUT_PATH, BUCEPHALUS_RESULT_PATH,
        BUCEPHALUS_RUNNER_SUPPORT_REL_DIR, BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER,
        BUCEPHALUS_TRAJECTORY_PATH, BUCEPHALUS_TRIAL_INPUT_PATH,
    };

    const TEST_HOST_GRADER_CAPABILITY: &str = "host_eval_capability";
    const TEST_PACKAGE_DIGEST: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TEST_PACKAGE_DIGEST_PREFIX: &str =
        "sha256_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    static RUNTIME_CONTROL_TEST_LOCK: Mutex<()> = Mutex::new(());
    static MODAL_ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_runtime_control_tests() -> MutexGuard<'static, ()> {
        RUNTIME_CONTROL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_modal_env_tests() -> MutexGuard<'static, ()> {
        MODAL_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn test_slot_commit_state<'a>(
        schedule_progress: &'a mut ScheduleProgress,
        trial_index: usize,
        pruned_variants: &'a mut HashSet<usize>,
        consecutive_failures: &'a mut BTreeMap<usize, usize>,
    ) -> SlotCommitState<'a> {
        SlotCommitState {
            schedule_progress,
            trial_index,
            pruned_variants,
            consecutive_failures,
        }
    }

    #[test]
    fn execution_module_keeps_provider_implementations_in_provider_modules() {
        let execution_rs = include_str!("trial/execution.rs");
        let local_docker_rs = include_str!("trial/execution/local_docker.rs");
        let grade_rs = include_str!("trial/grade.rs");
        let modal_rs = include_str!("trial/execution/modal.rs");

        assert!(execution_rs.contains("pub(crate) mod local_docker;"));
        assert!(execution_rs.contains("pub(crate) mod modal;"));
        assert!(!execution_rs.contains("struct LocalDockerExecutionBackend"));
        assert!(!execution_rs.contains("struct ModalExecutionBackend"));
        assert!(!execution_rs.contains("MODAL_SANDBOX_SCRIPT"));
        assert!([
            "DockerRuntime",
            "ContainerHandle",
            "Vec<ContainerMount>",
            "ContainerSpec",
            "ExecSpec",
            "NetworkHandle",
        ]
        .iter()
        .all(|name| !execution_rs.contains(name)));
        assert!(!execution_rs.contains("RuntimeSyncKind"));
        assert!(local_docker_rs.contains("struct LocalDockerExecutionBackend"));
        assert!(!local_docker_rs.contains("ExecStatus { exit_code: None }"));
        assert!(!grade_rs.contains("ExecStatus { exit_code: None }"));
        assert!(modal_rs.contains("struct ModalExecutionBackend"));
    }

    use crate::config::*;
    use crate::experiment::commit::{
        load_jsonl_value_rows, DeterministicCommitter, RunCoordinator,
    };
    use crate::experiment::control::*;
    use crate::experiment::lease::{
        acquire_run_operation_lease, engine_lease_is_stale, operation_lease_is_stale,
        operation_lease_path, start_engine_lease_heartbeat_with_writer, EngineLeaseRecord,
        OperationLeaseRecord, RunOperationType,
    };
    use crate::experiment::preflight::*;
    use crate::experiment::runner::*;
    use crate::experiment::runtime::*;
    use crate::experiment::state::*;
    use crate::image::{
        ImageReference, ImageReferenceSource, ImageRequirement, ImageRequirementRole,
        ImageResolutionMode, ImageResolveReport, ImageResolveRequest, ImageResolver,
        ImageResolverChain, OciRegistryReferenceKind, ReferenceOnlyImageResolver,
        ScopedImageResolverCache,
    };
    use crate::model::*;
    use crate::package::authoring::*;
    use crate::package::cas::{
        materialize_package_cas_backed_path, package_blob_path_for_digest,
        parse_large_file_threshold_bytes, put_file_in_package_cas, read_cas_pointer,
        should_include_agent_artifact_path, write_cas_pointer, PACKAGE_BLOBS_DIR,
    };
    use crate::package::checks::{check_package, PACKAGE_CHECKS_SCHEMA_VERSION};
    use crate::package::compile::*;
    use crate::package::prepared_image::*;
    use crate::package::registry::load_grader_capability_manifest;
    use crate::package::sealed::*;
    use crate::package::staging::*;
    use crate::package::validate::*;
    use crate::persistence::journal::*;
    use crate::persistence::rows::*;
    use crate::persistence::store::SqliteRunStore as BackingSqliteStore;
    use crate::persistence::store::*;
    use crate::persistence::writer::RunStoreWriterGuard;
    use crate::trial::env::{
        build_exec_env, resolve_host_grader_command, resolve_runtime_agent_command,
        ResolvedGradingPhase,
    };
    use crate::trial::events::{load_event_rows, spawn_live_event_ingest, LiveEventIngestRequest};
    use crate::trial::execution::{
        sidecar_env_for_stage_for_test, TrialRunRequest, EvidenceBlobRef, ExecutionBackend,
        TrialRuntimeExecutionRequest,
    };
    use crate::trial::execution::local_docker::{
        acquire_docker_active_container_permit_for_test, build_container_spec,
        docker_network_mode, materialize_grader_inputs_for_test,
        planned_docker_active_container_units_for_test, LocalBindMountRuntimeSync,
        LocalContainerRuntimeSync, LocalDockerExecutionBackend,
        BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
    };
    use crate::trial::execution::modal::{
        acquire_modal_active_sandbox_permit_for_test, build_modal_runtime_transfer_archive_for_test,
        load_modal_runtime_worker_ids_for_test, modal_launch_spec_for_test,
        modal_launch_spec_with_grading_for_test, modal_launcher_go_source_for_test,
        modal_launcher_log_tail_bytes_for_test, modal_launcher_command_for_test,
        parse_modal_sandbox_result_for_test, planned_modal_active_sandbox_units_for_test,
        read_captured_file_value_for_test, read_modal_launcher_log_tail_for_test,
        record_modal_sandbox_cleanup, run_modal_launcher_command_for_test, ModalExecutionBackend,
        S3CompatibleRuntimeSync, BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES_ENV,
    };
    use crate::trial::execution::{
        map_container_path_to_host, persist_attempt_state, resolve_agent_artifact_mount_dir,
        run_host_grader, validate_container_workspace_path,
    };
    use crate::trial::grade::grading_retry_inputs;
    use crate::trial::layout::*;
    use crate::trial::preflight::stage_trial_preflight;
    use crate::trial::plan::parse_trial_runtime_config;
    use crate::trial::prepare::{
        build_runtime_contract_env, build_trial_input, prepare_task_environment,
        resolve_trial_io_host_path, resolve_trial_timeout_ms, PreparedTaskEnvironment,
        PrepareTaskEnvironmentRequest, PREPARED_RUNTIME_IMAGE_MAP_PACKAGE_REL_PATH, TrialPaths,
    };
    use crate::trial::spec::{
        parse_task_boundary_from_packaged_task, parse_task_row, CaseMaterializationOperation,
        CaseMaterializationStage, CaseMaterializationStepPlan, TaskBoundaryMaterialization,
        TaskMaterializationKind, TaskMaterializationSpec,
    };
    use crate::trial::state::{
        trial_state_path, write_trial_state, AttemptFsLayout, AttemptSlotRef,
        EphemeralNetworkState, EphemeralSandboxState, IoMountPlan, GradingSandboxState,
        TaskSandboxPlan, TaskSandboxState, TrialAttemptKey, TrialAttemptState, TrialPhase,
        TrialStateGuard,
    };
    use crate::util::*;

    const BUCEPHALUS_CONTRACT_STATE_DIR: &str = "/bucephalus/state";
    const BUCEPHALUS_CONTRACT_WORKSPACE_DIR: &str = "/bucephalus/workspace";

    fn prepare_task_environment_for_test(
        project_root: &Path,
        trial_dir: &Path,
        trial_experiment: &Value,
        variant: &Variant,
        task_boundary: &TaskBoundaryMaterialization,
        agent_runtime: &AgentRuntimeConfig,
    ) -> Result<PreparedTaskEnvironment> {
        prepare_task_environment(
            project_root,
            trial_dir,
            PrepareTaskEnvironmentRequest {
                run_id: "run_1",
                trial_experiment,
                variant,
                task_idx: 0,
                repl: 0,
                task_boundary,
                agent_runtime,
            },
        )
    }

    fn test_schedule_engine_request<'a>(
        run_dir: &'a Path,
        trials_dir: &'a Path,
        variants: &'a [Variant],
        tasks: &'a [Value],
        schedule: &'a [TrialSlot],
        policy_config: &'a PolicyConfig,
        evaluation_config: &'a EvaluationConfig,
    ) -> ScheduleEngineRequest<'a> {
        ScheduleEngineRequest {
            run_dir,
            run_id: "run_1",
            workload_type: "agent_runtime",
            project_root: run_dir,
            variants,
            tasks,
            schedule,
            policy_config,
            evaluation_config,
            metric_definitions: &[],
            variant_runtime_profiles: &[],
            executor_kind: ExecutorKind::LocalDocker,
            materialize_mode: MaterializationMode::OutputsOnly,
            trials_dir,
            recovered_active_trials: &[],
            baseline_id: "base",
            max_concurrency: 1,
            stdout_progress: false,
        }
    }

    fn test_schedule_engine_state<'a>(
        schedule_progress: &'a mut ScheduleProgress,
        trial_index: &'a mut usize,
        consecutive_failures: &'a mut BTreeMap<usize, usize>,
        pruned_variants: &'a mut HashSet<usize>,
        run_sink: &'a mut dyn RunSink,
    ) -> ScheduleEngineState<'a> {
        ScheduleEngineState {
            schedule_progress,
            trial_index,
            consecutive_failures,
            pruned_variants,
            run_sink,
        }
    }

    fn test_trial_slot(task_idx: usize) -> TrialSlot {
        TrialSlot {
            variant_idx: 0,
            task_idx,
            repl_idx: 0,
        }
    }

    fn test_trial_schedule(slot_count: usize) -> Vec<TrialSlot> {
        (0..slot_count).map(test_trial_slot).collect()
    }

    #[test]
    fn stdout_progress_bar_reports_completed_fraction() {
        assert_eq!(
            format_progress_bar(3, 10, 10),
            "[###-------] 30.0%"
        );
        assert_eq!(format_progress_bar(0, 0, 10), "[##########] 100.0%");
    }

    #[test]
    fn stdout_progress_table_is_ascii_and_includes_trial_counts() {
        let table = render_ascii_table(
            "Bucephalus run progress",
            &[
                ("Completed trials", "2/5"),
                ("Progress", "[##########--------------] 40.0%"),
            ],
        );

        assert!(table.contains("[##########--------------] 40.0%"));
        assert!(table.is_ascii());
    }

    #[test]
    fn stdout_progress_option_is_not_persisted_in_run_session() {
        let execution = RunExecutionOptions {
            stdout_progress: true,
            ..RunExecutionOptions::default()
        };

        let encoded = serde_json::to_value(&execution).expect("serialize execution options");
        assert!(encoded.get("stdout_progress").is_none());
        let decoded = serde_json::from_value::<RunExecutionOptions>(encoded).unwrap();
        assert!(!decoded.stdout_progress);
    }

    struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "{}_{}_{}",
                prefix,
                std::process::id(),
                Utc::now().timestamp_micros()
            ));
            ensure_dir(&path).expect("temp dir");
            Self { path }
        }
    }

    #[test]
    fn host_agent_command_clears_parent_environment_before_declaring_contract_env() {
        let execution_rs = include_str!("trial/execution.rs");
        assert!(
            execution_rs.contains(".env_clear()\n        .envs(&env)"),
            "host agent execution must not inherit cloud worker environment"
        );
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn stdout_progress_reads_latest_agent_output_preview() {
        let root = TempDirGuard::new("bucephalus_stdout_agent_output");
        let run_dir = root.path.join("runs").join("run_1");
        let agent_dir = run_dir
            .join("trials")
            .join("trial_1")
            .join("agent");
        ensure_dir(&agent_dir).expect("agent dir");
        fs::write(
            agent_dir.join("result.json"),
            serde_json::to_vec_pretty(&json!({
                "answer": {
                    "summary": "the raw answer the user needs to see",
                    "confidence": 0.9
                },
                "metrics": {"resolved": 1.0}
            }))
            .expect("agent result json"),
        )
        .expect("write agent result");

        let progress = latest_agent_output_progress_for_trial(&run_dir, "trial_1");
        let preview = progress.output_preview.as_deref().unwrap_or("");

        assert!(
            preview.contains("the raw answer the user needs to see"),
            "preview was {preview}"
        );
        assert_eq!(progress.capture_status, "captured");
    }

    #[test]
    fn stdout_progress_reports_missing_agent_result_file() {
        let root = TempDirGuard::new("bucephalus_stdout_agent_output_missing");
        let run_dir = root.path.join("runs").join("run_1");

        let progress = latest_agent_output_progress_for_trial(&run_dir, "trial_1");

        assert_eq!(progress.output_preview, None);
        assert_eq!(progress.capture_status, "missing agent result file");
    }

    fn create_run_dir(prefix: &str, run_id: &str) -> (TempDirGuard, PathBuf) {
        let root = TempDirGuard::new(prefix);
        let run_dir = root.path.join("runs").join(run_id);
        ensure_dir(&run_dir).expect("run dir");
        (root, run_dir)
    }

    fn write_smoke_run_fixture(run_dir: &Path, slot_statuses: &[&str]) -> RunResult {
        write_run_control(run_dir, "run_smoke", "completed", &[], None).expect("run control");
        let schedule = slot_statuses
            .iter()
            .enumerate()
            .map(|(idx, _)| TrialSlot {
                variant_idx: idx,
                task_idx: 0,
                repl_idx: 0,
            })
            .collect::<Vec<_>>();
        let progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "run_smoke".to_string(),
            total_slots: slot_statuses.len(),
            next_schedule_index: slot_statuses.len(),
            next_trial_index: slot_statuses.len(),
            schedule,
            completed_slots: slot_statuses
                .iter()
                .enumerate()
                .map(|(idx, status)| SlotCompletion {
                    schedule_index: idx,
                    trial_id: format!("trial_{}", idx + 1),
                    status: (*status).to_string(),
                    slot_commit_id: format!("commit_{}", idx + 1),
                    attempt: 1,
                })
                .collect(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        write_schedule_progress(run_dir, &progress).expect("schedule progress");
        RunResult {
            run_dir: run_dir.to_path_buf(),
            run_id: "run_smoke".to_string(),
            account_db_path: run_dir.join("account.sqlite"),
        }
    }

    #[test]
    fn smoke_test_requires_successful_trial_slots() {
        let (_root, run_dir) = create_run_dir("bucephalus_smoke_failed_slot", "run_smoke");
        let result = write_smoke_run_fixture(&run_dir, &["error", "completed"]);

        let err = ensure_smoke_test_completed(result).expect_err("failed slot must fail smoke");

        assert!(err.to_string().contains("trial slots failed"));
        assert!(err.to_string().contains("status=error"));
    }

    #[test]
    fn smoke_test_accepts_all_successful_trial_slots() {
        let (_root, run_dir) = create_run_dir("bucephalus_smoke_success_slots", "run_smoke");
        let result = write_smoke_run_fixture(&run_dir, &["completed", "completed"]);

        ensure_smoke_test_completed(result).expect("all successful slots should pass smoke");
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

    #[test]
    fn active_account_id_rejects_empty_and_missing_identity_inputs() {
        assert_eq!(
            account_id_from_parts(Some(" account_1 "), None, None, None).unwrap(),
            "account_1"
        );
        for (result, expected) in [
            (
                account_id_from_parts(Some(" "), Some("user"), None, Some("/home/user")),
                "BUCEPHALUS_ACCOUNT_ID",
            ),
            (
                account_id_from_parts(None, None, None, None),
                "USER or USERNAME",
            ),
        ] {
            assert!(result.unwrap_err().to_string().contains(expected));
        }
    }

    fn prepared_trial_io_fixture(output_host: PathBuf, events_host: PathBuf) -> PreparedTrialIo {
        PreparedTrialIo {
            trial_input_host: PathBuf::from("/tmp/trial_input.json"),
            result_host: output_host.clone(),
            events_host,
            trial_input_path: BUCEPHALUS_TRIAL_INPUT_PATH.to_string(),
            result_path: BUCEPHALUS_RESULT_PATH.to_string(),
            mapped_grader_output_path: BUCEPHALUS_MAPPED_GRADER_OUTPUT_PATH.to_string(),
            trajectory_path: BUCEPHALUS_TRAJECTORY_PATH.to_string(),
        }
    }

    fn task_sandbox_plan_fixture(
        image: &str,
        workdir: &str,
        network_mode: &str,
    ) -> TaskSandboxPlan {
        TaskSandboxPlan {
            image: image.to_string(),
            workdir: workdir.to_string(),
            platform: None,
            materialization: TaskMaterializationSpec {
                kind: TaskMaterializationKind::TaskImage,
                task_bundle_ref: None,
                platform: None,
            },
            case_materialization: Vec::new(),
            io_mounts: IoMountPlan {
                in_dir: BUCEPHALUS_CONTRACT_IN_DIR.to_string(),
                out_dir: BUCEPHALUS_CONTRACT_OUT_DIR.to_string(),
                telemetry_mounts: Vec::new(),
            },
            artifact_mount: None,
            network_mode: network_mode.to_string(),
            time_limit_ms: 30_000,
        }
    }

    fn modal_runtime_sync_fixture(with_remote_config: bool) -> S3CompatibleRuntimeSync {
        S3CompatibleRuntimeSync::for_test(
            "bucephalus-bucket",
            "runs/run_1/trial_1/attempt_1",
            with_remote_config.then_some("https://r2.example"),
            with_remote_config.then_some("auto"),
            with_remote_config.then_some("bucephalus-r2"),
            with_remote_config,
        )
    }

    #[test]
    fn evidence_blob_ref_local_path_writes_local_artifact_ref() {
        let root = TempDirGuard::new("bucephalus_evidence_blob_local");
        let blob_path = root.path.join("stdout.log");
        fs::write(&blob_path, "hello").expect("write blob");
        let store = ArtifactStore::new(root.path.join("artifacts"));

        let object_ref = crate::trial::schedule::evidence_blob_ref(
            &store,
            Some(EvidenceBlobRef::LocalPath(blob_path)),
        )
        .expect("local evidence ref")
        .expect("ref present");

        assert!(object_ref.starts_with("artifact://sha256/"));
        assert_eq!(
            store.read_ref(&object_ref).expect("read artifact"),
            b"hello"
        );
    }

    #[test]
    fn artifact_store_rejects_invalid_refs() {
        let root = TempDirGuard::new("bucephalus_artifact_ref_invalid");
        let store = ArtifactStore::new(root.path.join("artifacts"));

        for artifact_ref in [
            "artifact://sha256/../../escape",
            "artifact://sha256/not-hex",
            "sha256:0123",
        ] {
            assert!(
                store.read_ref(artifact_ref).is_err(),
                "artifact ref should be rejected: {}",
                artifact_ref
            );
        }
    }

    #[test]
    fn evidence_blob_ref_remote_ref_does_not_require_local_file() {
        let root = TempDirGuard::new("bucephalus_evidence_blob_remote");
        let store = ArtifactStore::new(root.path.join("artifacts"));

        let object_ref = crate::trial::schedule::evidence_blob_ref(
            &store,
            Some(EvidenceBlobRef::RemoteRef {
                uri: "s3://bucephalus-runtime/run/trial/stdout.log".to_string(),
            }),
        )
        .expect("remote evidence ref")
        .expect("ref present");

        assert_eq!(object_ref, "s3://bucephalus-runtime/run/trial/stdout.log");
        assert!(!root.path.join("artifacts").exists());
    }

    #[test]
    fn evidence_record_schema_accepts_modal_executor() {
        let artifact_ref = format!("artifact://sha256/{}", "a".repeat(64));
        let record = json!({
            "schema_version": "evidence_record_v1",
            "schedule_idx": 0,
            "slot_commit_id": "slot_1",
            "attempt": 1,
            "row_seq": 0,
            "ts": "2026-01-01T00:00:00Z",
            "ids": {
                "run_id": "run_1",
                "trial_id": "trial_1",
                "variant_id": "baseline",
                "case_id": "task_1",
                "repl_idx": 0
            },
            "runtime": {
                "executor": "modal",
                "exit_status": "0",
                "duration_ms": 1.0
            },
            "evidence": {
                "trial_input_ref": artifact_ref,
                "trial_output_ref": artifact_ref,
                "workspace_pre_ref": artifact_ref,
                "workspace_post_ref": artifact_ref,
                "diff_incremental_ref": artifact_ref,
                "diff_cumulative_ref": artifact_ref,
                "patch_incremental_ref": artifact_ref,
                "patch_cumulative_ref": artifact_ref
            }
        });
        let schema = compile_schema("evidence_record_v1.jsonschema").expect("schema");
        match schema.validate(&record) {
            Ok(_) => {}
            Err(errors) => {
                let messages = errors.map(|err| err.to_string()).collect::<Vec<_>>();
                panic!(
                    "evidence_record_v1 schema should accept modal executor: {}",
                    messages.join(" | ")
                );
            }
        };
    }

    #[test]
    fn case_v2_schema_accepts_mapped_task_shapes() {
        let schema = compile_schema("case_v2.jsonschema").expect("schema");
        let rows = vec![
            json!({
                "schema_version": "case_v2",
                "id": "prompt_only",
                "inputs": { "prompt": "Summarize this input string." },
                "resources": { "workspace": { "source": "empty" } }
            }),
            json!({
                "schema_version": "case_v2",
                "id": "script_setup",
                "inputs": { "prompt": "Run the evaluation harness." },
                "resources": {
                    "workspace": {
                        "source": "container_image",
                        "image": "python:3.11-slim",
                        "workdir": "/workspace/task"
                    },
                    "assets": {
                        "setup_script": {
                            "type": "file",
                            "path": "fixtures/setup.sh"
                        }
                    }
                },
                "materialization": [
                    {
                        "id": "prepare-fixtures",
                        "stage": "case",
                        "operation": "command",
                        "command": ["bash", "/workspace/task/fixtures/setup.sh"],
                        "network": "none"
                    }
                ]
            }),
            json!({
                "schema_version": "case_v2",
                "id": "dataset_pack_workspace",
                "inputs": { "prompt": "Use the unpacked dataset files." },
                "resources": {
                    "workspace": {
                        "source": "dataset_pack",
                        "dataset_pack_ref": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    }
                }
            }),
            json!({
                "schema_version": "case_v2",
                "id": "git_checkout_workspace",
                "inputs": { "issue": "Fix the regression." },
                "resources": {
                    "workspace": {
                        "source": "git_checkout",
                        "repo": "https://example.invalid/project.git",
                        "commit": "0123456789abcdef"
                    }
                }
            })
        ];

        for row in rows {
            if let Err(errors) = schema.validate(&row) {
                let messages = errors.map(|err| err.to_string()).collect::<Vec<_>>();
                panic!(
                    "case_v2 schema should accept mapped row {}: {}",
                    row.pointer("/id").and_then(Value::as_str).unwrap_or("<unknown>"),
                    messages.join(" | ")
                );
            }
        }
    }

    #[test]
    fn trial_summary_schema_accepts_extra_output_keys() {
        let summary = json!({
            "schema_version": "trial_summary_v1",
            "ids": {
                "run_id": "run_1",
                "trial_id": "trial_1",
                "variant_id": "base",
                "task_id": "case_1",
                "repl_idx": 0
            },
            "outcome": "success",
            "primary_metric": {
                "name": "resolved",
                "value": 1.0
            },
            "agent": {
                "outcome": "success",
                "exit_status": "0",
                "result": "agent/result.json",
                "events": "agent/events.jsonl",
                "stdout": "agent/stdout.log",
                "stderr": "agent/stderr.log"
            },
            "grader": {
                "status": "success",
                "mapped_output": "grader/mapped_output.json"
            },
            "artifacts": {
                "candidate_patch": "candidate.patch",
                "metadata": "runner/trial_metadata.json",
                "prepared_task_environment": "runner/prepared_task_environment.json",
                "state_inventory": "runner/state_inventory.json",
                "contract_trace": "runner/contract_trace.json",
                "host_eval": "grader/host_eval.json"
            },
            "metrics": {}
        });
        let schema = compile_schema("trial_summary_v1.jsonschema").expect("schema");
        if let Err(errors) = schema.validate(&summary) {
            let messages = errors.map(|err| err.to_string()).collect::<Vec<_>>();
            panic!(
                "trial_summary schema should accept extra output keys: {}",
                messages.join(" | ")
            );
        };
    }

    #[test]
    fn case_v2_schema_rejects_task_row_shape() {
        let schema = compile_schema("case_v2.jsonschema").expect("schema");
        let row = json!({
            "schema_version": "case_v2",
            "id": "legacy_shape",
            "task": { "id": "legacy_shape", "prompt": "hello" },
            "runtime": {
                "container_image": {
                    "image": "python:3.11-slim",
                    "workdir": "/workspace/task"
                }
            }
        });

        let errors = schema
            .validate(&row)
            .expect_err("case_v2 must reject task-row-shaped payloads")
            .map(|err| err.to_string())
            .collect::<Vec<_>>();
        let joined = errors.join(" | ");
        assert!(
            joined.contains("Additional properties are not allowed"),
            "unexpected schema errors: {}",
            joined
        );
    }

    #[test]
    fn remote_executor_option_is_rejected_until_executor_is_wired() {
        let execution = RunExecutionOptions {
            executor: Some(ExecutorKind::Remote),
            ..container_execution()
        };

        let err = ensure_supported_executor(&execution).expect_err("remote executor should fail");
        assert!(err.to_string().contains("no remote trial executor is wired"));

        let missing = RunExecutionOptions {
            executor: None,
            ..container_execution()
        };
        let err = ensure_supported_executor(&missing).expect_err("missing executor should fail");
        assert!(err.to_string().contains("execution.executor is required"));
    }

    #[test]
    fn modal_executor_option_is_supported() {
        let execution = RunExecutionOptions {
            executor: Some(ExecutorKind::Modal),
            ..container_execution()
        };

        assert_eq!(
            ensure_supported_executor(&execution).expect("modal executor supported"),
            ExecutorKind::Modal
        );
    }

    #[test]
    fn modal_s3_sync_env_requires_bucket_prefix_and_formats_remote_refs() {
        let _lock = lock_modal_env_tests();
        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_S3_BUCKET", None),
            ("BUCEPHALUS_S3_BUCKET", None),
            ("BUCEPHALUS_MODAL_S3_PREFIX", None),
            ("BUCEPHALUS_S3_PREFIX", None),
            ("BUCEPHALUS_MODAL_S3_ENDPOINT_URL", None),
            ("BUCEPHALUS_S3_ENDPOINT_URL", None),
            ("BUCEPHALUS_MODAL_S3_REGION", None),
            ("AWS_REGION", None),
            ("BUCEPHALUS_MODAL_S3_SECRET", None),
            ("BUCEPHALUS_MODAL_S3_FORCE_PATH_STYLE", None),
            ("BUCEPHALUS_S3_FORCE_PATH_STYLE", None),
        ]);

        let missing =
            S3CompatibleRuntimeSync::from_env_for_test("run_a", "trial_1", 2).expect_err(
                "modal S3 sync must require an explicit bucket",
            );
        assert!(
            missing
                .to_string()
                .contains("requires BUCEPHALUS_MODAL_S3_BUCKET"),
            "unexpected error: {missing}"
        );

        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_S3_BUCKET", Some("bucephalus-bucket")),
            ("BUCEPHALUS_MODAL_S3_PREFIX", None),
            ("BUCEPHALUS_S3_PREFIX", None),
        ]);
        let missing =
            S3CompatibleRuntimeSync::from_env_for_test("run_a", "trial_1", 2).expect_err(
                "modal S3 sync must require an explicit prefix",
            );
        assert!(
            missing
                .to_string()
                .contains("requires BUCEPHALUS_MODAL_S3_PREFIX"),
            "unexpected error: {missing}"
        );

        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_S3_BUCKET", Some("bucephalus-bucket")),
            ("BUCEPHALUS_MODAL_S3_PREFIX", Some("/runs/root/")),
            ("BUCEPHALUS_MODAL_S3_FORCE_PATH_STYLE", Some("maybe")),
        ]);
        assert!(
            S3CompatibleRuntimeSync::from_env_for_test("run_a", "trial_1", 2)
                .unwrap_err()
                .to_string()
                .contains("BUCEPHALUS_MODAL_S3_FORCE_PATH_STYLE must be a boolean"),
        );

        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_S3_BUCKET", Some("bucephalus-bucket")),
            ("BUCEPHALUS_MODAL_S3_PREFIX", Some("/runs/root/")),
            ("BUCEPHALUS_MODAL_S3_ENDPOINT_URL", Some("https://r2.example")),
            ("BUCEPHALUS_MODAL_S3_REGION", Some("auto")),
            ("BUCEPHALUS_MODAL_S3_SECRET", Some("bucephalus-r2")),
            ("BUCEPHALUS_MODAL_S3_FORCE_PATH_STYLE", Some("true")),
        ]);
        let sync = S3CompatibleRuntimeSync::from_env_for_test("run_a", "trial_1", 2)
            .expect("modal S3 sync");

        assert_eq!(
            sync.uri_for_contract_path_for_test("/bucephalus/out/result.json"),
            "s3://bucephalus-bucket/runs/root/run_a/trial_1/attempt_2/out/result.json"
        );
        assert_eq!(
            sync.uri_for_contract_path_for_test("bucephalus/out/stdout.log"),
            "s3://bucephalus-bucket/runs/root/run_a/trial_1/attempt_2/out/stdout.log"
        );
    }

    #[test]
    fn modal_backend_requires_explicit_app_name() {
        let _lock = lock_modal_env_tests();
        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_APP_NAME", None),
            ("BUCEPHALUS_MODAL_ENVIRONMENT", None),
        ]);

        let err = ModalExecutionBackend::from_env().expect_err("modal app name must be explicit");
        assert!(err.to_string().contains("requires BUCEPHALUS_MODAL_APP_NAME"));

        let _guard = EnvVarGuard::set(&[("BUCEPHALUS_MODAL_APP_NAME", Some(" "))]);
        let err = ModalExecutionBackend::from_env().expect_err("modal app name must be nonempty");
        assert!(err.to_string().contains("app name must not be empty"));

        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_APP_NAME", Some("bucephalus-modal")),
            ("BUCEPHALUS_MODAL_LAUNCHER", None),
        ]);
        let err = ModalExecutionBackend::from_env().expect_err("modal launcher must be explicit");
        assert!(err.to_string().contains("BUCEPHALUS_MODAL_LAUNCHER"));
    }

    #[test]
    fn modal_case_asset_prefix_requires_package_lock_digest() {
        let root = TempDirGuard::new("bucephalus_modal_case_asset_prefix_digest");
        let sync = modal_runtime_sync_fixture(false);

        let err = sync
            .immutable_case_asset_prefix_for_test(&root.path)
            .expect_err("modal case asset prefix must require package lock");
        assert!(err.to_string().contains("requires readable package.lock"));
    }

    #[test]
    fn modal_launch_spec_uses_contract_paths_for_runtime_interface() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_modal_launch_spec_contract");
        atomic_write_json_pretty(
            &paths.exp_dir.join("package.lock"),
            &json!({
                "schema_version": "sealed_package_lock_v1",
                "package_digest": TEST_PACKAGE_DIGEST
            }),
        )
        .expect("package lock");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::from([
            ("STATIC_ENV".to_string(), "ok".to_string()),
            ("OPENAI_API_KEY".to_string(), "secret-value".to_string()),
        ]);
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let dynamic_mount_source = root.path.join("fixture-pack");
        fs::write(&dynamic_mount_source, "fixture").expect("dynamic mount source");
        let dynamic_mounts = vec![ResolvedMountReference {
            host_path: dynamic_mount_source.clone(),
            mount_path: format!("{}/dataset_pack", BUCEPHALUS_CONTRACT_WORKSPACE_DIR),
            read_only: true,
        }];
        let runtime_experiment = json!({
            "runtime": {
                "secrets": [
                    {"name": "OPENAI_API_KEY", "from": "env"}
                ]
            },
            "policy": {
                "task_sandbox": {
                    "resources": {
                        "cpu_count": 2,
                        "memory_mb": 4096
                    }
                }
            }
        });
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &["--flag".to_string()],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &dynamic_mounts,
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let backend = ModalExecutionBackend::for_test("bucephalus-test", Some("dev"));
        let sync = modal_runtime_sync_fixture(true);
        let plan = task_sandbox_plan_fixture("python:3.11-slim", "/workspace/task", "none");
        let spec = modal_launch_spec_for_test(
            &backend,
            &sync,
            &request,
            &paths.trial_dir,
            &plan,
            vec!["python".to_string(), "/agent.py".to_string()],
        )
        .expect("modal launch spec");

        assert_eq!(spec.pointer("/app_name"), Some(&json!("bucephalus-test")));
        assert_eq!(spec.pointer("/environment_name"), Some(&json!("dev")));
        assert_eq!(spec.pointer("/image"), Some(&json!("python:3.11-slim")));
        assert_eq!(spec.pointer("/workdir"), Some(&json!("/workspace/task")));
        assert_eq!(spec.pointer("/block_network"), Some(&json!(true)));
        assert_eq!(spec.pointer("/cpu_count"), Some(&json!(2)));
        assert_eq!(spec.pointer("/memory_mb"), Some(&json!(4096)));
        assert_eq!(
            spec.pointer("/secret_env"),
            Some(&json!(["OPENAI_API_KEY"]))
        );
        assert_eq!(spec.pointer("/sync/type"), Some(&json!("s3_compatible")));
        assert_eq!(spec.pointer("/sync/bucket"), Some(&json!("bucephalus-bucket")));
        assert_eq!(
            spec.pointer("/sync/prefix"),
            Some(&json!("runs/run_1/trial_1/attempt_1"))
        );
        assert_eq!(
            spec.pointer("/sync/immutable_case_asset_prefix"),
            Some(&json!(format!(
                "runs/packages/{TEST_PACKAGE_DIGEST_PREFIX}/case_assets"
            )))
        );
        assert_eq!(spec.pointer("/sync/endpoint_url"), Some(&json!("https://r2.example")));
        assert_eq!(spec.pointer("/sync/region"), Some(&json!("auto")));
        assert_eq!(spec.pointer("/sync/modal_secret_name"), Some(&json!("bucephalus-r2")));
        assert_eq!(spec.pointer("/sync/force_path_style"), Some(&json!(true)));
        assert_eq!(spec.pointer("/poll_interval_ms"), Some(&json!(1000)));
        assert_eq!(spec.pointer("/execs/0/phase"), Some(&json!("agent")));
        assert_eq!(
            spec.pointer("/execs/0/command"),
            Some(&json!(["python", "/agent.py"]))
        );
        assert_eq!(
            spec.pointer("/execs/0/workdir"),
            Some(&json!("/workspace/task"))
        );

        let env = spec
            .pointer("/execs/0/env")
            .and_then(Value::as_object)
            .expect("env object");
        assert_eq!(env.get("STATIC_ENV"), Some(&json!("ok")));
        assert!(
            !env.contains_key("OPENAI_API_KEY"),
            "secret env values must not be serialized into modal launch specs"
        );
        assert_eq!(
            env.get(BUCEPHALUS_ENV_TRIAL_INPUT_PATH),
            Some(&json!(BUCEPHALUS_TRIAL_INPUT_PATH))
        );
        assert_eq!(
            env.get(BUCEPHALUS_ENV_RESULT_PATH),
            Some(&json!(BUCEPHALUS_RESULT_PATH))
        );
        assert_eq!(
            env.get(BUCEPHALUS_ENV_MAPPED_GRADER_OUTPUT_PATH),
            Some(&json!(BUCEPHALUS_MAPPED_GRADER_OUTPUT_PATH))
        );
        assert_eq!(
            env.get(BUCEPHALUS_ENV_TRAJECTORY_PATH),
            Some(&json!(BUCEPHALUS_TRAJECTORY_PATH))
        );
        assert_ne!(
            env.get(BUCEPHALUS_ENV_TRIAL_INPUT_PATH),
            Some(&json!(io_paths.trial_input_host.to_string_lossy().to_string()))
        );

        assert_eq!(
            spec.pointer("/result/remote_path"),
            Some(&json!(BUCEPHALUS_RESULT_PATH))
        );
        assert_eq!(
            spec.pointer("/result/local_path"),
            Some(&json!(io_paths.result_host.to_string_lossy().to_string()))
        );
        assert_eq!(
            spec.pointer("/events/scratch_path"),
            Some(&json!(BUCEPHALUS_TRAJECTORY_PATH))
        );
        assert_eq!(
            spec.pointer("/events/local_path"),
            Some(&json!(io_paths.events_host.to_string_lossy().to_string()))
        );
        assert_eq!(spec.pointer("/events/durable_path"), Some(&json!(null)));
        assert_eq!(
            spec.pointer("/execs/0/stdout/remote_path"),
            Some(&json!("/bucephalus/out/stdout.log"))
        );
        assert_eq!(
            spec.pointer("/execs/0/stderr/remote_path"),
            Some(&json!("/bucephalus/out/stderr.log"))
        );

        let runtime_files = spec
            .pointer("/runtime_files")
            .and_then(Value::as_array)
            .expect("runtime files array");
        for file in runtime_files {
            let remote_path = file
                .get("remote_path")
                .and_then(Value::as_str)
                .expect("remote path");
            assert!(
                remote_path.starts_with("/bucephalus/"),
                "modal remote copy path must stay in contract namespace: {remote_path}"
            );
        }
        assert!(runtime_files.iter().any(|file| {
            file.get("priority").and_then(Value::as_str) == Some("runtime_transfer")
                && file.get("remote_path").and_then(Value::as_str) == Some(BUCEPHALUS_CONTRACT_IN_DIR)
        }));
        assert!(runtime_files.iter().any(|file| {
            file.get("priority").and_then(Value::as_str) == Some("runtime_transfer")
                && file.get("remote_path").and_then(Value::as_str)
                    == Some(BUCEPHALUS_CONTRACT_WORKSPACE_DIR)
        }));
        assert!(runtime_files.iter().any(|file| {
            file.get("priority").and_then(Value::as_str) == Some("runtime_transfer")
                && file.get("remote_path").and_then(Value::as_str)
                    == Some("/bucephalus/workspace/dataset_pack")
                && file.get("local_path").and_then(Value::as_str)
                    == Some(dynamic_mount_source.to_string_lossy().as_ref())
        }));
        assert_eq!(
            spec.pointer("/launch_mounts")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn modal_launch_spec_omits_agent_transfer_when_runtime_image_is_prepared() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_modal_prepared_runtime_image");
        let mut runtime = legacy_contract_runtime_fixture();
        let artifact_dir = root.path.join("agent-artifact");
        ensure_dir(&artifact_dir).expect("artifact dir");
        runtime.agent_artifact = Some(artifact_dir.clone());
        runtime.agent_artifact_mount_path = Some("/opt/agent".to_string());
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: runtime.agent_artifact.as_deref(),
            agent_artifact_mount_path: runtime.agent_artifact_mount_path.as_deref(),
            agent_artifact_read_only: runtime.agent_artifact_read_only,
        };
        let backend = ModalExecutionBackend::for_test("bucephalus-test", Some("dev"));
        let sync = modal_runtime_sync_fixture(true);
        let mut plan = task_sandbox_plan_fixture(
            "ghcr.io/acme/python-agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "/workspace/task",
            "none",
        );
        plan.artifact_mount = None;

        let spec = modal_launch_spec_for_test(
            &backend,
            &sync,
            &request,
            &paths.trial_dir,
            &plan,
            vec!["/opt/agent/bin/agentctl".to_string()],
        )
        .expect("modal launch spec");
        let runtime_files = spec
            .pointer("/runtime_files")
            .and_then(Value::as_array)
            .expect("runtime files array");

        assert_eq!(
            spec.pointer("/image"),
            Some(&json!("ghcr.io/acme/python-agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"))
        );
        assert!(
            runtime_files.iter().all(|file| {
                file.get("remote_path").and_then(Value::as_str) != Some("/opt/agent")
            }),
            "prepared runtime images must not ship the agent artifact through Modal runtime_files: {runtime_files:?}"
        );
    }

    #[test]
    fn modal_launcher_does_not_mount_runtime_transfer_dirs_to_r2() {
        let source = modal_launcher_go_source_for_test();
        assert!(
            !source.contains("\"/bucephalus/out\":"),
            "outputs must stay on sandbox-local storage until post-run export"
        );
        assert!(
            source.contains("CopyFromLocal(ctx, runtimeTransferArchive, runtimeTransferArchivePath"),
            "Go SDK lacks image.add_local_file, so the launcher must stage the prebuilt archive with one filesystem transfer"
        );
        assert!(
            source.contains("\"tar -xzf \"+runtimeTransferArchivePath+\" -C /\"")
                || source.contains("\"tar -xzf \" + runtimeTransferArchivePath + \" -C /\""),
            "runtime files should be extracted inside the sandbox"
        );
        assert!(
            source.contains("func bootstrapRuntimeTransferExec(execSpec map[string]any)"),
            "agent exec should own runtime transfer bootstrap"
        );
        assert!(
            source.contains("bootstrapRuntimeTransfer && index == 0"),
            "agent-only runs should avoid a separate pre-agent extraction exec"
        );
        assert!(
            source.contains("BUCEPHALUS_CONTAINER_STARTED_AT="),
            "agent exec should emit an in-container start marker before bootstrap work"
        );
        assert!(
            source.contains("BUCEPHALUS_AGENT_COMMAND_STARTED_AT="),
            "agent exec should emit a second marker after runtime transfer bootstrap"
        );
        assert!(
            !source.contains("modal.Sandbox.create"),
            "launcher must use Modal's official Go SDK, not Python snippets"
        );
    }

    #[test]
    fn modal_runtime_transfer_archive_keeps_symlink_escape_check() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_modal_symlink_escape");
        let source_dir = paths.exp_dir.join("source");
        let outside = paths.exp_dir.join("outside.txt");
        ensure_dir(&source_dir).expect("source dir");
        fs::write(&outside, "outside").expect("outside");
        #[cfg(unix)]
        symlink(&outside, source_dir.join("escape")).expect("symlink");
        #[cfg(not(unix))]
        fs::write(source_dir.join("escape"), "outside").expect("outside marker");
        let launch = json!({
            "runtime_files": [{
                "local_path": source_dir,
                "remote_path": "/bucephalus/in/source",
            }]
        });
        ensure_dir(&paths.trial_dir.join("modal")).expect("modal dir");
        let result = build_modal_runtime_transfer_archive_for_test(&paths.trial_dir.join("modal"), launch);
        #[cfg(unix)]
        assert!(
            result
                .expect_err("archive builder must reject escaping symlinks")
                .to_string()
                .contains("refusing to archive symlink outside directory artifact")
        );
        #[cfg(not(unix))]
        assert!(result.is_ok());
    }

    #[test]
    fn modal_runtime_transfer_archive_normalizes_file_metadata() -> Result<()> {
        let (root, paths) = create_trial_paths_fixture("bucephalus_modal_runtime_archive_metadata");
        let modal_dir = paths.trial_dir.join("modal");
        ensure_dir(&modal_dir)?;
        let payload_path = root.path.join("payload.txt");
        fs::write(&payload_path, "payload")?;
        let launch = json!({
            "runtime_files": [{
                "local_path": payload_path,
                "remote_path": "/bucephalus/in/payload.txt",
            }]
        });

        let archive_path = build_modal_runtime_transfer_archive_for_test(&modal_dir, launch)?;
        let archive_file = File::open(archive_path)?;
        let decoder = GzDecoder::new(archive_file);
        let mut archive = Archive::new(decoder);
        let mut found = false;
        for entry in archive.entries()? {
            let entry = entry?;
            if entry.path()?.as_ref() == Path::new("bucephalus/in/payload.txt") {
                let header = entry.header();
                assert_eq!(header.uid()?, 0);
                assert_eq!(header.gid()?, 0);
                assert_eq!(header.username()?, Some(""));
                assert_eq!(header.groupname()?, Some(""));
                assert_eq!(header.mtime()?, 0);
                found = true;
            }
        }
        assert!(found, "payload missing from modal runtime transfer archive");
        Ok(())
    }

    #[test]
    fn modal_launch_spec_separates_runtime_files_from_launch_mounts() {
        let (root, paths) =
            create_trial_paths_fixture("bucephalus_modal_runtime_files_launch_mounts");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let dynamic_mount_source = root.path.join("dynamic-mount.txt");
        fs::write(&dynamic_mount_source, "dynamic").expect("dynamic mount");
        let dynamic_mounts = vec![ResolvedMountReference {
            host_path: dynamic_mount_source.clone(),
            mount_path: "/bucephalus/workspace/dataset_pack".to_string(),
            read_only: true,
        }];
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &dynamic_mounts,
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let backend = ModalExecutionBackend::for_test("bucephalus-test", None);
        let sync = modal_runtime_sync_fixture(false);
        let plan = task_sandbox_plan_fixture("python:3.11-slim", "/workspace/task", "none");
        let spec = modal_launch_spec_for_test(
            &backend,
            &sync,
            &request,
            &paths.trial_dir,
            &plan,
            vec!["python".to_string(), "/agent.py".to_string()],
        )
        .expect("modal launch spec");

        let runtime_files = spec
            .pointer("/runtime_files")
            .and_then(Value::as_array)
            .expect("runtime files");
        assert!(runtime_files.iter().any(|file| {
            file.get("remote_path").and_then(Value::as_str) == Some(BUCEPHALUS_CONTRACT_IN_DIR)
        }));
        assert!(runtime_files.iter().any(|file| {
            file.get("remote_path").and_then(Value::as_str)
                == Some(BUCEPHALUS_CONTRACT_WORKSPACE_DIR)
        }));
        assert!(runtime_files.iter().any(|file| {
            file.get("remote_path").and_then(Value::as_str)
                == Some("/bucephalus/workspace/dataset_pack")
                && file.get("local_path").and_then(Value::as_str)
                    == Some(dynamic_mount_source.to_string_lossy().as_ref())
        }));
        assert_eq!(
            spec.pointer("/launch_mounts")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn modal_launch_spec_projects_case_assets_to_package_scoped_s3_prefix() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_modal_case_assets_s3");
        atomic_write_json_pretty(
            &paths.exp_dir.join("package.lock"),
            &json!({
                "schema_version": "sealed_package_lock_v1",
                "package_digest": TEST_PACKAGE_DIGEST
            }),
        )
        .expect("package lock");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let case_asset = root.path.join("case-image.png");
        fs::write(&case_asset, "image").expect("case asset");
        let dynamic_mounts = vec![ResolvedMountReference {
            host_path: case_asset.clone(),
            mount_path: "/bucephalus/case_assets/000_case-image.png".to_string(),
            read_only: true,
        }];
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &dynamic_mounts,
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let backend = ModalExecutionBackend::for_test("bucephalus-test", None);
        let sync = modal_runtime_sync_fixture(false);
        let plan = task_sandbox_plan_fixture("python:3.11-slim", "/workspace/task", "none");
        let spec = modal_launch_spec_for_test(
            &backend,
            &sync,
            &request,
            &paths.trial_dir,
            &plan,
            vec!["python".to_string(), "/agent.py".to_string()],
        )
        .expect("modal launch spec");

        assert_eq!(
            spec.pointer("/sync/immutable_case_asset_prefix"),
            Some(&json!(format!(
                "runs/packages/{TEST_PACKAGE_DIGEST_PREFIX}/case_assets"
            )))
        );
        let launch_mounts = spec
            .pointer("/launch_mounts")
            .and_then(Value::as_array)
            .expect("launch mounts");
        assert_eq!(launch_mounts.len(), 1);
        assert_eq!(
            launch_mounts[0].pointer("/remote_path"),
            Some(&json!("/bucephalus/case_assets/000_case-image.png"))
        );
        assert_eq!(
            launch_mounts[0].pointer("/local_path"),
            Some(&json!(case_asset.to_string_lossy().to_string()))
        );
        assert_eq!(
            launch_mounts[0].pointer("/priority"),
            Some(&json!("launch_required"))
        );
        let runtime_files = spec
            .pointer("/runtime_files")
            .and_then(Value::as_array)
            .expect("runtime files");
        assert!(
            !runtime_files.iter().any(|file| {
                file.pointer("/remote_path").and_then(Value::as_str)
                    == Some("/bucephalus/case_assets/000_case-image.png")
            }),
            "case assets should not be copied through the per-attempt Modal sync plane"
        );
    }

    #[test]
    fn modal_launch_spec_rejects_duplicate_copy_remote_paths() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_modal_duplicate_copy");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let duplicate_source = root.path.join("duplicate-input");
        fs::write(&duplicate_source, "duplicate").expect("duplicate source");
        let dynamic_mounts = vec![ResolvedMountReference {
            host_path: duplicate_source,
            mount_path: format!("{}/", BUCEPHALUS_CONTRACT_IN_DIR),
            read_only: true,
        }];
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &dynamic_mounts,
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let backend = ModalExecutionBackend::for_test("bucephalus-test", None);
        let sync = modal_runtime_sync_fixture(false);
        let plan = task_sandbox_plan_fixture("python:3.11-slim", "/workspace/task", "none");

        let err = modal_launch_spec_for_test(
            &backend,
            &sync,
            &request,
            &paths.trial_dir,
            &plan,
            vec!["python".to_string(), "/agent.py".to_string()],
        )
        .expect_err("modal launch copies must not target the same remote path twice");
        let msg = err.to_string();
        assert!(
            msg.contains("modal copy remote_path '/bucephalus/in' is declared more than once"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn modal_launch_spec_rejects_broad_copy_target_that_contains_contract_paths() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_modal_broad_copy");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let broad_source = root.path.join("broad-copy");
        ensure_dir(&broad_source).expect("broad copy source");
        fs::write(broad_source.join("payload.txt"), "payload").expect("payload");
        let dynamic_mounts = vec![ResolvedMountReference {
            host_path: broad_source,
            mount_path: "/bucephalus".to_string(),
            read_only: true,
        }];
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &dynamic_mounts,
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let backend = ModalExecutionBackend::for_test("bucephalus-test", None);
        let sync = modal_runtime_sync_fixture(false);
        let plan = task_sandbox_plan_fixture("python:3.11-slim", "/workspace/task", "none");

        let err = modal_launch_spec_for_test(
            &backend,
            &sync,
            &request,
            &paths.trial_dir,
            &plan,
            vec!["python".to_string(), "/agent.py".to_string()],
        )
        .expect_err("modal launch must reject a copy target that contains contract paths");
        let msg = err.to_string();
        assert!(
            msg.contains("modal copy remote_path '/bucephalus' overlaps with '/bucephalus/in'"),
            "unexpected error: {msg}"
        );
    }


    #[test]
    fn modal_copy_helper_creates_parent_dirs_for_file_copies() {
        let source = modal_launcher_go_source_for_test();
        assert!(
            source.contains("parent := path.Dir(remotePath)"),
            "modal copy helper must compute the parent for single-file uploads"
        );
        assert!(
            source.contains("return fsys.CopyFromLocal(ctx, localPath, remotePath, nil)"),
            "modal copy helper must create the remote parent before copying a single file"
        );
    }

    #[test]
    fn modal_bucket_mount_helper_slash_terminates_key_prefixes() {
        let source = modal_launcher_go_source_for_test();
        assert!(
            source.contains("keyPrefix += \"/\""),
            "Modal CloudBucketMount key_prefix values must be directory prefixes"
        );
    }

    #[test]
    fn modal_copy_helper_rejects_directory_symlink_escape() {
        let source = modal_launcher_go_source_for_test();
        assert!(
            source.contains("root, err := filepath.EvalSymlinks(localPath)"),
            "modal directory copy helper must establish a resolved source root"
        );
        assert!(
            source.contains("filepath.Rel(root, resolved)"),
            "modal directory copy helper must verify symlink targets stay inside the copied root"
        );
        assert!(
            source.contains("refusing to copy symlink outside directory artifact"),
            "modal directory copy helper must reject symlinks that escape the copied root"
        );
    }

    #[test]
    fn modal_launcher_persists_runtime_workers_before_completion() {
        let source = modal_launcher_go_source_for_test();
        assert!(
            source.contains("runtime_workers.json"),
            "modal launcher must persist worker ids for external kill/recover"
        );
        assert!(
            source.contains("writeRuntimeWorker(specPath, \"task\", sandbox)"),
            "modal launcher must record the task sandbox immediately after creation"
        );
    }

    #[test]
    fn modal_launcher_keeps_sandboxes_alive_for_staging() {
        let source = modal_launcher_go_source_for_test();
        assert!(
            source.contains("Command:           []string{\"sleep\", \"31536000\"}"),
            "Modal sandboxes must run an idle process while Bucephalus stages files and executes phases"
        );
    }

    #[test]
    fn modal_cleanup_terminates_persisted_sandbox_ids() {
        let source = modal_launcher_go_source_for_test();
        assert!(
            source.contains("mc.Sandboxes.FromID(ctx, sandboxID)"),
            "modal cleanup must recover sandbox handles from persisted ids"
        );
        assert!(
            source.contains("sandbox.Terminate(ctx, nil)"),
            "modal cleanup must terminate recovered sandbox handles"
        );
    }

    #[test]
    fn modal_launcher_log_tail_is_bounded_and_preserves_final_marker() -> Result<()> {
        let (_root, run_dir) = create_run_dir("bucephalus_modal_log_tail", "run_1");
        let modal_dir = run_dir.join("modal");
        ensure_dir(&modal_dir)?;
        let log_path = modal_dir.join("sandbox_stdout.log");
        let tail_bytes = usize::try_from(modal_launcher_log_tail_bytes_for_test())
            .expect("modal launcher log tail limit must fit usize");
        let marker =
            "BUCEPHALUS_MODAL_RESULT={\"sandbox_id\":\"sb-1\",\"exit_code\":0,\"timed_out\":false}\n";
        let mut bytes = b"too-old-to-keep\n".to_vec();
        bytes.resize(bytes.len() + tail_bytes + 128, b'x');
        bytes.extend(marker.as_bytes());
        fs::write(&log_path, bytes)?;

        let tail = read_modal_launcher_log_tail_for_test(&log_path)?;
        assert!(tail.len() <= tail_bytes);
        assert!(!tail.contains("too-old-to-keep"));
        assert!(tail.contains("BUCEPHALUS_MODAL_RESULT="));
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn modal_launcher_command_redirects_output_to_trial_logs() -> Result<()> {
        let (_root, run_dir) = create_run_dir("bucephalus_modal_command_logs", "run_1");
        let modal_dir = run_dir.join("modal");
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "printf 'BUCEPHALUS_MODAL_RESULT={\"sandbox_id\":\"sb-1\",\"exit_code\":0,\"timed_out\":false}\\n'; printf 'launcher warning\\n' >&2",
        );

        let (status, stdout_tail, stderr_tail, stdout_path) =
            run_modal_launcher_command_for_test(command, &modal_dir, "sandbox")?;

        assert!(status.success());
        assert!(stdout_tail.contains("BUCEPHALUS_MODAL_RESULT="));
        assert!(stderr_tail.contains("launcher warning"));
        assert!(stdout_path.exists());
        assert!(modal_dir.join("sandbox_stderr.log").exists());
        Ok(())
    }

    #[test]
    fn modal_launcher_command_uses_packaged_helper_without_go_run() {
        let (_root, run_dir) = create_run_dir("bucephalus_modal_packaged_helper", "run_1");
        let modal_dir = run_dir.join("modal");
        let spec_path = modal_dir.join("launch.json");
        let launcher = PathBuf::from("/opt/bucephalus/bin/bucephalus-modal-launcher");

        let command = modal_launcher_command_for_test(&launcher, &modal_dir, "launch", &spec_path);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert_eq!(command.get_program(), launcher.as_os_str());
        assert_eq!(
            args,
            vec!["launch".to_string(), spec_path.to_string_lossy().to_string()]
        );
        assert!(!modal_dir.join("go.mod").exists());
        assert!(!modal_dir.join("main.go").exists());
    }

    #[test]
    fn inline_runtime_capture_budget_is_explicitly_configured() -> Result<()> {
        let _lock = lock_modal_env_tests();
        let root = TempDirGuard::new("bucephalus_inline_capture_budget");
        let output_path = root.path.join("screenshot.txt");
        fs::write(&output_path, "x".repeat(64))?;

        let _unset = EnvVarGuard::set(&[(BUCEPHALUS_MAX_INLINE_CAPTURE_BYTES_ENV, None)]);
        let value = read_captured_file_value_for_test(&output_path, "text")?;
        assert_eq!(value.as_str().map(str::len), Some(64));
        drop(_unset);

        let _configured =
            EnvVarGuard::set(&[(BUCEPHALUS_MAX_INLINE_CAPTURE_BYTES_ENV, Some("16"))]);
        let err = read_captured_file_value_for_test(&output_path, "text")
            .expect_err("configured inline capture budget should reject oversized text");
        assert!(err.to_string().contains("too large to inline"));

        let bytes_value = read_captured_file_value_for_test(&output_path, "bytes")?;
        assert_eq!(bytes_value.get("bytes").and_then(Value::as_u64), Some(64));
        Ok(())
    }

    #[test]
    fn modal_launch_spec_wires_grader_transport_without_docker_shape() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_modal_grader_transport");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({
            "trial_runtime": {
                "agent": {
                    "outputs": {
                        "answer": {
                            "capture": {
                                "type": "result_json",
                                "path": BUCEPHALUS_RESULT_PATH,
                                "field": "/payload/answer"
                            }
                        }
                    }
                }
            }
        });
        let grader = GraderConfig {
            strategy: GradingStrategy::InTaskRuntime,
            command: vec!["python".to_string(), "/workspace/task/grade.py".to_string()],
            max_concurrency: None,
            in_task_runtime: Some(InTaskRuntimeGradingConfig::default()),
            injected: None,
            separate: None,
            host: None,
            inputs: BTreeMap::from([(
                "answer_file".to_string(),
                RuntimeInputConfig {
                    source: RuntimeTransportSourceConfig {
                        output: Some("agent.answer".to_string()),
                        field: Some("/value".to_string()),
                        case: None,
                        task: None,
                        object: None,
                    },
                    materialize: RuntimeInputMaterializeConfig {
                        as_kind: "json_file".to_string(),
                        path: Some("/bucephalus/out/grader_inputs/answer.json".to_string()),
                        name: None,
                    },
                    required: true,
                },
            )]),
            outputs: BTreeMap::from([(
                "score".to_string(),
                RuntimeOutputConfig {
                    capture: RuntimeOutputCaptureConfig {
                        capture_type: "file".to_string(),
                        path: Some("/bucephalus/out/score.json".to_string()),
                        format: Some("json".to_string()),
                        field: None,
                        required: true,
                    },
                },
            )]),
        };
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let backend = ModalExecutionBackend::for_test("bucephalus-test", None);
        let sync = modal_runtime_sync_fixture(false);
        let plan = task_sandbox_plan_fixture("python:3.11-slim", "/workspace/task", "none");
        let spec = modal_launch_spec_with_grading_for_test(
            &backend,
            &sync,
            &request,
            &paths.trial_dir,
            &plan,
            vec!["python".to_string(), "/agent.py".to_string()],
        )
        .expect("modal launch spec with grading");

        assert_eq!(spec.pointer("/grader/strategy"), Some(&json!("in_task_runtime")));
        assert_eq!(
            spec.pointer("/grader/command"),
            Some(&json!(["python", "/workspace/task/grade.py"]))
        );
        assert_eq!(spec.pointer("/grader/sandbox"), Some(&json!("task")));
        assert_eq!(
            spec.pointer("/grader/agent_outputs/answer/capture/type"),
            Some(&json!("result_json"))
        );
        assert_eq!(
            spec.pointer("/grader/inputs/answer_file/materialize/path"),
            Some(&json!("/bucephalus/out/grader_inputs/answer.json"))
        );
        assert_eq!(
            spec.pointer("/grader/outputs/score/capture/local_path"),
            Some(&json!(paths.out.join("score.json").to_string_lossy().to_string()))
        );
        assert_eq!(
            spec.pointer("/transport_envelope/remote_path"),
            Some(&json!("/bucephalus/out/runtime_transport_envelope.json"))
        );
        assert!(spec.pointer("/grader/docker").is_none());
    }

    #[test]
    fn modal_sandbox_result_uses_agent_exec_control_plane_state() {
        let value = json!({
            "sandbox_id": "sb-123",
            "exit_code": 99,
            "timed_out": false,
            "started_at": "2026-01-01T00:00:00Z",
            "ended_at": "2026-01-01T00:01:00Z",
            "runtime_transfer_archive_bytes": 123456,
            "execs": [
                {
                    "phase": "setup",
                    "process_id": "proc-setup",
                    "exit_code": 0,
                    "timed_out": false,
                    "started_at": "2026-01-01T00:00:01Z",
                    "ended_at": "2026-01-01T00:00:02Z"
                },
                {
                    "phase": "agent",
                    "process_id": "proc-agent",
                    "exit_code": 7,
                    "timed_out": true,
                    "started_at": "2026-01-01T00:00:03Z",
                    "container_started_at": "2026-01-01T00:00:04Z",
                    "agent_command_started_at": "2026-01-01T00:00:05Z",
                    "ended_at": "2026-01-01T00:00:09Z"
                }
            ]
        });

        let result = parse_modal_sandbox_result_for_test(&value).expect("modal result");
        assert_eq!(result.sandbox_id.as_deref(), Some("sb-123"));
        assert_eq!(result.process_id.as_deref(), Some("proc-agent"));
        assert_eq!(result.exit_code, Some(7));
        assert!(result.timed_out);
        assert_eq!(
            result.started_at.as_deref(),
            Some("2026-01-01T00:00:03Z")
        );
        assert_eq!(
            result.container_started_at.as_deref(),
            Some("2026-01-01T00:00:04Z")
        );
        assert_eq!(
            result.agent_command_started_at.as_deref(),
            Some("2026-01-01T00:00:05Z")
        );
        assert_eq!(result.runtime_transfer_archive_bytes, Some(123456));
        assert_eq!(
            result.ended_at.as_deref(),
            Some("2026-01-01T00:00:09Z")
        );
    }

    #[test]
    fn modal_sandbox_result_rejects_execs_without_agent_phase() {
        let value = json!({
            "sandbox_id": "sb-123",
            "exit_code": 0,
            "timed_out": false,
            "execs": [
                {
                    "phase": "setup",
                    "process_id": "proc-setup",
                    "exit_code": 0,
                    "timed_out": false
                },
                {
                    "phase": "grader",
                    "process_id": "proc-grader",
                    "exit_code": 0,
                    "timed_out": false
                }
            ]
        });

        let err = match parse_modal_sandbox_result_for_test(&value) {
            Ok(_) => panic!("modal result with execs must include an agent phase"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("modal sandbox launcher did not report agent exec result"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn modal_sandbox_result_rejects_missing_timeout_state() {
        let value = json!({
            "sandbox_id": "sb-123",
            "exit_code": 0,
            "started_at": "2026-01-01T00:00:00Z",
            "ended_at": "2026-01-01T00:01:00Z"
        });

        let err = match parse_modal_sandbox_result_for_test(&value) {
            Ok(_) => panic!("modal result must not default missing timed_out=false"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("modal sandbox launcher did not report timed_out"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn modal_sandbox_result_rejects_non_string_timings() {
        let value = json!({
            "sandbox_id": "sb-123",
            "exit_code": 0,
            "timed_out": false,
            "timings": {
                "modal_launcher_main_to_sandbox_create_start": 42
            }
        });

        let err = match parse_modal_sandbox_result_for_test(&value) {
            Ok(_) => panic!("modal result timings must not silently drop non-string values"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("modal sandbox timing 'modal_launcher_main_to_sandbox_create_start' must be a string"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn modal_executor_rejects_host_agent_site_before_requiring_sync_config() {
        let _lock = lock_modal_env_tests();
        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_S3_BUCKET", None),
            ("BUCEPHALUS_S3_BUCKET", None),
        ]);
        let (_root, paths) = create_trial_paths_fixture("bucephalus_modal_rejects_host");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({
            "trial_runtime": {
                "execution": {
                    "agent_site": "host"
                }
            }
        });
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let executor = ModalExecutionBackend::for_test("bucephalus-test", None);
        let err = match executor.execute_attempt(TrialRuntimeExecutionRequest {
            trial_dir: &paths.trial_dir,
            schedule_idx: 0,
            attempt_no: 1,
            run_request: &request,
            task_id: "task_1",
            variant_id: "baseline",
            repl_idx: 0,
            task_sandbox_plan: &task_sandbox_plan_fixture(
                "python:3.11-slim",
                "/workspace/task",
                "none",
            ),
        }) {
            Ok(_) => panic!("modal executor should reject host agent site before launch"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("agent_site=host"),
            "unexpected error: {err}"
        );
        assert!(
            !err.to_string().contains("S3_BUCKET"),
            "host-site validation should run before sync env validation: {err}"
        );
    }

    #[test]
    fn modal_executor_rejects_sidecars_before_requiring_sync_config() {
        let _lock = lock_modal_env_tests();
        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_S3_BUCKET", None),
            ("BUCEPHALUS_S3_BUCKET", None),
        ]);
        let (_root, paths) = create_trial_paths_fixture("bucephalus_modal_rejects_sidecars");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({
            "sidecars": {
                "mcp-bash": {
                    "image": "ghcr.io/acme/mcp-bash-server:v0.4",
                    "lifecycle": "per-trial"
                }
            },
            "trial_runtime": {
                "agent": {
                    "sidecars": ["mcp-bash"]
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let executor = ModalExecutionBackend::for_test("bucephalus-test", None);
        let err = match executor.execute_attempt(TrialRuntimeExecutionRequest {
            trial_dir: &paths.trial_dir,
            schedule_idx: 0,
            attempt_no: 1,
            run_request: &request,
            task_id: "task_1",
            variant_id: "baseline",
            repl_idx: 0,
            task_sandbox_plan: &task_sandbox_plan_fixture(
                "python:3.11-slim",
                "/workspace/task",
                "none",
            ),
        }) {
            Ok(_) => panic!("modal executor should reject sidecars before launch"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("does not yet support trial_runtime sidecars"),
            "unexpected error: {err}"
        );
        assert!(
            !err.to_string().contains("S3_BUCKET"),
            "sidecar support validation should run before sync env validation: {err}"
        );
    }

    #[test]
    fn modal_executor_requires_s3_sync_for_supported_request_before_launch() {
        let _lock = lock_modal_env_tests();
        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_S3_BUCKET", None),
            ("BUCEPHALUS_S3_BUCKET", None),
            ("BUCEPHALUS_MODAL_S3_PREFIX", None),
            ("BUCEPHALUS_S3_PREFIX", None),
        ]);
        let (_root, paths) = create_trial_paths_fixture("bucephalus_modal_requires_s3");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let executor = ModalExecutionBackend::for_test("bucephalus-test", None);
        let err = match executor.execute_attempt(TrialRuntimeExecutionRequest {
            trial_dir: &paths.trial_dir,
            schedule_idx: 0,
            attempt_no: 1,
            run_request: &request,
            task_id: "task_1",
            variant_id: "baseline",
            repl_idx: 0,
            task_sandbox_plan: &task_sandbox_plan_fixture(
                "python:3.11-slim",
                "/workspace/task",
                "none",
            ),
        }) {
            Ok(_) => panic!("modal executor should require S3 sync before launch"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("requires BUCEPHALUS_MODAL_S3_BUCKET"),
            "unexpected error: {err}"
        );
    }

    fn prepared_trial_io_fixture_with_contract_paths(
        trial_input_path: &str,
        result_path: &str,
        mapped_grader_output_path: &str,
        trajectory_path: &str,
    ) -> PreparedTrialIo {
        PreparedTrialIo {
            trial_input_host: PathBuf::from("/tmp/trial_input.json"),
            result_host: PathBuf::from("/out"),
            events_host: PathBuf::from("/events"),
            trial_input_path: trial_input_path.to_string(),
            result_path: result_path.to_string(),
            mapped_grader_output_path: mapped_grader_output_path.to_string(),
            trajectory_path: trajectory_path.to_string(),
        }
    }

    fn harness_success_command() -> Vec<String> {
        vec![
            "sh".to_string(),
            "-lc".to_string(),
            "printf '%s' '{\"checkpoints\":[]}'".to_string(),
        ]
    }

    fn harness_success_output_command() -> Vec<String> {
        vec![
            "/bin/sh".to_string(),
            "/opt/agent/bin/write_success_result.sh".to_string(),
        ]
    }

    fn agent_execution_fixture(_image: Option<&str>) -> AgentExecutionConfig {
        AgentExecutionConfig {}
    }

    fn legacy_contract_runtime_fixture() -> AgentRuntimeConfig {
        AgentRuntimeConfig {
            command_raw: vec!["sh".to_string(), "-lc".to_string(), "echo ok".to_string()],
            image: "img:latest".to_string(),
            network: "none".to_string(),
            sandbox_image: Some("img:latest".to_string()),
            image_source: ImageSource::Global,
            execution: agent_execution_fixture(Some("img:latest")),
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
            agent_artifact_digest: None,
            agent_artifact_resolved_path: None,
            integration_level: "cli_basic".to_string(),
            env: BTreeMap::new(),
            env_from_host: vec![],
            secret_files: Vec::new(),
            event_sinks: Vec::new(),
            output_mounts: Vec::new(),
            trajectory_path: None,
            causal_extraction: None,
            dependency_file_staging: Vec::new(),
        }
    }

    fn command_contains_flag_value(command: &[String], flag: &str, value: &str) -> bool {
        command
            .windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    }

    fn scratch_workspace() -> WorkspaceSpec {
        WorkspaceSpec {
            mode: WorkspaceMode::Scratch,
            base: WorkspaceBaseSpec {
                kind: WorkspaceBaseKind::Empty,
                dataset_pack_ref: None,
                repo: None,
                commit: None,
            },
            overlays: Vec::new(),
            aux_mounts: Vec::new(),
        }
    }

    fn runtime_task_boundary(
        task_payload: Value,
        task_image: &str,
        task_workdir: &str,
        time_limit_ms: Option<u64>,
    ) -> TaskBoundaryMaterialization {
        let task_id = task_payload
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "task_0".to_string());
        let materialization = TaskMaterializationSpec {
            kind: TaskMaterializationKind::TaskImage,
            task_bundle_ref: None,
            platform: None,
        };
        let declaration = json!({
            "schema_version": "task_row_v2",
            "id": task_id,
            "time_limit_ms": time_limit_ms,
            "task": task_payload.clone(),
            "runtime": {
                "container_image": {
                    "image": task_image,
                    "workdir": task_workdir
                }
            }
        });
        TaskBoundaryMaterialization {
            declaration,
            task_payload,
            workspace: scratch_workspace(),
            dependencies: json!({}),
            materialization,
            case_materialization: Vec::new(),
            task_id,
            task_image: task_image.to_string(),
            task_workdir: task_workdir.to_string(),
            time_limit_ms,
        }
    }

    fn runtime_task_boundary_from_row(task_row: Value) -> TaskBoundaryMaterialization {
        let parsed = parse_task_row(&task_row).expect("task row");
        let declaration = serde_json::to_value(&parsed).expect("task row value");
        TaskBoundaryMaterialization {
            declaration,
            task_payload: parsed.task.clone(),
            workspace: scratch_workspace(),
            dependencies: json!({}),
            materialization: TaskMaterializationSpec {
                kind: TaskMaterializationKind::TaskImage,
                task_bundle_ref: None,
                platform: parsed
                    .runtime
                    .container_image
                    .as_ref()
                    .and_then(|container| container.platform.clone()),
            },
            case_materialization: Vec::new(),
            task_id: parsed.task_id(),
            task_image: parsed
                .runtime
                .container_image
                .as_ref()
                .map(|container| container.image.clone())
                .unwrap_or_default(),
            task_workdir: parsed
                .runtime
                .container_image
                .as_ref()
                .map(|container| container.workdir.clone())
                .unwrap_or_default(),
            time_limit_ms: parsed.time_limit_ms,
        }
    }

    fn task_row_value(
        task_id: &str,
        image: &str,
        workdir: &str,
        time_limit_ms: Option<u64>,
    ) -> Value {
        json!({
            "schema_version": "task_row_v2",
            "id": task_id,
            "time_limit_ms": time_limit_ms,
            "task": { "id": task_id },
            "runtime": {
                "container_image": {
                    "image": image,
                    "workdir": workdir
                }
            }
        })
    }

    fn base_image_bundle_task_row(
        task_id: &str,
        image: &str,
        workdir: &str,
        task_bundle_ref: &str,
    ) -> Value {
        let _ = task_bundle_ref;
        json!({
            "schema_version": "task_row_v2",
            "id": task_id,
            "task": { "id": task_id },
            "runtime": {
                "container_image": {
                    "image": image,
                    "workdir": workdir
                }
            }
        })
    }

    fn base_image_bundle_task_boundary(
        task_id: &str,
        image: &str,
        workdir: &str,
        task_bundle_ref: &str,
    ) -> TaskBoundaryMaterialization {
        let mut boundary = runtime_task_boundary(
            json!({
                "id": task_id,
                "prompt": "solve it"
            }),
            image,
            workdir,
            None,
        );
        boundary.materialization = TaskMaterializationSpec {
            kind: TaskMaterializationKind::BaseImageBundle,
            task_bundle_ref: Some(task_bundle_ref.to_string()),
            platform: None,
        };
        boundary
    }

    fn ensure_test_agent_bundle(project_root: &Path, bundle_name: &str) -> PathBuf {
        let bundle_root = project_root.join(".lab").join("agents").join(bundle_name);
        let bin_dir = bundle_root.join("bin");
        ensure_dir(&bin_dir).expect("test bundle bin dir");
        for name in ["sh", "python", "python3", "node", "agentctl"] {
            fs::copy("/bin/sh", bin_dir.join(name)).expect("copy test bundle executable");
        }
        let write_success_script = bin_dir.join("write_success_result.sh");
        fs::write(
            &write_success_script,
            concat!(
                "#!/bin/sh\n",
                "printf '%s' '{\"checkpoints\":[]}' > /bucephalus/out/result.json\n"
            ),
        )
        .expect("write test bundle success script");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&write_success_script)
                .expect("script metadata")
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&write_success_script, perms).expect("script permissions");
        }
        bundle_root
    }

    fn container_execution() -> RunExecutionOptions {
        RunExecutionOptions {
            executor: Some(ExecutorKind::LocalDocker),
            ..RunExecutionOptions::default()
        }
    }

    fn docker_runtime_available() -> bool {
        crate::backend::docker::DockerRuntime::connect()
            .and_then(|runtime| runtime.ping())
            .is_ok()
    }

    fn ensure_docker_test_image(image: &str) {
        try_ensure_docker_test_image(image).expect("container image");
    }

    fn try_ensure_docker_test_image(image: &str) -> Result<()> {
        crate::backend::docker::DockerRuntime::connect()
            .and_then(|runtime| runtime.ensure_image(image))
            .map(|_| ())
    }

    fn try_build_docker_test_image(root: &Path, tag_suffix: &str, dockerfile: &str) -> Result<String> {
        try_ensure_docker_test_image("python:3.11-slim")?;
        let dockerfile_path = root.join("Dockerfile");
        fs::write(&dockerfile_path, dockerfile).expect("dockerfile");
        let tag = format!(
            "bucephalus-test-{}-{}:{}",
            sanitize_for_fs(tag_suffix),
            std::process::id(),
            Utc::now().timestamp_micros()
        );
        let output = Command::new("docker")
            .args(["build", "-t", &tag, root.to_string_lossy().as_ref()])
            .output()
            .context("docker build")?;
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "docker build failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(tag)
    }

    fn write_resolved_experiment(
        run_dir: &Path,
        integration_level: &str,
        include_events_path: bool,
    ) {
        let _ = include_events_path;
        let project_root = find_project_root(run_dir);
        let bundle_root = ensure_test_agent_bundle(&project_root, "agent-current");

        let resolved = json!({
            "experiment": { "id": "e", "name": "n" },
            "matrix": {
                "tasks": { "source": "file", "path": "tasks.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }],
                "repeats": 1
            },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "storage": { "backend": "local-fs" },
                "traces": { "backend": "local-stdout" },
                "network": { "task_sandbox": "none", "agent": "none" }
            },
            "policy": {
                "sanitization_profile": "hermetic_functional",
                "timeout_ms": 600000,
                "task_sandbox": {}
            },
            "trial_runtime": {
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": {"from": "task_row"},
                        "workdir": {"from": "task_row"}
                    }
                },
                "agent": {
                    "command": harness_success_command(),
                    "artifact_type": "structured_json",
                    "mount": {
                        "source": bundle_root.to_string_lossy().to_string(),
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    },
                    "image": "python:3.11-slim",
                    "integration_level": integration_level,
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json"
                            }
                        },
                        "patch": {
                            "capture": {
                                "type": "workspace_diff",
                                "format": "unified_diff"
                            }
                        }
                    }
                },
                "execution": {"agent_site": "agent_container"},
                "grader": {"strategy": "none"}
            }
        });
        atomic_write_json_pretty(&run_dir.join("resolved_experiment.json"), &resolved)
            .expect("write resolved");
        let (variants, baseline_id) = resolve_variant_plan(&resolved).expect("variant plan");
        write_resolved_variants(run_dir, &resolved, &baseline_id, &variants)
            .expect("write resolved variants");
    }

    fn write_resolved_experiment_with_command(
        run_dir: &Path,
        integration_level: &str,
        command: Vec<String>,
    ) {
        let project_root = find_project_root(run_dir);
        let bundle_root = ensure_test_agent_bundle(&project_root, "agent-current");
        let resolved = json!({
            "experiment": { "id": "e", "name": "n" },
            "matrix": {
                "tasks": { "source": "file", "path": "tasks.jsonl" },
                "variants": [{ "id": "base", "baseline": true, "config": {} }],
                "repeats": 1
            },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "storage": { "backend": "local-fs" },
                "traces": { "backend": "local-stdout" },
                "network": { "task_sandbox": "none", "agent": "none" }
            },
            "policy": {
                "sanitization_profile": "hermetic_functional",
                "timeout_ms": 600000,
                "task_sandbox": {}
            },
            "trial_runtime": {
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": {"from": "task_row"},
                        "workdir": {"from": "task_row"}
                    }
                },
                "agent": {
                    "command": command,
                    "artifact_type": "structured_json",
                    "mount": {
                        "source": bundle_root.to_string_lossy().to_string(),
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    },
                    "image": "python:3.11-slim",
                    "integration_level": integration_level,
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json"
                            }
                        },
                        "patch": {
                            "capture": {
                                "type": "workspace_diff",
                                "format": "unified_diff"
                            }
                        }
                    }
                },
                "execution": {"agent_site": "agent_container"},
                "grader": {"strategy": "none"}
            }
        });
        atomic_write_json_pretty(&run_dir.join("resolved_experiment.json"), &resolved)
            .expect("write resolved");
        let (variants, baseline_id) = resolve_variant_plan(&resolved).expect("variant plan");
        write_resolved_variants(run_dir, &resolved, &baseline_id, &variants)
            .expect("write resolved variants");
    }

    fn write_task_row_dataset(path: &Path, task_id: &str) {
        fs::write(
            path,
            format!(
                concat!(
                    "{{\"schema_version\":\"task_row_v2\",",
                    "\"id\":\"{}\",",
                    "\"task\":{{\"id\":\"{}\"}},",
                    "\"runtime\":{{\"container_image\":{{\"image\":\"python:3.11-slim\",",
                    "\"workdir\":\"/workspace/task\"}}}}}}\n"
                ),
                task_id, task_id
            ),
        )
        .expect("task row dataset");
    }

    fn write_packaged_task_dataset(path: &Path, task_id: &str) {
        write_task_row_dataset(path, task_id);
    }

    fn seed_parent_trial(
        run_dir: &Path,
        trial_id: &str,
        checkpoints: Value,
        trial_status: &str,
        pause_label: Option<&str>,
    ) -> PathBuf {
        let trial_dir = run_dir.join("trials").join(trial_id);
        ensure_dir(&trial_dir).expect("trial dir");
        ensure_dir(&trial_dir.join("workspace")).expect("workspace");
        ensure_dir(&trial_dir.join("state")).expect("state");

        fs::write(
            trial_dir.join("workspace").join("fixture.txt"),
            "workspace fixture",
        )
        .expect("workspace fixture");
        let trial_input = json!({
            "schema_version": "agent_task_v1",
            "ids": { "trial_id": trial_id, "variant_id": "base", "task_id": "task_1", "repl_idx": 0 },
            "task": {
                "id": "task_1"
            },
            "ext": {
                "task_boundary": {
                    "environment": {
                        "image": "python:3.11-slim"
                    },
                    "workspace": {
                        "mode": "scratch",
                        "base": { "kind": "empty" },
                        "overlays": [],
                        "aux_mounts": []
                    },
                    "dependencies": {},
                    "limits": {}
                }
            },
            "bindings": {
                "existing": "value"
            },
            "runtime": {
                "paths": {
                    "workspace": trial_dir.join("workspace").to_string_lossy().to_string(),
                    "state": trial_dir.join("state").to_string_lossy().to_string(),
                    "out": trial_dir.join("out").to_string_lossy().to_string(),
                    "tmp": trial_dir.join("tmp").to_string_lossy().to_string()
                },
                "network": { "mode_requested": "none" }
            }
        });
        atomic_write_json_pretty(&trial_dir.join("trial_input.json"), &trial_input)
            .expect("trial input");

        let trial_output = json!({
                        "outcome": "success",
            "checkpoints": checkpoints
        });
        atomic_write_json_pretty(&trial_dir.join("result.json"), &trial_output)
            .expect("trial output");

        write_trial_state(
            &trial_dir,
            trial_id,
            trial_status,
            pause_label,
            pause_label,
            if trial_status == "paused" {
                Some("paused_by_user")
            } else {
                None
            },
        )
        .expect("trial state");

        let run_id = run_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("run")
            .to_string();
        let artifact_store = ArtifactStore::new(run_dir.join("artifacts"));
        let input_ref = artifact_store
            .put_bytes(&serde_json::to_vec_pretty(&trial_input).expect("trial input bytes"))
            .expect("input ref");
        let output_ref = artifact_store
            .put_bytes(&serde_json::to_vec_pretty(&trial_output).expect("trial output bytes"))
            .expect("output ref");
        let workspace_ref = artifact_store
            .put_bytes(b"workspace_placeholder")
            .expect("workspace ref");
        let checkpoint_labels = trial_output
            .get("checkpoints")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        row.get("logical_name")
                            .and_then(Value::as_str)
                            .or_else(|| row.get("path").and_then(Value::as_str))
                    })
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut store = BackingSqliteStore::open(run_dir).expect("store");
        store
            .upsert_attempt_object(AttemptObjectUpsert {
                run_id: &run_id,
                trial_id,
                schedule_idx: 0,
                attempt: 1,
                role: "trial_input",
                object_ref: &input_ref,
                metadata: None,
            })
            .expect("attempt input");
        store
            .upsert_attempt_object(AttemptObjectUpsert {
                run_id: &run_id,
                trial_id,
                schedule_idx: 0,
                attempt: 1,
                role: "trial_output",
                object_ref: &output_ref,
                metadata: None,
            })
            .expect("attempt output");
        store
            .upsert_json_row(
                JsonRowTable::ChainState,
                &json!({
                    "schema_version": "task_chain_state_v1",
                    "run_id": run_id,
                    "chain_id": "base::task_1",
                    "step_index": 0,
                    "ids": {
                        "trial_id": trial_id,
                        "variant_id": "base",
                        "task_id": "task_1",
                        "repl_idx": 0
                    },
                    "snapshots": {
                        "chain_root_ref": workspace_ref,
                        "prev_ref": workspace_ref,
                        "post_ref": workspace_ref
                    },
                    "diffs": {
                        "incremental_ref": output_ref,
                        "cumulative_ref": output_ref,
                        "patch_incremental_ref": output_ref,
                        "patch_cumulative_ref": output_ref
                    },
                    "checkpoint_labels": checkpoint_labels,
                    "ext": {
                        "latest_workspace_ref": workspace_ref
                    },
                    "schedule_idx": 0,
                    "attempt": 1,
                    "row_seq": 0,
                    "slot_commit_id": "seed_slot_commit"
                }),
            )
            .expect("seed lineage");

        trial_dir
    }

    fn active_control_for_trial(trial_dir: &Path) -> ActiveRuntimeControl {
        let control_path = trial_dir.join("state").join("lab_control.json");
        let payload = json!({
            "schema_version": "control_plane_v1",
            "seq": 0,
            "action": "continue",
            "label": null,
            "requested_at": Utc::now().to_rfc3339(),
            "requested_by": "run_loop",
        });
        atomic_write_json_pretty(&control_path, &payload).expect("control file");
        ActiveRuntimeControl {
            runtime_id: BUILTIN_COMMAND_RUNTIME_ID.to_string(),
            runtime_version: BUILTIN_COMMAND_RUNTIME_VERSION.to_string(),
            command_path: control_path.to_string_lossy().to_string(),
            events_path: Some(
                trial_dir
                    .join("state")
                    .join("events.jsonl")
                    .to_string_lossy()
                    .to_string(),
            ),
        }
    }

    fn write_test_run_control(
        run_dir: &Path,
        run_id: &str,
        status: &str,
        active_trial_id: Option<&str>,
        active_control: Option<&ActiveRuntimeControl>,
    ) {
        let active_trials = active_trial_id
            .map(|trial_id| {
                vec![RunControlActiveTrial {
                    trial_id: trial_id.to_string(),
                    worker_id: "worker_1".to_string(),
                    schedule_idx: None,
                    variant_id: "base".to_string(),
                    started_at: Some(Utc::now().to_rfc3339()),
                    control: active_control.cloned(),
                }]
            })
            .unwrap_or_default();
        write_run_control(run_dir, run_id, status, &active_trials, None).expect("run control");
    }

    fn write_run_control_without_run_id(run_dir: &Path, status: &str) {
        let payload = json!({
            "schema_version": "run_control_v2",
            "status": status,
            "active_trials": {},
            "pause": Value::Null,
            "updated_at": Utc::now().to_rfc3339(),
        });
        let mut store = BackingSqliteStore::open(run_dir).expect("open sqlite store");
        store
            .put_runtime_json(RUNTIME_KEY_RUN_CONTROL, &payload)
            .expect("run control without id");
    }

    fn seed_continuable_container_run(prefix: &str) -> (TempDirGuard, PathBuf) {
        let (root, run_dir) = create_run_dir(prefix, "run_1");
        write_resolved_experiment_with_command(
            &run_dir,
            "cli_basic",
            harness_success_output_command(),
        );
        write_packaged_task_dataset(&run_dir.join("tasks.jsonl"), "task_1");
        write_test_run_control(&run_dir, "run_1", "paused", None, None);

        let resolved = load_json_file(&run_dir.join("resolved_experiment.json")).expect("resolved");
        let schedule = build_trial_schedule(
            1,
            1,
            1,
            parse_policies(&resolved).unwrap().scheduling,
            experiment_random_seed(&resolved),
        );
        let schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v1".to_string(),
            run_id: "run_1".to_string(),
            total_slots: schedule.len(),
            next_schedule_index: 0,
            next_trial_index: 0,
            schedule,
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        write_schedule_progress(&run_dir, &schedule_progress).expect("schedule progress");
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");

        (root, run_dir)
    }

    fn load_sqlite_json_row(run_dir: &Path, table: &str, run_id: &str) -> Value {
        let conn = rusqlite::Connection::open(account_sqlite_path_for_run(run_dir).unwrap())
            .expect("open sqlite");
        let sql = format!(
            "SELECT row_json FROM {} WHERE run_id=?1 ORDER BY schedule_idx, attempt, row_seq LIMIT 1",
            table
        );
        let raw: String = conn
            .query_row(&sql, [run_id], |row| row.get(0))
            .expect("row json");
        serde_json::from_str(&raw).expect("decode row json")
    }

    fn create_trial_paths_fixture(prefix: &str) -> (TempDirGuard, TrialPaths) {
        let root = TempDirGuard::new(prefix);
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        fs::write(exp_dir.join("README.md"), "fixture").expect("exp fixture");
        write_package_lock_fixture(&exp_dir, TEST_PACKAGE_DIGEST);
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial");
        let paths = TrialPaths::new(&trial_dir, &exp_dir).expect("trial paths");
        paths.prepare(true).expect("prepare");
        (root, paths)
    }

    fn write_package_lock_fixture(package_root: &Path, digest: &str) {
        atomic_write_json_pretty(
            &package_root.join("package.lock"),
            &json!({
                "schema_version": "sealed_package_lock_v1",
                "package_digest": digest
            }),
        )
        .expect("package lock");
    }

    fn base_variant_fixture() -> Variant {
        Variant {
            id: "base".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        }
    }

    fn task_runtime_experiment_fixture(timeout_ms: u64) -> Value {
        json!({
            "runtime": { "network": { "task_sandbox": "none" } },
            "trial_runtime": {
                "task": { "interface": "writable_workspace" },
                "agent": { "artifact_type": "structured_json" }
            },
            "policy": { "timeout_ms": timeout_ms }
        })
    }

    #[test]
    fn host_side_grader_input_materialization_rejects_non_contract_path() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_grader_input_contract_path");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({});
        let target = root.path.join("outside_contract_input.json");
        let target_raw = target.to_string_lossy().to_string();
        let grader = GraderConfig {
            strategy: GradingStrategy::Host,
            command: vec!["true".to_string()],
            max_concurrency: None,
            in_task_runtime: None,
            injected: None,
            separate: None,
            host: None,
            inputs: BTreeMap::from([(
                "answer".to_string(),
                RuntimeInputConfig {
                    source: RuntimeTransportSourceConfig {
                        output: None,
                        field: None,
                        case: Some("/answer".to_string()),
                        task: None,
                        object: None,
                    },
                    materialize: RuntimeInputMaterializeConfig {
                        as_kind: "json_file".to_string(),
                        path: Some(target_raw.clone()),
                        name: None,
                    },
                    required: true,
                },
            )]),
            outputs: BTreeMap::new(),
        };
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let err = materialize_grader_inputs_for_test(
            &request,
            &paths.trial_dir,
            &grader,
            &json!({"answer": "ok"}),
        )
        .expect_err("off-contract grader input materialization path should fail");

        assert!(
            err.to_string()
                .contains("must target a trial contract path"),
            "unexpected error: {err}"
        );
        assert!(
            !target.exists(),
            "off-contract grader input path must not be written: {}",
            target.display()
        );
    }

    #[test]
    fn trial_paths_drop_cleans_scratch_without_explicit_cleanup() {
        let root = TempDirGuard::new("bucephalus_trial_paths_drop_cleanup");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        fs::write(exp_dir.join("README.md"), "fixture").expect("exp fixture");
        assert!(
            TrialPaths::new(Path::new("/"), &exp_dir)
                .unwrap_err()
                .to_string()
                .contains("trial directory has no trial id")
        );
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");

        let scratch_dir = {
            let paths = TrialPaths::new(&trial_dir, &exp_dir).expect("trial paths");
            paths.prepare(true).expect("prepare");
            fs::write(paths.out.join("result.json"), "{}").expect("write result");
            assert!(
                paths.scratch_dir.exists(),
                "scratch dir should exist while trial paths live"
            );
            paths.scratch_dir.clone()
        };

        assert!(
            !scratch_dir.exists(),
            "trial path drop should cleanup scratch dir: {}",
            scratch_dir.display()
        );
    }

    #[test]
    fn contract_path_mapper_resolves_container_contract_paths() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_contract_mapper_container");
        let cases = vec![
            (
                format!("{}/trial_input.json", BUCEPHALUS_CONTRACT_IN_DIR),
                paths.in_dir.join("trial_input.json"),
            ),
            (
                format!("{}/result.json", BUCEPHALUS_CONTRACT_OUT_DIR),
                paths.out.join("result.json"),
            ),
        ];
        for (raw, expected) in cases {
            let resolved = map_container_path_to_host(&raw, &paths).expect("resolve path");
            assert_eq!(resolved, expected, "path mismatch for {}", raw);
        }

        let err = map_container_path_to_host("/stateful/not_state", &paths).expect_err("reject");
        assert!(
            err.to_string().contains("unsupported container mount path"),
            "unexpected error: {}",
            err
        );

        let err = map_container_path_to_host("/state/events.jsonl", &paths).expect_err("reject");
        assert!(
            err.to_string().contains("unsupported container mount path"),
            "unexpected error: {}",
            err
        );

        let err = map_container_path_to_host(
            &format!("{}/events.jsonl", BUCEPHALUS_CONTRACT_STATE_DIR),
            &paths,
        )
        .expect_err("state is not a container mount root");
        assert!(
            err.to_string().contains("unsupported container mount path"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn contract_path_mapper_enforces_mode_specific_paths() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_contract_mapper_modes");
        let staged_support = task_workdir_support_destination_path("pkg.json");
        let resolved = resolve_trial_io_host_path(&staged_support, &paths).expect("support file");
        assert_eq!(
            resolved,
            paths
                .workspace
                .join(BUCEPHALUS_RUNNER_SUPPORT_REL_DIR)
                .join("pkg.json")
        );

        let err = resolve_trial_io_host_path("/unknown/pkg.json", &paths).expect_err("reject");
        assert!(
            err.to_string().contains("unsupported container mount path"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn contract_path_mapper_rejects_legacy_dataset_runtime_io_paths_in_container_mode() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_contract_mapper_dataset_legacy");
        let err = resolve_trial_io_host_path("/dataset/tasks.jsonl", &paths)
            .expect_err("reject legacy dataset runtime io path");
        assert!(
            err.to_string().contains("unsupported container mount path"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn contract_path_mapper_resolves_event_paths_and_rejects_invalid_roots() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_contract_mapper_events");
        let trial_dir = paths.in_dir.parent().expect("trial dir").to_path_buf();

        let in_path = format!("{}/trial_input.json", BUCEPHALUS_CONTRACT_IN_DIR);
        let resolved_in = resolve_event_path_for_trial(&in_path, &trial_dir).expect("in path");
        assert_eq!(resolved_in, trial_dir.join("in").join("trial_input.json"));

        let err = resolve_event_path_for_trial("/dataset/tasks.jsonl", &trial_dir)
            .expect_err("reject legacy dataset path");
        assert!(
            err.to_string()
                .contains("unsupported runtime event path for trial"),
            "unexpected error: {}",
            err
        );

        let err = resolve_event_path_for_trial("/harness/logs/events.jsonl", &trial_dir)
            .expect_err("reject");
        assert!(
            err.to_string()
                .contains("unsupported runtime event path for trial"),
            "unexpected error: {}",
            err
        );
    }

    fn write_state_inventory_fixture(
        paths: &TrialPaths,
        experiment: &Value,
        runtime: &AgentRuntimeConfig,
        effective_network_mode: &str,
    ) -> Result<()> {
        write_state_inventory(StateInventoryInput {
            trial_dir: &paths.trial_dir,
            json_value: experiment,
            agent_runtime: runtime,
            secret_file_mounts: &[],
            exec_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            effective_network_mode,
            invocation_source: "runtime_agent",
            task_sandbox_image: Some("img:latest"),
            task_sandbox_workdir: "/workspace/task",
        })
    }

    #[test]
    fn write_state_inventory_container_excludes_legacy_dataset_mount() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_state_inventory_container");
        let runtime = legacy_contract_runtime_fixture();
        let experiment = json!({
            "version": "0.3",
            "design": { "sanitization_profile": "hermetic_functional" },
            "runtime": { "network": { "task_sandbox": "none" } }
        });
        write_state_inventory_fixture(&paths, &experiment, &runtime, "none")
            .expect("write state inventory");
        let inventory = load_json_file(&trial_state_inventory_path(&paths.trial_dir))
            .expect("load state inventory");
        let mounts = inventory
            .pointer("/mounts")
            .and_then(|v| v.as_array())
            .expect("mounts");
        let names = mounts
            .iter()
            .filter_map(|row| row.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["in", "workdir", "out", "tmp"]);
        assert!(
            !mounts.iter().any(|row| {
                row.get("path").and_then(|v| v.as_str()) == Some("/dataset")
                    || row.get("name").and_then(|v| v.as_str()) == Some("dataset")
            }),
            "legacy dataset mount unexpectedly present: {:?}",
            mounts
        );
        assert!(inventory.pointer("/planes/agent_runtime").is_some());
        assert!(inventory.pointer("/planes/task_sandbox").is_some());
        let agent_runtime_mounts = inventory
            .pointer("/planes/agent_runtime/mounts")
            .and_then(|v| v.as_array())
            .expect("agent runtime mounts");
        let task_sandbox_mounts = inventory
            .pointer("/planes/task_sandbox/mounts")
            .and_then(|v| v.as_array())
            .expect("task sandbox mounts");
        assert!(
            !agent_runtime_mounts
                .iter()
                .any(|row| { row.get("name").and_then(|v| v.as_str()) == Some("deps") }),
            "agent runtime deps mount unexpectedly present: {:?}",
            agent_runtime_mounts
        );
        assert!(
            !task_sandbox_mounts
                .iter()
                .any(|row| { row.get("name").and_then(|v| v.as_str()) == Some("deps") }),
            "task sandbox deps mount unexpectedly present: {:?}",
            task_sandbox_mounts
        );
    }

    #[test]
    fn write_state_inventory_local_excludes_legacy_dataset_mount() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_state_inventory_local");
        let runtime = legacy_contract_runtime_fixture();
        let experiment = json!({
            "version": "0.3",
            "design": { "sanitization_profile": "hermetic_functional" },
            "runtime": { "network": { "task_sandbox": "none" } }
        });
        write_state_inventory_fixture(&paths, &experiment, &runtime, "full")
            .expect("write state inventory");
        let inventory = load_json_file(&trial_state_inventory_path(&paths.trial_dir))
            .expect("load state inventory");
        let mounts = inventory
            .pointer("/mounts")
            .and_then(|v| v.as_array())
            .expect("mounts");
        let names = mounts
            .iter()
            .filter_map(|row| row.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["in", "workdir", "out", "tmp"]);
        assert_eq!(
            inventory
                .pointer("/network/enforcement_effective")
                .and_then(Value::as_str),
            Some("not_enforced")
        );
        assert!(
            !mounts
                .iter()
                .any(|row| row.get("name").and_then(|v| v.as_str()) == Some("dataset")),
            "legacy dataset mount unexpectedly present: {:?}",
            mounts
        );
        assert!(inventory.pointer("/planes/agent_runtime").is_some());
        assert!(inventory.pointer("/planes/task_sandbox").is_some());
    }

    #[test]
    fn write_state_inventory_container_reports_agent_bundle_mount_when_present() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_state_inventory_bundle");
        let mut runtime = legacy_contract_runtime_fixture();
        let bundle_dir = root.path.join("agent_bundle");
        ensure_dir(&bundle_dir).expect("bundle dir");
        runtime.agent_artifact = Some(bundle_dir);
        runtime.agent_artifact_mount_path = Some("/opt/agent".to_string());
        let experiment = json!({
            "version": "0.3",
            "design": { "sanitization_profile": "hermetic_functional" },
            "runtime": { "network": { "task_sandbox": "none" } }
        });
        write_state_inventory_fixture(&paths, &experiment, &runtime, "none")
            .expect("write state inventory");
        let inventory = load_json_file(&trial_state_inventory_path(&paths.trial_dir))
            .expect("load state inventory");
        let agent_runtime_mounts = inventory
            .pointer("/planes/agent_runtime/mounts")
            .and_then(|v| v.as_array())
            .expect("agent runtime mounts");
        assert!(
            agent_runtime_mounts
                .iter()
                .any(|row| row.get("name").and_then(|v| v.as_str()) == Some("agent_bundle")),
            "agent bundle mount should be present when runtime.agent.bundle is configured: {:?}",
            agent_runtime_mounts
        );
    }

    #[test]
    fn write_state_inventory_requires_explicit_task_network_and_agent_command() {
        let (_root, paths) =
            create_trial_paths_fixture("bucephalus_state_inventory_network_required");
        let mut runtime = legacy_contract_runtime_fixture();
        let experiment = json!({
            "version": "0.3",
            "design": { "sanitization_profile": "hermetic_functional" }
        });

        let err = write_state_inventory_fixture(&paths, &experiment, &runtime, "none")
            .expect_err("state inventory must not invent a task network mode");
        assert!(
            err.to_string()
                .contains("missing /runtime/network/task_sandbox"),
            "unexpected error: {}",
            err
        );
        runtime.command_raw.clear();
        let experiment = json!({
            "version": "0.3",
            "runtime": { "network": { "task_sandbox": "none" } }
        });

        let err = write_state_inventory_fixture(&paths, &experiment, &runtime, "none")
            .expect_err("state inventory must not invent harness identity");
        assert!(
            err.to_string()
                .contains("state inventory requires non-empty agent command"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn prepared_task_environment_manifest_requires_task_sandbox_plan() {
        let manifest = PreparedTaskEnvironmentManifest {
            schema_version: "prepared_task_environment_v1".to_string(),
            declaration: json!({}),
            declaration_digest: "sha256:test".to_string(),
            run_id: "run_1".to_string(),
            trial_id: "trial_1".to_string(),
            variant_id: "base".to_string(),
            task_id: "task_1".to_string(),
            task_index: 0,
            repl_idx: 0,
            task_image: "python:3.11-slim".to_string(),
            workspace_root: "/tmp/workspace".to_string(),
            aux_mounts: Vec::new(),
            output_mounts: Vec::new(),
            contract_files: PreparedContractFilePaths {
                trial_input: "/bucephalus/in/trial_input.json".to_string(),
                result: "/bucephalus/out/result.json".to_string(),
                mapped_grader_output: "/bucephalus/out/mapped_grader_output.json".to_string(),
                trajectory: "/bucephalus/out/trajectory.jsonl".to_string(),
            },
            runtime_env: BTreeMap::new(),
            prepared_runtime_image: None,
            task_sandbox_plan: None,
        };
        let err = manifest
            .validate()
            .expect_err("prepared manifests must not fall back to legacy task workdir fields");
        assert!(
            err.to_string().contains("missing required task_sandbox_plan"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn prepared_task_environment_uses_prepared_runtime_image_map() {
        let _lock = lock_runtime_control_tests();
        let _prepared_map_env =
            EnvVarGuard::set(&[("BUCEPHALUS_PREPARED_RUNTIME_IMAGE_MAP", None)]);
        let root = TempDirGuard::new("bucephalus_prepared_runtime_image_map");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        let artifact = root.path.join("agent-linux-x64.tar.gz");
        fs::write(&artifact, b"agent-runtime").expect("agent artifact");
        let artifact_digest = sha256_file(&artifact).expect("artifact digest");
        let map_path = root
            .path
            .join(PREPARED_RUNTIME_IMAGE_MAP_PACKAGE_REL_PATH);
        ensure_dir(map_path.parent().expect("map parent")).expect("map parent");
        fs::write(
            &map_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "prepared_runtime_image_map_v1",
                "generated_at": "2026-01-01T00:00:00Z",
                "entries": [
                    {
                        "base_image": "python:3.11-slim",
                        "agent_artifact_digest": artifact_digest,
                        "agent_artifact_mount_path": "/opt/agent",
                        "runner_contract_version": PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION,
                        "prepared_image": "ghcr.io/acme/python-agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    }
                ]
            }))
            .expect("map json"),
        )
        .expect("write map");

        let mut runtime = legacy_contract_runtime_fixture();
        runtime.agent_artifact = Some(artifact);
        runtime.agent_artifact_mount_path = Some("/opt/agent".to_string());
        runtime.agent_artifact_digest = Some(artifact_digest.clone());
        runtime.agent_artifact_read_only = true;
        let variant = Variant {
            id: "base".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let task_boundary = runtime_task_boundary(
            json!({"id": "task_1"}),
            "python:3.11-slim",
            "/workspace/task",
            Some(30_000),
        );

        let prepared = prepare_task_environment_for_test(
            &root.path,
            &trial_dir,
            &json!({
                "runtime": { "network": { "task_sandbox": "none" } },
                "trial_runtime": {
                    "task": { "interface": "writable_workspace" },
                    "agent": { "artifact_type": "structured_json" }
                }
            }),
            &variant,
            &task_boundary,
            &runtime,
        )
        .expect("prepare task environment");
        let plan = prepared
            .manifest
            .task_sandbox_plan
            .as_ref()
            .expect("task sandbox plan");

        assert_eq!(
            prepared.manifest.task_image,
            "python:3.11-slim",
            "manifest task_image should preserve the authored/base task image"
        );
        assert_eq!(
            plan.image,
            "ghcr.io/acme/python-agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(
            plan.artifact_mount.is_none(),
            "prepared images already contain the agent runtime and must not re-mount it"
        );
        let prepared_image = prepared
            .manifest
            .prepared_runtime_image
            .as_ref()
            .expect("prepared runtime image metadata");
        assert_eq!(prepared_image.base_image, "python:3.11-slim");
        assert_eq!(prepared_image.agent_artifact_digest, artifact_digest);
        assert_eq!(prepared_image.agent_artifact_mount_path, "/opt/agent");
        prepared
            .manifest
            .validate()
            .expect("prepared manifest validates");
    }

    #[test]
    fn prepared_task_environment_env_map_overrides_package_map() {
        let _lock = lock_runtime_control_tests();
        let root = TempDirGuard::new("bucephalus_prepared_runtime_image_env_override");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        let artifact = root.path.join("agent-linux-x64.tar.gz");
        fs::write(&artifact, b"agent-runtime").expect("agent artifact");
        let artifact_digest = sha256_file(&artifact).expect("artifact digest");
        let package_map_path = root
            .path
            .join(PREPARED_RUNTIME_IMAGE_MAP_PACKAGE_REL_PATH);
        ensure_dir(package_map_path.parent().expect("map parent")).expect("map parent");
        fs::write(
            &package_map_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "prepared_runtime_image_map_v1",
                "generated_at": "2026-01-01T00:00:00Z",
                "entries": [{
                    "base_image": "python:3.11-slim",
                    "agent_artifact_digest": artifact_digest,
                    "agent_artifact_mount_path": "/opt/agent",
                    "runner_contract_version": PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION,
                    "prepared_image": "ghcr.io/acme/package-map@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                }]
            }))
            .expect("package map json"),
        )
        .expect("write package map");
        let override_map_path = root.path.join("override-prepared-runtime-images.json");
        fs::write(
            &override_map_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "prepared_runtime_image_map_v1",
                "generated_at": "2026-01-01T00:00:00Z",
                "entries": [{
                    "base_image": "python:3.11-slim",
                    "agent_artifact_digest": artifact_digest,
                    "agent_artifact_mount_path": "/opt/agent",
                    "runner_contract_version": PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION,
                    "prepared_image": "ghcr.io/acme/env-map@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }]
            }))
            .expect("override map json"),
        )
        .expect("write override map");
        let _env = EnvVarGuard::set(&[(
            "BUCEPHALUS_PREPARED_RUNTIME_IMAGE_MAP",
            Some(override_map_path.to_string_lossy().as_ref()),
        ), (
            "BUCEPHALUS_TEST_USE_PREPARED_RUNTIME_IMAGE_MAP_ENV",
            Some("1"),
        )]);

        let mut runtime = legacy_contract_runtime_fixture();
        runtime.agent_artifact = Some(artifact);
        runtime.agent_artifact_mount_path = Some("/opt/agent".to_string());
        runtime.agent_artifact_digest = Some(artifact_digest);
        let variant = Variant {
            id: "base".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let task_boundary = runtime_task_boundary(
            json!({"id": "task_1"}),
            "python:3.11-slim",
            "/workspace/task",
            Some(30_000),
        );

        let prepared = prepare_task_environment_for_test(
            &root.path,
            &trial_dir,
            &json!({
                "runtime": {
                    "network": {
                        "task_sandbox": "none"
                    }
                },
                "trial_runtime": {
                    "task": {"interface": "writable_workspace"},
                    "agent": {"artifact_type": "structured_json"}
                }
            }),
            &variant,
            &task_boundary,
            &runtime,
        )
        .expect("prepare task environment");
        assert_eq!(
            prepared
                .manifest
                .task_sandbox_plan
                .as_ref()
                .expect("task sandbox plan")
                .image,
            "ghcr.io/acme/env-map@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }

    #[test]
    fn prepare_runtime_images_dry_run_emits_map_for_unique_task_agent_pairs() {
        let _lock = lock_runtime_control_tests();
        let root = create_dx_authoring_fixture("bucephalus_prepare_runtime_images");
        let data_dir = root.path.join(".lab").join("experiments").join("data");
        fs::write(
            data_dir.join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"task_row_v2","id":"TASK001","task":{"id":"TASK001"},"runtime":{"container_image":{"image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n",
                r#"{"schema_version":"task_row_v2","id":"TASK002","task":{"id":"TASK002"},"runtime":{"container_image":{"image":"python:3.12-slim","workdir":"/workspace/task"}}}"#,
                "\n",
                r#"{"schema_version":"task_row_v2","id":"TASK003","task":{"id":"TASK003"},"runtime":{"container_image":{"image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("dataset rows");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        let report = prepare_runtime_images(
            &build.package_dir,
            PreparedRuntimeImageOptions {
                repository: "ghcr.io/acme/bucephalus-prepared".to_string(),
                out: None,
                push: false,
                dry_run: true,
                skip_existing: true,
            },
        )
        .expect("prepare runtime image map");

        assert_eq!(report.built, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.entries.len(), 2, "duplicate task images should group");
        assert!(report
            .entries
            .iter()
            .all(|entry| entry.runner_contract_version
                == PREPARED_RUNTIME_IMAGE_CONTRACT_VERSION));
        assert!(report.entries.iter().all(|entry| entry
            .prepared_image
            .starts_with("ghcr.io/acme/bucephalus-prepared:prepared-")));
        assert_eq!(
            report.map_path,
            build
                .package_dir
                .join(PREPARED_RUNTIME_IMAGE_MAP_PACKAGE_REL_PATH)
        );
        let map = load_prepared_runtime_image_map_for_test(&report.map_path).expect("load map");
        assert_eq!(map.schema_version, "prepared_runtime_image_map_v1");
        assert_eq!(map.entries.len(), 2);
        assert_eq!(
            map.entries
                .iter()
                .map(|entry| entry.base_image.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["python:3.11-slim", "python:3.12-slim"])
        );
    }

    #[test]
    fn prepared_task_environment_schema_accepts_case_materialization_plan() {
        let manifest = json!({
            "schema_version": "prepared_task_environment_v1",
            "declaration": {
                "schema_version": "case_v2",
                "id": "case_v2_setup"
            },
            "declaration_digest": "sha256:test",
            "run_id": "run_1",
            "trial_id": "trial_1",
            "variant_id": "base",
            "task_id": "case_v2_setup",
            "task_index": 0,
            "repl_idx": 0,
            "task_image": "python:3.11-slim",
            "workspace_root": "/tmp/workspace",
            "aux_mounts": [],
            "output_mounts": [],
            "contract_files": {
                "trial_input": "/bucephalus/in/trial_input.json",
                "result": "/bucephalus/out/result.json",
                "mapped_grader_output": "/bucephalus/out/mapped_grader_output.json",
                "trajectory": "/bucephalus/out/trajectory.jsonl"
            },
            "runtime_env": {},
            "task_sandbox_plan": {
                "image": "python:3.11-slim",
                "workdir": "/workspace/task",
                "materialization": { "kind": "task_image" },
                "case_materialization": [
                    {
                        "id": "setup",
                        "stage": "case",
                        "operation": "command",
                        "command": ["bash", "-lc", "true"],
                        "network": "none"
                    }
                ],
                "io_mounts": {
                    "in_dir": "/bucephalus/in",
                    "out_dir": "/bucephalus/out",
                    "telemetry_mounts": []
                },
                "network_mode": "none",
                "time_limit_ms": 600000
            }
        });
        let schema = compile_schema("prepared_task_environment_v1.jsonschema").expect("schema");
        if let Err(errors) = schema.validate(&manifest) {
            let messages = errors.map(|err| err.to_string()).collect::<Vec<_>>();
            panic!(
                "prepared_task_environment schema should accept carried case materialization: {}",
                messages.join(" | ")
            );
        };
    }

    #[test]
    fn prepared_task_environment_schema_accepts_prepared_runtime_image() {
        let manifest = json!({
            "schema_version": "prepared_task_environment_v1",
            "declaration": {
                "schema_version": "case_v2",
                "id": "case_v2_prepared"
            },
            "declaration_digest": "sha256:test",
            "run_id": "run_1",
            "trial_id": "trial_1",
            "variant_id": "base",
            "task_id": "case_v2_prepared",
            "task_index": 0,
            "repl_idx": 0,
            "task_image": "python:3.11-slim",
            "workspace_root": "/tmp/workspace",
            "aux_mounts": [],
            "output_mounts": [],
            "contract_files": {
                "trial_input": "/bucephalus/in/trial_input.json",
                "result": "/bucephalus/out/result.json",
                "mapped_grader_output": "/bucephalus/out/mapped_grader_output.json",
                "trajectory": "/bucephalus/out/trajectory.jsonl"
            },
            "runtime_env": {},
            "prepared_runtime_image": {
                "image": "ghcr.io/acme/python-agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "base_image": "python:3.11-slim",
                "agent_artifact_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "agent_artifact_mount_path": "/opt/agent",
                "runner_contract_version": "bucephalus_prepared_runtime_image_v1"
            },
            "task_sandbox_plan": {
                "image": "ghcr.io/acme/python-agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "workdir": "/workspace/task",
                "materialization": { "kind": "task_image" },
                "io_mounts": {
                    "in_dir": "/bucephalus/in",
                    "out_dir": "/bucephalus/out",
                    "telemetry_mounts": []
                },
                "network_mode": "none",
                "time_limit_ms": 600000
            }
        });
        let schema = compile_schema("prepared_task_environment_v1.jsonschema").expect("schema");
        if let Err(errors) = schema.validate(&manifest) {
            let messages = errors.map(|err| err.to_string()).collect::<Vec<_>>();
            panic!(
                "prepared_task_environment schema should accept prepared runtime images: {}",
                messages.join(" | ")
            );
        };
    }

    #[test]
    fn run_session_state_roundtrip_normalizes_execution_options() {
        let (_root, run_dir) = create_run_dir("bucephalus_run_session_state", "run_1");
        let behavior = RunBehavior {
            network_mode_override: Some("full".to_string()),
            require_network_none: false,
            smoke_test: false,
        };
        let execution = RunExecutionOptions {
            executor: Some(ExecutorKind::LocalDocker),
            materialize: None,
            run_root: None,
            runtime_env: BTreeMap::new(),
            runtime_env_files: Vec::new(),
            secret_files: BTreeMap::new(),
            stdout_progress: false,
        };
        write_run_session_state(&run_dir, "run_1", &behavior, &execution).expect("write state");
        let state = load_run_session_state(&run_dir).expect("load state");
        assert_eq!(state.schema_version, "run_session_state_v1");
        assert_eq!(state.run_id, "run_1");
        assert_eq!(
            state.behavior.network_mode_override.as_deref(),
            Some("full")
        );
        assert_eq!(state.execution.executor, Some(ExecutorKind::LocalDocker));
        assert_eq!(
            state.execution.materialize,
            Some(MaterializationMode::OutputsOnly)
        );
    }

    #[test]
    fn continue_run_accepts_paused_and_interrupted_terminal_statuses() {
        for status in ["paused", "interrupted"] {
            let (_root, run_dir) = create_run_dir("bucephalus_continue_statuses", "run_1");
            write_test_run_control(&run_dir, "run_1", status, None, None);

            let err =
                continue_run(&run_dir).expect_err("continue should reach run session state load");
            assert!(
                err.to_string()
                    .contains("run_session_state_v1 not found in runtime store"),
                "status {} produced unexpected error: {}",
                status,
                err
            );
        }
    }

    #[test]
    fn continue_run_uses_persisted_behavior() {
        let (_root, run_dir) = create_run_dir("bucephalus_continue_persisted_behavior", "run_1");
        let dataset_path = run_dir.join("tasks.jsonl");
        write_packaged_task_dataset(&dataset_path, "task_1");
        let mut resolved = current_trial_runtime_experiment_base();
        resolved["experiment"] = json!({ "id": "e", "name": "n" });
        resolved["matrix"]["tasks"] = json!({
            "source": "file",
            "path": "tasks.jsonl",
            "suite_id": "s",
            "split_id": "dev",
            "limit": 1
        });
        resolved["scheduling"] = json!({
            "comparison": "paired",
            "random_seed": 1,
            "shuffle_tasks": false,
            "max_concurrency": 1
        });
        resolved["policy"]["sanitization_profile"] = json!("standard_runtime");
        resolved["trial_runtime"]["agent"]["command"] = json!(harness_success_command());
        resolved["runtime"]["network"]["agent"] = json!("full");
        resolved["runtime"]["network"]["task_sandbox"] = json!("full");
        atomic_write_json_pretty(&run_dir.join("resolved_experiment.json"), &resolved)
            .expect("resolved");
        write_test_run_control(&run_dir, "run_1", "failed", None, None);
        let schedule =
            build_trial_schedule(1, 1, 1, parse_policies(&resolved).unwrap().scheduling, 1);
        let schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v1".to_string(),
            run_id: "run_1".to_string(),
            total_slots: schedule.len(),
            next_schedule_index: 0,
            next_trial_index: 0,
            schedule,
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        write_schedule_progress(&run_dir, &schedule_progress).expect("progress");
        let behavior = RunBehavior {
            network_mode_override: None,
            require_network_none: true,
            smoke_test: false,
        };
        write_run_session_state(&run_dir, "run_1", &behavior, &container_execution())
            .expect("run session");

        let err = continue_run(&run_dir).expect_err("continue should honor persisted behavior");
        assert!(
            err.to_string()
                .contains("strict run requires network mode 'none'"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn continue_run_e2e_executes_container_trial_and_persists_runtime_state() {
        let _runtime_guard = lock_runtime_control_tests();
        let (_root, run_dir) = seed_continuable_container_run("bucephalus_continue_e2e_runtime");

        let result = continue_run(&run_dir).expect("continue run");
        assert_eq!(result.run_id, "run_1");

        let control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        let trial_dir = run_dir.join("trials").join("trial_1");
        let trial_state = load_json_file(&trial_state_path(&trial_dir)).expect("trial state");
        if control.pointer("/status").and_then(Value::as_str) != Some("completed") {
            panic!("control={:?} trial_state={:?}", control, trial_state);
        }

        let schedule_progress = load_schedule_progress(&run_dir).expect("schedule progress");
        let trial_output =
            load_json_file(&trial_agent_dir(&trial_dir).join("result.json")).expect("trial output");
        assert_eq!(schedule_progress.next_schedule_index, 1);
        assert_eq!(schedule_progress.completed_slots.len(), 1);
        assert_eq!(schedule_progress.completed_slots[0].schedule_index, 0);
        assert_eq!(schedule_progress.completed_slots[0].trial_id, "trial_1");
        if schedule_progress.completed_slots[0].status != "completed" {
            panic!(
                "slot={:?} trial_state={:?} trial_output={:?}",
                schedule_progress.completed_slots[0], trial_state, trial_output
            );
        }
        assert_eq!(schedule_progress.completed_slots[0].attempt, 1);
        assert!(
            !schedule_progress.completed_slots[0]
                .slot_commit_id
                .is_empty(),
            "slot commit id should be persisted"
        );

        let store = BackingSqliteStore::open(&run_dir).expect("store");
        assert_eq!(store.row_count("evidence_rows").expect("evidence count"), 1);
        assert_eq!(
            store
                .row_count("chain_state_rows")
                .expect("chain state count"),
            1
        );
        assert!(
            store
                .latest_attempt_object_ref("run_1", "trial_1", "trial_input")
                .expect("trial_input ref")
                .is_some(),
            "trial input should be persisted into attempt objects"
        );
        assert!(
            store
                .latest_attempt_object_ref("run_1", "trial_1", "trial_output")
                .expect("trial_output ref")
                .is_some(),
            "trial output should be persisted into attempt objects"
        );
        assert!(
            store
                .has_lineage_for_trial("run_1", "trial_1")
                .expect("lineage"),
            "chain state should materialize lineage rows"
        );
        assert!(
            !run_dir
                .join("runtime")
                .join("worker_payload")
                .join("trial_1")
                .exists(),
            "worker payload spool should be cleaned up after commit"
        );
    }

    #[test]
    fn continue_run_e2e_commits_slot_identity_to_sqlite() {
        let _runtime_guard = lock_runtime_control_tests();
        let (_root, run_dir) = seed_continuable_container_run("bucephalus_continue_e2e_sqlite");

        continue_run(&run_dir).expect("continue run");

        let evidence = load_sqlite_json_row(&run_dir, "evidence_rows", "run_1");
        assert_eq!(
            evidence.pointer("/run_id").and_then(Value::as_str),
            Some("run_1")
        );
        assert_eq!(
            evidence.pointer("/schedule_idx").and_then(Value::as_u64),
            Some(0)
        );
        assert_eq!(
            evidence.pointer("/attempt").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            evidence.pointer("/row_seq").and_then(Value::as_u64),
            Some(0)
        );
        assert_eq!(
            evidence.pointer("/ids/trial_id").and_then(Value::as_str),
            Some("trial_1")
        );
        assert!(
            evidence
                .pointer("/slot_commit_id")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "evidence row should be annotated with slot identity"
        );

        let chain_state = load_sqlite_json_row(&run_dir, "chain_state_rows", "run_1");
        assert_eq!(
            chain_state.pointer("/run_id").and_then(Value::as_str),
            Some("run_1")
        );
        assert_eq!(
            chain_state.pointer("/schedule_idx").and_then(Value::as_u64),
            Some(0)
        );
        assert_eq!(
            chain_state.pointer("/attempt").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            chain_state.pointer("/row_seq").and_then(Value::as_u64),
            Some(0)
        );
        assert!(
            chain_state
                .pointer("/slot_commit_id")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "chain state row should be annotated with slot identity"
        );
    }

    #[test]
    fn resolve_agent_runtime_custom_image_supports_command_override_string() {
        let root = TempDirGuard::new("bucephalus_command_override_string");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": "agentctl",
                    "mount": {
                        "source": ".lab/agents/agent-current.tar.gz",
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    },
                    "image": "debian:bookworm-slim"
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let agent_runtime =
            resolve_agent_runtime(&spec, &exp_dir, &root.path).expect("resolve runtime");
        assert_eq!(agent_runtime.command_raw, vec!["agentctl"]);
    }

    #[test]
    fn resolve_agent_runtime_parses_file_backed_event_sink() {
        let root = TempDirGuard::new("bucephalus_event_sink_parse");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["agentctl", "run", "--events", "__BUCEPHALUS_EVENT_PATH_agent_events__"],
                    "mount": {
                        "source": ".lab/agents/agent-current.tar.gz",
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    },
                    "image": "debian:bookworm-slim",
                    "events": [
                        {
                            "id": "agent_events",
                            "format": "jsonl",
                            "mode": "jsonl",
                            "ingest": true
                        }
                    ]
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let agent_runtime =
            resolve_agent_runtime(&spec, &exp_dir, &root.path).expect("resolve runtime");
        assert_eq!(agent_runtime.event_sinks.len(), 1);
        assert_eq!(agent_runtime.event_sinks[0].id, "agent_events");
        assert_eq!(
            agent_runtime.event_sinks[0].path,
            lab_core::BUCEPHALUS_TRAJECTORY_PATH
        );
        assert!(!agent_runtime.event_sinks[0].persist);
    }

    #[test]
    fn resolve_agent_runtime_rejects_author_supplied_event_sink_path() {
        let root = TempDirGuard::new("bucephalus_event_sink_path_rejected");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["agentctl", "run"],
                    "image": "debian:bookworm-slim",
                    "events": [
                        {
                            "id": "agent_events",
                            "format": "jsonl",
                            "path": "/bucephalus/out/agent-events.jsonl",
                            "mode": "jsonl"
                        }
                    ]
                },
                "execution": { "agent_site": "agent_container" }
            }
        });
        let err = match resolve_agent_runtime(&spec, &exp_dir, &root.path) {
            Ok(_) => panic!("author-supplied event sink path must be rejected"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("runner-owned"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_agent_runtime_rejects_scalar_artifact() {
        let root = TempDirGuard::new("bucephalus_scalar_artifact_rejected");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["agentctl"],
                    "mount": "./agent",
                    "image": "debian:bookworm-slim"
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let err = match resolve_agent_runtime(&spec, &exp_dir, &root.path) {
            Ok(_) => panic!("scalar artifact should be rejected"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("trial_runtime.agent.mount must be an object"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn resolve_agent_runtime_parses_explicit_artifact_mount() {
        let root = TempDirGuard::new("bucephalus_explicit_artifact_mount");
        let exp_dir = root.path.join("exp");
        let agent_dir = exp_dir.join("agent");
        ensure_dir(&agent_dir).expect("agent dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["agentctl"],
                    "mount": {
                        "source": "./agent",
                        "mount": {
                            "path": "/opt/custom-agent",
                            "read_only": false
                        }
                    },
                    "image": "debian:bookworm-slim"
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let runtime =
            resolve_agent_runtime(&spec, &exp_dir, &root.path).expect("resolve runtime");
        assert_eq!(
            runtime.agent_artifact_mount_path.as_deref(),
            Some("/opt/custom-agent")
        );
        assert!(!runtime.agent_artifact_read_only);
        assert_eq!(
            runtime.agent_artifact.as_deref(),
            Some(agent_dir.as_path())
        );
    }

    #[test]
    fn resolve_agent_runtime_shorthand_mount_requires_valid_agent_root() {
        let _guard = EnvVarGuard::set(&[(crate::local_storage::BUCEPHALUS_HOME_ENV, Some("relative-home"))]);
        let root = TempDirGuard::new("bucephalus_shorthand_agent_root");
        let exp_dir = root.path.join("exp");
        let agent_dir = exp_dir.join("agent");
        ensure_dir(&agent_dir).expect("agent dir");

        let explicit =
            resolve_agent_artifact_path("./agent", &exp_dir).expect("explicit relative artifact");
        assert_eq!(explicit, normalize_path(&agent_dir));

        let err = resolve_agent_artifact_path("agent-current", &exp_dir)
            .expect_err("shorthand artifact should require valid default agent root");
        assert!(
            err.to_string()
                .contains("resolve default agent root for shorthand artifact reference"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn resolve_agent_runtime_rejects_artifact_mount_over_task_workspace() {
        let root = TempDirGuard::new("bucephalus_artifact_mount_workspace_rejected");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir.join("agent")).expect("agent dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["agentctl"],
                    "mount": {
                        "source": "./agent",
                        "mount": {
                            "path": "/workspace/task",
                            "read_only": true
                        }
                    },
                    "image": "debian:bookworm-slim"
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let err = match resolve_agent_runtime(&spec, &exp_dir, &root.path) {
            Ok(_) => panic!("agent artifact mounts must not shadow task workspaces"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("targets reserved runner path '/workspace/task'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_packaged_agent_runtime_rejects_absolute_mount_source() {
        let root = TempDirGuard::new("bucephalus_packaged_absolute_mount_source");
        let package_dir = root.path.join("package");
        ensure_dir(&package_dir).expect("package dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["agentctl"],
                    "mount": {
                        "source": "/tmp/host-agent",
                        "mount": {
                            "path": "/opt/custom-agent",
                            "read_only": true
                        }
                    },
                    "image": "debian:bookworm-slim"
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let err = match resolve_packaged_agent_runtime(&spec, &package_dir, "base") {
            Ok(_) => panic!("sealed package runtime must not resolve absolute host mount sources"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("trial_runtime.agent.mount must be relative to the package root"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_packaged_agent_runtime_rejects_absolute_mount_resolved_path() {
        let root = TempDirGuard::new("bucephalus_packaged_absolute_mount_resolved_path");
        let package_dir = root.path.join("package");
        ensure_dir(&package_dir.join("agent_builds").join("build_0001"))
            .expect("package artifact dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["agentctl"],
                    "mount": {
                        "source": "agent_builds/build_0001",
                        "resolved_path": "/tmp/host-agent",
                        "mount": {
                            "path": "/opt/custom-agent",
                            "read_only": true
                        }
                    },
                    "image": "debian:bookworm-slim"
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let err = match resolve_packaged_agent_runtime(&spec, &package_dir, "base") {
            Ok(_) => panic!("sealed package runtime must not resolve absolute host resolved_path"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains(
                "trial_runtime.agent.mount.resolved_path must be relative to the package root"
            ),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_agent_runtime_parses_output_mounts() {
        let root = TempDirGuard::new("bucephalus_output_mount_parse");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["agentctl", "run"],
                    "mount": {
                        "source": ".lab/agents/agent-current.tar.gz",
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    },
                    "image": "debian:bookworm-slim",
                    "output_mounts": [
                        {
                            "id": "session_context",
                            "kind": "directory",
                            "path": "session-context",
                            "env": "BUCEPHALUS_SESSION_CONTEXT_ROOT",
                            "persist": true
                        }
                    ]
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let agent_runtime =
            resolve_agent_runtime(&spec, &exp_dir, &root.path).expect("resolve runtime");
        assert_eq!(agent_runtime.output_mounts.len(), 1);
        let mount = &agent_runtime.output_mounts[0];
        assert_eq!(mount.id, "session_context");
        assert_eq!(mount.path, "session-context");
        assert_eq!(
            mount.env.as_deref(),
            Some("BUCEPHALUS_SESSION_CONTEXT_ROOT")
        );
        assert_eq!(mount.container_path(), "/bucephalus/out/session-context");
    }

    #[test]
    fn resolve_agent_runtime_rejects_output_mount_path_escape() {
        let root = TempDirGuard::new("bucephalus_output_mount_escape");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["agentctl", "run"],
                    "mount": {
                        "source": ".lab/agents/agent-current.tar.gz",
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    },
                    "image": "debian:bookworm-slim",
                    "output_mounts": [
                        {
                            "id": "bad",
                            "kind": "directory",
                            "path": "../context",
                            "env": "BUCEPHALUS_SESSION_CONTEXT_ROOT"
                        }
                    ]
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let err = match resolve_agent_runtime(&spec, &exp_dir, &root.path) {
            Ok(_) => panic!("output mount path escape should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("trial_runtime.agent.output_mounts[0].path"),
            "{}",
            err
        );
    }

    #[test]
    fn build_runtime_contract_env_includes_bucephalus_keys() {
        let io = prepared_trial_io_fixture(
            PathBuf::from("/tmp/out.json"),
            PathBuf::from("/tmp/events.jsonl"),
        );
        let input = json!({
            "ids": {
                "trial_id": "trial_1",
                "variant_id": "control",
                "task_id": "task_1",
                "repl_idx": 0
            }
        });
        let env = build_runtime_contract_env("run_1", &input, &io, None, Some(12345))
            .expect("runtime env");
        assert_eq!(
            env.get(BUCEPHALUS_ENV_TRIAL_INPUT_PATH).map(String::as_str),
            Some(BUCEPHALUS_TRIAL_INPUT_PATH)
        );
        assert_eq!(
            env.get(BUCEPHALUS_ENV_RESULT_PATH).map(String::as_str),
            Some(BUCEPHALUS_RESULT_PATH)
        );
    }

    #[test]
    fn build_runtime_contract_env_rejects_missing_runtime_ids() {
        let io = prepared_trial_io_fixture(
            PathBuf::from("/tmp/out.json"),
            PathBuf::from("/tmp/events.jsonl"),
        );
        let input = json!({ "ids": { "trial_id": "trial_1" } });
        let err = build_runtime_contract_env("run_1", &input, &io, None, Some(12345))
            .expect_err("missing runtime ids should fail");
        assert!(err.to_string().contains("/ids/variant_id"), "{}", err);
    }

    #[test]
    fn perf_env_rejects_invalid_flags_and_cli_timestamp() {
        let root = TempDirGuard::new("bucephalus_perf_env_invalid");
        let record = || crate::perf::PerfRecord {
            run_dir: &root.path, run_id: "run_1", trial_id: None, schedule_idx: None, attempt: None,
            sample_kind: "event", stage: "perf", duration_ms: None, detail: json!({}),
        };
        for (env_name, expected) in [
            ("BUCEPHALUS_PERF_CAPTURE", "BUCEPHALUS_PERF_CAPTURE must be a boolean"),
            ("BUCEPHALUS_PERF_RESOURCE_SAMPLING", "BUCEPHALUS_PERF_RESOURCE_SAMPLING must be a boolean"),
        ] {
            let _guard = EnvVarGuard::set(&[(env_name, Some("maybe"))]);
            assert!(crate::perf::record(record()).unwrap_err().to_string().contains(expected));
        }

        let _guard = EnvVarGuard::set(&[(crate::perf::CLI_INVOKED_AT_MS_ENV, Some("bad"))]);
        let err = crate::perf::record_cli_latency(&root.path, "run_1", "cli", json!({}))
            .unwrap_err();
        assert!(err.to_string().contains("BUCEPHALUS_CLI_INVOKED_AT_MS"));
    }

    #[test]
    fn resolve_harness_rejects_grader_support_files() {
        let root = TempDirGuard::new("bucephalus_reject_grader_support_files");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["sh", "-lc", "echo ok"],
                    "mount": {
                        "source": ".lab/agents/agent-current.tar.gz",
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    },
                    "image": "debian:bookworm-slim"
                },
                "execution": {
                    "agent_site": "agent_container"
                },
                "grader": {
                    "support_files": [
                        {
                            "source_from_host": "./support",
                            "destination_path": "__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/evaluation"
                        }
                    ]
                }
            }
        });

        let err = match resolve_agent_runtime(&spec, &exp_dir, &root.path) {
            Ok(_) => panic!("trial_runtime.grader.support_files must fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("trial_runtime.grader.support_files is not supported"),
            "unexpected error: {}",
            err
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_runtime_asset_specs_reject_symlink_escape_from_project_root() {
        let root = TempDirGuard::new("bucephalus_runtime_asset_symlink_escape");
        let exp_dir = root.path.join("exp");
        let support_dir = exp_dir.join("support");
        ensure_dir(&support_dir).expect("support dir");
        let outside = root.path.join("outside.py");
        fs::write(&outside, "print('outside')").expect("outside support file");
        symlink(&outside, support_dir.join("tool.py")).expect("support symlink");

        let err = parse_build_runtime_asset_specs(
            Some(&json!([{
                "build_source_path": "support/tool.py",
                "runtime_path": "/workspace/task/.bucephalus/support/tool.py"
            }])),
            "trial_runtime.agent.support_files",
            &exp_dir,
            &exp_dir,
        )
        .expect_err("support file symlink escaping project root must fail");
        assert!(
            err.to_string().contains("resolves outside project root"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_harness_rejects_secret_env_aliases() {
        let root = TempDirGuard::new("bucephalus_secret_env_aliases");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let spec = json!({
            "runtime": { "network": { "agent": "none" } },
            "trial_runtime": {
                "agent": {
                    "command": ["sh", "-lc", "echo ok"],
                    "mount": {
                        "source": ".lab/agents/agent-current.tar.gz",
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    },
                    "image": "debian:bookworm-slim",
                    "secret_env": ["ANTHROPIC_API_KEY"]
                },
                "execution": {
                    "agent_site": "agent_container"
                }
            }
        });

        let err = match resolve_agent_runtime(&spec, &exp_dir, &root.path) {
            Ok(_) => panic!("should reject legacy aliases"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("trial_runtime.agent.secret_env is not supported"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn replay_grade_maps_by_integration_level() {
        assert_eq!(replay_grade_for_integration("control_full"), "strict");
        assert_eq!(replay_grade_for_integration("control_checkpoint"), "checkpointed");
        assert_eq!(replay_grade_for_integration("cli_events"), "best_effort");
        assert_eq!(replay_grade_for_integration("cli_basic"), "best_effort");
        assert_eq!(replay_grade_for_integration("something_unknown"), "none");
    }

    #[test]
    fn run_operation_lease_is_exclusive() {
        let run_dir = std::env::temp_dir().join(format!(
            "bucephalus_lock_test_{}_{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        ensure_dir(&run_dir).expect("temp run dir");

        let lock1 = acquire_run_operation_lease(&run_dir, RunOperationType::Continue)
            .expect("first lock must succeed");
        let lease = load_json_file(&operation_lease_path(&run_dir)).expect("operation lease json");
        let owner_host = lease
            .pointer("/owner_host")
            .and_then(Value::as_str)
            .expect("operation lease owner_host");
        assert!(!owner_host.trim().is_empty());
        let err = acquire_run_operation_lease(&run_dir, RunOperationType::Continue)
            .expect_err("second lock must fail");
        assert!(
            err.to_string().contains("operation_in_progress"),
            "unexpected lock error: {}",
            err
        );
        drop(lock1);
        let lock2 = acquire_run_operation_lease(&run_dir, RunOperationType::Continue)
            .expect("lock should be re-acquirable");
        drop(lock2);
        let _ = fs::remove_dir_all(run_dir);
    }

    #[test]
    fn run_operation_lease_rejects_malformed_existing_lease() {
        let root = TempDirGuard::new("bucephalus_malformed_operation_lease");
        let run_dir = root.path.join("run_1");
        ensure_dir(&run_dir).expect("run dir");
        let lease_path = operation_lease_path(&run_dir);
        ensure_dir(lease_path.parent().expect("lease parent")).expect("lease parent dir");
        fs::write(&lease_path, b"{not-json").expect("malformed lease");

        let err = acquire_run_operation_lease(&run_dir, RunOperationType::Continue)
            .expect_err("malformed operation lease should be surfaced");

        assert!(
            err.to_string().contains("failed to parse operation lease"),
            "unexpected error: {err}"
        );
        assert_eq!(
            fs::read_to_string(&lease_path).expect("lease still present"),
            "{not-json"
        );
    }

    #[test]
    fn run_operation_lease_missing_run_dir_fails_before_creating_runtime_dir() {
        let root = TempDirGuard::new("bucephalus_missing_operation_lease");
        let run_dir = root.path.join("missing-run");
        let err = acquire_run_operation_lease(&run_dir, RunOperationType::Continue)
            .expect_err("missing run dir should not acquire operation lease");
        assert!(
            err.to_string()
                .contains("resolve run directory for continue operation"),
            "unexpected error: {}",
            err
        );
        assert!(
            !run_dir.exists(),
            "operation lease acquisition should not create a missing run directory"
        );
    }

    #[test]
    fn fork_selector_parser_accepts_supported_kinds() {
        match parse_fork_selector("checkpoint:ckpt_a").expect("checkpoint selector") {
            ForkSelector::Checkpoint(v) => assert_eq!(v, "ckpt_a"),
            _ => panic!("expected checkpoint"),
        }
        match parse_fork_selector("step:12").expect("step selector") {
            ForkSelector::Step(v) => assert_eq!(v, 12),
            _ => panic!("expected step"),
        }
        match parse_fork_selector("event_seq:34").expect("event_seq selector") {
            ForkSelector::EventSeq(v) => assert_eq!(v, 34),
            _ => panic!("expected event_seq"),
        }
        assert!(parse_fork_selector("bad").is_err());
        assert!(parse_fork_selector("unknown:1").is_err());
    }

    #[test]
    fn control_ack_received_matches_action_and_control_version() {
        let root = std::env::temp_dir().join(format!(
            "bucephalus_ack_test_{}_{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        ensure_dir(&root).expect("temp dir");
        let events_path = root.join("harness_events.jsonl");
        let line = r#"{"event_type":"control_ack","seq":9,"step_index":2,"control_version":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","action_observed":"stop"}"#;
        atomic_write_bytes(&events_path, format!("{}\n", line).as_bytes()).expect("write events");

        assert!(control_ack_received(
            &events_path,
            "stop",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )
        .expect("parse ack"));
        assert!(!control_ack_received(
            &events_path,
            "checkpoint",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )
        .expect("parse ack"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_resume_selector_prefers_requested_label() {
        let (_root, run_dir) = create_run_dir("bucephalus_resume_sel_test", "run_1");
        seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([
                {"path": format!("{}/ckpt_a", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "a", "step": 1},
                {"path": format!("{}/ckpt_b", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "b", "step": 2}
            ]),
            "paused",
            Some("a"),
        );
        let selector =
            resolve_resume_selector(&run_dir, "run_1", "trial_1", Some("a")).expect("selector");
        assert_eq!(selector, "checkpoint:a");
    }

    #[test]
    fn resolve_resume_selector_defaults_to_latest_step() {
        let (_root, run_dir) = create_run_dir("bucephalus_resume_default_test", "run_1");
        seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([
                {"path": format!("{}/ckpt_a", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "a", "step": 3},
                {"path": format!("{}/ckpt_b", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "b", "step": 5}
            ]),
            "paused",
            Some("b"),
        );
        let selector =
            resolve_resume_selector(&run_dir, "run_1", "trial_1", None).expect("selector");
        assert_eq!(selector, "checkpoint:b");
    }

    #[test]
    fn resolve_resume_selector_errors_when_label_not_found() {
        let (_root, run_dir) = create_run_dir("bucephalus_resume_missing_label_test", "run_1");
        seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([{"path": format!("{}/ckpt_a", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "a", "step": 1}]),
            "paused",
            Some("a"),
        );
        let err = resolve_resume_selector(&run_dir, "run_1", "trial_1", Some("missing"))
            .expect_err("should fail");
        assert!(
            err.to_string().contains("resume_checkpoint_not_found"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn parse_fork_selector_rejects_empty_checkpoint_name() {
        let err = match parse_fork_selector("checkpoint: ") {
            Ok(_) => panic!("empty checkpoint should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("checkpoint name empty"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn resolve_selector_checkpoint_non_strict_uses_lineage_token_when_available() {
        let (_root, run_dir) = create_run_dir("bucephalus_fork_selector_path_missing", "run_1");
        let trial_dir = seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([{"path": format!("{}/cp_missing", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp1", "step": 3}]),
            "completed",
            None,
        );
        let output = json!({
            "checkpoints": [
                {"path": format!("{}/cp_missing", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp1", "step": 3}
            ]
        });
        let selector = parse_fork_selector("checkpoint:cp1").expect("selector");
        let source = resolve_selector_checkpoint(&selector, Some(&output), &trial_dir, false)
            .expect("selector resolution");
        assert!(
            source
                .as_deref()
                .is_some_and(|token| token.starts_with("lineage:")),
            "expected lineage token, got {:?}",
            source
        );
    }

    #[test]
    fn resolve_selector_checkpoint_strict_uses_lineage_not_fs_path() {
        let (_root, run_dir) = create_run_dir("bucephalus_fork_selector_strict_missing", "run_1");
        let trial_dir = seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([{"path": format!("{}/cp_missing", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp1", "step": 3}]),
            "completed",
            None,
        );
        let output = json!({
            "checkpoints": [
                {"path": format!("{}/cp_missing", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp1", "step": 3}
            ]
        });
        let selector = parse_fork_selector("checkpoint:cp1").expect("selector");
        let token = resolve_selector_checkpoint(&selector, Some(&output), &trial_dir, true)
            .expect("strict resolution should succeed with lineage");
        assert!(
            token
                .as_deref()
                .is_some_and(|value| value.starts_with("lineage:")),
            "unexpected token: {:?}",
            token
        );
    }

    #[test]
    fn replay_trial_requires_prepared_environment_manifest_and_rejects_legacy_trial_input() {
        let (_root, run_dir) =
            create_run_dir("bucephalus_replay_no_legacy_dataset_trial_dir", "run_1");
        write_resolved_experiment(&run_dir, "cli_events", true);
        write_packaged_task_dataset(&run_dir.join("tasks.jsonl"), "task_1");
        let parent_trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "completed", None);
        assert!(
            !parent_trial_dir.join("dataset").exists(),
            "parent trial should not carry legacy dataset dir"
        );

        assert!(
            parent_trial_dir.join("trial_input.json").exists(),
            "seeded trial input should exist so replay must ignore legacy input"
        );
        let err = match replay_trial(&run_dir, "trial_1", false) {
            Ok(_) => panic!("replay should require prepared_task_environment metadata"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("prepared_task_environment"),
            "replay should fail on missing prepared environment manifest, got: {}",
            msg
        );
        assert!(
            msg.contains("trial_1"),
            "replay failure should identify the affected trial, got: {}",
            msg
        );
    }

    #[test]
    fn fork_trial_requires_prepared_environment_manifest_without_legacy_input_only() {
        let (_root, run_dir) = create_run_dir("bucephalus_fork_missing_env_manifest", "run_1");
        write_resolved_experiment(&run_dir, "cli_events", true);
        seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([{"path": format!("{}/cp_missing", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp1", "step": 1}]),
            "completed",
            None,
        );
        let conn = rusqlite::Connection::open(account_sqlite_path_for_run(&run_dir).unwrap())
            .expect("open sqlite");
        conn.execute("DELETE FROM lineage_versions", [])
            .expect("delete lineage versions");
        conn.execute("DELETE FROM lineage_heads", [])
            .expect("delete lineage heads");

        let err = match fork_trial(
            &run_dir,
            "trial_1",
            "checkpoint:cp1",
            &BTreeMap::new(),
            false,
        ) {
            Ok(_) => panic!("fork should require prepared_task_environment metadata"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("prepared_task_environment"),
            "fork should fail on missing prepared environment manifest, got: {}",
            msg
        );
        assert!(
            !msg.contains("input_only"),
            "fork should not advertise legacy input_only mode, got: {}",
            msg
        );
    }

    #[test]
    fn strict_fork_requires_parent_output_payload() {
        let (_root, run_dir) = create_run_dir("bucephalus_fork_missing_parent_output", "run_1");
        write_resolved_experiment(&run_dir, "control_full", true);
        let trial_dir = seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([{"path": format!("{}/cp1", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp1", "step": 1}]),
            "completed",
            None,
        );
        prepare_task_environment_for_test(
            &run_dir,
            &trial_dir,
            &task_runtime_experiment_fixture(600000),
            &base_variant_fixture(),
            &runtime_task_boundary(
                json!({"id": "task_1"}),
                "python:3.11-slim",
                "/workspace/task",
                Some(600000),
            ),
            &legacy_contract_runtime_fixture(),
        )
        .expect("prepared parent manifest");

        let conn = rusqlite::Connection::open(account_sqlite_path_for_run(&run_dir).unwrap())
            .expect("open sqlite");
        conn.execute(
            "DELETE FROM attempt_objects WHERE trial_id='trial_1' AND role='trial_output'",
            [],
        )
        .expect("delete parent output");

        let err = fork_trial(&run_dir, "trial_1", "checkpoint:cp1", &BTreeMap::new(), true)
            .expect_err("strict fork should require parent output payload");
        assert!(
            err.to_string().contains("trial output payload not found"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn pause_run_rejects_target_trial_that_is_not_active() {
        let (_root, run_dir) = create_run_dir("bucephalus_pause_not_active", "run_1");
        write_resolved_experiment(&run_dir, "cli_events", true);
        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);
        let control = active_control_for_trial(&trial_dir);
        write_test_run_control(
            &run_dir,
            "run_1",
            "running",
            Some("trial_1"),
            Some(&control),
        );

        let err = pause_run(&run_dir, Some("trial_2"), Some("pause"), 1)
            .expect_err("pause should reject non-active target");
        assert!(
            err.to_string().contains("pause_target_not_active"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn control_operations_require_run_control_run_id() {
        let (_pause_root, pause_dir) = create_run_dir("bucephalus_pause_missing_run_id", "run_1");
        write_run_control_without_run_id(&pause_dir, "running");
        let err = pause_run(&pause_dir, None, Some("pause"), 1)
            .expect_err("pause should require run_control run_id");
        assert!(
            err.to_string().contains("missing run_id in run_control.json"),
            "unexpected pause error: {}",
            err
        );

        let (_kill_root, kill_dir) = create_run_dir("bucephalus_kill_missing_run_id", "run_1");
        write_run_control_without_run_id(&kill_dir, "running");
        let err = kill_run(&kill_dir).expect_err("kill should require run_control run_id");
        assert!(
            err.to_string().contains("missing run_id in run_control.json"),
            "unexpected kill error: {}",
            err
        );

        let (_resume_root, resume_dir) =
            create_run_dir("bucephalus_resume_missing_run_id", "run_1");
        write_run_control_without_run_id(&resume_dir, "paused");
        let err = resume_trial(&resume_dir, None, None, &BTreeMap::new(), false)
            .expect_err("resume should require run_control run_id");
        assert!(
            err.to_string().contains("missing run_id in run_control.json"),
            "unexpected resume error: {}",
            err
        );
    }

    #[test]
    fn resume_run_requires_run_to_be_paused() {
        let (_root, run_dir) = create_run_dir("bucephalus_resume_not_paused", "run_1");
        write_resolved_experiment(&run_dir, "control_full", true);
        let trial_dir = seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([{"path": format!("{}/cp1", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp1", "step": 1}]),
            "paused",
            Some("cp1"),
        );
        ensure_dir(&trial_dir.join("state").join("cp1")).expect("checkpoint path");
        let control = active_control_for_trial(&trial_dir);
        write_test_run_control(
            &run_dir,
            "run_1",
            "running",
            Some("trial_1"),
            Some(&control),
        );

        let err = resume_trial(&run_dir, None, None, &BTreeMap::new(), false)
            .expect_err("resume should fail for non-paused run");
        assert!(
            err.to_string().contains("resume_non_paused"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn resume_run_requires_trial_state_to_be_paused() {
        let (_root, run_dir) = create_run_dir("bucephalus_resume_trial_state", "run_1");
        write_resolved_experiment(&run_dir, "control_full", true);
        let trial_dir = seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([{"path": format!("{}/cp1", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp1", "step": 1}]),
            "completed",
            None,
        );
        ensure_dir(&trial_dir.join("state").join("cp1")).expect("checkpoint path");
        let attempt = runtime_trial_attempt_state_fixture(TrialPhase::Committed);
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &attempt)
            .expect("db attempt");
        let control = active_control_for_trial(&trial_dir);
        write_test_run_control(&run_dir, "run_1", "paused", Some("trial_1"), Some(&control));

        let err = resume_trial(&run_dir, None, None, &BTreeMap::new(), false)
            .expect_err("resume should fail when trial state is not paused");
        assert!(
            err.to_string().contains("resume_trial_not_paused"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn resume_trial_requires_prepared_environment_manifest_for_fork_resume() {
        let (_root, run_dir) = create_run_dir("bucephalus_resume_success", "run_1");
        write_resolved_experiment(&run_dir, "control_full", true);
        let trial_dir = seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([
                {"path": format!("{}/cp_old", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp_old", "step": 1},
                {"path": format!("{}/cp_resume", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp_resume", "step": 2}
            ]),
            "paused",
            Some("cp_resume"),
        );
        ensure_dir(&trial_dir.join("state").join("cp_resume")).expect("checkpoint path");
        let mut attempt = runtime_trial_attempt_state_fixture(TrialPhase::Paused);
        attempt.paused_from_phase = Some(TrialPhase::AgentRunning);
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &attempt)
            .expect("db attempt");
        let control = active_control_for_trial(&trial_dir);
        write_test_run_control(&run_dir, "run_1", "paused", Some("trial_1"), Some(&control));

        let mut set_bindings = BTreeMap::new();
        set_bindings.insert("resume.override".to_string(), json!(42));
        let err = match resume_trial(&run_dir, None, None, &set_bindings, false) {
            Ok(_) => panic!("resume should require prepared_task_environment metadata"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("prepared_task_environment"),
            "resume should fail on missing prepared environment manifest, got: {}",
            msg
        );
    }

    #[test]
    fn resume_trial_uses_db_paused_attempt_when_trial_state_file_is_missing() {
        let (_root, run_dir) = create_run_dir("bucephalus_resume_db_paused_no_trial_state", "run_1");
        write_resolved_experiment(&run_dir, "control_full", true);
        let trial_dir = seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([
                {"path": format!("{}/cp_resume", BUCEPHALUS_CONTRACT_STATE_DIR), "logical_name": "cp_resume", "step": 2}
            ]),
            "paused",
            Some("cp_resume"),
        );
        fs::remove_file(trial_state_path(&trial_dir)).expect("remove trial_state mirror");
        let mut attempt = runtime_trial_attempt_state_fixture(TrialPhase::Paused);
        attempt.paused_from_phase = Some(TrialPhase::AgentRunning);
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &attempt)
            .expect("db attempt");
        let control = active_control_for_trial(&trial_dir);
        write_test_run_control(&run_dir, "run_1", "paused", Some("trial_1"), Some(&control));

        let err = resume_trial(&run_dir, None, None, &BTreeMap::new(), false)
            .expect_err("resume should get past missing trial_state and fail on fork metadata");
        let msg = err.to_string();
        assert!(
            msg.contains("prepared_task_environment"),
            "resume should use DB paused state instead of requiring trial_state, got: {}",
            msg
        );
    }

    #[test]
    fn validate_required_fields_passes_on_complete_spec() {
        let spec = current_trial_runtime_experiment_base();
        validate_required_fields(&spec).expect("valid spec should pass");
    }

    #[test]
    fn validate_required_fields_rejects_declared_extra_output_path_escape() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["extra_outputs"] = json!([
            {
                "id": "escaped",
                "source_path": "../state/private.json",
                "summary_path": "grader/private.json"
            }
        ]);
        let err = validate_required_fields(&spec).expect_err("artifact escape should fail");
        assert!(
            err.to_string().contains("must not contain '..'"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_required_fields_rejects_removed_legacy_section_surface() {
        let mut spec = current_trial_runtime_experiment_base();
        let legacy_section = ["ben", "chmark"].concat();
        spec[legacy_section.as_str()]["artifacts"] = json!([
            {
                "id": "legacy",
                "source_path": "grader/legacy.json"
            }
        ]);
        let err = validate_required_fields(&spec).expect_err("legacy section should fail");
        let expected = format!("/{legacy_section} is not supported in v1");
        assert!(
            err.to_string().contains(&expected),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_required_fields_reports_all_missing() {
        let spec = json!({
            "experiment": { "name": "n" },
            "matrix": { "tasks": {} },
            "runtime": {
                "compute": {"backend": "local-docker"},
                "storage": {"backend": "local-fs"},
                "traces": {"backend": "local-stdout"},
                "network": {}
            },
            "policy": { "task_sandbox": {} }
        });
        let err = validate_required_fields(&spec).expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("/experiment/id"),
            "missing experiment id: {}",
            msg
        );
        assert!(
            msg.contains("/matrix/repeats"),
            "missing repeats: {}",
            msg
        );
        assert!(
            msg.contains("/trial_runtime/agent/command"),
            "missing trial_runtime.agent.command: {}",
            msg
        );
        assert!(
            msg.contains("/trial_runtime/agent/artifact_type"),
            "missing trial_runtime.agent.artifact_type: {}",
            msg
        );
        assert!(
            msg.contains("/runtime/network/task_sandbox"),
            "missing task_sandbox.network: {}",
            msg
        );
        assert!(
            msg.contains("/runtime/network/agent"),
            "missing agent.network: {}",
            msg
        );
        assert!(
            msg.contains("/trial_runtime/agent/outputs/result/capture/path"),
            "missing result capture path: {}",
            msg
        );
        assert!(
            msg.contains("/trial_runtime/agent/outputs/result/capture/type"),
            "missing result capture type: {}",
            msg
        );
        assert!(
            msg.contains("/trial_runtime/execution/agent_site"),
            "missing execution agent_site: {}",
            msg
        );
        assert!(
            msg.contains("/policy/timeout_ms"),
            "missing timeout: {}",
            msg
        );
        assert!(
            msg.contains("/matrix/tasks/source"),
            "missing matrix task source: {}",
            msg
        );
        assert!(
            msg.contains("/matrix/tasks/path"),
            "missing matrix task path: {}",
            msg
        );
        assert!(
            msg.contains("/matrix/variants"),
            "missing matrix variants: {}",
            msg
        );
    }

    #[test]
    fn validate_required_fields_allows_missing_integration_level() {
        let spec = current_trial_runtime_experiment_base();
        validate_required_fields(&spec).expect("missing integration_level should default");
    }

    #[test]
    fn validate_required_fields_requires_image_for_container_mode() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["trial_runtime"]["agent"]
            .as_object_mut()
            .unwrap()
            .remove("image");
        let err = validate_required_fields(&spec).expect_err("should fail");
        assert!(
            err.to_string().contains("/trial_runtime/agent/image"),
            "missing trial_runtime.agent.image: {}",
            err
        );
    }

    #[test]
    fn validate_required_fields_allows_missing_task_sandbox_profile() {
        let spec = current_trial_runtime_experiment_base();
        validate_required_fields(&spec).expect("task_sandbox.profile should default");
    }

    #[test]
    fn resolve_variant_plan_ignores_version_field() {
        let spec = json!({
            "version": "1.0",
            "matrix": { "variants": [
                { "id": "base", "baseline": true, "config": { "temperature": 0.7 } },
                { "id": "hot", "config": { "temperature": 0.9 } }
            ] }
        });
        let (variants, baseline_id) = resolve_variant_plan(&spec).expect("variant plan");
        assert_eq!(baseline_id, "base");
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[1].id, "hot");
    }

    #[test]
    fn resolve_variant_plan_reads_matrix_variants() {
        let spec = json!({
            "matrix": { "variants": [
                { "id": "base", "baseline": true, "config": {} },
                { "id": "old", "config": { "temperature": 0.7 } }
            ] }
        });
        let (variants, baseline_id) = resolve_variant_plan(&spec).expect("variants alias");
        assert_eq!(baseline_id, "base");
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[1].id, "old");
        assert_eq!(variants[1].bindings["temperature"], json!(0.7));
    }

    #[test]
    fn resolve_variant_plan_rejects_bad_variant_plan_entry() {
        let spec = json!({
            "matrix": { "variants": [
                { "baseline": true, "config": { "temperature": 0.8 } },
                { "id": "t2", "config": {} }
            ] }
        });

        let err = resolve_variant_plan(&spec).expect_err("bad variant plan should fail");
        assert!(
            err.to_string().contains("/matrix/variants[0]"),
            "unexpected error: {}",
            err
        );

        let spec = json!({
            "matrix": { "variants": [
                { "id": "base", "baseline": true, "config": {} },
                { "id": "t2", "config": [] }
            ] }
        });
        let err = resolve_variant_plan(&spec).expect_err("bad variant bindings type should fail");
        assert!(
            err.to_string().contains("/matrix/variants[1].config"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn resolve_variant_plan_uses_baseline_when_no_variant_plan_present() {
        let spec = json!({
            "matrix": { "variants": [{ "id": "base", "baseline": true, "config": {} }] }
        });

        let (variants, baseline_id) = resolve_variant_plan(&spec).expect("baseline only");
        assert_eq!(baseline_id, "base");
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].id, "base");
    }

    #[test]
    fn load_run_variants_uses_experiment_when_manifest_missing() {
        let (_root, run_dir) = create_run_dir("bucephalus_variants_from_experiment", "run_1");
        let spec = json!({
            "matrix": { "variants": [
                { "id": "base", "baseline": true, "config": {} },
                { "id": "alt", "config": { "temperature": 1.2 } }
            ] }
        });

        let (variants, baseline_id) =
            load_run_variants(&run_dir, &spec).expect("load variants from experiment");
        assert_eq!(baseline_id, "base");
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].id, "base");
        assert_eq!(variants[1].id, "alt");
    }

    #[test]
    fn load_run_variants_prefers_resolved_manifest_over_experiment() {
        let (_root, run_dir) = create_run_dir("bucephalus_variants_manifest_preferred", "run_1");
        let project_root = find_project_root(&run_dir);
        let bundle_root = ensure_test_agent_bundle(&project_root, "agent-current");
        let _ = bundle_root;
        let original = json!({
            "matrix": { "variants": [
                { "id": "base", "baseline": true, "config": {} },
                { "id": "alt", "config": { "temperature": 1.2 } }
            ] },
            "trial_runtime": { "agent": { "command": harness_success_command() } }
        });
        let (resolved_variants, resolved_baseline) =
            resolve_variant_plan(&original).expect("resolve variants");
        write_resolved_variants(&run_dir, &original, &resolved_baseline, &resolved_variants)
            .expect("write manifest");

        let changed = json!({
            "matrix": { "variants": [
                { "id": "changed", "baseline": true, "config": {} },
                { "id": "new", "config": { "temperature": 0.2 } }
            ] }
        });
        let (loaded_variants, loaded_baseline) =
            load_run_variants(&run_dir, &changed).expect("load manifest variants");

        assert_eq!(loaded_baseline, "base");
        assert_eq!(loaded_variants.len(), 2);
        assert_eq!(loaded_variants[0].id, "base");
        assert_eq!(loaded_variants[1].id, "alt");
    }

    #[test]
    fn load_run_variants_rejects_manifest_without_variant_digest() {
        let (_root, run_dir) = create_run_dir("bucephalus_variants_manifest_missing_digest", "run_1");
        fs::write(
            run_dir.join("resolved_variants.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": "resolved_variants_v1",
                "generated_at": "2026-03-10T00:00:00Z",
                "baseline_id": "base",
                "variants": [
                    {
                        "id": "base",
                        "bindings": {},
                        "args": [],
                        "env": {},
                        "image": null,
                        "runtime_overrides": null
                    }
                ]
            }))
            .expect("serialize manifest"),
        )
        .expect("write manifest");

        let err = load_run_variants(&run_dir, &json!({})).expect_err("missing variant_digest");
        assert!(err.to_string().contains("variant_digest"), "{}", err);
    }

    #[test]
    fn variant_digest_changes_with_variant_configuration() {
        let base = Variant {
            id: "base".to_string(),
            bindings: json!({}),
            args: vec!["--temperature".to_string(), "0.7".to_string()],
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let mut changed = base.clone();
        changed.args = vec!["--temperature".to_string(), "1.2".to_string()];

        let base_digest = variant_digest(&base).expect("base digest");
        let changed_digest = variant_digest(&changed).expect("changed digest");
        assert_ne!(base_digest, changed_digest);
    }

    #[test]
    fn resolve_variant_plan_parses_runtime_overrides() {
        let spec = json!({
            "matrix": { "variants": [
                {
                    "id": "base",
                    "baseline": true,
                    "config": {},
                    "overrides": { "policy": { "timeout_ms": 123000 } }
                },
                {
                    "id": "treatment",
                    "config": {},
                    "overrides": { "agent": { "custom_image": { "image": "example:variant" } } }
                }
            ] }
        });

        let (variants, baseline_id) = resolve_variant_plan(&spec).expect("variant plan");
        assert_eq!(baseline_id, "base");
        assert_eq!(variants.len(), 2);
        assert!(variants[0].runtime_overrides.is_some());
        assert!(variants[1].runtime_overrides.is_some());
    }

    #[test]
    fn resolve_variant_plan_rejects_invalid_runtime_overrides_shape() {
        let spec = json!({
            "matrix": { "variants": [
                { "id": "base", "baseline": true, "config": {}, "overrides": "bad" }
            ] }
        });
        let err = resolve_variant_plan(&spec).expect_err("baseline runtime_overrides should fail");
        assert!(
            err.to_string().contains("/matrix/variants[0].overrides"),
            "unexpected error: {}",
            err
        );

        let spec = json!({
            "matrix": { "variants": [
                { "id": "base", "baseline": true, "config": {} },
                { "id": "treatment", "config": {}, "overrides": "bad" }
            ] }
        });
        let err = resolve_variant_plan(&spec).expect_err("variant runtime_overrides should fail");
        assert!(
            err.to_string().contains("/matrix/variants[1].overrides"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn resolve_runtime_for_variant_merges_runtime_overrides() {
        let base = json!({
            "trial_runtime": {
                "agent": {
                    "image": "base:image",
                    "command": ["echo", "base"],
                    "env": {
                        "A": "1",
                        "B": "2"
                    },
                    "mount": {
                        "source": ".lab/agents/agent-current.tar.gz",
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    }
                },
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": "base:image",
                        "workdir": "/workspace/task"
                    }
                },
                "execution": {"agent_site": "agent_container"},
                "grader": {"strategy": "none"}
            }
        });
        let variant = Variant {
            id: "treatment".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: Some(json!({
                "agent": {
                    "image": "treatment:image",
                    "env": {
                        "B": "override",
                        "C": "3"
                    }
                },
                "task": {
                    "workspace": {
                        "image": "treatment-task:image"
                    }
                }
            })),
        };

        let merged = resolve_runtime_for_variant(&base, &variant).expect("merge");
        assert_eq!(
            merged
                .pointer("/trial_runtime/agent/image")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "treatment:image"
        );
        assert_eq!(
            merged
                .pointer("/trial_runtime/agent/command")
                .and_then(|v| v.as_array())
                .map(|v| v.len())
                .unwrap_or(0),
            2
        );
        assert_eq!(
            merged
                .pointer("/trial_runtime/agent/env/A")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "1"
        );
        assert_eq!(
            merged
                .pointer("/trial_runtime/agent/env/B")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "override"
        );
        assert_eq!(
            merged
                .pointer("/trial_runtime/agent/env/C")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "3"
        );
        assert_eq!(
            merged
                .pointer("/trial_runtime/task/workspace/image")
                .and_then(Value::as_str)
                .unwrap_or(""),
            "treatment-task:image"
        );
    }

    #[test]
    fn validate_required_fields_requires_grader_strategy() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["trial_runtime"]["grader"]
            .as_object_mut()
            .unwrap()
            .remove("strategy");
        let err = validate_required_fields(&spec).expect_err("should fail");
        assert!(
            err.to_string().contains("/trial_runtime/grader/strategy"),
            "missing grader strategy: {}",
            err
        );
    }

    #[test]
    fn p0_freeze_evaluation_adaptation_trial_shape_fixture_parses() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../testdata/p0_evaluation_adaptation_trial_shape.json"
        ))
        .expect("fixture json");
        let resolved = fixture
            .pointer("/resolved_experiment")
            .cloned()
            .expect("resolved fixture");
        let evaluation = parse_evaluation_config(&resolved).expect("evaluation config");
        assert_eq!(evaluation.policy.task_model, TaskModel::Dependent);
        assert_eq!(evaluation.policy.scoring_lifecycle, "predict_then_score");
        assert_eq!(
            evaluation.policy.required_evidence_classes,
            vec!["agent_patch".to_string(), "grader_report".to_string()]
        );
        let dataset_task = fixture
            .pointer("/dataset_task_row")
            .cloned()
            .expect("dataset task row");
        let boundary = runtime_task_boundary_from_row(dataset_task);
        assert_eq!(
            boundary
                .task_payload
                .pointer("/id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "project__case__12345"
        );
        assert_eq!(boundary.task_image, "ghcr.io/acme/project-eval-task:20260222");
        assert_eq!(boundary.task_workdir, "/testbed");
        assert_eq!(boundary.time_limit_ms, Some(1_800_000));
    }

    #[test]
    fn parse_evaluation_config_reads_typed_grader_contract() {
        let spec = json!({
            "trial_runtime": {
                "grader": {
                    "strategy": "injected",
                    "command": ["python3", "./grader.py"],
                    "injected": {
                        "bundle": "./graders/bundle.tar.gz",
                        "copy_dest": "/opt/grader"
                    }
                }
            }
        });

        let evaluation = parse_evaluation_config(&spec).expect("evaluation config");
        let grader = evaluation.grader.expect("grader config");
        assert_eq!(grader.strategy, GradingStrategy::Injected);
        assert_eq!(grader.command, vec!["python3", "./grader.py"]);
        let injected = grader.injected.expect("injected config");
        assert_eq!(injected.bundle, "./graders/bundle.tar.gz");
        assert_eq!(injected.copy_dest, "/opt/grader");
        assert!(grader.separate.is_none());
        assert!(grader.host.is_none());
    }

    #[test]
    fn parse_evaluation_config_reads_host_grader_runtime_boundary() {
        let spec = json!({
            "trial_runtime": {
                "grader": {
                    "strategy": "host",
                    "max_concurrency": 1,
                    "host": {
                        "capability": "host_eval_capability"
                    },
                    "command": [
                        "python3",
                        "__BUCEPHALUS_HOST_GRADER_CAPABILITY__/host_eval_capability/run_host_evaluation.py"
                    ],
                    "conclusion": {
                        "mode": "direct"
                    }
                }
            }
        });

        let evaluation = parse_evaluation_config(&spec).expect("evaluation config");
        let grader = evaluation.grader.expect("grader config");
        assert_eq!(grader.strategy, GradingStrategy::Host);
        assert_eq!(grader.max_concurrency, Some(1));
        assert_eq!(
            grader.host.expect("host config").capability,
            TEST_HOST_GRADER_CAPABILITY
        );
    }

    #[test]
    fn parse_evaluation_config_reads_current_policy_surface() {
        let spec = json!({
            "evaluation": {
                "policy": {
                    "task_model": "dependent",
                    "scoring_lifecycle": "predict_then_score",
                    "required_evidence_classes": ["agent_patch"]
                }
            }
        });

        let evaluation = parse_evaluation_config(&spec).expect("evaluation config");
        assert_eq!(evaluation.policy.task_model, TaskModel::Dependent);
        assert_eq!(evaluation.policy.scoring_lifecycle, "predict_then_score");
        assert_eq!(
            evaluation.policy.required_evidence_classes,
            vec!["agent_patch".to_string()]
        );
    }

    #[test]
    fn parse_evaluation_config_rejects_unknown_task_model() {
        let spec = json!({
            "evaluation": {
                "policy": {
                    "task_model": "mystery_model"
                }
            }
        });

        let err = parse_evaluation_config(&spec).expect_err("unknown task model should fail");
        assert!(
            err.to_string().contains("unknown task_model 'mystery_model'"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn parse_evaluation_config_rejects_grader_without_required_command() {
        let spec = json!({
            "trial_runtime": {
                "grader": {
                    "strategy": "host",
                    "host": {
                        "capability": "host_eval_capability"
                    }
                }
            }
        });

        let err = parse_evaluation_config(&spec).expect_err("missing command should fail");
        assert!(
            err.to_string()
                .contains("/trial_runtime/grader.command is required when strategy=host"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn parse_evaluation_config_rejects_none_strategy_with_runtime_fields() {
        let spec = json!({
            "trial_runtime": {
                "grader": {
                    "strategy": "none",
                    "command": ["python3", "grader.py"]
                }
            }
        });

        let err = parse_evaluation_config(&spec).expect_err("none strategy should be inert");
        assert!(
            err.to_string()
                .contains("/trial_runtime/grader.command must be omitted when strategy=none"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn parse_evaluation_config_rejects_invalid_grader_transport_shape() {
        let spec = json!({
            "trial_runtime": {
                "grader": {
                    "strategy": "in_task_runtime",
                    "command": ["python3", "grader.py"],
                    "in_task_runtime": {},
                    "inputs": {
                        "agent_result": {
                            "source": {"output": 7},
                            "materialize": {"as": "json_file", "path": "/bucephalus/out/grader_input.json"}
                        }
                    }
                }
            }
        });

        let err = parse_evaluation_config(&spec).expect_err("invalid transport should fail");
        assert!(
            err.to_string()
                .contains("invalid /trial_runtime/grader.inputs"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn declared_metrics_extract_canonical_ids_from_source_pointers() {
        let resolved = json!({
            "metrics": [
                {
                    "id": "latency",
                    "label": "Latency",
                    "semantic_key": "runtime.latency",
                    "source": { "type": "agent_response", "pointer": "/metrics/speed" },
                    "unit": "ms",
                    "direction": "minimize",
                    "primary": true
                }
            ]
        });
        let definitions = parse_metric_definitions(&resolved).expect("metric definitions");
        let (metrics, primary) = crate::trial::events::extract_declared_metrics(
            &definitions,
            &json!({ "metrics": { "speed": 123.0 } }),
        )
        .expect("declared metrics");

        assert_eq!(metrics.pointer("/latency"), Some(&json!(123.0)));
        assert!(metrics.pointer("/speed").is_none());
        assert_eq!(primary, Some(("latency".to_string(), json!(123.0))));
        assert_eq!(definitions[0].semantic_key.as_deref(), Some("runtime.latency"));
        let required = parse_metric_definitions(&json!({"metrics": [{
            "id": "score",
            "source": { "type": "agent_response", "pointer": "/metrics/score" },
            "required": true
        }]}))
        .expect("required metric definition");
        crate::trial::events::extract_declared_metrics(&required, &json!({}))
            .expect_err("required metric should fail when missing");
    }

    #[test]
    fn declared_metrics_reject_legacy_string_sources() {
        let resolved = json!({
            "metrics": [
                {
                    "id": "latency",
                    "source": "output",
                    "json_pointer": "/metrics/latency"
                }
            ]
        });

        let err = parse_metric_definitions(&resolved).expect_err("legacy source rejected");
        assert!(
            err.to_string().contains("metrics[0] source must be an object"),
            "{err}"
        );
    }

    #[test]
    fn declared_metrics_reject_unsupported_object_sources() {
        let resolved = json!({
            "metrics": [
                {
                    "id": "latency",
                    "source": {
                        "type": "output",
                        "pointer": "/metrics/latency"
                    }
                }
            ]
        });

        let err = parse_metric_definitions(&resolved).expect_err("unsupported source rejected");
        assert!(
            err.to_string()
                .contains("metrics[0] source.type 'output' is not supported"),
            "{err}"
        );
    }

    #[test]
    fn agent_response_loader_accepts_arbitrary_json_without_schema_fields() {
        let root = TempDirGuard::new("agent_response_loader_raw_json");
        let result_path = root.path.join("result.json");
        fs::write(
            &result_path,
            r#"{"metrics":{"latency_ms":12.5},"answer":{"text":"done"}}"#,
        )
        .expect("write result");

        let loaded = crate::trial::artifacts::load_agent_response_resilient(&result_path)
            .expect("load response");
        assert!(loaded.result_present);
        assert!(loaded.parse_error.is_none());
        assert_eq!(
            loaded.response.pointer("/metrics/latency_ms"),
            Some(&json!(12.5))
        );
    }

    #[test]
    fn artifact_type_from_trial_input_requires_explicit_supported_type() {
        let missing = crate::trial::artifacts::artifact_type_from_trial_input(&json!({
            "schema_version": "trial_input_v1"
        }))
        .unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("missing required string field artifact_type"),
            "unexpected error: {missing}"
        );

        let unknown = crate::trial::artifacts::artifact_type_from_trial_input(&json!({
            "schema_version": "trial_input_v1",
            "artifact_type": "answer"
        }))
        .unwrap_err();
        assert!(
            unknown
                .to_string()
                .contains("artifact_type 'answer' is not supported"),
            "unexpected error: {unknown}"
        );
    }

    #[test]
    fn p6_run_control_writer_emits_active_trials_without_legacy_mirrors() {
        let (_root, run_dir) = create_run_dir("bucephalus_run_control_writer", "run_1");
        write_test_run_control(&run_dir, "run_1", "running", Some("trial_1"), None);
        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");

        assert_eq!(
            run_control
                .pointer("/schema_version")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "run_control_v2"
        );
        assert_eq!(
            run_control
                .pointer("/active_trials/trial_1/trial_id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "trial_1"
        );
        assert!(
            run_control.pointer("/active_trial_id").is_none(),
            "legacy /active_trial_id should be removed in P6 cleanup"
        );
        assert!(
            run_control.pointer("/active_adapter").is_none(),
            "legacy /active_adapter should be removed in P6 cleanup"
        );
    }

    #[test]
    fn p1_run_control_v2_schema_accepts_writer_payload() {
        let (_root, run_dir) = create_run_dir("bucephalus_run_control_v2_schema", "run_1");
        write_test_run_control(&run_dir, "run_1", "running", Some("trial_1"), None);
        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        let schema = compile_schema("run_control_v2.jsonschema").expect("schema");
        match schema.validate(&run_control) {
            Ok(_) => {}
            Err(errors) => {
                let mut messages = Vec::new();
                for err in errors {
                    messages.push(err.to_string());
                }
                panic!(
                    "run_control_v2 schema validation failed: {}",
                    messages.join(" | ")
                );
            }
        };
    }

    #[test]
    fn p1_run_control_helpers_read_active_trial_and_control_from_v2_shape() {
        let run_control = json!({
            "schema_version": "run_control_v2",
            "run_id": "run_1",
            "status": "running",
            "active_trials": {
                "trial_alpha": {
                    "trial_id": "trial_alpha",
                    "worker_id": "worker_1",
                    "schedule_idx": 7,
                    "variant_id": "base",
                    "started_at": "2026-02-22T00:00:00Z",
                    "control": {
                        "id": "builtin.command_contract",
                        "version": "v1",
                        "command_path": "/tmp/control.json",
                        "events_path": "/tmp/events.jsonl"
                    }
                }
            },
            "updated_at": "2026-02-22T00:00:00Z"
        });

        let ids = run_control_active_trial_ids(&run_control);
        assert_eq!(ids, vec!["trial_alpha".to_string()]);
        let control = run_control_active_runtime_control_for_trial(&run_control, "trial_alpha")
            .expect("active runtime control");
        assert_eq!(
            control
                .pointer("/command_path")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "/tmp/control.json"
        );
    }

    #[derive(Debug, Clone)]
    struct DeterminismCompletion {
        schedule_idx: usize,
        classification: String,
    }

    #[derive(Debug, Deserialize)]
    struct P2EDeterminismFixture {
        schema_version: String,
        arrivals: Vec<P2EDeterminismArrival>,
        expected_commit_schedule_idx: Vec<usize>,
    }

    #[derive(Debug, Deserialize)]
    struct P2EDeterminismArrival {
        tick: usize,
        schedule_idx: usize,
        #[serde(rename = "trial_id")]
        _trial_id: String,
        classification: String,
    }

    struct OutOfOrderCompletionSimulator {
        by_tick: BTreeMap<usize, Vec<DeterminismCompletion>>,
    }

    impl OutOfOrderCompletionSimulator {
        fn from_fixture(fixture: &P2EDeterminismFixture) -> Self {
            let mut by_tick: BTreeMap<usize, Vec<DeterminismCompletion>> = BTreeMap::new();
            for row in fixture.arrivals.iter() {
                by_tick
                    .entry(row.tick)
                    .or_default()
                    .push(DeterminismCompletion {
                        schedule_idx: row.schedule_idx,
                        classification: row.classification.clone(),
                    });
            }
            Self { by_tick }
        }

        fn max_tick(&self) -> usize {
            self.by_tick.keys().copied().max().unwrap_or(0)
        }

        fn poll_tick(&mut self, tick: usize) -> Vec<DeterminismCompletion> {
            self.by_tick.remove(&tick).unwrap_or_default()
        }
    }

    fn load_p2e_determinism_fixture() -> P2EDeterminismFixture {
        let fixture: P2EDeterminismFixture =
            serde_json::from_str(include_str!("../testdata/p2e_determinism_fixture.json"))
                .expect("p2e fixture json");
        assert_eq!(fixture.schema_version, "p2e_determinism_fixture_v1");
        fixture
    }

    fn drain_ready_completions_by_slot(
        pending: &mut BTreeMap<usize, DeterminismCompletion>,
    ) -> Vec<DeterminismCompletion> {
        pending
            .keys()
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|schedule_idx| pending.remove(&schedule_idx))
            .collect()
    }

    #[test]
    fn p5b_local_worker_capacity_ceiling_resolves_with_warning() {
        let (effective, warning) = resolve_local_worker_max_in_flight(8, Some(3));
        assert_eq!(effective, 3);
        assert!(
            warning
                .as_deref()
                .unwrap_or("")
                .contains("capacity ceiling applied"),
            "expected capacity warning, got: {:?}",
            warning
        );

        let (effective_noop, warning_noop) = resolve_local_worker_max_in_flight(2, Some(4));
        assert_eq!(effective_noop, 2);
        assert!(warning_noop.is_none());
    }

    #[test]
    fn local_worker_capacity_has_no_implicit_default_ceiling() {
        let (effective, warning) = resolve_local_worker_max_in_flight(10_000, None);
        assert_eq!(effective, 10_000);
        assert!(warning.is_none());

        let (explicit_effective, explicit_warning) =
            resolve_local_worker_max_in_flight(10_000, Some(10_000));
        assert_eq!(explicit_effective, 10_000);
        assert!(explicit_warning.is_none());
    }

    #[test]
    fn slot_broker_rejects_completion_from_non_owner_without_dropping_claim() {
        let (_root, run_dir) = create_run_dir("bucephalus_slot_broker_owner_guard", "run_1");
        let project_root = run_dir.clone();
        let trials_dir = run_dir.join("trials");
        ensure_dir(&trials_dir).expect("trials dir");
        let variants = vec![Variant {
            id: "base".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        }];
        let schedule = vec![test_trial_slot(0)];
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .ensure_schedule_slots("run_1", &schedule)
            .expect("schedule slots");
        let committed = HashSet::new();
        let pending = HashSet::new();
        let pruned = HashSet::new();
        let broker = SlotBroker::new(
            &run_dir,
            "run_1",
            &project_root,
            &trials_dir,
            &variants,
            &schedule,
            SlotBrokerRecovery {
                committed_schedules: &committed,
                pending_completion_schedules: &pending,
                pruned_variants: &pruned,
                trial_index: 0,
            },
        )
        .expect("broker");

        let PulledWork::Trial { launch, .. } = broker
            .claim_next("worker_1")
            .expect("claim")
            .expect("work")
        else {
            panic!("expected trial work");
        };
        let err = broker
            .complete_owned("worker_2", &launch.trial_id, launch.schedule_idx)
            .expect_err("non-owner completion must be rejected");
        assert!(
            err.to_string().contains("ownership fault"),
            "unexpected error: {}",
            err
        );
        assert_eq!(broker.active_trials().len(), 1);
        broker
            .complete_owned("worker_1", &launch.trial_id, launch.schedule_idx)
            .expect("owner completion remains valid");
        assert!(broker.active_trials().is_empty());
    }

    #[test]
    fn p2e_out_of_order_completion_simulator_replays_fixture_ticks() {
        let fixture = load_p2e_determinism_fixture();
        let mut simulator = OutOfOrderCompletionSimulator::from_fixture(&fixture);

        let tick0 = simulator.poll_tick(0);
        assert_eq!(tick0.len(), 2);
        assert_eq!(tick0[0].schedule_idx, 2);
        assert_eq!(tick0[0].classification, "arrive_2");
        assert_eq!(tick0[1].schedule_idx, 0);
        assert_eq!(tick0[1].classification, "arrive_0");

        let tick1 = simulator.poll_tick(1);
        assert_eq!(tick1.len(), 2);
        assert_eq!(tick1[0].schedule_idx, 3);
        assert_eq!(tick1[0].classification, "arrive_3");
        assert_eq!(tick1[1].schedule_idx, 1);
        assert_eq!(tick1[1].classification, "arrive_1");

        let tick2 = simulator.poll_tick(2);
        assert!(tick2.is_empty(), "fixture should have no tick=2 arrivals");
    }

    #[test]
    fn p2e_determinism_fixture_commits_arrived_slots_without_prefix_wait() {
        let fixture = load_p2e_determinism_fixture();
        let mut simulator = OutOfOrderCompletionSimulator::from_fixture(&fixture);
        let max_tick = simulator.max_tick();

        let mut pending: BTreeMap<usize, DeterminismCompletion> = BTreeMap::new();
        let mut committed_schedule_idx = Vec::new();
        for tick in 0..=max_tick {
            for completion in simulator.poll_tick(tick) {
                pending.insert(completion.schedule_idx, completion);
            }
            let ready = drain_ready_completions_by_slot(&mut pending);
            for completion in ready {
                committed_schedule_idx.push(completion.schedule_idx);
            }
        }
        let trailing = drain_ready_completions_by_slot(&mut pending);
        for completion in trailing {
            committed_schedule_idx.push(completion.schedule_idx);
        }

        assert_eq!(
            committed_schedule_idx, fixture.expected_commit_schedule_idx,
            "commits should land in their own schedule slots without waiting for a prefix"
        );
        assert!(
            pending.is_empty(),
            "pending completion buffer should fully drain by final commit"
        );
    }

    fn write_run_control_multi_active_fixture(run_dir: &Path, status: &str, trials: &[&str]) {
        let mut active_trials = serde_json::Map::new();
        for (idx, trial_id) in trials.iter().enumerate() {
            active_trials.insert(
                (*trial_id).to_string(),
                json!({
                    "trial_id": trial_id,
                    "worker_id": format!("worker_{}", idx),
                    "schedule_idx": idx,
                    "variant_id": "base",
                    "started_at": "2026-02-22T00:00:00Z",
                    "control": {
                        "id": BUILTIN_COMMAND_RUNTIME_ID,
                        "version": BUILTIN_COMMAND_RUNTIME_VERSION,
                        "command_path": format!("/tmp/{}.control.json", trial_id),
                        "events_path": format!("/tmp/{}.events.jsonl", trial_id)
                    }
                }),
            );
        }
        let payload = json!({
            "schema_version": "run_control_v2",
            "run_id": "run_1",
            "status": status,
            "active_trials": active_trials,
            "updated_at": "2026-02-22T00:00:00Z"
        });
        let mut store = BackingSqliteStore::open(run_dir).expect("open sqlite store");
        store
            .put_runtime_json(RUNTIME_KEY_RUN_CONTROL, &payload)
            .expect("run control fixture");
    }

    #[test]
    fn p2e_pause_scaffolding_marks_interrupted_when_multi_flight_pause_fails() {
        let (_root, run_dir) = create_run_dir("bucephalus_p2e_pause_scaffold", "run_1");
        write_resolved_experiment(&run_dir, "cli_events", true);
        write_run_control_multi_active_fixture(&run_dir, "running", &["trial_a", "trial_b"]);

        let err = match pause_run(&run_dir, None, Some("checkpoint"), 1) {
            Ok(_) => {
                panic!("pause fan-out should fail when fixture trial dirs/controls are absent")
            }
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("pause_partial_failure"),
            "unexpected error: {}",
            err
        );
        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        assert_eq!(
            run_control
                .pointer("/status")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "interrupted"
        );
    }

    #[test]
    fn p2e_resume_scaffolding_requires_trial_id_when_multi_flight_is_active() {
        let (_root, run_dir) = create_run_dir("bucephalus_p2e_resume_scaffold", "run_1");
        write_run_control_multi_active_fixture(&run_dir, "paused", &["trial_a", "trial_b"]);

        let err = match resume_trial(&run_dir, None, None, &BTreeMap::new(), false) {
            Ok(_) => {
                panic!("resume without trial_id should fail when multiple active trials exist")
            }
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("resume_multiple_active_trials"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn p3a_deterministic_committer_commits_out_of_order_slots_and_dedupes() {
        let (_root, run_dir) = create_run_dir("bucephalus_p3a_committer", "run_1");
        let mut schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v1".to_string(),
            run_id: "run_1".to_string(),
            total_slots: 3,
            next_schedule_index: 0,
            next_trial_index: 2,
            schedule: test_trial_schedule(3),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let mut run_sink = SqliteRunJournal::new(&run_dir).expect("sink");
        let mut committer = DeterministicCommitter::from_progress(&schedule_progress, &[]);
        let policy_config = PolicyConfig::default();
        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();

        let inserted = committer
            .enqueue_trial(
                1,
                TrialExecutionResult::minimal("trial_2".to_string(), "completed", Some(0)),
            )
            .expect("enqueue idx=1");
        assert!(inserted, "first enqueue should be accepted");
        let mut commit_state = test_slot_commit_state(
            &mut schedule_progress,
            2,
            &mut pruned_variants,
            &mut consecutive_failures,
        );
        assert_eq!(
            committer
                .drain_ready(&run_dir, &policy_config, &mut commit_state, &mut run_sink)
                .expect("drain"),
            1,
            "idx=1 should commit directly to slot 1"
        );
        assert_eq!(
            schedule_progress
                .completed_slots
                .iter()
                .map(|slot| slot.schedule_index)
                .collect::<Vec<_>>(),
            vec![1]
        );

        committer
            .enqueue_trial(
                0,
                TrialExecutionResult::minimal("trial_1".to_string(), "completed", Some(0)),
            )
            .expect("enqueue idx=0");
        let mut commit_state = test_slot_commit_state(
            &mut schedule_progress,
            2,
            &mut pruned_variants,
            &mut consecutive_failures,
        );
        assert_eq!(
            committer
                .drain_ready(&run_dir, &policy_config, &mut commit_state, &mut run_sink)
                .expect("drain"),
            1,
            "idx=0 should commit independently after idx=1"
        );
        assert_eq!(
            schedule_progress
                .completed_slots
                .iter()
                .map(|slot| slot.schedule_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );

        let duplicate_committed = committer
            .enqueue_trial(
                1,
                TrialExecutionResult::minimal("trial_2".to_string(), "completed", Some(0)),
            )
            .expect("enqueue duplicate committed");
        assert!(
            !duplicate_committed,
            "duplicate completion for committed slot must be idempotently dropped"
        );
    }

    #[test]
    fn p3b_trial_preflight_stages_frozen_input_and_records_task_image() {
        let root = TempDirGuard::new("bucephalus_p3b_preflight");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        let trial_input_path = trial_dir.join("trial_input.json");
        atomic_write_json_pretty(
            &trial_input_path,
            &json!({
                "schema_version": "agent_task_v1",
                "ids": { "trial_id": "trial_1" }
            }),
        )
        .expect("trial input");

        let evaluation = EvaluationConfig {
            policy: TaskPolicyConfig::default(),
            grader: Some(GraderConfig::in_task_runtime(vec![
                "echo".to_string(),
                "ok".to_string(),
            ])),
        };
        stage_trial_preflight(
            &evaluation,
            &trial_dir,
            ("run_1", "trial_1", 4, "candidate"),
            (
                &json!({ "id": "task_9" }),
                Some("ghcr.io/acme/task:20260222"),
                &trial_input_path,
            ),
        )
        .expect("preflight");

        let preflight =
            load_json_file(&trial_preflight_path(&trial_dir)).expect("preflight json");
        assert_eq!(
            preflight
                .pointer("/schema_version")
                .and_then(Value::as_str),
            Some("trial_preflight_v1")
        );
        assert_eq!(
            preflight
                .pointer("/environment_image")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "ghcr.io/acme/task:20260222"
        );
        assert!(
            preflight
                .pointer("/grading/enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
        assert!(
            trial_dir
                .join("artifacts")
                .join("frozen_agent_input")
                .join("trial_input.json")
                .exists(),
            "frozen trial_input must be staged for grading/replay"
        );
    }

    #[test]
    fn p3b_trial_preflight_rejects_grading_opt_out_for_graded_tasks() {
        let root = TempDirGuard::new("bucephalus_p3b_preflight_grading_gate");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        let trial_input_path = trial_dir.join("trial_input.json");
        atomic_write_json_pretty(
            &trial_input_path,
            &json!({
                "schema_version": "agent_task_v1",
                "ids": { "trial_id": "trial_1" }
            }),
        )
        .expect("trial input");

        let evaluation = EvaluationConfig {
            policy: TaskPolicyConfig::default(),
            grader: Some(GraderConfig::in_task_runtime(vec![
                "echo".to_string(),
                "ok".to_string(),
            ])),
        };
        let err = stage_trial_preflight(
            &evaluation,
            &trial_dir,
            ("run_1", "trial_1", 4, "candidate"),
            (
                &json!({
                    "id": "task_9",
                    "grading": { "enabled": false }
                }),
                Some("ghcr.io/acme/task:20260222"),
                &trial_input_path,
            ),
        )
        .expect_err("grading opt-out should be rejected");
        assert!(
            err.to_string().contains("grading.enabled=false"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn p3c_run_control_writer_supports_multi_flight_active_trials() {
        let (_root, run_dir) = create_run_dir("bucephalus_p3c_run_control", "run_1");
        let active_trials = vec![
            RunControlActiveTrial {
                trial_id: "trial_1".to_string(),
                worker_id: "worker_a".to_string(),
                schedule_idx: Some(1),
                variant_id: "base".to_string(),
                started_at: Some("2026-02-22T00:00:00Z".to_string()),
                control: None,
            },
            RunControlActiveTrial {
                trial_id: "trial_2".to_string(),
                worker_id: "worker_b".to_string(),
                schedule_idx: Some(2),
                variant_id: "candidate".to_string(),
                started_at: Some("2026-02-22T00:00:01Z".to_string()),
                control: None,
            },
        ];
        write_run_control(&run_dir, "run_1", "running", &active_trials, None)
            .expect("write run control v2");
        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        assert_eq!(
            run_control
                .pointer("/active_trials/trial_1/schedule_idx")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            1
        );
        assert_eq!(
            run_control
                .pointer("/active_trials/trial_2/variant_id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "candidate"
        );
        assert!(
            run_control.pointer("/active_trial_id").is_none(),
            "legacy active_trial_id mirror field should be removed"
        );
        assert!(
            run_control.pointer("/active_adapter").is_none(),
            "legacy active_adapter mirror field should be removed"
        );
    }

    #[test]
    fn p4_cutover_uses_parallel_engine_path_for_isolate_policy() {
        let (_root, run_dir) = create_run_dir("bucephalus_p4_parallel_path", "run_1");
        write_run_control(&run_dir, "run_1", "paused", &[], None).expect("run control");
        let trials_dir = run_dir.join("trials");
        ensure_dir(&trials_dir).expect("trials dir");

        let mut schedule_progress = new_schedule_progress("run_1", &[]);
        let mut trial_index = 0_usize;
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut run_sink = SqliteRunJournal::new(&run_dir).expect("sink");
        let policy_config = PolicyConfig::default();
        let evaluation_config = EvaluationConfig::default();
        let mut request = test_schedule_engine_request(
            &run_dir,
            &trials_dir,
            &[],
            &[],
            &[],
            &policy_config,
            &evaluation_config,
        );
        request.max_concurrency = 2;
        execute_schedule_engine(
            request,
            test_schedule_engine_state(
                &mut schedule_progress,
                &mut trial_index,
                &mut consecutive_failures,
                &mut pruned_variants,
                &mut run_sink,
            ),
        )
        .expect("parallel engine should no-op cleanly");

        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        assert_eq!(
            run_control
                .pointer("/status")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "running"
        );
        let active_trials = run_control
            .pointer("/active_trials")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        assert!(
            active_trials.is_empty(),
            "parallel engine should end with no active trials"
        );
    }

    #[test]
    fn scheduler_treats_next_schedule_index_as_hint_not_authority() {
        let _disk_headroom = EnvVarGuard::set(&[(BUCEPHALUS_MIN_FREE_BYTES_ENV, Some("1"))]);
        let (_root, run_dir) = create_run_dir("bucephalus_scheduler_cursor_hint", "run_1");
        write_run_control(&run_dir, "run_1", "interrupted", &[], None).expect("run control");
        let trials_dir = run_dir.join("trials");
        ensure_dir(&trials_dir).expect("trials dir");

        let variants = vec![Variant {
            id: "base".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        }];
        let schedule = vec![test_trial_slot(0)];
        let mut schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "run_1".to_string(),
            total_slots: schedule.len(),
            next_schedule_index: schedule.len(),
            next_trial_index: 0,
            schedule: schedule.clone(),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        write_schedule_progress(&run_dir, &schedule_progress).expect("schedule progress");

        let mut trial_index = 0_usize;
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut pruned_variants = HashSet::from([0_usize]);
        let mut run_sink = SqliteRunJournal::new(&run_dir).expect("sink");
        let tasks = vec![json!({"id":"task_1"})];
        let policy_config = PolicyConfig::default();
        let evaluation_config = EvaluationConfig::default();
        execute_schedule_engine(
            test_schedule_engine_request(
                &run_dir,
                &trials_dir,
                &variants,
                &tasks,
                &schedule,
                &policy_config,
                &evaluation_config,
            ),
            test_schedule_engine_state(
                &mut schedule_progress,
                &mut trial_index,
                &mut consecutive_failures,
                &mut pruned_variants,
                &mut run_sink,
            ),
        )
        .expect("scheduler should process uncommitted slot despite stale cursor");

        assert_eq!(schedule_progress.completed_slots.len(), 1);
        assert_eq!(schedule_progress.completed_slots[0].schedule_index, 0);
        assert_eq!(schedule_progress.completed_slots[0].status, "skipped_pruned");
        let slot = BackingSqliteStore::open(&run_dir)
            .expect("store")
            .schedule_slot("run_1", 0)
            .expect("slot query")
            .expect("slot row");
        assert_eq!(slot.state, "committed");
        assert_eq!(slot.slot_status.as_deref(), Some("skipped_pruned"));
    }

    #[test]
    fn scheduler_does_not_dispatch_active_slot_without_recovery() {
        let _disk_headroom = EnvVarGuard::set(&[(BUCEPHALUS_MIN_FREE_BYTES_ENV, Some("1"))]);
        let (_root, run_dir) = create_run_dir("bucephalus_scheduler_active_slot_guard", "run_1");
        write_run_control(&run_dir, "run_1", "interrupted", &[], None).expect("run control");
        let trials_dir = run_dir.join("trials");
        ensure_dir(&trials_dir).expect("trials dir");
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");

        let variants = vec![Variant {
            id: "base".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        }];
        let schedule = vec![test_trial_slot(0)];
        let mut schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "run_1".to_string(),
            total_slots: schedule.len(),
            next_schedule_index: 0,
            next_trial_index: 1,
            schedule: schedule.clone(),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        write_schedule_progress(&run_dir, &schedule_progress).expect("schedule progress");
        {
            let mut store = BackingSqliteStore::open(&run_dir).expect("store");
            store
                .ensure_schedule_slots("run_1", &schedule)
                .expect("schedule slots");
            store
                .claim_schedule_slot(
                    "run_1",
                    0,
                    "trial_1",
                    "worker_1",
                    "live-owner",
                    None,
                )
                .expect("claim")
                .expect("active slot");
        }

        let mut trial_index = 1_usize;
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut run_sink = SqliteRunJournal::new(&run_dir).expect("sink");
        let tasks = vec![json!({"id":"task_1"})];
        let policy_config = PolicyConfig::default();
        let evaluation_config = EvaluationConfig::default();
        execute_schedule_engine(
            test_schedule_engine_request(
                &run_dir,
                &trials_dir,
                &variants,
                &tasks,
                &schedule,
                &policy_config,
                &evaluation_config,
            ),
            test_schedule_engine_state(
                &mut schedule_progress,
                &mut trial_index,
                &mut consecutive_failures,
                &mut pruned_variants,
                &mut run_sink,
            ),
        )
        .expect("scheduler should not dispatch active slot");

        assert!(schedule_progress.completed_slots.is_empty());
        let slot = BackingSqliteStore::open(&run_dir)
            .expect("store")
            .schedule_slot("run_1", 0)
            .expect("slot query")
            .expect("slot row");
        assert_eq!(slot.state, "active");
        assert_eq!(slot.trial_id.as_deref(), Some("trial_1"));
    }

    #[test]
    fn schedule_slot_commit_rejects_different_active_trial_owner() {
        let (_root, run_dir) = create_run_dir("bucephalus_schedule_slot_owner_guard", "run_1");
        let schedule = vec![test_trial_slot(0)];
        let mut store = BackingSqliteStore::open(&run_dir).expect("store");
        store
            .ensure_schedule_slots("run_1", &schedule)
            .expect("schedule slots");
        store
            .claim_schedule_slot(
                "run_1",
                0,
                "trial_1",
                "worker_1",
                "owner_1",
                None,
            )
            .expect("claim")
            .expect("slot claimed");

        let err = store
            .mark_schedule_slot_committed("run_1", 0, "trial_2", 1, "commit_1", "completed")
            .expect_err("different active owner must not commit slot");
        assert!(
            err.to_string().contains("schedule_slot_owner_mismatch"),
            "unexpected error: {}",
            err
        );
        let slot = store
            .schedule_slot("run_1", 0)
            .expect("slot query")
            .expect("slot row");
        assert_eq!(slot.state, "active");
        assert_eq!(slot.trial_id.as_deref(), Some("trial_1"));
    }

    #[test]
    fn schedule_slot_transaction_rolls_back_facts_progress_and_slot_on_mid_commit_failure() {
        let (_root, run_dir) = create_run_dir("bucephalus_schedule_slot_atomic_rollback", "run_1");
        let schedule = vec![test_trial_slot(0)];
        let initial_progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "run_1".to_string(),
            total_slots: schedule.len(),
            next_schedule_index: 0,
            next_trial_index: 1,
            schedule: schedule.clone(),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        write_schedule_progress(&run_dir, &initial_progress).expect("progress");

        let mut store = BackingSqliteStore::open(&run_dir).expect("store");
        store
            .ensure_schedule_slots("run_1", &schedule)
            .expect("schedule slots");
        store
            .claim_schedule_slot("run_1", 0, "trial_1", "worker_1", "owner_1", None)
            .expect("claim")
            .expect("claimed slot");

        let slot_commit_id = "slot_atomic_rollback";
        let commit_record = SlotCommitRecord {
            schema_version: "slot_commit_record_v1".to_string(),
            record_type: "commit".to_string(),
            run_id: "run_1".to_string(),
            schedule_idx: 0,
            slot_commit_id: slot_commit_id.to_string(),
            trial_id: "trial_1".to_string(),
            slot_status: "completed".to_string(),
            attempt: 1,
            recorded_at: Utc::now().to_rfc3339(),
            expected_rows: None,
            payload_digest: None,
            written_rows: Some(SlotCommitRowCounts {
                trials: 1,
                metrics: 0,
                events: 0,
                contract_stages: 0,
                variant_snapshots: 0,
                evidence: 0,
                chain_states: 0,
                conclusions: 0,
                predictions: 0,
                scores: 0,
            }),
            facts_fsync_completed: Some(true),
            runtime_fsync_completed: Some(true),
        };
        let mut next_progress = initial_progress.clone();
        next_progress.completed_slots.push(SlotCompletion {
            schedule_index: 0,
            trial_id: "trial_1".to_string(),
            status: "completed".to_string(),
            slot_commit_id: slot_commit_id.to_string(),
            attempt: 1,
        });
        next_progress.next_schedule_index = 1;
        let trial_rows = vec![TrialRecord {
            run_id: "run_1".to_string(),
            trial_id: "trial_1".to_string(),
            schedule_idx: 0,
            slot_commit_id: slot_commit_id.to_string(),
            attempt: 1,
            row_seq: 0,
            baseline_id: "base".to_string(),
            workload_type: "agent_harness".to_string(),
            variant_id: "base".to_string(),
            task_index: 0,
            task_id: "task_1".to_string(),
            repl_idx: 0,
            outcome: "success".to_string(),
            success: true,
            status_code: "completed".to_string(),
            integration_level: "test".to_string(),
            network_mode_requested: "none".to_string(),
            network_mode_effective: "none".to_string(),
            primary_metric_name: "score".to_string(),
            primary_metric_value: json!(1),
            metrics: json!({"score": 1}),
            bindings: json!({}),
            events_total: 0,
            has_events: false,
        }];
        let commit_value = serde_json::to_value(&commit_record).expect("commit value");
        let progress_value = serde_json::to_value(&next_progress).expect("progress value");

        let err = store
            .commit_schedule_slot_transaction(SlotCommitTransactionInput {
                run_id: "run_1",
                schedule_idx: 0,
                slot: Some(&schedule[0]),
                trial_id: "trial_1",
                attempt: 1,
                slot_commit_id,
                slot_status: "completed",
                commit_record: &commit_value,
                schedule_progress: &progress_value,
                trial_rows: &trial_rows,
                metric_rows: &[],
                event_rows: &[],
                contract_stage_rows: &[],
                variant_snapshot_rows: &[],
                evidence_rows: &[],
                chain_state_rows: &[],
                trial_conclusion_rows: &[],
                fail_after_facts: true,
            })
            .expect_err("failpoint should roll back transaction");
        assert!(
            err.to_string()
                .contains("slot_commit_transaction_failpoint_after_facts"),
            "unexpected error: {}",
            err
        );

        assert_eq!(store.row_count("trial_rows").expect("trial row count"), 0);
        assert!(
            load_slot_commit_records(&run_dir)
                .expect("commit records")
                .is_empty()
        );
        let persisted_progress = load_schedule_progress(&run_dir).expect("progress");
        assert!(persisted_progress.completed_slots.is_empty());
        assert_eq!(persisted_progress.next_schedule_index, 0);
        let slot = store
            .schedule_slot("run_1", 0)
            .expect("slot query")
            .expect("slot");
        assert_eq!(slot.state, "active");
        assert_eq!(slot.trial_id.as_deref(), Some("trial_1"));
        assert_eq!(slot.slot_commit_id, None);
    }

    #[test]
    fn p5a_recovered_active_trials_commit_as_worker_lost_deterministically() {
        let (_root, run_dir) = create_run_dir("bucephalus_p5a_worker_lost", "run_1");
        let trials_dir = run_dir.join("trials");
        ensure_dir(&trials_dir).expect("trials dir");
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");

        let variants = vec![Variant {
            id: "base".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        }];
        let schedule = vec![test_trial_slot(0)];
        let mut schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v1".to_string(),
            run_id: "run_1".to_string(),
            total_slots: 1,
            next_schedule_index: 0,
            next_trial_index: 0,
            schedule: schedule.clone(),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let recovered_active_trials = vec![RunControlActiveTrial {
            trial_id: "trial_orphan".to_string(),
            worker_id: "worker_dead".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: None,
        }];
        let mut trial_index = 0_usize;
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut run_sink = SqliteRunJournal::new(&run_dir).expect("sink");
        let policy_config = PolicyConfig {
            pruning_max_consecutive_failures: Some(1),
            ..PolicyConfig::default()
        };
        let tasks = vec![json!({"id":"task_1"})];
        let evaluation_config = EvaluationConfig::default();
        let mut request = test_schedule_engine_request(
            &run_dir,
            &trials_dir,
            &variants,
            &tasks,
            &schedule,
            &policy_config,
            &evaluation_config,
        );
        request.materialize_mode = MaterializationMode::Full;
        request.recovered_active_trials = &recovered_active_trials;
        execute_schedule_engine(
            request,
            test_schedule_engine_state(
                &mut schedule_progress,
                &mut trial_index,
                &mut consecutive_failures,
                &mut pruned_variants,
                &mut run_sink,
            ),
        )
        .expect("parallel recovery handling");

        assert_eq!(schedule_progress.next_schedule_index, 1);
        assert_eq!(schedule_progress.completed_slots.len(), 1);
        assert_eq!(schedule_progress.completed_slots[0].schedule_index, 0);
        assert_eq!(
            schedule_progress.completed_slots[0].trial_id,
            "trial_orphan"
        );
        assert_eq!(schedule_progress.completed_slots[0].status, "failed");
        assert_eq!(consecutive_failures.get(&0).copied().unwrap_or(0), 1);
        assert!(pruned_variants.contains(&0));
    }

    #[test]
    fn p7_pause_run_rejects_active_trial_without_runtime_container_state() {
        let _runtime_guard = lock_runtime_control_tests();
        let (_root, run_dir) = create_run_dir("bucephalus_p7_pause_legacy_active_trial", "run_1");
        let _trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);
        let active_trials = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_parallel_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: None,
        }];
        write_run_control(&run_dir, "run_1", "running", &active_trials, None).expect("control");

        let err =
            pause_run(&run_dir, None, Some("worker_pause"), 2).expect_err("pause should fail");
        assert!(
            err.to_string().contains("pause_missing_runtime_container"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn p7_pause_run_uses_persisted_runtime_container_when_runtime_control_missing() {
        let _runtime_guard = lock_runtime_control_tests();
        if !docker_runtime_available() {
            return;
        }
        if let Err(err) = try_ensure_docker_test_image("python:3.11-slim") {
            eprintln!("skipping persisted runtime pause test: {err}");
            return;
        }

        let (_root, run_dir) = create_run_dir("bucephalus_p7_pause_runtime_state", "run_1");
        write_resolved_experiment(&run_dir, "cli_events", true);
        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);

        let docker = crate::backend::docker::DockerRuntime::connect().expect("docker runtime");
        let handle = match docker.create_and_start_container_checked(
            &crate::backend::docker::ContainerSpec::idle("python:3.11-slim"),
            "test idle container",
        ) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!("skipping persisted runtime pause test: {err}");
                return;
            }
        };
        if let Err(err) = docker.pause_container(&handle) {
            let _ = docker.remove_container(&handle, true);
            eprintln!("skipping persisted runtime pause test: {err}");
            return;
        }
        if let Err(err) = docker.unpause_container(&handle) {
            let _ = docker.remove_container(&handle, true);
            eprintln!("skipping persisted runtime pause test: {err}");
            return;
        }

        let runtime_state = runtime_trial_attempt_state_with_task_container(
            TrialPhase::AgentRunning,
            &handle.container_id,
        );
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &runtime_state)
            .expect("db runtime state");

        let active_trials = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_parallel_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: None,
        }];
        write_run_control(&run_dir, "run_1", "running", &active_trials, None).expect("control");

        let paused =
            pause_run(&run_dir, None, Some("docker_pause"), 2).expect("pause should succeed");
        assert_eq!(paused.trial_id, "trial_1");
        assert!(paused.checkpoint_acked);
        assert!(paused.stop_acked);

        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        assert_eq!(
            run_control.pointer("/status").and_then(Value::as_str),
            Some("paused")
        );

        let trial_state = load_json_file(&trial_state_path(&trial_dir)).expect("trial state");
        assert_eq!(
            trial_state.pointer("/status").and_then(Value::as_str),
            Some("paused")
        );

        let runtime_state =
            trial::state::load_trial_attempt_state(&trial_dir).expect("runtime state");
        assert_eq!(runtime_state.state.phase, TrialPhase::Paused);

        let inspected = docker
            .inspect_container(&handle)
            .expect("inspect paused container");
        assert_eq!(inspected.status.as_deref(), Some("paused"));

        let _ = docker.remove_container(&handle, true);
    }

    #[test]
    fn p7_kill_run_uses_persisted_runtime_container_when_runtime_control_missing() {
        let _runtime_guard = lock_runtime_control_tests();
        if !docker_runtime_available() {
            return;
        }
        if let Err(err) = try_ensure_docker_test_image("python:3.11-slim") {
            eprintln!("skipping persisted runtime kill test: {err}");
            return;
        }

        let (_root, run_dir) = create_run_dir("bucephalus_p7_kill_runtime_state", "run_1");
        write_resolved_experiment(&run_dir, "cli_events", true);
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");
        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);

        let docker = crate::backend::docker::DockerRuntime::connect().expect("docker runtime");
        let handle = match docker.create_and_start_container_checked(
            &crate::backend::docker::ContainerSpec::idle("python:3.11-slim"),
            "test idle container",
        ) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!("skipping persisted runtime kill test: {err}");
                return;
            }
        };

        let runtime_state = runtime_trial_attempt_state_with_task_container(
            TrialPhase::AgentRunning,
            &handle.container_id,
        );
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &runtime_state)
            .expect("db runtime state");

        let active_trials = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_parallel_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: None,
        }];
        write_run_control(&run_dir, "run_1", "running", &active_trials, None).expect("control");

        let killed = kill_run(&run_dir).expect("kill should succeed");
        assert_eq!(killed.killed_trials, vec!["trial_1".to_string()]);

        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        assert_eq!(
            run_control.pointer("/status").and_then(Value::as_str),
            Some("killed")
        );

        let trial_state = load_json_file(&trial_state_path(&trial_dir)).expect("trial state");
        assert_eq!(
            trial_state.pointer("/status").and_then(Value::as_str),
            Some("killed")
        );

        let runtime_state =
            trial::state::load_trial_attempt_state(&trial_dir).expect("runtime state");
        assert_eq!(runtime_state.state.phase, TrialPhase::Killed);

        let inspect_err = docker
            .inspect_container(&handle)
            .expect_err("killed container should be removed");
        assert!(
            inspect_err.to_string().contains("not found")
                || inspect_err.to_string().contains("404"),
            "unexpected inspect error: {}",
            inspect_err
        );
    }

    #[test]
    fn p7_kill_run_requires_runtime_container_ids() {
        let _runtime_guard = lock_runtime_control_tests();
        let (_root, run_dir) =
            create_run_dir("bucephalus_p7_kill_runtime_missing_container", "run_1");
        write_resolved_experiment(&run_dir, "cli_events", true);
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");
        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);

        let runtime_state = runtime_trial_attempt_state_fixture(TrialPhase::AgentRunning);
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &runtime_state)
            .expect("db runtime state");

        let active_trials = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_parallel_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: None,
        }];
        write_run_control(&run_dir, "run_1", "running", &active_trials, None).expect("control");

        let err = kill_run(&run_dir).expect_err("kill should fail");
        assert!(
            err.to_string().contains("kill_missing_runtime_container"),
            "unexpected error: {}",
            err
        );

        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        assert_eq!(
            run_control.pointer("/status").and_then(Value::as_str),
            Some("interrupted")
        );
        let active = run_control
            .pointer("/active_trials")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        assert_eq!(active.len(), 1);
        assert!(active.contains_key("trial_1"));

        let trial_state = load_json_file(&trial_state_path(&trial_dir)).expect("trial state");
        assert_eq!(
            trial_state.pointer("/status").and_then(Value::as_str),
            Some("running")
        );
    }

    #[test]
    fn p7_kill_run_routes_modal_runtime_state_to_modal_cleanup_backend() {
        let _runtime_guard = lock_runtime_control_tests();
        let _modal_env = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_APP_NAME", Some("bucephalus-test")),
            ("BUCEPHALUS_MODAL_LAUNCHER", Some("modal-sandbox-launcher")),
        ]);
        let (_root, run_dir) = create_run_dir("bucephalus_p7_kill_modal_backend_cleanup", "run_1");
        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &RunExecutionOptions {
                executor: Some(ExecutorKind::Modal),
                ..RunExecutionOptions::default()
            },
        )
        .expect("run session");
        trial::state::write_trial_attempt_state(
            &trial_dir,
            &runtime_trial_attempt_state_fixture(TrialPhase::AgentRunning),
        )
        .expect("write runtime state");

        let active_trials = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_parallel_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: None,
        }];
        write_run_control(&run_dir, "run_1", "running", &active_trials, None).expect("control");

        let err = kill_run(&run_dir).expect_err("modal cleanup should require worker ids");
        assert!(
            err.to_string()
                .contains("modal_cleanup_missing_runtime_worker"),
            "unexpected error: {}",
            err
        );

        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        assert_eq!(
            run_control.pointer("/status").and_then(Value::as_str),
            Some("interrupted")
        );
    }

    #[test]
    fn cleanup_trial_owned_containers_errors_when_runtime_state_has_no_container_ids() {
        let _runtime_guard = lock_runtime_control_tests();
        let (_root, run_dir) =
            create_run_dir("bucephalus_cleanup_missing_runtime_container", "run_1");
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");
        let trial_dir = run_dir.join("trials").join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        trial::state::write_trial_attempt_state(
            &trial_dir,
            &runtime_trial_attempt_state_fixture(TrialPhase::AgentRunning),
        )
        .expect("write runtime state");

        let err = cleanup_trial_runtime_required(&run_dir, "run_1", "trial_1", &trial_dir)
            .expect_err("cleanup should not silently succeed without persisted container ids");
        assert!(
            err.to_string()
                .contains("cleanup_missing_runtime_container"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn cleanup_trial_owned_runtime_requires_run_session_state() {
        let (_root, run_dir) = create_run_dir("bucephalus_cleanup_missing_run_session", "run_1");
        let trial_dir = run_dir.join("trials").join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");

        let err = cleanup_trial_runtime_required(&run_dir, "run_1", "trial_1", &trial_dir)
            .expect_err("cleanup should require persisted run session state");
        assert!(
            err.to_string()
                .contains("run_session_state_v1 not found in runtime store"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn cleanup_trial_owned_runtime_removes_labeled_ephemeral_networks_without_containers() {
        let _runtime_guard = lock_runtime_control_tests();
        if !docker_runtime_available() {
            return;
        }
        let (_root, run_dir) = create_run_dir("bucephalus_cleanup_ephemeral_network", "run_1");
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");
        let trial_dir = run_dir.join("trials").join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        let docker = crate::backend::docker::DockerRuntime::connect().expect("docker runtime");
        let network_name = format!("bucephalus_test_network_{}", std::process::id());
        let mut labels = BTreeMap::new();
        labels.insert("bucephalus.run_id".to_string(), "run_1".to_string());
        labels.insert("bucephalus.trial_id".to_string(), "trial_1".to_string());
        labels.insert("bucephalus.role".to_string(), "ephemeral_network".to_string());
        docker
            .create_network(&network_name, true, labels)
            .expect("create labeled network");

        cleanup_trial_runtime_required(&run_dir, "run_1", "trial_1", &trial_dir)
            .expect("cleanup should remove labeled orphan network");

        let remaining = docker
            .list_networks_by_labels(&[
                "bucephalus.run_id=run_1".to_string(),
                "bucephalus.trial_id=trial_1".to_string(),
            ])
            .expect("list networks");
        assert!(
            remaining.is_empty(),
            "orphan ephemeral network should be removed"
        );
    }

    #[test]
    fn p7_kill_run_partial_runtime_failure_sets_interrupted_and_keeps_active_trial() {
        let _runtime_guard = lock_runtime_control_tests();
        let (_root, run_dir) = create_run_dir("bucephalus_p7_kill_partial_runtime_failure", "run_1");
        write_resolved_experiment(&run_dir, "cli_events", true);
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");
        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);

        let active_trials = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_parallel_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: None,
        }];
        write_run_control(&run_dir, "run_1", "running", &active_trials, None).expect("control");

        let err = kill_run(&run_dir).expect_err("kill should fail");
        assert!(
            err.to_string().contains("kill_partial_failure"),
            "unexpected error: {}",
            err
        );

        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        assert_eq!(
            run_control.pointer("/status").and_then(Value::as_str),
            Some("interrupted")
        );
        let active = run_control
            .pointer("/active_trials")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        assert_eq!(active.len(), 1);
        assert!(active.contains_key("trial_1"));

        let trial_state = load_json_file(&trial_state_path(&trial_dir)).expect("trial state");
        assert_eq!(
            trial_state.pointer("/status").and_then(Value::as_str),
            Some("running")
        );
    }

    #[test]
    fn p7_resume_trial_unpauses_persisted_runtime_container_without_forking() {
        let _runtime_guard = lock_runtime_control_tests();
        if !docker_runtime_available() {
            return;
        }
        if let Err(err) = try_ensure_docker_test_image("python:3.11-slim") {
            eprintln!("skipping persisted runtime resume test: {err}");
            return;
        }

        let (_root, run_dir) = create_run_dir("bucephalus_p7_resume_runtime_state", "run_1");
        write_resolved_experiment(&run_dir, "cli_events", true);
        let trial_dir = seed_parent_trial(
            &run_dir,
            "trial_1",
            json!([]),
            "paused",
            Some("docker_pause"),
        );

        let docker = crate::backend::docker::DockerRuntime::connect().expect("docker runtime");
        let handle = match docker.create_and_start_container_checked(
            &crate::backend::docker::ContainerSpec::idle("python:3.11-slim"),
            "test idle container",
        ) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!("skipping persisted runtime resume test: {err}");
                return;
            }
        };
        if let Err(err) = docker.pause_container(&handle) {
            let _ = docker.remove_container(&handle, true);
            eprintln!("skipping persisted runtime resume test: {err}");
            return;
        }

        let mut runtime_state = runtime_trial_attempt_state_with_task_container(
            TrialPhase::Paused,
            &handle.container_id,
        );
        runtime_state.paused_from_phase = Some(TrialPhase::AgentRunning);
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &runtime_state)
            .expect("db runtime state");

        let active_trials = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_parallel_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: None,
        }];
        write_run_control(&run_dir, "run_1", "paused", &active_trials, None).expect("control");

        let resumed = resume_trial(&run_dir, None, None, &BTreeMap::new(), false)
            .expect("resume should succeed");
        assert_eq!(resumed.trial_id, "trial_1");
        assert!(matches!(resumed.mode, ResumeMode::RuntimeUnpause));
        assert!(resumed.selector.is_none());
        assert!(resumed.fork.is_none());

        let run_control = load_json_file(&run_control_path(&run_dir)).expect("run control");
        assert_eq!(
            run_control.pointer("/status").and_then(Value::as_str),
            Some("running")
        );

        let trial_state = load_json_file(&trial_state_path(&trial_dir)).expect("trial state");
        assert_eq!(
            trial_state.pointer("/status").and_then(Value::as_str),
            Some("running")
        );

        let runtime_state =
            trial::state::load_trial_attempt_state(&trial_dir).expect("runtime state");
        assert_eq!(runtime_state.state.phase, TrialPhase::AgentRunning);
        assert_eq!(runtime_state.state.paused_from_phase, None);

        let inspected = docker
            .inspect_container(&handle)
            .expect("inspect resumed container");
        assert_eq!(inspected.status.as_deref(), Some("running"));

        let _ = docker.remove_container(&handle, true);
    }

    fn p7_trial_result_with_trial_record(schedule_idx: usize) -> TrialExecutionResult {
        let trial_id = format!("trial_{}", schedule_idx + 1);
        let mut result = TrialExecutionResult::minimal(trial_id.clone(), "completed", Some(0));
        result.deferred_trial_records.push(TrialRecord {
            run_id: "run_1".to_string(),
            trial_id,
            schedule_idx,
            slot_commit_id: String::new(),
            attempt: 0,
            row_seq: 0,
            baseline_id: "base".to_string(),
            workload_type: "agent_harness".to_string(),
            variant_id: "base".to_string(),
            task_index: schedule_idx,
            task_id: format!("task_{}", schedule_idx),
            repl_idx: 0,
            outcome: "success".to_string(),
            success: true,
            status_code: "0".to_string(),
            integration_level: "cli_basic".to_string(),
            network_mode_requested: "none".to_string(),
            network_mode_effective: "none".to_string(),
            primary_metric_name: "success".to_string(),
            primary_metric_value: json!(1.0),
            metrics: json!({"success": 1.0, "status_code": "0"}),
            bindings: json!({}),
            events_total: 0,
            has_events: false,
        });
        result
    }

    struct FlushFailRunSink;

    impl RunSink for FlushFailRunSink {
        fn write_run_manifest(&mut self, _run: &RunManifestRecord) -> Result<()> {
            Ok(())
        }

        fn write_metric_definitions(&mut self, _rows: &[MetricDefinitionRecord]) -> Result<()> {
            Ok(())
        }

        fn append_trial_record(&mut self, _row: &TrialRecord) -> Result<()> {
            Ok(())
        }

        fn append_metric_rows(&mut self, _rows: &[MetricRow]) -> Result<()> {
            Ok(())
        }

        fn append_event_rows(&mut self, _rows: &[EventRow]) -> Result<()> {
            Ok(())
        }

        fn append_contract_stage_rows(&mut self, _rows: &[ContractStageRow]) -> Result<()> {
            Ok(())
        }

        fn append_variant_snapshot(&mut self, _rows: &[VariantSnapshotRow]) -> Result<()> {
            Ok(())
        }

        fn flush(&mut self) -> Result<()> {
            Err(anyhow::anyhow!("flush_failed"))
        }
    }

    #[test]
    fn p7_commit_trial_slot_treats_sink_flush_failure_as_non_authoritative_mirror_failure() {
        let (_root, run_dir) = create_run_dir("bucephalus_p7_commit_flush_fail", "run_1");
        ensure_dir(&run_dir.join("runtime")).expect("runtime dir");

        let mut schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v1".to_string(),
            run_id: "run_1".to_string(),
            total_slots: 1,
            next_schedule_index: 0,
            next_trial_index: 0,
            schedule: vec![test_trial_slot(0)],
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: "2026-02-22T00:00:00Z".to_string(),
        };
        write_schedule_progress(&run_dir, &schedule_progress).expect("progress");

        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut slot_attempts: HashMap<usize, usize> = HashMap::new();
        let trial_result =
            TrialExecutionResult::minimal("trial_1".to_string(), "completed", Some(0));
        let mut sink = FlushFailRunSink;
        let mut commit_state = test_slot_commit_state(
            &mut schedule_progress,
            1,
            &mut pruned_variants,
            &mut consecutive_failures,
        );
        RunCoordinator::commit_trial_slot(
            &run_dir,
            &PolicyConfig::default(),
            &mut commit_state,
            0,
            &trial_result,
            &mut sink,
            &mut slot_attempts,
        )
        .expect("mirror flush failure should not roll back authoritative slot commit");
        assert_eq!(schedule_progress.next_schedule_index, 1);
        assert!(
            schedule_progress
                .completed_slots
                .iter()
                .any(|slot| slot.schedule_index == 0),
            "slot should commit even if a non-authoritative sink mirror fails"
        );

        let persisted = load_schedule_progress(&run_dir).expect("load persisted progress");
        assert_eq!(persisted.next_schedule_index, 1);
        assert_eq!(persisted.completed_slots.len(), 1);
        let slot = BackingSqliteStore::open(&run_dir)
            .expect("store")
            .schedule_slot("run_1", 0)
            .expect("slot query")
            .expect("slot row");
        assert_eq!(slot.state, "committed");
        assert_eq!(slot.trial_id.as_deref(), Some("trial_1"));
    }

    #[test]
    fn p7_commit_trial_slot_persists_trial_conclusion_rows() {
        let (_root, run_dir) = create_run_dir("bucephalus_p7_commit_trial_conclusions", "run_1");
        ensure_dir(&run_dir.join("runtime")).expect("runtime dir");

        let mut schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "run_1".to_string(),
            total_slots: 1,
            next_schedule_index: 0,
            next_trial_index: 0,
            schedule: vec![test_trial_slot(0)],
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: "2026-02-22T00:00:00Z".to_string(),
        };
        write_schedule_progress(&run_dir, &schedule_progress).expect("progress");

        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut slot_attempts: HashMap<usize, usize> = HashMap::new();
        let mut trial_result =
            TrialExecutionResult::minimal("trial_1".to_string(), "completed", Some(0));
        trial_result.deferred_trial_conclusion_records.push(json!({
            "schema_version": "trial_conclusion_v1",
            "payload": { "resolved": 1.0 },
            "reported_outcome": "success",
            "primary_metric": { "name": "resolved", "value": 1.0 },
            "grader": { "name": "test_grader", "strategy": "in_task_runtime" }
        }));

        let mut run_sink = BufferedRunSink::default();
        let mut commit_state = test_slot_commit_state(
            &mut schedule_progress,
            1,
            &mut pruned_variants,
            &mut consecutive_failures,
        );
        RunCoordinator::commit_trial_slot(
            &run_dir,
            &PolicyConfig::default(),
            &mut commit_state,
            0,
            &trial_result,
            &mut run_sink,
            &mut slot_attempts,
        )
        .expect("commit trial slot");

        let store = BackingSqliteStore::open(&run_dir).expect("open sqlite store");
        assert_eq!(
            store
                .row_count("trial_conclusion_rows")
                .expect("conclusion row count"),
            1,
            "expected one persisted trial conclusion row"
        );
        let row = load_sqlite_json_row(&run_dir, "trial_conclusion_rows", "run_1");
        assert_eq!(
            row.pointer("/schema_version").and_then(Value::as_str),
            Some("trial_conclusion_v1")
        );
        assert_eq!(
            row.pointer("/schedule_idx").and_then(Value::as_u64),
            Some(0)
        );
        assert_eq!(row.pointer("/attempt").and_then(Value::as_u64), Some(1));
        assert_eq!(row.pointer("/row_seq").and_then(Value::as_u64), Some(0));
        assert!(
            row.pointer("/slot_commit_id")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty()),
            "slot_commit_id should be annotated onto persisted conclusion rows"
        );
    }

    #[test]
    fn p7_commit_trial_slot_marks_runtime_state_committed() {
        let (_root, run_dir) = create_run_dir("bucephalus_p7_commit_runtime_state", "run_1");
        ensure_dir(&run_dir.join("runtime")).expect("runtime dir");

        let trial_dir = run_dir.join("trials").join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        trial::state::write_trial_attempt_state(
            &trial_dir,
            &runtime_trial_attempt_state_fixture(TrialPhase::CommitPending),
        )
        .expect("write runtime state");

        let mut schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "run_1".to_string(),
            total_slots: 1,
            next_schedule_index: 0,
            next_trial_index: 0,
            schedule: vec![test_trial_slot(0)],
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: "2026-02-22T00:00:00Z".to_string(),
        };
        write_schedule_progress(&run_dir, &schedule_progress).expect("progress");

        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut slot_attempts: HashMap<usize, usize> = HashMap::new();
        let trial_result =
            TrialExecutionResult::minimal("trial_1".to_string(), "completed", Some(0));
        let mut run_sink = BufferedRunSink::default();
        let mut commit_state = test_slot_commit_state(
            &mut schedule_progress,
            1,
            &mut pruned_variants,
            &mut consecutive_failures,
        );
        RunCoordinator::commit_trial_slot(
            &run_dir,
            &PolicyConfig::default(),
            &mut commit_state,
            0,
            &trial_result,
            &mut run_sink,
            &mut slot_attempts,
        )
        .expect("commit trial slot");

        let persisted = trial::state::load_trial_attempt_state(&trial_dir).expect("load state");
        assert_eq!(persisted.state.phase, TrialPhase::Committed);
    }

    fn p7_commit_trial_rows_for_arrival_order(
        prefix: &str,
        arrival_order: &[usize],
    ) -> (Vec<String>, Vec<usize>) {
        let (_root, run_dir) = create_run_dir(prefix, "run_1");
        let slot_count = arrival_order.len();
        let mut schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v1".to_string(),
            run_id: "run_1".to_string(),
            total_slots: slot_count,
            next_schedule_index: 0,
            next_trial_index: slot_count,
            schedule: test_trial_schedule(slot_count),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let policy_config = PolicyConfig::default();
        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut run_sink = BufferedRunSink::default();
        let mut committer = DeterministicCommitter::from_progress(&schedule_progress, &[]);

        for schedule_idx in arrival_order {
            let inserted = committer
                .enqueue_trial(
                    *schedule_idx,
                    p7_trial_result_with_trial_record(*schedule_idx),
                )
                .expect("enqueue trial");
            assert!(inserted, "arrival order should not contain duplicates");
            let mut commit_state = test_slot_commit_state(
                &mut schedule_progress,
                slot_count,
                &mut pruned_variants,
                &mut consecutive_failures,
            );
            let _ = committer
                .drain_ready(&run_dir, &policy_config, &mut commit_state, &mut run_sink)
                .expect("drain ready");
        }
        let mut commit_state = test_slot_commit_state(
            &mut schedule_progress,
            slot_count,
            &mut pruned_variants,
            &mut consecutive_failures,
        );
        let _ = committer
            .drain_ready(&run_dir, &policy_config, &mut commit_state, &mut run_sink)
            .expect("final drain");

        let committed_trial_ids = run_sink
            .trial_records
            .iter()
            .map(|row| row.trial_id.clone())
            .collect::<Vec<_>>();
        let committed_schedule_idx = schedule_progress
            .completed_slots
            .iter()
            .map(|slot| slot.schedule_index)
            .collect::<Vec<_>>();
        (committed_trial_ids, committed_schedule_idx)
    }

    #[test]
    fn p7_parallel_completions_commit_to_slots_without_normalizing_arrival_order() {
        let serial_arrivals = [0usize, 1, 2, 3];
        let parallel_arrivals = [2usize, 0, 3, 1];

        let (_serial_trial_ids, serial_commit_idx) =
            p7_commit_trial_rows_for_arrival_order("bucephalus_p7_serial_parity", &serial_arrivals);
        let (parallel_trial_ids, parallel_commit_idx) = p7_commit_trial_rows_for_arrival_order(
            "bucephalus_p7_parallel_parity",
            &parallel_arrivals,
        );

        assert_eq!(serial_commit_idx, vec![0, 1, 2, 3]);
        assert_eq!(parallel_commit_idx, serial_commit_idx);
        assert_eq!(
            parallel_trial_ids,
            vec![
                "trial_3".to_string(),
                "trial_1".to_string(),
                "trial_4".to_string(),
                "trial_2".to_string()
            ],
            "facts should commit as completions arrive while progress remains addressable by slot"
        );
    }

    #[test]
    fn p7_out_of_order_completion_commits_directly_to_its_slot() {
        let (_root, run_dir) = create_run_dir("bucephalus_p7_pending_recovery", "run_1");
        let slot_count = 2usize;
        let mut schedule_progress = ScheduleProgress {
            schema_version: "schedule_progress_v1".to_string(),
            run_id: "run_1".to_string(),
            total_slots: slot_count,
            next_schedule_index: 0,
            next_trial_index: slot_count,
            schedule: test_trial_schedule(slot_count),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let policy_config = PolicyConfig::default();
        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut run_sink = BufferedRunSink::default();

        let mut committer = DeterministicCommitter::from_progress(&schedule_progress, &[]);
        committer
            .enqueue_trial(1, p7_trial_result_with_trial_record(1))
            .expect("enqueue slot 1 result");
        let mut commit_state = test_slot_commit_state(
            &mut schedule_progress,
            slot_count,
            &mut pruned_variants,
            &mut consecutive_failures,
        );
        let committed = committer
            .drain_ready(&run_dir, &policy_config, &mut commit_state, &mut run_sink)
            .expect("drain slot 1");
        assert_eq!(committed, 1, "slot 1 should commit without waiting for slot 0");
        assert_eq!(schedule_progress.next_schedule_index, 2);
        assert_eq!(
            schedule_progress
                .completed_slots
                .iter()
                .map(|slot| slot.schedule_index)
                .collect::<Vec<_>>(),
            vec![1]
        );
        let pending_records = committer.pending_trial_completion_records();
        assert!(
            pending_records.is_empty(),
            "slot-addressed committer should not retain prefix-blocked pending records"
        );
        persist_pending_trial_completions(&run_dir, &pending_records).expect("persist pending");

        let journal_records = load_slot_commit_records(&run_dir).expect("load journal");
        let mut restarted =
            DeterministicCommitter::from_progress(&schedule_progress, &journal_records);
        let persisted = load_pending_trial_completion_records(&run_dir).expect("load pending");
        assert!(
            persisted.is_empty(),
            "no prefix-blocked pending completion should persist across restart"
        );
        for (schedule_idx, result) in persisted {
            restarted
                .enqueue_trial(schedule_idx, result)
                .expect("re-enqueue persisted completion");
        }
        restarted
            .enqueue_trial(
                0,
                TrialExecutionResult::worker_lost(
                    "trial_1".to_string(),
                    Some(0),
                    Some("worker_lost".to_string()),
                ),
            )
            .expect("enqueue recovered slot 0");

        let mut commit_state = test_slot_commit_state(
            &mut schedule_progress,
            slot_count,
            &mut pruned_variants,
            &mut consecutive_failures,
        );
        let committed_after_restart = restarted
            .drain_ready(&run_dir, &policy_config, &mut commit_state, &mut run_sink)
            .expect("drain after restart");
        assert_eq!(
            committed_after_restart, 1,
            "only recovered slot 0 should need to commit after restart"
        );
        assert_eq!(schedule_progress.next_schedule_index, 2);
        assert_eq!(
            schedule_progress
                .completed_slots
                .iter()
                .map(|slot| slot.schedule_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(
            run_sink
                .trial_records
                .iter()
                .any(|row| row.schedule_idx == 1 && row.trial_id == "trial_2"),
            "slot 1 completion should appear in committed facts"
        );
    }

    #[test]
    fn p7_release_gate_rejects_non_isolate_state_policy() {
        let (_root, run_dir) = create_run_dir("bucephalus_p7_release_gate", "run_1");
        write_run_control(&run_dir, "run_1", "paused", &[], None).expect("run control");
        let trials_dir = run_dir.join("trials");
        ensure_dir(&trials_dir).expect("trials dir");

        let mut schedule_progress = new_schedule_progress("run_1", &[]);
        let mut trial_index = 0_usize;
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut run_sink = SqliteRunJournal::new(&run_dir).expect("sink");
        let policy_config = PolicyConfig {
            state: StatePolicy::PersistPerTask,
            ..PolicyConfig::default()
        };
        let evaluation_config = EvaluationConfig::default();
        let mut request = test_schedule_engine_request(
            &run_dir,
            &trials_dir,
            &[],
            &[],
            &[],
            &policy_config,
            &evaluation_config,
        );
        request.materialize_mode = MaterializationMode::Full;
        request.max_concurrency = 4;
        let err = execute_schedule_engine(
            request,
            test_schedule_engine_state(
                &mut schedule_progress,
                &mut trial_index,
                &mut consecutive_failures,
                &mut pruned_variants,
                &mut run_sink,
            ),
        )
        .expect_err("non-isolate policy should be rejected by the release gate");
        assert!(
            err.to_string().contains("supports only isolate_per_trial"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn parse_task_boundary_extracts_runtime_fields() {
        let task = task_row_value(
            "task_1",
            "python:3.11-slim",
            "/workspace/task",
            Some(120_000),
        );

        let parsed = parse_task_boundary_from_packaged_task(&task).expect("parse boundary");
        assert_eq!(
            parsed
                .task_payload
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "task_1"
        );
        assert_eq!(parsed.task_image, "python:3.11-slim");
        assert_eq!(parsed.task_workdir, "/workspace/task");
        assert!(parsed.workspace.overlays.is_empty());
        assert!(parsed.workspace.aux_mounts.is_empty());
        assert_eq!(parsed.time_limit_ms, Some(120_000));
    }

    #[test]
    fn parse_task_case_boundary_exposes_named_inputs_and_workspace_resource() {
        let case = json!({
            "schema_version": "case_v1",
            "id": "case_image_1",
            "inputs": {
                "prompt": "Describe this image.",
                "image": {
                    "type": "file",
                    "path": "images/case_image_1.png",
                    "media_type": "image/png"
                },
                "metadata": {
                    "difficulty": "hard"
                }
            },
            "resources": {
                "workspace": {
                    "type": "container_image",
                    "image": "ghcr.io/acme/vision-task:latest",
                    "workdir": "/workspace/task",
                    "platform": "linux/amd64"
                }
            },
            "metadata": {
                "suite": "vision"
            },
            "limits": {
                "timeout_ms": 90000
            }
        });

        let parsed = parse_task_boundary_from_packaged_task(&case).expect("parse case boundary");

        assert_eq!(parsed.task_id, "case_image_1");
        assert_eq!(
            parsed.task_payload.pointer("/inputs/prompt").and_then(Value::as_str),
            Some("Describe this image.")
        );
        assert_eq!(
            parsed
                .task_payload
                .pointer("/inputs/image/media_type")
                .and_then(Value::as_str),
            Some("image/png")
        );
        assert_eq!(parsed.task_image, "ghcr.io/acme/vision-task:latest");
        assert_eq!(parsed.task_workdir, "/workspace/task");
        assert_eq!(
            parsed.materialization.platform.as_deref(),
            Some("linux/amd64")
        );
        assert_eq!(parsed.time_limit_ms, Some(90000));
    }

    #[test]
    fn parse_task_case_boundary_allows_pure_data_case_without_workspace_resource() {
        let case = json!({
            "schema_version": "case_v1",
            "id": "case_json_1",
            "inputs": {
                "request": {
                    "prompt": "Classify this.",
                    "choices": ["A", "B", "C"]
                }
            }
        });

        let parsed = parse_task_boundary_from_packaged_task(&case).expect("parse data-only case");

        assert_eq!(parsed.task_id, "case_json_1");
        assert_eq!(parsed.task_image, "");
        assert_eq!(parsed.task_workdir, "");
        assert_eq!(
            parsed
                .task_payload
                .pointer("/inputs/request/choices/1")
                .and_then(Value::as_str),
            Some("B")
        );
    }

    #[test]
    fn parse_case_v2_lowers_container_workspace_without_task_row_shape() {
        let case = json!({
            "schema_version": "case_v2",
            "id": "case_v2_container",
            "inputs": {
                "prompt": "Fix the test."
            },
            "resources": {
                "workspace": {
                    "source": "container_image",
                    "mode": "patch",
                    "image": "python:3.11-slim",
                    "workdir": "/workspace/task",
                    "platform": "linux/amd64"
                }
            },
            "metadata": {
                "suite": "migration"
            },
            "limits": {
                "timeout_ms": 120000
            }
        });

        let parsed = parse_task_boundary_from_packaged_task(&case).expect("parse case_v2");

        assert_eq!(parsed.task_id, "case_v2_container");
        assert_eq!(parsed.task_image, "python:3.11-slim");
        assert_eq!(parsed.task_workdir, "/workspace/task");
        assert_eq!(parsed.workspace.mode, WorkspaceMode::Patch);
        assert_eq!(parsed.workspace.base.kind, WorkspaceBaseKind::Empty);
        assert_eq!(
            parsed.task_payload.pointer("/inputs/prompt").and_then(Value::as_str),
            Some("Fix the test.")
        );
        assert_eq!(parsed.materialization.platform.as_deref(), Some("linux/amd64"));
        assert_eq!(parsed.time_limit_ms, Some(120000));
    }

    #[test]
    fn parse_case_v2_lowers_dataset_pack_workspace_boundary() {
        let case = json!({
            "schema_version": "case_v2",
            "id": "case_v2_pack",
            "inputs": {
                "prompt": "Use the mounted files."
            },
            "resources": {
                "workspace": {
                    "source": "dataset_pack",
                    "mode": "scratch",
                    "dataset_pack_ref": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "overlays": [
                        {
                            "path": ".bucephalus/README.md",
                            "content": "hello",
                            "encoding": "utf8"
                        }
                    ],
                    "aux_mounts": [
                        {
                            "dataset_pack_ref": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                            "mount_path": "/bucephalus/workspace/support"
                        }
                    ]
                }
            }
        });

        let parsed = parse_task_boundary_from_packaged_task(&case).expect("parse case_v2 pack");

        assert_eq!(parsed.task_id, "case_v2_pack");
        assert_eq!(parsed.task_image, "");
        assert_eq!(parsed.workspace.base.kind, WorkspaceBaseKind::DatasetPack);
        assert_eq!(
            parsed.workspace.base.dataset_pack_ref.as_deref(),
            Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(parsed.workspace.overlays.len(), 1);
        assert_eq!(parsed.workspace.aux_mounts.len(), 1);
    }

    #[test]
    fn parse_case_v2_lowers_case_command_materialization() {
        let case = json!({
            "schema_version": "case_v2",
            "id": "case_v2_setup",
            "inputs": {},
            "resources": {
                "workspace": {
                    "source": "container_image",
                    "image": "python:3.11-slim",
                    "workdir": "/workspace/task"
                }
            },
            "materialization": [
                {
                    "id": "setup",
                    "stage": "case",
                    "operation": "command",
                    "command": ["bash", ".bucephalus/setup.sh"]
                }
            ]
        });

        let parsed =
            parse_task_boundary_from_packaged_task(&case).expect("case materialization command");

        assert_eq!(parsed.case_materialization.len(), 1);
        assert_eq!(parsed.case_materialization[0].id, "setup");
        assert_eq!(
            parsed.case_materialization[0].command,
            vec!["bash".to_string(), ".bucephalus/setup.sh".to_string()]
        );
    }

    #[test]
    fn prepare_task_environment_carries_case_v2_materialization_plan() {
        let (_root, paths) =
            create_trial_paths_fixture("bucephalus_prepare_case_v2_materialization");
        let case = json!({
            "schema_version": "case_v2",
            "id": "case_v2_setup",
            "inputs": { "prompt": "prepare then solve" },
            "resources": {
                "workspace": {
                    "source": "container_image",
                    "image": "python:3.11-slim",
                    "workdir": "/workspace/task"
                }
            },
            "materialization": [
                {
                    "id": "setup",
                    "stage": "case",
                    "operation": "command",
                    "command": ["bash", "-lc", "printf ready > .ready"],
                    "workdir": "/workspace/task",
                    "network": "none",
                    "timeout_ms": 5000
                }
            ]
        });
        let boundary = parse_task_boundary_from_packaged_task(&case).expect("case_v2 boundary");

        let prepared = prepare_task_environment_for_test(
            &paths.exp_dir,
            &paths.trial_dir,
            &task_runtime_experiment_fixture(600000),
            &base_variant_fixture(),
            &boundary,
            &legacy_contract_runtime_fixture(),
        )
        .expect("prepare case_v2 environment");
        let plan = prepared
            .manifest
            .task_sandbox_plan
            .as_ref()
            .expect("task sandbox plan");

        assert_eq!(prepared.trial_input.pointer("/case/inputs/prompt"), Some(&json!("prepare then solve")));
        assert!(prepared.trial_input.pointer("/task").is_none());
        assert_eq!(plan.case_materialization.len(), 1);
        assert_eq!(plan.case_materialization[0].id, "setup");
        assert_eq!(plan.case_materialization[0].stage, CaseMaterializationStage::Case);
        assert_eq!(
            plan.case_materialization[0].operation,
            CaseMaterializationOperation::Command
        );
        assert_eq!(
            plan.case_materialization[0].command,
            vec![
                "bash".to_string(),
                "-lc".to_string(),
                "printf ready > .ready".to_string()
            ]
        );
        assert_eq!(
            plan.case_materialization[0].workdir.as_deref(),
            Some("/workspace/task")
        );
        assert_eq!(plan.case_materialization[0].timeout_ms, Some(5000));
    }

    #[test]
    fn parse_case_v2_rejects_unsupported_materialization_operation() {
        let case = json!({
            "schema_version": "case_v2",
            "id": "case_v2_mount",
            "inputs": {},
            "resources": {
                "workspace": {
                    "source": "container_image",
                    "image": "python:3.11-slim",
                    "workdir": "/workspace/task"
                }
            },
            "materialization": [
                {
                    "id": "mount_fixture",
                    "stage": "case",
                    "operation": "mount",
                    "mount": { "path": "/workspace/task/fixtures", "read_only": true }
                }
            ]
        });

        let err = parse_task_boundary_from_packaged_task(&case)
            .expect_err("unsupported materialization operation");
        assert!(
            err.to_string().contains("operation=mount"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn parse_task_boundary_rejects_legacy_base_image_bundle_row() {
        let task = json!({
            "schema_version": "task_row_v1",
            "id": "task_1",
            "image": "registry.example.invalid/eval.x86_64.project__issue-12907:latest",
            "workdir": "/testbed",
            "task": {
                "id": "task_1",
                "prompt": "solve this"
            },
            "materialization": {
                "kind": "base_image_bundle",
                "task_bundle_ref": "tasks/task_1"
            }
        });

        let err = parse_task_boundary_from_packaged_task(&task)
            .expect_err("legacy base_image_bundle rows are no longer accepted");
        assert!(
            err.to_string().contains("task_row_v1"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn parse_task_boundary_rejects_unsupported_keys() {
        let task = json!({
            "schema_version": "task_row_v2",
            "id": "task_1",
            "task": { "id": "task_1" },
            "runtime": {
                "container_image": {
                    "image": "python:3.11-slim",
                    "workdir": "/workspace/task"
                }
            },
            "unsupported_kind": "custom_magic"
        });
        let err = parse_task_boundary_from_packaged_task(&task).expect_err("should fail");
        assert!(
            err.to_string().contains("unknown field"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn prepare_task_environment_records_base_image_bundle_without_legacy_workspace_copy() {
        let root = TempDirGuard::new("bucephalus_base_image_bundle_prepare");
        let bundle_dir = root
            .path
            .join("tasks")
            .join("task_bundles")
            .join("task_1_bundle");
        ensure_dir(&bundle_dir.join("src")).expect("bundle dir");
        fs::write(bundle_dir.join("src/main.py"), "print('ok')\n").expect("bundle file");

        let task_boundary = base_image_bundle_task_boundary(
            "task_1",
            "python:3.11-slim",
            "/workspace/task",
            "tasks/task_bundles/task_1_bundle",
        );
        let variant = preflight_test_variant();
        let runtime = legacy_contract_runtime_fixture();
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");

        let prepared = prepare_task_environment_for_test(
            &root.path,
            &trial_dir,
            &task_runtime_experiment_fixture(30000),
            &variant,
            &task_boundary,
            &runtime,
        )
        .expect("prepare task environment");

        assert!(
            !prepared.trial_paths.workspace.join("src/main.py").exists(),
            "base_image_bundle must not fall back to legacy host workspace materialization"
        );
        assert!(
            prepared.dynamic_mounts.is_empty(),
            "base_image_bundle should not produce legacy aux mounts"
        );
        let task_sandbox_plan = prepared
            .manifest
            .task_sandbox_plan
            .as_ref()
            .expect("task sandbox plan");
        assert_eq!(task_sandbox_plan.image, "python:3.11-slim");
        assert_eq!(task_sandbox_plan.workdir, "/workspace/task");
        assert_eq!(
            task_sandbox_plan.materialization.kind,
            TaskMaterializationKind::BaseImageBundle
        );
        assert_eq!(task_sandbox_plan.io_mounts.in_dir, BUCEPHALUS_CONTRACT_IN_DIR);
        assert_eq!(
            task_sandbox_plan.io_mounts.out_dir,
            BUCEPHALUS_CONTRACT_OUT_DIR
        );
        assert!(task_sandbox_plan.artifact_mount.is_none());
        assert_eq!(task_sandbox_plan.time_limit_ms, 30_000);
    }

    #[test]
    fn build_agent_task_uses_run_id_and_limits_without_embedding_setup_manifest() {
        let root = TempDirGuard::new("bucephalus_task_boundary_trial_input");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp");
        fs::write(exp_dir.join("harness.sh"), "#!/bin/sh\n").expect("harness");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial");
        let paths = TrialPaths::new(&trial_dir, &exp_dir).expect("paths");
        paths.prepare(true).expect("prepare");

        let json_value = json!({
            "runtime": {
                "network": {
                    "task_sandbox": "none"
                }
            },
            "trial_runtime": {
                "agent": {
                    "artifact_type": "structured_json"
                }
            }
        });
        let variant = Variant {
            id: "baseline".to_string(),
            bindings: json!({ "model": "demo" }),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let task_boundary = runtime_task_boundary(
            json!({ "id": "task_1", "prompt": "x" }),
            "python:3.11-slim",
            BUCEPHALUS_CONTRACT_WORKSPACE_DIR,
            Some(90_000),
        );

        let input = build_trial_input(
            &json_value,
            "run_actual_1",
            "trial_1",
            &variant,
            0,
            "cli_basic",
            &task_boundary,
        )
        .expect("trial input");

        assert_eq!(
            input
                .pointer("/ids/run_id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "run_actual_1"
        );
        assert_eq!(
            input
                .pointer("/runtime/time_limit_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            90000
        );
        assert!(
            input.pointer("/bindings").is_none(),
            "agent-facing trial_input must not carry variant bindings"
        );
        assert!(
            input.pointer("/ext/task_boundary").is_none(),
            "agent-facing trial_input must not carry a task setup manifest"
        );
    }

    #[test]
    fn build_trial_input_exposes_case_payload() {
        let task_payload = json!({
            "id": "task_1",
            "input": { "prompt": "canonical prompt" },
            "prompt": "different top-level prompt",
            "workload": {
                "input": { "prompt": "different nested prompt" }
            }
        });
        let task_boundary = TaskBoundaryMaterialization {
            declaration: json!({"schema_version": "task_row_v2"}),
            task_payload: task_payload.clone(),
            workspace: WorkspaceSpec {
                mode: WorkspaceMode::Scratch,
                base: WorkspaceBaseSpec {
                    kind: WorkspaceBaseKind::Empty,
                    dataset_pack_ref: None,
                    repo: None,
                    commit: None,
                },
                overlays: Vec::new(),
                aux_mounts: Vec::new(),
            },
            dependencies: json!({}),
            materialization: TaskMaterializationSpec {
                kind: TaskMaterializationKind::TaskImage,
                task_bundle_ref: None,
                platform: None,
            },
            case_materialization: Vec::new(),
            task_id: "task_1".to_string(),
            task_image: "python:3.11-slim".to_string(),
            task_workdir: "/workspace/task".to_string(),
            time_limit_ms: None,
        };
        let input = build_trial_input(
            &json!({
                "policy": {
                    "timeout_ms": 600000,
                    "task_sandbox": { "network": "none", "allowed_hosts": [] },
                    "sanitization_profile": "hermetic_functional"
                },
                "runtime": {
                    "network": {
                        "task_sandbox": "none"
                    }
                },
                "trial_runtime": {
                    "agent": { "artifact_type": "structured_json" }
                }
            }),
            "run_1",
            "trial_1",
            &base_variant_fixture(),
            0,
            "cli_basic",
            &task_boundary,
        )
        .expect("trial input");

        assert_eq!(input.pointer("/case"), Some(&task_payload));
        assert!(input.pointer("/task").is_none());
    }

    #[test]
    fn build_trial_input_requires_explicit_task_network() {
        let task_boundary = runtime_task_boundary(
            json!({ "id": "task_1", "prompt": "x" }),
            "python:3.11-slim",
            BUCEPHALUS_CONTRACT_WORKSPACE_DIR,
            Some(90_000),
        );
        let err = build_trial_input(
            &json!({
                "trial_runtime": {
                    "agent": { "artifact_type": "structured_json" }
                }
            }),
            "run_1",
            "trial_1",
            &base_variant_fixture(),
            0,
            "cli_basic",
            &task_boundary,
        )
        .expect_err("trial input must not invent a task network mode");
        assert!(
            err.to_string()
                .contains("missing /runtime/network/task_sandbox"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn prepare_task_environment_materializes_packaged_case_file_inputs() {
        let _lock = lock_modal_env_tests();
        let root = create_dx_authoring_fixture("bucephalus_prepare_task_case_assets");
        let data_dir = root
            .path
            .join(".lab")
            .join("experiments")
            .join("data");
        let image_dir = data_dir.join("images");
        ensure_dir(&image_dir).expect("image dir");
        let image_bytes = vec![b'i'; 8 * 1024 * 1024 + 1];
        fs::write(image_dir.join("case001.png"), &image_bytes).expect("case image");
        fs::write(
            data_dir.join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"case_v1","id":"CASE001","inputs":{"image":{"type":"file","path":"images/case001.png","media_type":"image/png"}},"resources":{"workspace":{"type":"container_image","image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("task case dataset");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build case package");
        let packaged_tasks =
            load_jsonl_value_rows(&build.package_dir.join("tasks").join("tasks.jsonl"))
                .expect("packaged tasks");
        let boundary = parse_task_boundary_from_packaged_task(&packaged_tasks[0])
            .expect("packaged case boundary");

        let trial_dir = root
            .path
            .join("runs")
            .join("run_1")
            .join("trials")
            .join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        let prepared = prepare_task_environment_for_test(
            &build.package_dir,
            &trial_dir,
            &task_runtime_experiment_fixture(600000),
            &base_variant_fixture(),
            &boundary,
            &legacy_contract_runtime_fixture(),
        )
        .expect("prepare task environment");

        let image_input = prepared
            .trial_input
            .pointer("/case/inputs/image")
            .and_then(Value::as_object)
            .expect("trial image input");
        let runtime_path = image_input
            .get("path")
            .and_then(Value::as_str)
            .expect("runtime image path");
        assert!(runtime_path.starts_with("/bucephalus/case_assets/"));
        assert!(image_input.get("package_path").is_none());
        assert!(image_input.get("uri").is_none());
        let mount = prepared
            .dynamic_mounts
            .iter()
            .find(|mount| mount.mount_path == runtime_path)
            .expect("case asset mount");
        assert!(mount.read_only);
        assert!(
            !mount.host_path.starts_with(&prepared.trial_paths.in_dir),
            "immutable case asset should not be copied into the per-trial input dir"
        );
        assert_eq!(
            fs::read(&mount.host_path).expect("projected image"),
            image_bytes
        );
    }

    #[test]
    fn prepare_task_environment_materializes_packaged_case_directory_inputs() {
        let _lock = lock_modal_env_tests();
        let root = create_dx_authoring_fixture("bucephalus_prepare_task_case_dir_assets");
        let data_dir = root
            .path
            .join(".lab")
            .join("experiments")
            .join("data");
        let attachment_dir = data_dir.join("attachments").join("case001");
        ensure_dir(&attachment_dir).expect("attachment dir");
        let prompt_bytes = vec![b'd'; 8 * 1024 * 1024 + 1];
        fs::write(attachment_dir.join("prompt.txt"), &prompt_bytes).expect("prompt file");
        fs::write(
            data_dir.join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"case_v1","id":"CASE001","inputs":{"attachments":{"type":"directory","path":"attachments/case001"}},"resources":{"workspace":{"type":"container_image","image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("task case dataset");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build case package");
        let packaged_tasks =
            load_jsonl_value_rows(&build.package_dir.join("tasks").join("tasks.jsonl"))
                .expect("packaged tasks");
        let boundary = parse_task_boundary_from_packaged_task(&packaged_tasks[0])
            .expect("packaged case boundary");

        let run_dir = root.path.join("runs").join("run_1");
        ensure_dir(&run_dir).expect("run dir");
        let trial_dir = run_dir.join("trial_1");
        let prepared = prepare_task_environment_for_test(
            &build.package_dir,
            &trial_dir,
            &task_runtime_experiment_fixture(600000),
            &base_variant_fixture(),
            &boundary,
            &legacy_contract_runtime_fixture(),
        )
        .expect("prepare task environment");

        let attachments = prepared
            .trial_input
            .pointer("/case/inputs/attachments")
            .and_then(Value::as_object)
            .expect("trial directory input");
        let runtime_path = attachments
            .get("path")
            .and_then(Value::as_str)
            .expect("runtime directory path");
        assert!(runtime_path.starts_with("/bucephalus/case_assets/"));
        let mount = prepared
            .dynamic_mounts
            .iter()
            .find(|mount| mount.mount_path == runtime_path)
            .expect("case directory mount");
        assert!(mount.read_only);
        assert!(
            !mount.host_path.starts_with(&prepared.trial_paths.in_dir),
            "immutable case directory should not be copied into the per-trial input dir"
        );
        assert_eq!(
            fs::read(mount.host_path.join("prompt.txt")).expect("projected directory file"),
            prompt_bytes
        );
        assert!(attachments.get("package_path").is_none());
        assert!(attachments.get("uri").is_none());

        let second = prepare_task_environment_for_test(
            &build.package_dir,
            &run_dir.join("trial_2"),
            &task_runtime_experiment_fixture(600000),
            &base_variant_fixture(),
            &boundary,
            &legacy_contract_runtime_fixture(),
        )
        .expect("prepare second task environment");
        let second_runtime_path = second
            .trial_input
            .pointer("/case/inputs/attachments/path")
            .and_then(Value::as_str)
            .expect("second runtime directory path");
        let second_mount = second
            .dynamic_mounts
            .iter()
            .find(|mount| mount.mount_path == second_runtime_path)
            .expect("second case directory mount");
        assert_eq!(
            mount.host_path, second_mount.host_path,
            "CAS-backed case directories should reuse a run-scoped immutable projection"
        );
    }

    #[test]
    fn prepare_task_environment_rejects_unsealed_task_case_asset_paths() {
        let root = TempDirGuard::new("bucephalus_prepare_unsealed_task_case_asset");
        let boundary = parse_task_boundary_from_packaged_task(&json!({
            "schema_version": "case_v1",
            "id": "CASE001",
            "inputs": {
                "image": {
                    "type": "file",
                    "path": "images/case001.png",
                    "media_type": "image/png"
                }
            },
            "resources": {
                "workspace": {
                    "type": "container_image",
                    "image": "python:3.11-slim",
                    "workdir": "/workspace/task"
                }
            }
        }))
        .expect("case boundary");

        let err = match prepare_task_environment_for_test(
            &root.path,
            &root.path.join("trial_1"),
            &json!({
                "runtime": { "network": { "task_sandbox": "none" } },
                "trial_runtime": {
                    "task": { "interface": "writable_workspace" },
                    "agent": { "artifact_type": "structured_json" }
                },
                "policy": { "timeout_ms": 600000 }
            }),
            &base_variant_fixture(),
            &boundary,
            &legacy_contract_runtime_fixture(),
        ) {
            Ok(_) => panic!("unsealed case asset path should not reach execution"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(msg.contains("not been sealed"), "unexpected error: {msg}");
        assert!(msg.contains("images/case001.png"), "unexpected error: {msg}");
    }


    #[test]
    fn schedule_variant_sequential_orders_variant_then_task_then_repl() {
        let slots = build_trial_schedule(2, 3, 2, SchedulingPolicy::VariantSequential, 1);
        assert_eq!(slots.len(), 12);

        for slot in &slots[0..6] {
            assert_eq!(slot.variant_idx, 0);
        }
        for slot in &slots[6..12] {
            assert_eq!(slot.variant_idx, 1);
        }

        assert_eq!(slots[0].task_idx, 0);
        assert_eq!(slots[0].repl_idx, 0);
        assert_eq!(slots[1].task_idx, 0);
        assert_eq!(slots[1].repl_idx, 1);
        assert_eq!(slots[2].task_idx, 1);
        assert_eq!(slots[2].repl_idx, 0);
    }

    #[test]
    fn schedule_paired_interleaved_orders_task_then_repl_then_variant() {
        let slots = build_trial_schedule(2, 3, 2, SchedulingPolicy::PairedInterleaved, 1);
        assert_eq!(slots.len(), 12);

        for slot in &slots[0..4] {
            assert_eq!(slot.task_idx, 0);
        }
        assert_eq!(slots[0].variant_idx, 0);
        assert_eq!(slots[0].repl_idx, 0);
        assert_eq!(slots[1].variant_idx, 1);
        assert_eq!(slots[1].repl_idx, 0);
        assert_eq!(slots[2].variant_idx, 0);
        assert_eq!(slots[2].repl_idx, 1);
        assert_eq!(slots[3].variant_idx, 1);
        assert_eq!(slots[3].repl_idx, 1);
    }

    #[test]
    fn schedule_paired_interleaved_pairs_variants_on_same_task() {
        let slots = build_trial_schedule(3, 4, 1, SchedulingPolicy::PairedInterleaved, 1);
        assert_eq!(slots.len(), 12);

        for task_idx in 0..4 {
            let task_slots: Vec<_> = slots.iter().filter(|s| s.task_idx == task_idx).collect();
            assert_eq!(task_slots.len(), 3);
            let variant_ids: Vec<_> = task_slots.iter().map(|s| s.variant_idx).collect();
            assert_eq!(variant_ids, vec![0, 1, 2]);
        }
    }

    #[test]
    fn schedule_randomized_contains_all_slots() {
        let slots = build_trial_schedule(2, 3, 2, SchedulingPolicy::Randomized, 42);
        assert_eq!(slots.len(), 12);

        let mut seen = HashSet::new();
        for slot in &slots {
            let key = (slot.variant_idx, slot.task_idx, slot.repl_idx);
            assert!(seen.insert(key), "duplicate slot: {:?}", key);
        }
        assert_eq!(seen.len(), 12);
    }

    #[test]
    fn schedule_randomized_is_deterministic_with_same_seed() {
        let a = build_trial_schedule(2, 4, 2, SchedulingPolicy::Randomized, 1337);
        let b = build_trial_schedule(2, 4, 2, SchedulingPolicy::Randomized, 1337);
        for (sa, sb) in a.iter().zip(b.iter()) {
            assert_eq!(sa.variant_idx, sb.variant_idx);
            assert_eq!(sa.task_idx, sb.task_idx);
            assert_eq!(sa.repl_idx, sb.repl_idx);
        }
    }

    #[test]
    fn schedule_randomized_different_seed_produces_different_order() {
        let a = build_trial_schedule(2, 4, 2, SchedulingPolicy::Randomized, 1);
        let b = build_trial_schedule(2, 4, 2, SchedulingPolicy::Randomized, 2);
        let same = a.iter().zip(b.iter()).all(|(sa, sb)| {
            sa.variant_idx == sb.variant_idx
                && sa.task_idx == sb.task_idx
                && sa.repl_idx == sb.repl_idx
        });
        assert!(!same, "different seeds should produce different orderings");
    }

    #[test]
    fn schedule_single_variant_single_task_single_repl() {
        for policy in [
            SchedulingPolicy::VariantSequential,
            SchedulingPolicy::PairedInterleaved,
            SchedulingPolicy::Randomized,
        ] {
            let slots = build_trial_schedule(1, 1, 1, policy, 1);
            assert_eq!(slots.len(), 1);
            assert_eq!(slots[0].variant_idx, 0);
            assert_eq!(slots[0].task_idx, 0);
            assert_eq!(slots[0].repl_idx, 0);
        }
    }

    #[test]
    fn schedule_empty_when_zero_tasks() {
        let slots = build_trial_schedule(2, 0, 3, SchedulingPolicy::VariantSequential, 1);
        assert!(slots.is_empty());
    }


    #[test]
    fn retry_with_empty_retry_on_retries_any_failure() {
        assert!(should_retry_outcome("error", "0", &[]));
        assert!(should_retry_outcome("success", "1", &[]));
        assert!(!should_retry_outcome("success", "0", &[]));
    }

    #[test]
    fn retry_on_error_only_retries_error_outcome() {
        let triggers = vec!["error".to_string()];
        assert!(should_retry_outcome("error", "0", &triggers));
        assert!(should_retry_outcome("error", "1", &triggers));
        assert!(!should_retry_outcome("success", "0", &triggers));
        assert!(!should_retry_outcome("success", "1", &triggers));
    }

    #[test]
    fn retry_on_failure_retries_nonzero_exit() {
        let triggers = vec!["failure".to_string()];
        assert!(should_retry_outcome("success", "1", &triggers));
        assert!(should_retry_outcome("error", "137", &triggers));
        assert!(!should_retry_outcome("success", "0", &triggers));
        assert!(!should_retry_outcome("error", "0", &triggers));
    }

    #[test]
    fn retry_on_timeout_retries_timeout_outcome() {
        let triggers = vec!["timeout".to_string()];
        assert!(should_retry_outcome("timeout", "0", &triggers));
        assert!(should_retry_outcome("timeout", "1", &triggers));
        assert!(!should_retry_outcome("error", "0", &triggers));
        assert!(!should_retry_outcome("success", "0", &triggers));
    }

    #[test]
    fn retry_on_multiple_triggers() {
        let triggers = vec!["error".to_string(), "timeout".to_string()];
        assert!(should_retry_outcome("error", "0", &triggers));
        assert!(should_retry_outcome("timeout", "0", &triggers));
        assert!(!should_retry_outcome("success", "1", &triggers));
    }

    #[test]
    fn grader_verdict_maps_to_trial_outcome() {
        assert_eq!(grader_verdict_to_trial_outcome("pass"), Some("success"));
        assert_eq!(grader_verdict_to_trial_outcome("fail"), Some("failure"));
        assert_eq!(
            grader_verdict_to_trial_outcome("missing"),
            Some("missing")
        );
        assert_eq!(grader_verdict_to_trial_outcome("error"), Some("error"));
        assert_eq!(grader_verdict_to_trial_outcome("unknown"), None);
    }

    #[test]
    fn trial_conclusion_outcome_maps_to_trial_outcome() {
        assert_eq!(
            trial_conclusion_outcome_to_trial_outcome("success"),
            Some("success")
        );
        assert_eq!(
            trial_conclusion_outcome_to_trial_outcome("failure"),
            Some("failure")
        );
        assert_eq!(
            trial_conclusion_outcome_to_trial_outcome("timeout"),
            Some("timeout")
        );
        assert_eq!(
            trial_conclusion_outcome_to_trial_outcome("pass"),
            Some("success")
        );
        assert_eq!(trial_conclusion_outcome_to_trial_outcome("unknown"), None);
    }

    #[test]
    fn grading_retry_inputs_ignore_agent_exit_when_mapped_output_is_valid() {
        let (outcome, exit_status) = grading_retry_inputs(
            true,
            Some(&json!({
                "schema_version": "trial_conclusion_v1",
                "reported_outcome": "success"
            })),
            None,
            "137",
            true,
            None,
        );
        assert_eq!(outcome, "success");
        assert_eq!(exit_status, "0");
    }

    #[test]
    fn grading_retry_inputs_treat_missing_mapped_output_as_error() {
        let (outcome, exit_status) = grading_retry_inputs(
            true,
            None,
            Some("mapped_grader_output_missing: /bucephalus/out/mapped_grader_output.json"),
            "0",
            true,
            None,
        );
        assert_eq!(outcome, "error");
        assert_eq!(exit_status, "0");
    }

    #[test]
    fn grading_retry_inputs_preserve_agent_timeout_when_grader_is_skipped() {
        let (outcome, exit_status) = grading_retry_inputs(
            true,
            None,
            Some("agent_timeout: grader skipped"),
            "timeout",
            false,
            None,
        );
        assert_eq!(outcome, "timeout");
        assert_eq!(exit_status, "timeout");
    }

    #[test]
    fn check_dataset_task_ids_rejects_grading_opt_out() {
        let evaluation = EvaluationConfig {
            policy: TaskPolicyConfig::default(),
            grader: Some(GraderConfig::in_task_runtime(vec![
                "python3".to_string(),
                "/opt/grader/run.py".to_string(),
            ])),
        };
        let mut task = task_row_value("task_1", "python:3.11-slim", "/workspace/task", None);
        task.pointer_mut("/task")
            .and_then(Value::as_object_mut)
            .expect("task object")
            .insert("grading".to_string(), json!({ "enabled": false }));
        let checks = check_dataset_task_ids(&[task], &evaluation, &[]);
        let grading_gate = checks
            .iter()
            .find(|check| {
                check
                    .message
                    .contains("graded tasks require mapped grading output")
            })
            .expect("grading opt-out check");
        assert!(!grading_gate.passed, "grading opt-out should fail validation");
        assert_eq!(grading_gate.severity, PreflightSeverity::Error);
    }

    #[test]
    fn parse_policies_rejects_unknown_scheduling() {
        let spec = json!({
            "policy": {
                "policies": {
                    "scheduling": "unknown_value",
                    "state": "unknown_state",
                    "retry": { "max_attempts": 1 }
                }
            }
        });
        let err = parse_policies(&spec).unwrap_err();
        assert!(err.to_string().contains("unknown policy.policies.scheduling 'unknown_value'"), "unexpected error: {err}");
    }

    #[test]
    fn parse_policies_rejects_unknown_state() {
        let spec = json!({
            "policy": {
                "policies": {
                    "scheduling": "variant_sequential",
                    "state": "unknown_state"
                }
            }
        });
        let err = parse_policies(&spec).unwrap_err();
        assert!(err.to_string().contains("unknown policy.policies.state 'unknown_state'"), "unexpected error: {err}");
    }

    #[test]
    fn resolve_effective_task_policy_rejects_unknown_overrides() {
        let experiment_policy = PolicyConfig::default();
        let task_policy = TaskPolicyConfig::default();
        let state_err = resolve_effective_task_policy(
            &experiment_policy,
            &task_policy,
            &json!({"policy_override": {"state_policy": "mystery_state"}}),
        )
        .unwrap_err();
        assert!(state_err.to_string().contains("unknown policy_override.state_policy 'mystery_state'"), "unexpected error: {state_err}");
        let model_err = resolve_effective_task_policy(
            &experiment_policy,
            &task_policy,
            &json!({"policy_override": {"task_model": "mystery_model"}}),
        )
        .unwrap_err();
        assert!(model_err.to_string().contains("unknown policy_override.task_model 'mystery_model'"), "unexpected error: {model_err}");
    }

    #[test]
    fn inv02_timeout_policy_propagates_to_runtime_env() {
        let io = prepared_trial_io_fixture(
            PathBuf::from("/tmp/out.json"),
            PathBuf::from("/tmp/events.jsonl"),
        );
        let input = json!({
            "ids": {
                "trial_id": "trial_1",
                "variant_id": "base",
                "task_id": "task_1",
                "repl_idx": 0
            },
            "policy": {
                "timeout_ms": 456000
            }
        });
        let timeout_ms = resolve_trial_timeout_ms(&input);
        let env = build_runtime_contract_env("run_1", &input, &io, None, timeout_ms)
            .expect("runtime env");
        assert_eq!(
            env.get(BUCEPHALUS_ENV_TIMEOUT_MS).map(String::as_str),
            Some("456000")
        );
    }

    #[test]
    fn inv03_preflight_fails_below_min_disk_headroom() {
        let check = check_disk_headroom_with_threshold(Path::new("."), u64::MAX);
        assert!(
            !check.passed,
            "disk check should fail when threshold is too high"
        );
        assert!(check.message.contains("required="));
        assert!(check.message.contains("available="));
    }

    #[test]
    fn inv03_preflight_passes_at_or_above_min_disk_headroom() {
        let check = check_disk_headroom_with_threshold(Path::new("."), 1);
        assert!(check.passed, "disk check should pass for tiny threshold");
    }

    #[test]
    fn preflight_parse_parallelism_clamps_and_rejects_zero() {
        assert_eq!(parse_parallelism("1").expect("parallelism"), 1);
        assert_eq!(
            parse_parallelism("128").expect("parallelism"),
            MAX_PREFLIGHT_IMAGE_PROBE_PARALLELISM
        );
        assert!(parse_parallelism("0").is_err());
        assert!(parse_parallelism("abc").is_err());
    }

    #[test]
    fn preflight_image_probe_parallelism_rejects_invalid_env() {
        let _guard = EnvVarGuard::set(&[(
            BUCEPHALUS_PREFLIGHT_IMAGE_PROBE_PARALLELISM_ENV,
            Some("invalid"),
        )]);
        let err = preflight_image_probe_parallelism()
            .expect_err("invalid image probe parallelism must fail");
        assert!(
            err.to_string()
                .contains(BUCEPHALUS_PREFLIGHT_IMAGE_PROBE_PARALLELISM_ENV),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn preflight_bounded_probe_preserves_order_and_bounds_concurrency() {
        let images = vec![
            "img_a".to_string(),
            "img_b".to_string(),
            "img_c".to_string(),
            "img_d".to_string(),
            "img_e".to_string(),
        ];
        let in_flight = std::sync::Arc::new(AtomicUsize::new(0));
        let max_in_flight = std::sync::Arc::new(AtomicUsize::new(0));
        let results = run_bounded_image_probes(&images, "test_probe", |idx, image| {
            let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(current, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(((images.len() - idx) * 2) as u64));
            in_flight.fetch_sub(1, Ordering::SeqCst);
            format!("{}:{}", idx, image)
        })
        .expect("bounded probes");

        assert_eq!(
            results,
            vec![
                "0:img_a".to_string(),
                "1:img_b".to_string(),
                "2:img_c".to_string(),
                "3:img_d".to_string(),
                "4:img_e".to_string(),
            ]
        );
        let allowed_parallelism = preflight_image_probe_parallelism()
            .expect("parallelism")
            .min(images.len())
            .max(1);
        assert!(
            max_in_flight.load(Ordering::SeqCst) <= allowed_parallelism,
            "bounded image probes exceeded allowed parallelism"
        );
    }

    fn preflight_test_runtime_profile(
        image_source: ImageSource,
        sandbox_image: Option<&str>,
    ) -> VariantRuntimeProfile {
        let mut agent_runtime = legacy_contract_runtime_fixture();
        agent_runtime.command_raw = vec!["agentctl".to_string()];
        agent_runtime.image = sandbox_image.unwrap_or("python:3.11-slim").to_string();
        agent_runtime.network = "none".to_string();
        agent_runtime.sandbox_image = sandbox_image.map(|value| value.to_string());
        agent_runtime.image_source = image_source;
        agent_runtime.execution = agent_execution_fixture(Some("python:3.11-slim"));

        VariantRuntimeProfile {
            experiment: json!({
                "trial_runtime": {
                    "task": {
                        "interface": "writable_workspace",
                        "workspace": {
                            "source": "container_image",
                            "image": { "from": "task_row" },
                            "workdir": { "from": "task_row" }
                        }
                    },
                    "agent": {
                        "command": ["agentctl"],
                        "artifact_type": "structured_json",
                        "outputs": {
                            "result": {
                                "capture": {
                                    "type": "file",
                                    "path": DEFAULT_CONTAINER_RESULT_PATH,
                                    "format": "json"
                                }
                            }
                        }
                    },
                    "execution": {
                        "agent_site": "task_runtime"
                    },
                    "grader": {
                        "strategy": "none"
                    }
                }
            }),
            variant_args: Vec::new(),
            agent_runtime,
            agent_runtime_env: BTreeMap::new(),
            secret_file_mounts: Vec::new(),
            invocation_source: "test".to_string(),
            configured_network_mode: "none".to_string(),
            effective_network_mode: "none".to_string(),
        }
    }

    #[test]
    fn hermetic_preflight_requires_agent_runtime_image() {
        let variant = preflight_test_variant();
        let mut profile = preflight_test_runtime_profile(ImageSource::Global, Some("img:latest"));
        profile.agent_runtime.image.clear();

        let checks = check_agent_runtime_hermetic_for_variants(&[variant], &[profile]);
        assert_eq!(checks.len(), 1);
        assert!(!checks[0].passed, "{:?}", checks[0]);
        assert!(
            checks[0]
                .message
                .contains("trial_runtime.agent.image is required"),
            "unexpected message: {}",
            checks[0].message
        );
    }

    #[test]
    fn preflight_agent_artifact_is_not_implicit_for_image_native_agents() {
        let mut profile = preflight_test_runtime_profile(ImageSource::Global, Some("img:latest"));
        profile.agent_runtime.agent_artifact = None;

        let check = check_agent_bundle_container_compatible(&profile);
        assert!(check.passed, "{:?}", check);
        assert!(
            check
                .message
                .contains("trial_runtime.agent.mount is not declared"),
            "unexpected message: {}",
            check.message
        );
    }

    #[test]
    fn resolve_run_isolation_grade_ignores_agent_cli_flags() {
        let mut profile = preflight_test_runtime_profile(ImageSource::Global, Some("img:latest"));
        profile.variant_args = vec!["--dangerous".to_string()];
        assert_eq!(
            resolve_run_isolation_grade(&[profile], &RunBehavior::default()),
            "hermetic"
        );
    }

    #[test]
    fn resolve_run_isolation_grade_marks_missing_agent_image_invalid() {
        let mut profile = preflight_test_runtime_profile(ImageSource::Global, Some("img:latest"));
        profile.agent_runtime.image.clear();
        profile.experiment["trial_runtime"]["execution"]["agent_site"] = json!("agent_container");
        assert_eq!(
            resolve_run_isolation_grade(&[profile], &RunBehavior::default()),
            "invalid"
        );
    }

    #[test]
    fn resolve_run_isolation_grade_allows_task_runtime_agent_without_agent_image() {
        let mut profile = preflight_test_runtime_profile(ImageSource::Global, Some("img:latest"));
        profile.agent_runtime.image.clear();
        assert_eq!(
            resolve_run_isolation_grade(&[profile], &RunBehavior::default()),
            "hermetic"
        );
    }

    #[test]
    fn resolve_run_isolation_grade_marks_scientific_container_runs_hermetic() {
        let profile = preflight_test_runtime_profile(ImageSource::Global, Some("img:latest"));
        let behavior = RunBehavior::default();

        assert_eq!(
            resolve_run_isolation_grade(&[profile], &behavior),
            "hermetic"
        );
    }

    fn preflight_test_variant() -> Variant {
        Variant {
            id: "test_variant".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        }
    }

    #[test]
    fn prepare_task_environment_carries_dependency_file_staging_into_dynamic_mounts() {
        let root = TempDirGuard::new("bucephalus_prepare_dependency_mounts");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");

        let staged_grader = root.path.join("grader.py");
        fs::write(&staged_grader, "#!/usr/bin/env python3\nprint('ok')\n").expect("grader");

        let mut runtime = legacy_contract_runtime_fixture();
        runtime.dependency_file_staging = vec![DependencyFileStagingSpec {
            source_from_host: staged_grader.clone(),
            destination_path: task_workdir_support_destination_path("grader.py"),
            required: true,
            read_only: true,
        }];

        let variant = preflight_test_variant();
        let task_boundary = runtime_task_boundary(
            json!({
                "id": "task_1",
                "task": {
                    "input": {
                        "prompt": "solve it"
                    }
                }
            }),
            "python:3.11-slim",
            "/testbed",
            None,
        );

        let prepared = prepare_task_environment_for_test(
            &root.path,
            &trial_dir,
            &current_trial_runtime_experiment_base(),
            &variant,
            &task_boundary,
            &runtime,
        )
        .expect("prepare task environment");

        assert_eq!(prepared.dynamic_mounts.len(), 1);
        assert_eq!(
            fs::read_to_string(
                prepared.dynamic_mounts[0]
                    .host_path
                    .join("support")
                    .join("grader.py")
            )
            .expect("projected staged grader"),
            "#!/usr/bin/env python3\nprint('ok')\n"
        );
        assert_eq!(
            prepared.dynamic_mounts[0].mount_path,
            "/testbed/.bucephalus"
        );
        assert_eq!(prepared.manifest.aux_mounts.len(), 1);
        assert_eq!(
            prepared.manifest.aux_mounts[0].mount_path,
            "/testbed/.bucephalus"
        );
    }

    #[test]
    fn prepare_task_environment_creates_output_mount_directories() {
        let root = TempDirGuard::new("bucephalus_prepare_output_mounts");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");

        let mut runtime = legacy_contract_runtime_fixture();
        runtime.output_mounts = vec![AgentRuntimeOutputMount {
            id: "session_context".to_string(),
            kind: "directory".to_string(),
            path: "session-context".to_string(),
            env: Some("BUCEPHALUS_SESSION_CONTEXT_ROOT".to_string()),
            persist: true,
        }];

        let variant = preflight_test_variant();
        let task_boundary = runtime_task_boundary(
            json!({
                "id": "task_1",
                "task": {
                    "input": {
                        "prompt": "solve it"
                    }
                }
            }),
            "python:3.11-slim",
            "/testbed",
            None,
        );

        let prepared = prepare_task_environment_for_test(
            &root.path,
            &trial_dir,
            &current_trial_runtime_experiment_base(),
            &variant,
            &task_boundary,
            &runtime,
        )
        .expect("prepare task environment");

        let host_path = prepared.trial_paths.out.join("session-context");
        assert!(host_path.is_dir(), "output mount dir should exist");
        assert_eq!(prepared.manifest.output_mounts.len(), 1);
        assert_eq!(
            prepared.manifest.output_mounts[0].container_path,
            "/bucephalus/out/session-context"
        );
        assert_eq!(
            prepared.manifest.output_mounts[0].env.as_deref(),
            Some("BUCEPHALUS_SESSION_CONTEXT_ROOT")
        );
    }

    #[test]
    fn preflight_resolve_images_reports_missing_global_image() {
        let check = resolve_preflight_images("container_ready", &[], None, "global image missing")
            .expect_err("missing global image should fail");
        assert_eq!(check.name, "container_ready");
        assert!(!check.passed);
        assert!(check.message.contains("global image missing"));
    }

    #[test]
    fn preflight_resolve_images_rejects_agent_image_when_tasks_absent() {
        let check = resolve_preflight_images("container_ready", &[], None, "task image missing")
            .expect_err("task image must be explicit");

        assert!(!check.passed);
        assert!(check.message.contains("task image missing"));
    }

    #[test]
    fn preflight_resolve_images_reports_per_task_scan_errors() {
        let scan = PerTaskImageScanResult {
            unique_images: Vec::new(),
            missing_task_ids: Vec::new(),
            parse_errors: vec!["line 1: malformed".to_string()],
        };
        let check = resolve_preflight_images("container_ready", &[], Some(&scan), "unused")
            .expect_err("parse errors should fail");
        assert!(!check.passed);
        assert!(check
            .message
            .contains("failed to parse packaged case rows"));
    }

    #[test]
    fn preflight_resolve_images_prefers_per_task_images_over_task_image_sentinel() {
        let scan = PerTaskImageScanResult {
            unique_images: vec![
                "repo/task-a:latest".to_string(),
                "repo/task-b:latest".to_string(),
            ],
            missing_task_ids: Vec::new(),
            parse_errors: Vec::new(),
        };

        let images = resolve_preflight_images("container_ready", &[], Some(&scan), "unused")
            .expect("per-task images should resolve");

        assert_eq!(
            images,
            vec![
                "repo/task-a:latest".to_string(),
                "repo/task-b:latest".to_string(),
            ]
        );
    }

    #[test]
    fn image_reference_parser_keeps_sources_backend_agnostic() {
        let registry = ImageReference::parse("ghcr.io/acme/task:latest").expect("registry ref");
        assert_eq!(registry.source, ImageReferenceSource::OciRegistry);
        assert_eq!(registry.raw(), "ghcr.io/acme/task:latest");

        let layout = ImageReference::parse("oci-layout:///tmp/image").expect("oci layout ref");
        assert_eq!(layout.source, ImageReferenceSource::OciLayout);

        let archive =
            ImageReference::parse("docker-archive:///tmp/image.tar").expect("archive ref");
        assert_eq!(archive.source, ImageReferenceSource::DockerArchive);

        let remote = ImageReference::parse("s3://bucket/image.oci").expect("remote object ref");
        assert_eq!(remote.source, ImageReferenceSource::RemoteObject);
    }

    #[test]
    fn oci_registry_reference_parser_handles_common_reference_forms() {
        let implicit = ImageReference::parse("alpine")
            .expect("implicit docker hub")
            .as_oci_registry_reference()
            .expect("oci registry ref");
        assert_eq!(implicit.registry, "docker.io");
        assert_eq!(implicit.repository, "library/alpine");
        assert_eq!(implicit.kind, OciRegistryReferenceKind::Tag("latest".to_string()));

        let explicit = ImageReference::parse("localhost:5000/team/task:2026")
            .expect("explicit registry")
            .as_oci_registry_reference()
            .expect("oci registry ref");
        assert_eq!(explicit.registry, "localhost:5000");
        assert_eq!(explicit.repository, "team/task");
        assert_eq!(explicit.kind, OciRegistryReferenceKind::Tag("2026".to_string()));

        let digest =
            "ghcr.io/acme/task@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let pinned = ImageReference::parse(digest)
            .expect("digest ref")
            .as_oci_registry_reference()
            .expect("oci registry ref");
        assert_eq!(pinned.registry, "ghcr.io");
        assert_eq!(pinned.repository, "acme/task");
        assert_eq!(
            pinned.kind,
            OciRegistryReferenceKind::Digest(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string()
            )
        );
    }

    #[test]
    fn oci_registry_reference_parser_rejects_malformed_registry_refs() {
        let err = ImageReference::parse("ghcr.io/acme/task@not-a-digest")
            .expect("source parse")
            .as_oci_registry_reference()
            .expect_err("digest should require algorithm");
        assert!(err.to_string().contains("digest"));

        let err = ImageReference::parse("ghcr.io//task:latest")
            .expect("source parse")
            .as_oci_registry_reference()
            .expect_err("empty repository component should fail");
        assert!(err.to_string().contains("empty path component"));

        let err = ImageReference::parse("oci-layout:///tmp/image")
            .expect("layout source")
            .as_oci_registry_reference()
            .expect_err("layout should not parse as registry");
        assert!(err.to_string().contains("not an OCI registry reference"));
    }

    #[test]
    fn preflight_image_requirements_preserve_role_separate_from_backend() {
        let scan = PerTaskImageScanResult {
            unique_images: vec!["oci-layout:///tmp/task-image".to_string()],
            missing_task_ids: Vec::new(),
            parse_errors: Vec::new(),
        };

        let requirements = resolve_preflight_image_requirements(
            "container_ready",
            &[],
            Some(&scan),
            "unused",
        )
        .expect("requirements");

        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].role, ImageRequirementRole::TaskSandbox);
        assert_eq!(requirements[0].image.source, ImageReferenceSource::OciLayout);
        assert_eq!(requirements[0].image.raw(), "oci-layout:///tmp/task-image");
    }

    #[test]
    fn reference_only_image_resolver_does_not_materialize() {
        let requirement =
            ImageRequirement::new(ImageRequirementRole::TaskSandbox, "python:3.11-slim", None)
                .expect("task image requirement");
        let resolver = ReferenceOnlyImageResolver;
        let report = resolver
            .resolve(&ImageResolveRequest {
                requirement,
                mode: ImageResolutionMode::ReferenceOnly,
            })
            .expect("resolve reference only");

        assert!(!report.materialized);
        assert_eq!(report.requirement.role, ImageRequirementRole::TaskSandbox);
        assert_eq!(report.requirement.image.source, ImageReferenceSource::OciRegistry);
    }

    #[test]
    fn image_resolver_chain_is_scoped_and_does_not_require_global_cache() {
        let requirement =
            ImageRequirement::new(ImageRequirementRole::TaskSandbox, "python:3.11-slim", None)
                .expect("task image requirement");
        let reference_only = ReferenceOnlyImageResolver;
        let chain = ImageResolverChain::new(vec![&reference_only]);

        let report = chain
            .resolve(&ImageResolveRequest {
                requirement,
                mode: ImageResolutionMode::ReferenceOnly,
            })
            .expect("resolve through scoped chain");

        assert!(!report.materialized);
        assert_eq!(report.requirement.image.raw(), "python:3.11-slim");
    }

    #[test]
    fn reference_only_image_resolver_does_not_claim_materializing_modes() {
        let requirement =
            ImageRequirement::new(ImageRequirementRole::TaskSandbox, "python:3.11-slim", None)
                .expect("requirement");
        let reference_only = ReferenceOnlyImageResolver;
        let chain = ImageResolverChain::new(vec![&reference_only]);

        let err = chain
            .resolve(&ImageResolveRequest {
                requirement,
                mode: ImageResolutionMode::Manifest,
            })
            .expect_err("reference-only resolver must not satisfy manifest probes");

        assert!(err.to_string().contains("no image resolver supports"));
    }

    struct CountingImageResolver {
        calls: AtomicUsize,
    }

    impl ImageResolver for CountingImageResolver {
        fn resolve(&self, request: &ImageResolveRequest) -> Result<ImageResolveReport> {
            let current = self.calls.fetch_add(1, Ordering::SeqCst);
            if current == 0 {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(ImageResolveReport {
                requirement: request.requirement.clone(),
                resolved_digest: Some(format!("sha256:{:064x}", current + 1)),
                platform: request.requirement.platform.clone(),
                manifest_size_bytes: Some(123),
                materialized: false,
            })
        }
    }

    #[test]
    fn scoped_image_resolver_cache_single_flights_duplicate_resolution() {
        let inner = CountingImageResolver {
            calls: AtomicUsize::new(0),
        };
        let cache = ScopedImageResolverCache::new(&inner);
        let request = ImageResolveRequest {
            requirement: ImageRequirement::new(
                ImageRequirementRole::TaskSandbox,
                "ghcr.io/acme/task:latest",
                Some("linux/amd64".to_string()),
            )
            .expect("requirement"),
            mode: ImageResolutionMode::Manifest,
        };

        std::thread::scope(|scope| {
            for _ in 0..16 {
                let cache_ref = &cache;
                let request_ref = &request;
                scope.spawn(move || {
                    let report = cache_ref.resolve(request_ref).expect("cached resolve");
                    assert_eq!(
                        report.resolved_digest.as_deref(),
                        Some("sha256:0000000000000000000000000000000000000000000000000000000000000001")
                    );
                });
            }
        });

        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            1,
            "duplicate image resolution should be single-flighted within a scoped cache"
        );
        assert_eq!(cache.cache_len(), 1);
    }

    #[test]
    fn scoped_image_resolver_cache_is_not_global_state() {
        let inner = CountingImageResolver {
            calls: AtomicUsize::new(0),
        };
        let request = ImageResolveRequest {
            requirement: ImageRequirement::new(
                ImageRequirementRole::TaskSandbox,
                "ghcr.io/acme/task:latest",
                None,
            )
            .expect("requirement"),
            mode: ImageResolutionMode::Manifest,
        };

        {
            let cache = ScopedImageResolverCache::new(&inner);
            cache.resolve(&request).expect("first scoped resolve");
            assert_eq!(cache.cache_len(), 1);
        }
        {
            let cache = ScopedImageResolverCache::new(&inner);
            cache.resolve(&request).expect("second scoped resolve");
            assert_eq!(cache.cache_len(), 1);
        }

        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            2,
            "cache entries must die with the resolver scope instead of persisting globally"
        );
    }

    struct PanicOnceImageResolver {
        calls: AtomicUsize,
    }

    impl ImageResolver for PanicOnceImageResolver {
        fn resolve(&self, request: &ImageResolveRequest) -> Result<ImageResolveReport> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("simulated resolver panic");
            }
            Ok(ImageResolveReport {
                requirement: request.requirement.clone(),
                resolved_digest: None,
                platform: request.requirement.platform.clone(),
                manifest_size_bytes: None,
                materialized: false,
            })
        }
    }

    #[test]
    fn scoped_image_resolver_cache_clears_inflight_entry_after_panic() {
        let inner = PanicOnceImageResolver {
            calls: AtomicUsize::new(0),
        };
        let cache = ScopedImageResolverCache::new(&inner);
        let request = ImageResolveRequest {
            requirement: ImageRequirement::new(
                ImageRequirementRole::TaskSandbox,
                "ghcr.io/acme/panic-once:latest",
                None,
            )
            .expect("requirement"),
            mode: ImageResolutionMode::Manifest,
        };

        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cache.resolve(&request);
        }));
        assert!(panic_result.is_err());
        assert_eq!(
            cache.cache_len(),
            0,
            "panic cleanup should remove the in-flight cache entry"
        );

        let report = cache
            .resolve(&request)
            .expect("resolver should be callable after panic cleanup");
        assert_eq!(report.requirement.image.raw(), "ghcr.io/acme/panic-once:latest");
        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn docker_image_singleflight_locks_are_evicted_when_unused() {
        let image = format!(
            "bucephalus-lock-cleanup-test:{}",
            Utc::now()
                .timestamp_nanos_opt()
                .expect("timestamp nanos")
        );
        assert!(!crate::backend::docker::docker_image_lock_exists_for_test(
            &image, None
        ));

        let lock = crate::backend::docker::docker_image_lock_for_test(&image, None);
        assert!(crate::backend::docker::docker_image_lock_exists_for_test(
            &image, None
        ));

        crate::backend::docker::docker_cleanup_image_lock_for_test(&image, None, &lock);
        assert!(
            !crate::backend::docker::docker_image_lock_exists_for_test(&image, None),
            "unused image locks should not accumulate in the global Docker coordinator"
        );
    }

    #[test]
    fn docker_image_singleflight_locks_stay_while_waiters_exist() {
        let image = format!(
            "bucephalus-lock-waiter-cleanup-test:{}",
            Utc::now()
                .timestamp_nanos_opt()
                .expect("timestamp nanos")
        );
        let lock = crate::backend::docker::docker_image_lock_for_test(&image, None);
        let waiter = lock.clone();

        crate::backend::docker::docker_cleanup_image_lock_for_test(&image, None, &lock);
        assert!(
            crate::backend::docker::docker_image_lock_exists_for_test(&image, None),
            "cleanup must not remove a lock while another caller still holds a clone"
        );

        drop(waiter);
        crate::backend::docker::docker_cleanup_image_lock_for_test(&image, None, &lock);
        assert!(
            !crate::backend::docker::docker_image_lock_exists_for_test(&image, None),
            "lock should be evicted once the last waiter is gone"
        );
    }

    #[test]
    fn preflight_image_budget_is_explicitly_configured() {
        let _lock = lock_modal_env_tests();
        let scan = PerTaskImageScanResult {
            unique_images: vec![
                "repo/task-a:latest".to_string(),
                "repo/task-b:latest".to_string(),
                "repo/task-c:latest".to_string(),
            ],
            missing_task_ids: Vec::new(),
            parse_errors: Vec::new(),
        };

        let _unset = EnvVarGuard::set(&[(BUCEPHALUS_MAX_PREFLIGHT_IMAGES_ENV, None)]);
        let images = resolve_preflight_images("container_ready", &[], Some(&scan), "unused")
            .expect("unique image count should not be capped unless configured");
        assert_eq!(images.len(), 3);
        drop(_unset);

        let _configured = EnvVarGuard::set(&[(BUCEPHALUS_MAX_PREFLIGHT_IMAGES_ENV, Some("2"))]);
        let check = resolve_preflight_images("container_ready", &[], Some(&scan), "unused")
            .expect_err("configured image budget should be enforced");
        assert!(!check.passed);
        assert!(check.message.contains("unique_images=3"));
        assert!(check.message.contains(BUCEPHALUS_MAX_PREFLIGHT_IMAGES_ENV));
    }

    fn support_matrix_experiment(agent_site: &str, task_interface: &str) -> Value {
        let task = match task_interface {
            "input_only" => json!({
                "interface": "input_only"
            }),
            "writable_workspace" => json!({
                "interface": "writable_workspace",
                "workspace": {
                    "source": "container_image",
                    "image": { "from": "task_row" },
                    "workdir": { "from": "task_row" }
                }
            }),
            other => panic!("unsupported test task interface: {}", other),
        };
        let mut agent = json!({
            "command": ["sh", "-lc", "true"],
            "artifact_type": "structured_json",
            "outputs": {
                "result": {
                    "capture": {
                        "type": "file",
                        "path": DEFAULT_CONTAINER_RESULT_PATH,
                        "format": "json"
                    }
                }
            }
        });
        if agent_site == "agent_container" {
            agent["image"] = json!("node:20-alpine");
        }
        json!({
            "trial_runtime": {
                "task": task,
                "agent": agent,
                "execution": {
                    "agent_site": agent_site
                },
                "grader": {
                    "strategy": "none"
                }
            }
        })
    }

    #[test]
    fn preflight_support_matrix_allows_host_input_only_without_grader() {
        let experiment = support_matrix_experiment("host", "input_only");
        let runtime = parse_trial_runtime_config(&experiment).expect("trial runtime");

        let checks = check_trial_runtime_support_matrix(&runtime, None);

        assert_eq!(checks.len(), 1);
        assert!(checks[0].passed, "{:?}", checks[0]);
    }

    #[test]
    fn preflight_support_matrix_rejects_host_writable_container_workspace() {
        let experiment = support_matrix_experiment("host", "writable_workspace");
        let runtime = parse_trial_runtime_config(&experiment).expect("trial runtime");
        let scan = PerTaskImageScanResult {
            unique_images: vec!["python:3.11-slim".to_string()],
            missing_task_ids: Vec::new(),
            parse_errors: Vec::new(),
        };

        let checks = check_trial_runtime_support_matrix(&runtime, Some(&scan));

        assert_eq!(checks.len(), 1);
        assert!(!checks[0].passed, "{:?}", checks[0]);
        assert!(
            checks[0]
                .message
                .contains("host agent execution does not materialize task files or task workspaces"),
            "unexpected message: {}",
            checks[0].message
        );
    }

    #[test]
    fn preflight_support_matrix_rejects_agent_container_image_mismatch() {
        let experiment = support_matrix_experiment("agent_container", "writable_workspace");
        let runtime = parse_trial_runtime_config(&experiment).expect("trial runtime");
        let scan = PerTaskImageScanResult {
            unique_images: vec!["python:3.11-slim".to_string()],
            missing_task_ids: Vec::new(),
            parse_errors: Vec::new(),
        };

        let checks = check_trial_runtime_support_matrix(&runtime, Some(&scan));

        assert_eq!(checks.len(), 1);
        assert!(!checks[0].passed, "{:?}", checks[0]);
        assert!(
            checks[0].message.contains("execution currently uses task row image"),
            "unexpected message: {}",
            checks[0].message
        );
    }

    #[test]
    fn preflight_container_ready_reports_variant_profile_mismatch() {
        let variant = preflight_test_variant();

        let checks = check_container_ready_for_variants(&[variant], &[], &[], None, false);

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "container_ready");
        assert!(!checks[0].passed, "{:?}", checks[0]);
        assert!(checks[0].message.contains("variant/runtime profile count mismatch"));
    }

    #[test]
    fn preflight_probe_output_blockers_detect_avx_incompatibility_warning() {
        let blockers = detect_known_probe_output_blockers(
            "",
            "warn: CPU lacks AVX support, strange crashes may occur.",
        );
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("CPU lacks AVX support"));
    }

    #[test]
    fn preflight_probe_output_blockers_detect_missing_tool_registry_warning() {
        let blockers = detect_known_probe_output_blockers(
            "[harness] Agent 'coding' references tool 'Skill' which is not available",
            "",
        );
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("references tool 'Skill'"));
    }

    #[test]
    fn preflight_agent_runtime_reachable_reports_missing_required_env_var() {
        let root = TempDirGuard::new("bucephalus_preflight_missing_required_env");
        let variant = preflight_test_variant();
        let mut profile = preflight_test_runtime_profile(ImageSource::Global, Some("img:latest"));
        profile.agent_runtime.env_from_host = vec!["OPENAI_API_KEY".to_string()];
        profile.agent_runtime_env.clear();

        let check = check_agent_runtime_reachable_with_scan(
            &profile,
            &variant,
            &[],
            None,
            &root.path,
            &root.path,
        );

        assert_eq!(check.name, "agent_runtime_reachable");
        assert!(!check.passed, "{:?}", check);
        assert!(check.message.contains("missing required runtime env var"));
        assert!(check.message.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn preflight_contract_smoke_result_validation_rejects_missing_payload() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_preflight_missing_result");
        let failures = validate_preflight_result_payload(&paths.out.join("result.json"));
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("contract smoke did not write result payload")),
            "unexpected failures: {:?}",
            failures
        );
    }

    #[test]
    fn preflight_blocking_error_filter_is_check_specific() {
        let checks = vec![
            PreflightCheck {
                name: "container_ready",
                passed: true,
                severity: PreflightSeverity::Error,
                message: "ok".to_string(),
            },
            PreflightCheck {
                name: "grader_reachable",
                passed: false,
                severity: PreflightSeverity::Warning,
                message: "warn".to_string(),
            },
            PreflightCheck {
                name: "container_ready",
                passed: false,
                severity: PreflightSeverity::Error,
                message: "failed".to_string(),
            },
        ];
        assert!(has_blocking_preflight_error(&checks, "container_ready"));
        assert!(!has_blocking_preflight_error(
            &checks,
            "grader_reachable"
        ));
    }

    fn inv07_spec_with_runtime_bindings() -> Value {
        json!({
            "experiment": { "id": "e", "name": "n" },
            "matrix": {
                "tasks": { "source": "file", "path": "tasks.jsonl", "suite_id": "s", "split_id": "dev", "limit": 1 },
                "variants": [
                    { "id": "base", "baseline": true, "config": { "model_provider": "openai", "model": "gpt-5" } },
                    { "id": "alt", "config": { "model_provider": "anthropic", "model": "claude-sonnet-4" } }
                ],
                "repeats": 1
            },
            "scheduling": { "comparison": "paired", "random_seed": 1, "shuffle_tasks": false, "max_concurrency": 1 },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "storage": { "backend": "local-fs" },
                "traces": { "backend": "local-stdout" },
                "network": { "task_sandbox": "none", "agent": "none" }
            },
            "policy": {
                "sanitization_profile": "hermetic_functional",
                "timeout_ms": 600000,
                "task_sandbox": {}
            },
            "trial_runtime": {
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": { "from": "task_row" },
                        "workdir": { "from": "task_row" }
                    }
                },
                "agent": {
                    "command": ["agentctl", "run", "--provider", "$model_provider", "--model", "$model"],
                    "artifact_type": "structured_json",
                    "mount": {
                        "source": ".lab/agents/agent-current.tar.gz",
                        "mount": {
                            "path": "/opt/agent",
                            "read_only": true
                        }
                    },
                    "image": "img",
                    "env": {
                        "OPENAI_API_KEY": "$OPENAI_API_KEY",
                        "STATIC_FLAG": "1"
                    },
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json",
                                "required": true
                            }
                        }
                    }
                },
                "execution": {
                    "agent_site": "agent_container"
                },
                "grader": {
                    "strategy": "none"
                }
            }
        })
    }

    fn inv07_resolve_runtime_profiles(
        spec: &Value,
        exp_dir: &Path,
        runtime_env: BTreeMap<String, String>,
    ) -> (Vec<Variant>, Vec<VariantRuntimeProfile>) {
        let (variants, _) = resolve_variant_plan(spec).expect("variant plan");
        let execution = RunExecutionOptions {
            runtime_env,
            ..RunExecutionOptions::default()
        };
        let mut profiles = Vec::new();
        for variant in &variants {
            profiles.push(
                resolve_variant_runtime_profile(
                    spec,
                    variant,
                    exp_dir,
                    &RunBehavior::default(),
                    &execution,
                )
                .expect("runtime profile"),
            );
        }
        (variants, profiles)
    }

    fn inv07_spec_with_runtime_secret_files() -> Value {
        let mut spec = inv07_spec_with_runtime_bindings();
        let runtime = spec
            .pointer_mut("/runtime")
            .and_then(Value::as_object_mut)
            .expect("runtime object");
        runtime.insert(
            "secrets".to_string(),
            json!([
                {
                    "name": "codex_oauth",
                    "from": "file",
                    "mount": {
                        "target": "/root/.codex/auth.json",
                        "required_for_variants": ["base"]
                    }
                }
            ]),
        );
        spec
    }

    fn inv07_spec_with_runtime_secret_file_cache() -> Value {
        let mut spec = inv07_spec_with_runtime_secret_files();
        let secret = spec
            .pointer_mut("/runtime/secrets/0")
            .and_then(Value::as_object_mut)
            .expect("secret file object");
        secret.insert(
            "credential_cache".to_string(),
            json!({
                "kind": "run_scoped",
                "target": "/bucephalus/credentials/codex_oauth/auth.json",
                "env": "CODEX_AUTH_CACHE_FILE"
            }),
        );
        spec
    }

    fn write_empty_run_staging_manifest(run_dir: &Path) {
        fs::write(
            run_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": "manifest_v1",
                "run_id": "run_1",
                "runner_version": "test",
                "created_at": "2026-01-01T00:00:00Z",
                "run_mode": "full"
            }))
            .expect("run manifest json"),
        )
        .expect("run manifest");
        ensure_dir(&run_dir.join(".lab").join("agents")).expect("agent bundle dir");
        fs::write(
            run_dir.join(".lab").join("agents").join("agent-current.tar.gz"),
            "agent bundle",
        )
        .expect("agent bundle");
        fs::write(
            run_dir.join(STAGING_MANIFEST_FILE),
            serde_json::to_vec_pretty(&json!({
                "schema_version": STAGING_MANIFEST_SCHEMA_VERSION,
                "variants": {
                    "base": [],
                    "alt": []
                }
            }))
            .expect("staging manifest json"),
        )
        .expect("staging manifest");
    }

    #[test]
    fn runtime_secret_files_resolve_to_active_variant_mounts() {
        let root = TempDirGuard::new("bucephalus_runtime_secret_file_mounts");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        fs::write(exp_dir.join("tasks.jsonl"), "{\"id\":\"task_1\"}\n").expect("dataset");
        let secret_path = root.path.join("auth.json");
        fs::write(&secret_path, "{}\n").expect("secret");
        let spec = inv07_spec_with_runtime_secret_files();
        let (variants, _) = resolve_variant_plan(&spec).expect("variant plan");
        let mut execution = RunExecutionOptions::default();
        execution
            .runtime_env
            .insert("OPENAI_API_KEY".to_string(), "test-token".to_string());
        execution
            .secret_files
            .insert("codex_oauth".to_string(), secret_path.clone());

        let base_profile = resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &exp_dir,
            &RunBehavior::default(),
            &execution,
        )
        .expect("base profile");
        assert_eq!(base_profile.secret_file_mounts.len(), 1);
        assert_eq!(base_profile.secret_file_mounts[0].id, "codex_oauth");
        assert_eq!(
            base_profile.secret_file_mounts[0].source_from_host,
            normalize_path(&secret_path)
        );
        assert_eq!(
            base_profile.secret_file_mounts[0].target_path,
            "/root/.codex/auth.json"
        );

        let alt_profile = resolve_variant_runtime_profile(
            &spec,
            &variants[1],
            &exp_dir,
            &RunBehavior::default(),
            &execution,
        )
        .expect("alt profile");
        assert!(alt_profile.secret_file_mounts.is_empty());
    }

    #[test]
    fn runtime_secret_files_require_launch_time_source_binding() {
        let root = TempDirGuard::new("bucephalus_runtime_secret_file_missing");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        fs::write(exp_dir.join("tasks.jsonl"), "{\"id\":\"task_1\"}\n").expect("dataset");
        let spec = inv07_spec_with_runtime_secret_files();
        let (variants, _) = resolve_variant_plan(&spec).expect("variant plan");
        let mut execution = RunExecutionOptions::default();
        execution
            .runtime_env
            .insert("OPENAI_API_KEY".to_string(), "test-token".to_string());

        let err = match resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &exp_dir,
            &RunBehavior::default(),
            &execution,
        ) {
            Ok(_) => panic!("missing secret binding should fail"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(msg.contains("missing required secret file 'codex_oauth'"));
        assert!(msg.contains("--secret-file codex_oauth=HOST_PATH"));
    }

    #[test]
    fn runtime_secret_files_reject_workspace_target() {
        let root = TempDirGuard::new("bucephalus_runtime_secret_workspace_target");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let mut spec = inv07_spec_with_runtime_secret_files();
        set_json_pointer_value(
            &mut spec,
            "/runtime/secrets/0/mount/target",
            json!("/workspace/task/auth.json"),
        )
        .expect("rewrite secret target");
        let (variants, _) = resolve_variant_plan(&spec).expect("variant plan");
        let mut execution = RunExecutionOptions::default();
        execution
            .runtime_env
            .insert("OPENAI_API_KEY".to_string(), "test-token".to_string());

        let err = match resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &exp_dir,
            &RunBehavior::default(),
            &execution,
        ) {
            Ok(_) => panic!("secret mount must not target task workspace roots"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("targets reserved runner path '/workspace/task'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn runtime_secret_credential_cache_rejects_state_target() {
        let root = TempDirGuard::new("bucephalus_runtime_secret_cache_state_target");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let mut spec = inv07_spec_with_runtime_secret_file_cache();
        set_json_pointer_value(
            &mut spec,
            "/runtime/secrets/0/credential_cache/target",
            json!("/bucephalus/state/auth.json"),
        )
        .expect("rewrite credential cache target");
        let (variants, _) = resolve_variant_plan(&spec).expect("variant plan");
        let mut execution = RunExecutionOptions::default();
        execution
            .runtime_env
            .insert("OPENAI_API_KEY".to_string(), "test-token".to_string());

        let err = match resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &exp_dir,
            &RunBehavior::default(),
            &execution,
        ) {
            Ok(_) => panic!("credential cache mount must not target runner state roots"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("targets reserved runner path '/bucephalus/state'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn runtime_secret_credential_cache_rejects_secret_target_overlap() {
        let root = TempDirGuard::new("bucephalus_runtime_secret_cache_overlap");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        let mut spec = inv07_spec_with_runtime_secret_file_cache();
        set_json_pointer_value(
            &mut spec,
            "/runtime/secrets/0/credential_cache/target",
            json!("/root/.codex/cache.json"),
        )
        .expect("rewrite credential cache target");
        let (variants, _) = resolve_variant_plan(&spec).expect("variant plan");
        let mut execution = RunExecutionOptions::default();
        execution
            .runtime_env
            .insert("OPENAI_API_KEY".to_string(), "test-token".to_string());

        let err = match resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &exp_dir,
            &RunBehavior::default(),
            &execution,
        ) {
            Ok(_) => panic!("credential cache directory must not overlap read-only secret target"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("writable credential cache directory '/root/.codex' must not overlap read-only secret target '/root/.codex/auth.json'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn runtime_secret_files_prepare_run_scoped_credential_cache() {
        let root = TempDirGuard::new("bucephalus_runtime_secret_cache");
        let run_dir = root.path.join("run_1");
        ensure_dir(&run_dir).expect("run dir");
        write_empty_run_staging_manifest(&run_dir);
        let secret_path = root.path.join("auth.json");
        fs::write(&secret_path, "{\"refresh_token\":\"seed\"}\n").expect("secret");
        let spec = inv07_spec_with_runtime_secret_file_cache();
        let (variants, _) = resolve_variant_plan(&spec).expect("variant plan");
        let mut execution = RunExecutionOptions::default();
        execution
            .runtime_env
            .insert("OPENAI_API_KEY".to_string(), "test-token".to_string());
        execution
            .secret_files
            .insert("codex_oauth".to_string(), secret_path.clone());

        let base_profile = resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &run_dir,
            &RunBehavior::default(),
            &execution,
        )
        .expect("base profile");
        let mount = base_profile
            .secret_file_mounts
            .first()
            .expect("secret mount");
        let cache = mount.credential_cache.as_ref().expect("credential cache");
        assert!(cache
            .host_dir
            .starts_with(run_dir.join("runtime").join("credential_caches")));
        assert!(!cache.host_dir.to_string_lossy().contains("sha256:"));
        assert_eq!(cache.target_path, "/bucephalus/credentials/codex_oauth/auth.json");
        assert_eq!(cache.env.as_deref(), Some("CODEX_AUTH_CACHE_FILE"));
        assert_eq!(
            base_profile
                .agent_runtime_env
                .get("CODEX_AUTH_CACHE_FILE")
                .map(String::as_str),
            Some("/bucephalus/credentials/codex_oauth/auth.json")
        );
        assert_eq!(
            fs::read_to_string(&cache.host_file).expect("cache content"),
            "{\"refresh_token\":\"seed\"}\n"
        );

        fs::write(&cache.host_file, "{\"refresh_token\":\"rotated\"}\n")
            .expect("rotated cache");
        fs::write(&secret_path, "{\"refresh_token\":\"original\"}\n").expect("source update");
        let preserved_profile = resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &run_dir,
            &RunBehavior::default(),
            &execution,
        )
        .expect("preserved profile");
        let preserved_cache = preserved_profile.secret_file_mounts[0]
            .credential_cache
            .as_ref()
            .expect("preserved cache");
        assert_eq!(
            fs::read_to_string(&preserved_cache.host_file).expect("preserved content"),
            "{\"refresh_token\":\"rotated\"}\n"
        );

        let mut missing_secret_execution = RunExecutionOptions::default();
        missing_secret_execution
            .runtime_env
            .insert("OPENAI_API_KEY".to_string(), "test-token".to_string());
        let err = match resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &run_dir,
            &RunBehavior::default(),
            &missing_secret_execution,
        ) {
            Ok(_) => panic!("credential cache must not replace launch-time secret binding"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(msg.contains("missing required secret file 'codex_oauth'"));
        assert!(msg.contains("--secret-file codex_oauth=HOST_PATH"));

        let alt_profile = resolve_variant_runtime_profile(
            &spec,
            &variants[1],
            &run_dir,
            &RunBehavior::default(),
            &execution,
        )
        .expect("alt profile");
        assert!(alt_profile.secret_file_mounts.is_empty());
        assert!(!alt_profile
            .agent_runtime_env
            .contains_key("CODEX_AUTH_CACHE_FILE"));
    }

    #[test]
    fn credential_cache_seed_is_singleflight_and_leaves_no_tmp_files() -> Result<()> {
        let root = TempDirGuard::new("bucephalus_credential_cache_seed_race");
        let source = root.path.join("auth.json");
        let cache = root.path.join("cache").join("auth.json");
        fs::write(&source, "{\"refresh_token\":\"seed\"}\n")?;

        std::thread::scope(|scope| {
            for _ in 0..32 {
                let source = &source;
                let cache = &cache;
                scope.spawn(move || {
                    seed_credential_cache_file_for_test(source, cache, "codex_oauth").unwrap();
                });
            }
        });

        assert_eq!(fs::read_to_string(&cache)?, "{\"refresh_token\":\"seed\"}\n");
        assert!(fs::read_dir(cache.parent().expect("cache parent"))?
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains(".seed.tmp")));
        Ok(())
    }

    #[test]
    fn credential_cache_seed_rejects_target_without_file_name() -> Result<()> {
        let root = TempDirGuard::new("bucephalus_credential_cache_target_name");
        let source = root.path.join("auth.json");
        fs::write(&source, "{}\n")?;
        let err = seed_credential_cache_file_for_test(&source, &root.path.join("cache/.."), "id")
            .unwrap_err();
        assert!(err.to_string().contains("has no file name"));
        Ok(())
    }

    #[test]
    fn strict_run_rejects_task_llm_egress_when_network_none_is_required() {
        let root = TempDirGuard::new("bucephalus_runtime_llm_egress");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        fs::write(exp_dir.join("tasks.jsonl"), "{\"id\":\"task_1\"}\n").expect("dataset");
        let mut spec = inv07_spec_with_runtime_bindings();
        spec["policy"]["sanitization_profile"] = json!("standard_runtime");
        *spec
            .pointer_mut("/runtime/network/task_sandbox")
            .expect("task sandbox network") = json!("none");
        let behavior_network = "llm_egress".to_string();
        let (variants, _) = resolve_variant_plan(&spec).expect("variant plan");
        let mut execution = RunExecutionOptions::default();
        execution
            .runtime_env
            .insert("OPENAI_API_KEY".to_string(), "test-token".to_string());
        let behavior = RunBehavior {
            network_mode_override: Some(behavior_network),
            require_network_none: true,
            smoke_test: false,
        };

        let err = match resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &exp_dir,
            &behavior,
            &execution,
        ) {
            Ok(_) => panic!("strict profile should reject agent llm egress"),
            Err(err) => err,
        };

        assert!(
            err.to_string()
                .contains("strict run requires network mode 'none'"),
            "unexpected error: {}",
            err
        );
        assert_eq!(docker_network_mode("none"), Some("none".to_string()));
    }

    #[test]
    fn inv07_runtime_bindings_resolve_variant_values_into_command() {
        let root = TempDirGuard::new("bucephalus_inv07_variant_runtime_bindings");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        fs::write(exp_dir.join("tasks.jsonl"), "{\"id\":\"task_1\"}\n").expect("dataset");
        let spec = inv07_spec_with_runtime_bindings();
        let mut runtime_env = BTreeMap::new();
        runtime_env.insert("OPENAI_API_KEY".to_string(), "test-token".to_string());
        let (_variants, profiles) = inv07_resolve_runtime_profiles(&spec, &exp_dir, runtime_env);

        assert_eq!(
            profiles[0].agent_runtime.command_raw,
            vec![
                "agentctl".to_string(),
                "run".to_string(),
                "--provider".to_string(),
                "openai".to_string(),
                "--model".to_string(),
                "gpt-5".to_string()
            ]
        );
        assert_eq!(
            profiles[1].agent_runtime.command_raw,
            vec![
                "agentctl".to_string(),
                "run".to_string(),
                "--provider".to_string(),
                "anthropic".to_string(),
                "--model".to_string(),
                "claude-sonnet-4".to_string()
            ]
        );
    }

    #[test]
    fn inv07_runtime_bindings_resolve_launch_env_into_public_env() {
        let root = TempDirGuard::new("bucephalus_inv07_launch_env_binding");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        fs::write(exp_dir.join("tasks.jsonl"), "{\"id\":\"task_1\"}\n").expect("dataset");
        let spec = inv07_spec_with_runtime_bindings();
        let mut runtime_env = BTreeMap::new();
        runtime_env.insert("OPENAI_API_KEY".to_string(), "test-token".to_string());
        let (_variants, profiles) = inv07_resolve_runtime_profiles(&spec, &exp_dir, runtime_env);

        assert_eq!(
            profiles[0]
                .agent_runtime_env
                .get("OPENAI_API_KEY")
                .map(String::as_str),
            Some("test-token")
        );
        assert_eq!(
            profiles[0]
                .agent_runtime_env
                .get("STATIC_FLAG")
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn output_mount_env_is_injected_into_agent_runtime_env() {
        let root = TempDirGuard::new("bucephalus_output_mount_env");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        fs::write(exp_dir.join("tasks.jsonl"), "{\"id\":\"task_1\"}\n").expect("dataset");
        let mut spec = inv07_spec_with_runtime_bindings();
        spec.pointer_mut("/trial_runtime/agent")
            .and_then(Value::as_object_mut)
            .expect("agent runtime object")
            .insert(
                "output_mounts".to_string(),
                json!([
                    {
                        "id": "session_context",
                        "kind": "directory",
                        "path": "session-context",
                        "env": "BUCEPHALUS_SESSION_CONTEXT_ROOT",
                        "persist": true
                    }
                ]),
            );
        let mut runtime_env = BTreeMap::new();
        runtime_env.insert("OPENAI_API_KEY".to_string(), "test-token".to_string());
        let (_variants, profiles) = inv07_resolve_runtime_profiles(&spec, &exp_dir, runtime_env);

        assert_eq!(
            profiles[0]
                .agent_runtime_env
                .get("BUCEPHALUS_SESSION_CONTEXT_ROOT")
                .map(String::as_str),
            Some("/bucephalus/out/session-context")
        );
    }

    #[test]
    fn inv07_runtime_bindings_fail_when_required_launch_env_is_missing() {
        let root = TempDirGuard::new("bucephalus_inv07_missing_launch_env");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp dir");
        fs::write(exp_dir.join("tasks.jsonl"), "{\"id\":\"task_1\"}\n").expect("dataset");
        let spec = inv07_spec_with_runtime_bindings();
        let (variants, _) = resolve_variant_plan(&spec).expect("variant plan");
        let err = match resolve_variant_runtime_profile(
            &spec,
            &variants[0],
            &exp_dir,
            &RunBehavior::default(),
            &RunExecutionOptions::default(),
        ) {
            Ok(_) => panic!("missing runtime env should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("missing runtime binding $OPENAI_API_KEY"),
            "unexpected error: {}",
            err
        );
    }

    fn inv06_write_resolved_experiment(
        run_dir: &Path,
        dataset_path: &str,
        run_id: &str,
        run_status: &str,
    ) -> Value {
        let project_root = find_project_root(run_dir);
        let bundle_root = ensure_test_agent_bundle(&project_root, "agent-current");
        let resolved = json!({
            "experiment": { "id": "e", "name": "n" },
            "matrix": {
                "tasks": { "source": "file", "path": dataset_path, "suite_id": "s", "split_id": "dev", "limit": 1 },
                "variants": [{ "id": "base", "baseline": true, "config": {} }],
                "repeats": 1
            },
            "scheduling": { "comparison": "paired", "random_seed": 1, "shuffle_tasks": false, "max_concurrency": 1 },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "storage": { "backend": "local-fs" },
                "traces": { "backend": "local-stdout" },
                "network": { "task_sandbox": "none", "agent": "none" }
            },
            "policy": {
                "sanitization_profile": "hermetic_functional",
                "timeout_ms": 600000,
                "task_sandbox": {}
            },
            "trial_runtime": {
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": { "from": "task_row" },
                        "workdir": { "from": "task_row" }
                    }
                },
                "agent": {
                    "command": [
                        "sh",
                        "-lc",
                        "printf '%s' '{\"checkpoints\":[]}'"
                    ],
                    "artifact_type": "structured_json",
                    "mount": {
                        "source": bundle_root.to_string_lossy().to_string(),
                        "mount": { "path": "/opt/agent", "read_only": true }
                    },
                    "image": "python:3.11-slim",
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json"
                            }
                        }
                    }
                },
                "execution": { "agent_site": "agent_container" },
                "grader": { "strategy": "none" }
            }
        });
        atomic_write_json_pretty(&run_dir.join("resolved_experiment.json"), &resolved)
            .expect("resolved experiment");
        let (variants, baseline_id) = resolve_variant_plan(&resolved).expect("variant plan");
        write_resolved_variants(run_dir, &resolved, &baseline_id, &variants)
            .expect("write variants");
        write_run_control(run_dir, run_id, run_status, &[], None).expect("run control");
        write_run_session_state(
            run_dir,
            run_id,
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");
        let schedule =
            build_trial_schedule(1, 1, 1, parse_policies(&resolved).unwrap().scheduling, 1);
        let progress = ScheduleProgress {
            schema_version: "schedule_progress_v1".to_string(),
            run_id: run_id.to_string(),
            total_slots: schedule.len(),
            next_schedule_index: 0,
            next_trial_index: 0,
            schedule,
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        write_schedule_progress(run_dir, &progress).expect("schedule progress");
        resolved
    }

    #[test]
    fn inv06_recover_then_continue_succeeds_with_minimal_env() {
        let (_root, run_dir) = create_run_dir("bucephalus_inv06_recover_continue", "run_1");
        let dataset_path = run_dir.join("tasks.jsonl");
        fs::write(&dataset_path, "{\"id\":\"task_1\"}\n").expect("dataset");
        inv06_write_resolved_experiment(&run_dir, "tasks.jsonl", "run_1", "running");
        write_schedule_progress(
            &run_dir,
            &ScheduleProgress {
                schema_version: "schedule_progress_v1".to_string(),
                run_id: "run_1".to_string(),
                total_slots: 0,
                next_schedule_index: 0,
                next_trial_index: 0,
                schedule: Vec::new(),
                completed_slots: Vec::new(),
                pruned_variants: Vec::new(),
                consecutive_failures: BTreeMap::new(),
                updated_at: Utc::now().to_rfc3339(),
            },
        )
        .expect("schedule progress");

        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);
        let active = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: Some(active_control_for_trial(&trial_dir)),
        }];
        write_run_control(&run_dir, "run_1", "running", &active, None).expect("run control");

        let recovered = recover_run(&run_dir, true).expect("recover");
        assert_eq!(recovered.recovered_status, "interrupted");

        let continue_err =
            continue_run(&run_dir).expect_err("continue should reach deterministic terminal guard");
        assert!(
            continue_err.to_string().contains("nothing to continue"),
            "unexpected continue error: {}",
            continue_err
        );
    }

    #[test]
    fn recover_run_fails_untracked_runtime_active_trial_without_container_ids() {
        let (_root, run_dir) = create_run_dir("bucephalus_recover_runtime_only_active", "run_1");
        let dataset_path = run_dir.join("tasks.jsonl");
        fs::write(&dataset_path, "{\"id\":\"task_1\"}\n").expect("dataset");
        inv06_write_resolved_experiment(&run_dir, "tasks.jsonl", "run_1", "running");

        let _trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);
        let runtime_state = runtime_trial_attempt_state_fixture(TrialPhase::AgentRunning);
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &runtime_state)
            .expect("db runtime state");
        write_run_control(&run_dir, "run_1", "running", &[], None).expect("run control");

        let err = match recover_run(&run_dir, true) {
            Ok(_) => panic!("recover must not abandon active runtime state without container cleanup"),
            Err(err) => err,
        };
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("cleanup_missing_runtime_container"),
            "unexpected error: {}",
            msg
        );

        let runtime_state = BackingSqliteStore::open(&run_dir)
            .expect("store")
            .load_latest_trial_attempt("run_1", "trial_1")
            .expect("db runtime")
            .expect("db runtime");
        assert_eq!(runtime_state.state.phase, TrialPhase::AgentRunning);
    }

    #[test]
    fn recover_run_prefers_durable_paused_runtime_state_over_stale_run_control() {
        let (_root, run_dir) = create_run_dir("bucephalus_recover_runtime_paused", "run_1");
        let dataset_path = run_dir.join("tasks.jsonl");
        fs::write(&dataset_path, "{\"id\":\"task_1\"}\n").expect("dataset");
        inv06_write_resolved_experiment(&run_dir, "tasks.jsonl", "run_1", "running");

        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "paused", Some("cp1"));
        let mut runtime_state = runtime_trial_attempt_state_fixture(TrialPhase::Paused);
        runtime_state.paused_from_phase = Some(TrialPhase::AgentRunning);
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &runtime_state)
            .expect("db runtime state");

        let active = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: Some(active_control_for_trial(&trial_dir)),
        }];
        write_run_control(&run_dir, "run_1", "running", &active, None).expect("run control");

        let recovered = recover_run(&run_dir, true).expect("recover");
        assert_eq!(recovered.active_trials_released, 0);

        let persisted = BackingSqliteStore::open(&run_dir)
            .expect("store")
            .load_latest_trial_attempt("run_1", "trial_1")
            .expect("db runtime")
            .expect("db runtime");
        assert_eq!(persisted.state.phase, TrialPhase::Paused);
        assert_eq!(
            persisted.state.paused_from_phase,
            Some(TrialPhase::AgentRunning)
        );
        let trial_state = load_json_file(&trial_state_path(&trial_dir)).expect("trial state");
        assert_eq!(trial_state["status"], "paused");
    }

    #[test]
    fn recover_run_reconciles_commit_pending_runtime_state_from_committed_slot() {
        let (_root, run_dir) = create_run_dir("bucephalus_recover_commit_pending", "run_1");
        let dataset_path = run_dir.join("tasks.jsonl");
        fs::write(&dataset_path, "{\"id\":\"task_1\"}\n").expect("dataset");
        inv06_write_resolved_experiment(&run_dir, "tasks.jsonl", "run_1", "running");

        let _trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);
        ensure_dir(&run_dir.join("runtime")).expect("runtime dir");

        let mut schedule_progress = load_schedule_progress(&run_dir).expect("schedule progress");
        let mut pruned_variants: HashSet<usize> = HashSet::new();
        let mut consecutive_failures: BTreeMap<usize, usize> = BTreeMap::new();
        let mut slot_attempts: HashMap<usize, usize> = HashMap::new();
        let trial_result =
            TrialExecutionResult::minimal("trial_1".to_string(), "completed", Some(0));
        let mut run_sink = BufferedRunSink::default();
        let mut commit_state = test_slot_commit_state(
            &mut schedule_progress,
            1,
            &mut pruned_variants,
            &mut consecutive_failures,
        );
        RunCoordinator::commit_trial_slot(
            &run_dir,
            &PolicyConfig::default(),
            &mut commit_state,
            0,
            &trial_result,
            &mut run_sink,
            &mut slot_attempts,
        )
        .expect("commit trial slot");

        let runtime_state = runtime_trial_attempt_state_fixture(TrialPhase::CommitPending);
        BackingSqliteStore::open(&run_dir)
            .expect("store")
            .upsert_trial_attempt_state("run_1", "trial_1", &runtime_state)
            .expect("db runtime state");

        let recovered = recover_run(&run_dir, true).expect("recover");
        assert_eq!(recovered.active_trials_released, 0);

        let persisted = BackingSqliteStore::open(&run_dir)
            .expect("store")
            .load_latest_trial_attempt("run_1", "trial_1")
            .expect("db runtime")
            .expect("db runtime");
        assert_eq!(persisted.state.phase, TrialPhase::Committed);
    }

    #[test]
    fn recover_run_preserves_uncommitted_active_slot_for_continue() {
        let (_root, run_dir) = create_run_dir("bucephalus_recover_active_slot_gap", "run_1");
        let dataset_path = run_dir.join("tasks.jsonl");
        fs::write(&dataset_path, "{\"id\":\"task_1\"}\n{\"id\":\"task_2\"}\n").expect("dataset");
        inv06_write_resolved_experiment(&run_dir, "tasks.jsonl", "run_1", "running");

        let schedule = vec![
            TrialSlot {
                variant_idx: 0,
                task_idx: 0,
                repl_idx: 0,
            },
            TrialSlot {
                variant_idx: 0,
                task_idx: 1,
                repl_idx: 0,
            },
        ];
        write_schedule_progress(
            &run_dir,
            &ScheduleProgress {
                schema_version: "schedule_progress_v2".to_string(),
                run_id: "run_1".to_string(),
                total_slots: schedule.len(),
                next_schedule_index: 0,
                next_trial_index: 1,
                schedule,
                completed_slots: Vec::new(),
                pruned_variants: Vec::new(),
                consecutive_failures: BTreeMap::new(),
                updated_at: Utc::now().to_rfc3339(),
            },
        )
        .expect("schedule progress");

        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);
        {
            let mut store = BackingSqliteStore::open(&run_dir).expect("store");
            let progress = load_schedule_progress(&run_dir).expect("schedule progress");
            store
                .ensure_schedule_slots("run_1", &progress.schedule)
                .expect("schedule slots");
            store
                .claim_schedule_slot(
                    "run_1",
                    0,
                    "trial_1",
                    "worker_1",
                    "stale-owner",
                    None,
                )
                .expect("claim slot")
                .expect("claimed slot");
        }
        let active = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_1".to_string(),
            schedule_idx: Some(0),
            variant_id: "base".to_string(),
            started_at: Some(Utc::now().to_rfc3339()),
            control: Some(active_control_for_trial(&trial_dir)),
        }];
        write_run_control(&run_dir, "run_1", "running", &active, None).expect("run control");

        recover_run(&run_dir, true).expect("recover");

        let progress = load_schedule_progress(&run_dir).expect("schedule progress");
        let slot_zero_was_committed = progress
            .completed_slots
            .iter()
            .any(|slot| slot.schedule_index == 0);
        assert!(!slot_zero_was_committed);
        assert_eq!(progress.next_schedule_index, 0);
        let slot = BackingSqliteStore::open(&run_dir)
            .expect("store")
            .schedule_slot("run_1", 0)
            .expect("slot query")
            .expect("slot row");
        assert_eq!(slot.state, "pending");
        assert_eq!(slot.trial_id, None);
    }

    #[test]
    fn recover_run_releases_authoritative_active_slot_without_run_control_trial() {
        let (_root, run_dir) = create_run_dir("bucephalus_recover_orphan_active_slot", "run_1");
        let dataset_path = run_dir.join("tasks.jsonl");
        fs::write(&dataset_path, "{\"id\":\"task_1\"}\n").expect("dataset");
        inv06_write_resolved_experiment(&run_dir, "tasks.jsonl", "run_1", "running");

        {
            let progress = load_schedule_progress(&run_dir).expect("schedule progress");
            let mut store = BackingSqliteStore::open(&run_dir).expect("store");
            store
                .ensure_schedule_slots("run_1", &progress.schedule)
                .expect("schedule slots");
            store
                .claim_schedule_slot(
                    "run_1",
                    0,
                    "trial_1",
                    "worker_1",
                    "stale-owner",
                    None,
                )
                .expect("claim slot")
                .expect("claimed slot");
        }

        let recovered = recover_run(&run_dir, true).expect("recover");
        assert_eq!(recovered.active_trials_released, 1);

        let progress = load_schedule_progress(&run_dir).expect("schedule progress");
        assert!(progress.completed_slots.is_empty());
        assert_eq!(progress.next_schedule_index, 0);
        let slot = BackingSqliteStore::open(&run_dir)
            .expect("store")
            .schedule_slot("run_1", 0)
            .expect("slot query")
            .expect("slot row");
        assert_eq!(slot.state, "pending");
        assert_eq!(slot.trial_id, None);
        assert_eq!(slot.worker_id, None);
        assert_eq!(slot.owner_id, None);
    }

    #[test]
    fn recover_run_releases_durable_claim_intent_without_active_slot_row() {
        let (_root, run_dir) = create_run_dir("bucephalus_recover_claim_intent", "run_1");
        let dataset_path = run_dir.join("tasks.jsonl");
        fs::write(&dataset_path, "{\"id\":\"task_1\"}\n").expect("dataset");
        inv06_write_resolved_experiment(&run_dir, "tasks.jsonl", "run_1", "running");

        let trial_dir = seed_parent_trial(&run_dir, "trial_1", json!([]), "running", None);
        atomic_write_json_pretty(
            &trial_claim_intent_path(&trial_dir),
            &json!({
                "schema_version": "trial_claim_intent_v1",
                "run_id": "run_1",
                "trial_id": "trial_1",
                "schedule_idx": 0,
                "worker_id": "worker_1",
                "variant_id": "base",
                "started_at": Utc::now().to_rfc3339(),
                "created_at": Utc::now().to_rfc3339()
            }),
        )
        .expect("claim intent");

        let recovered = recover_run(&run_dir, true).expect("recover");
        assert_eq!(recovered.active_trials_released, 1);

        let slot = BackingSqliteStore::open(&run_dir)
            .expect("store")
            .schedule_slot("run_1", 0)
            .expect("slot query")
            .expect("slot row");
        assert_eq!(slot.state, "pending");
        assert_eq!(slot.trial_id, None);
        let trial_state = load_json_file(&trial_state_path(&trial_dir)).expect("trial state");
        assert_eq!(
            trial_state.pointer("/status").and_then(Value::as_str),
            Some("failed")
        );
        assert_eq!(
            trial_state.pointer("/exit_reason").and_then(Value::as_str),
            Some("worker_lost_recovered")
        );
    }

    #[test]
    fn recover_run_rejects_completed_and_killed_runs() {
        for status in ["completed", "killed"] {
            let (_root, run_dir) =
                create_run_dir(&format!("bucephalus_recover_terminal_{}", status), "run_1");
            let dataset_path = run_dir.join("tasks.jsonl");
            fs::write(&dataset_path, "{\"id\":\"task_1\"}\n").expect("dataset");
            inv06_write_resolved_experiment(&run_dir, "tasks.jsonl", "run_1", status);

            let err = recover_run(&run_dir, true).expect_err("terminal run should not recover");
            let msg = err.to_string();
            assert!(
                msg.contains("nothing to recover"),
                "unexpected recover error for {}: {}",
                status,
                msg
            );
        }
    }

    #[test]
    fn recover_run_rejects_preflight_failed_before_loading_schedule_progress() {
        let (_root, run_dir) = create_run_dir("bucephalus_recover_preflight_failed", "run_1");
        write_run_control(&run_dir, "run_1", "preflight_failed", &[], None)
            .expect("run control");
        write_run_session_state(
            &run_dir,
            "run_1",
            &RunBehavior::default(),
            &container_execution(),
        )
        .expect("run session");

        let err = recover_run(&run_dir, true).expect_err("preflight failure should not recover");
        let msg = err.to_string();
        assert!(
            msg.contains("failed preflight before schedule execution"),
            "unexpected recover error: {}",
            msg
        );
    }

    #[test]
    fn inv06_continue_handles_relative_and_absolute_dataset_paths() {
        let root = TempDirGuard::new("bucephalus_inv06_dataset_paths");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).expect("exp");
        let abs_dataset = root.path.join("dataset_abs.jsonl");
        fs::write(&abs_dataset, "{\"id\":\"task_1\"}\n").expect("abs dataset");

        for use_absolute in [false, true] {
            let dataset_path = if use_absolute {
                abs_dataset.to_string_lossy().to_string()
            } else {
                "tasks.jsonl".to_string()
            };
            let spec = json!({
                "matrix": { "tasks": { "source": "file", "path": dataset_path } }
            });
            let resolved = resolve_dataset_path(&spec, &exp_dir).expect("dataset path");
            let expected = if use_absolute {
                abs_dataset.clone()
            } else {
                exp_dir.join("tasks.jsonl")
            };
            assert_eq!(
                resolved, expected,
                "dataset mode absolute={} should resolve correctly",
                use_absolute
            );
        }
    }

    #[test]
    fn inv06_load_tasks_honors_zero_limit() {
        let root = TempDirGuard::new("bucephalus_inv06_load_tasks_limit_zero");
        let dataset_path = root.path.join("tasks.jsonl");
        fs::write(
            &dataset_path,
            concat!(
                "{\"schema_version\":\"task_row_v2\",\"id\":\"task_1\",\"task\":{\"id\":\"task_1\"},\"runtime\":{\"container_image\":{\"image\":\"python:3.11-slim\",\"workdir\":\"/workspace/task\"}}}\n",
                "{\"schema_version\":\"task_row_v2\",\"id\":\"task_2\",\"task\":{\"id\":\"task_2\"},\"runtime\":{\"container_image\":{\"image\":\"python:3.11-slim\",\"workdir\":\"/workspace/task\"}}}\n",
                "{\"schema_version\":\"task_row_v2\",\"id\":\"task_3\",\"task\":{\"id\":\"task_3\"},\"runtime\":{\"container_image\":{\"image\":\"python:3.11-slim\",\"workdir\":\"/workspace/task\"}}}\n"
            ),
        )
        .expect("dataset");
        let spec = json!({
            "matrix": { "tasks": { "source": "file", "limit": 0 } }
        });

        let tasks = load_tasks(&dataset_path, &spec).expect("load tasks");
        assert!(
            tasks.is_empty(),
            "matrix.tasks.limit=0 should produce zero loaded tasks"
        );
    }

    #[test]
    fn inv06_count_tasks_honors_zero_limit() {
        let root = TempDirGuard::new("bucephalus_inv06_count_tasks_limit_zero");
        let dataset_path = root.path.join("tasks.jsonl");
        fs::write(
            &dataset_path,
            "{\"id\":\"task_1\"}\n{\"id\":\"task_2\"}\n{\"id\":\"task_3\"}\n",
        )
        .expect("dataset");
        let spec = json!({
            "matrix": { "tasks": { "source": "file", "limit": 0 } }
        });

        let count = count_tasks(&dataset_path, &spec).expect("count tasks");
        assert_eq!(count, 0, "matrix.tasks.limit=0 should produce zero task count");
    }

    #[test]
    fn inv06_load_task_rows_for_build_reads_task_rows() {
        let root = TempDirGuard::new("bucephalus_inv06_load_task_rows_for_build");
        let dataset_path = root.path.join("task_rows.jsonl");
        write_task_row_dataset(&dataset_path, "task_1");
        let spec = json!({
            "matrix": { "tasks": { "source": "file", "limit": 1 } }
        });

        let tasks = load_task_rows_for_build(&dataset_path, &spec).expect("load task rows");
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].get("schema_version").and_then(Value::as_str),
            Some("task_row_v2")
        );
        assert_eq!(
            tasks[0].pointer("/id").and_then(Value::as_str),
            Some("task_1")
        );
    }

    #[test]
    fn load_task_rows_for_build_rejects_reserved_contract_workdir() {
        let root = TempDirGuard::new("bucephalus_task_row_reserved_workdir");
        let dataset_path = root.path.join("task_rows.jsonl");
        fs::write(
            &dataset_path,
            r#"{"schema_version":"task_row_v2","id":"task_1","task":{"id":"task_1"},"runtime":{"container_image":{"image":"python:3.11-slim","workdir":"/bucephalus/out"}}}"#,
        )
        .expect("dataset");
        let spec = json!({
            "matrix": { "tasks": { "source": "file", "limit": 1 } }
        });

        let err = load_task_rows_for_build(&dataset_path, &spec)
            .expect_err("task rows must not use reserved runner contract paths as workdir");
        let msg = err.to_string();
        assert!(
            msg.contains("runtime.container_image.workdir must be under"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn inv06_build_load_task_rows_rejects_task_boundary_rows() {
        let root = TempDirGuard::new("bucephalus_inv06_build_rejects_task_boundary");
        let dataset_path = root.path.join("task_rows.jsonl");
        fs::write(
            &dataset_path,
            "{\"task\":{\"id\":\"task_1\"},\"environment\":{\"image\":\"python:3.11-slim\"},\"workspace\":{\"mode\":\"scratch\",\"base\":{\"kind\":\"empty\"},\"overlays\":[],\"aux_mounts\":[]},\"dependencies\":{},\"limits\":{}}\n",
        )
        .expect("dataset");
        let spec = json!({
            "matrix": { "tasks": { "source": "file", "limit": 1 } }
        });

        let err = load_task_rows_for_build(&dataset_path, &spec)
            .expect_err("build should reject task row");
        assert!(
            err.to_string().contains("case_v2"),
            "unexpected runtime error: {}",
            err
        );
    }

    #[test]
    fn dataset_provider_rejects_unsupported_value_during_build() {
        let root = TempDirGuard::new("bucephalus_dataset_provider_build");
        let dataset_path = root.path.join("tasks.jsonl");
        fs::write(&dataset_path, "").expect("dataset");
        let spec = json!({
            "matrix": { "tasks": { "source": "remote_http", "limit": 1 } }
        });

        let err = load_task_rows_for_build(&dataset_path, &spec)
            .expect_err("unsupported provider should fail package build");
        assert!(
            err.to_string().contains("matrix.tasks.source='remote_http' is not supported"),
            "unexpected provider error: {}",
            err
        );
    }

    #[test]
    fn dataset_provider_rejects_unsupported_value_during_runtime_load() {
        let root = TempDirGuard::new("bucephalus_dataset_provider_runtime");
        let dataset_path = root.path.join("tasks.jsonl");
        fs::write(&dataset_path, "").expect("dataset");
        let spec = json!({
            "matrix": { "tasks": { "source": "remote_http", "limit": 1 } }
        });

        let err = load_tasks(&dataset_path, &spec)
            .expect_err("unsupported provider should fail runtime load");
        assert!(
            err.to_string().contains("matrix.tasks.source='remote_http' is not supported"),
            "unexpected provider error: {}",
            err
        );
    }

    #[test]
    fn inv06_runtime_load_tasks_rejects_legacy_task_declaration_rows() {
        let root = TempDirGuard::new("bucephalus_inv06_runtime_rejects_legacy_task_declaration");
        let dataset_path = root.path.join("tasks.jsonl");
        fs::write(
            &dataset_path,
            "{\"schema_version\":\"task_declaration_v1\",\"task\":{\"id\":\"task_1\"},\"environment\":{\"image\":\"python:3.11-slim\"},\"workspace\":{\"mode\":\"scratch\",\"base\":{\"kind\":\"empty\"},\"overlays\":[],\"aux_mounts\":[]},\"dependencies\":{},\"limits\":{}}\n",
        )
        .expect("dataset");
        let spec = json!({
            "matrix": { "tasks": { "source": "file", "limit": 1 } }
        });

        let err =
            load_tasks(&dataset_path, &spec).expect_err("runtime should reject legacy declaration");
        assert!(
            err.to_string().contains("case_v2"),
            "unexpected runtime error: {}",
            err
        );
    }

    #[test]
    fn copy_dir_filtered_preserves_directory_symlinks_without_recursing() {
        let root = TempDirGuard::new("bucephalus_copy_dir_filtered_symlink");
        let workspace = root.path.join("workspace");
        ensure_dir(&workspace).expect("workspace");
        fs::write(workspace.join("keep.txt"), "keep").expect("keep");
        symlink(Path::new("."), workspace.join("loop")).expect("loop symlink");

        let copied = root.path.join("copied");
        copy_dir_filtered(&workspace, &copied, &[]).expect("copy");

        let copied_loop = copied.join("loop");
        let metadata = fs::symlink_metadata(&copied_loop).expect("copied symlink metadata");
        assert!(
            metadata.file_type().is_symlink(),
            "{:?}",
            metadata.file_type()
        );
        assert_eq!(
            fs::read_link(&copied_loop).expect("copied symlink target"),
            PathBuf::from(".")
        );
    }

    #[test]
    fn outputs_only_materialization_exposes_runtime_surfaces_after_scratch_cleanup() {
        let root = TempDirGuard::new("bucephalus_outputs_only_materialization");
        let run_dir = root.path.join("runs").join("run_1");
        let trial_dir = run_dir.join("trials").join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");

        let trial_paths = TrialPaths::new(&trial_dir, &root.path).expect("trial paths");
        trial_paths.prepare(false).expect("prepare trial paths");
        fs::write(
            trial_paths.runtime.result.clone(),
            "{\"ok\":true}\n",
        )
        .expect("write result");
        fs::write(
            trial_paths.out.join("agent_report.json"),
            "{\"cwd\":\"/bucephalus/workspace\"}\n",
        )
        .expect("write agent report");
        fs::write(trial_paths.out.join("candidate.patch"), "diff --git a/x b/x\n")
            .expect("write candidate patch");
        fs::write(trial_paths.runtime.trajectory.clone(), "{}\n").expect("write events");
        fs::write(
            trial_paths.out.join(MAPPED_GRADER_OUTPUT_FILENAME),
            "{\"mapped\":true}\n",
        )
        .expect("write mapped grader output");
        fs::write(
            trial_paths.out.join("harness_manifest.json"),
            "{\"schema_version\":\"harness_manifest_v1\"}\n",
        )
        .expect("write harness manifest");
        let host_eval = trial_paths.out.join("host_eval");
        ensure_dir(&host_eval).expect("host eval dir");
        fs::write(host_eval.join("report.json"), "{}\n").expect("write host eval report");
        ensure_dir(&trial_runner_dir(&trial_dir)).expect("runner dir");
        fs::write(
            trial_contract_trace_path(&trial_dir),
            "{\"schema_version\":\"trial_contract_trace_v1\"}\n",
        )
        .expect("write contract trace");

        materialize_trial_runtime_layout(
            &trial_dir,
            &trial_paths,
            &json!({
                "extra_outputs": [
                    {
                        "id": "host_eval",
                        "source_path": "host_eval",
                        "summary_path": "grader/host_eval"
                    }
                ]
            }),
            MaterializationMode::OutputsOnly,
        )
        .expect("materialize outputs");
        trial_paths.cleanup_scratch().expect("cleanup scratch");

        assert!(!trial_dir.join("out").exists());
        assert!(
            trial_agent_dir(&trial_dir).join("result.json").exists(),
            "agent result should be materialized under the agent runtime surface"
        );
        assert!(!trial_dir.join("result.json").exists());
        assert!(!trial_dir.join("runtime").exists());
        assert!(trial_candidate_patch_path(&trial_dir).exists());
        assert!(
            trial_agent_dir(&trial_dir).join("events.jsonl").exists(),
            "agent events should live under the agent runtime surface"
        );
        assert!(
            trial_grader_dir(&trial_dir).join("mapped_output.json").exists(),
            "mapped grader output should live under the grader runtime surface"
        );
        assert!(
            trial_grader_dir(&trial_dir)
                .join("host_eval")
                .join("report.json")
                .exists(),
            "host eval files should live under the grader runtime surface"
        );
        assert!(
            trial_runner_dir(&trial_dir)
                .join("harness_manifest.json")
                .exists(),
            "runner-owned harness manifest should live under the runner surface"
        );
        assert!(
            trial_contract_trace_path(&trial_dir).exists(),
            "contract trace should live under the runner surface"
        );
        for sloppy_root_file in [
            "harness_stdout.log",
            "harness_stderr.log",
            "grader_stdout.log",
            "grader_stderr.log",
            "trial_preflight.json",
            "trial_metadata.json",
            "state_inventory.json",
            "harness_manifest.json",
            "trial_runtime_state.json",
            "trial_state.json",
        ] {
            assert!(
                !trial_dir.join(sloppy_root_file).exists(),
                "{} should not be written at the trial root",
                sloppy_root_file
            );
        }
    }

    #[test]
    fn cleanup_scratch_removes_read_only_dependency_tree() {
        let root = TempDirGuard::new("bucephalus_cleanup_scratch_read_only");
        let run_dir = root.path.join("runs").join("run_1");
        let trial_dir = run_dir.join("trials").join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");

        let trial_paths = TrialPaths::new(&trial_dir, &root.path).expect("trial paths");
        trial_paths.prepare(false).expect("prepare trial paths");

        let support_dir = trial_paths
            .workspace
            .join(BUCEPHALUS_RUNNER_SUPPORT_REL_DIR)
            .join("evaluation")
            .join("integration")
            .join("bucephalus");
        ensure_dir(&support_dir).expect("support dir");
        fs::write(
            support_dir.join("evaluation_harness.py"),
            "#!/usr/bin/env python3\nprint('ok')\n",
        )
        .expect("support file");
        set_staged_path_read_only(&trial_paths.workspace.join(BUCEPHALUS_RUNNER_SUPPORT_REL_DIR))
            .expect("mark support tree read only");

        trial_paths.cleanup_scratch().expect("cleanup scratch");

        assert!(
            !trial_paths.scratch_dir.exists(),
            "scratch dir should be removed even when staged support files are read only"
        );
    }

    #[test]
    fn outputs_only_materialization_preserves_directory_symlinks_without_recursing() {
        let root = TempDirGuard::new("bucephalus_outputs_only_symlink_materialization");
        let run_dir = root.path.join("runs").join("run_1");
        let trial_dir = run_dir.join("trials").join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");

        let trial_paths = TrialPaths::new(&trial_dir, &root.path).expect("trial paths");
        trial_paths.prepare(false).expect("prepare trial paths");
        fs::write(
            trial_paths.runtime.result.clone(),
            "{\"ok\":true}\n",
        )
        .expect("write result");
        fs::write(trial_paths.out.join("keep.txt"), "keep").expect("write out file");
        symlink(Path::new("."), trial_paths.out.join("loop")).expect("loop symlink");

        materialize_trial_runtime_layout(
            &trial_dir,
            &trial_paths,
            &json!({}),
            MaterializationMode::Full,
        )
        .expect("materialize outputs");

        let materialized_loop = trial_dir.join("out").join("loop");
        let metadata = fs::symlink_metadata(&materialized_loop).expect("materialized symlink");
        assert!(
            metadata.file_type().is_symlink(),
            "{:?}",
            metadata.file_type()
        );
        assert_eq!(
            fs::read_link(&materialized_loop).expect("materialized symlink target"),
            PathBuf::from(".")
        );
    }

    #[test]
    fn inv04_agent_artifact_mount_cache_unpacks_tar_once() {
        let root = TempDirGuard::new("bucephalus_inv04_artifact_mount_cache");
        let artifact_src = root.path.join("artifact_src");
        ensure_dir(&artifact_src).expect("artifact src");
        fs::write(artifact_src.join("agent.txt"), "agent payload").expect("artifact payload");
        let artifact_tar = root.path.join("agent-runtime.tar.gz");
        let tar_status = Command::new("tar")
            .args([
                "-czf",
                artifact_tar.to_string_lossy().as_ref(),
                "-C",
                artifact_src.to_string_lossy().as_ref(),
                ".",
            ])
            .status()
            .expect("create tar");
        assert!(tar_status.success(), "failed to create artifact tarball");

        let first_mount = resolve_agent_artifact_mount_dir(&artifact_tar).expect("first unpack");
        assert!(
            !first_mount.to_string_lossy().contains(':'),
            "artifact mount cache path must be colon-safe for docker bind mounts: {}",
            first_mount.display()
        );
        assert!(
            first_mount.join("agent.txt").exists(),
            "unpacked artifact payload missing"
        );

        let second_mount = resolve_agent_artifact_mount_dir(&artifact_tar)
            .expect("second unpack should be cached");
        assert_eq!(
            first_mount, second_mount,
            "artifact mount path should be stable across repeated calls"
        );
        assert!(
            second_mount.join(".bucephalus_ready").exists(),
            "cached artifact should include ready marker"
        );
    }

    #[test]
    fn agent_artifact_directory_mount_returns_canonical_path() -> Result<()> {
        let root = TempDirGuard::new("bucephalus_artifact_dir_canonical");
        let artifact_dir = root.path.join("artifact").join("..").join("artifact");
        ensure_dir(&artifact_dir)?;
        fs::write(artifact_dir.join("agent.txt"), "agent payload")?;

        let mount_dir = resolve_agent_artifact_mount_dir(&artifact_dir)?;

        assert_eq!(mount_dir, fs::canonicalize(&artifact_dir)?);
        assert!(mount_dir.join("agent.txt").exists());
        Ok(())
    }

    #[test]
    fn agent_artifact_mount_cache_removes_stale_same_digest_staging_dirs() -> Result<()> {
        let root = TempDirGuard::new("bucephalus_artifact_mount_cache_stale_tmp");
        let artifact_src = root.path.join("artifact_src");
        ensure_dir(&artifact_src)?;
        fs::write(artifact_src.join("agent.txt"), "agent payload")?;
        let artifact_tar = root.path.join("agent-runtime.tar.gz");
        let tar_status = Command::new("tar")
            .args([
                "-czf",
                artifact_tar.to_string_lossy().as_ref(),
                "-C",
                artifact_src.to_string_lossy().as_ref(),
                ".",
            ])
            .status()?;
        assert!(tar_status.success(), "failed to create artifact tarball");

        let digest = sha256_file(&artifact_tar)?;
        let digest_path_component = digest.replace(':', "_");
        let cache_root = root.path.join(".bucephalus_artifact_cache");
        ensure_dir(&cache_root)?;
        let stale_staging = cache_root.join(format!("{}.tmp.old-run", digest_path_component));
        ensure_dir(&stale_staging)?;
        fs::write(stale_staging.join("partial"), "partial unpack")?;

        let mount_dir = resolve_agent_artifact_mount_dir(&artifact_tar)?;

        assert!(mount_dir.join("agent.txt").exists());
        assert!(
            !stale_staging.exists(),
            "stale same-digest artifact staging dir should be cleaned before unpack"
        );
        Ok(())
    }

    fn write_raw_tar_file(path: &Path, entry_name: &str, payload: &[u8]) {
        let mut header = [0u8; 512];
        let name = entry_name.as_bytes();
        assert!(name.len() <= 100, "test tar entry name too long");
        header[..name.len()].copy_from_slice(name);
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size = format!("{:011o}\0", payload.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        for byte in &mut header[148..156] {
            *byte = b' ';
        }
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        let checksum_text = format!("{:06o}\0 ", checksum);
        header[148..156].copy_from_slice(checksum_text.as_bytes());

        let mut archive = Vec::new();
        archive.extend_from_slice(&header);
        archive.extend_from_slice(payload);
        let padding = (512 - (payload.len() % 512)) % 512;
        archive.extend(std::iter::repeat_n(0, padding));
        archive.extend(std::iter::repeat_n(0, 1024));
        fs::write(path, archive).expect("write raw tar");
    }

    #[test]
    fn agent_artifact_archive_rejects_parent_path_entries() {
        let root = TempDirGuard::new("bucephalus_artifact_tar_escape");
        let artifact_tar = root.path.join("agent-runtime.tar");
        write_raw_tar_file(&artifact_tar, "../escape.txt", b"escape");

        let err = resolve_agent_artifact_mount_dir(&artifact_tar)
            .expect_err("archive entries with parent components must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("must not contain '..'"),
            "unexpected error: {msg}"
        );
        assert!(
            !root.path.join("escape.txt").exists(),
            "escaping archive entry must not be written outside the artifact cache"
        );
    }

    fn create_dx_authoring_fixture(prefix: &str) -> TempDirGuard {
        let root = TempDirGuard::new(prefix);
        let dataset_dir = root.path.join(".lab").join("experiments").join("data");
        ensure_dir(&dataset_dir).expect("dataset dir");
        let workspace_base_digest = "f".repeat(64);
        let workspace_base_pack = root
            .path
            .join(".lab")
            .join("dataset_packs")
            .join("sha256")
            .join(&workspace_base_digest);
        ensure_dir(&workspace_base_pack).expect("workspace base pack dir");
        fs::write(workspace_base_pack.join("README.md"), "seed").expect("workspace base content");
        let eval_suite_row =
            r#"{"schema_version":"task_row_v2","id":"TASK001","time_limit_ms":600000,"task":{"id":"TASK001"},"runtime":{"container_image":{"image":"python:3.11-slim","workdir":"/workspace/task"}}}"#
                .to_string();
        fs::write(dataset_dir.join("eval_suite.task_rows.jsonl"), &eval_suite_row)
            .expect("dataset row");
        let project_eval_row = r#"{"schema_version":"task_row_v2","id":"project_eval_issue_12907","task":{"id":"project_eval_issue_12907","workload":{"input":{"repo":"example/project","instance_id":"project__issue-12907","base_commit":"deadbeef"}}},"runtime":{"container_image":{"image":"registry.example.invalid/project.eval.x86_64.project__issue-12907:latest","workdir":"/testbed"}}}"#;
        fs::write(
            dataset_dir.join("project_eval_curated.task_rows.jsonl"),
            project_eval_row,
        )
        .expect("project eval dataset row");
        write_test_host_grader_capability_manifest(&root.path);

        let artifact_bin = root
            .path
            .join(".lab")
            .join("agents")
            .join("agent-minimal-linux-dir")
            .join("bin");
        ensure_dir(&artifact_bin).expect("artifact dir");
        let artifact_entrypoint = artifact_bin.join("agentctl");
        fs::write(&artifact_entrypoint, "#!/bin/sh\necho agentctl\n").expect("artifact binary");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&artifact_entrypoint)
                .expect("artifact metadata")
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&artifact_entrypoint, perms).expect("artifact executable perms");
        }

        let overrides_dir = root.path.join(".lab").join("experiments").join("overrides");
        ensure_dir(&overrides_dir).expect("overrides dir");
        fs::write(
            overrides_dir.join("providers.a.ts"),
            "export const P='A';\n",
        )
        .expect("patch A");
        fs::write(
            overrides_dir.join("providers.b.ts"),
            "export const P='B';\n",
        )
        .expect("patch B");
        fs::write(
            overrides_dir.join("defaults.eval-lmstudio-headless.json"),
            "{\n  \"models\": {\"default\": \"\"}\n}\n",
        )
        .expect("defaults config");
        fs::write(
            root.path.join("defaults.eval-lmstudio-headless.json"),
            "{\n  \"models\": {\"default\": \"\"}\n}\n",
        )
        .expect("root defaults config");
        let codex_auth_dir = overrides_dir.join(".config").join("agent");
        ensure_dir(&codex_auth_dir).expect("codex auth dir");
        fs::write(codex_auth_dir.join("codex-auth.json"), "{}\n").expect("codex auth");

        let grader_dir = root.path.join("evaluation_support").join("integration").join("bucephalus");
        ensure_dir(&grader_dir).expect("grader dir");
        fs::write(
            grader_dir.join("evaluation_harness.py"),
            "#!/usr/bin/env python3\nprint('ok')\n",
        )
        .expect("grader executable");
        root
    }

    fn agent_artifact_value(source: &str) -> Value {
        let source = if source.starts_with('.') || source.starts_with('/') {
            source.to_string()
        } else {
            format!(".lab/agents/{}", source)
        };
        json!({
            "source": source,
            "mount": {
                "path": "/opt/agent",
                "read_only": true
            }
        })
    }

    fn set_all_variant_agent_override(spec: &mut Value, field: &str, value: Value) {
        let variants = spec
            .pointer_mut("/matrix/variants")
            .and_then(Value::as_array_mut)
            .expect("matrix variants");
        for variant in variants {
            let agent = variant
                .pointer_mut("/overrides/agent")
                .and_then(Value::as_object_mut)
                .expect("variant agent override");
            agent.insert(field.to_string(), value.clone());
        }
    }

    fn minimal_dx_spec() -> Value {
        json!({
            "experiment": {
                "id": "eval_suite_qwen35b_a3b_only",
                "name": "Eval Suite: Qwen3.5 35B A3B",
                "tags": ["eval-v0", "single-variant"]
            },
            "matrix": {
                "tasks": {
                    "source": "file",
                    "path": ".lab/experiments/data/eval_suite.task_rows.jsonl",
                    "suite_id": "eval_suite",
                    "split_id": "test",
                    "limit": 20
                },
                "variants": [{
                    "id": "qwen_35b_a3b",
                    "baseline": true,
                    "config": { "model_provider": "lmstudio", "model": "qwen3.5-35b-a3b" }
                }],
                "repeats": 1
            },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "storage": { "backend": "local-fs" },
                "traces": { "backend": "local-stdout" },
                "network": { "task_sandbox": "full", "agent": "full" }
            },
            "policy": {
                "timeout_ms": 600000,
                "task_sandbox": {}
            },
            "metrics": [{
                "id": "resolved",
                "source": {
                    "type": "grader_output",
                    "output": "mapped",
                    "pointer": "/payload/resolved"
                },
                "primary": true
            }],
            "trial_runtime": {
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": {"from": "task_row"},
                        "workdir": {"from": "task_row"}
                    }
                },
                "agent": {
                    "mount": agent_artifact_value("agent-minimal-linux-dir"),
                    "image": "python:3.11-slim",
                    "artifact_type": "structured_json",
                    "command": [
                        "agentctl",
                        "run",
                        "--dangerous",
                        "--config",
                        "defaults.eval-lmstudio-headless.json",
                        "--provider",
                        "$model_provider",
                        "--model",
                        "$model"
                    ],
                    "env": { "MEMORY_DAEMON_URL": "" },
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json"
                            }
                        }
                    }
                },
                "execution": { "agent_site": "agent_container" },
                "grader": {
                    "strategy": "in_task_runtime",
                    "in_task_runtime": {},
                    "command": [
                        "python3",
                        "__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/evaluation/integration/bucephalus/evaluation_harness.py"
                    ],
                    "outputs": {
                        "mapped": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/mapped_grader_output.json",
                                "format": "json"
                            }
                        }
                    },
                    "_runtime_assets": [{
                        "build_source_path": "evaluation_support", "read_only": true, "required": true,
                        "runtime_path": "__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/evaluation"
                    }]
                }
            }
        })
    }

    fn minimal_new_dx_spec() -> Value {
        json!({
            "experiment": {
                "id": "eval_suite_multi_build",
                "name": "Eval Suite Multi Build",
                "tags": ["eval-v0", "multi-build"]
            },
            "matrix": {
                "tasks": {
                    "source": "file",
                    "path": ".lab/experiments/data/eval_suite.task_rows.jsonl",
                    "suite_id": "eval_suite",
                    "split_id": "test",
                    "limit": 10
                },
                "variants": [
                {
                    "id": "qwen",
                    "baseline": true,
                    "config": { "model_provider": "lmstudio", "model": "qwen3.5-35b-a3b" },
                    "overrides": {
                        "agent": {
                            "mount": agent_artifact_value("agent-minimal-linux-dir"),
                            "image": "python:3.11-slim",
                            "env": { "BASELINE_ONLY": "1" },
                            "command": [
                                "agentctl",
                                "run",
                                "--dangerous",
                                "--config",
                                "defaults.eval-lmstudio-headless.json",
                                "--provider",
                                "$model_provider",
                                "--model",
                                "$model"
                            ]
                        }
                    }
                },
                {
                    "id": "sonnet",
                    "config": { "model_provider": "anthropic", "model": "claude-sonnet-4" },
                    "overrides": {
                        "agent": {
                            "mount": agent_artifact_value("agent-minimal-linux-dir"),
                            "image": "python:3.11-slim",
                            "env": { "ANTHROPIC_REGION": "us" },
                            "command": [
                                "agentctl",
                                "run",
                                "--alternate",
                                "--config",
                                "defaults.eval-lmstudio-headless.json",
                                "--provider",
                                "$model_provider",
                                "--model",
                                "$model"
                            ]
                        }
                    }
                }
            ],
                "repeats": 1
            },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "storage": { "backend": "local-fs" },
                "traces": { "backend": "local-stdout" },
                "network": { "task_sandbox": "full", "agent": "full" }
            },
            "policy": {
                "timeout_ms": 600000,
                "task_sandbox": {}
            },
            "metrics": [{
                "id": "resolved",
                "source": {
                    "type": "grader_output",
                    "output": "mapped",
                    "pointer": "/payload/resolved"
                },
                "primary": true
            }],
            "trial_runtime": {
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": {"from": "task_row"},
                        "workdir": {"from": "task_row"}
                    }
                },
                "agent": {
                    "mount": agent_artifact_value("agent-minimal-linux-dir"),
                    "image": "python:3.11-slim",
                    "artifact_type": "structured_json",
                    "command": [
                        "agentctl",
                        "run",
                        "--dangerous",
                        "--config",
                        "defaults.eval-lmstudio-headless.json",
                        "--provider",
                        "$model_provider",
                        "--model",
                        "$model"
                    ],
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json"
                            }
                        }
                    }
                },
                "execution": { "agent_site": "agent_container" },
                "grader": {
                    "strategy": "in_task_runtime",
                    "in_task_runtime": {},
                    "command": [
                        "python3",
                        "__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/evaluation/integration/bucephalus/evaluation_harness.py"
                    ],
                    "outputs": {
                        "mapped": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/mapped_grader_output.json",
                                "format": "json"
                            }
                        }
                    },
                    "_runtime_assets": [{
                        "build_source_path": "evaluation_support", "read_only": true, "required": true,
                        "runtime_path": "__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/evaluation"
                    }]
                }
            }
        })
    }

    fn minimal_project_eval_dx_spec() -> Value {
        json!({
            "experiment": {
                "id": "project_eval_qwen35b_a3b_only",
                "name": "Project Eval Suite: Qwen3.5 35B A3B",
                "tags": ["project-eval", "single-variant"]
            },
            "matrix": {
                "tasks": {
                    "source": "file",
                    "path": ".lab/experiments/data/project_eval_curated.task_rows.jsonl",
                    "suite_id": "project_eval_curated",
                    "split_id": "test",
                    "limit": 20
                },
                "variants": [{
                    "id": "qwen_35b_a3b",
                    "baseline": true,
                    "config": { "model_provider": "lmstudio", "model": "qwen3.5-35b-a3b" }
                }],
                "repeats": 1
            },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "storage": { "backend": "local-fs" },
                "traces": { "backend": "local-stdout" },
                "network": { "task_sandbox": "full", "agent": "full" }
            },
            "policy": {
                "timeout_ms": 600000,
                "task_sandbox": {}
            },
            "metrics": [{
                "id": "resolved",
                "source": {
                    "type": "grader_output",
                    "output": "mapped",
                    "pointer": "/payload/resolved"
                },
                "primary": true
            }],
            "trial_runtime": {
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": {
                            "from": "task_row",
                            "rewrites": [{
                                "match_prefix": "registry.example.invalid/project.eval.x86_64.",
                                "replace_prefix": "ghcr.io/acme/project.eval.x86_64.",
                                "platform": "linux/amd64"
                            }]
                        },
                        "workdir": {"from": "task_row"}
                    }
                },
                "agent": {
                    "mount": agent_artifact_value("agent-minimal-linux-dir"),
                    "image": "python:3.11-slim",
                    "artifact_type": "structured_json",
                    "command": [
                        "agentctl",
                        "run",
                        "--dangerous",
                        "--provider",
                        "$model_provider",
                        "--model",
                        "$model"
                    ],
                    "env": { "MEMORY_DAEMON_URL": "" },
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json"
                            }
                        }
                    }
                },
                "execution": { "agent_site": "agent_container" },
                "grader": {
                    "strategy": "host",
                    "host": {"capability": TEST_HOST_GRADER_CAPABILITY},
                    "command": [
                        "python3",
                        "__BUCEPHALUS_HOST_GRADER_CAPABILITY__/host_eval_capability/run_host_evaluation.py",
                        "--grader-input"
                    ],
                    "outputs": {
                        "mapped": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/mapped_grader_output.json",
                                "format": "json"
                            }
                        }
                    }
                }
            }
        })
    }

    fn write_executable_script(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write script");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(path).expect("script metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).expect("script executable perms");
        }
    }

    fn write_test_host_grader_capability_manifest(project_root: &Path) {
        let capability_dir = project_root
            .join("manifests")
            .join("grader_capabilities")
            .join(TEST_HOST_GRADER_CAPABILITY);
        let capability_root = capability_dir.join("files");
        ensure_dir(&capability_root).expect("capability root");
        fs::write(
            capability_root.join("run_host_evaluation.py"),
            "#!/usr/bin/env python3\nprint('ok')\n",
        )
        .expect("host grader capability script");
        atomic_write_json_pretty(
            &capability_dir.join("capability.json"),
            &json!({
                "id": TEST_HOST_GRADER_CAPABILITY,
                "runtime": {"kind": "host"},
                "root": capability_root.to_string_lossy().to_string(),
                "allowed_paths": ["run_host_evaluation.py"]
            }),
        )
        .expect("host grader capability manifest");
    }

    #[test]
    fn build_experiment_package_rewrites_runtime_sources() {
        let root = create_dx_authoring_fixture("bucephalus_build_package");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let out_dir = root.path.join("package");
        let build =
            build_experiment_package(&spec_path, None, Some(&out_dir)).expect("build package");
        assert!(build.manifest_path.exists(), "manifest missing");
        assert!(build.checksums_path.exists(), "checksums missing");
        assert!(
            build.package_dir.join(STAGING_MANIFEST_FILE).exists(),
            "runtime staging manifest missing"
        );

        let manifest = load_json_file(&build.manifest_path).expect("manifest json");
        assert_eq!(
            manifest
                .pointer("/schema_version")
                .and_then(Value::as_str)
                .unwrap_or(""),
            "sealed_run_package_v2"
        );
        assert_eq!(
            manifest
                .pointer("/resolved_experiment/matrix/tasks/path")
                .and_then(Value::as_str)
                .unwrap_or(""),
            "tasks/tasks.jsonl"
        );
        let packaged_tasks =
            load_jsonl_value_rows(&build.package_dir.join("tasks").join("tasks.jsonl"))
                .expect("packaged tasks");
        assert_eq!(packaged_tasks.len(), 1);
        let packaged_task_row = parse_task_row(&packaged_tasks[0]).expect("packaged task row");
        assert_eq!(packaged_task_row.schema_version, "task_row_v2");
        let artifact = manifest
            .pointer("/resolved_experiment/matrix/variants/0/overrides/agent/mount/source")
            .and_then(Value::as_str)
            .unwrap_or("");
        assert!(
            artifact.starts_with("agent_builds/"),
            "artifact path should be packaged, got {}",
            artifact
        );
        assert_eq!(
            manifest
                .pointer("/resolved_experiment/trial_runtime/agent/command/4")
                .and_then(Value::as_str)
                .unwrap_or(""),
            "__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/defaults.eval-lmstudio-headless.json"
        );
        assert!(
            build
                .package_dir
                .join(PACKAGED_RUNTIME_ASSETS_DIR)
                .join("defaults.eval-lmstudio-headless.json")
                .exists(),
            "relative command path should be copied into packaged runtime assets"
        );
        let staging_manifest = load_json_file(&build.package_dir.join(STAGING_MANIFEST_FILE))
            .expect("staging manifest");
        assert_eq!(
            staging_manifest
                .pointer("/schema_version")
                .and_then(Value::as_str)
                .unwrap_or(""),
            STAGING_MANIFEST_SCHEMA_VERSION
        );
        assert!(
            staging_manifest
                .pointer("/variants/qwen")
                .and_then(Value::as_array)
                .is_some_and(|entries| entries.iter().any(|entry| {
                    entry.pointer("/runtime_path").and_then(Value::as_str)
                        == Some("__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/evaluation")
                })),
            "qwen variant should include grader support directory staging entry"
        );
        assert!(
            staging_manifest
                .pointer("/variants/qwen")
                .and_then(Value::as_array)
                .is_some_and(|entries| entries.iter().any(|entry| {
                    entry.pointer("/runtime_path")
                        .and_then(Value::as_str)
                        == Some("__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/defaults.eval-lmstudio-headless.json")
                        && entry.pointer("/packaged_path").and_then(Value::as_str)
                            == Some("runtime_assets/defaults.eval-lmstudio-headless.json")
                })),
            "qwen variant should include rewritten runtime config staging entry"
        );
        let summary = experiment_summary(&build.package_dir).expect("load experiment summary");
        assert_eq!(summary.exp_id, "eval_suite_multi_build");
        assert_eq!(summary.task_count, 1);
    }

    #[test]
    fn experiment_summary_rejects_malformed_packaged_tasks() {
        let root = create_dx_authoring_fixture("bucephalus_summary_bad_tasks");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        let tasks_path = build.package_dir.join("tasks").join("tasks.jsonl");
        fs::write(&tasks_path, "{\"id\":\"TASK001\",\"task\":{\"id\":\"TASK001\"}}\n")
            .expect("write malformed tasks");
        reseal_package_after_payload_change(&build.package_dir, &build.manifest_path, &tasks_path);

        let err = experiment_summary(&build.package_dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("failed to load tasks for experiment summary preflight")
                && msg.contains("not a valid packaged case_v1, case_v2, or task_row_v2"),
            "unexpected error: {msg}"
        );
    }

    fn reseal_package_after_payload_change(package_dir: &Path, manifest_path: &Path, changed: &Path) {
        let changed_rel = changed
            .strip_prefix(package_dir)
            .map(as_portable_rel)
            .expect("changed path under package dir");
        let checksums_path = package_dir.join("checksums.json");
        let mut checksums = load_json_file(&checksums_path).expect("checksums json");
        let files = checksums
            .pointer_mut("/files")
            .and_then(Value::as_object_mut)
            .expect("checksums files");
        files.insert(
            changed_rel,
            json!(sha256_file(changed).expect("changed payload digest")),
        );
        let package_digest = canonical_json_digest(
            checksums
                .pointer("/files")
                .expect("checksums files after update"),
        );
        atomic_write_json_pretty(&checksums_path, &checksums).expect("write checksums");
        atomic_write_json_pretty(
            &package_dir.join("package.lock"),
            &json!({
                "schema_version": "sealed_package_lock_v1",
                "package_digest": package_digest,
            }),
        )
        .expect("write package lock");

        let mut manifest = load_json_file(manifest_path).expect("manifest json");
        set_json_pointer_value(&mut manifest, "/package_digest", json!(package_digest))
            .expect("set package digest");
        atomic_write_json_pretty(manifest_path, &manifest).expect("write manifest");
    }

    #[test]
    fn build_experiment_package_keeps_host_grader_out_of_task_runtime_staging() {
        let root = create_dx_authoring_fixture("bucephalus_build_project_eval_host_grader");
        let spec = minimal_project_eval_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        let manifest = load_json_file(&build.manifest_path).expect("manifest json");
        let grader = manifest
            .pointer("/resolved_experiment/trial_runtime/grader")
            .expect("host grader");

        assert_eq!(
            grader.pointer("/strategy").and_then(Value::as_str),
            Some("host")
        );
        assert_eq!(
            grader.pointer("/host/capability").and_then(Value::as_str),
            Some(TEST_HOST_GRADER_CAPABILITY)
        );
        assert_eq!(
            grader.pointer("/command/1").and_then(Value::as_str),
            Some(
                "__BUCEPHALUS_HOST_GRADER_CAPABILITY__/host_eval_capability/run_host_evaluation.py"
            )
        );
        assert!(
            build
                .package_dir
                .join(HOST_GRADER_CAPABILITIES_DIR)
                .join(TEST_HOST_GRADER_CAPABILITY)
                .join("run_host_evaluation.py")
                .is_file(),
            "host grader capability file should be sealed into the package"
        );
        let packaged_tasks =
            load_jsonl_value_rows(&build.package_dir.join("tasks").join("tasks.jsonl"))
                .expect("packaged tasks");
        let packaged_task = parse_task_row(&packaged_tasks[0]).expect("packaged task");
        let packaged_container = packaged_task
            .runtime
            .container_image
            .as_ref()
            .expect("container image");
        assert!(
            packaged_container
                .image
                .starts_with("ghcr.io/acme/project.eval.x86_64."),
            "task image should be rewritten to a declared pullable ref: {}",
            packaged_container.image
        );
        assert_eq!(packaged_container.platform.as_deref(), Some("linux/amd64"));

        let staging_manifest = load_json_file(&build.package_dir.join(STAGING_MANIFEST_FILE))
            .expect("staging manifest");
        let variant_entries = staging_manifest
            .pointer("/variants/qwen_35b_a3b")
            .and_then(Value::as_array)
            .expect("variant staging entries");
        assert!(
            !variant_entries.iter().any(|entry| {
                entry
                    .pointer("/runtime_path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| path.contains("run_host_evaluation.py"))
                    || entry
                        .pointer("/packaged_path")
                        .and_then(Value::as_str)
                        .is_some_and(|path| {
                            path.contains("run_host_evaluation.py")
                        })
            }),
            "host grader capability must not be staged as task runtime assets"
        );
    }

    #[test]
    fn build_experiment_package_rejects_incomplete_host_grader_capability_token() {
        let root = create_dx_authoring_fixture("bucephalus_build_bad_host_grader_token");
        let mut spec = minimal_project_eval_dx_spec();
        spec["trial_runtime"]["grader"]["command"][1] =
            json!("__BUCEPHALUS_HOST_GRADER_CAPABILITY__/host_eval_capability");
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let err = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect_err("incomplete host grader capability token should fail");
        assert!(
            err.to_string().contains(
                "trial_runtime.grader.command[1] must reference a host grader capability"
            ),
            "unexpected error: {}",
            err
        );
    }

    #[cfg(unix)]
    #[test]
    fn host_grader_capability_rejects_symlink_escape_from_capability_root() {
        let root = TempDirGuard::new("bucephalus_host_grader_symlink_escape");
        let capability = "host_escape_capability";
        let capability_dir = root
            .path
            .join("manifests")
            .join("grader_capabilities")
            .join(capability);
        let capability_root = capability_dir.join("files");
        ensure_dir(&capability_root).expect("capability root");
        let outside = root.path.join("outside.py");
        fs::write(&outside, "#!/usr/bin/env python3\nprint('escape')\n").expect("outside file");
        symlink(&outside, capability_root.join("escape.py")).expect("capability symlink");
        atomic_write_json_pretty(
            &capability_dir.join("capability.json"),
            &json!({
                "id": capability,
                "runtime": {"kind": "host"},
                "root": capability_root.to_string_lossy().to_string(),
                "allowed_paths": ["escape.py"]
            }),
        )
        .expect("host grader capability manifest");

        let manifest =
            load_grader_capability_manifest(&root.path, capability).expect("capability manifest");
        let err = manifest
            .resolve_host_path("escape.py")
            .expect_err("symlink escapes must be rejected");
        assert!(
            err.to_string().contains("resolves outside capability root"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn package_blob_path_for_digest_uses_package_blobs_layout() {
        let package_dir = Path::new("/tmp/package");
        let digest = format!("sha256:{}", "a".repeat(64));
        let path = package_blob_path_for_digest(package_dir, &digest).expect("blob path");
        assert_eq!(
            path,
            package_dir
                .join(PACKAGE_BLOBS_DIR)
                .join("sha256")
                .join("a".repeat(64))
                .join("blob")
        );
        assert!(package_blob_path_for_digest(package_dir, "md5:abc").is_err());
        assert!(package_blob_path_for_digest(package_dir, "sha256:not_hex").is_err());
    }

    #[test]
    fn package_cas_write_stores_blob_under_package_dir() {
        let root = TempDirGuard::new("bucephalus_package_cas_write");
        let package_dir = root.path.join(".lab").join("builds").join("pkg");
        ensure_dir(&package_dir).expect("package dir");
        let source = root.path.join("asset.bin");
        fs::write(&source, b"package-local bytes").expect("source bytes");

        let (digest, blob) =
            put_file_in_package_cas(&package_dir, &source).expect("write package cas");

        assert!(blob.starts_with(package_dir.join(PACKAGE_BLOBS_DIR)));
        assert!(blob.is_file(), "blob missing: {}", blob.display());
        assert_eq!(sha256_file(&blob).expect("blob digest"), digest);
        assert!(
            !root.path.join(".lab").join("objects").exists(),
            "package CAS must not write to .lab/objects"
        );
    }

    #[test]
    fn agent_artifact_excludes_paths_outside_artifact_root() {
        let root = TempDirGuard::new("bucephalus_agent_artifact_exclude_root");
        let artifact_root = root.path.join("agent");
        let outside = root.path.join("outside.py");
        ensure_dir(&artifact_root).expect("artifact root");
        fs::write(&outside, "print('outside')").expect("outside file");

        assert!(!should_include_agent_artifact_path(
            &artifact_root,
            &outside
        ));
    }

    #[test]
    fn materialize_package_cas_pointer_after_package_copy() {
        let root = TempDirGuard::new("bucephalus_package_cas_copy_materialize");
        let package_dir = root.path.join(".lab").join("builds").join("pkg");
        let runtime_assets = package_dir.join(PACKAGED_RUNTIME_ASSETS_DIR);
        ensure_dir(&runtime_assets).expect("runtime assets");
        let source = root.path.join("asset.bin");
        fs::write(&source, b"copied package bytes").expect("source bytes");
        let (digest, _) =
            put_file_in_package_cas(&package_dir, &source).expect("write package cas");
        let pointer = runtime_assets.join("asset.bin");
        write_cas_pointer(&pointer, digest, 20).expect("pointer");

        let copied = root.path.join("copied_pkg");
        copy_dir_preserve_all(&package_dir, &copied, &[]).expect("copy package");
        let materialized = root.path.join("materialized.bin");
        materialize_package_cas_backed_path(
            &copied,
            &copied.join(PACKAGED_RUNTIME_ASSETS_DIR).join("asset.bin"),
            &materialized,
        )
        .expect("materialize copied package pointer");

        assert_eq!(
            fs::read(&materialized).expect("materialized bytes"),
            b"copied package bytes"
        );
    }

    #[test]
    fn materialize_package_cas_pointer_copies_without_mutating_blob() {
        let root = TempDirGuard::new("bucephalus_package_cas_materialize_copy");
        let package_dir = root.path.join(".lab").join("builds").join("pkg");
        let runtime_assets = package_dir.join(PACKAGED_RUNTIME_ASSETS_DIR);
        ensure_dir(&runtime_assets).expect("runtime assets");
        let source = root.path.join("asset.bin");
        fs::write(&source, b"immutable package bytes").expect("source bytes");
        let (digest, _) =
            put_file_in_package_cas(&package_dir, &source).expect("write package cas");
        let pointer = runtime_assets.join("asset.bin");
        write_cas_pointer(&pointer, digest.clone(), 23).expect("pointer");

        let materialized = root.path.join("materialized.bin");
        materialize_package_cas_backed_path(&package_dir, &pointer, &materialized)
            .expect("materialize package pointer");
        fs::write(&materialized, b"runtime mutation").expect("mutate materialized copy");

        let blob = package_blob_path_for_digest(&package_dir, &digest).expect("blob path");
        assert_eq!(
            fs::read(&blob).expect("package blob bytes"),
            b"immutable package bytes",
            "materialized runtime files must not be hardlinked to package CAS blobs"
        );
    }

    #[test]
    fn runtime_asset_file_symlink_is_dereferenced_inside_source_tree() {
        let root = TempDirGuard::new("bucephalus_runtime_asset_symlink_file");
        let package_dir = root.path.join(".lab").join("builds").join("pkg");
        ensure_dir(&package_dir).expect("package dir");
        let source_dir = root.path.join("runtime_asset");
        ensure_dir(&source_dir).expect("source dir");
        fs::write(source_dir.join("real.txt"), "sealed bytes").expect("real file");
        symlink(Path::new("real.txt"), source_dir.join("linked.txt")).expect("file symlink");
        let destination = package_dir.join(PACKAGED_RUNTIME_ASSETS_DIR).join("asset");

        copy_runtime_asset_into_package(&source_dir, &destination, &package_dir)
            .expect("copy runtime asset");

        let packaged_link = destination.join("linked.txt");
        let metadata = fs::symlink_metadata(&packaged_link).expect("packaged linked file");
        assert!(
            !metadata.file_type().is_symlink(),
            "runtime asset file symlinks should be packaged as regular files"
        );
        assert_eq!(
            fs::read_to_string(packaged_link).expect("packaged linked bytes"),
            "sealed bytes"
        );
    }

    #[test]
    fn runtime_asset_symlink_outside_source_tree_is_rejected() {
        let root = TempDirGuard::new("bucephalus_runtime_asset_symlink_escape");
        let package_dir = root.path.join(".lab").join("builds").join("pkg");
        ensure_dir(&package_dir).expect("package dir");
        let source_dir = root.path.join("runtime_asset");
        ensure_dir(&source_dir).expect("source dir");
        let external = root.path.join("external.txt");
        fs::write(&external, "host bytes").expect("external file");
        symlink(&external, source_dir.join("escape.txt")).expect("escaping symlink");
        let destination = package_dir.join(PACKAGED_RUNTIME_ASSETS_DIR).join("asset");

        let err = copy_runtime_asset_into_package(&source_dir, &destination, &package_dir)
            .expect_err("escaping runtime asset symlink should fail");

        assert!(
            err.to_string().contains("resolves outside source tree"),
            "{}",
            err
        );
    }

    #[test]
    fn runtime_asset_directory_symlink_is_rejected() {
        let root = TempDirGuard::new("bucephalus_runtime_asset_symlink_dir");
        let package_dir = root.path.join(".lab").join("builds").join("pkg");
        ensure_dir(&package_dir).expect("package dir");
        let source_dir = root.path.join("runtime_asset");
        let nested = source_dir.join("nested");
        ensure_dir(&nested).expect("nested dir");
        symlink(Path::new("nested"), source_dir.join("linked_dir")).expect("dir symlink");
        let destination = package_dir.join(PACKAGED_RUNTIME_ASSETS_DIR).join("asset");

        let err = copy_runtime_asset_into_package(&source_dir, &destination, &package_dir)
            .expect_err("directory symlink should fail");

        assert!(
            err.to_string()
                .contains("runtime asset directory symlink is not supported"),
            "{}",
            err
        );
    }

    #[test]
    fn build_experiment_package_writes_large_runtime_asset_to_package_blobs() {
        let root = create_dx_authoring_fixture("bucephalus_build_package_blobs");
        fs::write(
            root.path.join("defaults.eval-lmstudio-headless.json"),
            vec![b'x'; 8 * 1024 * 1024],
        )
        .expect("large runtime asset");
        let mut spec = minimal_new_dx_spec();
        set_all_variant_agent_override(&mut spec, "mount", agent_artifact_value("agent-minimal-linux-dir"));
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let out_dir = root.path.join(".lab").join("builds").join("pkg");

        let build =
            build_experiment_package(&spec_path, None, Some(&out_dir)).expect("build package");
        let pointer_path = build
            .package_dir
            .join(PACKAGED_RUNTIME_ASSETS_DIR)
            .join("defaults.eval-lmstudio-headless.json");
        let pointer = read_cas_pointer(&pointer_path)
            .expect("read pointer")
            .expect("runtime asset should be CAS pointer");
        let blob = package_blob_path_for_digest(&build.package_dir, &pointer.digest)
            .expect("package blob path");
        assert!(blob.is_file(), "package-local blob missing");
        assert!(blob.starts_with(build.package_dir.join(PACKAGE_BLOBS_DIR)));
        assert!(
            !root.path.join(".lab").join("objects").exists(),
            "large package runtime assets must not use .lab/objects"
        );

        let blob_rel = blob
            .strip_prefix(&build.package_dir)
            .map(as_portable_rel)
            .expect("blob relative path");
        let checksums = load_json_file(&build.checksums_path).expect("checksums");
        assert!(
            checksums
                .pointer("/files")
                .and_then(Value::as_object)
                .is_some_and(|files| files.contains_key(&blob_rel)),
            "checksums.json should include package blob {}",
            blob_rel
        );

        let copied = root.path.join("copied_pkg");
        copy_dir_preserve_all(&build.package_dir, &copied, &[]).expect("copy package");
        load_sealed_package_for_run(&copied).expect("copied package should pass integrity");
    }

    #[test]
    fn build_experiment_package_rejects_invalid_cas_threshold_env() {
        let err = parse_large_file_threshold_bytes("invalid")
            .expect_err("invalid CAS threshold must fail");

        assert!(
            err.to_string()
                .contains("BUCEPHALUS_CAS_FILE_THRESHOLD_BYTES must be an unsigned byte count"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn build_experiment_package_stages_manifest_declared_grader_paths() {
        let root = create_dx_authoring_fixture("bucephalus_build_package_manifest_grader_paths");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let out_dir = root.path.join("package");
        let build =
            build_experiment_package(&spec_path, None, Some(&out_dir)).expect("build package");
        let manifest = load_json_file(&build.manifest_path).expect("manifest json");

        assert_eq!(
            manifest
                .pointer("/resolved_experiment/trial_runtime/grader/command/1")
                .and_then(Value::as_str)
                .unwrap_or(""),
            "__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/evaluation/integration/bucephalus/evaluation_harness.py"
        );
        let staging_manifest = load_json_file(&build.package_dir.join(STAGING_MANIFEST_FILE))
            .expect("staging manifest");
        assert!(
            staging_manifest
                .pointer("/variants/qwen_35b_a3b")
                .and_then(Value::as_array)
                .is_some_and(|entries| entries.iter().any(|entry| {
                    entry.pointer("/runtime_path").and_then(Value::as_str)
                        == Some("__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/evaluation")
                })),
            "grader support directory should be staged for the baseline variant"
        );
    }

    #[test]
    fn rewrite_grader_paths_for_package_rejects_host_grader_task_assets() {
        let root = TempDirGuard::new("bucephalus_host_grader_boundary");
        let exp_dir = root.path.join("exp");
        let package_dir = root.path.join("package");
        ensure_dir(&exp_dir.join("scripts")).expect("scripts dir");
        ensure_dir(&package_dir).expect("package dir");
        write_test_host_grader_capability_manifest(&exp_dir);
        fs::write(
            exp_dir.join("scripts").join("grader.py"),
            "#!/usr/bin/env python3\n",
        )
        .expect("grader script");

        let mut evaluation_root = json!({
            "grader": {
                "strategy": "host",
                "host": { "capability": "host_eval_capability" },
                "command": ["python3", "./scripts/grader.py"]
            }
        });
        let mut file_copies = BTreeMap::new();
        let mut file_counter = 0usize;
        let mut public_path_copies = BTreeMap::new();
        let mut staging_manifest_entries = Vec::new();

        let err = rewrite_grader_paths_for_package(
            evaluation_root.pointer_mut("/grader").expect("grader"),
            &exp_dir,
            &package_dir,
            &mut file_copies,
            &mut file_counter,
            &mut public_path_copies,
            &mut staging_manifest_entries,
        )
        .expect_err("host grader task-local files must be rejected");
        assert!(
            err.to_string().contains("host grader files cannot be staged"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn rewrite_grader_paths_for_package_rejects_host_grader_runtime_assets() {
        let root = TempDirGuard::new("bucephalus_host_grader_runtime_assets");
        let exp_dir = root.path.join("exp");
        let package_dir = root.path.join("package");
        ensure_dir(&exp_dir).expect("exp dir");
        ensure_dir(&package_dir).expect("package dir");
        write_test_host_grader_capability_manifest(&exp_dir);

        let mut evaluation_root = json!({
            "grader": {
                "strategy": "host",
                "host": { "capability": "host_eval_capability" },
                "command": [
                    "python3",
                    "__BUCEPHALUS_HOST_GRADER_CAPABILITY__/host_eval_capability/run_host_evaluation.py"
                ],
                "_runtime_assets": [{
                    "build_source_path": "./grader",
                    "runtime_path": "__BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support/grader"
                }]
            }
        });
        let mut file_copies = BTreeMap::new();
        let mut file_counter = 0usize;
        let mut public_path_copies = BTreeMap::new();
        let mut staging_manifest_entries = Vec::new();

        let err = rewrite_grader_paths_for_package(
            evaluation_root.pointer_mut("/grader").expect("grader"),
            &exp_dir,
            &package_dir,
            &mut file_copies,
            &mut file_counter,
            &mut public_path_copies,
            &mut staging_manifest_entries,
        )
        .expect_err("host grader runtime assets must be rejected");
        assert!(
            err.to_string()
                .contains("trial_runtime.grader._runtime_assets is not valid for strategy='host'"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn rewrite_grader_paths_for_package_requires_explicit_strategy() {
        let root = TempDirGuard::new("bucephalus_grader_strategy_required_for_rewrite");
        let exp_dir = root.path.join("exp");
        let package_dir = root.path.join("package");
        ensure_dir(&exp_dir).expect("exp dir");
        ensure_dir(&package_dir).expect("package dir");

        let mut grader = json!({
            "command": ["python3", "-m", "grader"]
        });
        let mut file_copies = BTreeMap::new();
        let mut file_counter = 0usize;
        let mut public_path_copies = BTreeMap::new();
        let mut staging_manifest_entries = Vec::new();

        let err = rewrite_grader_paths_for_package(
            &mut grader,
            &exp_dir,
            &package_dir,
            &mut file_copies,
            &mut file_counter,
            &mut public_path_copies,
            &mut staging_manifest_entries,
        )
        .expect_err("staging must not invent a grader strategy");
        assert!(
            err.to_string()
                .contains("trial_runtime.grader.strategy is required"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn collect_runtime_command_env_staging_entries_requires_explicit_grader_strategy() {
        let experiment = json!({
            "trial_runtime": {
                "agent": {
                    "command": ["python3", "-m", "agent"]
                },
                "grader": {
                    "command": ["python3", "-m", "grader"]
                }
            }
        });
        let catalog = BTreeMap::new();

        let err = collect_runtime_command_env_staging_entries(&experiment, &catalog)
            .expect_err("staging collection must not invent a grader strategy");
        assert!(
            err.to_string()
                .contains("trial_runtime.grader.strategy is required"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn resolve_grader_runtime_assets_keeps_host_capability_unstaged() {
        let root = TempDirGuard::new("bucephalus_host_grader_capability_unstaged");
        ensure_dir(&root.path).expect("root dir");
        let experiment = json!({
            "trial_runtime": {
                "grader": {
                    "strategy": "host",
                    "host": { "capability": "host_eval_capability" },
                    "command": [
                        "python3",
                        "__BUCEPHALUS_HOST_GRADER_CAPABILITY__/host_eval_capability/run_host_evaluation.py",
                        "--grader-input"
                    ],
                    "conclusion": { "mode": "direct" }
                }
            }
        });

        let assets = resolve_grader_runtime_assets(&experiment, &root.path, &root.path)
            .expect("host capability validates");
        assert!(
            assets.is_empty(),
            "host grader capability references are runner-owned and must not stage files"
        );
    }

    #[test]
    fn resolve_grader_runtime_assets_requires_explicit_strategy() {
        let root = TempDirGuard::new("bucephalus_grader_runtime_assets_strategy_required");
        ensure_dir(&root.path).expect("root dir");
        let experiment = json!({
            "trial_runtime": {
                "grader": {
                    "command": ["python3", "-m", "grader"]
                }
            }
        });

        let err = resolve_grader_runtime_assets(&experiment, &root.path, &root.path)
            .expect_err("runtime asset resolution must not invent a grader strategy");
        assert!(
            err.to_string()
                .contains("trial_runtime.grader.strategy is required"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn build_experiment_package_uses_builtin_dataset_path_override() {
        let root = create_dx_authoring_fixture("bucephalus_build_dataset_path_override");
        let custom_dir = root.path.join("custom");
        ensure_dir(&custom_dir).expect("custom dataset dir");
        fs::write(
            custom_dir.join("tasks_override.jsonl"),
            concat!(
                r#"{"schema_version":"task_row_v2","id":"TASK_OVERRIDE","task":{"id":"TASK_OVERRIDE"},"runtime":{"container_image":{"image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("override dataset");

        let mut spec = minimal_dx_spec();
        set_json_pointer_value(
            &mut spec,
            "/matrix/tasks/path",
            json!("custom/tasks_override.jsonl"),
        )
        .expect("set dataset path override");
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        let packaged_tasks =
            load_jsonl_value_rows(&build.package_dir.join("tasks").join("tasks.jsonl"))
                .expect("packaged tasks");
        assert_eq!(packaged_tasks.len(), 1);
        let packaged_task_row = parse_task_row(&packaged_tasks[0]).expect("packaged task row");
        assert_eq!(packaged_task_row.id.as_str(), "TASK_OVERRIDE");
        let manifest = load_json_file(&build.manifest_path).expect("manifest json");
        assert!(
            manifest
                .pointer("/resolved_experiment/trial_runtime/grader/_runtime_assets")
                .is_none(),
            "packaging-only runtime asset catalogs must not leak into the sealed runtime contract"
        );
    }

    fn package_check<'a>(report: &'a Value, id: &str) -> &'a Value {
        report
            .pointer("/checks")
            .and_then(Value::as_array)
            .and_then(|checks| {
                checks
                    .iter()
                    .find(|check| check.pointer("/id").and_then(Value::as_str) == Some(id))
            })
            .unwrap_or_else(|| panic!("missing package check {id}"))
    }

    #[test]
    fn build_experiment_package_warns_on_mutable_task_image_refs() {
        let root = create_dx_authoring_fixture("bucephalus_build_mutable_task_images");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        let report = load_json_file(&build.package_checks_path).expect("package checks");
        let check = package_check(&report, "images.task_refs_digest_pinned");

        assert_eq!(check.pointer("/status").and_then(Value::as_str), Some("warn"));
        assert!(
            check
                .pointer("/evidence/mutable_images")
                .and_then(Value::as_array)
                .is_some_and(|images| images.iter().any(|image| {
                    image.as_str() == Some("python:3.11-slim")
                })),
            "mutable task image should be reported in package checks: {}",
            check
        );
    }

    #[test]
    fn build_experiment_package_passes_digest_pinned_task_image_refs() {
        let root = create_dx_authoring_fixture("bucephalus_build_pinned_task_images");
        fs::write(
            root.path
                .join(".lab")
                .join("experiments")
                .join("data")
                .join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"task_row_v2","id":"TASK001","task":{"id":"TASK001"},"runtime":{"container_image":{"image":"python:3.11-slim@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("digest-pinned task row");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        let report = load_json_file(&build.package_checks_path).expect("package checks");
        let check = package_check(&report, "images.task_refs_digest_pinned");

        assert_eq!(check.pointer("/status").and_then(Value::as_str), Some("pass"));
        assert_eq!(
            check
                .pointer("/evidence/mutable_images")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn build_experiment_package_accepts_case_rows() {
        let root = create_dx_authoring_fixture("bucephalus_build_task_cases");
        let data_dir = root
            .path
            .join(".lab")
            .join("experiments")
            .join("data");
        let image_dir = data_dir.join("images");
        ensure_dir(&image_dir).expect("image dir");
        fs::write(image_dir.join("case001.png"), b"case image").expect("case image");
        fs::write(
            data_dir.join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"case_v1","id":"CASE001","inputs":{"prompt":"Describe this image.","image":{"type":"file","path":"images/case001.png","media_type":"image/png"}},"resources":{"workspace":{"type":"container_image","image":"python:3.11-slim","workdir":"/workspace/task"}},"metadata":{"suite":"vision"},"limits":{"timeout_ms":123000}}"#,
                "\n"
            ),
        )
        .expect("task case dataset");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package with task case");
        let packaged_tasks =
            load_jsonl_value_rows(&build.package_dir.join("tasks").join("tasks.jsonl"))
                .expect("packaged cases");
        assert_eq!(packaged_tasks.len(), 1);
        assert_eq!(
            packaged_tasks[0]
                .pointer("/schema_version")
                .and_then(Value::as_str),
            Some("case_v1")
        );
        let boundary = parse_task_boundary_from_packaged_task(&packaged_tasks[0])
            .expect("packaged case boundary");
        assert_eq!(boundary.task_id, "CASE001");
        assert_eq!(
            boundary.task_payload.pointer("/inputs/prompt").and_then(Value::as_str),
            Some("Describe this image.")
        );
        assert_eq!(boundary.task_image, "python:3.11-slim");
        assert_eq!(boundary.time_limit_ms, Some(123000));
    }

    #[test]
    fn build_experiment_package_strips_public_authoring_aliases_from_resolved_package() {
        let root = create_dx_authoring_fixture("bucephalus_build_public_aliases");
        let data_dir = root
            .path
            .join(".lab")
            .join("experiments")
            .join("data");
        ensure_dir(&data_dir).expect("data dir");
        fs::write(
            data_dir.join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"case_v2","id":"CASE001","inputs":{"prompt":"Hello"},"resources":{"workspace":{"source":"container_image","image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("case dataset");

        let spec = json!({
            "experiment": {
                "id": "public_aliases",
                "name": "Public Aliases"
            },
            "runtime": {
                "compute": { "backend": "local-docker" },
                "storage": { "backend": "local-fs" },
                "traces": { "backend": "local-stdout" },
                "network": { "task_sandbox": "none", "agent": "none" }
            },
            "matrix": {
                "cases": {
                    "source": "file",
                    "path": ".lab/experiments/data/eval_suite.task_rows.jsonl"
                },
                "variants": [{
                    "id": "baseline",
                    "baseline": true,
                    "config": { "mode": "balanced" }
                }],
                "repeats": 1
            },
            "stages": {
                "case": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": {"from": "case_row"},
                        "workdir": {"from": "case_row"}
                    }
                },
                "agent": {
                    "image": "python:3.11-slim",
                    "artifact_type": "structured_json",
                    "command": ["python", "-c", "print('ok')"],
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json"
                            }
                        }
                    }
                },
                "execution": { "agent_site": "agent_container" },
                "grader": { "strategy": "none" }
            },
            "metrics": [{
                "id": "resolved",
                "source": {
                    "type": "agent_response",
                    "pointer": "/metrics/resolved"
                },
                "primary": true
            }],
            "policy": {
                "timeout_ms": 600000,
                "task_sandbox": {}
            }
        });

        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package with public aliases");
        let resolved = load_json_file(&build.package_dir.join("resolved_experiment.json"))
            .expect("resolved experiment");

        assert!(resolved.pointer("/matrix/tasks").is_some());
        assert!(resolved.pointer("/trial_runtime").is_some());
        assert!(resolved.pointer("/matrix/cases").is_none());
        assert!(resolved.pointer("/stages").is_none());
        assert!(resolved.pointer("/cases").is_none());
    }

    #[test]
    fn build_experiment_package_seals_task_case_file_inputs_from_dataset_dir() {
        let root = create_dx_authoring_fixture("bucephalus_build_task_case_assets");
        let data_dir = root
            .path
            .join(".lab")
            .join("experiments")
            .join("data");
        let data_images = data_dir.join("images");
        ensure_dir(&data_images).expect("data image dir");
        fs::write(data_images.join("case001.png"), b"dataset-local image")
            .expect("dataset image");
        let root_images = root.path.join("images");
        ensure_dir(&root_images).expect("root image dir");
        fs::write(root_images.join("case001.png"), b"wrong image").expect("root image");
        fs::write(
            data_dir.join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"case_v1","id":"CASE001","inputs":{"prompt":"Describe this image.","image":{"type":"file","path":"images/case001.png","media_type":"image/png"}},"resources":{"workspace":{"type":"container_image","image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("task case dataset");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package with case asset");
        let packaged_tasks =
            load_jsonl_value_rows(&build.package_dir.join("tasks").join("tasks.jsonl"))
                .expect("packaged cases");
        let image = packaged_tasks[0]
            .pointer("/inputs/image")
            .and_then(Value::as_object)
            .expect("packaged image input");
        let package_path = image
            .get("package_path")
            .and_then(Value::as_str)
            .expect("packaged case asset path");

        assert!(packaged_tasks[0].pointer("/inputs/image/path").is_none());
        let expected_uri = format!("package://{}", package_path);
        assert_eq!(
            image.get("uri").and_then(Value::as_str),
            Some(expected_uri.as_str())
        );
        assert_eq!(
            fs::read(build.package_dir.join(package_path)).expect("packaged image"),
            b"dataset-local image"
        );
        let checksums = load_json_file(&build.checksums_path).expect("checksums");
        assert!(
            checksums
                .pointer("/files")
                .and_then(Value::as_object)
                .is_some_and(|files| files.contains_key(package_path)),
            "sealed checksums should cover packaged case asset"
        );
    }

    #[test]
    fn build_experiment_package_deduplicates_reused_task_case_assets() {
        let root = create_dx_authoring_fixture("bucephalus_build_task_case_asset_dedup");
        let data_dir = root
            .path
            .join(".lab")
            .join("experiments")
            .join("data");
        let image_dir = data_dir.join("images");
        ensure_dir(&image_dir).expect("image dir");
        fs::write(image_dir.join("shared.png"), b"shared image").expect("shared image");
        fs::write(
            data_dir.join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"case_v1","id":"CASE001","inputs":{"image":{"type":"file","path":"images/shared.png","media_type":"image/png"}},"resources":{"workspace":{"type":"container_image","image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n",
                r#"{"schema_version":"case_v1","id":"CASE002","inputs":{"request":{"image":{"type":"file","path":"images/shared.png","media_type":"image/png"}}},"resources":{"workspace":{"type":"container_image","image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("task case dataset");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package with duplicate case assets");
        let packaged_tasks =
            load_jsonl_value_rows(&build.package_dir.join("tasks").join("tasks.jsonl"))
                .expect("packaged cases");
        let first_path = packaged_tasks[0]
            .pointer("/inputs/image/package_path")
            .and_then(Value::as_str)
            .expect("first package path");
        let second_path = packaged_tasks[1]
            .pointer("/inputs/request/image/package_path")
            .and_then(Value::as_str)
            .expect("second package path");
        assert_eq!(first_path, second_path);

        let checksums = load_json_file(&build.checksums_path).expect("checksums");
        let task_asset_count = checksums
            .pointer("/files")
            .and_then(Value::as_object)
            .expect("checksum files")
            .keys()
            .filter(|path| path.starts_with("tasks/assets/"))
            .count();
        assert_eq!(
            task_asset_count, 1,
            "reusing the same case image should not create one packaged copy per case"
        );
    }

    #[test]
    fn build_experiment_package_fails_when_task_case_asset_is_missing() {
        let root = create_dx_authoring_fixture("bucephalus_build_task_case_missing_asset");
        let data_dir = root
            .path
            .join(".lab")
            .join("experiments")
            .join("data");
        fs::write(
            data_dir.join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"case_v1","id":"CASE001","inputs":{"image":{"type":"file","path":"images/missing.png","media_type":"image/png"}},"resources":{"workspace":{"type":"container_image","image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("task case dataset");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let err = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect_err("missing case asset should fail the package build");
        let msg = err.to_string();
        assert!(msg.contains("CASE001"), "unexpected error: {msg}");
        assert!(msg.contains("images/missing.png"), "unexpected error: {msg}");
    }

    #[test]
    fn build_experiment_package_rejects_task_case_asset_kind_mismatch() {
        let root = create_dx_authoring_fixture("bucephalus_build_task_case_asset_kind");
        let data_dir = root
            .path
            .join(".lab")
            .join("experiments")
            .join("data");
        fs::write(data_dir.join("not_a_directory.txt"), "plain file").expect("case file");
        fs::write(
            data_dir.join("eval_suite.task_rows.jsonl"),
            concat!(
                r#"{"schema_version":"case_v1","id":"CASE001","inputs":{"attachments":{"type":"directory","path":"not_a_directory.txt"}},"resources":{"workspace":{"type":"container_image","image":"python:3.11-slim","workdir":"/workspace/task"}}}"#,
                "\n"
            ),
        )
        .expect("task case dataset");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let err = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect_err("case asset kind mismatch should fail at package build time");
        let msg = err.to_string();
        assert!(msg.contains("CASE001"), "unexpected error: {msg}");
        assert!(
            msg.contains("declares type=directory"),
            "unexpected error: {msg}"
        );
        assert!(msg.contains("not_a_directory.txt"), "unexpected error: {msg}");
    }

    #[test]
    fn build_experiment_package_fails_fast_on_invalid_task_row() {
        let root = create_dx_authoring_fixture("bucephalus_build_package_invalid_task_row");
        fs::write(
            root.path
                .join(".lab")
                .join("experiments")
                .join("data")
                .join("eval_suite.task_rows.jsonl"),
            "{\"id\":\"TASK001\",\"image\":\"python:3.11-slim\",\"workdir\":\"/workspace/task\",\"task\":{\"id\":\"TASK001\"},\"materialization\":{\"kind\":\"task_image\"}}\n",
        )
        .expect("invalid task row dataset");
        let spec = minimal_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let err = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect_err("invalid task row should fail build");
        assert!(
            err.to_string().contains("case_v2"),
            "unexpected build error: {}",
            err
        );
    }

    #[test]
    fn compile_tasks_for_package_seals_workspace_inputs_into_task_bundles() {
        let root = TempDirGuard::new("bucephalus_compile_tasks_for_package");
        let dataset_bundle_src = root.path.join("dataset_bundle_src");
        ensure_dir(&dataset_bundle_src).expect("dataset bundle src");
        fs::write(dataset_bundle_src.join("README.md"), "dataset pack\n").expect("pack file");

        let git_bundle_src = root.path.join("git_bundle_src");
        ensure_dir(&git_bundle_src).expect("git bundle src");
        fs::write(git_bundle_src.join("README.md"), "git checkout bundle\n").expect("git bundle");

        let package_dir = root.path.join("package");
        ensure_dir(&package_dir).expect("package dir");
        let dataset_path = root.path.join("tasks.jsonl");
        fs::write(&dataset_path, "").expect("dataset file");

        let task_values = vec![
            base_image_bundle_task_row(
                "task_dataset",
                "python:3.11-slim",
                "/workspace/task",
                dataset_bundle_src.to_string_lossy().as_ref(),
            ),
            base_image_bundle_task_row(
                "task_git",
                "python:3.11-slim",
                "/workspace/task",
                git_bundle_src.to_string_lossy().as_ref(),
            ),
        ];
        let packaged_tasks = compile_tasks_for_package(
            &task_values,
            &dataset_path,
            &package_dir,
            &json!({}),
        )
        .expect("compile packaged tasks");
        assert_eq!(packaged_tasks.len(), 2);
        let dataset_row = parse_task_row(&packaged_tasks[0]).expect("dataset row");
        let git_row = parse_task_row(&packaged_tasks[1]).expect("git row");

        assert!(dataset_row.runtime.container_image.is_some());
        assert!(git_row.runtime.container_image.is_some());
    }

    #[test]
    fn build_experiment_package_rejects_external_exec_shim_artifact() {
        let root = create_dx_authoring_fixture("bucephalus_build_reject_external_exec");
        let artifact_bin = root
            .path
            .join(".lab")
            .join("agents")
            .join("agent-external-exec")
            .join("bin");
        ensure_dir(&artifact_bin).expect("artifact dir");
        write_executable_script(
            &artifact_bin.join("agentctl"),
            "#!/usr/bin/env sh\nexec /usr/local/bin/bun /workspace/packages/apps/launcher/index.ts \"$@\"\n",
        );

        let mut spec = minimal_new_dx_spec();
        set_all_variant_agent_override(
            &mut spec,
            "mount",
            agent_artifact_value("agent-external-exec"),
        );
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let err = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect_err("external shim artifact should fail");
        assert!(
            err.to_string().contains("image-resident path"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn build_experiment_package_rejects_opt_agent_script_delegate() {
        let root = create_dx_authoring_fixture("bucephalus_build_reject_opt_agent_script");
        let artifact_bin = root
            .path
            .join(".lab")
            .join("agents")
            .join("agent-opt-script")
            .join("bin");
        ensure_dir(&artifact_bin).expect("artifact dir");
        write_executable_script(
            &artifact_bin.join("agentctl"),
            "#!/usr/bin/env sh\nexec /opt/agent/bin/bun /opt/agent/packages/apps/launcher/dist/index.js \"$@\"\n",
        );

        let mut spec = minimal_new_dx_spec();
        set_all_variant_agent_override(
            &mut spec,
            "mount",
            agent_artifact_value("agent-opt-script"),
        );
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let err = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect_err("artifact script delegate should fail");
        assert!(
            err.to_string().contains("readable script path"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn build_experiment_package_accepts_explicit_artifact_command_without_entrypoint_shim() {
        let root = create_dx_authoring_fixture("bucephalus_build_explicit_artifact_command");
        let artifact_root = root
            .path
            .join(".lab")
            .join("agents")
            .join("agent-explicit-command");
        let artifact_bin = artifact_root.join("bin");
        let script_dir = artifact_root
            .join("packages")
            .join("apps")
            .join("launcher")
            .join("dist");
        ensure_dir(&artifact_bin).expect("artifact bin dir");
        ensure_dir(&script_dir).expect("script dir");
        write_executable_script(&artifact_bin.join("bun"), "#!/bin/sh\nexit 0\n");
        write_executable_script(
            &artifact_bin.join("agentctl"),
            "#!/usr/bin/env sh\nexec /usr/local/bin/bun /workspace/packages/apps/launcher/index.ts \"$@\"\n",
        );
        fs::write(script_dir.join("index.js"), "console.log('ok');\n").expect("launcher");

        let mut spec = minimal_new_dx_spec();
        set_all_variant_agent_override(
            &mut spec,
            "mount",
            agent_artifact_value("agent-explicit-command"),
        );
        set_all_variant_agent_override(
            &mut spec,
            "command",
            json!([
                "/opt/agent/bin/bun",
                "/opt/agent/packages/apps/launcher/dist/index.js"
            ]),
        );
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("explicit artifact command should pass");
    }

    #[cfg(unix)]
    #[test]
    fn validate_agent_artifact_root_rejects_command_symlink_escape() {
        let root = TempDirGuard::new("bucephalus_artifact_command_symlink_escape");
        let artifact_root = root.path.join("agent");
        let artifact_bin = artifact_root.join("bin");
        ensure_dir(&artifact_bin).expect("artifact bin dir");
        let outside = root.path.with_file_name("outside_agentctl");
        write_executable_script(&outside, "#!/bin/sh\nexit 0\n");
        symlink(&outside, artifact_bin.join("agentctl")).expect("artifact command symlink");

        let err = validate_agent_artifact_root(
            &artifact_root,
            "/opt/agent",
            &["/opt/agent/bin/agentctl".to_string()],
            "agent artifact",
        )
        .expect_err("artifact command symlink escapes must fail");
        assert!(err.to_string().contains("escapes artifact root"));
    }

    #[test]
    fn build_experiment_package_rejects_artifact_not_referenced_by_command() {
        let root = create_dx_authoring_fixture("bucephalus_build_reject_no_executable");
        let artifact_root = root
            .path
            .join(".lab")
            .join("agents")
            .join("agent-empty-artifact");
        ensure_dir(&artifact_root).expect("artifact dir");
        fs::write(artifact_root.join("README.md"), "no executables here").expect("readme");

        let mut spec = minimal_new_dx_spec();
        set_all_variant_agent_override(
            &mut spec,
            "mount",
            agent_artifact_value("agent-empty-artifact"),
        );
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");

        let err = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect_err("non-executable artifact should fail");
        assert!(
            err.to_string()
                .contains("did not resolve to artifact executable"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn p0_i06_preflight_grader_reachability_rejects_external_absolute_script_path() {
        let _runtime_guard = lock_runtime_control_tests();
        if !docker_runtime_available() {
            eprintln!("skipping p0_i06 test: docker daemon unavailable");
            return;
        }
        ensure_docker_test_image("python:3.11-slim");

        let evaluation_config = EvaluationConfig {
            policy: TaskPolicyConfig::default(),
            grader: Some(GraderConfig::in_task_runtime(vec![
                "python3".to_string(),
                "/opt/external/evaluation_harness.py".to_string(),
            ])),
        };
        let mut runtime_profile =
            preflight_test_runtime_profile(ImageSource::Global, Some("python:3.11-slim"));
        set_json_pointer_value(
            &mut runtime_profile.experiment,
            "/runtime/network/task_sandbox",
            json!("none"),
        )
        .expect("runtime network");
        let tasks = vec![task_row_value(
            "TASK001",
            "python:3.11-slim",
            "/workspace/task",
            Some(30_000),
        )];

        let variant = preflight_test_variant();
        let root = TempDirGuard::new("bucephalus_p0_grader_reachability_forbidden");
        let check = check_grader_reachable(
            &evaluation_config,
            &runtime_profile,
            &variant,
            &tasks,
            &root.path,
        );
        assert!(
            !check.passed,
            "preflight must fail when grader script path is outside the runtime boundary"
        );
        assert!(
            check
                .message
                .contains("forbidden grader script path"),
            "unexpected message: {}",
            check.message
        );
    }

    #[test]
    fn p0_i06_preflight_grader_reachability_allows_runner_staged_deps_script_path() {
        assert!(
            is_runner_staged_script_path(&task_workdir_support_destination_path(
                "evaluation_harness.py"
            )),
            "runner-staged script path should be accepted without requiring the script to exist in the task image"
        );
    }

    #[test]
    fn p0_container_mount_args_use_contract_io_mounts_without_host_workspace_bind() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_p0_no_dataset_mount");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({
            "runtime": {
                "policy": {
                    "sandbox": {
                        "hardening": {
                            "no_new_privileges": true,
                            "drop_all_caps": true
                        }
                    }
                }
            }
        });
        let dynamic_mounts = vec![ResolvedMountReference {
            host_path: root.path.join("fixture-pack"),
            mount_path: format!("{}/dataset_pack", BUCEPHALUS_CONTRACT_WORKSPACE_DIR),
            read_only: true,
        }];
        fs::write(&dynamic_mounts[0].host_path, "fixture").expect("fixture pack");
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &dynamic_mounts,
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let spec = build_container_spec(
            &LocalBindMountRuntimeSync,
            &request,
            request.task_image,
            "/workspace/task",
            request.network_mode,
            false,
            &[],
        )
        .expect("container spec");
        let mounts = &spec.mounts;
        assert!(
            mounts
                .iter()
                .any(|mount| mount.container_path == BUCEPHALUS_CONTRACT_IN_DIR && mount.read_only),
            "missing in-dir mount: {:?}",
            mounts
        );
        assert!(
            !mounts
                .iter()
                .any(|mount| mount.container_path == "/workspace/task"),
            "task container should not bind the host workspace into the task workdir: {:?}",
            mounts
        );
        assert!(
            mounts
                .iter()
                .any(|mount| mount.container_path == BUCEPHALUS_CONTRACT_OUT_DIR && !mount.read_only),
            "missing out-dir mount: {:?}",
            mounts
        );
        assert!(
            !mounts
                .iter()
                .any(|mount| mount.container_path == "/dataset"),
            "legacy /dataset mount should not be present: {:?}",
            mounts
        );
    }

    #[test]
    fn container_spec_requires_runtime_sync_mounts() {
        struct RejectingRuntimeSync;

        impl LocalContainerRuntimeSync for RejectingRuntimeSync {
            fn container_mounts(
                &self,
                _request: &TrialRunRequest<'_>,
                _include_agent_artifact: bool,
                _extra_mounts: &[ResolvedMountReference],
            ) -> Result<Vec<crate::backend::docker::ContainerMount>> {
                Err(anyhow::anyhow!("runtime sync refused mount preparation"))
            }
        }
        let _backend = LocalDockerExecutionBackend::with_runtime_sync(RejectingRuntimeSync);

        let (_root, paths) = create_trial_paths_fixture("bucephalus_runtime_sync_required");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let err = build_container_spec(
            &RejectingRuntimeSync,
            &request,
            request.task_image,
            request.task_workdir,
            request.network_mode,
            false,
            &[],
        )
        .expect_err("container spec must not rebuild mounts after runtime sync fails");
        assert!(
            err.to_string().contains("runtime sync refused"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn modal_executor_rejects_missing_grader_config() {
        let _lock = lock_modal_env_tests();
        let _guard = EnvVarGuard::set(&[
            ("BUCEPHALUS_MODAL_S3_BUCKET", Some("bucephalus-bucket")),
            ("BUCEPHALUS_MODAL_S3_PREFIX", Some("runs")),
        ]);
        let (_root, paths) = create_trial_paths_fixture("bucephalus_modal_rejects_grading");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let task_sandbox_plan = TaskSandboxPlan {
            image: "python:3.11-slim".to_string(),
            workdir: "/workspace/task".to_string(),
            platform: None,
            materialization: TaskMaterializationSpec {
                kind: TaskMaterializationKind::TaskImage,
                task_bundle_ref: None,
                platform: None,
            },
            case_materialization: Vec::new(),
            io_mounts: IoMountPlan {
                in_dir: BUCEPHALUS_CONTRACT_IN_DIR.to_string(),
                out_dir: BUCEPHALUS_CONTRACT_OUT_DIR.to_string(),
                telemetry_mounts: Vec::new(),
            },
            artifact_mount: None,
            network_mode: "none".to_string(),
            time_limit_ms: 30_000,
        };

        let executor = ModalExecutionBackend::for_test("bucephalus-test", None);
        let err = match executor.execute_attempt(TrialRuntimeExecutionRequest {
                trial_dir: &paths.trial_dir,
                schedule_idx: 0,
                attempt_no: 1,
                run_request: &request,
                task_id: "task_1",
                variant_id: "baseline",
                repl_idx: 0,
                task_sandbox_plan: &task_sandbox_plan,
            }) {
            Ok(_) => panic!("modal executor should reject unsupported grading before launch"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("grading enabled without grader config"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn modal_executor_rejects_case_materialization_before_sync_config() {
        let _lock = lock_modal_env_tests();
        let (_root, paths) = create_trial_paths_fixture("bucephalus_modal_rejects_case_setup");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let task_sandbox_plan = TaskSandboxPlan {
            image: "python:3.11-slim".to_string(),
            workdir: "/workspace/task".to_string(),
            platform: None,
            materialization: TaskMaterializationSpec {
                kind: TaskMaterializationKind::TaskImage,
                task_bundle_ref: None,
                platform: None,
            },
            case_materialization: vec![CaseMaterializationStepPlan {
                id: "setup".to_string(),
                stage: CaseMaterializationStage::Case,
                operation: CaseMaterializationOperation::Command,
                command: vec!["bash".to_string(), "-lc".to_string(), "true".to_string()],
                resource: None,
                source: json!({}),
                mount: None,
                workdir: Some("/workspace/task".to_string()),
                network: Some("none".to_string()),
                timeout_ms: Some(1000),
                hidden: false,
            }],
            io_mounts: IoMountPlan {
                in_dir: BUCEPHALUS_CONTRACT_IN_DIR.to_string(),
                out_dir: BUCEPHALUS_CONTRACT_OUT_DIR.to_string(),
                telemetry_mounts: Vec::new(),
            },
            artifact_mount: None,
            network_mode: "none".to_string(),
            time_limit_ms: 30_000,
        };

        let executor = ModalExecutionBackend::for_test("bucephalus-test", None);
        let err = match executor.execute_attempt(TrialRuntimeExecutionRequest {
            trial_dir: &paths.trial_dir,
            schedule_idx: 0,
            attempt_no: 1,
            run_request: &request,
            task_id: "task_1",
            variant_id: "baseline",
            repl_idx: 0,
            task_sandbox_plan: &task_sandbox_plan,
        }) {
            Ok(_) => panic!("modal executor should reject case materialization before launch"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("executor modal does not yet support case materialization steps"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn p0_container_mounts_secret_file_readonly_and_credential_cache_writable() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_p0_credential_cache_mount");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({
            "runtime": {
                "policy": {
                    "sandbox": {
                        "hardening": {
                            "no_new_privileges": true,
                            "drop_all_caps": true
                        }
                    }
                }
            }
        });
        let secret_source = root.path.join("auth.json");
        fs::write(&secret_source, "{}\n").expect("secret source");
        let cache_dir = root.path.join("runtime").join("credential_caches").join("codex");
        ensure_dir(&cache_dir).expect("cache dir");
        let cache_file = cache_dir.join("auth.json");
        fs::write(&cache_file, "{}\n").expect("cache file");
        let secret_file_mounts = vec![ResolvedSecretFileMount {
            id: "codex_oauth".to_string(),
            source_from_host: secret_source.clone(),
            target_path: "/root/.codex/auth.json".to_string(),
            credential_cache: Some(ResolvedCredentialCacheMount {
                id: "codex_oauth".to_string(),
                host_dir: cache_dir.clone(),
                host_file: cache_file,
                target_dir: "/bucephalus/credentials/codex_oauth".to_string(),
                target_path: "/bucephalus/credentials/codex_oauth/auth.json".to_string(),
                env: Some("CODEX_AUTH_CACHE_FILE".to_string()),
            }),
        }];
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &secret_file_mounts,
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let spec = build_container_spec(
            &LocalBindMountRuntimeSync,
            &request,
            request.task_image,
            "/workspace/task",
            request.network_mode,
            false,
            &[],
        )
        .expect("container spec");
        assert!(spec.mounts.iter().any(|mount| {
            mount.host_path == secret_source
                && mount.container_path == "/root/.codex/auth.json"
                && mount.read_only
        }));
        assert!(spec.mounts.iter().any(|mount| {
            mount.host_path == cache_dir
                && mount.container_path == "/bucephalus/credentials/codex_oauth"
                && !mount.read_only
        }));
    }

    #[test]
    fn container_mounts_reject_duplicate_targets() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_duplicate_mount_targets");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let first_source = root.path.join("first_secret.txt");
        let second_source = root.path.join("second_secret.txt");
        fs::write(&first_source, "first\n").expect("first secret");
        fs::write(&second_source, "second\n").expect("second secret");
        let secret_file_mounts = vec![
            ResolvedSecretFileMount {
                id: "first".to_string(),
                source_from_host: first_source,
                target_path: "/root/.config/token".to_string(),
                credential_cache: None,
            },
            ResolvedSecretFileMount {
                id: "second".to_string(),
                source_from_host: second_source,
                target_path: "/root/.config/token/".to_string(),
                credential_cache: None,
            },
        ];
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &secret_file_mounts,
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let err = build_container_spec(
            &LocalBindMountRuntimeSync,
            &request,
            request.task_image,
            "/workspace/task",
            request.network_mode,
            false,
            &[],
        )
        .expect_err("duplicate container mount targets must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("container mount target '/root/.config/token' is declared more than once"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn container_mounts_reject_parent_child_target_overlap() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_overlapping_mount_targets");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let artifact_dir = root.path.join("agent-artifact");
        ensure_dir(&artifact_dir).expect("artifact dir");
        let secret_source = root.path.join("secret.txt");
        fs::write(&secret_source, "secret\n").expect("secret");
        let secret_file_mounts = vec![ResolvedSecretFileMount {
            id: "token".to_string(),
            source_from_host: secret_source,
            target_path: "/opt/custom-agent/token".to_string(),
            credential_cache: None,
        }];
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &secret_file_mounts,
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: Some(&artifact_dir),
            agent_artifact_mount_path: Some("/opt/custom-agent"),
            agent_artifact_read_only: true,
        };

        let err = build_container_spec(
            &LocalBindMountRuntimeSync,
            &request,
            request.task_image,
            "/workspace/task",
            request.network_mode,
            true,
            &[],
        )
        .expect_err("parent/child container mount targets must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("container mount target '/opt/custom-agent' overlaps with '/opt/custom-agent/token'")
                || msg.contains("container mount target '/opt/custom-agent/token' overlaps with '/opt/custom-agent'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn p0_base_image_bundle_avoids_host_workspace_bind_mount() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_p0_base_image_bundle_mount");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::BaseImageBundle,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let spec = build_container_spec(
            &LocalBindMountRuntimeSync,
            &request,
            request.task_image,
            "/workspace/task",
            request.network_mode,
            false,
            &[],
        )
        .expect("container spec");
        let mounts = &spec.mounts;
        assert!(
            !mounts
                .iter()
                .any(|mount| mount.container_path == "/workspace/task"
                    || mount.container_path == BUCEPHALUS_CONTRACT_WORKSPACE_DIR),
            "base_image_bundle should copy into the task workdir instead of keeping host workspace binds: {:?}",
            mounts
        );
    }

    #[test]
    fn p0_i03_injected_container_env_includes_agent_path() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_p0_path_env");
        let mut runtime = legacy_contract_runtime_fixture();
        runtime.command_raw = vec!["agentctl".to_string(), "run".to_string()];
        runtime.image = "image:latest".to_string();
        runtime.sandbox_image = Some("image:latest".to_string());
        runtime.execution = agent_execution_fixture(Some("image:latest"));
        runtime.agent_artifact = Some(PathBuf::from("/tmp/agent-artifact"));
        runtime.agent_artifact_mount_path = Some("/opt/custom-agent".to_string());
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "image:latest",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: runtime.agent_artifact.as_deref(),
            agent_artifact_mount_path: runtime.agent_artifact_mount_path.as_deref(),
            agent_artifact_read_only: runtime.agent_artifact_read_only,
        };
        let args = build_exec_env(&request, "/workspace/task", None, true);
        assert!(
            args.get("PATH").is_some_and(|value| {
                value
                    == "/opt/custom-agent/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
            }),
            "PATH injection env missing: {:?}",
            args
        );
    }

    #[test]
    fn runtime_command_workspace_binding_tracks_declared_task_workdir() {
        let rendered = resolve_agent_runtime_command(
            &["agent".to_string(), "$WORKSPACE".to_string()],
            &json!({}),
            &BTreeMap::new(),
        )
        .expect("render command");
        assert_eq!(rendered[1], TASK_WORKDIR_TEMPLATE_PLACEHOLDER);

        let (_root, paths) = create_trial_paths_fixture("bucephalus_runtime_workspace_binding");
        let mut runtime = legacy_contract_runtime_fixture();
        runtime.command_raw = rendered;
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &BTreeMap::new(),
            runtime_overrides_env: &BTreeMap::new(),
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let resolved = resolve_runtime_agent_command(&request).expect("resolve runtime command");
        assert_eq!(
            resolved,
            vec!["agent".to_string(), "/workspace/task".to_string()]
        );
    }

    #[test]
    fn resolve_runtime_agent_command_does_not_infer_agent_specific_io_flags() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_agent_file_io_flags");
        let mut runtime = legacy_contract_runtime_fixture();
        runtime.command_raw = vec![
            "/opt/agent/bin/bun".to_string(),
            "/opt/agent/bin/agentctl.js".to_string(),
            "run".to_string(),
            "--provider".to_string(),
            "codex".to_string(),
            "--model".to_string(),
            "codex-spark".to_string(),
        ];
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &BTreeMap::new(),
            runtime_overrides_env: &BTreeMap::new(),
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let resolved = resolve_runtime_agent_command(&request).expect("resolve runtime command");
        assert_eq!(resolved, runtime.command_raw);
    }

    #[test]
    fn resolve_agent_runtime_command_interpolates_variant_bindings() {
        let rendered = resolve_agent_runtime_command(
            &[
                "agent".to_string(),
                "--provider".to_string(),
                "$provider".to_string(),
                "--model".to_string(),
                "$model".to_string(),
                "--reasoning".to_string(),
                "$reasoning".to_string(),
                "--temperature=$temperature".to_string(),
            ],
            &json!({
                "provider": "gemini",
                "model": "gemini-3-flash-preview",
                "reasoning": "medium",
                "temperature": 0.7
            }),
            &BTreeMap::new(),
        )
        .expect("render command");

        assert_eq!(
            rendered,
            vec![
                "agent".to_string(),
                "--provider".to_string(),
                "gemini".to_string(),
                "--model".to_string(),
                "gemini-3-flash-preview".to_string(),
                "--reasoning".to_string(),
                "medium".to_string(),
                "--temperature=0.7".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_runtime_agent_command_projects_declared_event_path() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_agent_event_path");
        let mut runtime = legacy_contract_runtime_fixture();
        runtime.command_raw = vec![
            "agentctl".to_string(),
            "run".to_string(),
            "--events".to_string(),
            "__BUCEPHALUS_EVENT_PATH_agent_events__".to_string(),
        ];
        runtime.event_sinks.push(AgentRuntimeEventSink {
            id: "agent_events".to_string(),
            format: "jsonl".to_string(),
            path: lab_core::BUCEPHALUS_TRAJECTORY_PATH.to_string(),
            mode: "jsonl".to_string(),
            persist: true,
            ingest: true,
        });
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.out.join("agent-events.jsonl"),
        );
        let empty_json = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &BTreeMap::new(),
            runtime_overrides_env: &BTreeMap::new(),
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let resolved = resolve_runtime_agent_command(&request).expect("resolve runtime command");
        assert!(
            command_contains_flag_value(
                &resolved,
                "--events",
                lab_core::BUCEPHALUS_TRAJECTORY_PATH
            ),
            "agent command should receive runner-owned event path: {:?}",
            resolved
        );
    }

    #[test]
    fn resolve_runtime_agent_command_does_not_duplicate_existing_agent_input_flags() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_agent_existing_io_flags");
        let mut runtime = legacy_contract_runtime_fixture();
        runtime.command_raw = vec![
            "agentctl".to_string(),
            "run".to_string(),
            "--input-file".to_string(),
            "/tmp/task.json".to_string(),
            "--output".to_string(),
            "/tmp/result.json".to_string(),
        ];
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &BTreeMap::new(),
            runtime_overrides_env: &BTreeMap::new(),
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let resolved = resolve_runtime_agent_command(&request).expect("resolve runtime command");
        assert_eq!(
            resolved
                .iter()
                .filter(|arg| arg.as_str() == "--input-file")
                .count(),
            1,
            "agent command should not duplicate --input-file: {:?}",
            resolved
        );
        assert_eq!(
            resolved
                .iter()
                .filter(|arg| arg.as_str() == "--output")
                .count(),
            1,
            "agent command should not duplicate --output: {:?}",
            resolved
        );
    }

    #[test]
    fn parse_runtime_env_file_rejects_nonportable_key() {
        let guard = TempDirGuard::new("runtime_env_file_bad_key");
        let path = guard.path.join("env.list");
        fs::write(&path, "GOOD=value\nBAD-KEY=value\n").expect("write env file");

        let err = parse_runtime_env_file(&path)
            .expect_err("env files must reject names that are not portable env vars");
        let msg = err.to_string();
        assert!(
            msg.contains("portable environment variable name"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_runtime_env_inputs_rejects_nonportable_cli_key() {
        let mut runtime_env = BTreeMap::new();
        runtime_env.insert("BAD KEY".to_string(), "value".to_string());
        let execution = RunExecutionOptions {
            runtime_env,
            ..RunExecutionOptions::default()
        };

        let err = resolve_runtime_env_inputs(&execution)
            .expect_err("--env inputs must reject names that cannot cross runtimes safely");
        let msg = err.to_string();
        assert!(
            msg.contains("portable environment variable name"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_env_templates_rejects_nonportable_yaml_key() {
        let mut env = BTreeMap::new();
        env.insert("1BAD".to_string(), "value".to_string());

        let err = resolve_env_templates(&env, &json!({}), &BTreeMap::new(), "variant.env")
            .expect_err("YAML env maps must reject nonportable env var names");
        let msg = err.to_string();
        assert!(
            msg.contains("portable environment variable name"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn build_exec_env_replaces_workspace_placeholder() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_workspace_env_placeholder");
        let runtime = legacy_contract_runtime_fixture();
        let mut runtime_env = BTreeMap::new();
        runtime_env.insert(
            "CONFIG_DIR".to_string(),
            format!("{}/config", TASK_WORKDIR_TEMPLATE_PLACEHOLDER),
        );
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &BTreeMap::new(),
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let args = build_exec_env(&request, "/workspace/task", None, true);
        assert!(
            args.get("CONFIG_DIR") == Some(&"/workspace/task/config".to_string()),
            "workspace placeholder should resolve in container env: {:?}",
            args
        );
        assert!(
            args.get("WORKSPACE") == Some(&"/workspace/task".to_string()),
            "WORKSPACE env should match the declared task workdir: {:?}",
            args
        );
    }

    #[test]
    fn host_grader_receives_launch_env_and_host_contract_paths() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_host_grader_env");
        let runtime = legacy_contract_runtime_fixture();
        let mut runtime_env = BTreeMap::new();
        runtime_env.insert("ANTHROPIC_API_KEY".to_string(), "test-key".to_string());
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &BTreeMap::new(),
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "full",
            grader: None,
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let resolved = ResolvedGradingPhase {
            image: "host".to_string(),
            workdir: paths.exp_dir.to_string_lossy().to_string(),
            command: vec![
                "sh".to_string(),
                "-lc".to_string(),
                "printf '%s\n%s\n%s\n%s\n' \"$ANTHROPIC_API_KEY\" \"$TRANSPORT_VALUE\" \"$BUCEPHALUS_RESULT_PATH\" \"$WORKSPACE\" > \"$BUCEPHALUS_MAPPED_GRADER_OUTPUT_PATH\"".to_string(),
            ],
            extra_mounts: Vec::new(),
            injected_bundle_host_path: None,
            injected_copy_dest: None,
        };
        let mut transport_env = BTreeMap::new();
        transport_env.insert("TRANSPORT_VALUE".to_string(), "declared-input".to_string());

        let outcome = run_host_grader(
            &request,
            &resolved,
            "success",
            &transport_env,
            &paths.state.join("host_grader_stdout.log"),
            &paths.state.join("host_grader_stderr.log"),
        )
        .expect("host grader");

        assert_eq!(outcome.exit_code, Some(0));
        let output =
            fs::read_to_string(paths.out.join(MAPPED_GRADER_OUTPUT_FILENAME)).expect("mapped env");
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "test-key");
        assert_eq!(lines[1], "declared-input");
        assert_eq!(lines[2], io_paths.result_host.to_string_lossy());
        assert_eq!(lines[3], "/workspace/task");
    }

    #[test]
    fn preflight_grading_smoke_ignores_grade_error_marker_when_mapped_output_is_valid() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_preflight_marker_ignore");
        atomic_write_json_pretty(
            &paths.out.join(MAPPED_GRADER_OUTPUT_FILENAME),
            &json!({
                "schema_version": "trial_conclusion_v1",
                "payload": { "resolved": 1.0 },
                "reported_outcome": "success",
                "primary_metric": { "name": "resolved", "value": 1.0 },
                "grader": { "name": "test_grader", "strategy": "in_task_runtime" }
            }),
        )
        .expect("mapped output");
        fs::write(
            paths.out.join(GRADING_ERROR_FILENAME),
            "grader_command_failed:1\n",
        )
        .expect("grade marker");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let grader = GraderConfig::in_task_runtime(vec![
            "python3".to_string(),
            task_workdir_support_destination_path("grader.py"),
        ]);
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };
        let failures = validate_preflight_grading_smoke_outputs(
            &request,
            &GRADING_POLICY_EXIT_CODE.to_string(),
        );
        assert!(
            failures.is_empty(),
            "valid mapped output should suppress grade-error marker failures: {:?}",
            failures
        );
    }

    #[test]
    fn validate_grading_contract_accepts_hidden_asset_isolation_plan() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_hidden_asset_guard");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let grader = in_task_grader_config(
            vec![
                "python3".to_string(),
                task_workdir_support_destination_path("grader.py"),
            ],
            InTaskRuntimeGradingConfig {
                hidden_paths: vec!["/testbed/.hidden".to_string()],
                revealed_paths: vec!["/testbed/.hidden".to_string()],
            },
        );
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/testbed",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        crate::trial::grade::validate_grading_contract(&request)
            .expect("hidden asset isolation should now be supported");
    }

    fn host_grader_config(capability: &str, command: Vec<String>) -> GraderConfig {
        GraderConfig {
            strategy: GradingStrategy::Host,
            command,
            max_concurrency: None,
            in_task_runtime: None,
            injected: None,
            separate: None,
            host: Some(HostGradingConfig {
                capability: capability.to_string(),
            }),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
        }
    }

    fn separate_grader_config() -> GraderConfig {
        GraderConfig {
            strategy: GradingStrategy::Separate,
            command: vec!["python".to_string(), "grade.py".to_string()],
            max_concurrency: None,
            in_task_runtime: None,
            injected: None,
            separate: Some(SeparateGradingConfig {
                image: "python:3.11-slim".to_string(),
                workdir: "/workspace/task".to_string(),
            }),
            host: None,
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
        }
    }

    fn in_task_grader_config(
        command: Vec<String>,
        in_task_runtime: InTaskRuntimeGradingConfig,
    ) -> GraderConfig {
        GraderConfig {
            strategy: GradingStrategy::InTaskRuntime,
            command,
            max_concurrency: None,
            in_task_runtime: Some(in_task_runtime),
            injected: None,
            separate: None,
            host: None,
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
        }
    }

    #[test]
    fn resolve_host_grader_command_rejects_incomplete_capability_token() {
        let root = TempDirGuard::new("bucephalus_host_grader_bad_token");
        let grader = host_grader_config(
            "host_eval_capability",
            vec![
                "python3".to_string(),
                "__BUCEPHALUS_HOST_GRADER_CAPABILITY__/host_eval_capability".to_string(),
            ],
        );

        let err = resolve_host_grader_command(&grader, &root.path)
            .expect_err("incomplete host grader capability token should fail");
        assert!(
            err.to_string().contains(
                "trial_runtime.grader.command[1] must reference a host grader capability"
            ),
            "unexpected error: {}",
            err
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_host_grader_command_rejects_package_capability_symlink_escape() {
        let root = TempDirGuard::new("bucephalus_host_grader_package_symlink_escape");
        let capability = "host_eval_capability";
        let capability_root = root.path.join(HOST_GRADER_CAPABILITIES_DIR).join(capability);
        ensure_dir(&capability_root).expect("capability root");
        let outside = root.path.join("outside.py");
        fs::write(&outside, "#!/usr/bin/env python3\nprint('escape')\n").expect("outside file");
        symlink(&outside, capability_root.join("run_host_evaluation.py"))
            .expect("capability symlink");
        let grader = host_grader_config(
            capability,
            vec![
                "python3".to_string(),
                format!("{HOST_GRADER_CAPABILITY_PREFIX}/{capability}/run_host_evaluation.py"),
            ],
        );

        let err = resolve_host_grader_command(&grader, &root.path)
            .expect_err("package capability symlink escapes must fail");
        assert!(
            err.to_string()
                .contains("resolves outside package capability root")
        );
    }

    #[test]
    fn validate_grading_contract_rejects_mismatched_hidden_asset_visibility_lists() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_hidden_asset_guard_lengths");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let grader = in_task_grader_config(
            vec![
                "python3".to_string(),
                task_workdir_support_destination_path("grader.py"),
            ],
            InTaskRuntimeGradingConfig {
                hidden_paths: vec!["/testbed/.hidden".to_string()],
                revealed_paths: vec![
                    "/testbed/.hidden".to_string(),
                    "/testbed/.hidden_extra".to_string(),
                ],
            },
        );
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/testbed",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let err = crate::trial::grade::validate_grading_contract(&request)
            .expect_err("mismatched hidden/revealed lengths should fail");
        assert!(
            err.to_string().contains("matching lengths"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn p7_local_docker_executor_hides_in_task_runtime_assets_until_grading() {
        let _runtime_guard = lock_runtime_control_tests();
        if !docker_runtime_available() {
            eprintln!("skipping in-task-image hidden asset test: docker daemon unavailable");
            return;
        }

        let root = TempDirGuard::new("bucephalus_p7_hidden_asset_runtime");
        let image = match try_build_docker_test_image(
            &root.path,
            "hidden-assets",
            concat!(
                "FROM python:3.11-slim\n",
                "RUN mkdir -p /workspace/task/.hidden\n",
                "RUN python3 - <<'PY'\n",
                "from pathlib import Path\n",
                "Path('/workspace/task/.hidden/grader.py').write_text(",
                "\"from pathlib import Path\\n\"",
                "\"agent_file = Path('/workspace/task/agent_visible.txt')\\n\"",
                "\"if not agent_file.exists():\\n    raise SystemExit('missing agent output')\\n\"",
                "\"Path('/bucephalus/out/grade.json').write_text('{\\\"resolved\\\":1.0}')\\n\"",
                ")\n",
                "PY\n",
                "WORKDIR /workspace/task\n",
            ),
        ) {
            Ok(image) => image,
            Err(err) => {
                eprintln!("skipping in-task-image hidden asset test: {err}");
                return;
            }
        };

        let agent_bundle = ensure_test_agent_bundle(&root.path, "hidden-assets-agent");
        write_executable_script(
            &agent_bundle.join("bin/agent.sh"),
            concat!(
                "#!/bin/sh\n",
                "set -e\n",
                "if [ -e \"$WORKSPACE/.hidden/grader.py\" ]; then\n",
                "  echo 'hidden grader asset leaked into agent step' >&2\n",
                "  exit 17\n",
                "fi\n",
                "printf 'agent-visible\\n' > \"$WORKSPACE/agent_visible.txt\"\n",
                "printf '%s' '{\"checkpoints\":[]}' > /bucephalus/out/result.json\n",
            ),
        );

        let mut runtime = legacy_contract_runtime_fixture();
        runtime.command_raw = vec!["/bin/sh".to_string(), "/opt/agent/bin/agent.sh".to_string()];
        runtime.image = image.clone();
        runtime.sandbox_image = Some(image.clone());
        runtime.execution = agent_execution_fixture(Some(&image));
        runtime.agent_artifact = Some(agent_bundle.clone());
        runtime.agent_artifact_mount_path = Some("/opt/agent".to_string());

        let grader_outputs = serde_json::from_value(json!({
            "grade": {
                "capture": {
                    "type": "file",
                    "path": "/bucephalus/out/grade.json",
                    "format": "json",
                    "required": true
                }
            }
        }))
        .expect("grader outputs");
        let mut grader = in_task_grader_config(
            vec![
                "python3".to_string(),
                "/workspace/task/.hidden/grader.py".to_string(),
            ],
            InTaskRuntimeGradingConfig {
                hidden_paths: vec!["/workspace/task/.hidden/grader.py".to_string()],
                revealed_paths: vec!["/workspace/task/.hidden/grader.py".to_string()],
            },
        );
        grader.outputs = grader_outputs;
        let task = task_row_value("task_hidden", &image, "/workspace/task", Some(30_000));
        let task_boundary = parse_task_boundary_from_packaged_task(&task).expect("task boundary");
        let variant = preflight_test_variant();
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        let runtime_experiment = json!({
            "runtime": {
                "network": {
                    "task_sandbox": "none"
                }
            },
            "trial_runtime": {
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": { "from": "task_row" },
                        "workdir": { "from": "task_row" }
                    }
                },
                "agent": {
                    "artifact": ".",
                    "artifact_type": "structured_json",
                    "command": ["/bin/sh", "/opt/agent/bin/agent.sh"],
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json"
                            }
                        }
                    }
                },
                "execution": {
                    "agent_site": "task_runtime"
                },
                "grader": {
                    "strategy": "in_task_runtime",
                    "command": ["python3", "/workspace/task/.hidden/grader.py"],
                    "outputs": {
                        "grade": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/grade.json",
                                "format": "json",
                                "required": true
                            }
                        }
                    }
                }
            },
            "metrics": [
                {
                    "id": "resolved",
                    "source": {
                        "type": "grader_output",
                        "output": "grade",
                        "pointer": "/resolved"
                    },
                    "primary": true,
                    "required": true
                }
            ],
            "policy": {
                "task_sandbox": {
                    "hardening": {
                        "no_new_privileges": true,
                        "drop_all_caps": true
                    }
                }
            }
        });

        let prepared = prepare_task_environment_for_test(
            &root.path,
            &trial_dir,
            &runtime_experiment,
            &variant,
            &task_boundary,
            &runtime,
        )
        .expect("prepare task environment");
        let task_sandbox_plan = prepared
            .manifest
            .task_sandbox_plan
            .clone()
            .expect("task sandbox plan");
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let request = TrialRunRequest {
            package_root: &root.path,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &prepared.trial_paths,
            dynamic_mounts: &prepared.dynamic_mounts,
            secret_file_mounts: &[],
            io_paths: &prepared.io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: &task_boundary.task_image,
            task_workdir: &task_boundary.task_workdir,
            task_materialization_kind: task_boundary.materialization.kind.clone(),
            agent_artifact: runtime.agent_artifact.as_deref(),
            agent_artifact_mount_path: runtime.agent_artifact_mount_path.as_deref(),
            agent_artifact_read_only: runtime.agent_artifact_read_only,
        };

        let executor = LocalDockerExecutionBackend::new();
        let outcome = executor
            .execute_attempt(TrialRuntimeExecutionRequest {
                trial_dir: &trial_dir,
                schedule_idx: 0,
                attempt_no: 1,
                run_request: &request,
                task_id: &task_boundary.task_id,
                variant_id: &variant.id,
                repl_idx: 0,
                task_sandbox_plan: &task_sandbox_plan,
            })
            .expect("execute trial runtime");

        assert_eq!(
            outcome.agent_exit_status,
            "0",
            "agent stdout:\n{}\nagent stderr:\n{}",
            fs::read_to_string(trial_agent_stdout_path(&trial_dir)).unwrap_or_default(),
            fs::read_to_string(trial_agent_stderr_path(&trial_dir)).unwrap_or_default()
        );
        assert!(
            outcome.trial_conclusion_row.is_some(),
            "grader transport should synthesize a conclusion; grader stdout:\n{}\ngrader stderr:\n{}\ngrade_error_reason={:?}",
            fs::read_to_string(trial_grader_stdout_path(&trial_dir)).unwrap_or_default(),
            fs::read_to_string(trial_grader_stderr_path(&trial_dir)).unwrap_or_default(),
            outcome.grade_error_reason
        );
    }

    #[test]
    fn validate_grading_contract_rejects_missing_grader_command() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_missing_grader_command");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let grader = in_task_grader_config(Vec::new(), InTaskRuntimeGradingConfig::default());
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let err = crate::trial::grade::validate_grading_contract(&request)
            .expect_err("missing grader command should be rejected");
        assert!(
            err.to_string().contains("no grader command resolved"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn p0_i03_task_sandbox_container_spec_is_generic() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_p0_task_sandbox_spec");
        let mut runtime = legacy_contract_runtime_fixture();
        runtime.command_raw = vec!["agent".to_string(), "run".to_string()];
        runtime.image = "example/task-image:latest".to_string();
        runtime.sandbox_image = Some("example/task-image:latest".to_string());
        runtime.execution = agent_execution_fixture(Some("example/task-image:latest"));
        runtime.agent_artifact = Some(PathBuf::from("/tmp/agent-artifact"));
        runtime.agent_artifact_mount_path = Some("/opt/agent".to_string());
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let empty_json = json!({});
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &empty_json,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "example/task-image:latest",
            task_workdir: "/workspace",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: runtime.agent_artifact.as_deref(),
            agent_artifact_mount_path: runtime.agent_artifact_mount_path.as_deref(),
            agent_artifact_read_only: runtime.agent_artifact_read_only,
        };
        let spec = build_container_spec(
            &LocalBindMountRuntimeSync,
            &request,
            "example/task-image:latest",
            "/workspace",
            request.network_mode,
            false,
            &[],
        )
        .expect("container spec");
        assert_eq!(spec.platform, None);

        assert!(
            !spec
                .mounts
                .iter()
                .any(|mount| mount.container_path == "/opt/agent"),
            "task sandbox spec must not mount the agent bundle: {:?}",
            spec.mounts
        );
    }

    #[test]
    fn p0_i04_artifact_digest_pin_rejects_mutation() {
        let root = TempDirGuard::new("bucephalus_p0_artifact_digest_pin");
        let artifact_dir = root.path.join("artifact");
        ensure_dir(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("agent.txt"), "v1").expect("artifact v1");
        let digest_before = compute_artifact_content_digest(&artifact_dir).expect("digest before");
        fs::write(artifact_dir.join("agent.txt"), "v2").expect("artifact v2");
        let mut runtime = legacy_contract_runtime_fixture();
        runtime.command_raw = vec!["agentctl".to_string()];
        runtime.image = "image".to_string();
        runtime.sandbox_image = Some("image".to_string());
        runtime.execution = agent_execution_fixture(Some("image"));
        runtime.agent_artifact = Some(artifact_dir.clone());
        runtime.agent_artifact_mount_path = Some("/opt/agent".to_string());
        runtime.agent_artifact_digest = Some(format!("sha256:{}", digest_before));
        runtime.agent_artifact_resolved_path = Some(artifact_dir.clone());
        let err = validate_agent_artifact_pin(&runtime).expect_err("digest mismatch expected");
        assert!(
            err.to_string().contains("digest mismatch"),
            "unexpected error: {}",
            err
        );
    }


    #[test]
    fn sanitize_name_for_path_strips_special_chars() {
        assert_eq!(
            sanitize_name_for_path("hello world.v2/3"),
            "hello_world_v2_3"
        );
    }

    #[test]
    fn sanitize_name_for_path_all_special_returns_experiment() {
        assert_eq!(sanitize_name_for_path("@#$%^&"), "experiment");
    }

    #[test]
    fn sanitize_name_for_path_trims_leading_trailing_underscores() {
        assert_eq!(sanitize_name_for_path("__hello__"), "hello");
    }

    #[test]
    fn sanitize_name_for_path_preserves_alphanumeric_hyphen() {
        assert_eq!(sanitize_name_for_path("a-b_c"), "a-b_c");
    }

    #[test]
    fn sanitize_name_for_path_empty_returns_experiment() {
        assert_eq!(sanitize_name_for_path(""), "experiment");
    }

    #[test]
    fn sanitize_name_for_path_unicode_to_underscores() {
        let result = sanitize_name_for_path("café");
        assert!(!result.contains('é'), "unicode char should be replaced");
    }

    #[test]
    fn sanitize_name_for_path_numbers_preserved() {
        assert_eq!(sanitize_name_for_path("test123"), "test123");
    }

    #[test]
    fn sanitize_name_for_path_mixed_special_and_alpha() {
        let result = sanitize_name_for_path("my@experiment.v2");
        assert!(!result.contains('@'));
        assert!(!result.contains('.'));
        assert!(result.contains("my"));
        assert!(result.contains("v2"));
    }

    #[test]
    fn sanitize_name_for_path_only_underscores() {
        assert_eq!(sanitize_name_for_path("___"), "experiment");
    }

    #[test]
    fn sanitize_name_for_path_single_char() {
        assert_eq!(sanitize_name_for_path("x"), "x");
    }

    #[test]
    fn experiment_workload_type_reads_explicit_value() {
        let spec = json!({"experiment": {"id": "e1"}});
        assert_eq!(experiment_workload_type(&spec).unwrap(), "agent_runtime");
    }

    #[test]
    fn experiment_workload_type_rejects_removed_field() {
        let spec = json!({"experiment": {"workload_type": "  "}});
        let err = experiment_workload_type(&spec).unwrap_err();
        assert!(err
            .to_string()
            .contains("/experiment/workload_type is not supported in v1"));
    }

    #[test]
    fn experiment_workload_type_missing_field_defaults() {
        let spec = json!({"experiment": {"id": "e1"}});
        assert_eq!(experiment_workload_type(&spec).unwrap(), "agent_runtime");
    }

    #[test]
    fn experiment_workload_type_removed_field_fails_even_when_trimmed() {
        let spec = json!({"experiment": {"workload_type": "  agent_runtime  "}});
        assert!(experiment_workload_type(&spec).is_err());
    }

    #[test]
    fn experiment_random_seed_reads_scheduling_random_seed() {
        let spec = json!({"scheduling": {"random_seed": 99}});
        assert_eq!(experiment_random_seed(&spec), 99);
    }

    #[test]
    fn experiment_random_seed_defaults_to_one() {
        assert_eq!(experiment_random_seed(&json!({})), 1);
    }

    #[test]
    fn experiment_max_concurrency_parses_supported_values() {
        for (spec, expected) in [
            (json!({"scheduling": {"max_concurrency": 0}}), 1),
            (json!({}), 1),
            (json!({"scheduling": {"max_concurrency": 128}}), 128),
            (json!({"scheduling": {"max_concurrency": -1}}), 1),
        ] {
            assert_eq!(experiment_max_concurrency(&spec).unwrap(), expected);
        }
    }

    #[test]
    fn configured_network_modes_are_plane_specific() {
        let task_full = json!({"runtime": {"network": {"task_sandbox": "full"}}});
        let agent_egress = json!({"runtime": {"network": {"agent": "llm_egress"}}});
        let default_none = json!({"runtime": {"network": {"default": "none"}}});
        assert_eq!(configured_task_network_mode(&task_full).unwrap(), "full");
        assert_eq!(
            configured_agent_network_mode(&agent_egress).unwrap(),
            "llm_egress"
        );
        assert_eq!(configured_agent_network_mode(&default_none).unwrap(), "none");
        assert!(configured_task_network_mode(&json!({"runtime": {"network": {}}})).is_err());
        assert!(configured_agent_network_mode(&json!({"runtime": {"network": {}}})).is_err());
    }

    #[test]
    fn trial_index_from_trial_id_parses_standard_format() {
        assert_eq!(trial_index_from_trial_id("trial_5"), Some(5));
    }

    #[test]
    fn trial_index_from_trial_id_rejects_non_numeric() {
        assert_eq!(trial_index_from_trial_id("trial_abc"), None);
    }

    #[test]
    fn trial_index_from_trial_id_rejects_no_prefix() {
        assert_eq!(trial_index_from_trial_id("5"), None);
    }

    #[test]
    fn trial_index_from_trial_id_handles_zero() {
        assert_eq!(trial_index_from_trial_id("trial_0"), None);
    }

    #[test]
    fn trial_index_from_trial_id_large_number() {
        assert_eq!(trial_index_from_trial_id("trial_999999"), Some(999999));
    }

    #[test]
    fn trial_index_from_trial_id_empty_suffix() {
        assert_eq!(trial_index_from_trial_id("trial_"), None);
    }

    #[test]
    fn recover_reconciled_status_rejects_completed() {
        assert!(recover_reconciled_status("completed").is_err());
    }

    #[test]
    fn recover_reconciled_status_rejects_killed() {
        assert!(recover_reconciled_status("killed").is_err());
    }

    #[test]
    fn recover_reconciled_status_rejects_unknown() {
        assert!(recover_reconciled_status("unknown").is_err());
    }

    #[test]
    fn recover_reconciled_status_running_to_interrupted() {
        assert_eq!(recover_reconciled_status("running").unwrap(), "interrupted");
    }

    #[test]
    fn recover_reconciled_status_interrupted_to_interrupted() {
        assert_eq!(
            recover_reconciled_status("interrupted").unwrap(),
            "interrupted"
        );
    }

    #[test]
    fn recover_reconciled_status_paused_to_interrupted() {
        assert_eq!(recover_reconciled_status("paused").unwrap(), "interrupted");
    }

    #[test]
    fn recover_reconciled_status_failed_to_interrupted() {
        assert_eq!(recover_reconciled_status("failed").unwrap(), "interrupted");
    }

    #[test]
    fn recover_reconciled_status_empty_to_interrupted() {
        assert!(recover_reconciled_status("").is_err());
    }

    #[test]
    fn as_portable_rel_converts_backslashes() {
        assert_eq!(as_portable_rel(Path::new("a\\b\\c")), "a/b/c");
    }

    #[test]
    fn as_portable_rel_preserves_forward_slashes() {
        assert_eq!(as_portable_rel(Path::new("a/b/c")), "a/b/c");
    }

    #[test]
    fn as_portable_rel_mixed_separators() {
        assert_eq!(as_portable_rel(Path::new("a\\b/c\\d")), "a/b/c/d");
    }

    #[test]
    fn as_portable_rel_empty_path() {
        assert_eq!(as_portable_rel(Path::new("")), "");
    }

    #[test]
    fn strip_contract_prefix_exact_match_returns_empty() {
        assert_eq!(strip_contract_prefix("/in", "/in"), Some(""));
    }

    #[test]
    fn strip_contract_prefix_with_subpath_returns_rest() {
        assert_eq!(
            strip_contract_prefix("/in/data.json", "/in"),
            Some("/data.json")
        );
    }

    #[test]
    fn strip_contract_prefix_partial_match_returns_none() {
        assert_eq!(strip_contract_prefix("/infoo", "/in"), None);
    }

    #[test]
    fn strip_contract_prefix_no_slash_boundary_returns_none() {
        assert_eq!(
            strip_contract_prefix("/bucephalus/inbox", "/bucephalus/in"),
            None
        );
    }

    #[test]
    fn strip_contract_prefix_longer_prefix_returns_none() {
        assert!(strip_contract_prefix("/in", "/in/extra").is_none());
    }

    #[test]
    fn strip_contract_prefix_completely_different_returns_none() {
        assert!(strip_contract_prefix("/out/data", "/in").is_none());
    }

    #[test]
    fn resolve_contract_path_components_maps_all_roots() {
        let cases = vec![
            (BUCEPHALUS_CONTRACT_IN_DIR, ContractPathRoot::In),
            (BUCEPHALUS_CONTRACT_OUT_DIR, ContractPathRoot::Out),
        ];
        for (dir, expected_root) in cases {
            let path = format!("{}/file.txt", dir);
            let (root, rest) = resolve_contract_path_components(&path)
                .unwrap_or_else(|| panic!("should resolve {}", dir));
            assert_eq!(root, expected_root, "root mismatch for {}", dir);
            assert_eq!(rest, "/file.txt", "rest mismatch for {}", dir);
        }
    }

    #[test]
    fn resolve_contract_path_components_unknown_root_returns_none() {
        assert!(resolve_contract_path_components("/unknown/path").is_none());
    }

    #[test]
    fn resolve_contract_path_components_exact_root() {
        let (root, rest) = resolve_contract_path_components(BUCEPHALUS_CONTRACT_IN_DIR).unwrap();
        assert_eq!(root, ContractPathRoot::In);
        assert_eq!(rest, "");
    }


    fn test_contract_roots(trial_dir: &Path) -> ContractPathHostRoots {
        ContractPathHostRoots::from_trial_dir(trial_dir)
    }

    #[test]
    fn map_contract_path_container_mode_maps_in_dir() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let result = map_contract_path_to_host(
            &format!("{}/task.json", BUCEPHALUS_CONTRACT_IN_DIR),
            &roots,
            ContractPathMode::ContainerMount,
        )
        .unwrap();
        assert_eq!(result, trial.join("in").join("task.json"));
    }

    #[test]
    fn map_contract_path_runtime_events_mode_maps_out_dir() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let result = map_contract_path_to_host(
            &format!("{}/events.jsonl", BUCEPHALUS_CONTRACT_OUT_DIR),
            &roots,
            ContractPathMode::RuntimeEvents,
        )
        .unwrap();
        assert_eq!(result, trial.join("out").join("events.jsonl"));
    }

    #[test]
    fn map_contract_path_container_mode_maps_out_dir() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let result = map_contract_path_to_host(
            &format!("{}/result.json", BUCEPHALUS_CONTRACT_OUT_DIR),
            &roots,
            ContractPathMode::ContainerMount,
        )
        .unwrap();
        assert_eq!(result, trial.join("out").join("result.json"));
    }

    #[test]
    fn map_contract_path_container_mode_maps_task_support_dir() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let result = map_contract_path_to_host(
            &task_workdir_support_destination_path("dep.tar"),
            &roots,
            ContractPathMode::ContainerMount,
        )
        .unwrap();
        assert_eq!(
            result,
            trial
                .join("workspace")
                .join(BUCEPHALUS_RUNNER_SUPPORT_REL_DIR)
                .join("dep.tar")
        );
    }

    #[test]
    fn map_contract_path_container_mode_maps_task_workdir_placeholder() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let result = map_contract_path_to_host(
            &format!("{}/src/main.py", BUCEPHALUS_TASK_WORKDIR_PLACEHOLDER),
            &roots,
            ContractPathMode::ContainerMount,
        )
        .unwrap();
        assert_eq!(result, trial.join("workspace").join("src").join("main.py"));
    }

    #[test]
    fn map_contract_path_container_mode_rejects_empty() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let err = map_contract_path_to_host("", &roots, ContractPathMode::ContainerMount)
            .expect_err("should reject empty");
        assert!(
            err.to_string().contains("empty"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn map_contract_path_container_mode_rejects_relative() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let err =
            map_contract_path_to_host("relative/path", &roots, ContractPathMode::ContainerMount)
                .expect_err("should reject relative");
        assert!(
            err.to_string().contains("absolute"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn map_contract_path_container_mode_trims_whitespace() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let padded = format!("  {}  ", BUCEPHALUS_CONTRACT_IN_DIR);
        let result =
            map_contract_path_to_host(&padded, &roots, ContractPathMode::ContainerMount).unwrap();
        assert_eq!(result, trial.join("in"));
    }

    #[test]
    fn map_contract_path_runtime_events_mode_rejects_task_support_placeholder() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let err = map_contract_path_to_host(
            &task_workdir_support_destination_path("data.bin"),
            &roots,
            ContractPathMode::RuntimeEvents,
        )
        .expect_err("RuntimeEvents should reject task workdir placeholders");
        assert!(
            err.to_string().contains("absolute"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn map_contract_path_runtime_events_allows_state() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let result = map_contract_path_to_host(
            &format!("{}/events.jsonl", BUCEPHALUS_CONTRACT_STATE_DIR),
            &roots,
            ContractPathMode::RuntimeEvents,
        )
        .unwrap();
        assert_eq!(result, trial.join("state").join("events.jsonl"));
    }

    #[test]
    fn map_contract_path_nested_subpath_resolves() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let result = map_contract_path_to_host(
            &format!("{}/nested/deep/file.json", BUCEPHALUS_CONTRACT_IN_DIR),
            &roots,
            ContractPathMode::ContainerMount,
        )
        .unwrap();
        assert_eq!(
            result,
            trial
                .join("in")
                .join("nested")
                .join("deep")
                .join("file.json")
        );
    }

    #[test]
    fn map_contract_path_double_slash_in_subpath() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        let result = map_contract_path_to_host(
            &format!("{}//file.json", BUCEPHALUS_CONTRACT_IN_DIR),
            &roots,
            ContractPathMode::ContainerMount,
        )
        .unwrap();
        assert!(result.to_string_lossy().contains("file.json"));
    }

    #[test]
    fn map_contract_path_container_mode_unknown_path_fails() {
        let trial = PathBuf::from("/tmp/trial_1");
        let roots = test_contract_roots(&trial);
        assert!(map_contract_path_to_host(
            "/unknown/root/file",
            &roots,
            ContractPathMode::ContainerMount
        )
        .is_err());
    }

    #[test]
    fn mode_allows_root_container_mount_allows_all() {
        for root in [ContractPathRoot::In, ContractPathRoot::Out] {
            assert!(
                mode_allows_root(ContractPathMode::ContainerMount, root),
                "ContainerMount should allow {:?}",
                root
            );
        }
    }

    #[test]
    fn mode_allows_root_runtime_events_allows_out() {
        assert!(mode_allows_root(
            ContractPathMode::RuntimeEvents,
            ContractPathRoot::Out
        ));
    }

    #[test]
    fn contract_path_host_roots_from_trial_dir_creates_expected_dirs() {
        let trial_dir = PathBuf::from("/tmp/trial_1");
        let roots = ContractPathHostRoots::from_trial_dir(&trial_dir);
        assert_eq!(roots.in_dir, trial_dir.join("in"));
        assert_eq!(roots.out_dir, trial_dir.join("out"));
        assert_eq!(roots.workspace_dir, trial_dir.join("workspace"));
    }

    #[test]
    fn resolve_event_path_for_trial_out_events_resolves() {
        let trial = PathBuf::from("/tmp/trial_1");
        let result = resolve_event_path_for_trial(
            &format!("{}/events.jsonl", BUCEPHALUS_CONTRACT_OUT_DIR),
            &trial,
        )
        .unwrap();
        assert_eq!(result, trial.join("out").join("events.jsonl"));
    }

    #[test]
    fn resolve_event_path_for_trial_rejects_task_support_placeholder() {
        let trial = PathBuf::from("/tmp/trial_1");
        assert!(resolve_event_path_for_trial(
            &task_workdir_support_destination_path("data.bin"),
            &trial
        )
        .is_err());
    }

    #[test]
    fn validate_container_workspace_path_rejects_non_workspace_root() {
        let err = validate_container_workspace_path("/some/other/path").expect_err("should reject");
        assert!(
            err.to_string().contains("mount_path must be under"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_container_workspace_path_exact_match() {
        validate_container_workspace_path(BUCEPHALUS_CONTRACT_WORKSPACE_DIR).unwrap();
    }

    #[test]
    fn validate_container_workspace_path_subpath() {
        let path = format!("{}/src/main.py", BUCEPHALUS_CONTRACT_WORKSPACE_DIR);
        validate_container_workspace_path(&path).unwrap();
    }

    #[test]
    fn validate_container_workspace_path_rejects_dot_dot() {
        let path = format!("{}/../escape", BUCEPHALUS_CONTRACT_WORKSPACE_DIR);
        assert!(validate_container_workspace_path(&path).is_err());
    }


    fn current_trial_runtime_experiment_base() -> Value {
        json!({
            "experiment": {"id": "e"},
            "matrix": {
                "tasks": {"source": "file", "path": "tasks.jsonl"},
                "variants": [{"id": "baseline", "baseline": true, "config": {}}],
                "repeats": 1
            },
            "runtime": {
                "compute": {"backend": "local-docker"},
                "storage": {"backend": "local-fs"},
                "traces": {"backend": "local-stdout"},
                "network": {"task_sandbox": "none", "agent": "none"}
            },
            "policy": {
                "timeout_ms": 60000,
                "task_sandbox": {}
            },
            "trial_runtime": {
                "task": {
                    "interface": "writable_workspace",
                    "workspace": {
                        "source": "container_image",
                        "image": {"from": "task_row"},
                        "workdir": {"from": "task_row"}
                    }
                },
                "agent": {
                    "command": ["sh", "-lc", "true"],
                    "image": "alpine:latest",
                    "artifact_type": "structured_json",
                    "outputs": {
                        "result": {
                            "capture": {
                                "type": "file",
                                "path": "/bucephalus/out/result.json",
                                "format": "json"
                            }
                        },
                        "patch": {
                            "capture": {
                                "type": "workspace_diff",
                                "format": "unified_diff"
                            }
                        }
                    }
                },
                "execution": {"agent_site": "agent_container"},
                "grader": {"strategy": "none"}
            }
        })
    }

    #[test]
    fn parse_trial_runtime_accepts_case_transport_sources() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["trial_runtime"]["grader"] = json!({
            "strategy": "in_task_runtime",
            "in_task_runtime": {},
            "command": ["python3", "grade.py"],
            "inputs": {
                "prompt": {
                    "source": {"case": "input.prompt"},
                    "materialize": {"as": "json_file", "path": "/bucephalus/out/grader_inputs/prompt.json"},
                    "required": true
                }
            },
            "outputs": {
                "report": {
                    "capture": {
                        "type": "file",
                        "path": "/bucephalus/out/report.json",
                        "format": "json",
                        "required": true
                    }
                }
            }
        });

        let runtime = parse_trial_runtime_config(&spec).expect("case transport source");
        assert_eq!(
            runtime.grader.inputs["prompt"].source.case.as_deref(),
            Some("input.prompt")
        );
    }

    #[test]
    fn validate_required_fields_batch3_rejects_legacy_v1_shape() {
        let spec = json!({
            "version": "1.0",
            "experiment": {"id": "e1", "name": "test"},
            "dataset": {"path": "tasks.jsonl"},
            "design": {"replications": 1},
            "baseline": {"variant_id": "baseline"},
            "runtime": {"image": "img:latest", "command": ["python", "main.py"]}
        });
        let err = validate_required_fields(&spec).unwrap_err();
        assert!(err.to_string().contains("experiment version '1.0'"));
    }

    #[test]
    fn validate_required_fields_rejects_legacy_runtime_surface() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["task_runtime"] = json!({
            "agent": {"command": ["python", "main.py"]}
        });
        let err = validate_required_fields(&spec).expect_err("legacy /runtime should be rejected");
        assert!(
            err.to_string()
                .contains("/task_runtime is not supported; define task behavior under /trial_runtime/task"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_required_fields_allows_defaulted_authoring_fields() {
        let spec = current_trial_runtime_experiment_base();
        validate_required_fields(&spec)
            .expect("defaulted sanitization_profile and task_sandbox.profile should be optional");
    }

    #[test]
    fn trial_runtime_sidecars_expose_env_only_to_declared_stage() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_sidecar_stage_env");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let mut runtime_experiment = current_trial_runtime_experiment_base();
        runtime_experiment["sidecars"] = json!({
            "mcp-bash": {
                "image": "ghcr.io/acme/mcp-bash-server:v0.4",
                "lifecycle": "per-trial",
                "expose": {
                    "MCP_URL": "http://mcp-bash:8080"
                }
            }
        });
        runtime_experiment["trial_runtime"]["agent"]["sidecars"] = json!(["mcp-bash"]);
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: None,
            grading_enabled: false,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let agent_env = sidecar_env_for_stage_for_test(&request, "agent").expect("agent env");
        let grader_env = sidecar_env_for_stage_for_test(&request, "grader").expect("grader env");

        assert_eq!(
            agent_env.get("MCP_URL").map(String::as_str),
            Some("http://mcp-bash:8080")
        );
        assert!(
            grader_env.is_empty(),
            "sidecar env must not leak to stages that did not declare the sidecar"
        );
    }

    #[test]
    fn docker_active_container_plan_counts_unique_sidecars_and_separate_grader() {
        let (_root, paths) = create_trial_paths_fixture("bucephalus_docker_active_plan");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let mut runtime_experiment = current_trial_runtime_experiment_base();
        runtime_experiment["sidecars"] = json!({
            "cache": {"image": "redis:7", "lifecycle": "per-trial"},
            "mcp-bash": {"image": "ghcr.io/acme/mcp-bash-server:v0.4", "lifecycle": "per-trial"}
        });
        runtime_experiment["trial_runtime"]["agent"]["sidecars"] = json!(["cache", "mcp-bash"]);
        runtime_experiment["trial_runtime"]["grader"]["sidecars"] = json!(["cache"]);
        let grader = separate_grader_config();
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let units =
            planned_docker_active_container_units_for_test(&request).expect("container units");

        assert_eq!(
            units, 4,
            "task sandbox + two unique sidecars + separate grader should be counted"
        );
    }

    #[test]
    fn docker_active_container_cap_rejects_trial_that_cannot_fit() {
        let _lock = lock_runtime_control_tests();
        let (_root, paths) = create_trial_paths_fixture("bucephalus_docker_active_cap");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let mut runtime_experiment = current_trial_runtime_experiment_base();
        runtime_experiment["sidecars"] = json!({
            "cache": {"image": "redis:7", "lifecycle": "per-trial"},
            "mcp-bash": {"image": "ghcr.io/acme/mcp-bash-server:v0.4", "lifecycle": "per-trial"}
        });
        runtime_experiment["trial_runtime"]["agent"]["sidecars"] = json!(["cache", "mcp-bash"]);
        let grader = separate_grader_config();
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        {
            let _guard =
                EnvVarGuard::set(&[(BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS_ENV, Some("nope"))]);
            let err = match acquire_docker_active_container_permit_for_test(&request) {
                Ok(_) => panic!("invalid container cap must not fall back to default"),
                Err(err) => err,
            };
            assert!(err.to_string().contains("positive integer"));
        }

        let _guard = EnvVarGuard::set(&[(
            BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS_ENV,
            Some("3"),
        )]);
        let err = match acquire_docker_active_container_permit_for_test(&request) {
            Ok(_) => panic!("single trial that needs four containers must not fit under cap of three"),
            Err(err) => err,
        };

        assert!(
            err.to_string()
                .contains(BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS_ENV),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn modal_active_sandbox_cap_counts_separate_grader_sandbox() {
        let _lock = lock_modal_env_tests();
        let _guard = EnvVarGuard::set(&[(BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES_ENV, Some("1"))]);
        let (_root, paths) = create_trial_paths_fixture("bucephalus_modal_active_cap");
        let runtime = legacy_contract_runtime_fixture();
        let runtime_env = BTreeMap::new();
        let overrides = BTreeMap::new();
        let io_paths = prepared_trial_io_fixture(
            paths.out.join("result.json"),
            paths.state.join("events.jsonl"),
        );
        let runtime_experiment = json!({});
        let grader = separate_grader_config();
        let request = TrialRunRequest {
            package_root: &paths.exp_dir,
            runtime_experiment: &runtime_experiment,
            runtime: &runtime,
            variant_args: &[],
            runtime_env: &runtime_env,
            runtime_overrides_env: &overrides,
            trial_paths: &paths,
            dynamic_mounts: &[],
            secret_file_mounts: &[],
            io_paths: &io_paths,
            network_mode: "none",
            grader: Some(&grader),
            grading_enabled: true,
            run_id: "run_1",
            task_image: "python:3.11-slim",
            task_workdir: "/workspace/task",
            task_materialization_kind: TaskMaterializationKind::TaskImage,
            agent_artifact: None,
            agent_artifact_mount_path: None,
            agent_artifact_read_only: true,
        };

        let units = planned_modal_active_sandbox_units_for_test(&request).expect("sandbox units");
        let err = match acquire_modal_active_sandbox_permit_for_test(&request) {
            Ok(_) => panic!("separate grader should require a second modal sandbox"),
            Err(err) => err,
        };

        assert_eq!(units, 2);
        assert!(
            err.to_string()
                .contains(BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES_ENV),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_trial_runtime_rejects_host_stages_with_sidecars() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["sidecars"] = json!({
            "svc": {"image": "python:3.11-slim", "lifecycle": "per-trial"}
        });
        spec["trial_runtime"]["agent"]["image"] = Value::Null;
        spec["trial_runtime"]["agent"]["sidecars"] = json!(["svc"]);
        spec["trial_runtime"]["execution"]["agent_site"] = json!("host");

        let err = parse_trial_runtime_config(&spec)
            .expect_err("host agent stages cannot attach container sidecars");
        assert!(
            err.to_string().contains("agent_site=host cannot attach"),
            "unexpected error: {}",
            err
        );

        let mut spec = current_trial_runtime_experiment_base();
        spec["sidecars"] = json!({
            "svc": {"image": "python:3.11-slim", "lifecycle": "per-trial"}
        });
        spec["trial_runtime"]["grader"] = json!({
            "strategy": "host",
            "command": ["echo", "ok"],
            "host": {"capability": "__BUCEPHALUS_HOST_GRADER_CAPABILITY__demo"},
            "sidecars": ["svc"],
            "inputs": {},
            "outputs": {
                "score": {
                    "capture": {
                        "type": "file",
                        "path": "/bucephalus/out/score.json",
                        "format": "json"
                    }
                }
            }
        });

        let err = parse_trial_runtime_config(&spec)
            .expect_err("host grader stages cannot attach container sidecars");
        assert!(
            err.to_string()
                .contains("grader.strategy=host cannot attach"),
            "unexpected error: {}",
            err
        );

        let mut spec = current_trial_runtime_experiment_base();
        spec["sidecars"] = json!({
            "svc": {"image": "python:3.11-slim", "lifecycle": "per-trial"}
        });
        spec["trial_runtime"]["grader"] = json!({
            "strategy": "none",
            "sidecars": ["svc"]
        });

        let err = parse_trial_runtime_config(&spec)
            .expect_err("disabled grader stages cannot attach container sidecars");
        assert!(
            err.to_string()
                .contains("grader.strategy=none cannot attach"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_required_fields_rejects_sidecar_ids_that_cannot_be_runtime_aliases() {
        for invalid_id in ["bad/id", "bad_id", "BadId", "-bad", "bad-"] {
            let mut spec = current_trial_runtime_experiment_base();
            spec["sidecars"] = json!({
                invalid_id: {"image": "python:3.11-slim", "lifecycle": "per-trial"}
            });
            spec["trial_runtime"]["agent"]["sidecars"] = json!([invalid_id]);

            let err = validate_required_fields(&spec)
                .expect_err("sidecar aliases should be portable runtime ids");
            assert!(
                err.to_string().contains("portable runtime alias"),
                "unexpected error for {}: {}",
                invalid_id,
                err
            );
        }
    }

    #[test]
    fn validate_required_fields_requires_explicit_sidecar_lifecycle() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["sidecars"] = json!({
            "svc": {"image": "python:3.11-slim"}
        });
        spec["trial_runtime"]["agent"]["sidecars"] = json!(["svc"]);

        let err =
            validate_required_fields(&spec).expect_err("sidecar lifecycle must be explicit");
        assert!(
            err.to_string().contains("/sidecars/svc lifecycle is required"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_required_fields_rejects_hermetic_task_network_full() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["policy"]["sanitization_profile"] = json!("hermetic_functional");
        spec["runtime"]["network"]["task_sandbox"] = json!("full");
        let err = validate_required_fields(&spec).expect_err("hermetic task network should fail");
        assert!(
            err.to_string()
                .contains("sanitization_profile=hermetic_functional requires"),
            "unexpected error: {}",
            err
        );
        assert!(
            err.to_string()
                .contains("omit policy.sanitization_profile or set it to standard_runtime"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_required_fields_rejects_hermetic_agent_network_full() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["policy"]["sanitization_profile"] = json!("hermetic_functional");
        spec["runtime"]["network"]["agent"] = json!("full");
        let err = validate_required_fields(&spec).expect_err("hermetic agent network should fail");
        assert!(
            err.to_string()
                .contains("requires runtime.network.agent 'none'"),
            "unexpected error: {}",
            err
        );
        assert!(
            err.to_string()
                .contains("omit policy.sanitization_profile or set it to standard_runtime"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_required_fields_allows_network_full_without_hermetic_profile() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["runtime"]["network"]["task_sandbox"] = json!("full");
        spec["runtime"]["network"]["agent"] = json!("full");
        validate_required_fields(&spec).expect("sanitization_profile is optional");
    }

    #[test]
    fn validate_required_fields_keeps_egress_mode_agent_only() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["runtime"]["network"]["agent"] = json!("llm_egress");
        validate_required_fields(&spec).expect("agent egress mode should pass");

        spec["runtime"]["network"]["task_sandbox"] = json!("llm_egress");
        let err = validate_required_fields(&spec).expect_err("task sandbox egress should fail");
        assert!(
            err.to_string()
                .contains("/runtime/network/task_sandbox must be one of: none, full, allowlist_enforced"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_required_fields_rejects_patch_file_outside_out_mount() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["trial_runtime"]["agent"]["outputs"]["result"]["capture"]["path"] =
            json!("/tmp/result.json");
        let err = validate_required_fields(&spec).expect_err("patch outside out mount should fail");
        assert!(
            err.to_string().contains("trial_runtime.agent.outputs.result"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_required_fields_allows_patch_file_under_out_mount() {
        let mut spec = current_trial_runtime_experiment_base();
        spec["trial_runtime"]["agent"]["outputs"]["patch"] = json!({
            "capture": {
                "type": "file",
                "path": "/bucephalus/out/candidate.patch",
                "format": "text"
            }
        });
        validate_required_fields(&spec).expect("file patch under /bucephalus/out should pass");
    }


    #[test]
    fn parse_fork_selector_checkpoint_valid() {
        match parse_fork_selector("checkpoint:cp1").unwrap() {
            ForkSelector::Checkpoint(name) => assert_eq!(name, "cp1"),
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn parse_fork_selector_step_valid() {
        match parse_fork_selector("step:42").unwrap() {
            ForkSelector::Step(s) => assert_eq!(s, 42),
            _ => panic!("expected Step"),
        }
    }

    #[test]
    fn parse_fork_selector_event_seq_valid() {
        match parse_fork_selector("event_seq:100").unwrap() {
            ForkSelector::EventSeq(s) => assert_eq!(s, 100),
            _ => panic!("expected EventSeq"),
        }
    }

    #[test]
    fn parse_fork_selector_missing_colon_fails() {
        assert!(parse_fork_selector("checkpoint_name").is_err());
    }

    #[test]
    fn parse_fork_selector_unknown_kind_fails() {
        match parse_fork_selector("snapshot:x") {
            Ok(_) => panic!("should fail for unknown kind"),
            Err(err) => assert!(err.to_string().contains("checkpoint|step|event_seq")),
        }
    }

    #[test]
    fn parse_fork_selector_step_non_integer_fails() {
        assert!(parse_fork_selector("step:abc").is_err());
    }

    #[test]
    fn parse_fork_selector_event_seq_non_integer_fails() {
        assert!(parse_fork_selector("event_seq:xyz").is_err());
    }

    #[test]
    fn parse_fork_selector_step_negative_fails() {
        assert!(parse_fork_selector("step:-1").is_err());
    }

    #[test]
    fn parse_fork_selector_checkpoint_with_colons() {
        match parse_fork_selector("checkpoint:a:b:c").unwrap() {
            ForkSelector::Checkpoint(name) => assert_eq!(name, "a:b:c"),
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn parse_fork_selector_step_zero_accepted() {
        match parse_fork_selector("step:0").unwrap() {
            ForkSelector::Step(s) => assert_eq!(s, 0),
            _ => panic!("expected Step(0)"),
        }
    }

    #[test]
    fn parse_fork_selector_checkpoint_with_slashes() {
        match parse_fork_selector("checkpoint:/path/to/cp").unwrap() {
            ForkSelector::Checkpoint(name) => assert_eq!(name, "/path/to/cp"),
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn parse_fork_selector_empty_checkpoint_value_fails() {
        assert!(parse_fork_selector("checkpoint:").is_err());
    }

    #[test]
    fn parse_fork_selector_whitespace_checkpoint_value_fails() {
        assert!(parse_fork_selector("checkpoint:   ").is_err());
    }

    #[test]
    fn parse_fork_selector_large_step_value() {
        match parse_fork_selector("step:999999999").unwrap() {
            ForkSelector::Step(s) => assert_eq!(s, 999999999),
            _ => panic!("expected Step"),
        }
    }

    #[test]
    fn parse_fork_selector_empty_string_fails() {
        assert!(parse_fork_selector("").is_err());
    }

    #[test]
    fn resolve_selector_checkpoint_by_name_finds_match() {
        let cp_path = format!("{}/checkpoint_1.json", BUCEPHALUS_CONTRACT_STATE_DIR);
        let output = json!({"checkpoints": [{"logical_name": "cp1", "path": &cp_path, "step": 1}]});
        let (_root, run_dir) = create_run_dir("resolve_cp_name", "run_1");
        let trial_dir = seed_parent_trial(&run_dir, "trial_1", output["checkpoints"].clone(), "completed", None);
        let result = resolve_selector_checkpoint(
            &ForkSelector::Checkpoint("cp1".to_string()),
            Some(&output),
            &trial_dir,
            true,
        )
        .unwrap();
        assert!(result
            .as_deref()
            .is_some_and(|token| token.starts_with("lineage:")));
    }

    #[test]
    fn resolve_selector_checkpoint_by_name_no_match_strict_fails() {
        let root = TempDirGuard::new("resolve_cp_strict_fail");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).unwrap();
        let output = json!({"checkpoints": []});
        let err = resolve_selector_checkpoint(
            &ForkSelector::Checkpoint("missing".to_string()),
            Some(&output),
            &trial_dir,
            true,
        )
        .expect_err("strict should fail");
        assert!(err.to_string().contains("strict_source_unavailable"));
    }

    #[test]
    fn resolve_selector_checkpoint_by_name_no_match_nonstrict_returns_none() {
        let root = TempDirGuard::new("resolve_cp_nonstrict");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).unwrap();
        let output = json!({"checkpoints": []});
        let result = resolve_selector_checkpoint(
            &ForkSelector::Checkpoint("missing".to_string()),
            Some(&output),
            &trial_dir,
            false,
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_selector_checkpoint_by_step_highest_lte() {
        let cp5_path = format!("{}/cp5.json", BUCEPHALUS_CONTRACT_STATE_DIR);
        let output = json!({"checkpoints": [
            {"logical_name": "cp3", "path": &format!("{}/cp3.json", BUCEPHALUS_CONTRACT_STATE_DIR), "step": 3},
            {"logical_name": "cp5", "path": &cp5_path, "step": 5},
            {"logical_name": "cp8", "path": &format!("{}/cp8.json", BUCEPHALUS_CONTRACT_STATE_DIR), "step": 8}
        ]});
        let (_root, run_dir) = create_run_dir("resolve_cp_step", "run_1");
        let trial_dir = seed_parent_trial(&run_dir, "trial_1", output["checkpoints"].clone(), "completed", None);
        let result =
            resolve_selector_checkpoint(&ForkSelector::Step(5), Some(&output), &trial_dir, false)
                .unwrap();
        assert!(result
            .as_deref()
            .is_some_and(|token| token.starts_with("lineage:")));
    }

    #[test]
    fn resolve_selector_checkpoint_by_step_no_qualifying_strict_fails() {
        let root = TempDirGuard::new("resolve_cp_step_strict");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).unwrap();
        let output = json!({"checkpoints": [
            {"logical_name": "cp3", "path": "/state/cp3.json", "step": 3},
            {"logical_name": "cp5", "path": "/state/cp5.json", "step": 5}
        ]});
        assert!(resolve_selector_checkpoint(
            &ForkSelector::Step(1),
            Some(&output),
            &trial_dir,
            true
        )
        .is_err());
    }

    #[test]
    fn resolve_selector_checkpoint_by_event_seq_highest_lte() {
        let cp_path = format!("{}/cp10.json", BUCEPHALUS_CONTRACT_STATE_DIR);
        let output = json!({"checkpoints": [
            {"logical_name": "cp5", "path": &format!("{}/cp5.json", BUCEPHALUS_CONTRACT_STATE_DIR), "step": 5},
            {"logical_name": "cp10", "path": &cp_path, "step": 10},
            {"logical_name": "cp20", "path": &format!("{}/cp20.json", BUCEPHALUS_CONTRACT_STATE_DIR), "step": 20}
        ]});
        let (_root, run_dir) = create_run_dir("resolve_cp_event_seq", "run_1");
        let trial_dir = seed_parent_trial(&run_dir, "trial_1", output["checkpoints"].clone(), "completed", None);
        let result = resolve_selector_checkpoint(
            &ForkSelector::EventSeq(15),
            Some(&output),
            &trial_dir,
            false,
        )
        .unwrap();
        assert!(result
            .as_deref()
            .is_some_and(|token| token.starts_with("lineage:")));
    }

    #[test]
    fn resolve_selector_checkpoint_no_output_strict_fails() {
        let root = TempDirGuard::new("resolve_cp_no_output_strict");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).unwrap();
        assert!(resolve_selector_checkpoint(
            &ForkSelector::Checkpoint("any".to_string()),
            None,
            &trial_dir,
            true
        )
        .is_err());
    }

    #[test]
    fn resolve_selector_checkpoint_no_output_nonstrict_returns_none() {
        let root = TempDirGuard::new("resolve_cp_no_output_nonstrict");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).unwrap();
        assert!(resolve_selector_checkpoint(
            &ForkSelector::Checkpoint("any".to_string()),
            None,
            &trial_dir,
            false
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn resolve_selector_checkpoint_empty_checkpoints_strict_fails() {
        let root = TempDirGuard::new("resolve_cp_empty_strict");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).unwrap();
        assert!(resolve_selector_checkpoint(
            &ForkSelector::Step(5),
            Some(&json!({"checkpoints": []})),
            &trial_dir,
            true
        )
        .is_err());
    }

    #[test]
    fn resolve_selector_checkpoint_empty_checkpoints_nonstrict_returns_none() {
        let root = TempDirGuard::new("resolve_cp_empty_nonstrict");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).unwrap();
        assert!(resolve_selector_checkpoint(
            &ForkSelector::Step(5),
            Some(&json!({"checkpoints": []})),
            &trial_dir,
            false
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn control_ack_received_missing_file_returns_false() {
        let root = TempDirGuard::new("ack_missing");
        assert!(
            !control_ack_received(&root.path.join("events.jsonl"), "pause", "v1").unwrap()
        );
    }

    #[test]
    fn control_ack_received_wrong_action_returns_false() {
        let root = TempDirGuard::new("ack_wrong_action");
        let events_path = root.path.join("events.jsonl");
        fs::write(
            &events_path,
            r#"{"event_type":"control_ack","action_observed":"resume","control_version":"v1"}"#,
        )
        .unwrap();
        assert!(!control_ack_received(&events_path, "pause", "v1").unwrap());
    }

    #[test]
    fn control_ack_received_wrong_version_returns_false() {
        let root = TempDirGuard::new("ack_wrong_version");
        let events_path = root.path.join("events.jsonl");
        fs::write(
            &events_path,
            r#"{"event_type":"control_ack","action_observed":"pause","control_version":"v2"}"#,
        )
        .unwrap();
        assert!(!control_ack_received(&events_path, "pause", "v1").unwrap());
    }

    #[test]
    fn control_ack_received_skips_invalid_json_lines() {
        let root = TempDirGuard::new("ack_invalid_json");
        let events_path = root.path.join("events.jsonl");
        fs::write(&events_path, "not valid json\n{\"event_type\":\"control_ack\",\"action_observed\":\"pause\",\"control_version\":\"v1\"}\n").unwrap();
        assert!(control_ack_received(&events_path, "pause", "v1").unwrap());
    }

    #[test]
    fn control_ack_received_skips_empty_lines() {
        let root = TempDirGuard::new("ack_empty_lines");
        let events_path = root.path.join("events.jsonl");
        fs::write(&events_path, "\n\n{\"event_type\":\"control_ack\",\"action_observed\":\"pause\",\"control_version\":\"v1\"}\n\n").unwrap();
        assert!(control_ack_received(&events_path, "pause", "v1").unwrap());
    }

    #[test]
    fn control_ack_received_skips_non_control_ack_events() {
        let root = TempDirGuard::new("ack_other_events");
        let events_path = root.path.join("events.jsonl");
        fs::write(&events_path, "{\"event_type\":\"step\",\"data\":\"x\"}\n{\"event_type\":\"control_ack\",\"action_observed\":\"pause\",\"control_version\":\"v1\"}\n").unwrap();
        assert!(control_ack_received(&events_path, "pause", "v1").unwrap());
    }

    #[test]
    fn load_event_rows_rejects_missing_event_type() {
        let root = TempDirGuard::new("event_missing_type");
        let events_path = root.path.join("events.jsonl");
        fs::write(&events_path, "{}\n").unwrap();

        let err = load_event_rows(&events_path, "run_1", "trial_1", 0, "variant_1", "task_1", 0)
            .expect_err("event rows must not invent event_type");
        assert!(
            err.to_string().contains("event row 1 missing event_type"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn read_control_seq_missing_file_returns_zero() {
        let root = TempDirGuard::new("ctrl_seq_missing");
        assert_eq!(
            read_control_seq(&root.path.join("control.json")).unwrap(),
            0
        );
    }

    #[test]
    fn read_control_seq_missing_seq_field_returns_zero() {
        let root = TempDirGuard::new("ctrl_seq_no_field");
        let path = root.path.join("control.json");
        atomic_write_json_pretty(&path, &json!({"status": "running"})).unwrap();
        assert_eq!(read_control_seq(&path).unwrap(), 0);
    }

    #[test]
    fn read_control_seq_reads_valid_seq() {
        let root = TempDirGuard::new("ctrl_seq_valid");
        let path = root.path.join("control.json");
        atomic_write_json_pretty(&path, &json!({"seq": 42})).unwrap();
        assert_eq!(read_control_seq(&path).unwrap(), 42);
    }

    #[test]
    fn apply_variant_binding_overrides_adds_new_keys() {
        let mut variant = Variant {
            id: "baseline".to_string(),
            bindings: json!({"existing": "value"}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let mut overrides = BTreeMap::new();
        overrides.insert("new_key".to_string(), json!("new_value"));
        apply_variant_binding_overrides(&mut variant, &overrides).unwrap();
        assert_eq!(variant.bindings["new_key"], json!("new_value"));
    }

    #[test]
    fn apply_variant_binding_overrides_overwrites_existing() {
        let mut variant = Variant {
            id: "baseline".to_string(),
            bindings: json!({"key": "old"}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let mut overrides = BTreeMap::new();
        overrides.insert("key".to_string(), json!("new"));
        apply_variant_binding_overrides(&mut variant, &overrides).unwrap();
        assert_eq!(variant.bindings["key"], json!("new"));
    }

    #[test]
    fn apply_variant_binding_overrides_preserves_untouched_keys() {
        let mut variant = Variant {
            id: "baseline".to_string(),
            bindings: json!({"keep": "this", "change": "old"}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let mut overrides = BTreeMap::new();
        overrides.insert("change".to_string(), json!("new"));
        apply_variant_binding_overrides(&mut variant, &overrides).unwrap();
        assert_eq!(variant.bindings["keep"], json!("this"));
        assert_eq!(variant.bindings["change"], json!("new"));
    }

    #[test]
    fn apply_variant_binding_overrides_empty_map_is_noop() {
        let mut variant = Variant {
            id: "baseline".to_string(),
            bindings: json!({"key": "value"}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let original = variant.bindings.clone();
        apply_variant_binding_overrides(&mut variant, &BTreeMap::new()).unwrap();
        assert_eq!(variant.bindings, original);
    }

    #[test]
    fn apply_variant_binding_overrides_creates_bindings_object_if_missing() {
        let mut variant = Variant {
            id: "baseline".to_string(),
            bindings: Value::Null,
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let mut overrides = BTreeMap::new();
        overrides.insert("key".to_string(), json!("value"));
        apply_variant_binding_overrides(&mut variant, &overrides).unwrap();
        assert_eq!(variant.bindings["key"], json!("value"));
    }

    #[test]
    fn apply_variant_binding_overrides_nested_key() {
        let mut variant = Variant {
            id: "baseline".to_string(),
            bindings: json!({}),
            args: Vec::new(),
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let mut overrides = BTreeMap::new();
        overrides.insert("nested.deep.key".to_string(), json!(42));
        apply_variant_binding_overrides(&mut variant, &overrides).unwrap();
        assert_eq!(variant.bindings["nested"]["deep"]["key"], json!(42));
    }


    #[test]
    fn schedule_variant_sequential_multi_replication() {
        let slots = build_trial_schedule(2, 3, 2, SchedulingPolicy::VariantSequential, 0);
        assert_eq!(slots.len(), 12);
        assert!(slots[..6].iter().all(|s| s.variant_idx == 0));
        assert!(slots[6..].iter().all(|s| s.variant_idx == 1));
    }

    #[test]
    fn schedule_paired_interleaved_multi_replication() {
        let slots = build_trial_schedule(2, 3, 2, SchedulingPolicy::PairedInterleaved, 0);
        assert_eq!(slots.len(), 12);
        assert_eq!(slots[0].task_idx, 0);
        assert_eq!(slots[0].variant_idx, 0);
        assert_eq!(slots[1].task_idx, 0);
        assert_eq!(slots[1].repl_idx, 0);
        assert_eq!(slots[1].variant_idx, 1);
        assert_eq!(slots[2].task_idx, 0);
        assert_eq!(slots[2].repl_idx, 1);
        assert_eq!(slots[2].variant_idx, 0);
    }

    #[test]
    fn schedule_randomized_large_is_deterministic_with_seed() {
        let a = build_trial_schedule(5, 10, 3, SchedulingPolicy::Randomized, 12345);
        let b = build_trial_schedule(5, 10, 3, SchedulingPolicy::Randomized, 12345);
        assert_eq!(a.len(), 150);
        for (i, (sa, sb)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(
                (sa.variant_idx, sa.task_idx, sa.repl_idx),
                (sb.variant_idx, sb.task_idx, sb.repl_idx),
                "mismatch at slot {}",
                i
            );
        }
    }

    #[test]
    fn schedule_randomized_zero_seed_still_deterministic() {
        let a = build_trial_schedule(2, 3, 1, SchedulingPolicy::Randomized, 0);
        let b = build_trial_schedule(2, 3, 1, SchedulingPolicy::Randomized, 0);
        for (sa, sb) in a.iter().zip(b.iter()) {
            assert_eq!(
                (sa.variant_idx, sa.task_idx, sa.repl_idx),
                (sb.variant_idx, sb.task_idx, sb.repl_idx)
            );
        }
    }

    #[test]
    fn schedule_randomized_max_seed_does_not_overflow() {
        let slots = build_trial_schedule(2, 2, 1, SchedulingPolicy::Randomized, u64::MAX);
        assert_eq!(slots.len(), 4);
    }

    #[test]
    fn schedule_single_variant_many_tasks() {
        let slots = build_trial_schedule(1, 100, 1, SchedulingPolicy::VariantSequential, 0);
        assert_eq!(slots.len(), 100);
        assert!(slots.iter().all(|s| s.variant_idx == 0));
    }

    #[test]
    fn schedule_many_variants_single_task() {
        let slots = build_trial_schedule(50, 1, 1, SchedulingPolicy::VariantSequential, 0);
        assert_eq!(slots.len(), 50);
        for (i, slot) in slots.iter().enumerate() {
            assert_eq!(slot.variant_idx, i);
        }
    }

    #[test]
    fn schedule_slot_count_equals_product() {
        for policy in [
            SchedulingPolicy::VariantSequential,
            SchedulingPolicy::PairedInterleaved,
            SchedulingPolicy::Randomized,
        ] {
            let slots = build_trial_schedule(3, 4, 2, policy, 42);
            assert_eq!(slots.len(), 3 * 4 * 2, "policy {:?}", policy);
        }
    }

    #[test]
    fn schedule_randomized_different_seeds_produce_different_orders() {
        let a = build_trial_schedule(3, 5, 1, SchedulingPolicy::Randomized, 111);
        let b = build_trial_schedule(3, 5, 1, SchedulingPolicy::Randomized, 222);
        let same = a
            .iter()
            .zip(b.iter())
            .all(|(sa, sb)| sa.variant_idx == sb.variant_idx && sa.task_idx == sb.task_idx);
        assert!(!same, "different seeds should produce different orderings");
    }

    #[test]
    fn schedule_variant_sequential_order_is_variant_first() {
        let slots = build_trial_schedule(2, 3, 1, SchedulingPolicy::VariantSequential, 0);
        assert_eq!((slots[0].variant_idx, slots[0].task_idx), (0, 0));
        assert_eq!((slots[1].variant_idx, slots[1].task_idx), (0, 1));
        assert_eq!((slots[2].variant_idx, slots[2].task_idx), (0, 2));
        assert_eq!((slots[3].variant_idx, slots[3].task_idx), (1, 0));
    }

    #[test]
    fn schedule_paired_interleaved_order_is_task_first() {
        let slots = build_trial_schedule(2, 3, 1, SchedulingPolicy::PairedInterleaved, 0);
        assert_eq!((slots[0].task_idx, slots[0].variant_idx), (0, 0));
        assert_eq!((slots[1].task_idx, slots[1].variant_idx), (0, 1));
        assert_eq!((slots[2].task_idx, slots[2].variant_idx), (1, 0));
    }

    #[test]
    fn parse_policies_retry_on_empty_array() {
        let spec = json!({"policy": {"policies": {"retry": {"max_attempts": 3, "retry_on": []}}}});
        let config = parse_policies(&spec).unwrap();
        assert_eq!(config.retry_max_attempts, 3);
        assert!(config.retry_on.is_empty());
    }

    #[test]
    fn parse_policies_retry_on_multiple_triggers() {
        let spec = json!({"policy": {"policies": {"retry": {"max_attempts": 2, "retry_on": ["error", "timeout"]}}}});
        let config = parse_policies(&spec).unwrap();
        assert_eq!(config.retry_on.len(), 2);
    }

    #[test]
    fn parse_policies_concurrency_max_in_flight() {
        let spec =
            json!({"policy": {"policies": {"concurrency": {"max_in_flight_per_variant": 4}}}});
        assert_eq!(
            parse_policies(&spec)
                .unwrap()
                .concurrency
                .max_in_flight_per_variant,
            Some(4)
        );
    }

    #[test]
    fn parse_policies_concurrency_require_chain_lease() {
        let spec = json!({"policy": {"policies": {"concurrency": {"require_chain_lease": false}}}});
        assert!(!parse_policies(&spec)
            .unwrap()
            .concurrency
            .require_chain_lease);
    }

    #[test]
    fn parse_policies_pruning_max_consecutive_failures() {
        let spec = json!({"policy": {"policies": {"pruning": {"max_consecutive_failures": 5}}}});
        assert_eq!(
            parse_policies(&spec)
                .unwrap()
                .pruning_max_consecutive_failures,
            Some(5)
        );
    }

    #[test]
    fn parse_policies_pruning_default_none() {
        assert!(parse_policies(&json!({"policy": {"policies": {}}}))
            .unwrap()
            .pruning_max_consecutive_failures
            .is_none());
    }

    #[test]
    fn parse_policies_scheduling_paired_interleaved() {
        assert_eq!(
            parse_policies(&json!({"policy": {"policies": {"scheduling": "paired_interleaved"}}}))
                .unwrap()
                .scheduling,
            SchedulingPolicy::PairedInterleaved
        );
    }

    #[test]
    fn parse_policies_scheduling_randomized() {
        assert_eq!(
            parse_policies(&json!({"policy": {"policies": {"scheduling": "randomized"}}}))
                .unwrap()
                .scheduling,
            SchedulingPolicy::Randomized
        );
    }

    #[test]
    fn parse_policies_scheduling_default_variant_sequential() {
        assert_eq!(
            parse_policies(&json!({"policy": {"policies": {}}}))
                .unwrap()
                .scheduling,
            SchedulingPolicy::VariantSequential
        );
    }

    #[test]
    fn parse_policies_scheduling_default_paired_interleaved_for_paired_design() {
        assert_eq!(
            parse_policies(&json!({"scheduling": {"comparison": "paired"}, "policy": {"policies": {}}}))
                .unwrap()
                .scheduling,
            SchedulingPolicy::PairedInterleaved
        );
    }

    #[test]
    fn parse_policies_explicit_variant_sequential_overrides_paired_default() {
        assert_eq!(
            parse_policies(&json!({"scheduling": {"comparison": "paired"}, "policy": {"policies": {"scheduling": "variant_sequential"}}}))
                .unwrap()
                .scheduling,
            SchedulingPolicy::VariantSequential
        );
    }

    #[test]
    fn parse_policies_no_policies_section_uses_defaults() {
        let config = parse_policies(&json!({})).unwrap();
        assert_eq!(config.scheduling, SchedulingPolicy::VariantSequential);
        assert_eq!(config.retry_max_attempts, 1);
        assert!(config.retry_on.is_empty());
    }

    #[test]
    fn parse_policies_retry_max_attempts() {
        assert_eq!(
            parse_policies(&json!({"policy": {"policies": {"retry": {"max_attempts": 5}}}}))
                .unwrap()
                .retry_max_attempts,
            5
        );
    }

    #[test]
    fn should_retry_outcome_error_always_retried_default() {
        assert!(should_retry_outcome("error", "0", &[]));
    }

    #[test]
    fn should_retry_outcome_success_never_retried() {
        assert!(!should_retry_outcome("success", "0", &[]));
    }

    #[test]
    fn should_retry_outcome_timeout_with_timeout_trigger() {
        assert!(should_retry_outcome(
            "timeout",
            "0",
            &["timeout".to_string()]
        ));
    }

    #[test]
    fn should_retry_outcome_timeout_without_trigger() {
        assert!(!should_retry_outcome(
            "timeout",
            "0",
            &["error".to_string()]
        ));
    }

    #[test]
    fn should_retry_outcome_failure_with_failure_trigger() {
        assert!(should_retry_outcome(
            "completed",
            "1",
            &["failure".to_string()]
        ));
    }

    #[test]
    fn should_retry_outcome_nonzero_exit_default_retried() {
        assert!(should_retry_outcome("completed", "1", &[]));
    }

    #[test]
    fn should_retry_outcome_error_with_error_trigger() {
        assert!(should_retry_outcome("error", "0", &["error".to_string()]));
    }

    #[test]
    fn should_retry_outcome_error_without_error_trigger() {
        assert!(!should_retry_outcome(
            "error",
            "0",
            &["timeout".to_string()]
        ));
    }

    #[test]
    fn should_retry_outcome_success_with_triggers_never_retried() {
        assert!(!should_retry_outcome(
            "success",
            "0",
            &["error".to_string(), "timeout".to_string()]
        ));
    }

    #[test]
    fn normalize_schedule_progress_fills_missing_attempt() {
        let mut progress = ScheduleProgress {
            schema_version: String::new(),
            run_id: "run_001".to_string(),
            total_slots: 1,
            next_schedule_index: 1,
            next_trial_index: 1,
            schedule: vec![],
            completed_slots: vec![SlotCompletion {
                schedule_index: 0,
                trial_id: "trial_1".to_string(),
                status: "completed".to_string(),
                slot_commit_id: "abc".to_string(),
                attempt: 0,
            }],
            pruned_variants: vec![],
            consecutive_failures: BTreeMap::new(),
            updated_at: String::new(),
        };
        normalize_schedule_progress(&mut progress);
        assert_eq!(progress.completed_slots[0].attempt, 1);
    }

    #[test]
    fn write_schedule_progress_rejects_missing_commit_id() {
        let root = TempDirGuard::new("sched_missing_commit_id");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir).unwrap();
        let progress = ScheduleProgress {
            schema_version: String::new(),
            run_id: "run_001".to_string(),
            total_slots: 1,
            next_schedule_index: 1,
            next_trial_index: 1,
            schedule: vec![],
            completed_slots: vec![SlotCompletion {
                schedule_index: 0,
                trial_id: "trial_1".to_string(),
                status: "completed".to_string(),
                slot_commit_id: "".to_string(),
                attempt: 1,
            }],
            pruned_variants: vec![],
            consecutive_failures: BTreeMap::new(),
            updated_at: String::new(),
        };
        let err = write_schedule_progress(&run_dir, &progress).expect_err("missing commit id");
        assert!(err.to_string().contains("missing slot_commit_id"));
    }

    #[test]
    fn normalize_schedule_progress_sets_schema_version() {
        let mut progress = ScheduleProgress {
            schema_version: "old".to_string(),
            run_id: "run_001".to_string(),
            total_slots: 0,
            next_schedule_index: 0,
            next_trial_index: 0,
            schedule: vec![],
            completed_slots: vec![],
            pruned_variants: vec![],
            consecutive_failures: BTreeMap::new(),
            updated_at: String::new(),
        };
        normalize_schedule_progress(&mut progress);
        assert_eq!(progress.schema_version, "schedule_progress_v2");
    }

    #[test]
    fn load_schedule_progress_rejects_v1_schema() {
        let root = TempDirGuard::new("sched_v1");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        let progress = json!({"schema_version": "schedule_progress_v1", "run_id": "run_001", "total_slots": 0, "next_schedule_index": 0, "next_trial_index": 0, "schedule": [], "completed_slots": [], "pruned_variants": [], "consecutive_failures": {}, "use_container": false, "updated_at": ""});
        let mut store = BackingSqliteStore::open(&run_dir).unwrap();
        assert!(store
            .put_runtime_json(RUNTIME_KEY_SCHEDULE_PROGRESS, &progress)
            .unwrap_err()
            .to_string()
            .contains("schema_version 'schedule_progress_v1'"));
    }

    #[test]
    fn write_and_load_schedule_progress_roundtrip() {
        let root = TempDirGuard::new("sched_roundtrip");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        let progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "run_001".to_string(),
            total_slots: 6,
            next_schedule_index: 3,
            next_trial_index: 3,
            schedule: vec![TrialSlot {
                variant_idx: 0,
                task_idx: 0,
                repl_idx: 0,
            }],
            completed_slots: vec![SlotCompletion {
                schedule_index: 0,
                trial_id: "trial_1".to_string(),
                status: "completed".to_string(),
                slot_commit_id: "abc123".to_string(),
                attempt: 1,
            }],
            pruned_variants: vec![],
            consecutive_failures: BTreeMap::new(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        };
        write_schedule_progress(&run_dir, &progress).unwrap();
        let loaded = load_schedule_progress(&run_dir).unwrap();
        assert_eq!(loaded.run_id, "run_001");
        assert_eq!(loaded.total_slots, 6);
    }

    #[test]
    fn default_slot_attempt_returns_one() {
        assert_eq!(default_slot_attempt(), 1);
    }


    #[test]
    fn variant_digest_deterministic() {
        let v = Variant {
            id: "v1".to_string(),
            bindings: json!({"key": "value"}),
            args: vec![],
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        assert_eq!(variant_digest(&v).unwrap(), variant_digest(&v).unwrap());
    }

    #[test]
    fn variant_digest_changes_with_bindings() {
        let v1 = Variant {
            id: "v1".to_string(),
            bindings: json!({"key": "a"}),
            args: vec![],
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        let v2 = Variant {
            id: "v1".to_string(),
            bindings: json!({"key": "b"}),
            args: vec![],
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        };
        assert_ne!(variant_digest(&v1).unwrap(), variant_digest(&v2).unwrap());
    }

    #[test]
    fn variant_digest_changes_with_env() {
        let mut env1 = BTreeMap::new();
        env1.insert("FOO".to_string(), "bar".to_string());
        let mut env2 = BTreeMap::new();
        env2.insert("FOO".to_string(), "baz".to_string());
        let v1 = Variant {
            id: "v1".to_string(),
            bindings: json!({}),
            args: vec![],
            env: env1,
            image: None,
            runtime_overrides: None,
        };
        let v2 = Variant {
            id: "v1".to_string(),
            bindings: json!({}),
            args: vec![],
            env: env2,
            image: None,
            runtime_overrides: None,
        };
        assert_ne!(variant_digest(&v1).unwrap(), variant_digest(&v2).unwrap());
    }

    #[test]
    fn variant_digest_changes_with_image() {
        let v1 = Variant {
            id: "v1".to_string(),
            bindings: json!({}),
            args: vec![],
            env: BTreeMap::new(),
            image: Some("img:v1".to_string()),
            runtime_overrides: None,
        };
        let v2 = Variant {
            id: "v1".to_string(),
            bindings: json!({}),
            args: vec![],
            env: BTreeMap::new(),
            image: Some("img:v2".to_string()),
            runtime_overrides: None,
        };
        assert_ne!(variant_digest(&v1).unwrap(), variant_digest(&v2).unwrap());
    }

    #[test]
    fn write_resolved_variants_persists_behavior_surface_digests() {
        let root = TempDirGuard::new("bucephalus_variant_behavior_digests");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir).expect("run dir");
        let project_root = find_project_root(&run_dir);
        let bundle_root = ensure_test_agent_bundle(&project_root, "agent-current");
        let _ = bundle_root;
        let resolved = json!({
            "matrix": { "variants": [
                { "id": "base", "baseline": true, "config": {} },
                {
                    "id": "alt",
                    "config": { "temperature": 1.2 },
                    "overrides": {
                        "agent": {
                            "command": ["agentctl", "run", "--alternate"],
                            "env": { "PARALLEL_TOOLS": "1" }
                        }
                    }
                }
            ] },
            "trial_runtime": { "agent": { "command": harness_success_command() } }
        });
        let (variants, baseline_id) = resolve_variant_plan(&resolved).expect("variant plan");
        write_resolved_variants(&run_dir, &resolved, &baseline_id, &variants)
            .expect("write resolved variants");

        let manifest =
            load_json_file(&run_dir.join("resolved_variants.json")).expect("resolved variants");
        let manifest_variants = manifest
            .pointer("/variants")
            .and_then(Value::as_array)
            .expect("variant array");
        assert_eq!(manifest_variants.len(), variants.len());
        for (idx, variant) in variants.iter().enumerate() {
            let expected = resolved_variant_behavior_digest(&resolved, variant)
                .expect("behavior surface digest");
            assert_eq!(
                manifest_variants[idx]
                    .get("variant_digest")
                    .and_then(Value::as_str),
                Some(expected.as_str())
            );
        }
    }

    #[test]
    fn find_variant_by_id_finds_match() {
        let variants = vec![
            Variant {
                id: "baseline".to_string(),
                bindings: json!({}),
                args: vec![],
                env: BTreeMap::new(),
                image: None,
                runtime_overrides: None,
            },
            Variant {
                id: "treatment".to_string(),
                bindings: json!({"temp": 0.5}),
                args: vec![],
                env: BTreeMap::new(),
                image: None,
                runtime_overrides: None,
            },
        ];
        assert_eq!(
            find_variant_by_id(&variants, "treatment").unwrap().id,
            "treatment"
        );
    }

    #[test]
    fn find_variant_by_id_empty_id_returns_first() {
        let variants = vec![Variant {
            id: "baseline".to_string(),
            bindings: json!({}),
            args: vec![],
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        }];
        assert_eq!(find_variant_by_id(&variants, "").unwrap().id, "baseline");
    }

    #[test]
    fn find_variant_by_id_missing_fails() {
        let variants = vec![Variant {
            id: "baseline".to_string(),
            bindings: json!({}),
            args: vec![],
            env: BTreeMap::new(),
            image: None,
            runtime_overrides: None,
        }];
        assert!(find_variant_by_id(&variants, "missing").is_err());
    }

    #[test]
    fn resolve_variant_plan_single_baseline_only() {
        let spec = json!({"matrix": {"variants": [{"id": "baseline", "baseline": true, "config": {"x": 1}}]}});
        let (variants, baseline_id) = resolve_variant_plan(&spec).unwrap();
        assert_eq!(baseline_id, "baseline");
        assert_eq!(variants.len(), 1);
    }

    #[test]
    fn resolve_variant_plan_baseline_plus_treatments() {
        let spec = json!({"matrix": {"variants": [{"id": "baseline", "baseline": true, "config": {}}, {"id": "v1", "config": {"key": "a"}}, {"id": "v2", "config": {"key": "b"}}]}});
        let (variants, _) = resolve_variant_plan(&spec).unwrap();
        assert_eq!(variants.len(), 3);
    }

    #[test]
    fn resolve_variant_plan_variant_bindings_preserved() {
        let spec = json!({"matrix": {"variants": [{"id": "baseline", "baseline": true, "config": {"temp": 0.5}}, {"id": "v1", "config": {"temp": 0.9}}]}});
        let (variants, _) = resolve_variant_plan(&spec).unwrap();
        assert_eq!(variants[0].bindings["temp"], json!(0.5));
        assert_eq!(variants[1].bindings["temp"], json!(0.9));
    }

    #[test]
    fn resolve_variant_plan_empty_bindings_default_to_object() {
        let spec = json!({"matrix": {"variants": [{"id": "baseline", "baseline": true}]}});
        let (variants, _) = resolve_variant_plan(&spec).unwrap();
        assert!(variants[0].bindings.is_object());
    }


    #[test]
    fn operation_lease_is_stale_expired_returns_true() {
        let record = OperationLeaseRecord {
            schema_version: "v1".to_string(),
            operation_id: "op1".to_string(),
            op_type: "run".to_string(),
            owner_pid: 12345,
            owner_host: "localhost".to_string(),
            acquired_at: "2024-01-01T00:00:00Z".to_string(),
            expires_at: "2024-01-01T00:05:00Z".to_string(),
            stale_takeover_of: None,
        };
        let now = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:10:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(operation_lease_is_stale(&record, now));
    }

    #[test]
    fn operation_lease_is_stale_fresh_returns_false() {
        let record = OperationLeaseRecord {
            schema_version: "v1".to_string(),
            operation_id: "op1".to_string(),
            op_type: "run".to_string(),
            owner_pid: 12345,
            owner_host: "localhost".to_string(),
            acquired_at: "2024-01-01T00:00:00Z".to_string(),
            expires_at: "2024-01-01T00:10:00Z".to_string(),
            stale_takeover_of: None,
        };
        let now = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!operation_lease_is_stale(&record, now));
    }

    #[test]
    fn engine_lease_is_stale_expired_returns_true() {
        let record = EngineLeaseRecord {
            schema_version: "v1".to_string(),
            run_id: "run_001".to_string(),
            owner_id: "o1".to_string(),
            pid: 12345,
            hostname: "localhost".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            heartbeat_at: "2024-01-01T00:00:00Z".to_string(),
            expires_at: "2024-01-01T00:05:00Z".to_string(),
            epoch: 0,
        };
        let now = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:10:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(engine_lease_is_stale(&record, now));
    }

    #[test]
    fn engine_lease_is_stale_fresh_returns_false() {
        let record = EngineLeaseRecord {
            schema_version: "v1".to_string(),
            run_id: "run_001".to_string(),
            owner_id: "o1".to_string(),
            pid: 12345,
            hostname: "localhost".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            heartbeat_at: "2024-01-01T00:00:00Z".to_string(),
            expires_at: "2024-01-01T00:10:00Z".to_string(),
            epoch: 0,
        };
        let now = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!engine_lease_is_stale(&record, now));
    }

    #[test]
    fn write_run_control_running_status() {
        let root = TempDirGuard::new("run_ctrl_running");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        write_run_control(&run_dir, "run_001", "running", &[], None).unwrap();
        let loaded = load_json_file(&run_control_path(&run_dir)).unwrap();
        assert_eq!(loaded["status"], "running");
    }

    #[test]
    fn write_run_control_paused_with_label() {
        let root = TempDirGuard::new("run_ctrl_paused");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        let pause = RunControlPauseMetadata {
            label: "user_pause".to_string(),
            requested_at: "2024-01-01T00:00:00Z".to_string(),
            requested_by: None,
        };
        write_run_control(&run_dir, "run_001", "paused", &[], Some(&pause)).unwrap();
        let loaded = load_json_file(&run_control_path(&run_dir)).unwrap();
        assert_eq!(loaded["status"], "paused");
        assert_eq!(loaded["pause"]["label"], "user_pause");
    }

    #[test]
    fn write_run_control_active_trials_serialized() {
        let root = TempDirGuard::new("run_ctrl_active");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        let trials = vec![RunControlActiveTrial {
            trial_id: "trial_1".to_string(),
            worker_id: "worker_a".to_string(),
            schedule_idx: Some(0),
            variant_id: "baseline".to_string(),
            started_at: Some("2024-01-01T00:00:00Z".to_string()),
            control: None,
        }];
        write_run_control(&run_dir, "run_001", "running", &trials, None).unwrap();
        let loaded = load_json_file(&run_control_path(&run_dir)).unwrap();
        let active = loaded["active_trials"].as_object().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active["trial_1"]["trial_id"], "trial_1");
    }

    #[test]
    fn write_run_control_schema_version() {
        let root = TempDirGuard::new("run_ctrl_schema");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        write_run_control(&run_dir, "run_001", "running", &[], None).unwrap();
        assert_eq!(
            load_json_file(&run_control_path(&run_dir)).unwrap()["schema_version"],
            "run_control_v2"
        );
    }

    #[test]
    fn write_run_control_run_id_persisted() {
        let root = TempDirGuard::new("run_ctrl_id");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        write_run_control(&run_dir, "my_run_123", "running", &[], None).unwrap();
        assert_eq!(
            load_json_file(&run_control_path(&run_dir)).unwrap()["run_id"],
            "my_run_123"
        );
    }

    #[test]
    fn write_run_control_updated_at_present() {
        let root = TempDirGuard::new("run_ctrl_updated");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        write_run_control(&run_dir, "run_001", "running", &[], None).unwrap();
        assert!(
            !load_json_file(&run_control_path(&run_dir)).unwrap()["updated_at"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn write_run_control_persists_in_sqlite_without_runtime_file() {
        let root = TempDirGuard::new("run_ctrl_sqlite_only");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        write_run_control(&run_dir, "run_sqlite", "running", &[], None).unwrap();
        assert!(
            !run_control_path(&run_dir).exists(),
            "runtime/run_control.json should not be the canonical write target"
        );
        let store = BackingSqliteStore::open(&run_dir).expect("open sqlite store");
        let persisted = store
            .get_runtime_json(RUNTIME_KEY_RUN_CONTROL)
            .expect("load run control from sqlite")
            .expect("run control row should exist");
        assert_eq!(
            persisted.pointer("/run_id").and_then(Value::as_str),
            Some("run_sqlite")
        );
        assert_eq!(
            persisted.pointer("/status").and_then(Value::as_str),
            Some("running")
        );
    }

    #[test]
    fn append_durable_json_row_evidence_rows_route_to_sqlite_store() {
        let root = TempDirGuard::new("append_durable_json_row_sqlite_route");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        write_run_control(&run_dir, "run_evidence", "running", &[], None).unwrap();
        let evidence_path = run_dir.join("runtime").join("evidence_records.row.json");
        append_durable_json_row(
            &evidence_path,
            &json!({
                "run_id": "run_evidence",
                "schedule_idx": 0,
                "attempt": 1,
                "row_seq": 0,
                "slot_commit_id": "slot_x",
                "kind": "test"
            }),
        )
        .expect("append_durable_json_row should route into sqlite");
        let store = BackingSqliteStore::open(&run_dir).expect("open sqlite store");
        assert_eq!(store.row_count("evidence_rows").expect("row count"), 1);
    }

    #[test]
    fn evidence_attempt_objects_extracts_identity_and_refs() {
        let row = json!({
            "ids": {"run_id": "run_1", "trial_id": "trial_1"},
            "schedule_idx": 2,
            "attempt": 3,
            "evidence": {"trial_input_ref": "artifact://input", "trial_output_ref": "artifact://output"}
        });
        let objects = evidence_attempt_objects(&row)
            .expect("parse evidence objects")
            .expect("evidence objects");

        assert_eq!(
            (objects.run_id, objects.trial_id, objects.schedule_idx, objects.attempt),
            ("run_1", "trial_1", 2, 3)
        );
        assert_eq!(objects.refs.len(), 2);
        assert!(
            evidence_attempt_objects(&json!({
                "run_id": "run_1",
                "ids": {"trial_id": "trial_1"},
                "evidence": {"trial_output_ref": "artifact://output"}
            }))
                .expect("parse evidence objects")
                .is_none()
        );
    }

    #[test]
    fn chain_state_lineage_extracts_identity_refs_and_labels() {
        let row = json!({
            "ids": {"run_id": "run_1", "trial_id": "trial_1"},
            "chain_id": "chain_a",
            "step_index": 4,
            "snapshots": {"prev_ref": "artifact://prev", "post_ref": "artifact://post"},
            "diffs": {"incremental_ref": "artifact://diff", "patch_cumulative_ref": "artifact://patch"},
            "ext": {"workspace_ref": "artifact://workspace"},
            "checkpoint_labels": ["cp1"]
        });
        let lineage = chain_state_lineage(&row).expect("parse chain lineage");

        assert_eq!(
            (lineage.run_id, lineage.trial_id, lineage.chain_key, lineage.step_index),
            ("run_1", "trial_1", "chain_a", 4)
        );
        assert_eq!(lineage.pre_snapshot_ref, Some("artifact://prev"));
        assert_eq!(lineage.diff_incremental_ref, Some("artifact://diff"));
        assert_eq!(lineage.patch_cumulative_ref, Some("artifact://patch"));
        assert_eq!(lineage.workspace_ref, Some("artifact://workspace"));
        assert_eq!(lineage.checkpoint_labels, row.pointer("/checkpoint_labels"));
        assert_eq!(
            lineage
                .checkpoint_labels_json()
                .expect("serialize checkpoint labels"),
            "[\"cp1\"]"
        );
        let version_id = lineage.version_id();
        assert!(version_id.starts_with("sha256:"));
        assert_eq!(version_id.len(), "sha256:".len() + 64);
    }

    #[test]
    fn chain_state_lineage_requires_chain_identity() {
        let err = match chain_state_lineage(&json!({
            "run_id": "run_1",
            "ids": {"trial_id": "trial_1"},
            "step_index": 1
        })) {
            Ok(_) => panic!("missing chain id should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("missing /chain_id"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn append_durable_json_row_without_slot_identity_errors() {
        let root = TempDirGuard::new("append_durable_json_row_missing_identity");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        write_run_control(&run_dir, "run_missing_identity", "running", &[], None).unwrap();
        let evidence_path = run_dir.join("runtime").join("evidence_records.row.json");
        let err = append_durable_json_row(
            &evidence_path,
            &json!({
                "schema_version": "evidence_record_v1",
                "ids": {
                    "trial_id": "trial_1"
                }
            }),
        )
        .expect_err("append_durable_json_row must reject rows without sqlite slot identity");
        assert!(
            err.to_string().contains("missing row identity fields"),
            "unexpected error: {}",
            err
        );

        let store = BackingSqliteStore::open(&run_dir).expect("open sqlite store");
        assert_eq!(
            store.row_count("evidence_rows").expect("row count"),
            0,
            "rows without slot identity must not be routed into sqlite evidence table"
        );
        assert!(
            !evidence_path.exists(),
            "no durable row file should be created"
        );
    }

    #[test]
    fn append_uncommitted_json_row_spools_without_sqlite_slot_identity() {
        let root = TempDirGuard::new("append_uncommitted_json_row_spool");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        write_run_control(&run_dir, "run_spool", "running", &[], None).unwrap();
        let evidence_path = run_dir
            .join("runtime")
            .join("worker_payload")
            .join("trial_1")
            .join("evidence_records.jsonl");
        append_uncommitted_json_row(
            &evidence_path,
            &json!({
                "schema_version": "evidence_record_v1",
                "ids": {
                    "run_id": "run_spool",
                    "trial_id": "trial_1"
                }
            }),
        )
        .expect("uncommitted worker payload rows should spool before slot commit metadata exists");

        let rows = load_jsonl_value_rows(&evidence_path).expect("load spooled rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].pointer("/ids/trial_id").and_then(Value::as_str),
            Some("trial_1")
        );
        let store = BackingSqliteStore::open(&run_dir).expect("open sqlite store");
        assert_eq!(
            store.row_count("evidence_rows").expect("row count"),
            0,
            "uncommitted worker payload rows must not be routed into durable sqlite tables"
        );
    }

    #[test]
    fn write_trial_state_running() {
        let root = TempDirGuard::new("trial_state_running");
        write_trial_state(&root.path, "trial_1", "running", None, None, None).unwrap();
        let loaded = load_json_file(&trial_state_path(&root.path)).unwrap();
        assert_eq!(loaded["status"], "running");
        assert_eq!(loaded["trial_id"], "trial_1");
    }

    fn runtime_trial_attempt_state_fixture(phase: TrialPhase) -> TrialAttemptState {
        TrialAttemptState {
            key: TrialAttemptKey {
                schedule_idx: 0,
                attempt: 1,
            },
            slot: AttemptSlotRef {
                variant_id: "variant_a".to_string(),
                task_id: "task_a".to_string(),
                repl_idx: 0,
            },
            phase,
            paused_from_phase: None,
            fs: AttemptFsLayout {
                attempt_dir: "/tmp/attempt".to_string(),
                in_dir: "/tmp/in".to_string(),
                out_dir: "/tmp/out".to_string(),
                telemetry_mounts: Vec::new(),
                logs_dir: "/tmp/logs".to_string(),
            },
            task_sandbox: None,
            grading_sandbox: None,
            ephemerals: Vec::new(),
            ephemeral_networks: Vec::new(),
            agent_phase: None,
            grading_phase: None,
            mapping_phase: None,
            candidate_artifact: None,
            cleanup: Default::default(),
        }
    }

    fn runtime_trial_attempt_state_with_task_container(
        phase: TrialPhase,
        container_id: &str,
    ) -> TrialAttemptState {
        let mut state = runtime_trial_attempt_state_fixture(phase);
        state.task_sandbox = Some(TaskSandboxState {
            container_id: container_id.to_string(),
            image: "python:3.11-slim".to_string(),
            workdir: "/workspace/task".to_string(),
            platform: None,
            materialization: TaskMaterializationSpec {
                kind: TaskMaterializationKind::TaskImage,
                task_bundle_ref: None,
                platform: None,
            },
        });
        state
    }

    #[test]
    fn modal_sandbox_state_records_cleanup_for_persisted_runtime_ids() {
        let mut state =
            runtime_trial_attempt_state_with_task_container(TrialPhase::AgentFinished, "sb-task");
        state.grading_sandbox = Some(GradingSandboxState {
            container_id: "sb-grader".to_string(),
            strategy: GradingStrategy::Separate,
            workdir: "/workspace/task".to_string(),
        });

        record_modal_sandbox_cleanup(&mut state, "task");
        record_modal_sandbox_cleanup(&mut state, "grading");

        assert!(state.cleanup.containers.iter().any(|record| {
            record.role == "task"
                && record.container_id == "sb-task"
                && record.status == "removed"
                && record.error.is_none()
        }));
        assert!(state.cleanup.containers.iter().any(|record| {
            record.role == "grading"
                && record.container_id == "sb-grader"
                && record.status == "removed"
                && record.error.is_none()
        }));
    }

    #[test]
    fn modal_sandbox_cleanup_records_each_role_for_shared_sandbox_id() {
        let mut state =
            runtime_trial_attempt_state_with_task_container(TrialPhase::AgentFinished, "sb-shared");
        state.grading_sandbox = Some(GradingSandboxState {
            container_id: "sb-shared".to_string(),
            strategy: GradingStrategy::InTaskRuntime,
            workdir: "/workspace/task".to_string(),
        });

        record_modal_sandbox_cleanup(&mut state, "task");
        record_modal_sandbox_cleanup(&mut state, "grading");
        record_modal_sandbox_cleanup(&mut state, "task");

        let task_records = state
            .cleanup
            .containers
            .iter()
            .filter(|record| record.role == "task" && record.container_id == "sb-shared")
            .count();
        let grading_records = state
            .cleanup
            .containers
            .iter()
            .filter(|record| record.role == "grading" && record.container_id == "sb-shared")
            .count();
        assert_eq!(task_records, 1);
        assert_eq!(grading_records, 1);
    }

    #[test]
    fn modal_sandbox_cleanup_marks_persisted_container_rows_removed() {
        let (_root, run_dir) = create_run_dir("bucephalus_modal_cleanup_db", "run_1");
        let mut state =
            runtime_trial_attempt_state_with_task_container(TrialPhase::CommitPending, "sb-shared");
        state.grading_sandbox = Some(GradingSandboxState {
            container_id: "sb-shared".to_string(),
            strategy: GradingStrategy::InTaskRuntime,
            workdir: "/workspace/task".to_string(),
        });
        record_modal_sandbox_cleanup(&mut state, "task");
        record_modal_sandbox_cleanup(&mut state, "grading");

        let mut store = BackingSqliteStore::open(&run_dir).expect("open run store");
        store
            .upsert_trial_attempt_state("run_1", "trial_1", &state)
            .expect("persist modal runtime state");

        assert!(
            store
                .trial_attempt_container_ids("run_1", "trial_1")
                .expect("active container ids")
                .is_empty(),
            "modal sandbox ids must not remain active after successful launcher cleanup"
        );
    }

    #[test]
    fn trial_attempt_container_ids_include_ephemeral_sidecars() {
        let mut state =
            runtime_trial_attempt_state_with_task_container(TrialPhase::AgentRunning, "task_1");
        state.ephemerals.push(EphemeralSandboxState {
            id: "mcp-bash".to_string(),
            container_id: "sidecar_1".to_string(),
            image: "ghcr.io/acme/mcp-bash-server:v0.4".to_string(),
            lifecycle: "per-trial".to_string(),
        });

        let ids = trial::state::trial_attempt_container_ids(&state);

        assert_eq!(ids, vec!["task_1".to_string(), "sidecar_1".to_string()]);
    }

    #[test]
    fn trial_attempt_state_persists_ephemeral_networks() {
        let root = TempDirGuard::new("bucephalus_ephemeral_network_state");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        let mut state = runtime_trial_attempt_state_fixture(TrialPhase::AgentMaterializing);
        state.ephemeral_networks.push(EphemeralNetworkState {
            name: "bucephalus_ephemeral_test".to_string(),
            internal: true,
        });

        trial::state::write_trial_attempt_state(&trial_dir, &state).expect("write state");
        let loaded = trial::state::load_trial_attempt_state(&trial_dir).expect("load state");

        assert_eq!(loaded.state.ephemeral_networks.len(), 1);
        assert_eq!(
            loaded.state.ephemeral_networks[0].name,
            "bucephalus_ephemeral_test"
        );
        assert!(loaded.state.ephemeral_networks[0].internal);
    }

    #[test]
    fn modal_worker_id_loader_keeps_state_ids_when_sidecar_json_is_corrupt() {
        let (root, paths) = create_trial_paths_fixture("bucephalus_modal_corrupt_workers");
        let state =
            runtime_trial_attempt_state_with_task_container(TrialPhase::AgentRunning, "sb-state");
        persist_attempt_state(&paths.exp_dir, "run_1", &paths.trial_dir, &state)
            .expect("persist attempt state");
        let modal_dir = paths.trial_dir.join("modal");
        ensure_dir(&modal_dir).expect("modal dir");
        fs::write(modal_dir.join("runtime_workers.json"), "{not json")
            .expect("write corrupt runtime workers");

        let ids = load_modal_runtime_worker_ids_for_test(&paths.trial_dir)
            .expect("state ids should survive corrupt modal worker sidecar");
        assert_eq!(ids, vec!["sb-state".to_string()]);
        drop(root);
    }

    #[test]
    fn live_event_ingest_handle_drop_stops_background_thread() -> Result<()> {
        let (_root, run_dir) = create_run_dir("bucephalus_live_ingest_drop", "run_1");
        let events_path = run_dir.join("events.jsonl");
        let handle = spawn_live_event_ingest(LiveEventIngestRequest {
            run_dir: run_dir.clone(),
            events_path: events_path.clone(),
            run_id: "run_1".to_string(),
            trial_id: "trial_1".to_string(),
            schedule_idx: 0,
            variant_id: "baseline".to_string(),
            task_id: "task_1".to_string(),
            repl_idx: 0,
            attempt: 1,
        });

        drop(handle);
        fs::write(
            &events_path,
            "{\"event_type\":\"late_after_drop\",\"ts\":\"2026-01-01T00:00:00Z\"}\n",
        )?;
        std::thread::sleep(Duration::from_millis(700));

        let conn = rusqlite::Connection::open(account_sqlite_path_for_run(&run_dir)?)?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM event_rows", [], |row| row.get(0))?;
        assert_eq!(
            count, 0,
            "dropping the live ingest handle must not leave a detached polling thread"
        );
        Ok(())
    }

    #[test]
    fn engine_lease_guard_drop_wakes_heartbeat_thread_promptly() -> Result<()> {
        let (_root, run_dir) = create_run_dir("bucephalus_engine_lease_drop", "run_1");
        let guard = start_engine_lease_heartbeat_with_writer(&run_dir, "run_1", None)?;

        let started = Instant::now();
        drop(guard);

        assert!(
            started.elapsed() < Duration::from_millis(500),
            "dropping the engine lease guard should wake the heartbeat thread instead of waiting for the heartbeat interval"
        );
        Ok(())
    }

    #[test]
    fn concurrent_trial_attempt_state_upserts_persist_container_ids() -> Result<()> {
        let (_root, run_dir) = create_run_dir("bucephalus_concurrent_attempt_upserts", "run_1");
        let worker_count = 16;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(worker_count));
        let mut handles = Vec::new();

        for idx in 0..worker_count {
            let run_dir = run_dir.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || -> Result<()> {
                let trial_id = format!("trial_{idx}");
                let container_id = format!("container-{idx}");
                let mut state = runtime_trial_attempt_state_with_task_container(
                    TrialPhase::AgentRunning,
                    &container_id,
                );
                state.key.schedule_idx = idx;
                state.slot.repl_idx = idx;
                state.slot.task_id = format!("task_{idx}");
                barrier.wait();

                let mut store = BackingSqliteStore::open(&run_dir)?;
                store.upsert_trial_attempt_state("run_1", &trial_id, &state)?;
                Ok(())
            }));
        }

        for handle in handles {
            handle.join().expect("worker thread panicked")?;
        }

        let store = BackingSqliteStore::open(&run_dir).expect("open run store");
        for idx in 0..worker_count {
            let trial_id = format!("trial_{idx}");
            let expected = format!("container-{idx}");
            let ids = store
                .trial_attempt_container_ids("run_1", &trial_id)
                .expect("container ids");
            assert_eq!(ids, vec![expected]);
        }
        Ok(())
    }

    #[test]
    fn persist_attempt_state_writes_runtime_file_when_sqlite_unavailable() {
        let root = TempDirGuard::new("bucephalus_attempt_state_db_unavailable");
        let run_dir = root.path.join("run_dir_is_a_file");
        fs::write(&run_dir, "not a directory").expect("write file at run dir path");
        let trial_dir = root.path.join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        let state = runtime_trial_attempt_state_with_task_container(
            TrialPhase::AgentRunning,
            "container-before-db-error",
        );

        let err = persist_attempt_state(&run_dir, "run_1", &trial_dir, &state)
            .expect_err("sqlite persistence should fail");
        assert!(
            err.to_string()
                .contains("persist trial runtime state in runtime store"),
            "unexpected error: {err}"
        );

        let persisted =
            crate::trial::state::load_trial_attempt_state(&trial_dir).expect("runtime state file");
        let persisted_task = persisted
            .state
            .task_sandbox
            .as_ref()
            .expect("persisted task sandbox");
        assert_eq!(persisted_task.container_id, "container-before-db-error");
        assert_eq!(
            crate::trial::state::load_trial_attempt_container_ids(&trial_dir)
                .expect("runtime container ids"),
            vec!["container-before-db-error".to_string()]
        );
    }

    #[test]
    fn runtime_container_lookup_uses_runtime_file_without_sqlite() {
        let (_root, run_dir) = create_run_dir("bucephalus_runtime_lookup_file_first", "run_1");
        let trial_dir = run_dir.join("trials").join("trial_1");
        ensure_dir(&trial_dir).expect("trial dir");
        trial::state::write_trial_attempt_state(
            &trial_dir,
            &runtime_trial_attempt_state_with_task_container(
                TrialPhase::AgentRunning,
                "container-from-runtime-file",
            ),
        )
        .expect("runtime state file");

        let handles = runtime_trial_container_handles("run_1", "trial_1", &trial_dir)
            .expect("runtime handles from file");
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].container_id, "container-from-runtime-file");
    }

    #[test]
    fn persist_attempt_state_uses_installed_run_store_writer() -> Result<()> {
        let (_root, run_dir) = create_run_dir("bucephalus_attempt_state_writer_scope", "run_1");
        let (_guard, writer) = RunStoreWriterGuard::start(&run_dir, "run_1")?;
        let _scope = crate::trial::execution::RunStoreWriterScope::install(writer);
        let worker_count = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(worker_count));
        let mut handles = Vec::new();

        for idx in 0..worker_count {
            let run_dir = run_dir.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || -> Result<()> {
                let trial_dir = run_dir.join("trials").join(format!("trial_{idx}"));
                ensure_dir(&trial_dir)?;
                let mut state = runtime_trial_attempt_state_with_task_container(
                    TrialPhase::AgentRunning,
                    &format!("writer-container-{idx}"),
                );
                state.key.schedule_idx = idx;
                state.slot.task_id = format!("task_{idx}");
                barrier.wait();
                persist_attempt_state(&run_dir, "run_1", &trial_dir, &state)?;
                Ok(())
            }));
        }

        for handle in handles {
            handle.join().expect("writer worker panicked")?;
        }

        let store = BackingSqliteStore::open(&run_dir)?;
        for idx in 0..worker_count {
            let ids = store.trial_attempt_container_ids("run_1", &format!("trial_{idx}"))?;
            assert_eq!(ids, vec![format!("writer-container-{idx}")]);
        }
        Ok(())
    }

    #[test]
    fn run_store_writer_scope_does_not_cross_run_directories_with_same_run_id() -> Result<()> {
        let (_writer_root, writer_run_dir) =
            create_run_dir("bucephalus_writer_scope_primary", "run_1");
        let (_other_root, other_run_dir) =
            create_run_dir("bucephalus_writer_scope_other", "run_1");
        let (_guard, writer) = RunStoreWriterGuard::start(&writer_run_dir, "run_1")?;
        let _scope = crate::trial::execution::RunStoreWriterScope::install(writer);

        let trial_dir = other_run_dir.join("trials").join("trial_1");
        ensure_dir(&trial_dir)?;
        let state = runtime_trial_attempt_state_with_task_container(
            TrialPhase::AgentRunning,
            "other-run-container",
        );
        persist_attempt_state(&other_run_dir, "run_1", &trial_dir, &state)?;

        let other_store = BackingSqliteStore::open(&other_run_dir)?;
        assert_eq!(
            other_store.trial_attempt_container_ids("run_1", "trial_1")?,
            vec!["other-run-container".to_string()]
        );

        let writer_store = BackingSqliteStore::open(&writer_run_dir)?;
        assert!(
            writer_store
                .trial_attempt_container_ids("run_1", "trial_1")?
                .is_empty(),
            "writer scoped to a different run directory must not receive this attempt"
        );
        Ok(())
    }

    #[test]
    fn trial_runtime_state_reconciles_abandoned_and_committed() {
        let root = TempDirGuard::new("trial_runtime_state_reconcile");
        trial::state::write_trial_attempt_state(
            &root.path,
            &runtime_trial_attempt_state_fixture(TrialPhase::AgentRunning),
        )
        .expect("write runtime state");

        trial::state::reconcile_trial_attempt_as_abandoned(&root.path)
            .expect("reconcile abandoned");
        let abandoned = trial::state::load_trial_attempt_state(&root.path).expect("load abandoned");
        assert_eq!(abandoned.state.phase, TrialPhase::Abandoned);

        trial::state::reconcile_trial_attempt_as_committed(&root.path)
            .expect("reconcile committed");
        let committed = trial::state::load_trial_attempt_state(&root.path).expect("load committed");
        assert_eq!(committed.state.phase, TrialPhase::Committed);

        trial::state::reconcile_trial_attempt_as_abandoned(&root.path)
            .expect("reconcile abandoned after commit");
        let still_committed =
            trial::state::load_trial_attempt_state(&root.path).expect("load after commit");
        assert_eq!(still_committed.state.phase, TrialPhase::Committed);
    }

    #[test]
    fn trial_runtime_state_preserves_paused_and_killed_from_abandon_reconcile() {
        let root = TempDirGuard::new("trial_runtime_state_terminal_reconcile");

        let mut paused_state = runtime_trial_attempt_state_fixture(TrialPhase::Paused);
        paused_state.paused_from_phase = Some(TrialPhase::AgentRunning);
        trial::state::write_trial_attempt_state(&root.path, &paused_state)
            .expect("write paused runtime state");
        trial::state::reconcile_trial_attempt_as_abandoned(&root.path)
            .expect("abandon paused reconcile");
        let paused = trial::state::load_trial_attempt_state(&root.path).expect("load paused");
        assert_eq!(paused.state.phase, TrialPhase::Paused);

        trial::state::write_trial_attempt_state(
            &root.path,
            &runtime_trial_attempt_state_fixture(TrialPhase::Killed),
        )
        .expect("write killed runtime state");
        trial::state::reconcile_trial_attempt_as_abandoned(&root.path)
            .expect("abandon killed reconcile");
        let killed = trial::state::load_trial_attempt_state(&root.path).expect("load killed");
        assert_eq!(killed.state.phase, TrialPhase::Killed);
    }

    #[test]
    fn write_trial_state_paused_with_label() {
        let root = TempDirGuard::new("trial_state_paused");
        write_trial_state(
            &root.path,
            "trial_1",
            "paused",
            Some("checkpoint_pause"),
            None,
            None,
        )
        .unwrap();
        let loaded = load_json_file(&trial_state_path(&root.path)).unwrap();
        assert_eq!(loaded["pause_label"], "checkpoint_pause");
    }

    #[test]
    fn write_trial_state_completed() {
        let root = TempDirGuard::new("trial_state_completed");
        write_trial_state(&root.path, "trial_1", "completed", None, None, None).unwrap();
        assert_eq!(
            load_json_file(&trial_state_path(&root.path)).unwrap()["status"],
            "completed"
        );
    }

    #[test]
    fn write_trial_state_failed_with_exit_reason() {
        let root = TempDirGuard::new("trial_state_failed");
        write_trial_state(&root.path, "trial_1", "failed", None, None, Some("timeout")).unwrap();
        let loaded = load_json_file(&trial_state_path(&root.path)).unwrap();
        assert_eq!(loaded["exit_reason"], "timeout");
    }

    #[test]
    fn write_trial_state_schema_version() {
        let root = TempDirGuard::new("trial_state_schema");
        write_trial_state(&root.path, "trial_1", "running", None, None, None).unwrap();
        assert_eq!(
            load_json_file(&trial_state_path(&root.path)).unwrap()["schema_version"],
            "trial_state_v1"
        );
    }

    #[test]
    fn write_trial_state_with_checkpoint() {
        let root = TempDirGuard::new("trial_state_cp");
        write_trial_state(
            &root.path,
            "trial_1",
            "paused",
            None,
            Some("checkpoint_5"),
            None,
        )
        .unwrap();
        assert_eq!(
            load_json_file(&trial_state_path(&root.path)).unwrap()["checkpoint_selected"],
            "checkpoint_5"
        );
    }

    #[test]
    fn run_control_guard_marks_failed_on_drop() {
        let root = TempDirGuard::new("guard_drop_fail");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        {
            let _guard = RunControlGuard::new(&run_dir, "run_001");
        }
        assert_eq!(
            load_json_file(&run_control_path(&run_dir)).unwrap()["status"],
            "failed"
        );
    }

    #[test]
    fn run_control_guard_complete_prevents_drop_fail() {
        let root = TempDirGuard::new("guard_complete");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        {
            let mut guard = RunControlGuard::new(&run_dir, "run_001");
            guard.complete("completed").unwrap();
        }
        assert_eq!(
            load_json_file(&run_control_path(&run_dir)).unwrap()["status"],
            "completed"
        );
    }

    #[test]
    fn trial_state_guard_marks_aborted_on_drop() {
        let root = TempDirGuard::new("trial_guard_drop");
        {
            let _guard = TrialStateGuard::new(&root.path, "trial_1");
        }
        let loaded = load_json_file(&trial_state_path(&root.path)).unwrap();
        assert_eq!(loaded["status"], "failed");
        assert_eq!(loaded["exit_reason"], "aborted");
    }

    #[test]
    fn trial_state_guard_complete_prevents_drop_abort() {
        let root = TempDirGuard::new("trial_guard_complete");
        {
            let mut guard = TrialStateGuard::new(&root.path, "trial_1");
            guard.complete("completed", None).unwrap();
        }
        assert_eq!(
            load_json_file(&trial_state_path(&root.path)).unwrap()["status"],
            "completed"
        );
    }

    #[test]
    fn create_unique_run_dir_creates_expected_structure() {
        let root = TempDirGuard::new("unique_run");
        let (run_id, run_dir) = create_unique_run_dir(&root.path).unwrap();
        assert!(run_dir.exists());
        assert!(run_id.starts_with("run_"));
    }

    #[test]
    fn create_unique_run_dir_unique_ids() {
        let root = TempDirGuard::new("unique_run_ids");
        let (id_a, _) = create_unique_run_dir(&root.path).unwrap();
        let (id_b, _) = create_unique_run_dir(&root.path).unwrap();
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn run_control_path_correct() {
        assert_eq!(
            run_control_path(&PathBuf::from("/tmp/run_001")),
            PathBuf::from("/tmp/run_001/runtime/run_control.json")
        );
    }

    #[test]
    fn write_run_session_state_roundtrip() {
        let root = TempDirGuard::new("session_roundtrip");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        let behavior = RunBehavior {
            network_mode_override: Some("bridge".to_string()),
            require_network_none: false,
            smoke_test: false,
        };
        let execution = RunExecutionOptions {
            materialize: Some(MaterializationMode::Full),
            ..container_execution()
        };
        write_run_session_state(&run_dir, "run_001", &behavior, &execution).unwrap();
        let loaded = load_run_session_state(&run_dir).unwrap();
        assert_eq!(loaded.run_id, "run_001");
        assert_eq!(loaded.schema_version, "run_session_state_v1");
    }

    #[test]
    fn run_session_state_preserves_behavior() {
        let root = TempDirGuard::new("session_behavior");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        let behavior = RunBehavior {
            network_mode_override: Some("host".to_string()),
            require_network_none: true,
            smoke_test: false,
        };
        write_run_session_state(
            &run_dir,
            "run_002",
            &behavior,
            &container_execution(),
        )
        .unwrap();
        let loaded = load_run_session_state(&run_dir).unwrap();
        assert_eq!(
            loaded.behavior.network_mode_override,
            Some("host".to_string())
        );
        assert!(loaded.behavior.require_network_none);
    }

    #[test]
    fn write_run_session_state_requires_explicit_executor() {
        let root = TempDirGuard::new("session_executor_required");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        let err = write_run_session_state(
            &run_dir,
            "run_missing_executor",
            &RunBehavior::default(),
            &RunExecutionOptions {
                executor: None,
                materialize: Some(MaterializationMode::OutputsOnly),
                ..container_execution()
            },
        )
        .expect_err("run session state must not persist an implicit executor");
        assert!(
            err.to_string()
                .contains("run_session_state.execution.executor is required"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn run_session_state_preserves_execution_options() {
        let root = TempDirGuard::new("session_execution");
        let run_dir = root.path.join("run");
        ensure_dir(&run_dir.join("runtime")).unwrap();
        let behavior = RunBehavior {
            network_mode_override: None,
            require_network_none: false,
            smoke_test: false,
        };
        let execution = RunExecutionOptions {
            materialize: Some(MaterializationMode::MetadataOnly),
            ..container_execution()
        };
        write_run_session_state(&run_dir, "run_003", &behavior, &execution).unwrap();
        assert_eq!(
            load_run_session_state(&run_dir).unwrap().execution.executor,
            Some(ExecutorKind::LocalDocker)
        );
    }

    #[test]
    fn sqlite_schema_bootstrap_records_experiment_bundle_migration() {
        let (_root, run_dir) = create_run_dir("bucephalus_schema_migration", "run_1");
        let _store = BackingSqliteStore::open(&run_dir).expect("open sqlite store");
        let conn = rusqlite::Connection::open(account_sqlite_path_for_run(&run_dir).unwrap())
            .expect("open account sqlite");

        let migration_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE migration_id='20260516_experiment_bundles'",
                [],
                |row| row.get(0),
            )
            .expect("migration row count");
        assert_eq!(migration_count, 1);

        let bundle_table_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='experiment_bundles'",
                [],
                |row| row.get(0),
            )
            .expect("bundle table count");
        assert_eq!(bundle_table_count, 1);
    }

    #[test]
    fn run_control_active_trials_parses_v2() {
        let control = json!({"schema_version": "run_control_v2", "active_trials": {"trial_1": {"trial_id": "trial_1", "worker_id": "worker_a", "schedule_idx": 0, "variant_id": "base", "control": null}}});
        let trials = run_control_active_trials(&control).unwrap();
        assert_eq!(trials.len(), 1);
        assert_eq!(trials[0].trial_id, "trial_1");
    }

    #[test]
    fn run_control_active_trials_empty_when_none() {
        assert!(run_control_active_trials(&json!({"schema_version": "run_control_v2"}))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn run_control_active_trials_rejects_missing_worker_id() {
        let control = json!({"schema_version": "run_control_v2", "active_trials": {"trial_1": {"trial_id": "trial_1", "schedule_idx": 0, "variant_id": "base"}}});
        let err = run_control_active_trials(&control).unwrap_err();
        assert!(err.to_string().contains("trial_1 missing worker_id"));
    }

    #[test]
    fn run_control_active_trials_rejects_missing_variant_id() {
        let control = json!({"schema_version": "run_control_v2", "active_trials": {"trial_1": {"trial_id": "trial_1", "worker_id": "worker_a", "schedule_idx": 0}}});
        let err = run_control_active_trials(&control).unwrap_err();
        assert!(err.to_string().contains("trial_1 missing variant_id"));
    }

    #[test]
    fn atomic_write_bytes_creates_file() {
        let root = TempDirGuard::new("aw_bytes_create");
        let path = root.path.join("test.bin");
        atomic_write_bytes(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn atomic_write_bytes_overwrites_existing() {
        let root = TempDirGuard::new("aw_bytes_overwrite");
        let path = root.path.join("test.bin");
        atomic_write_bytes(&path, b"old").unwrap();
        atomic_write_bytes(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn atomic_write_bytes_creates_parent_dirs() {
        let root = TempDirGuard::new("aw_parent");
        let path = root.path.join("nested").join("deep").join("file.txt");
        atomic_write_bytes(&path, b"content").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"content");
    }

    #[test]
    fn atomic_write_json_pretty_roundtrip() {
        let root = TempDirGuard::new("aw_json_roundtrip");
        let path = root.path.join("test.json");
        let value = json!({"key": "value", "number": 42});
        atomic_write_json_pretty(&path, &value).unwrap();
        assert_eq!(load_json_file(&path).unwrap(), value);
    }

    #[test]
    fn load_json_file_missing_fails() {
        let root = TempDirGuard::new("load_json_missing");
        assert!(load_json_file(&root.path.join("missing.json")).is_err());
    }

    #[test]
    fn load_json_file_invalid_json_fails() {
        let root = TempDirGuard::new("load_json_invalid");
        let path = root.path.join("bad.json");
        fs::write(&path, "not valid json {{{").unwrap();
        assert!(load_json_file(&path).is_err());
    }

    #[test]
    fn load_json_file_valid_roundtrip() {
        let root = TempDirGuard::new("load_json_valid");
        let path = root.path.join("data.json");
        let value = json!({"array": [1, 2, 3], "nested": {"key": "value"}});
        atomic_write_json_pretty(&path, &value).unwrap();
        assert_eq!(load_json_file(&path).unwrap(), value);
    }

    #[test]
    fn set_json_pointer_value_creates_nested() {
        let mut root = json!({});
        set_json_pointer_value(&mut root, "/a/b/c", json!("deep")).unwrap();
        assert_eq!(root["a"]["b"]["c"], json!("deep"));
    }

    #[test]
    fn set_json_pointer_value_overwrites_existing() {
        let mut root = json!({"a": {"b": "old"}});
        set_json_pointer_value(&mut root, "/a/b", json!("new")).unwrap();
        assert_eq!(root["a"]["b"], json!("new"));
    }

    #[test]
    fn set_json_pointer_value_replaces_root() {
        let mut root = json!({"old": "data"});
        set_json_pointer_value(&mut root, "", json!("replaced")).unwrap();
        assert_eq!(root, json!("replaced"));
    }

    #[test]
    fn set_json_pointer_value_invalid_pointer_fails() {
        assert!(set_json_pointer_value(&mut json!({}), "no_slash", json!(1)).is_err());
    }

    #[test]
    fn set_json_pointer_value_deep_nested_creates_intermediates() {
        let mut root = json!({});
        set_json_pointer_value(&mut root, "/a/b/c/d", json!(42)).unwrap();
        assert_eq!(root["a"]["b"]["c"]["d"], json!(42));
    }

    #[test]
    fn copy_path_into_package_file() {
        let root = TempDirGuard::new("copy_pkg_file");
        let src = root.path.join("source.txt");
        let dst = root.path.join("dest").join("source.txt");
        fs::write(&src, "content").unwrap();
        copy_path_into_package(&src, &dst).unwrap();
        assert_eq!(fs::read_to_string(&dst).unwrap(), "content");
    }

    #[test]
    fn copy_path_into_package_dir() {
        let root = TempDirGuard::new("copy_pkg_dir");
        let src_dir = root.path.join("source_dir");
        ensure_dir(&src_dir).unwrap();
        fs::write(src_dir.join("file.txt"), "content").unwrap();
        let dst_dir = root.path.join("dest_dir");
        copy_path_into_package(&src_dir, &dst_dir).unwrap();
        assert_eq!(
            fs::read_to_string(dst_dir.join("file.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn stage_source_into_package_absolute_path() {
        let root = TempDirGuard::new("stage_abs");
        let exp_dir = root.path.join("exp");
        let pkg_dir = root.path.join("pkg");
        ensure_dir(&exp_dir).unwrap();
        ensure_dir(&pkg_dir).unwrap();
        let src = root.path.join("artifact.tar");
        fs::write(&src, "data").unwrap();
        let mut copies = BTreeMap::new();
        let mut counter = 0usize;
        let rel = stage_source_into_package(
            src.to_str().unwrap(),
            &exp_dir,
            &pkg_dir,
            "agent_builds",
            &mut copies,
            &mut counter,
        )
        .unwrap();
        assert!(!rel.is_empty());
        assert_eq!(counter, 1);
    }

    #[test]
    fn stage_source_into_package_relative_path() {
        let root = TempDirGuard::new("stage_rel");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).unwrap();
        fs::write(exp_dir.join("agent.tar"), "data").unwrap();
        let pkg_dir = root.path.join("pkg");
        ensure_dir(&pkg_dir).unwrap();
        let mut copies = BTreeMap::new();
        let mut counter = 0usize;
        assert!(!stage_source_into_package(
            "agent.tar",
            &exp_dir,
            &pkg_dir,
            "agent_builds",
            &mut copies,
            &mut counter
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn stage_source_into_package_missing_source_fails() {
        let root = TempDirGuard::new("stage_missing");
        let exp_dir = root.path.join("exp");
        let pkg_dir = root.path.join("pkg");
        ensure_dir(&exp_dir).unwrap();
        ensure_dir(&pkg_dir).unwrap();
        let mut copies = BTreeMap::new();
        let mut counter = 0usize;
        assert!(stage_source_into_package(
            "nonexistent.tar",
            &exp_dir,
            &pkg_dir,
            "agent_builds",
            &mut copies,
            &mut counter
        )
        .is_err());
    }

    #[test]
    fn stage_source_into_package_requires_named_source() {
        let root = TempDirGuard::new("stage_unnamed");
        let exp_dir = root.path.join("exp");
        let pkg_dir = root.path.join("pkg");
        ensure_dir(&exp_dir).unwrap();
        ensure_dir(&pkg_dir).unwrap();
        let mut copies = BTreeMap::new();
        let mut counter = 0usize;
        let err = stage_source_into_package(
            "/",
            &exp_dir,
            &pkg_dir,
            "agent_builds",
            &mut copies,
            &mut counter,
        )
        .unwrap_err();
        assert!(err.to_string().contains("must have a file name"));
        assert_eq!(counter, 0);
    }

    #[test]
    fn stage_source_into_package_directory_copied() {
        let root = TempDirGuard::new("stage_dir");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).unwrap();
        let src_dir = exp_dir.join("agent_dir");
        ensure_dir(&src_dir).unwrap();
        fs::write(src_dir.join("main.py"), "print('hello')").unwrap();
        let pkg_dir = root.path.join("pkg");
        ensure_dir(&pkg_dir).unwrap();
        let mut copies = BTreeMap::new();
        let mut counter = 0usize;
        let rel = stage_source_into_package(
            "agent_dir",
            &exp_dir,
            &pkg_dir,
            "agent_builds",
            &mut copies,
            &mut counter,
        )
        .unwrap();
        assert!(pkg_dir.join(rel.trim_start_matches('/')).exists());
    }

    #[test]
    fn stage_source_deduplicates_same_source() {
        let root = TempDirGuard::new("stage_dedup");
        let exp_dir = root.path.join("exp");
        ensure_dir(&exp_dir).unwrap();
        fs::write(exp_dir.join("artifact.tar"), "data").unwrap();
        let pkg_dir = root.path.join("pkg");
        ensure_dir(&pkg_dir).unwrap();
        let mut copies = BTreeMap::new();
        let mut counter = 0usize;
        let rel1 = stage_source_into_package(
            "artifact.tar",
            &exp_dir,
            &pkg_dir,
            "agent_builds",
            &mut copies,
            &mut counter,
        )
        .unwrap();
        let rel2 = stage_source_into_package(
            "artifact.tar",
            &exp_dir,
            &pkg_dir,
            "agent_builds",
            &mut copies,
            &mut counter,
        )
        .unwrap();
        assert_eq!(rel1, rel2);
        assert_eq!(counter, 1);
    }

    #[test]
    fn resolve_variant_plan_matrix_baseline_plus_two_treatments() {
        let exp = json!({
            "matrix": { "variants": [
                { "id": "ctrl", "baseline": true, "config": { "lr": 0.01 } },
                { "id": "fast", "config": { "lr": 0.1 } },
                { "id": "slow", "config": { "lr": 0.001 } }
            ] }
        });
        let (variants, baseline_id) = resolve_variant_plan(&exp).unwrap();
        assert_eq!(baseline_id, "ctrl");
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0].id, "ctrl");
        assert_eq!(variants[1].id, "fast");
        assert_eq!(variants[2].id, "slow");
        assert_eq!(variants[1].bindings["lr"], json!(0.1));
    }

    #[test]
    fn resolve_variant_plan_matrix_single_variant_returns_baseline_only() {
        let exp = json!({
            "matrix": { "variants": [{ "id": "base", "baseline": true, "config": { "x": 1 } }] }
        });
        let (variants, baseline_id) = resolve_variant_plan(&exp).unwrap();
        assert_eq!(baseline_id, "base");
        assert_eq!(variants.len(), 1);
    }

    #[test]
    fn resolve_variant_plan_matrix_config_defaults_to_empty_object() {
        let exp = json!({
            "matrix": { "variants": [
                { "id": "b", "baseline": true },
                { "id": "v1" }
            ] }
        });
        let (variants, _) = resolve_variant_plan(&exp).unwrap();
        assert!(variants[1].bindings.is_object());
        assert_eq!(variants[1].bindings.as_object().unwrap().len(), 0);
    }

    #[test]
    fn resolve_variant_plan_legacy_variant_array_bindings_fails() {
        let exp = json!({
            "baseline": { "variant_id": "b" },
            "variant_plan": [{ "variant_id": "v1", "bindings": [1, 2] }]
        });
        assert!(resolve_variant_plan(&exp).is_err());
    }

    #[test]
    fn resolve_variant_plan_missing_baseline_variant_id_fails() {
        let exp = json!({ "matrix": { "variants": [{ "baseline": true, "config": {} }] } });
        assert!(resolve_variant_plan(&exp).is_err());
    }

    #[test]
    fn resolve_variant_plan_matrix_overrides_attached() {
        let exp = json!({
            "matrix": { "variants": [
                { "id": "b", "baseline": true, "overrides": { "agent": { "timeout_ms": 5000 } } }
            ] }
        });
        let (variants, _) = resolve_variant_plan(&exp).unwrap();
        assert!(variants[0].runtime_overrides.is_some());
        assert_eq!(
            variants[0]
                .runtime_overrides
                .as_ref()
                .unwrap()
                .pointer("/agent/timeout_ms"),
            Some(&json!(5000))
        );
    }

    #[test]
    fn resolve_variant_plan_legacy_runtime_overrides_must_be_object() {
        let exp = json!({"matrix": {"variants": [{"id": "b", "baseline": true, "overrides": "bad"}]}});
        assert!(resolve_variant_plan(&exp).is_err());
    }

    #[test]
    fn resolve_variant_plan_variant_without_id_fails() {
        let exp = json!({
            "matrix": { "variants": [
                { "id": "b", "baseline": true },
                { "config": {} }
            ] }
        });
        assert!(resolve_variant_plan(&exp).is_err());
    }

    #[test]
    fn resolve_variant_plan_accepts_config() {
        let exp = json!({
            "matrix": { "variants": [
                { "id": "b", "baseline": true },
                { "id": "v1", "config": { "k": "v" } }
            ] }
        });
        let (variants, _) = resolve_variant_plan(&exp).unwrap();
        assert_eq!(variants[1].bindings["k"], json!("v"));
    }

    #[test]
    fn resolve_variant_plan_matrix_image_field_is_rejected() {
        let exp = json!({
            "matrix": { "variants": [{ "id": "b", "baseline": true, "image": "custom:latest" }] }
        });
        let err = resolve_variant_plan(&exp).expect_err("image is not a v1 variant field");
        assert!(err.to_string().contains("/matrix/variants[0]/image"));
    }


    #[test]
    fn build_runtime_contract_env_projects_contract_keys_without_task_image_from_agent_input() {
        let input = json!({
            "ids": { "trial_id": "t1", "variant_id": "v1", "case_id": "task_a", "repl_idx": 2 },
            "case": { "id": "task_a" },
            "environment": { "image": "poison/from-agent-input:latest" },
            "policy": { "timeout_ms": 30000 },
            "ext": {
                "task_boundary": {
                    "environment": { "image": "myimg:1" },
                    "workspace": {
                        "mode": "scratch",
                        "base": { "kind": "empty" },
                        "overlays": [],
                        "aux_mounts": []
                    },
                    "dependencies": {},
                    "limits": {}
                }
            }
        });
        let io = prepared_trial_io_fixture_with_contract_paths(
            "/bucephalus/in/trial_input.json",
            "/bucephalus/out/result.json",
            "/bucephalus/out/mapped_grader_output.json",
            "/bucephalus/out/trajectory.jsonl",
        );
        let env = build_runtime_contract_env("run_1", &input, &io, None, Some(30000))
            .expect("runtime env");
        assert_eq!(env.get(BUCEPHALUS_ENV_RUN_ID).unwrap(), "run_1");
        assert_eq!(env.get(BUCEPHALUS_ENV_TRIAL_ID).unwrap(), "t1");
        assert_eq!(env.get(BUCEPHALUS_ENV_VARIANT_ID).unwrap(), "v1");
        assert_eq!(env.get(BUCEPHALUS_ENV_CASE_ID).unwrap(), "task_a");
        assert_eq!(env.get(BUCEPHALUS_ENV_TASK_ID).unwrap(), "task_a");
        assert_eq!(env.get(BUCEPHALUS_ENV_REPL_IDX).unwrap(), "2");
        assert_eq!(env.get(BUCEPHALUS_ENV_TIMEOUT_MS).unwrap(), "30000");
        assert_eq!(
            env.get(BUCEPHALUS_ENV_TRIAL_INPUT_PATH).unwrap(),
            "/bucephalus/in/trial_input.json"
        );
        assert!(
            !env.contains_key(BUCEPHALUS_ENV_CASE_IMAGE),
            "case image must come from PreparedTaskEnvironment, not agent-facing input"
        );
        assert!(
            !env.contains_key(BUCEPHALUS_ENV_TASK_IMAGE),
            "task image must come from PreparedTaskEnvironment, not agent-facing input"
        );
    }

    #[test]
    fn build_runtime_contract_env_no_timeout_omits_key() {
        let input =
            json!({ "ids": { "trial_id": "t1", "variant_id": "v1", "case_id": "task_a", "repl_idx": 0 } });
        let io = prepared_trial_io_fixture_with_contract_paths(
            "/in/trial_input.json",
            "/out/result.json",
            "/out/mapped_grader_output.json",
            "/out/trajectory.jsonl",
        );
        let env = build_runtime_contract_env("run_1", &input, &io, None, None)
            .expect("runtime env");
        assert!(!env.contains_key(BUCEPHALUS_ENV_TIMEOUT_MS));
    }

    #[test]
    fn build_runtime_contract_env_no_task_image_omits_key() {
        let input =
            json!({ "ids": { "trial_id": "t1", "variant_id": "v1", "case_id": "task_a", "repl_idx": 0 }, "case": {} });
        let io = prepared_trial_io_fixture_with_contract_paths(
            "/in/trial_input.json",
            "/out/result.json",
            "/out/mapped_grader_output.json",
            "/out/trajectory.jsonl",
        );
        let env = build_runtime_contract_env("run_1", &input, &io, None, None)
            .expect("runtime env");
        assert!(!env.contains_key(BUCEPHALUS_ENV_CASE_IMAGE));
        assert!(!env.contains_key(BUCEPHALUS_ENV_TASK_IMAGE));
    }


    #[test]
    fn resolve_trial_timeout_ms_reads_policy_field() {
        let input = json!({ "policy": { "timeout_ms": 60000 } });
        assert_eq!(resolve_trial_timeout_ms(&input), Some(60000));
    }

    #[test]
    fn resolve_trial_timeout_ms_reads_runtime_time_limit_field() {
        let input = json!({ "runtime": { "time_limit_ms": 1200000 } });
        assert_eq!(resolve_trial_timeout_ms(&input), Some(1200000));
    }

    #[test]
    fn resolve_trial_timeout_ms_both_missing_returns_none() {
        let input = json!({});
        assert_eq!(resolve_trial_timeout_ms(&input), None);
    }


    fn write_empty_runtime_staging_manifest(package_dir: &Path) -> String {
        let path = package_dir.join(STAGING_MANIFEST_FILE);
        fs::write(
            &path,
            serde_json::to_string(&json!({
                "schema_version": STAGING_MANIFEST_SCHEMA_VERSION,
                "variants": {}
            }))
            .expect("staging manifest json"),
        )
        .expect("write staging manifest");
        sha256_file(&path).expect("staging manifest digest")
    }

    fn write_runtime_staging_manifest(package_dir: &Path, payload: &Value) {
        fs::write(
            package_dir.join(STAGING_MANIFEST_FILE),
            serde_json::to_string(payload).expect("staging manifest json"),
        )
        .expect("write staging manifest");
    }

    fn write_sealed_package_metadata(
        package_dir: &Path,
        files: Value,
        manifest_resolved_experiment: Value,
    ) -> String {
        let package_digest = canonical_json_digest(&files);
        fs::write(
            package_dir.join("checksums.json"),
            serde_json::to_string(&json!({
                "schema_version": "sealed_package_checksums_v2",
                "files": files
            }))
            .expect("checksums json"),
        )
        .expect("write checksums");
        fs::write(
            package_dir.join("package.lock"),
            format!(
                "{{\"schema_version\":\"sealed_package_lock_v1\",\"package_digest\":\"{}\"}}",
                package_digest
            ),
        )
        .expect("write package lock");
        fs::write(
            package_dir.join("manifest.json"),
            serde_json::to_string(&json!({
                "schema_version": "sealed_run_package_v2",
                "created_at": "2026-03-04T00:00:00Z",
                "resolved_experiment": manifest_resolved_experiment,
                "checksums_ref": "checksums.json",
                "package_digest": package_digest
            }))
            .expect("manifest json"),
        )
        .expect("write manifest");
        package_digest
    }

    #[test]
    fn load_sealed_package_for_run_directory_without_manifest_fails() {
        let guard = TempDirGuard::new("load_exp_no_manifest");
        let err = load_sealed_package_for_run(&guard.path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("run_input_invalid_kind"),
            "expected run_input_invalid_kind error, got: {msg}"
        );
    }

    #[test]
    fn load_sealed_package_for_run_missing_file_fails() {
        let path = Path::new("/nonexistent/experiment.yaml");
        let err = load_sealed_package_for_run(path).unwrap_err();
        assert!(
            err.to_string().contains("resolve run input path"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn register_experiment_bundle_missing_package_fails_at_input_boundary() {
        let guard = TempDirGuard::new("register_missing_experiment_bundle");
        let missing = guard.path.join("missing-package");
        let err = register_experiment_bundle(&missing).unwrap_err();
        assert!(
            err.to_string().contains("resolve sealed package path"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn load_authoring_input_for_build_rejects_manifest() {
        let guard = TempDirGuard::new("load_exp_manifest_as_build_input");
        let manifest = guard.path.join("manifest.json");
        fs::write(&manifest, r#"{}"#).unwrap();
        let err = load_authoring_input_for_build(&manifest, None).unwrap_err();
        assert!(err.to_string().contains("build_input_invalid_kind"));
    }

    #[test]
    fn load_authoring_input_for_build_missing_file_fails_at_input_boundary() {
        let guard = TempDirGuard::new("load_missing_authoring_input");
        let missing = guard.path.join("missing.yaml");
        let err = load_authoring_input_for_build(&missing, None).unwrap_err();
        assert!(
            err.to_string().contains("resolve build input path"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn load_sealed_package_for_run_directory_package_with_manifest_loads() {
        let guard = TempDirGuard::new("load_exp_pkg");
        fs::write(
            guard.path.join("resolved_experiment.json"),
            r#"{"version":"0.5","experiment":{"id":"e1","workload_type":"agent_runtime"}}"#,
        )
        .unwrap();
        let resolved_digest = sha256_file(&guard.path.join("resolved_experiment.json")).unwrap();
        let staging_digest = write_empty_runtime_staging_manifest(&guard.path);
        let files = json!({
            "resolved_experiment.json": resolved_digest,
            STAGING_MANIFEST_FILE: staging_digest
        });
        write_sealed_package_metadata(
            &guard.path,
            files,
            json!({"version":"0.5","experiment":{"id":"e1","workload_type":"agent_runtime"}}),
        );
        let loaded = load_sealed_package_for_run(&guard.path).unwrap();
        assert_eq!(loaded.json_value.pointer("/version"), Some(&json!("0.5")));
        assert_eq!(
            loaded.json_value.pointer("/experiment/id"),
            Some(&json!("e1"))
        );
    }

    #[test]
    fn load_sealed_package_for_run_rejects_unchecksummed_payload_file() {
        let root = create_dx_authoring_fixture("bucephalus_unsealed_payload_file");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");

        fs::write(build.package_dir.join("files").join("unchecked.txt"), "not sealed")
            .expect("unchecked package payload");

        let err = load_sealed_package_for_run(&build.package_dir)
            .expect_err("run loader must reject unchecksummed package payload files");
        let msg = err.to_string();
        assert!(
            msg.contains("unchecksummed payload file 'files/unchecked.txt'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn check_package_rejects_unchecksummed_payload_file() {
        let root = create_dx_authoring_fixture("bucephalus_check_unsealed_payload_file");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");

        fs::write(build.package_dir.join("files").join("unchecked.txt"), "not sealed")
            .expect("unchecked package payload");

        let err = check_package(&build.package_dir)
            .expect_err("package checks must reject unsealed package payload files");
        let msg = err.to_string();
        assert!(
            msg.contains("unchecksummed payload file 'files/unchecked.txt'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn check_package_requires_package_lock_digest() {
        let root = create_dx_authoring_fixture("bucephalus_check_missing_package_lock");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        fs::remove_file(build.package_dir.join("package.lock")).expect("remove package lock");

        let err = check_package(&build.package_dir)
            .expect_err("package checks must not fabricate a missing package digest");
        let msg = err.to_string();
        assert!(
            msg.contains("package check failed to read package lock"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn check_package_rejects_invalid_package_lock_digest() {
        let root = create_dx_authoring_fixture("bucephalus_check_invalid_package_lock");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        atomic_write_json_pretty(
            &build.package_dir.join("package.lock"),
            &json!({
                "schema_version": "sealed_package_lock_v1",
                "package_digest": "sha256:not_hex"
            }),
        )
        .expect_err("package.lock writer should reject malformed digest");
        fs::write(
            build.package_dir.join("package.lock"),
            r#"{"schema_version":"sealed_package_lock_v1","package_digest":"sha256:not_hex"}"#,
        )
        .expect("write malformed package lock");

        let err = check_package(&build.package_dir)
            .expect_err("package checks must reject malformed package lock digest");
        let msg = err.to_string();
        assert!(
            msg.contains("sealed_package_lock_v1 schema validation failed (package lock)"),
            "unexpected error: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_sealed_package_for_run_rejects_unsealed_payload_symlink() {
        let root = create_dx_authoring_fixture("bucephalus_unsealed_payload_symlink");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        let outside = root.path.join("outside_secret.txt");
        fs::write(&outside, "host-only secret").expect("outside secret");
        symlink(&outside, build.package_dir.join("files").join("leak.txt"))
            .expect("unchecked package symlink");

        let err = load_sealed_package_for_run(&build.package_dir)
            .expect_err("run loader must reject symlinks that are not sealed payload");
        let msg = err.to_string();
        assert!(
            msg.contains("unsealed symlink 'files/leak.txt'"),
            "unexpected error: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_sealed_package_for_run_rejects_checksums_ref_symlink_escape() {
        let root = TempDirGuard::new("bucephalus_checksums_ref_symlink_escape");
        let outside = root.path.with_file_name("outside_checksums.json");
        fs::write(&outside, r#"{"schema_version":"sealed_package_checksums_v2","files":{}}"#)
            .expect("outside checksums");
        symlink(&outside, root.path.join("checksums_link.json")).expect("checksums symlink");
        fs::write(
            root.path.join("manifest.json"),
            r#"{"schema_version":"sealed_run_package_v2","created_at":"2026-03-04T00:00:00Z","resolved_experiment":{},"checksums_ref":"checksums_link.json","package_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        )
        .expect("manifest");

        let err = load_sealed_package_for_run(&root.path)
            .expect_err("metadata refs must not escape the package root");
        assert!(err.to_string().contains("checksums_ref escapes package root"));
    }

    #[test]
    fn load_sealed_package_for_run_rejects_package_checks_ref_inside_payload_dir() {
        let root = create_dx_authoring_fixture("bucephalus_payload_package_checks_ref");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        let smuggled_ref = build.package_dir.join("files").join("package_checks.json");
        atomic_write_json_pretty(
            &smuggled_ref,
            &json!({
                "schema_version": PACKAGE_CHECKS_SCHEMA_VERSION,
                "generated_at": "2024-01-01T00:00:00Z",
                "package_digest": TEST_PACKAGE_DIGEST,
                "passed": true,
                "summary": {"checks": 0, "failed": 0, "warnings": 0},
                "checks": []
            }),
        )
        .expect("smuggled package checks metadata");
        let mut manifest = load_json_file(&build.manifest_path).expect("manifest");
        set_json_pointer_value(
            &mut manifest,
            "/package_checks_ref",
            json!("files/package_checks.json"),
        )
        .expect("rewrite package_checks_ref");
        atomic_write_json_pretty(&build.manifest_path, &manifest).expect("write manifest");

        let err = load_sealed_package_for_run(&build.package_dir)
            .expect_err("metadata refs under payload dirs must not be accepted");
        let msg = err.to_string();
        assert!(
            msg.contains("package_checks_ref must not point inside runtime payload directory 'files'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn load_sealed_package_for_run_rejects_checksums_ref_inside_payload_dir() {
        let root = create_dx_authoring_fixture("bucephalus_payload_checksums_ref");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        fs::copy(
            &build.checksums_path,
            build.package_dir.join("files").join("checksums.json"),
        )
        .expect("smuggled checksums metadata");
        let mut manifest = load_json_file(&build.manifest_path).expect("manifest");
        set_json_pointer_value(&mut manifest, "/checksums_ref", json!("files/checksums.json"))
            .expect("rewrite checksums_ref");
        atomic_write_json_pretty(&build.manifest_path, &manifest).expect("write manifest");

        let err = load_sealed_package_for_run(&build.package_dir)
            .expect_err("checksums ref under payload dirs must not be accepted");
        let msg = err.to_string();
        assert!(
            msg.contains("checksums_ref must not point inside runtime payload directory 'files'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn copy_verified_package_payload_for_run_revalidates_before_copying() {
        let root = create_dx_authoring_fixture("bucephalus_payload_copy_revalidates");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        load_sealed_package_for_run(&build.package_dir).expect("package initially sealed");

        fs::write(
            build.package_dir.join("files").join("late_payload.txt"),
            "added after initial verification",
        )
        .expect("late payload mutation");
        let run_dir = root.path.join("run_copy");
        ensure_dir(&run_dir).expect("run dir");

        let err = copy_verified_package_payload_for_run(&build.package_dir, &run_dir)
            .expect_err("copy must revalidate package payload before materializing it");
        let msg = err.to_string();
        assert!(
            msg.contains("unchecksummed payload file 'files/late_payload.txt'"),
            "unexpected error: {msg}"
        );
        assert!(
            !run_dir.join("files").join("late_payload.txt").exists(),
            "late unsealed payload must not be copied into the run directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_verified_package_payload_for_run_rejects_symlinked_destination_parent() {
        let root = create_dx_authoring_fixture("bucephalus_payload_copy_dest_symlink");
        let spec = minimal_new_dx_spec();
        let spec_path = root.path.join("experiment.yaml");
        fs::write(&spec_path, serde_yaml::to_string(&spec).expect("yaml")).expect("write spec");
        let build = build_experiment_package(&spec_path, None, Some(&root.path.join("package")))
            .expect("build package");
        let run_dir = root.path.join("run_copy");
        ensure_dir(&run_dir).expect("run dir");
        let outside = root.path.join("outside_tasks");
        ensure_dir(&outside).expect("outside tasks");
        symlink(&outside, run_dir.join("tasks")).expect("destination parent symlink");

        let err = copy_verified_package_payload_for_run(&build.package_dir, &run_dir)
            .expect_err("copy must reject destination parents that resolve outside run dir");
        let msg = err.to_string();
        assert!(
            msg.contains("destination parent resolves outside run directory"),
            "unexpected error: {msg}"
        );
        assert!(
            !outside.join("tasks.jsonl").exists(),
            "package payload must not be copied through the destination symlink"
        );
    }

    #[test]
    fn load_sealed_package_for_run_rejects_legacy_v1_experiment() {
        let guard = TempDirGuard::new("load_exp_pkg_v1_reject");
        fs::write(
            guard.path.join("resolved_experiment.json"),
            r#"{"version":"1.0","experiment":{"id":"e1"}}"#,
        )
        .unwrap();
        let resolved_digest = sha256_file(&guard.path.join("resolved_experiment.json")).unwrap();
        let staging_digest = write_empty_runtime_staging_manifest(&guard.path);
        let files = json!({
            "resolved_experiment.json": resolved_digest,
            STAGING_MANIFEST_FILE: staging_digest
        });
        write_sealed_package_metadata(
            &guard.path,
            files,
            json!({"version":"1.0","experiment":{"id":"e1"}}),
        );

        let loaded = load_sealed_package_for_run(&guard.path).expect("load sealed package");
        assert_eq!(
            loaded
                .json_value
                .pointer("/version")
                .and_then(Value::as_str)
                .unwrap_or(""),
            "1.0"
        );
    }

    #[test]
    fn load_sealed_package_for_run_ignores_manifest_resolved_experiment_payload() {
        let guard = TempDirGuard::new("load_exp_pkg_manifest_tamper");
        let resolved_path = guard.path.join("resolved_experiment.json");
        fs::write(
            &resolved_path,
            r#"{"version":"0.5","experiment":{"id":"from_checksums","workload_type":"agent_runtime"}}"#,
        )
        .unwrap();
        let resolved_digest = sha256_file(&resolved_path).unwrap();
        let staging_digest = write_empty_runtime_staging_manifest(&guard.path);
        let files = json!({
            "resolved_experiment.json": resolved_digest,
            STAGING_MANIFEST_FILE: staging_digest
        });
        write_sealed_package_metadata(
            &guard.path,
            files,
            json!({"version":"0.5","experiment":{"id":"tampered_manifest","workload_type":"agent_runtime"}}),
        );

        let loaded = load_sealed_package_for_run(&guard.path).unwrap();
        assert_eq!(
            loaded.json_value.pointer("/experiment/id"),
            Some(&json!("from_checksums"))
        );
    }

    #[test]
    fn load_sealed_package_for_run_rejects_checksum_mismatch() {
        let guard = TempDirGuard::new("load_exp_pkg_bad_checksum");
        fs::write(guard.path.join("resolved_experiment.json"), "{}").unwrap();
        let _staging_digest = write_empty_runtime_staging_manifest(&guard.path);
        let files = json!({
            "resolved_experiment.json": "deadbeef",
            STAGING_MANIFEST_FILE: "deadbeef"
        });
        write_sealed_package_metadata(&guard.path, files, json!({}));
        let err = load_sealed_package_for_run(&guard.path).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn load_sealed_package_for_run_rejects_missing_package_cas_blob() {
        let guard = TempDirGuard::new("load_exp_pkg_missing_cas_blob");
        fs::write(
            guard.path.join("resolved_experiment.json"),
            r#"{"version":"0.5","experiment":{"id":"e1","workload_type":"agent_runtime"}}"#,
        )
        .unwrap();
        let resolved_digest = sha256_file(&guard.path.join("resolved_experiment.json")).unwrap();
        let staging_digest = write_empty_runtime_staging_manifest(&guard.path);
        let pointer_path = guard
            .path
            .join(PACKAGED_RUNTIME_ASSETS_DIR)
            .join("large.bin");
        let missing_digest = format!("sha256:{}", "b".repeat(64));
        write_cas_pointer(&pointer_path, missing_digest, 123).expect("pointer");
        let pointer_digest = sha256_file(&pointer_path).unwrap();
        let files = json!({
            "resolved_experiment.json": resolved_digest,
            STAGING_MANIFEST_FILE: staging_digest,
            "runtime_assets/large.bin": pointer_digest
        });
        write_sealed_package_metadata(&guard.path, files, json!({}));

        let err = load_sealed_package_for_run(&guard.path).unwrap_err();
        assert!(
            err.to_string().contains("references missing package blob"),
            "{}",
            err
        );
    }

    #[test]
    fn load_staging_specs_from_package_rejects_missing_variant_entries() {
        let guard = TempDirGuard::new("load_staging_specs_missing_variant");
        write_runtime_staging_manifest(
            &guard.path,
            &json!({
                "schema_version": STAGING_MANIFEST_SCHEMA_VERSION,
                "variants": {
                    "control": []
                }
            }),
        );

        let err = load_staging_specs_from_package(&guard.path, "treatment")
            .expect_err("missing variant entry should fail");
        assert!(
            err.to_string()
                .contains("runtime staging manifest missing entries for variant 'treatment'"),
            "{}",
            err
        );
    }

    #[test]
    fn load_staging_specs_from_package_rejects_destination_outside_contract_roots() {
        let guard = TempDirGuard::new("load_staging_specs_bad_destination");
        let packaged = guard
            .path
            .join(PACKAGED_RUNTIME_ASSETS_DIR)
            .join("defaults.json");
        ensure_dir(packaged.parent().unwrap()).expect("deps dir");
        fs::write(&packaged, "{}").expect("packaged runtime deps");
        write_runtime_staging_manifest(
            &guard.path,
            &json!({
                "schema_version": STAGING_MANIFEST_SCHEMA_VERSION,
                "variants": {
                    "control": [{
                        "original_relative_path": "overrides/defaults.json",
                        "packaged_path": "runtime_assets/defaults.json",
                        "runtime_path": "/tmp/defaults.json",
                        "required": true,
                        "read_only": true
                    }]
                }
            }),
        );

        let err = load_staging_specs_from_package(&guard.path, "control")
            .expect_err("invalid destination path should fail");
        assert!(
            err.to_string().contains(
                "must be under __BUCEPHALUS_TASK_WORKDIR__/.bucephalus/support or /bucephalus/in/runtime"
            ),
            "{}",
            err
        );
    }


    #[test]
    fn deterministic_committer_from_empty_progress() {
        let progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "r1".to_string(),
            total_slots: 10,
            next_schedule_index: 0,
            next_trial_index: 1,
            schedule: Vec::new(),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let committer = DeterministicCommitter::from_progress(&progress, &[]);
        assert!(committer.committed_schedules.is_empty());
        assert!(committer.committed_keys.is_empty());
        assert!(committer.pending_by_schedule.is_empty());
    }

    #[test]
    fn deterministic_committer_from_progress_with_completed_slots() {
        let progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "r1".to_string(),
            total_slots: 10,
            next_schedule_index: 3,
            next_trial_index: 4,
            schedule: Vec::new(),
            completed_slots: vec![
                SlotCompletion {
                    schedule_index: 0,
                    trial_id: "trial_1".into(),
                    status: "completed".into(),
                    slot_commit_id: "c1".into(),
                    attempt: 1,
                },
                SlotCompletion {
                    schedule_index: 1,
                    trial_id: "trial_2".into(),
                    status: "completed".into(),
                    slot_commit_id: "c2".into(),
                    attempt: 1,
                },
                SlotCompletion {
                    schedule_index: 2,
                    trial_id: "trial_3".into(),
                    status: "failed".into(),
                    slot_commit_id: "c3".into(),
                    attempt: 1,
                },
            ],
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let committer = DeterministicCommitter::from_progress(&progress, &[]);
        assert!(committer.is_committed_schedule(0));
        assert!(committer.is_committed_schedule(1));
        assert!(committer.is_committed_schedule(2));
        assert_eq!(committer.committed_keys.len(), 3);
    }

    #[test]
    fn deterministic_committer_enqueue_skipped_at_next_idx() {
        let progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "r".to_string(),
            total_slots: 5,
            next_schedule_index: 0,
            next_trial_index: 1,
            schedule: Vec::new(),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let mut committer = DeterministicCommitter::from_progress(&progress, &[]);
        let enqueued = committer.enqueue_skipped(0).unwrap();
        assert!(enqueued);
        assert_eq!(committer.pending_by_schedule.len(), 1);
    }

    #[test]
    fn deterministic_committer_enqueue_duplicate_returns_false() {
        let progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "r".to_string(),
            total_slots: 5,
            next_schedule_index: 0,
            next_trial_index: 1,
            schedule: Vec::new(),
            completed_slots: Vec::new(),
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let mut committer = DeterministicCommitter::from_progress(&progress, &[]);
        committer.enqueue_skipped(0).unwrap();
        let second = committer.enqueue_skipped(0).unwrap();
        assert!(!second, "duplicate enqueue should return false");
    }

    #[test]
    fn deterministic_committer_enqueue_committed_slot_errors() {
        let progress = ScheduleProgress {
            schema_version: "schedule_progress_v2".to_string(),
            run_id: "r".to_string(),
            total_slots: 5,
            next_schedule_index: 5,
            next_trial_index: 6,
            schedule: Vec::new(),
            completed_slots: vec![SlotCompletion {
                schedule_index: 2,
                trial_id: "trial_3".to_string(),
                status: "completed".to_string(),
                slot_commit_id: "c3".to_string(),
                attempt: 1,
            }],
            pruned_variants: Vec::new(),
            consecutive_failures: BTreeMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let mut committer = DeterministicCommitter::from_progress(&progress, &[]);
        let err = committer.enqueue_skipped(2).unwrap_err();
        assert!(err.to_string().contains("already committed schedule_idx"));
    }

    #[test]
    fn deterministic_committer_commit_key_for_slot_completion_deterministic() {
        let slot = SlotCompletion {
            schedule_index: 7,
            trial_id: "trial_8".to_string(),
            status: "completed".to_string(),
            slot_commit_id: "xyz".to_string(),
            attempt: 1,
        };
        let key = DeterministicCommitter::commit_key_for_slot_completion(&slot);
        assert_eq!(key, "7:trial_8:completed");
    }


    #[test]
    fn highest_attempt_by_schedule_empty_returns_empty() {
        let result = highest_attempt_by_schedule(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn highest_attempt_by_schedule_tracks_max_attempt_per_index() {
        let records = vec![
            SlotCommitRecord {
                schema_version: "v1".to_string(),
                record_type: "slot_commit".to_string(),
                run_id: "r1".to_string(),
                schedule_idx: 0,
                slot_commit_id: "c1".to_string(),
                trial_id: "t1".to_string(),
                slot_status: "completed".to_string(),
                attempt: 1,
                recorded_at: Utc::now().to_rfc3339(),
                payload_digest: None,
                expected_rows: None,
                written_rows: None,
                facts_fsync_completed: None,
                runtime_fsync_completed: None,
            },
            SlotCommitRecord {
                schema_version: "v1".to_string(),
                record_type: "slot_commit".to_string(),
                run_id: "r1".to_string(),
                schedule_idx: 0,
                slot_commit_id: "c2".to_string(),
                trial_id: "t1".to_string(),
                slot_status: "failed".to_string(),
                attempt: 3,
                recorded_at: Utc::now().to_rfc3339(),
                payload_digest: None,
                expected_rows: None,
                written_rows: None,
                facts_fsync_completed: None,
                runtime_fsync_completed: None,
            },
            SlotCommitRecord {
                schema_version: "v1".to_string(),
                record_type: "slot_commit".to_string(),
                run_id: "r1".to_string(),
                schedule_idx: 1,
                slot_commit_id: "c3".to_string(),
                trial_id: "t2".to_string(),
                slot_status: "completed".to_string(),
                attempt: 2,
                recorded_at: Utc::now().to_rfc3339(),
                payload_digest: None,
                expected_rows: None,
                written_rows: None,
                facts_fsync_completed: None,
                runtime_fsync_completed: None,
            },
        ];
        let result = highest_attempt_by_schedule(&records);
        assert_eq!(*result.get(&0).unwrap(), 3);
        assert_eq!(*result.get(&1).unwrap(), 2);
    }


    #[test]
    fn output_peer_path_replaces_filename() {
        assert_eq!(
            output_peer_path("/bucephalus/out/result.json", "prediction.json"),
            "/bucephalus/out/prediction.json"
        );
    }

    #[test]
    fn output_peer_path_no_parent_returns_filename() {
        assert_eq!(
            output_peer_path("result.json", "prediction.json"),
            "prediction.json"
        );
    }


    #[test]
    fn find_project_root_returns_parent_of_dot_lab() {
        let guard = TempDirGuard::new("find_root_lab");
        let lab_dir = guard.path.join(".lab");
        fs::create_dir_all(&lab_dir).unwrap();
        let result = find_project_root(&lab_dir);
        assert_eq!(result, guard.path);
    }

    #[test]
    fn find_project_root_no_dot_lab_returns_input() {
        let guard = TempDirGuard::new("find_root_none");
        let result = find_project_root(&guard.path);
        assert_eq!(result, guard.path);
    }


    #[test]
    fn validate_required_fields_v1_whitespace_experiment_id_fails() {
        let mut spec = json!({
            "version": "1.0",
            "experiment": {"id": "e1", "name": "test"},
            "dataset": {"path": "tasks.jsonl"},
            "design": {"replications": 1},
            "baseline": {"variant_id": "baseline"},
            "runtime": {"image": "img:latest", "command": ["python", "main.py"]}
        });
        spec["experiment"]["id"] = json!("  ");
        let err = validate_required_fields(&spec).unwrap_err();
        assert!(
            err.to_string().contains("experiment version '1.0'"),
            "err: {err}"
        );
    }

    #[test]
    fn validate_required_fields_v1_whitespace_baseline_variant_id_fails() {
        let mut spec = json!({
            "version": "1.0",
            "experiment": {"id": "e1", "name": "test"},
            "dataset": {"path": "tasks.jsonl"},
            "design": {"replications": 1},
            "baseline": {"variant_id": "baseline"},
            "runtime": {"image": "img:latest", "command": ["python", "main.py"]}
        });
        spec["baseline"]["variant_id"] = json!("  ");
        let err = validate_required_fields(&spec).unwrap_err();
        assert!(
            err.to_string().contains("experiment version '1.0'"),
            "err: {err}"
        );
    }
}
