import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import * as tar from "tar";
import {
  inspectSealedPackageArchive,
  SealedPackageInspectionError,
} from "../src/imports/sealedPackage";

const PACKAGE_DIGEST = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

describe("sealed package import inspection", () => {
  test("accepts a documented sealed package manifest without registry proposals", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const archivePath = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        checksums_ref: "checksums.json",
        package_digest: PACKAGE_DIGEST,
        resolved_experiment: currentResolvedExperiment(),
      });

      const inspection = await inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      });

      expect(inspection.packageDigest).toBe(PACKAGE_DIGEST);
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
      const archivePath = await writePackage(
        root,
        {
          schema_version: "sealed_run_package_v2",
          created_at: "2026-05-27T00:00:00Z",
          checksums_ref: "checksums.json",
          package_digest: PACKAGE_DIGEST,
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

  test("does not destructure legacy-looking resolved experiment fields into registry entities", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const archivePath = await writePackage(root, {
        schema_version: "sealed_run_package_v2",
        created_at: "2026-05-27T00:00:00Z",
        checksums_ref: "checksums.json",
        package_digest: PACKAGE_DIGEST,
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

      expect(inspection.packageDigest).toBe(PACKAGE_DIGEST);
      expect(inspection.resolvedExperimentJson.experiment).toEqual({ id: "legacy_exp" });
      expect(inspection.diagnostics).toEqual([]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("fails transparently for an invalid sealed package manifest", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    try {
      const archivePath = await writePackage(root, {
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
});

async function writePackage(root: string, manifest: unknown, tasks: unknown[] = []): Promise<string> {
  const packageDir = join(root, "package");
  const archivePath = join(root, "package.tgz");
  await mkdir(packageDir, { recursive: true });
  await writeFile(join(packageDir, "manifest.json"), JSON.stringify(manifest));
  const entries = ["manifest.json"];
  if (tasks.length > 0) {
    await mkdir(join(packageDir, "tasks"), { recursive: true });
    await writeFile(join(packageDir, "tasks", "tasks.jsonl"), tasks.map((task) => JSON.stringify(task)).join("\n"));
    entries.push("tasks/tasks.jsonl");
  }
  await tar.c({ gzip: true, cwd: packageDir, file: archivePath }, entries);
  return archivePath;
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
