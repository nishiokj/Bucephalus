import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import * as tar from "tar";
import { canonicalJsonStringify, sha256Digest, type JsonObject } from "../src/primitives";
import {
  inspectSealedPackageArchive,
  SealedPackageInspectionError,
} from "../src/imports/sealedPackage";

describe("sealed package import inspection", () => {
  test("accepts a documented sealed package manifest without registry proposals", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath, packageDigest } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: currentResolvedExperiment(),
      });

      const inspection = await inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      });

      expect(inspection.packageDigest).toBe(packageDigest);
      expect(inspection.resolvedExperimentJson).toEqual(currentResolvedExperiment());
      expect(inspection.imageRefs).toEqual([]);
      expect(inspection.diagnostics).toEqual([]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("records packaged task image refs for Cloud runner requirements", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath } = await writePackage(
        root,
        {
          schema_version: "sealed_run_package_v2",
          created_at: "2026-05-27T00:00:00Z",
          resolved_experiment: currentResolvedExperiment(),
        },
        [
          {
            schema_version: "task_row_v2",
            id: "task-1",
            task: {},
            runtime: {
              container_image: {
                image: "ghcr.io/acme/task@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                workdir: "/workspace",
              },
            },
          },
        ],
      );

      const inspection = await inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      });

      expect(inspection.imageRefs).toEqual([
        "ghcr.io/acme/task@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      ]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("records case_v2 workspace image refs for Cloud runner requirements", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath } = await writePackage(
        root,
        {
          schema_version: "sealed_run_package_v2",
          created_at: "2026-05-27T00:00:00Z",
          resolved_experiment: currentResolvedExperiment(),
        },
        [
          {
            schema_version: "case_v2",
            id: "case-1",
            inputs: { prompt: "hello" },
            resources: {
              workspace: {
                source: "container_image",
                image: "ghcr.io/acme/case-workspace@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                workdir: "/workspace",
              },
            },
          },
        ],
      );

      const inspection = await inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      });

      expect(inspection.imageRefs).toEqual([
        "ghcr.io/acme/case-workspace@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      ]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects malformed packaged task image declarations before Cloud acceptance", async () => {
    for (const [taskPatch, message] of [
      [
        {
          runtime: {
            container_image: "ghcr.io/acme/task:latest",
          },
        },
        "/tasks/0/runtime/container_image must be an object",
      ],
      [
        {
          runtime: {
            container_image: {
              image: 7,
            },
          },
        },
        "/tasks/0/runtime/container_image/image must be a non-empty image ref string",
      ],
      [
        {
          runtime: {
            container_image: {
              image: "",
            },
          },
        },
        "/tasks/0/runtime/container_image/image must be a non-empty image ref string",
      ],
      [
        {
          resources: {
            workspace: {
              image: false,
            },
          },
        },
        "/tasks/0/resources/workspace/image must be a non-empty image ref string",
      ],
    ] as const) {
      const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
      try {
        const { archivePath } = await writePackage(
          root,
          {
            schema_version: "sealed_run_package_v2",
            created_at: "2026-05-27T00:00:00Z",
            resolved_experiment: currentResolvedExperiment(),
          },
          [
            {
              schema_version: "task_row_v2",
              id: "task-1",
              task: {},
              ...taskPatch,
            },
          ],
        );

        await expect(inspectSealedPackageArchive({
          archivePath,
          workDir: join(root, "work"),
        })).rejects.toThrow(message);
      } finally {
        await rm(root, { recursive: true, force: true });
      }
    }
  });

  test("records resolved runtime agent and sidecar image refs for Cloud runner requirements", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: {
          ...currentResolvedExperiment(),
          trial_runtime: {
            agent: {
              image: "ghcr.io/acme/agent@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            },
            grader: {
              separate: {
                image: "ghcr.io/acme/grader@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
              },
            },
          },
          sidecars: {
            cache: {
              image: "ghcr.io/acme/cache@sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            },
          },
          ephemerals: {
            "mcp-bash": {
              image: "ghcr.io/acme/mcp-bash@sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            },
          },
        },
      });

      const inspection = await inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      });

      expect(inspection.imageRefs).toEqual([
        "ghcr.io/acme/agent@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "ghcr.io/acme/cache@sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "ghcr.io/acme/grader@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        "ghcr.io/acme/mcp-bash@sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
      ]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("records defensive stage-authored image refs before Cloud acceptance", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: {
          ...currentResolvedExperiment(),
          stages: {
            case: {
              workspace: {
                image: "ghcr.io/acme/stage-case@sha256:1111111111111111111111111111111111111111111111111111111111111111",
              },
            },
            agent: {
              image: "ghcr.io/acme/stage-agent@sha256:2222222222222222222222222222222222222222222222222222222222222222",
            },
            grader: {
              separate: {
                image: "ghcr.io/acme/stage-grader@sha256:3333333333333333333333333333333333333333333333333333333333333333",
              },
            },
          },
        },
      });

      const inspection = await inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      });

      expect(inspection.imageRefs).toEqual([
        "ghcr.io/acme/stage-agent@sha256:2222222222222222222222222222222222222222222222222222222222222222",
        "ghcr.io/acme/stage-case@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "ghcr.io/acme/stage-grader@sha256:3333333333333333333333333333333333333333333333333333333333333333",
      ]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("records defensive top-level trial runtime alias image refs before Cloud acceptance", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: {
          ...currentResolvedExperiment(),
          task: {
            workspace: {
              image: "ghcr.io/acme/top-task@sha256:1111111111111111111111111111111111111111111111111111111111111111",
            },
          },
          agent: {
            image: "ghcr.io/acme/top-agent@sha256:2222222222222222222222222222222222222222222222222222222222222222",
          },
          grader: {
            separate: {
              image: "ghcr.io/acme/top-grader@sha256:3333333333333333333333333333333333333333333333333333333333333333",
            },
          },
        },
      });

      const inspection = await inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      });

      expect(inspection.imageRefs).toEqual([
        "ghcr.io/acme/top-agent@sha256:2222222222222222222222222222222222222222222222222222222222222222",
        "ghcr.io/acme/top-grader@sha256:3333333333333333333333333333333333333333333333333333333333333333",
        "ghcr.io/acme/top-task@sha256:1111111111111111111111111111111111111111111111111111111111111111",
      ]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects malformed resolved runtime image declarations before Cloud acceptance", async () => {
    const cases: Array<[JsonObject, string]> = [
      [
        {
          trial_runtime: {
            agent: {
              image: false,
            },
          },
        },
        "/resolved_experiment/trial_runtime/agent/image must be a non-empty image ref string",
      ],
      [
        {
          trial_runtime: {
            grader: {
              separate: "local",
            },
          },
        },
        "/resolved_experiment/trial_runtime/grader/separate must be an object",
      ],
      [
        {
          trial_runtime: {
            task: {
              workspace: ["image"],
            },
          },
        },
        "/resolved_experiment/trial_runtime/task/workspace must be an object",
      ],
      [
        {
          sidecars: {
            cache: "redis:7",
          },
        },
        "/resolved_experiment/sidecars/cache must be an object",
      ],
      [
        {
          sidecars: {
            cache: {
              image: "",
            },
          },
        },
        "/resolved_experiment/sidecars/cache/image must be a non-empty image ref string",
      ],
      [
        {
          ephemerals: {
            "mcp-bash": "ghcr.io/acme/mcp-bash:latest",
          },
        },
        "/resolved_experiment/ephemerals/mcp-bash must be an object",
      ],
      [
        {
          ephemerals: {
            "mcp-bash": {
              image: false,
            },
          },
        },
        "/resolved_experiment/ephemerals/mcp-bash/image must be a non-empty image ref string",
      ],
      [
        {
          stages: {
            case: {
              workspace: {
                image: false,
              },
            },
          },
        },
        "/resolved_experiment/stages/case/workspace/image must be a non-empty image ref string",
      ],
      [
        {
          task: {
            workspace: {
              image: false,
            },
          },
        },
        "/resolved_experiment/task/workspace/image must be a non-empty image ref string",
      ],
    ];
    for (const [patch, message] of cases) {
      const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
      try {
        const { archivePath } = await writePackage(root, {
          schema_version: "sealed_run_package_v2",
          created_at: "2026-05-27T00:00:00Z",
          resolved_experiment: {
            ...currentResolvedExperiment(),
            ...patch,
          },
        });

        await expect(inspectSealedPackageArchive({
          archivePath,
          workDir: join(root, "work"),
        })).rejects.toThrow(message);
      } finally {
        await rm(root, { recursive: true, force: true });
      }
    }
  });

  test("does not destructure legacy-looking resolved experiment fields into registry entities", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath, packageDigest } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: {
          experiment: { id: "legacy_exp" },
          baseline: { variant_id: "baseline" },
          variant_plan: [{ variant_id: "treatment", bindings: {} }],
          runtime: { compute: { backend: "local-docker" } },
          matrix: { cases: { source: "file", path: "cases.jsonl" } },
          metrics: [{ id: "resolved", direction: "maximize" }],
        },
      });

      const inspection = await inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      });

      expect(inspection.packageDigest).toBe(packageDigest);
      expect(inspection.resolvedExperimentJson.experiment).toEqual({ id: "legacy_exp" });
      expect(inspection.diagnostics).toEqual([]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("fails transparently for an invalid sealed package manifest", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath } = await writeRawPackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        checksums_ref: "checksums.json",
        resolved_experiment: currentResolvedExperiment(),
      });

      await expect(
        inspectSealedPackageArchive({
          archivePath,
          workDir: join(root, "work"),
        }),
      ).rejects.toThrow(SealedPackageInspectionError);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects forged package digests before import acceptance", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: currentResolvedExperiment(),
        package_digest: "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
      });

      await expect(inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      })).rejects.toThrow("package digest mismatch");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects unchecksummed payload files", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { packageDir } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: currentResolvedExperiment(),
      }, []);
      await mkdir(join(packageDir, "files"), { recursive: true });
      await writeFile(join(packageDir, "files", "late.txt"), "not sealed");
      const archivePath = await archivePackage(root, packageDir);

      await expect(inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      })).rejects.toThrow("unchecksummed payload file");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects duplicate archive file entries before extraction", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { packageDir } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: currentResolvedExperiment(),
      });
      const archivePath = join(root, "duplicate.tgz");
      await tar.c({ gzip: true, cwd: packageDir, file: archivePath }, ["manifest.json", "manifest.json"]);

      await expect(inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      })).rejects.toThrow("duplicate file");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects unsafe archive paths before extraction", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    const outsideEntry = `../customer-a-prod-openai-token-${Math.random().toString(16).slice(2)}.env`;
    const outside = join(root, outsideEntry);
    try {
      const archivePath = join(root, "evil.tgz");
      await writeFile(outside, "evil");
      await tar.c({ gzip: true, cwd: root, file: archivePath }, [outsideEntry]);

      let caught: unknown;
      try {
        await inspectSealedPackageArchive({
          archivePath,
          workDir: join(root, "work"),
        });
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(SealedPackageInspectionError);
      const error = caught as SealedPackageInspectionError;
      const encoded = JSON.stringify({
        message: error.message,
        diagnostics: error.diagnostics,
      });
      expect(error.message).toContain("unsafe entry path");
      expect(error.diagnostics[0]?.pointer).toBe("/archive_entry");
      expect(encoded).not.toContain("customer-a-prod-openai-token");
      expect(encoded).not.toContain(outsideEntry);
    } finally {
      await rm(outside, { force: true });
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects manifest metadata refs that point into runtime payload roots", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: currentResolvedExperiment(),
        package_checks_ref: "runtime_assets/package_checks.json",
      });

      await expect(inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      })).rejects.toThrow("package_checks_ref must not point inside runtime payload directory");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("does not expose secret-looking package entry names in inspection errors", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    const secretRel = "files/customer-a-prod-openai-token.env";
    try {
      const { packageDir } = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        resolved_experiment: currentResolvedExperiment(),
      }, []);
      await mkdir(join(packageDir, "files"), { recursive: true });
      await writeFile(join(packageDir, secretRel), "not sealed");
      const archivePath = await archivePackage(root, packageDir);

      let caught: unknown;
      try {
        await inspectSealedPackageArchive({
          archivePath,
          workDir: join(root, "work"),
        });
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(SealedPackageInspectionError);
      const error = caught as SealedPackageInspectionError;
      const encoded = JSON.stringify({
        message: error.message,
        diagnostics: error.diagnostics,
      });
      expect(error.message).toContain("unchecksummed payload file");
      expect(error.diagnostics[0]?.pointer).toBe("/package_entry");
      expect(encoded).not.toContain(secretRel);
      expect(encoded).not.toContain("customer-a");
      expect(encoded).not.toContain("openai-token");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects package CAS pointers that reference missing blobs", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const { archivePath } = await writePackage(
        root,
        {
          schema_version: "sealed_run_package_v2",
          created_at: "2026-05-27T00:00:00Z",
          resolved_experiment: currentResolvedExperiment(),
        },
        [],
        {
          "runtime_assets/large.bin": JSON.stringify({
            schema_version: "bucephalus_cas_pointer_v1",
            kind: "file",
            digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            size_bytes: 12,
          }),
        },
      );

      await expect(inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      })).rejects.toThrow("references missing blob");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects package CAS pointers whose blob checksum is not the pointer digest", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const actualDigest = sha256Digest(Buffer.from("actual blob"));
      const expectedDigest = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
      const blobRel = `blobs/sha256/${expectedDigest.slice("sha256:".length)}/blob`;
      const { archivePath } = await writePackage(
        root,
        {
          schema_version: "sealed_run_package_v2",
          created_at: "2026-05-27T00:00:00Z",
          resolved_experiment: currentResolvedExperiment(),
        },
        [],
        {
          "runtime_assets/large.bin": JSON.stringify({
            schema_version: "bucephalus_cas_pointer_v1",
            kind: "file",
            digest: expectedDigest,
            size_bytes: 11,
          }),
          [blobRel]: "actual blob",
        },
      );
      expect(actualDigest).not.toBe(expectedDigest);

      await expect(inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      })).rejects.toThrow("blob digest mismatch");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});

async function writePackage(
  root: string,
  manifest: JsonObject,
  tasks: unknown[] = [],
  files: Record<string, string | Buffer> = {},
): Promise<{ archivePath: string; packageDir: string; packageDigest: string }> {
  const packageDir = join(root, "package");
  await mkdir(packageDir, { recursive: true });
  await writeFile(join(packageDir, "resolved_experiment.json"), JSON.stringify(manifest.resolved_experiment));
  await writeFile(join(packageDir, "staging_manifest.json"), JSON.stringify({
    schema_version: "package_staging_manifest_v1",
  }));
  if (tasks.length > 0) {
    await mkdir(join(packageDir, "tasks"), { recursive: true });
    await writeFile(join(packageDir, "tasks", "tasks.jsonl"), tasks.map((task) => JSON.stringify(task)).join("\n"));
  }
  for (const [rel, contents] of Object.entries(files)) {
    const path = join(packageDir, rel);
    await mkdir(join(path, ".."), { recursive: true });
    await writeFile(path, contents);
  }

  const checksums = await checksumsForPackage(packageDir);
  const packageDigest = typeof manifest.package_digest === "string"
    ? manifest.package_digest
    : sha256Digest(canonicalJsonStringify(checksums.files));
  await writeFile(join(packageDir, "checksums.json"), JSON.stringify(checksums));
  await writeFile(join(packageDir, "package.lock"), JSON.stringify({
    schema_version: "sealed_package_lock_v1",
    package_digest: packageDigest,
  }));
  await writeFile(join(packageDir, "manifest.json"), JSON.stringify({
    ...manifest,
    checksums_ref: "checksums.json",
    package_digest: packageDigest,
  }));
  return {
    archivePath: await archivePackage(root, packageDir),
    packageDir,
    packageDigest,
  };
}

async function writeRawPackage(root: string, manifest: unknown): Promise<{ archivePath: string; packageDir: string }> {
  const packageDir = join(root, "package");
  await mkdir(packageDir, { recursive: true });
  await writeFile(join(packageDir, "manifest.json"), JSON.stringify(manifest));
  return {
    archivePath: await archivePackage(root, packageDir),
    packageDir,
  };
}

async function archivePackage(root: string, packageDir: string): Promise<string> {
  const archivePath = join(root, `package-${Math.random().toString(16).slice(2)}.tgz`);
  await tar.c({ gzip: true, cwd: packageDir, file: archivePath }, (await readdir(packageDir)).sort());
  return archivePath;
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

function currentResolvedExperiment() {
  return {
    experiment: {
      id: "current_exp",
      name: "Current Experiment",
    },
    runtime: {
      compute: { backend: "local-docker" },
      storage: { backend: "local-fs" },
    },
    matrix: {
      variants: [{ id: "baseline", baseline: true, config: { model: "gpt-5" } }],
      cases: { source: "file", path: "cases.jsonl" },
      repeats: 1,
      seeds: [1],
    },
    metrics: [{ id: "resolved", direction: "maximize", primary: true }],
  };
}
