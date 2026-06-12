import { describe, expect, test } from "bun:test";
import { handleRunRoute } from "../src/routes/runs";
import type { PackageRepository, RunRepository } from "../src/packages/repository";
import type {
  RuntimeEventRecord,
  RuntimeEventRowInsert,
  RuntimeRepository,
  WorkerLifecycleEventRecord,
} from "../src/runtime/repository";
import { mergeRuntimeEventRecords } from "../src/runtime/repository";
import type { RunnerRepository } from "../src/runners/repository";

const CORE_RUN_ID = "run_20260611_000001_000001_000001";
const ATTEMPT_ID = "attempt-1";

function ingestRequest(body: unknown): { request: Request; url: URL } {
  const url = new URL(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/runtime/event-rows`);
  return {
    request: new Request(url, {
      method: "POST",
      headers: { authorization: "Bearer attempt-token", "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
    url,
  };
}

function ingestRow(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    core_run_id: CORE_RUN_ID,
    trial_id: "trial-001",
    schedule_idx: 0,
    attempt: 0,
    row_seq: 0,
    event_type: "agent.step",
    ts: "2026-06-11T00:00:00Z",
    payload: { event_type: "agent.step" },
    ...overrides,
  };
}

function fakeRepositories(): {
  runs: RunRepository;
  runtime: RuntimeRepository;
  upserts: RuntimeEventRowInsert[][];
  verifications: unknown[];
} {
  const upserts: RuntimeEventRowInsert[][] = [];
  const verifications: unknown[] = [];
  const runs = {
    async verifyAttemptToken(input: unknown) {
      verifications.push(input);
      return { runId: "cloud-run-1" };
    },
  } as unknown as RunRepository;
  const runtime = {
    async upsertEventRows(rows: RuntimeEventRowInsert[]) {
      upserts.push(rows);
      return rows.length;
    },
  } as unknown as RuntimeRepository;
  return { runs, runtime, upserts, verifications };
}

async function callIngest(
  body: unknown,
  repos = fakeRepositories(),
): Promise<{ response: Response; repos: ReturnType<typeof fakeRepositories> }> {
  const { request, url } = ingestRequest(body);
  const response = await handleRunRoute(
    request,
    url,
    {} as PackageRepository,
    repos.runs,
    repos.runtime,
    {} as RunnerRepository,
    "worker-token",
  );
  expect(response).not.toBeNull();
  return { response: response!, repos };
}

describe("runtime event row ingestion", () => {
  test("lands a normalized batch under the attempt token", async () => {
    const { response, repos } = await callIngest({
      runner_instance_id: "runner-1",
      rows: [ingestRow(), ingestRow({ row_seq: 1, variant_id: "sonnet-4-6", task_id: "task-9" })],
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ received: 2, upserted: 2 });
    expect(repos.verifications).toEqual([
      { attemptId: ATTEMPT_ID, runnerInstanceId: "runner-1", token: "attempt-token" },
    ]);
    const rows = repos.upserts.flat();
    expect(rows[0]).toMatchObject({
      core_run_id: CORE_RUN_ID,
      variant_id: "unknown",
      task_id: "unknown",
      row: { source: "worker_live_ingest", cloud_run_id: "cloud-run-1", attempt_id: ATTEMPT_ID },
    });
    expect(rows[1]).toMatchObject({ variant_id: "sonnet-4-6", task_id: "task-9" });
  });

  test("rejects a missing bearer token before touching the store", async () => {
    const repos = fakeRepositories();
    const url = new URL(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/runtime/event-rows`);
    const request = new Request(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ runner_instance_id: "runner-1", rows: [ingestRow()] }),
    });
    await expect(handleRunRoute(
      request,
      url,
      {} as PackageRepository,
      repos.runs,
      repos.runtime,
      {} as RunnerRepository,
      "worker-token",
    )).rejects.toThrow("requires a valid attempt token");
    expect(repos.upserts).toHaveLength(0);
  });

  test("rejects empty, oversized, and malformed batches", async () => {
    await expect(callIngest({ runner_instance_id: "runner-1", rows: [] }))
      .rejects.toThrow("/rows must be a non-empty array");
    await expect(callIngest({
      runner_instance_id: "runner-1",
      rows: Array.from({ length: 501 }, (_, index) => ingestRow({ row_seq: index })),
    })).rejects.toThrow("at most 500 rows");
    await expect(callIngest({
      runner_instance_id: "runner-1",
      rows: [ingestRow({ core_run_id: "../escape" })],
    })).rejects.toThrow("is not a Core run id");
    await expect(callIngest({
      runner_instance_id: "runner-1",
      rows: [ingestRow({ row_seq: -1 })],
    })).rejects.toThrow("non-negative integer");
  });
});

describe("runtime event stream exposure", () => {
  test("merges worker lifecycle events with trial rows, tagged by source", async () => {
    const trialRow: RuntimeEventRecord = {
      core_run_id: CORE_RUN_ID,
      trial_id: "trial-001",
      schedule_idx: 0,
      attempt: 0,
      row_seq: 0,
      slot_commit_id: "",
      variant_id: "sonnet-4-6",
      task_id: "task-9",
      repl_idx: 0,
      seq: 0,
      event_type: "agent.step",
      ts: "2026-06-11T00:00:05Z",
      payload: { event_type: "agent.step" },
      row: {},
    };
    const workerEvent: WorkerLifecycleEventRecord = {
      event_id: "evt-1",
      seq: 1,
      event_type: "worker.materializing",
      payload: { package_digest: "sha256:abc" },
      created_at: "2026-06-11T00:00:01Z",
    };
    const runs = {
      async getRun() {
        return { run_id: "cloud-run-1" };
      },
    } as unknown as RunRepository;
    const runtime = {
      async eventRows() {
        return [trialRow];
      },
      async workerLifecycleEvents() {
        return [workerEvent];
      },
    } as unknown as RuntimeRepository;

    const url = new URL("https://cloud.example/v1/runs/cloud-run-1/runtime/events");
    const response = await handleRunRoute(
      new Request(url),
      url,
      {} as PackageRepository,
      runs,
      runtime,
      {} as RunnerRepository,
      "worker-token",
    );
    const body = await response!.json();
    expect(body.events).toHaveLength(2);
    expect(body.events[0]).toMatchObject({
      source: "worker",
      event_id: "worker:evt-1",
      event_type: "worker.materializing",
      recorded_at: "2026-06-11T00:00:01Z",
    });
    expect(body.events[1]).toMatchObject({
      source: "trial",
      event_id: `trial:${CORE_RUN_ID}:trial-001:0:0:0`,
      event_type: "agent.step",
      recorded_at: "2026-06-11T00:00:05Z",
    });
  });
});

describe("runtime event record merge", () => {
  function record(overrides: Partial<RuntimeEventRecord>): RuntimeEventRecord {
    return {
      core_run_id: CORE_RUN_ID,
      trial_id: "trial-001",
      schedule_idx: 0,
      attempt: 0,
      row_seq: 0,
      slot_commit_id: "",
      variant_id: "v",
      task_id: "t",
      repl_idx: 0,
      seq: 0,
      event_type: "agent.step",
      ts: null,
      payload: {},
      row: {},
      ...overrides,
    };
  }

  test("store rows win over snapshot rows and intra-source duplicates collapse", async () => {
    const storeLive = record({ row: { source: "worker_live_ingest" } });
    const storeDuplicate = record({ row: { source: "runner_direct" } });
    const snapshotDuplicate = record({ row: { source: "worker_runtime_snapshot" } });
    const snapshotOnly = record({ row_seq: 1, row: { source: "worker_runtime_snapshot" } });

    const merged = mergeRuntimeEventRecords([storeLive, storeDuplicate], [snapshotDuplicate, snapshotOnly]);
    expect(merged).toHaveLength(2);
    expect(merged[0]!.row).toEqual({ source: "worker_live_ingest" });
    expect(merged[1]!.row_seq).toBe(1);
  });
});
