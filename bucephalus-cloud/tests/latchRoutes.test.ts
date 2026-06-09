import { describe, expect, test } from "bun:test";
import { handleLatchRoute } from "../src/routes/latch";
import { HttpError } from "../src/http";
import type { LatchSubmissionRepository } from "../src/latch/repository";
import type { RegistryRepository } from "../src/registry/repository";

const UPLOAD_ID = "66666666-6666-4666-8666-666666666666";
const SUBMISSION_ID = "88888888-8888-4888-8888-888888888888";

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

  test("rejects latch resolve case limits before registry lookup", async () => {
    let registryCalls = 0;
    const repository = {
      async resolveAlias() {
        registryCalls += 1;
        return "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
      },
      async getContentObject() {
        registryCalls += 1;
        return null;
      },
    };

    await expect(handleLatchRoute(
      new Request("https://cloud.example/v1/latch/resolve", {
        method: "POST",
        body: JSON.stringify({
          benchmark: "demo-bench",
          case_limit: 201,
        }),
      }),
      new URL("https://cloud.example/v1/latch/resolve"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/case_limit must be an integer from 1 to 200");
    expect(registryCalls).toBe(0);
  });

  test("rejects malformed latch benchmark digests before registry lookup", async () => {
    let registryCalls = 0;
    const repository = {
      async resolveAlias() {
        registryCalls += 1;
        return "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
      },
      async getContentObject() {
        registryCalls += 1;
        return null;
      },
    };

    for (const [body, message, rawSecret] of [
      [
        {
          benchmark_ref: {
            digest: "sha256:not-hex-token=raw-latch-digest-secret",
          },
        },
        "/benchmark_ref/digest must be sha256:<64 lowercase hex chars>",
        "raw-latch-digest-secret",
      ],
      [
        {
          benchmark: "sha256:not-hex-token=raw-latch-benchmark-secret",
        },
        "/benchmark must be sha256:<64 lowercase hex chars>",
        "raw-latch-benchmark-secret",
      ],
    ] as const) {
      let caught: unknown;
      try {
        await handleLatchRoute(
          new Request("https://cloud.example/v1/latch/resolve", {
            method: "POST",
            body: JSON.stringify(body),
          }),
          new URL("https://cloud.example/v1/latch/resolve"),
          repository as unknown as RegistryRepository,
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_digest");
      expect(error.message).toBe(message);
      expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain(rawSecret);
    }
    expect(registryCalls).toBe(0);
  });

  test("registers a completed latch upload as a benchmark submission", async () => {
    const observed: Record<string, unknown> = {};
    const submissions = {
      async createSubmission(input: Record<string, unknown>) {
        Object.assign(observed, input);
        return {
          submission_id: SUBMISSION_ID,
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
          upload_id: UPLOAD_ID,
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
    expect(body.submission_id).toBe(SUBMISSION_ID);
    expect(body.benchmark).toEqual({
      id: "demo-bench",
      content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    });
    expect(observed).toMatchObject({
      dispatchId: "dispatch-1",
      uploadId: UPLOAD_ID,
      benchmarkRef: "demo-bench",
      resolutionId: "resolution-1",
      archiveDigest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      gradingStatus: "passed",
    });
  });

  test("redacts latch submission JSON blobs in wire responses", async () => {
    const observed: Record<string, unknown> = {};
    const submissions = {
      async createSubmission(input: Record<string, unknown>) {
        Object.assign(observed, input);
        return {
          submission_id: SUBMISSION_ID,
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
          dispatch_id: "dispatch-token=raw-latch-dispatch-token",
          upload_id: UPLOAD_ID,
          archive_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          benchmark: {
            id: "/Users/alice/private/demo-bench",
            content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          },
          resolution: {
            resolution_id: "resolution-token=raw-latch-resolution-token",
          },
          summary: {
            workspace: "/Users/alice/private/bench",
            note: "stored raw for owner",
          },
          lifecycle: {
            grading: {
              status: "failed in /private/tmp/grading token=raw-latch-grading-token",
            },
            logs: ["failed near file:///private/tmp/bench/log.txt token=raw-latch-token"],
          },
          result: {
            api_key: "sk-abcdefghijklmnopqrstuvwxyz",
            artifact_url: "https://alice:secret@example.com/result.json?token=raw-query#debug",
          },
        }),
      }),
      new URL("https://cloud.example/v1/latch/submissions"),
      {} as RegistryRepository,
      submissions as unknown as LatchSubmissionRepository,
    );
    const body = await response!.json();
    const text = JSON.stringify(body);

    expect(observed.summaryJson).toEqual({
      workspace: "/Users/alice/private/bench",
      note: "stored raw for owner",
    });
    expect(observed.dispatchId).toBe("dispatch-token=raw-latch-dispatch-token");
    expect(observed.benchmarkRef).toBe("/Users/alice/private/demo-bench");
    expect(observed.resolutionId).toBe("resolution-token=raw-latch-resolution-token");
    expect(observed.gradingStatus).toBe("failed in /private/tmp/grading token=raw-latch-grading-token");
    expect(body.dispatch_id).toBe("dispatch-token=[redacted-secret]");
    expect(body.benchmark.id).toBe("[redacted-local-path]");
    expect(body.resolution_id).toBe("resolution-token=[redacted-secret]");
    expect(body.grading_status).toContain("[redacted-local-path]");
    expect(body.grading_status).toContain("token=[redacted-secret]");
    expect(body.summary).toEqual({
      workspace: "[redacted-local-path]",
      note: "stored raw for owner",
    });
    expect(body.lifecycle.logs).toEqual([
      "failed near file://[redacted-local-path] token=[redacted-secret]",
    ]);
    expect(body.result).toEqual({
      api_key: "[redacted]",
      artifact_url: "https://example.com/result.json [redacted URL credentials/query]",
    });
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("/private/tmp");
    expect(text).not.toContain("raw-latch-dispatch-token");
    expect(text).not.toContain("raw-latch-resolution-token");
    expect(text).not.toContain("raw-latch-grading-token");
    expect(text).not.toContain("raw-latch-token");
    expect(text).not.toContain("raw-query");
    expect(text).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
  });

  test("latch submission paths fail clearly on malformed encoding before repository calls", async () => {
    let submissionCalls = 0;
    const submissions = {
      async getSubmission() {
        submissionCalls += 1;
        return null;
      },
    };

    let caught: unknown;
    try {
      await handleLatchRoute(
        new Request("https://cloud.example/v1/latch/submissions/%E0%A4%A-token=raw-latch-path-secret"),
        new URL("https://cloud.example/v1/latch/submissions/%E0%A4%A-token=raw-latch-path-secret"),
        {} as RegistryRepository,
        submissions as unknown as LatchSubmissionRepository,
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_path_param");
    expect(error.message).toBe("/submission_id must be valid percent-encoded UTF-8");
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("raw-latch-path-secret");
    expect(submissionCalls).toBe(0);
  });

  test("latch submission ids reject malformed UUIDs before repository calls", async () => {
    const calls: string[] = [];
    const submissions = {
      async getSubmission() {
        calls.push("getSubmission");
        return null;
      },
      async createSubmission() {
        calls.push("createSubmission");
        return null;
      },
    };

    const cases: Array<{
      request: Request;
      url: URL;
      message: string;
      forbidden: string;
    }> = [
      {
        request: new Request("https://cloud.example/v1/latch/submissions/token=raw-submission-id-secret"),
        url: new URL("https://cloud.example/v1/latch/submissions/token=raw-submission-id-secret"),
        message: "/submission_id must be a UUID",
        forbidden: "raw-submission-id-secret",
      },
      {
        request: new Request("https://cloud.example/v1/latch/submissions", {
          method: "POST",
          body: JSON.stringify({
            dispatch_id: "dispatch-1",
            upload_id: "token=raw-latch-upload-secret /Users/alice/private/upload",
            archive_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            benchmark: { id: "demo-bench" },
            resolution: {},
          }),
        }),
        url: new URL("https://cloud.example/v1/latch/submissions"),
        message: "/upload_id must be a UUID",
        forbidden: "raw-latch-upload-secret",
      },
    ];

    for (const testCase of cases) {
      let caught: unknown;
      try {
        await handleLatchRoute(
          testCase.request,
          testCase.url,
          {} as RegistryRepository,
          submissions as unknown as LatchSubmissionRepository,
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_request");
      expect(error.message).toBe(testCase.message);
      const encoded = JSON.stringify({ message: error.message, detail: error.detail });
      expect(encoded).not.toContain(testCase.forbidden);
      expect(encoded).not.toContain("/Users/alice");
    }
    expect(calls).toEqual([]);
  });

  test("rejects invalid latch submission list limits before listing", async () => {
    let listCalls = 0;
    const submissions = {
      async listSubmissions() {
        listCalls += 1;
        return [];
      },
    };

    await expect(handleLatchRoute(
      new Request("https://cloud.example/v1/latch/submissions?limit=token=raw-latch-limit"),
      new URL("https://cloud.example/v1/latch/submissions?limit=token=raw-latch-limit"),
      {} as RegistryRepository,
      submissions as unknown as LatchSubmissionRepository,
    )).rejects.toThrow("limit must be an integer from 1 to 200");
    expect(listCalls).toBe(0);
  });

  test("rejects malformed latch submission archive digests", async () => {
    await expect(handleLatchRoute(
      new Request("https://cloud.example/v1/latch/submissions", {
        method: "POST",
        body: JSON.stringify({
          dispatch_id: "dispatch-1",
          upload_id: UPLOAD_ID,
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
