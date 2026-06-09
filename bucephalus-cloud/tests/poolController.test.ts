import { describe, expect, test } from "bun:test";
import { matchesCapabilities, reconcileOnce } from "../src/poolController";
import type { JsonObject } from "../src/primitives";
import type { RunnerProvisionRequestRecord } from "../src/runners/repository";

type PoolControllerConfigFixture = Parameters<typeof reconcileOnce>[0];
type CreateProvisionRequestInput = {
  runnerPoolId: string;
  runId: string;
  provider: string;
  requirements: JsonObject;
  metadata: JsonObject;
};
type MarkProvisioningInput = {
  provisionRequestId: string;
  providerInstanceId?: string | null;
  instanceName?: string | null;
  metadata?: JsonObject;
};
type FakeRunnersOverrides = {
  poolCapabilities?: unknown;
  demand?: unknown[];
  instances?: unknown[];
  openProvisionRequests?: unknown[];
  createProvisionRequest?: (input: CreateProvisionRequestInput) => RunnerProvisionRequestRecord | null | Promise<RunnerProvisionRequestRecord | null>;
  markProvisioning?: (input: MarkProvisioningInput) => RunnerProvisionRequestRecord | Promise<RunnerProvisionRequestRecord>;
};

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

  test("rejects runners missing Cloud-only perimeter and secret resources", () => {
    expect(matchesCapabilities(
      { executors: ["runner-docker"], resources: ["core_runner", "docker_daemon", "registry_pull"] },
      {
        executor: "runner-docker",
        requires: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver", "network_perimeter"],
      },
    )).toBe(false);
  });

  test("accepts runners that explicitly advertise Cloud-only perimeter and secret resources", () => {
    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver", "network_perimeter"],
      },
      {
        executor: "runner-docker",
        requires: ["core_runner", "secret_resolver", "network_perimeter"],
      },
    )).toBe(true);
  });

  test("keeps accelerator runs off generic docker runners", () => {
    expect(matchesCapabilities(
      { executors: ["runner-docker"], resources: ["core_runner", "docker_daemon", "registry_pull"] },
      {
        executor: "runner-docker",
        requires: ["core_runner", "docker_daemon", "registry_pull", "accelerator:nvidia-l4"],
      },
    )).toBe(false);

    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull", "accelerator:nvidia-l4"],
      },
      {
        executor: "runner-docker",
        requires: ["core_runner", "accelerator:nvidia-l4"],
      },
    )).toBe(true);
  });

  test("does not let modal runners claim docker-backed runs through shared core resources", () => {
    expect(matchesCapabilities(
      { executors: ["modal"], resources: ["core_runner", "modal", "registry_pull", "secret_resolver"] },
      { executor: "runner-docker", requires: ["core_runner", "registry_pull", "secret_resolver"] },
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

  test("rejects runners that omit explicit shape capabilities", () => {
    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
      },
      {
        executor: "runner-docker",
        requires: ["core_runner"],
        arch: "x86_64",
        cpu_count: 1,
        memory_mb: 1024,
        disk_mb: 20480,
        isolation: "reusable_vm",
      },
    )).toBe(false);

    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
        arch: "x86_64",
        cpu_count: 1,
        memory_mb: 1024,
      },
      {
        executor: "runner-docker",
        requires: ["core_runner"],
        arch: "x86_64",
        cpu_count: 1,
        memory_mb: 1024,
        disk_mb: 20480,
        isolation: "reusable_vm",
      },
    )).toBe(false);
  });

  test("rejects single-use VM runs on reusable-only runners", () => {
    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
        isolation: ["reusable_vm"],
      },
      {
        executor: "runner-docker",
        requires: ["core_runner"],
        isolation: "single_use_vm",
      },
    )).toBe(false);
  });

  test("treats an empty isolation list as explicitly unconstrained capacity", () => {
    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
        isolation: [],
      },
      {
        executor: "runner-docker",
        requires: ["core_runner"],
        isolation: "single_use_vm",
      },
    )).toBe(true);
  });

  test("does not match malformed persisted run requirement shapes", () => {
    const capabilities = {
      executors: ["runner-docker"],
      resources: ["core_runner", "docker_daemon", "registry_pull"],
      arch: "x86_64",
      cpu_count: 4,
      memory_mb: 8192,
      disk_mb: 65536,
      isolation: ["reusable_vm"],
    };

    for (const requirements of [
      { requires: ["core_runner"] },
      { executor: "runner-docker", requires: "core_runner" },
      { executor: "runner-docker", requires: ["core_runner"], cpu_count: "2" },
      { executor: "runner-docker", requires: ["core_runner"], memory_mb: "4096" },
      { executor: "runner-docker", requires: ["core_runner"], disk_mb: "32768" },
      { executor: "runner-docker", requires: ["core_runner"], isolation: ["reusable_vm"] },
      { executor: "runner-docker", requires: ["core_runner"], arch: 7 },
    ]) {
      expect(matchesCapabilities(capabilities, requirements as never)).toBe(false);
    }
  });
});

describe("pool controller provisioning", () => {
  test("does not provision accelerator demand from a generic docker pool", async () => {
    const observed = { createProvisionRequestCalls: 0 };
    const runners = fakeRunners({
      poolCapabilities: {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
      },
      demand: [gpuDemand()],
      createProvisionRequest() {
        observed.createProvisionRequestCalls += 1;
        throw new Error("generic pool should not provision accelerator demand");
      },
    });

    await reconcileOnce(poolControllerConfig(), runners as never);

    expect(observed.createProvisionRequestCalls).toBe(0);
  });

  test("does not provision accelerator demand when a matching accelerator instance is already claimable", async () => {
    const observed = { createProvisionRequestCalls: 0 };
    const runners = fakeRunners({
      poolCapabilities: gpuCapabilities(),
      demand: [gpuDemand()],
      instances: [{
        runner_instance_id: "runner-gpu-1",
        runner_pool_id: "pool-1",
        instance_name: "gpu-1",
        status: "online",
        capabilities: gpuCapabilities(),
        metadata: {},
        last_heartbeat_at: "2026-06-04T00:00:00Z",
        created_at: "2026-06-04T00:00:00Z",
        updated_at: "2026-06-04T00:00:00Z",
      }],
      createProvisionRequest() {
        observed.createProvisionRequestCalls += 1;
        throw new Error("matching accelerator instance should satisfy demand");
      },
    });

    await reconcileOnce(poolControllerConfig(), runners as never);

    expect(observed.createProvisionRequestCalls).toBe(0);
  });

  test("does not provision secret-ref demand from a pool without secret resolver capacity", async () => {
    const observed = { createProvisionRequestCalls: 0 };
    const runners = fakeRunners({
      poolCapabilities: {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
      },
      demand: [secretDemand()],
      createProvisionRequest() {
        observed.createProvisionRequestCalls += 1;
        throw new Error("generic pool should not provision persisted secret-ref demand");
      },
    });

    await reconcileOnce(poolControllerConfig(), runners as never);

    expect(observed.createProvisionRequestCalls).toBe(0);
  });

  test("provisions malformed persisted secret-ref demand with secret resolver requirement restored", async () => {
    const observed: {
      createProvisionRequestCalls: number;
      provisionRequirements?: JsonObject;
    } = { createProvisionRequestCalls: 0 };
    const runners = fakeRunners({
      poolCapabilities: {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver"],
      },
      demand: [secretDemand()],
      createProvisionRequest(input) {
        observed.createProvisionRequestCalls += 1;
        observed.provisionRequirements = input.requirements;
        return {
          provision_request_id: "provision-secret-1",
          runner_pool_id: "pool-1",
          run_id: "run-secret",
          status: "requested",
          provider: "exec",
          provider_instance_id: null,
          instance_name: null,
          runner_instance_id: null,
          requirements: input.requirements,
          metadata: {},
          error_message: null,
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        };
      },
    });

    await reconcileOnce(poolControllerConfig({
      provisionCommand: [
        "bun",
        "-e",
        "process.stdout.write(JSON.stringify({provider_instance_id:'secret-provider-1'})+'\\n')",
      ],
    }), runners as never);

    expect(observed.createProvisionRequestCalls).toBe(1);
    expect(observed.provisionRequirements).toMatchObject({
      executor: "runner-docker",
      requires: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver"],
    });
  });

  test("provisions accelerator demand only from a pool that advertises the accelerator resource", async () => {
    const observed: {
      createProvisionRequestCalls: number;
      provisionRequirements?: JsonObject;
      providerInstanceId?: string;
    } = { createProvisionRequestCalls: 0 };
    const runners = fakeRunners({
      poolCapabilities: gpuCapabilities(),
      demand: [gpuDemand()],
      createProvisionRequest(input) {
        observed.createProvisionRequestCalls += 1;
        observed.provisionRequirements = input.requirements;
        return {
          provision_request_id: "provision-1",
          runner_pool_id: "pool-1",
          run_id: "run-gpu",
          status: "requested",
          provider: "exec",
          provider_instance_id: null,
          instance_name: null,
          runner_instance_id: null,
          requirements: input.requirements,
          metadata: {},
          error_message: null,
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        };
      },
      markProvisioning(input) {
        if (input.providerInstanceId) {
          observed.providerInstanceId = input.providerInstanceId;
        }
        return {
          provision_request_id: "provision-1",
          runner_pool_id: "pool-1",
          run_id: "run-gpu",
          status: input.providerInstanceId ? "provisioning" : "requested",
          provider: "exec",
          provider_instance_id: input.providerInstanceId ?? null,
          instance_name: input.instanceName ?? null,
          runner_instance_id: null,
          requirements: observed.provisionRequirements ?? {},
          metadata: input.metadata ?? {},
          error_message: null,
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        };
      },
    });

    await reconcileOnce(poolControllerConfig({
      provisionCommand: [
        "bun",
        "-e",
        "process.stdout.write(JSON.stringify({provider_instance_id:'gpu-provider-1'})+'\\n')",
      ],
    }), runners as never);

    expect(observed.createProvisionRequestCalls).toBe(1);
    expect(observed.provisionRequirements).toMatchObject({
      executor: "runner-docker",
      requires: ["core_runner", "docker_daemon", "registry_pull", "accelerator:nvidia-l4"],
      accelerators: ["nvidia-l4"],
    });
    expect(observed.providerInstanceId).toBe("gpu-provider-1");
  });
});

function gpuCapabilities() {
  return {
    executors: ["runner-docker"],
    resources: ["core_runner", "docker_daemon", "registry_pull", "accelerator:nvidia-l4"],
    arch: "x86_64",
    cpu_count: 8,
    memory_mb: 32768,
    disk_mb: 65536,
    isolation: ["reusable_vm"],
  };
}

function gpuDemand(overrides: Record<string, unknown> = {}) {
  return {
    run_id: "run-gpu",
    secret_refs: {},
    created_at: "2026-06-04T00:00:00Z",
    run_requirements: {
      executor: "runner-docker",
      requires: ["core_runner", "docker_daemon", "registry_pull", "accelerator:nvidia-l4"],
      image_refs: [],
      secret_ids: [],
      network_perimeter: {
        default: "none",
        task_sandbox: "none",
        agent: "none",
        egress_hosts: [],
      },
      sidecars: [],
      accelerators: ["nvidia-l4"],
      arch: "x86_64",
      cpu_count: 1,
      memory_mb: 1024,
      disk_mb: 20480,
      isolation: "reusable_vm",
      timeout_ms: null,
      max_parallel_trials: 1,
      ...overrides,
    },
  };
}

function secretDemand() {
  return {
    ...gpuDemand(),
    run_id: "run-secret",
    secret_refs: {
      OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
    },
    run_requirements: {
      executor: "runner-docker",
      requires: ["core_runner", "docker_daemon", "registry_pull"],
      image_refs: [],
      secret_ids: [],
      network_perimeter: {
        default: "none",
        task_sandbox: "none",
        agent: "none",
        egress_hosts: [],
      },
      sidecars: [],
      accelerators: [],
      timeout_ms: null,
      max_parallel_trials: 1,
    },
  };
}

function poolControllerConfig(overrides: Partial<PoolControllerConfigFixture> = {}): PoolControllerConfigFixture {
  return {
    apiUrl: "https://cloud.example",
    workerToken: "worker-token",
    runnerPoolId: "pool-1",
    provider: "exec",
    provisionCommand: ["bun", "-e", "process.stdout.write('{}\\n')"],
    reapCommand: ["bun", "-e", "process.stdout.write('{}\\n')"],
    pollMs: 1,
    staleInstanceSeconds: 90,
    provisioningTimeoutSeconds: 600,
    providerCommandTimeoutMs: 5000,
    demandLimit: 50,
    reapIdleCompletedRunners: true,
    idleReapDelaySeconds: 0,
    healthHost: "127.0.0.1",
    healthPort: 0,
    ...overrides,
  };
}

function fakeRunners(overrides: FakeRunnersOverrides) {
  const poolCapabilities = overrides.poolCapabilities ?? {
    executors: ["runner-docker"],
    resources: ["core_runner", "docker_daemon", "registry_pull"],
  };
  return {
    async getPool() {
      return {
        runner_pool_id: "pool-1",
        name: "gpu-pool",
        status: "active",
        capabilities: poolCapabilities,
        metadata: {},
        created_at: "2026-06-04T00:00:00Z",
        updated_at: "2026-06-04T00:00:00Z",
      };
    },
    async markStaleInstancesOffline() {
      return [];
    },
    async failStaleUnacceptedProvisionRequests() {
      return [];
    },
    async listReapableProvisionRequests() {
      return [];
    },
    async listIdleCompletedRunProvisionRequests() {
      return [];
    },
    async listQueuedDemand() {
      return overrides.demand ?? [];
    },
    async listClaimableInstances() {
      return overrides.instances ?? [];
    },
    async listOpenProvisionRequests() {
      return overrides.openProvisionRequests ?? [];
    },
    async createProvisionRequest(input: CreateProvisionRequestInput) {
      if (overrides.createProvisionRequest) {
        return overrides.createProvisionRequest(input);
      }
      throw new Error("createProvisionRequest should not be called");
    },
    async markProvisioning(input: MarkProvisioningInput) {
      if (overrides.markProvisioning) {
        return overrides.markProvisioning(input);
      }
      return {};
    },
    async failProvisionRequest() {
      throw new Error("failProvisionRequest should not be called");
    },
    async markProvisionRequestReaped() {
      throw new Error("markProvisionRequestReaped should not be called");
    },
  };
}
