import { mkdtemp, rm, writeFile } from "node:fs/promises";
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
