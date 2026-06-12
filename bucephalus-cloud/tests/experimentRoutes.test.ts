import { describe, expect, test } from "bun:test";
import { chmod, mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import * as tar from "tar";
import { handleExperimentRoute } from "../src/routes/experiments";
import type { AuthContext } from "../src/auth";
import { HttpError } from "../src/http";
import type { ImportRepository } from "../src/imports/repository";
import type { PackageRepository, RunRepository } from "../src/packages/repository";
import type { RunnerRepository } from "../src/runners/repository";
import type { CloudSecretRepository } from "../src/secrets/repository";
import { canonicalJsonStringify, sha256Digest } from "../src/primitives";

describe("Hosted experiment routes", () => {
  test("hosted authoring build looks up source uploads under the authenticated owner before building", async () => {
    const observed: { uploadId?: string; ownerKey: string | null | undefined } = { ownerKey: undefined };
    const imports = {
      async getUpload(uploadId: string, ownerKey?: string | null) {
        observed.uploadId = uploadId;
        observed.ownerKey = ownerKey;
        return null;
      },
    };

    await expect(handleExperimentRoute(
      new Request("https://cloud.example/v1/experiments/builds", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          input_kind: "authoring_context",
          upload_id: "source-upload",
          entrypoint: "experiment.yaml",
        }),
      }),
      new URL("https://cloud.example/v1/experiments/builds"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
      {} as RunRepository,
      {} as RunnerRepository,
      authContext("user-a"),
      {} as CloudSecretRepository,
    )).rejects.toMatchObject({
      status: 404,
      code: "upload_not_found",
      message: "Upload not found",
    });
    expect(observed).toEqual({
      uploadId: "source-upload",
      ownerKey: "issuer:user-a",
    });
  });

  test("sealed package build looks up uploaded packages under the authenticated owner before import", async () => {
    const observed: { uploadId?: string; ownerKey: string | null | undefined } = { ownerKey: undefined };
    const imports = {
      async getUpload(uploadId: string, ownerKey?: string | null) {
        observed.uploadId = uploadId;
        observed.ownerKey = ownerKey;
        return null;
      },
    };

    await expect(handleExperimentRoute(
      new Request("https://cloud.example/v1/experiments/builds", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          input_kind: "sealed_package",
          upload_id: "package-upload",
        }),
      }),
      new URL("https://cloud.example/v1/experiments/builds"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
      {} as RunRepository,
      {} as RunnerRepository,
      authContext("user-b"),
      {} as CloudSecretRepository,
    )).rejects.toMatchObject({
      status: 404,
      code: "upload_not_found",
      message: "Upload not found",
    });
    expect(observed).toEqual({
      uploadId: "package-upload",
      ownerKey: "issuer:user-b",
    });
  });

  test("hosted authoring build runs Core, imports the produced package, and reports Cloud readiness", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    const previousDatabaseUrl = process.env.DATABASE_URL;
    const previousWorkerToken = process.env.BUCEPHALUS_CLOUD_WORKER_TOKEN;
    const previousImageDigest = process.env.BUCEPHALUS_CLOUD_API_IMAGE_DIGEST;
    const previousReleaseVersion = process.env.BUCEPHALUS_CLOUD_RELEASE_VERSION;
    const previousReleaseGitSha = process.env.BUCEPHALUS_RELEASE_GIT_SHA;
    const previousCoreVersion = process.env.BUCEPHALUS_CLOUD_CORE_VERSION;
    const previousEvidencePolicy = process.env.BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      process.env.DATABASE_URL = "postgres://should-not-leak";
      process.env.BUCEPHALUS_CLOUD_WORKER_TOKEN = "should-not-leak";
      process.env.BUCEPHALUS_CLOUD_API_IMAGE_DIGEST = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
      process.env.BUCEPHALUS_CLOUD_RELEASE_VERSION = "0.3.37";
      process.env.BUCEPHALUS_RELEASE_GIT_SHA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
      process.env.BUCEPHALUS_CLOUD_CORE_VERSION = "0.3.37";
      process.env.BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY = "warn";
      const { packageDir, packageDigest } = await writePackageDirectory(root);
      const corePath = await writeFakeCoreBuilder(root, packageDir);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root);
      const imports = importsForHostedAuthoringBuild(contextArchive, packageDigest);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
            label: "demo",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        imports as unknown as ImportRepository,
        packagesRecordingArtifact(packageDigest) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      expect(response!.status).toBe(201);
      const body = await response!.json();
      expect(body.build_kind).toBe("hosted_authoring_build");
      expect(body.build_environment).toEqual(expect.objectContaining({
        schema_version: "hosted_build_environment_v1",
        target: { kind: "hosted_cloud", name: "default" },
        source: {
          input_kind: "authoring_context",
          upload_id: "source-upload",
          filename: "authoring-context.tgz",
          media_type: "application/gzip",
          content_digest: sha256Digest(await readFile(contextArchive)),
          byte_size: (await readFile(contextArchive)).byteLength,
          entrypoint: "experiment.yaml",
        },
        runtime_options: {},
        builder: expect.objectContaining({
          kind: "hosted_authoring_builder",
          image_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          release_version: "0.3.37",
          git_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }),
        core: expect.objectContaining({
          executed: true,
          command: "bucephalus build",
          path: corePath,
          version: "0.3.37",
          timeout_ms: 600000,
        }),
        package_contract: {
          input_kind: "authoring_context",
          authoring_compiler: "core_universal_v1",
          authoring_provenance: {
            status: "hosted_attested",
            source: "hosted_core",
            message: "Cloud ran hosted Core authoring for this package and recorded the builder/core environment.",
          },
          sealed_schema_version: "sealed_run_package_v2",
          readiness_schema_version: "hosted_cloud_readiness_v1",
          cloud_readiness_required: true,
        },
        evidence: expect.objectContaining({
          policy: "warn",
          status: "complete",
          missing: [],
        }),
      }));
      expect(body.build_environment.evidence.checks).toContainEqual(expect.objectContaining({
        name: "builder_image_digest",
        status: "passed",
        code: "builder_image_digest_recorded",
      }));
      expect(body.authoring_build.status).toBe("succeeded");
      expect(body.authoring_build.source_upload_id).toBe("source-upload");
      expect(body.authoring_build.entrypoint).toBe("experiment.yaml");
      expect(body.status).toBe("cloud_runnable");
      expect(body.package_digest).toBe(packageDigest);
      expect(body.import.status).toBe("accepted");
      expect(body.cloud_readiness.status).toBe("cloud_runnable");
      expect(body.cloud_readiness.package_provenance).toEqual(expect.objectContaining({
        status: "hosted_attested",
        source: "hosted_core",
        input_kind: "authoring_context",
        source_upload_id: "source-upload",
      }));
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      restoreEnv("DATABASE_URL", previousDatabaseUrl);
      restoreEnv("BUCEPHALUS_CLOUD_WORKER_TOKEN", previousWorkerToken);
      restoreEnv("BUCEPHALUS_CLOUD_API_IMAGE_DIGEST", previousImageDigest);
      restoreEnv("BUCEPHALUS_CLOUD_RELEASE_VERSION", previousReleaseVersion);
      restoreEnv("BUCEPHALUS_RELEASE_GIT_SHA", previousReleaseGitSha);
      restoreEnv("BUCEPHALUS_CLOUD_CORE_VERSION", previousCoreVersion);
      restoreEnv("BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY", previousEvidencePolicy);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build reports Core build failures without importing a package", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-fail-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-fail.sh");
      await writeFile(corePath, [
        "#!/bin/sh",
        "echo bad authoring >&2",
        "echo 'api_key=sk-abcdefghijklmnopqrstuvwxyz' >&2",
        "echo 'secret_ref=gcp-secret-manager://projects/acme/secrets/demo/versions/latest'",
        "exit 42",
        "",
      ].join("\n"));
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root);
      const imports = importsForHostedAuthoringBuild(contextArchive, digest());
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        imports as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.build_kind).toBe("hosted_authoring_build");
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.status).toBe("failed");
      expect(body.authoring_build.source_upload_id).toBe("source-upload");
      expect(body.authoring_build.entrypoint).toBe("experiment.yaml");
      expect(body.authoring_build.code).toBe("authoring_build_failed");
      expect(body.authoring_build.detail.stderr_tail).toContain("bad authoring");
      expect(body.authoring_build.detail.stderr_tail).toContain("api_key=[redacted]");
      expect(body.authoring_build.detail.stderr_tail).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
      expect(body.authoring_build.detail.stdout_tail).toContain("[redacted]");
      expect(body.authoring_build.detail.stdout_tail).not.toContain("gcp-secret-manager://");
      expect(body.cloud_readiness.status).toBe("unavailable");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build rejects unsafe entrypoints before fetching uploads or recording build evidence", async () => {
    const imports = {
      getUpload() {
        throw new Error("upload lookup should not run for an invalid authoring entrypoint");
      },
    };

    await expect(handleExperimentRoute(
      new Request("https://cloud.example/v1/experiments/builds", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          input_kind: "authoring_context",
          upload_id: "source-upload",
          entrypoint: "../experiment.yaml",
        }),
      }),
      new URL("https://cloud.example/v1/experiments/builds"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
      {} as RunRepository,
      {} as RunnerRepository,
      authContext("user-a"),
      {} as CloudSecretRepository,
    )).rejects.toMatchObject({
      status: 400,
      code: "invalid_authoring_context_path",
      message: "/entrypoint must be a relative POSIX path without empty, current, or parent components",
    });
  });

  test("hosted authoring build reports missing package output after successful Core", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-missing-output-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-no-output.sh");
      await writeFile(corePath, "#!/bin/sh\necho success without package\nexit 0\n");
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(contextArchive, digest()) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.code).toBe("authoring_build_missing_package");
      expect(body.authoring_build.error).toContain("did not create a package output directory");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build reports invalid package output path after successful Core", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-invalid-output-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-file-output.sh");
      await writeFile(corePath, [
        "#!/bin/sh",
        "out=\"\"",
        "while [ \"$#\" -gt 0 ]; do",
        "  if [ \"$1\" = \"--out\" ]; then out=\"$2\"; shift 2; else shift; fi",
        "done",
        "printf 'not a package dir' > \"$out\"",
        "exit 0",
        "",
      ].join("\n"));
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(contextArchive, digest()) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.code).toBe("authoring_build_invalid_package");
      expect(body.authoring_build.error).toContain("package output path is not a directory");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build reports empty package output after successful Core", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-empty-output-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-empty-output.sh");
      await writeFile(corePath, [
        "#!/bin/sh",
        "out=\"\"",
        "while [ \"$#\" -gt 0 ]; do",
        "  if [ \"$1\" = \"--out\" ]; then out=\"$2\"; shift 2; else shift; fi",
        "done",
        "mkdir -p \"$out\"",
        "exit 0",
        "",
      ].join("\n"));
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(contextArchive, digest()) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.code).toBe("authoring_build_empty_package");
      expect(body.authoring_build.error).toContain("empty package directory");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build times out stuck Core builders without importing a package", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-timeout-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    const previousTimeout = process.env.BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      process.env.BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS = "250";
      const corePath = join(root, "fake-core-hangs.sh");
      await writeFile(corePath, "#!/bin/sh\necho starting hosted build\nsleep 5\necho should-not-finish\n");
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(contextArchive, digest()) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.build_kind).toBe("hosted_authoring_build");
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.code).toBe("authoring_build_timed_out");
      expect(body.authoring_build.detail.timeout_ms).toBe(250);
      expect(body.authoring_build.detail.stdout_tail).not.toContain("should-not-finish");
      expect(body.cloud_readiness.status).toBe("unavailable");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      restoreEnv("BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS", previousTimeout);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build rejects blocked context paths before Core runs", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-blocked-context-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-should-not-run.sh");
      await writeFile(corePath, "#!/bin/sh\necho should-not-run >&2\nexit 99\n");
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root, { includeDotenv: true });
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(contextArchive, digest()) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.source_upload_id).toBe("source-upload");
      expect(body.authoring_build.entrypoint).toBe("experiment.yaml");
      expect(body.authoring_build.code).toBe("blocked_authoring_context_path");
      expect(body.authoring_build.error).not.toContain("should-not-run");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build rejects non-archive source uploads before extraction or Core", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-non-archive-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-should-not-run.sh");
      await writeFile(corePath, "#!/bin/sh\necho should-not-run >&2\nexit 99\n");
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(contextArchive, digest(), {
          sourceFilename: "experiment.yaml",
          sourceMediaType: "application/x-yaml",
        }) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.code).toBe("invalid_authoring_context_upload");
      expect(body.authoring_build.detail.filename).toBe("experiment.yaml");
      expect(body.authoring_build.detail.media_type).toBe("application/x-yaml");
      expect(body.authoring_build.error).not.toContain("should-not-run");
      expect(body.cloud_readiness.checks).toContainEqual(expect.objectContaining({
        name: "authoring_build",
        status: "blocked",
        code: "invalid_authoring_context_upload",
      }));
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build rejects source objects whose bytes do not match upload digest", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-digest-mismatch-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-should-not-run.sh");
      await writeFile(corePath, "#!/bin/sh\necho should-not-run >&2\nexit 99\n");
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(contextArchive, digest(), {
          sourceContentDigest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        }) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.code).toBe("authoring_context_source_digest_mismatch");
      expect(body.authoring_build.detail.expected_digest).toBe("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
      expect(body.authoring_build.detail.content_digest).toBe(sha256Digest(await readFile(contextArchive)));
      expect(body.authoring_build.error).not.toContain("should-not-run");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build rejects missing source digest before touching object storage", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-missing-digest-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-should-not-run.sh");
      await writeFile(corePath, "#!/bin/sh\necho should-not-run >&2\nexit 99\n");
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      await expect(handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(join(root, "missing-source.tgz"), digest(), {
          sourceContentDigest: null,
          sourceByteSize: 123,
          sourceStoragePath: join(root, "missing-source.tgz"),
        }) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      )).rejects.toMatchObject({
        status: 409,
        code: "invalid_build_source_upload",
        message: "Hosted build source upload is missing content_digest",
      });
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build rejects missing source size before touching object storage", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-missing-size-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-should-not-run.sh");
      await writeFile(corePath, "#!/bin/sh\necho should-not-run >&2\nexit 99\n");
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      await expect(handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(join(root, "missing-source.tgz"), digest(), {
          sourceContentDigest: "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
          sourceByteSize: null,
          sourceStoragePath: join(root, "missing-source.tgz"),
        }) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      )).rejects.toMatchObject({
        status: 409,
        code: "invalid_build_source_upload",
        message: "Hosted build source upload is missing byte_size",
      });
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build rejects source objects whose bytes do not match upload size", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-size-mismatch-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-should-not-run.sh");
      await writeFile(corePath, "#!/bin/sh\necho should-not-run >&2\nexit 99\n");
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root);
      const actualBytes = (await readFile(contextArchive)).byteLength;
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(contextArchive, digest(), {
          sourceByteSize: actualBytes + 1,
        }) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.code).toBe("authoring_context_source_size_mismatch");
      expect(body.authoring_build.detail.expected_byte_size).toBe(actualBytes + 1);
      expect(body.authoring_build.detail.byte_size).toBe(actualBytes);
      expect(body.authoring_build.error).not.toContain("should-not-run");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("hosted authoring build rejects generated context directories before Core runs", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-build-generated-context-"));
    const previousCore = process.env.BUCEPHALUS_CLOUD_CORE_CLI;
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const corePath = join(root, "fake-core-should-not-run.sh");
      await writeFile(corePath, "#!/bin/sh\necho should-not-run >&2\nexit 99\n");
      await chmod(corePath, 0o755);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = corePath;
      const contextArchive = await writeAuthoringContextArchive(root, { includeNodeModules: true });
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            input_kind: "authoring_context",
            upload_id: "source-upload",
            entrypoint: "experiment.yaml",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsForHostedAuthoringBuild(contextArchive, digest()) as unknown as ImportRepository,
        packagesRecordingArtifact(digest()) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.status).toBe("failed");
      expect(body.import).toBeNull();
      expect(body.authoring_build.code).toBe("blocked_authoring_context_path");
      expect(body.authoring_build.detail.path).toBe("node_modules");
      expect(body.authoring_build.error).not.toContain("should-not-run");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_CORE_CLI", previousCore);
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("build response separates sealed package import from hosted Cloud readiness", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-"));
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
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
      expect(body.build_environment.package_contract.input_kind).toBe("sealed_package");
      expect(body.build_environment.package_contract.authoring_compiler).toBeNull();
      expect(body.build_environment.package_contract.authoring_provenance).toEqual({
        status: "external_unattested",
        source: "sealed_package_manifest",
        message: "Cloud verified sealed package integrity and hosted readiness, but sealed_run_package_v2 does not attest the package's original authoring environment.",
      });
      expect(body.build_environment.target).toEqual({ kind: "hosted_cloud", name: "default" });
      expect(body.build_environment.source).toEqual({
        input_kind: "sealed_package",
        upload_id: "upload-1",
        filename: "package.tgz",
        media_type: "application/gzip",
        content_digest: packageDigest,
        byte_size: 1,
      });
      expect(body.build_environment.runtime_options).toEqual({});
      expect(body.build_environment.builder.kind).toBe("sealed_package_importer");
      expect(body.build_environment.core).toEqual({
        executed: false,
        command: null,
        path: null,
        version: null,
        timeout_ms: null,
        reason: "Sealed package input was imported directly; Cloud did not run hosted Core authoring.",
      });
      expect(body.authoring_build.status).toBe("unavailable");
      expect(body.authoring_build.message).toContain("Sealed package input was imported directly");
      expect(body.authoring_build.source_upload_id).toBeUndefined();
      expect(body.authoring_build.entrypoint).toBeUndefined();
      expect(body.status).toBe("cloud_runnable");
      expect(body.package_digest).toBe(packageDigest);
      expect(body.cloud_readiness.status).toBe("cloud_runnable");
      expect(body.cloud_readiness.package_provenance).toEqual(expect.objectContaining({
        status: "external_unattested",
        source: "sealed_package_manifest",
        input_kind: "sealed_package",
        source_upload_id: "upload-1",
      }));
      expect(body.cloud_readiness.target).toEqual({ kind: "hosted_cloud", name: "default" });
      expect(body.cloud_readiness.secret_requirements).toEqual([
        {
          id: "GEMINI_API_KEY",
          target: "",
          required_for_variants: [],
        },
      ]);
      expect(body.cloud_readiness.required_actions).toContainEqual({
        action: "upload_hosted_secret",
        stage: "before_run",
        requirement_id: "GEMINI_API_KEY",
        description: "Upload hosted secret 'GEMINI_API_KEY' before creating a run, then pass --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY.",
        command: "buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY",
        blocking: false,
      });
      expect(body.cloud_readiness.run_requirements.requires).toContain("secret_resolver");
      expect(body.cloud_readiness.checks.map((check: { name: string; status: string }) => [check.name, check.status])).toContainEqual(["secrets", "warning"]);
      expect(body.import.import_type).toBe("sealed_package");
      expect(body.import.status).toBe("accepted");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("accepted imports without package digests are cloud-blocked instead of accepted", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-missing-digest-"));
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
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
        importsAcceptingCompletedUploadWithoutPackageDigest(archivePath, packageDigest) as unknown as ImportRepository,
        packagesRecordingArtifact(packageDigest) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.status).toBe("cloud_blocked");
      expect(body.package_digest).toBeNull();
      expect(body.cloud_readiness.status).toBe("cloud_blocked");
      expect(body.cloud_readiness.checks).toContainEqual(expect.objectContaining({
        name: "package_import",
        status: "blocked",
        code: "package_digest_missing_after_import",
      }));
      expect(body.cloud_readiness.required_actions).toContainEqual(expect.objectContaining({
        action: "contact_support",
        stage: "operator",
        blocking: true,
      }));
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("sealed package build rejects completed uploads missing source digest", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-missing-digest-"));
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const { archivePath, packageDigest } = await writePackageArchive(root);
      await expect(handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            upload_id: "upload-1",
            label: "demo",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsAcceptingCompletedUpload(archivePath, packageDigest, {
          contentDigest: null,
        }) as unknown as ImportRepository,
        packagesRecordingArtifact(packageDigest) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      )).rejects.toMatchObject({
        status: 409,
        code: "invalid_build_source_upload",
        message: "Hosted build source upload is missing content_digest",
      });
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("sealed package build rejects completed uploads missing source byte size", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-missing-size-"));
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const { archivePath, packageDigest } = await writePackageArchive(root);
      await expect(handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            upload_id: "upload-1",
            label: "demo",
          }),
        }),
        new URL("https://cloud.example/v1/experiments/builds"),
        importsAcceptingCompletedUpload(archivePath, packageDigest, {
          byteSize: null,
        }) as unknown as ImportRepository,
        packagesRecordingArtifact(packageDigest) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      )).rejects.toMatchObject({
        status: 409,
        code: "invalid_build_source_upload",
        message: "Hosted build source upload is missing byte_size",
      });
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("build environment reports partial provenance as readiness warnings", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-partial-evidence-"));
    const evidenceEnvNames = [
      "BUCEPHALUS_CLOUD_BUILDER_IMAGE_DIGEST",
      "BUCEPHALUS_CLOUD_API_IMAGE_DIGEST",
      "BUCEPHALUS_CLOUD_RELEASE_VERSION",
      "BUCEPHALUS_RELEASE_VERSION",
      "BUCEPHALUS_CLOUD_GIT_SHA",
      "BUCEPHALUS_GIT_SHA",
      "BUCEPHALUS_RELEASE_GIT_SHA",
      "GITHUB_SHA",
      "BUCEPHALUS_CLOUD_CORE_VERSION",
      "BUCEPHALUS_CORE_VERSION",
      "BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY",
    ];
    const previousEnv = Object.fromEntries(
      evidenceEnvNames.map((name) => [name, process.env[name]]),
    );
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      for (const name of evidenceEnvNames) {
        delete process.env[name];
      }
      process.env.BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY = "warn";
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
      const body = await response!.json();
      expect(body.status).toBe("cloud_runnable");
      expect(body.build_environment.evidence.policy).toBe("warn");
      expect(body.build_environment.evidence.status).toBe("partial");
      expect(body.build_environment.evidence.missing).toEqual([
        "builder_image_digest",
        "builder_release_version",
        "builder_git_sha",
      ]);
      expect(body.cloud_readiness.checks).toContainEqual(expect.objectContaining({
        name: "build_environment",
        status: "warning",
        code: "builder_image_digest_missing",
        detail: expect.objectContaining({
          evidence: "builder_image_digest",
          evidence_policy: "warn",
          evidence_status: "partial",
        }),
      }));
    } finally {
      for (const [name, value] of Object.entries(previousEnv)) {
        restoreEnv(name, value);
      }
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("build evidence enforcement blocks otherwise runnable packages with partial provenance", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-enforced-evidence-"));
    const evidenceEnvNames = [
      "BUCEPHALUS_CLOUD_BUILDER_IMAGE_DIGEST",
      "BUCEPHALUS_CLOUD_API_IMAGE_DIGEST",
      "BUCEPHALUS_CLOUD_RELEASE_VERSION",
      "BUCEPHALUS_RELEASE_VERSION",
      "BUCEPHALUS_CLOUD_GIT_SHA",
      "BUCEPHALUS_GIT_SHA",
      "BUCEPHALUS_RELEASE_GIT_SHA",
      "GITHUB_SHA",
      "BUCEPHALUS_CLOUD_CORE_VERSION",
      "BUCEPHALUS_CORE_VERSION",
      "BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY",
    ];
    const previousEnv = Object.fromEntries(
      evidenceEnvNames.map((name) => [name, process.env[name]]),
    );
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      for (const name of evidenceEnvNames) {
        delete process.env[name];
      }
      process.env.BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY = "enforce";
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
      const body = await response!.json();
      expect(body.import.status).toBe("accepted");
      expect(body.build_environment.evidence.policy).toBe("enforce");
      expect(body.build_environment.evidence.status).toBe("partial");
      expect(body.build_environment.evidence.missing).not.toContain("core_version");
      expect(body.status).toBe("cloud_blocked");
      expect(body.cloud_readiness.status).toBe("cloud_blocked");
      expect(body.cloud_readiness.checks).toContainEqual(expect.objectContaining({
        name: "build_environment",
        status: "blocked",
        code: "builder_image_digest_missing",
      }));
      expect(body.cloud_readiness.required_actions).toContainEqual(expect.objectContaining({
        action: "complete_build_environment_evidence",
        stage: "operator",
        blocking: true,
      }));
    } finally {
      for (const [name, value] of Object.entries(previousEnv)) {
        restoreEnv(name, value);
      }
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("build reports cloud_blocked when an imported package has local-only image refs", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-blocked-"));
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
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
        packagesRecordingArtifact(packageDigest, {
          image_refs: ["peter-gregory-v2-nova:local"],
        }) as unknown as PackageRepository,
        {} as RunRepository,
        runnersWithDockerPool() as unknown as RunnerRepository,
        authContext("user-a"),
        {} as CloudSecretRepository,
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      expect(body.import.status).toBe("accepted");
      expect(body.status).toBe("cloud_blocked");
      expect(body.cloud_readiness.status).toBe("cloud_blocked");
      expect(body.cloud_readiness.checks).toContainEqual(expect.objectContaining({
        name: "runtime_contract",
        status: "blocked",
        code: "package_images_not_cloud_pinned",
      }));
      expect(body.cloud_readiness.required_actions).toContainEqual(expect.objectContaining({
        action: "package_images_not_cloud_pinned",
        stage: "before_rebuild",
        blocking: true,
      }));
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("build reports cloud_blocked when runtime options contain ignored typos", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-runtime-typo-"));
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const { archivePath, packageDigest } = await writePackageArchive(root);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            upload_id: "upload-1",
            label: "demo",
            runtime_options: {
              memory_mbb: 8192,
            },
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
      const body = await response!.json();
      expect(body.import.status).toBe("accepted");
      expect(body.status).toBe("cloud_blocked");
      expect(body.cloud_readiness.status).toBe("cloud_blocked");
      expect(body.cloud_readiness.checks).toContainEqual(expect.objectContaining({
        name: "runtime_contract",
        status: "blocked",
        code: "unknown_cloud_runtime_option",
      }));
      expect(body.cloud_readiness.required_actions).toContainEqual(expect.objectContaining({
        action: "unknown_cloud_runtime_option",
        stage: "before_rebuild",
        blocking: true,
      }));
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("build reports cloud_blocked when no hosted runner pool can satisfy resources", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-experiment-build-unschedulable-"));
    const previousDataDir = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    try {
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "data");
      const { archivePath, packageDigest } = await writePackageArchive(root);
      const response = await handleExperimentRoute(
        new Request("https://cloud.example/v1/experiments/builds", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            upload_id: "upload-1",
            label: "demo",
            runtime_options: {
              memory_mb: 131072,
            },
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
      const body = await response!.json();
      expect(body.import.status).toBe("accepted");
      expect(body.status).toBe("cloud_blocked");
      expect(body.build_environment.runtime_options).toEqual({
        memory_mb: 131072,
      });
      expect(body.cloud_readiness.run_requirements.memory_mb).toBe(131072);
      expect(body.cloud_readiness.checks).toContainEqual(expect.objectContaining({
        name: "runner_capacity",
        status: "blocked",
        code: "run_unschedulable",
      }));
      expect(body.cloud_readiness.required_actions).toContainEqual(expect.objectContaining({
        action: "run_unschedulable",
        stage: "operator",
        blocking: true,
      }));
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previousDataDir);
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
    expect(body.package_provenance).toEqual(cloudReadyPackage().package_provenance);
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

  test("doctor rejects malformed package digests before package lookup", async () => {
    const packages = {
      async getArtifact() {
        throw new Error("getArtifact should not be called");
      },
    };

    await expect(handleExperimentRoute(
      new Request("https://cloud.example/v1/experiments/doctor", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:short",
        }),
      }),
      new URL("https://cloud.example/v1/experiments/doctor"),
      {} as ImportRepository,
      packages as unknown as PackageRepository,
      {} as RunRepository,
      runnersWithDockerPool() as unknown as RunnerRepository,
      authContext("user-a"),
      {} as CloudSecretRepository,
    )).rejects.toMatchObject({
      status: 400,
      code: "invalid_request",
      message: "/package_digest must be sha256:<64 lowercase hex chars>",
    });
  });

  test("doctor rejects unknown runtime options with an actionable pointer", async () => {
    await expect(handleExperimentRoute(
      new Request("https://cloud.example/v1/experiments/doctor", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: digest(),
          secret_refs: {
            GEMINI_API_KEY: "gcp-secret-manager://projects/acme/secrets/gemini/versions/latest",
          },
          runtime_options: {
            memory_mbb: 8192,
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
    )).rejects.toThrow("/runtime_options/memory_mbb is not supported");
  });

  test("doctor rejects missing secret refs with package requirement detail", async () => {
    try {
      await handleExperimentRoute(
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
      );
      throw new Error("doctor should reject missing package secrets");
    } catch (error) {
      expect(error).toBeInstanceOf(HttpError);
      const httpError = error as HttpError;
      expect(httpError.code).toBe("invalid_secret_refs");
      expect(httpError.message).toBe("Run secret refs must match the package secret requirements");
      expect(httpError.detail?.missing_secret_ids).toEqual(["GEMINI_API_KEY"]);
      expect(httpError.detail?.next_commands).toEqual([
        "buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY",
        "buc run <package-digest> --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY",
      ]);
      expect(httpError.detail?.required_actions).toContainEqual({
        action: "upload_hosted_secret",
        stage: "before_run",
        requirement_id: "GEMINI_API_KEY",
        description: "Upload hosted secret 'GEMINI_API_KEY' before creating a run, then pass --secret-ref GEMINI_API_KEY=bucephalus://GEMINI_API_KEY.",
        command: "buc secrets put GEMINI_API_KEY --from-env GEMINI_API_KEY",
        blocking: true,
      });
    }
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
  options: {
    contentDigest?: string | null;
    byteSize?: number | null;
  } = {},
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
        content_digest: options.contentDigest === undefined ? packageDigest : options.contentDigest,
        byte_size: options.byteSize === undefined ? 1 : options.byteSize,
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

function importsAcceptingCompletedUploadWithoutPackageDigest(
  archivePath: string,
  packageDigest: string,
): Pick<ImportRepository, "getUpload" | "createImportJob" | "updateImportInspection" | "getImportJob"> {
  const imports = importsAcceptingCompletedUpload(archivePath, packageDigest);
  return {
    ...imports,
    async getImportJob() {
      const job = await imports.getImportJob("import-1");
      return {
        ...job!,
        status: "accepted",
        package_digest: null,
      };
    },
  };
}

function importsForHostedAuthoringBuild(
  sourceArchivePath: string,
  packageDigest: string,
  options: {
    sourceFilename?: string;
    sourceMediaType?: string;
    sourceContentDigest?: string | null;
    sourceByteSize?: number | null;
    sourceStoragePath?: string;
  } = {},
): Pick<ImportRepository, "getUpload" | "createUpload" | "markUploaded" | "completeUpload" | "createImportJob" | "updateImportInspection" | "getImportJob"> {
  let status = "inspecting";
  let packageStoragePath: string | null = null;
  let packageByteSize = 0;
  return {
    async getUpload(uploadId: string) {
      if (uploadId === "source-upload") {
        const sourceBytes = options.sourceContentDigest === undefined || options.sourceByteSize === undefined
          ? await readFile(sourceArchivePath)
          : null;
        return {
          upload_id: "source-upload",
          filename: options.sourceFilename ?? "authoring-context.tgz",
          media_type: options.sourceMediaType ?? "application/gzip",
          expected_digest: null,
          content_digest: options.sourceContentDigest === undefined
            ? sha256Digest(sourceBytes!)
            : options.sourceContentDigest,
          byte_size: options.sourceByteSize === undefined
            ? sourceBytes!.byteLength
            : options.sourceByteSize,
          storage_path: options.sourceStoragePath ?? sourceArchivePath,
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
        expected_digest: null,
        content_digest: packageDigest,
        byte_size: packageByteSize,
        storage_path: packageStoragePath,
        status: packageStoragePath ? "completed" : "created",
        created_at: "2026-06-04T00:00:00Z",
        uploaded_at: packageStoragePath ? "2026-06-04T00:00:00Z" : null,
        completed_at: packageStoragePath ? "2026-06-04T00:00:00Z" : null,
        error_message: null,
      };
    },
    async createUpload(input: { filename: string; mediaType: string }) {
      expect(input.filename).toBe("package.tgz");
      expect(input.mediaType).toBe("application/gzip");
      return {
        upload_id: "package-upload",
        filename: "package.tgz",
        media_type: "application/gzip",
        expected_digest: null,
        content_digest: null,
        byte_size: null,
        storage_path: null,
        status: "created",
        created_at: "2026-06-04T00:00:00Z",
        uploaded_at: null,
        completed_at: null,
        error_message: null,
      };
    },
    async markUploaded(input: { storagePath: string; byteSize: number }) {
      packageStoragePath = input.storagePath;
      packageByteSize = input.byteSize;
      return {
        upload_id: "package-upload",
        filename: "package.tgz",
        media_type: "application/gzip",
        expected_digest: null,
        content_digest: packageDigest,
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
        expected_digest: null,
        content_digest: packageDigest,
        byte_size: packageByteSize,
        storage_path: packageStoragePath,
        status: "completed",
        created_at: "2026-06-04T00:00:00Z",
        uploaded_at: "2026-06-04T00:00:00Z",
        completed_at: "2026-06-04T00:00:00Z",
        error_message: null,
      };
    },
    async createImportJob(input: { uploadId: string; label?: string | null }) {
      expect(input.uploadId).toBe("package-upload");
      expect(input.label).toBe("demo");
      return "import-1";
    },
    async updateImportInspection(input: { status: "accepted" | "failed" }) {
      status = input.status;
    },
    async getImportJob() {
      return {
        import_id: "import-1",
        upload_id: "package-upload",
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

function packagesRecordingArtifact(
  packageDigest: string,
  overrides: Record<string, unknown> = {},
): Pick<PackageRepository, "upsertArtifact" | "getArtifact"> {
  let artifact: Record<string, unknown> | null = null;
  return {
    async upsertArtifact(input: { packageDigest: string; packageProvenance: Record<string, unknown> }) {
      expect(input.packageDigest).toBe(packageDigest);
      artifact = {
        ...cloudReadyPackage(),
        package_digest: packageDigest,
        package_provenance: input.packageProvenance,
        ...overrides,
      };
      return artifact as never;
    },
    async getArtifact() {
      return (artifact ?? {
        ...cloudReadyPackage(),
        package_digest: packageDigest,
        ...overrides,
      }) as never;
    },
  };
}

async function writePackageArchive(root: string): Promise<{ archivePath: string; packageDigest: string }> {
  const { packageDir, packageDigest } = await writePackageDirectory(root);
  const archivePath = join(root, "package.tgz");
  await tar.c({ gzip: true, cwd: packageDir, file: archivePath }, (await readdir(packageDir)).sort());
  return { archivePath, packageDigest };
}

async function writePackageDirectory(root: string): Promise<{ packageDir: string; packageDigest: string }> {
  const packageDir = join(root, "package");
  await mkdir(packageDir, { recursive: true });
  const resolvedExperiment = cloudReadyPackage().resolved_experiment_json;
  await writeFile(join(packageDir, "resolved_experiment.json"), JSON.stringify(resolvedExperiment));
  await writeFile(join(packageDir, "staging_manifest.json"), JSON.stringify({
    schema_version: "runtime_path_staging_manifest_v1",
    variants: {
      baseline: [],
    },
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
  return { packageDir, packageDigest };
}

async function writeAuthoringContextArchive(root: string, options: { includeDotenv?: boolean; includeNodeModules?: boolean } = {}): Promise<string> {
  const contextDir = join(root, "authoring-context");
  await mkdir(contextDir, { recursive: true });
  await writeFile(join(contextDir, "experiment.yaml"), "experiment:\n  id: demo\n  name: Demo\n");
  await writeFile(join(contextDir, "cases.jsonl"), "{}\n");
  if (options.includeDotenv) {
    await writeFile(join(contextDir, ".env"), "SECRET=not-for-cloud\n");
  }
  if (options.includeNodeModules) {
    await mkdir(join(contextDir, "node_modules/pkg"), { recursive: true });
    await writeFile(join(contextDir, "node_modules/pkg/index.js"), "not for cloud\n");
  }
  const archivePath = join(root, "authoring-context.tgz");
  await tar.c({ gzip: true, cwd: contextDir, file: archivePath }, (await readdir(contextDir)).sort());
  return archivePath;
}

async function writeFakeCoreBuilder(root: string, packageDir: string): Promise<string> {
  const corePath = join(root, "fake-core.sh");
  await writeFile(corePath, [
    "#!/bin/sh",
    "set -eu",
    "out=\"\"",
    "while [ \"$#\" -gt 0 ]; do",
    "  if [ \"$1\" = \"--out\" ]; then out=\"$2\"; shift 2; else shift; fi",
    "done",
    "if [ -z \"$out\" ]; then echo missing --out >&2; exit 64; fi",
    "if [ -n \"${DATABASE_URL:-}\" ]; then echo DATABASE_URL leaked >&2; exit 65; fi",
    "if [ -n \"${BUCEPHALUS_CLOUD_WORKER_TOKEN:-}\" ]; then echo worker token leaked >&2; exit 66; fi",
    "if [ -z \"${BUCEPHALUS_HOME:-}\" ]; then echo missing BUCEPHALUS_HOME >&2; exit 67; fi",
    "if [ \"${BUCEPHALUS_HOME:-}\" != \"${HOME:-}\" ]; then echo HOME mismatch >&2; exit 68; fi",
    "if [ -z \"${TMPDIR:-}\" ]; then echo missing TMPDIR >&2; exit 71; fi",
    "if [ \"${TMPDIR:-}\" != \"${BUCEPHALUS_HOME:-}/tmp\" ]; then echo TMPDIR mismatch >&2; exit 72; fi",
    "if [ \"${TMP:-}\" != \"${TMPDIR:-}\" ]; then echo TMP mismatch >&2; exit 73; fi",
    "if [ \"${TEMP:-}\" != \"${TMPDIR:-}\" ]; then echo TEMP mismatch >&2; exit 74; fi",
    "if [ -z \"${USER:-}\" ]; then echo missing USER >&2; exit 69; fi",
    "if [ -z \"${USERNAME:-}\" ]; then echo missing USERNAME >&2; exit 70; fi",
    "touch \"$TMPDIR/core-temp-probe\"",
    "mkdir -p \"$out\"",
    `cp -R ${JSON.stringify(`${packageDir}/.`)} "$out/"`,
    "echo '{\"ok\":true}'",
    "",
  ].join("\n"));
  await chmod(corePath, 0o755);
  return corePath;
}

function restoreEnv(name: string, previous: string | undefined): void {
  if (previous === undefined) {
    delete process.env[name];
  } else {
    process.env[name] = previous;
  }
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
    package_provenance: {
      schema_version: "cloud_package_provenance_v1",
      status: "hosted_attested",
      source: "hosted_core",
      message: "Cloud ran hosted Core authoring for this package and recorded the builder/core environment.",
    },
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
