import { describe, expect, test } from "bun:test";
import { handleRunnerRoute } from "../src/routes/runners";
import type { RunnerRepository } from "../src/runners/repository";

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
    )).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
      message: "runner pool management requires a valid runner admin token",
    });

    await expect(handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-pools", {
        headers: {
          "x-bucephalus-worker-token": "admin-token",
        },
      }),
      new URL("https://cloud.example/v1/runner-pools"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    )).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
      message: "runner pool management requires a valid runner admin token",
    });

    const headerResponse = await handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-pools", {
        headers: {
          "x-bucephalus-runner-admin-token": "admin-token",
        },
      }),
      new URL("https://cloud.example/v1/runner-pools"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
        adminToken: "admin-token",
      },
    );

    expect(await headerResponse!.json()).toEqual({ runner_pools: [] });

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

  test("legacy worker-token header remains valid for runner admin only without a separate admin token", async () => {
    const runners = {
      async listPools() {
        return [];
      },
    };

    const response = await handleRunnerRoute(
      new Request("https://cloud.example/v1/runner-pools", {
        headers: {
          "x-bucephalus-worker-token": "worker-token",
        },
      }),
      new URL("https://cloud.example/v1/runner-pools"),
      runners as unknown as RunnerRepository,
      {
        workerToken: "worker-token",
      },
    );

    expect(response).not.toBeNull();
    expect(await response!.json()).toEqual({ runner_pools: [] });
  });

  test("still accepts the worker token for runner registration", async () => {
    const runners = {
      async registerInstance() {
        return {
          runner_instance_id: "runner-instance-1",
          runner_pool_id: "runner-pool-1",
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
          runner_pool_id: "runner-pool-1",
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
    expect(body.runner_instance_id).toBe("runner-instance-1");
  });
});
