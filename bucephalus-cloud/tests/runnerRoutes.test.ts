import { describe, expect, test } from "bun:test";
import { handleRunnerRoute } from "../src/routes/runners";
import { HttpError } from "../src/http";
import type { RunnerRepository } from "../src/runners/repository";

const RUNNER_POOL_ID = "11111111-1111-4111-8111-111111111111";
const RUNNER_INSTANCE_ID = "22222222-2222-4222-8222-222222222222";

describe("runner management routes", () => {
  test("requires the runner admin token for pool management when configured", async () => {
    const runners = {
      async listPools() {
        return [];
      },
    };

    await expect(handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-pools", {
        headers: {
          authorization: "Bearer worker-token",
        },
      }),
      new URL("https://cloud.example/v1/runner-pools"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    )).rejects.toThrow("runner pool management requires a valid worker token");

    const response = await handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-pools", {
        headers: {
          authorization: "Bearer admin-token",
        },
      }),
      new URL("https://cloud.example/v1/runner-pools"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    );

    expect(response).not.toBeNull();
    expect(await response!.json()).toEqual({ runner_pools: [] });
  });

  test("still accepts the worker token for runner registration", async () => {
    const runners = {
      async registerInstance() {
        return {
          runner_instance_id: RUNNER_INSTANCE_ID,
          runner_pool_id: RUNNER_POOL_ID,
          instance_name: "vm-1",
          status: "online",
          capabilities: { executors: [], resources: [] },
          metadata: {},
          last_heartbeat_at: "2026-06-04T00:00:00Z",
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        };
      },
    };

    const response = await handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-instances/register", {
        method: "POST",
        headers: {
          authorization: "Bearer worker-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_pool_id: RUNNER_POOL_ID,
          instance_name: "vm-1",
          capabilities: {},
          metadata: {},
        }),
      }),
      new URL("https://cloud.example/v1/runner-instances/register"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.runner_instance_id).toBe(RUNNER_INSTANCE_ID);
  });

  test("normalizes runner executor capabilities during registration", async () => {
    const observed: { executors?: string[] } = {};
    const runners = {
      async registerInstance(input: { capabilities: { executors: string[] } }) {
        observed.executors = input.capabilities.executors;
        return {
          runner_instance_id: RUNNER_INSTANCE_ID,
          runner_pool_id: RUNNER_POOL_ID,
          instance_name: "vm-1",
          status: "online",
          capabilities: input.capabilities,
          metadata: {},
          last_heartbeat_at: "2026-06-04T00:00:00Z",
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        };
      },
    };

    const response = await handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-instances/register", {
        method: "POST",
        headers: {
          authorization: "Bearer worker-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_pool_id: RUNNER_POOL_ID,
          instance_name: "vm-1",
          capabilities: {
            executors: ["runner_docker", "local-docker", "modal"],
            resources: ["core_runner"],
          },
          metadata: {},
        }),
      }),
      new URL("https://cloud.example/v1/runner-instances/register"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    );

    expect(response).not.toBeNull();
    expect(observed.executors).toEqual(["modal", "runner-docker"]);
  });

  test("rejects malformed runner pool ids during registration before repository calls", async () => {
    let registerCalls = 0;
    const runners = {
      async registerInstance() {
        registerCalls += 1;
        return null;
      },
    };

    let caught: unknown;
    try {
      await handleRunnerRoute(
        new Request("https://cloud.example/v1/runner-instances/register", {
          method: "POST",
          headers: {
            authorization: "Bearer worker-token",
            "content-type": "application/json",
          },
          body: JSON.stringify({
            runner_pool_id: "token=raw-register-pool-secret /Users/alice/private/pool",
            instance_name: "vm-1",
            capabilities: {},
            metadata: {},
          }),
        }),
        new URL("https://cloud.example/v1/runner-instances/register"),
        runners as unknown as RunnerRepository,
        {
          workerToken: "worker-token",
          adminToken: "admin-token",
        },
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    const text = JSON.stringify({ message: error.message, detail: error.detail });
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_request");
    expect(error.message).toBe("/runner_pool_id must be a UUID");
    expect(text).not.toContain("raw-register-pool-secret");
    expect(text).not.toContain("/Users/alice");
    expect(registerCalls).toBe(0);
  });

  test("rejects invalid runner list limits before listing", async () => {
    const calls: string[] = [];
    const runners = {
      async listInstances() {
        calls.push("instances");
        return [];
      },
      async listProvisionRequests() {
        calls.push("provision_requests");
        return [];
      },
    };

    for (const path of [
      "/v1/runner-instances?limit=501",
      "/v1/runner-provision-requests?limit=token=raw-runner-limit",
    ]) {
      await expect(handleRunnerRoute(
        new Request(`https://cloud.example${path}`, {
          headers: {
            authorization: "Bearer admin-token",
          },
        }),
        new URL(`https://cloud.example${path}`),
        runners as unknown as RunnerRepository,
        {
          workerToken: "worker-token",
          adminToken: "admin-token",
        },
      )).rejects.toThrow("limit must be an integer from 1 to 500");
    }
    expect(calls).toEqual([]);
  });

  test("validates runner pool query filters before listing", async () => {
    const calls: Array<{ route: string; runnerPoolId?: string; limit?: number }> = [];
    const runners = {
      async listInstances(input: { runnerPoolId?: string; limit?: number }) {
        calls.push({ route: "instances", ...input });
        return [];
      },
      async listProvisionRequests(input: { runnerPoolId?: string; limit?: number }) {
        calls.push({ route: "provision_requests", ...input });
        return [];
      },
    };

    for (const path of [
      `/v1/runner-instances?runner_pool_id=${RUNNER_POOL_ID.toUpperCase()}`,
      `/v1/runner-provision-requests?runner_pool_id=${RUNNER_POOL_ID}`,
    ]) {
      const response = await handleRunnerRoute(
        new Request(`https://cloud.example${path}`, {
          headers: {
            authorization: "Bearer admin-token",
          },
        }),
        new URL(`https://cloud.example${path}`),
        runners as unknown as RunnerRepository,
        {
          workerToken: "worker-token",
          adminToken: "admin-token",
        },
      );
      expect(response).not.toBeNull();
    }

    expect(calls).toEqual([
      { route: "instances", runnerPoolId: RUNNER_POOL_ID, limit: 100 },
      { route: "provision_requests", runnerPoolId: RUNNER_POOL_ID, limit: 100 },
    ]);

    for (const [path, rawSecret] of [
      [
        "/v1/runner-instances?runner_pool_id=token%3Draw-runner-pool-query-secret",
        "raw-runner-pool-query-secret",
      ],
      [
        "/v1/runner-provision-requests?runner_pool_id=%2FUsers%2Falice%2Fprivate%2Fpool",
        "/Users/alice",
      ],
      [
        "/v1/runner-instances?runner_pool_id=",
        "raw-empty-runner-pool-secret",
      ],
    ] as const) {
      let caught: unknown;
      try {
        await handleRunnerRoute(
          new Request(`https://cloud.example${path}`, {
            headers: {
              authorization: "Bearer admin-token",
            },
          }),
          new URL(`https://cloud.example${path}`),
          runners as unknown as RunnerRepository,
          {
            workerToken: "worker-token",
            adminToken: "admin-token",
          },
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_request");
      expect(error.message).toBe("/runner_pool_id must be a UUID");
      expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain(rawSecret);
    }
    expect(calls).toEqual([
      { route: "instances", runnerPoolId: RUNNER_POOL_ID, limit: 100 },
      { route: "provision_requests", runnerPoolId: RUNNER_POOL_ID, limit: 100 },
    ]);
  });

  test("redacts runner metadata and provision diagnostics in management responses", async () => {
    const runners = {
      async listInstances() {
        return [{
          runner_instance_id: RUNNER_INSTANCE_ID,
          runner_pool_id: RUNNER_POOL_ID,
          instance_name: "vm-1 token=raw-instance-name-token",
          status: "unhealthy",
          capabilities: {
            executors: ["runner-docker"],
            resources: ["core_runner"],
            labels: ["file:///private/tmp/runner-label"],
          },
          metadata: {
            debug_path: "/Users/alice/private/runner/debug.json",
            access_token: "raw-instance-token",
            health: {
              reason: "failed in /private/tmp/runner token=raw-health-token",
            },
          },
          last_heartbeat_at: "2026-06-04T00:00:00Z",
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        }];
      },
      async listProvisionRequests() {
        return [{
          provision_request_id: "33333333-3333-4333-8333-333333333333",
          runner_pool_id: RUNNER_POOL_ID,
          run_id: null,
          status: "failed",
          provider: "gcp",
          provider_instance_id: "instance-token=raw-provider-instance-token",
          instance_name: "runner-/Users/alice/private/vm",
          runner_instance_id: null,
          requirements: {
            image_refs: [
              "https://registry.example/acme/runtime?token=raw-provision-image-token",
            ],
            secret_ids: ["OPENAI_API_KEY"],
          },
          metadata: {
            bootstrap_log: "file:///private/tmp/bootstrap.log",
            api_key: "sk-abcdefghijklmnopqrstuvwxyz",
          },
          error_message:
            "provider failed in /Users/alice/private/bootstrap token=raw-provider-error-token",
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        }];
      },
    };

    const instanceResponse = await handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-instances", {
        headers: {
          authorization: "Bearer admin-token",
        },
      }),
      new URL("https://cloud.example/v1/runner-instances"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    );
    const provisionResponse = await handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-provision-requests", {
        headers: {
          authorization: "Bearer admin-token",
        },
      }),
      new URL("https://cloud.example/v1/runner-provision-requests"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    );

    expect(instanceResponse).not.toBeNull();
    expect(provisionResponse).not.toBeNull();
    const instanceBody = await instanceResponse!.json();
    const provisionBody = await provisionResponse!.json();
    const text = JSON.stringify({ instanceBody, provisionBody });

    expect(instanceBody.runner_instances[0].instance_name).toContain("token=[redacted-secret]");
    expect(instanceBody.runner_instances[0].metadata.debug_path).toBe("[redacted-local-path]");
    expect(instanceBody.runner_instances[0].metadata.access_token).toBe("[redacted]");
    expect(instanceBody.runner_instances[0].metadata.health.reason).toContain("[redacted-local-path]");
    expect(provisionBody.provision_requests[0].requirements.secret_ids).toEqual(["OPENAI_API_KEY"]);
    expect(provisionBody.provision_requests[0].requirements.image_refs[0]).toContain("[redacted URL credentials/query]");
    expect(provisionBody.provision_requests[0].metadata.api_key).toBe("[redacted]");
    expect(provisionBody.provision_requests[0].error_message).toContain("[redacted-local-path]");
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("/private/tmp");
    expect(text).not.toContain("raw-instance-name-token");
    expect(text).not.toContain("raw-instance-token");
    expect(text).not.toContain("raw-health-token");
    expect(text).not.toContain("raw-provider-instance-token");
    expect(text).not.toContain("raw-provision-image-token");
    expect(text).not.toContain("raw-provider-error-token");
    expect(text).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
  });

  test("rejects malformed runner pool path ids before repository calls", async () => {
    let poolCalls = 0;
    const runners = {
      async getPool() {
        poolCalls += 1;
        return null;
      },
      async setPoolStatus() {
        poolCalls += 1;
        return null;
      },
    };

    for (const [path, rawSecret] of [
      [
        "/v1/runner-pools/token=raw-runner-pool-path-secret",
        "raw-runner-pool-path-secret",
      ],
      [
        "/v1/runner-pools/%2FUsers%2Falice%2Fprivate%2Fpool/drain",
        "/Users/alice",
      ],
      [
        "/v1/runner-pools/%20/disable",
        "raw-blank-runner-pool-secret",
      ],
    ] as const) {
      let caught: unknown;
      try {
        await handleRunnerRoute(
          new Request(`https://cloud.example${path}`, {
            method: path.endsWith("/drain") || path.endsWith("/disable") ? "POST" : "GET",
            headers: {
              authorization: "Bearer admin-token",
            },
          }),
          new URL(`https://cloud.example${path}`),
          runners as unknown as RunnerRepository,
          {
            workerToken: "worker-token",
            adminToken: "admin-token",
          },
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_request");
      expect(error.message).toBe("/runner_pool_id must be a UUID");
      expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain(rawSecret);
    }
    expect(poolCalls).toBe(0);
  });

  test("runner expire-stale accepts empty bodies and validates provided fields before mutation", async () => {
    const calls: Array<{ staleAfterSeconds: number; runnerPoolId?: string }> = [];
    const runners = {
      async markStaleInstancesOffline(input: { staleAfterSeconds: number; runnerPoolId?: string }) {
        calls.push(input);
        return [];
      },
    };

    const response = await handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-instances/expire-stale", {
        method: "POST",
        headers: {
          authorization: "Bearer admin-token",
        },
      }),
      new URL("https://cloud.example/v1/runner-instances/expire-stale"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    );
    expect(response).not.toBeNull();
    expect(calls).toEqual([{ staleAfterSeconds: 90 }]);

    const scopedResponse = await handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-instances/expire-stale", {
        method: "POST",
        headers: {
          authorization: "Bearer admin-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          stale_after_seconds: "120",
          runner_pool_id: ` ${RUNNER_POOL_ID.toUpperCase()} `,
        }),
      }),
      new URL("https://cloud.example/v1/runner-instances/expire-stale"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    );
    expect(scopedResponse).not.toBeNull();
    expect(calls).toEqual([
      { staleAfterSeconds: 90 },
      { staleAfterSeconds: 120, runnerPoolId: RUNNER_POOL_ID },
    ]);

    let caught: unknown;
    try {
      await handleRunnerRoute(
        new Request("https://cloud.example/v1/runner-instances/expire-stale", {
          method: "POST",
          headers: {
            authorization: "Bearer admin-token",
            "content-type": "application/json",
          },
          body: "{",
        }),
        new URL("https://cloud.example/v1/runner-instances/expire-stale"),
        runners as unknown as RunnerRepository,
        {
          workerToken: "worker-token",
          adminToken: "admin-token",
        },
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_json");
    expect(error.message).toBe("Request body must be valid JSON");
    expect(calls).toEqual([
      { staleAfterSeconds: 90 },
      { staleAfterSeconds: 120, runnerPoolId: RUNNER_POOL_ID },
    ]);

    for (const [body, message, rawSecret] of [
      [
        { stale_after_seconds: "token=raw-stale-secret" },
        "/stale_after_seconds must be a positive integer",
        "raw-stale-secret",
      ],
      [
        { stale_after_seconds: 0 },
        "/stale_after_seconds must be a positive integer",
        "raw-stale-zero-secret",
      ],
      [
        { runner_pool_id: 7 },
        "/runner_pool_id must be a UUID",
        "raw-pool-number-secret",
      ],
      [
        { runner_pool_id: "   " },
        "/runner_pool_id must be a UUID",
        "raw-pool-blank-secret",
      ],
    ] as const) {
      let invalidCaught: unknown;
      try {
        await handleRunnerRoute(
          new Request("https://cloud.example/v1/runner-instances/expire-stale", {
            method: "POST",
            headers: {
              authorization: "Bearer admin-token",
              "content-type": "application/json",
            },
            body: JSON.stringify(body),
          }),
          new URL("https://cloud.example/v1/runner-instances/expire-stale"),
          runners as unknown as RunnerRepository,
          {
            workerToken: "worker-token",
            adminToken: "admin-token",
          },
        );
      } catch (error) {
        invalidCaught = error;
      }

      expect(invalidCaught).toBeInstanceOf(HttpError);
      const invalidError = invalidCaught as HttpError;
      expect(invalidError.status).toBe(400);
      expect(invalidError.code).toBe("invalid_request");
      expect(invalidError.message).toBe(message);
      expect(JSON.stringify({ message: invalidError.message, detail: invalidError.detail })).not.toContain(rawSecret);
    }
    expect(calls).toEqual([
      { staleAfterSeconds: 90 },
      { staleAfterSeconds: 120, runnerPoolId: RUNNER_POOL_ID },
    ]);
  });

  test("runner instance paths fail clearly on malformed encoding before repository calls", async () => {
    let heartbeatCalls = 0;
    const runners = {
      async heartbeatInstance() {
        heartbeatCalls += 1;
        return null;
      },
    };

    let caught: unknown;
    try {
      await handleRunnerRoute(
        new Request("https://cloud.example/v1/runner-instances/%E0%A4%A-token=raw-runner-path-secret/heartbeat", {
          method: "POST",
          headers: {
            authorization: "Bearer worker-token",
            "content-type": "application/json",
          },
          body: JSON.stringify({
            runner_instance_id: RUNNER_INSTANCE_ID,
          }),
        }),
        new URL("https://cloud.example/v1/runner-instances/%E0%A4%A-token=raw-runner-path-secret/heartbeat"),
        runners as unknown as RunnerRepository,
        {
          workerToken: "worker-token",
          adminToken: "admin-token",
        },
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_path_param");
    expect(error.message).toBe("/runner_instance_id must be valid percent-encoded UTF-8");
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("raw-runner-path-secret");
    expect(heartbeatCalls).toBe(0);
  });

  test("rejects malformed runner instance path ids before repository calls", async () => {
    const calls: string[] = [];
    const runners = {
      async heartbeatInstance() {
        calls.push("heartbeat");
        return null;
      },
      async setInstanceStatus() {
        calls.push("status");
        return null;
      },
    };

    for (const [path, request, rawSecret] of [
      [
        "/v1/runner-instances/token=raw-runner-instance-path-secret/heartbeat",
        {
          token: "worker-token",
          body: {},
        },
        "raw-runner-instance-path-secret",
      ],
      [
        "/v1/runner-instances/%2FUsers%2Falice%2Fprivate%2Finstance/drain",
        {
          token: "admin-token",
        },
        "/Users/alice",
      ],
      [
        "/v1/runner-instances/%20/unhealthy",
        {
          token: "worker-token",
          body: { reason: "health_probe_failed" },
        },
        "raw-blank-runner-instance-secret",
      ],
      [
        "/v1/runner-instances/not-a-uuid/offline",
        {
          token: "worker-token",
          body: { reason: "worker_shutdown" },
        },
        "not-a-uuid",
      ],
    ] as const) {
      let caught: unknown;
      try {
        const init: RequestInit = {
          method: "POST",
          headers: {
            authorization: `Bearer ${request.token}`,
            ...(request.body !== undefined ? { "content-type": "application/json" } : {}),
          },
        };
        if (request.body !== undefined) {
          init.body = JSON.stringify(request.body);
        }
        await handleRunnerRoute(
          new Request(`https://cloud.example${path}`, init),
          new URL(`https://cloud.example${path}`),
          runners as unknown as RunnerRepository,
          {
            workerToken: "worker-token",
            adminToken: "admin-token",
          },
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_request");
      expect(error.message).toBe("/runner_instance_id must be a UUID");
      expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain(rawSecret);
    }
    expect(calls).toEqual([]);
  });

  test("rejects malformed runner shape capabilities during registration", async () => {
    const runners = {
      async registerInstance() {
        throw new Error("registerInstance should not be called");
      },
    };

    for (const [capabilities, message] of [
      [{ executors: "runner-docker", resources: ["core_runner"] }, "/capabilities/executors must be an array"],
      [{ executors: ["kubernetes"], resources: ["core_runner"] }, "/capabilities/executors/0 'kubernetes' is not supported"],
      [{ executors: ["runner-docker"], resources: "core_runner" }, "/capabilities/resources must be an array"],
      [{ executors: ["runner-docker"], resources: ["core_runner", 7] }, "/capabilities/resources/1 must be a non-empty string"],
      [{ executors: ["runner-docker"], resources: ["core_runner"], arch: "sparc" }, "Unsupported runner architecture"],
      [{ executors: ["runner-docker"], resources: ["core_runner"], cpu_count: 0 }, "/capabilities/cpu_count must be a positive integer"],
      [{ executors: ["runner-docker"], resources: ["core_runner"], cpu_count: 2147483648 }, "/capabilities/cpu_count must be a positive integer"],
      [{ executors: ["runner-docker"], resources: ["core_runner"], memory_mb: "many" }, "/capabilities/memory_mb must be a positive integer"],
      [{ executors: ["runner-docker"], resources: ["core_runner"], memory_mb: "999999999999999999999999" }, "/capabilities/memory_mb must be a positive integer"],
      [{ executors: ["runner-docker"], resources: ["core_runner"], disk_mb: -1 }, "/capabilities/disk_mb must be a positive integer"],
      [{ executors: ["runner-docker"], resources: ["core_runner"], isolation: "reusable_vm" }, "/capabilities/isolation must be an array"],
      [{ executors: ["runner-docker"], resources: ["core_runner"], isolation: ["reusable_vm", null] }, "/capabilities/isolation/1 must be a non-empty string"],
      [{ executors: ["runner-docker"], resources: ["core_runner"], isolation: ["reusable_vm", "shared_host"] }, "Unsupported runner isolation mode"],
    ]) {
      await expect(handleRunnerRoute(
        new Request("https://cloud.example/v1/runner-instances/register", {
          method: "POST",
          headers: {
            authorization: "Bearer worker-token",
            "content-type": "application/json",
          },
          body: JSON.stringify({
            runner_pool_id: RUNNER_POOL_ID,
            instance_name: "vm-1",
            capabilities,
            metadata: {},
          }),
        }),
        new URL("https://cloud.example/v1/runner-instances/register"),
        runners as unknown as RunnerRepository,
        {
          workerToken: "worker-token",
          adminToken: "admin-token",
        },
      )).rejects.toThrow(message);
    }
  });

  test("rejects malformed runner shape capabilities during heartbeat", async () => {
    const runners = {
      async heartbeatInstance() {
        throw new Error("heartbeatInstance should not be called");
      },
    };

    await expect(handleRunnerRoute(
      new Request(`https://cloud.example/v1/runner-instances/${RUNNER_INSTANCE_ID}/heartbeat`, {
        method: "POST",
        headers: {
          authorization: "Bearer worker-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          capabilities: {
            executors: ["runner-docker"],
            resources: ["core_runner"],
            cpu_count: 0,
          },
        }),
      }),
      new URL(`https://cloud.example/v1/runner-instances/${RUNNER_INSTANCE_ID}/heartbeat`),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    )).rejects.toThrow("/capabilities/cpu_count must be a positive integer");
  });
});
