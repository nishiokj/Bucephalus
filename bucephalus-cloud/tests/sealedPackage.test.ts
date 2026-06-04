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
      ]);
    } finally {
      await rm(root, { recursive: true, force: true });
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

  test("rejects unsafe archive paths before extraction", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-package-"));
    const outsideEntry = `../evil-${Math.random().toString(16).slice(2)}`;
    const outside = join(root, outsideEntry);
    try {
      const archivePath = join(root, "evil.tgz");
      await writeFile(outside, "evil");
      await tar.c({ gzip: true, cwd: root, file: archivePath }, [outsideEntry]);

      await expect(inspectSealedPackageArchive({
        archivePath,
        workDir: join(root, "work"),
      })).rejects.toThrow("unsafe entry path");
    } finally {
      await rm(outside, { force: true });
      await rm(root, { recursive: true, force: true });
    }
  });
});

async function writePackage(
  root: string,
  manifest: JsonObject,
  tasks: unknown[] = [],
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
