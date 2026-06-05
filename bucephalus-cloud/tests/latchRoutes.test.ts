import { describe, expect, test } from "bun:test";
import { handleLatchRoute } from "../src/routes/latch";
import type { LatchSubmissionRepository } from "../src/latch/repository";
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

  test("registers a completed latch upload as a benchmark submission", async () => {
    const observed: Record<string, unknown> = {};
    const submissions = {
      async createSubmission(input: Record<string, unknown>) {
        Object.assign(observed, input);
        return {
          submission_id: "submission-1",
          dispatch_id: input.dispatchId,
          upload_id: input.uploadId,
          owner_key: null,
          benchmark_ref: input.benchmarkRef,
          benchmark_digest: input.benchmarkDigest,
          resolution_id: input.resolutionId,
          archive_digest: input.archiveDigest,
          grading_status: input.gradingStatus,
          summary_json: input.summaryJson,
          lifecycle_json: input.lifecycleJson,
          result_json: input.resultJson,
          created_at: "2026-06-05T00:00:00Z",
          updated_at: "2026-06-05T00:00:00Z",
        };
      },
    };

    const response = await handleLatchRoute(
      new Request("https://cloud.example/v1/latch/submissions", {
        method: "POST",
        body: JSON.stringify({
          dispatch_id: "dispatch-1",
          upload_id: "upload-1",
          archive_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          benchmark: {
            id: "demo-bench",
            content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          },
          resolution: {
            resolution_id: "resolution-1",
          },
          summary: {
            case_count: 1,
          },
          lifecycle: {
            grading: {
              status: "passed",
            },
          },
          result: {
            schema_version: "latch_result_v1",
          },
        }),
      }),
      new URL("https://cloud.example/v1/latch/submissions"),
      {} as RegistryRepository,
      submissions as unknown as LatchSubmissionRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    const body = await response!.json();
    expect(body.submission_id).toBe("submission-1");
    expect(body.benchmark).toEqual({
      id: "demo-bench",
      content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    });
    expect(observed).toMatchObject({
      dispatchId: "dispatch-1",
      uploadId: "upload-1",
      benchmarkRef: "demo-bench",
      resolutionId: "resolution-1",
      archiveDigest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      gradingStatus: "passed",
    });
  });

  test("rejects malformed latch submission archive digests", async () => {
    await expect(handleLatchRoute(
      new Request("https://cloud.example/v1/latch/submissions", {
        method: "POST",
        body: JSON.stringify({
          dispatch_id: "dispatch-1",
          upload_id: "upload-1",
          archive_digest: "sha256:not-hex",
          benchmark: { id: "demo-bench" },
          resolution: {},
        }),
      }),
      new URL("https://cloud.example/v1/latch/submissions"),
      {} as RegistryRepository,
      {} as LatchSubmissionRepository,
    )).rejects.toThrow("archive_digest");
  });
});
