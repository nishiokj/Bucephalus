import { describe, expect, test } from "bun:test";
import { handleLatchRoute } from "../src/routes/latch";
import type { RegistryRepository } from "../src/registry/repository";

describe("latch routes", () => {
  test("resolves a Tier-1 benchmark registry object into a latch manifest", async () => {
    const repository = {
      async resolveAlias(kind: string, alias: string) {
        expect(kind).toBe("benchmark");
        expect(alias).toBe("demo-bench");
        return "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
      },
      async getContentObject(digest: string) {
        expect(digest).toBe("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        return {
          kind: "benchmark",
          schema_version: "benchmark_v1",
          canonical_json: {
            id: "demo-bench",
            tier_1_eligible: true,
            staging_shape: "file",
            grader_shape: "artifact_pure",
            manifest: {
              schema_version: "latch_manifest_v1",
              cases: [
                { case_id: "case-1", task_prompt: "One" },
                { case_id: "case-2", task_prompt: "Two" },
              ],
            },
            materials: [
              {
                id: "seed",
                text: "hello",
                filename: "seed.txt",
              },
            ],
          },
        };
      },
    };

    const response = await handleLatchRoute(
      new Request("https://cloud.example/v1/latch/resolve", {
        method: "POST",
        body: JSON.stringify({ benchmark: "demo-bench", case_limit: 1 }),
      }),
      new URL("https://cloud.example/v1/latch/resolve"),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    const body = await response!.json();
    expect(body.schema_version).toBe("latch_resolution_v1");
    expect(body.benchmark.content_digest).toBe("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    expect(body.manifest.schema_version).toBe("latch_manifest_v1");
    expect(body.manifest.cases).toHaveLength(1);
    expect(body.materials).toHaveLength(1);
  });
});
