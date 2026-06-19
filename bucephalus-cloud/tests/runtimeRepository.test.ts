import { describe, expect, test } from "bun:test";
import {
  declaredRuntimeResources,
  filterRuntimeResources,
  RuntimeRepository,
  runtimeAccessTargetFromInventory,
  runtimeApiResourceForKind,
  runtimeApiResourceList,
  runtimeLogTargetForResource,
  runtimeResourceEventsForResource,
  runtimeAttemptObjectsFromSnapshots,
  runtimeContractStagesFromSnapshots,
  runtimeEventRowsFromSnapshots,
  runtimeMetricObservationsFromTrialResults,
  runtimeValueRecordsFromSnapshots,
  runtimeResourceHealthSummary,
  runtimeResourceMetricsView,
  runtimeSnapshotFromWorkerEventPayload,
  runtimeTrialResultsFromSnapshots,
  type RuntimeEventRecord,
  type RuntimeResourceRecord,
} from "../src/runtime/repository";
import type { JsonObject } from "../src/primitives";

describe("runtime repository worker snapshots", () => {
  test("summarizes runtime health without treating progressing resources as problems", () => {
    const inventory = {
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceList",
      metadata: runtimeResourceListMetadataFixture(),
      cloud_run_id: "run-1",
      core_run_ids: ["core-run-1"],
      resources: [
        runtimeResourceFixture({
          kind: "RunnerInstance",
          name: "runner-1",
          status: {
            phase: "online",
            actions: ["cordon", "drain"],
            access: {
              reachable: true,
              port_forward: false,
              exec: true,
              runner_instance_id: "runner-1",
              attempt_id: "attempt-1",
              worker_id: "worker-1",
            },
            conditions: [
              runtimeCondition("Ready", "True", "Online", "Runner is online"),
              runtimeCondition("PortForwardReady", "False", "CapabilityMissing", "Runner does not advertise runtime_port_forward"),
            ],
          },
          audit: {
            source: "cloud.runner_instances",
          },
        }),
        runtimeResourceFixture({
          kind: "TrialContainer",
          name: "trial-1.agent.container-1",
          status: {
            phase: "running",
            access: {
              reachable: true,
              port_forward: true,
              exec: true,
            },
            conditions: [runtimeCondition("Ready", "True", "Running", "Container is running")],
          },
        }),
        runtimeResourceFixture({
          kind: "RunnerProvision",
          name: "provision-1",
          status: {
            phase: "provisioning",
            conditions: [runtimeCondition("Ready", "False", "Provisioning", "Runner is still provisioning")],
          },
        }),
        runtimeResourceFixture({
          kind: "Exec",
          name: "exec-1",
          status: {
            phase: "failed",
            conditions: [runtimeCondition("Ready", "False", "Failed", "command exited 2")],
          },
        }),
      ],
    } satisfies Parameters<typeof runtimeResourceHealthSummary>[0];

    const health = runtimeResourceHealthSummary(inventory);

    expect(health.kind).toBe("RuntimeResourceHealth");
    expect(health.summary).toMatchObject({
      total: 4,
      ready: 1,
      degraded: 1,
      unknown: 1,
      problem: 1,
      access_targets: 2,
      reachable_access_targets: 2,
      port_forward_ready: 1,
      exec_ready: 2,
      actions_available: 1,
      observed_resources: 0,
      observed_current: 0,
      observed_stale: 0,
      observed_unknown: 4,
    });
    expect(health.resources.map((row) => row.health)).toEqual(["problem", "degraded", "unknown", "ready"]);
    expect(health.resources.find((row) => row.resource === "RunnerProvision/provision-1")).toMatchObject({
      health: "unknown",
      condition_summary: "Ready=False Provisioning",
    });
    expect(health.resources.find((row) => row.resource === "RunnerInstance/runner-1")).toMatchObject({
      health: "degraded",
      access_summary: expect.stringContaining("reachable yes"),
      degraded_conditions: [
        expect.objectContaining({ type: "PortForwardReady", status: "False" }),
      ],
    });
  });

  test("classifies unsatisfied declared runtime requirements as health problems", () => {
    const inventory = {
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceList",
      metadata: runtimeResourceListMetadataFixture(),
      cloud_run_id: "run-1",
      core_run_ids: [],
      resources: [
        runtimeResourceFixture({
          kind: "AcceleratorRequirement",
          name: "gpu-a10",
          status: {
            phase: "Unsatisfied",
            reason: "AcceleratorCapabilityMissing",
            message: "No active runner attempt advertises accelerator:gpu-a10.",
            conditions: [
              runtimeCondition("Ready", "False", "Unsatisfied", "No active runner attempt advertises accelerator:gpu-a10."),
            ],
          },
          audit: {
            source: "cloud.runs.run_requirements.accelerators",
          },
        }),
      ],
    } satisfies Parameters<typeof runtimeResourceHealthSummary>[0];

    const health = runtimeResourceHealthSummary(inventory);

    expect(health.summary).toMatchObject({
      total: 1,
      ready: 0,
      degraded: 0,
      unknown: 0,
      problem: 1,
      observed_resources: 0,
      observed_current: 0,
      observed_stale: 0,
      observed_unknown: 1,
    });
    expect(health.resources[0]).toMatchObject({
      resource: "AcceleratorRequirement/gpu-a10",
      health: "problem",
      phase: "Unsatisfied",
      reason: "AcceleratorCapabilityMissing",
      message: "No active runner attempt advertises accelerator:gpu-a10.",
      condition_summary: "Ready=False Unsatisfied",
      ready: expect.objectContaining({
        type: "Ready",
        status: "False",
        reason: "Unsatisfied",
      }),
    });
  });

  test("marks stale observedGeneration as degraded runtime health", () => {
    const inventory = {
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceList",
      metadata: runtimeResourceListMetadataFixture(),
      cloud_run_id: "run-1",
      core_run_ids: ["core-run-1"],
      resources: [
        runtimeResourceFixture({
          kind: "RunnerInstance",
          name: "runner-stale",
          generation: 9,
          status: {
            phase: "online",
            observedGeneration: 7,
            conditions: [
              runtimeCondition("Ready", "True", "Online", "Runner is online"),
              runtimeCondition("Observed", "False", "ObservedGenerationStale", "Status observedGeneration 7 is behind metadata generation 9"),
            ],
          },
          audit: {
            source: "cloud.runner_instances",
          },
        }),
      ],
    } satisfies Parameters<typeof runtimeResourceHealthSummary>[0];

    const health = runtimeResourceHealthSummary(inventory);

    expect(health.summary).toMatchObject({
      total: 1,
      ready: 0,
      degraded: 1,
      unknown: 0,
      problem: 0,
      observed_resources: 1,
      observed_current: 0,
      observed_stale: 1,
      observed_unknown: 0,
    });
    expect(health.resources[0]).toMatchObject({
      resource: "RunnerInstance/runner-stale",
      health: "degraded",
      observed: "stale",
      condition_summary: "Observed=False ObservedGenerationStale",
      degraded_conditions: [
        expect.objectContaining({
          type: "Observed",
          status: "False",
          reason: "ObservedGenerationStale",
        }),
      ],
    });
  });

  test("includes all runtime access request kinds in the resource inventory", async () => {
    const run = cloudRunRecord();
    const inventory = await RuntimeRepository.prototype.resources.call({
      async coreRunIdsForCloudRun() {
        return [];
      },
      async workerRuntimeSnapshots() {
        return [];
      },
      async runAttempts() {
        return [attemptRecord({
          runner_instance_capabilities: {
            executors: ["runner-docker"],
            resources: ["core_runner", "runtime_exec"],
            arch: "x86_64",
            cpu_count: 4,
            memory_mb: 8192,
            disk_mb: 32768,
            isolation: ["reusable_vm"],
          },
        })];
      },
      async provisionRequests() {
        return [provisionRequestRecord()];
      },
      async runnerPools() {
        return [runnerPoolRecord()];
      },
      async listPortForwards() {
        return [accessRequestRecord({ kind: "port_forward", access_request_id: "pf-1", target_port: 8080 })];
      },
      async listExecRequests() {
        return [accessRequestRecord({ kind: "exec", access_request_id: "exec-1", protocol: "exec", target_port: null, command: ["python", "-V"] })];
      },
      async coreRuns() {
        return [{
          core_run_id: "core-run-1",
          experiment_id: "experiment-1",
          project_root: "/workspace/project",
          run_dir: "/workspace/project/.buc/runs/core-run-1",
          artifact_root: "/workspace/project/.buc/runs/core-run-1/artifacts",
          runtime_status: "running",
          manifest: {
            schema_version: "run_manifest_v1",
            run_id: "core-run-1",
            experiment_id: "experiment-1",
          },
          created_at_ms: 1_801_958_390_000,
          updated_at_ms: 1_801_958_400_000,
        }];
      },
      async runManifests() {
        return [{
          core_run_id: "core-run-1",
          experiment_id: "experiment-1",
          project_root: "/workspace/project",
          run_dir: "/workspace/project/.buc/runs/core-run-1",
          artifact_root: "/workspace/project/.buc/runs/core-run-1/artifacts",
          runtime_status: "running",
          manifest: {
            schema_version: "run_manifest_v1",
            run_id: "core-run-1",
            workload_type: "benchmark",
            baseline_id: "baseline-a",
            variant_ids: ["variant-a", "variant-b"],
          },
          updated_at_ms: 1_801_958_400_000,
        }];
      },
      async metricDefinitions() {
        return [{
          experiment_id: "experiment-1",
          metric_id: "pass@1",
          semantic_key: "accuracy.pass_at_1",
          label: "Pass@1",
          value_type: "number",
          unit: "ratio",
          direction: "maximize",
          source_type: "json_pointer",
          source_pointer: "/metrics/pass@1",
          required: true,
          primary_metric: true,
          definition: {
            id: "pass@1",
            semantic_key: "accuracy.pass_at_1",
            label: "Pass@1",
            source: { type: "json_pointer", pointer: "/metrics/pass@1" },
          },
          updated_at_ms: 1_801_958_405_000,
        }];
      },
      async scheduleSlots() {
        return [scheduleSlotRecord()];
      },
      async slotCommitRecords() {
        return [{
          core_run_id: "core-run-1",
          schedule_idx: 0,
          attempt: 1,
          record_type: "commit",
          slot_commit_id: "slot-commit-1",
          record: {
            schema_version: "slot_commit_record_v1",
            record_type: "commit",
            run_id: "core-run-1",
            schedule_idx: 0,
            slot_commit_id: "slot-commit-1",
            trial_id: "trial-1",
            slot_status: "passed",
            attempt: 1,
            recorded_at: "2026-06-04T00:00:20Z",
            written_rows: {
              trials: 1,
              metrics: 1,
              events: 2,
              contract_stages: 3,
            },
            facts_fsync_completed: true,
            runtime_fsync_completed: true,
          },
          recorded_at_ms: 1_801_958_420_000,
        }];
      },
      async pendingTrialCompletions() {
        return [{
          core_run_id: "core-run-1",
          schedule_idx: 2,
          trial_result: {
            trial_id: "trial-pending-1",
            slot_status: "passed",
            deferred_trial_records: [{ trial_id: "trial-pending-1" }],
            deferred_metric_rows: [{ metric_name: "pass@1" }],
            deferred_event_rows: [{ event_type: "trial.completed" }],
            deferred_contract_stage_rows: [],
            deferred_variant_snapshot_rows: [],
            deferred_evidence_records: [],
            deferred_chain_state_records: [],
            deferred_trial_conclusion_records: [],
          },
          updated_at_ms: 1_801_958_430_000,
        }];
      },
      async variantSnapshots() {
        return [{
          core_run_id: "core-run-1",
          trial_id: "trial-1",
          schedule_idx: 0,
          attempt: 1,
          row_seq: 0,
          slot_commit_id: "slot-commit-1",
          variant_id: "variant-a",
          baseline_id: "baseline-a",
          task_id: "task-a",
          repl_idx: 0,
          binding_name: "prompt",
          binding_value: { temperature: 0.2 },
          binding_value_text: "{\"temperature\":0.2}",
          row: {
            run_id: "core-run-1",
            trial_id: "trial-1",
            schedule_idx: 0,
            attempt: 1,
            row_seq: 0,
            slot_commit_id: "slot-commit-1",
            variant_id: "variant-a",
            baseline_id: "baseline-a",
            task_id: "task-a",
            repl_idx: 0,
            binding_name: "prompt",
            binding_value: { temperature: 0.2 },
            binding_value_text: "{\"temperature\":0.2}",
          },
        }];
      },
      async evidenceRows() {
        return [{
          core_run_id: "core-run-1",
          schedule_idx: 0,
          attempt: 1,
          row_seq: 0,
          slot_commit_id: "slot-commit-1",
          row: {
            schema_version: "evidence_record_v1",
            kind: "grader_report",
            ids: { trial_id: "trial-1" },
            run_id: "core-run-1",
            schedule_idx: 0,
            attempt: 1,
            row_seq: 0,
            slot_commit_id: "slot-commit-1",
          },
        }];
      },
      async chainStateRows() {
        return [{
          core_run_id: "core-run-1",
          schedule_idx: 0,
          attempt: 1,
          row_seq: 0,
          slot_commit_id: "slot-commit-1",
          row: {
            schema_version: "chain_state_v1",
            kind: "lineage_state",
            trial_id: "trial-1",
            run_id: "core-run-1",
            schedule_idx: 0,
            attempt: 1,
            row_seq: 0,
            slot_commit_id: "slot-commit-1",
          },
        }];
      },
      async trialConclusionRows() {
        return [{
          core_run_id: "core-run-1",
          schedule_idx: 0,
          attempt: 1,
          row_seq: 0,
          slot_commit_id: "slot-commit-1",
          row: {
            schema_version: "trial_conclusion_v1",
            trial_id: "trial-1",
            status: "completed",
            outcome: "passed",
            run_id: "core-run-1",
            schedule_idx: 0,
            attempt: 1,
            row_seq: 0,
            slot_commit_id: "slot-commit-1",
          },
        }];
      },
      async lineageVersions() {
        return [{
          core_run_id: "core-run-1",
          version_id: "lineage-version-1",
          chain_key: "agent.workspace",
          step_index: 4,
          trial_id: "trial-1",
          parent_version_id: "lineage-version-0",
          pre_snapshot_ref: "runtime://snapshots/pre",
          post_snapshot_ref: "runtime://snapshots/post",
          diff_incremental_ref: "runtime://diffs/inc",
          diff_cumulative_ref: "runtime://diffs/cum",
          patch_incremental_ref: "runtime://patches/inc",
          patch_cumulative_ref: "runtime://patches/cum",
          workspace_ref: "runtime://workspace/post",
          checkpoint_labels: {
            branch: "main",
          },
        }];
      },
      async lineageHeads() {
        return [{
          core_run_id: "core-run-1",
          chain_key: "agent.workspace",
          latest_version_id: "lineage-version-1",
          step_index: 4,
          latest_workspace_ref: "runtime://workspace/post",
        }];
      },
      async trialAttempts() {
        return [trialAttemptRecord()];
      },
      async trialContainers() {
        return [trialContainerRecord()];
      },
      async contractStages() {
        return [{
          core_run_id: "core-run-1",
          trial_id: "trial-1",
          schedule_idx: 0,
          attempt: 0,
          row_seq: 2,
          variant_id: "variant-a",
          task_id: "task-a",
          repl_idx: 0,
          stage: "agent_execution",
          status: "ok",
          recorded_at: "2026-06-04T00:00:10Z",
          detail: {
            status: "ok",
            duration_ms: 1200,
          },
          row: {
            source: "runtime.contract_stage_rows",
          },
        }];
      },
      async runtimeValueRecords() {
        return [{
          core_run_id: "core-run-1",
          key: "run_control_v2",
          value: {
            status: "running",
            active_trials: 1,
          },
          source: "bucephalus_runtime.runtime_kv",
          updated_at_ms: 1_801_958_400_000,
          observed_at: "2027-02-04T00:00:00.000Z",
          row: {
            source: "bucephalus_runtime.runtime_kv",
          },
        }];
      },
      async metricObservations() {
        return [{
          core_run_id: "core-run-1",
          trial_id: "trial-1",
          schedule_idx: 0,
          attempt: 0,
          row_seq: 7,
          variant_id: "variant-a",
          task_id: "task-a",
          repl_idx: 0,
          outcome: "success",
          metric_name: "pass@1",
          metric_value: 0.91,
          metric_source: "bucephalus_runtime.metric_rows",
          row: {
            source: "runtime.metric_rows",
          },
        }];
      },
      async performanceSamples() {
        return [{
          core_run_id: "core-run-1",
          sample_id: "perf-1",
          trial_id: "trial-1",
          schedule_idx: 0,
          attempt: 0,
          sample_seq: 3,
          sample_kind: "duration",
          stage: "agent_execution",
          duration_ms: 42.5,
          process_rss_kb: 131072,
          payload: {
            stage: "agent_execution",
            sample_kind: "duration",
            duration_ms: 42.5,
          },
          recorded_at_ms: 1_801_958_400_000,
        }];
      },
      async runtimeOperations() {
        return [{
          core_run_id: "core-run-1",
          op_kind: "replay",
          op_id: "replay-1",
          payload: {
            schema_version: "replay_manifest_v1",
            operation: "replay",
            replay_id: "replay-1",
            parent_trial_id: "trial-1",
            trial_id: "trial-1-replay",
            strict: true,
            integration_level: "full",
            replay_grade: "trusted",
            created_at: "2026-06-04T00:00:20Z",
          },
          updated_at_ms: 1_801_958_420_000,
        }];
      },
      async attemptObjects() {
        return [attemptObjectRecord()];
      },
    }, "run-1", run);

    for (const resource of inventory.resources) {
      expect(resource.metadata.generation).toEqual(expect.any(Number));
      expect(resource.status.observedGeneration).toBe(resource.metadata.generation);
      expect(runtimeConditionsByType(resource).Observed).toMatchObject({
        status: "True",
        reason: "ObservedGenerationCurrent",
      });
    }

    const runResource = inventory.resources.find((resource) => resource.kind === "Run");
    expect(runResource).toMatchObject({
      status: {
        phase: "running",
        access: {
          reachable: true,
          reason: "reachable",
          port_forward: false,
          exec: true,
          runner_instance_id: "runner-instance-1",
          attempt_id: "attempt-1",
          worker_id: "worker-1",
        },
      },
    });
    expect(runtimeConditionsByType(runResource!).Reachable).toMatchObject({
      status: "True",
      reason: "Reachable",
    });
    expect(runtimeConditionsByType(runResource!).PortForwardReady).toMatchObject({
      status: "False",
      reason: "CapabilityMissing",
    });
    expect(runtimeConditionsByType(runResource!).ExecReady).toMatchObject({
      status: "True",
      reason: "CapabilityAdvertised",
    });
    expect(filterRuntimeResources([runResource!], {
      fieldSelector: "status.access.exec=true,status.access.port_forward=false",
    })).toEqual([runResource!]);
    expect(runtimeAccessTargetFromInventory(inventory.resources, {
      resourceKind: "Run",
      resourceName: "run-1",
    })).toEqual({
      kind: "Run",
      name: "run-1",
      uid: "run-1",
      resourceVersion: runResource!.metadata.resourceVersion!,
      runnerInstanceId: "runner-instance-1",
      attemptId: "attempt-1",
      workerId: "worker-1",
    });

    const coreRun = inventory.resources.find((resource) => resource.kind === "CoreRun");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["CoreRun"],
      labelSelector: "bucephalus.dev/core-run-id=core-run-1,bucephalus.dev/runtime-status=running",
      fieldSelector: "status.experiment_id=experiment-1",
    })).toEqual([coreRun!]);
    expect(coreRun).toMatchObject({
      kind: "CoreRun",
      metadata: {
        name: "core-run-1",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/experiment-id": "experiment-1",
          "bucephalus.dev/runtime-status": "running",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
        created_at: "2027-02-06T23:59:50.000Z",
        updated_at: "2027-02-07T00:00:00.000Z",
      },
      spec: {
        core_run_id: "core-run-1",
      },
      status: expect.objectContaining({
        phase: "running",
        experiment_id: "experiment-1",
        runtime_status: "running",
        project_root: "/workspace/project",
        run_dir: "/workspace/project/.buc/runs/core-run-1",
        artifact_root: "/workspace/project/.buc/runs/core-run-1/artifacts",
        created_at: "2027-02-06T23:59:50.000Z",
        observed_at: "2027-02-07T00:00:00.000Z",
        created_at_ms: 1_801_958_390_000,
        updated_at_ms: 1_801_958_400_000,
        manifest: expect.objectContaining({
          run_id: "core-run-1",
          experiment_id: "experiment-1",
        }),
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.runs",
      }),
    });

    const runnerPool = inventory.resources.find((resource) => resource.kind === "RunnerPool");
    expect(runnerPool).toMatchObject({
      metadata: {
        name: "runner-pool-1",
        uid: "runner-pool-1",
        labels: {
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/runner-pool-id": "runner-pool-1",
        },
      },
      spec: {
        runner_pool_id: "runner-pool-1",
        name: "on-demand-docker",
        capabilities: {
          executors: ["runner-docker"],
          resources: ["core_runner", "runtime_exec"],
          arch: "x86_64",
          cpu_count: 4,
          memory_mb: 8192,
          disk_mb: 32768,
          isolation: ["reusable_vm"],
        },
        metadata: {
          provider: "gcp",
        },
      },
      status: {
        phase: "active",
        run_scope: "current_run",
        runner_instance_ids: ["runner-instance-1"],
        worker_ids: ["worker-1"],
        active_attempt_ids: ["attempt-1"],
        runner_instances: {
          total: 1,
          by_phase: {
            online: 1,
          },
        },
        attempts: {
          total: 1,
          active: 1,
          by_phase: {
            running: 1,
          },
        },
        provision_requests: {
          total: 1,
          pending: 1,
          by_phase: {
            provisioning: 1,
          },
        },
      },
      audit: {
        source: "cloud.runner_pools",
      },
    });
    expect(runtimeConditionsByType(runnerPool!).Ready).toMatchObject({
      status: "True",
      reason: "Active",
    });

    const runnerInstance = inventory.resources.find((resource) => resource.kind === "RunnerInstance");
    expect(runnerInstance).toMatchObject({
      metadata: {
        name: "runner-instance-1",
        uid: "runner-instance-1",
        annotations: {
          "bucephalus.dev/audit-source": "cloud.runner_instances",
          "bucephalus.dev/phase": "online",
        },
        labels: {
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/runner-instance-id": "runner-instance-1",
          "bucephalus.dev/runner-pool-id": "runner-pool-1",
          "bucephalus.dev/current-attempt-id": "attempt-1",
          "bucephalus.dev/provider": "gce",
          "topology.kubernetes.io/zone": "us-central1-a",
        },
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "RunnerPool", name: "runner-pool-1", uid: "runner-pool-1" }),
        ]),
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        runner_pool_id: "runner-pool-1",
        instance_name: "runner-1",
        capabilities: {
          executors: ["runner-docker"],
          resources: ["core_runner", "runtime_exec"],
          arch: "x86_64",
          cpu_count: 4,
          memory_mb: 8192,
          disk_mb: 32768,
          isolation: ["reusable_vm"],
        },
        attempt_ids: ["attempt-1"],
        worker_ids: ["worker-1"],
      },
      status: {
        phase: "online",
        provider: "gce",
        provider_instance_id: "gce://projects/proj-1/zones/us-central1-a/instances/buc-runner-1",
        project_id: "proj-1",
        zone: "us-central1-a",
        instance_name: "buc-runner-1",
        last_heartbeat_at: "2026-06-04T00:00:00Z",
        current_attempt_id: "attempt-1",
        active_attempts: 1,
        actions: ["cordon", "drain"],
        access: {
          port_forward: false,
          exec: true,
          reachable: true,
          reason: "reachable",
          runner_instance_id: "runner-instance-1",
          attempt_id: "attempt-1",
          worker_id: "worker-1",
        },
      },
      audit: {
        source: "cloud.runner_instances",
      },
    });
    expect(runtimeConditionsByType(runnerInstance!).Ready).toMatchObject({
      status: "True",
      reason: "Online",
    });
    expect(runnerInstance!.status).toMatchObject({
      reason: "CapabilityMissing",
      message: "Runner does not advertise runtime_port_forward",
    });
    expect(runtimeConditionsByType(runnerInstance!).Reachable).toMatchObject({
      status: "True",
    });
    expect(runtimeConditionsByType(runnerInstance!).ExecReady).toMatchObject({
      status: "True",
      reason: "CapabilityAdvertised",
    });
    const runnerInstanceVersion = runnerInstance!.metadata.resourceVersion;
    const runnerInstanceGeneration = runnerInstance!.metadata.generation;
    expect(runnerInstanceGeneration).toEqual(expect.any(Number));
    expect(runnerInstance!.status.observedGeneration).toBe(runnerInstanceGeneration);
    expect(runnerInstanceVersion).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(filterRuntimeResources([runnerInstance!], {
      fieldSelector: `metadata.generation=${runnerInstanceGeneration}`,
    })).toEqual([runnerInstance!]);
    expect(filterRuntimeResources([runnerInstance!], {
      fieldSelector: `metadata.resourceVersion=${runnerInstanceVersion}`,
    })).toEqual([runnerInstance!]);
    expect(filterRuntimeResources([runnerInstance!], {
      fieldSelector: "status.reason=CapabilityMissing",
    })).toEqual([runnerInstance!]);
    expect(filterRuntimeResources([runnerInstance!], {
      fieldSelector: "status.provider=gce,status.zone=us-central1-a",
    })).toEqual([runnerInstance!]);
    expect(filterRuntimeResources([runnerInstance!], {
      fieldSelector: "status.access.reachable=true",
    })).toEqual([runnerInstance!]);

    const runnerAttempt = inventory.resources.find((resource) => resource.kind === "RunnerAttempt");
    expect(runnerAttempt).toMatchObject({
      metadata: {
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "RunnerPool", name: "runner-pool-1", uid: "runner-pool-1" }),
          expect.objectContaining({ kind: "RunnerInstance", name: "runner-instance-1", uid: "runner-instance-1" }),
        ]),
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        runner_pool_id: "runner-pool-1",
        capabilities: {
          executors: ["runner-docker"],
          resources: ["core_runner", "runtime_exec"],
          arch: "x86_64",
          cpu_count: 4,
          memory_mb: 8192,
          disk_mb: 32768,
          isolation: ["reusable_vm"],
        },
      },
      status: {
        runner_instance_status: "online",
        access: {
          port_forward: false,
          exec: true,
        },
      },
      audit: {
        runner_source: "cloud.runner_instances",
      },
    });
    expect(runtimeConditionsByType(runnerAttempt!).PortForwardReady).toMatchObject({
      status: "False",
      reason: "CapabilityMissing",
    });
    expect(runtimeConditionsByType(runnerAttempt!).ExecReady).toMatchObject({
      status: "True",
      reason: "CapabilityAdvertised",
    });
    expect(runtimeConditionsByType(runnerAttempt!).Reachable).toMatchObject({
      status: "True",
    });
    expect(runnerAttempt!.status).toMatchObject({
      reason: "CapabilityMissing",
      message: "Runner does not advertise runtime_port_forward",
    });

    const provisionRequest = inventory.resources.find((resource) => resource.kind === "RunnerProvisionRequest");
    expect(provisionRequest).toMatchObject({
      metadata: {
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "RunnerPool", name: "runner-pool-1", uid: "runner-pool-1" }),
        ]),
      },
      status: {
        phase: "provisioning",
      },
    });

    const trial = inventory.resources.find((resource) => resource.kind === "Trial");
    expect(trial).toMatchObject({
      metadata: {
        name: "trial-1",
        uid: "core-run-1:trial-1",
        labels: {
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/variant-id": "variant-a",
          "bucephalus.dev/task-id": "task-a",
          "bucephalus.dev/worker-id": "worker-1",
          "bucephalus.dev/attempt-id": "attempt-1",
          "bucephalus.dev/runner-instance-id": "runner-instance-1",
          "bucephalus.dev/runner-pool-id": "runner-pool-1",
        },
        ownerReferences: [
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ],
      },
      spec: {
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        schedule_idx: 0,
        attempt: 0,
        variant_id: "variant-a",
        task_id: "task-a",
        repl_idx: 0,
      },
      status: {
        phase: "running",
        schedule_slot: "core-run-1.0",
        containers: {
          total: 1,
          by_phase: {
            running: 1,
          },
          roles: ["agent"],
        },
        runner_binding: {
          worker_id: "worker-1",
          attempt_id: "attempt-1",
          attempt_phase: "running",
          runner_instance_id: "runner-instance-1",
          runner_instance_status: "online",
          runner_pool_id: "runner-pool-1",
          access: {
            port_forward: false,
            exec: true,
          },
        },
        access: {
          reachable: true,
          reason: "reachable",
          port_forward: false,
          exec: true,
          runner_instance_id: "runner-instance-1",
          attempt_id: "attempt-1",
          worker_id: "worker-1",
        },
      },
      audit: {
        source: "bucephalus_runtime.trial_attempts",
      },
    });
    expect(runtimeConditionsByType(trial!).RunnerBound).toMatchObject({
      status: "True",
      reason: "Bound",
    });
    expect(runtimeConditionsByType(trial!).Reachable).toMatchObject({
      status: "True",
    });
    expect(runtimeAccessTargetFromInventory(inventory.resources, {
      resourceKind: "Trial",
      resourceName: "trial-1",
    })).toMatchObject({
      kind: "Trial",
      name: "trial-1",
      runnerInstanceId: "runner-instance-1",
      attemptId: "attempt-1",
      workerId: "worker-1",
    });

    const scheduleSlot = inventory.resources.find((resource) => resource.kind === "ScheduleSlot");
    expect(scheduleSlot).toMatchObject({
      metadata: {
        name: "core-run-1.0",
        uid: "core-run-1:0",
        labels: {
          "bucephalus.dev/worker-id": "worker-1",
          "bucephalus.dev/attempt-id": "attempt-1",
          "bucephalus.dev/runner-instance-id": "runner-instance-1",
          "bucephalus.dev/runner-pool-id": "runner-pool-1",
        },
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "RunnerPool", name: "runner-pool-1", uid: "runner-pool-1" }),
          expect.objectContaining({ kind: "RunnerInstance", name: "runner-instance-1", uid: "runner-instance-1" }),
          expect.objectContaining({ kind: "RunnerAttempt", name: "attempt-1", uid: "attempt-1" }),
        ]),
      },
      status: {
        worker_id: "worker-1",
        runner_binding: {
          worker_id: "worker-1",
          attempt_id: "attempt-1",
          attempt_phase: "running",
          runner_instance_id: "runner-instance-1",
          runner_instance_status: "online",
          runner_pool_id: "runner-pool-1",
          access: {
            port_forward: false,
            exec: true,
          },
        },
        access: {
          reachable: true,
          reason: "reachable",
          port_forward: false,
          exec: true,
          runner_instance_id: "runner-instance-1",
          attempt_id: "attempt-1",
          worker_id: "worker-1",
        },
      },
    });
    expect(runtimeConditionsByType(scheduleSlot!).RunnerBound).toMatchObject({
      status: "True",
      reason: "Bound",
    });
    expect(runtimeConditionsByType(scheduleSlot!).Reachable).toMatchObject({
      status: "True",
    });

    const runManifest = inventory.resources.find((resource) => resource.kind === "RunManifest");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["RunManifest"],
      labelSelector: "bucephalus.dev/experiment-id=experiment-1,bucephalus.dev/workload-type=benchmark",
      fieldSelector: "status.variants_total=2",
    })).toEqual([runManifest!]);
    expect(runManifest).toMatchObject({
      kind: "RunManifest",
      metadata: {
        name: "core-run-1",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/experiment-id": "experiment-1",
          "bucephalus.dev/runtime-status": "running",
          "bucephalus.dev/workload-type": "benchmark",
          "bucephalus.dev/baseline-id": "baseline-a",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
      },
      spec: {
        core_run_id: "core-run-1",
      },
      status: expect.objectContaining({
        phase: "Current",
        experiment_id: "experiment-1",
        runtime_status: "running",
        workload_type: "benchmark",
        baseline_id: "baseline-a",
        variants_total: 2,
        project_root: "/workspace/project",
        run_dir: "/workspace/project/.buc/runs/core-run-1",
        artifact_root: "/workspace/project/.buc/runs/core-run-1/artifacts",
        observed_at: "2027-02-07T00:00:00.000Z",
        updated_at_ms: 1_801_958_400_000,
        manifest: expect.objectContaining({
          schema_version: "run_manifest_v1",
          workload_type: "benchmark",
        }),
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.run_manifests",
      }),
    });

    const metricDefinition = inventory.resources.find((resource) => resource.kind === "MetricDefinition");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["MetricDefinition"],
      labelSelector: "bucephalus.dev/metric-id=pass@1,bucephalus.dev/primary-metric=true",
      fieldSelector: "status.primary_metric=true",
    })).toEqual([metricDefinition!]);
    expect(metricDefinition).toMatchObject({
      kind: "MetricDefinition",
      metadata: {
        name: "experiment-1.pass-1",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/experiment-id": "experiment-1",
          "bucephalus.dev/metric-id": "pass@1",
          "bucephalus.dev/semantic-key": "accuracy.pass_at_1",
          "bucephalus.dev/source-type": "json_pointer",
          "bucephalus.dev/direction": "maximize",
          "bucephalus.dev/required": "true",
          "bucephalus.dev/primary-metric": "true",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
          expect.objectContaining({ kind: "RunManifest", name: "core-run-1" }),
        ]),
      },
      spec: {
        experiment_id: "experiment-1",
        metric_id: "pass@1",
        semantic_key: "accuracy.pass_at_1",
        source_type: "json_pointer",
        source_pointer: "/metrics/pass@1",
      },
      status: expect.objectContaining({
        phase: "Primary",
        label: "Pass@1",
        value_type: "number",
        unit: "ratio",
        direction: "maximize",
        required: true,
        primary_metric: true,
        observed_at: "2027-02-07T00:00:05.000Z",
        updated_at_ms: 1_801_958_405_000,
        definition: expect.objectContaining({
          id: "pass@1",
          semantic_key: "accuracy.pass_at_1",
        }),
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.metric_definitions",
      }),
    });

    const slotCommit = inventory.resources.find((resource) => resource.kind === "SlotCommit");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["SlotCommit"],
      labelSelector: "bucephalus.dev/record-type=commit,bucephalus.dev/slot-commit-id=slot-commit-1",
      fieldSelector: "status.slot_status=passed",
    })).toEqual([slotCommit!]);
    expect(slotCommit).toMatchObject({
      kind: "SlotCommit",
      metadata: {
        name: "core-run-1.0.1.commit",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/schedule-idx": "0",
          "bucephalus.dev/attempt": "1",
          "bucephalus.dev/record-type": "commit",
          "bucephalus.dev/slot-commit-id": "slot-commit-1",
          "bucephalus.dev/slot-status": "passed",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
          expect.objectContaining({ kind: "ScheduleSlot", name: "core-run-1.0", uid: "core-run-1:0" }),
        ]),
      },
      spec: {
        core_run_id: "core-run-1",
        schedule_idx: 0,
        attempt: 1,
        record_type: "commit",
        slot_commit_id: "slot-commit-1",
      },
      status: expect.objectContaining({
        phase: "Committed",
        trial_id: "trial-1",
        slot_status: "passed",
        recorded_at: "2026-06-04T00:00:20Z",
        written_rows: expect.objectContaining({
          trials: 1,
          metrics: 1,
          events: 2,
          contract_stages: 3,
        }),
        facts_fsync_completed: true,
        runtime_fsync_completed: true,
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.slot_commit_records",
      }),
    });

    const pendingCompletion = inventory.resources.find((resource) => resource.kind === "PendingTrialCompletion");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["PendingTrialCompletion"],
      labelSelector: "bucephalus.dev/schedule-idx=2,bucephalus.dev/trial-id=trial-pending-1",
      fieldSelector: "status.deferred_rows.total=3",
    })).toEqual([pendingCompletion!]);
    expect(pendingCompletion).toMatchObject({
      kind: "PendingTrialCompletion",
      metadata: {
        name: "core-run-1.2",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/trial-id": "trial-pending-1",
          "bucephalus.dev/schedule-idx": "2",
          "bucephalus.dev/slot-status": "passed",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-pending-1", uid: "core-run-1:trial-pending-1" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
          expect.objectContaining({ kind: "ScheduleSlot", name: "core-run-1.2", uid: "core-run-1:2" }),
        ]),
      },
      spec: {
        core_run_id: "core-run-1",
        schedule_idx: 2,
      },
      status: expect.objectContaining({
        phase: "Pending",
        trial_id: "trial-pending-1",
        slot_status: "passed",
        updated_at_ms: 1_801_958_430_000,
        deferred_rows: expect.objectContaining({
          trials: 1,
          metrics: 1,
          events: 1,
          total: 3,
        }),
        trial_result: expect.objectContaining({
          trial_id: "trial-pending-1",
        }),
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.pending_trial_completions",
      }),
    });

    const variantSnapshot = inventory.resources.find((resource) => resource.kind === "VariantSnapshot");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["VariantSnapshot"],
      labelSelector: "bucephalus.dev/binding-name=prompt,bucephalus.dev/slot-commit-id=slot-commit-1",
      fieldSelector: "spec.binding_name=prompt",
    })).toEqual([variantSnapshot!]);
    expect(variantSnapshot).toMatchObject({
      kind: "VariantSnapshot",
      metadata: {
        name: "core-run-1.0.1.0.prompt",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/schedule-idx": "0",
          "bucephalus.dev/attempt": "1",
          "bucephalus.dev/row-seq": "0",
          "bucephalus.dev/slot-commit-id": "slot-commit-1",
          "bucephalus.dev/variant-id": "variant-a",
          "bucephalus.dev/baseline-id": "baseline-a",
          "bucephalus.dev/task-id": "task-a",
          "bucephalus.dev/binding-name": "prompt",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
          expect.objectContaining({ kind: "ScheduleSlot", name: "core-run-1.0", uid: "core-run-1:0" }),
          expect.objectContaining({ kind: "SlotCommit", name: "core-run-1.0.1.commit" }),
        ]),
      },
      spec: expect.objectContaining({
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        schedule_idx: 0,
        attempt: 1,
        row_seq: 0,
        slot_commit_id: "slot-commit-1",
        variant_id: "variant-a",
        baseline_id: "baseline-a",
        task_id: "task-a",
        binding_name: "prompt",
        binding_value: { temperature: 0.2 },
      }),
      status: {
        phase: "Committed",
        binding_value_text: "{\"temperature\":0.2}",
        row: expect.objectContaining({
          binding_name: "prompt",
        }),
      },
      audit: expect.objectContaining({
        source: "bucephalus_runtime.variant_snapshot_rows",
      }),
    });

    const evidenceRecord = inventory.resources.find((resource) => resource.kind === "EvidenceRecord");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["EvidenceRecord"],
      labelSelector: "bucephalus.dev/record-kind=grader_report",
      fieldSelector: "status.trial_id=trial-1",
    })).toEqual([evidenceRecord!]);
    expect(evidenceRecord).toMatchObject({
      kind: "EvidenceRecord",
      metadata: {
        name: "core-run-1.0.1.0.evidencerecord",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/schedule-idx": "0",
          "bucephalus.dev/attempt": "1",
          "bucephalus.dev/row-seq": "0",
          "bucephalus.dev/slot-commit-id": "slot-commit-1",
          "bucephalus.dev/schema-version": "evidence_record_v1",
          "bucephalus.dev/record-kind": "grader_report",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
          expect.objectContaining({ kind: "ScheduleSlot", name: "core-run-1.0", uid: "core-run-1:0" }),
          expect.objectContaining({ kind: "SlotCommit", name: "core-run-1.0.1.commit" }),
        ]),
      },
      spec: {
        core_run_id: "core-run-1",
        schedule_idx: 0,
        attempt: 1,
        row_seq: 0,
        slot_commit_id: "slot-commit-1",
      },
      status: expect.objectContaining({
        phase: "Committed",
        trial_id: "trial-1",
        schema_version: "evidence_record_v1",
        record_kind: "grader_report",
        row: expect.objectContaining({
          kind: "grader_report",
        }),
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.evidence_rows",
      }),
    });

    const chainState = inventory.resources.find((resource) => resource.kind === "ChainState");
    expect(chainState).toMatchObject({
      kind: "ChainState",
      metadata: {
        name: "core-run-1.0.1.0.chainstate",
        labels: expect.objectContaining({
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/schema-version": "chain_state_v1",
          "bucephalus.dev/record-kind": "lineage_state",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "SlotCommit", name: "core-run-1.0.1.commit" }),
        ]),
      },
      status: expect.objectContaining({
        phase: "Committed",
        trial_id: "trial-1",
        schema_version: "chain_state_v1",
        record_kind: "lineage_state",
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.chain_state_rows",
      }),
    });

    const trialConclusion = inventory.resources.find((resource) => resource.kind === "TrialConclusion");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["TrialConclusion"],
      labelSelector: "bucephalus.dev/outcome=passed",
      fieldSelector: "status.status=completed",
    })).toEqual([trialConclusion!]);
    expect(trialConclusion).toMatchObject({
      kind: "TrialConclusion",
      metadata: {
        name: "core-run-1.0.1.0.trialconclusion",
        labels: expect.objectContaining({
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/schema-version": "trial_conclusion_v1",
          "bucephalus.dev/record-kind": "trial_conclusion_v1",
          "bucephalus.dev/outcome": "passed",
          "bucephalus.dev/status": "completed",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "SlotCommit", name: "core-run-1.0.1.commit" }),
        ]),
      },
      status: expect.objectContaining({
        phase: "Committed",
        trial_id: "trial-1",
        schema_version: "trial_conclusion_v1",
        record_kind: "trial_conclusion_v1",
        outcome: "passed",
        status: "completed",
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.trial_conclusion_rows",
      }),
    });

    const lineageVersion = inventory.resources.find((resource) => resource.kind === "LineageVersion");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["LineageVersion"],
      labelSelector: "bucephalus.dev/chain-key=agent.workspace,bucephalus.dev/version-id=lineage-version-1",
      fieldSelector: "status.current_head=true",
    })).toEqual([lineageVersion!]);
    expect(lineageVersion).toMatchObject({
      kind: "LineageVersion",
      metadata: {
        name: "agent.workspace.4.lineage-version-1",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/chain-key": "agent.workspace",
          "bucephalus.dev/version-id": "lineage-version-1",
          "bucephalus.dev/parent-version-id": "lineage-version-0",
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/step-index": "4",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
      },
      spec: {
        core_run_id: "core-run-1",
        chain_key: "agent.workspace",
        version_id: "lineage-version-1",
        step_index: 4,
        trial_id: "trial-1",
        parent_version_id: "lineage-version-0",
      },
      status: expect.objectContaining({
        phase: "Current",
        current_head: true,
        pre_snapshot_ref: "runtime://snapshots/pre",
        post_snapshot_ref: "runtime://snapshots/post",
        diff_incremental_ref: "runtime://diffs/inc",
        diff_cumulative_ref: "runtime://diffs/cum",
        patch_incremental_ref: "runtime://patches/inc",
        patch_cumulative_ref: "runtime://patches/cum",
        workspace_ref: "runtime://workspace/post",
        checkpoint_labels: {
          branch: "main",
        },
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.lineage_versions",
      }),
    });

    const lineageHead = inventory.resources.find((resource) => resource.kind === "LineageHead");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["LineageHead"],
      labelSelector: "bucephalus.dev/latest-version-id=lineage-version-1",
      fieldSelector: "status.step_index=4",
    })).toEqual([lineageHead!]);
    expect(lineageHead).toMatchObject({
      kind: "LineageHead",
      metadata: {
        name: "agent.workspace",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/chain-key": "agent.workspace",
          "bucephalus.dev/latest-version-id": "lineage-version-1",
          "bucephalus.dev/step-index": "4",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
      },
      spec: {
        core_run_id: "core-run-1",
        chain_key: "agent.workspace",
      },
      status: {
        phase: "Current",
        latest_version_id: "lineage-version-1",
        step_index: 4,
        latest_workspace_ref: "runtime://workspace/post",
      },
      audit: expect.objectContaining({
        source: "bucephalus_runtime.lineage_heads",
      }),
    });

    const trialContainer = inventory.resources.find((resource) => resource.kind === "TrialContainer");
    expect(trialContainer).toMatchObject({
      metadata: {
        name: "trial-1.agent.container-1",
        labels: {
          "bucephalus.dev/worker-id": "worker-1",
          "bucephalus.dev/attempt-id": "attempt-1",
          "bucephalus.dev/runner-instance-id": "runner-instance-1",
          "bucephalus.dev/runner-pool-id": "runner-pool-1",
        },
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "ScheduleSlot", name: "core-run-1.0", uid: "core-run-1:0" }),
          expect.objectContaining({ kind: "RunnerPool", name: "runner-pool-1", uid: "runner-pool-1" }),
          expect.objectContaining({ kind: "RunnerInstance", name: "runner-instance-1", uid: "runner-instance-1" }),
          expect.objectContaining({ kind: "RunnerAttempt", name: "attempt-1", uid: "attempt-1" }),
        ]),
      },
      status: {
        schedule_slot: "core-run-1.0",
        worker_id: "worker-1",
        runner_binding: {
          worker_id: "worker-1",
          attempt_id: "attempt-1",
          attempt_phase: "running",
          runner_instance_id: "runner-instance-1",
          runner_instance_status: "online",
          runner_pool_id: "runner-pool-1",
          access: {
            port_forward: false,
            exec: true,
          },
        },
        access: {
          reachable: true,
          reason: "reachable",
          port_forward: false,
          exec: true,
          runner_instance_id: "runner-instance-1",
          attempt_id: "attempt-1",
          worker_id: "worker-1",
        },
      },
    });
    expect(runtimeConditionsByType(trialContainer!).RunnerBound).toMatchObject({
      status: "True",
      reason: "Bound",
    });
    expect(runtimeConditionsByType(trialContainer!).Reachable).toMatchObject({
      status: "True",
    });
    expect(filterRuntimeResources([trial!, scheduleSlot!, trialContainer!], {
      fieldSelector: "status.access.exec=true,status.access.reachable=true",
    })).toEqual([trial!, scheduleSlot!, trialContainer!]);

    const trialStage = inventory.resources.find((resource) => resource.kind === "TrialStage");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["TrialStage"],
      labelSelector: "bucephalus.dev/stage=agent_execution",
      fieldSelector: "status.status=ok",
    })).toEqual([trialStage!]);
    expect(trialStage).toMatchObject({
      kind: "TrialStage",
      metadata: {
        name: "trial-1.0.agent_execution",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/stage": "agent_execution",
          "bucephalus.dev/attempt": "0",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
      },
      spec: expect.objectContaining({
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        attempt: 0,
        stage: "agent_execution",
      }),
      status: expect.objectContaining({
        phase: "ok",
        status: "ok",
        recorded_at: "2026-06-04T00:00:10Z",
        detail: {
          status: "ok",
          duration_ms: 1200,
        },
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.contract_stage_rows",
        observed_at: "2026-06-04T00:00:10Z",
      }),
    });

    const runtimeValue = inventory.resources.find((resource) => resource.kind === "RuntimeValue");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["RuntimeValue"],
      labelSelector: "bucephalus.dev/runtime-key=run_control_v2",
      fieldSelector: "status.value.status=running",
    })).toEqual([runtimeValue!]);
    expect(runtimeValue).toMatchObject({
      kind: "RuntimeValue",
      metadata: {
        name: "core-run-1.run_control_v2",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/runtime-key": "run_control_v2",
          "bucephalus.dev/value-source": "bucephalus_runtime.runtime_kv",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
        created_at: "2027-02-04T00:00:00.000Z",
        updated_at: "2027-02-04T00:00:00.000Z",
      },
      spec: {
        core_run_id: "core-run-1",
        key: "run_control_v2",
      },
      status: expect.objectContaining({
        phase: "Current",
        source: "bucephalus_runtime.runtime_kv",
        observed_at: "2027-02-04T00:00:00.000Z",
        value_summary: "status=running, active_trials=1",
        value: {
          status: "running",
          active_trials: 1,
        },
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.runtime_kv",
      }),
    });

    const metricObservation = inventory.resources.find((resource) => resource.kind === "MetricObservation");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["MetricObservation"],
      labelSelector: "bucephalus.dev/metric-name=pass@1",
      fieldSelector: "status.metric_value=0.91",
    })).toEqual([metricObservation!]);
    expect(metricObservation).toMatchObject({
      kind: "MetricObservation",
      metadata: {
        name: "trial-1.7.pass-1",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/metric-name": "pass@1",
          "bucephalus.dev/metric-source": "bucephalus_runtime.metric_rows",
          "bucephalus.dev/attempt": "0",
          "bucephalus.dev/variant-id": "variant-a",
          "bucephalus.dev/task-id": "task-a",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
      },
      spec: expect.objectContaining({
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        schedule_idx: 0,
        attempt: 0,
        row_seq: 7,
        variant_id: "variant-a",
        task_id: "task-a",
        repl_idx: 0,
        outcome: "success",
        metric_name: "pass@1",
      }),
      status: expect.objectContaining({
        phase: "observed",
        metric_value: 0.91,
        metric_source: "bucephalus_runtime.metric_rows",
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.metric_rows",
        row: {
          source: "runtime.metric_rows",
        },
      }),
    });

    const performanceSample = inventory.resources.find((resource) => resource.kind === "PerformanceSample");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["PerformanceSample"],
      labelSelector: "bucephalus.dev/sample-kind=duration,bucephalus.dev/stage=agent_execution",
      fieldSelector: "status.duration_ms=42.5",
    })).toEqual([performanceSample!]);
    expect(performanceSample).toMatchObject({
      kind: "PerformanceSample",
      metadata: {
        name: "agent_execution.duration.perf-1",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/sample-kind": "duration",
          "bucephalus.dev/stage": "agent_execution",
          "bucephalus.dev/attempt": "0",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
      },
      spec: expect.objectContaining({
        core_run_id: "core-run-1",
        sample_id: "perf-1",
        trial_id: "trial-1",
        schedule_idx: 0,
        attempt: 0,
        sample_seq: 3,
        sample_kind: "duration",
        stage: "agent_execution",
      }),
      status: expect.objectContaining({
        phase: "Recorded",
        duration_ms: 42.5,
        process_rss_kb: 131072,
        recorded_at_ms: 1_801_958_400_000,
        payload: {
          stage: "agent_execution",
          sample_kind: "duration",
          duration_ms: 42.5,
        },
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.performance_samples",
      }),
    });

    const runtimeOperation = inventory.resources.find((resource) => resource.kind === "RuntimeOperation");
    expect(filterRuntimeResources(inventory.resources, {
      kinds: ["RuntimeOperation"],
      labelSelector: "bucephalus.dev/op-kind=replay,bucephalus.dev/parent-trial-id=trial-1",
      fieldSelector: "status.operation=replay",
    })).toEqual([runtimeOperation!]);
    expect(runtimeOperation).toMatchObject({
      kind: "RuntimeOperation",
      metadata: {
        name: "replay.replay-1",
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/op-kind": "replay",
          "bucephalus.dev/op-id": "replay-1",
          "bucephalus.dev/trial-id": "trial-1-replay",
          "bucephalus.dev/parent-trial-id": "trial-1",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1-replay", uid: "core-run-1:trial-1-replay" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
      },
      spec: {
        core_run_id: "core-run-1",
        op_kind: "replay",
        op_id: "replay-1",
      },
      status: expect.objectContaining({
        phase: "Recorded",
        operation: "replay",
        trial_id: "trial-1-replay",
        parent_trial_id: "trial-1",
        strict: true,
        replay_grade: "trusted",
        integration_level: "full",
        payload: expect.objectContaining({ replay_id: "replay-1" }),
      }),
      audit: expect.objectContaining({
        source: "bucephalus_runtime.runtime_ops",
      }),
    });

    const trialArtifact = inventory.resources.find((resource) => resource.kind === "TrialArtifact");
    expect(filterRuntimeResources(inventory.resources, {
      labelSelector: "bucephalus.dev/artifact-role=agent_result",
      fieldSelector: "kind=TrialArtifact,status.content_available=true",
    })).toEqual([trialArtifact!]);
    expect(trialArtifact).toMatchObject({
      kind: "TrialArtifact",
      metadata: {
        labels: expect.objectContaining({
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/trial-id": "trial-1",
          "bucephalus.dev/artifact-role": "agent_result",
          "bucephalus.dev/attempt": "0",
          "bucephalus.dev/sha256": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Trial", name: "trial-1", uid: "core-run-1:trial-1" }),
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
        ]),
      },
      spec: expect.objectContaining({
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        attempt: 0,
        role: "agent_result",
        object_ref: "artifact://sha256/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      }),
      status: expect.objectContaining({
        phase: "recorded",
        content_available: true,
        media_type: "application/json; charset=utf-8",
        sha256: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      }),
      audit: {
        source: "bucephalus_runtime.attempt_objects",
        observed_at: "2027-02-07T00:00:00.000Z",
      },
    });

    expect(inventory.resources.filter((resource) => resource.kind === "PortForward" || resource.kind === "Exec").map((resource) => ({
      kind: resource.kind,
      uid: resource.metadata.uid,
      annotations: resource.metadata.annotations,
      ownerReferences: resource.metadata.ownerReferences,
      spec: resource.spec,
      status: resource.status,
    }))).toEqual([
      {
        kind: "PortForward",
        uid: "pf-1",
        annotations: expect.objectContaining({
          "bucephalus.dev/access-request-id": "pf-1",
          "bucephalus.dev/access-target": "TrialContainer/trial-1.agent.container-1",
          "bucephalus.dev/access-target-uid": "trial-1.agent.container-1",
          "bucephalus.dev/access-target-resource-version": "sha256:trial-container",
          "bucephalus.dev/audit-source": "cloud.runtime_access_requests",
          "bucephalus.dev/phase": "requested",
          "bucephalus.dev/reason": "debug",
          "bucephalus.dev/requester": "issuer:user-a",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
          expect.objectContaining({ kind: "TrialContainer", name: "trial-1.agent.container-1" }),
          expect.objectContaining({ kind: "RunnerInstance", name: "runner-instance-1", uid: "runner-instance-1" }),
          expect.objectContaining({ kind: "RunnerAttempt", name: "attempt-1", uid: "attempt-1" }),
        ]),
        spec: {
          access_request_id: "pf-1",
          resource_kind: "TrialContainer",
          resource_name: "trial-1.agent.container-1",
          target_ref: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "TrialContainer",
            name: "trial-1.agent.container-1",
            uid: "trial-1.agent.container-1",
            resourceVersion: "sha256:trial-container",
          },
          protocol: "tcp",
          target_port: 8080,
          reason: "debug",
        },
        status: expect.objectContaining({
          phase: "requested",
          actions: ["cancel"],
          runner_binding: {
            runner_instance_id: "runner-instance-1",
            attempt_id: "attempt-1",
            worker_id: "worker-1",
          },
          conditions: expect.arrayContaining([
            expect.objectContaining({
              type: "ClientReachable",
              status: "Unknown",
              reason: "Requested",
            }),
          ]),
        }),
      },
      {
        kind: "Exec",
        uid: "exec-1",
        annotations: expect.objectContaining({
          "bucephalus.dev/access-request-id": "exec-1",
          "bucephalus.dev/access-target": "TrialContainer/trial-1.agent.container-1",
          "bucephalus.dev/access-target-uid": "trial-1.agent.container-1",
          "bucephalus.dev/access-target-resource-version": "sha256:trial-container",
          "bucephalus.dev/audit-source": "cloud.runtime_access_requests",
          "bucephalus.dev/phase": "requested",
          "bucephalus.dev/reason": "debug",
          "bucephalus.dev/requester": "issuer:user-a",
        }),
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
          expect.objectContaining({ kind: "TrialContainer", name: "trial-1.agent.container-1" }),
          expect.objectContaining({ kind: "RunnerInstance", name: "runner-instance-1", uid: "runner-instance-1" }),
          expect.objectContaining({ kind: "RunnerAttempt", name: "attempt-1", uid: "attempt-1" }),
        ]),
        spec: {
          access_request_id: "exec-1",
          resource_kind: "TrialContainer",
          resource_name: "trial-1.agent.container-1",
          target_ref: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "TrialContainer",
            name: "trial-1.agent.container-1",
            uid: "trial-1.agent.container-1",
            resourceVersion: "sha256:trial-container",
          },
          protocol: "exec",
          command: ["python", "-V"],
          reason: "debug",
        },
        status: expect.objectContaining({
          phase: "requested",
          actions: ["cancel"],
          runner_binding: {
            runner_instance_id: "runner-instance-1",
            attempt_id: "attempt-1",
            worker_id: "worker-1",
          },
        }),
      },
    ]);
  });

  test("distinguishes active port-forward lifecycle from client reachability", async () => {
    const run = cloudRunRecord();
    const inventory = await RuntimeRepository.prototype.resources.call({
      async coreRunIdsForCloudRun() {
        return [];
      },
      async workerRuntimeSnapshots() {
        return [];
      },
      async runAttempts() {
        return [];
      },
      async provisionRequests() {
        return [];
      },
      async runnerPools() {
        return [];
      },
      async listPortForwards() {
        return [
          accessRequestRecord({
            access_request_id: "pf-local",
            status: "active",
            connection: { local_port: 18080 },
          }),
          accessRequestRecord({
            access_request_id: "pf-client",
            status: "active",
            connection: { client_endpoint: "tcp://127.0.0.1:18080" },
          }),
        ];
      },
      async listExecRequests() {
        return [];
      },
      async scheduleSlots() {
        return [];
      },
      async trialContainers() {
        return [];
      },
    } as any, "run-1", run, { kinds: ["PortForward"] });

    const localOnly = inventory.resources.find((resource) => resource.metadata.name === "pf-local");
    const clientReachable = inventory.resources.find((resource) => resource.metadata.name === "pf-client");

    expect(runtimeConditionsByType(localOnly!).Active).toMatchObject({
      status: "True",
      reason: "TunnelActive",
    });
    expect(runtimeConditionsByType(localOnly!).ClientReachable).toMatchObject({
      status: "False",
      reason: "ClientEndpointMissing",
      message: "PortForward is active but has not reported a client-reachable endpoint",
    });
    expect(runtimeConditionsByType(clientReachable!).ClientReachable).toMatchObject({
      status: "True",
      reason: "ClientEndpointReported",
    });
  });

  test("surfaces active port-forwards without client endpoints as degraded health", () => {
    const inventory = {
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceList",
      metadata: runtimeResourceListMetadataFixture(),
      cloud_run_id: "run-1",
      core_run_ids: [],
      resources: [
        runtimeResourceFixture({
          kind: "PortForward",
          name: "pf-local",
          status: {
            phase: "active",
            connection: { local_port: 18080 },
            conditions: [
              runtimeCondition("Ready", "True", "Active", "Resource phase is active"),
              runtimeCondition("Active", "True", "TunnelActive", "PortForward tunnel is active"),
              runtimeCondition("ClientReachable", "False", "ClientEndpointMissing", "PortForward is active but has not reported a client-reachable endpoint"),
            ],
          },
        }),
      ],
    } satisfies Parameters<typeof runtimeResourceHealthSummary>[0];

    const health = runtimeResourceHealthSummary(inventory);

    expect(health.summary).toMatchObject({
      total: 1,
      degraded: 1,
      problem: 0,
    });
    expect(health.resources[0]).toMatchObject({
      resource: "PortForward/pf-local",
      health: "degraded",
      condition_summary: "ClientReachable=False ClientEndpointMissing",
      reason: "ClientEndpointMissing",
      message: "PortForward is active but has not reported a client-reachable endpoint",
      degraded_conditions: [
        expect.objectContaining({ type: "ClientReachable", status: "False" }),
      ],
    });
  });

  test("discovers runtime API resources with verbs, subresources, and live counts", () => {
    const discovery = runtimeApiResourceList("run-1", [
      runtimeResourceFixture({
        kind: "CoreRun",
        name: "core-run-1",
      }),
      runtimeResourceFixture({
        kind: "RunnerInstance",
        name: "runner-1",
      }),
      runtimeResourceFixture({
        kind: "RunnerAttempt",
        name: "attempt-1",
      }),
      runtimeResourceFixture({
        kind: "Trial",
        name: "trial-1",
      }),
      runtimeResourceFixture({
        kind: "TrialContainer",
        name: "trial-1.agent.container-1",
      }),
      runtimeResourceFixture({
        kind: "TrialContainer",
        name: "trial-2.agent.container-1",
      }),
      runtimeResourceFixture({
        kind: "TrialStage",
        name: "trial-1.0.agent-execution",
      }),
      runtimeResourceFixture({
        kind: "SlotCommit",
        name: "core-run-1.0.1.commit",
      }),
      runtimeResourceFixture({
        kind: "PendingTrialCompletion",
        name: "core-run-1.2",
      }),
      runtimeResourceFixture({
        kind: "RunManifest",
        name: "core-run-1",
      }),
      runtimeResourceFixture({
        kind: "MetricDefinition",
        name: "experiment-1.pass-1",
      }),
      runtimeResourceFixture({
        kind: "VariantSnapshot",
        name: "core-run-1.0.1.0.prompt",
      }),
      runtimeResourceFixture({
        kind: "EvidenceRecord",
        name: "core-run-1.0.1.0.evidencerecord",
      }),
      runtimeResourceFixture({
        kind: "ChainState",
        name: "core-run-1.0.1.0.chainstate",
      }),
      runtimeResourceFixture({
        kind: "TrialConclusion",
        name: "core-run-1.0.1.0.trialconclusion",
      }),
      runtimeResourceFixture({
        kind: "LineageVersion",
        name: "agent.workspace.4.lineage-version-1",
      }),
      runtimeResourceFixture({
        kind: "LineageHead",
        name: "agent.workspace",
      }),
      runtimeResourceFixture({
        kind: "RuntimeValue",
        name: "core-run-1.run-control-v2",
      }),
      runtimeResourceFixture({
        kind: "MetricObservation",
        name: "trial-1.7.pass-1",
      }),
      runtimeResourceFixture({
        kind: "PerformanceSample",
        name: "agent-execution.duration.perf-1",
      }),
      runtimeResourceFixture({
        kind: "RuntimeOperation",
        name: "replay.replay-1",
      }),
      runtimeResourceFixture({
        kind: "TrialArtifact",
        name: "trial-1.agent-result.sha256-bbbbbbbbbbbbbbbbbbbbbb",
      }),
      runtimeResourceFixture({
        kind: "PortForward",
        name: "pf-1",
      }),
      runtimeResourceFixture({
        kind: "Event",
        name: "event-runtime-access",
      }),
    ]);

    expect(discovery).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeApiResourceList",
      cloud_run_id: "run-1",
    });
    expect(discovery.resources.find((resource) => resource.kind === "CoreRun")).toMatchObject({
      group: "bucephalus.dev",
      version: "v1alpha1",
      name: "coreruns",
      singularName: "corerun",
      shortNames: ["core"],
      categories: expect.arrayContaining(["core", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics"],
      access: [],
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=CoreRun",
        resource: "/v1/runs/{run_id}/runtime/resources/CoreRun/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/CoreRun/{name}",
        operationReview: "/v1/runs/{run_id}/runtime/resources/CoreRun/{name}/operations/{operation}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=CoreRun",
      }),
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind CoreRun" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} CoreRun/{name}" },
        { purpose: "status", command: "bucephalus-cloud run status {run_id} CoreRun/{name}" },
        { purpose: "wait", command: "bucephalus-cloud run wait {run_id} CoreRun/{name} --for condition=Ready" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "CoreRun", type: "string", jsonPath: ".spec.core_run_id", description: expect.any(String), priority: 0 },
        { name: "Experiment", type: "string", jsonPath: ".status.experiment_id", description: expect.any(String), priority: 0 },
        { name: "Status", type: "string", jsonPath: ".status.runtime_status", description: expect.any(String), priority: 0 },
        { name: "RunDir", type: "string", jsonPath: ".status.run_dir", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/run-id",
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/experiment-id",
        "bucephalus.dev/runtime-status",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "RunnerInstance")).toMatchObject({
      group: "bucephalus.dev",
      version: "v1alpha1",
      name: "runnerinstances",
      singularName: "runnerinstance",
      scope: "run",
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics", "logs", "actions/cordon", "actions/drain", "actions/uncordon", "port-forward", "exec"],
      actions: ["cordon", "drain", "uncordon"],
      access: ["logs", "port-forward", "exec"],
      supports: {
        list: true,
        get: true,
        watch: true,
        describe: true,
        create: false,
        delete: false,
        actions: true,
        access: true,
        labelSelector: true,
        fieldSelector: true,
      },
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=RunnerInstance",
        resource: "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}",
        operationReview: "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/operations/{operation}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=RunnerInstance",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/metrics",
          logs: "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/logs",
          "actions/cordon": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/actions/cordon",
          "actions/drain": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/actions/drain",
          "actions/uncordon": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/actions/uncordon",
          "port-forward": "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/port-forward",
          exec: "/v1/runs/{run_id}/runtime/resources/RunnerInstance/{name}/exec",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind RunnerInstance" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} RunnerInstance/{name}" },
        { purpose: "describe", command: "bucephalus-cloud run describe {run_id} RunnerInstance/{name}" },
        { purpose: "status", command: "bucephalus-cloud run status {run_id} RunnerInstance/{name}" },
        { purpose: "wait", command: "bucephalus-cloud run wait {run_id} RunnerInstance/{name} --for condition=Ready" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} RunnerInstance/{name} --follow" },
        { purpose: "logs/stdout", command: "bucephalus-cloud run logs {run_id} RunnerInstance/{name} --stream stdout --follow" },
        { purpose: "port-forward", command: "bucephalus-cloud run port-forward {run_id} RunnerInstance/{name} --target-port PORT" },
        { purpose: "exec", command: "bucephalus-cloud run exec {run_id} RunnerInstance/{name} -- COMMAND [ARG...]" },
        { purpose: "cordon", command: "bucephalus-cloud run cordon {run_id} RunnerInstance/{name}" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Name", type: "string", jsonPath: ".metadata.name", description: expect.any(String), priority: 0 },
        { name: "Phase", type: "string", jsonPath: ".status.phase", description: expect.any(String), priority: 0 },
        { name: "Ready", type: "string", jsonPath: '.status.conditions[?(@.type=="Ready")].status', description: expect.any(String), priority: 0 },
        { name: "Observed", type: "string", jsonPath: '.status.conditions[?(@.type=="Observed")].status', description: expect.any(String), priority: 0 },
        { name: "Reachable", type: "boolean", jsonPath: ".status.access.reachable", description: expect.any(String), priority: 0 },
        { name: "PortForward", type: "boolean", jsonPath: ".status.access.port_forward", description: expect.any(String), priority: 0 },
        { name: "Exec", type: "boolean", jsonPath: ".status.access.exec", description: expect.any(String), priority: 0 },
        { name: "Provider", type: "string", jsonPath: ".status.provider", description: expect.any(String), priority: 0 },
        { name: "VM", type: "string", jsonPath: ".status.instance_name", description: expect.any(String), priority: 0 },
        { name: "Zone", type: "string", jsonPath: ".status.zone", description: expect.any(String), priority: 1 },
        { name: "ProviderID", type: "string", jsonPath: ".status.provider_instance_id", description: expect.any(String), priority: 1 },
        { name: "RunnerStatus", type: "string", jsonPath: ".status.access.runner_instance_status", description: expect.any(String), priority: 1 },
        { name: "Actions", type: "string", jsonPath: ".status.actions", description: expect.any(String), priority: 1 },
      ]),
      fieldSelectors: expect.arrayContaining([
        "status.access.reachable",
        "status.access.port_forward",
        "status.access.exec",
        "status.access.runner_instance_id",
        "status.access.runner_instance_status",
        "status.access.attempt_id",
        "status.access.worker_id",
        "status.provider",
        "status.provider_instance_id",
        "status.instance_name",
        "status.project_id",
        "status.zone",
        "status.conditions.<type>",
        "status.conditions.<type>.status",
        "status.conditions.<type>.reason",
        "status.conditions.<type>.message",
        "status.<path>",
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/run-id",
        "bucephalus.dev/runner-instance-id",
        "bucephalus.dev/runner-pool-id",
        "bucephalus.dev/current-attempt-id",
        "bucephalus.dev/provider",
        "topology.kubernetes.io/zone",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "RunnerAttempt")).toMatchObject({
      subresources: ["status", "events", "metrics", "logs", "port-forward", "exec"],
      access: ["logs", "port-forward", "exec"],
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=RunnerAttempt",
        resource: "/v1/runs/{run_id}/runtime/resources/RunnerAttempt/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/RunnerAttempt/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=RunnerAttempt",
        subresources: expect.objectContaining({
          logs: "/v1/runs/{run_id}/runtime/resources/RunnerAttempt/{name}/logs",
          "port-forward": "/v1/runs/{run_id}/runtime/resources/RunnerAttempt/{name}/port-forward",
          exec: "/v1/runs/{run_id}/runtime/resources/RunnerAttempt/{name}/exec",
        }),
      }),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "Trial")).toMatchObject({
      name: "trials",
      singularName: "trial",
      shortNames: ["trial"],
      categories: expect.arrayContaining(["trial", "access-target"]),
      subresources: ["status", "events", "metrics", "logs", "port-forward", "exec"],
      access: ["logs", "port-forward", "exec"],
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=Trial",
        resource: "/v1/runs/{run_id}/runtime/resources/Trial/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/Trial/{name}",
        operationReview: "/v1/runs/{run_id}/runtime/resources/Trial/{name}/operations/{operation}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=Trial",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/Trial/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/Trial/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/Trial/{name}/metrics",
          logs: "/v1/runs/{run_id}/runtime/resources/Trial/{name}/logs",
          "port-forward": "/v1/runs/{run_id}/runtime/resources/Trial/{name}/port-forward",
          exec: "/v1/runs/{run_id}/runtime/resources/Trial/{name}/exec",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind Trial" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} Trial/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} Trial/{name} --follow" },
        { purpose: "logs/stdout", command: "bucephalus-cloud run logs {run_id} Trial/{name} --stream stdout --follow" },
        { purpose: "port-forward", command: "bucephalus-cloud run port-forward {run_id} Trial/{name} --target-port PORT" },
        { purpose: "exec", command: "bucephalus-cloud run exec {run_id} Trial/{name} -- COMMAND [ARG...]" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Task", type: "string", jsonPath: ".spec.task_id", description: expect.any(String), priority: 0 },
        { name: "Outcome", type: "string", jsonPath: ".status.outcome", description: expect.any(String), priority: 0 },
        { name: "Events", type: "integer", jsonPath: ".status.events_total", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/trial-id",
        "bucephalus.dev/variant-id",
        "bucephalus.dev/task-id",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "TrialContainer")).toMatchObject({
      subresources: ["status", "events", "metrics", "logs", "port-forward", "exec"],
      access: ["logs", "port-forward", "exec"],
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=TrialContainer",
        resource: "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=TrialContainer",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}/metrics",
          logs: "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}/logs",
          "port-forward": "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}/port-forward",
          exec: "/v1/runs/{run_id}/runtime/resources/TrialContainer/{name}/exec",
        },
      },
      fieldSelectors: expect.arrayContaining([
        "metadata.ownerReferences.kind",
        "status.access.reachable",
        "status.access.exec",
        "status.<path>",
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/run-id",
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/trial-id",
        "bucephalus.dev/container-role",
        "bucephalus.dev/worker-id",
        "bucephalus.dev/attempt-id",
        "bucephalus.dev/runner-instance-id",
        "bucephalus.dev/runner-pool-id",
      ]),
      count: 2,
    });
    expect(discovery.resources.find((resource) => resource.kind === "SlotCommit")).toMatchObject({
      name: "slotcommits",
      singularName: "slotcommit",
      shortNames: ["commit"],
      categories: expect.arrayContaining(["scheduler", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics"],
      access: [],
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=SlotCommit",
        resource: "/v1/runs/{run_id}/runtime/resources/SlotCommit/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/SlotCommit/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=SlotCommit",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/SlotCommit/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/SlotCommit/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/SlotCommit/{name}/metrics",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind SlotCommit" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} SlotCommit/{name}" },
        { purpose: "describe", command: "bucephalus-cloud run describe {run_id} SlotCommit/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} SlotCommit/{name} --follow" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Schedule", type: "integer", jsonPath: ".spec.schedule_idx", description: expect.any(String), priority: 0 },
        { name: "Attempt", type: "integer", jsonPath: ".spec.attempt", description: expect.any(String), priority: 0 },
        { name: "Record", type: "string", jsonPath: ".spec.record_type", description: expect.any(String), priority: 0 },
        { name: "Trial", type: "string", jsonPath: ".status.trial_id", description: expect.any(String), priority: 0 },
        { name: "SlotStatus", type: "string", jsonPath: ".status.slot_status", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/trial-id",
        "bucephalus.dev/schedule-idx",
        "bucephalus.dev/attempt",
        "bucephalus.dev/record-type",
        "bucephalus.dev/slot-commit-id",
        "bucephalus.dev/slot-status",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "PendingTrialCompletion")).toMatchObject({
      name: "pendingtrialcompletions",
      singularName: "pendingtrialcompletion",
      shortNames: ["pendingcompletion"],
      categories: expect.arrayContaining(["scheduler", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics"],
      access: [],
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=PendingTrialCompletion",
        resource: "/v1/runs/{run_id}/runtime/resources/PendingTrialCompletion/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/PendingTrialCompletion/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=PendingTrialCompletion",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/PendingTrialCompletion/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/PendingTrialCompletion/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/PendingTrialCompletion/{name}/metrics",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind PendingTrialCompletion" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} PendingTrialCompletion/{name}" },
        { purpose: "describe", command: "bucephalus-cloud run describe {run_id} PendingTrialCompletion/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} PendingTrialCompletion/{name} --follow" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Schedule", type: "integer", jsonPath: ".spec.schedule_idx", description: expect.any(String), priority: 0 },
        { name: "Trial", type: "string", jsonPath: ".status.trial_id", description: expect.any(String), priority: 0 },
        { name: "SlotStatus", type: "string", jsonPath: ".status.slot_status", description: expect.any(String), priority: 0 },
        { name: "Rows", type: "integer", jsonPath: ".status.deferred_rows.total", description: expect.any(String), priority: 1 },
        { name: "Updated", type: "date", jsonPath: ".status.observed_at", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/trial-id",
        "bucephalus.dev/schedule-idx",
        "bucephalus.dev/slot-status",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "RunManifest")).toMatchObject({
      name: "runmanifests",
      singularName: "runmanifest",
      shortNames: ["manifest"],
      categories: expect.arrayContaining(["declared", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=RunManifest",
        resource: "/v1/runs/{run_id}/runtime/resources/RunManifest/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/RunManifest/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=RunManifest",
      }),
      printerColumns: expect.arrayContaining([
        { name: "CoreRun", type: "string", jsonPath: ".spec.core_run_id", description: expect.any(String), priority: 0 },
        { name: "Experiment", type: "string", jsonPath: ".status.experiment_id", description: expect.any(String), priority: 0 },
        { name: "Workload", type: "string", jsonPath: ".status.workload_type", description: expect.any(String), priority: 0 },
        { name: "Baseline", type: "string", jsonPath: ".status.baseline_id", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/experiment-id",
        "bucephalus.dev/runtime-status",
        "bucephalus.dev/workload-type",
        "bucephalus.dev/baseline-id",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "MetricDefinition")).toMatchObject({
      name: "metricdefinitions",
      singularName: "metricdefinition",
      shortNames: ["metricdef"],
      categories: expect.arrayContaining(["declared", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=MetricDefinition",
        resource: "/v1/runs/{run_id}/runtime/resources/MetricDefinition/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/MetricDefinition/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=MetricDefinition",
      }),
      printerColumns: expect.arrayContaining([
        { name: "Metric", type: "string", jsonPath: ".spec.metric_id", description: expect.any(String), priority: 0 },
        { name: "Primary", type: "boolean", jsonPath: ".status.primary_metric", description: expect.any(String), priority: 0 },
        { name: "Required", type: "boolean", jsonPath: ".status.required", description: expect.any(String), priority: 0 },
        { name: "Direction", type: "string", jsonPath: ".status.direction", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/experiment-id",
        "bucephalus.dev/metric-id",
        "bucephalus.dev/semantic-key",
        "bucephalus.dev/source-type",
        "bucephalus.dev/direction",
        "bucephalus.dev/required",
        "bucephalus.dev/primary-metric",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "VariantSnapshot")).toMatchObject({
      name: "variantsnapshots",
      singularName: "variantsnapshot",
      shortNames: ["variantbind"],
      categories: expect.arrayContaining(["trial", "provenance", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics"],
      access: [],
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=VariantSnapshot",
        resource: "/v1/runs/{run_id}/runtime/resources/VariantSnapshot/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/VariantSnapshot/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=VariantSnapshot",
      }),
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind VariantSnapshot" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} VariantSnapshot/{name}" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Schedule", type: "integer", jsonPath: ".spec.schedule_idx", description: expect.any(String), priority: 0 },
        { name: "Binding", type: "string", jsonPath: ".spec.binding_name", description: expect.any(String), priority: 0 },
        { name: "Variant", type: "string", jsonPath: ".spec.variant_id", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/trial-id",
        "bucephalus.dev/slot-commit-id",
        "bucephalus.dev/variant-id",
        "bucephalus.dev/binding-name",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "EvidenceRecord")).toMatchObject({
      name: "evidencerecords",
      singularName: "evidencerecord",
      shortNames: ["evidence"],
      categories: expect.arrayContaining(["trial", "provenance", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=EvidenceRecord",
        resource: "/v1/runs/{run_id}/runtime/resources/EvidenceRecord/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/EvidenceRecord/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=EvidenceRecord",
      }),
      printerColumns: expect.arrayContaining([
        { name: "Seq", type: "integer", jsonPath: ".spec.row_seq", description: expect.any(String), priority: 0 },
        { name: "Trial", type: "string", jsonPath: ".status.trial_id", description: expect.any(String), priority: 1 },
        { name: "Record", type: "string", jsonPath: ".status.record_kind", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/trial-id",
        "bucephalus.dev/slot-commit-id",
        "bucephalus.dev/schema-version",
        "bucephalus.dev/record-kind",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "ChainState")).toMatchObject({
      name: "chainstates",
      singularName: "chainstate",
      shortNames: ["chain"],
      categories: expect.arrayContaining(["provenance", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=ChainState",
        resource: "/v1/runs/{run_id}/runtime/resources/ChainState/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/ChainState/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=ChainState",
      }),
      printerColumns: expect.arrayContaining([
        { name: "Seq", type: "integer", jsonPath: ".spec.row_seq", description: expect.any(String), priority: 0 },
        { name: "Record", type: "string", jsonPath: ".status.record_kind", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/slot-commit-id",
        "bucephalus.dev/schema-version",
        "bucephalus.dev/record-kind",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "TrialConclusion")).toMatchObject({
      name: "trialconclusions",
      singularName: "trialconclusion",
      shortNames: ["conclusion"],
      categories: expect.arrayContaining(["trial", "provenance", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=TrialConclusion",
        resource: "/v1/runs/{run_id}/runtime/resources/TrialConclusion/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/TrialConclusion/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=TrialConclusion",
      }),
      printerColumns: expect.arrayContaining([
        { name: "Outcome", type: "string", jsonPath: ".status.outcome", description: expect.any(String), priority: 0 },
        { name: "Status", type: "string", jsonPath: ".status.status", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/trial-id",
        "bucephalus.dev/outcome",
        "bucephalus.dev/status",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "LineageVersion")).toMatchObject({
      name: "lineageversions",
      singularName: "lineageversion",
      shortNames: ["lineagever"],
      categories: expect.arrayContaining(["provenance", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=LineageVersion",
        resource: "/v1/runs/{run_id}/runtime/resources/LineageVersion/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/LineageVersion/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=LineageVersion",
      }),
      printerColumns: expect.arrayContaining([
        { name: "Chain", type: "string", jsonPath: ".spec.chain_key", description: expect.any(String), priority: 0 },
        { name: "Step", type: "integer", jsonPath: ".spec.step_index", description: expect.any(String), priority: 0 },
        { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: expect.any(String), priority: 0 },
        { name: "Current", type: "boolean", jsonPath: ".status.current_head", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/chain-key",
        "bucephalus.dev/version-id",
        "bucephalus.dev/parent-version-id",
        "bucephalus.dev/trial-id",
        "bucephalus.dev/step-index",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "LineageHead")).toMatchObject({
      name: "lineageheads",
      singularName: "lineagehead",
      shortNames: ["lineage"],
      categories: expect.arrayContaining(["provenance", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=LineageHead",
        resource: "/v1/runs/{run_id}/runtime/resources/LineageHead/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/LineageHead/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=LineageHead",
      }),
      printerColumns: expect.arrayContaining([
        { name: "Chain", type: "string", jsonPath: ".spec.chain_key", description: expect.any(String), priority: 0 },
        { name: "Step", type: "integer", jsonPath: ".status.step_index", description: expect.any(String), priority: 0 },
        { name: "Version", type: "string", jsonPath: ".status.latest_version_id", description: expect.any(String), priority: 0 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/chain-key",
        "bucephalus.dev/latest-version-id",
        "bucephalus.dev/step-index",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "TrialStage")).toMatchObject({
      name: "trialstages",
      singularName: "trialstage",
      shortNames: ["stage"],
      categories: expect.arrayContaining(["trial", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics"],
      access: [],
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=TrialStage",
        resource: "/v1/runs/{run_id}/runtime/resources/TrialStage/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/TrialStage/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=TrialStage",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/TrialStage/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/TrialStage/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/TrialStage/{name}/metrics",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind TrialStage" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} TrialStage/{name}" },
        { purpose: "describe", command: "bucephalus-cloud run describe {run_id} TrialStage/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} TrialStage/{name} --follow" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: expect.any(String), priority: 0 },
        { name: "Stage", type: "string", jsonPath: ".spec.stage", description: expect.any(String), priority: 0 },
        { name: "Status", type: "string", jsonPath: ".status.status", description: expect.any(String), priority: 0 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/trial-id",
        "bucephalus.dev/stage",
        "bucephalus.dev/attempt",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "RuntimeValue")).toMatchObject({
      name: "runtimevalues",
      singularName: "runtimevalue",
      shortNames: ["rv", "kv"],
      categories: expect.arrayContaining(["observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics"],
      access: [],
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=RuntimeValue",
        resource: "/v1/runs/{run_id}/runtime/resources/RuntimeValue/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/RuntimeValue/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=RuntimeValue",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/RuntimeValue/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/RuntimeValue/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/RuntimeValue/{name}/metrics",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind RuntimeValue" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} RuntimeValue/{name}" },
        { purpose: "describe", command: "bucephalus-cloud run describe {run_id} RuntimeValue/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} RuntimeValue/{name} --follow" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Key", type: "string", jsonPath: ".spec.key", description: expect.any(String), priority: 0 },
        { name: "Summary", type: "string", jsonPath: ".status.value_summary", description: expect.any(String), priority: 0 },
        { name: "Source", type: "string", jsonPath: ".status.source", description: expect.any(String), priority: 1 },
        { name: "Updated", type: "date", jsonPath: ".status.observed_at", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/runtime-key",
        "bucephalus.dev/value-source",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "MetricObservation")).toMatchObject({
      name: "metricobservations",
      singularName: "metricobservation",
      shortNames: ["metricobs", "metric"],
      categories: expect.arrayContaining(["trial", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics"],
      access: [],
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=MetricObservation",
        resource: "/v1/runs/{run_id}/runtime/resources/MetricObservation/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/MetricObservation/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=MetricObservation",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/MetricObservation/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/MetricObservation/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/MetricObservation/{name}/metrics",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind MetricObservation" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} MetricObservation/{name}" },
        { purpose: "describe", command: "bucephalus-cloud run describe {run_id} MetricObservation/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} MetricObservation/{name} --follow" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: expect.any(String), priority: 0 },
        { name: "Metric", type: "string", jsonPath: ".spec.metric_name", description: expect.any(String), priority: 0 },
        { name: "Value", type: "number", jsonPath: ".status.metric_value", description: expect.any(String), priority: 0 },
        { name: "Source", type: "string", jsonPath: ".status.metric_source", description: expect.any(String), priority: 1 },
        { name: "Seq", type: "integer", jsonPath: ".spec.row_seq", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/trial-id",
        "bucephalus.dev/metric-name",
        "bucephalus.dev/metric-source",
        "bucephalus.dev/attempt",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "PerformanceSample")).toMatchObject({
      name: "performancesamples",
      singularName: "performancesample",
      shortNames: ["perf"],
      categories: expect.arrayContaining(["observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics"],
      access: [],
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=PerformanceSample",
        resource: "/v1/runs/{run_id}/runtime/resources/PerformanceSample/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/PerformanceSample/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=PerformanceSample",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/PerformanceSample/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/PerformanceSample/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/PerformanceSample/{name}/metrics",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind PerformanceSample" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} PerformanceSample/{name}" },
        { purpose: "describe", command: "bucephalus-cloud run describe {run_id} PerformanceSample/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} PerformanceSample/{name} --follow" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Stage", type: "string", jsonPath: ".spec.stage", description: expect.any(String), priority: 0 },
        { name: "Kind", type: "string", jsonPath: ".spec.sample_kind", description: expect.any(String), priority: 0 },
        { name: "DurationMs", type: "number", jsonPath: ".status.duration_ms", description: expect.any(String), priority: 0 },
        { name: "RssKb", type: "integer", jsonPath: ".status.process_rss_kb", description: expect.any(String), priority: 1 },
        { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/trial-id",
        "bucephalus.dev/sample-kind",
        "bucephalus.dev/stage",
        "bucephalus.dev/attempt",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "RuntimeOperation")).toMatchObject({
      name: "runtimeoperations",
      singularName: "runtimeoperation",
      shortNames: ["op"],
      categories: expect.arrayContaining(["observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics"],
      access: [],
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=RuntimeOperation",
        resource: "/v1/runs/{run_id}/runtime/resources/RuntimeOperation/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/RuntimeOperation/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=RuntimeOperation",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/RuntimeOperation/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/RuntimeOperation/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/RuntimeOperation/{name}/metrics",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind RuntimeOperation" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} RuntimeOperation/{name}" },
        { purpose: "describe", command: "bucephalus-cloud run describe {run_id} RuntimeOperation/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} RuntimeOperation/{name} --follow" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Kind", type: "string", jsonPath: ".spec.op_kind", description: expect.any(String), priority: 0 },
        { name: "OpId", type: "string", jsonPath: ".spec.op_id", description: expect.any(String), priority: 0 },
        { name: "Trial", type: "string", jsonPath: ".status.trial_id", description: expect.any(String), priority: 0 },
        { name: "Parent", type: "string", jsonPath: ".status.parent_trial_id", description: expect.any(String), priority: 1 },
        { name: "Updated", type: "date", jsonPath: ".status.observed_at", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/op-kind",
        "bucephalus.dev/op-id",
        "bucephalus.dev/trial-id",
        "bucephalus.dev/parent-trial-id",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "TrialArtifact")).toMatchObject({
      name: "trialartifacts",
      singularName: "trialartifact",
      shortNames: ["artifact"],
      categories: expect.arrayContaining(["trial", "observability"]),
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      subresources: ["status", "events", "metrics", "content"],
      access: [],
      supports: expect.objectContaining({
        list: true,
        get: true,
        watch: true,
        describe: true,
        access: false,
      }),
      pathTemplates: {
        collection: "/v1/runs/{run_id}/runtime/resources?kind=TrialArtifact",
        resource: "/v1/runs/{run_id}/runtime/resources/TrialArtifact/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/TrialArtifact/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=TrialArtifact",
        subresources: {
          status: "/v1/runs/{run_id}/runtime/resources/TrialArtifact/{name}/status",
          events: "/v1/runs/{run_id}/runtime/resources/TrialArtifact/{name}/events",
          metrics: "/v1/runs/{run_id}/runtime/resources/TrialArtifact/{name}/metrics",
          content: "/v1/runs/{run_id}/runtime/resources/TrialArtifact/{name}/content",
        },
      },
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind TrialArtifact" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} TrialArtifact/{name}" },
        { purpose: "describe", command: "bucephalus-cloud run describe {run_id} TrialArtifact/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} TrialArtifact/{name} --follow" },
        { purpose: "content", command: "bucephalus-cloud run artifact {run_id} TrialArtifact/{name} --out FILE" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: expect.any(String), priority: 0 },
        { name: "Role", type: "string", jsonPath: ".spec.role", description: expect.any(String), priority: 0 },
        { name: "Content", type: "boolean", jsonPath: ".status.content_available", description: expect.any(String), priority: 0 },
        { name: "Digest", type: "string", jsonPath: ".status.sha256", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/trial-id",
        "bucephalus.dev/artifact-role",
        "bucephalus.dev/attempt",
        "bucephalus.dev/sha256",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "PortForward")).toMatchObject({
      verbs: expect.arrayContaining(["create", "delete"]),
      actions: ["cancel"],
      supports: expect.objectContaining({ create: true, delete: true, actions: true }),
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=PortForward",
        resource: "/v1/runs/{run_id}/runtime/resources/PortForward/{name}?view=resource",
        describe: "/v1/runs/{run_id}/runtime/resources/PortForward/{name}",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=PortForward",
        create: "/v1/runs/{run_id}/runtime/resources/{target_kind}/{target_name}/port-forward",
        delete: "/v1/runs/{run_id}/runtime/resources/PortForward/{name}",
      }),
      exampleCommands: expect.arrayContaining([
        { purpose: "list", command: "bucephalus-cloud run resources {run_id} --kind PortForward" },
        { purpose: "watch/not-ready", command: "bucephalus-cloud run watch {run_id} --kind PortForward --field-selector status.conditions.Ready!=True" },
        { purpose: "watch/client-unreachable", command: "bucephalus-cloud run watch {run_id} --kind PortForward --field-selector status.conditions.ClientReachable!=True" },
        { purpose: "get", command: "bucephalus-cloud run get {run_id} PortForward/{name}" },
        { purpose: "events", command: "bucephalus-cloud run events {run_id} PortForward/{name} --follow" },
        { purpose: "delete", command: "bucephalus-cloud run delete {run_id} PortForward/{name}" },
        { purpose: "cancel", command: "bucephalus-cloud run cancel {run_id} PortForward/{name}" },
      ]),
      printerColumns: expect.arrayContaining([
        { name: "TargetPort", type: "integer", jsonPath: ".spec.target_port", description: expect.any(String), priority: 0 },
        { name: "LocalPort", type: "integer", jsonPath: ".spec.local_port", description: expect.any(String), priority: 0 },
        { name: "ClientReachable", type: "string", jsonPath: '.status.conditions[?(@.type=="ClientReachable")].status', description: expect.any(String), priority: 0 },
        { name: "Connection", type: "string", jsonPath: ".status.connection", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/run-id",
        "bucephalus.dev/runner-instance-id",
        "bucephalus.dev/attempt-id",
        "bucephalus.dev/resource-kind",
        "bucephalus.dev/resource-name",
      ]),
      count: 1,
    });
    expect(discovery.resources.find((resource) => resource.kind === "Exec")).toMatchObject({
      verbs: expect.arrayContaining(["create", "delete"]),
      subresources: expect.arrayContaining(["logs", "actions/cancel"]),
      access: ["logs"],
      supports: expect.objectContaining({ create: true, delete: true, actions: true }),
      pathTemplates: expect.objectContaining({
        create: "/v1/runs/{run_id}/runtime/resources/{target_kind}/{target_name}/exec",
        delete: "/v1/runs/{run_id}/runtime/resources/Exec/{name}",
        subresources: expect.objectContaining({
          logs: "/v1/runs/{run_id}/runtime/resources/Exec/{name}/logs",
        }),
      }),
      exampleCommands: expect.arrayContaining([
        { purpose: "logs/stdout", command: "bucephalus-cloud run logs {run_id} Exec/{name} --stream stdout --follow" },
      ]),
      count: 0,
    });
    expect(discovery.resources.find((resource) => resource.kind === "Event")).toMatchObject({
      name: "events",
      singularName: "event",
      shortNames: ["ev"],
      categories: ["observability"],
      verbs: expect.arrayContaining(["list", "get", "watch", "describe"]),
      printerColumns: expect.arrayContaining([
        { name: "Type", type: "string", jsonPath: ".spec.event_type", description: expect.any(String), priority: 0 },
        { name: "Seq", type: "integer", jsonPath: ".spec.row_seq", description: expect.any(String), priority: 0 },
        { name: "Message", type: "string", jsonPath: ".status.message", description: expect.any(String), priority: 1 },
      ]),
      labelSelectors: expect.arrayContaining([
        "bucephalus.dev/run-id",
        "bucephalus.dev/core-run-id",
        "bucephalus.dev/trial-id",
        "bucephalus.dev/event-type",
        "bucephalus.dev/event-source",
        "bucephalus.dev/resource-kind",
        "bucephalus.dev/resource-name",
      ]),
      count: 1,
    });
  });

  test("explains one runtime API resource kind through server-side alias resolution", () => {
    const resources = [
      runtimeResourceFixture({ kind: "TrialContainer", name: "trial-1.agent.container-1" }),
      runtimeResourceFixture({ kind: "TrialContainer", name: "trial-2.agent.container-1" }),
      runtimeResourceFixture({ kind: "RunnerInstance", name: "runner-1" }),
    ];

    expect(runtimeApiResourceForKind("run-1", resources, "container")).toMatchObject({
      kind: "TrialContainer",
      name: "trialcontainers",
      singularName: "trialcontainer",
      shortNames: ["container"],
      count: 2,
      pathTemplates: expect.objectContaining({
        collection: "/v1/runs/{run_id}/runtime/resources?kind=TrialContainer",
        watch: "/v1/runs/{run_id}/runtime/resources/watch?kind=TrialContainer",
      }),
      fieldSelectors: expect.arrayContaining(["kind", "status.conditions.<type>"]),
      labelSelectors: expect.arrayContaining(["bucephalus.dev/trial-id"]),
    });
    expect(runtimeApiResourceForKind("run-1", resources, "runner-instance")).toMatchObject({
      kind: "RunnerInstance",
      name: "runnerinstances",
      count: 1,
      actions: ["cordon", "drain", "uncordon"],
      access: ["logs", "port-forward", "exec"],
    });
    expect(() => runtimeApiResourceForKind("run-1", resources, "missing")).toThrow("Runtime API resource kind not found: missing");
  });

  test("projects source-labeled metrics for a runtime resource", () => {
    const resource = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-1",
      generation: 7,
      spec: {
        cpu_count: 4,
        memory_mb: 8192,
      },
      status: {
        phase: "online",
        observedGeneration: 7,
        actions: ["cordon", "drain"],
        access: {
          reachable: true,
          port_forward: true,
          exec: false,
        },
        attempts: {
          active: 2,
        },
        conditions: [
          runtimeCondition("Ready", "True", "Online", "Runner is online"),
          runtimeCondition("ExecReady", "False", "CapabilityMissing", "Runner does not advertise runtime_exec"),
        ],
      },
      audit: {
        source: "cloud.runner_instances",
      },
    });
    const metrics = runtimeResourceMetricsView("run-1", ["core-run-1"], resource, [
      runtimeEvent({
        event_type: "runtime.resource.runner_instance.cordoned",
        source: "cloud.run_events",
        payload: {
          resource_ref: {
            kind: "RunnerInstance",
            name: "runner-1",
          },
        },
      }),
      runtimeEvent({
        event_type: "runtime.access.exec.requested",
        source: "cloud.run_events",
        payload: {
          runner_binding: {
            runner_instance_id: "runner-1",
          },
        },
      }),
    ]);

    expect(metrics).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceMetrics",
      cloud_run_id: "run-1",
      core_run_ids: ["core-run-1"],
      resource_ref: {
        kind: "RunnerInstance",
        name: "runner-1",
      },
      phase: "online",
      summary: expect.objectContaining({
        events_total: 2,
        lifecycle_metrics: expect.any(Number),
        condition_metrics: expect.any(Number),
        access_metrics: 3,
        event_metrics: expect.any(Number),
        numeric_spec_metrics: 2,
      }),
    });
    expect(metrics.metrics).toEqual(expect.arrayContaining([
      expect.objectContaining({ name: "lifecycle.generation", value: 7, source: "lifecycle" }),
      expect.objectContaining({ name: "lifecycle.observed", value: 1, unit: "boolean", source: "lifecycle" }),
      expect.objectContaining({ name: "conditions.false", value: 1, source: "condition" }),
      expect.objectContaining({
        name: "conditions.by_type.ready",
        value: 1,
        unit: "boolean",
        source: "condition",
        labels: { type: "Ready", status: "True", reason: "Online" },
      }),
      expect.objectContaining({
        name: "conditions.by_type.execready",
        value: 0,
        unit: "boolean",
        source: "condition",
        labels: { type: "ExecReady", status: "False", reason: "CapabilityMissing" },
      }),
      expect.objectContaining({ name: "access.port_forward_ready", value: 1, source: "access" }),
      expect.objectContaining({ name: "access.exec_ready", value: 0, source: "access" }),
      expect.objectContaining({ name: "events.total", value: 2, source: "event" }),
      expect.objectContaining({
        name: "events.by_type.runtime_access_exec_requested",
        value: 1,
        source: "event",
        labels: { event_type: "runtime.access.exec.requested" },
      }),
      expect.objectContaining({ name: "spec.cpu_count", value: 4, unit: "cores", source: "spec" }),
      expect.objectContaining({ name: "spec.memory_mb", value: 8192, unit: "MiB", source: "spec" }),
      expect.objectContaining({ name: "status.attempts.active", value: 2, source: "status" }),
    ]));
  });

  test("scopes resource metrics event scans to the selected resource identity", async () => {
    let observedEventInput: unknown;
    const resource = runtimeResourceFixture({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      status: {
        phase: "running",
      },
    });
    const metrics = await RuntimeRepository.prototype.resourceMetrics.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [resource],
        };
      },
      async eventRows(_cloudRunId: string, input: unknown) {
        observedEventInput = input;
        return [
          runtimeEvent({
            event_type: "runtime.resource.trial_container.ready",
            row_seq: 17,
            resource_refs: [
              {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "TrialContainer",
                name: "trial-1.agent.container-1",
              },
            ],
          }),
        ];
      },
    } as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
    });

    expect(observedEventInput).toMatchObject({
      limit: expect.any(Number),
      resourceKind: "TrialContainer",
      resourceName: "trial-1.agent.container-1",
    });
    expect(metrics).toMatchObject({
      resource_ref: {
        kind: "TrialContainer",
        name: "trial-1.agent.container-1",
      },
      summary: {
        events_total: 1,
      },
      metrics: expect.arrayContaining([
        expect.objectContaining({
          name: "events.by_type.runtime_resource_trial_container_ready",
          value: 1,
          source: "event",
        }),
      ]),
    });
  });

  test("projects bounded collection metrics for filtered runtime resources", async () => {
    const runner = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-1",
      status: {
        phase: "online",
        access: {
          reachable: true,
          port_forward: true,
          exec: true,
        },
        conditions: [
          runtimeCondition("Ready", "True", "Online", "Runner is online"),
        ],
      },
    });
    const trial = runtimeResourceFixture({
      kind: "Trial",
      name: "trial-1",
      status: {
        phase: "running",
        conditions: [
          runtimeCondition("Ready", "False", "Running", "Trial is still running"),
        ],
      },
    });
    const observedInputs: Record<string, unknown>[] = [];
    const observedEventInputs: unknown[] = [];

    const repository = {
      async resources(_cloudRunId: string, _run: unknown, input: Record<string, unknown>) {
        observedInputs.push(input);
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          metadata: {
            resourceVersion: "sha256:inventory",
            continue: null,
            remainingItemCount: 0,
            total: 2,
            returned: 2,
          },
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [runner, trial],
        };
      },
      async eventRows(_cloudRunId: string, input: unknown) {
        observedEventInputs.push(input);
        const filter = input && typeof input === "object" ? input as Record<string, unknown> : {};
        if (filter.resourceKind === "RunnerInstance") return [
          runtimeEvent({
            event_type: "runtime.access.exec.requested",
            source: "cloud.run_events",
            resource_refs: [
              {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "RunnerInstance",
                name: "runner-1",
              },
            ],
          }),
        ];
        if (filter.resourceKind === "Trial") return [
          runtimeEvent({
            event_type: "runtime.trial.started",
            source: "cloud.run_events",
            resource_refs: [
              {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "Trial",
                name: "trial-1",
              },
            ],
          }),
        ];
        return [];
      },
    };

    const metrics = await RuntimeRepository.prototype.resourceMetricsList.call(repository, "run-1", cloudRunRecord(), {
      kinds: ["RunnerInstance", "Trial"],
      fieldSelector: "status.phase=running",
      limit: 1,
    });

    expect(observedInputs[0]).toEqual({
      kinds: ["RunnerInstance", "Trial"],
      fieldSelector: "status.phase=running",
    });
    const continueToken = metrics.metadata.continue;
    if (!continueToken) throw new Error("expected runtime resource metrics list to return a continue token");
    expect(metrics).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceMetricsList",
      cloud_run_id: "run-1",
      core_run_ids: ["core-run-1"],
      metadata: {
        resourceVersion: expect.stringMatching(/^sha256:/),
        continue: continueToken,
        remainingItemCount: 1,
        total: 2,
        returned: 1,
      },
      summary: {
        resources_total: 2,
        resources_returned: 1,
        events_total: 1,
      },
    });
    expect(metrics.resources).toHaveLength(1);
    expect(metrics.resources[0]).toMatchObject({
      kind: "RuntimeResourceMetrics",
      resource_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerInstance",
        name: "runner-1",
      },
      summary: {
        access_metrics: 3,
        events_total: 1,
      },
    });
    expect(metrics.summary.metrics_total).toBe(metrics.resources[0]!.summary.metrics_total);
    expect(observedEventInputs[0]).toMatchObject({
      limit: expect.any(Number),
      resourceKind: "RunnerInstance",
      resourceName: "runner-1",
    });
    const nextMetrics = await RuntimeRepository.prototype.resourceMetricsList.call(repository, "run-1", cloudRunRecord(), {
      kinds: ["RunnerInstance", "Trial"],
      fieldSelector: "status.phase=running",
      limit: 1,
      continueToken,
    });
    expect(observedInputs[1]).toEqual({
      kinds: ["RunnerInstance", "Trial"],
      fieldSelector: "status.phase=running",
    });
    expect(nextMetrics).toMatchObject({
      metadata: {
        continue: null,
        remainingItemCount: 0,
        total: 2,
        returned: 1,
      },
      summary: {
        resources_total: 2,
        resources_returned: 1,
      },
    });
    expect(nextMetrics.resources[0]).toMatchObject({
      resource_ref: {
        kind: "Trial",
        name: "trial-1",
      },
      summary: {
        events_total: 1,
      },
    });
    expect(observedEventInputs[1]).toMatchObject({
      limit: expect.any(Number),
      resourceKind: "Trial",
      resourceName: "trial-1",
    });
  });

  test("projects runtime audit events as selectable resources", async () => {
    const run = cloudRunRecord();
    const auditEvent = runtimeEvent({
      source: "cloud.run_events",
      core_run_id: "core-run-1",
      trial_id: "trial-1",
      task_id: "task-1",
      row_seq: 7,
      seq: 3,
      event_type: "runtime.access.port_forward.requested",
      payload: {
        message: "Port forward requested",
        resource_ref: {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RunnerInstance",
          name: "runner-instance-1",
          uid: "runner-instance-1",
        },
      },
    });
    const repository = {
      async coreRunIdsForCloudRun() {
        return ["core-run-1"];
      },
      async workerRuntimeSnapshots() {
        return [];
      },
      async runAttempts() {
        return [attemptRecord()];
      },
      async provisionRequests() {
        return [];
      },
      async listPortForwards() {
        return [];
      },
      async listExecRequests() {
        return [];
      },
      async runnerPools() {
        return [];
      },
      async scheduleSlots() {
        return [];
      },
      async trialContainers() {
        return [];
      },
      async eventRows(_cloudRunId: string, input: { limit?: number | undefined }) {
        expect(input.limit).toBeGreaterThanOrEqual(250);
        return [auditEvent];
      },
    } as any;

    const inventory = await RuntimeRepository.prototype.resources.call(repository, "run-1", run);
    const eventResource = inventory.resources.find((resource) => resource.kind === "Event");
    if (!eventResource) {
      throw new Error("Expected Event resource in runtime inventory");
    }
    const eventName = eventResource.metadata.name;
    expect(Array.isArray(eventResource.metadata.ownerReferences)).toBe(true);
    const describeInventory = JSON.parse(JSON.stringify(inventory)) as typeof inventory;
    expect(eventResource).toMatchObject({
      metadata: {
        labels: {
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/core-run-id": "core-run-1",
          "bucephalus.dev/event-type": "runtime.access.port_forward.requested",
          "bucephalus.dev/resource-kind": "RunnerInstance",
          "bucephalus.dev/resource-name": "runner-instance-1",
        },
        annotations: {
          "bucephalus.dev/event-type": "runtime.access.port_forward.requested",
          "bucephalus.dev/event-source": "cloud.run_events",
        },
        ownerReferences: expect.arrayContaining([
          expect.objectContaining({ kind: "Run", name: "run-1", uid: "run-1" }),
          expect.objectContaining({ kind: "RunnerInstance", name: "runner-instance-1", uid: "runner-instance-1" }),
        ]),
      },
      spec: {
        event_type: "runtime.access.port_forward.requested",
        source: "cloud.run_events",
        row_seq: 7,
        resource_refs: expect.arrayContaining([
          expect.objectContaining({ kind: "RunnerInstance", name: "runner-instance-1", uid: "runner-instance-1" }),
        ]),
        payload: {
          message: "Port forward requested",
        },
      },
      status: {
        phase: "recorded",
        reason: "RuntimeAccessPortForwardRequested",
        message: "Port forward requested",
      },
      audit: {
        source: "cloud.run_events",
        event_type: "runtime.access.port_forward.requested",
        row_seq: 7,
      },
    });
    expect(eventResource?.metadata.generation).toBeGreaterThan(0);
    expect(eventResource?.status.observedGeneration).toBe(eventResource?.metadata.generation);

    const filtered = await RuntimeRepository.prototype.resources.call(repository, "run-1", run, {
      kinds: ["Event"],
      fieldSelector: "spec.event_type=runtime.access.port_forward.requested",
    });
    expect(filtered.resources.map((resource) => resource.metadata.name)).toEqual([eventName]);

    const described = await RuntimeRepository.prototype.describeResource.call({
      async resources() {
        return describeInventory;
      },
      async eventRows(_cloudRunId: string, input: { limit?: number | undefined }) {
        expect(input.limit).toBeGreaterThanOrEqual(250);
        return [auditEvent];
      },
    } as any, "run-1", run, {
      kind: "Event",
      name: eventName,
    });
    expect(described.event_list.events).toEqual([
      expect.objectContaining({
        row_seq: auditEvent.row_seq,
        event_type: auditEvent.event_type,
        resource_refs: expect.arrayContaining([
          expect.objectContaining({ kind: "RunnerInstance", name: "runner-instance-1", uid: "runner-instance-1" }),
        ]),
      }),
    ]);
    expect(described.event_list).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceEventList",
      cloud_run_id: "run-1",
      core_run_ids: describeInventory.core_run_ids,
      generated_at: expect.any(String),
      resource: {
        kind: "Event",
        metadata: {
          name: eventName,
        },
      },
      event_filter: {
        resource_kind: "Event",
        resource_name: eventName,
      },
      metadata: {
        resourceVersion: "event-row-seq:7",
        returned: 1,
        limit: 100,
      },
    });
    expect(described.related_resources.map((related) => ({
      relationship: related.relationship,
      kind: related.resource.kind,
      name: related.resource.metadata.name,
    }))).toEqual(expect.arrayContaining([
      { relationship: "owner", kind: "Run", name: "run-1" },
      { relationship: "owner", kind: "RunnerInstance", name: "runner-instance-1" },
    ]));
  });

  test("scopes ordinary resource describe event scans to the selected resource identity", async () => {
    let observedEventInput: unknown;
    const resource = runtimeResourceFixture({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      status: { phase: "running" },
    });
    const described = await RuntimeRepository.prototype.describeResource.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [resource],
        };
      },
      async eventRows(_cloudRunId: string, input: unknown) {
        observedEventInput = input;
        return [
          runtimeEvent({
            row_seq: 17,
            event_type: "runtime.resource.trial_container.ready",
            resource_refs: [
              {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "TrialContainer",
                name: "trial-1.agent.container-1",
              },
            ],
          }),
        ];
      },
    } as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      eventLimit: 25,
    });

    expect(observedEventInput).toMatchObject({
      limit: expect.any(Number),
      resourceKind: "TrialContainer",
      resourceName: "trial-1.agent.container-1",
    });
    expect(described.event_list).toMatchObject({
      event_filter: {
        resource_kind: "TrialContainer",
        resource_name: "trial-1.agent.container-1",
      },
      metadata: {
        returned: 1,
        limit: 25,
      },
      events: [
        {
          row_seq: 17,
          event_type: "runtime.resource.trial_container.ready",
        },
      ],
    });
  });

  test("builds runtime inspect bundles with inventory, events, discovery, and log refs", async () => {
    const run = cloudRunRecord();
    const observed: { filter?: unknown; eventInputs: Array<{ limit?: number | undefined; resourceKind?: string | undefined; resourceName?: string | undefined }> } = {
      eventInputs: [],
    };
    const bundle = await RuntimeRepository.prototype.inspectBundle.call({
      async resources(cloudRunId: string, _run: unknown, input: unknown) {
        observed.filter = input;
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          metadata: {
            resourceVersion: "sha256:inspect-inventory",
            continue: null,
            remainingItemCount: 0,
            total: 1,
            returned: 1,
          },
          cloud_run_id: cloudRunId,
          core_run_ids: ["core-run-1"],
          resources: [
            runtimeResourceFixture({
              kind: "Run",
              name: "run-1",
            }),
            runtimeResourceFixture({
              kind: "RunnerInstance",
              name: "runner-instance-1",
              labels: {
                "bucephalus.dev/runner-instance-id": "runner-instance-1",
                "bucephalus.dev/current-attempt-id": "attempt-1",
              },
              spec: {
                runner_instance_id: "runner-instance-1",
                attempt_ids: ["attempt-1"],
                worker_ids: ["worker-1"],
              },
              status: {
                phase: "online",
                current_attempt_id: "attempt-1",
                active_attempts: 1,
              },
            }),
            runtimeResourceFixture({
              kind: "RunnerAttempt",
              name: "attempt-1",
              labels: {
                "bucephalus.dev/runner-instance-id": "runner-instance-1",
                "bucephalus.dev/worker-id": "worker-1",
              },
              spec: {
                runner_instance_id: "runner-instance-1",
                worker_id: "worker-1",
              },
              status: {
                phase: "running",
              },
            }),
            runtimeResourceFixture({
              kind: "Trial",
              name: "trial-1",
              spec: {
                core_run_id: "core-run-1",
                trial_id: "trial-1",
                attempt: 0,
              },
              status: {
                phase: "running",
              },
            }),
            runtimeResourceFixture({
              kind: "TrialContainer",
              name: "trial-1.agent.container-1",
              spec: {
                core_run_id: "core-run-1",
                trial_id: "trial-1",
                attempt: 0,
              },
              status: {
                phase: "running",
              },
            }),
          ],
        };
      },
      async eventRows(_cloudRunId: string, input: { limit?: number | undefined; resourceKind?: string | undefined; resourceName?: string | undefined }) {
        observed.eventInputs.push(input);
        if (input.limit === 26) {
          return [
            runtimeEvent({
              row_seq: 1,
              event_type: "trial.observed",
            }),
          ];
        }
        if (input.resourceKind && input.resourceName) {
          return [
            runtimeEvent({
              row_seq: 100 + observed.eventInputs.length,
              event_type: "runtime.resource.observed",
              resource_refs: [
                {
                  apiVersion: "bucephalus.dev/v1alpha1",
                  kind: input.resourceKind,
                  name: input.resourceName,
                },
              ],
            }),
          ];
        }
        return [
          runtimeEvent({
            row_seq: 100 + observed.eventInputs.length,
            event_type: "run.observed",
          }),
        ];
      },
    } as any, "run-1", run, {
      eventLimit: 25,
      kinds: ["RunnerInstance", "Trial"],
      labelSelector: "bucephalus.dev/run-id=run-1",
      fieldSelector: "status.phase!=completed",
    });

    expect(observed.eventInputs[0]).toEqual({ limit: 26 });
    expect(observed.eventInputs.slice(1)).toEqual([
      { limit: 250 },
      { limit: 250, resourceKind: "RunnerInstance", resourceName: "runner-instance-1" },
      { limit: 250, resourceKind: "RunnerAttempt", resourceName: "attempt-1" },
      { limit: 250, resourceKind: "Trial", resourceName: "trial-1" },
      { limit: 250, resourceKind: "TrialContainer", resourceName: "trial-1.agent.container-1" },
    ]);
    expect(observed.filter).toEqual({
      kinds: ["RunnerInstance", "Trial"],
      labelSelector: "bucephalus.dev/run-id=run-1",
      fieldSelector: "status.phase!=completed",
    });
    expect(bundle).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeInspectBundle",
      cloud_run_id: "run-1",
      generated_at: expect.any(String),
      resource_filter: {
        kinds: ["RunnerInstance", "Trial"],
        label_selector: "bucephalus.dev/run-id=run-1",
        field_selector: "status.phase!=completed",
      },
      api_resources: {
        kind: "RuntimeApiResourceList",
        resources: expect.arrayContaining([
          expect.objectContaining({ kind: "RunnerInstance", count: 1 }),
          expect.objectContaining({ kind: "RunnerAttempt", count: 1 }),
          expect.objectContaining({ kind: "Trial", count: 1 }),
          expect.objectContaining({ kind: "TrialContainer", count: 1 }),
        ]),
      },
      resource_inventory: {
        kind: "RuntimeResourceList",
        core_run_ids: ["core-run-1"],
      },
      resource_health: {
        kind: "RuntimeResourceHealth",
        cloud_run_id: "run-1",
        core_run_ids: ["core-run-1"],
        summary: {
          total: 5,
          ready: expect.any(Number),
          problem: expect.any(Number),
        },
        resources: expect.arrayContaining([
          expect.objectContaining({
            resource: "RunnerInstance/runner-instance-1",
            resource_ref: expect.objectContaining({
              kind: "RunnerInstance",
              name: "runner-instance-1",
            }),
          }),
          expect.objectContaining({
            resource: "Trial/trial-1",
            health: "unknown",
          }),
        ]),
      },
      resource_metrics: {
        kind: "RuntimeResourceMetricsList",
        cloud_run_id: "run-1",
        core_run_ids: ["core-run-1"],
        metadata: {
          total: 5,
          returned: 5,
        },
        summary: {
          resources_total: 5,
          resources_returned: 5,
          metrics_total: expect.any(Number),
          events_total: 5,
        },
        resources: expect.arrayContaining([
          expect.objectContaining({
            kind: "RuntimeResourceMetrics",
            resource_ref: expect.objectContaining({
              kind: "Trial",
              name: "trial-1",
            }),
          }),
        ]),
      },
      event_list: {
        kind: "RuntimeEventList",
        metadata: {
          resourceVersion: "event-row-seq:1",
          continue: null,
          remainingItemCount: 0,
          limit: 25,
          returned: 1,
          after_row_seq: null,
          next_after_row_seq: 1,
        },
        events: [
          expect.objectContaining({ event_type: "trial.observed" }),
        ],
      },
      log_refs: expect.arrayContaining([
        {
          resource: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "RunnerInstance",
            name: "runner-instance-1",
            uid: "runner-instance-1",
          },
          streams: ["stdout", "stderr"],
          urls: {
            stdout: "/v1/runs/run-1/runtime/resources/RunnerInstance/runner-instance-1/logs?stream=stdout",
            stderr: "/v1/runs/run-1/runtime/resources/RunnerInstance/runner-instance-1/logs?stream=stderr",
          },
        },
        {
          resource: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "Trial",
            name: "trial-1",
            uid: "trial-1",
          },
          streams: ["stdout", "stderr"],
          urls: {
            stdout: "/v1/runs/run-1/runtime/resources/Trial/trial-1/logs?stream=stdout",
            stderr: "/v1/runs/run-1/runtime/resources/Trial/trial-1/logs?stream=stderr",
          },
        },
        {
          resource: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "RunnerAttempt",
            name: "attempt-1",
            uid: "attempt-1",
          },
          streams: ["stdout", "stderr"],
          urls: {
            stdout: "/v1/runs/run-1/runtime/resources/RunnerAttempt/attempt-1/logs?stream=stdout",
            stderr: "/v1/runs/run-1/runtime/resources/RunnerAttempt/attempt-1/logs?stream=stderr",
          },
        },
        {
          resource: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "TrialContainer",
            name: "trial-1.agent.container-1",
            uid: "trial-1.agent.container-1",
          },
          streams: ["stdout", "stderr"],
          urls: {
            stdout: "/v1/runs/run-1/runtime/resources/TrialContainer/trial-1.agent.container-1/logs?stream=stdout",
            stderr: "/v1/runs/run-1/runtime/resources/TrialContainer/trial-1.agent.container-1/logs?stream=stderr",
          },
        },
      ]),
    });
  });

  test("advertises lifecycle actions on cordoned and draining runner instance resources", async () => {
    const run = cloudRunRecord();
    const inventoryForPhase = async (phase: string) => await RuntimeRepository.prototype.resources.call({
        async coreRunIdsForCloudRun() {
          return [];
        },
        async workerRuntimeSnapshots() {
          return [];
        },
        async runAttempts() {
          return [attemptRecord({ runner_instance_status: phase })];
        },
        async provisionRequests() {
          return [];
        },
        async runnerPools() {
          return [runnerPoolRecord()];
        },
        async listPortForwards() {
          return [];
        },
        async listExecRequests() {
          return [];
        },
        async scheduleSlots() {
          return [];
        },
        async trialContainers() {
          return [];
        },
      }, "run-1", run);

    const inventory = await inventoryForPhase("draining");
    const runnerInstance = inventory.resources.find((resource) => resource.kind === "RunnerInstance");
    expect(runnerInstance?.status.actions).toEqual(["uncordon"]);
    expect(runtimeConditionsByType(runnerInstance!).Ready).toMatchObject({
      status: "True",
      reason: "Draining",
    });
    expect(runnerInstance?.metadata.resourceVersion).toMatch(/^sha256:[0-9a-f]{64}$/);

    const cordonedInventory = await inventoryForPhase("cordoned");
    const cordonedRunnerInstance = cordonedInventory.resources.find((resource) => resource.kind === "RunnerInstance");
    expect(cordonedRunnerInstance?.status.actions).toEqual(["uncordon", "drain"]);
    expect(runtimeConditionsByType(cordonedRunnerInstance!).Ready).toMatchObject({
      status: "True",
      reason: "Cordoned",
    });
  });

  test("changes runtime resource versions when visible resource state changes", async () => {
    async function inventoryForStatus(status: string) {
      return await RuntimeRepository.prototype.resources.call({
        async coreRunIdsForCloudRun() {
          return [];
        },
        async workerRuntimeSnapshots() {
          return [];
        },
        async runAttempts() {
          return [attemptRecord({ runner_instance_status: status })];
        },
        async provisionRequests() {
          return [];
        },
        async runnerPools() {
          return [runnerPoolRecord()];
        },
        async listPortForwards() {
          return [];
        },
        async listExecRequests() {
          return [];
        },
        async scheduleSlots() {
          return [];
        },
        async trialContainers() {
          return [];
        },
      }, "run-1", cloudRunRecord());
    }

    const onlineInventory = await inventoryForStatus("online");
    const drainingInventory = await inventoryForStatus("draining");
    const online = onlineInventory.resources.find((resource) => resource.kind === "RunnerInstance")!;
    const draining = drainingInventory.resources.find((resource) => resource.kind === "RunnerInstance")!;

    expect(onlineInventory.metadata).toMatchObject({
      resourceVersion: expect.stringMatching(/^sha256:[0-9a-f]{64}$/),
      continue: null,
      remainingItemCount: 0,
    });
    expect(drainingInventory.metadata.resourceVersion).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(drainingInventory.metadata.resourceVersion).not.toBe(onlineInventory.metadata.resourceVersion);
    expect(online.metadata.resourceVersion).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(draining.metadata.resourceVersion).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(draining.metadata.resourceVersion).not.toBe(online.metadata.resourceVersion);
  });

  test("paginates runtime resource lists with opaque collection continue tokens", async () => {
    async function runnerInventory(input: { limit?: number; continueToken?: string | null } = {}) {
      return await RuntimeRepository.prototype.resources.call({
        async coreRunIdsForCloudRun() {
          return [];
        },
        async workerRuntimeSnapshots() {
          return [];
        },
        async runAttempts() {
          return [
            attemptRecord({
              attempt_id: "attempt-a",
              runner_instance_id: "runner-instance-a",
              runner_instance_name: "runner-a",
            }),
            attemptRecord({
              attempt_id: "attempt-b",
              runner_instance_id: "runner-instance-b",
              runner_instance_name: "runner-b",
            }),
          ];
        },
        async provisionRequests() {
          return [];
        },
        async runnerPools() {
          return [runnerPoolRecord()];
        },
        async listPortForwards() {
          return [];
        },
        async listExecRequests() {
          return [];
        },
        async scheduleSlots() {
          return [];
        },
        async trialContainers() {
          return [];
        },
      }, "run-1", cloudRunRecord(), {
        kinds: ["RunnerInstance"],
        ...input,
      });
    }

    const firstPage = await runnerInventory({ limit: 1 });
    const nextPageToken = firstPage.metadata.continue;
    const firstPageResourceVersion = firstPage.metadata.resourceVersion;
    expect(firstPage.resources.map((resource) => resource.metadata.name)).toEqual(["runner-instance-a"]);
    expect(firstPage.metadata).toMatchObject({
      resourceVersion: expect.stringMatching(/^sha256:[0-9a-f]{64}$/),
      continue: expect.any(String),
      remainingItemCount: 1,
      total: 2,
      returned: 1,
    });
    expect(typeof nextPageToken).toBe("string");

    const secondPage = await runnerInventory({ limit: 1, continueToken: nextPageToken });
    expect(secondPage.metadata.resourceVersion).toBe(firstPageResourceVersion);
    expect(secondPage.metadata.continue).toBeNull();
    expect(secondPage.metadata.remainingItemCount).toBe(0);
    expect(secondPage.metadata.total).toBe(2);
    expect(secondPage.metadata.returned).toBe(1);
    expect(secondPage.resources.map((resource) => resource.metadata.name)).toEqual(["runner-instance-b"]);

    const staleContinue = Buffer.from(JSON.stringify({
      resourceVersion: "sha256:stale",
      offset: 1,
    }), "utf8").toString("base64url");
    await expect(runnerInventory({ limit: 1, continueToken: staleContinue })).rejects.toMatchObject({
      status: 410,
      code: "runtime_resource_continue_expired",
    });
  });

  test("builds resource watch snapshots from known resource versions", async () => {
    const runner = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-1",
      status: { phase: "online" },
    });
    const event = runtimeResourceFixture({
      kind: "Event",
      name: "event-1",
      status: { phase: "recorded" },
    });
    const runtime = {
      async resources(_cloudRunId: string, _run: ReturnType<typeof cloudRunRecord>) {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          metadata: {
            resourceVersion: "sha256:inventory",
            continue: null,
            remainingItemCount: 0,
            total: 2,
            returned: 2,
          },
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [runner, event],
        };
      },
    };

    const initial = await RuntimeRepository.prototype.watchResources.call(
      runtime,
      "run-1",
      cloudRunRecord(),
    );

    expect(initial.kind).toBe("RuntimeResourceWatchList");
    expect(initial.resource_versions["runnerinstance/runner-1"]).toMatch(/^sha256:/);
    expect(initial.events.map((event) => event.type)).toEqual(["ADDED", "ADDED"]);
    expect(initial.events[0]?.resource_ref).toMatchObject({ kind: "RunnerInstance", name: "runner-1" });
    expect(initial.resource_inventory.metadata.resourceVersion).toBe("sha256:inventory");
    expect(initial.resource_inventory.resources).toHaveLength(2);

    const unchanged = await RuntimeRepository.prototype.watchResources.call(
      runtime,
      "run-1",
      cloudRunRecord(),
      {
        resourceVersion: "sha256:inventory",
      },
    );

    expect(unchanged.resource_versions).toEqual(initial.resource_versions);
    expect(unchanged.events).toEqual([]);
    expect(unchanged.resource_inventory.metadata.resourceVersion).toBe("sha256:inventory");

    const next = await RuntimeRepository.prototype.watchResources.call(
      runtime,
      "run-1",
      cloudRunRecord(),
      {
        knownResourceVersions: new Map([
          ["runner/runner-1", initial.resource_versions["runnerinstance/runner-1"]!],
          ["event/event-1", "sha256:old"],
          ["exec/deleted", "sha256:gone"],
        ]),
      },
    );

    expect(next.events).toHaveLength(2);
    expect(next.events[0]).toMatchObject({
      type: "MODIFIED",
      resource_ref: { kind: "Event", name: "event-1" },
      previous_resource_version: "sha256:old",
    });
    expect(next.events[1]).toMatchObject({
      type: "DELETED",
      resource_ref: { kind: "Exec", name: "deleted" },
      previous_resource_version: "sha256:gone",
    });
  });

  test("records structured involved refs on port-forward and exec runtime access audit events", async () => {
    const observed: {
      jsonPayloads: JsonObject[];
      eventInsertValues?: unknown[];
      accessInsertValues?: unknown[];
    } = {
      jsonPayloads: [],
    };
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("select attempt.*")) {
        return [attemptRecord()];
      }
      if (query.includes("insert into cloud.runtime_access_requests")) {
        observed.accessInsertValues = values;
        if (query.includes("'exec'")) {
          return [accessRequestRecord({
            kind: "exec",
            access_request_id: "exec-1",
            resource_kind: "RunnerAttempt",
            resource_name: "attempt-1",
            protocol: "exec",
            target_port: null,
            command: ["python", "-V"],
            reason: "inspect process",
            expires_at: "2026-06-04T00:02:00Z",
          })];
        }
        return [accessRequestRecord({
          kind: "port_forward",
          access_request_id: "pf-1",
          resource_kind: "RunnerAttempt",
          resource_name: "attempt-1",
          target_port: 8080,
          expires_at: "2026-06-04T00:05:00Z",
        })];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.eventInsertValues = values;
        return [];
      }
      return [];
    }) as any;
    const sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => {
        observed.jsonPayloads.push(payload);
        return payload;
      },
    };
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = sql;
    const targetResource = runtimeResourceFixture({
      kind: "RunnerAttempt",
      name: "attempt-1",
      labels: {
        "bucephalus.dev/runner-instance-id": "runner-instance-1",
        "bucephalus.dev/worker-id": "worker-1",
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        worker_id: "worker-1",
      },
      status: {
        phase: "running",
        runner_instance_status: "online",
      },
    });
    targetResource.metadata.resourceVersion = "sha256:attempt-1";
    (repo as any).resources = async () => ({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceList",
      metadata: {
        resourceVersion: "sha256:inventory",
        continue: null,
        remainingItemCount: 0,
        total: 1,
        returned: 1,
      },
      cloud_run_id: "run-1",
      core_run_ids: [],
      resources: [targetResource],
    });

    const request = await RuntimeRepository.prototype.createPortForwardRequest.call(repo as any, {
      run: cloudRunRecord(),
      resourceKind: "RunnerAttempt",
      resourceName: "attempt-1",
      resourceVersion: "sha256:attempt-1",
      targetPort: 8080,
      ttlSeconds: 300,
      requester: "issuer:user-a",
      reason: "debug",
    });

    const eventPayload = observed.jsonPayloads.find((payload) => payload.access_request_id === request.access_request_id);
    expect(observed.eventInsertValues).toContain("runtime.access.port_forward.requested");
    expect(observed.accessInsertValues).toContain(300);
    expect(observed.accessInsertValues).toContain("attempt-1");
    expect(observed.accessInsertValues).toContain("sha256:attempt-1");
    expect(observed.accessInsertValues).toContain("worker-1");
    expect(eventPayload).toMatchObject({
      access_request_id: "pf-1",
      access_resource_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "PortForward",
        name: "pf-1",
        uid: "pf-1",
      },
      target_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerAttempt",
        name: "attempt-1",
      },
      resolved_target: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerAttempt",
        name: "attempt-1",
        uid: "attempt-1",
        resourceVersion: "sha256:attempt-1",
        runner_instance_id: "runner-instance-1",
        attempt_id: "attempt-1",
        worker_id: "worker-1",
        runner_binding: {
          runner_instance_id: "runner-instance-1",
          attempt_id: "attempt-1",
          worker_id: "worker-1",
        },
      },
      resource_kind: "RunnerAttempt",
      resource_name: "attempt-1",
      runner_binding: {
        runner_instance_id: "runner-instance-1",
        attempt_id: "attempt-1",
        worker_id: "worker-1",
      },
      runner_instance_id: "runner-instance-1",
      attempt_id: "attempt-1",
      requester: "issuer:user-a",
      reason: "debug",
      resource_version_precondition: "sha256:attempt-1",
      expires_at: "2026-06-04T00:05:00Z",
    });

    observed.jsonPayloads = [];
    delete observed.eventInsertValues;
    delete observed.accessInsertValues;

    const execRequest = await RuntimeRepository.prototype.createExecRequest.call(repo as any, {
      run: cloudRunRecord(),
      resourceKind: "RunnerAttempt",
      resourceName: "attempt-1",
      resourceVersion: "sha256:attempt-1",
      command: ["python", "-V"],
      ttlSeconds: 120,
      requester: "issuer:user-a",
      reason: "inspect process",
    });

    const execPayload = observed.jsonPayloads.find((payload) => payload.access_request_id === execRequest.access_request_id);
    expect(observed.eventInsertValues!).toContain("runtime.access.exec.requested");
    expect(observed.accessInsertValues!).toContain(120);
    expect(observed.accessInsertValues!).toContain("attempt-1");
    expect(observed.accessInsertValues!).toContain("sha256:attempt-1");
    expect(observed.accessInsertValues!).toContain("worker-1");
    expect(execPayload).toMatchObject({
      access_request_id: "exec-1",
      kind: "exec",
      access_resource_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "Exec",
        name: "exec-1",
        uid: "exec-1",
      },
      target_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerAttempt",
        name: "attempt-1",
      },
      resolved_target: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerAttempt",
        name: "attempt-1",
        uid: "attempt-1",
        resourceVersion: "sha256:attempt-1",
        runner_instance_id: "runner-instance-1",
        attempt_id: "attempt-1",
        worker_id: "worker-1",
        runner_binding: {
          runner_instance_id: "runner-instance-1",
          attempt_id: "attempt-1",
          worker_id: "worker-1",
        },
      },
      resource_kind: "RunnerAttempt",
      resource_name: "attempt-1",
      protocol: "exec",
      command: ["python", "-V"],
      runner_binding: {
        runner_instance_id: "runner-instance-1",
        attempt_id: "attempt-1",
        worker_id: "worker-1",
      },
      runner_instance_id: "runner-instance-1",
      attempt_id: "attempt-1",
      requester: "issuer:user-a",
      reason: "inspect process",
      resource_version_precondition: "sha256:attempt-1",
      expires_at: "2026-06-04T00:02:00Z",
    });
  });

  test("expires live runtime access requests and records audit events", async () => {
    const observed: {
      eventTypes: string[];
      payloads: JsonObject[];
    } = {
      eventTypes: [],
      payloads: [],
    };
    const expiredRows = [
      {
        ...accessRequestRecord({
        access_request_id: "pf-expired",
        kind: "port_forward",
        status: "expired",
        resource_kind: "TrialContainer",
        resource_name: "trial-1.agent.container-1",
        target_port: 8080,
        expires_at: "2026-06-04T00:05:00Z",
        error_message: "runtime access request expired",
        }),
        previous_status: "active",
      },
      {
        ...accessRequestRecord({
        access_request_id: "exec-expired",
        kind: "exec",
        status: "expired",
        resource_kind: "TrialContainer",
        resource_name: "trial-1.agent.container-1",
        command: ["python", "-V"],
        expires_at: "2026-06-04T00:06:00Z",
        error_message: "runtime access request expired",
        }),
        previous_status: "accepted",
      },
    ];
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runtime_access_requests")) {
        return expiredRows;
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.eventTypes.push(String(values[3]));
        observed.payloads.push(values[4] as JsonObject);
        return [];
      }
      return [];
    }) as any;
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };

    const expired = await RuntimeRepository.prototype.expireRuntimeAccessRequests.call(repo as any);

    expect(expired.map((request) => request.access_request_id)).toEqual(["pf-expired", "exec-expired"]);
    expect(observed.eventTypes).toEqual([
      "runtime.access.port_forward.expired",
      "runtime.access.exec.expired",
    ]);
    expect(observed.payloads).toEqual([
      expect.objectContaining({
        access_request_id: "pf-expired",
        previous_status: "active",
        status: "expired",
        expires_at: "2026-06-04T00:05:00Z",
      }),
      expect.objectContaining({
        access_request_id: "exec-expired",
        previous_status: "accepted",
        status: "expired",
        expires_at: "2026-06-04T00:06:00Z",
      }),
    ]);
  });

  test("records previous lifecycle phase on worker runtime access transition audit events", async () => {
    const observed: {
      eventTypes: string[];
      payloads: JsonObject[];
    } = {
      eventTypes: [],
      payloads: [],
    };
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("with candidates as") && query.includes("select * from updated")) {
        return [];
      }
      if (query.includes("update cloud.runtime_access_requests") && query.includes("kind = 'port_forward'")) {
        return [{
          ...accessRequestRecord({
            access_request_id: "pf-1",
            kind: "port_forward",
            status: "active",
            resource_kind: "TrialContainer",
            resource_name: "trial-1.agent.container-1",
            target_port: 8080,
            connection: { client_endpoint: "tcp://127.0.0.1:18080" },
          }),
          previous_status: "accepted",
        }];
      }
      if (query.includes("update cloud.runtime_access_requests") && query.includes("kind = 'exec'")) {
        return [{
          ...accessRequestRecord({
            access_request_id: "exec-1",
            kind: "exec",
            status: "completed",
            resource_kind: "TrialContainer",
            resource_name: "trial-1.agent.container-1",
            command: ["python", "-V"],
            connection: { exit_code: 0, stdout_tail: "ok\n" },
          }),
          previous_status: "active",
        }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.eventTypes.push(String(values[3]));
        observed.payloads.push(values[4] as JsonObject);
        return [];
      }
      return [];
    }) as any;
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };

    await RuntimeRepository.prototype.updatePortForwardRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "pf-1",
      status: "active",
      connection: { client_endpoint: "tcp://127.0.0.1:18080" },
    });
    await RuntimeRepository.prototype.updateExecRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "exec-1",
      status: "completed",
      connection: { exit_code: 0, stdout_tail: "ok\n" },
    });

    expect(observed.eventTypes).toEqual([
      "runtime.access.port_forward.active",
      "runtime.access.exec.completed",
    ]);
    expect(observed.payloads).toEqual([
      expect.objectContaining({
        access_request_id: "pf-1",
        previous_status: "accepted",
        status: "active",
        connection: expect.objectContaining({ client_endpoint: "tcp://127.0.0.1:18080" }),
      }),
      expect.objectContaining({
        access_request_id: "exec-1",
        previous_status: "active",
        status: "completed",
        connection: expect.objectContaining({ exit_code: 0 }),
      }),
    ]);
  });

  test("records previous lifecycle phase on runtime access cancel audit events", async () => {
    const observed: {
      eventTypes: string[];
      payloads: JsonObject[];
    } = {
      eventTypes: [],
      payloads: [],
    };
    const updatedAt = "2026-06-04T00:04:00Z";
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runtime_access_requests") && query.includes("kind = 'port_forward'")) {
        return [{
          ...accessRequestRecord({
            access_request_id: "pf-1",
            kind: "port_forward",
            status: "cancelled",
            resource_kind: "TrialContainer",
            resource_name: "trial-1.agent.container-1",
            target_port: 8080,
            requester: "issuer:user-a",
            reason: "cleanup tunnel",
            updated_at: updatedAt,
          }),
          previous_status: "active",
        }];
      }
      if (query.includes("update cloud.runtime_access_requests") && query.includes("kind = 'exec'")) {
        return [{
          ...accessRequestRecord({
            access_request_id: "exec-1",
            kind: "exec",
            status: "cancelled",
            resource_kind: "TrialContainer",
            resource_name: "trial-1.agent.container-1",
            command: ["python", "-V"],
            requester: "issuer:user-a",
            reason: "stop command",
            updated_at: updatedAt,
          }),
          previous_status: "accepted",
        }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.eventTypes.push(String(values[3]));
        observed.payloads.push(values[4] as JsonObject);
        return [];
      }
      return [];
    }) as any;
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };

    await RuntimeRepository.prototype.cancelPortForwardRequest.call(repo as any, {
      cloudRunId: "run-1",
      accessRequestId: "pf-1",
      requester: "issuer:user-a",
      reason: "cleanup tunnel",
      resourceVersion: "sha256:pf-1",
    });
    await RuntimeRepository.prototype.cancelExecRequest.call(repo as any, {
      cloudRunId: "run-1",
      accessRequestId: "exec-1",
      requester: "issuer:user-a",
      reason: "stop command",
      resourceVersion: "sha256:exec-1",
    });

    expect(observed.eventTypes).toEqual([
      "runtime.access.port_forward.cancelled",
      "runtime.access.exec.cancelled",
    ]);
    expect(observed.payloads).toEqual([
      expect.objectContaining({
        access_request_id: "pf-1",
        action: "cancel",
        previous_status: "active",
        status: "cancelled",
        cancelled_at: updatedAt,
        requester: "issuer:user-a",
        reason: "cleanup tunnel",
        resource_version_precondition: "sha256:pf-1",
      }),
      expect.objectContaining({
        access_request_id: "exec-1",
        action: "cancel",
        previous_status: "accepted",
        status: "cancelled",
        cancelled_at: updatedAt,
        requester: "issuer:user-a",
        reason: "stop command",
        resource_version_precondition: "sha256:exec-1",
      }),
    ]);
  });

  test("rejects runtime access creation without a concrete target resource", async () => {
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async () => {
        throw new Error("sql should not be called without a runtime access target");
      },
    };
    (repo as any).resources = async () => {
      throw new Error("resource inventory should not be loaded without a runtime access target");
    };

    await expect(RuntimeRepository.prototype.createPortForwardRequest.call(repo as any, {
      run: cloudRunRecord(),
      targetPort: 8080,
    })).rejects.toMatchObject({
      status: 400,
      code: "runtime_access_target_required",
    });

    await expect(RuntimeRepository.prototype.createExecRequest.call(repo as any, {
      run: cloudRunRecord(),
      command: ["python", "-V"],
    })).rejects.toMatchObject({
      status: 400,
      code: "runtime_access_target_required",
    });
  });

  test("uses the selected target runner binding when checking port-forward capability", async () => {
    const observed: {
      selectValues?: unknown[];
    } = {};
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("select attempt.*")) {
        observed.selectValues = values;
        return [];
      }
      return [];
    }) as any;
    const sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = sql;
    (repo as any).resources = async () => ({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceList",
      cloud_run_id: "run-1",
      core_run_ids: [],
      resources: [
        runtimeResourceFixture({
          kind: "RunnerAttempt",
          name: "attempt-with-port",
          spec: {
            runner_instance_id: "runner-with-port",
            worker_id: "worker-with-port",
          },
          status: {
            phase: "running",
            runner_instance_status: "online",
            access: {
              port_forward: true,
              exec: true,
            },
          },
        }),
        runtimeResourceFixture({
          kind: "RunnerAttempt",
          name: "attempt-without-port",
          labels: {
            "bucephalus.dev/worker-id": "worker-without-port",
          },
          spec: {
            runner_instance_id: "runner-without-port",
            worker_id: "worker-without-port",
          },
          status: {
            phase: "running",
            runner_instance_status: "online",
            access: {
              port_forward: false,
              exec: true,
            },
          },
        }),
        runtimeResourceFixture({
          kind: "ScheduleSlot",
          name: "core-run-1.0",
          spec: {
            core_run_id: "core-run-1",
            schedule_idx: 0,
          },
          status: {
            phase: "active",
            trial_id: "trial-1",
            attempt: 0,
            worker_id: "worker-without-port",
            runner_binding: {
              runner_instance_id: "runner-without-port",
              runner_instance_status: "online",
              attempt_id: "attempt-without-port",
              worker_id: "worker-without-port",
              access: {
                port_forward: false,
                exec: true,
              },
            },
            access: {
              reachable: true,
              reason: "reachable",
              port_forward: false,
              exec: true,
              runner_instance_id: "runner-without-port",
              runner_instance_status: "online",
              attempt_id: "attempt-without-port",
              worker_id: "worker-without-port",
            },
          },
        }),
        runtimeResourceFixture({
          kind: "TrialContainer",
          name: "trial-1.agent.container-1",
          spec: {
            core_run_id: "core-run-1",
            trial_id: "trial-1",
            attempt: 0,
          },
          status: {
            phase: "running",
            runner_binding: {
              runner_instance_id: "runner-without-port",
              runner_instance_status: "online",
              attempt_id: "attempt-without-port",
              worker_id: "worker-without-port",
              access: {
                port_forward: false,
                exec: true,
              },
            },
            access: {
              reachable: true,
              reason: "reachable",
              port_forward: false,
              exec: true,
              runner_instance_id: "runner-without-port",
              runner_instance_status: "online",
              attempt_id: "attempt-without-port",
              worker_id: "worker-without-port",
            },
          },
        }),
      ],
    });

    await expect(RuntimeRepository.prototype.createPortForwardRequest.call(repo as any, {
      run: cloudRunRecord(),
      resourceKind: "TrialContainer",
      resourceName: "trial-1.agent.container-1",
      targetPort: 8080,
    })).rejects.toThrow("Port forwarding requires an active runner attempt whose runner advertises runtime_port_forward");

    expect(observed.selectValues).toContain("runner-without-port");
    expect(observed.selectValues).toContain("attempt-without-port");
    expect(observed.selectValues).toContain("worker-without-port");
    expect(observed.selectValues).not.toContain("runner-with-port");
  });

  test("uses the selected target runner binding when checking exec capability", async () => {
    const observed: {
      selectValues?: unknown[];
    } = {};
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("select attempt.*")) {
        observed.selectValues = values;
        return [];
      }
      return [];
    }) as any;
    const sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = sql;
    (repo as any).resources = async () => ({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceList",
      cloud_run_id: "run-1",
      core_run_ids: [],
      resources: [
        runtimeResourceFixture({
          kind: "RunnerAttempt",
          name: "attempt-with-exec",
          spec: {
            runner_instance_id: "runner-with-exec",
            worker_id: "worker-with-exec",
          },
          status: {
            phase: "running",
            runner_instance_status: "online",
            access: {
              port_forward: true,
              exec: true,
            },
          },
        }),
        runtimeResourceFixture({
          kind: "RunnerAttempt",
          name: "attempt-without-exec",
          labels: {
            "bucephalus.dev/worker-id": "worker-without-exec",
          },
          spec: {
            runner_instance_id: "runner-without-exec",
            worker_id: "worker-without-exec",
          },
          status: {
            phase: "running",
            runner_instance_status: "online",
            access: {
              port_forward: true,
              exec: false,
            },
          },
        }),
        runtimeResourceFixture({
          kind: "ScheduleSlot",
          name: "core-run-1.0",
          spec: {
            core_run_id: "core-run-1",
            schedule_idx: 0,
          },
          status: {
            phase: "active",
            trial_id: "trial-1",
            attempt: 0,
            worker_id: "worker-without-exec",
            runner_binding: {
              runner_instance_id: "runner-without-exec",
              runner_instance_status: "online",
              attempt_id: "attempt-without-exec",
              worker_id: "worker-without-exec",
              access: {
                port_forward: true,
                exec: false,
              },
            },
            access: {
              reachable: true,
              reason: "reachable",
              port_forward: true,
              exec: false,
              runner_instance_id: "runner-without-exec",
              runner_instance_status: "online",
              attempt_id: "attempt-without-exec",
              worker_id: "worker-without-exec",
            },
          },
        }),
        runtimeResourceFixture({
          kind: "TrialContainer",
          name: "trial-1.agent.container-1",
          spec: {
            core_run_id: "core-run-1",
            trial_id: "trial-1",
            attempt: 0,
          },
          status: {
            phase: "running",
            runner_binding: {
              runner_instance_id: "runner-without-exec",
              runner_instance_status: "online",
              attempt_id: "attempt-without-exec",
              worker_id: "worker-without-exec",
              access: {
                port_forward: true,
                exec: false,
              },
            },
            access: {
              reachable: true,
              reason: "reachable",
              port_forward: true,
              exec: false,
              runner_instance_id: "runner-without-exec",
              runner_instance_status: "online",
              attempt_id: "attempt-without-exec",
              worker_id: "worker-without-exec",
            },
          },
        }),
      ],
    });

    await expect(RuntimeRepository.prototype.createExecRequest.call(repo as any, {
      run: cloudRunRecord(),
      resourceKind: "TrialContainer",
      resourceName: "trial-1.agent.container-1",
      command: ["python", "-V"],
    })).rejects.toThrow("Runtime exec requires an active runner attempt whose runner advertises runtime_exec");

    expect(observed.selectValues).toContain("runner-without-exec");
    expect(observed.selectValues).toContain("attempt-without-exec");
    expect(observed.selectValues).toContain("worker-without-exec");
    expect(observed.selectValues).not.toContain("runner-with-exec");
  });

  test("rejects active port-forward updates without a usable connection handle", async () => {
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async () => {
        throw new Error("sql should not be called for invalid active port-forward updates");
      },
      json: (payload: JsonObject) => payload,
    };

    await expect(RuntimeRepository.prototype.updatePortForwardRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "pf-1",
      status: "active",
      connection: {
        mode: "worker_command",
      },
    })).rejects.toThrow("Active port-forward updates must include an auditable connection handle");
  });

  test("rejects completed exec updates without an exit code", async () => {
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async () => {
        throw new Error("sql should not be called for invalid completed exec updates");
      },
      json: (payload: JsonObject) => payload,
    };

    await expect(RuntimeRepository.prototype.updateExecRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "exec-1",
      status: "completed",
      connection: {
        mode: "worker_command",
        stdout_tail: "ok\n",
      },
    })).rejects.toThrow("Completed exec updates must include a numeric connection.exit_code");
  });

  test("rejects port-forward activation before the worker accepts the resource", async () => {
    const observed = {
      updateCount: 0,
      updateQuery: "",
      insertedEvent: false,
    };
    const tx = ((strings: TemplateStringsArray) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runtime_access_requests")) {
        observed.updateCount += 1;
        if (observed.updateCount === 2) {
          observed.updateQuery = query;
        }
        return [];
      }
      if (query.includes("select status")) {
        return [{ status: "requested" }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.insertedEvent = true;
      }
      return [];
    }) as any;
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };

    await expect(RuntimeRepository.prototype.updatePortForwardRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "pf-1",
      status: "active",
      connection: {
        local_probe: "tcp://127.0.0.1:18080",
      },
    })).rejects.toMatchObject({
      status: 409,
      code: "runtime_access_transition_invalid",
      detail: expect.objectContaining({
        access_request_id: "pf-1",
        kind: "port_forward",
        current_status: "requested",
        target_status: "active",
        allowed_previous_statuses: ["accepted", "active"],
      }),
    });
    expect(observed.updateQuery).toContain("and status = any(");
    expect(observed.insertedEvent).toBe(false);
  });

  test("rejects exec completion before the worker accepts the resource", async () => {
    const observed = {
      updateCount: 0,
      updateQuery: "",
      insertedEvent: false,
    };
    const tx = ((strings: TemplateStringsArray) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runtime_access_requests")) {
        observed.updateCount += 1;
        if (observed.updateCount === 2) {
          observed.updateQuery = query;
        }
        return [];
      }
      if (query.includes("select status")) {
        return [{ status: "requested" }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.insertedEvent = true;
      }
      return [];
    }) as any;
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };

    await expect(RuntimeRepository.prototype.updateExecRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "exec-1",
      status: "completed",
      connection: {
        exit_code: 0,
        stdout_tail: "ok\n",
      },
    })).rejects.toMatchObject({
      status: 409,
      code: "runtime_access_transition_invalid",
      detail: expect.objectContaining({
        access_request_id: "exec-1",
        kind: "exec",
        current_status: "requested",
        target_status: "completed",
        allowed_previous_statuses: ["accepted", "active"],
      }),
    });
    expect(observed.updateQuery).toContain("and status = any(");
    expect(observed.insertedEvent).toBe(false);
  });

  test("rejects worker port-forward updates after cancellation wins the race", async () => {
    const observed = {
      updateQuery: "",
      insertedEvent: false,
    };
    const tx = ((strings: TemplateStringsArray) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runtime_access_requests")) {
        observed.updateQuery = query;
        return [];
      }
      if (query.includes("select status")) {
        return [{ status: "cancelled" }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.insertedEvent = true;
      }
      return [];
    }) as any;
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };

    await expect(RuntimeRepository.prototype.updatePortForwardRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "pf-1",
      status: "active",
      connection: {
        local_probe: "tcp://127.0.0.1:18080",
      },
    })).rejects.toMatchObject({
      status: 409,
      code: "runtime_access_request_not_active",
      detail: expect.objectContaining({
        access_request_id: "pf-1",
        kind: "port_forward",
        current_status: "cancelled",
      }),
    });
    expect(observed.updateQuery).toContain("and status = any(");
    expect(observed.insertedEvent).toBe(false);
  });

  test("expires due port-forward requests before worker activation updates", async () => {
    const observed: {
      updateCount: number;
      eventTypes: string[];
      payloads: JsonObject[];
    } = {
      updateCount: 0,
      eventTypes: [],
      payloads: [],
    };
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runtime_access_requests")) {
        observed.updateCount += 1;
        if (observed.updateCount === 1) {
          return [{
            ...accessRequestRecord({
            access_request_id: "pf-ttl",
            kind: "port_forward",
            status: "expired",
            expires_at: "2026-06-04T00:05:00Z",
            error_message: "runtime access request expired",
            }),
            previous_status: "accepted",
          }];
        }
        return [];
      }
      if (query.includes("select status")) {
        return [{ status: "expired" }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.eventTypes.push(String(values[3]));
        observed.payloads.push(values[4] as JsonObject);
      }
      return [];
    }) as any;
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };

    await expect(RuntimeRepository.prototype.updatePortForwardRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "pf-ttl",
      status: "active",
      connection: {
        local_probe: "tcp://127.0.0.1:18080",
      },
    })).rejects.toMatchObject({
      status: 409,
      code: "runtime_access_request_not_active",
      detail: expect.objectContaining({
        access_request_id: "pf-ttl",
        kind: "port_forward",
        current_status: "expired",
      }),
    });

    expect(observed.updateCount).toBe(2);
    expect(observed.eventTypes).toEqual(["runtime.access.port_forward.expired"]);
    expect(observed.payloads).toEqual([
      expect.objectContaining({
        access_request_id: "pf-ttl",
        previous_status: "accepted",
        status: "expired",
        expires_at: "2026-06-04T00:05:00Z",
      }),
    ]);
  });

  test("rejects worker exec updates after the request is already terminal", async () => {
    const observed = {
      updateQuery: "",
      insertedEvent: false,
    };
    const tx = ((strings: TemplateStringsArray) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runtime_access_requests")) {
        observed.updateQuery = query;
        return [];
      }
      if (query.includes("select status")) {
        return [{ status: "expired" }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.insertedEvent = true;
      }
      return [];
    }) as any;
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };

    await expect(RuntimeRepository.prototype.updateExecRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "exec-1",
      status: "completed",
      connection: {
        exit_code: 0,
        stdout_tail: "ok\n",
      },
    })).rejects.toMatchObject({
      status: 409,
      code: "runtime_access_request_not_active",
      detail: expect.objectContaining({
        access_request_id: "exec-1",
        kind: "exec",
        current_status: "expired",
      }),
    });
    expect(observed.updateQuery).toContain("and status = any(");
    expect(observed.insertedEvent).toBe(false);
  });

  test("expires due exec requests before worker completion updates", async () => {
    const observed: {
      updateCount: number;
      eventTypes: string[];
      payloads: JsonObject[];
    } = {
      updateCount: 0,
      eventTypes: [],
      payloads: [],
    };
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runtime_access_requests")) {
        observed.updateCount += 1;
        if (observed.updateCount === 1) {
          return [{
            ...accessRequestRecord({
            access_request_id: "exec-ttl",
            kind: "exec",
            status: "expired",
            protocol: "exec",
            command: ["python", "-V"],
            expires_at: "2026-06-04T00:06:00Z",
            error_message: "runtime access request expired",
            }),
            previous_status: "active",
          }];
        }
        return [];
      }
      if (query.includes("select status")) {
        return [{ status: "expired" }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.eventTypes.push(String(values[3]));
        observed.payloads.push(values[4] as JsonObject);
      }
      return [];
    }) as any;
    const repo = Object.create(RuntimeRepository.prototype) as RuntimeRepository;
    (repo as any).sql = {
      begin: async (callback: (tx: any) => Promise<unknown>) => await callback(tx),
      json: (payload: JsonObject) => payload,
    };

    await expect(RuntimeRepository.prototype.updateExecRequest.call(repo as any, {
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "exec-ttl",
      status: "completed",
      connection: {
        exit_code: 0,
        stdout_tail: "ok\n",
      },
    })).rejects.toMatchObject({
      status: 409,
      code: "runtime_access_request_not_active",
      detail: expect.objectContaining({
        access_request_id: "exec-ttl",
        kind: "exec",
        current_status: "expired",
      }),
    });

    expect(observed.updateCount).toBe(2);
    expect(observed.eventTypes).toEqual(["runtime.access.exec.expired"]);
    expect(observed.payloads).toEqual([
      expect.objectContaining({
        access_request_id: "exec-ttl",
        previous_status: "active",
        status: "expired",
        expires_at: "2026-06-04T00:06:00Z",
      }),
    ]);
  });

  test("projects declared run requirements as Kubernetes-shaped resources without secret values", () => {
    const resources = declaredRuntimeResources({
      run_id: "run-1",
      package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      run_label: "security",
      status: "running",
      env: {
        PUBLIC_FLAG: "1",
        SENSITIVE_ENV: "secret-env-value",
      },
      secret_refs: {
        OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
      },
      package_provenance: {},
      runtime_options: {
        max_parallel_trials: 4,
      },
      run_requirements: {
        executor: "runner-docker",
        requires: ["core_runner", "docker_daemon", "secret_resolver", "sidecar:redis"],
        image_refs: ["us-docker.pkg.dev/acme/runners/agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        secret_ids: ["OPENAI_API_KEY"],
        network_perimeter: {
          default: "none",
          task_sandbox: "none",
          agent: "none",
          egress_hosts: ["api.openai.com"],
        },
        sidecars: ["redis"],
        accelerators: [],
        arch: "x86_64",
        cpu_count: 4,
        memory_mb: 8192,
        disk_mb: 32768,
        isolation: "single_use_vm",
        timeout_ms: 600000,
        max_parallel_trials: 4,
      },
      created_at: "2026-06-04T00:00:00Z",
      updated_at: "2026-06-04T00:01:00Z",
      started_at: "2026-06-04T00:00:30Z",
      completed_at: null,
      error_message: null,
    });

    expect(resources.map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual([
      "Run/run-1",
      "Package/sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "RunnerShape/requested",
      "CapabilityRequirement/core_runner",
      "CapabilityRequirement/docker_daemon",
      "CapabilityRequirement/secret_resolver",
      "CapabilityRequirement/sidecar-redis",
      "ImagePull/us-docker.pkg.dev-acme-runners-agent-sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "SecretBinding/openai_api_key",
      "NetworkPerimeter/declared",
      "SidecarRequirement/redis",
    ]);
    expect(resources.find((resource) => resource.kind === "RunnerShape")).toMatchObject({
      spec: {
        executor: "runner-docker",
        cpu_count: 4,
        memory_mb: 8192,
        isolation: "single_use_vm",
      },
      status: {
        phase: "Pending",
        reason: "WaitingForRunner",
        message: "No active runner attempt has reported capabilities for this run.",
      },
    });
    const runResource = resources.find((resource) => resource.kind === "Run");
    expect(runtimeConditionsByType(runResource!).Ready).toMatchObject({
      status: "True",
      reason: "Running",
    });
    expect(runResource!.status).toMatchObject({
      reason: "Unreachable",
      message: "Runtime access target is not reachable: run has no active runner attempt",
    });
    expect(runtimeConditionsByType(runResource!).Reachable).toMatchObject({
      status: "False",
      reason: "Unreachable",
      message: "Runtime access target is not reachable: run has no active runner attempt",
    });
    expect(runtimeConditionsByType(runResource!).ExecReady).toMatchObject({
      status: "False",
      reason: "AttemptUnreachable",
    });
    expect(resources.find((resource) => resource.kind === "SecretBinding")).toMatchObject({
      spec: {
        secret_id: "OPENAI_API_KEY",
      },
      status: {
        phase: "Pending",
        reason: "WaitingForRunner",
        message: "No active runner attempt has reported whether secret resolution is available for OPENAI_API_KEY.",
      },
    });
    expect(resources.find((resource) => resource.kind === "ImagePull")).toMatchObject({
      status: {
        phase: "Pending",
        reason: "WaitingForRunner",
      },
    });
    expect(resources.find((resource) => resource.kind === "NetworkPerimeter")).toMatchObject({
      status: {
        phase: "Pending",
        reason: "WaitingForRunner",
      },
    });
    expect(resources.find((resource) => resource.kind === "SidecarRequirement" && resource.metadata.name === "redis")).toMatchObject({
      status: {
        phase: "Pending",
        reason: "WaitingForRunner",
      },
    });
    expect(JSON.stringify(resources)).not.toContain("gcp-secret-manager://");
    expect(JSON.stringify(resources)).not.toContain("secret-env-value");

    const activeResources = declaredRuntimeResources({
      ...cloudRunRecord(),
      run_requirements: {
        ...cloudRunRecord().run_requirements,
        requires: ["core_runner", "docker_daemon", "secret_resolver", "network_perimeter", "sidecar:redis", "accelerator:gpu-a10"],
        image_refs: ["us-docker.pkg.dev/acme/runners/agent@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
        secret_ids: ["OPENAI_API_KEY"],
        network_perimeter: {
          default: "none",
          task_sandbox: "none",
          agent: "none",
          egress_hosts: ["api.openai.com"],
        },
        sidecars: ["redis"],
        accelerators: ["gpu-a10"],
        isolation: "single_use_vm",
      },
    }, [attemptRecord({
      runner_instance_capabilities: {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver", "network_perimeter", "sidecar:redis"],
        arch: "x86_64",
        cpu_count: 4,
        memory_mb: 8192,
        disk_mb: 32768,
        isolation: ["single_use_vm"],
      },
    })]);
    expect(activeResources.find((resource) => resource.kind === "RunnerShape")).toMatchObject({
      status: {
        phase: "Satisfied",
        reason: "RunnerShapeSatisfied",
        satisfied_attempt_ids: ["attempt-1"],
      },
    });
    expect(runtimeConditionsByType(activeResources.find((resource) => resource.kind === "RunnerShape")!).Ready).toMatchObject({
      status: "True",
      reason: "Satisfied",
    });
    expect(activeResources.find((resource) => resource.kind === "CapabilityRequirement" && resource.metadata.name === "core_runner")).toMatchObject({
      status: {
        phase: "Satisfied",
        reason: "CapabilitySatisfied",
        satisfied_attempt_ids: ["attempt-1"],
      },
    });
    expect(activeResources.find((resource) => resource.kind === "CapabilityRequirement" && resource.metadata.name === "secret_resolver")).toMatchObject({
      status: {
        phase: "Satisfied",
        reason: "CapabilitySatisfied",
        satisfied_attempt_ids: ["attempt-1"],
      },
    });
    expect(activeResources.find((resource) => resource.kind === "CapabilityRequirement" && resource.metadata.name === "accelerator-gpu-a10")).toMatchObject({
      status: {
        phase: "Unsatisfied",
        reason: "CapabilityMissing",
        missing_attempt_ids: ["attempt-1"],
      },
    });
    expect(runtimeConditionsByType(activeResources.find((resource) => resource.kind === "CapabilityRequirement" && resource.metadata.name === "accelerator-gpu-a10")!).Ready).toMatchObject({
      status: "False",
      reason: "Unsatisfied",
    });
    expect(activeResources.find((resource) => resource.kind === "ImagePull")).toMatchObject({
      status: {
        phase: "Satisfied",
        reason: "RegistryPullAvailable",
        satisfied_attempt_ids: ["attempt-1"],
      },
    });
    expect(activeResources.find((resource) => resource.kind === "SecretBinding" && resource.metadata.name === "openai_api_key")).toMatchObject({
      status: {
        phase: "Satisfied",
        reason: "SecretResolverAvailable",
        satisfied_attempt_ids: ["attempt-1"],
      },
    });
    expect(activeResources.find((resource) => resource.kind === "NetworkPerimeter")).toMatchObject({
      status: {
        phase: "Satisfied",
        reason: "NetworkPerimeterAvailable",
        satisfied_attempt_ids: ["attempt-1"],
      },
    });
    expect(activeResources.find((resource) => resource.kind === "SidecarRequirement" && resource.metadata.name === "redis")).toMatchObject({
      status: {
        phase: "Satisfied",
        reason: "SidecarCapabilityAvailable",
        satisfied_attempt_ids: ["attempt-1"],
      },
    });
    const acceleratorRequirement = activeResources.find((resource) => resource.kind === "AcceleratorRequirement" && resource.metadata.name === "gpu-a10");
    expect(acceleratorRequirement).toMatchObject({
      status: {
        phase: "Unsatisfied",
        reason: "AcceleratorCapabilityMissing",
        missing_attempt_ids: ["attempt-1"],
      },
    });
    expect(runtimeConditionsByType(acceleratorRequirement!).Ready).toMatchObject({
      status: "False",
      reason: "Unsatisfied",
    });

    const offlineResources = declaredRuntimeResources({
      ...cloudRunRecord(),
      run_requirements: {
        ...cloudRunRecord().run_requirements,
        requires: ["core_runner", "runtime_exec"],
        image_refs: ["us-docker.pkg.dev/acme/runners/agent@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],
        secret_ids: ["OPENAI_API_KEY"],
      },
    }, [attemptRecord({
      runner_instance_status: "offline",
      runner_instance_capabilities: {
        executors: ["runner-docker"],
        resources: ["core_runner", "runtime_exec", "registry_pull", "secret_resolver"],
        arch: "x86_64",
        cpu_count: 4,
        memory_mb: 8192,
        disk_mb: 32768,
        isolation: ["single_use_vm"],
      },
    })]);
    expect(offlineResources.find((resource) => resource.kind === "RunnerShape")).toMatchObject({
      status: {
        phase: "Unsatisfied",
        reason: "RunnerUnavailable",
        active_attempt_ids: ["attempt-1"],
        unavailable_attempts: [
          {
            attempt_id: "attempt-1",
            runner_instance_id: "runner-instance-1",
            runner_instance_status: "offline",
          },
        ],
      },
    });
    expect(offlineResources.find((resource) => resource.kind === "CapabilityRequirement" && resource.metadata.name === "core_runner")).toMatchObject({
      status: {
        phase: "Unsatisfied",
        reason: "RunnerUnavailable",
      },
    });
    expect(offlineResources.find((resource) => resource.kind === "ImagePull")).toMatchObject({
      status: {
        phase: "Unsatisfied",
        reason: "RunnerUnavailable",
      },
    });
    const offlineRun = offlineResources.find((resource) => resource.kind === "Run");
    expect(offlineRun).toMatchObject({
      status: {
        access: {
          port_forward: false,
          exec: false,
          runner_instance_id: "runner-instance-1",
          runner_instance_status: "offline",
          attempt_id: "attempt-1",
        },
      },
    });
    expect(runtimeConditionsByType(offlineRun!).Reachable).toMatchObject({
      status: "False",
      reason: "Unreachable",
      message: "Runtime access target is not reachable: runner status is offline",
    });
    expect(runtimeConditionsByType(offlineRun!).ExecReady).toMatchObject({
      status: "False",
      reason: "AttemptUnreachable",
      message: "Runner attempt is not reachable: runner status is offline",
    });
  });

  test("filters runtime resources with kind, label, and field selectors", () => {
    const resources: RuntimeResourceRecord[] = [
      runtimeResourceFixture({
        kind: "RunnerAttempt",
        name: "attempt-1",
        generation: 7,
        labels: {
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/resource-kind": "RunnerAttempt",
          "bucephalus.dev/runner-instance-id": "runner-1",
        },
        status: {
          phase: "running",
          access: {
            exec: true,
          },
        },
        audit: {
          source: "cloud.run_attempts",
        },
      }),
      runtimeResourceFixture({
        kind: "PortForward",
        name: "pf-1",
        ownerReferences: [
          {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "Run",
            name: "run-1",
            uid: "run-1",
          },
          {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "TrialContainer",
            name: "trial-1.agent.container-1",
            uid: "trial-1.agent.container-1",
          },
          {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "RunnerInstance",
            name: "runner-1",
            uid: "runner-1",
          },
        ],
        labels: {
          "bucephalus.dev/run-id": "run-1",
          "bucephalus.dev/resource-kind": "TrialContainer",
        },
        status: {
          phase: "requested",
          conditions: [
            runtimeCondition("Ready", "False", "Requested", "Resource is waiting in phase requested"),
            runtimeCondition("ClientReachable", "Unknown", "Requested", "PortForward client reachability is unknown while phase is requested"),
          ],
        },
        audit: {
          source: "cloud.runtime_access_requests",
        },
      }),
      runtimeResourceFixture({
        kind: "TrialContainer",
        name: "trial-1.agent.container-1",
        labels: {
          "bucephalus.dev/run-id": "run-1",
        },
        status: {
          phase: "completed",
        },
        audit: {
          source: "bucephalus_runtime.trial_attempt_containers",
        },
      }),
    ];

    expect(filterRuntimeResources(resources, {
      kinds: ["RunnerAttempt", "PortForward"],
      labelSelector: "bucephalus.dev/run-id=run-1,!missing",
      fieldSelector: "status.phase!=completed",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual([
      "RunnerAttempt/attempt-1",
      "PortForward/pf-1",
    ]);

    expect(filterRuntimeResources(resources, {
      kinds: ["container", "portforwards"],
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual([
      "PortForward/pf-1",
      "TrialContainer/trial-1.agent.container-1",
    ]);

    expect(filterRuntimeResources(resources, {
      labelSelector: "bucephalus.dev/resource-kind=TrialContainer",
      fieldSelector: "kind=PortForward,audit.source=cloud.runtime_access_requests",
    }).map((resource) => resource.metadata.name)).toEqual(["pf-1"]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "kind=pf",
    }).map((resource) => resource.metadata.name)).toEqual(["pf-1"]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "metadata.ownerReferences.kind=runner",
    }).map((resource) => resource.metadata.name)).toEqual(["pf-1"]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "status.conditions.ClientReachable=Unknown",
    }).map((resource) => resource.metadata.name)).toEqual(["pf-1"]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "status.conditions.clientreachable.status=Unknown,status.conditions.ClientReachable.reason=Requested",
    }).map((resource) => resource.metadata.name)).toEqual(["pf-1"]);

    expect(filterRuntimeResources(resources, {
      labelSelector: "bucephalus.dev/resource-kind in (RunnerAttempt, TrialContainer)",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual([
      "RunnerAttempt/attempt-1",
      "PortForward/pf-1",
    ]);

    expect(filterRuntimeResources(resources, {
      labelSelector: "bucephalus.dev/resource-kind notin (TrialContainer)",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual([
      "RunnerAttempt/attempt-1",
      "TrialContainer/trial-1.agent.container-1",
    ]);

    expect(filterRuntimeResources(resources, {
      labelSelector: "bucephalus.dev/run-id==run-1",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual([
      "RunnerAttempt/attempt-1",
      "PortForward/pf-1",
      "TrialContainer/trial-1.agent.container-1",
    ]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "metadata.ownerReferences.kind=RunnerInstance",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual(["PortForward/pf-1"]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "metadata.generation=7",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual(["RunnerAttempt/attempt-1"]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "metadata.ownerReferences.name=trial-1.agent.container-1",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual(["PortForward/pf-1"]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "metadata.ownerReferences.uid=runner-1",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual(["PortForward/pf-1"]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "metadata.ownerReferences=TrialContainer/trial-1.agent.container-1",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual(["PortForward/pf-1"]);

    expect(filterRuntimeResources(resources, {
      fieldSelector: "metadata.ownerReferences.kind!=RunnerInstance",
    }).map((resource) => `${resource.kind}/${resource.metadata.name}`)).toEqual([
      "RunnerAttempt/attempt-1",
      "TrialContainer/trial-1.agent.container-1",
    ]);

    expect(() => filterRuntimeResources(resources, {
      fieldSelector: "status.phase in (running,requested)",
    })).toThrow("field_selector does not support in/notin expressions");

    expect(() => filterRuntimeResources(resources, {
      labelSelector: "bucephalus.dev/resource-kind in RunnerAttempt",
    })).toThrow("label_selector set expressions must use key in (value,...)");
  });

  test("resolves runtime access targets from visible targetable resources", () => {
    const resources = [
      runtimeResourceFixture({
        kind: "Run",
        name: "run-1",
        status: {
          phase: "running",
          access: {
            reachable: true,
            reason: "reachable",
            port_forward: true,
            exec: true,
            runner_instance_id: "runner-instance-1",
            runner_instance_status: "online",
            attempt_id: "attempt-1",
            worker_id: "worker-1",
          },
        },
      }),
      runtimeResourceFixture({
        kind: "Package",
        name: "sha256-aaa",
      }),
      runtimeResourceFixture({
        kind: "RunnerInstance",
        name: "runner-instance-1",
        spec: {
          runner_instance_id: "runner-instance-1",
        },
        status: {
          phase: "online",
          current_attempt_id: "attempt-1",
          active_attempts: 1,
        },
      }),
      runtimeResourceFixture({
        kind: "RunnerAttempt",
        name: "attempt-1",
        labels: {
          "bucephalus.dev/worker-id": "worker-1",
        },
        spec: {
          runner_instance_id: "runner-instance-1",
          worker_id: "worker-1",
        },
        status: {
          phase: "running",
          runner_instance_status: "online",
        },
      }),
      runtimeResourceFixture({
        kind: "ScheduleSlot",
        name: "core-run-1.0",
        spec: {
          core_run_id: "core-run-1",
          schedule_idx: 0,
        },
        status: {
          phase: "active",
          trial_id: "trial-1",
          attempt: 0,
          worker_id: "worker-1",
          runner_binding: {
            worker_id: "worker-1",
            attempt_id: "attempt-1",
            runner_instance_id: "runner-instance-1",
            runner_instance_status: "online",
          },
        },
      }),
      runtimeResourceFixture({
        kind: "TrialContainer",
        name: "trial-1.agent.container-1",
        spec: {
          core_run_id: "core-run-1",
          trial_id: "trial-1",
          schedule_idx: 0,
          attempt: 0,
        },
        status: {
          phase: "running",
          runner_binding: {
            worker_id: "worker-1",
            attempt_id: "attempt-1",
            runner_instance_id: "runner-instance-1",
            runner_instance_status: "online",
          },
        },
      }),
    ];

    expect(runtimeAccessTargetFromInventory(resources, {
      resourceKind: "Run",
      resourceName: "run-1",
    })).toMatchObject({
      kind: "Run",
      name: "run-1",
      uid: "run-1",
      runnerInstanceId: "runner-instance-1",
      attemptId: "attempt-1",
      workerId: "worker-1",
    });
    expect(runtimeAccessTargetFromInventory(resources, {
      resourceKind: "trialcontainer",
      resourceName: "TRIAL-1.AGENT.CONTAINER-1",
    })).toMatchObject({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      runnerInstanceId: "runner-instance-1",
      attemptId: "attempt-1",
      workerId: "worker-1",
    });
    expect(runtimeAccessTargetFromInventory(resources, {
      resourceKind: "ScheduleSlot",
      resourceName: "core-run-1.0",
    })).toMatchObject({
      kind: "ScheduleSlot",
      name: "core-run-1.0",
      runnerInstanceId: "runner-instance-1",
      attemptId: "attempt-1",
      workerId: "worker-1",
    });
    expect(runtimeAccessTargetFromInventory(resources, {
      resourceKind: "RunnerInstance",
      resourceName: "runner-instance-1",
    })).toMatchObject({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      runnerInstanceId: "runner-instance-1",
      attemptId: "attempt-1",
      workerId: "worker-1",
    });
    expect(runtimeAccessTargetFromInventory(resources, {
      resourceKind: "runner",
      resourceName: "runner-instance-1",
    })).toMatchObject({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      runnerInstanceId: "runner-instance-1",
      attemptId: "attempt-1",
      workerId: "worker-1",
    });
    expect(runtimeAccessTargetFromInventory(resources, {
      resourceKind: "RunnerAttempt",
      resourceName: "attempt-1",
    })).toMatchObject({
      kind: "RunnerAttempt",
      name: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      attemptId: "attempt-1",
      workerId: "worker-1",
    });

    const target = resources.find((resource) => resource.kind === "TrialContainer");
    expect(target).toBeDefined();
    target!.metadata.resourceVersion = "sha256:current";
    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "TrialContainer",
      resourceName: "trial-1.agent.container-1",
      resourceVersion: "sha256:stale",
    })).toThrow("TrialContainer/trial-1.agent.container-1 changed since it was reviewed");
  });

  test("rejects incomplete, unknown, and unsupported runtime access targets", () => {
    const resources = [
      runtimeResourceFixture({
        kind: "Package",
        name: "sha256-aaa",
      }),
      runtimeResourceFixture({
        kind: "RunnerAttempt",
        name: "attempt-1",
      }),
    ];

    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "RunnerAttempt",
    })).toThrow("resource_kind and resource_name must be provided together");
    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "TrialContainer",
      resourceName: "missing",
    })).toThrow("Runtime access target resource not found");
    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "Package",
      resourceName: "sha256-aaa",
    })).toThrow("Package/sha256-aaa is not a supported runtime access target");
  });

  test("rejects visible runtime access targets that are not reachable", () => {
    const resources = [
      runtimeResourceFixture({
        kind: "RunnerInstance",
        name: "runner-offline",
        status: {
          phase: "offline",
          active_attempts: 1,
          current_attempt_id: "attempt-1",
        },
      }),
      runtimeResourceFixture({
        kind: "RunnerAttempt",
        name: "attempt-ended",
        status: {
          phase: "completed",
          runner_instance_status: "online",
        },
      }),
      runtimeResourceFixture({
        kind: "ScheduleSlot",
        name: "core-run-1.0",
        status: {
          phase: "active",
        },
      }),
      runtimeResourceFixture({
        kind: "ScheduleSlot",
        name: "core-run-1.1",
        status: {
          phase: "active",
          trial_id: "trial-1",
          attempt: 0,
        },
      }),
      runtimeResourceFixture({
        kind: "TrialContainer",
        name: "trial-1.agent.container-2",
        spec: {
          core_run_id: "core-run-1",
          trial_id: "trial-1",
          attempt: 0,
        },
        status: {
          phase: "running",
        },
      }),
      runtimeResourceFixture({
        kind: "TrialContainer",
        name: "trial-1.agent.container-1",
        spec: {
          trial_id: "trial-1",
          attempt: 0,
        },
        status: {
          phase: "completed",
        },
      }),
    ];

    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "RunnerInstance",
      resourceName: "runner-offline",
    })).toThrow("RunnerInstance/runner-offline is not currently reachable for runtime access: phase is offline");
    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "RunnerAttempt",
      resourceName: "attempt-ended",
    })).toThrow("RunnerAttempt/attempt-ended is not currently reachable for runtime access: phase is completed");
    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "ScheduleSlot",
      resourceName: "core-run-1.0",
    })).toThrow("ScheduleSlot/core-run-1.0 is not currently reachable for runtime access: slot has not been assigned to a trial");
    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "ScheduleSlot",
      resourceName: "core-run-1.1",
    })).toThrow("ScheduleSlot/core-run-1.1 is not currently reachable for runtime access: slot has no active worker assignment");
    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "TrialContainer",
      resourceName: "trial-1.agent.container-2",
    })).toThrow("TrialContainer/trial-1.agent.container-2 is not currently reachable for runtime access: container is not bound to an active runner attempt");
    expect(() => runtimeAccessTargetFromInventory(resources, {
      resourceKind: "TrialContainer",
      resourceName: "trial-1.agent.container-1",
    })).toThrow("TrialContainer/trial-1.agent.container-1 is not currently reachable for runtime access: phase is completed");
  });

  test("selects runtime events related to a resource identity", () => {
    const events = [
      runtimeEvent({
        row_seq: 1,
        event_type: "runtime.access.port_forward.requested",
        payload: {
          access_request_id: "pf-1",
        },
      }),
      runtimeEvent({
        row_seq: 2,
        event_type: "runtime.access.port_forward.active",
        payload: {
          access_request_id: "pf-other",
        },
      }),
      runtimeEvent({
        row_seq: 3,
        event_type: "runtime.access.port_forward.requested",
        payload: {
          access_resource_ref: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "PortForward",
            name: "pf-1",
            uid: "pf-1",
          },
          target_ref: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "TrialContainer",
            name: "trial-1.agent.container-1",
          },
        },
      }),
    ];

    expect(runtimeResourceEventsForResource(events, {
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "PortForward",
      metadata: {
        name: "pf-1",
        uid: "pf-1",
        labels: {
          "bucephalus.dev/run-id": "run-1",
        },
        annotations: {},
        ownerReferences: [],
      },
      spec: {
        access_request_id: "pf-1",
      },
      status: {
        phase: "requested",
      },
      audit: {
        source: "cloud.runtime_access_requests",
      },
    }).map((event) => event.row_seq)).toEqual([1, 3]);
    expect(runtimeResourceEventsForResource(events, {
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "TrialContainer",
      metadata: {
        name: "trial-1.agent.container-1",
        labels: {},
        annotations: {},
        ownerReferences: [],
      },
      spec: {},
      status: {
        phase: "running",
      },
      audit: {},
    }).map((event) => event.row_seq)).toEqual([3]);
  });

  test("selects runner runtime events by runner, attempt, and worker identity", () => {
    const events = [
      runtimeEvent({
        row_seq: 0,
        event_type: "runtime.resource.runner_pool.observed",
        row: {
          source: "cloud.run_events",
          runner_pool_id: "runner-pool-1",
        },
      }),
      runtimeEvent({
        row_seq: 1,
        event_type: "worker.cleanup.starting",
        payload: {
          runner_instance_id: "runner-instance-1",
          worker_id: "worker-1",
        },
      }),
      runtimeEvent({
        row_seq: 2,
        event_type: "worker.core.completed",
        row: {
          source: "cloud.run_events",
          attempt_id: "attempt-2",
        },
      }),
      runtimeEvent({
        row_seq: 3,
        event_type: "runtime.access.exec.active",
        payload: {
          runner_instance_id: "runner-other",
          worker_id: "worker-other",
          attempt_id: "attempt-other",
        },
      }),
      runtimeEvent({
        row_seq: 4,
        event_type: "runtime.resource.runner_pool.updated",
        payload: {
          runner_pool_id: "runner-pool-1",
        },
      }),
    ];

    const runnerPool = runtimeResourceFixture({
      kind: "RunnerPool",
      name: "runner-pool-1",
      labels: {
        "bucephalus.dev/runner-pool-id": "runner-pool-1",
      },
      spec: {
        runner_pool_id: "runner-pool-1",
      },
      status: {
        phase: "active",
      },
    });
    const runnerInstance = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      labels: {
        "bucephalus.dev/runner-instance-id": "runner-instance-1",
        "bucephalus.dev/current-attempt-id": "attempt-1",
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        attempt_ids: ["attempt-1", "attempt-2"],
        worker_ids: ["worker-1"],
      },
      status: {
        phase: "online",
        current_attempt_id: "attempt-1",
        actions: ["cordon", "drain"],
      },
    });
    const runnerAttempt = runtimeResourceFixture({
      kind: "RunnerAttempt",
      name: "attempt-1",
      labels: {
        "bucephalus.dev/runner-instance-id": "runner-instance-1",
        "bucephalus.dev/worker-id": "worker-1",
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        worker_id: "worker-1",
      },
      status: {
        phase: "running",
      },
    });

    expect(runtimeResourceEventsForResource(events, runnerPool).map((event) => event.row_seq)).toEqual([0, 4]);
    expect(runtimeResourceEventsForResource(events, runnerInstance).map((event) => event.row_seq)).toEqual([1, 2]);
    expect(runtimeResourceEventsForResource(events, runnerAttempt).map((event) => event.row_seq)).toEqual([1]);
  });

  test("serves runner attempt logs from worker control-plane events", async () => {
    const run = cloudRunRecord();
    const runnerAttempt = runtimeResourceFixture({
      kind: "RunnerAttempt",
      name: "attempt-1",
      labels: {
        "bucephalus.dev/runner-instance-id": "runner-instance-1",
        "bucephalus.dev/worker-id": "worker-1",
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        worker_id: "worker-1",
      },
      status: {
        phase: "running",
      },
    });
    const logs = await RuntimeRepository.prototype.resourceLogs.call({
      async describeResource(_cloudRunId: string, _run: ReturnType<typeof cloudRunRecord>, input: { kind: string; name: string }) {
        expect(input).toMatchObject({ kind: "RunnerAttempt", name: "attempt-1" });
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceDescribe",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resource: runnerAttempt,
          related_resources: [],
        };
      },
      async eventRows(_cloudRunId: string, input: { limit?: number | undefined; eventTypes?: string[] | undefined; resourceKind?: string | undefined; resourceName?: string | undefined }) {
        expect(input.limit).toBeGreaterThanOrEqual(250);
        expect(input.eventTypes).toEqual(["worker.core.completed", "worker.core.failed"]);
        expect(input.resourceKind).toBeUndefined();
        expect(input.resourceName).toBeUndefined();
        return [
          runtimeEvent({
            row_seq: 1,
            event_type: "worker.core.completed",
            core_run_id: "core-run-1",
            ts: "2026-06-04T00:00:10Z",
            payload: {
              attempt_id: "attempt-1",
              runner_instance_id: "runner-instance-1",
              stdout_tail: "line 1\nline 2\n",
              stderr_tail: "warn\n",
            },
          }),
          runtimeEvent({
            row_seq: 2,
            event_type: "runtime.resource.runner_instance.observed",
            payload: {
              attempt_id: "attempt-1",
              runner_instance_id: "runner-instance-1",
              stdout_tail: "not this attempt\n",
            },
          }),
          runtimeEvent({
            row_seq: 3,
            event_type: "worker.core.failed",
            core_run_id: "core-run-1",
            ts: "2026-06-04T00:00:20Z",
            payload: {
              attempt_id: "attempt-1",
              runner_instance_id: "runner-instance-1",
              stdout_tail: "line 3\n",
              stderr_tail: "boom\n",
            },
          }),
        ];
      },
    } as any, "run-1", run, {
      kind: "RunnerAttempt",
      name: "attempt-1",
      stream: "stdout",
      tailLines: 2,
    });

    expect(new TextDecoder().decode(logs.bytes)).toBe("line 2\nline 3\n");
    expect(logs.media_type).toBe("text/plain; charset=utf-8");
    expect(logs.object).toMatchObject({
      core_run_id: "core-run-1",
      trial_id: "",
      schedule_idx: -1,
      role: "stdout",
      object_ref: "runtime://cloud-run/run-1/runner-attempt/attempt-1/stdout",
      content_available: true,
      media_type: "text/plain; charset=utf-8",
      byte_size: 14,
      relative_path: "runner-attempt/attempt-1/stdout.log",
    });
    expect(logs.object.sha256).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(logs.object.metadata).toMatchObject({
      source: "cloud.run_events",
      resource_kind: "RunnerAttempt",
      resource_name: "attempt-1",
      event_types: ["worker.core.completed", "worker.core.failed"],
      runner_instance_id: "runner-instance-1",
      attempt_id: "attempt-1",
      worker_id: "worker-1",
    });
  });

  test("serves runner instance logs from worker control-plane events", async () => {
    const run = cloudRunRecord();
    const runnerInstance = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      labels: {
        "bucephalus.dev/runner-instance-id": "runner-instance-1",
        "bucephalus.dev/current-attempt-id": "attempt-1",
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        attempt_ids: ["attempt-1", "attempt-2"],
        worker_ids: ["worker-1", "worker-2"],
      },
      status: {
        phase: "online",
        current_attempt_id: "attempt-1",
        active_attempts: 2,
      },
    });
    const logs = await RuntimeRepository.prototype.resourceLogs.call({
      async describeResource(_cloudRunId: string, _run: ReturnType<typeof cloudRunRecord>, input: { kind: string; name: string }) {
        expect(input).toMatchObject({ kind: "RunnerInstance", name: "runner-instance-1" });
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceDescribe",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resource: runnerInstance,
          related_resources: [],
        };
      },
      async eventRows(_cloudRunId: string, input: { limit?: number | undefined; eventTypes?: string[] | undefined; resourceKind?: string | undefined; resourceName?: string | undefined }) {
        expect(input.limit).toBeGreaterThanOrEqual(250);
        expect(input.eventTypes).toEqual(["worker.core.completed", "worker.core.failed"]);
        expect(input.resourceKind).toBeUndefined();
        expect(input.resourceName).toBeUndefined();
        return [
          runtimeEvent({
            row_seq: 1,
            event_type: "worker.core.completed",
            core_run_id: "core-run-1",
            ts: "2026-06-04T00:00:10Z",
            payload: {
              attempt_id: "attempt-1",
              runner_instance_id: "runner-instance-1",
              stdout_tail: "attempt 1 done\n",
              stderr_tail: "",
            },
          }),
          runtimeEvent({
            row_seq: 2,
            event_type: "worker.core.completed",
            core_run_id: "core-run-2",
            ts: "2026-06-04T00:00:20Z",
            payload: {
              attempt_id: "attempt-2",
              runner_instance_id: "runner-instance-1",
              stdout_tail: "attempt 2 done\n",
              stderr_tail: "",
            },
          }),
          runtimeEvent({
            row_seq: 4,
            event_type: "runtime.resource.runner_instance.observed",
            payload: {
              runner_instance_id: "runner-instance-1",
              stdout_tail: "not a log event\n",
              stderr_tail: "",
            },
          }),
          runtimeEvent({
            row_seq: 3,
            event_type: "worker.core.completed",
            core_run_id: "core-run-other",
            payload: {
              attempt_id: "attempt-other",
              runner_instance_id: "runner-other",
              stdout_tail: "wrong runner\n",
            },
          }),
        ];
      },
    } as any, "run-1", run, {
      kind: "RunnerInstance",
      name: "runner-instance-1",
      stream: "stdout",
    });

    expect(new TextDecoder().decode(logs.bytes)).toBe("attempt 1 done\nattempt 2 done\n");
    expect(logs.object).toMatchObject({
      core_run_id: "core-run-1",
      trial_id: "",
      schedule_idx: -1,
      role: "stdout",
      object_ref: "runtime://cloud-run/run-1/runner-instance/runner-instance-1/stdout",
      content_available: true,
      media_type: "text/plain; charset=utf-8",
      byte_size: 30,
      relative_path: "runner-instance/runner-instance-1/stdout.log",
    });
    expect(logs.object.metadata).toMatchObject({
      source: "cloud.run_events",
      resource_kind: "RunnerInstance",
      resource_name: "runner-instance-1",
      event_types: ["worker.core.completed"],
      runner_instance_id: "runner-instance-1",
      attempt_ids: ["attempt-1", "attempt-2"],
      worker_ids: ["worker-1", "worker-2"],
    });
  });

  test("serves exec logs from runtime access request connection tails", async () => {
    const run = cloudRunRecord();
    const execResource = runtimeResourceFixture({
      kind: "Exec",
      name: "exec-1",
      spec: {
        access_request_id: "exec-1",
        resource_kind: "TrialContainer",
        resource_name: "trial-1.agent.container-1",
        target_ref: {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "TrialContainer",
          name: "trial-1.agent.container-1",
        },
        command: ["python", "-V"],
      },
      status: {
        phase: "completed",
        runner_instance_id: "runner-instance-1",
        attempt_id: "attempt-1",
        connection: {
          exit_code: 0,
          stdout_tail: "Python 3.12\nready\n",
          stderr_tail: "warn\n",
        },
      },
    });
    const repo = {
      async describeResource(_cloudRunId: string, _run: ReturnType<typeof cloudRunRecord>, input: { kind: string; name: string }) {
        expect(input).toMatchObject({ kind: "Exec", name: "exec-1" });
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceDescribe",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resource: execResource,
          related_resources: [],
        };
      },
    };

    const stdout = await RuntimeRepository.prototype.resourceLogs.call(repo as any, "run-1", run, {
      kind: "Exec",
      name: "exec-1",
      stream: "stdout",
      tailLines: 1,
    });
    const stderr = await RuntimeRepository.prototype.resourceLogs.call(repo as any, "run-1", run, {
      kind: "Exec",
      name: "exec-1",
      stream: "stderr",
    });

    expect(new TextDecoder().decode(stdout.bytes)).toBe("ready\n");
    expect(stdout.media_type).toBe("text/plain; charset=utf-8");
    expect(stdout.object).toMatchObject({
      core_run_id: "",
      role: "stdout",
      object_ref: "runtime://cloud-run/run-1/exec/exec-1/stdout",
      content_available: true,
      media_type: "text/plain; charset=utf-8",
      byte_size: 6,
      relative_path: "exec/exec-1/stdout.log",
      metadata: expect.objectContaining({
        source: "cloud.runtime_access_requests",
        resource_kind: "Exec",
        resource_name: "exec-1",
        runner_instance_id: "runner-instance-1",
        attempt_id: "attempt-1",
        phase: "completed",
        exit_code: 0,
      }),
    });
    expect(stdout.object.sha256).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(new TextDecoder().decode(stderr.bytes)).toBe("warn\n");
    expect(stderr.object.object_ref).toBe("runtime://cloud-run/run-1/exec/exec-1/stderr");
  });

  test("serves trial logs from the selected Trial resource subresource", async () => {
    const run = cloudRunRecord();
    const trial = runtimeResourceFixture({
      kind: "Trial",
      name: "trial-1",
      spec: {
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        attempt: 2,
      },
      status: {
        phase: "running",
      },
    });
    const logs = await RuntimeRepository.prototype.resourceLogs.call({
      async describeResource(_cloudRunId: string, _run: ReturnType<typeof cloudRunRecord>, input: { kind: string; name: string }) {
        expect(input).toMatchObject({ kind: "Trial", name: "trial-1" });
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceDescribe",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resource: trial,
          related_resources: [],
        };
      },
      async artifactContent(_cloudRunId: string, input: { trialId: string; role: string; coreRunId?: string | null; attempt?: number | undefined }) {
        expect(input).toEqual({
          trialId: "trial-1",
          role: "stderr",
          coreRunId: "core-run-1",
          attempt: 2,
        });
        return {
          bytes: new TextEncoder().encode("line 1\nline 2\nline 3\n"),
          media_type: "text/plain; charset=utf-8",
          object: {
            core_run_id: "core-run-1",
            trial_id: "trial-1",
            role: "stderr",
            object_ref: "artifact://sha256/trial-1-stderr",
          },
        };
      },
    } as any, "run-1", run, {
      kind: "Trial",
      name: "trial-1",
      stream: "stderr",
      tailLines: 2,
    });

    expect(logs.resource.kind).toBe("Trial");
    expect(logs.resource.metadata.name).toBe("trial-1");
    expect(logs.stream).toBe("stderr");
    expect(new TextDecoder().decode(logs.bytes)).toBe("line 2\nline 3\n");
    expect(logs.object).toMatchObject({
      core_run_id: "core-run-1",
      trial_id: "trial-1",
      role: "stderr",
      object_ref: "artifact://sha256/trial-1-stderr",
    });
  });

  test("serves artifact content from the selected TrialArtifact resource subresource", async () => {
    const run = cloudRunRecord();
    const artifact = runtimeResourceFixture({
      kind: "TrialArtifact",
      name: "trial-1.agent-result.sha256-aaaaaaaa",
      spec: {
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        schedule_idx: 3,
        attempt: 2,
        role: "agent_result",
        object_ref: "artifact://sha256/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      },
      status: {
        phase: "recorded",
        content_available: true,
      },
    });
    const content = await RuntimeRepository.prototype.resourceArtifactContent.call({
      async describeResource(_cloudRunId: string, _run: ReturnType<typeof cloudRunRecord>, input: { kind: string; name: string }) {
        expect(input).toMatchObject({ kind: "artifact", name: "trial-1.agent-result.sha256-aaaaaaaa" });
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceDescribe",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resource: artifact,
          related_resources: [],
        };
      },
      async artifactContent(_cloudRunId: string, input: { trialId: string; role: string; coreRunId?: string | null; attempt?: number | undefined; objectRef?: string | null }) {
        expect(input).toEqual({
          trialId: "trial-1",
          role: "agent_result",
          coreRunId: "core-run-1",
          attempt: 2,
          objectRef: "artifact://sha256/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        });
        return {
          bytes: new TextEncoder().encode("{\"ok\":true}"),
          media_type: "application/json; charset=utf-8",
          object: {
            core_run_id: "core-run-1",
            trial_id: "trial-1",
            schedule_idx: 3,
            attempt: 2,
            role: "agent_result",
            object_ref: "artifact://sha256/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            metadata: null,
            recorded_at_ms: 1,
            content_available: true,
            media_type: "application/json; charset=utf-8",
            byte_size: 11,
            sha256: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            relative_path: "agent/result.json",
          },
        };
      },
    } as any, "run-1", run, {
      kind: "artifact",
      name: "trial-1.agent-result.sha256-aaaaaaaa",
    });

    expect(content.resource.kind).toBe("TrialArtifact");
    expect(content.media_type).toBe("application/json; charset=utf-8");
    expect(new TextDecoder().decode(content.bytes)).toBe("{\"ok\":true}");
  });

  test("describes direct owner and dependent runtime resources", async () => {
    const runner = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      status: { phase: "online" },
    });
    const container = runtimeResourceFixture({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      ownerReferences: [{
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerInstance",
        name: "runner-instance-1",
        uid: "runner-instance-1",
      }],
      status: { phase: "running" },
    });
    const portForward = runtimeResourceFixture({
      kind: "PortForward",
      name: "pf-1",
      ownerReferences: [{
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "TrialContainer",
        name: "trial-1.agent.container-1",
        uid: "trial-1.agent.container-1",
      }],
      spec: { access_request_id: "pf-1" },
      status: { phase: "active" },
    });

    const described = await RuntimeRepository.prototype.describeResource.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: [],
          resources: [runner, container, portForward],
        };
      },
      async eventRows() {
        return [];
      },
    } as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
    });

    expect(described.related_resources.map((related) => ({
      relationship: related.relationship,
      kind: related.resource.kind,
      name: related.resource.metadata.name,
    }))).toEqual([
      { relationship: "owner", kind: "RunnerInstance", name: "runner-instance-1" },
      { relationship: "dependent", kind: "PortForward", name: "pf-1" },
    ]);
    expect(described.operations).toEqual(expect.arrayContaining([
      expect.objectContaining({
        purpose: "describe",
        command: "bucephalus-cloud run describe run-1 TrialContainer/trial-1.agent.container-1",
        supported: true,
        reason: null,
      }),
      expect.objectContaining({
        purpose: "port-forward",
        command: "bucephalus-cloud run port-forward run-1 TrialContainer/trial-1.agent.container-1 --target-port PORT",
        supported: false,
        reason: "runtime_access_target_unreachable",
      }),
    ]));
  });

  test("reviews runtime resource operation support from described operations", async () => {
    const runnerAttempt = runtimeResourceFixture({
      kind: "RunnerAttempt",
      name: "attempt-1",
      labels: {
        "bucephalus.dev/worker-id": "worker-1",
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        worker_id: "worker-1",
      },
      status: {
        phase: "running",
        runner_instance_status: "online",
      },
    });
    const scheduleSlot = runtimeResourceFixture({
      kind: "ScheduleSlot",
      name: "core-run-1.0",
      spec: {
        core_run_id: "core-run-1",
        schedule_idx: 0,
      },
      status: {
        phase: "active",
        trial_id: "trial-1",
        attempt: 0,
        worker_id: "worker-1",
        runner_binding: {
          worker_id: "worker-1",
          attempt_id: "attempt-1",
          runner_instance_id: "runner-instance-1",
          runner_instance_status: "online",
        },
      },
    });
    const container = runtimeResourceFixture({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      generation: 8,
      spec: {
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        schedule_idx: 0,
        attempt: 0,
      },
      status: {
        phase: "running",
        observedGeneration: 7,
        runner_binding: {
          worker_id: "worker-1",
          attempt_id: "attempt-1",
          runner_instance_id: "runner-instance-1",
          runner_instance_status: "online",
        },
        access: {
          reachable: true,
          port_forward: false,
          exec: true,
        },
      },
    });
    const repo = {
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [runnerAttempt, scheduleSlot, container],
        };
      },
    };

    const review = await RuntimeRepository.prototype.reviewResourceOperation.call(repo as any, "run-1", cloudRunRecord(), {
      kind: "container",
      name: "trial-1.agent.container-1",
      operation: "port-forward",
    });

    expect(review).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceOperationReview",
      cloud_run_id: "run-1",
      resource_ref: {
        kind: "TrialContainer",
        name: "trial-1.agent.container-1",
      },
      resource_version: expect.stringMatching(/^sha256:[0-9a-f]{64}$/),
      resource_generation: 8,
      observed_generation: 7,
      operation: "port-forward",
      matched_operation: "port-forward",
      supported: false,
      reason: "runtime_port_forward_unavailable",
      message: "port-forward requires an active runner attempt whose runner advertises runtime_port_forward",
      command: "bucephalus-cloud run port-forward run-1 TrialContainer/trial-1.agent.container-1 --target-port PORT",
      subresource: "port-forward",
      requires_active_run: true,
    });

    const waitReview = await RuntimeRepository.prototype.reviewResourceOperation.call(repo as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      operation: "wait",
    });

    expect(waitReview).toMatchObject({
      kind: "RuntimeResourceOperationReview",
      operation: "wait",
      resource_version: expect.stringMatching(/^sha256:[0-9a-f]{64}$/),
      resource_generation: 8,
      observed_generation: 7,
      matched_operation: "wait",
      supported: true,
      command: "bucephalus-cloud run wait run-1 TrialContainer/trial-1.agent.container-1 --for condition=Ready",
      verb: "get",
      subresource: "status",
      action: null,
      requires_active_run: false,
    });

    const watchNotReadyReview = await RuntimeRepository.prototype.reviewResourceOperation.call(repo as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      operation: "watch/not-ready",
    });

    expect(watchNotReadyReview).toMatchObject({
      kind: "RuntimeResourceOperationReview",
      operation: "watch/not-ready",
      resource_version: expect.stringMatching(/^sha256:[0-9a-f]{64}$/),
      resource_generation: 8,
      observed_generation: 7,
      matched_operation: "watch/not-ready",
      supported: true,
      command: "bucephalus-cloud run watch run-1 --kind TrialContainer --field-selector status.conditions.Ready!=True",
      verb: "watch",
      subresource: null,
      action: null,
      requires_active_run: false,
    });

    const unmatched = await RuntimeRepository.prototype.reviewResourceOperation.call(repo as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      operation: "attach",
    });

    expect(unmatched).toMatchObject({
      matched_operation: null,
      resource_version: expect.stringMatching(/^sha256:[0-9a-f]{64}$/),
      resource_generation: 8,
      observed_generation: 7,
      supported: false,
      reason: "operation_unavailable",
      message: "TrialContainer/trial-1.agent.container-1 does not currently advertise runtime operation attach",
      command: null,
      subresource: null,
      requires_active_run: null,
    });
  });

  test("reviews access operations through the same active binding precondition as creation", async () => {
    const container = runtimeResourceFixture({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      spec: {
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        schedule_idx: 0,
        attempt: 0,
      },
      status: {
        phase: "running",
        runner_binding: {
          worker_id: "worker-1",
          attempt_id: "attempt-1",
          runner_instance_id: "runner-instance-1",
          runner_instance_status: "online",
        },
        access: {
          reachable: true,
          port_forward: true,
          exec: true,
        },
      },
    });
    const repo = {
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [container],
        };
      },
    };

    const review = await RuntimeRepository.prototype.reviewResourceOperation.call(repo as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      operation: "exec",
    });

    expect(review).toMatchObject({
      matched_operation: "exec",
      supported: false,
      reason: "runtime_access_target_unreachable",
      message: "TrialContainer/trial-1.agent.container-1 is not currently reachable for runtime access: target is not bound to an active runner attempt",
      command: "bucephalus-cloud run exec run-1 TrialContainer/trial-1.agent.container-1 -- COMMAND [ARG...]",
      subresource: "exec",
      requires_active_run: true,
    });
  });

  test("gets raw runtime resource manifests without describe wrapper", async () => {
    const resource = runtimeResourceFixture({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      status: { phase: "running" },
    });

    const result = await RuntimeRepository.prototype.getResource.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [resource],
        };
      },
    } as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
    });

    expect(result).toEqual(resource);
    expect(result).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "TrialContainer",
      metadata: {
        name: "trial-1.agent.container-1",
      },
      status: {
        phase: "running",
      },
    });
  });

  test("lists resource-scoped runtime events with filters and pagination", async () => {
    const resource = runtimeResourceFixture({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
    });
    const observed: { input?: unknown } = {};
    const result = await RuntimeRepository.prototype.resourceEvents.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [resource],
        };
      },
      async eventRows(_cloudRunId: string, input: unknown) {
        observed.input = input;
        return [
          runtimeEvent({
            row_seq: 6,
            event_type: "runtime.access.port_forward.requested",
            payload: {
              target_ref: {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "TrialContainer",
                name: "trial-1.agent.container-1",
              },
            },
          }),
          runtimeEvent({
            row_seq: 7,
            event_type: "runtime.access.exec.requested",
            payload: {
              target_ref: {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "TrialContainer",
                name: "trial-2.agent.container-1",
              },
            },
          }),
        ];
      },
    } as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      limit: 2,
      afterRowSeq: 5,
      filter: {
        eventTypes: ["runtime.access.port_forward.requested"],
      },
    });

    expect(result).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceEventList",
      cloud_run_id: "run-1",
      core_run_ids: ["core-run-1"],
      generated_at: expect.any(String),
      resource: {
        kind: "TrialContainer",
        metadata: {
          name: "trial-1.agent.container-1",
        },
      },
      event_filter: {
        event_types: ["runtime.access.port_forward.requested"],
        sources: [],
        resource_kind: "TrialContainer",
        resource_name: "trial-1.agent.container-1",
        trial_id: null,
        task_id: null,
      },
      metadata: {
        resourceVersion: "event-row-seq:6",
        continue: null,
        remainingItemCount: 0,
        limit: 2,
        returned: 1,
        after_row_seq: 5,
        next_after_row_seq: 6,
      },
    });
    expect(result.events.map((event) => event.row_seq)).toEqual([6]);
    expect(observed.input).toMatchObject({
      limit: 250,
      afterRowSeq: 5,
      eventTypes: ["runtime.access.port_forward.requested"],
      resourceKind: "TrialContainer",
      resourceName: "trial-1.agent.container-1",
    });
  });

  test("scopes ordinary resource event scans to the selected resource identity", async () => {
    const resource = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-instance-1",
    });
    const observed: { input?: unknown } = {};
    const result = await RuntimeRepository.prototype.resourceEvents.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: [],
          resources: [resource],
        };
      },
      async eventRows(_cloudRunId: string, input: unknown) {
        observed.input = input;
        return [
          runtimeEvent({
            row_seq: 4,
            event_type: "runtime.resource.runner_instance.online",
            payload: {
              resource_ref: {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "RunnerInstance",
                name: "runner-instance-1",
              },
            },
          }),
        ];
      },
    } as any, "run-1", cloudRunRecord(), {
      kind: "RunnerInstance",
      name: "runner-instance-1",
      limit: 25,
    });

    expect(observed.input).toMatchObject({
      limit: 250,
      resourceKind: "RunnerInstance",
      resourceName: "runner-instance-1",
    });
    expect(result.metadata).toMatchObject({
      resourceVersion: "event-row-seq:4",
      returned: 1,
      next_after_row_seq: 4,
    });
    expect(result.events.map((event) => event.row_seq)).toEqual([4]);
  });

  test("builds run-scoped runtime event list metadata for follow clients", async () => {
    const observed: { input?: unknown } = {};
    const result = await RuntimeRepository.prototype.events.call({
      async eventRows(_cloudRunId: string, input: unknown) {
        observed.input = input;
        return [
          runtimeEvent({
            row_seq: 7,
            seq: 70,
            event_type: "runtime.access.port_forward.requested",
          }),
          runtimeEvent({
            row_seq: 9,
            seq: 90,
            event_type: "runtime.resource.runner_instance.drained",
          }),
        ];
      },
    } as any, "run-1", {
      limit: 25,
      afterRowSeq: 6,
      eventTypes: ["runtime.access.port_forward.requested", "runtime.resource.runner_instance.drained"],
      sources: ["cloud.run_events"],
      resourceKind: "RunnerInstance",
      resourceName: "runner-1",
      trialId: "trial-1",
      taskId: "task-1",
    });

    expect(observed.input).toMatchObject({
      limit: 26,
      afterRowSeq: 6,
      eventTypes: ["runtime.access.port_forward.requested", "runtime.resource.runner_instance.drained"],
    });
    expect(result).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeEventList",
      cloud_run_id: "run-1",
      generated_at: expect.any(String),
      event_filter: {
        event_types: ["runtime.access.port_forward.requested", "runtime.resource.runner_instance.drained"],
        sources: ["cloud.run_events"],
        resource_kind: "RunnerInstance",
        resource_name: "runner-1",
        trial_id: "trial-1",
        task_id: "task-1",
      },
      metadata: {
        resourceVersion: "event-row-seq:9",
        continue: null,
        remainingItemCount: 0,
        limit: 25,
        returned: 2,
        after_row_seq: 6,
        next_after_row_seq: 9,
      },
      events: [
        expect.objectContaining({ row_seq: 7 }),
        expect.objectContaining({ row_seq: 9 }),
      ],
    });
  });

  test("builds resource-scoped runtime event list cursors", async () => {
    const resource = runtimeResourceFixture({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
    });
    const result = await RuntimeRepository.prototype.resourceEvents.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [resource],
        };
      },
      async eventRows() {
        return [
          runtimeEvent({
            row_seq: 6,
            event_type: "runtime.access.port_forward.requested",
            payload: {
              target_ref: {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "TrialContainer",
                name: "trial-1.agent.container-1",
              },
            },
          }),
          runtimeEvent({
            row_seq: 8,
            event_type: "runtime.access.exec.requested",
            payload: {
              target_ref: {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "TrialContainer",
                name: "trial-1.agent.container-1",
              },
            },
          }),
        ];
      },
    } as any, "run-1", cloudRunRecord(), {
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      limit: 1,
      afterRowSeq: 5,
    });

    expect(result.metadata).toMatchObject({
      resourceVersion: "event-row-seq:6",
      continue: "event-row-seq:6",
      remainingItemCount: null,
      limit: 1,
      returned: 1,
      after_row_seq: 5,
      next_after_row_seq: 6,
    });
    expect(result.events.map((event) => event.row_seq)).toEqual([6]);
  });

  test("builds opaque runtime event list cursors when another page is known", async () => {
    const observed: { input?: { limit?: number } } = {};
    const result = await RuntimeRepository.prototype.events.call({
      async eventRows(_cloudRunId: string, input: { limit?: number }) {
        observed.input = input;
        return [
          runtimeEvent({
            row_seq: 7,
            seq: 70,
            event_type: "runtime.access.port_forward.requested",
          }),
          runtimeEvent({
            row_seq: 9,
            seq: 90,
            event_type: "runtime.resource.runner_instance.drained",
          }),
        ];
      },
    } as any, "run-1", {
      limit: 1,
      afterRowSeq: 6,
    });

    expect(observed.input).toMatchObject({ limit: 2 });
    expect(result.metadata).toMatchObject({
      resourceVersion: "event-row-seq:7",
      continue: "event-row-seq:7",
      remainingItemCount: null,
      limit: 1,
      returned: 1,
      after_row_seq: 6,
      next_after_row_seq: 7,
    });
    expect(result.events.map((event) => event.row_seq)).toEqual([7]);
  });

  test("reads compact runtime resource status", async () => {
    const resource = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      generation: 8,
      status: {
        phase: "cordoned",
        reason: "Cordoned",
        message: "Runner is cordoned",
        observedGeneration: 8,
        actions: ["uncordon", "drain"],
        conditions: [
          {
            type: "Ready",
            status: "True",
            reason: "Cordoned",
            message: "Runner remains reachable while cordoned",
          },
        ],
      },
      audit: {
        source: "cloud.runner_instances",
        updated_at: "2026-06-18T00:00:00Z",
      },
    });
    resource.metadata.resourceVersion = "sha256:runner-version";

    const result = await RuntimeRepository.prototype.resourceStatus.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [resource],
        };
      },
    } as any, "run-1", cloudRunRecord(), {
      kind: "runner",
      name: "runner-instance-1",
    });

    expect(result).toMatchObject({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceStatus",
      cloud_run_id: "run-1",
      core_run_ids: ["core-run-1"],
      resource_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerInstance",
        name: "runner-instance-1",
        uid: "runner-instance-1",
      },
      generation: 8,
      observedGeneration: 8,
      resourceVersion: "sha256:runner-version",
      phase: "cordoned",
      reason: "Cordoned",
      message: "Runner is cordoned",
      actions: ["uncordon", "drain"],
      conditions: [
        {
          type: "Ready",
          status: "True",
          reason: "Cordoned",
        },
      ],
      status: expect.objectContaining({
        phase: "cordoned",
      }),
      audit: {
        source: "cloud.runner_instances",
        updated_at: "2026-06-18T00:00:00Z",
      },
    });
  });

  test("cordons runner instance resources with an audited runtime event", async () => {
    const resource = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      labels: {
        "bucephalus.dev/run-id": "run-1",
        "bucephalus.dev/runner-instance-id": "runner-instance-1",
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        attempt_ids: ["attempt-1"],
      },
      status: {
        phase: "online",
        current_attempt_id: "attempt-1",
        actions: ["cordon", "drain"],
      },
      audit: {
        source: "cloud.runner_instances",
      },
    });
    resource.metadata.resourceVersion = "sha256:runner-online";
    const observed: {
      jsonPayloads: JsonObject[];
      updateValues?: unknown[];
      insertValues?: unknown[];
      describe?: { kind: string; name: string; eventLimit?: number | undefined };
    } = {
      jsonPayloads: [],
    };
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runner_instances")) {
        observed.updateValues = values;
        return [{ runner_instance_id: "runner-instance-1", status: "cordoned" }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.insertValues = values;
        return [];
      }
      return [];
    }) as any;
    const sql = {
      begin: async (callback: (tx: any) => Promise<void>) => await callback(tx),
      json: (payload: JsonObject) => {
        observed.jsonPayloads.push(payload);
        return payload;
      },
    };
    const describe = {
      apiVersion: "bucephalus.dev/v1alpha1" as const,
      kind: "RuntimeResourceDescribe" as const,
      cloud_run_id: "run-1",
      core_run_ids: [],
      resource: {
        ...resource,
        status: {
          ...resource.status,
          phase: "cordoned",
        },
      },
      related_resources: [],
    };

    const result = await RuntimeRepository.prototype.cordonRunnerInstanceResource.call({
      sql,
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: [],
          resources: [resource],
        };
      },
      async describeResource(_runId: string, _run: ReturnType<typeof cloudRunRecord>, input: { kind: string; name: string; eventLimit?: number | undefined }) {
        observed.describe = input;
        return describe;
      },
    } as any, {
      run: cloudRunRecord(),
      resourceKind: "RunnerInstance",
      resourceName: "runner-instance-1",
      resourceVersion: "sha256:runner-online",
      requester: "issuer:user-a",
      reason: "pause scheduling",
    });

    expect(result.resource.status.phase).toBe("cordoned");
    expect(observed.updateValues).toContain("runner-instance-1");
    expect(observed.insertValues).toContain("runtime.resource.runner_instance.cordoned");
    expect(observed.jsonPayloads[0]).toMatchObject({
      last_runtime_action: {
        action: "cordon",
        requested_by: "issuer:user-a",
        reason: "pause scheduling",
      },
    });
    expect(observed.jsonPayloads[1]).toMatchObject({
      action: "cordon",
      resource_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerInstance",
        name: "runner-instance-1",
        uid: "runner-instance-1",
      },
      resource_kind: "RunnerInstance",
      resource_name: "runner-instance-1",
      runner_instance_id: "runner-instance-1",
      attempt_id: "attempt-1",
      previous_status: "online",
      status: "cordoned",
      requester: "issuer:user-a",
      reason: "pause scheduling",
      resource_version_precondition: "sha256:runner-online",
    });
    expect(observed.describe).toEqual({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      eventLimit: 100,
    });
  });

  test("drains runner instance resources with an audited runtime event", async () => {
    const resource = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      labels: {
        "bucephalus.dev/run-id": "run-1",
        "bucephalus.dev/runner-instance-id": "runner-instance-1",
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        attempt_ids: ["attempt-1"],
      },
      status: {
        phase: "online",
        current_attempt_id: "attempt-1",
        actions: ["cordon", "drain"],
      },
      audit: {
        source: "cloud.runner_instances",
      },
    });
    resource.metadata.resourceVersion = "sha256:runner-online";
    const observed: {
      jsonPayloads: JsonObject[];
      updateValues?: unknown[];
      insertValues?: unknown[];
      describe?: { kind: string; name: string; eventLimit?: number | undefined };
    } = {
      jsonPayloads: [],
    };
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runner_instances")) {
        observed.updateValues = values;
        return [{ runner_instance_id: "runner-instance-1", status: "draining" }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.insertValues = values;
        return [];
      }
      return [];
    }) as any;
    const sql = {
      begin: async (callback: (tx: any) => Promise<void>) => await callback(tx),
      json: (payload: JsonObject) => {
        observed.jsonPayloads.push(payload);
        return payload;
      },
    };
    const describe = {
      apiVersion: "bucephalus.dev/v1alpha1" as const,
      kind: "RuntimeResourceDescribe" as const,
      cloud_run_id: "run-1",
      core_run_ids: [],
      resource: {
        ...resource,
        status: {
          ...resource.status,
          phase: "draining",
        },
      },
      related_resources: [],
    };

    const result = await RuntimeRepository.prototype.drainRunnerInstanceResource.call({
      sql,
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: [],
          resources: [resource],
        };
      },
      async describeResource(_runId: string, _run: ReturnType<typeof cloudRunRecord>, input: { kind: string; name: string; eventLimit?: number | undefined }) {
        observed.describe = input;
        return describe;
      },
    } as any, {
      run: cloudRunRecord(),
      resourceKind: "RunnerInstance",
      resourceName: "runner-instance-1",
      resourceVersion: "sha256:runner-online",
      requester: "issuer:user-a",
      reason: "maintenance",
    });

    expect(result.resource.status.phase).toBe("draining");
    expect(observed.updateValues).toContain("runner-instance-1");
    expect(observed.insertValues).toContain("runtime.resource.runner_instance.drained");
    expect(observed.jsonPayloads[0]).toMatchObject({
      last_runtime_action: {
        action: "drain",
        requested_by: "issuer:user-a",
        reason: "maintenance",
      },
    });
    expect(observed.jsonPayloads[1]).toMatchObject({
      action: "drain",
      resource_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerInstance",
        name: "runner-instance-1",
        uid: "runner-instance-1",
      },
      resource_kind: "RunnerInstance",
      resource_name: "runner-instance-1",
      runner_instance_id: "runner-instance-1",
      attempt_id: "attempt-1",
      previous_status: "online",
      status: "draining",
      requester: "issuer:user-a",
      reason: "maintenance",
      resource_version_precondition: "sha256:runner-online",
    });
    expect(observed.describe).toEqual({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      eventLimit: 100,
    });
  });

  test("uncordons draining runner instance resources with an audited runtime event", async () => {
    const resource = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      labels: {
        "bucephalus.dev/run-id": "run-1",
        "bucephalus.dev/runner-instance-id": "runner-instance-1",
      },
      spec: {
        runner_instance_id: "runner-instance-1",
        attempt_ids: ["attempt-1"],
      },
      status: {
        phase: "draining",
        current_attempt_id: "attempt-1",
        actions: ["uncordon"],
      },
      audit: {
        source: "cloud.runner_instances",
      },
    });
    resource.metadata.resourceVersion = "sha256:runner-draining";
    const observed: {
      jsonPayloads: JsonObject[];
      updateValues?: unknown[];
      insertValues?: unknown[];
      describe?: { kind: string; name: string; eventLimit?: number | undefined };
    } = {
      jsonPayloads: [],
    };
    const tx = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      const query = strings.raw.join(" ");
      if (query.includes("update cloud.runner_instances")) {
        observed.updateValues = values;
        return [{ runner_instance_id: "runner-instance-1", status: "online" }];
      }
      if (query.includes("insert into cloud.run_events")) {
        observed.insertValues = values;
        return [];
      }
      return [];
    }) as any;
    const sql = {
      begin: async (callback: (tx: any) => Promise<void>) => await callback(tx),
      json: (payload: JsonObject) => {
        observed.jsonPayloads.push(payload);
        return payload;
      },
    };
    const describe = {
      apiVersion: "bucephalus.dev/v1alpha1" as const,
      kind: "RuntimeResourceDescribe" as const,
      cloud_run_id: "run-1",
      core_run_ids: [],
      resource: {
        ...resource,
        status: {
          ...resource.status,
          phase: "online",
        },
      },
      related_resources: [],
    };

    const result = await RuntimeRepository.prototype.uncordonRunnerInstanceResource.call({
      sql,
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: [],
          resources: [resource],
        };
      },
      async describeResource(_runId: string, _run: ReturnType<typeof cloudRunRecord>, input: { kind: string; name: string; eventLimit?: number | undefined }) {
        observed.describe = input;
        return describe;
      },
    } as any, {
      run: cloudRunRecord(),
      resourceKind: "RunnerInstance",
      resourceName: "runner-instance-1",
      resourceVersion: "sha256:runner-draining",
      requester: "issuer:user-a",
      reason: "resume scheduling",
    });

    expect(result.resource.status.phase).toBe("online");
    expect(observed.updateValues).toContain("runner-instance-1");
    expect(observed.insertValues).toContain("runtime.resource.runner_instance.uncordoned");
    expect(observed.jsonPayloads[0]).toMatchObject({
      last_runtime_action: {
        action: "uncordon",
        requested_by: "issuer:user-a",
        reason: "resume scheduling",
      },
    });
    expect(observed.jsonPayloads[1]).toMatchObject({
      action: "uncordon",
      resource_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerInstance",
        name: "runner-instance-1",
        uid: "runner-instance-1",
      },
      resource_kind: "RunnerInstance",
      resource_name: "runner-instance-1",
      runner_instance_id: "runner-instance-1",
      attempt_id: "attempt-1",
      previous_status: "draining",
      status: "online",
      requester: "issuer:user-a",
      reason: "resume scheduling",
      resource_version_precondition: "sha256:runner-draining",
    });
    expect(observed.describe).toEqual({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      eventLimit: 100,
    });
  });

  test("cancels runtime access resources through the resource action plane", async () => {
    const resource = runtimeResourceFixture({
      kind: "PortForward",
      name: "pf-1",
      labels: {
        "bucephalus.dev/run-id": "run-1",
      },
      spec: {
        access_request_id: "pf-1",
      },
      status: {
        phase: "active",
        actions: ["cancel"],
      },
      audit: {
        source: "cloud.runtime_access_requests",
      },
    });
    resource.metadata.resourceVersion = "sha256:pf-1";
    const observed: {
      cancel?: {
        cloudRunId: string;
        accessRequestId: string;
        requester?: string | null | undefined;
        reason?: string | null | undefined;
        resourceVersion?: string | null | undefined;
      };
      describe?: { kind: string; name: string; eventLimit?: number | undefined };
    } = {};
    const describe = {
      apiVersion: "bucephalus.dev/v1alpha1" as const,
      kind: "RuntimeResourceDescribe" as const,
      cloud_run_id: "run-1",
      core_run_ids: [],
      resource: {
        ...resource,
        status: {
          ...resource.status,
          phase: "cancelled",
        },
      },
      related_resources: [],
    };

    const result = await RuntimeRepository.prototype.cancelRuntimeAccessResource.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: [],
          resources: [resource],
        };
      },
      async cancelPortForwardRequest(input: {
        cloudRunId: string;
        accessRequestId: string;
        requester?: string | null | undefined;
        reason?: string | null | undefined;
        resourceVersion?: string | null | undefined;
      }) {
        observed.cancel = input;
        return accessRequestRecord({
          kind: "port_forward",
          status: "cancelled",
        });
      },
      async describeResource(_runId: string, _run: ReturnType<typeof cloudRunRecord>, input: { kind: string; name: string; eventLimit?: number | undefined }) {
        observed.describe = input;
        return describe;
      },
    } as any, {
      run: cloudRunRecord(),
      resourceKind: "PortForward",
      resourceName: "pf-1",
      resourceVersion: "sha256:pf-1",
      requester: "issuer:user-a",
      reason: "cleanup tunnel",
    });

    expect(result.resource.status.phase).toBe("cancelled");
    expect(observed.cancel).toEqual({
      cloudRunId: "run-1",
      accessRequestId: "pf-1",
      requester: "issuer:user-a",
      reason: "cleanup tunnel",
      resourceVersion: "sha256:pf-1",
    });
    expect(observed.describe).toEqual({
      kind: "PortForward",
      name: "pf-1",
      eventLimit: 100,
    });
  });

  test("rejects runtime resource actions that are not advertised by the selected resource", async () => {
    const runner = runtimeResourceFixture({
      kind: "RunnerInstance",
      name: "runner-instance-1",
      spec: {
        runner_instance_id: "runner-instance-1",
      },
      status: {
        phase: "online",
      },
    });
    runner.metadata.resourceVersion = "sha256:current-runner";
    const portForward = runtimeResourceFixture({
      kind: "PortForward",
      name: "pf-1",
      spec: {
        access_request_id: "pf-1",
      },
      status: {
        phase: "active",
      },
    });
    const run = cloudRunRecord();

    await expect(RuntimeRepository.prototype.cordonRunnerInstanceResource.call({
      sql: {
        begin: async () => {
          throw new Error("cordon should not mutate with a stale resourceVersion");
        },
      },
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: [],
          resources: [runner],
        };
      },
    } as any, {
      run,
      resourceKind: "RunnerInstance",
      resourceName: "runner-instance-1",
      resourceVersion: "sha256:stale-runner",
      requester: "issuer:user-a",
      reason: "stale client",
    })).rejects.toMatchObject({
      status: 409,
      code: "runtime_resource_version_conflict",
      detail: {
        resource_kind: "RunnerInstance",
        resource_name: "runner-instance-1",
        expected_resource_version: "sha256:stale-runner",
        resource_version: "sha256:current-runner",
      },
    });

    await expect(RuntimeRepository.prototype.cordonRunnerInstanceResource.call({
      sql: {
        begin: async () => {
          throw new Error("cordon should not mutate without status.actions");
        },
      },
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: [],
          resources: [runner],
        };
      },
    } as any, {
      run,
      resourceKind: "RunnerInstance",
      resourceName: "runner-instance-1",
      requester: "issuer:user-a",
      reason: "stale client",
    })).rejects.toMatchObject({
      status: 409,
      code: "runtime_resource_action_unavailable",
      detail: {
        action: "cordon",
        resource_kind: "RunnerInstance",
        resource_name: "runner-instance-1",
        available_actions: [],
      },
    });

    await expect(RuntimeRepository.prototype.cancelRuntimeAccessResource.call({
      async resources() {
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          cloud_run_id: "run-1",
          core_run_ids: [],
          resources: [portForward],
        };
      },
      async cancelPortForwardRequest() {
        throw new Error("cancel should not mutate without status.actions");
      },
    } as any, {
      run,
      resourceKind: "PortForward",
      resourceName: "pf-1",
      requester: "issuer:user-a",
      reason: "stale client",
    })).rejects.toMatchObject({
      status: 409,
      code: "runtime_resource_action_unavailable",
      detail: {
        action: "cancel",
        resource_kind: "PortForward",
        resource_name: "pf-1",
        available_actions: [],
      },
    });
  });

  test("filters normalized runtime events by event metadata and involved objects", async () => {
    const events = await RuntimeRepository.prototype.eventRows.call({
      async coreRunIdsForCloudRun() {
        return [];
      },
      async workerRuntimeSnapshots() {
        return [];
      },
      async cloudControlPlaneEventRows() {
        return [
          runtimeEvent({
            source: "cloud.run_events",
            row_seq: 1,
            event_type: "runtime.access.port_forward.requested",
            payload: {
              access_resource_ref: {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "PortForward",
                name: "pf-1",
                uid: "pf-1",
              },
              target_ref: {
                apiVersion: "bucephalus.dev/v1alpha1",
                kind: "TrialContainer",
                name: "trial-1.agent.container-1",
              },
              trial_id: "trial-1",
              task_id: "task-1",
            },
          }),
          runtimeEvent({
            source: "cloud.run_events",
            row_seq: 2,
            event_type: "runtime.access.exec.requested",
            payload: {
              resource_kind: "Exec",
              resource_name: "exec-1",
              trial_id: "trial-2",
              task_id: "task-2",
            },
          }),
          runtimeEvent({
            source: "runtime.event_rows",
            row_seq: 3,
            event_type: "trial.observed",
            trial_id: "trial-1",
            task_id: "task-1",
          }),
        ];
      },
    }, "run-1", {
      eventTypes: ["runtime.access.port_forward.requested", "trial.observed"],
      sources: ["cloud.run_events"],
      resourceKind: "PortForward",
      resourceName: "PF-1",
      trialId: "TRIAL-1",
      taskId: "TASK-1",
      limit: 50,
    });

    expect(events.map((event) => event.row_seq)).toEqual([1]);
    expect(events[0]?.resource_refs).toEqual(expect.arrayContaining([
      {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "PortForward",
        name: "pf-1",
        uid: "pf-1",
      },
      {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "TrialContainer",
        name: "trial-1.agent.container-1",
      },
    ]));
  });

  test("pushes cloud control-plane event type filters into the SQL read", async () => {
    const observed: { query: string; values: unknown[] } = {
      query: "",
      values: [],
    };
    const sql = ((strings: TemplateStringsArray, ...values: unknown[]) => {
      observed.query = strings.raw.join(" ");
      observed.values = values;
      return Promise.resolve([
        {
          event_id: "event-1",
          run_id: "run-1",
          attempt_id: "attempt-1",
          seq: 12,
          event_type: "worker.core.completed",
          payload: {
            stdout_tail: "ok\n",
          },
          created_at: "2026-06-04T00:00:12Z",
        },
      ]);
    }) as any;

    const rows = await (RuntimeRepository.prototype as any).cloudControlPlaneEventRows.call({ sql }, "run-1", {
      afterRowSeq: 7,
      eventTypes: ["worker.core.completed", "worker.core.completed", " "],
      sources: ["cloud.run_events"],
    });

    expect(observed.query).toContain("event_type = any(");
    expect(observed.values).toEqual(["run-1", 7, false, ["worker.core.completed"]]);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      source: "cloud.run_events",
      row_seq: 12,
      event_type: "worker.core.completed",
      row: {
        attempt_id: "attempt-1",
      },
      payload: {
        stdout_tail: "ok\n",
      },
    });
  });

  test("skips cloud control-plane event reads when the source filter excludes them", async () => {
    let queried = false;
    const sql = (() => {
      queried = true;
      throw new Error("cloud.run_events should not be queried");
    }) as any;

    const rows = await (RuntimeRepository.prototype as any).cloudControlPlaneEventRows.call({ sql }, "run-1", {
      eventTypes: ["worker.core.completed"],
      sources: ["runtime.event_rows"],
    });

    expect(rows).toEqual([]);
    expect(queried).toBe(false);
  });

  test("derives log targets from trial runtime resources", () => {
    expect(runtimeLogTargetForResource({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "Trial",
      metadata: {
        name: "trial-1",
        labels: {},
        annotations: {},
        ownerReferences: [],
      },
      spec: {
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        attempt: 2,
      },
      status: {
        phase: "running",
      },
      audit: {},
    })).toEqual({
      coreRunId: "core-run-1",
      trialId: "trial-1",
      attempt: 2,
    });

    expect(runtimeLogTargetForResource({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "TrialContainer",
      metadata: {
        name: "trial-1.agent.container-1",
        labels: {},
        annotations: {},
        ownerReferences: [],
      },
      spec: {
        core_run_id: "core-run-1",
        trial_id: "trial-1",
        attempt: 2,
      },
      status: {
        phase: "running",
      },
      audit: {},
    })).toEqual({
      coreRunId: "core-run-1",
      trialId: "trial-1",
      attempt: 2,
    });

    expect(runtimeLogTargetForResource({
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "ScheduleSlot",
      metadata: {
        name: "core-run-1.4",
        labels: {},
        annotations: {},
        ownerReferences: [],
      },
      spec: {
        core_run_id: "core-run-1",
      },
      status: {
        phase: "assigned",
        trial_id: "trial-2",
        attempt: 0,
      },
      audit: {},
    })).toEqual({
      coreRunId: "core-run-1",
      trialId: "trial-2",
      attempt: 0,
    });
  });

  test("normalizes worker runtime snapshot event payloads", () => {
    const snapshot = runtimeSnapshotFromWorkerEventPayload({
      core_run_id: "run_20260529_000001_000001_000001",
      runtime_values: {
        run_control_v2: { status: "completed" },
        schedule_progress_v2: { committed: 1 },
        ignored_scalar: "nope",
      },
      trial_summaries: [
        { trial_id: "trial-1", summary: { outcome: "success" }, contract_trace: { stages: {} } },
        { trial_id: "", summary: { outcome: "ignored" } },
      ],
      evidence_records: [
        {
          ids: { trial_id: "trial-1" },
          schedule_idx: 0,
          attempt: 0,
          evidence: {
            trial_output_ref: "artifact://sha256/output",
          },
        },
      ],
      omitted: ["runtime/run_session_state.json", 12],
    });

    expect(snapshot).toMatchObject({
      core_run_id: "run_20260529_000001_000001_000001",
      run_dir_name: "run_20260529_000001_000001_000001",
      runtime_values: {
        run_control_v2: { status: "completed" },
        schedule_progress_v2: { committed: 1 },
      },
      trial_summaries: [
        { trial_id: "trial-1", summary: { outcome: "success" }, contract_trace: { stages: {} } },
      ],
      omitted: ["runtime/run_session_state.json"],
    });
    expect(runtimeValueRecordsFromSnapshots(snapshot ? [{ ...snapshot, seq: 3, created_at: "2026-06-04T00:00:00Z" }] : [])).toEqual(expect.arrayContaining([
      expect.objectContaining({
        core_run_id: "run_20260529_000001_000001_000001",
        key: "run_control_v2",
        value: { status: "completed" },
        source: "worker_runtime_snapshot",
        snapshot_seq: 3,
        observed_at: "2026-06-04T00:00:00Z",
      }),
      expect.objectContaining({
        key: "schedule_progress_v2",
        value: { committed: 1 },
      }),
    ]));
  });

  test("ignores events that do not declare a Core run id", () => {
    expect(runtimeSnapshotFromWorkerEventPayload({
      runtime_values: {
        run_control_v2: { status: "completed" },
      },
    })).toBeNull();
  });

  test("derives results rows from Core trial summaries in worker snapshots", () => {
    const snapshot = runtimeSnapshotFromWorkerEventPayload({
      core_run_id: "run_20260529_000001_000001_000001",
      trial_summaries: [
        {
          trial_id: "trial-1",
          summary: {
            schema_version: "trial_summary_v1",
            ids: {
              run_id: "run_20260529_000001_000001_000001",
              trial_id: "trial-1",
              variant_id: "variant-a",
              task_id: "task-a",
              repl_idx: 2,
            },
            outcome: "success",
            primary_metric: {
              name: "score",
              value: 0.91,
            },
            metrics: {
              score: 0.91,
              latency_ms: 124,
            },
          },
          contract_trace: {
            ids: {
              trial_id: "trial-1",
              variant_id: "variant-a",
              task_id: "task-a",
              repl_idx: 2,
              schedule_idx: 4,
            },
            overall_status: "ok",
            score_trust: "trusted",
            score: {
              metric: "score",
              value: 0.91,
            },
            stages: {
              agent_execution: { status: "ok", exit_status: "0" },
              grade_mapping: { status: "ok" },
            },
          },
          trial_events: [
            { event_type: "step", ts: "2026-05-29T00:00:00Z", message: "started" },
            { type: "finish", timestamp: "2026-05-29T00:00:01Z" },
          ],
        },
      ],
      evidence_records: [
        {
          ids: {
            trial_id: "trial-1",
          },
          schedule_idx: 4,
          attempt: 0,
          recorded_at_ms: 12345,
          evidence: {
            trial_output_ref: "artifact://sha256/output",
            stdout_ref: "artifact://sha256/stdout",
            workspace_bundle_ref: "artifact://sha256/workspace",
          },
        },
      ],
    });
    if (!snapshot) {
      throw new Error("snapshot did not normalize");
    }

    const rows = runtimeTrialResultsFromSnapshots([snapshot]);
    expect(rows).toMatchObject([
      {
        core_run_id: "run_20260529_000001_000001_000001",
        trial_id: "trial-1",
        schedule_idx: 4,
        attempt: 0,
        row_seq: 0,
        variant_id: "variant-a",
        task_id: "task-a",
        repl_idx: 2,
        outcome: "success",
        primary_metric_name: "score",
        primary_metric_value: 0.91,
        metrics: {
          score: 0.91,
          latency_ms: 124,
        },
        bindings: {},
        events_total: 0,
        has_events: false,
        row: {
          source: "worker_runtime_snapshot",
        },
      },
    ]);

    expect(runtimeMetricObservationsFromTrialResults(rows)).toMatchObject([
      {
        metric_name: "score",
        metric_value: 0.91,
        metric_source: "worker_runtime_snapshot",
      },
      {
        metric_name: "latency_ms",
        metric_value: 124,
        metric_source: "worker_runtime_snapshot",
      },
    ]);
    expect(runtimeContractStagesFromSnapshots([snapshot])).toMatchObject([
      {
        core_run_id: "run_20260529_000001_000001_000001",
        trial_id: "trial-1",
        schedule_idx: 4,
        variant_id: "variant-a",
        task_id: "task-a",
        repl_idx: 2,
        stage: "agent_execution",
        status: "ok",
        detail: {
          status: "ok",
          exit_status: "0",
        },
        row: {
          source: "worker_runtime_snapshot",
        },
      },
      {
        stage: "grade_mapping",
        status: "ok",
        detail: {
          overall_status: "ok",
          score_trust: "trusted",
          score: {
            metric: "score",
            value: 0.91,
          },
        },
      },
    ]);
    expect(runtimeEventRowsFromSnapshots([snapshot])).toMatchObject([
      {
        core_run_id: "run_20260529_000001_000001_000001",
        trial_id: "trial-1",
        schedule_idx: 4,
        variant_id: "variant-a",
        task_id: "task-a",
        repl_idx: 2,
        row_seq: 0,
        seq: 0,
        event_type: "step",
        ts: "2026-05-29T00:00:00Z",
        payload: {
          message: "started",
        },
        row: {
          source: "worker_runtime_snapshot",
        },
      },
      {
        row_seq: 1,
        seq: 1,
        event_type: "finish",
        ts: "2026-05-29T00:00:01Z",
      },
    ]);
    expect(runtimeAttemptObjectsFromSnapshots([snapshot])).toMatchObject([
      {
        core_run_id: "run_20260529_000001_000001_000001",
        trial_id: "trial-1",
        schedule_idx: 4,
        attempt: 0,
        role: "trial_output",
        object_ref: "artifact://sha256/output",
        recorded_at_ms: 12345,
      },
      {
        role: "stdout",
        object_ref: "artifact://sha256/stdout",
      },
      {
        role: "workspace_bundle",
        object_ref: "artifact://sha256/workspace",
      },
    ]);
  });

  test("uses contract trace identity when summary ids are incomplete", () => {
    const snapshot = runtimeSnapshotFromWorkerEventPayload({
      core_run_id: "run_20260529_000001_000001_000001",
      trial_summaries: [
        {
          trial_id: "fallback-trial",
          summary: {
            outcome: { status: "failed" },
            primary_metric: { name: "score", value: Number.NaN },
            metrics: { score: 0.1 },
          },
          contract_trace: {
            ids: {
              trial_id: "trial-from-contract",
              schedule_idx: 7,
              attempt: 2,
              variant_id: "variant-from-contract",
              task_id: "task-from-contract",
              repl_idx: 3,
            },
            stages: {},
          },
        },
      ],
    });
    if (!snapshot) {
      throw new Error("snapshot did not normalize");
    }

    expect(runtimeTrialResultsFromSnapshots([snapshot])).toMatchObject([
      {
        trial_id: "trial-from-contract",
        schedule_idx: 7,
        attempt: 2,
        variant_id: "variant-from-contract",
        task_id: "task-from-contract",
        repl_idx: 3,
        outcome: "failed",
        primary_metric_value: null,
      },
    ]);
  });
});

function runtimeEvent(overrides: Partial<RuntimeEventRecord> = {}): RuntimeEventRecord {
  return {
    source: "cloud.run_events",
    core_run_id: "",
    trial_id: "",
    schedule_idx: -1,
    attempt: 0,
    row_seq: 0,
    slot_commit_id: "",
    variant_id: "",
    task_id: "",
    repl_idx: 0,
    seq: 0,
    event_type: "runtime.event",
    ts: "2026-06-04T00:00:00Z",
    resource_refs: [],
    payload: {},
    row: {
      source: "cloud.run_events",
    },
    ...overrides,
  };
}

function runtimeResourceFixture(overrides: {
  kind: string;
  name: string;
  generation?: number;
  labels?: Record<string, string>;
  annotations?: Record<string, string>;
  ownerReferences?: RuntimeResourceRecord["metadata"]["ownerReferences"];
  spec?: JsonObject;
  status?: JsonObject;
  audit?: JsonObject;
}): RuntimeResourceRecord {
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: overrides.kind,
    metadata: {
      name: overrides.name,
      uid: overrides.name,
      ...(overrides.generation ? { generation: overrides.generation } : {}),
      labels: overrides.labels ?? {},
      annotations: overrides.annotations ?? {},
      ownerReferences: overrides.ownerReferences ?? [],
    },
    spec: overrides.spec ?? {},
    status: overrides.status ?? {},
    audit: overrides.audit ?? {},
  };
}

function runtimeResourceListMetadataFixture() {
  return {
    resourceVersion: "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    continue: null,
    remainingItemCount: 0 as const,
    total: 0,
    returned: 0,
  };
}

function runtimeCondition(type: string, status: "True" | "False" | "Unknown", reason: string, message: string): JsonObject {
  return { type, status, reason, message };
}

function runtimeConditionsByType(resource: RuntimeResourceRecord): Record<string, JsonObject> {
  const conditions = Array.isArray(resource.status.conditions)
    ? resource.status.conditions.filter((condition): condition is JsonObject => isJsonObject(condition))
    : [];
  return Object.fromEntries(conditions
    .filter((condition) => typeof condition.type === "string")
    .map((condition) => [String(condition.type), condition]));
}

function isJsonObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function cloudRunRecord() {
  return {
    run_id: "run-1",
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    run_label: "debug",
    status: "running",
    env: {},
    secret_refs: {},
    package_provenance: {},
    runtime_options: {},
    run_requirements: {
      executor: "runner-docker" as const,
      requires: [],
      image_refs: [],
      secret_ids: [],
      network_perimeter: {
        default: "none" as const,
        task_sandbox: "none" as const,
        agent: "none" as const,
        egress_hosts: [],
      },
      sidecars: [],
      accelerators: [],
      arch: "x86_64" as const,
      cpu_count: 4,
      memory_mb: 8192,
      disk_mb: 32768,
      isolation: "reusable_vm" as const,
      timeout_ms: null,
      max_parallel_trials: 4,
    },
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:01:00Z",
    started_at: "2026-06-04T00:00:30Z",
    completed_at: null,
    error_message: null,
  };
}

function accessRequestRecord(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    access_request_id: "pf-1",
    run_id: "run-1",
    kind: "port_forward",
    status: "requested",
    resource_kind: "TrialContainer",
    resource_name: "trial-1.agent.container-1",
    target_uid: "trial-1.agent.container-1",
    target_resource_version: "sha256:trial-container",
    protocol: "tcp",
    target_port: 8080,
    local_port: null,
    command: [],
    runner_instance_id: "runner-instance-1",
    attempt_id: "attempt-1",
    worker_id: "worker-1",
    requester: "issuer:user-a",
    reason: "debug",
    connection: {},
    error_message: null,
    expires_at: null,
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
    ...overrides,
  };
}

function runnerPoolRecord(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    runner_pool_id: "runner-pool-1",
    name: "on-demand-docker",
    status: "active",
    capabilities: {
      executors: ["runner-docker"],
      resources: ["core_runner", "runtime_exec"],
      arch: "x86_64",
      cpu_count: 4,
      memory_mb: 8192,
      disk_mb: 32768,
      isolation: ["reusable_vm"],
    },
    metadata: {
      provider: "gcp",
    },
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:01:00Z",
    ...overrides,
  };
}

function provisionRequestRecord(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    provision_request_id: "provision-request-1",
    runner_pool_id: "runner-pool-1",
    run_id: "run-1",
    status: "provisioning",
    provider: "gcp",
    provider_instance_id: null,
    instance_name: "runner-2",
    runner_instance_id: null,
    requirements: {
      executor: "runner-docker",
      resources: ["core_runner"],
    },
    metadata: {},
    error_message: null,
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:30Z",
    ...overrides,
  };
}

function scheduleSlotRecord(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    core_run_id: "core-run-1",
    schedule_idx: 0,
    state: "active",
    trial_id: "trial-1",
    attempt: 0,
    worker_id: "worker-1",
    owner_id: "owner-1",
    lease_expires_at: "2026-06-04T00:02:00Z",
    slot_commit_id: "slot-commit-1",
    slot_status: "active",
    slot: {
      idx: 0,
    },
    ...overrides,
  };
}

function trialAttemptRecord(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    core_run_id: "core-run-1",
    trial_id: "trial-1",
    schedule_idx: 0,
    attempt: 0,
    phase: "running",
    paused_from_phase: null,
    variant_id: "variant-a",
    task_id: "task-a",
    repl_idx: 0,
    state: {
      phase: "running",
    },
    updated_at_ms: 1_801_958_400_000,
    ...overrides,
  };
}

function trialContainerRecord(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    core_run_id: "core-run-1",
    trial_id: "trial-1",
    schedule_idx: 0,
    attempt: 0,
    role: "agent",
    container_id: "container-1",
    status: "running",
    image: "ghcr.io/acme/agent@sha256:fixture",
    workdir: "/workspace/task",
    updated_at_ms: 1_801_958_400_000,
    ...overrides,
  };
}

function attemptObjectRecord(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    core_run_id: "core-run-1",
    trial_id: "trial-1",
    schedule_idx: 0,
    attempt: 0,
    role: "agent_result",
    object_ref: "artifact://sha256/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    metadata: {
      media_type: "application/json; charset=utf-8",
    },
    recorded_at_ms: 1_801_958_400_000,
    content_available: true,
    media_type: "application/json; charset=utf-8",
    byte_size: null,
    sha256: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    relative_path: "",
    ...overrides,
  };
}

function attemptRecord(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    attempt_id: "attempt-1",
    run_id: "run-1",
    worker_id: "worker-1",
    runner_instance_id: "runner-instance-1",
    status: "running",
    lease_expires_at: "2026-06-04T00:01:00Z",
    heartbeat_at: "2026-06-04T00:00:00Z",
    started_at: "2026-06-04T00:00:00Z",
    ended_at: null,
    error_message: null,
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
    attempt_token: null,
    runner_pool_id: "runner-pool-1",
    runner_instance_status: "online",
    runner_instance_name: "runner-1",
    runner_instance_provider_instance_id: "gce://projects/proj-1/zones/us-central1-a/instances/buc-runner-1",
    runner_instance_capabilities: {
      executors: ["runner-docker"],
      resources: ["core_runner"],
    },
    runner_instance_metadata: {},
    runner_instance_last_heartbeat_at: "2026-06-04T00:00:00Z",
    runner_instance_created_at: "2026-06-04T00:00:00Z",
    runner_instance_updated_at: "2026-06-04T00:00:00Z",
    ...overrides,
  };
}
