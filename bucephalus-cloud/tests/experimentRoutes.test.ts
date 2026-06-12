import { describe, expect, test } from "bun:test";
import { handleExperimentRoute } from "../src/routes/experiments";
import type { AuthContext } from "../src/auth";
import type { ImportRepository } from "../src/imports/repository";
import type { PackageRepository, RunRepository } from "../src/packages/repository";
import type { RunnerRepository } from "../src/runners/repository";
import type { CloudSecretRepository } from "../src/secrets/repository";

describe("Hosted experiment routes", () => {
  test("doctor reports runnable package requirements without creating a run", async () => {
    const response = await handleExperimentRoute(
      new Request("https://cloud.example/v1/experiments/doctor", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: digest(),
          secret_refs: {
            GEMINI_API_KEY: "gcp-secret-manager://projects/acme/secrets/gemini/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/experiments/doctor"),
      {} as ImportRepository,
      packagesReturning(cloudReadyPackage()) as unknown as PackageRepository,
      {} as RunRepository,
      runnersWithDockerPool() as unknown as RunnerRepository,
      authContext("user-a"),
      {} as CloudSecretRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    const body = await response!.json();
    expect(body.ok).toBe(true);
    expect(body.status).toBe("runnable");
    expect(body.package_digest).toBe(digest());
    expect(body.secret_requirements).toEqual([
      {
        id: "GEMINI_API_KEY",
        target: "",
        required_for_variants: [],
      },
    ]);
    expect(body.supplied_secret_ids).toEqual(["GEMINI_API_KEY"]);
    expect(body.run_requirements.executor).toBe("runner-docker");
    expect(body.run_requirements.requires).toContain("registry_pull");
    expect(body.run_requirements.requires).toContain("secret_resolver");
  });

  test("doctor rejects local image refs before a run is created", async () => {
    await expect(handleExperimentRoute(
      new Request("https://cloud.example/v1/experiments/doctor", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: digest(),
          secret_refs: {
            GEMINI_API_KEY: "gcp-secret-manager://projects/acme/secrets/gemini/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/experiments/doctor"),
      {} as ImportRepository,
      packagesReturning({
        ...cloudReadyPackage(),
        image_refs: ["peter-gregory-v2-nova:local"],
      }) as unknown as PackageRepository,
      {} as RunRepository,
      runnersWithDockerPool() as unknown as RunnerRepository,
      authContext("user-a"),
      {} as CloudSecretRepository,
    )).rejects.toThrow("not digest-pinned remote registry refs");
  });

  test("doctor rejects missing secret refs with package requirement detail", async () => {
    await expect(handleExperimentRoute(
      new Request("https://cloud.example/v1/experiments/doctor", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: digest(),
        }),
      }),
      new URL("https://cloud.example/v1/experiments/doctor"),
      {} as ImportRepository,
      packagesReturning(cloudReadyPackage()) as unknown as PackageRepository,
      {} as RunRepository,
      runnersWithDockerPool() as unknown as RunnerRepository,
      authContext("user-a"),
      {} as CloudSecretRepository,
    )).rejects.toThrow("Run secret refs must match the package secret requirements");
  });
});

function digest(): string {
  return "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
}

function packagesReturning(record: Record<string, unknown>): Pick<PackageRepository, "getArtifact"> {
  return {
    async getArtifact() {
      return record as never;
    },
  };
}

function cloudReadyPackage() {
  return {
    package_digest: digest(),
    upload_id: "upload-1",
    storage_path: "/tmp/package.tgz",
    byte_size: 1,
    media_type: "application/gzip",
    manifest_json: {},
    resolved_experiment_json: {
      experiment: {
        name: "Peter Gregory v2",
      },
      runtime: {
        compute: { backend: "local-docker" },
        secrets: [
          {
            name: "GEMINI_API_KEY",
            from: "env",
          },
        ],
      },
    },
    target: {
      schema_version: "cloud_package_target_v1",
      platforms: ["linux/amd64"],
    },
    image_refs: [
      "us-central1-docker.pkg.dev/acme/buc/peter-gregory-v2-nova@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ],
    diagnostics: [],
    status: "accepted",
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
  };
}

function runnersWithDockerPool(): Pick<RunnerRepository, "listPools"> {
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
