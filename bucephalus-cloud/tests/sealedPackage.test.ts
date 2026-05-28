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
  test("proposes entities for the documented current resolved experiment shape", async () => {
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

      expect(inspection.proposals.map((proposal) => proposal.entity.kind)).toEqual([
        "experiment_package",
        "variant",
        "metric",
        "runtime_profile",
        "dataset",
      ]);
      expect(inspection.diagnostics).toEqual([]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("fails transparently for unsupported legacy-only variant shapes", async () => {
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

async function writePackage(root: string, manifest: unknown): Promise<string> {
  const packageDir = join(root, "package");
  const archivePath = join(root, "package.tgz");
  await mkdir(packageDir, { recursive: true });
  await writeFile(join(packageDir, "manifest.json"), JSON.stringify(manifest));
  await tar.c({ gzip: true, cwd: packageDir, file: archivePath }, ["manifest.json"]);
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
