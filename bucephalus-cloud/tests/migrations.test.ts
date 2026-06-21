import { describe, expect, test } from "bun:test";

import { migrationAliases, migrationFiles } from "../src/db/migrate";

describe("cloud SQL migration manifest", () => {
  test("uses a single migration per numeric sequence prefix", async () => {
    const files = await migrationFiles();
    const byPrefix = new Map<string, string[]>();
    for (const file of files) {
      const prefix = file.match(/^(\d{4})_/)?.[1];
      expect(prefix, `migration file must start with a numeric prefix: ${file}`).toBeTruthy();
      byPrefix.set(prefix!, [...(byPrefix.get(prefix!) ?? []), file]);
    }

    const duplicates = [...byPrefix.entries()]
      .filter(([, names]) => names.length > 1)
      .map(([prefix, names]) => `${prefix}: ${names.join(", ")}`);
    expect(duplicates).toEqual([]);
  });

  test("keeps aliases for renamed runtime migrations", async () => {
    const files = await migrationFiles();
    const fileSet = new Set(files);

    expect(fileSet.has("0023_runtime_access_requests.sql")).toBe(true);
    expect(fileSet.has("0024_runner_instance_cordoned.sql")).toBe(true);
    expect(fileSet.has("0018_runtime_access_requests.sql")).toBe(false);
    expect(fileSet.has("0019_runner_instance_cordoned.sql")).toBe(false);

    expect(migrationAliases("0023_runtime_access_requests.sql")).toEqual(["0018_runtime_access_requests.sql"]);
    expect(migrationAliases("0024_runner_instance_cordoned.sql")).toEqual(["0019_runner_instance_cordoned.sql"]);
  });
});
