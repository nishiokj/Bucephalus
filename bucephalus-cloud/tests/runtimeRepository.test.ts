import { describe, expect, test } from "bun:test";
import {
  runtimeAttemptObjectsFromSnapshots,
  runtimeContractStagesFromSnapshots,
  runtimeEventRowsFromSnapshots,
  runtimeMetricObservationsFromTrialResults,
  runtimeSnapshotFromWorkerEventPayload,
  runtimeTrialResultsFromSnapshots,
  runtimeValuesFromSnapshots,
} from "../src/runtime/repository";

describe("runtime repository worker snapshots", () => {
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
    expect(runtimeValuesFromSnapshots(snapshot ? [snapshot] : [], "run_control_v2")).toEqual([
      { status: "completed" },
    ]);
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
        schedule_idx: 0,
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
});
