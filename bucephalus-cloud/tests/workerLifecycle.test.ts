import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import { discoverCoreRunIdsFromRunRoot } from "../src/worker";

describe("worker lifecycle cleanup helpers", () => {
  test("discovers Core run IDs from the attempt run root only", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-lifecycle-"));
    try {
      const runRoot = join(root, "run-root");
      await mkdir(join(runRoot, "run_20260529_000001_000001_000001"), { recursive: true });
      await mkdir(join(runRoot, "not-a-run"), { recursive: true });
      await writeFile(join(runRoot, "run_file"), "");

      await expect(discoverCoreRunIdsFromRunRoot(runRoot)).resolves.toEqual([
        "run_20260529_000001_000001_000001",
      ]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("missing run root has no Core run IDs", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-lifecycle-"));
    try {
      await expect(discoverCoreRunIdsFromRunRoot(join(root, "missing"))).resolves.toEqual([]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});
