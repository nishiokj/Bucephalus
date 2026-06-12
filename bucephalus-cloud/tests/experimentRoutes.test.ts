import { describe, expect, test } from "bun:test";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import * as tar from "tar";
import { handleExperimentRoute } from "../src/routes/experiments";
import type { AuthContext } from "../src/auth";
import type { ImportRepository } from "../src/imports/repository";
import type { PackageRepository, RunRepository } from "../src/packages/repository";
import type { RunnerRepository } from "../src/runners/repository";
import type { CloudSecretRepository } from "../src/secrets/repository";
import { canonicalJsonStringify, sha256Digest } from "../src/primitives";

describe("Hosted experiment routes", () => {
  test("build response is explicit that sealed package import is not hosted YAML authoring", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-"));
    try {
      const { archivePath, packageDigest } = await writePackageArchive(root);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            upload_id: "upload-1",
            label: "demo",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsAcceptingCompletedUpload(archivePath, packageDigest) as unknown as ImportRepository,
        packagesRecordingArtifact(packageDigest) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      expect(response!.status).toBe(201);
      const body = await response!.json();
      expect(body.build_id).toBe("import-1");
      expect(body.build_kind).toBe("sealed_package_import");
      expect(body.authoring_build.status).toBe("unavailable");
      expect(body.authoring_build.message).toContain("experiment.yaml is not implemented");
      expect(body.status).toBe("accepted");
      expect(body.package_digest).toBe(packageDigest);
      expect(body.import.import_type).toBe("sealed_package");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

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

function importsAcceptingCompletedUpload(
  archivePath: string,
  packageDigest: string,
): Pick<ImportRepository, "getUpload" | "createImportJob" | "updateImportInspection" | "getImportJob"> {
  let status = "inspecting";
  return {
    async getUpload(uploadId: string) {
      expect(uploadId).toBe("upload-1");
      return {
        upload_id: "upload-1",
        filename: "package.tgz",
        media_type: "application/gzip",
        expected_digest: null,
        content_digest: packageDigest,
        byte_size: 1,
        storage_path: archivePath,
        status: "completed",
        created_at: "2026-06-04T00:00:00Z",
        uploaded_at: "2026-06-04T00:00:00Z",
        completed_at: "2026-06-04T00:00:00Z",
        error_message: null,
      };
    },
    async createImportJob(input: { uploadId: string; label?: string | null }) {
      expect(input.uploadId).toBe("upload-1");
      expect(input.label).toBe("demo");
      return "import-1";
    },
    async updateImportInspection(input: { status: "accepted" | "failed" }) {
      status = input.status;
    },
    async getImportJob() {
      return {
        import_id: "import-1",
        upload_id: "upload-1",
        import_type: "sealed_package",
        status,
        label: "demo",
        package_digest: status === "accepted" ? packageDigest : null,
        manifest_json: status === "accepted" ? {} : null,
        resolved_experiment_json: status === "accepted" ? { experiment: { name: "Peter Gregory v2" } } : null,
        created_at: "2026-06-04T00:00:00Z",
        updated_at: "2026-06-04T00:00:00Z",
        error_message: null,
        diagnostics: [],
      };
    },
  };
}

function packagesRecordingArtifact(packageDigest: string): Pick<PackageRepository, "upsertArtifact"> {
  return {
    async upsertArtifact(input: { packageDigest: string }) {
      expect(input.packageDigest).toBe(packageDigest);
      return {
        ...cloudReadyPackage(),
        package_digest: packageDigest,
      } as never;
    },
  };
}

async function writePackageArchive(root: string): Promise<{ archivePath: string; packageDigest: string }> {
  const packageDir = join(root, "package");
  await mkdir(packageDir, { recursive: true });
  const resolvedExperiment = cloudReadyPackage().resolved_experiment_json;
  await writeFile(join(packageDir, "resolved_experiment.json"), JSON.stringify(resolvedExperiment));
  await writeFile(join(packageDir, "staging_manifest.json"), JSON.stringify({
    schema_version: "package_staging_manifest_v1",
  }));
  const checksums = await checksumsForPackage(packageDir);
  const packageDigest = sha256Digest(canonicalJsonStringify(checksums.files));
  await writeFile(join(packageDir, "checksums.json"), JSON.stringify(checksums));
  await writeFile(join(packageDir, "package.lock"), JSON.stringify({
    schema_version: "sealed_package_lock_v1",
    package_digest: packageDigest,
  }));
  await writeFile(join(packageDir, "package_checks.json"), JSON.stringify({
    schema_version: "package_checks_v1",
    package_digest: packageDigest,
    passed: true,
    checks: [],
    summary: {
      checks: 0,
      failed: 0,
      warnings: 0,
    },
  }));
  await writeFile(join(packageDir, "manifest.json"), JSON.stringify({
    schema_version: "sealed_run_package_v2",
    created_at: "2026-06-04T00:00:00Z",
    resolved_experiment: resolvedExperiment,
    checksums_ref: "checksums.json",
    package_checks_ref: "package_checks.json",
    package_digest: packageDigest,
  }));
  const archivePath = join(root, "package.tgz");
  await tar.c({ gzip: true, cwd: packageDir, file: archivePath }, (await readdir(packageDir)).sort());
  return { archivePath, packageDigest };
}

async function checksumsForPackage(packageDir: string): Promise<{ schema_version: string; files: Record<string, string> }> {
  const files: Record<string, string> = {};
  async function visit(relDir: string): Promise<void> {
    const dir = relDir ? join(packageDir, relDir) : packageDir;
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const rel = relDir ? `${relDir}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        await visit(rel);
      } else if (entry.isFile() && !["manifest.json", "checksums.json", "package.lock"].includes(rel)) {
        files[rel] = sha256Digest(await readFile(join(packageDir, rel)));
      }
    }
  }
  await visit("");
  return {
    schema_version: "sealed_package_checksums_v2",
    files: Object.fromEntries(Object.entries(files).sort(([left], [right]) => left.localeCompare(right))),
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
