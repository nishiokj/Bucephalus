import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import { handleRunRoute } from "../src/routes/runs";
import type { AuthContext } from "../src/auth";
import type { PackageRepository, RunAttemptRecord, RunRepository } from "../src/packages/repository";
import type { RuntimeRepository } from "../src/runtime/repository";

describe("Cloud run routes", () => {
  test("redacts env values and secret refs from user-facing run list responses", async () => {
    const runs = {
      async listRuns() {
        return [runRecord()];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs"),
      new URL("https://cloud.example/v1/runs"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
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
      "worker-token",
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.attempt.attempt_token).toBe("attempt-token");
    expect(body.run.env.SENSITIVE_ENV).toBe("secret-env-value");
    expect(body.run.secret_refs.OPENAI_API_KEY).toBe("gcp-secret-manager://projects/acme/secrets/openai/versions/1");
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
      const observed: { token?: string; attemptId?: string; packageDigest?: string | null | undefined } = {};
      const packages = {
        async getArtifact() {
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
        "worker-token",
      );

      expect(response).not.toBeNull();
      expect(await response!.text()).toBe("package bytes");
      expect(observed).toEqual({
        token: "attempt-token",
        attemptId: "attempt-1",
        packageDigest: digest,
      });
    } finally {
      await rm(root, { recursive: true, force: true });
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

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs"),
      new URL("https://cloud.example/v1/runs"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
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
      "worker-token",
      authContext("user-b"),
    );

    expect(observed.packageOwnerKey).toBe("issuer:user-b");
    expect(observed.runOwnerKey).toBe("issuer:user-b");
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
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("Run secret refs must match");
  });

  test("run creation accepts env-only package secret refs", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithEnvSecret();
      },
    };
    const observed: { secretRefs?: Record<string, string>; secretIds?: string[] } = {};
    const runs = {
      async createRun(input: { secretRefs: Record<string, string>; runRequirements: { secret_ids: string[] } }) {
        observed.secretRefs = input.secretRefs;
        observed.secretIds = input.runRequirements.secret_ids;
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
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.secretRefs).toEqual({
      OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
    });
    expect(observed.secretIds).toEqual(["OPENAI_API_KEY"]);
  });
});

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
    status: "accepted",
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
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
