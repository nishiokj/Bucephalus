import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import { handleRunRoute } from "../src/routes/runs";
import type { AuthContext } from "../src/auth";
import { HttpError } from "../src/http";
import type { PackageRepository, RunAttemptRecord, RunRepository } from "../src/packages/repository";
import type { CloudSecretRepository } from "../src/secrets/repository";
import type { RuntimeRepository } from "../src/runtime/repository";
import type { RunnerRepository } from "../src/runners/repository";

describe("Cloud run routes", () => {
  test("run detail includes attempts with failure messages", async () => {
    const failedAttempt = {
      ...attemptRecord(),
      status: "failed",
      ended_at: "2026-06-04T00:00:30Z",
      error_message: "package shape rejected by worker",
    };
    const runs = {
      async getRun() {
        return { ...runRecord(), status: "failed", error_message: failedAttempt.error_message };
      },
      async listAttempts() {
        return [failedAttempt];
      },
    };
    const packages = packagesByDigest([packageRecordWithSecrets()]);
    const runtime = runtimeProgress([]);

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1"),
      new URL("https://cloud.example/v1/runs/run-1"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.error_message).toBe("package shape rejected by worker");
    expect(body.attempts).toHaveLength(1);
    expect(body.attempts[0].attempt_id).toBe("attempt-1");
    expect(body.attempts[0].status).toBe("failed");
    expect(body.attempts[0].error_message).toBe("package shape rejected by worker");
    expect(body.attempts[0].attempt_token).toBeUndefined();
  });

  test("redacts env values and secret refs from user-facing run list responses", async () => {
    const runs = {
      async listRuns() {
        return [runRecord()];
      },
    };
    const packages = packagesByDigest([packageRecordWithSecrets()]);
    const runtime = runtimeProgress([]);

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs"),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.runs[0].env).toBeUndefined();
    expect(body.runs[0].secret_refs).toBeUndefined();
    expect(body.runs[0].env_keys).toEqual(["PUBLIC_FLAG", "SENSITIVE_ENV"]);
    expect(body.runs[0].secret_ids).toEqual(["OPENAI_API_KEY"]);
    expect(JSON.stringify(body)).not.toContain("secret-env-value");
    expect(JSON.stringify(body)).not.toContain("projects/acme/secrets/openai");
  });

  test("run list includes experiment grouping and trial progress from batched enrichment", async () => {
    const first = {
      ...runRecord(),
      run_id: "run-1",
      package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    };
    const second = {
      ...runRecord(),
      run_id: "run-2",
      package_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      status: "running",
    };
    const observed: { packageDigests?: string[]; progressRunIds?: string[] } = {};
    const packages = {
      async listArtifactsByDigests(packageDigests: string[]) {
        observed.packageDigests = packageDigests;
        return [
          {
            ...packageRecordWithSecrets(),
            package_digest: first.package_digest,
            resolved_experiment_json: {
              experiment: { name: "Batchable Experiment" },
              matrix: {
                variants: [{ id: "a" }],
                tasks: { source: "file", path: "tasks.jsonl", limit: 8 },
                repeats: 1,
              },
            },
          },
          {
            ...packageRecordWithSecrets(),
            package_digest: second.package_digest,
            resolved_experiment_json: {
              experiment: { name: "Matrix Experiment" },
              matrix: {
                variants: [{ id: "a" }, { id: "b" }],
                tasks: { source: "file", path: "tasks.jsonl", limit: 3 },
                repeats: 2,
              },
            },
          },
        ];
      },
    };
    const runtime = {
      async trialProgressForCloudRuns(runIds: string[]) {
        observed.progressRunIds = runIds;
        return [
          {
            cloud_run_id: "run-1",
            trials_completed: 3,
            trials_total: 8,
            schedule_progress_v2: { schema_version: "schedule_progress_v2", total_slots: 8 },
          },
          {
            cloud_run_id: "run-2",
            trials_completed: 1,
            trials_total: 12,
            schedule_progress_v2: { schema_version: "schedule_progress_v2", total_slots: 12 },
          },
        ];
      },
    };
    const runs = {
      async listRuns() {
        return [first, second];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs"),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(observed.packageDigests).toEqual([first.package_digest, second.package_digest]);
    expect(observed.progressRunIds).toEqual(["run-1", "run-2"]);
    expect(body.runs[0].experiment_name).toBe("Batchable Experiment");
    expect(body.runs[0].trials_completed).toBe(3);
    expect(body.runs[0].trials_total).toBe(8);
    expect(body.runs[1].experiment_name).toBe("Matrix Experiment");
    expect(body.runs[1].trials_completed).toBe(1);
    expect(body.runs[1].trials_total).toBe(12);
  });

  test("run list can skip runtime enrichment while keeping package names", async () => {
    const run = {
      ...runRecord(),
      run_id: "run-1",
      package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      status: "queued",
    };
    const observed: { packageDigests?: string[]; progressCalled?: boolean } = {};
    const packages = {
      async listArtifactsByDigests(packageDigests: string[]) {
        observed.packageDigests = packageDigests;
        return [{
          ...packageRecordWithSecrets(),
          package_digest: run.package_digest,
          resolved_experiment_json: { experiment: { name: "Fast List Experiment" } },
        }];
      },
    };
    const runtime = {
      async trialProgressForCloudRuns() {
        observed.progressCalled = true;
        throw new Error("trialProgressForCloudRuns should not be called");
      },
    };
    const runs = {
      async listRuns() {
        return [run];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs?include_runtime=false"),
      new URL("https://cloud.example/v1/runs?include_runtime=false"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(observed.packageDigests).toEqual([run.package_digest]);
    expect(observed.progressCalled).toBeUndefined();
    expect(body.runs[0].experiment_name).toBe("Fast List Experiment");
    expect(body.runs[0].trials_completed).toBeUndefined();
    expect(body.runs[0].trials_total).toBeUndefined();
    expect(body.runs[0].pending_reason).toBeUndefined();
  });

  test("run list can omit run config while preserving display fields", async () => {
    const run = {
      ...runRecord(),
      runtime_options: {
        variant: "opus-4.8",
        params: { prompt: "large prompt config" },
      },
      run_requirements: {
        ...runRecord().run_requirements,
        executor: "runner-docker",
      },
    };
    const packageRecord = packageRecordWithSecrets();
    (packageRecord.resolved_experiment_json as Record<string, unknown>).experiment = { name: "Summary Experiment" };
    const packages = packagesByDigest([packageRecord]);
    const runs = {
      async listRuns() {
        return [run];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs?include_runtime=false&include_config=false"),
      new URL("https://cloud.example/v1/runs?include_runtime=false&include_config=false"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      runtimeProgress([]) as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(body.runs[0].experiment_name).toBe("Summary Experiment");
    expect(body.runs[0].variant).toBe("opus-4.8");
    expect(body.runs[0].runtime).toBe("runner-docker");
    expect(body.runs[0].region).toBeUndefined();
    expect(body.runs[0].runtime_options).toBeUndefined();
    expect(body.runs[0].run_requirements).toBeUndefined();
    expect(body.runs[0].package_provenance).toBeUndefined();
    expect(body.runs[0].env_keys).toBeUndefined();
    expect(body.runs[0].secret_ids).toBeUndefined();
    expect(body.runs[0].pending_reason).toBeUndefined();
    expect(body.runs[0].trials_completed).toBeUndefined();
    expect(body.runs[0].trials_total).toBeUndefined();
    expect(body.runs[0].error_message).toBeUndefined();
  });

  test("run detail includes pending reason for queued runs", async () => {
    const runs = {
      async getRun() {
        return { ...runRecord(), status: "waiting_for_runner" };
      },
      async listAttempts() {
        return [];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1"),
      new URL("https://cloud.example/v1/runs/run-1"),
      packagesByDigest([packageRecordWithSecrets()]) as unknown as PackageRepository,
      runs as unknown as RunRepository,
      runtimeProgress([{ cloud_run_id: "run-1", trials_completed: 0, trials_total: null }]) as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(body.pending_reason).toBe("waiting_for_capacity");
  });

  test("run list marks queued runs with no matching runner separately from busy capacity", async () => {
    const modalRun = {
      ...runRecord(),
      run_id: "run-modal",
      run_requirements: {
        ...runRecord().run_requirements,
        executor: "modal" as const,
      },
    };
    const dockerRun = {
      ...runRecord(),
      run_id: "run-docker",
    };
    const runs = {
      async listRuns() {
        return [modalRun, dockerRun];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs"),
      new URL("https://cloud.example/v1/runs"),
      packagesByDigest([packageRecordWithSecrets()]) as unknown as PackageRepository,
      runs as unknown as RunRepository,
      runtimeProgress([]) as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(body.runs[0].pending_reason).toBe("no_matching_runner");
    expect(body.runs[1].pending_reason).toBe("waiting_for_capacity");
  });

  test("package lists can omit full config while preserving queue fields", async () => {
    const record = {
      ...packageRecordWithSecrets(),
      manifest_json: {
        name: "Manifest Name",
        description: "A package summary",
        tags: ["nightly", "eval"],
        owner: "bench-team",
      },
      resolved_experiment_json: {
        experiment: { name: "Resolved Name" },
        runtime: {
          secrets: [{ name: "OPENAI_API_KEY", mount: { target: "/run/secrets/openai" } }],
        },
      },
      diagnostics: [{ level: "info", message: "large diagnostics", code: "ok" }],
      image_refs: ["registry.example/worker@sha256:abc"],
      owner_key: "issuer:user-a",
    };
    const packages = {
      async listArtifacts(input: { limit?: number }) {
        expect(input.limit).toBe(5);
        return [record];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/packages?limit=5&include_config=false"),
      new URL("https://cloud.example/v1/packages?limit=5&include_config=false"),
      packages as unknown as PackageRepository,
      {} as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(body.packages[0].name).toBe("Resolved Name");
    expect(body.packages[0].description).toBe("A package summary");
    expect(body.packages[0].tags).toEqual(["eval", "nightly"]);
    expect(body.packages[0].owner).toBe("issuer:user-a");
    expect(body.packages[0].secret_requirements).toEqual([{
      id: "OPENAI_API_KEY",
      target: "/run/secrets/openai",
      required_for_variants: [],
    }]);
    expect(body.packages[0].manifest_json).toBeUndefined();
    expect(body.packages[0].resolved_experiment_json).toBeUndefined();
    expect(body.packages[0].diagnostics).toBeUndefined();
    expect(body.packages[0].image_refs).toBeUndefined();
    expect(body.packages[0].package_provenance).toBeUndefined();
  });

  test("rejects malformed pagination query values instead of silently defaulting", async () => {
    const packages = {
      async listArtifacts() {
        throw new Error("listArtifacts should not be called");
      },
    };
    const runs = {
      async listRuns() {
        throw new Error("listRuns should not be called");
      },
    };
    const runtime = {
      async eventRows() {
        throw new Error("eventRows should not be called");
      },
      async workerLifecycleEvents() {
        throw new Error("workerLifecycleEvents should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/packages?limit=potato"),
      new URL("https://cloud.example/v1/packages?limit=potato"),
      packages as unknown as PackageRepository,
      {} as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toThrow("/limit must be an integer");

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs?limit=0"),
      new URL("https://cloud.example/v1/runs?limit=0"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toThrow("/limit must be >= 1");

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/events?after_row_seq=nan"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/events?after_row_seq=nan"),
      {} as PackageRepository,
      { async getRun() { return runRecord(); } } as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toThrow("/after_row_seq must be an integer");

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/events?continue=not-a-cursor"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/events?continue=not-a-cursor"),
      {} as PackageRepository,
      { async getRun() { return runRecord(); } } as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toThrow("/continue must be formatted as event-row-seq:<row_seq>");

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/events?after_row_seq=1&continue=event-row-seq:1"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/events?after_row_seq=1&continue=event-row-seq:1"),
      {} as PackageRepository,
      { async getRun() { return runRecord(); } } as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toThrow("Runtime event queries accept either after_row_seq or continue, not both");
  });

  test("runtime event route follows event-row-seq continue cursors", async () => {
    const observed: { eventInput?: unknown; workerInput?: unknown } = {};
    const runtime = {
      async eventRows(_runId: string, input: unknown) {
        observed.eventInput = input;
        return [{
          source: "runtime.event_rows",
          core_run_id: "core-run-1",
          trial_id: "trial-1",
          schedule_idx: 0,
          attempt: 0,
          row_seq: 8,
          slot_commit_id: "slot-1",
          variant_id: "variant-1",
          task_id: "task-1",
          repl_idx: 0,
          seq: 80,
          event_type: "trial.started",
          ts: "2026-06-11T00:00:01Z",
          resource_refs: [],
          payload: {},
          row: {},
        }];
      },
      async workerLifecycleEvents(_runId: string, input: unknown) {
        observed.workerInput = input;
        return [{
          event_id: "evt-9",
          seq: 9,
          event_type: "worker.ready",
          payload: {},
          created_at: "2026-06-11T00:00:02Z",
        }];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/events?limit=1&continue=event-row-seq:7"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/events?limit=1&continue=event-row-seq:7"),
      {} as PackageRepository,
      { async getRun() { return runRecord(); } } as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(observed.eventInput).toMatchObject({
      limit: 1,
      afterRowSeq: 7,
      sources: ["runtime.event_rows", "worker_runtime_snapshot"],
    });
    expect(observed.workerInput).toMatchObject({ limit: 1, afterRowSeq: 7 });
    expect(body.metadata).toMatchObject({
      resourceVersion: "event-row-seq:8",
      continue: "event-row-seq:8",
      remainingItemCount: null,
      limit: 1,
      returned: 1,
      after_row_seq: 7,
      next_after_row_seq: 8,
    });
    expect(body.events.map((event: { row_seq: number }) => event.row_seq)).toEqual([8]);
  });

  test("keeps env values and secret refs in worker claim responses", async () => {
    const runs = {
      async claimNextRun() {
        return {
          run: runRecord(),
          attempt: attemptRecord("attempt-token"),
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/worker/runs/claim", {
        method: "POST",
        headers: {
          authorization: "Bearer worker-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: "runner-instance-1",
        }),
      }),
      new URL("https://cloud.example/v1/worker/runs/claim"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.attempt.attempt_token).toBe("attempt-token");
    expect(body.run.env.SENSITIVE_ENV).toBe("secret-env-value");
    expect(body.run.secret_refs.OPENAI_API_KEY).toBe("gcp-secret-manager://projects/acme/secrets/openai/versions/1");
  });

  test("worker run claim rejects missing worker token before queue access", async () => {
    let claimCalls = 0;
    const runs = {
      async claimNextRun() {
        claimCalls += 1;
        return null;
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/worker/runs/claim", {
        method: "POST",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: "runner-instance-1",
        }),
      }),
      new URL("https://cloud.example/v1/worker/runs/claim"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
      message: "worker run claim requires a valid worker token",
    });
    expect(claimCalls).toBe(0);
  });

  test("worker lease expiration rejects missing worker token before queue access", async () => {
    let expireCalls = 0;
    const runs = {
      async expireLeases() {
        expireCalls += 1;
        return [];
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/worker/runs/expire-leases", {
        method: "POST",
      }),
      new URL("https://cloud.example/v1/worker/runs/expire-leases"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
      message: "worker lease expiration requires a valid worker token",
    });
    expect(expireCalls).toBe(0);
  });

  test("worker lease expiration also expires runtime access resources", async () => {
    const observed: { expiredRuns?: boolean; expiredAccess?: boolean } = {};
    const runs = {
      async expireLeases() {
        observed.expiredRuns = true;
        return [];
      },
    };
    const runtime = {
      async expireRuntimeAccessRequests() {
        observed.expiredAccess = true;
        return [runtimeAccessRequestRecord("pf-1", "port_forward")];
      },
      runtimeAccessRequestResourceForRunId(runId: string, request: { access_request_id: string; kind: string }) {
        return runtimeAccessResource(runId, request);
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/worker/runs/expire-leases", {
        method: "POST",
        headers: { authorization: "Bearer worker-token" },
      }),
      new URL("https://cloud.example/v1/worker/runs/expire-leases"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(observed).toEqual({ expiredRuns: true, expiredAccess: true });
    expect(body.expired_access_resources).toEqual([
      expect.objectContaining({ kind: "PortForward" }),
    ]);
  });

  test("worker attempt heartbeat requires the attempt token", async () => {
    const observed: { token?: string; attemptId?: string; runnerInstanceId?: string | null | undefined } = {};
    const runs = {
      async verifyAttemptToken(input: { token: string; attemptId: string; runnerInstanceId?: string | null }) {
        observed.token = input.token;
        observed.attemptId = input.attemptId;
        observed.runnerInstanceId = input.runnerInstanceId;
      },
      async heartbeatAttempt() {
        return attemptRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/worker/run-attempts/attempt-1/heartbeat", {
        method: "POST",
        headers: {
          authorization: "Bearer attempt-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: "runner-instance-1",
        }),
      }),
      new URL("https://cloud.example/v1/worker/run-attempts/attempt-1/heartbeat"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    expect(response).not.toBeNull();
    expect(observed).toEqual({
      token: "attempt-token",
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
    });
  });

  test("worker runtime resource endpoints list pending port-forwards for an attempt", async () => {
    const observed: { token?: string; attemptId?: string; runnerInstanceId?: string | null | undefined } = {};
    const runs = {
      async verifyAttemptToken(input: { token: string; attemptId: string; runnerInstanceId?: string | null }) {
        observed.token = input.token;
        observed.attemptId = input.attemptId;
        observed.runnerInstanceId = input.runnerInstanceId;
      },
    };
    const runtime = {
      async portForwardRequestsForAttempt(input: { attemptId: string; runnerInstanceId: string }) {
        expect(input).toEqual({ attemptId: "attempt-1", runnerInstanceId: "runner-instance-1" });
        return [runtimeAccessRequestRecord("pf-1", "port_forward")];
      },
      runtimeAccessRequestResourceForRunId(runId: string, request: { access_request_id: string; kind: string }) {
        return runtimeAccessResource(runId, request);
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward?runner_instance_id=runner-instance-1", {
        headers: { authorization: "Bearer attempt-token" },
      }),
      new URL("https://cloud.example/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward?runner_instance_id=runner-instance-1"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(observed).toEqual({
      token: "attempt-token",
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
    });
    expect(body.resources).toEqual([
      expect.objectContaining({ kind: "PortForward", metadata: expect.objectContaining({ name: "pf-1" }) }),
    ]);
  });

  test("worker runtime resource endpoints update exec lifecycle", async () => {
    const observed: { token?: string; update?: unknown } = {};
    const runs = {
      async verifyAttemptToken(input: { token: string }) {
        observed.token = input.token;
      },
    };
    const runtime = {
      async updateExecRequest(input: unknown) {
        observed.update = input;
        return runtimeAccessRequestRecord("exec-1", "exec");
      },
      runtimeAccessRequestResourceForRunId(runId: string, request: { access_request_id: string; kind: string }) {
        return runtimeAccessResource(runId, request);
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/complete", {
        method: "POST",
        headers: {
          authorization: "Bearer attempt-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: "runner-instance-1",
          connection: {
            exit_code: 0,
            stdout: "ok",
          },
        }),
      }),
      new URL("https://cloud.example/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/complete"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(observed.token).toBe("attempt-token");
    expect(observed.update).toEqual({
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "exec-1",
      status: "completed",
      connection: {
        exit_code: 0,
        stdout: "ok",
      },
      errorMessage: null,
    });
    expect(body.resource.kind).toBe("Exec");
  });

  test("worker runtime resource endpoints complete port-forward lifecycle", async () => {
    const observed: { token?: string; update?: unknown } = {};
    const runs = {
      async verifyAttemptToken(input: { token: string }) {
        observed.token = input.token;
      },
    };
    const runtime = {
      async updatePortForwardRequest(input: unknown) {
        observed.update = input;
        return runtimeAccessRequestRecord("pf-1", "port_forward");
      },
      runtimeAccessRequestResourceForRunId(runId: string, request: { access_request_id: string; kind: string }) {
        return runtimeAccessResource(runId, request);
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/completed", {
        method: "POST",
        headers: {
          authorization: "Bearer attempt-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: "runner-instance-1",
          connection: {
            client_endpoint: "tcp://127.0.0.1:18080",
          },
        }),
      }),
      new URL("https://cloud.example/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/completed"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    );

    const body = await response!.json();
    expect(observed.token).toBe("attempt-token");
    expect(observed.update).toEqual({
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      accessRequestId: "pf-1",
      status: "completed",
      connection: {
        client_endpoint: "tcp://127.0.0.1:18080",
      },
      errorMessage: null,
    });
    expect(body.resource.kind).toBe("PortForward");
  });

  test("package content download requires an attempt token for that package", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-package-content-"));
    try {
      const packagePath = join(root, "package.tgz");
      await writeFile(packagePath, "package bytes");
      const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
      const observed: {
        token?: string;
        attemptId?: string;
        packageDigest?: string | null | undefined;
        artifactDigest?: string;
        artifactOwnerKey: string | undefined;
      } = { artifactOwnerKey: undefined };
      const packages = {
        async getArtifact(packageDigest: string, ownerKey?: string) {
          observed.artifactDigest = packageDigest;
          observed.artifactOwnerKey = ownerKey;
          return {
            package_digest: digest,
            upload_id: "upload-1",
            storage_path: packagePath,
            byte_size: 13,
            media_type: "application/gzip",
            manifest_json: {},
            resolved_experiment_json: {},
            target: null,
            image_refs: [],
            diagnostics: [],
            package_provenance: packageProvenance(),
            status: "accepted",
            created_at: "2026-06-04T00:00:00Z",
            updated_at: "2026-06-04T00:00:00Z",
          };
        },
      };
      const runs = {
        async verifyAttemptToken(input: { token: string; attemptId: string; packageDigest?: string | null }) {
          observed.token = input.token;
          observed.attemptId = input.attemptId;
          observed.packageDigest = input.packageDigest;
          return { runId: "run-1", ownerKey: "issuer:user-a", packageDigest: digest };
        },
      };

      const response = await handleRunRoute(
        new Request(`https://cloud.example/v1/packages/${digest}/content`, {
          headers: {
            authorization: "Bearer attempt-token",
            "x-bucephalus-attempt-id": "attempt-1",
          },
        }),
        new URL(`https://cloud.example/v1/packages/${digest}/content`),
        packages as unknown as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        runnersWithDockerPool() as any,
        "worker-token",
      );

      expect(response).not.toBeNull();
      expect(await response!.text()).toBe("package bytes");
      expect(observed).toEqual({
        token: "attempt-token",
        attemptId: "attempt-1",
        packageDigest: digest,
        artifactDigest: digest,
        artifactOwnerKey: "issuer:user-a",
      });
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("package content download rejects missing attempt auth before package lookup", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let packageLookups = 0;
    const packages = {
      async getArtifact() {
        packageLookups += 1;
        return null;
      },
    };
    const runs = {
      async verifyAttemptToken() {
        throw new Error("verifyAttemptToken should not run without a bearer token");
      },
    };

    await expect(handleRunRoute(
      new Request(`https://cloud.example/v1/packages/${digest}/content`, {
        headers: {
          "x-bucephalus-attempt-id": "attempt-1",
        },
      }),
      new URL(`https://cloud.example/v1/packages/${digest}/content`),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
      message: "worker attempt requires a valid attempt token",
    });
    expect(packageLookups).toBe(0);
  });

  test("package content download rejects missing attempt id before package lookup", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let packageLookups = 0;
    const packages = {
      async getArtifact() {
        packageLookups += 1;
        return null;
      },
    };
    const runs = {
      async verifyAttemptToken() {
        throw new Error("verifyAttemptToken should not run without an attempt id");
      },
    };

    await expect(handleRunRoute(
      new Request(`https://cloud.example/v1/packages/${digest}/content`, {
        headers: {
          authorization: "Bearer attempt-token",
        },
      }),
      new URL(`https://cloud.example/v1/packages/${digest}/content`),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toMatchObject({
      status: 400,
      code: "missing_header",
      message: "package content download requires x-bucephalus-attempt-id",
    });
    expect(packageLookups).toBe(0);
  });

  test("package content download rejects attempt tokens not bound to the requested package before artifact lookup", async () => {
    const requestedDigest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let packageLookups = 0;
    const observed: { packageDigest: string | null | undefined } = { packageDigest: undefined };
    const packages = {
      async getArtifact() {
        packageLookups += 1;
        return null;
      },
    };
    const runs = {
      async verifyAttemptToken(input: { packageDigest?: string | null }) {
        observed.packageDigest = input.packageDigest;
        throw new HttpError(401, "unauthorized", "worker attempt requires a valid attempt token");
      },
    };

    await expect(handleRunRoute(
      new Request(`https://cloud.example/v1/packages/${requestedDigest}/content`, {
        headers: {
          authorization: "Bearer attempt-token-for-another-run",
          "x-bucephalus-attempt-id": "attempt-1",
        },
      }),
      new URL(`https://cloud.example/v1/packages/${requestedDigest}/content`),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
      message: "worker attempt requires a valid attempt token",
    });
    expect(observed.packageDigest).toBe(requestedDigest);
    expect(packageLookups).toBe(0);
  });

  test("package content download rejects malformed package digests before auth binding or artifact lookup", async () => {
    const packages = {
      async getArtifact() {
        throw new Error("getArtifact should not be called");
      },
    };
    const runs = {
      async verifyAttemptToken() {
        throw new Error("verifyAttemptToken should not be called for malformed package digests");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/packages/sha256:short/content", {
        headers: {
          authorization: "Bearer attempt-token",
          "x-bucephalus-attempt-id": "attempt-1",
        },
      }),
      new URL("https://cloud.example/v1/packages/sha256:short/content"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
    )).rejects.toMatchObject({
      status: 400,
      code: "invalid_request",
      message: "/package_digest must be sha256:<64 lowercase hex chars>",
    });
  });

  test("package content download can stream an artifact stored in R2", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const previousFetch = globalThis.fetch;
    const previousEnv = captureEnv([
      "BUCEPHALUS_CLOUD_STORAGE_BACKEND",
      "BUCEPHALUS_CLOUD_R2_ACCOUNT_ID",
      "BUCEPHALUS_CLOUD_R2_BUCKET",
      "BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID",
      "BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY",
    ]);
    const requests: string[] = [];
    globalThis.fetch = (async (url: string | URL | Request) => {
      requests.push(String(url));
      return new Response("package bytes", { status: 200 });
    }) as typeof fetch;
    process.env.BUCEPHALUS_CLOUD_STORAGE_BACKEND = "r2";
    process.env.BUCEPHALUS_CLOUD_R2_ACCOUNT_ID = "account-id";
    process.env.BUCEPHALUS_CLOUD_R2_BUCKET = "buc-artifacts";
    process.env.BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID = "access-key";
    process.env.BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY = "secret-key";
    try {
      const observed: { artifactDigest?: string; artifactOwnerKey: string | undefined } = {
        artifactOwnerKey: undefined,
      };
      const packages = {
        async getArtifact(packageDigest: string, ownerKey?: string) {
          observed.artifactDigest = packageDigest;
          observed.artifactOwnerKey = ownerKey;
          return {
            package_digest: digest,
            upload_id: "upload-1",
            storage_path: "r2://buc-artifacts/uploads/upload-1/content.blob",
            byte_size: 13,
            media_type: "application/gzip",
            manifest_json: {},
            resolved_experiment_json: {},
            target: null,
            image_refs: [],
            diagnostics: [],
            package_provenance: packageProvenance(),
            status: "accepted",
            created_at: "2026-06-04T00:00:00Z",
            updated_at: "2026-06-04T00:00:00Z",
          };
        },
      };
      const runs = {
        async verifyAttemptToken() {
          return { runId: "run-1", ownerKey: "issuer:user-a", packageDigest: digest };
        },
      };

      const response = await handleRunRoute(
        new Request(`https://cloud.example/v1/packages/${digest}/content`, {
          headers: {
            authorization: "Bearer attempt-token",
            "x-bucephalus-attempt-id": "attempt-1",
          },
        }),
        new URL(`https://cloud.example/v1/packages/${digest}/content`),
        packages as unknown as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        runnersWithDockerPool() as any,
        "worker-token",
      );

      expect(await response!.text()).toBe("package bytes");
      expect(requests).toEqual([
        "https://account-id.r2.cloudflarestorage.com/buc-artifacts/uploads/upload-1/content.blob",
      ]);
      expect(observed).toEqual({
        artifactDigest: digest,
        artifactOwnerKey: "issuer:user-a",
      });
    } finally {
      globalThis.fetch = previousFetch;
      restoreEnv(previousEnv);
    }
  });

  test("worker runtime artifact upload stores bytes and persists content metadata", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-runtime-artifact-upload-"));
    const previousEnv = captureEnv([
      "BUCEPHALUS_CLOUD_DATA_DIR",
      "BUCEPHALUS_CLOUD_STORAGE_BACKEND",
    ]);
    process.env.BUCEPHALUS_CLOUD_DATA_DIR = root;
    process.env.BUCEPHALUS_CLOUD_STORAGE_BACKEND = "filesystem";
    try {
      const observed: {
        verify?: { token: string; attemptId: string; runnerInstanceId?: string | null };
        persisted?: Record<string, unknown>;
      } = {};
      const runs = {
        async verifyAttemptToken(input: { token: string; attemptId: string; runnerInstanceId?: string | null }) {
          observed.verify = input;
          return { runId: "cloud-run-1", ownerKey: "issuer:user-a", packageDigest: runRecord().package_digest };
        },
      };
      const runtime = {
        async upsertAttemptObjectContent(input: Record<string, unknown>) {
          observed.persisted = input;
          return {
            core_run_id: input.core_run_id,
            trial_id: input.trial_id,
            schedule_idx: input.schedule_idx,
            attempt: input.attempt,
            role: input.role,
            object_ref: input.object_ref,
            storage_path: input.storage_path,
            media_type: input.media_type,
            byte_size: input.byte_size,
            sha256: input.sha256,
            relative_path: input.relative_path,
            metadata: input.metadata,
            recorded_at_ms: input.recorded_at_ms,
          };
        },
      };

      const response = await handleRunRoute(
        new Request("https://cloud.example/v1/worker/run-attempts/attempt-1/runtime/artifacts", {
          method: "POST",
          headers: {
            authorization: "Bearer attempt-token",
            "content-type": "application/json; charset=utf-8",
            "x-bucephalus-runner-instance-id": "runner-instance-1",
            "x-bucephalus-core-run-id": "run_20260614_051159_561175_000001",
            "x-bucephalus-trial-id": "trial_1",
            "x-bucephalus-schedule-idx": "0",
            "x-bucephalus-trial-attempt": "0",
            "x-bucephalus-artifact-role": "agent_result",
            "x-bucephalus-artifact-relative-path": "agent/result.json",
          },
          body: JSON.stringify({ final: "generated answer" }),
        }),
        new URL("https://cloud.example/v1/worker/run-attempts/attempt-1/runtime/artifacts"),
        {} as PackageRepository,
        runs as unknown as RunRepository,
        runtime as unknown as RuntimeRepository,
        runnersWithDockerPool() as any,
        "worker-token",
      );

      const body = await response!.json();
      expect(body.artifact).toMatchObject({
        core_run_id: "run_20260614_051159_561175_000001",
        trial_id: "trial_1",
        role: "agent_result",
        content_available: true,
        media_type: "application/json; charset=utf-8",
        byte_size: 28,
        relative_path: "agent/result.json",
      });
      expect(observed.verify).toEqual({
        token: "attempt-token",
        attemptId: "attempt-1",
        runnerInstanceId: "runner-instance-1",
      });
      expect(String(observed.persisted?.object_ref)).toContain("runtime://run_20260614_051159_561175_000001/trial_1/0/agent_result/sha256%3A");
      expect(await readFile(String(observed.persisted?.storage_path), "utf8")).toBe(JSON.stringify({ final: "generated answer" }));
    } finally {
      restoreEnv(previousEnv);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("deprecated run-scoped runtime compatibility routes are not handled", async () => {
    const runs = {
      async getRun() {
        throw new Error("deprecated runtime routes should not fetch runs");
      },
    };
    const deprecatedPaths = [
      "/v1/runs/run-1/runtime",
      "/v1/runs/run-1/runtime/results",
      "/v1/runs/run-1/runtime/kv/run_control_v2",
      "/v1/runs/run-1/runtime/artifacts/trial_1/agent_result",
      "/v1/runs/run-1/runtime/port-forwards",
      "/v1/runs/run-1/runtime/port-forwards/pf-1",
      "/v1/runs/run-1/runtime/execs",
      "/v1/runs/run-1/runtime/execs/exec-1",
    ];

    for (const path of deprecatedPaths) {
      const response = await handleRunRoute(
        new Request(`https://cloud.example${path}`),
        new URL(`https://cloud.example${path}`),
        {} as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        runnersWithDockerPool() as any,
        "worker-token",
        authContext("user-a"),
      );
      expect(response).toBeNull();
    }
  });

  test("runtime resource API lists repository-backed runner and access resources", async () => {
    const observed: { input?: unknown; runId?: string; ownerKey: string | undefined } = { ownerKey: undefined };
    const runner = runtimeRouteResource("RunnerInstance", "runner-1", {
      labels: { "bucephalus.dev/run-id": "run-1" },
      status: {
        phase: "online",
        access: { reachable: true, port_forward: true, exec: true },
      },
    });
    const runs = {
      async getRun(runId: string, ownerKey?: string) {
        observed.runId = runId;
        observed.ownerKey = ownerKey;
        return runRecord();
      },
    };
    const runtime = {
      async resources(runId: string, _run: unknown, input: unknown) {
        observed.runId = runId;
        observed.input = input;
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceList",
          metadata: {
            resourceVersion: "sha256:list",
            continue: null,
            remainingItemCount: 0,
            total: 1,
            returned: 1,
          },
          cloud_run_id: "run-1",
          core_run_ids: ["core-run-1"],
          resources: [runner],
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources?kind=RunnerInstance,Trial&category=runner,access-target&field_selector=status.access.exec=true&label_selector=bucephalus.dev/run-id=run-1&limit=2&continue=opaque-cursor"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources?kind=RunnerInstance,Trial&category=runner,access-target&field_selector=status.access.exec=true&label_selector=bucephalus.dev/run-id=run-1&limit=2&continue=opaque-cursor"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    const body = await response!.json();
    expect(observed.ownerKey).toBe("issuer:user-a");
    expect(observed.input).toEqual({
      kinds: ["RunnerInstance", "Trial"],
      categories: ["runner", "access-target"],
      labelSelector: "bucephalus.dev/run-id=run-1",
      fieldSelector: "status.access.exec=true",
      limit: 2,
      continueToken: "opaque-cursor",
      requester: "issuer:user-a",
    });
    expect(body.resources).toEqual([runner]);
  });

  test("runtime inspect route forwards filters, event limit, and requester", async () => {
    const observed: { input?: unknown; runId?: string; ownerKey?: string | null | undefined } = {};
    const runs = {
      async getRun(_runId: string, ownerKey?: string | null) {
        observed.ownerKey = ownerKey;
        return runRecord();
      },
    };
    const runtime = {
      async inspectBundle(runId: string, _run: unknown, input: unknown) {
        observed.runId = runId;
        observed.input = input;
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeInspectBundle",
          cloud_run_id: runId,
          generated_at: "2026-06-19T00:00:00Z",
          resource_filter: {
            kinds: ["RunnerInstance", "Trial"],
            categories: ["runner", "access-target"],
            label_selector: "bucephalus.dev/run-id=run-1",
            field_selector: "status.access.exec=true",
          },
          api_resources: { resources: [] },
          resource_inventory: { resources: [], metadata: {} },
          resource_health: { summary: {}, resources: [] },
          resource_metrics: { summary: {}, resources: [] },
          event_list: { metadata: {}, events: [] },
          log_refs: [],
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/inspect?kind=RunnerInstance,Trial&category=runner,access-target&field_selector=status.access.exec=true&label_selector=bucephalus.dev/run-id=run-1&event_limit=33"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/inspect?kind=RunnerInstance,Trial&category=runner,access-target&field_selector=status.access.exec=true&label_selector=bucephalus.dev/run-id=run-1&event_limit=33"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    const body = await response!.json();
    expect(observed.ownerKey).toBe("issuer:user-a");
    expect(observed.runId).toBe("run-1");
    expect(observed.input).toEqual({
      kinds: ["RunnerInstance", "Trial"],
      categories: ["runner", "access-target"],
      labelSelector: "bucephalus.dev/run-id=run-1",
      fieldSelector: "status.access.exec=true",
      eventLimit: 33,
      requester: "issuer:user-a",
    });
    expect(body.kind).toBe("RuntimeInspectBundle");
  });

  test("runtime resource watch route forwards cursors and bookmark opt-in", async () => {
    const observed: { input?: unknown; runId?: string } = {};
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async watchResources(runId: string, _run: unknown, input: unknown) {
        observed.runId = runId;
        observed.input = input;
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceWatchList",
          cloud_run_id: "run-1",
          generated_at: "2026-06-19T00:00:00Z",
          core_run_ids: ["core-run-1"],
          resource_versions: {},
          events: [{
            type: "BOOKMARK",
            resource_ref: { apiVersion: "bucephalus.dev/v1alpha1", kind: "RuntimeResourceList", name: "run-1" },
            resource_version: "sha256:list",
          }],
          resource_inventory: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "RuntimeResourceList",
            metadata: {
              resourceVersion: "sha256:list",
              continue: null,
              remainingItemCount: 0,
              total: 0,
              returned: 0,
            },
            cloud_run_id: "run-1",
            core_run_ids: ["core-run-1"],
            resources: [],
          },
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/watch?kind=RunnerInstance&category=runner&field_selector=status.access.exec=true&resource_version=sha256%3Alist&known_resource=RunnerInstance%2Frunner-1%3Dsha256%3Aold&allow_bookmarks=true"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/watch?kind=RunnerInstance&category=runner&field_selector=status.access.exec=true&resource_version=sha256%3Alist&known_resource=RunnerInstance%2Frunner-1%3Dsha256%3Aold&allow_bookmarks=true"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    const input = observed.input as {
      filter: { kinds: string[]; categories: string[]; labelSelector: string | null; fieldSelector: string | null };
      resourceVersion: string;
      knownResourceVersions: Map<string, string>;
      allowBookmarks: boolean;
      requester: string;
    };
    expect(observed.runId).toBe("run-1");
    expect(input.filter).toEqual({
      kinds: ["RunnerInstance"],
      categories: ["runner"],
      labelSelector: null,
      fieldSelector: "status.access.exec=true",
    });
    expect(input.resourceVersion).toBe("sha256:list");
    expect(Array.from(input.knownResourceVersions.entries())).toEqual([["RunnerInstance/runner-1", "sha256:old"]]);
    expect(input.allowBookmarks).toBe(true);
    expect(input.requester).toBe("issuer:user-a");
    const body = await response!.json();
    expect(body.events).toEqual([{
      type: "BOOKMARK",
      resource_ref: { apiVersion: "bucephalus.dev/v1alpha1", kind: "RuntimeResourceList", name: "run-1" },
      resource_version: "sha256:list",
    }]);
  });

  test("runtime resource event routes forward continue cursors and filters to repository", async () => {
    const observed: { input?: unknown } = {};
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async resourceEvents(_runId: string, _run: unknown, input: unknown) {
        observed.input = input;
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceEventList",
          cloud_run_id: "run-1",
          metadata: {
            resourceVersion: "event-row-seq:6",
            continue: null,
            remainingItemCount: 0,
            limit: 3,
            returned: 0,
            after_row_seq: 5,
            next_after_row_seq: 5,
          },
          events: [],
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1/events?limit=3&continue=event-row-seq:5&event_type=runtime.access.exec.requested&source=cloud.run_events&trial_id=trial-1&task_id=task-1"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1/events?limit=3&continue=event-row-seq:5&event_type=runtime.access.exec.requested&source=cloud.run_events&trial_id=trial-1&task_id=task-1"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    await expect(response!.json()).resolves.toMatchObject({
      kind: "RuntimeResourceEventList",
      cloud_run_id: "run-1",
    });
    expect(observed.input).toEqual({
      kind: "Trial",
      name: "trial-1",
      limit: 3,
      afterRowSeq: undefined,
      continueToken: "event-row-seq:5",
      filter: {
        eventTypes: ["runtime.access.exec.requested"],
        sources: ["cloud.run_events"],
        trialId: "trial-1",
        taskId: "task-1",
      },
      requester: "issuer:user-a",
    });
  });

  test("runtime resource operation reviews use repository access support instead of stale route defaults", async () => {
    const observed: { input?: unknown } = {};
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async reviewResourceOperation(_runId: string, _run: unknown, input: unknown) {
        observed.input = input;
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceOperationReview",
          cloud_run_id: "run-1",
          generated_at: "2026-01-01T00:00:00.000Z",
          core_run_ids: ["core-run-1"],
          resource_ref: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: "TrialContainer",
            name: "trial-1.agent.container-1",
          },
          resource_version: "sha256:target",
          resource_generation: 2,
          observed_generation: 2,
          operation: "port-forward",
          matched_operation: "port-forward",
          supported: true,
          reason: null,
          message: null,
          command: "buc runs port-forward run-1 TrialContainer/trial-1.agent.container-1 --target-port PORT",
          verb: "create",
          subresource: "port-forward",
          action: null,
          requires_running_run: true,
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/TrialContainer/trial-1.agent.container-1/operations/port-forward"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/TrialContainer/trial-1.agent.container-1/operations/port-forward"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    const body = await response!.json();
    expect(observed.input).toEqual({
      kind: "TrialContainer",
      name: "trial-1.agent.container-1",
      operation: "port-forward",
      requester: "issuer:user-a",
    });
    expect(body).toMatchObject({
      kind: "RuntimeResourceOperationReview",
      generated_at: "2026-01-01T00:00:00.000Z",
      core_run_ids: ["core-run-1"],
      supported: true,
      reason: null,
      message: null,
      subresource: "port-forward",
    });
  });

  test("decodes slash-qualified runtime operation review names as one path segment", async () => {
    const observed: {
      ownerKey?: string | undefined;
      runtimeRunId?: string;
      runtimeStatus?: string;
      kind?: string;
      name?: string;
      operation?: string;
      requester?: string | null | undefined;
    } = {};
    const runs = {
      async getRun(runId: string, ownerKey?: string) {
        observed.ownerKey = ownerKey;
        expect(runId).toBe("run-1");
        return runRecord();
      },
    };
    const runtime = {
      async reviewResourceOperation(runId: string, run: ReturnType<typeof runRecord>, input: { kind: string; name: string; operation: string; requester?: string | null | undefined }) {
        observed.runtimeRunId = runId;
        observed.runtimeStatus = run.status;
        observed.kind = input.kind;
        observed.name = input.name;
        observed.operation = input.operation;
        observed.requester = input.requester;
        return {
          apiVersion: "bucephalus.dev/v1alpha1",
          kind: "RuntimeResourceOperationReview",
          cloud_run_id: runId,
          generated_at: "2026-01-01T00:00:00.000Z",
          core_run_ids: ["core-run-1"],
          resource_ref: {
            apiVersion: "bucephalus.dev/v1alpha1",
            kind: input.kind,
            name: input.name,
          },
          resource_version: "sha256:review",
          resource_generation: 4,
          observed_generation: 4,
          operation: input.operation,
          matched_operation: input.operation,
          supported: true,
          reason: null,
          message: null,
          command: "buc runs logs run-1 Trial/trial-1 --stream stdout --follow",
          verb: "get",
          subresource: "logs",
          action: null,
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1/operations/logs%2Fstdout"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1/operations/logs%2Fstdout"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(await response!.json()).toMatchObject({
      kind: "RuntimeResourceOperationReview",
      cloud_run_id: "run-1",
      generated_at: "2026-01-01T00:00:00.000Z",
      core_run_ids: ["core-run-1"],
      resource_ref: {
        kind: "Trial",
        name: "trial-1",
      },
      operation: "logs/stdout",
      resource_version: "sha256:review",
      resource_generation: 4,
      observed_generation: 4,
      matched_operation: "logs/stdout",
      supported: true,
      subresource: "logs",
    });
    expect(observed).toEqual({
      ownerKey: "issuer:user-a",
      runtimeRunId: "run-1",
      runtimeStatus: "created",
      kind: "Trial",
      name: "trial-1",
      operation: "logs/stdout",
      requester: "issuer:user-a",
    });
  });

  test("runtime resource byte subresources expose cloud run identity headers", async () => {
    const observed: {
      ownerKeys: Array<string | undefined>;
      logs?: { runId: string; input: unknown };
      content?: { runId: string; input: unknown };
    } = { ownerKeys: [] };
    const runs = {
      async getRun(runId: string, ownerKey?: string) {
        expect(runId).toBe("run-1");
        observed.ownerKeys.push(ownerKey);
        return runRecord();
      },
    };
    const runtime = {
      async resourceLogs(runId: string, _run: ReturnType<typeof runRecord>, input: unknown) {
        observed.logs = { runId, input };
        return {
          stream: "stderr",
          resource: {
            kind: "Trial",
            metadata: { name: "trial-1", resourceVersion: "sha256:trial-rv" },
          },
          bytes: new TextEncoder().encode("log line\n"),
          media_type: "text/plain; charset=utf-8",
          object: {
            core_run_id: "core-run-1",
            trial_id: "trial-1",
            role: "stderr",
            object_ref: "runtime://cloud-run/run-1/trial/trial-1/stderr",
            media_type: "text/plain; charset=utf-8",
            sha256: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          },
        };
      },
      async resourceArtifactContent(runId: string, _run: ReturnType<typeof runRecord>, input: unknown) {
        observed.content = { runId, input };
        return {
          resource: {
            kind: "TrialArtifact",
            metadata: { name: "artifact-1", resourceVersion: "sha256:artifact-rv" },
          },
          bytes: new TextEncoder().encode("{\"ok\":true}"),
          media_type: "application/json; charset=utf-8",
          object: {
            core_run_id: "core-run-1",
            trial_id: "trial-1",
            role: "agent_result",
            object_ref: "artifact://sha256/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            media_type: "application/json; charset=utf-8",
            sha256: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          },
        };
      },
    };

    const logsResponse = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1/logs?stream=stderr"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1/logs?stream=stderr"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(logsResponse).not.toBeNull();
    expect(await logsResponse!.text()).toBe("log line\n");
    expect(logsResponse!.headers.get("x-bucephalus-run-id")).toBe("run-1");
    expect(logsResponse!.headers.get("x-bucephalus-log-stream")).toBe("stderr");
    expect(logsResponse!.headers.get("x-bucephalus-resource-kind")).toBe("Trial");
    expect(logsResponse!.headers.get("x-bucephalus-resource-name")).toBe("trial-1");
    expect(logsResponse!.headers.get("x-bucephalus-resource-version")).toBe("sha256:trial-rv");
    expect(logsResponse!.headers.get("x-bucephalus-core-run-id")).toBe("core-run-1");
    expect(logsResponse!.headers.get("x-bucephalus-trial-id")).toBe("trial-1");
    expect(logsResponse!.headers.get("x-bucephalus-artifact-role")).toBe("stderr");
    expect(logsResponse!.headers.get("x-bucephalus-object-ref")).toBe("runtime://cloud-run/run-1/trial/trial-1/stderr");
    expect(logsResponse!.headers.get("x-bucephalus-artifact-sha256")).toBe("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    const contentResponse = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/TrialArtifact/artifact-1/content"),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/TrialArtifact/artifact-1/content"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(contentResponse).not.toBeNull();
    expect(await contentResponse!.text()).toBe("{\"ok\":true}");
    expect(contentResponse!.headers.get("x-bucephalus-run-id")).toBe("run-1");
    expect(contentResponse!.headers.get("x-bucephalus-resource-kind")).toBe("TrialArtifact");
    expect(contentResponse!.headers.get("x-bucephalus-resource-name")).toBe("artifact-1");
    expect(contentResponse!.headers.get("x-bucephalus-resource-version")).toBe("sha256:artifact-rv");
    expect(contentResponse!.headers.get("x-bucephalus-core-run-id")).toBe("core-run-1");
    expect(contentResponse!.headers.get("x-bucephalus-trial-id")).toBe("trial-1");
    expect(contentResponse!.headers.get("x-bucephalus-artifact-role")).toBe("agent_result");
    expect(contentResponse!.headers.get("x-bucephalus-object-ref")).toBe("artifact://sha256/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    expect(contentResponse!.headers.get("x-bucephalus-artifact-sha256")).toBe("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    expect(observed).toEqual({
      ownerKeys: ["issuer:user-a", "issuer:user-a"],
      logs: {
        runId: "run-1",
        input: {
          kind: "Trial",
          name: "trial-1",
          stream: "stderr",
          tailLines: undefined,
          requester: "issuer:user-a",
        },
      },
      content: {
        runId: "run-1",
        input: {
          kind: "TrialArtifact",
          name: "artifact-1",
          requester: "issuer:user-a",
        },
      },
    });
  });

  test("runtime resource API creates port-forward requests from the selected resource path", async () => {
    const observed: { ownerKey?: string | undefined; input?: unknown } = {};
    const runs = {
      async getRun(runId: string, ownerKey?: string) {
        observed.ownerKey = ownerKey;
        expect(runId).toBe("run-1");
        return runRecord();
      },
    };
    const runtime = {
      async createPortForwardRequest(input: unknown) {
        observed.input = input;
        return runtimeAccessRequestRecord("pf-1", "port_forward");
      },
      runtimeAccessRequestResource(run: { run_id: string }, request: { access_request_id: string; kind: string }) {
        return runtimeAccessResource(run.run_id, request);
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1.0/port-forward", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          target_port: "8080",
          local_port: 18080,
          resource_version: "rv-1",
          reason: "debug failing service",
        }),
      }),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1.0/port-forward"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    const body = await response!.json();
    expect(response!.status).toBe(201);
    expect(observed.ownerKey).toBe("issuer:user-a");
    expect(observed.input).toMatchObject({
      run: expect.objectContaining({ run_id: "run-1" }),
      targetPort: 8080,
      localPort: 18080,
      resourceKind: "Trial",
      resourceName: "trial-1.0",
      resourceVersion: "rv-1",
      requester: "issuer:user-a",
      reason: "debug failing service",
    });
    expect(body.resource.kind).toBe("PortForward");
  });

  test("runtime resource API creates exec requests from the selected resource path", async () => {
    const observed: { input?: unknown } = {};
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async createExecRequest(input: unknown) {
        observed.input = input;
        return runtimeAccessRequestRecord("exec-1", "exec");
      },
      runtimeAccessRequestResource(run: { run_id: string }, request: { access_request_id: string; kind: string }) {
        return runtimeAccessResource(run.run_id, request);
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/RunnerInstance/runner-1/exec", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          command: ["bash", "-lc", "id"],
          ttl_seconds: 60,
          resource_version: "rv-runner-1",
        }),
      }),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/RunnerInstance/runner-1/exec"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    const body = await response!.json();
    expect(response!.status).toBe(201);
    expect(observed.input).toMatchObject({
      run: expect.objectContaining({ run_id: "run-1" }),
      command: ["bash", "-lc", "id"],
      resourceKind: "RunnerInstance",
      resourceName: "runner-1",
      resourceVersion: "rv-runner-1",
      ttlSeconds: 60,
      requester: "issuer:user-a",
    });
    expect(body.resource.kind).toBe("Exec");
  });

  test("runtime resource API performs lifecycle actions through action subresources", async () => {
    const observed: { input?: unknown } = {};
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async cordonRunnerInstanceResource(input: unknown) {
        observed.input = input;
        return { resource: { kind: "RunnerInstance", metadata: { name: "runner-1" } } };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/RunnerInstance/runner-1/actions/cordon", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          resource_version: "rv-2",
          reason: "drain for inspection",
        }),
      }),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/RunnerInstance/runner-1/actions/cordon"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(await response!.json()).toEqual({ resource: { kind: "RunnerInstance", metadata: { name: "runner-1" } } });
    expect(observed.input).toMatchObject({
      run: expect.objectContaining({ run_id: "run-1" }),
      resourceKind: "RunnerInstance",
      resourceName: "runner-1",
      resourceVersion: "rv-2",
      requester: "issuer:user-a",
      reason: "drain for inspection",
    });
  });

  test("runtime resource API deletes access resources through the selected resource", async () => {
    const observed: { input?: unknown } = {};
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async cancelRuntimeAccessResource(input: unknown) {
        observed.input = input;
        return { resource: { kind: "PortForward", metadata: { name: "pf-1" } } };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/PortForward/pf-1", {
        method: "DELETE",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          resource_version: "rv-pf-1",
          reason: "done debugging",
        }),
      }),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/PortForward/pf-1"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(await response!.json()).toEqual({ resource: { kind: "PortForward", metadata: { name: "pf-1" } } });
    expect(observed.input).toMatchObject({
      run: expect.objectContaining({ run_id: "run-1" }),
      resourceKind: "PortForward",
      resourceName: "pf-1",
      resourceVersion: "rv-pf-1",
      requester: "issuer:user-a",
      reason: "done debugging",
    });
  });

  test("runtime resource API cancels access resources through the cancel action subresource", async () => {
    const observed: { input?: unknown } = {};
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async cancelRuntimeAccessResource(input: unknown) {
        observed.input = input;
        return { resource: { kind: "Exec", metadata: { name: "exec-1" } } };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/Exec/exec-1/actions/cancel", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          resource_version: "rv-exec-1",
          reason: "stop debug command",
        }),
      }),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/Exec/exec-1/actions/cancel"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(await response!.json()).toEqual({ resource: { kind: "Exec", metadata: { name: "exec-1" } } });
    expect(observed.input).toMatchObject({
      run: expect.objectContaining({ run_id: "run-1" }),
      resourceKind: "Exec",
      resourceName: "exec-1",
      resourceVersion: "rv-exec-1",
      requester: "issuer:user-a",
      reason: "stop debug command",
    });
  });

  test("runtime resource API completes port-forwards through the complete action subresource", async () => {
    const observed: { input?: unknown } = {};
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async completeRuntimeAccessResource(input: unknown) {
        observed.input = input;
        return { resource: { kind: "PortForward", metadata: { name: "pf-1" } } };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs/run-1/runtime/resources/PortForward/pf-1/actions/complete", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          resource_version: "rv-pf-1",
          reason: "local tunnel ended",
        }),
      }),
      new URL("https://cloud.example/v1/runs/run-1/runtime/resources/PortForward/pf-1/actions/complete"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(await response!.json()).toEqual({ resource: { kind: "PortForward", metadata: { name: "pf-1" } } });
    expect(observed.input).toMatchObject({
      run: expect.objectContaining({ run_id: "run-1" }),
      resourceKind: "PortForward",
      resourceName: "pf-1",
      resourceVersion: "rv-pf-1",
      requester: "issuer:user-a",
      reason: "local tunnel ended",
    });
  });

  test("runtime resource API requires reviewed resource versions before mutations", async () => {
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async createPortForwardRequest() {
        throw new Error("port-forward should not be created without a reviewed resource version");
      },
      async createExecRequest() {
        throw new Error("exec should not be created without a reviewed resource version");
      },
      async cordonRunnerInstanceResource() {
        throw new Error("cordon should not be performed without a reviewed resource version");
      },
      async cancelRuntimeAccessResource() {
        throw new Error("delete should not be performed without a reviewed resource version");
      },
      async completeRuntimeAccessResource() {
        throw new Error("complete should not be performed without a reviewed resource version");
      },
    };
    const cases = [
      {
        operation: "port-forward",
        request: new Request("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1.0/port-forward", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ target_port: 8080 }),
        }),
        url: new URL("https://cloud.example/v1/runs/run-1/runtime/resources/Trial/trial-1.0/port-forward"),
      },
      {
        operation: "exec",
        request: new Request("https://cloud.example/v1/runs/run-1/runtime/resources/RunnerInstance/runner-1/exec", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ command: ["bash", "-lc", "id"] }),
        }),
        url: new URL("https://cloud.example/v1/runs/run-1/runtime/resources/RunnerInstance/runner-1/exec"),
      },
      {
        operation: "cordon",
        request: new Request("https://cloud.example/v1/runs/run-1/runtime/resources/RunnerInstance/runner-1/actions/cordon", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ reason: "drain for inspection" }),
        }),
        url: new URL("https://cloud.example/v1/runs/run-1/runtime/resources/RunnerInstance/runner-1/actions/cordon"),
      },
      {
        operation: "delete",
        request: new Request("https://cloud.example/v1/runs/run-1/runtime/resources/PortForward/pf-1", {
          method: "DELETE",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ reason: "done debugging" }),
        }),
        url: new URL("https://cloud.example/v1/runs/run-1/runtime/resources/PortForward/pf-1"),
      },
      {
        operation: "complete",
        request: new Request("https://cloud.example/v1/runs/run-1/runtime/resources/PortForward/pf-1/actions/complete", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ reason: "local tunnel ended" }),
        }),
        url: new URL("https://cloud.example/v1/runs/run-1/runtime/resources/PortForward/pf-1/actions/complete"),
      },
    ];

    for (const testCase of cases) {
      await expect(handleRunRoute(
        testCase.request,
        testCase.url,
        {} as PackageRepository,
        runs as unknown as RunRepository,
        runtime as unknown as RuntimeRepository,
        runnersWithDockerPool() as any,
        "worker-token",
        authContext("user-a"),
      )).rejects.toMatchObject({
        status: 428,
        code: "runtime_resource_version_required",
        detail: {
          operation: testCase.operation,
          required: ["resource_version"],
        },
      });
    }
  });

  test("lists only runs for the authenticated owner", async () => {
    const observed: { ownerKey?: string | undefined } = {};
    const runs = {
      async listRuns(input: { ownerKey?: string | undefined }) {
        observed.ownerKey = input.ownerKey;
        return [];
      },
    };
    const packages = {
      async listArtifactsByDigests() {
        throw new Error("listArtifactsByDigests should not be called for empty run lists");
      },
    };
    const runtime = {
      async trialProgressForCloudRuns() {
        throw new Error("trialProgressForCloudRuns should not be called for empty run lists");
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs"),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(observed.ownerKey).toBe("issuer:user-a");
  });

  test("filters run lists by package digest before enrichment", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const observed: { ownerKey?: string | undefined; packageDigest?: string | undefined; limit?: number | undefined } = {};
    const runs = {
      async listRuns(input: { ownerKey?: string | undefined; packageDigest?: string | undefined; limit?: number | undefined }) {
        observed.ownerKey = input.ownerKey;
        observed.packageDigest = input.packageDigest;
        observed.limit = input.limit;
        return [];
      },
    };
    const packages = {
      async listArtifactsByDigests() {
        throw new Error("listArtifactsByDigests should not be called for empty run lists");
      },
    };
    const runtime = {
      async trialProgressForCloudRuns() {
        throw new Error("trialProgressForCloudRuns should not be called for empty run lists");
      },
    };

    const response = await handleRunRoute(
      new Request(`https://cloud.example/v1/runs?package_digest=${encodeURIComponent(digest)}&limit=12`),
      new URL(`https://cloud.example/v1/runs?package_digest=${encodeURIComponent(digest)}&limit=12`),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      runtime as unknown as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(observed).toEqual({
      ownerKey: "issuer:user-a",
      packageDigest: digest,
      limit: 12,
    });
  });

  test("creates runs only from packages owned by the authenticated owner", async () => {
    const observed: { packageOwnerKey?: string | undefined; runOwnerKey?: string | null | undefined } = {};
    const packages = {
      async getArtifact(_digest: string, ownerKey?: string) {
        observed.packageOwnerKey = ownerKey;
        return {
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          upload_id: "upload-1",
          storage_path: "/tmp/package.tgz",
          byte_size: 1,
          media_type: "application/gzip",
          manifest_json: {
            schema_version: "sealed_run_package_v2",
            resolved_experiment: {
              runtime: {
                compute: { backend: "local-docker" },
              },
            },
          },
          resolved_experiment_json: {},
          target: null,
          image_refs: [],
          diagnostics: [],
          package_provenance: packageProvenance(),
          status: "accepted",
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        };
      },
    };
    const runs = {
      async createRun(input: { ownerKey?: string | null | undefined }) {
        observed.runOwnerKey = input.ownerKey;
        return runRecord();
      },
    };

    await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-b"),
    );

    expect(observed.packageOwnerKey).toBe("issuer:user-b");
    expect(observed.runOwnerKey).toBe("issuer:user-b");
  });

  test("package responses carry the resolved experiment name", async () => {
    const packages = {
      async getArtifact() {
        const record = packageRecordWithSecrets();
        return {
          ...record,
          resolved_experiment_json: {
            ...record.resolved_experiment_json,
            experiment: { name: "Peter Gregory v2 State-Only Cloud Demo (pg_024)" },
          },
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
      new URL("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
      packages as unknown as PackageRepository,
      {} as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.name).toBe("Peter Gregory v2 State-Only Cloud Demo (pg_024)");
  });

  test("package inspection rejects malformed package digests before artifact lookup", async () => {
    const packages = {
      async getArtifact() {
        throw new Error("getArtifact should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/packages/sha256:NOTHEX"),
      new URL("https://cloud.example/v1/packages/sha256:NOTHEX"),
      packages as unknown as PackageRepository,
      {} as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toMatchObject({
      status: 400,
      code: "invalid_request",
      message: "/package_digest must be sha256:<64 lowercase hex chars>",
    });
  });

  test("package responses expose declared secret requirements without values", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
      new URL("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
      packages as unknown as PackageRepository,
      {} as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.secret_requirements).toEqual([
      {
        id: "OPENAI_API_KEY",
        target: "/run/secrets/openai",
        required_for_variants: [],
      },
    ]);
    expect(body.package_provenance).toEqual(packageProvenance());
    expect(JSON.stringify(body)).not.toContain("sk-");
  });

  test("run creation rejects missing package secret refs before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("Run secret refs must match");
  });

  test("run creation rejects malformed package digests before package lookup", async () => {
    const packages = {
      async getArtifact() {
        throw new Error("getArtifact should not be called");
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:short",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toMatchObject({
      status: 400,
      code: "invalid_request",
      message: "/package_digest must be sha256:<64 lowercase hex chars>",
    });
  });

  test("run creation accepts env-only package secret refs", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithEnvSecret();
      },
    };
    const observed: { secretRefs?: Record<string, string>; secretIds?: string[]; packageProvenance?: unknown } = {};
    const runs = {
      async createRun(input: { secretRefs: Record<string, string>; runRequirements: { secret_ids: string[] }; packageProvenance: unknown }) {
        observed.secretRefs = input.secretRefs;
        observed.secretIds = input.runRequirements.secret_ids;
        observed.packageProvenance = input.packageProvenance;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          secret_refs: {
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.secretRefs).toEqual({
      OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
    });
    expect(observed.secretIds).toEqual(["OPENAI_API_KEY"]);
    expect(observed.packageProvenance).toEqual(packageProvenance());
    expect((await response!.json()).package_provenance).toEqual(packageProvenance());
  });

  test("run creation translates hosted bucephalus:// refs to backing provider refs", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const observed: { secretRefs?: Record<string, string> } = {};
    const runs = {
      async createRun(input: { secretRefs: Record<string, string> }) {
        observed.secretRefs = input.secretRefs;
        return runRecord();
      },
    };
    const secrets = {
      async getSecret(ownerKey: string, name: string) {
        expect(ownerKey).toBe("issuer:user-a");
        expect(name).toBe("OPENAI_API_KEY");
        return {
          secret_id: "secret-1",
          owner_key: ownerKey,
          name,
          store_name: "buc-abc-OPENAI_API_KEY",
          backing_ref: "gcp-secret-manager://projects/bucephalus-prod/secrets/buc-abc-OPENAI_API_KEY/versions/7",
          version: 1,
          created_at: "2026-06-11T00:00:00Z",
          updated_at: "2026-06-11T00:00:00Z",
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          secret_refs: {
            OPENAI_API_KEY: "bucephalus://OPENAI_API_KEY",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
      secrets as unknown as CloudSecretRepository,
    );

    expect(response!.status).toBe(201);
    expect(observed.secretRefs).toEqual({
      OPENAI_API_KEY: "gcp-secret-manager://projects/bucephalus-prod/secrets/buc-abc-OPENAI_API_KEY/versions/7",
    });
  });

  test("run creation rejects invalid plain env names before queueing", async () => {
    const packages = {
      async getArtifact() {
        throw new Error("getArtifact should not be called");
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          env: {
            "bad-name": "1",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("invalid environment variable name 'bad-name'");
  });

  test("run creation rejects malformed env maps instead of treating them as empty", async () => {
    const packages = {
      async getArtifact() {
        throw new Error("getArtifact should not be called");
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          env: "PUBLIC_MODE=smoke",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("/env must be an object");
  });

  test("run creation rejects malformed secret ref maps instead of treating them as empty", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          secret_refs: "OPENAI_API_KEY=bucephalus://OPENAI_API_KEY",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("/secret_refs must be an object");
  });

  test("run creation rejects env names reserved for Cloud runtime state", async () => {
    const packages = {
      async getArtifact() {
        throw new Error("getArtifact should not be called");
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          env: {
            DATABASE_URL: "postgres://user-db",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("reserved for Cloud runtime/control-plane state");
  });

  test("run creation rejects plain env keys that collide with secret refs", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          env: {
            OPENAI_API_KEY: "not-a-secret",
          },
          secret_refs: {
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("cannot also be supplied in /secret_refs");
  });

  test("run creation rejects runtime region before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          runtime_options: {
            region: "us-east-1",
          },
          secret_refs: {
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("/runtime_options/region is not supported for hosted Cloud runs");
  });

  test("run creation rejects runtime compatibility aliases before queueing", async () => {
    for (const [key, value] of [
      ["executor", "modal"],
      ["cpu", 2],
    ] as const) {
      const packages = {
        async getArtifact() {
          return packageRecordWithSecrets();
        },
      };
      const runs = {
        async createRun() {
          throw new Error("createRun should not be called");
        },
      };

      await expect(handleRunRoute(
        new Request("https://cloud.example/v1/runs", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            runtime_options: {
              [key]: value,
            },
            secret_refs: {
              OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
            },
          }),
        }),
        new URL("https://cloud.example/v1/runs"),
        packages as unknown as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        runnersWithDockerPool() as any,
        "worker-token",
        authContext("user-a"),
      )).rejects.toThrow(`/runtime_options/${key} is not supported for hosted Cloud runs`);
    }
  });

  test("run creation persists validated plain env separately from secret refs", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const observed: { env?: Record<string, string>; secretRefs?: Record<string, string> } = {};
    const runs = {
      async createRun(input: { env: Record<string, string>; secretRefs: Record<string, string> }) {
        observed.env = input.env;
        observed.secretRefs = input.secretRefs;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          env: {
            PUBLIC_MODE: "smoke",
          },
          secret_refs: {
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    );

    expect(response!.status).toBe(201);
    expect(observed.env).toEqual({ PUBLIC_MODE: "smoke" });
    expect(observed.secretRefs).toEqual({
      OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
    });
  });

  test("run creation rejects hosted refs that name no uploaded secret", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };
    const secrets = {
      async getSecret() {
        return null;
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          secret_refs: {
            OPENAI_API_KEY: "bucephalus://OPENAI_API_KEY",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
      secrets as unknown as CloudSecretRepository,
    )).rejects.toThrow("buc secrets put OPENAI_API_KEY --from-env OPENAI_API_KEY");
  });

  test("run creation rejects requirements that no active runner pool can satisfy", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          runtime_options: {
            backend: "modal",
          },
          secret_refs: {
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPool() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("No active runner pool can satisfy this run");
  });

  test("run creation rejects matching pools without an active worker image", async () => {
    const packages = {
      async getArtifact() {
        return {
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          upload_id: "upload-1",
          storage_path: "/tmp/package.tgz",
          byte_size: 1,
          media_type: "application/gzip",
          manifest_json: {
            schema_version: "sealed_run_package_v2",
            resolved_experiment: {
              runtime: {
                compute: { backend: "local-docker" },
              },
            },
          },
          resolved_experiment_json: {},
          target: null,
          image_refs: [],
          diagnostics: [],
          status: "accepted",
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        };
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      runnersWithDockerPoolWithoutWorkerImage() as any,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("has no active worker image promoted");
  });
});

function runnersWithDockerPool(): Pick<RunnerRepository, "listPools" | "listInstances"> {
  return {
    async listPools() {
      return [
        {
          runner_pool_id: "pool-1",
          name: "docker",
          status: "active",
          active_worker_image_id: "worker-image-1",
          capabilities: {
            executors: ["runner-docker"],
            resources: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver"],
            arch: "x86_64",
            cpu_count: 4,
            memory_mb: 8192,
            disk_mb: 65536,
            isolation: ["reusable_vm"],
          },
          metadata: {},
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        },
      ];
    },
    async listInstances() {
      return [
        {
          runner_instance_id: "runner-instance-1",
          runner_pool_id: "pool-1",
          instance_name: "runner-1",
          status: "online",
          capabilities: {
            executors: ["runner-docker"],
            resources: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver"],
            arch: "x86_64",
            cpu_count: 4,
            memory_mb: 8192,
            disk_mb: 65536,
            isolation: ["reusable_vm"],
          },
          metadata: {},
          last_heartbeat_at: "2026-06-04T00:00:00Z",
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        },
      ];
    },
  };
}

function runnersWithDockerPoolWithoutWorkerImage(): Pick<RunnerRepository, "listPools"> {
  return {
    async listPools() {
      const pool = (await runnersWithDockerPool().listPools())[0]!;
      return [{ ...pool, active_worker_image_id: null }];
    },
  };
}

function runRecord() {
  return {
    run_id: "run-1",
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    run_label: "security",
    status: "created",
    env: {
      PUBLIC_FLAG: "1",
      SENSITIVE_ENV: "secret-env-value",
    },
    secret_refs: {
      OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
    },
    runtime_options: {},
    run_requirements: {
      executor: "runner-docker",
      requires: [],
      image_refs: [],
      secret_ids: ["OPENAI_API_KEY"],
      network_perimeter: {
        default: "none",
        task_sandbox: "none",
        agent: "none",
        egress_hosts: [],
      },
      sidecars: [],
      accelerators: [],
      arch: "x86_64",
      cpu_count: 1,
      memory_mb: 1024,
      disk_mb: 20480,
      isolation: "reusable_vm",
      timeout_ms: null,
      max_parallel_trials: 1,
    },
    package_provenance: packageProvenance(),
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
    started_at: null,
    completed_at: null,
    error_message: null,
  };
}

function attemptRecord(attemptToken?: string): RunAttemptRecord {
  return {
    attempt_id: "attempt-1",
    run_id: "run-1",
    worker_id: "runner-instance-1",
    runner_instance_id: "runner-instance-1",
    status: "running",
    lease_expires_at: "2026-06-04T00:01:00Z",
    heartbeat_at: "2026-06-04T00:00:00Z",
    started_at: "2026-06-04T00:00:00Z",
    ended_at: null,
    error_message: null,
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
    ...(attemptToken ? { attempt_token: attemptToken } : {}),
  };
}

function runtimeAccessRequestRecord(accessRequestId: string, kind: "port_forward" | "exec") {
  return {
    access_request_id: accessRequestId,
    run_id: "run-1",
    kind,
    status: "requested",
    resource_kind: kind === "exec" ? "RunnerInstance" : "Trial",
    resource_name: kind === "exec" ? "runner-1" : "trial-1.0",
    target_uid: null,
    target_resource_version: null,
    protocol: kind === "exec" ? "exec" : "tcp",
    target_port: kind === "exec" ? null : 8080,
    local_port: null,
    command: kind === "exec" ? ["bash", "-lc", "id"] : null,
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
  };
}

function runtimeAccessResource(runId: string, request: { access_request_id: string; kind: string }) {
  const kind = request.kind === "exec" ? "Exec" : "PortForward";
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind,
    metadata: {
      name: request.access_request_id,
      labels: {
        "bucephalus.dev/cloud-run-id": runId,
      },
    },
    spec: {},
    status: {},
  };
}

function runtimeRouteResource(
  kind: string,
  name: string,
  overrides: {
    labels?: Record<string, string>;
    annotations?: Record<string, string>;
    spec?: Record<string, unknown>;
    status?: Record<string, unknown>;
  } = {},
) {
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind,
    metadata: {
      name,
      uid: `${kind.toLowerCase()}-${name}`,
      generation: 1,
      resourceVersion: "sha256:resource",
      labels: overrides.labels ?? {},
      annotations: overrides.annotations ?? {},
      ownerReferences: [],
    },
    spec: overrides.spec ?? {},
    status: overrides.status ?? {},
    audit: {},
  };
}

function packageRecordWithSecrets() {
  return {
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    upload_id: "upload-1",
    storage_path: "/tmp/package.tgz",
    byte_size: 1,
    media_type: "application/gzip",
    manifest_json: {},
    resolved_experiment_json: {
      runtime: {
        secrets: [
          {
            name: "OPENAI_API_KEY",
            mount: {
              target: "/run/secrets/openai",
            },
          },
        ],
      },
    },
    target: null,
    image_refs: [],
    diagnostics: [],
    package_provenance: packageProvenance(),
    status: "accepted",
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
  };
}

function packageRecordWithEnvSecret() {
  return {
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    upload_id: "upload-1",
    storage_path: "/tmp/package.tgz",
    byte_size: 1,
    media_type: "application/gzip",
    manifest_json: {},
    resolved_experiment_json: {
      runtime: {
        secrets: [
          {
            name: "OPENAI_API_KEY",
            from: "env",
          },
        ],
      },
    },
    target: null,
    image_refs: [],
    diagnostics: [],
    package_provenance: packageProvenance(),
    status: "accepted",
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
  };
}

function packageProvenance() {
  return {
    schema_version: "cloud_package_provenance_v1",
    status: "hosted_attested",
    source: "hosted_core",
    message: "Cloud ran hosted Core authoring for this package and recorded the builder/core environment.",
  };
}

function packagesByDigest(artifacts: Array<ReturnType<typeof packageRecordWithSecrets>>) {
  return {
    async listArtifactsByDigests(packageDigests: string[], ownerKey?: string) {
      expect(ownerKey).toBeUndefined();
      const requested = new Set(packageDigests);
      return artifacts.filter((artifact) => requested.has(artifact.package_digest));
    },
  };
}

function runtimeProgress(progress: Array<{ cloud_run_id: string; trials_completed: number | null; trials_total?: number | null }>) {
  return {
    async trialProgressForCloudRuns(runIds: string[]) {
      const requested = new Set(runIds);
      return progress
        .filter((item) => requested.has(item.cloud_run_id))
        .map((item) => ({
          cloud_run_id: item.cloud_run_id,
          trials_completed: item.trials_completed,
          trials_total: item.trials_total ?? null,
        }));
    },
  };
}

function authContext(subject: string): AuthContext {
  return {
    subject,
    issuer: "issuer",
    audience: "audience",
    claims: {
      sub: subject,
      iss: "issuer",
      aud: "audience",
    },
  };
}

function captureEnv(keys: string[]): Record<string, string | undefined> {
  return Object.fromEntries(keys.map((key) => [key, process.env[key]]));
}

function restoreEnv(previous: Record<string, string | undefined>): void {
  for (const [key, value] of Object.entries(previous)) {
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
}
