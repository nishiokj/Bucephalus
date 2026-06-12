import { describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import * as tar from "tar";
import { handleExperimentRoute } from "../src/routes/experiments";
import type { AuthContext } from "../src/auth";
import type { ImportRepository } from "../src/imports/repository";
import type { ImportDiagnostic } from "../src/imports/sealedPackage";
import type { PackageRepository, RunRepository } from "../src/packages/repository";
import type { RunnerPoolRecord, RunnerRepository } from "../src/runners/repository";
import type { CloudSecretRepository } from "../src/secrets/repository";
import { sha256Digest, type JsonObject } from "../src/primitives";

const realCoreSmoke = process.env.BUCEPHALUS_CLOUD_REAL_CORE_SMOKE === "1";
const smokeTest = realCoreSmoke ? test : test.skip;

describe("hosted authoring real Core smoke", () => {
  smokeTest("runs the real Core builder, imports its package, and evaluates Cloud readiness", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-hosted-real-core-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    const previousTimeout = process.env.BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS;
    try {
      const coreCli = resolve(process.env.BUCEPHALUS_CLOUD_CORE_CLI ?? "target/debug/bucephalus");
      expect(existsSync(coreCli), `real Core binary is required at ${coreCli}`).toBe(true);
      const cookbookDir = resolve("cookbook/agent-eval");
      const sourceArchive = join(root, "agent-eval-authoring.tgz");
      await tar.c({ gzip: true, cwd: cookbookDir, file: sourceArchive }, (await readdir(cookbookDir)).sort());

      process.env.BUCEPHALUS_CLOUD_CORE_CLI = coreCli;
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      process.env.BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS = "60000";

      const imports = capturingImportRepository(sourceArchive);
      const packages = capturingPackageRepository();
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
            label: "real-core-smoke",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        imports as unknown as ImportRepository,
        packages as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      expect(response!.status).toBe(201);
      const body = await response!.json();
      if (body.authoring_build?.status !== "succeeded") {
        console.error(JSON.stringify(body, null, 2));
      }
      expect(body.build_kind).toBe("hosted_authoring_build");
      expect(body.authoring_build.status).toBe("succeeded");
      expect(body.authoring_build.source_upload_id).toBe("source-upload");
      expect(body.authoring_build.core.command).toBe("bucephalus build");
      expect(body.authoring_build.entrypoint).toBe("experiment.yaml");
      expect(body.import.status).toBe("accepted");
      expect(body.package_digest).toMatch(/^sha256:[a-f0-9]{64}$/);
      expect(body.build_environment.source).toEqual(expect.objectContaining({
        input_kind: "authoring_context",
        upload_id: "source-upload",
        content_digest: sha256Digest(await readFile(sourceArchive)),
        byte_size: (await readFile(sourceArchive)).byteLength,
        entrypoint: "experiment.yaml",
      }));
      expect(body.build_environment.package_contract).toEqual(expect.objectContaining({
        authoring_compiler: "core_universal_v1",
        cloud_readiness_required: true,
        input_kind: "authoring_context",
      }));
      expect(packages.artifact?.package_digest).toBe(body.package_digest);
      const resolvedExperiment = packages.artifact?.resolved_experiment_json as JsonObject;
      expect(resolvedExperiment.experiment).toEqual(expect.objectContaining({
        id: "cookbook_agent_eval",
      }));
      expect(body.cloud_readiness.status).toBe("cloud_blocked");
      expect(body.cloud_readiness.checks).toContainEqual(expect.objectContaining({
        name: "runtime_contract",
        status: "blocked",
        code: "package_images_not_cloud_pinned",
      }));
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      restoreEnv("BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS", previousTimeout);
      await rm(root, { recursive: true, force: true });
    }
  });
});

function capturingImportRepository(sourceArchivePath: string): Pick<ImportRepository, "getUpload" | "createUpload" | "markUploaded" | "completeUpload" | "createImportJob" | "updateImportInspection" | "getImportJob"> {
  const state: {
    packageStoragePath: string | null;
    packageByteSize: number | null;
    packageContentDigest: string | null;
    importStatus: "inspecting" | "accepted" | "failed";
    packageDigest: string | null;
    manifestJson: JsonObject | null;
    resolvedExperimentJson: JsonObject | null;
    diagnostics: ImportDiagnostic[];
    errorMessage: string | null;
  } = {
    packageStoragePath: null,
    packageByteSize: null,
    packageContentDigest: null,
    importStatus: "inspecting",
    packageDigest: null,
    manifestJson: null,
    resolvedExperimentJson: null,
    diagnostics: [],
    errorMessage: null,
  };
  return {
    async getUpload(uploadId: string) {
      if (uploadId === "source-upload") {
        const sourceBytes = await readFile(sourceArchivePath);
        return {
          upload_id: "source-upload",
          filename: "authoring-context.tgz",
          media_type: "application/gzip",
          expected_digest: sha256Digest(sourceBytes),
          content_digest: sha256Digest(sourceBytes),
          byte_size: sourceBytes.byteLength,
          storage_path: sourceArchivePath,
          status: "completed",
          created_at: "2026-06-04T00:00:00Z",
          uploaded_at: "2026-06-04T00:00:00Z",
          completed_at: "2026-06-04T00:00:00Z",
          error_message: null,
        };
      }
      expect(uploadId).toBe("package-upload");
      return {
        upload_id: "package-upload",
        filename: "package.tgz",
        media_type: "application/gzip",
        expected_digest: state.packageContentDigest,
        content_digest: state.packageContentDigest,
        byte_size: state.packageByteSize,
        storage_path: state.packageStoragePath,
        status: state.packageStoragePath ? "completed" : "created",
        created_at: "2026-06-04T00:00:00Z",
        uploaded_at: state.packageStoragePath ? "2026-06-04T00:00:00Z" : null,
        completed_at: state.packageStoragePath ? "2026-06-04T00:00:00Z" : null,
        error_message: null,
      };
    },
    async createUpload(input: { filename: string; mediaType: string; expectedDigest?: string | null; byteSize?: number | null }) {
      expect(input.filename).toBe("package.tgz");
      expect(input.mediaType).toBe("application/gzip");
      state.packageContentDigest = input.expectedDigest ?? null;
      state.packageByteSize = input.byteSize ?? null;
      return {
        upload_id: "package-upload",
        filename: "package.tgz",
        media_type: "application/gzip",
        expected_digest: input.expectedDigest ?? null,
        content_digest: null,
        byte_size: input.byteSize ?? null,
        storage_path: null,
        status: "created",
        created_at: "2026-06-04T00:00:00Z",
        uploaded_at: null,
        completed_at: null,
        error_message: null,
      };
    },
    async markUploaded(input: { storagePath: string; byteSize: number; contentDigest: string }) {
      state.packageStoragePath = input.storagePath;
      state.packageByteSize = input.byteSize;
      state.packageContentDigest = input.contentDigest;
      return {
        upload_id: "package-upload",
        filename: "package.tgz",
        media_type: "application/gzip",
        expected_digest: input.contentDigest,
        content_digest: input.contentDigest,
        byte_size: input.byteSize,
        storage_path: input.storagePath,
        status: "uploaded",
        created_at: "2026-06-04T00:00:00Z",
        uploaded_at: "2026-06-04T00:00:00Z",
        completed_at: null,
        error_message: null,
      };
    },
    async completeUpload(uploadId: string) {
      expect(uploadId).toBe("package-upload");
      return {
        upload_id: "package-upload",
        filename: "package.tgz",
        media_type: "application/gzip",
        expected_digest: state.packageContentDigest,
        content_digest: state.packageContentDigest,
        byte_size: state.packageByteSize,
        storage_path: state.packageStoragePath,
        status: "completed",
        created_at: "2026-06-04T00:00:00Z",
        uploaded_at: "2026-06-04T00:00:00Z",
        completed_at: "2026-06-04T00:00:00Z",
        error_message: null,
      };
    },
    async createImportJob(input: { uploadId: string; label?: string | null }) {
      expect(input.uploadId).toBe("package-upload");
      expect(input.label).toBe("real-core-smoke");
      return "import-real-core";
    },
    async updateImportInspection(input: {
      status: "accepted" | "failed";
      packageDigest?: string | null;
      manifestJson?: JsonObject | null;
      resolvedExperimentJson?: JsonObject | null;
      diagnostics?: ImportDiagnostic[];
      errorMessage?: string | null;
    }) {
      state.importStatus = input.status;
      state.packageDigest = input.packageDigest ?? null;
      state.manifestJson = input.manifestJson ?? null;
      state.resolvedExperimentJson = input.resolvedExperimentJson ?? null;
      state.diagnostics = input.diagnostics ?? [];
      state.errorMessage = input.errorMessage ?? null;
    },
    async getImportJob() {
      return {
        import_id: "import-real-core",
        upload_id: "package-upload",
        import_type: "sealed_package",
        status: state.importStatus,
        label: "real-core-smoke",
        package_digest: state.packageDigest,
        manifest_json: state.manifestJson,
        resolved_experiment_json: state.resolvedExperimentJson,
        created_at: "2026-06-04T00:00:00Z",
        updated_at: "2026-06-04T00:00:00Z",
        error_message: state.errorMessage,
        diagnostics: state.diagnostics,
      };
    },
  };
}

function capturingPackageRepository(): Pick<PackageRepository, "upsertArtifact" | "getArtifact"> & { artifact: Record<string, unknown> | null } {
  const repository = {
    artifact: null as Record<string, unknown> | null,
    async upsertArtifact(input: Record<string, unknown>) {
      repository.artifact = {
        ...input,
        package_digest: input.packageDigest,
        upload_id: input.uploadId,
        storage_path: input.storagePath,
        byte_size: input.byteSize,
        media_type: input.mediaType,
        manifest_json: input.manifestJson,
        resolved_experiment_json: input.resolvedExperimentJson,
        image_refs: input.imageRefs,
        status: "accepted",
        created_at: "2026-06-04T00:00:00Z",
        updated_at: "2026-06-04T00:00:00Z",
      };
      return repository.artifact as never;
    },
    async getArtifact(packageDigest: string) {
      expect(repository.artifact?.package_digest).toBe(packageDigest);
      return {
        package_digest: repository.artifact?.package_digest,
        upload_id: repository.artifact?.upload_id,
        storage_path: repository.artifact?.storage_path,
        byte_size: repository.artifact?.byte_size,
        media_type: repository.artifact?.media_type,
        manifest_json: repository.artifact?.manifest_json,
        resolved_experiment_json: repository.artifact?.resolved_experiment_json,
        target: repository.artifact?.target,
        image_refs: repository.artifact?.image_refs,
        diagnostics: repository.artifact?.diagnostics,
        status: "accepted",
        created_at: "2026-06-04T00:00:00Z",
        updated_at: "2026-06-04T00:00:00Z",
      } as never;
    },
  };
  return repository;
}

function runnersWithDockerPool(): Pick<RunnerRepository, "listPools"> {
  return {
    async listPools(): Promise<RunnerPoolRecord[]> {
      return [{
        runner_pool_id: "pool-1",
        name: "docker-pool",
        status: "active",
        active_worker_image_id: null,
        capabilities: {
          executors: ["runner-docker"],
          resources: ["core_runner", "docker_daemon", "registry_pull"],
          arch: "x86_64",
          cpu_count: 16,
          memory_mb: 65536,
          disk_mb: 200000,
          isolation: ["reusable_vm", "single_use_vm"],
        },
        metadata: {},
        created_at: "2026-06-04T00:00:00Z",
        updated_at: "2026-06-04T00:00:00Z",
      }];
    },
  };
}

function authContext(subject: string): AuthContext {
  return {
    subject,
    issuer: "https://issuer.example",
    audience: ["cloud"],
    claims: {},
  };
}

function restoreEnv(name: string, previous: string | undefined): void {
  if (previous === undefined) {
    delete process.env[name];
  } else {
    process.env[name] = previous;
  }
}
