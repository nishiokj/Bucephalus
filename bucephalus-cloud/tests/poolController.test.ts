import { describe, expect, test } from "bun:test";
import { matchesCapabilities, reconcileOnce } from "../src/poolController";

describe("pool controller matching", () => {
  test("matches executor and every required resource", () => {
    expect(matchesCapabilities(
      { executors: ["runner-docker"], resources: ["core_runner", "docker_daemon", "registry_pull"] },
      { executor: "runner-docker", requires: ["core_runner", "registry_pull"] },
    )).toBe(true);
  });

  test("rejects incompatible executor", () => {
    expect(matchesCapabilities(
      { executors: ["modal"], resources: ["core_runner", "modal"] },
      { executor: "runner-docker", requires: ["core_runner"] },
    )).toBe(false);
  });

  test("rejects missing resources", () => {
    expect(matchesCapabilities(
      { executors: ["runner-docker"], resources: ["core_runner"] },
      { executor: "runner-docker", requires: ["core_runner", "docker_daemon"] },
    )).toBe(false);
  });

  test("matches explicit VM shape requirements", () => {
    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
        arch: "arm64",
        cpu_count: 4,
        memory_mb: 8192,
        disk_mb: 65536,
        isolation: ["reusable_vm"],
      },
      {
        executor: "runner-docker",
        requires: ["core_runner"],
        arch: "arm64",
        cpu_count: 2,
        memory_mb: 4096,
        disk_mb: 32768,
        isolation: "reusable_vm",
      },
    )).toBe(true);
  });

  test("rejects insufficient VM shape", () => {
    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
        arch: "x86_64",
        cpu_count: 2,
        memory_mb: 2048,
        disk_mb: 32768,
        isolation: ["reusable_vm"],
      },
      {
        executor: "runner-docker",
        requires: ["core_runner"],
        arch: "arm64",
        cpu_count: 4,
        memory_mb: 8192,
        disk_mb: 65536,
        isolation: "single_use_vm",
      },
    )).toBe(false);
  });

  test("reconciles configured pool capabilities before matching queued demand", async () => {
    const calls: string[] = [];
    const stale = {
      markStaleInstancesOffline: async () => [],
      failStaleUnacceptedProvisionRequests: async () => [],
      listReapableProvisionRequests: async () => [],
      listIdleCompletedRunProvisionRequests: async () => [],
      listQueuedDemand: async () => [],
      listClaimableInstances: async () => [],
      listOpenProvisionRequests: async () => [],
    };
    const runners = {
      ...stale,
      async getPool() {
        calls.push("getPool");
        return {
          runner_pool_id: "pool-1",
          name: "gcp",
          status: "active",
          capabilities: {
            executors: ["runner-docker"],
            resources: ["core_runner", "docker_daemon", "registry_pull"],
            isolation: ["reusable_vm"],
          },
          metadata: {},
          created_at: "2026-06-09T00:00:00Z",
          updated_at: "2026-06-09T00:00:00Z",
        };
      },
      async setPoolCapabilities(input: any) {
        calls.push("setPoolCapabilities");
        expect(input.capabilities.resources).toContain("network_perimeter");
        return {
          runner_pool_id: "pool-1",
          name: "gcp",
          status: "active",
          capabilities: input.capabilities,
          metadata: {},
          created_at: "2026-06-09T00:00:00Z",
          updated_at: "2026-06-09T00:00:00Z",
        };
      },
    };

    await reconcileOnce({
      apiUrl: "https://api.example",
      workerToken: "worker",
      runnerPoolId: "pool-1",
      provider: "exec",
      provisionCommand: ["true"],
      reapCommand: ["true"],
      configuredPoolCapabilities: {
        arch: "x86_64",
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver", "network_perimeter"],
        isolation: ["reusable_vm", "single_use_vm"],
      },
      pollMs: 2000,
      staleInstanceSeconds: 90,
      provisioningTimeoutSeconds: 600,
      providerCommandTimeoutMs: 600000,
      demandLimit: 50,
      reapIdleCompletedRunners: true,
      idleReapDelaySeconds: 0,
      healthHost: "127.0.0.1",
      healthPort: 0,
    }, runners as any);

    expect(calls).toEqual(["getPool", "setPoolCapabilities"]);
  });
});
