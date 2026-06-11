import { mkdtemp, rm, readFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
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

  test("matches modal executor only when modal resource is advertised", () => {
    expect(matchesCapabilities(
      { executors: ["runner-docker", "modal"], resources: ["core_runner", "registry_pull", "modal"] },
      { executor: "modal", requires: ["core_runner", "modal", "registry_pull"] },
    )).toBe(true);

    expect(matchesCapabilities(
      { executors: ["runner-docker", "modal"], resources: ["core_runner", "registry_pull"] },
      { executor: "modal", requires: ["core_runner", "modal", "registry_pull"] },
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

  test("passes active worker image state to the provision command", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-pool-controller-"));
    const capturePath = join(root, "provision-input.json");
    try {
      const markProvisioningMetadata: unknown[] = [];
      const runners = {
        markStaleInstancesOffline: async () => [],
        failStaleUnacceptedProvisionRequests: async () => [],
        listReapableProvisionRequests: async () => [],
        listIdleCompletedRunProvisionRequests: async () => [],
        listClaimableInstances: async () => [],
        listOpenProvisionRequests: async () => [],
        async getPool() {
          return poolRecord();
        },
        async listQueuedDemand() {
          return [{
            run_id: "run-1",
            run_requirements: {
              executor: "runner-docker",
              requires: ["core_runner"],
              image_refs: [],
              isolation: "reusable_vm",
            },
            created_at: "2026-06-09T00:00:00Z",
          }];
        },
        async getActiveWorkerImageForPool() {
          return workerImageRecord();
        },
        async createProvisionRequest(input: any) {
          expect(input.metadata.worker_image.image_ref).toBe(workerImageRecord().image_ref);
          return {
            provision_request_id: "provision-1",
            runner_pool_id: "pool-1",
            run_id: "run-1",
            status: "requested",
            provider: "exec",
            provider_instance_id: null,
            instance_name: null,
            runner_instance_id: null,
            requirements: input.requirements,
            metadata: input.metadata,
            error_message: null,
            created_at: "2026-06-09T00:00:00Z",
            updated_at: "2026-06-09T00:00:00Z",
          };
        },
        async markProvisioning(input: any) {
          markProvisioningMetadata.push(input.metadata);
          return {};
        },
        async failProvisionRequest(input: any) {
          throw new Error(`provision failed unexpectedly: ${input.message}`);
        },
      };

      await reconcileOnce({
        apiUrl: "https://api.example",
        workerToken: "worker",
        runnerPoolId: "pool-1",
        provider: "exec",
        provisionCommand: [
          "bun",
          "-e",
          "const input = await Bun.stdin.text(); await Bun.write(process.argv[1], input); console.log(JSON.stringify({ provider_instance_id: 'gcp://runner-1', instance_name: 'runner-1' }));",
          capturePath,
        ],
        reapCommand: ["true"],
        configuredPoolCapabilities: poolRecord().capabilities,
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

      const provisionInput = JSON.parse(await readFile(capturePath, "utf8"));
      expect(provisionInput.worker_image).toBe(workerImageRecord().image_ref);
      expect(markProvisioningMetadata[0]).toEqual({
        provider_started_at: expect.any(String),
        worker_image: {
          runner_worker_image_id: "worker-image-1",
          image_ref: workerImageRecord().image_ref,
          release_version: "2026.06.11",
          release_git_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        },
      });
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});

function poolRecord() {
  return {
    runner_pool_id: "pool-1",
    name: "gcp",
    status: "active",
    active_worker_image_id: "worker-image-1",
    capabilities: {
      arch: "x86_64",
      executors: ["runner-docker"],
      resources: ["core_runner", "docker_daemon", "registry_pull"],
      isolation: ["reusable_vm"],
    },
    metadata: {},
    created_at: "2026-06-09T00:00:00Z",
    updated_at: "2026-06-09T00:00:00Z",
  };
}

function workerImageRecord() {
  return {
    runner_worker_image_id: "worker-image-1",
    image_ref: "us-central1-docker.pkg.dev/project/repo/worker@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    registry_host: "us-central1-docker.pkg.dev",
    repository: "us-central1-docker.pkg.dev/project/repo/worker",
    digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    release_version: "2026.06.11",
    release_git_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    promotion_evidence_uri: null,
    promotion_evidence_sha256: null,
    modal_launcher_sha256: null,
    worker_runner_sha256: null,
    boundary_verified_at: null,
    metadata: {},
    created_at: "2026-06-09T00:00:00Z",
  };
}
